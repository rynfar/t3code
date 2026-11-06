use super::*;
use serde_json::json;
use t3_client::{session::RpcError, supervisor::ClientFixture};

fn setup(
    cx: &mut gpui::TestAppContext,
) -> (Entity<AppState>, ClientFixture, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, fixture) = cx.update(|cx| {
        crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
    });
    state.update(cx, |state, cx| {
        let config = state.server_config.config.as_mut().unwrap();
        config.settings.guest_sharing_enabled = Some(true);
        config.environment.capabilities.thread_project_from_scratch = Some(true);
        config.scratch_workspace_root = Some("/fixture/b".into());
        state
            .shell
            .snapshot
            .as_mut()
            .unwrap()
            .threads
            .iter_mut()
            .find(|thread| thread.id == "menu-thread")
            .unwrap()
            .worktree_path = Some("/fixture/wt".into());
        state.rebuild_sidebar(cx);
        assert!(state.selected_thread.is_none());
    });
    fixture.grants.send(Some(serde_json::from_value(json!({"authenticated":true,"permissions":["access:write", "orchestration:operate"]})).unwrap())).unwrap();
    (state, fixture, runtime)
}
fn created() -> serde_json::Value {
    json!({"url":"https://fixture.invalid/guest?synthetic", "share":{"id":"share-1", "host":"fixture.invalid", "createdAt":"2026-01-01T00:00:00Z", "expiresAt":"2026-01-02T00:00:00Z", "guestName":null, "messages":[]}})
}

#[gpui::test]
async fn guest_create_uses_existing_payload_and_refuses_stale_results(
    cx: &mut gpui::TestAppContext,
) {
    cx.executor().allow_parking();
    for mode in [
        "success",
        "failure",
        "interrupted",
        "generation",
        "revoked",
        "thread-replaced",
    ] {
        let (state, mut fixture, _runtime) = setup(cx);
        let (dialog, cx) =
            cx.add_window_view(|window, cx| GuestShareDialog::new(state.clone(), window, cx));
        cx.update(|window, cx| {
            let owner = ThreadMenuOwner::capture(state.read(cx), "menu-thread").unwrap();
            open(owner, window.window_handle().into(), cx);
            dialog.update(cx, |dialog, cx| dialog.create(window, cx));
        });
        let task = dialog.update(cx, |dialog, _| dialog.task.take().unwrap());
        let (method, payload, reply) = fixture.next_request().await;
        assert_eq!(method, rules::CREATE);
        assert_eq!(payload, json!({"threadId":"menu-thread"}));
        cx.update(|window, cx| {
            dialog.update(cx, |dialog, cx| {
                dialog.close(window, cx);
                assert!(dialog.pending.is_some(), "creating cannot be cancelled");
            })
        });
        match mode {
            "generation" => {
                fixture.generation.send(2).unwrap();
            }
            "revoked" => {
                fixture
                    .grants
                    .send(Some(crate::context_menu::project_filter_tests::session(
                        true,
                    )))
                    .unwrap();
            }
            "thread-replaced" => state.update(cx, |state, _| {
                state
                    .shell
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .threads
                    .iter_mut()
                    .find(|thread| thread.id == "menu-thread")
                    .unwrap()
                    .created_at = "replacement".into();
            }),
            _ => {}
        }
        let result = if mode == "failure" {
            Err(RpcError::Closed)
        } else if mode == "interrupted" {
            Err(RpcError::Failure(t3_core::rpc::RpcFailure {
                tag: None,
                message: "Interrupted".into(),
                error: json!(null),
            }))
        } else {
            Ok(created())
        };
        assert!(reply.send(result).is_ok());
        task.await;
        dialog.read_with(cx, |dialog, _| match mode {
            "success" => assert!(matches!(dialog.pending.as_ref().map(|pending| &pending.state), Some(CreateState::Created(url, false)) if url == "https://fixture.invalid/guest?synthetic")),
            "interrupted" => assert!(matches!(dialog.pending.as_ref().map(|pending| &pending.state), Some(CreateState::Idle))),
            "failure" => assert!(matches!(dialog.pending.as_ref().map(|pending| &pending.state), Some(CreateState::Failed(message)) if message == "connection closed")),
            _ => assert!(dialog.pending.is_none(), "stale result must discard its URL"),
        });
        assert!(fixture.take_commands().is_empty());
    }
}

