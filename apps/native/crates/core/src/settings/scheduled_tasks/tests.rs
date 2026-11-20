use serde_json::json;

use super::*;
use crate::shell::Lineage;

fn task(schedule: Value) -> Value {
    json!({
        "id": "task-1", "title": "Nightly", "prompt": "Check Sentry", "enabled": true, "schedule": schedule,
        "projectId": "project-1", "threadId": null,
        "workspaceStrategy": { "type": "worktree", "baseRef": "develop", "startFromOrigin": false, "branch": "t3/x" },
        "modelSelection": { "instanceId": "codex", "model": "gpt-5", "options": [{ "id": "reasoningEffort", "value": "high" }] },
        "runtimeMode": "auto-accept-edits", "interactionMode": "default", "createdBy": "user", "creationSource": "web",
        "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-01T00:00:00.000Z",
        "nextRunAt": "2026-11-02T09:00:00.000Z", "lastRunAt": null, "lastRunStatus": "failed", "lastRunError": "boom", "runCount": 3,
    })
}

const NOW: i64 = 1_793_000_000_000;

#[test]
fn the_list_replaces_itself_and_skips_tasks_it_cannot_read() {
    let list = decode_list_with_timezone(&json!({
        "timeZone": "America/Los_Angeles",
        "tasks": [
            task(json!({ "type": "fixed_time", "timeOfDay": "9:00", "weekdays": [1, 2, 3, 4, 5] })),
            { "id": "broken" },
            task(json!({ "type": "someday" })),
        ]
    }))
    .unwrap();
    assert_eq!(list.time_zone.as_deref(), Some("America/Los_Angeles"));
    let tasks = list.tasks;
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].schedule, Schedule::FixedTime { time_of_day: "9:00".into(), weekdays: vec![1, 2, 3, 4, 5] });
    assert_eq!(tasks[0].last_run_status.badge(), Some("failed"));
    assert_eq!(tasks[0].last_run_error.as_deref(), Some("boom"));
    assert_eq!(tasks[1].schedule, Schedule::Unknown);
    assert_eq!(decode_list(&json!({})), None);
}

#[test]
fn schedule_and_state_labels_match_the_web() {
    let once = |_: i64| "Nov 2, 2026, 9:00 AM".to_owned();
    let label = |schedule: Value| Schedule::decode(&schedule).label(once);
    assert_eq!(label(json!({ "type": "webhook", "signature": null })), "On webhook");
    assert_eq!(label(json!({ "type": "interval", "everyMs": 900_000 })), "Every 15 min");
    assert_eq!(label(json!({ "type": "interval", "everyMs": 90_500 })), "Every 91 sec");
    assert_eq!(label(json!({ "type": "fixed_time", "timeOfDay": "09:00" })), "Daily at 09:00");
    assert_eq!(label(json!({ "type": "fixed_time", "timeOfDay": "09:00", "weekdays": [1, 2, 3, 4, 5] })), "Weekdays at 09:00");
    assert_eq!(label(json!({ "type": "fixed_time", "timeOfDay": "18:30", "weekdays": [0, 6] })), "Sun, Sat at 18:30");
    assert_eq!(label(json!({ "type": "once", "runAt": "2026-11-02T17:00:00Z" })), "Once, Nov 2, 2026, 9:00 AM");

    let fixed = Schedule::decode(&json!({ "type": "fixed_time", "timeOfDay": "09:00" }));
    assert_eq!(fixed.label_with_timezone(Some("Etc/UTC"), None, NOW, once), "Daily at 09:00 UTC");
    assert_eq!(
        fixed.label_with_timezone(Some("America/Los_Angeles"), Some("2026-07-01T16:00:00.000Z"), NOW, once),
        "Daily at 09:00 PDT"
    );
    assert_eq!(
        fixed.label_with_timezone(Some("America/Los_Angeles"), Some("2026-01-15T17:00:00.000Z"), NOW, once),
        "Daily at 09:00 PST"
    );

    assert_eq!(relative_run_label(None, NOW), "Not scheduled");
    assert_eq!(relative_run_label(Some(&crate::sidebar::time::format_iso(NOW + 59_000)), NOW), "in under a minute");
    assert_eq!(relative_run_label(Some(&crate::sidebar::time::format_iso(NOW + 30 * 60_000)), NOW), "in 30m");
    assert_eq!(relative_run_label(Some(&crate::sidebar::time::format_iso(NOW + 150 * 60_000)), NOW), "in 3h");
    assert_eq!(relative_run_label(Some(&crate::sidebar::time::format_iso(NOW + 60 * 3_600_000)), NOW), "in 3d");
    assert_eq!(relative_run_label(Some(&crate::sidebar::time::format_iso(NOW - 5 * 60_000)), NOW), "5m ago");

    let mut row = decode_task(&task(json!({ "type": "interval", "everyMs": 60_000 }))).unwrap();
    row.next_run_at = Some(crate::sidebar::time::format_iso(NOW + 30 * 60_000));
    assert_eq!(row.state_label(NOW), "Next run in 30m");
    row.enabled = false;
    assert_eq!(row.state_label(NOW), "Paused");
    row.blocked_by_policy = Some("Webhook tasks are turned off by your organization.".into());
    assert_eq!(row.state_label(NOW), "Webhook tasks are turned off by your organization.");
}

