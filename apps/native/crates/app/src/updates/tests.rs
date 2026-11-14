use super::*;
use gpui::{FocusHandle, HeadlessAppContext, TestAppContext};
use serde_json::json;
use std::sync::Arc;
use t3_client::store::{SavedEnvironment, SavedEnvironments};
use t3_client::supervisor::{ClientFixture, ConnectionStatus};
use t3_core::server_config::ServerConfigState;
use t3_core::shell::ShellState;
use t3_core::updates::HANDOFF_TIMEOUT_MS;

struct Harness {
    outer: FocusHandle,
    control: Entity<UpdateControl>,
}

impl Render for Harness {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(div().id("outer-focus").track_focus(&self.outer))
            .child(self.control.clone())
            .children(overlay(cx))
            .children(crate::settings::overlay(window, cx))
    }
}

async fn install_env(
    runtime: &tokio::runtime::Runtime,
    cx: &mut TestAppContext,
    state: &Entity<AppState>,
    id: &str,
    _label: &str,
    config_json: serde_json::Value,
    permissions: &[&str],
) -> (ClientFixture, tokio::sync::mpsc::Sender<t3_client::supervisor::ClientEvent>) {
    let session = serde_json::from_value(json!({
        "authenticated": true,
        "permissions": permissions,
    }))
    .unwrap();
    let (client, fixture) = ClientFixture::handle(session);
    let mut config = ServerConfigState::default();
    config.apply(
        serde_json::from_value(json!({
            "type": "snapshot",
            "config": config_json,
        }))
        .unwrap(),
    );
    let mut shell = ShellState::default();
    shell.synchronized = true;
    let (ready, loaded) = tokio::sync::oneshot::channel();
    let mut ready = Some(ready);
    let target_id = id.to_owned();
    let _sub = state.update(cx, |_, cx| {
        cx.subscribe(state, move |state, _, event, _| {
            if *event == AppEvent::ServerConfig
                && state
                    .fleet
                    .environment(&target_id)
                    .is_some_and(|e| e.status == ConnectionStatus::Connected && e.summary.config_loaded)
                && let Some(tx) = ready.take()
            {
                let _ = tx.send(());
            }
        })
    });
    let install = state.update(cx, |state, _| {
        state
            .connections
            .install_fixture(id, client, shell, config, ConnectionStatus::Connected)
    });
    let events = runtime.spawn(install).await.unwrap();
    loaded.await.unwrap();
    (fixture, events)
}

#[test]
fn disconnected_saved_environment_uses_saved_label() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        Arc::new(crate::icons::Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.allow_parking();
    let (state, _fixture) = cx.update(|cx| {
        crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
    });
    let (ready, loaded) = tokio::sync::oneshot::channel();
    let mut ready = Some(ready);
    let _loaded = state.update(&mut cx, |_, cx| {
        cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::ServerConfig
                && state.fleet.environment("saved-raw-id").is_some_and(|e| {
                    matches!(e.status, ConnectionStatus::Reconnecting { .. })
                })
                && let Some(ready) = ready.take()
            {
                let _ = ready.send(());
            }
        })
    });
    let session = serde_json::from_value(json!({"authenticated":true,"permissions":[]})).unwrap();
    let (actor, _env_fixture) = ClientFixture::handle(session);
    let install = state.update(&mut cx, |state, _| {
        state.environments = SavedEnvironments {
            active: None,
            environments: vec![SavedEnvironment {
                id: "saved-raw-id".into(),
                environment_id: None,
                label: "Work Linux".into(),
                disabled: false,
                http_base: "http://127.0.0.1:4000".into(),
                access_token: Some("tok".into()),
                server_version: Some("0.1.0".into()),
                local: false,
                ssh: None,
            }],
        };
        state.connections.install_fixture(
            "saved-raw-id",
            actor,
            ShellState::default(),
            ServerConfigState::default(),
            ConnectionStatus::Reconnecting {
                reason: "offline".into(),
                attempt: 1,
            },
        )
    });
    let _events = runtime.block_on(install);
    cx.update(|cx| cx.foreground_executor().clone())
        .block_test(loaded)
        .unwrap();

    state.read_with(&cx, |state, _| {
        let p = plan(state, false);
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.rows[0].label, "Work Linux");
    });
}

