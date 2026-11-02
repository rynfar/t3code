use serde_json::json;

use super::*;
use crate::vcs::{ChangeRequest, SourceControlProvider};

fn status(changes: bool, upstream: bool, ahead: u64, behind: u64) -> VcsStatus {
    let mut status = VcsStatus::default();
    status.local.ref_name = Some("feature".into());
    status.local.has_primary_remote = true;
    status.local.has_working_tree_changes = changes;
    status.remote.has_upstream = upstream;
    status.remote.ahead_count = ahead;
    status.remote.behind_count = behind;
    status.remote.ahead_of_default_count = None;
    status
}

fn open_pr(mut status: VcsStatus) -> VcsStatus {
    status.remote.pr = Some(ChangeRequest {
        number: 1,
        title: "t".into(),
        url: "u".into(),
        base_ref: "main".into(),
        head_ref: "feature".into(),
        state: "open".into(),
        is_draft: None,
    });
    status
}

fn quick(status: &VcsStatus, default_ref: bool) -> (String, QuickActionKind) {
    let action = resolve_quick_action(Some(status), false, default_ref, status.local.has_primary_remote);
    (action.label, action.kind)
}

#[test]
fn terminology_follows_the_provider() {
    assert_eq!(terminology(None).short_label, "PR");
    assert_eq!(terminology(Some("gitlab")), Terminology { short_label: "MR", singular: "merge request" });
    assert_eq!(terminology(Some("azure-devops")).singular, "pull request");
    assert_eq!(terminology(Some("something")).short_label, "change request");
}

#[test]
fn quick_action_for_a_dirty_tree() {
    assert_eq!(quick(&status(true, true, 0, 0), false), ("Commit, push & PR".into(), QuickActionKind::Run(StackedAction::CommitPushPr)));
    assert_eq!(quick(&status(true, true, 0, 0), true), ("Commit & push".into(), QuickActionKind::Run(StackedAction::CommitPush)));
    assert_eq!(quick(&open_pr(status(true, true, 0, 0)), false).1, QuickActionKind::Run(StackedAction::CommitPush));
    assert_eq!(quick(&status(true, true, 0, 2), false), ("Commit".into(), QuickActionKind::Run(StackedAction::Commit)));
    let mut no_remote = status(true, false, 0, 0);
    no_remote.local.has_primary_remote = false;
    assert_eq!(quick(&no_remote, false).1, QuickActionKind::Run(StackedAction::Commit));
}

#[test]
fn quick_action_for_a_clean_tree() {
    assert_eq!(quick(&status(false, true, 2, 0), false), ("Push & create PR".into(), QuickActionKind::Run(StackedAction::CreatePr)));
    assert_eq!(quick(&status(false, true, 2, 0), true), ("Push".into(), QuickActionKind::Run(StackedAction::CommitPush)));
    assert_eq!(quick(&open_pr(status(false, true, 2, 0)), false), ("Push".into(), QuickActionKind::Run(StackedAction::Push)));
    assert_eq!(quick(&status(false, true, 0, 3), false), ("Pull".into(), QuickActionKind::Pull));
    assert_eq!(quick(&status(false, true, 1, 3), false), ("Sync ref".into(), QuickActionKind::Hint));
    assert_eq!(quick(&status(false, false, 0, 0), false), ("Push".into(), QuickActionKind::Hint));
    assert_eq!(quick(&status(false, false, 1, 0), false).1, QuickActionKind::Run(StackedAction::CreatePr));
    let mut unpublished = status(false, false, 0, 0);
    unpublished.local.has_primary_remote = false;
    assert_eq!(quick(&unpublished, false), ("Publish repository".into(), QuickActionKind::Publish));
    let up_to_date = resolve_quick_action(Some(&status(false, true, 0, 0)), false, false, true);
    assert!(up_to_date.disabled);
    assert_eq!(up_to_date.disabled_reason().as_deref(), Some("Branch is up to date. No action needed."));
    let mut merged_ahead = status(false, true, 0, 0);
    merged_ahead.remote.ahead_of_default_count = Some(4);
    assert_eq!(quick(&merged_ahead, false), ("Create PR".into(), QuickActionKind::Run(StackedAction::CreatePr)));
    assert_eq!(quick(&merged_ahead, true).1, QuickActionKind::Hint);
}

