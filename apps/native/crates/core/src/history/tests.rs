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

#[test]
fn work_anchor_paging_resumes_from_updated_cursor_and_clears_when_done() {
    let mut snap = snapshot(10, &[("a", 1)]);
    if let ThreadStreamItem::Snapshot { ref mut projection, .. } = snap {
        let deferred = Some(crate::turn_item::DeferredWork {
            cursor: "w1".into(),
            item_count: 2,
            last_ordinal: 3,
        });
        std::sync::Arc::make_mut(&mut projection.turn_items[0]).deferred_work = deferred.clone();
        std::sync::Arc::make_mut(&mut projection.visible_turn_items[0].item).deferred_work = deferred;
    }
    let mut state = ThreadState::default();
    state.apply(snap);

    let first = state.begin_work_load("a").expect("first work load");
    assert_eq!(first.cursor, "w1");
    assert!(state.begin_work_load("a").is_none(), "concurrent load for same anchor is deduped");
    assert!(state.history.is_loading_work("a"));

    let partial = HistoryPage {
        snapshot_sequence: 10,
        items: vec![serde_json::from_value(row("w-item-1", 2, 0)).unwrap()],
        next_cursor: Some("w2".into()),
        has_more_history: true,
        has_user_turn: false,
    };
    assert_eq!(state.apply_work_page("a", "w1", partial), HistoryStep::Continue("w2".into()));
    assert!(!state.history.is_loading_work("a"));
    assert_eq!(visible(&state), ["a", "w-item-1"]);
    assert_eq!(
        state.projection.as_ref().unwrap().visible_turn_items[0].item.deferred_work.as_ref().map(|d| d.cursor.as_str()),
        Some("w2")
    );

    let second = state.begin_work_load("a").expect("second work load");
    assert_eq!(second.cursor, "w2");
    assert_eq!(state.fail_work_load("a", "w2", "boom".into()), HistoryStep::Failed("boom".into()));
    assert_eq!(state.history.work_error("a"), Some("boom"));

    let retry = state.begin_work_load("a").expect("retry after failure");
    assert_eq!(retry.cursor, "w2");
    assert_eq!(state.history.work_error("a"), None);

    let final_page = HistoryPage {
        snapshot_sequence: 10,
        items: vec![serde_json::from_value(row("w-item-2", 3, 0)).unwrap()],
        next_cursor: None,
        has_more_history: false,
        has_user_turn: false,
    };
    assert_eq!(state.apply_work_page("a", "w2", final_page), HistoryStep::Done);
    assert_eq!(visible(&state), ["a", "w-item-1", "w-item-2"]);
    assert!(state.projection.as_ref().unwrap().visible_turn_items[0].item.deferred_work.is_none());
}

#[test]
fn stale_work_merge_or_removed_anchor_reconciles_pending_work() {
    let mut snap = snapshot(1, &[("a", 1)]);
    if let ThreadStreamItem::Snapshot { ref mut projection, .. } = snap {
        let deferred = Some(crate::turn_item::DeferredWork {
            cursor: "w1".into(),
            item_count: 1,
            last_ordinal: 2,
        });
        std::sync::Arc::make_mut(&mut projection.turn_items[0]).deferred_work = deferred.clone();
        std::sync::Arc::make_mut(&mut projection.visible_turn_items[0].item).deferred_work = deferred;
    }
    let mut state = ThreadState::default();
    state.apply(snap);
    let target = state.begin_work_load("a").unwrap();
    let WorkPageOutcome::Ready(merge) = state.begin_work_merge("a", &target.cursor, page()) else {
        panic!("expected ready merge");
    };
    let job = merge.run();
    // Snapshot replacement invalidates the checked-out merge and clears pending work.
    state.apply(snapshot(5, &[("b", 1)]));
    assert_eq!(state.finish_work_merge(job), HistoryStep::Stale);
    assert!(!state.history.is_loading_work("a"));
    assert_eq!(visible(&state), ["b"]);
}

