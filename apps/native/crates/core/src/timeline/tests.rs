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
    setup: Option<Arc<crate::worktree_setup::Snapshot>>,
}

impl Input {
    fn idle() -> Self {
        Input { activity: ThreadActivity::default(), runs: HashSet::new(), groups: HashSet::new(), setup: None }
    }

    fn rows(&self, entries: &[Arc<TimelineEntry>]) -> Vec<TimelineRow> {
        let none = HashSet::new();
        let input = RowsInput {
            entries,
            activity: &self.activity,
            expanded_run_ids: &self.runs,
            expanded_attempt_ids: &none,
            expanded_work_group_ids: &self.groups,
            worktree_setup: self.setup.as_ref(),
        };
        derive_rows(&input, &mut RowsCache::default()).iter().map(|row| (**row).clone()).collect()
    }
}

fn ids(rows: &[TimelineRow]) -> Vec<&str> {
    rows.iter().map(|row| row.id.as_str()).collect()
}

fn running(run: &str) -> ThreadActivity {
    ThreadActivity {
        latest_run: Some(LatestRun { run_id: run.into(), status: "running".into(), started_at: Some("2026-01-01T00:00:00Z".into()), completed_at: None, turn_ended: false }),
        running_run_id: Some(run.into()),
        is_working: true,
        runless_work_active: false,
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
fn only_the_terminal_assistant_message_gets_meta_and_copy() {
    let entries = vec![
        message("user-1", Role::User, "Write a poem", None, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"),
        message("assistant-thought", Role::Assistant, "Ground it.", Some("turn-1"), "2026-01-01T00:00:10Z", "2026-01-01T00:00:11Z"),
        message("assistant-final", Role::Assistant, "Here is the poem.", Some("turn-1"), "2026-01-01T00:00:20Z", "2026-01-01T00:00:30Z"),
    ];
    let mut input = Input::idle();
    input.runs.insert("turn-1".into());
    let controls: Vec<(bool, bool)> = input
        .rows(&entries)
        .iter()
        .filter_map(|row| match &row.kind {
            RowKind::Message { message, show_assistant_meta, show_copy, .. } if message.role == Role::Assistant => Some((*show_assistant_meta, *show_copy)),
            _ => None,
        })
        .collect();
    assert_eq!(controls, vec![(false, false), (true, true)]);
}

#[test]
fn a_running_responses_terminal_reply_has_no_meta_until_it_settles() {
    for streaming in [false, true] {
        let mut reply = message("assistant-reply", Role::Assistant, "Training finished, checking memory next.", Some("turn-1"), "2026-01-01T00:00:10Z", "2026-01-01T00:00:12Z");
        if let EntryKind::Message(message) = &mut Arc::make_mut(&mut reply).kind {
            Arc::make_mut(message).streaming = streaming;
        }
        let entries = vec![
            reply,
            work("command-entry", "turn-1", "2026-01-01T00:00:15Z", |w| {
                w.command = Some("nvidia-smi".into());
                w.status = Some(ToolStatus::InProgress);
            }),
        ];
        let mut input = Input::idle();
        input.activity = running("turn-1");
        let rows = input.rows(&entries);
        let reply = rows.iter().find(|row| row.id == "assistant-reply-entry").expect("terminal reply");
        assert_eq!(reply.kind, RowKind::Message {
            message: entries[0].message().expect("assistant message").clone(),
            show_assistant_meta: false,
            show_copy: false,
            copy_streaming: true,
            grouped_with_next: false,
            grouped_with_previous: false,
            continues_steer: false,
        });
        assert!(!rows.iter().any(|row| matches!(row.kind, RowKind::AssistantMeta { .. })));

        input.activity = ThreadActivity::default();
        let rows = input.rows(&entries[..1]);
        let reply = rows.iter().find(|row| row.id == "assistant-reply-entry").expect("settled terminal reply");
        assert!(matches!(reply.kind, RowKind::Message { show_assistant_meta: true, show_copy: true, copy_streaming, .. } if copy_streaming == streaming));
    }
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
            turn_ended: false,
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
    assert_eq!(format_day_aware("2026-03-09T08:00:00Z", now, &crate::sidebar::time::FixedZone(0), crate::client_settings::TimestampFormat::TwelveHour, &TimestampEnglish { british: false }), "yesterday at 8:00 AM");
    assert_eq!(format_day_aware("2026-01-05T08:00:00Z", now, &crate::sidebar::time::FixedZone(0), crate::client_settings::TimestampFormat::TwelveHour, &TimestampEnglish { british: false }), "1/5 8:00 AM");
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
    let input = Input { activity: running("run-1"), runs: HashSet::new(), groups: HashSet::new(), setup: None };
    let none = HashSet::new();
    let derive = |entries: &[Arc<TimelineEntry>], cache: &mut RowsCache| {
        let rows_input = RowsInput {
            entries,
            activity: &input.activity,
            expanded_run_ids: &input.runs,
            expanded_attempt_ids: &none,
            expanded_work_group_ids: &input.groups,
            worktree_setup: None,
        };
        derive_rows(&rows_input, cache).iter().map(|row| (**row).clone()).collect::<Vec<_>>()
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
            let input = RowsInput { entries: &entries, activity: &activity, expanded_run_ids: &none, expanded_attempt_ids: &none, expanded_work_group_ids: &none, worktree_setup: None };
            derive_rows(&input, rows_cache).iter().map(|row| row_summary(row)).collect::<Vec<_>>()
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

fn projected(items: Vec<serde_json::Value>) -> Vec<crate::thread::ProjectedTurnItem> {
    items
        .into_iter()
        .enumerate()
        .map(|(position, item)| {
            let id = item["id"].clone();
            let thread = item["threadId"].clone();
            serde_json::from_value(serde_json::json!({
                "position": position, "visibility": "local", "sourceThreadId": thread, "sourceItemId": id, "item": item,
            }))
            .expect("projected item decodes")
        })
        .collect()
}

fn shape(rows: &[TimelineRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match &row.kind {
            RowKind::TurnFold { label, .. } => format!("fold:{label}"),
            RowKind::Message { message, .. } => format!("{}:{}", if message.role == Role::User { "user" } else { "assistant" }, message.id),
            RowKind::Work { .. } => "work".into(),
            RowKind::WorkLive { .. } => "work-live".into(),
            RowKind::WorkToggle { .. } => "work-toggle".into(),
            RowKind::Working { compacting } => if *compacting { "working:compacting".into() } else { "working".into() },
            RowKind::Thinking { .. } => "thinking".into(),
            RowKind::ContextCompaction { active, .. } => format!("compaction:{}", if *active { "active" } else { "settled" }),
            RowKind::AttemptFold { .. } => "attempt-fold".into(),
            other => format!("{other:?}").split([' ', '{', '(']).next().unwrap_or_default().to_owned(),
        })
        .collect()
}

/// `MessagesTimeline.logic.test.ts` "folds each run of a provider-native subagent thread like a normal turn".
#[test]
fn provider_native_subagent_runs_fold_and_stay_live_while_running() {
    let at = |second: u32| format!("2026-09-25T22:{:02}:{:02}.000Z", 51 + second / 60, second % 60);
    let base = |id: &str, ordinal: u64, second: u32, end: u32| {
        serde_json::json!({
            "id": id, "threadId": "subagent-child", "runId": null, "nodeId": "task-root", "providerThreadId": null,
            "providerTurnId": null, "nativeItemRef": null, "parentItemId": null, "ordinal": ordinal, "status": "completed",
            "title": null, "startedAt": at(second), "completedAt": at(end), "updatedAt": at(end),
        })
    };
    let with = |mut value: serde_json::Value, extra: serde_json::Value| {
        if let (Some(object), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
            object.extend(extra.clone());
        }
        value
    };
    let prompt = |id: &str, ordinal, second| {
        with(base(id, ordinal, second, second), serde_json::json!({
            "type": "user_message", "messageId": id, "text": format!("Prompt {id}"), "attachments": [],
            "inputIntent": "turn_start", "createdBy": "agent", "creationSource": "provider",
        }))
    };
    let answer = |id: &str, ordinal, second| {
        with(base(id, ordinal, second, second), serde_json::json!({ "type": "assistant_message", "messageId": id, "text": format!("Answer {id}"), "streaming": false }))
    };
    let items = |resume: &str| {
        let mut items = vec![
            prompt("launch", 1, 0),
            with(base("launch-ls", 2, 4, 4), serde_json::json!({ "type": "command_execution", "input": "ls src" })),
            with(base("launch-thinking", 3, 8, 8), serde_json::json!({ "type": "reasoning", "title": "Thinking", "text": "Not there.", "streaming": false })),
            answer("launch-answer", 4, 8),
            prompt("resume", 5, 72),
        ];
        let mut ls = with(base("resume-ls", 6, 77, 77), serde_json::json!({ "type": "command_execution", "input": "ls src" }));
        if resume == "running" {
            ls["status"] = "running".into();
            ls["completedAt"] = serde_json::Value::Null;
        }
        items.push(ls);
        if resume == "failed" {
            items.push(with(base("resume-error", 7, 80, 80), serde_json::json!({
                "type": "error", "status": "failed",
                "failure": { "class": "provider_error", "message": "Subagent failed", "code": null, "retryable": null },
            })));
        }
        if resume != "running" {
            items.push(answer("resume-answer", 8, 80));
        }
        projected(items)
    };
    let rows = |resume: &str, working: bool, expanded: Option<&str>| {
        let visible = items(resume);
        let entries = derive_entries(&mut EntriesCache::default(), &visible, &[], &[], &[]);
        let mut input = Input::idle();
        input.activity.is_working = working;
        input.activity.runless_work_active = working;
        input.activity.active_turn_started_at = working.then(|| at(72));
        input.runs.extend(expanded.map(str::to_owned));
        shape(&input.rows(&entries))
    };
    let settled = rows("completed", false, None);
    assert_eq!(settled, ["user:launch", "fold:Worked for 8s", "assistant:launch-answer", "user:resume", "fold:Worked for 8s", "assistant:resume-answer"]);
    let launch_fold = {
        let visible = items("completed");
        let entries = derive_entries(&mut EntriesCache::default(), &visible, &[], &[], &[]);
        Input::idle().rows(&entries).iter().find_map(|row| match &row.kind {
            RowKind::TurnFold { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
    };
    assert_eq!(
        rows("completed", false, launch_fold.as_deref()),
        ["user:launch", "fold:Worked for 8s", "work-toggle", "assistant:launch-answer", "user:resume", "fold:Worked for 8s", "assistant:resume-answer"]
    );
    assert_eq!(rows("running", true, None), ["user:launch", "fold:Worked for 8s", "assistant:launch-answer", "user:resume", "working", "work-live"]);
    assert_eq!(rows("failed", false, None), ["user:launch", "fold:Worked for 8s", "assistant:launch-answer", "user:resume", "work", "work", "assistant:resume-answer"]);
}

fn projection_json(thread: serde_json::Value, runs: serde_json::Value, nodes: serde_json::Value, provider_turns: serde_json::Value) -> crate::thread::ThreadProjection {
    let mut thread_json = serde_json::json!({
        "id": "t", "projectId": "p", "title": "T", "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "m" }, "runtimeMode": "full-access", "interactionMode": "default",
        "branch": null, "worktreePath": null,
        "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t" },
        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z", "archivedAt": null,
    });
    if let (Some(object), Some(patch)) = (thread_json.as_object_mut(), thread.as_object()) {
        object.extend(patch.clone());
    }
    serde_json::from_value(serde_json::json!({
        "thread": thread_json, "runs": runs, "attempts": [], "nodes": nodes, "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": provider_turns, "runtimeRequests": [], "messages": [], "plans": [], "turnItems": [],
        "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [],
        "updatedAt": "2026-01-01T00:00:00Z",
    }))
    .expect("projection decodes")
}

/// `deriveRunlessWorkStartedAt`: a provider-native subagent thread works through a runless root turn.
#[test]
fn a_running_runless_root_turn_makes_a_subagent_thread_work() {
    let node = |status: &str| serde_json::json!([{ "id": "root", "kind": "root_turn", "runId": null, "status": status, "startedAt": "2026-01-01T00:01:00Z", "completedAt": null }]);
    let subagent = serde_json::json!({
        "creationSource": "provider",
        "lineage": { "parentThreadId": "parent", "relationshipToParent": "subagent", "rootThreadId": "parent" },
    });
    let activity = derive_activity(&projection_json(subagent.clone(), serde_json::json!([]), node("running"), serde_json::json!([])));
    assert!(activity.is_working && activity.runless_work_active);
    assert_eq!(activity.active_turn_started_at.as_deref(), Some("2026-01-01T00:01:00Z"));
    assert!(!derive_activity(&projection_json(subagent, serde_json::json!([]), node("completed"), serde_json::json!([]))).is_working);
    let not_subagent = derive_activity(&projection_json(serde_json::json!({}), serde_json::json!([]), node("running"), serde_json::json!([])));
    assert!(!not_subagent.runless_work_active);
}

/// `MessagesTimeline.logic.test.ts` "does not show Thinking once the open run's provider turn has ended".
#[test]
fn thinking_stops_once_the_open_runs_provider_turn_ends() {
    let entries = vec![message("user", Role::User, "Take over the queue", Some("turn-1"), "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z")];
    let mut input = Input::idle();
    input.activity = running("turn-1");
    assert!(matches!(input.rows(&entries).last().map(|row| &row.kind), Some(RowKind::Thinking { .. })));
    if let Some(run) = input.activity.latest_run.as_mut() {
        run.turn_ended = true;
    }
    assert!(!input.rows(&entries).iter().any(|row| matches!(row.kind, RowKind::Thinking { .. })));
}

#[test]
fn the_activity_run_reports_its_provider_turn_ending() {
    let run = |status: &str| {
        serde_json::json!([{
            "id": "run-1", "ordinal": 1, "providerInstanceId": "codex", "modelSelection": { "instanceId": "codex", "model": "m" },
            "userMessageId": "u", "rootNodeId": null, "status": "running", "requestedAt": "2026-01-01T00:00:00Z",
            "startedAt": "2026-01-01T00:00:00Z", "completedAt": null, "activeAttemptId": "attempt-1",
            "turnStatus": status,
        }])
    };
    let turns = |status: &str| serde_json::json!([{ "id": "pt-1", "runAttemptId": "attempt-1", "ordinal": 1, "status": status }]);
    let ended = derive_activity(&projection_json(serde_json::json!({}), run("x"), serde_json::json!([]), turns("completed")));
    assert!(ended.latest_run.as_ref().is_some_and(|r| r.turn_ended));
    let live = derive_activity(&projection_json(serde_json::json!({}), run("x"), serde_json::json!([]), turns("running")));
    assert!(live.latest_run.as_ref().is_some_and(|r| !r.turn_ended));
}

/// `MessagesTimeline.logic.test.ts` "a steer that supersedes an attempt".
#[test]
fn a_reply_finished_before_the_steer_stays_visible() {
    let at = |second: u32| format!("2026-01-01T00:00:{second:02}Z");
    let attempt = |id: &str, status: &str, completed: u32| TimelineAttempt {
        id: id.into(),
        run_id: "run-steered".into(),
        status: status.into(),
        completed_at: Some(at(completed)),
    };
    let superseded = attempt("attempt-1", "superseded", 10);
    let next = attempt("attempt-2", "completed", 14);
    let with_attempt = |entry: Arc<TimelineEntry>, attempt: &TimelineAttempt| Arc::new(TimelineEntry { attempt: Some(attempt.clone()), ..(*entry).clone() });
    let reply = |id: &str, attempt: &TimelineAttempt, completed: u32| {
        let item: crate::turn_item::TurnItem = serde_json::from_value(serde_json::json!({
            "id": id, "threadId": "t", "runId": "run-steered", "nodeId": null, "parentItemId": null, "ordinal": 1, "status": "completed",
            "title": null, "startedAt": at(2), "completedAt": at(completed), "updatedAt": at(completed),
            "type": "assistant_message", "messageId": id, "text": format!("{id} text"), "streaming": false,
        }))
        .expect("item decodes");
        let entry = message(id, Role::Assistant, &format!("{id} text"), Some("run-steered"), &at(2), &at(completed));
        let EntryKind::Message(m) = &entry.kind else { unreachable!() };
        let mut m = (**m).clone();
        m.item = Some(Arc::new(item));
        Arc::new(TimelineEntry { id: id.into(), attempt: Some(attempt.clone()), kind: EntryKind::Message(Arc::new(m)), ..(*entry).clone() })
    };
    let rows_for = |reply_completed: u32, run_status: &str| {
        let steer = {
            let entry = message("steer-user", Role::User, "What are you working on?", Some("run-steered"), &at(10), &at(10));
            let EntryKind::Message(m) = &entry.kind else { unreachable!() };
            let mut m = (**m).clone();
            m.meta.input_intent = Some("steer".into());
            Arc::new(TimelineEntry { attempt: Some(next.clone()), kind: EntryKind::Message(Arc::new(m)), ..(*entry).clone() })
        };
        let entries = vec![
            with_attempt(message("initial-user", Role::User, "Build it", Some("run-steered"), &at(0), &at(0)), &superseded),
            with_attempt(work("old-work-entry", "run-steered", &at(1), |w| w.label = "Read".into()), &superseded),
            reply("old-reply-entry", &superseded, reply_completed),
            steer,
            reply("new-reply-entry", &next, 14),
        ];
        let mut input = Input::idle();
        input.activity.latest_run = Some(LatestRun {
            run_id: "run-steered".into(),
            status: run_status.into(),
            started_at: Some(at(0)),
            completed_at: (run_status == "completed").then(|| at(14)),
            turn_ended: false,
        });
        input.rows(&entries)
    };
    for status in ["running", "completed"] {
        let rows = rows_for(4, status);
        assert!(!rows.iter().any(|row| matches!(row.kind, RowKind::AttemptFold { .. })), "{status}: {:?}", shape(&rows));
        assert!(ids(&rows).contains(&"old-reply-entry"), "{status}: {:?}", shape(&rows));
    }
    let cut_off = rows_for(11, "running");
    assert!(cut_off.iter().any(|row| matches!(&row.kind, RowKind::AttemptFold { attempt_id, label, .. } if attempt_id == "attempt-1" && label == "Superseded attempt")));
    assert!(!ids(&cut_off).contains(&"old-reply-entry"));
}

/// `MessagesTimeline.logic.test.ts` "shows each compaction trigger as one live status".
#[test]
fn a_compact_turn_shows_one_live_status_and_no_extra_working_rows() {
    let compact_prompt = |reason: Option<&str>| {
        let entry = message("compact", Role::User, "/compact", Some("run-1"), "2026-01-01T00:00:05Z", "2026-01-01T00:00:05Z");
        let EntryKind::Message(m) = &entry.kind else { unreachable!() };
        let mut m = (**m).clone();
        m.meta.idle_compaction = reason.map(str::to_owned);
        Arc::new(TimelineEntry { kind: EntryKind::Message(Arc::new(m)), ..(*entry).clone() })
    };
    let compaction = |status: &str| {
        let item: crate::turn_item::TurnItem = serde_json::from_value(serde_json::json!({
            "id": "compaction", "threadId": "t", "runId": "run-1", "nodeId": null, "parentItemId": null, "ordinal": 2, "status": status,
            "title": null, "startedAt": "2026-01-01T00:00:06Z", "completedAt": null, "updatedAt": "2026-01-01T00:00:06Z",
            "type": "compaction", "driver": null, "beforeTokenCount": 899000, "afterTokenCount": 19000,
        }))
        .expect("item decodes");
        let row = crate::thread::ProjectedTurnItem {
            position: 0,
            visibility: crate::thread::Visibility::Local,
            source_thread_id: "t".into(),
            source_item_id: "compaction".into(),
            item: Arc::new(item),
        };
        Arc::new(TimelineEntry {
            id: "compaction".into(),
            created_at: "2026-01-01T00:00:06Z".into(),
            attempt: None,
            kind: EntryKind::Work(Arc::new(entries::projected_work_entry(&row))),
        })
    };
    let mut input = Input::idle();
    input.activity = running("run-1");
    for (reason, live) in [(Some("beforeCacheExpires"), "compaction:active"), (Some("onReturn"), "working:compacting"), (None, "compaction:active")] {
        for entries in [vec![compact_prompt(reason)], vec![compact_prompt(reason), compaction("running")]] {
            assert_eq!(shape(&input.rows(&entries)), [live], "{reason:?}");
        }
        let finishing = input.rows(&[compact_prompt(reason), compaction("completed")]);
        assert_eq!(shape(&finishing), ["compaction:settled"], "{reason:?}");
    }
}

/// Attempts resolve through nodes, so a node that arrives later re-resolves cached entries
/// (`session-logic.ts` reuses entries only while `nodes` is unchanged).
#[test]
fn a_new_node_re_resolves_cached_attempts() {
    let visible = projected(vec![serde_json::json!({
        "id": "work-1", "threadId": "t", "runId": "run-1", "nodeId": "child", "parentItemId": null, "ordinal": 1, "status": "completed",
        "title": null, "startedAt": "2026-01-01T00:00:01Z", "completedAt": null, "updatedAt": "2026-01-01T00:00:01Z",
        "type": "command_execution", "input": "ls",
    })]);
    let attempts: Vec<crate::thread::RunAttempt> =
        serde_json::from_value(serde_json::json!([{ "id": "attempt-1", "runId": "run-1", "rootNodeId": "root", "status": "superseded" }])).expect("attempts");
    let nodes: Vec<crate::thread::Entity> =
        serde_json::from_value(serde_json::json!([{ "id": "child", "rootNodeId": "root", "parentNodeId": "root" }])).expect("nodes");
    let mut cache = EntriesCache::default();
    let before = derive_entries(&mut cache, &visible, &attempts, &[], &[]);
    assert!(before[0].attempt.is_none());
    let after = derive_entries(&mut cache, &visible, &attempts, &nodes, &[]);
    assert_eq!(after[0].attempt.as_ref().map(|a| a.id.as_str()), Some("attempt-1"));
}

#[test]
fn a_streamed_token_replaces_only_its_own_row() {
    let streaming = |text: &str| {
        let entry = message("reply", Role::Assistant, text, Some("run-1"), "2026-01-01T00:00:01Z", "2026-01-01T00:00:02Z");
        let EntryKind::Message(m) = &entry.kind else { unreachable!() };
        let mut m = (**m).clone();
        m.streaming = true;
        Arc::new(TimelineEntry { kind: EntryKind::Message(Arc::new(m)), ..(*entry).clone() })
    };
    let prompt = message("user-1", Role::User, "Go", Some("run-1"), "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z");
    let activity = running("run-1");
    let none = HashSet::new();
    let mut cache = RowsCache::default();
    let mut derive = |entries: &[Arc<TimelineEntry>]| {
        derive_rows(&RowsInput { entries, activity: &activity, expanded_run_ids: &none, expanded_attempt_ids: &none, expanded_work_group_ids: &none, worktree_setup: None }, &mut cache)
    };
    let first = derive(&[prompt.clone(), streaming("Hel")]);
    let second = derive(&[prompt, streaming("Hello")]);
    let changed: Vec<&str> = first.iter().zip(&second).filter(|(a, b)| !Arc::ptr_eq(a, b)).map(|(_, b)| b.id.as_str()).collect();
    assert_eq!(changed, ["reply-entry"]);
}

#[test]
fn withheld_output_is_requested_by_revision_and_decoded() {
    let command = |status: &str| -> crate::turn_item::TurnItem {
        serde_json::from_value(serde_json::json!({
            "id": "cmd", "threadId": "t", "runId": "r", "nodeId": null, "parentItemId": null, "ordinal": 1, "status": status,
            "title": null, "startedAt": null, "completedAt": null, "updatedAt": "2026-01-01T00:00:09Z",
            "type": "command_execution", "input": "ls", "outputOmitted": true,
        }))
        .expect("item decodes")
    };
    assert!(needs_detail_fetch(&command("completed")));
    assert_eq!(detail_request(&command("running"))["revision"], "live");
    assert_eq!(detail_request(&command("completed")), serde_json::json!({ "threadId": "t", "itemId": "cmd", "revision": "2026-01-01T00:00:09Z" }));
    let mut fetched = serde_json::to_value(serde_json::json!({ "item": {
        "id": "cmd", "threadId": "t", "runId": "r", "nodeId": null, "parentItemId": null, "ordinal": 1, "status": "completed",
        "title": null, "startedAt": null, "completedAt": null, "updatedAt": "2026-01-01T00:00:09Z",
        "type": "command_execution", "input": "ls", "output": "a\nb", "exitCode": 0,
    }}))
    .expect("json");
    let item = decode_detail(fetched.take()).expect("decodes").expect("present");
    assert!(matches!(&item.kind, crate::turn_item::TurnItemKind::CommandExecution { output: Some(output), .. } if output == "a\nb"));
    assert!(decode_detail(serde_json::json!({ "item": null })).expect("decodes").is_none());
    assert_eq!(command_output_text(r#"{"stdout":"out","stderr":"","interrupted":false}"#).as_deref(), Some("out"));
}

/// `threadFind.ts` counting and wrapping, and stepping inside the navigation window.
#[test]
fn find_steps_through_the_window_and_wraps() {
    assert_eq!(format_count(0, 0), "0/0");
    assert_eq!(format_count(2, 5), "3/5");
    assert_eq!(format_count(9, 5), "5/5");
    assert_eq!(step_index(4, 5, 1), 0);
    assert_eq!(step_index(0, 5, -1), 4);
    let result = decode_result(serde_json::json!({
        "snapshotSequence": 7, "totalMatches": 5, "activeIndex": 1,
        "match": { "entryId": "m1", "runId": "r1", "occurrence": 0 },
        "navigation": [
            { "entryId": "m0", "runId": null, "startIndex": 0, "count": 1 },
            { "entryId": "m1", "runId": "r1", "startIndex": 1, "count": 3 },
        ],
    }))
    .expect("result decodes");
    assert_eq!(step_locally(&result, 1, 1).map(|(index, m)| (index, m.entry_id, m.occurrence)), Some((2, "m1".into(), 1)));
    assert_eq!(step_locally(&result, 1, -1).map(|(_, m)| m.entry_id), Some("m0".into()));
    assert!(step_locally(&result, 3, 1).is_none(), "match 5 lies outside the window");
    assert_eq!(bar_label("needle", Some(&result), 1, false, false), "2/5");
    assert_eq!(bar_label("needle", None, 0, true, false), "Searching…");
    let start = FindStart { entry_id: "m1".into(), occurrence: 2 };
    assert_eq!(
        search_request("t", " needle ", Some(&start), -1),
        serde_json::json!({ "threadId": "t", "query": "needle", "skills": [], "start": { "entryId": "m1", "occurrence": 2 }, "offset": -1 })
    );
}

/// `timelineTurnFoldRunIdsByEntryId`: find opens the fold hiding its match.
#[test]
fn fold_membership_names_the_fold_hiding_each_entry() {
    let entries = vec![
        message("user-1", Role::User, "Build it", None, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"),
        message("assistant-first", Role::Assistant, "Checklist", Some("turn-1"), "2026-01-01T00:00:05Z", "2026-01-01T00:00:06Z"),
        work("work-entry-1", "turn-1", "2026-01-01T00:00:08Z", |_| {}),
        message("assistant-final", Role::Assistant, "Done", Some("turn-1"), "2026-01-01T00:00:20Z", "2026-01-01T00:00:22Z"),
    ];
    let (runs, attempts) = fold_membership(&entries, &ThreadActivity::default());
    assert_eq!(runs.get("work-entry-1").map(String::as_str), Some("turn-1"));
    assert_eq!(runs.get("assistant-first-entry").map(String::as_str), Some("turn-1"));
    assert!(!runs.contains_key("assistant-final-entry"));
    assert!(attempts.is_empty());
}

fn projection_with(runs: serde_json::Value, items: serde_json::Value, sessions: serde_json::Value) -> crate::thread::ThreadProjection {
    let mut projection = projection_json(serde_json::json!({}), runs, serde_json::json!([]), serde_json::json!([]));
    projection.turn_items = serde_json::from_value::<Vec<crate::turn_item::TurnItem>>(items).expect("items").into_iter().map(Arc::new).collect();
    projection.provider_sessions = serde_json::from_value(sessions).expect("sessions");
    projection
}

fn status_run(id: &str, ordinal: u64, status: &str, started: bool) -> serde_json::Value {
    serde_json::json!({
        "id": id, "ordinal": ordinal, "providerInstanceId": "codex", "modelSelection": { "instanceId": "codex", "model": "m" },
        "userMessageId": "u", "rootNodeId": "root", "status": status, "requestedAt": "2026-01-01T00:00:00Z",
        "startedAt": if started { serde_json::json!("2026-01-01T00:00:00Z") } else { serde_json::Value::Null },
        "completedAt": if started && status != "running" { serde_json::json!("2026-01-01T00:00:05Z") } else { serde_json::Value::Null },
    })
}

fn run_json(status: &str) -> serde_json::Value {
    serde_json::json!([status_run("run-1", 1, status, true)])
}

fn item_json(id: &str, kind: serde_json::Value) -> serde_json::Value {
    let mut item = serde_json::json!({
        "id": id, "threadId": "t", "runId": "run-1", "nodeId": "root", "parentItemId": null, "ordinal": 1, "status": "failed",
        "title": null, "startedAt": "2026-01-01T00:00:03Z", "completedAt": null, "updatedAt": "2026-01-01T00:00:04Z",
    });
    if let (Some(object), Some(extra)) = (item.as_object_mut(), kind.as_object()) {
        object.extend(extra.clone());
    }
    item
}

fn limit_failure(reset_at: &str) -> serde_json::Value {
    item_json("e1", serde_json::json!({
        "type": "error",
        "failure": { "class": "usage_limit", "message": "{\"error\":{\"message\":\"Limit hit\"}}", "code": "subscription_sharing_usage_limit_exceeded", "resetAt": reset_at, "retryable": null },
    }))
}

fn status_shell(extra: serde_json::Value) -> crate::shell::ThreadShell {
    let mut value = serde_json::json!({
        "id": "t", "projectId": "p", "title": "T", "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "m" }, "runtimeMode": "auto", "interactionMode": "default",
        "branch": null, "worktreePath": null, "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t" },
        "latestRunId": "run-1", "latestRunCompletedAt": "2026-01-01T00:00:05Z", "activeRunId": null, "status": "failed",
        "pendingRuntimeRequest": null, "latestVisibleMessage": null, "latestUserMessageAt": null, "hasActionableProposedPlan": false,
        "itemCount": 0, "visibleItemCount": 0, "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:06Z",
        "archivedAt": null, "settledOverride": null, "settledAt": null, "deletedAt": null,
    });
    if let (Some(object), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        object.extend(extra.clone());
    }
    serde_json::from_value(value).expect("shell")
}

/// `threadErrorSummary`: the failed run's root failure, unless the session reports another error.
#[test]
fn the_error_banner_reads_the_failed_runs_root_failure() {
    let failure = limit_failure("2026-01-01T05:00:00Z");
    let error = thread_error(&projection_with(run_json("failed"), serde_json::json!([failure]), serde_json::json!([]))).expect("an error");
    assert!(error.is_usage_limit() && error.chatgpt_limit() && !error.workspace_limit() && error.limit_blocked());
    assert_eq!(error.reset_at.as_deref(), Some("2026-01-01T05:00:00Z"));
    assert_eq!(error_banner_message(&error.message), "Limit hit");
    assert!(error_can_expand(&error.message, &error_banner_message(&error.message)));
    assert!(!error_can_expand("Short", "Short"));
    let session = serde_json::json!([{ "id": "s", "providerInstanceId": "codex", "lastError": "Session crashed" }]);
    let other = thread_error(&projection_with(run_json("failed"), serde_json::json!([failure]), session)).expect("an error");
    assert_eq!((other.message.as_str(), other.class, other.reset_at), ("Session crashed", None, None));
    assert!(thread_error(&projection_with(run_json("completed"), serde_json::json!([failure]), serde_json::json!([]))).is_none());
}

/// `usageLimitRunPresentedAsLatest`: queued runs that never started leave the limited run latest,
/// and the recovery banner takes over from the error banner.
#[test]
fn a_usage_limited_run_stays_latest_behind_unstarted_runs() {
    let runs = serde_json::json!([status_run("run-1", 1, "failed", true), status_run("run-2", 2, "queued", false)]);
    let projection = projection_with(runs, serde_json::json!([limit_failure("2026-01-01T05:00:00Z")]), serde_json::json!([]));
    assert_eq!(presented_latest_run(&projection).map(|run| run.id.as_str()), Some("run-1"));
    let error = thread_error(&projection).expect("an error");
    assert!(error.limit_blocked());
    let shell = status_shell(serde_json::json!({}));
    assert!(timeline_thread_error(Some(error.clone()), Some(&shell)).is_none());
    assert!(timeline_thread_error(Some(error.clone()), Some(&status_shell(serde_json::json!({ "latestRunId": null })))).is_some());

    let now = parse_iso_ms("2026-01-01T01:00:00Z").expect("now");
    let recovery = limit_recovery(Some(&error), &shell, now).expect("recovery");
    assert_eq!(recovery.id(), "usage-limit-recovery:run-1");
    assert!(recovery.can_schedule && !recovery.scheduled && !recovery.snoozed && !recovery.reset_passed);
    assert_eq!(recovery.description(now, &crate::sidebar::time::FixedZone(0), crate::client_settings::TimestampFormat::TwelveHour, &TimestampEnglish { british: false }), "Resets 5:00 AM");
    let resume = recovery.toggle_resume("c", "t").expect("resume");
    assert_eq!(resume["limitRecovery"], serde_json::json!({ "runId": "run-1", "resetAt": "2026-01-01T05:00:00Z", "autoResume": true }));
    assert_eq!(recovery.toggle_snooze("c", "t", now).expect("snooze")["limitRecovery"]["snooze"], true);
    assert_eq!(recovery.toggle_snooze("c", "t", parse_iso_ms("2026-01-01T06:00:00Z").expect("later")), Err(SnoozeRefused));

    let scheduled = status_shell(serde_json::json!({
        "snoozedUntil": "2026-01-01T05:00:00.000Z",
        "limitRecovery": { "runId": "run-1", "resetAt": "2026-01-01T05:00:00Z", "autoResume": true, "snooze": true },
    }));
    let recovery = limit_recovery(Some(&error), &scheduled, now).expect("recovery");
    assert!(recovery.scheduled && recovery.snoozed);
    assert_eq!(recovery.toggle_snooze("c", "t", now).expect("wake")["limitRecovery"]["snooze"], false);
    let after = limit_recovery(Some(&error), &scheduled, parse_iso_ms("2026-01-01T05:00:00Z").expect("reset")).expect("recovery");
    assert!(!after.snoozed && after.reset_passed && after.deadline().is_none());

    let no_reset = thread_error(&projection_with(run_json("failed"), serde_json::json!([item_json("e1", serde_json::json!({
        "type": "error", "failure": { "class": "usage_limit", "message": "Limit", "code": null, "retryable": null },
    }))]), serde_json::json!([]))).expect("an error");
    let recovery = limit_recovery(Some(&no_reset), &shell, now).expect("recovery");
    assert!(!recovery.can_schedule);
    assert_eq!(recovery.description(now, &crate::sidebar::time::FixedZone(0), crate::client_settings::TimestampFormat::TwelveHour, &TimestampEnglish { british: false }), "Reset time unavailable; retry manually");
}

/// `presentRunStop`: Stopping shows from the request until its result; an ended run's Stop only
/// once the provider stops answering, and only while its background work is pending.
#[test]
fn a_stop_shows_until_the_provider_confirms_it() {
    let request = item_json("stop", serde_json::json!({ "type": "run_interrupt_request", "message": "Stop", "status": "completed" }));
    let result = item_json("done", serde_json::json!({ "type": "run_interrupt_result", "message": "Stopped", "status": "completed" }));
    let stop = run_stop(&projection_with(run_json("running"), serde_json::json!([request]), serde_json::json!([])), false).expect("stopping");
    assert_eq!((stop.run_id.as_str(), stop.requested_at.as_str(), stop.title()), ("run-1", "2026-01-01T00:00:03Z", "Stopping"));
    assert!(!stop.unresponsive && !stop.background && stop.description().is_none());
    assert!(run_stop(&projection_with(run_json("running"), serde_json::json!([request, result]), serde_json::json!([])), false).is_none());
    assert!(run_stop(&projection_with(run_json("completed"), serde_json::json!([request]), serde_json::json!([])), true).is_none());

    let unanswered = item_json("stop", serde_json::json!({
        "type": "run_interrupt_request", "message": "Stop", "status": "completed", "unresponsiveAt": "2026-01-01T00:00:13Z",
    }));
    let stop = run_stop(&projection_with(run_json("running"), serde_json::json!([unanswered]), serde_json::json!([])), false).expect("unresponsive");
    assert_eq!(stop.title(), "Agent not responding to Stop");
    let ended = projection_with(run_json("completed"), serde_json::json!([unanswered]), serde_json::json!([]));
    assert!(run_stop(&ended, true).is_some_and(|stop| stop.background));
    assert!(run_stop(&ended, false).is_none());
    assert_eq!(elapsed_since("2026-01-01T00:00:03Z", parse_iso_ms("2026-01-01T00:00:03Z").expect("now")).as_deref(), Some("1s"));
}

/// `presentProviderGoal` as `goalNowItem` titles it.
#[test]
fn goal_lines_match_the_web() {
    let goal = |value: serde_json::Value, working: bool| goal_line(&value, working).expect("a goal");
    let set = goal(serde_json::json!({ "objective": "Ship it", "status": "active" }), false);
    assert_eq!((set.title.as_str(), set.objective.as_str(), set.can_resume), ("Goal set", "Ship it", false));
    assert_eq!(goal(serde_json::json!({ "objective": "Ship it", "status": "active" }), true).title, "Pursuing goal");
    let paused = goal(
        serde_json::json!({ "objective": "Ship it", "status": "paused", "tokensUsed": 12_400, "tokenBudget": 2_500_000, "timeUsedSeconds": 90, "checks": 1 }),
        false,
    );
    assert_eq!(paused.title, "Goal paused · 12k / 2.5m tokens · 1m 30s · 1 check");
    assert!(paused.can_resume);
    assert_eq!(goal(serde_json::json!({ "objective": "x", "status": "budget_limited", "tokensUsed": 999 }), false).title, "Goal reached its token budget · 999 tokens");
    assert!(!goal(serde_json::json!({ "objective": "x", "status": "complete" }), false).can_resume);
}

#[test]
fn upcoming_times_name_the_day() {
    let now = parse_iso_ms("2026-03-10T12:00:00Z").expect("now");
    assert_eq!(format_upcoming("2026-03-10T18:30:00Z", now, &crate::sidebar::time::FixedZone(0), crate::client_settings::TimestampFormat::TwelveHour, &TimestampEnglish { british: false }), "6:30 PM");
    assert_eq!(format_upcoming("2026-03-11T09:05:00Z", now, &crate::sidebar::time::FixedZone(0), crate::client_settings::TimestampFormat::TwelveHour, &TimestampEnglish { british: false }), "tomorrow at 9:05 AM");
    assert_eq!(format_upcoming("2026-03-14T09:05:00Z", now, &crate::sidebar::time::FixedZone(0), crate::client_settings::TimestampFormat::TwelveHour, &TimestampEnglish { british: false }), "3/14 9:05 AM");
    assert_eq!(format_upcoming("2027-01-02T09:05:00Z", now, &crate::sidebar::time::FixedZone(0), crate::client_settings::TimestampFormat::TwelveHour, &TimestampEnglish { british: false }), "1/2/2027 9:05 AM");
    assert_eq!(format_upcoming("2026-03-09T09:05:00Z", now, &crate::sidebar::time::FixedZone(0), crate::client_settings::TimestampFormat::TwelveHour, &TimestampEnglish { british: false }), "yesterday at 9:05 AM");
}

#[test]
fn relative_labels_match_the_web() {
    assert_eq!(relative_time_label(30_000), "just now");
    assert_eq!(relative_time_label(5 * 60_000), "5m ago");
    assert_eq!(relative_time_label(3 * 3_600_000), "3h ago");
    assert_eq!(relative_time_label(49 * 3_600_000), "2d ago");
}

fn checkpoint_file(path: &str, additions: u64, deletions: u64) -> crate::diff::CheckpointFile {
    serde_json::from_value(serde_json::json!({ "path": path, "kind": "modified", "additions": additions, "deletions": deletions })).expect("file")
}

/// `buildTurnDiffTree`: directories before files, numeric name order, merged single-child chains.
#[test]
fn the_changed_files_tree_matches_the_web() {
    let files = [
        checkpoint_file("src/b10.ts", 1, 0),
        checkpoint_file("README.md", 2, 2),
        checkpoint_file("src/B2.ts", 3, 1),
        checkpoint_file("apps/web/src/index.ts", 4, 0),
        checkpoint_file("apps/web/src/lib/x.ts", 1, 1),
    ];
    let tree = changed_file_tree(&files);
    let names: Vec<(&str, &str, DiffStat)> = tree
        .iter()
        .map(|node| match node {
            TreeNode::Directory { name, path, stat, .. } | TreeNode::File { name, path, stat } => (name.as_str(), path.as_str(), *stat),
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("apps/web/src", "apps/web/src", DiffStat { additions: 5, deletions: 1 }),
            ("src", "src", DiffStat { additions: 4, deletions: 1 }),
            ("README.md", "README.md", DiffStat { additions: 2, deletions: 2 }),
        ]
    );
    let TreeNode::Directory { children, .. } = &tree[1] else { panic!("src is a directory") };
    assert_eq!(children.iter().cloned().filter_map(|child| match child { TreeNode::File { name, .. } => Some(name), _ => None }).collect::<Vec<_>>(), ["B2.ts", "b10.ts"]);

    let collapsed = changed_file_rows(&tree, &std::collections::HashMap::new(), false);
    assert_eq!(collapsed.len(), 3);
    let open = changed_file_rows(&tree, &std::collections::HashMap::new(), true);
    assert_eq!(open.iter().map(|row| row.depth).collect::<Vec<_>>(), [0, 1, 2, 1, 0, 1, 1, 0]);
    let overrides = std::collections::HashMap::from([("src".to_owned(), true)]);
    assert_eq!(changed_file_rows(&tree, &overrides, false).len(), 5);
    assert_eq!(directory_paths(&tree).len(), 3);
    assert_eq!((changed_files_label(1), changed_files_label(1_234), hidden_files_label(2)), ("1 changed file".into(), "1,234 changed files".into(), "2 more files in the full diff".into()));
}

/// `deriveThreadCheckpointSummaries`: a turn's summary rides on its run's last assistant message.
#[test]
fn a_turns_changed_files_ride_on_its_last_assistant_message() {
    let mut projection = projection_json(serde_json::json!({}), serde_json::json!([]), serde_json::json!([]), serde_json::json!([]));
    let message = |id: &str, role: &str, run: &str| crate::thread::Entity { id: id.into(), value: serde_json::json!({ "id": id, "role": role, "runId": run }) };
    projection.messages = vec![message("u1", "user", "run-1"), message("a1", "assistant", "run-1"), message("a2", "assistant", "run-1"), message("a3", "assistant", "run-2")];
    let checkpoint = |id: &str, run: &str, ordinal: u64, at: &str| crate::thread::Entity {
        id: id.into(),
        value: serde_json::json!({ "id": id, "runId": run, "appRunOrdinal": ordinal, "status": "ready", "capturedAt": at, "files": [{ "path": "a.ts", "kind": "modified", "additions": ordinal, "deletions": 0 }] }),
    };
    projection.checkpoints = vec![checkpoint("c1", "run-1", 1, "2026-01-01T00:00:01Z"), checkpoint("c3", "run-3", 3, "2026-01-01T00:00:03Z")];
    let diffs = turn_diffs_by_message(&projection);
    assert_eq!(diffs.keys().collect::<Vec<_>>(), ["a2"]);
    assert_eq!(diffs["a2"].run_id, "run-1");
}

/// `canForkProjectedAssistantItem`: completed assistant messages fork unless the provider can't.
#[test]
fn forking_needs_a_completed_assistant_message_and_a_capable_provider() {
    let mut projection = projection_json(serde_json::json!({}), serde_json::json!([]), serde_json::json!([]), serde_json::json!([]));
    let item = |status: &str, provider_thread: Option<&str>| -> crate::turn_item::TurnItem {
        serde_json::from_value(item_json("a1", serde_json::json!({
            "type": "assistant_message", "messageId": "m1", "text": "Done", "streaming": false, "status": status, "providerThreadId": provider_thread,
        })))
        .expect("item")
    };
    assert!(can_fork(&item("completed", None), &fork_support(&projection)));
    assert!(!can_fork(&item("running", None), &fork_support(&projection)));
    projection.provider_threads = vec![crate::thread::Entity { id: "pt".into(), value: serde_json::json!({ "id": "pt", "providerSessionId": "ps" }) }];
    let session = |capabilities: serde_json::Value| vec![crate::thread::Entity { id: "ps".into(), value: serde_json::json!({ "id": "ps", "capabilities": capabilities }) }];
    projection.provider_sessions = session(serde_json::json!({
        "threads": { "canForkThread": true, "canForkFromTurn": true }, "identity": { "nativeThreadIds": "weak" }, "context": { "supportsFullThreadHandoff": false },
    }));
    assert!(!can_fork(&item("completed", Some("pt")), &fork_support(&projection)));
    projection.provider_sessions = session(serde_json::json!({
        "threads": { "canForkThread": true, "canForkFromTurn": true }, "identity": { "nativeThreadIds": "strong" }, "context": { "supportsFullThreadHandoff": false },
    }));
    assert!(can_fork(&item("completed", Some("pt")), &fork_support(&projection)));
    let command = fork_from_run("c", "t", "t2", "run-1", "Refactor");
    assert_eq!((command["type"].as_str(), command["sourcePoint"]["runId"].as_str(), command["title"].as_str()), (Some("thread.fork"), Some("run-1"), Some("Refactor fork")));
}


fn checkpoint(id: &str, run: Option<&str>, ordinal: Option<u64>, within_scope: u64, status: &str) -> crate::thread::Entity {
    crate::thread::Entity {
        id: id.into(),
        value: serde_json::json!({
            "id": id, "scopeId": "scope", "runId": run, "appRunOrdinal": ordinal, "ordinalWithinScope": within_scope,
            "status": status, "files": [], "capturedAt": "2026-01-01T00:00:00Z",
        }),
    }
}

#[test]
fn turn_starting_messages_rewind_to_the_previous_runs_checkpoint() {
    let entries = vec![
        message("u1", Role::User, "one", Some("r1"), "2026-01-01T00:00:01Z", "2026-01-01T00:00:01Z"),
        message("a1", Role::Assistant, "ok", Some("r1"), "2026-01-01T00:00:02Z", "2026-01-01T00:00:02Z"),
        message("u2", Role::User, "two", Some("r2"), "2026-01-01T00:00:03Z", "2026-01-01T00:00:03Z"),
        message("a2", Role::Assistant, "ok", Some("r2"), "2026-01-01T00:00:04Z", "2026-01-01T00:00:04Z"),
        message("u3", Role::User, "three", Some("r3"), "2026-01-01T00:00:05Z", "2026-01-01T00:00:05Z"),
    ];
    let checkpoints = vec![checkpoint("c0", None, None, 0, "ready"), checkpoint("c1", Some("r1"), Some(1), 1, "ready"), checkpoint("c2", Some("r2"), Some(2), 2, "missing")];
    let all: HashSet<u64> = [0, 1, 2].into();
    let none = HashSet::new();
    let input = RevertInput { entries: &entries, checkpoints: &checkpoints, thread_start_ready: true, available: &all, unsettled_run_id: Some("r3"), inherited: &none };
    let counts = revert_turn_counts(&input);
    assert_eq!(counts.get("u1"), Some(&0));
    assert_eq!(counts.get("u2"), Some(&1));
    assert_eq!(counts.get("u3"), None, "the unsettled run cannot rewind");
    let settled = revert_turn_counts(&RevertInput { unsettled_run_id: None, ..input });
    assert_eq!(settled.get("u3"), None, "the run before it has no ready checkpoint");
    let paged = revert_turn_counts(&RevertInput { thread_start_ready: false, unsettled_run_id: None, ..input });
    assert_eq!(paged.get("u1"), None);
    let only_start: HashSet<u64> = [0].into();
    let conversation = revert_turn_counts(&RevertInput { available: &only_start, ..input });
    assert_eq!((conversation.get("u1"), conversation.get("u2")), (Some(&0), None));
}

#[test]
fn finds_the_ready_checkpoint_for_a_turn_count() {
    let checkpoints = vec![
        checkpoint("start", None, None, 0, "ready"),
        checkpoint("tool", None, None, 3, "ready"),
        checkpoint("one-old", Some("r1"), Some(1), 1, "ready"),
        checkpoint("one", Some("r1"), Some(1), 2, "ready"),
        checkpoint("two", Some("r2"), Some(2), 4, "error"),
    ];
    assert_eq!(find_revert_checkpoint(&checkpoints, 0).map(|c| c.id), Some("start".into()));
    assert_eq!(find_revert_checkpoint(&checkpoints, 1), Some(RevertCheckpoint { id: "one".into(), scope_id: "scope".into() }));
    assert_eq!(find_revert_checkpoint(&checkpoints, 2), None);
    let command = rollback_command("cmd", "t", &find_revert_checkpoint(&checkpoints, 1).unwrap(), false);
    assert_eq!(command["type"], "checkpoint.rollback");
    assert_eq!((command["checkpointId"].as_str(), command["scopeId"].as_str(), command["restoreFiles"].as_bool()), (Some("one"), Some("scope"), Some(false)));
}

fn rewind_projection() -> crate::thread::ThreadProjection {
    let runs = serde_json::json!([
        { "id": "r1", "ordinal": 1, "providerInstanceId": "codex", "modelSelection": { "instanceId": "codex", "model": "m" }, "userMessageId": "u1",
          "status": "completed", "requestedAt": "2026-01-01T00:00:00Z", "activeAttemptId": "a1" },
        { "id": "r2", "ordinal": 2, "providerInstanceId": "codex", "modelSelection": { "instanceId": "codex", "model": "m" }, "userMessageId": "u2",
          "status": "completed", "requestedAt": "2026-01-01T00:00:00Z", "activeAttemptId": "a2" },
        { "id": "r3", "ordinal": 3, "providerInstanceId": "codex", "modelSelection": { "instanceId": "codex", "model": "m" }, "userMessageId": "u3",
          "status": "completed", "requestedAt": "2026-01-01T00:00:00Z", "activeAttemptId": "a3" },
    ]);
    let turns = serde_json::json!([
        { "id": "pt1", "runAttemptId": "a1", "ordinal": 1, "providerThreadId": "pth" },
        { "id": "pt2", "runAttemptId": "a2", "ordinal": 2, "providerThreadId": "pth" },
        { "id": "pt3", "runAttemptId": "a3", "ordinal": 3, "providerThreadId": "older" },
    ]);
    let mut projection = projection_json(serde_json::json!({ "activeProviderThreadId": "pth" }), runs, serde_json::json!([]), turns);
    projection.provider_threads = vec![crate::thread::Entity {
        id: "pth".into(),
        value: serde_json::json!({ "id": "pth", "providerSessionId": "s", "providerInstanceId": "codex", "appThreadId": "t", "lastRunOrdinal": 3 }),
    }];
    projection
}

#[test]
fn only_runs_of_the_active_conversation_can_be_rewound_to() {
    let mut projection = rewind_projection();
    let mut available: Vec<u64> = available_revert_turn_counts(&projection).into_iter().collect();
    available.sort();
    assert_eq!(available, [0, 1, 2]);
    projection.provider_threads[0].value["providerInstanceId"] = serde_json::json!("claude");
    assert!(available_revert_turn_counts(&projection).is_empty());
}

#[test]
fn a_rewind_lands_when_the_conversation_and_witness_run_roll_back() {
    let mut projection = rewind_projection();
    let watch = watch_rewind(&projection, 1, "cmd").unwrap();
    assert_eq!(rewind_progress(&watch, &projection), RewindProgress::Pending);
    projection.provider_threads[0].value["lastRunOrdinal"] = serde_json::json!(1);
    assert_eq!(rewind_progress(&watch, &projection), RewindProgress::Pending, "the witness has not rolled back yet");
    projection.runs[2].status = "rolled_back".into();
    assert_eq!(rewind_progress(&watch, &projection), RewindProgress::Done);

    let mut failing = rewind_projection();
    let watch = watch_rewind(&failing, 0, "cmd-2").unwrap();
    failing.thread.rollback_failure = Some(crate::thread::RollbackFailure { request_id: "other".into(), message: "nope".into() });
    assert_eq!(rewind_progress(&watch, &failing), RewindProgress::Pending);
    failing.thread.rollback_failure = Some(crate::thread::RollbackFailure { request_id: "cmd-2".into(), message: "Checkpoint unavailable.".into() });
    assert_eq!(rewind_progress(&watch, &failing), RewindProgress::Failed("Checkpoint unavailable.".into()));

    let mut fresh = rewind_projection();
    let watch = watch_rewind(&fresh, 0, "cmd-3").unwrap();
    fresh.runs.iter_mut().for_each(|run| run.status = "rolled_back".into());
    fresh.provider_threads.push(crate::thread::Entity {
        id: "fresh".into(),
        value: serde_json::json!({ "id": "fresh", "providerInstanceId": "codex", "appThreadId": "t", "ownerNodeId": null, "lastRunOrdinal": null }),
    });
    assert_eq!(rewind_progress(&watch, &fresh), RewindProgress::Done, "Claude starts a fresh root session at the thread start");
    assert_eq!(watch_rewind(&fresh, 0, "cmd-4").err().as_deref(), Some("The thread has changed. Select a message to rewind again."));
}

#[test]
fn recalled_prompts_drop_what_the_composer_cannot_restore() {
    assert_eq!(recallable_prompt("Ultrathink:\nfix it"), "fix it");
    assert_eq!(recallable_prompt("look\n\n<review_comment file=\"a\">x</review_comment>\n<review_comment>y</review_comment>"), "look");
    assert_eq!(
        recallable_prompt("see @server-log:3-5 now\n\n<terminal_context>\n- Server log lines 3-5:\n  boom\n</terminal_context>"),
        "see now"
    );
    assert_eq!(recallable_prompt("keep\n<terminal_context>\nno headers\n</terminal_context>"), "keep\n<terminal_context>\nno headers\n</terminal_context>");
    assert_eq!(recallable_prompt("fix ![shot](t3-context:image/abc) please"), "fix please");
    assert_eq!(recallable_prompt("PLEASE IMPLEMENT THIS PLAN:\n# Plan"), "");
    assert_eq!(merge_restored_prompt("draft", "restored"), "draft\n\nrestored");
    assert_eq!(merge_restored_prompt("", "restored"), "restored");
    assert_eq!(merge_restored_prompt("draft", ""), "draft");
}


fn background_task(task_id: &str, kind: &str, description: Option<&str>, awaited: Option<bool>) -> crate::shell::PendingBackgroundTask {
    crate::shell::PendingBackgroundTask {
        task_id: task_id.into(),
        kind: kind.into(),
        description: description.map(str::to_owned),
        child_thread_id: (kind == "subagent").then(|| format!("child-{task_id}")),
        awaited,
    }
}

#[test]
fn background_work_titles_follow_present_pending_background_work() {
    assert_eq!(background_work(&[]), None);
    let one = background_work(&[background_task("t1", "command", Some("npm run dev"), None)]).unwrap();
    assert_eq!((one.title.as_str(), one.waiting, one.description()), ("Running · npm run dev", false, None));
    assert!(one.items[0].command && one.all_commands());
    let unnamed = background_work(&[background_task("t1", "monitor", None, Some(true))]).unwrap();
    assert_eq!(unnamed.title, "Waiting on a monitor");
    let subagent = background_work(&[background_task("s1", "subagent", Some("Subagent: /root/team/code_reviewer"), None)]).unwrap();
    assert_eq!(subagent.title, "Waiting · Code Reviewer", "subagents hold the turn");
    assert_eq!(subagent.only_child_thread(), Some("child-s1"));
    let many = background_work(&[
        background_task("c1", "command", Some("a"), None),
        background_task("s1", "subagent", Some("Explore"), None),
        background_task("c2", "command", Some("b"), None),
        background_task("x", "background_task", None, None),
    ])
    .unwrap();
    assert_eq!(many.title, "Waiting on 1 subagent, 2 commands and 1 background task");
    assert_eq!(many.description().as_deref(), Some("Explore, a, b, background task"));
    assert_eq!(many.only_child_thread(), None);
}

#[test]
fn subagent_titles_drop_prefix_and_path() {
    assert_eq!(format_subagent_display_title("Subagent: Find the bug"), "Find the bug");
    assert_eq!(format_subagent_display_title("/root/a/b_c d/"), "B C D");
    assert_eq!(format_subagent_display_title("plain"), "plain");
}

#[test]
fn force_stop_notices_name_what_kept_running() {
    assert_eq!(force_stop_notice(&serde_json::json!({ "outcome": "stopped", "sharedWithThreadIds": [] })), None);
    assert_eq!(force_stop_notice(&serde_json::json!({ "outcome": "releasing", "sharedWithThreadIds": [] })).unwrap().0, "Turn stopped; the agent is still shutting down");
    let shared = force_stop_notice(&serde_json::json!({ "outcome": "shared", "sharedWithThreadIds": ["a"] })).unwrap();
    assert_eq!(shared.1.as_deref(), Some("It is shared with 1 working thread. Stop those to free it."));
    assert_eq!(force_stop_request("t", "r"), serde_json::json!({ "threadId": "t", "runId": "r" }));
}

/// An open command row keeps its output subscription only while it is drawn, so folding its
/// group drops it and reopening the group brings it back.
#[test]
fn open_command_items_follow_folded_groups() {
    let command = |id: &str, input: &str| {
        let item: crate::turn_item::TurnItem =
            serde_json::from_value(serde_json::json!({ "id": id, "threadId": "t1", "status": "completed", "type": "command_execution", "input": input })).unwrap();
        Arc::new(item)
    };
    let entries = vec![
        message("user-1", Role::User, "Go", Some("run-1"), "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"),
        work("cmd-1", "run-1", "2026-01-01T00:00:01Z", |w| {
            w.item_type = Some("command_execution");
            w.status = Some(ToolStatus::Completed);
            w.item = Some(command("item-1", "git status"));
        }),
        work("cmd-2", "run-1", "2026-01-01T00:00:02Z", |w| {
            w.item_type = Some("command_execution");
            w.status = Some(ToolStatus::Completed);
            w.item = Some(command("item-2", "vp test run"));
        }),
        work("edit-1", "run-1", "2026-01-01T00:00:03Z", |w| {
            w.item_type = Some("file_change");
            w.changed_files = vec!["src/a.ts".into()];
            w.status = Some(ToolStatus::Completed);
        }),
    ];
    let open: HashSet<String> = ["cmd-2", "edit-1"].map(str::to_owned).into();
    let open_items = |input: &Input| {
        let rows: Vec<Arc<TimelineRow>> = input.rows(&entries).into_iter().map(Arc::new).collect();
        open_command_items(&rows, &open).into_iter().map(|(entry, item)| (entry, item.id.clone())).collect::<Vec<_>>()
    };
    let mut input = Input::idle();
    input.runs.insert("run-1".into());
    input.groups.insert("work-group:cmd-1".into());
    assert_eq!(open_items(&input), [("cmd-2".to_owned(), "item-2".to_owned())], "only open commands, not the open file change");
    input.groups.clear();
    assert_eq!(open_items(&input), [], "a folded group draws none of its entries");
    input.groups.insert("work-group:cmd-1".into());
    assert_eq!(open_items(&input), [("cmd-2".to_owned(), "item-2".to_owned())]);
    input.runs.clear();
    assert_eq!(open_items(&input), [], "a folded turn draws none of its entries");
}

#[test]
fn keeps_the_working_header_in_place_across_worktree_setup_handoff() {
    use crate::worktree_setup::Snapshot;
    let setup = |phase: &str, stages: serde_json::Value| -> Arc<Snapshot> {
        let value = serde_json::json!({
            "threadId": "thread-setup", "phase": phase, "startedAt": "2026-01-01T00:00:00Z", "endedAt": null,
            "branch": "feature", "baseRef": "main", "worktreePath": null, "setupScript": null,
            "stages": stages, "error": null, "sequence": 3,
        });
        Arc::new(crate::worktree_setup::decode_event(value).unwrap().unwrap())
    };
    let stage = |id: &str, status: &str| {
        serde_json::json!({
            "id": id, "status": status, "startedAt": "2026-01-01T00:00:10Z",
            "endedAt": if status == "done" { serde_json::json!("2026-01-01T00:00:11Z") } else { serde_json::Value::Null },
            "percent": null, "detail": null, "tail": [],
        })
    };
    let kinds = |rows: &[TimelineRow]| -> Vec<&'static str> {
        rows.iter()
            .map(|row| match &row.kind {
                RowKind::Message { .. } => "message",
                RowKind::Working { .. } => "working",
                RowKind::Thinking { .. } => "thinking",
                RowKind::WorktreeSetup { embedded: false, .. } => "worktree-setup",
                RowKind::WorktreeSetup { embedded: true, .. } => "worktree-setup:embedded",
                _ => "other",
            })
            .collect()
    };
    let user = message("user-1", Role::User, "Build it", None, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z");
    let mut assistant = message("assistant-1", Role::Assistant, "On it", Some("turn-1"), "2026-01-01T00:00:30Z", "2026-01-01T00:00:30Z");
    if let EntryKind::Message(m) = &mut Arc::make_mut(&mut assistant).kind {
        Arc::make_mut(m).streaming = true;
    }
    let mut working = running("turn-1");
    working.latest_run = None;
    let mut input = Input { activity: working, runs: HashSet::new(), groups: HashSet::new(), setup: Some(setup("running", serde_json::json!([]))) };

    let rows = input.rows(&[]);
    assert_eq!(kinds(&rows), ["working", "worktree-setup"]);
    assert_eq!((rows[0].id.as_str(), rows[0].created_at.as_deref()), ("working-indicator-row", Some("2026-01-01T00:00:00Z")));
    assert_eq!(rows[1].id, "worktree-setup-row");

    input.setup = Some(setup("failed", serde_json::json!([])));
    assert_eq!(kinds(&input.rows(&[user.clone(), assistant])), ["message", "worktree-setup", "working", "message", "thinking"]);

    let handed_off = serde_json::json!([stage("setup-script", "running"), stage("agent", "done")]);
    input.setup = Some(setup("running", handed_off.clone()));
    assert_eq!(kinds(&input.rows(std::slice::from_ref(&user))), ["message", "working", "worktree-setup"]);
    input.setup = Some(setup("done", serde_json::json!([stage("setup-script", "done"), stage("agent", "done")])));
    assert_eq!(kinds(&input.rows(std::slice::from_ref(&user))), ["message", "working", "worktree-setup"]);

    // The turn started: a running setup leaves the card to the working row's chip.
    input.activity.latest_run = running("turn-1").latest_run;
    input.setup = Some(setup("running", handed_off));
    assert_eq!(kinds(&input.rows(std::slice::from_ref(&user))), ["message", "working", "thinking"]);
    input.setup = Some(setup("running", serde_json::json!([stage("setup-script", "done"), stage("agent", "done")])));
    assert_eq!(kinds(&input.rows(std::slice::from_ref(&user))), ["message", "working", "thinking"]);
    // A settled setup the agent took over collapses where it was.
    input.setup = Some(setup("done", serde_json::json!([stage("setup-script", "failed"), stage("agent", "done")])));
    assert_eq!(kinds(&input.rows(std::slice::from_ref(&user))), ["message", "worktree-setup:embedded", "working", "thinking"]);
}

#[test]
fn a_new_setup_snapshot_rederives_rows_a_streamed_token_would_patch() {
    let setup = |sequence: u64| -> Arc<crate::worktree_setup::Snapshot> {
        let value = serde_json::json!({ "threadId": "t", "phase": "running", "startedAt": "2026-01-01T00:00:00Z", "stages": [], "sequence": sequence });
        Arc::new(crate::worktree_setup::decode_event(value).unwrap().unwrap())
    };
    let entries = vec![message("user-1", Role::User, "Go", None, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z")];
    let (none, activity) = (HashSet::new(), ThreadActivity::default());
    let mut cache = RowsCache::default();
    let mut derive = |snapshot: &Arc<crate::worktree_setup::Snapshot>| {
        let input = RowsInput { entries: &entries, activity: &activity, expanded_run_ids: &none, expanded_attempt_ids: &none, expanded_work_group_ids: &none, worktree_setup: Some(snapshot) };
        derive_rows(&input, &mut cache)
    };
    let (first, second) = (setup(1), setup(2));
    let before = derive(&first);
    let after = derive(&second);
    let card = |rows: &[Arc<TimelineRow>]| rows.iter().find_map(|row| match &row.kind {
        RowKind::WorktreeSetup { snapshot, .. } => Some(snapshot.sequence),
        _ => None,
    });
    assert_eq!((card(&before), card(&after)), (Some(1), Some(2)));
    assert!(Arc::ptr_eq(&before[0], &after[0]), "the unchanged message row keeps its identity");
}

/// `deriveTimelineEntriesFromVisibleTurnItems` and `deriveMessagesTimelineRows`: a completed
/// `html_render` result becomes its own frame row, outside the turn's fold; a running or failed
/// one stays a tool row.
#[test]
fn completed_html_renders_draw_as_frames_outside_the_fold() {
    let at = |second: u32| format!("2026-09-25T22:00:{second:02}.000Z");
    let item = |id: &str, ordinal: u64, second: u32, body: serde_json::Value| {
        let mut value = serde_json::json!({
            "id": id, "threadId": "t", "runId": "run-1", "nodeId": null, "providerThreadId": null, "providerTurnId": null,
            "parentItemId": null, "ordinal": ordinal, "status": "completed", "title": null,
            "startedAt": at(second), "completedAt": at(second), "updatedAt": at(second),
        });
        value.as_object_mut().unwrap().extend(body.as_object().unwrap().clone());
        value
    };
    let page = serde_json::json!({ "htmlRender": { "attachmentId": "t-page.html", "title": "Revenue", "height": 360 } });
    let render = |status: &str, output: serde_json::Value| {
        let mut value = item("render", 3, 6, serde_json::json!({ "type": "dynamic_tool", "toolName": "mcp__t3-code__html_render", "input": {}, "output": output, "outputOmitted": true }));
        value["status"] = status.into();
        value
    };
    let rows = |render: serde_json::Value| {
        let visible = projected(vec![
            item("prompt", 1, 0, serde_json::json!({ "type": "user_message", "messageId": "prompt", "text": "Chart it", "attachments": [], "inputIntent": "turn_start" })),
            item("ls", 2, 2, serde_json::json!({ "type": "command_execution", "input": "ls" })),
            render,
            item("answer", 4, 9, serde_json::json!({ "type": "assistant_message", "messageId": "answer", "text": "Done.", "streaming": false })),
        ]);
        Input::idle().rows(&derive_entries(&mut EntriesCache::default(), &visible, &[], &[], &[]))
    };
    let settled = rows(render("completed", page.clone()));
    assert_eq!(shape(&settled), ["user:prompt", "fold:Worked for 9s", "HtmlRender", "assistant:answer"]);
    let RowKind::HtmlRender(frame) = &settled[2].kind else { unreachable!() };
    assert_eq!((settled[2].id.as_str(), frame.run_id.as_deref(), frame.reference.title.as_str()), ("render", Some("run-1"), "Revenue"));
    let error = serde_json::json!({ "isError": true, "htmlRender": page["htmlRender"] });
    for unfinished in [render("running", page.clone()), render("failed", page.clone()), render("completed", error)] {
        assert_eq!(shape(&rows(unfinished)), ["user:prompt", "fold:Worked for 9s", "assistant:answer"]);
    }
}

#[test]
fn mcp_apps_keep_source_identity_and_stay_outside_work_folds() {
    let app = serde_json::json!({"attachmentId":"weather-app","server":"weather","tool":"get_weather","resourceUri":"ui://weather/dashboard"});
    let mut visible = projected(vec![serde_json::json!({
        "id":"app-item","threadId":"conversation","runId":"run-1","nodeId":null,"parentItemId":null,"ordinal":1,"status":"running","title":null,
        "startedAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","completedAt":null,
        "type":"dynamic_tool","toolName":"weather.get_weather","input":{},"outputOmitted":true,"output":{"t3McpApp":app}
    })]);
    visible[0].source_thread_id = "inherited-source".into();
    visible[0].source_item_id = "original-item".into();
    let entries = derive_entries(&mut EntriesCache::default(), &visible, &[], &[], &[]);
    let rows = Input::idle().rows(&entries);
    assert_eq!(shape(&rows), ["McpApp"]);
    let RowKind::McpApp(frame) = &rows[0].kind else { panic!("MCP app row") };
    assert_eq!((&*frame.source_thread_id, &*frame.source_item_id), ("inherited-source", "original-item"));
    assert_eq!(frame.reference.server,"weather");
}

#[test]
fn provider_status_stream_updates_the_banner_while_a_refresh_is_pending() {
    use crate::server_config::{ServerConfig, ServerConfigState, ServerProvider};
    use crate::providers::StatusRefresh;
    use serde_json::json;
    let original = json!({
        "instanceId":"work-codex", "driver":"codex", "displayName":"Work Codex", "enabled":true,
        "installed":true,"status":"error", "auth":{"status":"unauthenticated"}
    });
    let provider: ServerProvider = serde_json::from_value(original.clone()).unwrap();
    let mut config = ServerConfigState::default();
    config.config = Some(ServerConfig { providers: vec![provider.clone()], ..Default::default() });
    let before = provider_banner(&provider).unwrap();
    assert_eq!(before.instance_id, "work-codex");
    assert_eq!(before.refresh_tooltip, "Refresh Work Codex provider status");
    let read = serde_json::from_value(json!({"authenticated":true,"permissions":["orchestration:read"]})).unwrap();
    let mut refresh = StatusRefresh::default();
    refresh.set_target(Some(1), Some("thread-a"), Some("work-codex"));
    let ticket = refresh.start(Some(&read)).unwrap();
    let mut refreshed = original;
    refreshed["status"] = json!("warning");
    refreshed["message"] = json!("New diagnostic from refresh");
    refreshed["models"] = json!([{"slug":"new-model","name":"New model","isCustom":false}]);
    assert!(config.apply(serde_json::from_value(json!({"type":"providerStatuses","payload":{"providers":[refreshed.clone()]}})).unwrap()));
    let current = &config.config.as_ref().unwrap().providers[0];
    assert_eq!(current.models.len(), 1);
    let banner = provider_banner(current).unwrap();
    assert_ne!(before.key, banner.key);
    assert_eq!(banner.message, "New diagnostic from refresh");
    assert!(refresh.finish(&ticket));
    assert_eq!(provider_banner(&config.config.as_ref().unwrap().providers[0]).unwrap(), banner);
    assert_eq!(config.config.as_ref().unwrap().providers[0].models.len(), 1);
    let old = refresh.start(Some(&read)).unwrap();
    refresh.set_target(Some(2), Some("thread-a"), Some("work-codex"));
    let current = refresh.start(Some(&read)).unwrap();
    assert!(!refresh.finish(&old));
    assert!(refresh.pending());
    refreshed["status"] = json!("ready");
    refreshed["auth"] = json!({"status":"authenticated"});
    assert!(config.apply(serde_json::from_value(json!({"type":"providerStatuses","payload":{"providers":[refreshed]}})).unwrap()));
    assert!(provider_banner(&config.config.as_ref().unwrap().providers[0]).is_none());
    assert!(refresh.finish(&current));
    assert!(provider_banner(&config.config.as_ref().unwrap().providers[0]).is_none());

    let baseline = json!({
        "instanceId":"work-codex", "driver":"codex", "displayName":" Work Codex ", "enabled":true,
        "installed":true, "status":"ready", "version":"1.2.3", "auth":{"status":"authenticated"},
        "message":"Inventory diagnostic", "compatibilityAdvisory":{"status":"unsupported","message":"Use 2.0.0."}
    });
    let expected = |title: &str, message: &str, warning, key: [&str; 4]| Some(ProviderBanner {
        can_setup: false, instance_id: key[0].into(), refresh_tooltip: "Refresh Work Codex provider status".into(),
        key: key.join("\u{0}"), title: title.into(), message: message.into(), warning,
    });
    let unsupported = || expected("Work Codex 1.2.3 is unsupported", "Use 2.0.0.", true, ["work-codex", "unsupported", "1.2.3", "Use 2.0.0."]);
    let broken = || expected("Work Codex 1.2.3 is known to be broken", "Use 2.0.0.", false, ["work-codex", "broken", "1.2.3", "Use 2.0.0."]);
    let mut cases = vec![
        ("ready unsupported", json!({}), unsupported()),
        ("ready broken", json!({"compatibilityAdvisory":{"status":"broken","message":"Use 2.0.0."}}), broken()),
        ("unsupported unauthenticated ready", json!({"auth":{"status":"unauthenticated"}}), unsupported()),
        ("disabled broken", json!({"status":"disabled","compatibilityAdvisory":{"status":"broken","message":"Use 2.0.0."}}), None),
        ("disabled unsupported", json!({"status":"disabled"}), None),
        ("absent advisory", json!({"compatibilityAdvisory":null}), None),
        ("absent advisory message", json!({"compatibilityAdvisory":{"status":"unsupported"}}),
            expected("Work Codex 1.2.3 is unsupported", "Inventory diagnostic", true, ["work-codex", "unsupported", "1.2.3", ""])),
        ("empty advisory message is retained", json!({"compatibilityAdvisory":{"status":"broken","message":""}}),
            expected("Work Codex 1.2.3 is known to be broken", "", false, ["work-codex", "broken", "1.2.3", ""])),
        ("null advisory and empty status message", json!({"message":"","compatibilityAdvisory":{"status":"unsupported","message":null}}),
            expected("Work Codex 1.2.3 is unsupported", "No models are available for this provider.", true, ["work-codex", "unsupported", "1.2.3", ""])),
        ("absent messages", json!({"message":null,"compatibilityAdvisory":{"status":"broken"}}),
            expected("Work Codex 1.2.3 is known to be broken", "No models are available for this provider.", false, ["work-codex", "broken", "1.2.3", ""])),
        ("new version reappears", json!({"version":"1.2.4"}),
            expected("Work Codex 1.2.4 is unsupported", "Use 2.0.0.", true, ["work-codex", "unsupported", "1.2.4", "Use 2.0.0."])),
        ("missing version", json!({"version":null}),
            expected("Work Codex  is unsupported", "Use 2.0.0.", true, ["work-codex", "unsupported", "", "Use 2.0.0."])),
        ("new advisory message reappears", json!({"compatibilityAdvisory":{"status":"unsupported","message":"Use 3.0.0."}}),
            expected("Work Codex 1.2.3 is unsupported", "Use 3.0.0.", true, ["work-codex", "unsupported", "1.2.3", "Use 3.0.0."])),
        ("new instance reappears", json!({"instanceId":"home-codex"}),
            expected("Work Codex 1.2.3 is unsupported", "Use 2.0.0.", true, ["home-codex", "unsupported", "1.2.3", "Use 2.0.0."])),
        ("availability auth and diagnostic do not change advisory identity", json!({"status":"warning","auth":{"status":"unknown"},"message":"Changed","compatibilityAdvisory":{"status":"broken","message":"Use 2.0.0."}}), broken()),
        ("unsupported warning uses normal key", json!({"status":"warning"}),
            expected("Work Codex provider status", "Inventory diagnostic", true, ["work-codex", "warning", "authenticated", "Inventory diagnostic"])),
        ("unsupported error uses normal key", json!({"status":"error"}),
            expected("Work Codex provider status", "Inventory diagnostic", false, ["work-codex", "error", "authenticated", "Inventory diagnostic"])),
        ("normal empty message falls back but keeps empty key", json!({"status":"warning","message":"","compatibilityAdvisory":null}),
            expected("Work Codex provider status", "Work Codex provider has limited availability.", true, ["work-codex", "warning", "authenticated", ""])),
        ("normal absent message falls back", json!({"status":"error","message":null,"compatibilityAdvisory":null}),
            expected("Work Codex provider status", "Work Codex provider is unavailable.", false, ["work-codex", "error", "authenticated", ""])),
        ("normal version does not change key", json!({"status":"error","version":"9.0.0","compatibilityAdvisory":null}),
            expected("Work Codex provider status", "Inventory diagnostic", false, ["work-codex", "error", "authenticated", "Inventory diagnostic"])),
        ("normal Antigravity unknown auth suppressed", json!({"driver":"antigravity","status":"warning","auth":{"status":"unknown"},"compatibilityAdvisory":null}), None),
        ("Antigravity unsupported warning suppressed", json!({"driver":"antigravity","status":"warning","auth":{"status":"unknown"}}), None),
        ("Antigravity broken bypasses suppression", json!({"driver":"antigravity","status":"warning","auth":{"status":"unknown"},"compatibilityAdvisory":{"status":"broken","message":"Use 2.0.0."}}), broken().map(|mut banner| {banner.can_setup=true;banner})),
    ];
    for status in ["warning", "unknown", "error"] {
        cases.push((status, json!({"status":status,"auth":{"status":"unknown"},"compatibilityAdvisory":{"status":"broken","message":"Use 2.0.0."}}), broken()));
    }
    for advisory in ["supported", "graceful", "unknown"] {
        cases.push((advisory, json!({"compatibilityAdvisory":{"status":advisory}}), None));
    }
    for advisory in ["broken", "unsupported"] {
        cases.push((advisory, json!({"status":"error","auth":{"status":"unauthenticated"},"compatibilityAdvisory":{"status":advisory,"message":"Use 2.0.0."}}),
            expected("Work Codex is unauthenticated", "Inventory diagnostic", false, ["work-codex", "error", "unauthenticated", "Inventory diagnostic"])));
        cases.push((advisory, json!({"status":"error","auth":{"status":"unauthenticated"},"message":null,"compatibilityAdvisory":{"status":advisory,"message":"Use 2.0.0."}}),
            expected("Work Codex is unauthenticated", "Sign in via the CLI to authenticate again.", false, ["work-codex", "error", "unauthenticated", ""])));
    }
    cases.push((
        "setup preserves diagnostic precedence",
        json!({"status":"error","setup":{"canAuthenticate":true}}),
        expected(
            "Work Codex provider status",
            "Inventory diagnostic",
            false,
            [
                "work-codex",
                "error",
                "authenticated",
                "Inventory diagnostic",
            ],
        )
        .map(|mut banner| {
            banner.can_setup = true;
            banner
        }),
    ));
    cases.push((
        "setup preserves compatibility precedence",
        json!({"setup":{"canInstall":true}}),
        unsupported().map(|mut banner| {
            banner.can_setup = true;
            banner
        }),
    ));
    cases.push((
        "explicit false capabilities",
        json!({"status":"error","setup":{"canAuthenticate":false,"canInstall":false}}),
        expected(
            "Work Codex provider status",
            "Inventory diagnostic",
            false,
            [
                "work-codex",
                "error",
                "authenticated",
                "Inventory diagnostic",
            ],
        ),
    ));
    for setup in [json!({"canAuthenticate":true}), json!({"canInstall":true})] {
        cases.push(("capable sign-in", json!({"status":"error","auth":{"status":"unauthenticated"},"message":null,"compatibilityAdvisory":null,"setup":setup}),
            expected("Work Codex is unauthenticated", "Open provider setup to sign in.", false, ["work-codex", "error", "unauthenticated", ""]).map(|mut banner| {banner.can_setup=true;banner})));
        cases.push(("capable install", json!({"status":"error","installed":false,"message":null,"compatibilityAdvisory":null,"setup":setup}),
            expected("Work Codex provider status", "Open provider setup to install Codex on this environment.", false, ["work-codex", "error", "authenticated", ""]).map(|mut banner| {banner.can_setup=true;banner})));
    }
    let mut failures = Vec::new();
    for (case, patch, expected) in cases {
        let mut value = baseline.clone();
        value.as_object_mut().unwrap().extend(patch.as_object().unwrap().clone());
        let actual = provider_banner(&serde_json::from_value(value).unwrap());
        if actual != expected {
            failures.push(format!("{case}: expected {expected:?}, got {actual:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// `apps/web/src/timestampFormat.test.ts` and the usage-limit timeline browser cases.
fn timestamp_new_york() -> crate::sidebar::time::NamedZone {
    crate::sidebar::time::NamedZone::new("America/New_York").unwrap()
}

struct TimestampEnglish { british: bool }
impl TimestampLocale for TimestampEnglish {
    fn time(&self, local: crate::sidebar::time::LocalTime, format: crate::client_settings::TimestampFormat) -> String {
        use crate::client_settings::TimestampFormat;
        let hour12 = match format {
            TimestampFormat::Locale => !self.british,
            TimestampFormat::TwelveHour => true,
            TimestampFormat::TwentyFourHour => false,
        };
        if hour12 {
            format!("{}:{:02} {}", (local.hour + 11) % 12 + 1, local.minute, if local.hour < 12 { "AM" } else { "PM" })
        } else {
            format!("{}:{:02}", local.hour, local.minute)
        }
    }
    fn numeric_date(&self, local: crate::sidebar::time::LocalTime, include_year: bool) -> String {
        let date = if self.british { format!("{:02}/{:02}", local.day, local.month) } else { format!("{}/{}", local.month, local.day) };
        if include_year { format!("{date}/{}", local.year) } else { date }
    }
}

#[test]
fn explicit_timestamp_zone_picks_relative_calendar_days() {
    use crate::client_settings::TimestampFormat::TwelveHour;
    let now = parse_iso_ms("2026-07-01T23:00:00.000Z").unwrap();
    let locale = TimestampEnglish { british: false };
    assert_eq!(format_upcoming("2026-07-02T00:30:00.000Z", now, &timestamp_new_york(), TwelveHour, &locale), "8:30 PM");
    assert_eq!(format_upcoming("2026-07-02T05:30:00.000Z", now, &timestamp_new_york(), TwelveHour, &locale), "tomorrow at 1:30 AM");
    assert_eq!(format_day_aware("2026-07-01T03:30:00.000Z", now, &timestamp_new_york(), TwelveHour, &locale), "yesterday at 11:30 PM");
    // One render in Madrid must not cache the zone used by the next render in New York.
    let reset = Some("2026-07-01T19:30:00Z");
    let now = parse_iso_ms("2026-07-01T17:00:00Z").unwrap();
    let madrid = crate::sidebar::time::FixedZone(2 * 3600);
    assert_eq!(usage_limit_label(reset, now, &madrid, "Europe/Madrid", TwelveHour, &locale), "Usage limit reached. Retry after 9:30 PM (Europe/Madrid).");
    assert_eq!(usage_limit_label(reset, now, &timestamp_new_york(), "America/New_York", TwelveHour, &locale), "Usage limit reached. Retry after 3:30 PM (America/New_York).");
}

#[test]
fn timestamp_zones_resolve_dst_at_each_instant() {
    use crate::client_settings::TimestampFormat::TwelveHour;
    let locale = TimestampEnglish { british: false };
    for (now, target, upcoming, day_aware) in [
        ("2026-03-08T23:30:00Z", "2026-03-08T04:30:00Z", "yesterday at 11:30 PM", "yesterday at 11:30 PM"),
        ("2026-11-01T03:30:00Z", "2026-11-02T04:30:00Z", "tomorrow at 11:30 PM", "11:30 PM"),
    ] {
        let now = parse_iso_ms(now).unwrap();
        assert_eq!(format_upcoming(target, now, &timestamp_new_york(), TwelveHour, &locale), upcoming);
        assert_eq!(format_day_aware(target, now, &timestamp_new_york(), TwelveHour, &locale), day_aware);
    }
}

#[test]
fn usage_limit_timestamps_honor_format_locale_invalid_and_past_values() {
    use crate::client_settings::TimestampFormat::{Locale, TwelveHour, TwentyFourHour};
    let now = parse_iso_ms("2026-07-01T23:00:00Z").unwrap();
    let american = TimestampEnglish { british: false };
    let british = TimestampEnglish { british: true };
    for (format, locale, clock) in [(TwelveHour, &american, "8:30 PM"), (TwentyFourHour, &american, "20:30"), (Locale, &american, "8:30 PM"), (Locale, &british, "20:30")] {
        assert_eq!(usage_limit_label(Some("2026-07-02T00:30:00Z"), now, &timestamp_new_york(), "America/New_York", format, locale), format!("Usage limit reached. Retry after {clock} (America/New_York)."));
    }
    for (reset, expected) in [
        (None, "Usage limit reached."),
        (Some(""), "Usage limit reached."),
        (Some("invalid"), "Usage limit reached. Retry after  (America/New_York)."),
        (Some("2026-07-03T05:30:00Z"), "Usage limit reached. Retry after 7/3 1:30 AM (America/New_York)."),
        (Some("2027-01-02T06:30:00Z"), "Usage limit reached. Retry after 1/2/2027 1:30 AM (America/New_York)."),
        (Some("2026-07-01T21:00:00Z"), "Usage limit reached. Retry after 5:00 PM (America/New_York)."),
        (Some("2026-07-01T03:30:00Z"), "Usage limit reached. Retry after yesterday at 11:30 PM (America/New_York)."),
        (Some("2025-12-25T06:30:00Z"), "Usage limit reached. Retry after 12/25/2025 1:30 AM (America/New_York)."),
    ] {
        assert_eq!(usage_limit_label(reset, now, &timestamp_new_york(), "America/New_York", TwelveHour, &american), expected);
    }
    assert_eq!(format_upcoming("2026-07-03T05:30:00Z", now, &timestamp_new_york(), Locale, &british), "03/07 1:30");
    assert_eq!(format_day_aware("invalid", now, &timestamp_new_york(), Locale, &british), "");
    assert_eq!(format_upcoming("invalid", now, &timestamp_new_york(), Locale, &british), "");
    let new_year = parse_iso_ms("2027-01-01T02:00:00Z").unwrap();
    assert_eq!(format_upcoming("2027-01-02T03:30:00Z", new_year, &timestamp_new_york(), Locale, &british), "tomorrow at 22:30");
    assert_eq!(format_upcoming("2027-01-03T03:30:00Z", new_year, &timestamp_new_york(), Locale, &british), "02/01/2027 22:30");
}

#[test]
fn named_timestamp_zone_keeps_the_label_and_offsets_together() {
    use crate::client_settings::TimestampFormat::TwelveHour;
    use crate::sidebar::time::NamedZone;
    let zone = NamedZone::new("America/New_York").unwrap();
    let now = parse_iso_ms("2026-07-01T17:00:00Z").unwrap();
    assert_eq!(usage_limit_label(Some("2026-07-01T19:30:00Z"), now, &zone, zone.name(), TwelveHour, &TimestampEnglish { british: false }), "Usage limit reached. Retry after 3:30 PM (America/New_York).");
    let winter = parse_iso_ms("2026-01-01T17:00:00Z").unwrap();
    assert_eq!(format_day_aware("2026-01-01T16:30:00Z", winter, &zone, TwelveHour, &TimestampEnglish { british: false }), "11:30 AM");
    assert!(NamedZone::new("not/a-zone").is_none());
}
