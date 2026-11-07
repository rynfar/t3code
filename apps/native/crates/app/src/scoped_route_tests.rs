use super::*;
use gpui::AppContext;
use t3_client::environments::{EnvironmentEvent, Environments};
use t3_client::supervisor::{ClientEvent, ClientFixture, ConnectionStatus};
use t3_core::sidebar::grouping::GroupConsumer;
use t3_core::thread_actions::ProjectAction;

#[gpui::test]
async fn captured_project_route_rechecks_identity_after_watched_activation(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Change { Unchanged, ReadRevoked, Replaced }
    for change in [Change::Unchanged, Change::ReadRevoked, Change::Replaced] {
        let (state, mut original_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (connections, mut output) = Environments::start(runtime.handle(), false);
        state.update(cx, |state, _| {
            state.stop_pump();
            state.connections = connections;
            state.environments.environments.push(serde_json::from_value(serde_json::json!({
                "id":"pending-background", "environmentId":"pending-background-env", "label":"Pending background",
                "httpBase":"http://fixture.invalid", "accessToken":"", "disabled":false
            })).unwrap());
        });
        let mut view = state.read_with(cx, |state, _| state.connections.view());
        let (client, mut fixture) = ClientFixture::handle(serde_json::from_value(serde_json::json!({
            "authenticated":true,"permissions":["orchestration:read","orchestration:operate"]
        })).unwrap());
        let (shell, mut config) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
        if let Some(config) = config.config.as_mut() {
            config.environment.environment_id = "pending-background-env".into();
        }
        let sequence = shell.sequence().unwrap() + 1;
        let install = state.update(cx, |state, _| state.connections.install_fixture("pending-background", client, shell, config, ConnectionStatus::Connected));
        let events = runtime.spawn(install).await.unwrap();
        let installed = view.borrow_and_update().clone();
        state.update(cx, |state, cx| state.apply_fleet_view(installed, cx));
        let captured = state.read_with(cx, |state, cx| state.project_groups_for(GroupConsumer::Sidebar, cx).into_iter().find(|group| group.member_refs.iter().any(|reference| reference.project_id == "checkout-b")).unwrap());
        let target = state.read_with(cx, |state, cx| state.scoped_project_action_target(&captured, cx).unwrap());
        let owner = target.project.owner.clone();
        let before = state.read_with(cx, |state, _| (state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()));
        state.update(cx, |state, cx| state.run_scoped_project_action(&captured, ProjectAction::NewThread, cx));
        state.read_with(cx, |state, _| {
            assert!(state.pending_scoped_route.is_some(), "real watched activation must be pending before its delivery");
            assert!(!state.shell.synchronized);
            assert!(state.draft_project.is_none());
        });
        let (activated, _output) = runtime.spawn(async move {
            loop {
                let event = output.recv().await.expect("actual activation actor remains alive");
                if matches!(event, EnvironmentEvent::Activated { .. }) { return (event, output); }
            }
        }).await.unwrap();
        assert!(matches!(&activated, EnvironmentEvent::Activated { id, .. } if id == "pending-background"));
        if change == Change::Replaced {
            let mut replacement = target.project.project.clone();
            replacement.created_at = "replacement-during-activation".into();
            let reference = target.project.reference();
            runtime.spawn(async move { events.send(ClientEvent::Shell(t3_core::shell::ShellStreamItem::ProjectUpdated { sequence, project: Box::new(replacement) })).await.unwrap(); }).await.unwrap();
            let changed = runtime.spawn(async move {
                view.wait_for(|view| view.environment(&reference.environment_id).is_some_and(|environment| environment.summary.projects.iter().any(|project| project.reference() == reference && project.project.created_at == "replacement-during-activation"))).await.unwrap().clone()
            }).await.unwrap();
            state.update(cx, |state, cx| state.apply_fleet_view(changed, cx));
            state.read_with(cx, |state, _| {
                assert_eq!(state.scoped_environment(&owner.saved_id).unwrap().summary.owner, owner);
                assert!(state.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
                assert!(state.pending_scoped_route.is_some());
                assert!(state.draft_project.is_none());
            });
        }
        else if change == Change::ReadRevoked {
            fixture.grants.send_replace(Some(serde_json::from_value(serde_json::json!({
                "authenticated":true,"permissions":["orchestration:operate"]
            })).unwrap()));
            let saved_id = owner.saved_id.clone();
            let denied = runtime.spawn(async move {
                view.wait_for(|view| view.environment(&saved_id).is_some_and(|environment| environment.grants.as_ref().is_some_and(|session| !session.grants(t3_core::auth::ORCHESTRATION_READ_SCOPE) && session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)))).await.unwrap().clone()
            }).await.unwrap();
            state.update(cx, |state, cx| state.apply_fleet_view(denied, cx));
            state.read_with(cx, |state, cx| {
                assert_eq!(state.scoped_environment(&owner.saved_id).unwrap().summary.owner, owner);
                assert!(!state.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
                assert!(state.scoped_project_action_target(&captured, cx).is_ok(), "read drift must not change captured project identity");
                assert!(state.draft_project.is_none());
            });
        }
        state.update(cx, |state, cx| state.apply_environment_event(activated, cx));
        state.read_with(cx, |state, _| {
            assert!(state.environments.get(&owner.saved_id).is_some(), "activated fixture must retain its real saved environment record");
            assert_eq!(state.scoped_environment(&owner.saved_id).unwrap().summary.owner, owner);
        });
        let barrier = state.read_with(cx, |state, _| state.connections.destination(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
        let destination = runtime.spawn(barrier).await.unwrap();
        if change == Change::ReadRevoked {
            assert!(matches!(destination, Err(t3_client::environments::DestinationError::Denied)));
        } else {
            destination.unwrap();
        }
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert!(state.pending_scoped_route.is_none(), "activation must settle the actual pending route");
            assert_eq!(state.environments.active.as_deref(), Some(owner.saved_id.as_str()));
            assert!(state.selected_thread.is_none());
            if change != Change::Unchanged {
                assert!(state.draft_project.is_none(), "a captured project route must not open after identity or read access changes during watched activation");
                let toast = state.toasts.latest().expect("pending project refusal must be visible");
                assert_eq!(toast.kind, crate::toast::ToastKind::Error);
                assert_eq!(toast.title, "Project action stopped");
                assert!(toast.description.as_ref().is_some_and(|description| !description.is_empty()));
            } else {
                assert_eq!(state.draft_project.as_deref(), Some(target.project.project.id.as_str()), "unchanged actual watched activation still opens the captured draft");
                assert_ne!(state.environments.active, before.0);
            }
        });
        assert!(fixture.take_commands().is_empty(), "pending project navigation never launches providers or sends writes");
        let original_commands = original_fixture.take_commands();
        assert_eq!(original_commands, vec!["close-all-threads"], "switching away closes only the previous client's subscriptions; no RPC or provider writes: {original_commands:?}");
        state.update(cx, |state, _| state.stop_pump());
    }
}

#[gpui::test]
async fn watched_thread_navigation_preserves_same_scope_shelves(cx: &mut gpui::TestAppContext) {
    use t3_core::sidebar::{SETTLED_TAIL_INITIAL_COUNT, Shelf};
    use t3_core::sidebar::tag_filter::ThreadKey;

    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, mut original) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
    let (connections, mut output) = Environments::start(runtime.handle(), false);
    let (base_shell, base_config) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
    state.update(cx, |state, _| {
        state.stop_pump();
        state.connections = connections;
        state.environments.environments.push(serde_json::from_value(serde_json::json!({
            "id":"shelf-background", "environmentId":"shelf-background-env", "label":"Shelf background",
            "httpBase":"http://fixture.invalid", "accessToken":"", "disabled":false
        })).unwrap());
    });
    let mut view = state.read_with(cx, |state, _| state.connections.view());
    let session = || serde_json::from_value(serde_json::json!({
        "authenticated":true,"permissions":["orchestration:read","orchestration:operate"]
    })).unwrap();
    let (active_client, mut active) = ClientFixture::handle(session());
    let install = state.update(cx, |state, _| state.connections.install_fixture(
        "saved-fixture", active_client, base_shell.clone(), base_config.clone(), ConnectionStatus::Connected,
    ));
    let active_events = runtime.spawn(install).await.unwrap();
    let (background_client, mut background) = ClientFixture::handle(session());
    let mut background_shell = base_shell;
    let snapshot = background_shell.snapshot.as_mut().unwrap();
    snapshot.threads.retain(|thread| thread.id == "menu-thread");
    for index in 0..35 {
        snapshot.threads.push(serde_json::from_value(serde_json::json!({
            "id":format!("shelf-settled-{index:02}"),"projectId":"checkout-b","title":format!("Settled {index}"),
            "createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-02T00:00:00Z",
            "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
            "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":format!("shelf-settled-{index:02}")},
            "status":"idle","hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0,
            "settledOverride":"settled","settledAt":"2026-01-02T00:00:00Z"
        })).unwrap());
    }
    let mut background_config = base_config;
    background_config.config.as_mut().unwrap().environment.environment_id = "shelf-background-env".into();
    let install = state.update(cx, |state, _| state.connections.install_fixture(
        "shelf-background", background_client, background_shell, background_config, ConnectionStatus::Connected,
    ));
    let background_events = runtime.spawn(install).await.unwrap();
    let installed = view.borrow_and_update().clone();
    state.update(cx, |state, cx| state.apply_fleet_view(installed, cx));
    state.read_with(cx, |state, _| state.connections.activate(Some("saved-fixture")));
    let (activated, returned_output) = runtime.spawn(async move {
        loop {
            let event = output.recv().await.expect("owned activation actor remains alive");
            if matches!(event, EnvironmentEvent::Activated { .. }) { return (event, output); }
        }
    }).await.unwrap();
    output = returned_output;
    assert!(matches!(&activated, EnvironmentEvent::Activated { id, .. } if id == "saved-fixture"));
    state.update(cx, |state, cx| {
        state.apply_environment_event(activated, cx);
        state.select_thread("sibling", cx);
    });
    let (method, payload, reply) = active.next_request().await;
    assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
    assert_eq!(payload["type"], "thread.visit");
    assert_eq!(payload["threadId"], "sibling");
    reply.send(Ok(serde_json::json!({"sequence":1}))).unwrap();
    let target = ThreadKey::new("shelf-background", "menu-thread");
    let (owner, chosen, other, shelves) = state.update(cx, |state, cx| {
        assert_eq!(state.environments.active.as_deref(), Some("saved-fixture"));
        assert!(state.shell.synchronized);
        assert_eq!(state.selected_thread.as_deref(), Some("sibling"));
        assert_eq!(state.fleet.environments.len(), 2);
        assert!(state.fleet.environments.iter().all(|entry| entry.config.config.is_some() && entry.summary.synchronized && entry.summary.older_settled.is_none()));
        let owner = state.scoped_environment(&target.environment_id).unwrap().summary.owner.clone();
        assert!(state.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
        assert!(state.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
        let row = state.scoped_thread(&target).unwrap();
        assert_eq!(row.owner, owner);
        assert_eq!(row.thread.project_id, "checkout-b");
        let groups = state.project_groups_for(GroupConsumer::Sidebar, cx);
        let chosen = groups.iter().find(|group| group.member_refs.contains(&row.project_ref())).unwrap().key.clone();
        let other = groups.iter().find(|group| group.member_refs.iter().any(|reference| reference.environment_id == "shelf-background" && reference.project_id == "other")).unwrap().key.clone();
        assert_ne!(chosen, other);
        state.set_scoped_project_scope(vec![chosen.clone()], cx);
        for shelf in [Shelf::Working, Shelf::Snoozed, Shelf::Settled] { state.toggle_shelf(shelf, cx); }
        state.show_more_settled(cx);
        assert_eq!(state.sidebar.shelves.settled_visible, 35);
        assert!(state.sidebar.shelves.working_expanded && state.sidebar.shelves.snoozed_expanded && state.sidebar.shelves.settled_expanded);
        assert_eq!(state.sidebar.fleet.partition.settled.len(), 35);
        assert_eq!(state.sidebar.fleet.listed_settled_count, 35);
        assert_eq!(state.scoped_project_scope(&state.project_groups_for(GroupConsumer::Sidebar, cx)), vec![chosen.clone()]);
        assert!(state.sidebar.fleet.thread(&target).is_some());
        (owner, chosen, other, state.sidebar.shelves.clone())
    });
    let barrier = state.read_with(cx, |state, _| state.connections.destination(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
    let destination = runtime.spawn(barrier).await.unwrap().unwrap();
    state.read_with(cx, |state, _| assert!(destination.is_current(&state.fleet)));
    let (send, selected) = tokio::sync::oneshot::channel();
    let mut send = Some(send);
    let _selected = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == AppEvent::Selection && state.environments.active.as_deref() == Some("shelf-background") && state.selected_thread.as_deref() == Some("menu-thread") {
            if let Some(send) = send.take() { let _ = send.send(()); }
        }
    }));
    state.update(cx, |state, cx| {
        state.navigate_scoped(owner.clone(), ScopedRouteAction::Thread(target.thread_id.clone()), cx);
        let pending = state.pending_scoped_route.as_ref().expect("the admitted watched thread route must wait for its real activation");
        assert_eq!(pending.owner, owner);
        assert!(matches!(&pending.action, ScopedRouteAction::Thread(id) if id == "menu-thread"));
        assert_eq!(state.environments.active.as_deref(), Some("shelf-background"));
        assert!(state.selected_thread.is_none());
        assert!(!state.shell.synchronized);
        assert_eq!(state.scoped_environment(&target.environment_id).unwrap().summary.owner, owner);
        assert!(state.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
        assert_eq!(state.project_preferences.scope_keys, vec![chosen.clone()]);
        assert_eq!(state.scoped_project_scope(&state.project_groups_for(GroupConsumer::Sidebar, cx)), vec![chosen.clone()]);
        assert_eq!(state.sidebar.shelves.settled_expanded, shelves.settled_expanded, "an admitted watched thread route with unchanged effective scope must keep the Settled shelf expanded");
        assert_eq!(state.sidebar.shelves.settled_visible, shelves.settled_visible, "unchanged effective scope must keep the 35-row Settled tail");
        assert_eq!(state.sidebar.shelves, shelves);
    });
    let (activated, _output) = runtime.spawn(async move {
        loop {
            let event = output.recv().await.expect("owned thread activation actor remains alive");
            if matches!(event, EnvironmentEvent::Activated { .. }) { return (event, output); }
        }
    }).await.unwrap();
    assert!(matches!(&activated, EnvironmentEvent::Activated { id, .. } if id == "shelf-background"));
    state.update(cx, |state, cx| state.apply_environment_event(activated, cx));
    selected.await.unwrap();
    state.read_with(cx, |state, cx| {
        assert!(state.pending_scoped_route.is_none());
        assert_eq!(state.environments.active.as_deref(), Some(owner.saved_id.as_str()));
        assert_eq!(state.selected_thread.as_deref(), Some(target.thread_id.as_str()));
        assert!(state.draft_project.is_none());
        assert_eq!(state.scoped_environment(&target.environment_id).unwrap().summary.owner, owner);
        assert_eq!(state.active_thread_key(&target.thread_id), Some(target.clone()));
        assert_eq!(state.scoped_project_scope(&state.project_groups_for(GroupConsumer::Sidebar, cx)), vec![chosen.clone()]);
        assert_eq!(state.sidebar.shelves, shelves, "delivering the actual activation must also preserve unchanged-scope shelves and tail");
        assert_eq!(state.sidebar.fleet.listed_settled_count, 35);
    });
    let (method, payload, reply) = background.next_request().await;
    assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
    assert_eq!(payload["type"], "thread.visit");
    assert_eq!(payload["threadId"], target.thread_id);
    reply.send(Ok(serde_json::json!({"sequence":2}))).unwrap();
    state.update(cx, |state, cx| {
        state.set_scoped_project_scope(vec![other.clone()], cx);
        assert_eq!(state.scoped_project_scope(&state.project_groups_for(GroupConsumer::Sidebar, cx)), vec![other]);
        assert_eq!(state.sidebar.shelves.settled_visible, SETTLED_TAIL_INITIAL_COUNT, "a genuinely changed project scope still resets the Settled tail");
        assert!(state.sidebar.shelves.settled_expanded);
        assert_eq!(state.environments.active.as_deref(), Some(owner.saved_id.as_str()));
        assert_eq!(state.selected_thread.as_deref(), Some(target.thread_id.as_str()));
    });
    runtime.spawn_blocking(|| t3_client::store::commit_client_settings(serde_json::Map::new())).await.unwrap().unwrap();
    cx.run_until_parked();
    assert!(!original.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND));
    assert!(!active.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND));
    let background_commands = background.take_commands();
    assert_eq!(background_commands, vec!["open-thread"], "navigation opens only the selected destination subscription after its visit");
    state.update(cx, |state, _| state.stop_pump());
    drop(active_events);
    drop(background_events);
    drop(destination);
}
