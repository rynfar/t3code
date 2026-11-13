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



fn socket_update_config() -> Value {
    json!({"environment":{"environmentId":"update-host","label":"Update host","serverVersion":"1.0.0","capabilities":{"serverSelfUpdate":"boot-service"}},"providers":[{"instanceId":"codex","driver":"codex","enabled":true,"installed":false,"status":"missing"}]})
}
async fn socket_update_shell(connection: &mut Connection, config: Value) -> Value {
    let (config_request, shell) = shell_streams(connection, config).await;
    connection.chunk(&shell,json!([{"kind":"snapshot","snapshot":{"snapshotSequence":1,"projects":[],"threads":[]}},{"kind":"synchronized"}])).await;
    config_request
}
fn socket_update_ready(view: &FleetView) -> bool {
    view.environment("update").is_some_and(|e| e.status == ConnectionStatus::Connected
        && e.summary.synchronized && e.summary.config_loaded
        && e.grants.as_ref().is_some_and(|s| s.grants("environment:maintain")))
}
async fn socket_update_request(connection: &mut Connection, tag: &str, task: &mut tokio::task::JoinHandle<Vec<String>>) -> Value {
    loop {
        tokio::select! {
            frame = connection.recv() => {
                if frame["_tag"] == "Request" {
                    assert_eq!(frame["tag"],tag,"unexpected update request: {frame}");
                    return frame;
                }
            },
            joined = &mut *task => panic!("runner completed before {tag}: {joined:?}"),
        }
    }
}
async fn socket_update_fence(connection: &mut Connection, client: &ClientHandle) {
    let response = client.request("fixture.read",json!({}));
    loop {
        let frame = connection.recv().await;
        if frame["_tag"] == "Request" {
            assert_eq!(frame["tag"],"fixture.read","unsafe write before ordered fence: {frame}");
            connection.send(json!({"_tag":"Exit","requestId":frame["id"],"exit":{"_tag":"Success","value":{}}})).await;
            break;
        }
    }
    response.await.unwrap();
}

#[tokio::test]
async fn unresolved_provider_revocation_withholds_restart_through_owned_join_and_fence() {
    let mut server = FakeServer::start().await;
    server.set_session(json!({"authenticated":true,"permissions":["providers:manage","environment:maintain"]}));
    let (mut owner, _events) = Environments::start(&tokio::runtime::Handle::current(),false);
    let mut view = owner.view(); owner.watch("update",server.credentials(),None);
    let mut connection = server.connection().await;
    socket_update_shell(&mut connection,socket_update_config()).await;
    view.wait_for(|view| socket_update_ready(view) && view.environment("update").unwrap().grants.as_ref().unwrap().grants("providers:manage")).await.unwrap();
    let client = owner.client("update").await.unwrap();
    let plan = t3_core::updates::Plan {
        providers: vec![t3_core::updates::ProviderTarget { environment_id:"update".into(),driver:"codex".into(),instance_id:"codex".into() }],
        servers: vec![t3_core::updates::ServerTarget { environment_id:"update".into(),label:"Update host".into(),target_version:"2.0.0".into(),self_update:"boot-service".into() }], ..Default::default()
    };
    let capture = crate::updates::capture(&plan,&view.borrow()).unwrap().remove(0);
    let mut destinations = Vec::new();
    for scope in ["providers:manage","environment:maintain"] {
        destinations.push(crate::updates::UpdateDestination { receipt:owner.destination(&capture.owner,scope).await,capture:capture.clone(),scope });
    }
    let mut task = tokio::spawn(crate::updates::run(plan,destinations,view.clone()));
    let held = socket_update_request(&mut connection,"server.updateProvider",&mut task).await;
    assert_eq!(held["payload"],json!({"provider":"codex","instanceId":"codex"}));
    server.set_session(json!({"authenticated":true,"permissions":["environment:maintain"]}));
    let mut live = client.session_state(); client.refresh_session_state();
    live.wait_for(|s| s.as_ref().is_some_and(|s| s.grants("environment:maintain") && !s.grants("providers:manage"))).await.unwrap();
    view.wait_for(|v| socket_update_ready(v) && !v.environment("update").unwrap().grants.as_ref().unwrap().grants("providers:manage")).await.unwrap();
    let failures = loop {
        tokio::select! {
            joined = &mut task => break joined.unwrap(),
            frame = connection.recv() => assert_ne!(frame["_tag"],"Request","unsafe request while provider unresolved: {frame}"),
        }
    };
    assert!(failures.iter().any(|s| s.contains("unresolved")));
    assert!(failures.iter().any(|s| s.contains("restart withheld")));
    socket_update_fence(&mut connection,&client).await;
    drop((held,connection,client,owner));
}