#[gpui::test]
async fn promotion_uses_captured_destination_without_navigation(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    for mode in [
        "success",
        "failure",
        "interrupted",
        "generation",
        "revoked",
        "thread-replaced",
    ] {
        let (state, mut fixture, _runtime) = setup(cx);
        let notices = std::rc::Rc::new(std::cell::Cell::new(0));
        let changed = notices.clone();
        let _subscription = state.update(cx, |_, cx| {
            cx.subscribe(&state, move |_, _, event: &AppEvent, _| {
                if *event == AppEvent::Notice {
                    changed.set(changed.get() + 1);
                }
            })
        });
        let before = state.read_with(cx, |state, _| {
            (state.selected_thread.clone(), state.draft_project.clone())
        });
        let task = state.update(cx, |state, cx| {
            state.turn_into_project("menu-thread", cx).unwrap()
        });
        let (method, payload, reply) = fixture.next_request().await;
        assert_eq!(method, t3_core::projects::PROJECTS_CREATE_FROM_THREAD);
        assert_eq!(payload, json!({"threadId":"menu-thread"}));
        match mode {
            "generation" => {
                fixture.generation.send(2).unwrap();
            }
            "revoked" => {
                fixture
                    .grants
                    .send(Some(crate::context_menu::project_filter_tests::session(
                        false,
                    )))
                    .unwrap();
            }
            "thread-replaced" => state.update(cx, |state, _| {
                state
                    .shell
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .threads
                    .iter_mut()
                    .find(|thread| thread.id == "menu-thread")
                    .unwrap()
                    .created_at = "replacement".into();
            }),
            _ => {}
        }
        assert!(
            reply
                .send(if mode == "failure" {
                    Err(RpcError::Closed)
                } else if mode == "interrupted" {
                    Err(RpcError::Failure(t3_core::rpc::RpcFailure {
                        tag: None,
                        message: "Interrupted".into(),
                        error: json!(null),
                    }))
                } else {
                    Ok(json!({"projectId":"promoted"}))
                })
                .is_ok()
        );
        task.await;
        state.read_with(cx, |state, _| {
            assert_eq!(
                (state.selected_thread.clone(), state.draft_project.clone()),
                before
            );
            assert_eq!(
                notices.get(),
                usize::from(matches!(mode, "success" | "failure"))
            );
        });
        assert!(fixture.take_commands().is_empty());
    }
}

