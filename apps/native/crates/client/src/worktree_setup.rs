//! The open thread's worktree setup progress: `subscribeWorktreeSetup`, as the
//! `vcsEnvironment.worktreeSetup` subscription in `packages/client-runtime/src/state/vcs.ts` runs it.
//! Every event is a whole snapshot, or null once the server forgets it. Events are decoded and
//! folded into a `Feed` here on the network runtime, and the UI reads only the newest feed from a
//! `watch`, so a slow UI never queues snapshots. A dropped connection resubscribes (the server
//! starts over with its current snapshot) and keeps what was shown; a server failure ends it.
//! Dropping the handle interrupts it.

use std::sync::Arc;
use std::time::Duration;

use t3_core::worktree_setup::{Feed, decode_event, method, thread_request};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::session::RpcError;
use crate::supervisor::ClientHandle;

pub struct WorktreeSetupStream {
    pub feed: watch::Receiver<Feed>,
    task: JoinHandle<()>,
}

impl Drop for WorktreeSetupStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Follows `thread_id`'s setup, starting from `seed` so a snapshot held from an earlier
/// subscription still shows until a newer one arrives.
pub fn subscribe(runtime: &tokio::runtime::Handle, client: ClientHandle, thread_id: &str, seed: Feed) -> WorktreeSetupStream {
    let (tx, feed) = watch::channel(seed);
    let task = runtime.spawn(run(client, thread_request(thread_id), tx));
    WorktreeSetupStream { feed, task }
}

