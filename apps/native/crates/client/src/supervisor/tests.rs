use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::test_server::FakeServer;

const T: &str = "2026-11-01T00:00:00.000Z";

fn projection(thread_id: &str) -> Value {
    json!({
        "thread": {
            "id": thread_id, "projectId": "p1", "title": "Thread", "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
            "interactionMode": "default", "branch": null, "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": thread_id},
            "createdAt": T, "updatedAt": T, "archivedAt": null
        },
        "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [], "plans": [],
        "turnItems": [], "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [],
        "contextTransfers": [], "visibleTurnItems": [], "updatedAt": T
    })
}

async fn next_thread_sequence(events: &mut mpsc::Receiver<ClientEvent>) -> Option<u64> {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv()).await.expect("an event").expect("open");
        if let ClientEvent::Thread { item, .. } = event {
            return thread_sequence(&item);
        }
    }
}

#[tokio::test]
async fn reconnect_resumes_every_stream_after_the_last_sequence_it_delivered() {
    let mut server = FakeServer::start().await;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), server.credentials());
    client.open_thread("t1");

    let mut first = server.connection().await;
    assert!(first.path.contains("wsTicket=ticket"), "{}", first.path);
    let shell = first.request(method::SUBSCRIBE_SHELL).await;
    assert_eq!(shell["payload"].get("afterSequence"), None);
    let thread = first.request(method::SUBSCRIBE_THREAD).await;
    assert_eq!(thread["payload"].get("afterSequence"), None);
    assert_eq!(thread["payload"]["acceptBoundedSnapshot"], true);

    first
        .chunk(&shell, json!([{"kind": "snapshot", "snapshot": {"snapshotSequence": 40, "projects": [], "threads": []}}]))
        .await;
    first
        .chunk(
            &thread,
            json!([
                {"kind": "snapshot", "snapshotSequence": 5, "projection": projection("t1"), "hasMoreHistory": false},
                {"kind": "event", "sequence": 6, "event": {"type": "brand.new", "payload": {}}}
            ]),
        )
        .await;
    assert_eq!(next_thread_sequence(&mut events).await, Some(5));
    assert_eq!(next_thread_sequence(&mut events).await, Some(6));
    drop(first);

    let mut second = server.connection().await;
    let shell = second.request(method::SUBSCRIBE_SHELL).await;
    assert_eq!(shell["payload"]["afterSequence"], 40);
    let thread = second.request(method::SUBSCRIBE_THREAD).await;
    assert_eq!(thread["payload"]["afterSequence"], 6);
    assert_eq!(thread["payload"]["threadId"], "t1");
}
