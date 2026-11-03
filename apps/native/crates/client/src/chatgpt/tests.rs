use super::*;
use crate::{
    supervisor,
    test_server::{Connection, FakeServer},
};
use serde_json::json;

async fn reply(socket: &mut Connection, request: &Value, value: Value) {
    socket.send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":value}})).await;
}
fn request(install: auth::InstallState) -> SetupRequest {
    SetupRequest {
        instance: "codex".into(),
        method: "chatgpt".into(),
        environment: "remote-env".into(),
        return_url: "https://example.invalid/welcome#agents:remote-env".into(),
        local: false,
        helper: None,
        install,
    }
}
async fn client(server: &mut FakeServer) -> (ClientHandle, Connection, tokio::sync::mpsc::Receiver<supervisor::ClientEvent>) {
    let (client, events) = supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
    let mut socket = server.connection().await;
    let config = socket.request(t3_core::commands::method::SUBSCRIBE_SERVER_CONFIG).await;
    socket.chunk(&config, json!([{"type":"snapshot","config":{}}])).await;
    (client, socket, events)
}

#[tokio::test]
async fn setup_waits_for_its_install_then_its_auth_flow() {
    let mut server = FakeServer::start().await;
    let (client, mut socket, _events) = client(&mut server).await;
    let (tx, mut updates) = mpsc::unbounded_channel();
    let (_retry_tx, retry) = mpsc::unbounded_channel();
    let task = tokio::spawn(setup(client, request(auth::decode_install_state(json!({"phase":"idle"})).unwrap()), tx, retry));
    let install_stream = socket.request(auth::INSTALL_SUBSCRIBE).await;
    let install_start = socket.request(auth::INSTALL_START).await;
    reply(&mut socket, &install_start, json!({"phase":"downloading","operationId":"mine"})).await;
    socket
        .chunk(&install_stream, json!([{"phase":"idle"},{"phase":"succeeded","operationId":"previous","installedVersion":"1"},{"phase":"downloading","operationId":"previous"},{"phase":"succeeded","operationId":"mine","installedVersion":"1"}]))
        .await;
    let auth_stream = socket.request(auth::AUTH_SUBSCRIBE).await;
    let auth_start = socket.request(auth::AUTH_START).await;
    assert_eq!(auth_start["payload"]["callbackMode"], "client");
    assert_eq!(auth_start["payload"]["methodId"], "chatgpt");
    reply(&mut socket, &auth_start, json!({"phase":"starting","flowId":"mine"})).await;
    socket.chunk(&auth_stream,json!([{"phase":"waiting","flowId":"other","authorizationUrl":"https://example.invalid/other"},{"phase":"waiting","flowId":"mine","authorizationUrl":"https://example.invalid/mine"},{"phase":"succeeded","flowId":"mine"}])).await;
    assert!(task.await.unwrap().is_ok());
    let mut pages = Vec::new();
    let mut finished = false;
    let mut state = model::SetupState::default();
    while let Some(update) = updates.recv().await {
        update.apply_install(&mut state);
        if let Some(install) = &state.install {
            assert_eq!(install.operation_id.as_deref(), Some("mine"));
        }
        match update {
            Update::Page(page) => pages.push(page),
            Update::Finishing => finished = true,
            _ => {}
        }
    }
    assert_eq!(pages, vec!["https://example.invalid/mine"]);
    assert!(finished);
}

#[tokio::test]
async fn aborting_during_install_never_starts_auth_after_a_late_success() {
    let mut server = FakeServer::start().await;
    let (client, mut socket, _events) = client(&mut server).await;
    let (tx, _updates) = mpsc::unbounded_channel();
    let (_retry_tx, retry) = mpsc::unbounded_channel();
    let task = tokio::spawn(setup(client.clone(), request(auth::decode_install_state(json!({"phase":"idle"})).unwrap()), tx, retry));
    let stream = socket.request(auth::INSTALL_SUBSCRIBE).await;
    let start = socket.request(auth::INSTALL_START).await;
    reply(&mut socket, &start, json!({"phase":"downloading","operationId":"mine"})).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    socket.chunk(&stream, json!([{"phase":"succeeded","operationId":"mine","installedVersion":"1"}])).await;
    let probe = tokio::spawn(async move { client.request("fixture.marker", json!({})).await });
    loop {
        let frame = socket.recv().await;
        assert_ne!(frame["tag"], auth::AUTH_START);
        if frame["tag"] == "fixture.marker" {
            reply(&mut socket, &frame, Value::Null).await;
            break;
        }
    }
    assert!(probe.await.unwrap().is_ok());
}