#[test]
fn quick_action_without_status_branch_or_while_busy() {
    let busy = resolve_quick_action(Some(&status(true, true, 0, 0)), true, false, true);
    assert_eq!(busy.hint.as_deref(), Some("Git action in progress."));
    assert_eq!(resolve_quick_action(None, false, false, true).hint.as_deref(), Some("Git status is unavailable."));
    let mut detached = status(true, true, 0, 0);
    detached.local.ref_name = None;
    assert_eq!(resolve_quick_action(Some(&detached), false, false, true).hint.as_deref(), Some("Create and checkout a ref before pushing or opening a pull request."));
}

#[test]
fn quick_action_icons() {
    let icon = |label: &str, kind| QuickAction { label: label.into(), disabled: false, kind, hint: None }.icon();
    assert_eq!(icon("x", QuickActionKind::Run(StackedAction::Commit)), QuickActionIcon::Commit);
    assert_eq!(icon("x", QuickActionKind::Run(StackedAction::CommitPush)), QuickActionIcon::Push);
    assert_eq!(icon("x", QuickActionKind::Run(StackedAction::CreatePr)), QuickActionIcon::ChangeRequest);
    assert_eq!(icon("x", QuickActionKind::Pull), QuickActionIcon::Pull);
    assert_eq!(icon("Push", QuickActionKind::Hint), QuickActionIcon::Push);
    assert_eq!(icon("Sync ref", QuickActionKind::Hint), QuickActionIcon::Info);
}

#[test]
fn menu_items_and_their_disabled_reasons() {
    let clean = status(false, true, 0, 0);
    let items = build_menu_items(Some(&clean), false, true);
    assert_eq!(items.iter().map(|item| item.label.as_str()).collect::<Vec<_>>(), ["Commit", "Push", "Create PR"]);
    assert!(items.iter().all(|item| item.disabled));
    let reasons: Vec<_> = items.iter().map(|item| menu_disabled_reason(item, Some(&clean), false, true)).collect();
    assert_eq!(reasons[0].as_deref(), Some("Worktree is clean. Make changes before committing."));
    assert_eq!(reasons[1].as_deref(), Some("No local commits to push."));
    assert_eq!(reasons[2].as_deref(), Some("No local commits to include in a pull request."));

    let ahead = status(false, true, 2, 0);
    let items = build_menu_items(Some(&ahead), false, true);
    assert!(!items[1].disabled && !items[2].disabled);
    assert_eq!(menu_disabled_reason(&items[1], Some(&ahead), false, true), None);

    assert_eq!(build_menu_items(Some(&open_pr(ahead.clone())), false, true).len(), 2, "an open PR drops Create PR");
    assert_eq!(build_menu_items(Some(&ahead), false, false).len(), 1, "no remote leaves only Commit");
    assert!(build_menu_items(None, false, true).is_empty());

    let dirty_ahead = status(true, true, 2, 0);
    let items = build_menu_items(Some(&dirty_ahead), true, true);
    assert_eq!(menu_disabled_reason(&items[0], Some(&dirty_ahead), true, true).as_deref(), Some("Git action in progress."));
}

#[test]
fn push_reasons_name_what_blocks_it() {
    let mut item = MenuItem { id: MenuItemId::Push, label: "Push".into(), disabled: true };
    let reason = |status: &VcsStatus, item: &MenuItem| menu_disabled_reason(item, Some(status), false, true).unwrap_or_default();
    assert_eq!(reason(&status(true, true, 1, 0), &item), "Commit or stash local changes before pushing.");
    assert_eq!(reason(&status(false, true, 1, 1), &item), "Branch is behind upstream. Pull/rebase before pushing.");
    let mut detached = status(false, true, 1, 0);
    detached.local.ref_name = None;
    assert_eq!(reason(&detached, &item), "Detached HEAD: check out a branch before pushing.");
    item.id = MenuItemId::Pr;
    let mut gitlab = status(false, true, 0, 0);
    gitlab.local.source_control_provider = Some(SourceControlProvider { kind: "gitlab".into(), name: "GitLab".into() });
    assert_eq!(reason(&gitlab, &item), "No local commits to include in a merge request.");
}

