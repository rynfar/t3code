use std::sync::Arc;

use serde_json::json;

use super::*;

fn stage(id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "status": status,
        "startedAt": if status == "pending" { Value::Null } else { json!("2026-10-26T12:00:00.000Z") },
        "endedAt": if matches!(status, "done" | "failed" | "skipped") { json!("2026-10-26T12:00:03.000Z") } else { Value::Null },
        "percent": null,
        "detail": null,
        "tail": [],
    })
}

fn snapshot(phase: &str, stages: Vec<Value>, sequence: u64) -> Arc<Snapshot> {
    let value = json!({
        "threadId": "thread-1",
        "phase": phase,
        "startedAt": "2026-10-26T12:00:00.000Z",
        "endedAt": if phase == "running" { Value::Null } else { json!("2026-10-26T12:00:05.000Z") },
        "branch": "t3/feature",
        "baseRef": "main",
        "worktreePath": "/repo/.t3/worktrees/feature",
        "setupScript": null,
        "stages": stages,
        "error": null,
        "sequence": sequence,
    });
    Arc::new(decode_event(value).expect("decodes").expect("a snapshot"))
}

#[test]
fn unknown_stage_ids_phases_and_statuses_never_fail_a_frame() {
    let value = json!({
        "threadId": "thread-1",
        "phase": "paused",
        "startedAt": "2026-10-26T12:00:00.000Z",
        "endedAt": null,
        "branch": null,
        "baseRef": null,
        "worktreePath": null,
        "setupScript": { "name": "Install" },
        "stages": [stage("lfs", "running"), stage("fetch", "queued"), { "status": "done" }, stage("checkout", "running")],
        "error": null,
        "sequence": 4,
        "futureField": { "nested": true },
    });
    let decoded = decode_event(value).expect("decodes").expect("a snapshot");
    assert_eq!(decoded.phase, Phase::Unknown);
    assert_eq!(decoded.setup_script, None);
    let stages: Vec<(StageId, StageStatus)> = decoded.stages.iter().map(|stage| (stage.id, stage.status)).collect();
    assert_eq!(stages, [(StageId::Fetch, StageStatus::Unknown), (StageId::Checkout, StageStatus::Running)]);
    // A phase this client does not know is not shown, even while it would be running.
    assert_eq!(resolve_visible(Some(&Arc::new(decoded)), None, false, false), None);
    assert_eq!(decode_event(Value::Null).expect("null decodes"), None);
}

#[test]
fn stage_labels_name_the_setup_script() {
    let labels: Vec<&str> = [StageId::Fetch, StageId::Checkout, StageId::Submodules, StageId::SetupScript, StageId::Agent].into_iter().map(stage_label).collect();
    assert_eq!(labels, ["Fetch base branch", "Check out files", "Init submodules", "Run setup script", "Start agent"]);
    let setup = snapshot("running", vec![stage("setup-script", "running"), stage("agent", "pending")], 1);
    assert_eq!(stage_display_label(&setup.stages[0], Some("Install deps")), "Install deps");
    assert_eq!(stage_display_label(&setup.stages[1], Some("Install deps")), "Start agent");
}

#[test]
fn resolve_visible_keeps_running_and_failed_setups_until_a_follow_up() {
    let running = snapshot("running", vec![stage("checkout", "running")], 1);
    let clean = snapshot("done", vec![stage("checkout", "done"), stage("agent", "done")], 2);
    let script_failed = snapshot("done", vec![stage("setup-script", "failed"), stage("agent", "done")], 3);
    let failed = snapshot("failed", vec![stage("checkout", "failed")], 3);
    assert_eq!(resolve_visible(Some(&running), None, true, true), Some(running.clone()));
    assert_eq!(resolve_visible(Some(&clean), None, false, false), Some(clean.clone()));
    assert_eq!(resolve_visible(Some(&clean), None, true, false), None);
    assert_eq!(resolve_visible(Some(&script_failed), None, true, false), Some(script_failed.clone()));
    assert_eq!(resolve_visible(Some(&failed), None, true, false), Some(failed.clone()));
    assert_eq!(resolve_visible(Some(&failed), None, true, true), None);
    // The newer of the live and recorded snapshot wins.
    assert_eq!(resolve_visible(Some(&running), Some(&clean), false, false), Some(clean.clone()));
    assert_eq!(resolve_visible(None, Some(&running), false, false), Some(running));
}

#[test]
fn the_newest_snapshot_wins_and_the_last_one_is_held_after_the_server_drops_it() {
    let base = snapshot("running", vec![stage("checkout", "running"), stage("agent", "pending")], 1);
    let newest = snapshot("done", vec![stage("checkout", "done"), stage("setup-script", "done"), stage("agent", "done")], 9);
    let resolve = |latest: Option<&Arc<Snapshot>>, held: Option<&Arc<Snapshot>>| {
        progress("thread-1", false, Some("running"), &Feed { latest: latest.cloned(), held: held.cloned() })
    };
    assert_eq!(resolve(Some(&newest), Some(&base)), Progress { snapshot: Some(newest.clone()), preparing: false });
    assert_eq!(resolve(Some(&base), Some(&newest)), Progress { snapshot: Some(newest.clone()), preparing: false });
    let mut other = (*base).clone();
    other.thread_id = "another-thread".into();
    let other = Arc::new(other);
    assert_eq!(resolve(Some(&other), Some(&other)), Progress::default());

    let mut feed = Feed::default();
    feed.apply(Some(base.clone()));
    feed.apply(Some(newest.clone()));
    feed.apply(None);
    assert_eq!(feed, Feed { latest: None, held: Some(newest.clone()) });
    // An older snapshot never replaces a newer held one.
    feed.apply(Some(base.clone()));
    assert_eq!(feed.held, Some(newest));
    assert!(progress("thread-1", false, None, &Feed { latest: Some(base), held: None }).preparing);
    assert!(progress("thread-1", false, Some("preparing"), &Feed::default()).preparing);
}