#[tokio::test]
async fn own_server_reconnect_accepts_ready_but_rejects_live_maintain_or_policy_loss() {
    for denial in ["none","grant","policy"] {
        let mut server = FakeServer::start().await;
        server.set_session(json!({"authenticated":true,"permissions":["environment:maintain","providers:manage"]}));
        let (mut owner, _events) = Environments::start(&tokio::runtime::Handle::current(),false);
        let mut view = owner.view(); owner.watch("update",server.credentials(),None);
        let mut connection = server.connection().await;
        socket_update_shell(&mut connection,socket_update_config()).await;
        view.wait_for(|view| socket_update_ready(view)).await.unwrap();
        let client = owner.client("update").await.unwrap();
        let plan = t3_core::updates::Plan { providers:vec![t3_core::updates::ProviderTarget { environment_id:"update".into(),driver:"codex".into(),instance_id:"codex".into() }], servers:vec![t3_core::updates::ServerTarget { environment_id:"update".into(),label:"Update host".into(),target_version:"2.0.0".into(),self_update:"boot-service".into() }],..Default::default() };
        let capture = crate::updates::capture(&plan,&view.borrow()).unwrap().remove(0);
        let original = capture.owner.clone();
        let provider_receipt = owner.destination(&original,"providers:manage").await;
        let receipt = owner.destination(&original,"environment:maintain").await;
        let mut task = tokio::spawn(crate::updates::run(plan,vec![
            crate::updates::UpdateDestination { capture:capture.clone(),scope:"providers:manage",receipt:provider_receipt },
            crate::updates::UpdateDestination { capture,scope:"environment:maintain",receipt },
        ],view.clone()));
        let provider = socket_update_request(&mut connection,"server.updateProvider",&mut task).await;
        let status = match denial { "none" => "succeeded", "grant" => "failed", _ => "unchanged" };
        connection.send(json!({"_tag":"Exit","requestId":provider["id"],"exit":{"_tag":"Success","value":{"providers":[{"instanceId":"codex","updateState":{"status":status}}]}}})).await;
        let prepare = socket_update_request(&mut connection,"server.updateServer",&mut task).await;
        assert_eq!(prepare["payload"],json!({"targetVersion":"2.0.0"}));
        connection.send(json!({"_tag":"Exit","requestId":prepare["id"],"exit":{"_tag":"Success","value":{"method":"service","targetVersion":"2.1.0","updateId":"trial"}}})).await;
        let _old_subscription = socket_update_request(&mut connection,"subscribeServerLifecycle",&mut task).await;
        drop(connection);
        let mut connection = server.connection().await;
        let mut config = socket_update_config(); config["environment"]["serverVersion"] = json!("2.1.0");
        let config_request = socket_update_shell(&mut connection,config.clone()).await;
        view.wait_for(|v| socket_update_ready(v) && v.environment("update").unwrap().summary.owner.connection_generation > original.connection_generation).await.unwrap();
        {
            let view = view.borrow(); let owner = &view.environment("update").unwrap().summary.owner;
            assert_eq!(owner.generation,original.generation); assert_eq!(owner.canonical_id,original.canonical_id);
            assert_eq!(owner.saved_id,original.saved_id); assert!(owner.shell_generation >= original.shell_generation);
            assert_eq!(owner.connection_generation,*client.connection_generation().borrow());
        }
        let lifecycle = socket_update_request(&mut connection,"subscribeServerLifecycle",&mut task).await;
        if denial == "grant" {
            server.set_session(json!({"authenticated":true,"permissions":[]}));
            let mut live = client.session_state(); client.refresh_session_state();
            live.wait_for(|s| s.as_ref().is_some_and(|s| !s.grants("environment:maintain"))).await.unwrap();
            view.wait_for(|v| v.environment("update").unwrap().grants.as_ref().is_some_and(|s| !s.grants("environment:maintain"))).await.unwrap();
        } else if denial == "policy" {
            config["managedPolicy"] = json!({"limits":{"updates":{"mode":"off"}}});
            connection.chunk(&config_request,json!([{"type":"snapshot","config":config}])).await;
            view.wait_for(|v| v.environment("update").unwrap().config.config.as_ref().unwrap().managed_policy.is_some()).await.unwrap();
        }
        let failures = if denial == "none" {
            connection.chunk(&lifecycle,json!([{"type":"ready","payload":{"environment":{"serverVersion":"2.1.0"},"updateOutcome":{"id":"trial","status":"committed","targetVersion":"2.1.0"}}}])).await;
            task.await.unwrap()
        } else {
            loop {
                tokio::select! {
                    joined = &mut task => break joined.unwrap(),
                    frame = connection.recv() => assert_ne!(frame["_tag"],"Request","unsafe request after authorization loss: {frame}"),
                }
            }
        };
        assert_eq!(failures.is_empty(),denial == "none","{denial}: {failures:?}");
        if denial != "none" {
            let expected = if denial == "grant" { "no permission to maintain server" } else { "server capability or policy changed" };
            assert!(failures.iter().any(|error| error.contains(expected)),"{denial}: {failures:?}");
        }
        socket_update_fence(&mut connection,&client).await;
        drop((connection,client,owner));
    }
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
        let before = view.borrow().environment("a").unwrap().summary.clone();
        let mut row = entries["b"].shell.thread("same").unwrap().clone();
        row.delegated_from = Some(json!({"environmentId":"canonical-a","threadId":"omitted"}));
        row.pending_runtime_request = Some(serde_json::from_value(json!({"id":"ask","kind":"command"})).unwrap());
        entries.get_mut("b").unwrap().shell.apply(t3_core::shell::ShellStreamItem::ThreadUpdated { sequence:4, location:"active".into(), thread:Box::new(row.clone()) });
        publish(&entries,"b",true,&views);
        let missing = view.borrow().environment("a").unwrap().summary.clone();
        assert!(!Arc::ptr_eq(&before,&missing), "only an absent target changed");
        assert!(missing.threads.iter().all(|row| row.thread.pending_subagent_request.is_none()));
        assert_eq!(missing.composed_requests(&[]).get("omitted"), Some(&t3_core::sidebar::thread::SubagentRequest::Approval));
        publish(&entries,"a",false,&views);
        assert!(Arc::ptr_eq(&missing,&view.borrow().environment("a").unwrap().summary), "quiet publication reuses current missing-target inputs");
        entries.get_mut("b").unwrap().status = ConnectionStatus::Connecting;
        publish(&entries,"b",true,&views);
        let disconnected = view.borrow().environment("a").unwrap().summary.clone();
        assert!(!Arc::ptr_eq(&missing,&disconnected));
        assert!(disconnected.composed_requests(&[]).is_empty(), "nonparticipating linked source retires its absent-target request");
        entries.get_mut("b").unwrap().status = ConnectionStatus::Connected;
        publish(&entries,"b",true,&views);
        assert_eq!(view.borrow().environment("a").unwrap().summary.composed_requests(&[]).get("omitted"), Some(&t3_core::sidebar::thread::SubagentRequest::Approval));
        row.pending_runtime_request = None;
        entries.get_mut("b").unwrap().shell.apply(t3_core::shell::ShellStreamItem::ThreadUpdated { sequence:5, location:"active".into(), thread:Box::new(row) });
        publish(&entries,"b",true,&views);
        let cleared = view.borrow().environment("a").unwrap().summary.clone();
        assert!(cleared.composed_requests(&[]).is_empty(), "authoritative request clear retires the absent-target contribution");
    }

    #[tokio::test]
    async fn quiet_fleet_fixture_uses_real_actor_destination_request_and_watches() {
        let (entry, mut fixture) = entry("actor");
        let (mut owner, _events) = Environments::start(&tokio::runtime::Handle::current(), false);
        let mut view = owner.view();
        let events = owner.install_fixture("actor", entry.client.clone(), entry.shell.clone(), entry.config.clone(), ConnectionStatus::Connected).await;
        view.changed().await.unwrap();
        let token = view.borrow_and_update().environment("actor").unwrap().summary.owner.clone();
        let destination = owner.destination(&token, ORCHESTRATION_OPERATE_SCOPE).await.unwrap();
        let request = destination.client.request(t3_core::commands::method::DISPATCH_COMMAND, json!({"fixture":"scoped"}));
        let (method, payload, reply) = fixture.next_request().await;
        assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(payload, json!({"fixture":"scoped"}));
        reply.send(Ok(json!({"sequence":2}))).unwrap();
        assert_eq!(request.await.unwrap(), json!({"sequence":2}));
        fixture.grants.send_replace(Some(serde_json::from_value(json!({"authenticated":true,"permissions":[]})).unwrap()));
        view.changed().await.unwrap(); view.borrow_and_update();
        assert!(matches!(owner.destination(&token, ORCHESTRATION_OPERATE_SCOPE).await, Err(DestinationError::Denied)));
        fixture.grants.send_replace(Some(serde_json::from_value(json!({"authenticated":true,"permissions":[ORCHESTRATION_OPERATE_SCOPE]})).unwrap()));
        view.changed().await.unwrap(); view.borrow_and_update();
        assert!(owner.destination(&token, ORCHESTRATION_OPERATE_SCOPE).await.is_ok());
        events.send(ClientEvent::Status(ConnectionStatus::Connecting)).await.unwrap();
        view.changed().await.unwrap(); view.borrow_and_update();
        assert!(matches!(owner.destination(&token, ORCHESTRATION_OPERATE_SCOPE).await, Err(DestinationError::Disconnected)));
        owner.remove("actor");
        view.changed().await.unwrap();
        assert!(view.borrow().environments.is_empty());
        assert!(matches!(owner.destination(&token, ORCHESTRATION_OPERATE_SCOPE).await, Err(DestinationError::Missing)));

        let [(mut active_entry, mut active_fixture), (mut a_entry, mut a_fixture), (mut b_entry, mut b_fixture)] = ["active", "a", "b"].map(self::entry);
        for (entry, fixture) in [(&mut active_entry, &mut active_fixture), (&mut a_entry, &mut a_fixture), (&mut b_entry, &mut b_fixture)] {
            fixture.grants.send_replace(Some(serde_json::from_value(json!({"authenticated":true,"permissions":["providers:manage"]})).unwrap()));
            entry.config.apply(serde_json::from_value(json!({"type":"snapshot","config":{"environment":{"environmentId":"shared-update-host","label":"Update host"},"scratchWorkspaceRoot":"/scratch","providers":[{"instanceId":"codex","driver":"codex","enabled":true,"installed":true,"status":"ready"}]}})).unwrap());
        }
        let (mut updates_owner, mut updates_events) = Environments::start(&tokio::runtime::Handle::current(), false);
        let updates_view = updates_owner.view();
        let mut updates_inputs = Vec::new();
        for (id, entry) in [("active", &active_entry), ("a", &a_entry), ("b", &b_entry)] {
            updates_inputs.push(updates_owner.install_fixture(id, entry.client.clone(), entry.shell.clone(), entry.config.clone(), ConnectionStatus::Connected).await);
        }
        drop((active_entry, a_entry, b_entry));
        let update_tokens = {
            let view = updates_view.borrow();
            assert_eq!(view.environments.len(), 3);
            ["active", "a", "b"].map(|id| {
                let entry = view.environment(id).unwrap();
                assert_eq!(entry.summary.owner.saved_id, id);
                assert_eq!(entry.summary.owner.canonical_id.as_deref(), Some("shared-update-host"));
                assert_eq!(entry.status, ConnectionStatus::Connected);
                assert!(entry.summary.synchronized && entry.summary.config_loaded);
                assert!(entry.grants.as_ref().unwrap().grants("providers:manage"));
                let providers = &entry.config.config.as_ref().unwrap().providers;
                assert_eq!(providers.len(), 1);
                assert_eq!((providers[0].driver.as_str(), providers[0].instance_id.as_str()), ("codex", "codex"));
                entry.summary.owner.clone()
            })
        };
        assert_ne!(update_tokens[0], update_tokens[1]);
        assert_ne!(update_tokens[0], update_tokens[2]);
        assert_ne!(update_tokens[1], update_tokens[2]);
        for token in &update_tokens {
            let destination = updates_owner.destination(token, "providers:manage").await.unwrap();
            assert_eq!(&destination.owner, token);
            assert!(destination.is_current(&updates_view.borrow()));
        }
        updates_owner.activate(Some("active"));
        let (active_client, active_generation) = activated(&mut updates_events, "active").await;
        assert_eq!(active_generation, update_tokens[0].generation);
        let plan = t3_core::updates::Plan {
            providers: vec![t3_core::updates::ProviderTarget { environment_id: update_tokens[1].saved_id.clone(), driver: "codex".into(), instance_id: "codex".into() }],
            ..Default::default()
        };
        let captures = crate::updates::capture(&plan, &updates_view.borrow()).unwrap();
        assert_eq!(captures[0].owner, update_tokens[1]);
        let capture = captures.into_iter().next().unwrap();
        let receipt = updates_owner.destination(&capture.owner, "providers:manage");
        let runner_view = updates_view.clone();
        let mut update_task = tokio::spawn(async move {
            let destinations = vec![crate::updates::UpdateDestination { capture, scope: "providers:manage", receipt: receipt.await }];
            crate::updates::run(plan, destinations, runner_view).await
        });
        drop(active_client);
        let observed = tokio::select! {
            request = active_fixture.next_request() => Ok(("active", request)),
            request = a_fixture.next_request() => Ok(("a", request)),
            request = b_fixture.next_request() => Ok(("b", request)),
            completed = &mut update_task => Err(completed),
        };
        let (observed_destination, request, reply_sent, completed) = match observed {
            Ok((destination, (method, payload, reply))) => {
                let reply_sent = reply.send(Ok(json!({"providers":[{"instanceId":"codex","updateState":{"status":"succeeded"}}]}))).is_ok();
                (destination, Some((method, payload)), reply_sent, update_task.await)
            }
            Err(completed) => ("completed", None, false, completed),
        };
        drop((updates_inputs, updates_owner, updates_events, updates_view, active_fixture, a_fixture, b_fixture));
        let failures = completed.expect("the owned update runner task must join");
        assert!(reply_sent, "the update runner must await the acquired unary reply");
        assert!(failures.is_empty(), "the acquired provider update must finish: {failures:?}");
        let (method, payload) = request.expect("the update runner must send the planned provider request");
        assert_eq!(method, "server.updateProvider");
        assert_eq!(payload, json!({"provider":"codex","instanceId":"codex"}));
        assert_eq!(observed_destination, "a", "a saved provider target must not use the unrelated active or colliding saved client");
    }


    #[tokio::test]
    async fn captured_update_drivers_tolerate_own_stream_and_refuse_command_drift() {
        let (mut entry, mut fixture) = entry("updates");
        fixture.grants.send_replace(Some(serde_json::from_value(json!({"authenticated":true,"permissions":["providers:manage"]})).unwrap()));
        let provider = |driver: &str, command: &str, stamp: &str| json!({"instanceId":driver,"driver":driver,"enabled":true,"installed":false,"status":"missing","checkedAt":stamp,"versionAdvisory":{"status":"behind_latest","latestVersion":"2.0.0","canUpdate":true,"updateCommand":command}});
        let providers = vec![provider("codex","codex update","2026-11-01"),provider("claudeCode","claude update","2026-11-01")];
        entry.config.apply(serde_json::from_value(json!({"type":"providerStatuses","payload":{"providers":providers}})).unwrap());
        let (mut owner, _events) = Environments::start(&tokio::runtime::Handle::current(), false);
        let mut view = owner.view();
        let input = owner.install_fixture("updates", entry.client.clone(), entry.shell.clone(), entry.config.clone(), ConnectionStatus::Connected).await;
        let plan = t3_core::updates::Plan { providers: ["codex","claudeCode"].map(|driver| t3_core::updates::ProviderTarget { environment_id:"updates".into(), driver:driver.into(), instance_id:driver.into() }).to_vec(), ..Default::default() };
        let capture = crate::updates::capture(&plan, &view.borrow()).unwrap().remove(0);
        let receipt = owner.destination(&capture.owner,"providers:manage").await;
        let runner_view = view.clone();
        let mut task = tokio::spawn(crate::updates::run(plan.clone(), vec![crate::updates::UpdateDestination { capture: capture.clone(), scope:"providers:manage", receipt }], runner_view));
        let (method,payload,reply) = tokio::select! { request = fixture.next_request() => request, done = &mut task => panic!("runner ended before first request: {done:?}") };
        assert_eq!(method,"server.updateProvider"); assert_eq!(payload["provider"],"codex");
        let mut streamed = vec![provider("codex","codex update","2026-11-02"),provider("claudeCode","claude update","2026-11-02")];
        streamed[0]["updateState"] = json!({"status":"running"});
        input.send(ClientEvent::ServerConfig(Box::new(serde_json::from_value(json!({"type":"providerStatuses","payload":{"providers":streamed}})).unwrap()))).await.unwrap();
        view.wait_for(|view| view.environment("updates").unwrap().config.config.as_ref().unwrap().providers[0].update_state.as_ref().is_some_and(|s| s.status == "running")).await.unwrap();
        streamed[0]["updateState"] = json!({"status":"succeeded"});
        input.send(ClientEvent::ServerConfig(Box::new(serde_json::from_value(json!({"type":"providerStatuses","payload":{"providers":streamed}})).unwrap()))).await.unwrap();
        view.wait_for(|view| view.environment("updates").unwrap().config.config.as_ref().unwrap().providers[0].update_state.as_ref().is_some_and(|s| s.status == "succeeded")).await.unwrap();
        reply.send(Ok(json!({"providers":[{"instanceId":"codex","updateState":{"status":"succeeded"}}]}))).unwrap();
        let (method,payload,reply) = tokio::select! { request = fixture.next_request() => request, done = &mut task => panic!("runner ended before second request: {done:?}") };
        assert_eq!(method,"server.updateProvider"); assert_eq!(payload["provider"],"claudeCode");
        reply.send(Ok(json!({"providers":[{"instanceId":"claudeCode","updateState":{"status":"succeeded"}}]}))).unwrap();
        assert!(task.await.unwrap().is_empty());
        let plan = t3_core::updates::Plan { providers: vec![plan.providers[1].clone()], ..Default::default() };
        let capture = crate::updates::capture(&plan,&view.borrow()).unwrap().remove(0);
        let receipt = owner.destination(&capture.owner,"providers:manage").await;
        streamed[1]["versionAdvisory"]["updateCommand"] = json!("changed command");
        input.send(ClientEvent::ServerConfig(Box::new(serde_json::from_value(json!({"type":"providerStatuses","payload":{"providers":streamed}})).unwrap()))).await.unwrap();
        view.wait_for(|view| view.environment("updates").unwrap().config.config.as_ref().unwrap().providers[1].version_advisory.as_ref().unwrap().update_command.as_deref() == Some("changed command")).await.unwrap();
        let mut refused = tokio::spawn(crate::updates::run(plan,vec![crate::updates::UpdateDestination { capture, scope:"providers:manage", receipt }],view.clone()));
        let failures = tokio::select! {
            result = &mut refused => result.unwrap(),
            request = fixture.next_request() => panic!("command drift was dispatched: {} {}",request.0,request.1),
        };
        assert!(failures.iter().any(|e| e.contains("facts changed")));
        let probe = entry.client.request("fixture.read",json!({}));
        let (method,_,reply) = fixture.next_request().await;
        assert_eq!(method,"fixture.read"); reply.send(Ok(json!({}))).unwrap(); probe.await.unwrap();
        drop((input,owner,fixture));
    }

    #[tokio::test]
    async fn quiet_fleet_config_updates_publish_revisions_and_reuse_quiet_summary() {
        let (mut entry, _fixture) = entry("config");
        let mut loaded_config = entry.config.config.clone().unwrap();
        let providers = |work: &str, office: &str| serde_json::from_value::<Vec<t3_core::server_config::ServerProvider>>(json!([
            {"instanceId":"work","driver":"codex","enabled":true,"installed":true,"status":"ready","checkedAt":work,"versionAdvisory":{"status":"behind_latest","latestVersion":"2.0.0","canUpdate":true,"updateCommand":"npm update codex"}},
            {"instanceId":"office","driver":"codex","enabled":true,"installed":true,"status":"ready","checkedAt":office,"versionAdvisory":{"status":"behind_latest","latestVersion":"2.0.0","canUpdate":true,"updateCommand":"npm update codex"}}
        ])).unwrap();
        loaded_config.providers = providers("2026-01-02","2026-01-01");
        assert_eq!(t3_core::updates::one_click_candidates(&loaded_config.providers)[0].instance_id,"work");
        entry.config = ServerConfigState::default();
        let (mut owner, mut output) = Environments::start(&tokio::runtime::Handle::current(), false);
        let mut view = owner.view();
        let events = owner.install_fixture("config", entry.client.clone(), entry.shell.clone(), entry.config.clone(), ConnectionStatus::Connected).await;
        view.changed().await.unwrap();
        let initial = view.borrow_and_update().clone();
        let absent = initial.environment("config").unwrap().summary.clone();
        assert!(initial.environment("config").unwrap().config.config.is_none());
        assert!(!absent.config_loaded);
        owner.activate(Some("config"));
        loop { if matches!(output.recv().await, Some(EnvironmentEvent::Activated { .. })) { break; } }
        events.send(ClientEvent::ServerConfig(Box::new(t3_core::server_config::ServerConfigStreamEvent::Snapshot { config: Box::new(loaded_config.clone()) }))).await.unwrap();
        loop { if matches!(output.recv().await, Some(EnvironmentEvent::Active { .. })) { break; } }
        let loaded = view.borrow_and_update().clone();
        let current = loaded.environment("config").unwrap();
        assert!(current.config.config.is_some());
        let revision = current.settings_revision;
        let summary = current.summary.clone();
        assert!(summary.config_loaded);
        assert!(!Arc::ptr_eq(&absent,&summary), "first config publishes a new summary without a shell event");
        assert_eq!(absent.snapshot_sequence,summary.snapshot_sequence);
        assert_eq!(absent.owner.shell_generation,summary.owner.shell_generation);
        assert_eq!(absent.threads.iter().map(|row| &row.thread).collect::<Vec<_>>(),summary.threads.iter().map(|row| &row.thread).collect::<Vec<_>>());
        let equal = loaded_config.clone();
        events.send(ClientEvent::ServerConfig(Box::new(t3_core::server_config::ServerConfigStreamEvent::Snapshot { config: Box::new(equal) }))).await.unwrap();
        loop { if matches!(output.recv().await, Some(EnvironmentEvent::Active { .. })) { break; } }
        assert_eq!(view.borrow().environment("config").unwrap().settings_revision, revision + 1);
        assert!(Arc::ptr_eq(&view.borrow().environment("config").unwrap().summary, &summary));
        let mut changed = loaded_config; changed.settings.guest_sharing_enabled = Some(false);
        events.send(ClientEvent::ServerConfig(Box::new(t3_core::server_config::ServerConfigStreamEvent::Snapshot { config: Box::new(changed) }))).await.unwrap();
        loop { if matches!(output.recv().await, Some(EnvironmentEvent::Active { .. })) { break; } }
        assert!(Arc::ptr_eq(&view.borrow().environment("config").unwrap().summary, &summary));
        let before = view.borrow_and_update().clone();
        let original_owner = before.environment("config").unwrap().summary.owner.clone();
        assert!(!view.has_changed().unwrap());
        let flipped = providers("2026-01-03","2026-01-04");
        assert_eq!(t3_core::updates::one_click_candidates(&flipped)[0].instance_id,"office");
        events.send(ClientEvent::ServerConfig(Box::new(t3_core::server_config::ServerConfigStreamEvent::ProviderStatuses {
            payload: t3_core::server_config::ProvidersPayload { providers: flipped },
        }))).await.unwrap();
        loop {
            if let EnvironmentEvent::Active { id, events, .. } = output.recv().await.expect("active actor remains alive")
                && id == "config"
                && events.iter().any(|event| matches!(event, ClientEvent::ServerConfig(event)
                    if matches!(&**event, t3_core::server_config::ServerConfigStreamEvent::ProviderStatuses { payload }
                        if payload.providers.iter().any(|p| p.instance_id == "office" && p.checked_at.0.as_deref() == Some("2026-01-04"))))) {
                break;
            }
        }
        assert!(view.has_changed().unwrap(), "a stamp-only representative flip must publish after actor processing");
        let published = view.borrow_and_update().clone();
        let current = published.environment("config").unwrap();
        assert!(published.revision > before.revision);
        assert_eq!(current.summary.owner,original_owner);
        assert!(Arc::ptr_eq(&current.summary,&summary));
        assert_eq!(t3_core::updates::one_click_candidates(&current.config.config.as_ref().unwrap().providers)[0].instance_id,"office");
        let published_entry = published.environments.iter().find(|e| e.summary.owner.saved_id == "config").unwrap().clone();
        let same_winner = providers("2026-01-05","2026-01-06");
        assert_eq!(t3_core::updates::one_click_candidates(&same_winner)[0].instance_id,"office");
        events.send(ClientEvent::ServerConfig(Box::new(t3_core::server_config::ServerConfigStreamEvent::ProviderStatuses {
            payload: t3_core::server_config::ProvidersPayload { providers: same_winner },
        }))).await.unwrap();
        loop {
            if let EnvironmentEvent::Active { id, events, .. } = output.recv().await.expect("active actor remains alive")
                && id == "config"
                && events.iter().any(|event| matches!(event, ClientEvent::ServerConfig(event)
                    if matches!(&**event, t3_core::server_config::ServerConfigStreamEvent::ProviderStatuses { payload }
                        if payload.providers.iter().any(|p| p.instance_id == "office" && p.checked_at.0.as_deref() == Some("2026-01-06"))))) {
                break;
            }
        }
        assert!(!view.has_changed().unwrap(), "a same-winner stamp refresh must stay quiet");
        let quiet = view.borrow();
        assert_eq!(quiet.revision,published.revision);
        assert!(Arc::ptr_eq(&*quiet,&published));
        assert!(Arc::ptr_eq(quiet.environments.iter().find(|e| e.summary.owner.saved_id == "config").unwrap(),&published_entry));
        assert!(Arc::ptr_eq(&quiet.environment("config").unwrap().summary,&summary));
        assert_eq!(quiet.environment("config").unwrap().summary.owner,original_owner);
    }

    fn b_owner(view: &watch::Receiver<Arc<FleetView>>) -> EnvironmentOwner {
        view.borrow().environment("b").unwrap().summary.owner.clone()
    }
}