#[gpui::test]
async fn stop_sharing_rechecks_the_exact_share_and_live_operate_grant(
    cx: &mut gpui::TestAppContext,
) {
    cx.executor().allow_parking();
    for mode in [
        "success",
        "failure",
        "generation",
        "revoked",
        "share-replaced",
    ] {
        let (state, mut fixture, _runtime) = setup(cx);
        state.update(cx, |state, _| {
            let mut thread =
                serde_json::to_value(state.thread_shell("menu-thread").unwrap()).unwrap();
            thread["guestShare"] = created()["share"].clone();
            let mut projection = json!({"thread":thread, "updatedAt":"2026-01-01T00:00:00Z"});
            for key in [
                "runs",
                "attempts",
                "nodes",
                "subagents",
                "providerSessions",
                "providerThreads",
                "providerTurns",
                "runtimeRequests",
                "messages",
                "plans",
                "turnItems",
                "checkpointScopes",
                "checkpoints",
                "contextHandoffs",
                "contextTransfers",
                "visibleTurnItems",
            ] {
                projection[key] = json!([]);
            }
            state.thread.projection = Some(serde_json::from_value(projection).unwrap());
            state.selected_thread = Some("menu-thread".into());
        });
        let (indicator, cx) = cx.add_window_view(|_, cx| GuestIndicator::new(state.clone(), cx));
        indicator.update(cx, |view, cx| view.stop(cx));
        let task = indicator.update(cx, |view, _| view.task.take().unwrap());
        let (method, payload, reply) = fixture.next_request().await;
        assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(payload["type"], "thread.guest-share.close");
        assert_eq!(payload["threadId"], "menu-thread");
        assert!(
            payload["commandId"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        match mode {
            "generation" => {
                fixture.generation.send(2).unwrap();
            }
            "revoked" => {
                fixture
                    .grants
                    .send(Some(crate::context_menu::project_filter_tests::session(
                        false,
                    )))
                    .unwrap();
            }
            "share-replaced" => state.update(cx, |state, _| {
                state
                    .thread
                    .projection
                    .as_mut()
                    .unwrap()
                    .thread
                    .guest_share
                    .as_mut()
                    .unwrap()
                    .id = "replacement".into()
            }),
            _ => {}
        }
        assert!(
            reply
                .send(if mode == "failure" {
                    Err(RpcError::Closed)
                } else {
                    Ok(json!({"sequence":1}))
                })
                .is_ok()
        );
        task.await;
        indicator.read_with(cx, |view, _| assert!(view.closing.is_none()));
        fixture
            .grants
            .send(Some(crate::context_menu::project_filter_tests::session(
                false,
            )))
            .unwrap();
        indicator.update(cx, |view, cx| view.stop(cx));
        assert!(
            fixture.take_commands().is_empty(),
            "revoked operate refuses another close"
        );
    }
}

struct GuestFrame {
    dialog: Entity<GuestShareDialog>,
    outside: FocusHandle,
}
impl Render for GuestFrame {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .bg(hsla(palette(cx).background))
            .child(
                div()
                    .track_focus(&self.outside)
                    .child("Synthetic guest link fixture"),
            )
            .child(self.dialog.clone())
    }
}
#[test]
fn guest_dialog_frames_and_keyboard_focus() {
    use gpui::{AppContext, HeadlessAppContext, Keystroke, size};
    let Some(out) = std::env::var_os("T3_MENU_CAPTURE") else {
        return;
    };
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        std::sync::Arc::new(crate::icons::Assets),
        gpui_platform::current_headless_renderer,
    );
    let (state, mut fixture) = cx.update(|cx| {
        crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
    });
    state.update(&mut cx, |state, _| {
        state
            .server_config
            .config
            .as_mut()
            .unwrap()
            .settings
            .guest_sharing_enabled = Some(true)
    });
    fixture
        .grants
        .send(Some(
            serde_json::from_value(json!({"authenticated":true,"permissions":["access:write"]}))
                .unwrap(),
        ))
        .unwrap();
    let mut dialog = None;
    let mut outside = None;
    let window = cx
        .open_window(size(px(900.), px(700.)), |window, cx| {
            let view = cx.new(|cx| GuestShareDialog::new(state.clone(), window, cx));
            let focus = cx.focus_handle().tab_stop(true);
            window.focus(&focus, cx);
            dialog = Some(view.clone());
            outside = Some(focus.clone());
            cx.new(|_| GuestFrame {
                dialog: view,
                outside: focus,
            })
        })
        .unwrap();
    let dialog = dialog.unwrap();
    let outside = outside.unwrap();
    for dark in [false, true] {
        cx.update(|cx| {
            let mut preferences = crate::theme::preferences(cx).clone();
            let mode = if dark {
                t3_core::client_settings::AppearanceMode::Dark
            } else {
                t3_core::client_settings::AppearanceMode::Light
            };
            preferences.theme = t3_core::client_settings::ThemePreference::Default(mode);
            preferences.mode = mode;
            crate::theme::set_preferences(preferences, cx);
        });
        for phase in ["idle", "existing", "creating", "failed", "created"] {
            cx.update_window(window.into(), |_, window, cx| {
                dialog.update(cx, |dialog, cx| {
                    let owner = ThreadMenuOwner::capture(state.read(cx), "menu-thread").unwrap();
                    dialog.previous_focus = Some(outside.clone());
                    dialog.active_share = (phase == "existing")
                        .then(|| serde_json::from_value(created()["share"].clone()).unwrap());
                    dialog.pending = Some(Pending {
                        owner,
                        state: match phase {
                            "creating" => CreateState::Creating,
                            "failed" => CreateState::Failed(
                                "Guest links need a running T3 Connect tunnel.".into(),
                            ),
                            "created" => {
                                dialog.link.update(cx, |input, cx| {
                                    input.set_value(
                                        "https://fixture.invalid/guest?synthetic",
                                        window,
                                        cx,
                                    )
                                });
                                CreateState::Created(
                                    "https://fixture.invalid/guest?synthetic".into(),
                                    false,
                                )
                            }
                            _ => CreateState::Idle,
                        },
                    });
                    window.focus(&dialog.focus, cx);
                    cx.notify();
                })
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear();
            })
            .unwrap();
            cx.capture_screenshot(window.into())
                .unwrap()
                .save(out.join(format!(
                    "guest-{}-{phase}.png",
                    if dark { "dark" } else { "light" }
                )))
                .unwrap();
            cx.update_window(window.into(), |_, window, cx| {
                window.dispatch_keystroke(Keystroke::parse("tab").unwrap(), cx);
                assert!(dialog.read(cx).focus.contains_focused(window, cx));
                assert!(!outside.is_focused(window));
                if phase == "created" {
                    assert!(
                        dialog
                            .read(cx)
                            .link
                            .read(cx)
                            .focus_handle(cx)
                            .is_focused(window)
                    );
                    // Focus assertions precede the headless NSView raster accommodation.
                    let footer = dialog.read(cx).footer_focus[1].clone();
                    window.focus(&footer, cx);
                }
                window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
                if phase == "creating" {
                    assert!(dialog.read(cx).pending.is_some());
                    dialog.update(cx, |dialog, _| {
                        dialog.pending.as_mut().unwrap().state = CreateState::Idle
                    });
                    window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
                }
                assert!(dialog.read(cx).pending.is_none());
                assert!(outside.is_focused(window));
            })
            .unwrap();
        }
    }
    assert!(
        fixture.take_commands().is_empty(),
        "rendering and cancellation never create links or write the clipboard"
    );
    state.update(&mut cx, |state, _| state.stop_pump());
}
