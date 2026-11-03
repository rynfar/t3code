use std::time::Duration;

use serde_json::json;

use super::*;
use crate::test_server::FakeServer;

async fn connect(server: &FakeServer, timing: Timing) -> Session {
    Session::connect_with(&server.ws_url(), timing).await.expect("connects")
}

async fn next(stream: &mut StreamHandle) -> Option<Result<Value, RpcError>> {
    tokio::time::timeout(Duration::from_secs(10), stream.receiver.recv()).await.expect("stream delivers")
}

#[tokio::test]
async fn batched_frames_deliver_in_order_and_exit_ends_the_stream() {
    let mut server = FakeServer::start().await;
    let session = connect(&server, TIMING).await;
    let mut stream = session.stream("orchestration.subscribeShell", json!({})).unwrap();
    let mut socket = server.connection().await;
    let request = socket.request("orchestration.subscribeShell").await;
    let id = request["id"].clone();
    socket
        .send(json!([
            {"_tag": "Chunk", "requestId": id, "values": [1, 2]},
            {"_tag": "Pong"},
            {"_tag": "Chunk", "requestId": id, "values": [3]},
            {"_tag": "Exit", "requestId": id, "exit": {"_tag": "Success", "value": null}}
        ]))
        .await;
    for expected in [1, 2, 3] {
        assert_eq!(next(&mut stream).await.unwrap().unwrap(), json!(expected));
    }
    assert!(next(&mut stream).await.is_none());
}

#[tokio::test]
async fn a_chunk_is_acked_only_once_its_values_fit_the_consumer_queue() {
    let mut server = FakeServer::start().await;
    let session = connect(&server, Timing { stream_queue: 1, ..TIMING }).await;
    let mut stream = session.stream("orchestration.subscribeThread", json!({})).unwrap();
    let mut socket = server.connection().await;
    let request = socket.request("orchestration.subscribeThread").await;
    socket.chunk(&request, json!(["a", "b", "c"])).await;

    // "a" is taken and "b" fills the queue, so "c" and the ack wait on the consumer.
    assert_eq!(next(&mut stream).await.unwrap().unwrap(), json!("a"));
    let probe = session.clone();
    let unary = tokio::spawn(async move { probe.request("server.probe", json!({})).await });
    let sent_first = socket.recv().await;
    assert_eq!(sent_first["tag"], "server.probe", "no ack while values wait: {sent_first}");

    assert_eq!(next(&mut stream).await.unwrap().unwrap(), json!("b"));
    let ack = socket.recv().await;
    assert_eq!(ack, json!({"_tag": "Ack", "requestId": request["id"]}));
    assert_eq!(next(&mut stream).await.unwrap().unwrap(), json!("c"));

    socket.send(json!({"_tag": "Exit", "requestId": sent_first["id"], "exit": {"_tag": "Success", "value": 7}})).await;
    assert_eq!(unary.await.unwrap().unwrap(), json!(7));
}

#[tokio::test]
async fn dropping_a_stream_interrupts_it_and_ignores_later_chunks() {
    let mut server = FakeServer::start().await;
    let session = connect(&server, TIMING).await;
    let stream = session.stream("orchestration.subscribeThread", json!({})).unwrap();
    let mut socket = server.connection().await;
    let request = socket.request("orchestration.subscribeThread").await;
    drop(stream);
    assert_eq!(socket.recv().await, json!({"_tag": "Interrupt", "requestId": request["id"]}));
    socket.chunk(&request, json!(["late"])).await;

    let reply = tokio::spawn({
        let session = session.clone();
        async move { session.request("server.probe", json!({})).await }
    });
    let probe = socket.request("server.probe").await;
    socket.send(json!({"_tag": "Exit", "requestId": probe["id"], "exit": {"_tag": "Success", "value": "ok"}})).await;
    assert_eq!(reply.await.unwrap().unwrap(), json!("ok"));
}

#[tokio::test]
async fn a_defect_fails_every_pending_call_with_the_server_error_and_closes() {
    let mut server = FakeServer::start().await;
    let session = connect(&server, TIMING).await;
    let mut stream = session.stream("orchestration.subscribeThread", json!({})).unwrap();
    let unary = tokio::spawn({
        let session = session.clone();
        async move { session.request("orchestration.dispatchCommand", json!({})).await }
    });
    let mut socket = server.connection().await;
    socket.request("orchestration.dispatchCommand").await;
    socket.send(json!({"_tag": "Defect", "defect": {"message": "projection store unavailable"}})).await;

    let expected = "Server error: projection store unavailable";
    let Err(RpcError::Failure(failure)) = unary.await.unwrap() else { panic!("unary call should fail") };
    assert_eq!(failure.message, expected);
    let Some(Err(RpcError::Failure(failure))) = next(&mut stream).await else { panic!("stream should fail") };
    assert_eq!(failure.message, expected);
    assert_eq!(session.closed().await, expected);
    assert!(matches!(session.request("server.probe", json!({})).await, Err(RpcError::Closed)));
}

