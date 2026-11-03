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
    answer_config(&mut second, None).await;
    let shell = second.request(method::SUBSCRIBE_SHELL).await;
    assert_eq!(shell["payload"]["afterSequence"], 40);
    let thread = second.request(method::SUBSCRIBE_THREAD).await;
    assert_eq!(thread["payload"]["afterSequence"], 6);
    assert_eq!(thread["payload"]["threadId"], "t1");
    second.chunk(&shell, json!([{"kind": "thread.removed", "sequence": 41, "threadId": "gone"}])).await;
    let removal = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::Shell(item)) = events.recv().await {
                break shell_sequence(&item);
            }
        }
    });
    assert_eq!(removal.await.expect("the removal"), Some(41));
    drop(second);

    // Rows trimmed under other options never come back as deltas: start from a fresh snapshot.
    let mut third = server.connection().await;
    answer_config(&mut third, Some(json!({"shellThreadFamilies": true, "shellOlderSettled": true}))).await;
    let shell = third.request(method::SUBSCRIBE_SHELL).await;
    assert_eq!(shell["payload"].get("afterSequence"), None);
    assert_eq!(shell["payload"]["omitIdleSubagents"], true);
    assert_eq!(shell["payload"]["omitOlderSettled"], true);
    let thread = third.request(method::SUBSCRIBE_THREAD).await;
    assert_eq!(thread["payload"]["afterSequence"], 6);
}


async fn next_dispatch(rx: &mut mpsc::UnboundedReceiver<Command>) -> (Value, oneshot::Sender<Result<Value, RpcError>>) {
    match rx.recv().await {
        Some(Command::Request { method: method::DISPATCH_COMMAND, payload, reply }) => (payload, reply),
        _ => panic!("expected a dispatch"),
    }
}

#[tokio::test]
async fn ordered_dispatch_waits_for_each_command_and_stops_at_a_failure() {
    let (commands, mut rx) = mpsc::unbounded_channel();
    let credentials = Arc::new(Credentials { http_base: "http://127.0.0.1:1".into(), access_token: "token".into() });
    let handle = ClientHandle { commands, credentials, session_state: watch::channel(None).1 };
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

#[tokio::test]
async fn dispatches_reach_the_server_in_call_order_with_requests_and_streams() {
    let mut server = FakeServer::start().await;
    let (client, _events) = start(&tokio::runtime::Handle::current(), server.credentials());
    let mut connection = server.connection().await;
    answer_config(&mut connection, None).await;
    connection.request(method::SUBSCRIBE_SHELL).await;

    let dispatch = client.dispatch(json!({"type": "thread.turn.interrupt"}));
    let request = client.request("terminal.write", json!({"data": "a"}));
    let stream = client.stream("terminal.attach", json!({}));
    let received = tokio::spawn(async move {
        let mut tags = Vec::new();
        while tags.len() < 3 {
            let message = connection.recv().await;
            let tag = message["tag"].as_str().unwrap_or_default().to_owned();
            if message["_tag"] == "Request" {
                tags.push(tag);
            }
        }
        tags
    });
    tokio::select! {
        tags = received => assert_eq!(tags.expect("reader"), [method::DISPATCH_COMMAND, "terminal.write", "terminal.attach"]),
        _ = async { tokio::join!(stream, request, dispatch) } => unreachable!("the fake server never replies"),
    }
}

async fn next_grants(grants: &mut watch::Receiver<Option<AuthSessionState>>) -> AuthSessionState {
    tokio::time::timeout(Duration::from_secs(10), grants.changed()).await.expect("grants arrive").expect("open");
    grants.borrow_and_update().clone().expect("grants")
}

#[tokio::test]
async fn each_connection_fetches_the_grants_and_a_refresh_picks_up_changes() {
    let mut server = FakeServer::start().await;
    let (client, _events) = start(&tokio::runtime::Handle::current(), server.credentials());
    let mut grants = client.session_state();
    assert!(grants.borrow().is_none());
    let mut connection = server.connection().await;
    assert!(next_grants(&mut grants).await.grants("terminal:operate"));
    answer_config(&mut connection, None).await;
    connection.request(method::SUBSCRIBE_SHELL).await;

    server.set_session(json!({"authenticated": true, "auth": {}, "permissions": ["terminal:read"]}));
    client.refresh_session_state();
    let state = next_grants(&mut grants).await;
    assert!(state.grants("terminal:read") && !state.grants("terminal:operate"));
}

#[tokio::test]
async fn streams_deliver_chunks_and_interrupt_when_dropped() {
    let mut server = FakeServer::start().await;
    let (client, _events) = start(&tokio::runtime::Handle::current(), server.credentials());
    let mut connection = server.connection().await;
    answer_config(&mut connection, None).await;
    connection.request(method::SUBSCRIBE_SHELL).await;

    let mut stream = client.stream("subscribeVcsStatus", json!({ "cwd": "/repo" })).await.expect("stream opens");
    let request = connection.request("subscribeVcsStatus").await;
    assert_eq!(request["payload"]["cwd"], "/repo");
    connection.chunk(&request, json!([{ "_tag": "snapshot", "local": {}, "remote": null }])).await;
    let value = tokio::time::timeout(Duration::from_secs(10), stream.receiver.recv()).await.expect("a chunk").expect("open").expect("ok");
    assert_eq!(value["_tag"], "snapshot");

    drop(stream);
    loop {
        let message = connection.recv().await;
        if message["_tag"] == "Interrupt" {
            assert_eq!(message["requestId"], request["id"]);
            break;
        }
    }
}

async fn next_status(events: &mut mpsc::Receiver<ClientEvent>) -> ConnectionStatus {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv()).await.expect("an event arrives").expect("open");
        if let ClientEvent::Status(status) = event {
            return status;
        }
    }
}

