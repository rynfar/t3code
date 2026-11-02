use super::*;
use serde_json::{Value, json};

const T: &str = "2026-11-01T00:00:00.000Z";

fn base_item(id: &str, ordinal: u64, run_id: Option<&str>) -> Value {
    json!({
        "id": id, "threadId": "t1", "runId": run_id, "nodeId": "n1", "providerThreadId": null,
        "providerTurnId": null, "nativeItemRef": null, "parentItemId": null, "ordinal": ordinal,
        "status": "completed", "title": null, "startedAt": null, "completedAt": null, "updatedAt": T
    })
}

fn assistant(id: &str, ordinal: u64, run_id: Option<&str>, text: &str) -> Value {
    let mut value = base_item(id, ordinal, run_id);
    let object = value.as_object_mut().unwrap();
    object.insert("type".into(), json!("assistant_message"));
    object.insert("messageId".into(), json!(format!("m-{id}")));
    object.insert("text".into(), json!(text));
    object.insert("streaming".into(), json!(false));
    value
}

fn run(id: &str, status: &str) -> Value {
    json!({
        "id": id, "threadId": "t1", "ordinal": 1, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "providerThreadId": null,
        "userMessageId": "m0", "rootNodeId": null, "activeAttemptId": null, "status": status,
        "requestedAt": T, "startedAt": null, "completedAt": null, "checkpointId": null,
        "contextHandoffId": null
    })
}

fn projection(items: Vec<Value>, runs: Vec<Value>) -> ThreadProjection {
    let visible: Vec<Value> = items
        .iter()
        .filter(|item| item.get("id").is_some())
        .enumerate()
        .map(|(position, item)| json!({
            "position": position, "visibility": "local", "sourceThreadId": "t1",
            "sourceItemId": item["id"], "item": item
        }))
        .collect();
    serde_json::from_value(json!({
        "thread": {
            "id": "t1", "projectId": "p1", "title": "Thread", "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
            "interactionMode": "default", "branch": null, "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t1"},
            "createdAt": T, "updatedAt": T, "archivedAt": null
        },
        "runs": runs, "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [],
        "plans": [], "turnItems": items, "checkpointScopes": [], "checkpoints": [],
        "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": visible, "updatedAt": T
    }))
    .unwrap()
}

fn event(kind: &str, payload: Value) -> DomainEvent {
    serde_json::from_value(json!({
        "id": "e", "threadId": "t1", "occurredAt": "2026-11-01T00:00:05.000Z", "type": kind, "payload": payload
    }))
    .unwrap()
}

fn visible_ids(projection: &ThreadProjection) -> Vec<&str> {
    projection.visible_turn_items.iter().map(|row| row.source_item_id.as_str()).collect()
}

#[test]
fn streaming_update_replaces_in_place_and_new_items_insert_by_ordinal() {
    let mut p = projection(vec![assistant("a", 1, None, "x"), assistant("c", 3, None, "z")], vec![]);
    assert!(apply_event(&mut p, event("turn-item.updated", assistant("a", 1, None, "x more")), ApplyOptions::default()));
    assert_eq!(visible_ids(&p), ["a", "c"]);
    let TurnItemKind::AssistantMessage { text, .. } = &p.visible_turn_items[0].item.kind else { panic!() };
    assert_eq!(text, "x more");
    assert_eq!(p.updated_at, "2026-11-01T00:00:05.000Z");

    apply_event(&mut p, event("turn-item.updated", assistant("b", 2, None, "y")), ApplyOptions::default());
    assert_eq!(visible_ids(&p), ["a", "b", "c"]);
    assert!(p.visible_turn_items.iter().enumerate().all(|(i, row)| row.position == i));
}

#[test]
fn rolled_back_run_hides_its_items() {
    let mut p = projection(vec![assistant("a", 1, Some("r1"), "x"), assistant("b", 2, Some("r2"), "y")], vec![run("r1", "completed"), run("r2", "completed")]);
    apply_event(&mut p, event("run.updated", run("r2", "rolled_back")), ApplyOptions::default());
    assert_eq!(visible_ids(&p), ["a"]);
    apply_event(&mut p, event("turn-item.updated", assistant("b", 2, Some("r2"), "y2")), ApplyOptions::default());
    assert_eq!(visible_ids(&p), ["a"]);
}

#[test]
fn partial_timeline_drops_unloaded_history_but_keeps_new_run_items() {
    let mut p = projection(vec![assistant("a", 10, Some("r1"), "x")], vec![run("r1", "completed"), run("r2", "running")]);
    let options = ApplyOptions { partial_timeline: true, latest_local_turn_ordinal: None };
    assert!(!apply_event(&mut p, event("turn-item.updated", assistant("old", 2, Some("r0"), "o")), options));
    assert!(apply_event(&mut p, event("turn-item.updated", assistant("new", 3, Some("r2"), "n")), options));
    assert_eq!(visible_ids(&p), ["new", "a"]);
}

#[test]
fn touch_events_do_not_bump_activity_and_unknown_events_are_skipped() {
    let mut p = projection(vec![], vec![]);
    let mut thread = serde_json::to_value(json!({
        "id": "t1", "projectId": "p1", "title": "Renamed", "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t1"},
        "createdAt": T, "updatedAt": T, "archivedAt": null
    }))
    .unwrap();
    apply_event(&mut p, event("thread.visited", thread.clone()), ApplyOptions::default());
    assert_eq!(p.thread.title, "Renamed");
    assert_eq!(p.updated_at, T);
    thread["title"] = json!("Again");
    apply_event(&mut p, event("thread.metadata-updated", thread), ApplyOptions::default());
    assert_eq!(p.updated_at, "2026-11-01T00:00:05.000Z");
    assert!(!apply_event(&mut p, event("thread.brand-new-event", json!({})), ApplyOptions::default()));
}

#[test]
fn unknown_turn_item_types_are_filtered_from_snapshots() {
    let mut weird = base_item("w", 5, None);
    weird.as_object_mut().unwrap().insert("type".into(), json!("hologram"));
    let p = projection(vec![assistant("a", 1, None, "x"), weird, json!({"type": "from_the_future"})], vec![]);
    assert_eq!(p.turn_items.len(), 1);
    assert_eq!(visible_ids(&p), ["a"]);
    let minimal: ThreadStreamItem = serde_json::from_value(json!({
        "kind": "event", "sequence": 9, "event": {"type": "brand.new", "payload": {}}
    }))
    .unwrap();
    assert!(matches!(minimal, ThreadStreamItem::Event { ref event, .. } if matches!(event.body, EventBody::Unknown(_))));
}

#[test]
fn stream_items_ignore_events_at_or_below_the_snapshot_sequence() {
    let mut state = ThreadState {
        projection: Some(projection(vec![], vec![])),
        sequence: Some(7),
        ..ThreadState::default()
    };
    let stale: ThreadStreamItem = serde_json::from_value(json!({
        "kind": "event", "sequence": 7,
        "event": {"id": "e", "threadId": "t1", "occurredAt": T, "type": "turn-item.updated", "payload": assistant("a", 1, None, "x")}
    }))
    .unwrap();
    assert!(!state.apply(stale));
    let fresh: ThreadStreamItem = serde_json::from_value(json!({
        "kind": "event", "sequence": 8,
        "event": {"id": "e", "threadId": "t1", "occurredAt": T, "type": "turn-item.updated", "payload": assistant("a", 1, None, "x")}
    }))
    .unwrap();
    assert!(state.apply(fresh));
    assert_eq!(state.sequence, Some(8));
}