#[tokio::test]
async fn a_silent_server_times_out() {
    let fast = Timing { ping_interval: Duration::from_millis(20), ping_timeout: Duration::from_millis(80), ..TIMING };
    let mut server = FakeServer::start().await;
    let session = connect(&server, fast).await;
    let _socket = server.connection().await;
    assert!(session.closed().await.starts_with("The server stopped responding"));
}

#[tokio::test]
async fn chatgpt_reconnect_import_and_handoff_match_the_wire_contract() {
    use t3_core::settings::providers::chatgpt as gpt;
    let mut server = FakeServer::start().await;
    let session = connect(&server, TIMING).await;
    let mut socket = server.connection().await;
    let profile =
        json!({"clientId":"oaiapp_fixture","subject":"fake-subject","email":"fake@example.invalid","redirectUri":"http://127.0.0.1:14399/auth/callback","connectionLabel":"Fixture","sharingEnabled":true,"idTokenHint":"obviously-fake-hint"});
    let transfer = json!({"registration":profile,"credentials":{"clientId":"oaiapp_fixture","accessToken":"obviously-fake-access","refreshToken":null,"idToken":"obviously-fake-id","issuer":"https://example.invalid","expiresAt":1234.5,"earliestRefreshAt":null,"scopes":["openid"],"subject":"fake-subject","email":"fake@example.invalid"}});
    let request = tokio::spawn({
        let session = session.clone();
        async move { session.request(gpt::RECONNECT, gpt::reconnect_payload("codex_work", "chatgpt-profile:fake")).await }
    });
    let wire = socket.request(gpt::RECONNECT).await;
    assert_eq!(wire["payload"], json!({"instanceId":"codex_work","methodId":"chatgpt-profile:fake"}));
    socket.send(json!({"_tag":"Exit","requestId":wire["id"],"exit":{"_tag":"Success","value":profile}})).await;
    let decoded: Option<gpt::ReconnectProfile> = serde_json::from_value(request.await.unwrap().unwrap()).unwrap();
    assert_eq!(decoded.as_ref().unwrap().email.as_deref(), Some("fake@example.invalid"));
    let mut stream = session.stream(gpt::HANDOFF, gpt::handoff_payload("codex_work", "remote-env", "attempt-1", "https://example.invalid/welcome#agents:remote-env", decoded.as_ref())).unwrap();
    let wire = socket.request(gpt::HANDOFF).await;
    assert_eq!(wire["payload"], json!({"instanceId":"codex_work","environmentId":"remote-env","attemptId":"attempt-1","returnUrl":"https://example.invalid/welcome#agents:remote-env","profile":profile}));
    socket.chunk(&wire, json!([{"phase":"auth","state":{"phase":"waiting","flowId":"fake-flow","authorizationUrl":"https://example.invalid/fake","methods":[]}},{"phase":"finished","profile":transfer}])).await;
    let gpt::HandoffState::Auth { state } = serde_json::from_value(next(&mut stream).await.unwrap().unwrap()).unwrap() else { panic!("auth state") };
    assert_eq!(state.url(), Some("https://example.invalid/fake"));
    let gpt::HandoffState::Finished { profile: transferred } = serde_json::from_value(next(&mut stream).await.unwrap().unwrap()).unwrap() else { panic!("finished profile") };
    assert_eq!(transferred.credentials.access_token, "obviously-fake-access");
    let import = gpt::import_payload("codex_work", &transferred);
    assert_eq!(import, json!({"instanceId":"codex_work","profile":transfer}));
    let request = tokio::spawn({
        let session = session.clone();
        async move { session.request(gpt::IMPORT, import).await }
    });
    let wire = socket.request(gpt::IMPORT).await;
    assert_eq!(wire["payload"], json!({"instanceId":"codex_work","profile":transfer}));
    socket.send(json!({"_tag":"Exit","requestId":wire["id"],"exit":{"_tag":"Success","value":null}})).await;
    assert_eq!(request.await.unwrap().unwrap(), Value::Null);
    let request = tokio::spawn({
        let session = session.clone();
        async move { session.request(gpt::RECONNECT, gpt::reconnect_payload("codex_work", "chatgpt-change-account")).await }
    });
    let wire = socket.request(gpt::RECONNECT).await;
    socket.send(json!({"_tag":"Exit","requestId":wire["id"],"exit":{"_tag":"Success","value":null}})).await;
    let absent: Option<gpt::ReconnectProfile> = serde_json::from_value(request.await.unwrap().unwrap()).unwrap();
    assert!(absent.is_none());
    assert_eq!(gpt::handoff_payload("codex_work", "remote-env", "attempt-2", "https://example.invalid", None)["profile"], Value::Null);
}