#[test]
fn default_branch_confirmation_and_copy() {
    assert!(!requires_default_branch_confirmation(StackedAction::Commit, true));
    assert!(requires_default_branch_confirmation(StackedAction::CommitPushPr, true));
    assert!(!requires_default_branch_confirmation(StackedAction::Push, false));
    assert!(includes_commit(StackedAction::Commit, false, false));
    assert!(!includes_commit(StackedAction::CommitPush, false, false));
    assert!(includes_commit(StackedAction::CommitPush, false, true));
    assert!(!includes_commit(StackedAction::Push, true, true));

    let terms = terminology(None);
    let copy = default_branch_dialog_copy(StackedAction::CommitPush, "main", true, terms);
    assert_eq!(copy.title, "Commit & push to default ref?");
    assert_eq!(copy.continue_label, "Commit & push to main");
    assert_eq!(
        copy.description,
        "This action will commit and push changes on \"main\". You can continue on this ref or create a feature ref and run the same action there."
    );
    let copy = default_branch_dialog_copy(StackedAction::Push, "main", false, terms);
    assert_eq!((copy.title.as_str(), copy.continue_label.as_str()), ("Push to default ref?", "Push to main"));
    let copy = default_branch_dialog_copy(StackedAction::CreatePr, "main", false, terms);
    assert_eq!(copy.title, "Push & create PR from default ref?");
    assert!(copy.description.starts_with("This action will push local commits and create a pull request on \"main\"."));
    let copy = default_branch_dialog_copy(StackedAction::CommitPushPr, "trunk", true, terminology(Some("gitlab")));
    assert_eq!(copy.continue_label, "Commit, push & create MR");
    assert_eq!(feature_branch_unavailable_reason("main", true), None);
    assert!(feature_branch_unavailable_reason("main", false).is_some_and(|reason| reason.ends_with("continue on main.")));
}

#[test]
fn quiet_rows_for_threads_without_runs() {
    let clean = status(false, true, 0, 0);
    let quick = resolve_quick_action(Some(&clean), false, false, true);
    assert!(quiet_empty_thread(true, false, false, &quick, Some(&clean)));
    assert!(!quiet_empty_thread(false, false, false, &quick, Some(&clean)));
    let mut dirty = clean.clone();
    dirty.local.working_tree.insertions = 1;
    assert!(!quiet_empty_thread(true, false, false, &quick, Some(&dirty)));
}

#[test]
fn run_payload_omits_unset_options() {
    let payload = run_stacked_action("a", "/r", StackedAction::CommitPush, &RunRequest::default());
    assert_eq!(payload, json!({ "actionId": "a", "cwd": "/r", "action": "commit_push" }));
    let files = vec!["a.rs".to_owned()];
    let request = RunRequest { commit_message: Some("  fix  "), feature_branch: true, file_paths: Some(&files), thread_id: Some("t") };
    let payload = run_stacked_action("a", "/r", StackedAction::Commit, &request);
    assert_eq!(payload["commitMessage"], "fix");
    assert_eq!(payload["featureBranch"], true);
    assert_eq!(payload["filePaths"], json!(["a.rs"]));
    assert_eq!(payload["threadId"], "t");
    let empty = run_stacked_action("a", "/r", StackedAction::Commit, &RunRequest { commit_message: Some("  "), ..RunRequest::default() });
    assert!(empty.get("commitMessage").is_none());
}

fn progress(value: serde_json::Value) -> ProgressEvent {
    serde_json::from_value(value).expect("decodes")
}