#[gpui::test]
async fn active_run_keeps_other_environments_selectable_and_capsule_retries_and_dismisses(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _base_fixture) = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        let (state, fixture) =
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
        crate::settings::init(state.clone(), cx);
        init(cx);
        (state, fixture)
    });

    let (mut fixture_a, _events_a) = install_env(
        &runtime,
        cx,
        &state,
        "env-a",
        "Alpha",
        json!({
            "environment": {
                "environmentId": "env-a",
                "label": "Alpha",
                "serverVersion": "0.0.1",
                "capabilities": { "serverSelfUpdate": "npm" }
            },
            "providers": [{
                "instanceId": "codex",
                "driver": "codex",
                "enabled": true,
                "installed": true,
                "status": "ready",
                "versionAdvisory": {
                    "status": "behind_latest",
                    "latestVersion": "9.0.0",
                    "canUpdate": true,
                    "updateCommand": "update"
                }
            }]
        }),
        &["providers:manage", "environment:maintain"],
    )
    .await;
    let (mut fixture_b, _events_b) = install_env(
        &runtime,
        cx,
        &state,
        "env-b",
        "Beta",
        json!({
            "environment": {
                "environmentId": "env-b",
                "label": "Beta",
                "serverVersion": "0.0.1",
                "capabilities": { "serverSelfUpdate": "npm" }
            },
            "providers": []
        }),
        &["providers:manage", "environment:maintain"],
    )
    .await;

    let control = cx.new(|cx| UpdateControl::new(state.clone(), cx));
    let _window = cx.add_window(|_, cx| Harness {
        outer: cx.focus_handle().tab_stop(true),
        control: control.clone(),
    });
    cx.run_until_parked();

    // Start an update targeting ONLY env-a while env-b stays idle and selectable (P3.3).
    let (plan_a, captures_a) = state.read_with(cx, |state, _| {
        let mut p = plan(state, false);
        p.rows.retain(|r| r.environment_id == "env-a");
        p.providers.retain(|p| p.environment_id == "env-a");
        p.servers.retain(|s| s.environment_id == "env-a");
        p.restarts.retain(|r| r.environment_id == "env-a");
        let caps = t3_client::updates::capture(&p, &state.fleet).unwrap();
        (p, caps)
    });
    control.update(cx, |ctrl, cx| ctrl.start_run(plan_a, captures_a, cx));
    cx.run_until_parked();

    // P3.3: env-a is marked already updating, while env-b remains selectable in control.plan.
    control.read_with(cx, |ctrl, _| {
        assert!(ctrl.is_updating("env-a"));
        assert!(!ctrl.is_updating("env-b"));
        assert!(ctrl.capsule_timer.is_some());
        let row_a = ctrl.plan.rows.iter().find(|r| r.environment_id == "env-a").unwrap();
        assert_eq!(row_a.skipped.as_deref(), Some("already updating"));
        let row_b = ctrl.plan.rows.iter().find(|r| r.environment_id == "env-b").unwrap();
        assert!(row_b.skipped.is_none());
        assert!(ctrl.plan.servers.iter().any(|s| s.environment_id == "env-b"));
    });

    // Answer env-a's provider update with success, then fail server.updateServer.
    let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
    let mut settled_tx = Some(settled_tx);
    let _sub1 = cx.update(|cx| {
        cx.subscribe(&control, move |ctrl, _, cx| {
            let ctrl = ctrl.read(cx);
            if ctrl.active_runs == 0
                && ctrl
                    .run
                    .as_ref()
                    .is_some_and(|r| r.machines[0].result.is_some())
                && let Some(tx) = settled_tx.take()
            {
                let _ = tx.send(());
            }
        })
    });
    let (m1, _, r1) = fixture_a.next_request().await;
    assert_eq!(m1, "server.updateProvider");
    let _ = r1.send(Ok(json!({
        "providers": [{ "instanceId": "codex", "updateState": { "status": "succeeded" } }]
    })));
    let (m2, _, r2) = fixture_a.next_request().await;
    assert_eq!(m2, "server.updateServer");
    let _ = r2.send(Err(t3_client::session::RpcError::Failure(
        t3_core::rpc::RpcFailure {
            tag: None,
            message: "disk full".into(),
            error: serde_json::Value::Null,
        },
    )));
    settled_rx.await.unwrap();
    cx.run_until_parked();

    control.read_with(cx, |ctrl, _| {
        let run = ctrl.run.as_ref().unwrap();
        assert_eq!(run.machines[0].result, Some(updates::MachineResult::Failed));
        assert_eq!(run.machines[0].failure.as_deref(), Some("disk full"));
        assert!(ctrl.capsule_timer.is_none(), "settled capsule must hold zero idle timers");
        assert!(ctrl.handoff_timer.is_none());
    });

    // While env-b is still active (Waiting/Downloading), popover footer must hide Retry/Dismiss,
    // and once all machines settle with a failure, both Retry and Dismiss must render.
    // While any machine is active, popover footer must hide Retry/Dismiss;
    // when settled with zero failures, Dismiss stays hidden; once settled with a failure, both show.
    control.read_with(cx, |ctrl, _| {
        let settled_failed = ctrl.run.as_ref().unwrap();
        assert_eq!(capsule_popover_actions(settled_failed, &[]), (true, true));
        let mut with_active = settled_failed.clone();
        with_active.machines.push(updates::UpdateRunMachine {
            environment_id: "env-active".into(),
            label: "Active".into(),
            updates_providers: false,
            server: Some(updates::ServerTarget {
                environment_id: "env-active".into(),
                label: "Active".into(),
                target_version: "0.2.0".into(),
                self_update: "npm".into(),
            }),
            handed_off_version: None,
            started_at_ms: now_ms(),
            result: None,
            failure: None,
            provider_failure: None,
        });
        assert_eq!(
            capsule_popover_actions(
                &with_active,
                &[("env-active".into(), Some(ServerUpdateStage::Downloading))]
            ),
            (false, false)
        );
        let mut zero_failures = settled_failed.clone();
        zero_failures.machines[0].result = Some(updates::MachineResult::Done);
        zero_failures.machines[0].failure = None;
        assert_eq!(capsule_popover_actions(&zero_failures, &[]), (false, false));
        let mut track_switch = settled_failed.clone();
        track_switch.track_switch = true;
        assert_eq!(capsule_popover_actions(&track_switch, &[]), (false, true));
    });
    let (retry_tx, retry_rx) = tokio::sync::oneshot::channel();
    let mut retry_tx = Some(retry_tx);
    let _sub2 = cx.update(|cx| {
        cx.subscribe(&control, move |ctrl, _, cx| {
            let ctrl = ctrl.read(cx);
            if ctrl.active_runs == 0
                && ctrl
                    .run
                    .as_ref()
                    .is_some_and(|r| r.machines[0].result.is_some())
                && let Some(tx) = retry_tx.take()
            {
                let _ = tx.send(());
            }
        })
    });
    control.update(cx, |ctrl, cx| {
        ctrl.capsule_open = true;
        ctrl.retry_failed(cx);
    });
    let (m3, _, r3) = fixture_a.next_request().await;
    assert_eq!(m3, "server.updateServer");
    let _ = r3.send(Err(t3_client::session::RpcError::Failure(
        t3_core::rpc::RpcFailure {
            tag: None,
            message: "still failing".into(),
            error: serde_json::Value::Null,
        },
    )));
    retry_rx.await.unwrap();
    cx.run_until_parked();
    assert!(fixture_b.take_commands().is_empty());

    control.update(cx, |ctrl, cx| ctrl.dismiss_settled(cx));
    cx.run_until_parked();
    control.read_with(cx, |ctrl, _| {
        assert!(ctrl.run.is_none());
        assert!(!ctrl.capsule_open);
        assert!(ctrl.capsule_timer.is_none());
        assert!(ctrl.handoff_timer.is_none());
    });
}

