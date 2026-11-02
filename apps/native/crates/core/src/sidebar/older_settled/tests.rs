use serde_json::{Value, json};

use super::*;
use crate::shell::ShellStreamItem;

const CUT: &str = "2026-06-20T00:00:00.000Z";

fn thread(id: &str, settled_at: Option<&str>) -> Value {
    json!({
        "id": id, "projectId": "p1", "title": id, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "latestRunId": null, "activeRunId": null, "status": "idle", "pendingRuntimeRequest": null,
        "latestVisibleMessage": null, "latestUserMessageAt": null, "hasActionableProposedPlan": false,
        "itemCount": 0, "visibleItemCount": 0, "createdAt": "2026-06-01T00:00:00.000Z",
        "updatedAt": "2026-06-01T00:00:00.000Z", "archivedAt": null,
        "settledOverride": settled_at.map(|_| "settled"), "settledAt": settled_at, "deletedAt": null
    })
}

fn snapshot(sequence: u64, as_of: u64) -> ShellStreamItem {
    serde_json::from_value(json!({"kind": "snapshot", "snapshot": {
        "snapshotSequence": sequence, "projects": [], "archivedThreads": [], "threads": [thread("recent", Some("2026-06-25T00:00:00.000Z"))],
        "olderSettled": {"before": CUT, "asOfSequence": as_of, "counts": [{"projectId": "p1", "count": 30}]}
    }}))
    .unwrap()
}

fn page(count: usize, day: u32) -> FamilyRead {
    let threads: Vec<Value> = (0..count).map(|i| thread(&format!("old-{day}-{i:02}"), Some(&format!("2026-06-{day:02}T00:00:00.000Z")))).collect();
    serde_json::from_value(json!({"snapshotSequence": 5, "threads": threads})).unwrap()
}

#[test]
fn pages_chain_from_the_cut_and_restart_with_each_generation() {
    let mut shell = ShellState::default();
    shell.apply(snapshot(5, 5));
    let older = OlderSettled::from_cut(shell.snapshot.as_ref().unwrap().older_settled.as_ref().unwrap()).unwrap();
    let mut pages = SettledPages::default();
    pages.sync(&shell, Some(&older));
    let cut = Cursor { settled_at: older.before_ms, thread_id: None };
    assert_eq!(pages.listing_frontier(Some(&older)), Some(&cut));
    assert_eq!(pages.next_request(), None, "nothing is read until the shelf asks");

    assert!(pages.grow(Some(&older), 0, 10));
    let (generation, before) = pages.next_request().unwrap();
    assert_eq!(before, cut);
    assert!(!pages.grow(Some(&older), 0, 10), "one page at a time");
    assert!(!pages.apply_page(generation + 1, &before, Ok(page(25, 10))), "another generation's answer is stale");
    assert!(pages.apply_page(generation, &before, Ok(page(25, 10))));
    assert_eq!(pages.frontier, Some(Cursor { settled_at: parse_iso("2026-06-10T00:00:00.000Z").unwrap(), thread_id: Some("old-10-24".into()) }));

    assert!(pages.grow(Some(&older), 25, 35));
    let (generation, before) = pages.next_request().unwrap();
    assert!(pages.apply_page(generation, &before, Err("offline".into())));
    assert_eq!(pages.status, PageStatus::Failed);
    assert_eq!(pages.listing_frontier(Some(&older)), None, "a failed read lists everything it has");
    pages.retry();
    let (generation, retried) = pages.next_request().unwrap();
    assert_eq!(retried, before, "the failed page is read again");
    assert!(pages.apply_page(generation, &retried, Ok(page(3, 5))));
    assert_eq!(pages.frontier, None, "a short page ends the chain");
    assert!(!pages.grow(Some(&older), 28, 60));

    shell.apply(snapshot(9, 9));
    pages.sync(&shell, Some(&older));
    assert!(pages.reads.is_empty() && pages.requested == 2);
    assert_eq!(pages.next_request().map(|(_, before)| before), Some(cut), "a new snapshot reads the requested pages again");
}

#[test]
fn counts_hold_from_the_snapshot_until_what_they_count_changes() {
    let mut shell = ShellState::default();
    shell.apply(snapshot(5, 5));
    let mut counts = OlderSettledCounts::default();
    assert_eq!(counts.sync(&shell), None, "the snapshot's counts are exact at its own sequence");
    assert_eq!(counts.current().unwrap().counts.get("p1"), Some(&30));

    let update: ShellStreamItem = serde_json::from_value(json!({
        "kind": "thread.updated", "sequence": 6, "location": "active", "thread": thread("newly-old", Some("2026-06-01T00:00:00.000Z"))
    }))
    .unwrap();
    shell.apply(update);
    let key = counts.sync(&shell).expect("an older row arrived");
    assert_eq!(counts.current().unwrap().counts.get("p1"), Some(&30), "the snapshot's counts stand until a read lands");
    counts.apply_read("an older key", [("p1".to_owned(), 99)].into());
    assert_eq!(counts.current().unwrap().counts.get("p1"), Some(&30));
    counts.apply_read(&key, [("p1".to_owned(), 31)].into());
    assert_eq!(counts.current().unwrap().counts.get("p1"), Some(&31));
    assert_eq!(counts.sync(&shell), None, "that key is read");

    shell.apply(snapshot(20, 19));
    assert!(counts.sync(&shell).is_some(), "a snapshot behind its cut's sequence reads at once");
}
