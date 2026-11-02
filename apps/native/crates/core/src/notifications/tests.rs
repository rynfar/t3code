use serde_json::{Value, json};

use super::*;
use crate::shell::{ShellSnapshot, ShellStreamItem};

fn thread(id: &str, status: &str, extra: Value) -> ThreadShell {
    let mut value = json!({
        "id": id, "projectId": "p1", "title": format!("Title {id}"), "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "latestRunId": "run-1", "activeRunId": null, "status": status, "pendingRuntimeRequest": null,
        "latestVisibleMessage": null, "latestUserMessageAt": null, "hasActionableProposedPlan": false,
        "itemCount": 1, "visibleItemCount": 1, "createdAt": "2026-11-01T00:00:00.000Z",
        "updatedAt": "2026-11-01T00:00:00.000Z", "archivedAt": null, "settledOverride": null,
        "settledAt": null, "deletedAt": null
    });
    for (key, field) in extra.as_object().into_iter().flatten() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).unwrap()
}

/// The live shell the watcher reads: each step is a new snapshot, as a reconnect would send.
#[derive(Default)]
struct Watched {
    shell: ShellState,
    watcher: ThreadNotifications,
}

impl Watched {
    fn observe(&mut self, threads: &[ThreadShell]) -> Vec<ThreadNotification> {
        let snapshot = ShellSnapshot { threads: threads.to_vec(), ..ShellSnapshot::default() };
        self.shell.apply(ShellStreamItem::Snapshot { snapshot: Box::new(snapshot), resolved_repository_identity_roots: None });
        self.watcher.observe(&self.shell)
    }

    fn update(&mut self, sequence: u64, thread: ThreadShell) -> Vec<ThreadNotification> {
        self.shell.apply(ShellStreamItem::ThreadUpdated { sequence, location: "active".into(), thread: Box::new(thread) });
        self.watcher.observe(&self.shell)
    }
}

fn kinds(notifications: &[ThreadNotification]) -> Vec<(&str, &str)> {
    notifications.iter().map(|n| (n.thread_id.as_str(), n.title)).collect()
}

#[test]
fn transitions_notify_and_first_sightings_do_not() {
    let mut watcher = Watched::default();
    let completed = json!({"latestRunCompletedAt": "2026-11-01T00:05:00.000Z"});
    assert!(watcher.observe(&[thread("a", "completed", completed.clone()), thread("b", "running", json!({}))]).is_empty());

    let approval = json!({"pendingRuntimeRequest": {"id": "r1", "kind": "command_approval", "createdAt": "2026-11-01T00:06:00.000Z"}});
    let next = watcher.observe(&[thread("a", "completed", completed.clone()), thread("b", "running", approval)]);
    assert_eq!(kinds(&next), [("b", "Approval needed")]);
    assert_eq!(next[0].body, "Title b");

    let finished = json!({"latestRunCompletedAt": "2026-11-01T00:09:00.000Z"});
    assert_eq!(kinds(&watcher.observe(&[thread("a", "completed", completed.clone()), thread("b", "completed", finished.clone())])), [("b", "Thread completed")]);
    assert!(watcher.observe(&[thread("a", "completed", completed), thread("b", "completed", finished)]).is_empty(), "one completion notifies once");

    // A delta replaces one row; the next observation reads it and keeps the rest.
    let asking = json!({"latestRunId": "run-2", "pendingRuntimeRequest": {"id": "r2", "kind": "command_approval", "createdAt": "2026-11-01T00:10:00.000Z"}});
    assert_eq!(kinds(&watcher.update(1, thread("a", "running", asking))), [("a", "Approval needed")]);
    let renamed = json!({"latestRunCompletedAt": "2026-11-01T00:09:00.000Z", "title": "Renamed"});
    assert!(watcher.update(2, thread("b", "completed", renamed)).is_empty());
}

#[test]
fn archived_stopped_and_nested_threads_stay_quiet() {
    let mut watcher = Watched::default();
    let child = json!({"lineage": {"parentThreadId": "p", "relationshipToParent": "subagent", "rootThreadId": "p"}});
    watcher.observe(&[thread("archived", "running", json!({})), thread("stopped", "running", json!({})), thread("child", "running", child.clone())]);
    let done = |extra: Value| {
        let mut base = json!({"latestRunCompletedAt": "2026-11-01T00:09:00.000Z"});
        for (key, field) in extra.as_object().into_iter().flatten() {
            base[key] = field.clone();
        }
        base
    };
    let next = watcher.observe(&[
        thread("archived", "completed", done(json!({"archivedAt": "2026-11-01T00:09:00.000Z"}))),
        thread("stopped", "completed", done(json!({"latestRunStopRequested": true}))),
        thread("child", "completed", done(child)),
    ]);
    assert!(next.is_empty(), "{next:?}");
    let failed = watcher.observe(&[thread("stopped", "failed", json!({"latestRunCompletedAt": "2026-11-01T00:10:00.000Z", "lastErrorClass": "usage_limit"}))]);
    assert_eq!(kinds(&failed), [("stopped", "Usage limit reached")]);
}