#[test]
fn setup_blocks_send_until_the_agent_takes_over_unless_the_send_queues() {
    let running = snapshot("running", vec![stage("checkout", "running"), stage("agent", "pending")], 1);
    let handed_off = snapshot("running", vec![stage("setup-script", "running"), stage("agent", "done")], 2);
    assert!(blocks_send(Some(&running), true, false));
    assert!(!blocks_send(Some(&running), true, send_queues_behind_setup(Some("preparing"))));
    assert!(!blocks_send(Some(&handed_off), false, false));
    assert!(blocks_send(None, true, false));
    assert!(!blocks_send(None, false, false));
    assert!(agent_started(&handed_off) && !agent_started(&running));
    assert_eq!(background(Some(&handed_off), true), Some(handed_off.clone()));
    assert_eq!(background(Some(&handed_off), false), None);
    assert_eq!(background(Some(&running), true), None);
}

#[test]
fn a_running_stage_counts_while_finished_ones_keep_their_duration() {
    let running = snapshot("running", vec![stage("fetch", "done"), stage("checkout", "running"), stage("agent", "pending")], 1);
    assert_eq!(stage_elapsed(&running.stages[0], &running), Some(Elapsed::Fixed("3s".into())));
    assert_eq!(stage_elapsed(&running.stages[1], &running), Some(Elapsed::Live("2026-10-26T12:00:00.000Z".into())));
    assert_eq!(stage_elapsed(&running.stages[2], &running), None);
    let now = parse_iso_ms("2026-10-26T12:00:07.400Z").expect("parses");
    assert_eq!(live_label("2026-10-26T12:00:00.000Z", now).as_deref(), Some("7s"));
    // A stage interrupted by the setup ending stops at the setup's end.
    let failed = snapshot("failed", vec![stage("fetch", "done"), stage("checkout", "running")], 2);
    assert_eq!(stage_elapsed(&failed.stages[1], &failed), Some(Elapsed::Fixed("5s".into())));
}

#[test]
fn the_card_layout_follows_the_phase() {
    let mut script = stage("setup-script", "running");
    script["tail"] = json!(["installing", "linking"]);
    let running = snapshot("running", vec![stage("checkout", "done"), script], 1);
    let shown = layout(&running, false);
    assert_eq!((shown.show_header, shown.collapsed, shown.show_tail, shown.script_started), (false, false, true, true));
    assert_eq!(tail_rows(&running.stages[1].tail), ["", "", "installing", "linking"]);
    let lines: Vec<String> = (1..=6).map(|n| format!("line {n}")).collect();
    assert_eq!(tail_rows(&lines), ["line 3", "line 4", "line 5", "line 6"]);

    let mut failed = (*snapshot("failed", vec![stage("checkout", "failed"), stage("setup-script", "pending")], 2)).clone();
    failed.error = Some("git worktree add failed".into());
    let shown = layout(&failed, false);
    assert_eq!((shown.show_header, shown.show_tail, shown.script_started, shown.show_error), (true, false, false, true));
    assert_eq!((header_label(&failed), header_tone(&failed)), ("Worktree setup failed", Tone::Destructive));
    assert!(layout(&failed, true).collapsed);

    let script_failed = snapshot("done", vec![stage("setup-script", "failed"), stage("agent", "done")], 3);
    assert_eq!((header_label(&script_failed), header_tone(&script_failed)), ("Worktree ready, setup script failed", Tone::Warning));
    assert!(!layout(&script_failed, false).show_header);
    assert_eq!(summary_status(&script_failed), StageStatus::Failed);
    let cancelled = snapshot("cancelled", vec![stage("checkout", "skipped")], 4);
    assert_eq!((header_label(&cancelled), summary_status(&cancelled)), ("Worktree setup cancelled", StageStatus::Failed));
    assert_eq!(stage_trailing(&cancelled.stages[0]).as_deref(), Some("skipped"));
}

#[test]
fn checkout_shows_its_percent_while_running() {
    let mut checkout = stage("checkout", "running");
    checkout["percent"] = json!(42);
    checkout["detail"] = json!("Updating files");
    let running = snapshot("running", vec![checkout, stage("fetch", "pending")], 1);
    assert_eq!(stage_trailing(&running.stages[0]).as_deref(), Some("42%"));
    assert_eq!(stage_trailing(&running.stages[1]), None);
    assert_eq!(details(&running), [("Branch", "t3/feature"), ("Base", "main"), ("Path", "/repo/.t3/worktrees/feature")]);
}

#[test]
fn work_locally_relaunches_the_message_in_the_project_checkout() {
    let launch = json!({
        "commandId": "c1",
        "threadId": "t1",
        "projectId": "p1",
        "workspaceStrategy": { "type": "worktree", "baseRef": "main" },
        "initialMessage": { "messageId": "m1", "text": "Build it", "attachments": [] },
    });
    let relaunch = local_relaunch(&launch, "c2", "t2", "m2");
    assert_eq!(
        relaunch,
        json!({
            "commandId": "c2",
            "threadId": "t2",
            "projectId": "p1",
            "workspaceStrategy": { "type": "root" },
            "initialMessage": { "messageId": "m2", "text": "Build it", "attachments": [] },
        })
    );
    assert!(cancelled(&json!({ "cancelled": true })));
    assert!(!cancelled(&json!({ "cancelled": false })) && !cancelled(&json!(null)));
}