#[tokio::test]
async fn desktop_guard_retries_same_prepared_token_across_own_old_version_reconnect() {
    let mut server = FakeServer::start().await;
    server.set_session(json!({"authenticated":true,"permissions":["environment:maintain"]}));
    let (mut owner, _events) = Environments::start(&tokio::runtime::Handle::current(),false);
    let mut view = owner.view(); owner.watch("update",server.credentials(),None);
    let mut connection = server.connection().await;
    let mut config = socket_update_config();
    config["environment"]["capabilities"] = json!({"serverSelfUpdate":"desktop-managed","desktopAppUpdate":true});
    socket_update_shell(&mut connection,config.clone()).await;
    view.wait_for(|v| socket_update_ready(v)).await.unwrap();
    let client = owner.client("update").await.unwrap();
    let plan = t3_core::updates::Plan { servers:vec![t3_core::updates::ServerTarget { environment_id:"update".into(),label:"Update host".into(),target_version:"2.0.0".into(),self_update:"desktop-managed".into() }],..Default::default() };
    let capture = crate::updates::capture(&plan,&view.borrow()).unwrap().remove(0);
    let original = capture.owner.clone();
    let receipt = owner.destination(&original,"environment:maintain").await;
    let mut task = tokio::spawn(crate::updates::run(plan,vec![crate::updates::UpdateDestination { capture,scope:"environment:maintain",receipt }],view.clone()));
    let prepare = socket_update_request(&mut connection,"server.updateServer",&mut task).await;
    assert_eq!(prepare["payload"],json!({"targetVersion":"2.0.0"}));
    connection.send(json!({"_tag":"Exit","requestId":prepare["id"],"exit":{"_tag":"Success","value":{"method":"desktop-app","targetVersion":"2.1.0","desktopUpdateToken":"host-prepared"}}})).await;
    let first_commit = socket_update_request(&mut connection,"server.commitDesktopUpdate",&mut task).await;
    assert_eq!(first_commit["payload"],json!({"requestId":"host-prepared"}));
    drop(connection);
    let mut previous = original.clone();
    for (version, retry) in [("1.0.0",true),("2.1.0",false)] {
        let mut connection = server.connection().await;
        config["environment"]["serverVersion"] = json!(version);
        socket_update_shell(&mut connection,config.clone()).await;
        view.wait_for(|v| socket_update_ready(v) && v.environment("update").unwrap().summary.owner.connection_generation > previous.connection_generation).await.unwrap();
        let current = view.borrow().environment("update").unwrap().summary.owner.clone();
        assert_eq!(current.saved_id,original.saved_id);
        assert_eq!(current.canonical_id,original.canonical_id);
        assert_eq!(current.generation,original.generation);
        assert!(current.shell_generation > previous.shell_generation);
        assert_eq!(current.connection_generation,*client.connection_generation().borrow());
        let lifecycle = socket_update_request(&mut connection,"subscribeServerLifecycle",&mut task).await;
        connection.chunk(&lifecycle,json!([{"type":"ready","payload":{"environment":{"serverVersion":version}}}])).await;
        if retry {
            let commit = socket_update_request(&mut connection,"server.commitDesktopUpdate",&mut task).await;
            assert_eq!(commit["payload"],first_commit["payload"]);
            previous = current;
            drop(connection);
        } else {
            assert!(task.await.unwrap().is_empty());
            socket_update_fence(&mut connection,&client).await;
            drop((connection,client,owner));
            return;
        }
    }
    unreachable!("the committed host-returned version must join the runner");
}