#[gpui::test]
async fn saved_result_restores_toast_resumes_handoff_and_settles_on_reconnect_or_timeout(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _base_fixture) = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        let (state, fixture) =
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
        crate::settings::init(state.clone(), cx);
        init(cx);
        (state, fixture)
    });
    let (_fixture_a, events_a) = install_env(
        &runtime,
        cx,
        &state,
        "env-a",
        "Alpha",
        json!({
            "environment": {
                "environmentId": "env-a",
                "label": "Alpha",
                "serverVersion": "0.1.0",
                "capabilities": { "serverSelfUpdate": "npm" }
            },
            "providers": []
        }),
        &["environment:maintain"],
    )
    .await;

    let control = cx.new(|cx| UpdateControl::new(state.clone(), cx));
    let started = now_ms();

    // Mid-run ServerHandedOff persists accumulated provider outcomes and failures.
    control.update(cx, |ctrl, cx| {
        ctrl.run = Some(UpdateEverythingRun {
            track_switch: false,
            phase: updates::UpdateRunPhase::Providers,
            machines: vec![updates::UpdateRunMachine {
                environment_id: "env-a".into(),
                label: "Alpha".into(),
                updates_providers: true,
                server: Some(updates::ServerTarget {
                    environment_id: "env-a".into(),
                    label: "Alpha".into(),
                    target_version: "0.2.0".into(),
                    self_update: "npm".into(),
                }),
                handed_off_version: None,
                started_at_ms: started,
                result: None,
                failure: None,
                provider_failure: None,
            }],
        });
        ctrl.apply_event(
            t3_client::updates::UpdateEvent::ProvidersFinished {
                updated_providers: 1,
                failures: vec!["Beta / Codex: denied".into()],
            },
            cx,
        );
        ctrl.apply_event(
            t3_client::updates::UpdateEvent::ServerHandedOff {
                environment_id: "env-a".into(),
                target_version: "0.2.0".into(),
            },
            cx,
        );
    });
    control.read_with(cx, |ctrl, _| {
        let persisted = ctrl.last_saved_result.as_ref().expect("mid-run saved result");
        assert_eq!(persisted.updated_providers, 1);
        assert_eq!(persisted.failures, vec!["Beta / Codex: denied"]);
        assert_eq!(persisted.handed_off.len(), 1);
    });
    control.update(cx, |ctrl, _| {
        ctrl.run = None;
        ctrl.clear_run_outcomes();
        ctrl.last_saved_result = None;
    });

    // Relaunch with 0 prior completions still shows "<label> is still restarting." and
    // emits completion toast when the server reconnects on >= targetVersion (!behind).
    let saved = SavedUpdateEverythingResult {
        saved_at_ms: started,
        updated_servers: 0,
        updated_providers: 0,
        failures: Vec::new(),
        handed_off: vec![updates::HandedOffServer {
            environment_id: "env-a".into(),
            label: "Alpha".into(),
            target_version: "0.2.0".into(),
            started_at_ms: started,
        }],
    };
    control.update(cx, |ctrl, cx| {
        ctrl.restore_saved_result(serde_json::to_value(&saved).unwrap(), started + 1000, cx);
    });
    cx.run_until_parked();

    state.read_with(cx, |state, _| {
        let toast = state.toasts.latest().expect("restored toast");
        assert_eq!(toast.title.as_ref(), "Alpha is still restarting.");
    });
    control.read_with(cx, |ctrl, _| {
        let run = ctrl.run.as_ref().expect("resuming run");
        assert_eq!(
            updates::resolve_machine_status(&run.machines[0], run.phase, None),
            UpdateRunMachineStatus::Restarting
        );
        assert!(ctrl.handoff_timer.is_some());
    });

    // Reconnecting on a newer version (!behind("0.2.1", "0.2.0")) settles and emits completion toast.
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let mut done_tx = Some(done_tx);
    let _sub = state.update(cx, |_, cx| {
        cx.subscribe(&state, move |state, _, event, _| {
            if *event == AppEvent::ServerConfig
                && state
                    .fleet
                    .environment("env-a")
                    .and_then(|e| e.config.config.as_ref())
                    .is_some_and(|c| c.environment.server_version == "0.2.1")
                && let Some(tx) = done_tx.take()
            {
                let _ = tx.send(());
            }
        })
    });
    events_a
        .send(t3_client::supervisor::ClientEvent::ServerConfig(Box::new(
            serde_json::from_value(json!({
                "type": "snapshot",
                "config": {
                    "environment": {
                        "environmentId": "env-a",
                        "label": "Alpha",
                        "serverVersion": "0.2.1",
                        "capabilities": { "serverSelfUpdate": "npm" }
                    },
                    "providers": []
                }
            }))
            .unwrap(),
        )))
        .await
        .unwrap();
    done_rx.await.unwrap();
    cx.run_until_parked();

    control.read_with(cx, |ctrl, _| {
        assert!(ctrl.run.is_none());
        assert!(ctrl.capsule_timer.is_none());
        assert!(ctrl.handoff_timer.is_none());
    });
    state.read_with(cx, |state, _| {
        let toast = state.toasts.latest().expect("resumed completion toast");
        assert_eq!(toast.title.as_ref(), "Updated 1 server");
    });

    // Timeout case: expired handoff settles as Failed, drops handoff_timer, and emits failure toast.
    let old_saved = SavedUpdateEverythingResult {
        saved_at_ms: now_ms(),
        updated_servers: 0,
        updated_providers: 0,
        failures: Vec::new(),
        handed_off: vec![updates::HandedOffServer {
            environment_id: "env-a".into(),
            label: "Alpha".into(),
            target_version: "0.3.0".into(),
            started_at_ms: now_ms() - HANDOFF_TIMEOUT_MS + 500,
        }],
    };
    control.update(cx, |ctrl, cx| {
        ctrl.restore_saved_result(serde_json::to_value(&old_saved).unwrap(), now_ms(), cx);
    });
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    control.read_with(cx, |ctrl, _| {
        let run = ctrl.run.as_ref().expect("failed run stays visible");
        assert_eq!(run.machines[0].result, Some(updates::MachineResult::Failed));
        assert!(
            run.machines[0]
                .failure
                .as_deref()
                .is_some_and(|m| m.contains("Timed out"))
        );
        assert!(ctrl.handoff_timer.is_none());
        assert!(ctrl.capsule_timer.is_none());
    });
    state.read_with(cx, |state, _| {
        let toast = state.toasts.latest().expect("timeout failure toast");
        assert_eq!(toast.title.as_ref(), "Update failed");
        assert_eq!(
            toast.description.as_ref().map(|d| d.as_ref()),
            Some("Alpha: Timed out waiting for the server to resume after updating.")
        );
    });
}

