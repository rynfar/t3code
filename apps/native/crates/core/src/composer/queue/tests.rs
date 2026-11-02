use super::*;
use serde_json::{Value, json};

const T: &str = "2026-11-01T00:00:00.000Z";

fn run(id: &str, ordinal: u64, status: &str, queue_position: Option<u64>) -> Value {
    json!({
        "id": id, "threadId": "t1", "ordinal": ordinal, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "providerThreadId": "pt1",
        "userMessageId": format!("m-{id}"), "rootNodeId": null, "activeAttemptId": if status == "running" { json!("a1") } else { Value::Null },
        "status": status, "requestedAt": T, "startedAt": null, "completedAt": null,
        "queueHeld": status == "queued", "queuePosition": queue_position,
    })
}

fn message(id: &str, text: &str, extra: Value) -> Value {
    let mut value = json!({ "id": format!("m-{id}"), "text": text, "attachments": [] });
    if let (Value::Object(base), Value::Object(extra)) = (&mut value, extra) {
        base.extend(extra);
    }
    value
}

fn projection(runs: Vec<Value>, messages: Vec<Value>, turns: Vec<Value>, steering: bool) -> ThreadProjection {
    serde_json::from_value(json!({
        "thread": {
            "id": "t1", "projectId": "p1", "title": "Thread", "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "full-access",
            "interactionMode": "default", "branch": null, "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t1"},
            "activeProviderThreadId": "pt1", "createdAt": T, "updatedAt": T, "archivedAt": null
        },
        "runs": runs, "attempts": [], "nodes": [], "subagents": [],
        "providerSessions": [{"id": "s1", "status": "running", "capabilities": {"turns": {"supportsActiveSteering": steering}}}],
        "providerThreads": [{"id": "pt1", "appThreadId": "t1", "providerSessionId": "s1"}],
        "providerTurns": turns, "runtimeRequests": [], "messages": messages,
        "plans": [], "turnItems": [], "checkpointScopes": [], "checkpoints": [],
        "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [], "updatedAt": T
    }))
    .unwrap()
}

#[test]
fn queued_runs_sort_by_queue_position_and_skip_notices() {
    let projection = projection(
        vec![run("r1", 1, "completed", None), run("r3", 3, "queued", Some(1)), run("r2", 2, "queued", Some(2)), run("r4", 4, "queued", None)],
        vec![
            message("r2", "second", json!({})),
            message("r3", "first", json!({ "context": { "version": 1, "records": [], "guest": { "name": "Ann", "text": "guest words" } } })),
            message("r4", "done", json!({ "notification": { "kind": "x" } })),
        ],
        vec![],
        false,
    );
    let state = queue_state(&projection);
    let rows: Vec<(&str, &str, bool)> = state.queued.iter().map(|run| (run.run_id.as_str(), run.text.as_str(), run.editable)).collect();
    assert_eq!(rows, [("r3", "guest words", false), ("r2", "second", true)]);
    assert!(state.is_held);
    assert!(state.can_send_now, "nothing runs");
    assert_eq!(state.send_tooltip(), "Start a turn with this message");
    assert_eq!(send_now(&state, "c1", "t1", "r3").unwrap()["type"], "queued-run.start");
}

#[test]
fn a_running_turn_takes_a_steer_only_when_the_provider_can() {
    let runs = vec![run("r1", 1, "running", None), run("r2", 2, "queued", Some(1))];
    let turns = vec![json!({ "id": "pt", "runAttemptId": "a1", "status": "running" })];
    let messages = vec![message("r1", "go", json!({})), message("r2", "more", json!({}))];
    let incapable = queue_state(&projection(runs.clone(), messages.clone(), turns.clone(), false));
    assert!(!incapable.can_send());
    assert_eq!(send_now(&incapable, "c1", "t1", "r2"), None);
    let capable = queue_state(&projection(runs.clone(), messages, turns, true));
    assert!(capable.can_promote_to_steer);
    assert_eq!(capable.send_tooltip(), "Send into the current turn");
    assert_eq!(
        send_now(&capable, "c1", "t1", "r2").unwrap(),
        json!({ "type": "queued-message.promote-to-steer", "commandId": "c1", "threadId": "t1", "queuedRunId": "r2", "targetRunId": "r1" })
    );
    let compacting = queue_state(&projection(runs, vec![message("r1", "/compact", json!({}))], vec![], false));
    assert!(compacting.promote_skips_compaction && compacting.can_promote_to_steer);
    assert_eq!(compacting.send_tooltip(), "Skip compaction and send this message");
}

#[test]
fn rows_read_like_sent_messages() {
    let queued = |text: &str, attachments: Vec<Value>| QueuedRun { run_id: "r".into(), message_id: "m".into(), text: text.into(), attachments, editable: true };
    assert_eq!(preview_text(&queued(ATTACHMENT_ONLY_PROMPT, vec![])), "");
    assert_eq!(preview_text(&queued(&format!("Ultrathink: {ATTACHMENT_ONLY_PROMPT}"), vec![])), "");
    let image = json!({ "type": "image", "id": "a" });
    assert_eq!(preview_text(&queued("see ![x.png](t3-context://v1/image/image_1) and [T](t3-context://v1/thread/t2)", vec![image])), "see  and T");
    assert_eq!(preview_text(&queued("see ![x.png](t3-context://v1/image/image_1)", vec![])), "see x.png");
    assert_eq!(editable_text(&format!("Ultrathink:\n{ATTACHMENT_ONLY_PROMPT}")), "Ultrathink:");
    assert_eq!(editable_text("keep me"), "keep me");
    assert_eq!(edit_blocked_reason(1), Some("Answer the question to edit queued messages"));
    assert_eq!(edit_blocked_reason(0), None);
}

#[test]
fn queue_commands_match_the_contract() {
    assert_eq!(cancel_queued_run("c", "t", "r"), json!({ "type": "queued-run.cancel", "commandId": "c", "threadId": "t", "runId": "r" }));
    let edit = edit_queued_run("c", "t", "r", "new text");
    assert_eq!(edit, json!({ "type": "queued-run.edit", "commandId": "c", "threadId": "t", "runId": "r", "text": "new text" }));
    assert!(edit.get("attachments").is_none(), "absent attachments keep the queued ones");
}
