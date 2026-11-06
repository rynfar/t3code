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