#[tokio::test]
async fn handed_off_server_guard_rejects_same_saved_replacement_and_removal() {
    for replacement in [true,false] {
        let mut server = FakeServer::start().await;
        server.set_session(json!({"authenticated":true,"permissions":["environment:maintain"]}));
        let (mut owner, _events) = Environments::start(&tokio::runtime::Handle::current(),false);
        let mut view = owner.view(); owner.watch("update",server.credentials(),None);
        let mut connection = server.connection().await;
        socket_update_shell(&mut connection,socket_update_config()).await;
        view.wait_for(|v| socket_update_ready(v)).await.unwrap();
        let client = owner.client("update").await.unwrap();
        let plan = t3_core::updates::Plan { servers:vec![t3_core::updates::ServerTarget { environment_id:"update".into(),label:"Update host".into(),target_version:"2.0.0".into(),self_update:"boot-service".into() }],..Default::default() };
        let capture = crate::updates::capture(&plan,&view.borrow()).unwrap().remove(0);
        let original = capture.owner.clone();
        let receipt = owner.destination(&original,"environment:maintain").await;
        let mut task = tokio::spawn(crate::updates::run(plan,vec![crate::updates::UpdateDestination { capture,scope:"environment:maintain",receipt }],view.clone()));
        let prepare = socket_update_request(&mut connection,"server.updateServer",&mut task).await;
        assert_eq!(prepare["payload"],json!({"targetVersion":"2.0.0"}));
        connection.send(json!({"_tag":"Exit","requestId":prepare["id"],"exit":{"_tag":"Success","value":{"method":"service","targetVersion":"2.0.0","updateId":"owned-trial"}}})).await;
        let held_lifecycle = socket_update_request(&mut connection,"subscribeServerLifecycle",&mut task).await;
        let mut other = FakeServer::start().await;
        other.set_session(json!({"authenticated":true,"permissions":["environment:maintain"]}));
        if replacement {
            owner.watch("update",other.credentials(),None);
            view.wait_for(|v| v.environment("update").is_some_and(|e| e.summary.owner.generation > original.generation)).await.unwrap();
        } else {
            owner.remove("update");
            view.wait_for(|v| v.environment("update").is_none()).await.unwrap();
        }
        let mut closed = false;
        let failures = loop {
            tokio::select! {
                joined = &mut task => break joined.unwrap(),
                frame = connection.try_recv(), if !closed => match frame {
                    Some(frame) => assert_ne!(frame["_tag"], "Request", "unsafe write after owner loss: {frame}"),
                    None => closed = true,
                }
            }
        };
        let expected = if replacement { "server owner changed" } else { "environment removed" };
        assert!(failures.iter().any(|e| e.contains(expected)),"{expected}: {failures:?}");
        if !closed {
            while let Some(frame) = connection.try_recv().await {
                assert_ne!(frame["_tag"], "Request", "unsafe write after owner loss: {frame}");
            }
        }
        assert!(client.request("fixture.read", json!({})).await.is_err());
        if replacement {
            let mut other_connection = other.connection().await;
            let config_request = loop {
                let frame = other_connection.recv().await;
                if frame["_tag"] == "Request" {
                    assert_eq!(frame["tag"],method::SUBSCRIBE_SERVER_CONFIG,"replacement must not receive old update work: {frame}");
                    break frame;
                }
            };
            other_connection.chunk(&config_request,json!([{"type":"snapshot","config":socket_update_config()}])).await;
            let shell_request = loop {
                let frame = other_connection.recv().await;
                if frame["_tag"] == "Request" {
                    assert_eq!(frame["tag"],method::SUBSCRIBE_SHELL,"replacement must not receive old update work: {frame}");
                    break frame;
                }
            };
            other_connection.chunk(&shell_request,json!([{"kind":"snapshot","snapshot":{"snapshotSequence":1,"projects":[],"threads":[]}},{"kind":"synchronized"}])).await;
            view.wait_for(|v| socket_update_ready(v) && v.environment("update").unwrap().summary.owner.generation > original.generation).await.unwrap();
            let current = view.borrow().environment("update").unwrap().summary.owner.clone();
            assert_eq!(current.saved_id,original.saved_id);
            assert_eq!(current.canonical_id,original.canonical_id);
            assert_ne!(current.generation,original.generation);
            let new_client = owner.client("update").await.unwrap();
            assert_ne!(new_client.environment(),client.environment());
            socket_update_fence(&mut other_connection,&new_client).await;
            drop((other_connection,new_client));
        }
        drop((held_lifecycle,connection,client,owner,other));
    }
}