#[gpui::test]
async fn provider_pill_cutoff_auto_dismiss_settings_focus_and_dialog_disappearance_guard(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _base_fixture) = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        let (state, fixture) =
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
        crate::settings::init(state.clone(), cx);
        init(cx);
        (state, fixture)
    });

    // Seed initial historical terminal state before UpdateControl mounts.
    state.update(cx, |state, cx| {
        state.phase = Phase::Online(ConnectionStatus::Connected);
        let config = state.server_config.config.as_mut().unwrap();
        config.providers = vec![serde_json::from_value(json!({
            "instanceId": "codex",
            "driver": "codex",
            "enabled": true,
            "installed": true,
            "status": "ready",
            "checkedAt": "2026-01-01T00:00:00Z",
            "updateState": {
                "status": "succeeded",
                "finishedAt": "2025-12-31T23:59:00Z"
            }
        }))
        .unwrap()];
        cx.emit(AppEvent::ServerConfig);
    });

    let control = cx.new(|cx| UpdateControl::new(state.clone(), cx));
    let window = cx.add_window(|_, cx| Harness {
        outer: cx.focus_handle().tab_stop(true),
        control: control.clone(),
    });
    window
        .update(cx, |view, window, cx| {
            window.focus(&view.outer, cx);
        })
        .unwrap();
    cx.run_until_parked();

    // Historical succeeded provider before visible_after is suppressed.
    control.read_with(cx, |ctrl, _| {
        assert!(ctrl.pill.is_none());
        assert!(ctrl.success_timer.is_none());
    });

    // Newer succeeded state appears, then auto-dismisses after 3s.
    state.update(cx, |state, cx| {
        let p = &mut state.server_config.config.as_mut().unwrap().providers[0];
        p.checked_at = t3_core::server_config::CheckedAt(Some("2026-01-01T00:01:00Z".into()));
        p.update_state = Some(serde_json::from_value(json!({
            "status": "succeeded",
            "finishedAt": "2026-01-01T00:01:00Z"
        }))
        .unwrap());
        cx.emit(AppEvent::ServerConfig);
    });
    cx.run_until_parked();
    control.read_with(cx, |ctrl, _| {
        assert_eq!(ctrl.pill.as_ref().unwrap().tone, Tone::Success);
        assert!(ctrl.success_timer.is_some());
    });
    cx.executor().advance_clock(Duration::from_millis(3000));
    cx.run_until_parked();
    control.read_with(cx, |ctrl, _| {
        assert!(ctrl.pill.is_none());
        assert!(ctrl.success_timer.is_none());
    });

    // Newer failed pill appears; Tab -> Enter opens Settings -> Providers, Tab -> Enter dismisses.
    state.update(cx, |state, cx| {
        let p = &mut state.server_config.config.as_mut().unwrap().providers[0];
        p.checked_at = t3_core::server_config::CheckedAt(Some("2026-01-01T00:02:00Z".into()));
        p.update_state = Some(serde_json::from_value(json!({
            "status": "failed",
            "finishedAt": "2026-01-01T00:02:00Z",
            "message": "checksum mismatch"
        }))
        .unwrap());
        cx.emit(AppEvent::ServerConfig);
    });
    cx.run_until_parked();
    control.read_with(cx, |ctrl, _| {
        assert_eq!(ctrl.pill.as_ref().unwrap().tone, Tone::Error);
    });
    // Move focus into provider-update-main, then Enter opens Settings -> Providers.
    window
        .update(cx, |view, window, cx| {
            window.focus(&view.outer, cx);
            window.focus_next(cx);
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "enter");
    cx.run_until_parked();
    window
        .update(cx, |_, window, cx| {
            assert_eq!(
                crate::settings::setup_selection_for_test(window, cx).as_deref(),
                Some("codex")
            );
        })
        .unwrap();
    // Close settings, focus provider-update-main, then Tab -> Enter inside SidebarUpdates dismisses.
    cx.simulate_keystrokes(window.into(), "escape");
    cx.run_until_parked();
    window
        .update(cx, |view, window, cx| {
            window.focus(&view.outer, cx);
            window.focus_next(cx);
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "tab enter");
    cx.run_until_parked();
    control.read_with(cx, |ctrl, _| {
        assert!(ctrl.pill.is_none(), "keyboard dismiss hides pill");
    });
    // Changing finishedAt redisplays the dismissed pill.
    state.update(cx, |state, cx| {
        let p = &mut state.server_config.config.as_mut().unwrap().providers[0];
        p.checked_at = t3_core::server_config::CheckedAt(Some("2026-01-01T00:03:00Z".into()));
        p.update_state = Some(serde_json::from_value(json!({
            "status": "failed",
            "finishedAt": "2026-01-01T00:03:00Z",
            "message": "new failure"
        }))
        .unwrap());
        cx.emit(AppEvent::ServerConfig);
    });
    cx.run_until_parked();
    control.read_with(cx, |ctrl, _| {
        assert_eq!(ctrl.pill.as_ref().unwrap().tone, Tone::Error);
    });

    // Open UpdateDialog, disappear fleet entry -> disabled_reason becomes Some, Escape restores focus.
    let (_env_fx, _events) = install_env(
        &runtime,
        cx,
        &state,
        "env-guard",
        "Guard",
        json!({
            "environment": {
                "environmentId": "env-guard",
                "label": "Guard",
                "serverVersion": "0.0.1",
                "capabilities": { "serverSelfUpdate": "npm" }
            },
            "providers": []
        }),
        &["environment:maintain"],
    )
    .await;
    cx.run_until_parked();
    let dialog_plan = state.read_with(cx, |state, _| plan(state, false));
    assert!(dialog_plan.has_updates());
    window
        .update(cx, |view, window, cx| {
            window.focus(&view.outer, cx);
            overlay(cx).unwrap().update(cx, |dialog, cx| {
                dialog.open(dialog_plan, false, window, cx);
            });
        })
        .unwrap();
    cx.run_until_parked();
    state.update(cx, |state, cx| {
        let mut fleet = (*state.fleet).clone();
        fleet.environments.clear();
        state.fleet = Arc::new(fleet);
        cx.emit(AppEvent::ServerConfig);
    });
    cx.run_until_parked();
    control.read_with(cx, |ctrl, cx| {
        assert!(ctrl.dialog.read(cx).disabled_reason(cx).is_some());
    });
    cx.simulate_keystrokes(window.into(), "escape");
    cx.run_until_parked();
    control.read_with(cx, |ctrl, cx| {
        assert!(ctrl.dialog.read(cx).plan.is_none());
    });
    window
        .update(cx, |view, window, _| {
            assert!(view.outer.is_focused(window));
        })
        .unwrap();
}

#[gpui::test]
async fn channel_switch_run_sets_track_switch_and_dispatches_boot_service(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _base_fixture) = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        let (state, fixture) =
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
        crate::settings::init(state.clone(), cx);
        init(cx);
        (state, fixture)
    });
    let (mut fixture_boot, _) = install_env(
        &runtime,
        cx,
        &state,
        "env-boot",
        "Boot",
        json!({
            "environment": {
                "environmentId": "env-boot",
                "label": "Boot",
                "serverVersion": "0.1.3-nightly.20261001.3500",
                "databaseMigration": 69,
                "capabilities": { "serverSelfUpdate": "boot-service" }
            },
            "providers": []
        }),
        &["environment:maintain"],
    )
    .await;
    let control = cx.new(|cx| UpdateControl::new(state.clone(), cx));
    cx.run_until_parked();

    let release = updates::ChannelSwitchRelease {
        channel: updates::UpdateTrack::Latest,
        version: "0.1.3".into(),
        database_migration: Some(69),
    };
    let (switch, captures) = state.read_with(cx, |state, _| {
        let s = channel_switch_plan_with(state, &release, |_| false);
        let caps = t3_client::updates::capture_channel_switch(&s, &state.fleet).unwrap();
        (s, caps)
    });
    assert_eq!(switch.targets.len(), 1);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let mut done_tx = Some(done_tx);
    let _sub = cx.update(|cx| {
        cx.subscribe(&control, move |ctrl, _, cx| {
            let ctrl = ctrl.read(cx);
            if ctrl.active_runs == 0
                && ctrl.run.as_ref().is_some_and(|r| r.machines[0].result.is_some())
                && let Some(tx) = done_tx.take()
            {
                let _ = tx.send(());
            }
        })
    });
    control.update(cx, |ctrl, cx| {
        ctrl.start_channel_switch(switch, updates::UpdateTrack::Latest, captures, cx);
    });
    let (method, input, reply) = fixture_boot.next_request().await;
    assert_eq!(method, "server.updateServer");
    assert!(input.get("channel").is_none());
    assert_eq!(input["targetVersion"], "0.1.3");
    let _ = reply.send(Err(t3_client::session::RpcError::Failure(
        t3_core::rpc::RpcFailure {
            tag: None,
            message: "boot failed".into(),
            error: serde_json::Value::Null,
        },
    )));
    done_rx.await.unwrap();
    cx.run_until_parked();
    control.read_with(cx, |ctrl, _| {
        let run = ctrl.run.as_ref().unwrap();
        assert!(run.track_switch);
        assert_eq!(
            updates::describe_update_capsule(run, &ctrl.stages, now_ms()).action,
            Some(CapsuleAction::Dismiss)
        );
    });
}

