use super::*;
use serde_json::{Value, json};

const T: &str = "2026-11-01T00:00:00.000Z";

fn run(id: &str, ordinal: u64, status: &str, provider_thread: Option<&str>) -> Value {
    json!({
        "id": id, "threadId": "t1", "ordinal": ordinal, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "providerThreadId": provider_thread,
        "userMessageId": "m0", "rootNodeId": null, "activeAttemptId": null, "status": status,
        "requestedAt": T, "startedAt": null, "completedAt": null, "checkpointId": null, "contextHandoffId": null
    })
}

fn item(id: &str, run_id: &str, status: &str, kind: Value) -> Value {
    let mut value = json!({
        "id": id, "threadId": "t1", "runId": run_id, "nodeId": "n1", "parentItemId": null, "ordinal": 1,
        "status": status, "title": null, "startedAt": null, "completedAt": null, "updatedAt": T
    });
    if let (Value::Object(base), Value::Object(kind)) = (&mut value, kind) {
        base.extend(kind);
    }
    value
}

fn projection(runs: Vec<Value>, items: Vec<Value>, extra: Value) -> ThreadProjection {
    let visible: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(position, item)| json!({"position": position, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": item["id"], "item": item}))
        .collect();
    let mut value = json!({
        "thread": {
            "id": "t1", "projectId": "p1", "title": "Thread", "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "full-access",
            "interactionMode": "default", "branch": null, "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t1"},
            "activeProviderThreadId": "pt1", "createdAt": T, "updatedAt": T, "archivedAt": null
        },
        "runs": runs, "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [],
        "plans": [], "turnItems": items, "checkpointScopes": [], "checkpoints": [],
        "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": visible, "updatedAt": T
    });
    if let (Value::Object(base), Value::Object(extra)) = (&mut value, extra) {
        for (key, extra) in extra {
            if let (Some(Value::Object(target)), Value::Object(fields)) = (base.get_mut(&key), &extra) {
                target.extend(fields.clone());
            } else {
                base.insert(key, extra);
            }
        }
    }
    serde_json::from_value(value).unwrap()
}

fn session(id: &str, status: &str, handoff: bool) -> Value {
    json!({"id": id, "status": status, "capabilities": {"sessions": {"supportsProviderSwitchingViaHandoff": handoff}}})
}

#[test]
fn handoff_reads_the_attached_session_not_the_newest_one() {
    let attached = projection(
        vec![run("r1", 1, "completed", Some("pt1"))],
        vec![],
        json!({
            "providerThreads": [{"id": "pt1", "appThreadId": "t1", "providerSessionId": "s-old", "providerInstanceId": "codex", "nativeThreadRef": null}],
            "providerSessions": [session("s-old", "ready", false), session("s-new", "ready", true)]
        }),
    );
    assert!(!supports_provider_handoff(&attached));

    let running = projection(
        vec![run("r1", 1, "completed", Some("pt1")), run("r2", 2, "running", Some("pt2"))],
        vec![],
        json!({
            "providerThreads": [
                {"id": "pt1", "appThreadId": "t1", "providerSessionId": "s1", "providerInstanceId": "codex", "nativeThreadRef": null},
                {"id": "pt2", "appThreadId": "t1", "providerSessionId": "s2", "providerInstanceId": "codex", "nativeThreadRef": null}
            ],
            "providerSessions": [session("s1", "ready", false), session("s2", "ready", true)]
        }),
    );
    assert!(supports_provider_handoff(&running));
}

#[test]
fn handoff_without_a_session_follows_runs_imports_and_detached_native_threads() {
    assert!(supports_provider_handoff(&projection(vec![], vec![], json!({}))));
    assert!(!supports_provider_handoff(&projection(vec![run("r1", 1, "running", None)], vec![], json!({}))));
    let finished = vec![run("r1", 1, "completed", None)];
    assert!(!supports_provider_handoff(&projection(finished.clone(), vec![], json!({}))));
    assert!(supports_provider_handoff(&projection(finished.clone(), vec![], json!({"thread": {"historyOrigin": "v1_import"}}))));
    let detached = json!({"providerThreads": [{"id": "pt1", "appThreadId": "t1", "providerSessionId": null, "providerInstanceId": "codex", "nativeThreadRef": {"nativeId": "x"}}]});
    assert!(supports_provider_handoff(&projection(finished, vec![], detached)));
}

#[test]
fn compacting_covers_a_running_compaction_and_an_unsettled_compact_turn() {
    let user = |id: &str, run_id: &str, text: &str| {
        item(id, run_id, "completed", json!({"type": "user_message", "messageId": format!("m-{id}"), "text": text, "inputIntent": "turn", "attachments": []}))
    };
    let compaction = |status: &str| item("c1", "r1", status, json!({"type": "compaction"}));
    let runs = vec![run("r1", 1, "running", None)];
    assert!(is_compacting(&projection(runs.clone(), vec![compaction("running")], json!({})), Some("r1")));
    assert!(is_compacting(&projection(runs.clone(), vec![user("u1", "r1", " /COMPACT ")], json!({})), Some("r1")));
    assert!(!is_compacting(&projection(runs.clone(), vec![user("u1", "r1", "/compact"), compaction("completed")], json!({})), Some("r1")));
    assert!(!is_compacting(&projection(runs.clone(), vec![user("u1", "r1", "go on")], json!({})), Some("r1")));
    assert!(!is_compacting(&projection(runs, vec![compaction("running")], json!({})), None));
}

#[test]
fn stop_targets_the_live_run_or_background_work_when_thread_stop_is_missing() {
    let live = projection(vec![run("r1", 1, "completed", None), run("r2", 2, "waiting", None)], vec![], json!({}));
    assert_eq!(stop_target(Some(&live), None, true), StopTarget::Thread);
    assert_eq!(stop_target(Some(&live), None, false), StopTarget::Run("r2".into()));

    let command = item("i1", "r1", "running", json!({"type": "command_execution", "input": "npm run dev"}));
    let background = projection(vec![run("r1", 1, "completed", None)], vec![command], json!({}));
    assert_eq!(stop_target(Some(&background), None, false), StopTarget::Run("r1".into()));

    let persistent = item("i1", "r1", "running", json!({"type": "dynamic_tool", "toolName": "watch", "input": {"persistent": true}}));
    let idle = projection(vec![run("r1", 1, "completed", None)], vec![persistent], json!({}));
    assert_eq!(stop_target(Some(&idle), Some("r1"), false), StopTarget::Nothing, "a loaded detail outranks the shell");

    let tasks = json!({"providerThreads": [{"id": "pt1", "pendingBackgroundTasks": [{"taskId": "bash-1", "kind": "command"}]}]});
    assert_eq!(stop_target(Some(&projection(vec![run("r1", 1, "interrupted", None)], vec![], tasks)), None, false), StopTarget::Run("r1".into()));
    assert_eq!(stop_target(None, None, false), StopTarget::Nothing);
    // A thread whose detail is not loaded here, such as an app-owned subagent's child.
    assert_eq!(stop_target(None, Some("r7"), false), StopTarget::Run("r7".into()));
    assert_eq!(stop_target(None, Some("r7"), true), StopTarget::Thread);
}