#[tokio::test]
async fn a_server_on_another_protocol_blocks_until_retried() {
    let mut server = FakeServer::start().await;
    let newer = t3_core::compatibility::ORCHESTRATION_PROTOCOL_VERSION + 1;
    server.set_descriptor(Some(json!({ "environmentId": "env", "label": "Studio", "serverVersion": "9.0.0", "orchestrationProtocolVersion": newer })));
    let (client, mut events) = start(&tokio::runtime::Handle::current(), server.credentials());
    assert_eq!(next_status(&mut events).await, ConnectionStatus::Connecting);
    let ConnectionStatus::Unsupported { reason, server_outdated: false } = next_status(&mut events).await else { panic!("blocked as unsupported") };
    assert!(reason.contains("Studio"));

    // Fixed on the server: Retry connects, still reading as a recovery from that failure.
    server.set_descriptor(Some(json!({ "environmentId": "env", "label": "Studio", "serverVersion": "9.0.0", "orchestrationProtocolVersion": newer - 1 })));
    client.retry_now();
    assert!(matches!(next_status(&mut events).await, ConnectionStatus::Reconnecting { reason: last, .. } if last == reason));
    server.connection().await;
    assert_eq!(next_status(&mut events).await, ConnectionStatus::Connected);
}

#[tokio::test]
async fn a_socket_refused_for_its_protocol_blocks_instead_of_retrying() {
    let mut server = FakeServer::start().await;
    // No descriptor to check first: the socket's 426 is the only signal.
    let newer = t3_core::compatibility::ORCHESTRATION_PROTOCOL_VERSION + 1;
    let body = json!({"code": "orchestration_protocol_incompatible", "message": "Update this client.", "orchestrationProtocolVersion": newer});
    server.refuse_upgrades(Some((426, body)));
    let (client, mut events) = start(&tokio::runtime::Handle::current(), server.credentials());
    assert_eq!(next_status(&mut events).await, ConnectionStatus::Connecting);
    let ConnectionStatus::Unsupported { reason, server_outdated: false } = next_status(&mut events).await else { panic!("blocked as unsupported") };
    assert!(reason.starts_with("This client is not supported by this server."), "{reason}");

    // Nothing retries on its own; Retry checks again.
    server.refuse_upgrades(None);
    client.retry_now();
    assert!(matches!(next_status(&mut events).await, ConnectionStatus::Reconnecting { reason: last, .. } if last == reason));
    server.connection().await;
    assert_eq!(next_status(&mut events).await, ConnectionStatus::Connected);
}

#[tokio::test]
async fn a_prepared_route_is_readied_before_every_attempt_and_a_blocked_one_waits_for_retry() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut server = FakeServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let outcomes = Arc::new(Mutex::new(vec![Err(PrepareError::Blocked("SSH authentication cancelled for devbox.".to_owned()))]));
    let prepare: Prepare = {
        let (calls, outcomes) = (calls.clone(), outcomes.clone());
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            let outcome = outcomes.lock().expect("outcomes").pop().unwrap_or(Ok(()));
            Box::pin(async move { outcome })
        })
    };
    let (client, mut events) = start_prepared(&tokio::runtime::Handle::current(), server.credentials(), Some(prepare));
    assert_eq!(next_status(&mut events).await, ConnectionStatus::Connecting);
    let blocked = ConnectionStatus::Blocked { reason: "SSH authentication cancelled for devbox.".to_owned() };
    assert_eq!(next_status(&mut events).await, blocked);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Nothing retries on its own; Retry prepares again and connects.
    client.retry_now();
    assert!(matches!(next_status(&mut events).await, ConnectionStatus::Reconnecting { reason, attempt: 1 } if reason.contains("cancelled")));
    let first = server.connection().await;
    assert_eq!(next_status(&mut events).await, ConnectionStatus::Connected);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // A dropped session prepares the route again before reconnecting.
    drop(first);
    server.connection().await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_route_that_moved_stops_the_client_and_says_where() {
    let server = FakeServer::start().await;
    let prepare: Prepare = Arc::new(|| Box::pin(async { Err(PrepareError::Moved("http://127.0.0.1:52002".to_owned())) }));
    let (_client, mut events) = start_prepared(&tokio::runtime::Handle::current(), server.credentials(), Some(prepare));
    assert_eq!(next_status(&mut events).await, ConnectionStatus::Connecting);
    let moved = tokio::time::timeout(Duration::from_secs(10), events.recv()).await.expect("an event arrives");
    assert!(matches!(moved, Some(ClientEvent::Moved { ref http_base }) if http_base == "http://127.0.0.1:52002"), "{moved:?}");
    assert!(tokio::time::timeout(Duration::from_secs(10), events.recv()).await.expect("ends").is_none(), "the client stopped");
}
