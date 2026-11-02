use serde_json::{Value, json};

use super::*;
use crate::shell::ShellStreamItem;

const T: &str = "2026-11-01T00:00:00.000Z";

fn thread(id: &str) -> Value {
    json!({
        "id": id, "projectId": "p1", "title": id, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "latestRunId": null, "activeRunId": null, "status": "idle", "pendingRuntimeRequest": null,
        "latestVisibleMessage": null, "latestUserMessageAt": null, "hasActionableProposedPlan": false,
        "itemCount": 0, "visibleItemCount": 0, "createdAt": T, "updatedAt": T, "archivedAt": null,
        "settledOverride": null, "settledAt": null, "deletedAt": null
    })
}

fn read(sequence: u64, ids: &[&str]) -> FamilyRead {
    serde_json::from_value(json!({"snapshotSequence": sequence, "threads": ids.iter().map(|id| thread(id)).collect::<Vec<_>>()})).unwrap()
}

fn apply(shell: &mut ShellState, item: Value) {
    shell.apply(serde_json::from_value::<ShellStreamItem>(item).unwrap());
}

fn ids<'a>(rows: &[&'a ThreadShell]) -> Vec<&'a str> {
    rows.iter().map(|thread| thread.id.as_str()).collect()
}

#[test]
fn mounted_reads_merge_behind_the_shell_for_their_generation_only() {
    let mut shell = ShellState::default();
    let snapshot = |sequence: u64, as_of: u64| {
        json!({"kind": "snapshot", "snapshot": {
            "snapshotSequence": sequence, "projects": [], "threads": [thread("a")], "archivedThreads": [],
            "olderSettled": {"before": T, "asOfSequence": as_of, "counts": []}
        }})
    };
    apply(&mut shell, snapshot(10, 10));
    let mut mounts = MountedReads::default();
    mounts.mount("family:a", shell.generation, vec![read(10, &["a", "child"]), read(9, &["stale-read"])]);
    mounts.mount("pages", shell.generation, vec![read(12, &["old", "child"])]);
    assert_eq!(ids(&mounts.extras(&shell)), ["child", "old"], "shell rows win; reads before the cut wait");
    assert!(mounts.thread(&shell, "old").is_some());

    apply(&mut shell, json!({"kind": "thread.removed", "sequence": 11, "threadId": "child"}));
    assert_eq!(ids(&mounts.extras(&shell)), ["old", "child"], "the family read predates the removal; the page read does not");
    assert_eq!(shell.removed_at.get("child"), Some(&11));

    apply(&mut shell, snapshot(20, 20));
    assert!(shell.removed_at.is_empty());
    assert!(mounts.extras(&shell).is_empty(), "a new snapshot leaves earlier reads out until read again");
    mounts.mount("pages", shell.generation, vec![read(20, &["old"])]);
    assert_eq!(ids(&mounts.extras(&shell)), ["old"]);
    mounts.unmount("pages");
    assert!(mounts.extras(&shell).is_empty());
}
