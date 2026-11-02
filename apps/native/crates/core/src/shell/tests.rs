use super::*;
use serde_json::{Value, json};

fn thread(id: &str, title: &str) -> Value {
    json!({
        "id": id, "projectId": "p1", "title": title, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "forkedFrom": null, "activeProviderThreadId": null, "latestRunId": null, "activeRunId": null,
        "status": "idle", "pendingRuntimeRequest": null, "latestVisibleMessage": null,
        "latestUserMessageAt": null, "hasActionableProposedPlan": false, "itemCount": 0,
        "visibleItemCount": 0, "createdAt": "2026-11-01T00:00:00.000Z",
        "updatedAt": "2026-11-01T00:00:00.000Z", "archivedAt": null, "settledOverride": null,
        "settledAt": null, "deletedAt": null, "someFutureField": {"ignored": true}
    })
}

fn item(value: Value) -> ShellStreamItem {
    serde_json::from_value(value).unwrap()
}

fn snapshot_item(sequence: u64) -> ShellStreamItem {
    item(json!({
        "kind": "snapshot",
        "snapshot": {
            "schemaVersion": 1, "snapshotSequence": sequence, "archivedThreads": [],
            "projects": [{"id": "p1", "title": "T3", "workspaceRoot": "/w", "defaultModelSelection": null,
                          "scripts": [], "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-01T00:00:00.000Z"}],
            "threads": [thread("t1", "First")]
        }
    }))
}

#[test]
fn snapshot_then_deltas_track_sequence_and_ignore_stale_events() {
    let mut state = ShellState::default();
    assert!(state.apply(snapshot_item(10)));
    assert!(state.apply(item(json!({"kind": "synchronized"}))));
    assert!(!state.apply(item(json!({"kind": "synchronized"}))));

    let first = state.revision("t1");
    assert!(first.is_some());

    let stale = item(json!({"kind": "thread.updated", "sequence": 10, "location": "active", "thread": thread("t1", "Stale")}));
    assert!(!state.apply(stale));
    assert_eq!(state.thread("t1").unwrap().title, "First");
    assert_eq!(state.revision("t1"), first, "a stale delta leaves the row as it was");

    assert!(state.apply(item(json!({"kind": "thread.updated", "sequence": 11, "location": "active", "thread": thread("t2", "Second")}))));
    assert_eq!(state.sequence(), Some(11));
    assert_eq!(state.snapshot.as_ref().unwrap().threads.len(), 2);
    assert!(state.revision("t2").is_some());
    assert_eq!(state.revision("t1"), first, "another row's delta leaves this one as it was");

    assert!(state.apply(item(json!({"kind": "thread.updated", "sequence": 12, "location": "archive", "thread": thread("t2", "Second")}))));
    assert!(state.thread("t2").is_none());
    assert_eq!(state.revision("t2"), None);

    assert!(state.apply(item(json!({"kind": "project.removed", "sequence": 13, "projectId": "p1"}))));
    assert!(state.project("p1").is_none());
}

#[test]
fn identical_thread_update_only_advances_sequence() {
    let mut state = ShellState::default();
    state.apply(snapshot_item(1));
    let before = state.revision("t1");
    assert!(!state.apply(item(json!({"kind": "thread.updated", "sequence": 2, "location": "active", "thread": thread("t1", "First")}))));
    assert_eq!(state.sequence(), Some(2));
    assert_eq!(state.revision("t1"), before, "an identical row keeps its revision");

    assert!(state.apply(item(json!({"kind": "thread.updated", "sequence": 3, "location": "active", "thread": thread("t1", "Renamed")}))));
    let renamed = state.revision("t1");
    assert_ne!(renamed, before);
    state.apply(snapshot_item(4));
    assert_ne!(state.revision("t1"), renamed, "a new snapshot replaces every row");
}

#[test]
fn unknown_stream_kinds_decode_and_are_ignored() {
    let mut state = ShellState::default();
    assert!(!state.apply(item(json!({"kind": "something.new", "sequence": 5}))));
}

#[test]
fn identity_enrichment_snapshot_keeps_threads_and_only_updates_identity() {
    let mut state = ShellState::default();
    state.apply(snapshot_item(3));
    let enrichment = item(json!({
        "kind": "snapshot",
        "resolvedRepositoryIdentityRoots": ["/w"],
        "snapshot": {
            "schemaVersion": 1, "snapshotSequence": 3, "archivedThreads": [], "threads": [],
            "projects": [{"id": "p1", "title": "T3", "workspaceRoot": "/w", "defaultModelSelection": null,
                          "repositoryIdentity": {"canonicalKey": "github.com/x/y"},
                          "scripts": [], "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-01T00:00:00.000Z"}]
        }
    }));
    assert!(state.apply(enrichment));
    let snapshot = state.snapshot.as_ref().unwrap();
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.projects[0].repository_identity, Some(json!({"canonicalKey": "github.com/x/y"})));
}
