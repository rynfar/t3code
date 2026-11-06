use super::*;
use crate::test_server::{Connection, FakeServer};
use serde_json::{Value, json};
use t3_core::commands::method;

async fn shell(connection: &mut Connection) -> Value {
    shell_config(connection, json!({})).await
}

async fn shell_config(connection: &mut Connection, value: Value) -> Value {
    shell_streams(connection, value).await.1
}

async fn shell_streams(connection: &mut Connection, value: Value) -> (Value, Value) {
    let config = connection.request(method::SUBSCRIBE_SERVER_CONFIG).await;
    connection
        .chunk(&config, json!([{"type":"snapshot","config":value}]))
        .await;
    let shell = connection.request(method::SUBSCRIBE_SHELL).await;
    (config, shell)
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

async fn notification_batch(events: &mut mpsc::Receiver<EnvironmentEvent>, expected: &str) -> Vec<(String, String, &'static str)> {
    let mut notified = Vec::new();
    loop {
        match events.recv().await.unwrap() {
            EnvironmentEvent::Notifications { id, notifications, .. } => {
                notified.extend(notifications.into_iter().map(|n| (id.clone(), n.thread_id, n.title)));
            }
            EnvironmentEvent::Active { id, .. } if id == expected => return notified,
            _ => {}
        }
    }
}

#[tokio::test]
async fn delegated_requests_reobserve_the_unchanged_parent_and_suppress_the_task() {
    let (mut a, mut b) = (FakeServer::start().await, FakeServer::start().await);
    let (mut owner, mut events) = Environments::start(&tokio::runtime::Handle::current(), true);
    owner.watch("a", a.credentials(), None);
    owner.watch("b", b.credentials(), None);
    let (mut ca, mut cb) = (a.connection().await, b.connection().await);
    let sa = shell_config(&mut ca, json!({"environment":{"environmentId":"canonical-a"}})).await;
    let sb = shell(&mut cb).await;
    let row = |delegated: bool, asking: bool| json!({
        "id":"same", "projectId":"p", "title":"Task", "tags":["hidden"], "providerInstanceId":"replay",
        "modelSelection":{"instanceId":"replay","model":"replay"},"runtimeMode":"auto","interactionMode":"default",
        "lineage":{"rootThreadId":"same"}, "status":"running", "activityRunStatus":"running", "latestRunId":"r",
        "delegatedFrom":if delegated { json!({"environmentId":"canonical-a","threadId":"same"}) } else { Value::Null },
        "pendingRuntimeRequest":if asking { json!({"id":"ask","kind":"command","createdAt":"2026-11-01T00:00:00Z"}) } else { Value::Null },
        "hasActionableProposedPlan":false,"itemCount":1,"visibleItemCount":1, "createdAt":"2026-11-01T00:00:00Z", "updatedAt":"2026-11-01T00:00:00Z"
    });
    ca.chunk(&sa, json!([{"kind":"snapshot","snapshot":{"snapshotSequence":1,"projects":[],"threads":[row(false,false)]}},{"kind":"synchronized"}])).await;
    loop {
        if let EnvironmentEvent::Visibility(v) = events.recv().await.unwrap() && v.live.len() == 1 { break; }
    }
    cb.chunk(&sb, json!([{"kind":"snapshot","snapshot":{"snapshotSequence":1,"projects":[],"threads":[row(true,false)]}},{"kind":"synchronized"}])).await;
    loop {
        if let EnvironmentEvent::Visibility(v) = events.recv().await.unwrap() && v.live.len() == 2 { break; }
    }
    owner.activate(Some("b"));
    activated(&mut events, "b").await;
    cb.chunk(&sb, json!([{"kind":"thread.updated","sequence":2,"location":"active","thread":row(true,true)}])).await;
    assert_eq!(notification_batch(&mut events, "b").await, [("a".into(), "same".into(), "Approval needed")]);
    cb.chunk(&sb, json!([{"kind":"thread.updated","sequence":3,"location":"active","thread":row(true,false)}])).await;
    assert!(notification_batch(&mut events, "b").await.is_empty());
    drop(ca);
    loop {
        if let EnvironmentEvent::Visibility(v) = events.recv().await.unwrap() && v.delegated_under_parent.is_empty() {
            assert_eq!(v.live.len(), 2, "cached click destinations survive disconnect");
            break;
        }
    }
    cb.chunk(&sb, json!([{"kind":"thread.updated","sequence":4,"location":"active","thread":row(true,true)}])).await;
    assert_eq!(notification_batch(&mut events, "b").await, [("b".into(), "same".into(), "Approval needed")]);
    let mut ca = a.connection().await;
    let (config_a, sa) = shell_streams(&mut ca, json!({"environment":{"environmentId":"canonical-a"}})).await;
    ca.chunk(&sa, json!([{"kind":"snapshot","snapshot":{"snapshotSequence":1,"projects":[],"threads":[row(false,false)]}},{"kind":"synchronized"}])).await;
    loop {
        if let EnvironmentEvent::Visibility(v) = events.recv().await.unwrap() && !v.delegated_under_parent.is_empty() { break; }
    }
    owner.activate(Some("b"));
    let mut notifications = Vec::new();
    loop {
        match events.recv().await.unwrap() {
            EnvironmentEvent::Notifications { id, notifications: next, .. } => notifications.extend(next.into_iter().map(|n| (id.clone(), n.title))),
            EnvironmentEvent::Activated { id, .. } if id == "b" => break,
            _ => {}
        }
    }
    assert_eq!(notifications, [("a".into(), "Approval needed")]);
    ca.chunk(&config_a, json!([{"type":"snapshot","config":{"environment":{"environmentId":"canonical-a"},"settings":{"hideTaggedThreads":true,"hiddenThreadTags":["hidden"]}}}])).await;
    loop {
        if let EnvironmentEvent::Visibility(v) = events.recv().await.unwrap() && v.hidden.len() == 2 { break; }
    }
    cb.chunk(&sb, json!([{"kind":"thread.updated","sequence":5,"location":"active","thread":row(true,false)}])).await;
    assert!(notification_batch(&mut events, "b").await.is_empty());
    cb.chunk(&sb, json!([{"kind":"thread.updated","sequence":6,"location":"active","thread":row(true,true)}])).await;
    assert!(notification_batch(&mut events, "b").await.is_empty(), "tag-hidden parent consumes delegated attention");
    ca.chunk(&config_a, json!([{"type":"snapshot","config":{"environment":{"environmentId":"canonical-a"}}}])).await;
    loop {
        if let EnvironmentEvent::Visibility(v) = events.recv().await.unwrap() && !v.filtering { break; }
    }
    owner.activate(Some("b"));
    activated(&mut events, "b").await;
    let mut moved = FakeServer::start().await;
    owner.watch("a", moved.credentials(), None);
    ca.closed().await;
    let mut ca = moved.connection().await;
    let sa = shell_config(&mut ca, json!({"environment":{"environmentId":"canonical-a"}})).await;
    ca.chunk(&sa, json!([{"kind":"snapshot","snapshot":{"snapshotSequence":1,"projects":[],"threads":[row(false,false)]}},{"kind":"synchronized"}])).await;
    loop {
        match events.recv().await.unwrap() {
            EnvironmentEvent::Notifications { .. } => panic!("route replacement replayed attention"),
            EnvironmentEvent::Visibility(v) if !v.delegated_under_parent.is_empty() => break,
            _ => {}
        }
    }
    owner.activate(Some("b"));
    activated(&mut events, "b").await;
    owner.remove("a");
    loop {
        if let EnvironmentEvent::Visibility(v) = events.recv().await.unwrap() && v.live.len() == 1 {
            assert!(v.delegated_under_parent.is_empty());
            break;
        }
    }
    ca.closed().await;
    cb.chunk(&sb, json!([{"kind":"thread.updated","sequence":7,"location":"active","thread":row(true,false)}])).await;
    assert!(notification_batch(&mut events, "b").await.is_empty());
    cb.chunk(&sb, json!([{"kind":"thread.updated","sequence":8,"location":"active","thread":row(true,true)}])).await;
    assert_eq!(notification_batch(&mut events, "b").await, [("b".into(), "same".into(), "Approval needed")]);
    eprintln!("fixture: canonical delegated link notified saved parent only; disconnect exposed child without replay; reconnect restored parent rollup; tag reveal and route replacement stayed quiet; removal retired only parent and exposed child");
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


#[cfg(feature = "test-support")]
mod quiet_fleet {
    use super::*;
    use crate::supervisor::ClientFixture;
    use t3_core::auth::{AuthSessionState, ORCHESTRATION_OPERATE_SCOPE};

    fn entry(saved: &str) -> (Entry, ClientFixture) {
        let session: AuthSessionState = serde_json::from_value(json!({"authenticated":true,"permissions":[ORCHESTRATION_OPERATE_SCOPE]})).unwrap();
        let (client, fixture) = ClientFixture::handle(session);
        let mut config = ServerConfigState::default();
        config.apply(serde_json::from_value(json!({"type":"snapshot","config":{"environment":{"environmentId":format!("canonical-{saved}"),"label":saved},"scratchWorkspaceRoot":"/scratch"}})).unwrap());
        let row = json!({"id":"same","projectId":"p","title":saved,"providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},"runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":"same"},"status":"running","latestRunId":"run","activityRunStatus":"running","hasActionableProposedPlan":false,"itemCount":1,"visibleItemCount":1,"createdAt":"2026-11-01T00:00:00Z","updatedAt":"2026-11-01T00:00:00Z"});
        let mut shell = ShellState::default();
        shell.apply(serde_json::from_value(json!({"kind":"snapshot","snapshot":{"snapshotSequence":1,"projects":[{"id":"p","title":saved,"workspaceRoot":"/scratch","createdAt":"2026-11-01T00:00:00Z","updatedAt":"2026-11-01T00:00:00Z"}],"threads":[row]}})).unwrap());
        shell.synchronized = true;
        (Entry { client, credentials: Credentials { http_base:"http://fixture.invalid".into(), access_token:String::new() }, relay:tokio::spawn(std::future::pending()), generation:1, shell, config, status:ConnectionStatus::Connected }, fixture)
    }

    #[tokio::test]
    async fn quiet_fleet_retains_latest_summaries_and_rechecks_current_owner_grants() {
        let (a, _fa) = entry("a"); let (b, fb) = entry("b");
        let mut entries = HashMap::from([("b".into(), b), ("a".into(), a)]);
        let (views, mut view) = watch::channel(Arc::new(FleetView::default()));
        publish(&entries, "b", true, &views); publish(&entries, "a", true, &views);
        let initial = view.borrow_and_update().clone();
        assert_eq!(initial.environments.iter().map(|environment| environment.summary.owner.saved_id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert!(initial.environments.iter().all(|environment| environment.summary.projects[0].is_scratch));
        let a_arc = initial.environments[0].clone();
        let b_summary = initial.environments[1].summary.clone();
        let owner = initial.environments[1].summary.owner.clone();
        let destination = entries["b"].destination(&owner, ORCHESTRATION_OPERATE_SCOPE).unwrap();
        assert!(destination.is_current(&initial));
        let b = entries.get_mut("b").unwrap();
        let mut row = b.shell.thread("same").unwrap().clone(); row.updated_at = "2026-11-01T00:00:01Z".into(); row.item_count += 1;
        assert!(!apply_shell(b, &t3_core::shell::ShellStreamItem::ThreadUpdated { sequence:2, location:"active".into(), thread:Box::new(row) }));
        assert!(!view.has_changed().unwrap());
        assert_eq!(view.borrow().revision, initial.revision);
        let (event_tx, event_rx) = mpsc::channel(8); let (frame_tx, mut frames) = mpsc::channel(8);
        let relay = tokio::spawn(relay_events(event_rx, entries["b"].client.session_state(), entries["b"].client.connection_generation(), frame_tx, "b".into(), 1));
        fb.grants.send_replace(Some(serde_json::from_value(json!({"authenticated":true,"permissions":[]})).unwrap()));
        assert!(matches!(frames.recv().await, Some((id, 1, RelayFrame::Permission)) if id == "b"));
        publish(&entries, "b", false, &views);
        view.changed().await.unwrap();
        assert!(Arc::ptr_eq(&view.borrow().environments[1].summary, &b_summary));
        assert!(Arc::ptr_eq(&view.borrow().environments[0], &a_arc));
        assert!(matches!(entries["b"].destination(&owner, ORCHESTRATION_OPERATE_SCOPE), Err(DestinationError::Denied)));
        fb.grants.send_replace(Some(serde_json::from_value(json!({"authenticated":true,"permissions":[ORCHESTRATION_OPERATE_SCOPE]})).unwrap()));
        frames.recv().await.unwrap();
        assert!(entries["b"].destination(&owner, ORCHESTRATION_OPERATE_SCOPE).is_ok());
        fb.generation.send_replace(2);
        frames.recv().await.unwrap();
        assert!(matches!(entries["b"].destination(&owner, ORCHESTRATION_OPERATE_SCOPE), Err(DestinationError::Stale)));
        assert!(!destination.is_current(&view.borrow()));
        let b = entries.get_mut("b").unwrap();
        b.status = ConnectionStatus::Connecting; b.shell.synchronized = false;
        publish(&entries, "b", true, &views);
        let current = b_owner(&view);
        assert!(matches!(entries["b"].destination(&current, ORCHESTRATION_OPERATE_SCOPE), Err(DestinationError::Disconnected)));
        let b = entries.get_mut("b").unwrap(); b.status = ConnectionStatus::Connected; b.shell.synchronized = true; b.shell.generation += 1;
        assert!(matches!(b.destination(&current, ORCHESTRATION_OPERATE_SCOPE), Err(DestinationError::Stale)));
        publish(&entries, "b", true, &views);
        entries.remove("b"); publish(&entries, "b", true, &views);
        assert_eq!(view.borrow().environments.len(), 1);
        assert!(Arc::ptr_eq(&view.borrow().environments[0], &a_arc));
        drop(event_tx); relay.abort();
    }

    #[tokio::test]
    async fn quiet_fleet_reuses_linked_parent_request_rollup() {
        let (a, _fa) = entry("a"); let (mut b, _fb) = entry("b");
        let mut row = b.shell.thread("same").unwrap().clone();
        row.delegated_from = Some(json!({"environmentId":"canonical-a","threadId":"same"}));
        row.pending_runtime_request = Some(serde_json::from_value(json!({"id":"ask","kind":"command"})).unwrap());
        b.shell.apply(t3_core::shell::ShellStreamItem::ThreadUpdated { sequence:2, location:"active".into(), thread:Box::new(row) });
        let mut entries = HashMap::from([("a".into(),a),("b".into(),b)]);
        let (views, view) = watch::channel(Arc::new(FleetView::default()));
        publish(&entries,"a",true,&views); publish(&entries,"b",true,&views);
        assert_eq!(view.borrow().environment("a").unwrap().summary.threads[0].thread.pending_subagent_request, Some(t3_core::sidebar::thread::SubagentRequest::Approval));
        let mut row = entries["b"].shell.thread("same").unwrap().clone(); row.pending_runtime_request = None;
        entries.get_mut("b").unwrap().shell.apply(t3_core::shell::ShellStreamItem::ThreadUpdated { sequence:3, location:"active".into(), thread:Box::new(row) });
        publish(&entries,"b",true,&views);
        assert_eq!(view.borrow().environment("a").unwrap().summary.threads[0].thread.pending_subagent_request, None);
    }

    fn b_owner(view: &watch::Receiver<Arc<FleetView>>) -> EnvironmentOwner {
        view.borrow().environment("b").unwrap().summary.owner.clone()
    }
}
