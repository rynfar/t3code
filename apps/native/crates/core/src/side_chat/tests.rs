use serde_json::{Value, json};

use super::*;

fn shell(extra: Value) -> ThreadShell {
    let mut value = json!({
        "id": "s1", "projectId": "p1", "title": "Side chat", "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": "t1", "relationshipToParent": "fork", "rootThreadId": "t1", "sideChat": true},
        "latestRunId": null, "activeRunId": null, "status": "completed", "pendingRuntimeRequest": null,
        "latestVisibleMessage": null, "latestUserMessageAt": null, "hasActionableProposedPlan": false,
        "itemCount": 0, "visibleItemCount": 0, "createdAt": "2026-11-01T00:00:00.000Z",
        "updatedAt": "2026-11-01T00:00:00.000Z", "archivedAt": null, "settledOverride": null,
        "settledAt": null, "deletedAt": null
    });
    for (key, field) in extra.as_object().into_iter().flatten() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).expect("shell")
}

fn run(status: &str, preparation: Option<&str>) -> Run {
    serde_json::from_value(json!({
        "id": "r1", "ordinal": 1, "providerInstanceId": "codex", "modelSelection": {"instanceId": "codex", "model": "gpt"},
        "userMessageId": "m0", "rootNodeId": null, "status": status, "requestedAt": "2026-11-01T00:00:00.000Z",
        "startedAt": null, "completedAt": null,
        "workspacePreparation": preparation.map(|kind| json!({"type": kind}))
    }))
    .expect("run")
}

fn main_thread<'a>(capability: Option<bool>, runs: &'a [Run]) -> Parent<'a> {
    Parent { is_side_chat: false, is_subagent: false, capability, is_server_thread: true, worktree_path: None, runs }
}

#[test]
fn the_disabled_reason_follows_the_web_order() {
    let none: [Run; 0] = [];
    let parent = main_thread(Some(true), &none);
    assert_eq!(disabled_reason(&parent), None);
    assert!(offered(&parent));
    assert_eq!(disabled_reason(&Parent { is_side_chat: true, ..parent }), Some("Side chats cannot start side chats."));
    assert_eq!(disabled_reason(&Parent { is_subagent: true, ..parent }), Some("Subagent threads cannot start side chats."));
    assert!(!offered(&Parent { is_subagent: true, ..parent }));
    assert_eq!(disabled_reason(&main_thread(None, &none)), Some("Side chats are not ready yet. Try again in a moment."));
    assert_eq!(disabled_reason(&main_thread(Some(false), &none)), Some("Update this environment's T3 Code server to use side chats."));
    assert!(!offered(&main_thread(Some(false), &none)));
    assert_eq!(disabled_reason(&Parent { is_server_thread: false, ..parent }), Some("Side chats start after the first message."));
    let preparing = [run("preparing", Some("worktree"))];
    assert_eq!(disabled_reason(&main_thread(Some(true), &preparing)), Some("Available once the thread's worktree is set up."));
    assert_eq!(disabled_reason(&Parent { worktree_path: Some("/w"), ..main_thread(Some(true), &preparing) }), None);
    assert!(!worktree_pending(None, &[run("preparing", Some("root"))]));
}

#[test]
fn parent_of_reads_the_shell() {
    let none: [Run; 0] = [];
    let subagent = shell(json!({"lineage": {"parentThreadId": "t1", "relationshipToParent": "subagent", "rootThreadId": "t1"}}));
    assert!(Parent::of(&subagent, &none, Some(true)).is_subagent);
    let side = shell(json!({}));
    assert!(Parent::of(&side, &none, Some(true)).is_side_chat);
    assert!(is_side_chat_of(&side, "t1"));
    assert!(!is_side_chat_of(&side, "t2"));
}

#[test]
fn bring_back_waits_for_an_idle_parent_and_a_finished_reply() {
    let finished = run("completed", None);
    assert_eq!(bring_back_blocked_reason(true, Some(&finished)), Some("Available when the main thread is idle."));
    assert_eq!(bring_back_blocked_reason(false, None), Some("Available after a finished reply."));
    assert_eq!(bring_back_blocked_reason(false, Some(&finished)), None);
}

#[test]
fn tab_state_puts_requests_before_running() {
    assert_eq!(TabState::of(&shell(json!({}))), None);
    assert_eq!(TabState::of(&shell(json!({"status": "running"}))), Some(TabState::Running));
    let approval = json!({"status": "running", "pendingRuntimeRequest": {"id": "q", "kind": "command_approval"}});
    assert_eq!(TabState::of(&shell(approval)).map(TabState::label), Some("needs approval"));
    let input = json!({"pendingRuntimeRequest": {"id": "q", "kind": "user_input"}});
    assert_eq!(TabState::of(&shell(input)), Some(TabState::NeedsInput));
}

#[test]
fn commands_match_the_web_dispatch() {
    let fork = fork_command("c", "t1", "s1");
    assert_eq!(fork["sourcePoint"], json!({"type": "latest_stable"}));
    assert_eq!((fork["sideChat"].clone(), fork["title"].clone()), (json!(true), json!("Side chat")));
    let first = first_message_command("c", "s1", "m", "  why is   this slow?  ");
    assert_eq!(first["titleSeed"], json!(crate::composer::title_seed("  why is   this slow?  ")));
    assert_eq!(first["dispatchMode"], json!({"type": "start_immediately"}));
    assert_eq!(delete_command("c", "s1", true)["onlyIfUnused"], json!(true));
    assert!(delete_command("c", "s1", false).get("onlyIfUnused").is_none());
}