#[tokio::test]
async fn finished_handoff_retries_import_without_repeating_sign_in() {
    let mut destination = FakeServer::start().await;
    let mut helper = FakeServer::start().await;
    let (client, mut target, _events) = client(&mut destination).await;
    let (tx, mut updates) = mpsc::unbounded_channel();
    let (retry_tx, retry) = mpsc::unbounded_channel();
    let mut input = request(auth::decode_install_state(json!({"phase":"idle","installedVersion":"1"})).unwrap());
    input.helper = Some(helper.credentials());
    let task = tokio::spawn(setup(client, input, tx, retry));
    let mut local = helper.connection().await;
    let reconnect = target.request(model::RECONNECT).await;
    reply(&mut target, &reconnect, Value::Null).await;
    let handoff = local.request(model::HANDOFF).await;
    assert_eq!(handoff["payload"]["environmentId"], "remote-env");
    assert_eq!(handoff["payload"]["profile"], Value::Null);
    let profile = json!({"registration":{"clientId":"oaiapp_fixture"},"credentials":{"clientId":"oaiapp_fixture","accessToken":"obviously-fake-access","refreshToken":null,"idToken":"obviously-fake-id","issuer":"https://example.invalid","expiresAt":1234.5,"earliestRefreshAt":null,"scopes":["openid"],"subject":"fake-subject","email":null}});
    local.chunk(&handoff, json!([{"phase":"finished","profile":profile}])).await;
    let first = target.request(model::IMPORT).await;
    target.send(json!({"_tag":"Exit","requestId":first["id"],"exit":{"_tag":"Failure","cause":[{"_tag":"Fail","error":{"detail":"Fixture import failed"}}]}})).await;
    loop {
        if matches!(updates.recv().await.unwrap(), Update::TransferFailed) {
            break;
        }
    }
    retry_tx.send(()).unwrap();
    let second = target.request(model::IMPORT).await;
    assert_eq!(first["payload"], second["payload"]);
    assert_eq!(second["payload"], json!({"instanceId":"codex","profile":profile}));
    reply(&mut target, &second, Value::Null).await;
    assert!(task.await.unwrap().is_ok());
    assert!(matches!(updates.recv().await, Some(Update::Finishing)));
    assert!(updates.recv().await.is_none());
}

