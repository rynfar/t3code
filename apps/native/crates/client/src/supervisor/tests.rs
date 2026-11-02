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

/// The shell waits for the first config snapshot, whose capabilities decide its options.
async fn answer_config(connection: &mut crate::test_server::Connection, config: Option<Value>) {
    let request = connection.request(method::SUBSCRIBE_SERVER_CONFIG).await;
    match config {
        Some(config) => connection.chunk(&request, json!([{"type": "snapshot", "config": config}])).await,
        None => {
            let failure = json!({"_tag": "Failure", "cause": [{"_tag": "Fail", "error": {"message": "no config"}}]});
            connection.send(json!({"_tag": "Exit", "requestId": request["id"], "exit": failure})).await;
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
    answer_config(&mut first, None).await;
    let shell = first.request(method::SUBSCRIBE_SHELL).await;
    assert_eq!(shell["payload"].get("afterSequence"), None);
    assert_eq!(shell["payload"].get("omitIdleSubagents"), None, "a failed config asks for every row");
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
    answer_config(&mut second, Some(json!({"shellThreadFamilies": true, "shellOlderSettled": true}))).await;
    let shell = second.request(method::SUBSCRIBE_SHELL).await;
    assert_eq!(shell["payload"]["afterSequence"], 40);
    assert_eq!(shell["payload"]["omitIdleSubagents"], true);
    assert_eq!(shell["payload"]["omitOlderSettled"], true);
    let thread = second.request(method::SUBSCRIBE_THREAD).await;
    assert_eq!(thread["payload"]["afterSequence"], 6);
    assert_eq!(thread["payload"]["threadId"], "t1");
}


async fn next_dispatch(rx: &mut mpsc::UnboundedReceiver<Command>) -> (Value, oneshot::Sender<Result<Value, RpcError>>) {
    match rx.recv().await {
        Some(Command::Dispatch { command, reply }) => (command, reply),
        _ => panic!("expected a dispatch"),
    }
}

#[tokio::test]
async fn ordered_dispatch_waits_for_each_command_and_stops_at_a_failure() {
    let (commands, mut rx) = mpsc::unbounded_channel();
    let credentials = Arc::new(Credentials { http_base: "http://127.0.0.1:1".into(), access_token: "token".into() });
    let handle = ClientHandle { commands, credentials };
    let sends = vec![json!({"n": 1}), json!({"n": 2}), json!({"n": 3})];
    let run = tokio::spawn(async move { handle.dispatch_in_order(sends).await });

    let (first, reply) = next_dispatch(&mut rx).await;
    assert_eq!(first["n"], 1);
    tokio::task::yield_now().await;
    assert!(rx.try_recv().is_err(), "the second command went out before the first committed");
    let _ = reply.send(Ok(json!({"sequence": 1})));

    let (second, reply) = next_dispatch(&mut rx).await;
    assert_eq!(second["n"], 2);
    let _ = reply.send(Err(RpcError::Closed));

    let result = run.await.unwrap();
    assert!(matches!(result, Err((1, RpcError::Closed))));
    assert!(rx.try_recv().is_err(), "a command after the failure still went out");
}