#[test]
fn non_advancing_complete_page_fails_even_when_auto_page_is_off() {
    let (mut state, cursor) = loading_state();
    let stuck = HistoryPage {
        snapshot_sequence: 5,
        items: vec![serde_json::from_value(row("old-1", 1, 0)).unwrap()],
        next_cursor: Some(cursor.clone()),
        has_more_history: true,
        has_user_turn: false,
    };
    let PageOutcome::Step(step) = state.begin_history_merge_with(&cursor, stuck, true, false) else {
        panic!("expected non-advancing step");
    };
    assert_eq!(step, HistoryStep::Failed("History did not advance.".into()));
    assert!(!state.history.loading);
    assert_eq!(state.history.error.as_deref(), Some("History did not advance."));
}

fn question_item(id: &str, run_id: &str, ordinal: u64, request_id: &str) -> Value {
    json!({
        "id": id, "threadId": "t1", "runId": run_id, "nodeId": null, "parentItemId": null,
        "ordinal": ordinal, "status": "running", "title": null,
        "startedAt": T, "completedAt": null, "updatedAt": T,
        "type": "user_input_request",
        "requestId": request_id, "providerRequestId": request_id, "statusDetail": "pending",
        "questions": [{
            "id": "q1", "header": "Confirm", "question": "Proceed?", "options": []
        }],
        "answers": null
    })
}

fn question_row(id: &str, run_id: &str, ordinal: u64, position: usize, request_id: &str) -> Value {
    json!({
        "position": position, "visibility": "local", "sourceThreadId": "t1",
        "sourceItemId": id, "item": question_item(id, run_id, ordinal, request_id)
    })
}

fn snapshot_with_pending_question(run_status: &str) -> ThreadStreamItem {
    serde_json::from_value(json!({
        "kind": "snapshot", "snapshotSequence": 5, "historyCursor": "c1", "hasMoreHistory": true,
        "latestLocalTurnOrdinal": 20,
        "projection": {
            "thread": thread(),
            "runs": [{
                "id": "r-old", "threadId": "t1", "ordinal": 1, "providerInstanceId": "codex",
                "modelSelection": {"instanceId": "codex", "model": "gpt"}, "userMessageId": "m0",
                "rootNodeId": null, "status": run_status, "requestedAt": T, "startedAt": T, "completedAt": T
            }],
            "attempts": [], "nodes": [], "subagents": [], "providerSessions": [], "providerThreads": [],
            "providerTurns": [],
            "runtimeRequests": [{
                "id": "req-1", "threadId": "t1", "runId": "r-old", "providerSessionId": "s1",
                "kind": "user_input", "status": "pending",
                "responseCapability": { "type": "live" },
                "createdAt": T, "requestedAt": T, "resolvedAt": null
            }],
            "messages": [], "plans": [],
            "turnItems": [question_item("req-item", "r-old", 1, "req-1"), item("recent", 2)],
            "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
            "visibleTurnItems": [row("recent", 2, 0)],
            "updatedAt": T
        }
    }))
    .unwrap()
}

#[test]
fn pending_question_from_completed_run_stays_actionable_and_pages_back_while_rolled_back_drops() {
    let mut completed = ThreadState::default();
    completed.apply(snapshot_with_pending_question("completed"));
    let pending = crate::requests::pending_requests(completed.projection.as_ref().unwrap());
    assert_eq!(pending.user_inputs.len(), 1);
    assert_eq!(pending.user_inputs[0].request_id, "req-1");
    let cursor = completed.begin_history_load().unwrap();
    let older = decode_history_page(json!({
        "snapshotSequence": 5,
        "items": [question_row("req-item", "r-old", 1, 0, "req-1")],
        "nextCursor": null, "hasMoreHistory": false
    }))
    .unwrap();
    assert_eq!(completed.apply_history_page(&cursor, older.clone()), HistoryStep::Done);
    assert_eq!(visible(&completed), ["req-item", "recent"]);

    let mut rolled_back = ThreadState::default();
    rolled_back.apply(snapshot_with_pending_question("rolled_back"));
    let cursor = rolled_back.begin_history_load().unwrap();
    assert_eq!(rolled_back.apply_history_page(&cursor, older), HistoryStep::Done);
    assert_eq!(visible(&rolled_back), ["recent"]);
}