#[gpui::test]
async fn circular_update_button_mutes_during_run_and_capsule_popover_escapes(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _base_fixture) = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        let (state, fixture) =
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
        crate::settings::init(state.clone(), cx);
        init(cx);
        (state, fixture)
    });
    let control = cx.new(|cx| UpdateControl::new(state, cx));
    let (root, vcx) = cx.add_window_view(|_, cx| Harness {
        outer: cx.focus_handle().tab_stop(true),
        control: control.clone(),
    });
    control.update(vcx, |ctrl, cx| {
        ctrl.plan = updates::Plan {
            servers: vec![updates::ServerTarget {
                environment_id: "local".into(),
                label: "Local".into(),
                target_version: "0.2.0".into(),
                self_update: "npm".into(),
            }],
            providers: Vec::new(),
            restarts: Vec::new(),
            rows: Vec::new(),
        };
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear();
    });
    let button_bounds = vcx
        .debug_bounds("update-everything")
        .expect("32x32 circular update button renders when updates are available");
    assert_eq!(button_bounds.size.width, gpui::px(32.));
    assert_eq!(button_bounds.size.height, gpui::px(32.));

    control.update(vcx, |ctrl, cx| {
        ctrl.run = Some(updates::UpdateEverythingRun {
            track_switch: false,
            phase: updates::UpdateRunPhase::Servers,
            machines: vec![updates::UpdateRunMachine {
                environment_id: "local".into(),
                label: "Local".into(),
                updates_providers: false,
                server: None,
                handed_off_version: None,
                started_at_ms: now_ms(),
                result: None,
                failure: None,
                provider_failure: None,
            }],
        });
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear();
    });
    assert!(
        vcx.debug_bounds("update-everything").is_none(),
        "active update run must hide #update-everything"
    );
    let trigger_center = vcx
        .debug_bounds("update-capsule-trigger")
        .expect("#update-capsule-trigger renders during active run")
        .center();
    vcx.simulate_mouse_move(trigger_center, None::<gpui::MouseButton>, Default::default());
    vcx.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: trigger_center,
        modifiers: Default::default(),
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Left,
        position: trigger_center,
        modifiers: Default::default(),
        click_count: 1,
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear();
    });
    assert!(
        vcx.debug_bounds("update-capsule-popover").is_some(),
        "clicking capsule trigger opens upward popover"
    );

    vcx.update(|window, cx| {
        let outer = root.read(cx).outer.clone();
        window.focus(&outer, cx);
        window.focus_next(cx);
    });
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear();
    });
    assert!(
        vcx.debug_bounds("update-capsule-popover").is_none(),
        "Escape closes #update-capsule-popover"
    );
}