#[test]
fn a_draft_round_trips_a_task_and_validates_like_the_editor() {
    let stored = decode_task(&task(json!({ "type": "fixed_time", "timeOfDay": "09:00" }))).unwrap();
    let draft = Draft::from_task(&stored, |_| None);
    assert_eq!(draft.weekdays.len(), 7, "every day shows every toggle on");
    assert_eq!((draft.base_ref.as_str(), draft.start_from_origin), ("develop", false));
    let input = draft.upsert_input_with_thread_modes(true, false, None, |_| None).unwrap();
    assert_eq!(input["id"], "task-1");
    assert_eq!(input["requireExisting"], true);
    assert_eq!(input["schedule"], json!({ "type": "fixed_time", "timeOfDay": "09:00" }), "all seven days leave weekdays out");
    assert_eq!(input["workspaceStrategy"], json!({ "type": "worktree", "baseRef": "develop", "startFromOrigin": false }));
    assert_eq!(input["modelSelection"]["options"][0]["value"], "high", "unchanged model keeps its options");
    assert_eq!(input["runtimeMode"], "auto-accept-edits");
    assert_eq!(input["threadId"], Value::Null);
    assert_eq!(input.get("threadMode"), None, "older servers omit threadMode");

    let mut draft = Draft { title: "T".into(), prompt: "P".into(), project_id: "p".into(), ..Draft::default() };
    assert_eq!(draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap_err().title, "Scheduled task is incomplete");
    draft.model = Some(ModelSelection { instance_id: "codex".into(), model: "gpt-5".into(), options: None });
    draft.enabled = false;
    let input = draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap();
    assert_eq!(input.get("id"), None);
    assert_eq!(input["enabled"], false);
    assert_eq!(input["threadMode"], "new_thread");
    assert_eq!(input["schedule"], json!({ "type": "fixed_time", "timeOfDay": "09:00", "weekdays": [1, 2, 3, 4, 5] }));
    assert_eq!(input["creationSource"], "web");

    draft.toggle_weekday(1);
    draft.toggle_weekday(2);
    draft.toggle_weekday(3);
    draft.toggle_weekday(4);
    draft.toggle_weekday(5);
    assert_eq!(draft.weekdays, vec![5], "the last day stays on");

    draft.schedule_mode = ScheduleMode::Interval;
    draft.interval_minutes = "0.5".into();
    assert_eq!(draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap_err().title, "Invalid interval");
    draft.interval_minutes = "90".into();
    assert_eq!(draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap()["schedule"], json!({ "type": "interval", "everyMs": 5_400_000 }));

    draft.choose_schedule(ScheduleMode::Once, || "2026-11-02 10:00".into());
    assert_eq!(draft.run_at, "2026-11-02 10:00");
    assert_eq!(draft.upsert_input_with_thread_modes(false, true, None, |_| None).unwrap_err().title, "Once is not available here");
    assert_eq!(draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap_err().description, "That time does not exist in your time zone.");
    let input = draft.upsert_input_with_thread_modes(true, true, None, |_| Some("2026-11-02T18:00:00.000Z".into())).unwrap();
    assert_eq!(input["schedule"], json!({ "type": "once", "runAt": "2026-11-02T18:00:00.000Z" }));

    draft.prompt.clear();
    draft.choose_schedule(ScheduleMode::Webhook, String::new);
    assert_eq!(draft.prompt, DEFAULT_WEBHOOK_PROMPT);
    assert_eq!(
        draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap()["schedule"],
        json!({ "type": "webhook", "signature": null, "maxDeliveryAgeMinutes": null, "dedupe": {} })
    );

    draft.workspace_mode = WorkspaceMode::ExistingWorktree;
    assert_eq!(draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap_err().title, "Checkout path is required");
}

