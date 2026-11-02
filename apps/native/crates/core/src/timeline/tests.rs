use std::collections::HashSet;
use std::sync::Arc;

use super::*;
use crate::thread::{ThreadState, ThreadStreamItem};

fn message(id: &str, role: Role, text: &str, run: Option<&str>, created: &str, updated: &str) -> Arc<TimelineEntry> {
    Arc::new(TimelineEntry {
        id: format!("{id}-entry"),
        created_at: created.into(),
        attempt: None,
        kind: EntryKind::Message(Arc::new(ChatMessage {
            id: id.into(),
            role,
            text: Arc::from(text),
            run_id: run.map(str::to_owned),
            streaming: false,
            created_at: created.into(),
            updated_at: updated.into(),
            meta: MessageMeta { input_intent: (role == Role::User).then(|| "turn_start".into()), ..MessageMeta::default() },
            item: None,
        })),
    })
}

fn work(id: &str, run: &str, at: &str, edit: impl FnOnce(&mut WorkLogEntry)) -> Arc<TimelineEntry> {
    let mut entry = WorkLogEntry {
        id: id.replace("-entry", ""),
        created_at: at.into(),
        run_id: Some(run.into()),
        label: "Ran command".into(),
        tone: Tone::Tool,
        ..WorkLogEntry::default()
    };
    edit(&mut entry);
    Arc::new(TimelineEntry { id: id.into(), created_at: at.into(), attempt: None, kind: EntryKind::Work(Arc::new(entry)) })
}

struct Input {
    activity: ThreadActivity,
    runs: HashSet<String>,
    groups: HashSet<String>,
}

impl Input {
    fn idle() -> Self {
        Input { activity: ThreadActivity::default(), runs: HashSet::new(), groups: HashSet::new() }
    }

    fn rows(&self, entries: &[Arc<TimelineEntry>]) -> Vec<TimelineRow> {
        let none = HashSet::new();
        let input = RowsInput {
            entries,
            activity: &self.activity,
            expanded_run_ids: &self.runs,
            expanded_attempt_ids: &none,
            expanded_work_group_ids: &self.groups,
        };
        derive_rows(&input, &mut RowsCache::default())
    }
}

fn ids(rows: &[TimelineRow]) -> Vec<&str> {
    rows.iter().map(|row| row.id.as_str()).collect()
}

fn running(run: &str) -> ThreadActivity {
    ThreadActivity {
        latest_run: Some(LatestRun { run_id: run.into(), status: "running".into(), started_at: Some("2026-01-01T00:00:00Z".into()), completed_at: None }),
        running_run_id: Some(run.into()),
        is_working: true,
        active_turn_started_at: Some("2026-01-01T00:00:00Z".into()),
        pending_approval_ids: HashSet::new(),
    }
}

fn fixture() -> crate::thread::ThreadProjection {
    let item: ThreadStreamItem = serde_json::from_str(include_str!("fixtures/showcase.json")).expect("fixture decodes");
    let mut state = ThreadState::default();
    state.apply(item);
    state.projection.expect("snapshot")
}