#[test]
fn progress_tracks_phases_and_hooks() {
    let mut state = ActionProgress::begin(Operation::RunChangeRequest, "a1", 1_000);
    assert_eq!(state.presentation().status, "Starting source control action...");
    state.apply(&progress(json!({ "kind": "action_started", "actionId": "a1", "cwd": "/r", "action": "commit", "phases": ["commit"] })), 1_500);
    state.apply(&progress(json!({ "kind": "phase_started", "actionId": "a1", "cwd": "/r", "action": "commit", "phase": "commit", "label": "Committing..." })), 2_000);
    assert_eq!(state.presentation(), ProgressPresentation { status: "Committing...".into(), output: None, started_at_ms: 2_000 });
    state.apply(&progress(json!({ "kind": "hook_started", "actionId": "a1", "cwd": "/r", "action": "commit", "hookName": "pre-commit" })), 3_000);
    state.apply(&progress(json!({ "kind": "hook_output", "actionId": "a1", "cwd": "/r", "action": "commit", "hookName": "pre-commit", "stream": "stdout", "text": " lint ok " })), 3_100);
    assert_eq!(state.presentation(), ProgressPresentation { status: "Running pre-commit...".into(), output: Some("lint ok".into()), started_at_ms: 3_000 });
    state.apply(&progress(json!({ "kind": "hook_output", "actionId": "other", "cwd": "/r", "action": "commit", "hookName": null, "stream": "stdout", "text": "ignored" })), 3_200);
    assert_eq!(state.last_output_line.as_deref(), Some(" lint ok "));
    state.apply(&progress(json!({ "kind": "hook_finished", "actionId": "a1", "cwd": "/r", "action": "commit", "hookName": "pre-commit", "exitCode": 0, "durationMs": 100 })), 3_300);
    assert_eq!(state.presentation(), ProgressPresentation { status: "Committing...".into(), output: None, started_at_ms: 2_000 });
}

#[test]
fn pull_progress_never_shows_output() {
    let mut state = ActionProgress::begin(Operation::Pull, "p", 0);
    state.last_output_line = Some("x".into());
    assert_eq!(state.presentation(), ProgressPresentation { status: "Pulling latest changes...".into(), output: None, started_at_ms: 0 });
}

#[test]
fn terminal_events_carry_the_result_or_only_the_phase() {
    let finished = progress(json!({
        "kind": "action_finished", "actionId": "a", "cwd": "/r", "action": "commit_push_pr",
        "result": {
            "action": "commit_push_pr",
            "branch": { "status": "created", "name": "feature/x" },
            "commit": { "status": "created" }, "push": { "status": "pushed" },
            "pr": { "status": "created", "url": "https://x/1", "number": 1 },
            "toast": { "title": "Pushed", "description": "to origin", "cta": { "kind": "none" } }
        }
    }));
    let ProgressEvent::ActionFinished { result, .. } = finished else { panic!("finished") };
    assert_eq!(result.summary(terminology(None)), ("Created PR".into(), None));
    assert_eq!(result.created_branch(), Some("feature/x"));
    let plain = RunResult { toast: ResultToast { title: "Committed".into(), description: Some("abc".into()) }, ..RunResult::default() };
    assert_eq!(plain.summary(terminology(None)), ("Committed".into(), Some("abc".into())));
    assert_eq!(plain.created_branch(), None);

    let failed = progress(json!({ "kind": "action_failed", "actionId": "a", "cwd": "/r", "action": "push", "phase": "push", "message": "secret stderr" }));
    let ProgressEvent::ActionFailed { phase, .. } = failed else { panic!("failed") };
    assert_eq!(remote_failure_message(StackedAction::Push, phase.as_deref()), "Source control action 'push' failed during push.");
    assert_eq!(remote_failure_message(StackedAction::Commit, None), "Source control action 'commit' failed during execution.");
    assert_eq!(progress(json!({ "kind": "brand_new", "actionId": "a" })), ProgressEvent::Unknown);
}

#[test]
fn elapsed_time_switches_to_minutes() {
    assert_eq!(format_elapsed(0, 59_999), "59s");
    assert_eq!(format_elapsed(0, 61_000), "1m 1s");
    assert_eq!(format_elapsed(5_000, 0), "0s");
}
