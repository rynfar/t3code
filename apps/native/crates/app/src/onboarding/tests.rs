//! Real native RPC against a fake server; no T3 data or provider processes.
use serde_json::json;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};
use t3_client::session::Session;
use t3_core::{onboarding::projects as model, projects::PROJECTS_MUTATE};
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[tokio::test]
async fn onboarding_rpc_retries_ambiguous_creates_and_partial_history_without_duplicates() {
    let mut child = Command::new("node").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/src/onboarding/tests/rpc-server.cjs")).stdout(Stdio::piped()).spawn().unwrap();
    let mut port = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut port).unwrap();
    let _server = Server(child);
    let url = format!("ws://127.0.0.1:{}", port.trim());
    let mut session = Session::connect(&url).await.unwrap();
    let scan: model::Scan = serde_json::from_value(session.request(model::SCAN, json!({})).await.unwrap()).unwrap();
    let candidate = &scan.candidates[0];
    let key = model::key("fixture", &candidate.path);
    let mut progress = model::Progress::default();
    progress.begin_attempt(vec![key.clone()]);
    progress.attempts.insert(key.clone(), "stable-project".into());
    for _ in 0..4 {
        let id = progress.attempts[&key].clone();
        let failure = session.request(PROJECTS_MUTATE, model::create_payload(&id, candidate)).await.unwrap_err();
        let failure = super::projects::RequestFailure::from(failure);
        assert!(!failure.definitive, "{failure}");
        progress.failed_attempt(&key, failure.definitive);
        assert_eq!(progress.attempts[&key], "stable-project");
        session = Session::connect(&url).await.unwrap();
    }
    session.request(PROJECTS_MUTATE, model::create_payload(&progress.attempts[&key], candidate)).await.unwrap();
    let rejected = session.request(PROJECTS_MUTATE, model::create_payload("rejected-project", candidate)).await.unwrap_err();
    assert!(super::projects::RequestFailure::from(rejected).definitive);
    for expected in [2, 3] {
        progress.begin_attempt(vec![key.clone()]);
        let result = serde_json::from_value(session.request(model::IMPORT, model::import_payload("stable-project", candidate)).await.unwrap()).unwrap();
        progress.record(key.clone(), "stable-project".into(), result);
        assert_eq!(progress.imported, expected);
    }
    assert_eq!(progress.skipped, 0);
    assert_eq!(progress.completed.len(), 1);
    assert_eq!(progress.landing(), Some("stable-project"));
    let rejected = session.request("unsupported.onboarding", json!({})).await.unwrap_err();
    assert!(!super::projects::RequestFailure::from(rejected).definitive);
    let mut demand = model::Demand::default();
    let token = demand.begin(7);
    let mut held = Box::pin(session.request(model::IMPORT, json!({"projectId":"held-project","expectedWorkspaceRoot":"/fixture/repo"})));
    assert!(matches!(futures::poll!(held.as_mut()), std::task::Poll::Pending));
    assert_eq!(progress.completion(model::FinishIntent::Import, false, |_| true), model::Completion::Denied);
    assert_eq!(progress.completion(model::FinishIntent::Skip, false, |id| id == "stable-project"), model::Completion::Ready(Some("stable-project".into())));
    demand.cancel();
    drop(held);
    assert!(!demand.accepts(token, Some(7), true));
    let stats = session.request("fixture.inspect", json!({})).await.unwrap();
    assert_eq!(stats["interrupts"], 1);
    assert_eq!(stats["creates"], 1);
    assert_eq!(stats["createCalls"], 6);
    assert_eq!(stats["imports"], 2);
    println!(
        "create committed once across Defect, ClientProtocolError, post-commit ProjectMutationError and unknown failure; typed conflict confirmed; retry 2/1 -> 3/0 reports 3; revoked automatic completion denied; local skip retains landing, interrupts pending import, no extra writes"
    );
}