#[gpui::test]
async fn dialog_open_checks_provider_updates_across_connected_machines_and_clears_checking(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _base_fixture) = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        let (state, fixture) =
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
        crate::settings::init(state.clone(), cx);
        init(cx);
        (state, fixture)
    });
    let (mut fixture_a, _events_a) = install_env(
        &runtime,
        cx,
        &state,
        "env-a",
        "Alpha",
        json!({
            "environment": { "environmentId": "env-a", "label": "Alpha", "serverVersion": "0.1.0" },
            "providers": []
        }),
        &["providers:manage", "environment:maintain"],
    )
    .await;
    let (mut fixture_b, _events_b) = install_env(
        &runtime,
        cx,
        &state,
        "env-b",
        "Beta",
        json!({
            "environment": { "environmentId": "env-b", "label": "Beta", "serverVersion": "0.1.0" },
            "providers": []
        }),
        &["providers:manage"],
    )
    .await;
    let control = cx.new(|cx| UpdateControl::new(state, cx));
    let (_root, vcx) = cx.add_window_view(|_, cx| Harness {
        outer: cx.focus_handle().tab_stop(true),
        control: control.clone(),
    });
    control.update(vcx, |ctrl, cx| {
        ctrl.plan = updates::Plan {
            servers: vec![updates::ServerTarget {
                environment_id: "env-a".into(),
                label: "Alpha".into(),
                target_version: "0.2.0".into(),
                self_update: "npm".into(),
            }],
            providers: Vec::new(),
            restarts: Vec::new(),
            rows: Vec::new(),
        };
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear();
    });
    let button_center = vcx
        .debug_bounds("update-everything")
        .expect("#update-everything renders")
        .center();
    vcx.simulate_mouse_move(button_center, None::<gpui::MouseButton>, Default::default());
    vcx.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: button_center,
        modifiers: Default::default(),
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Left,
        position: button_center,
        modifiers: Default::default(),
        click_count: 1,
    });
    vcx.run_until_parked();
    control.read_with(vcx, |ctrl, cx| {
        assert!(ctrl.dialog.read(cx).checking);
    });
    let (method_a, input_a, reply_a) = fixture_a.next_request().await;
    let (method_b, input_b, reply_b) = fixture_b.next_request().await;
    assert_eq!(method_a, t3_core::settings::providers::SERVER_REFRESH_PROVIDERS);
    assert_eq!(method_b, t3_core::settings::providers::SERVER_REFRESH_PROVIDERS);
    assert_eq!(input_a, json!({ "refreshModels": true }));
    assert_eq!(input_b, json!({ "refreshModels": true }));
    let _ = reply_a.send(Ok(json!({ "providers": [] })));
    let _ = reply_b.send(Ok(json!({ "providers": [] })));
    let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
    let mut settled_tx = Some(settled_tx);
    let dialog_entity = control.read_with(vcx, |ctrl, _| ctrl.dialog.clone());
    let _obs = vcx.update(|_, cx| {
        cx.observe(&dialog_entity, move |dialog, cx| {
            if !dialog.read(cx).checking
                && let Some(tx) = settled_tx.take()
            {
                let _ = tx.send(());
            }
        })
    });
    vcx.run_until_parked();
    if !dialog_entity.read_with(vcx, |d, _| !d.checking) {
        settled_rx.await.unwrap();
        vcx.run_until_parked();
    }
    control.read_with(vcx, |ctrl, cx| {
        assert!(!ctrl.dialog.read(cx).checking);
    });
}