#[tokio::test]
async fn unmanaged_choice_never_subscribes_to_managed_install_before_the_mode_update() {
    let mut server = FakeServer::start().await;
    let (client, mut socket, _events) = client(&mut server).await;
    let choice = tokio::spawn({
        let client = client.clone();
        async move {
            let mut streams = Vec::new();
            for method in model::managed_subscriptions(false) {
                streams.push(client.stream(method, auth::subscribe_payload("codex")).await.unwrap());
            }
            client.request("fixture.choice-ready", json!({})).await.unwrap();
            streams
        }
    });
    let mut rejections = 0;
    loop {
        let frame = socket.recv().await;
        if frame["tag"] == auth::INSTALL_SUBSCRIBE {
            rejections += 1;
            socket.send(json!({"_tag":"Exit","requestId":frame["id"],"exit":{"_tag":"Failure","cause":[{"_tag":"Fail","error":{"_tag":"ProviderSetupError","operation":"observe-install","detail":"Choose managed setup to install Codex in T3 Code."}}]}})).await;
        }
        if frame["tag"] == "fixture.choice-ready" {
            reply(&mut socket, &frame, Value::Null).await;
            break;
        }
    }
    drop(choice.await.unwrap());
    assert_eq!(rejections, 0, "the default choice must not hit the managed-only observe-install guard");
    let settings = t3_core::server_config::ServerSettings::default();
    let patch = model::mode_payload(&settings, "codex", true);
    let update = tokio::spawn({
        let client = client.clone();
        async move { client.request(t3_core::commands::method::SERVER_UPDATE_SETTINGS, patch).await }
    });
    let frame = socket.request(t3_core::commands::method::SERVER_UPDATE_SETTINGS).await;
    assert_eq!(frame["payload"]["providerInstanceMutation"]["instance"]["config"]["setupMode"], "managed");
    reply(&mut socket, &frame, json!({})).await;
    update.await.unwrap().unwrap();
    let subscriptions = tokio::spawn({
        let client = client.clone();
        async move {
            let mut streams = Vec::new();
            for method in model::managed_subscriptions(true) {
                streams.push(client.stream(method, auth::subscribe_payload("codex")).await.unwrap());
            }
            client.request("fixture.managed-ready", json!({})).await.unwrap();
            streams
        }
    });
    let mut observed = Vec::new();
    loop {
        let frame = socket.recv().await;
        if frame["_tag"] == "Request" {
            if frame["tag"] == "fixture.managed-ready" {
                reply(&mut socket, &frame, Value::Null).await;
                break;
            }
            observed.push(frame["tag"].as_str().unwrap().to_owned());
            if frame["tag"] == auth::INSTALL_SUBSCRIBE {
                socket.chunk(&frame, json!([{"phase":"idle","installedVersion":null}])).await;
            }
        }
    }
    assert_eq!(observed, vec![auth::AUTH_SUBSCRIBE, auth::INSTALL_SUBSCRIBE]);
    drop(subscriptions.await.unwrap());
}

#[tokio::test]
async fn cancelling_queued_setup_never_dispatches_install_or_auth_after_terminal_observations() {
    let mut server = FakeServer::start().await;
    let (client, mut socket, _events) = client(&mut server).await;
    for terminal in [json!({"phase":"cancelled","operationId":"mine"}), json!({"phase":"succeeded","operationId":"mine","installedVersion":"1"})] {
        let mut state = model::SetupState::default();
        state.started_install(auth::decode_install_state(json!({"phase":"downloading","operationId":"mine"})).unwrap());
        let mut automatic = true;
        let (method, payload) = state.cancel_local("codex", &mut automatic).unwrap();
        let cancel = tokio::spawn({
            let client = client.clone();
            async move { client.request(method, payload).await }
        });
        let frame = socket.request(auth::INSTALL_CANCEL).await;
        reply(&mut socket, &frame, terminal.clone()).await;
        state.install_cancel_reply(auth::decode_install_state(cancel.await.unwrap().unwrap()).unwrap());
        state.observe_install(auth::decode_install_state(terminal).unwrap());
        let task = tokio::spawn({
            let client = client.clone();
            async move {
                if state.auto_start_ready(automatic, true, true) {
                    let (tx, _updates) = mpsc::unbounded_channel();
                    let (_retry_tx, retry) = mpsc::unbounded_channel();
                    let _ = setup(client.clone(), request(state.install.unwrap()), tx, retry).await;
                }
                client.request("fixture.cancel-settled", json!({})).await.unwrap();
            }
        });
        let mut starts = Vec::new();
        loop {
            let frame = socket.recv().await;
            if frame["tag"] == auth::INSTALL_START {
                starts.push(auth::INSTALL_START);
                reply(&mut socket, &frame, json!({"phase":"failed","operationId":"unexpected"})).await;
            } else if frame["tag"] == auth::AUTH_START {
                starts.push(auth::AUTH_START);
                reply(&mut socket, &frame, json!({"phase":"failed","flowId":"unexpected"})).await;
            } else if frame["tag"] == "fixture.cancel-settled" {
                reply(&mut socket, &frame, Value::Null).await;
                break;
            }
        }
        task.await.unwrap();
        assert!(starts.is_empty(), "Cancel restarted setup: {starts:?}");
    }
}
