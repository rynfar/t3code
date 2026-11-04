use super::*;
use crate::test_server::{Connection, FakeServer};
use serde_json::{Value, json};
use t3_core::commands::method;

async fn shell(connection: &mut Connection) -> Value {
    let config = connection.request(method::SUBSCRIBE_SERVER_CONFIG).await;
    connection
        .chunk(&config, json!([{"type":"snapshot","config":{}}]))
        .await;
    connection.request(method::SUBSCRIBE_SHELL).await
}

async fn activated(
    events: &mut mpsc::Receiver<EnvironmentEvent>,
    expected: &str,
) -> (ClientHandle, u64) {
    loop {
        match events.recv().await.unwrap() {
            EnvironmentEvent::Activated {
                id,
                client,
                generation,
                ..
            } => {
                assert_eq!(id, expected);
                return (client, generation);
            }
            EnvironmentEvent::Notifications { .. } => panic!("selection replayed a notification"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn two_environments_share_their_existing_supervisors_with_the_active_view() {
    let (mut a, mut b) = (FakeServer::start().await, FakeServer::start().await);
    let (mut owner, mut events) = Environments::start(&tokio::runtime::Handle::current(), true);
    owner.watch("a", a.credentials(), None);
    owner.watch("b", b.credentials(), None);
    owner.activate(Some("a"));
    let (_, a_epoch) = activated(&mut events, "a").await;
    let (mut ca, mut cb) = (a.connection().await, b.connection().await);
    let sa = shell(&mut ca).await;
    let sb = shell(&mut cb).await;
    let row = |status, completed: Option<&str>| {
        json!({
            "id":"same", "projectId":"p", "title":"Background thread", "providerInstanceId":"replay",
            "modelSelection":{"instanceId":"replay","model":"replay"},"runtimeMode":"auto","interactionMode":"default",
            "lineage":{"rootThreadId":"same"}, "status":status, "latestRunId":"r", "latestRunCompletedAt":completed,
            "hasActionableProposedPlan":false,"itemCount":1,"visibleItemCount":1, "createdAt":"2026-11-01T00:00:00Z", "updatedAt":"2026-11-01T00:00:00Z"
        })
    };
    let snapshot = |row| json!({"kind":"snapshot","snapshot":{"snapshotSequence":1,"projects":[],"threads":[row]}});
    ca.chunk(
        &sa,
        json!([snapshot(row("running", None)),{"kind":"synchronized"}]),
    )
    .await;
    cb.chunk(
        &sb,
        json!([snapshot(row("running", None)),{"kind":"synchronized"}]),
    )
    .await;
    // Visibility is the reduction barrier: both live shells were observed, no UI batch for b.
    loop {
        match events.recv().await.unwrap() {
            EnvironmentEvent::Visibility(v) if v.live.len() == 2 => break,
            EnvironmentEvent::Active { id, .. } => assert_eq!(id, "a"),
            _ => {}
        }
    }
    cb.chunk(&sb, json!([{"kind":"thread.updated","sequence":2,"location":"active","thread":row("completed",Some("2026-11-01T00:01:00Z"))}])).await;
    loop {
        match events.recv().await.unwrap() {
            EnvironmentEvent::Notifications {
                id, notifications, ..
            } => {
                assert_eq!(id, "b");
                assert_eq!(notifications.len(), 1);
                assert_eq!(notifications[0].thread_id, "same");
                break;
            }
            EnvironmentEvent::Active { id, .. } => assert_eq!(id, "a"),
            _ => {}
        }
    }
    owner.watch("b", b.credentials(), None);
    owner.activate(Some("b"));
    let (client, b_epoch) = activated(&mut events, "b").await;
    let rpc = client.request("fixture.read", json!({}));
    let request = cb.request("fixture.read").await;
    cb.send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":{"sameSocket":true}}})).await;
    assert_eq!(rpc.await.unwrap()["sameSocket"], true);
    owner.activate(Some("a"));
    assert_eq!(activated(&mut events, "a").await.1, a_epoch);
    owner.activate(Some("b"));
    assert_eq!(activated(&mut events, "b").await.1, b_epoch);
    owner.remove("a");
    loop {
        if let EnvironmentEvent::Visibility(v) = events.recv().await.unwrap() {
            assert_eq!(v.live, [ThreadKey::new("b", "same")].into());
            break;
        }
    }
    ca.closed().await;
    drop(cb);
    let mut cb = b.connection().await;
    let sb = shell(&mut cb).await;
    cb.chunk(
        &sb,
        json!([snapshot(row("completed",Some("2026-11-01T00:01:00Z"))), {"kind":"synchronized"}]),
    )
    .await;
    loop {
        match events.recv().await.unwrap() {
            EnvironmentEvent::Notifications { .. } => panic!("reconnect replay notified again"),
            EnvironmentEvent::Active { events, .. }
                if events.iter().any(|e| {
                    matches!(
                        e,
                        ClientEvent::Shell(t3_core::shell::ShellStreamItem::Synchronized)
                    )
                }) =>
            {
                break;
            }
            _ => {}
        }
    }
    owner.activate(Some("b"));
    assert_eq!(activated(&mut events, "b").await.1, b_epoch);
    // Events already queued for a removed lease cannot revoke or notify a new pairing.
    owner.remove("b");
    assert!(!owner.is_current(&EnvironmentEvent::Status {
        id: "b".into(),
        generation: b_epoch,
        status: ConnectionStatus::NeedsPairing {
            reason: "expired".into()
        }
    }));
    cb.closed().await;
    eprintln!(
        "fixture: watched a+b simultaneously; one socket each; switching reused generations {a_epoch}/{b_epoch}; background b notified once; removing a retained b; reconnect replay stayed quiet; removal closed owned sockets"
    );
}

#[tokio::test]
async fn removing_environment_cancels_its_in_flight_session_state_fetch() {
    let mut server = FakeServer::start().await;
    let mut sessions = server.hold_session_requests();
    let (mut owner, _events) = Environments::start(&tokio::runtime::Handle::current(), false);
    owner.watch("a", server.credentials(), None);
    let mut connection = server.connection().await;
    let fetch = sessions.request().await;
    owner.remove("a");
    connection.closed().await;
    fetch.closed().await;
    eprintln!(
        "fixture: removing a closed its WebSocket and cancelled the in-flight authenticated HTTP fetch"
    );
}