async fn run(client: ClientHandle, payload: serde_json::Value, tx: watch::Sender<Feed>) {
    let mut attempt = 0u32;
    loop {
        match client.stream(method::SUBSCRIBE, payload.clone()).await {
            Ok(mut stream) => {
                while let Some(first) = stream.receiver.recv().await {
                    // Fold everything already queued before publishing once.
                    let mut batch = vec![first];
                    while let Ok(next) = stream.receiver.try_recv() {
                        batch.push(next);
                    }
                    let mut events = Vec::with_capacity(batch.len());
                    let mut lost = false;
                    for item in batch {
                        match item {
                            Ok(value) => match decode_event(value) {
                                Ok(event) => events.push(event.map(Arc::new)),
                                Err(error) => tracing::warn!(%error, "undecodable worktree setup snapshot"),
                            },
                            Err(RpcError::Failure(failure)) => {
                                tracing::warn!(message = %failure.message, "worktree setup subscription failed");
                                return;
                            }
                            Err(RpcError::Closed) => {
                                lost = true;
                                break;
                            }
                        }
                    }
                    if !events.is_empty() {
                        attempt = 0;
                        tx.send_if_modified(|feed| {
                            let before = feed.clone();
                            for event in events {
                                feed.apply(event);
                            }
                            *feed != before
                        });
                    }
                    if lost {
                        break;
                    }
                }
            }
            Err(RpcError::Failure(failure)) => {
                tracing::warn!(message = %failure.message, "worktree setup subscription failed");
                return;
            }
            Err(RpcError::Closed) => {}
        }
        if tx.is_closed() {
            return;
        }
        attempt += 1;
        tokio::time::sleep(Duration::from_millis((250u64 << attempt.min(5)).min(5_000))).await;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use t3_core::commands::method as commands;
    use t3_core::worktree_setup::Phase;

    use super::*;
    use crate::supervisor::start;
    use crate::test_server::{Connection, FakeServer};

    const WAIT: Duration = Duration::from_secs(10);

    async fn connect(server: &mut FakeServer) -> Connection {
        let mut connection = server.connection().await;
        let config = connection.request(commands::SUBSCRIBE_SERVER_CONFIG).await;
        let failure = json!({"_tag": "Failure", "cause": [{"_tag": "Fail", "error": {"message": "no config"}}]});
        connection.send(json!({"_tag": "Exit", "requestId": config["id"], "exit": failure})).await;
        connection.request(commands::SUBSCRIBE_SHELL).await;
        connection
    }

    fn snapshot(phase: &str, sequence: u64) -> Value {
        json!({
            "threadId": "t1",
            "phase": phase,
            "startedAt": "2026-10-26T12:00:00.000Z",
            "endedAt": null,
            "branch": "t3/feature",
            "baseRef": "main",
            "worktreePath": null,
            "setupScript": null,
            "stages": [{ "id": "checkout", "status": "running", "startedAt": "2026-10-26T12:00:00.000Z", "endedAt": null, "percent": 12, "detail": null, "tail": [] }],
            "error": null,
            "sequence": sequence,
            "addedLater": [1, 2, 3],
        })
    }

    async fn next(feed: &mut watch::Receiver<Feed>) -> Feed {
        tokio::time::timeout(WAIT, feed.changed()).await.expect("an update").expect("the stream is live");
        feed.borrow_and_update().clone()
    }

    type Shown = Option<(Phase, u64)>;

    fn shown(feed: &Feed) -> (Shown, Shown) {
        let summary = |snapshot: &Option<Arc<t3_core::worktree_setup::Snapshot>>| snapshot.as_ref().map(|s| (s.phase, s.sequence));
        (summary(&feed.latest), summary(&feed.held))
    }

    #[tokio::test]
    async fn folds_snapshots_and_holds_the_last_one_after_the_server_drops_it() {
        let mut server = FakeServer::start().await;
        let (client, _events) = start(&tokio::runtime::Handle::current(), server.credentials());
        let mut connection = connect(&mut server).await;
        let mut stream = subscribe(&tokio::runtime::Handle::current(), client, "t1", Feed::default());
        let request = connection.request(method::SUBSCRIBE).await;
        assert_eq!(request["payload"], json!({ "threadId": "t1" }));

        connection.chunk(&request, json!([snapshot("running", 1), snapshot("done", 2)])).await;
        assert_eq!(shown(&next(&mut stream.feed).await), (Some((Phase::Done, 2)), Some((Phase::Done, 2))));
        // A frame this client cannot read is skipped; the stream carries on.
        connection.chunk(&request, json!(["not a snapshot", Value::Null])).await;
        assert_eq!(shown(&next(&mut stream.feed).await), (None, Some((Phase::Done, 2))));
    }

    #[tokio::test]
    async fn a_lost_connection_resubscribes_and_keeps_what_was_shown() {
        let mut server = FakeServer::start().await;
        let (client, _events) = start(&tokio::runtime::Handle::current(), server.credentials());
        let mut first = connect(&mut server).await;
        let mut stream = subscribe(&tokio::runtime::Handle::current(), client, "t1", Feed::default());
        let request = first.request(method::SUBSCRIBE).await;
        first.chunk(&request, json!([snapshot("running", 3)])).await;
        assert_eq!(shown(&next(&mut stream.feed).await), (Some((Phase::Running, 3)), Some((Phase::Running, 3))));
        drop(first);

        let mut second = connect(&mut server).await;
        let request = second.request(method::SUBSCRIBE).await;
        assert_eq!(shown(&stream.feed.borrow()), (Some((Phase::Running, 3)), Some((Phase::Running, 3))));
        second.chunk(&request, json!([snapshot("failed", 4)])).await;
        assert_eq!(shown(&next(&mut stream.feed).await), (Some((Phase::Failed, 4)), Some((Phase::Failed, 4))));

        drop(stream);
        loop {
            let message = second.recv().await;
            if message["_tag"] == "Interrupt" {
                assert_eq!(message["requestId"], request["id"]);
                break;
            }
        }
    }

    #[tokio::test]
    async fn a_server_failure_ends_the_subscription() {
        let mut server = FakeServer::start().await;
        let (client, _events) = start(&tokio::runtime::Handle::current(), server.credentials());
        let mut connection = connect(&mut server).await;
        let mut stream = subscribe(&tokio::runtime::Handle::current(), client, "t1", Feed::default());
        let request = connection.request(method::SUBSCRIBE).await;
        let error = json!({"_tag": "EnvironmentAuthorizationError", "message": "Not allowed."});
        connection.send(json!({"_tag": "Exit", "requestId": request["id"], "exit": {"_tag": "Failure", "cause": [{"_tag": "Fail", "error": error}]}})).await;
        assert!(tokio::time::timeout(WAIT, stream.feed.changed()).await.expect("the task ends").is_err());
    }
}