#[gpui::test]
async fn retry_failed_skips_already_updated_server_in_fleet(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _base_fixture) = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        let (state, fixture) =
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
        crate::settings::init(state.clone(), cx);
        init(cx);
        (state, fixture)
    });
    let (_fixture_retry, _events) = install_env(
        &runtime,
        cx,
        &state,
        "env-retry",
        "RetryBox",
        json!({
            "environment": {
                "environmentId": "env-retry",
                "label": "RetryBox",
                "serverVersion": "0.2.0",
                "capabilities": { "serverSelfUpdate": "npm" }
            },
            "providers": []
        }),
        &["environment:maintain"],
    )
    .await;
    let control = cx.new(|cx| UpdateControl::new(state, cx));
    cx.run_until_parked();

    control.update(cx, |ctrl, cx| {
        ctrl.capsule_open = true;
        ctrl.run = Some(updates::UpdateEverythingRun {
            track_switch: false,
            phase: updates::UpdateRunPhase::Servers,
            machines: vec![updates::UpdateRunMachine {
                environment_id: "env-retry".into(),
                label: "RetryBox".into(),
                updates_providers: false,
                server: Some(updates::ServerTarget {
                    environment_id: "env-retry".into(),
                    label: "RetryBox".into(),
                    target_version: "0.2.0".into(),
                    self_update: "npm".into(),
                }),
                handed_off_version: None,
                started_at_ms: now_ms(),
                result: Some(updates::MachineResult::Failed),
                failure: Some("timed out".into()),
                provider_failure: None,
            }],
        });
        ctrl.retry_failed(cx);
    });
    cx.run_until_parked();
    control.read_with(cx, |ctrl, _| {
        assert!(ctrl.run.is_none());
        assert!(!ctrl.capsule_open);
    });
}
