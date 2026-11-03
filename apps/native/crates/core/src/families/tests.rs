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
    assert!(std::ptr::eq(mounts.thread(&shell, "child").unwrap(), mounts.extras(&shell)[1]), "the row extras merge");

    apply(&mut shell, snapshot(20, 20));
    assert!(shell.removed_at.is_empty());
    assert!(mounts.extras(&shell).is_empty(), "a new snapshot leaves earlier reads out until read again");
    mounts.mount("pages", shell.generation, vec![read(20, &["old"])]);
    assert_eq!(ids(&mounts.extras(&shell)), ["old"]);
    mounts.unmount("pages");
    assert!(mounts.extras(&shell).is_empty());
}

#[test]
fn the_open_thread_reads_its_family_whenever_the_shell_leaves_it_out() {
    let mut shell = ShellState::default();
    let snapshot = |sequence: u64, ids: &[&str]| {
        json!({"kind": "snapshot", "snapshot": {"snapshotSequence": sequence, "projects": [], "threads": ids.iter().map(|id| thread(id)).collect::<Vec<_>>()}})
    };
    apply(&mut shell, snapshot(10, &["a"]));
    let mut mounts = MountedReads::default();
    assert!(!family_read_due(&shell, &mounts, "a", None), "the shell holds it");

    // A new snapshot can cut the open thread, say once it settled long enough ago.
    apply(&mut shell, snapshot(20, &[]));
    assert!(family_read_due(&shell, &mounts, "a", None));
    assert!(!family_read_due(&shell, &mounts, "a", Some(shell.generation)), "its read is on the way");
    mounts.mount("search", shell.generation, vec![read(20, &["a"])]);
    assert!(family_read_due(&shell, &mounts, "a", None), "a search can let go of it");
    mounts.mount(&family_key("a"), shell.generation, vec![read(20, &["a"])]);
    assert!(!family_read_due(&shell, &mounts, "a", None));
    assert_eq!(mounts.thread(&shell, "a").map(|thread| thread.id.as_str()), Some("a"));

    apply(&mut shell, snapshot(30, &[]));
    assert!(mounts.thread(&shell, "a").is_none());
    assert!(family_read_due(&shell, &mounts, "a", Some(shell.generation - 1)), "each snapshot reads it again");
    use crate::sidebar::tag_filter::{RouteVisibility, route_visibility};
    let hidden = HashSet::new();
    assert_eq!(route_visibility("a", mounts.thread(&shell, "a").is_some(), &hidden, true), RouteVisibility::Loading,
        "the selected thread must not redirect while its new-generation family is pending");
    mounts.mount(&family_key("a"), shell.generation, vec![read(30, &["a"])]);
    assert_eq!(route_visibility("a", mounts.thread(&shell, "a").is_some(), &hidden, true), RouteVisibility::Visible);
    assert_eq!(route_visibility("a", true, &HashSet::from(["a".into()]), true), RouteVisibility::Hidden);
}