#[test]
fn thread_modes_destination_picker_and_move_to_thread_follow_web() {
    let mut raw = task(json!({ "type": "interval", "everyMs": 60_000 }));
    raw["threadId"] = json!("thread-1");
    raw["threadMode"] = json!("fresh_session");
    let stored = decode_task(&raw).unwrap();
    assert_eq!(stored.effective_thread_mode(), ThreadMode::FreshSession);

    let mut draft = Draft::from_task(&stored, |_| None);
    assert_eq!(draft.thread_mode, ThreadMode::FreshSession);
    assert_eq!(draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap()["threadMode"], "fresh_session");
    assert_eq!(draft.upsert_input_with_thread_modes(true, false, None, |_| None).unwrap().get("threadMode"), None);

    // Changing project unbinds from the thread and moves Root workspace to Worktree.
    draft.workspace_mode = WorkspaceMode::Root;
    draft.change_project("project-2".into());
    assert_eq!(draft.thread_id, None);
    assert_eq!(draft.workspace_mode, WorkspaceMode::Worktree);
    assert!(draft.start_from_origin);
    assert_eq!(draft.thread_mode, ThreadMode::FreshSession, "unbinding keeps the chosen mode");

    // Moving a NewThread draft onto a thread flips it to Continue.
    draft.set_thread_mode(ThreadMode::NewThread);
    draft.move_to_thread("thread-9".into(), "project-2".into());
    assert_eq!(draft.thread_id.as_deref(), Some("thread-9"));
    assert_eq!(draft.thread_mode, ThreadMode::Continue);

    // Destination picker keeps only non-archived listed-lineage threads in the project, newest first.
    let side = Lineage { parent_thread_id: Some("parent".into()), relationship_to_parent: Some("side_chat".into()), root_thread_id: "parent".into(), side_chat: Some(true) };
    let sub = Lineage { parent_thread_id: Some("parent".into()), relationship_to_parent: Some("subagent".into()), root_thread_id: "parent".into(), side_chat: None };
    let root = Lineage { parent_thread_id: None, relationship_to_parent: None, root_thread_id: "root".into(), side_chat: None };
    let fork = Lineage { parent_thread_id: Some("parent".into()), relationship_to_parent: Some("fork".into()), root_thread_id: "parent".into(), side_chat: None };
    let candidates = destination_threads(
        [
            ("t-current", "p1", "Current", "2026-11-05T00:00:00Z", false, &root),
            ("t-old", "p1", "Deploy check", "2026-11-01T00:00:00Z", false, &root),
            ("t-new", "p1", "Deploy nightly", "2026-11-04T00:00:00Z", false, &fork),
            ("t-arch", "p1", "Deploy archived", "2026-11-06T00:00:00Z", true, &root),
            ("t-side", "p1", "Deploy side", "2026-11-06T00:00:00Z", false, &side),
            ("t-sub", "p1", "Deploy sub", "2026-11-06T00:00:00Z", false, &sub),
            ("t-other", "p2", "Deploy other", "2026-11-06T00:00:00Z", false, &root),
        ],
        "p1",
        Some("t-current"),
        "deploy",
    );
    let ids: Vec<&str> = candidates.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["t-new", "t-old"]);
}