#[test]
fn only_the_terminal_assistant_message_gets_copy() {
    let entries = vec![
        message("user-1", Role::User, "Write a poem", None, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"),
        message("assistant-thought", Role::Assistant, "Ground it.", Some("turn-1"), "2026-01-01T00:00:10Z", "2026-01-01T00:00:11Z"),
        message("assistant-final", Role::Assistant, "Here is the poem.", Some("turn-1"), "2026-01-01T00:00:20Z", "2026-01-01T00:00:30Z"),
    ];
    let mut input = Input::idle();
    input.runs.insert("turn-1".into());
    let copies: Vec<bool> = input
        .rows(&entries)
        .iter()
        .filter_map(|row| match &row.kind {
            RowKind::Message { message, show_copy, .. } if message.role == Role::Assistant => Some(*show_copy),
            _ => None,
        })
        .collect();
    assert_eq!(copies, vec![false, true]);
}

#[test]
fn folds_commentary_and_settled_work_before_the_terminal_response() {
    let entries = vec![
        message("user-1", Role::User, "Build it", None, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"),
        message("assistant-first", Role::Assistant, "Checklist", Some("turn-1"), "2026-01-01T00:00:05Z", "2026-01-01T00:00:06Z"),
        work("work-entry-1", "turn-1", "2026-01-01T00:00:08Z", |_| {}),
        message("assistant-final", Role::Assistant, "Done", Some("turn-1"), "2026-01-01T00:00:20Z", "2026-01-01T00:00:22Z"),
    ];
    let mut input = Input::idle();
    let rows = input.rows(&entries);
    assert_eq!(ids(&rows), vec!["user-1-entry", "turn-fold:turn-1", "assistant-final-entry"]);
    let RowKind::TurnFold { label, expanded, .. } = &rows[1].kind else { panic!("expected a fold") };
    assert_eq!((label.as_str(), *expanded), ("Worked for 22s", false));

    input.runs.insert("turn-1".into());
    assert_eq!(
        ids(&input.rows(&entries)),
        vec!["user-1-entry", "turn-fold:turn-1", "assistant-first-entry", "work-entry-1", "assistant-final-entry"]
    );
}

#[test]
fn folds_completed_activity_after_the_terminal_response_unless_failed_or_live() {
    for count in 1..=3 {
        let mut entries = vec![
            work("work-entry-before-text", "turn-1", "2026-01-01T00:00:01Z", |w| {
                w.label = "Status updated".into();
                w.tone = Tone::Info;
            }),
            message("assistant-final", Role::Assistant, "CI is re-running.", Some("turn-1"), "2026-01-01T00:00:05Z", "2026-01-01T00:00:06Z"),
        ];
        let after = |index: usize, status: ToolStatus| {
            work(&format!("work-entry-after-text-{index}"), "turn-1", &format!("2026-01-01T00:00:0{}Z", index + 7), |w| {
                w.item_type = Some("command_execution");
                w.status = Some(status);
            })
        };
        entries.extend((0..count).map(|index| after(index, ToolStatus::Completed)));
        let mut input = Input::idle();
        input.activity.latest_run = Some(LatestRun {
            run_id: "turn-1".into(),
            status: "completed".into(),
            started_at: Some("2026-01-01T00:00:00Z".into()),
            completed_at: Some("2026-01-01T00:00:10Z".into()),
        });
        assert_eq!(ids(&input.rows(&entries)), vec!["turn-fold:turn-1", "assistant-final-entry"]);

        input.runs.insert("turn-1".into());
        assert!(input.rows(&entries).iter().any(|row| row.id == "work-entry-after-text-0"
            || (row.id == "work-toggle:work-entry-after-text-0"
                && matches!(&row.kind, RowKind::WorkToggle { hidden_count, .. } if *hidden_count == count))));
        input.runs.clear();

        let mut failed = entries.clone();
        failed[2] = after(0, ToolStatus::Failed);
        assert!(input.rows(&failed).iter().any(|row| row.id == "work-entry-after-text-0"));

        let mut live = entries.clone();
        live[2] = after(0, ToolStatus::InProgress);
        input.activity = running("turn-1");
        assert!(input.rows(&live).iter().any(|row| matches!(&row.kind, RowKind::WorkLive { entry, .. } if entry.id == "work-after-text-0")));
    }
}

#[test]
fn a_working_turn_shows_the_working_row_and_live_thinking() {
    let entries = vec![message("user-1", Role::User, "Go", Some("run-1"), "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z")];
    let mut input = Input::idle();
    input.activity = running("run-1");
    let rows = input.rows(&entries);
    assert_eq!(ids(&rows), vec!["user-1-entry", "working-indicator-row", "live-activity-row"]);
    assert!(matches!(rows[2].kind, RowKind::Thinking { group_id: None, .. }));
}

#[test]
fn a_running_command_owns_the_shared_live_row() {
    let entries = vec![
        message("user-1", Role::User, "Go", Some("run-1"), "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"),
        work("cmd-1-entry", "run-1", "2026-01-01T00:00:02Z", |w| {
            w.item_type = Some("command_execution");
            w.command = Some("bash -lc 'vp test run'".into());
            w.status = Some(ToolStatus::InProgress);
        }),
    ];
    let mut input = Input::idle();
    input.activity = running("run-1");
    let rows = input.rows(&entries);
    assert_eq!(ids(&rows), vec!["user-1-entry", "working-indicator-row", "live-activity-row"]);
    let RowKind::WorkLive { entry, active, .. } = &rows[2].kind else { panic!("expected a live row: {rows:?}") };
    assert!(*active);
    assert_eq!(live_work_entry_label(entry, None, true), "Running vp");
}

#[test]
fn adjacent_settled_tools_group_into_one_summary_row() {
    let entries = vec![
        message("user-1", Role::User, "Go", Some("run-1"), "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"),
        work("cmd-1", "run-1", "2026-01-01T00:00:01Z", |w| {
            w.item_type = Some("command_execution");
            w.command = Some("git status".into());
            w.status = Some(ToolStatus::Completed);
        }),
        work("cmd-2", "run-1", "2026-01-01T00:00:02Z", |w| {
            w.item_type = Some("command_execution");
            w.command = Some("vp test run".into());
            w.status = Some(ToolStatus::Completed);
        }),
        work("edit-1", "run-1", "2026-01-01T00:00:03Z", |w| {
            w.item_type = Some("file_change");
            w.changed_files = vec!["src/a.ts".into()];
            w.status = Some(ToolStatus::Completed);
        }),
    ];
    let mut input = Input::idle();
    input.runs.insert("run-1".into());
    let rows = input.rows(&entries);
    assert_eq!(ids(&rows), vec!["user-1-entry", "turn-fold:run-1", "work-toggle:cmd-1"]);
    let RowKind::WorkToggle { summary, hidden_count, summary_kind, .. } = &rows[2].kind else { unreachable!() };
    assert_eq!(summary, "Ran 2 commands and changed 1 file");
    assert_eq!(*hidden_count, 3);
    assert_eq!(*summary_kind, SummaryKind::Mixed);

    input.groups.insert("work-group:cmd-1".into());
    let expanded = input.rows(&entries);
    assert_eq!(expanded[3].id, "work-group:cmd-1:details");
    assert!(matches!(&expanded[3].kind, RowKind::Work { expanded_group: true, entries, .. } if entries.len() == 3));
}

#[test]
fn consecutive_own_messages_group_and_steers_label_once() {
    let steer = |id: &str| {
        let entry = message(id, Role::User, "more", Some("run-1"), "2026-01-01T00:00:01Z", "2026-01-01T00:00:01Z");
        let EntryKind::Message(m) = &entry.kind else { unreachable!() };
        let mut m = (**m).clone();
        m.meta.input_intent = Some("steer".into());
        Arc::new(TimelineEntry { kind: EntryKind::Message(Arc::new(m)), ..(*entry).clone() })
    };
    let entries = vec![steer("s1"), steer("s2")];
    let rows = Input::idle().rows(&entries);
    let flags: Vec<(bool, bool, bool)> = rows
        .iter()
        .filter_map(|row| match row.kind {
            RowKind::Message { grouped_with_next, grouped_with_previous, continues_steer, .. } => {
                Some((grouped_with_next, grouped_with_previous, continues_steer))
            }
            _ => None,
        })
        .collect();
    assert_eq!(flags, vec![(true, false, false), (false, true, true)]);
}

#[test]
fn the_showcase_fixture_folds_into_prompt_fold_and_answer() {
    let projection = fixture();
    let entries = derive_entries(
        &mut EntriesCache::default(),
        &projection.visible_turn_items,
        &projection.attempts,
        &projection.nodes,
        &projection.plans,
    );
    assert!(!entries.iter().any(|e| matches!(&e.kind, EntryKind::Work(w) if w.command.as_deref() == Some("Preparing workspace"))));
    let mut input = Input::idle();
    input.activity = derive_activity(&projection);
    assert!(!input.activity.is_working);
    let rows = input.rows(&entries);
    let kinds: Vec<&str> = rows
        .iter()
        .map(|row| match &row.kind {
            RowKind::Message { message, .. } if message.role == Role::User => "user",
            RowKind::Message { .. } => "assistant",
            RowKind::TurnFold { .. } => "fold",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["user", "fold", "assistant"]);
    let RowKind::TurnFold { run_id, label, .. } = &rows[1].kind else { unreachable!() };
    assert!(label.starts_with("Worked for "), "{label}");

    input.runs.insert(run_id.clone());
    let labels: Vec<String> = input
        .rows(&entries)
        .iter()
        .filter_map(|row| match &row.kind {
            RowKind::Work { entries, display_label, .. } => Some(display_label.clone().unwrap_or_else(|| work_entry_display_label(&entries[0], None))),
            RowKind::WorkToggle { summary, .. } => Some(summary.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(labels, vec!["Received 1 update and read 1 file", "Changed 1 file and ran 1 command"]);
}

#[test]
fn entries_reuse_unchanged_items_between_derives() {
    let projection = fixture();
    let mut cache = EntriesCache::default();
    let mut derive = || {
        derive_entries(&mut cache, &projection.visible_turn_items, &projection.attempts, &projection.nodes, &projection.plans)
    };
    let first = derive();
    let second = derive();
    assert_eq!(first.len(), second.len());
    assert!(first.iter().zip(&second).all(|(a, b)| Arc::ptr_eq(a, b)));
}

#[test]
fn durations_and_tokens_match_the_web() {
    assert_eq!(format_duration(450.), "450ms");
    assert_eq!(format_duration(2_340.), "2.3s");
    assert_eq!(format_duration(2_000.), "2s");
    assert_eq!(format_duration(22_000.), "22s");
    assert_eq!(format_duration(59_600.), "1m");
    assert_eq!(format_duration(3_723_000.), "1h 2m 3s");
    assert_eq!(format_tokens(950), "950");
    assert_eq!(format_tokens(12_400), "12.4K");
    assert_eq!(format_tokens(1_500_000), "1.5M");
    assert_eq!(parse_iso_ms("1970-01-01T00:00:01.5Z"), Some(1_500));
    assert_eq!(parse_iso_ms("2026-01-01T02:00:00+02:00"), parse_iso_ms("2026-01-01T00:00:00Z"));
    assert_eq!(format_short_time("2026-01-01T13:05:00Z", 0).as_deref(), Some("1:05 PM"));
    let now = parse_iso_ms("2026-03-10T12:00:00Z").unwrap_or_default();
    assert_eq!(format_day_aware("2026-03-09T08:00:00Z", now, 0).as_deref(), Some("yesterday at 8:00 AM"));
    assert_eq!(format_day_aware("2026-01-05T08:00:00Z", now, 0).as_deref(), Some("1/5 8:00 AM"));
}

#[test]
fn command_program_names_follow_the_web() {
    let cases = [
        ("/bin/bash --rcfile /tmp/config -c 'git status'", Some("git")),
        ("zsh -lc 'CI=1 env -u DEBUG sudo -u root vp test run'", Some("vp")),
        ("sudo -u root pnpm test", Some("pnpm")),
        ("$HOME/.bun/bin/bun test", Some("bun")),
        ("rg -n \"if|for|while\" src", Some("rg")),
        ("vp test && git status", Some("vp")),
        ("echo `git status`", Some("echo")),
        ("cat <(rg needle src)", Some("cat")),
        ("cd packages/client-runtime && vp test run", Some("vp")),
        ("PATH+=:/tools npm test", Some("npm")),
        ("cd $(find . -type d | head -1) && git status", Some("git")),
        ("source ~/.nvm/nvm.sh && nvm use", Some("nvm")),
        ("unset DEBUG; node app.js", Some("node")),
        ("timeout 10s python3 script.py", Some("python3")),
        ("zsh -lc 'git status", None),
        ("if test -f package.json; then vp test; fi", None),
        ("export NODE_ENV=test", None),
        ("true", None),
    ];
    for (command, expected) in cases {
        assert_eq!(command_program_name(command).as_deref(), expected, "{command}");
    }
    assert_eq!(command_display_text("/bin/zsh -lc 'git status'"), "git status");
    assert_eq!(command_display_text("bash -c 'echo a' extra"), "bash -c 'echo a' extra");
}

#[test]
fn long_user_messages_collapse_by_visible_text() {
    assert!(!should_collapse_user_message("short"));
    assert!(should_collapse_user_message(&"line\n".repeat(9)));
    assert!(should_collapse_user_message(&"x".repeat(601)));
    assert!(!should_collapse_user_message(&format!("[label]({})", "a".repeat(700))));
}

#[test]
fn live_thought_lines_take_the_heading_or_first_sentence() {
    assert_eq!(live_thought_line("**Planning the fix**\nThen more."), "Planning the fix");
    assert_eq!(live_thought_line("I will read `main.rs` first. Then edit it."), "I will read main.rs first.");
}

#[test]
fn plan_titles_and_bodies() {
    let plan = "# Ship it\n\n## Summary\n\nDo the work.\n";
    assert_eq!(plan_title(plan).as_deref(), Some("Ship it"));
    assert_eq!(strip_displayed_plan_markdown(plan), "Do the work.");
}

#[test]
fn a_streamed_token_patches_rows_like_a_full_derive() {
    let streaming = |text: &str| {
        let entry = message("reply", Role::Assistant, text, Some("run-1"), "2026-01-01T00:00:01Z", "2026-01-01T00:00:02Z");
        let EntryKind::Message(m) = &entry.kind else { unreachable!() };
        let mut m = (**m).clone();
        m.streaming = true;
        Arc::new(TimelineEntry { kind: EntryKind::Message(Arc::new(m)), ..(*entry).clone() })
    };
    let prompt = message("user-1", Role::User, "Go", Some("run-1"), "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z");
    let input = Input { activity: running("run-1"), runs: HashSet::new(), groups: HashSet::new() };
    let none = HashSet::new();
    let derive = |entries: &[Arc<TimelineEntry>], cache: &mut RowsCache| {
        let rows_input = RowsInput {
            entries,
            activity: &input.activity,
            expanded_run_ids: &input.runs,
            expanded_attempt_ids: &none,
            expanded_work_group_ids: &input.groups,
        };
        derive_rows(&rows_input, cache)
    };
    let mut cache = RowsCache::default();
    let first = derive(&[prompt.clone(), streaming("Hel")], &mut cache);
    let patched = derive(&[prompt.clone(), streaming("Hello")], &mut cache);
    let full = derive(&[prompt, streaming("Hello")], &mut RowsCache::default());
    assert_eq!(ids(&patched), ids(&full));
    assert!(rows_equal(&first[0], &patched[0]));
    let text = |rows: &[TimelineRow]| {
        rows.iter().find_map(|row| match &row.kind {
            RowKind::Message { message, .. } if message.role == Role::Assistant => Some(message.text.to_string()),
            _ => None,
        })
    };
    assert_eq!(text(&patched).as_deref(), Some("Hello"));
    assert_eq!(text(&patched), text(&full));
}

fn row_summary(row: &TimelineRow) -> String {
    let detail = match &row.kind {
        RowKind::Work { entries, display_label, expanded_group } => {
            format!("work {:?} {display_label:?} {expanded_group}", entries.iter().map(|e| (&e.id, e.status)).collect::<Vec<_>>())
        }
        RowKind::WorkLive { entry, entries, active, .. } => format!("live {} {} {active}", entry.id, entries.len()),
        RowKind::WorkToggle { summary, hidden_count, has_failure, .. } => format!("toggle {summary} {hidden_count} {has_failure}"),
        RowKind::Message { message, show_assistant_meta, show_copy, .. } => {
            format!("message {} {} {} {show_assistant_meta} {show_copy}", message.id, message.text, message.streaming)
        }
        RowKind::AssistantMeta { message, .. } => format!("meta {}", message.id),
        RowKind::TurnFold { label, expanded, .. } => format!("fold {label} {expanded}"),
        RowKind::Event { item, subagents, .. } => format!("event {} {}", item.id, subagents.len()),
        other => format!("{other:?}"),
    };
    format!("{} {:?} {} {detail}", row.id, row.created_at, row.continues_work_log)
}

#[test]
fn incremental_derives_match_a_fresh_derive_at_every_streamed_frame() {
    let frames: Vec<ThreadStreamItem> = serde_json::from_str(include_str!("fixtures/stream.json")).expect("stream decodes");
    let mut state = ThreadState::default();
    let (mut entries_cache, mut rows_cache) = (EntriesCache::default(), RowsCache::default());
    let none = HashSet::new();
    let mut checked = 0;
    for frame in frames {
        if !state.apply(frame) {
            continue;
        }
        let Some(projection) = state.projection.as_ref() else { continue };
        let activity = derive_activity(projection);
        let derive = |entries_cache: &mut EntriesCache, rows_cache: &mut RowsCache| {
            let entries = derive_entries(entries_cache, &projection.visible_turn_items, &projection.attempts, &projection.nodes, &projection.plans);
            let input = RowsInput { entries: &entries, activity: &activity, expanded_run_ids: &none, expanded_attempt_ids: &none, expanded_work_group_ids: &none };
            derive_rows(&input, rows_cache).iter().map(row_summary).collect::<Vec<_>>()
        };
        let incremental = derive(&mut entries_cache, &mut rows_cache);
        let fresh = derive(&mut EntriesCache::default(), &mut RowsCache::default());
        assert_eq!(incremental, fresh);
        checked += 1;
    }
    assert!(checked > 10, "only {checked} frames changed the projection");
}

#[test]
fn answered_questions_preview_the_question_and_answer() {
    let answer = serde_json::json!({
        "requestId": "r1",
        "questionTextById": { "q1": "Which  database?" },
        "answers": { "q1": { "answers": ["Postgres", "SQLite"] } },
        "attachmentsByQuestionId": {}
    });
    assert_eq!(question_text_preview(&answer), "Which database?");
    assert_eq!(question_answer_preview(&answer), "Postgres, SQLite");
    assert!(has_question_answer(&answer));
}