mod ui_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use gpui::{
        AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div,
        point, px, size,
    };
    use serde_json::json;
    use t3_client::store::SavedEnvironment;
    use t3_client::supervisor::ConnectionStatus;
    use t3_core::client_settings::ClientSettingChange;
    use t3_core::mcp::gate::{CloudLinkState, ConnectReachability};
    use t3_core::onboarding::{
        PRIVACY_POLICY_LINK_LABEL, PhoneConnectState, TELEMETRY_NOTICE_DEFAULT,
        TELEMETRY_NOTICE_SEE_POLICY,
    };

    use super::super::phone::PhoneFixtureState;
    use super::super::{Step, WelcomeWizard};
    use crate::app_state::{AppEvent, AppState, Phase};

    fn phone_fixture(
        open: bool,
        can_read_relay: bool,
        can_write_relay: bool,
        link_state: Option<CloudLinkState>,
        link_pending: bool,
        link_error: Option<String>,
    ) -> PhoneFixtureState {
        PhoneFixtureState {
            open,
            can_read_relay,
            can_write_relay,
            link_state,
            link_pending,
            link_error,
        }
    }

    struct WelcomeHost {
        wizard: Entity<WelcomeWizard>,
        settings_rendered: Arc<AtomicBool>,
    }

    impl Render for WelcomeHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let overlay = crate::settings::overlay(window, cx);
            self.settings_rendered.store(overlay.is_some(), Ordering::SeqCst);
            div()
                .relative()
                .size_full()
                .child(self.wizard.clone())
                .children(overlay)
        }
    }

    fn mount_welcome(
        cx: &mut gpui::TestAppContext,
    ) -> (
        tokio::runtime::Runtime,
        Entity<AppState>,
        Entity<WelcomeWizard>,
        Arc<AtomicBool>,
        gpui::VisualTestContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::onboarding::init(cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            crate::settings::init(state.clone(), cx);
            crate::settings::store::update_client(&state, ClientSettingChange::Language("en".into()), cx);
            crate::updates::init(cx);
            state
        });
        state.update(cx, |state, cx| {
            state.environments.environments = vec![SavedEnvironment {
                id: "local-mac".into(),
                environment_id: Some("env-primary".into()),
                label: "Mac".into(),
                http_base: "http://127.0.0.1:3773".into(),
                access_token: Some("token".into()),
                server_version: Some("0.1.7".into()),
                local: true,
                disabled: false,
                ssh: None,
            }];
            state.environments.active = Some("local-mac".into());
            state.phase = Phase::Online(ConnectionStatus::Connected);
            state.server_config.config = Some(
                serde_json::from_value(json!({
                    "environment": { "environmentId": "env-primary", "label": "Mac" },
                    "cwd": "/tmp/workspace",
                    "settings": {},
                    "providers": []
                }))
                .unwrap(),
            );
            cx.emit(AppEvent::Phase);
        });
        let settings_rendered = Arc::new(AtomicBool::new(false));
        let rendered_flag = settings_rendered.clone();
        let window = cx.open_window(size(px(1024.), px(1600.)), |window, cx| {
            let wizard = cx.new(|cx| WelcomeWizard::new(state.clone(), window, cx));
            WelcomeHost {
                wizard,
                settings_rendered: rendered_flag,
            }
        });
        let wizard = window.read_with(cx, |root, _| root.wizard.clone()).unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        wizard.update(&mut vcx, |w, cx| {
            w.show(Step::Connection, cx);
            w.set_phone_fixture_state(phone_fixture(false, true, true, None, false, None), cx);
        });
        draw(&mut vcx);
        (runtime, state, wizard, settings_rendered, vcx)
    }

    fn draw(vcx: &mut gpui::VisualTestContext) {
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    }

    fn teardown(
        mut vcx: gpui::VisualTestContext,
        state: Entity<AppState>,
        wizard: Entity<WelcomeWizard>,
        runtime: tokio::runtime::Runtime,
        cx: &mut gpui::TestAppContext,
    ) {
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        drop(vcx);
        drop(wizard);
        drop(state);
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn phone_section_gates_visibility_and_toggles_qr_without_background_tasks(
        cx: &mut gpui::TestAppContext,
    ) {
        let (runtime, state, wizard, _settings_rendered, mut vcx) = mount_welcome(cx);

        // Localized labels resolve for PhoneSection controls.
        vcx.read(|cx| {
            assert_eq!(crate::i18n::t(cx, "Phone"), "Phone");
            assert_eq!(
                crate::i18n::t(cx, "Use T3 Code on your phone"),
                "Use T3 Code on your phone"
            );
            assert_eq!(crate::i18n::t(cx, "Set up"), "Set up");
            assert_eq!(crate::i18n::t(cx, "Hide"), "Hide");
            assert_eq!(
                crate::i18n::t(cx, "Scan with your phone to get the app."),
                "Scan with your phone to get the app."
            );
        });

        // Visible on primary environment with relay:read + relay:write, starts collapsed.
        assert!(vcx.debug_bounds("welcome-phone-section").is_some());
        assert!(!wizard.read_with(&vcx, |w, _| w.is_phone_open()));
        assert!(vcx.debug_bounds("welcome-phone-qr").is_none());

        // Clicking "Set up" expands QR and Step 2 without spawning timers/network tasks in tests.
        let toggle_center = vcx
            .debug_bounds("welcome-phone-toggle")
            .expect("Phone toggle button")
            .center();
        vcx.simulate_click(toggle_center, Default::default());
        draw(&mut vcx);
        assert!(wizard.read_with(&vcx, |w, _| w.is_phone_open()));
        assert!(vcx.debug_bounds("welcome-phone-qr").is_some());
        assert!(vcx.debug_bounds("welcome-phone-step-2").is_some());
        assert!(
            !wizard.read_with(&vcx, |w, _| w.phone_has_background_tasks()),
            "test/snapshot mode must never spawn real timers or background network calls"
        );

        // Switching language updates localized labels via crate::i18n::t without breaking layout.
        vcx.update(|_, cx| {
            crate::settings::store::update_client(&state, ClientSettingChange::Language("de".into()), cx);
        });
        draw(&mut vcx);
        vcx.read(|cx| {
            assert_eq!(crate::i18n::active_locale(cx), "de");
            assert_ne!(crate::i18n::t(cx, "Use T3 Code on your phone"), "");
            assert_ne!(crate::i18n::t(cx, "Hide"), "");
        });
        vcx.update(|_, cx| {
            crate::settings::store::update_client(&state, ClientSettingChange::Language("en".into()), cx);
        });
        draw(&mut vcx);

        // Clicking "Hide" collapses again.
        let toggle_center = vcx
            .debug_bounds("welcome-phone-toggle")
            .expect("Phone Hide button")
            .center();
        vcx.simulate_click(toggle_center, Default::default());
        draw(&mut vcx);
        assert!(!wizard.read_with(&vcx, |w, _| w.is_phone_open()));
        assert!(vcx.debug_bounds("welcome-phone-qr").is_none());

        // Missing relay:write hides the section.
        wizard.update(&mut vcx, |w, cx| {
            w.set_phone_fixture_state(phone_fixture(false, true, false, None, false, None), cx);
        });
        draw(&mut vcx);
        assert!(vcx.debug_bounds("welcome-phone-section").is_none());

        // Restoring relay scopes shows it again; managed policy denying connect hides it.
        wizard.update(&mut vcx, |w, cx| {
            w.set_phone_fixture_state(phone_fixture(false, true, true, None, false, None), cx);
        });
        state.update(&mut vcx, |state, cx| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.managed_policy =
                    Some(json!({"organization": "Acme", "limits": {"connect": false}}));
            }
            cx.emit(AppEvent::ServerConfig);
        });
        draw(&mut vcx);
        assert!(vcx.debug_bounds("welcome-phone-section").is_none());

        // Managed policy denying remote clients also hides it.
        state.update(&mut vcx, |state, cx| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.managed_policy =
                    Some(json!({"organization": "Acme", "limits": {"clients": ["local"]}}));
            }
            cx.emit(AppEvent::ServerConfig);
        });
        draw(&mut vcx);
        assert!(vcx.debug_bounds("welcome-phone-section").is_none());

        // Clearing policy shows it on local/primary, then switching active env to non-local hides it.
        state.update(&mut vcx, |state, cx| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.managed_policy = None;
            }
            cx.emit(AppEvent::ServerConfig);
        });
        draw(&mut vcx);
        assert!(vcx.debug_bounds("welcome-phone-section").is_some());

        state.update(&mut vcx, |state, cx| {
            for env in &mut state.environments.environments {
                env.local = false;
            }
            state.connect.local_server = None;
            cx.emit(AppEvent::Phase);
        });
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("welcome-phone-section").is_none(),
            "Phone section must hide on non-primary/remote environments"
        );

        teardown(vcx, state, wizard, runtime, cx);
    }

    #[gpui::test]
    fn phone_connect_link_renders_all_six_states_and_routes_actions(
        cx: &mut gpui::TestAppContext,
    ) {
        let (runtime, state, wizard, settings_rendered, mut vcx) = mount_welcome(cx);

        // 6. Signed out: shows "Sign in to T3 Connect" button and triggers sign-in action.
        state.update(&mut vcx, |state, cx| {
            state.cloud.identity = None;
            cx.emit(AppEvent::Phase);
        });
        wizard.update(&mut vcx, |w, cx| {
            w.set_phone_fixture_state(phone_fixture(true, true, true, None, false, None), cx);
        });
        draw(&mut vcx);
        assert_eq!(
            wizard.read_with(&vcx, |w, cx| w.phone_connect_state(cx)),
            PhoneConnectState::SignedOut
        );
        let sign_in_center = vcx
            .debug_bounds("welcome-phone-sign-in")
            .expect("Sign in to T3 Connect button")
            .center();
        vcx.simulate_click(sign_in_center, Default::default());
        draw(&mut vcx);
        assert_eq!(wizard.read_with(&vcx, |w, _| w.phone_sign_in_clicks()), 1);

        // Sign in with Clerk userId.
        state.update(&mut vcx, |state, cx| {
            state.cloud.identity = Some("user_alice".into());
            cx.emit(AppEvent::Phase);
        });

        // 1. Checking: signed in, link == None, pending == true.
        wizard.update(&mut vcx, |w, cx| {
            w.set_phone_fixture_state(phone_fixture(true, true, true, None, true, None), cx);
        });
        draw(&mut vcx);
        assert_eq!(
            wizard.read_with(&vcx, |w, cx| w.phone_connect_state(cx)),
            PhoneConnectState::Checking
        );
        assert!(vcx.debug_bounds("welcome-phone-settings").is_none());
        assert!(vcx.debug_bounds("welcome-phone-sign-in").is_none());

        // 2. Error: shows "Check T3 Connect" button.
        wizard.update(&mut vcx, |w, cx| {
            w.set_phone_fixture_state(phone_fixture(true, true, true, None, false, Some("network error".into())), cx);
        });
        draw(&mut vcx);
        assert_eq!(
            wizard.read_with(&vcx, |w, cx| w.phone_connect_state(cx)),
            PhoneConnectState::Error
        );
        assert!(vcx.debug_bounds("welcome-phone-settings").is_some());

        let shared_base = CloudLinkState {
            linked: true,
            cloud_user_id: Some("user_alice".into()),
            link_revoked: Some(false),
            managed_tunnel_active: Some(true),
            ..Default::default()
        };

        // 3. Reachable: checkmark + reachable copy, no action button.
        let reachable = CloudLinkState {
            reachability: Some(ConnectReachability::Reachable {
                checked_at: "2026-11-20T00:00:00Z".into(),
                ready_connections: Some(1),
                protocol: Some("quic".into()),
            }),
            ..shared_base.clone()
        };
        wizard.update(&mut vcx, |w, cx| {
            w.set_phone_fixture_state(phone_fixture(true, true, true, Some(reachable), false, None), cx);
        });
        draw(&mut vcx);
        assert_eq!(
            wizard.read_with(&vcx, |w, cx| w.phone_connect_state(cx)),
            PhoneConnectState::Reachable
        );
        assert!(vcx.debug_bounds("welcome-phone-settings").is_none());

        // 4. Shared + unknown reachability (`None`): no action button.
        wizard.update(&mut vcx, |w, cx| {
            w.set_phone_fixture_state(phone_fixture(true, true, true, Some(shared_base.clone()), false, None), cx);
        });
        draw(&mut vcx);
        assert_eq!(
            wizard.read_with(&vcx, |w, cx| w.phone_connect_state(cx)),
            PhoneConnectState::SharedUnknownReachability
        );
        assert!(vcx.debug_bounds("welcome-phone-settings").is_none());

        // 5a. Shared + unreachable: shows "Check T3 Connect" button.
        let unreachable = CloudLinkState {
            reachability: Some(ConnectReachability::Unreachable {
                checked_at: "2026-11-20T00:00:00Z".into(),
                cause: "tunnel_down".into(),
                reason: "Tunnel disconnected".into(),
                hint: None,
                fix: "Check T3 Connect".into(),
            }),
            ..shared_base
        };
        wizard.update(&mut vcx, |w, cx| {
            w.set_phone_fixture_state(phone_fixture(true, true, true, Some(unreachable), false, None), cx);
        });
        draw(&mut vcx);
        assert_eq!(
            wizard.read_with(&vcx, |w, cx| w.phone_connect_state(cx)),
            PhoneConnectState::SharedUnreachable
        );
        assert!(vcx.debug_bounds("welcome-phone-settings").is_some());

        // 5b. Signed in & not shared: shows "Set up T3 Connect" button, clicking opens Section::Access.
        wizard.update(&mut vcx, |w, cx| {
            w.set_phone_fixture_state(phone_fixture(true, true, true, None, false, None), cx);
        });
        draw(&mut vcx);
        assert_eq!(
            wizard.read_with(&vcx, |w, cx| w.phone_connect_state(cx)),
            PhoneConnectState::NotShared
        );
        let settings_center = vcx
            .debug_bounds("welcome-phone-settings")
            .expect("Set up T3 Connect button")
            .center();
        vcx.simulate_click(settings_center, Default::default());
        draw(&mut vcx);
        assert!(
            !wizard.read_with(&vcx, |w, _| w.is_open()),
            "clicking Set up T3 Connect dismisses WelcomeWizard so Section::Access is visible"
        );
        assert!(
            settings_rendered.load(Ordering::SeqCst),
            "clicking Set up T3 Connect opens Settings overlay at Section::Access"
        );
        assert!(
            !vcx.read(|cx| crate::settings::client_settings(cx).onboarding_completed()),
            "navigating to Sharing & access does not prematurely complete onboarding"
        );

        // Regression test: events emitted while configuring T3 Connect in Settings must NOT
        // cause WelcomeWizard::evaluate() to re-open over Settings (even with a fresh workspace).
        state.update(&mut vcx, |state, cx| {
            state.phase = Phase::Unpaired;
            state.environments.environments.clear();
            cx.emit(AppEvent::Phase);
            cx.emit(AppEvent::Shell);
            cx.emit(AppEvent::ServerConfig);
            cx.emit(AppEvent::Selection);
        });
        draw(&mut vcx);
        assert!(
            !wizard.read_with(&vcx, |w, _| w.is_open()),
            "WelcomeWizard must stay dismissed over Settings when AppEvents arrive"
        );
        assert!(
            settings_rendered.load(Ordering::SeqCst),
            "Settings overlay must remain visible while configuring T3 Connect"
        );

        // Dismiss Settings via backdrop click so SettingsView drops cleanly before TestAppContext exits.
        vcx.simulate_click(point(px(2.), px(2.)), Default::default());
        draw(&mut vcx);
        assert!(!settings_rendered.load(Ordering::SeqCst));

        // Explicit reopen (e.g. About -> Welcome page) still opens WelcomeWizard cleanly.
        wizard.update(&mut vcx, |w, cx| w.show(Step::Connection, cx));
        draw(&mut vcx);
        assert!(wizard.read_with(&vcx, |w, _| w.is_open()));

        teardown(vcx, state, wizard, runtime, cx);
    }

    #[gpui::test]
    fn telemetry_notice_privacy_policy_link_is_keyboard_accessible(
        cx: &mut gpui::TestAppContext,
    ) {
        let (runtime, state, wizard, _settings_rendered, mut vcx) = mount_welcome(cx);

        assert!(vcx.debug_bounds("welcome-telemetry-notice").is_some());
        assert!(vcx.debug_bounds("welcome-privacy-policy").is_some());

        // Focus the privacy policy link by tabbing through the WelcomeWizard focus ring.
        let link_focus = wizard.read_with(&vcx, |w, _| w.privacy_focus_handle());
        let mut reached_via_tab = false;
        for _ in 0..64 {
            vcx.simulate_keystrokes("tab");
            draw(&mut vcx);
            if vcx.update(|window, _| link_focus.is_focused(window)) {
                reached_via_tab = true;
                break;
            }
        }
        assert!(reached_via_tab, "Tab must reach the welcome privacy policy link");

        // Pressing Enter activates the link.
        vcx.simulate_keystrokes("enter");
        draw(&mut vcx);
        assert_eq!(wizard.read_with(&vcx, |w, _| w.privacy_policy_clicks()), 1);

        // Pressing Space also activates the link.
        vcx.simulate_keystrokes("space");
        draw(&mut vcx);
        assert_eq!(wizard.read_with(&vcx, |w, _| w.privacy_policy_clicks()), 2);

        // Non-English locales translate TelemetryNotice using the real catalog keys (incl. ja word order).
        vcx.update(|_, cx| {
            crate::settings::store::update_client(&state, ClientSettingChange::Language("de".into()), cx);
        });
        draw(&mut vcx);
        vcx.read(|cx| {
            assert_eq!(crate::i18n::active_locale(cx), "de");
            assert_ne!(crate::i18n::t(cx, TELEMETRY_NOTICE_DEFAULT), TELEMETRY_NOTICE_DEFAULT);
            assert_ne!(crate::i18n::t(cx, TELEMETRY_NOTICE_SEE_POLICY), TELEMETRY_NOTICE_SEE_POLICY);
            assert_eq!(crate::i18n::t(cx, PRIVACY_POLICY_LINK_LABEL), "Datenschutzerklärung");
        });
        assert!(vcx.debug_bounds("welcome-telemetry-notice").is_some());
        assert!(vcx.debug_bounds("welcome-privacy-policy").is_some());

        vcx.update(|_, cx| {
            crate::settings::store::update_client(&state, ClientSettingChange::Language("ja".into()), cx);
        });
        draw(&mut vcx);
        vcx.read(|cx| {
            assert_eq!(crate::i18n::active_locale(cx), "ja");
            assert_eq!(crate::i18n::t(cx, PRIVACY_POLICY_LINK_LABEL), "プライバシーポリシー");
        });
        assert!(vcx.debug_bounds("welcome-privacy-policy").is_some());

        vcx.update(|_, cx| {
            crate::settings::store::update_client(&state, ClientSettingChange::Language("en".into()), cx);
        });
        draw(&mut vcx);

        // Managed telemetry-off policy still renders TelemetryNotice with keyboard-accessible link.
        state.update(&mut vcx, |state, cx| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.managed_policy =
                    Some(json!({"organization": "Acme", "limits": {"telemetry": false}}));
            }
            cx.emit(AppEvent::ServerConfig);
        });
        draw(&mut vcx);
        assert!(vcx.debug_bounds("welcome-telemetry-notice").is_some());
        let link_bounds = vcx
            .debug_bounds("welcome-privacy-policy")
            .expect("Privacy policy link");
        let viewport = vcx.update(|window, _| window.viewport_size());
        vcx.simulate_click(link_bounds.center(), Default::default());
        draw(&mut vcx);
        assert_eq!(
            wizard.read_with(&vcx, |w, _| w.privacy_policy_clicks()),
            3,
            "link_bounds={link_bounds:?} viewport={viewport:?}"
        );

        teardown(vcx, state, wizard, runtime, cx);
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    fn snapshot_mode_opens_welcome_phone_without_background_timers_or_network(
        cx: &mut gpui::TestAppContext,
    ) {
        crate::snapshot_capture::set_snapshot_mode(true);
        let (runtime, state, wizard, _settings_rendered, mut vcx) = mount_welcome(cx);

        vcx.update(|_, cx| super::super::open(Step::Connection, cx));
        wizard.update(&mut vcx, |w, cx| {
            w.set_phone_fixture_state(phone_fixture(false, true, true, None, false, None), cx);
        });
        draw(&mut vcx);

        let toggle_center = vcx
            .debug_bounds("welcome-phone-toggle")
            .expect("Phone toggle in snapshot mode")
            .center();
        vcx.simulate_click(toggle_center, Default::default());
        draw(&mut vcx);

        assert!(wizard.read_with(&vcx, |w, _| w.is_phone_open()));
        assert!(vcx.debug_bounds("welcome-phone-qr").is_some());
        assert!(
            !wizard.read_with(&vcx, |w, _| w.phone_has_background_tasks()),
            "snapshot mode must suppress 30s refresh timers and background network calls"
        );

        teardown(vcx, state, wizard, runtime, cx);
    }
}