#[test]
fn editing_a_task_keeps_what_the_editor_does_not_show() {
    let mut raw = task(json!({ "type": "interval", "everyMs": 90_000 }));
    raw["workspaceStrategy"] = json!({ "type": "worktree", "baseRef": "main" });
    let stored = decode_task(&raw).unwrap();
    let mut draft = Draft::from_task(&stored, |_| None);
    draft.title = "Renamed".into();
    let input = draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap();
    assert_eq!(input["schedule"], json!({ "type": "interval", "everyMs": 90_000 }), "a fractional minute interval survives");
    assert_eq!(input["workspaceStrategy"]["startFromOrigin"], false, "an omitted startFromOrigin is local");
    assert_eq!(draft.upsert_input_with_thread_modes(true, true, Some("auto-accept-edits"), |_| None).unwrap()["runtimeMode"], "auto-accept-edits");
    assert_eq!(clamp_runtime_mode("full-access", Some("auto")), "auto");
    assert_eq!(clamp_runtime_mode("approval-required", Some("auto")), "approval-required");
    assert_eq!(clamp_runtime_mode("full-access", None), "full-access");
    assert_eq!(max_runtime_mode(Some(&json!({ "limits": { "maxRuntimeMode": "auto" } }))), Some("auto"));
}

#[test]
fn webhook_settings_validation_filters_and_deliveries_match_web() {
    let schedule = json!({
        "type": "webhook",
        "signature": { "header": "x-hub-signature-256", "encoding": "hex", "prefix": "sha256=" },
        "maxDeliveryAgeMinutes": 30,
        "dedupe": { "replayKey": "{{headers.x-github-delivery}}", "coalesceKey": "{{body.repository.full_name}}", "coalesceWindowSeconds": 120 },
        "filters": [{ "path": "body.action", "op": "in", "value": ["opened", "reopened"] }]
    });
    let mut raw = task(schedule);
    raw["webhook"] = json!({ "path": "/api/hooks/task-1/abc", "url": null, "hasSecret": true });
    let stored = decode_task(&raw).unwrap();
    let mut draft = Draft::from_task(&stored, |_| None);

    // Unchanged filters are omitted on save so server filters aren't clobbered.
    let saved_payload = draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap();
    assert_eq!(saved_payload["schedule"].get("filters"), None);
    assert_eq!(saved_payload["schedule"]["signature"], json!({ "header": "x-hub-signature-256", "encoding": "hex", "prefix": "sha256=" }));
    assert_eq!(saved_payload["schedule"]["maxDeliveryAgeMinutes"], 30);

    // Editing filters emits them.
    draft.add_filter();
    draft.filters[1].path = "headers.x-github-event".into();
    draft.filters[1].op = WebhookFilterOp::Equals;
    draft.filters[1].value = "pull_request".into();
    let edited_payload = draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap();
    assert_eq!(
        edited_payload["schedule"]["filters"],
        json!([
            { "path": "body.action", "op": "in", "value": ["opened", "reopened"] },
            { "path": "headers.x-github-event", "op": "equals", "value": "pull_request" }
        ])
    );

    // Invalid dedupe template and age limit are rejected.
    assert!(is_webhook_dedupe_template("{{body.id}}"));
    assert!(is_webhook_dedupe_template("pr-{{body.number}}-{{headers.x-event}}"));
    assert!(!is_webhook_dedupe_template("plain-text"));
    assert!(!is_webhook_dedupe_template("{{unknown.field}}"));
    assert!(!is_webhook_dedupe_template("{{body.id}} {{unclosed"));

    draft.replay_key = "not-a-placeholder".into();
    assert_eq!(draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap_err().title, "Invalid webhook settings");
    draft.replay_key = "{{headers.x-delivery}}".into();
    draft.max_delivery_age_minutes = "99999".into();
    assert_eq!(draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap_err().title, "Invalid age limit");
    draft.max_delivery_age_minutes = "60".into();

    // Requiring signature without a saved or typed secret is rejected.
    draft.has_saved_signature_secret = false;
    draft.signature_secret.clear();
    assert_eq!(draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap_err().title, "Invalid webhook signature");

    // Delivery decoding & labels.
    assert_eq!(webhook_delivery_label("accepted", false), "Ran");
    assert_eq!(webhook_delivery_label("accepted", true), "Opened window");
    assert_eq!(webhook_delivery_label("rejected_signature", false), "Bad signature");
    assert_eq!(webhook_delivery_label("future_outcome", false), "future outcome");

    let list = decode_webhook_deliveries(&json!({
        "deliveries": [{
            "id": "del-1", "taskId": "task-1", "receivedAt": "2026-11-02T09:00:00Z",
            "method": "POST", "contentType": "application/json", "bodyBytes": 42,
            "outcome": "accepted", "signatureVerified": true, "pendingWindow": true,
            "missingFields": ["body.missing"], "error": null
        }]
    }))
    .unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].pending_window);
    assert_eq!(list[0].missing_fields, vec!["body.missing"]);

    let detail = decode_webhook_delivery(&json!({
        "delivery": {
            "id": "del-1", "taskId": "task-1", "receivedAt": "2026-11-02T09:00:00Z",
            "method": "POST", "contentType": "application/json", "bodyBytes": 42,
            "outcome": "accepted", "signatureVerified": true,
            "missingFields": [], "error": null,
            "query": "foo=bar", "headers": { "x-test": "1" }, "body": "{\"ok\":true}",
            "bodyTruncated": false, "renderedPrompt": "Handle this webhook:\n{\"ok\":true}"
        }
    }))
    .unwrap();
    assert_eq!(detail.query, "foo=bar");
    assert_eq!(detail.headers.get("x-test").map(String::as_str), Some("1"));
    assert_eq!(detail.rendered_prompt.as_deref(), Some("Handle this webhook:\n{\"ok\":true}"));

    let hook = stored.webhook.unwrap();
    assert!(hook.has_secret);
    assert_eq!(
        webhook_address(&hook, Some("http://127.0.0.1:3773/")),
        ("http://127.0.0.1:3773/api/hooks/task-1/abc".to_owned(), true, "Only this computer can call this address. Link T3 Connect for a public URL.")
    );
    assert!(webhook_address(&hook, Some("http://mac.tailnet.ts.net:3773")).2.starts_with("Works wherever"));
    assert_eq!(webhook_address(&hook, None), ("/api/hooks/task-1/abc".to_owned(), false, "Link this environment to T3 Connect for a public URL."));
    let relay = Webhook { path: hook.path.clone(), url: Some("https://hooks.t3.codes/task-1/abc".into()), has_secret: true };
    assert_eq!(webhook_address(&relay, None), ("https://hooks.t3.codes/task-1/abc".to_owned(), true, ""));
    assert!(valid_time_of_day("9:05") && valid_time_of_day("23:59") && !valid_time_of_day("24:00") && !valid_time_of_day("9:5"));

    let mut fixed_draft = Draft::from_task(&decode_task(&task(json!({ "type": "fixed", "timeOfDay": "09:00", "daysOfWeek": ["mon"] }))).unwrap(), |_| None);
    fixed_draft.time_of_day.clear();
    let err = fixed_draft.upsert_input_with_thread_modes(true, true, None, |_| None).unwrap_err();
    assert_eq!(err.description, "Pick a time.");
}
