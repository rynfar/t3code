use serde_json::{Value, json};

use super::*;
use crate::thread::ThreadStreamItem;

const T: &str = "2026-11-01T00:00:00.000Z";

fn item(id: &str, ordinal: u64) -> Value {
    json!({
        "id": id, "threadId": "t1", "runId": "r1", "nodeId": null, "parentItemId": null, "ordinal": ordinal,
        "status": "completed", "title": null, "startedAt": null, "completedAt": null, "updatedAt": T,
        "type": "command_execution", "input": "pwd", "output": id
    })
}

fn row(id: &str, ordinal: u64, position: usize) -> Value {
    json!({"position": position, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": id, "item": item(id, ordinal)})
}

fn thread() -> Value {
    json!({
        "id": "t1", "projectId": "p1", "title": "Thread", "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t1"},
        "createdAt": T, "updatedAt": T, "archivedAt": null
    })
}

fn snapshot(sequence: u64, ids: &[(&str, u64)]) -> ThreadStreamItem {
    let rows: Vec<Value> = ids.iter().enumerate().map(|(i, (id, ordinal))| row(id, *ordinal, i)).collect();
    let items: Vec<Value> = ids.iter().map(|(id, ordinal)| item(id, *ordinal)).collect();
    serde_json::from_value(json!({
        "kind": "snapshot", "snapshotSequence": sequence, "historyCursor": "c1", "hasMoreHistory": true,
        "latestLocalTurnOrdinal": 20,
        "projection": {
            "thread": thread(),
            "runs": [{
                "id": "r1", "threadId": "t1", "ordinal": 1, "providerInstanceId": "codex",
                "modelSelection": {"instanceId": "codex", "model": "gpt"}, "userMessageId": "m0",
                "rootNodeId": null, "status": "running", "requestedAt": T, "startedAt": null, "completedAt": null
            }],
            "attempts": [], "nodes": [], "subagents": [], "providerSessions": [], "providerThreads": [],
            "providerTurns": [], "runtimeRequests": [], "messages": [], "plans": [], "turnItems": items,
            "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
            "visibleTurnItems": rows, "updatedAt": T
        }
    }))
    .unwrap()
}

fn event(sequence: u64, kind: &str, payload: Value) -> ThreadStreamItem {
    serde_json::from_value(json!({
        "kind": "event", "sequence": sequence,
        "event": {"id": "e", "threadId": "t1", "occurredAt": T, "type": kind, "payload": payload}
    }))
    .unwrap()
}

fn page() -> HistoryPage {
    decode_history_page(json!({
        "snapshotSequence": 5, "items": [row("old-1", 1, 0), row("old-2", 2, 1)],
        "nextCursor": null, "hasMoreHistory": false
    }))
    .unwrap()
}

fn visible(state: &ThreadState) -> Vec<&str> {
    state.projection.as_ref().map_or(vec![], |p| p.visible_turn_items.iter().map(|r| r.source_item_id.as_str()).collect())
}

fn loading_state() -> (ThreadState, String) {
    let mut state = ThreadState::default();
    state.apply(snapshot(5, &[("a", 10), ("b", 11)]));
    let cursor = state.begin_history_load().unwrap();
    (state, cursor)
}

#[test]
fn events_during_an_off_thread_merge_apply_after_it_in_order() {
    let (mut instant, cursor) = loading_state();
    assert_eq!(instant.apply_history_page(&cursor, page()), HistoryStep::Done);
    instant.apply(event(6, "turn-item.updated", item("c", 12)));
    instant.apply(event(7, "turn-item.updated", item("old-1", 1)));

    let (mut state, cursor) = loading_state();
    let PageOutcome::Merge(merge) = state.begin_history_merge(&cursor, page()) else { panic!("merges") };
    assert!(state.is_merging_history());
    assert_eq!(state.history_control().map(|c| c.loading), Some(true));
    assert!(!state.apply(event(6, "turn-item.updated", item("c", 12))));
    assert!(!state.apply(event(7, "turn-item.updated", item("old-1", 1))));
    assert_eq!(state.begin_history_load(), None);
    assert_eq!(state.finish_history_merge(merge.run()), HistoryStep::Done);

    assert!(!state.is_merging_history());
    assert_eq!(visible(&state), ["old-1", "old-2", "a", "b", "c"]);
    assert_eq!(visible(&state), visible(&instant));
    assert_eq!(state.sequence, Some(7));
    assert_eq!(state.history, instant.history);
    assert_eq!(state.history_control(), None);
}

#[test]
fn a_snapshot_or_deletion_during_a_merge_drops_it() {
    let (mut state, cursor) = loading_state();
    let PageOutcome::Merge(merge) = state.begin_history_merge(&cursor, page()) else { panic!("merges") };
    state.apply(snapshot(9, &[("z", 30)]));
    assert_eq!(state.finish_history_merge(merge.run()), HistoryStep::Stale);
    assert_eq!(visible(&state), ["z"]);
    assert!(state.history.has_more_history);

    let (mut state, cursor) = loading_state();
    let PageOutcome::Merge(merge) = state.begin_history_merge(&cursor, page()) else { panic!("merges") };
    assert!(state.projection.is_none(), "the projection is checked out");
    assert!(state.apply(event(6, "thread.deleted", thread())));
    assert_eq!(state.finish_history_merge(merge.run()), HistoryStep::Stale);
    assert!(state.deleted && state.projection.is_none());
    assert_eq!(state.history, ThreadHistoryMeta::default());
}
