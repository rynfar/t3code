use super::*;
use std::{cell::RefCell, rc::Rc};

#[gpui::test]
fn menu_refuses_replaced_thread_before_rename_callback(cx: &mut gpui::TestAppContext) {
    for mode in [
        "thread-replaced",
        "thread-missing",
        "archived",
        "deleted",
        "moved",
        "branch",
        "project-replaced",
        "root",
        "environment",
        "saved",
        "shell",
        "generation",
        "revoked",
        "unknown-grants",
        "disconnected",
    ] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (state, mut fixture) = cx
            .update(|cx| super::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (menu, cx) = cx.add_window_view(|_, cx| ContextMenu::new(state.clone(), cx));
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let _subscription = state.update(cx, |_, cx| {
            cx.subscribe(&menu, move |_, _, event, _| {
                if let ContextMenuEvent::Rename(id, _) = event {
                    recorded.borrow_mut().push(id.thread.clone());
                }
            })
        });
        cx.update(|window, cx| {
            menu.update(cx, |menu, cx| {
                menu.open_for_thread("menu-thread", point(px(10.), px(10.)), window, cx)
            })
        });
        match mode {
            "generation" => {
                fixture.generation.send(2).unwrap();
            }
            "revoked" => {
                fixture
                    .grants
                    .send(Some(project_filter_tests::session(false)))
                    .unwrap();
            }
            "unknown-grants" => {
                fixture.grants.send(None).unwrap();
            }
            _ => state.update(cx, |state, _| match mode {
                "environment" => {
                    state
                        .server_config
                        .config
                        .as_mut()
                        .unwrap()
                        .environment
                        .environment_id = "replacement".into()
                }
                "saved" => state.environments.active = None,
                "shell" => state.shell.generation += 1,
                "disconnected" => state.phase = crate::app_state::Phase::Unpaired,
                "project-replaced" => {
                    state
                        .shell
                        .snapshot
                        .as_mut()
                        .unwrap()
                        .projects
                        .iter_mut()
                        .find(|project| project.id == "checkout-b")
                        .unwrap()
                        .created_at = "replacement".into()
                }
                "root" => {
                    state
                        .shell
                        .snapshot
                        .as_mut()
                        .unwrap()
                        .projects
                        .iter_mut()
                        .find(|project| project.id == "checkout-b")
                        .unwrap()
                        .workspace_root = "/replacement".into()
                }
                "thread-missing" => state
                    .shell
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .threads
                    .retain(|thread| thread.id != "menu-thread"),
                _ => {
                    let thread = state
                        .shell
                        .snapshot
                        .as_mut()
                        .unwrap()
                        .threads
                        .iter_mut()
                        .find(|thread| thread.id == "menu-thread")
                        .unwrap();
                    match mode {
                        "archived" => thread.archived_at = Some("2026-02-01T00:00:00Z".into()),
                        "deleted" => thread.deleted_at = Some("2026-02-01T00:00:00Z".into()),
                        "moved" => thread.project_id = "other".into(),
                        "branch" => thread.branch = Some("replacement".into()),
                        _ => thread.created_at = "2026-02-01T00:00:00Z".into(),
                    }
                }
            }),
        }
        cx.update(|_, cx| {
            menu.update(cx, |menu, cx| {
                menu.activate(MenuAction::Thread(ThreadAction::Rename), cx)
            })
        });
        assert!(
            events.borrow().is_empty(),
            "{mode}: stale menu callback must be refused"
        );
        assert!(fixture.take_commands().is_empty());
    }
}

#[gpui::test]
fn header_find_uses_its_current_target_and_sidebar_omits_it(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let (state, mut fixture) =
        cx.update(|cx| project_filter_tests::install_state(cx, runtime.handle().clone()));
    let found = Rc::new(std::cell::Cell::new(0));
    let record = found.clone();
    cx.update(|cx| {
        cx.on_action(move |_: &crate::thread_view::FindInThread, _| record.set(record.get() + 1));
    });
    let (menu, cx) = cx.add_window_view(|_, cx| ContextMenu::new(state.clone(), cx));
    state.update(cx, |state, _| {
        state.selected_thread = Some("menu-thread".into())
    });
    fixture
        .grants
        .send(Some(project_filter_tests::session(false)))
        .unwrap();
    cx.update(|window, cx| {
        menu.update(cx, |menu, cx| {
            menu.open_for_thread("menu-thread", point(px(10.), px(10.)), window, cx);
            assert!(
                !menu
                    .open
                    .as_ref()
                    .unwrap()
                    .items
                    .iter()
                    .any(|item| item.action == MenuAction::Thread(ThreadAction::Find))
            );
            menu.open_for_header("menu-thread", point(px(10.), px(10.)), window, cx);
            assert_eq!(
                menu.open.as_ref().unwrap().items[0].action,
                MenuAction::Thread(ThreadAction::Find)
            );
            assert!(!menu.open.as_ref().unwrap().items[0].disabled);
            menu.activate(MenuAction::Thread(ThreadAction::Find), cx);
        })
    });
    assert_eq!(found.get(), 1);
    assert!(fixture.take_commands().is_empty());
    cx.update(|window, cx| {
        menu.update(cx, |menu, cx| {
            menu.open_for_header("menu-thread", point(px(10.), px(10.)), window, cx)
        })
    });
    state.update(cx, |state, _| {
        state.selected_thread = Some("sibling".into())
    });
    cx.update(|_, cx| {
        menu.update(cx, |menu, cx| {
            menu.activate(MenuAction::Thread(ThreadAction::Find), cx)
        })
    });
    assert_eq!(
        found.get(),
        1,
        "a stale header never focuses its replacement thread"
    );
}

struct MenuFrame {
    menu: Entity<ContextMenu>,
    outside: FocusHandle,
    guest: Option<Entity<crate::guest_share::GuestShareDialog>>,
}
impl Render for MenuFrame {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .bg(hsla(palette(cx).background))
            .text_color(hsla(palette(cx).foreground))
            .child(
                div()
                    .track_focus(&self.outside)
                    .child("Synthetic thread menu"),
            )
            .child(self.menu.clone())
            .children(self.guest.clone())
    }
}

#[test]
fn production_menu_frames() {
    use gpui::{AppContext, HeadlessAppContext, Keystroke, PlatformInput, size};
    use serde_json::json;
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
    cx.allow_parking();
    let (state, mut fixture) = cx.update(|cx| {
        let installed = project_filter_tests::install_state(cx, runtime.handle().clone());
        crate::keymap::init(cx);
        installed
    });
    assert!(state.read_with(&cx, |state, _| state.selected_thread.is_none()));
    state.update(&mut cx, |state, cx| {
        let config = state.server_config.config.as_mut().unwrap();
        config.settings.guest_sharing_enabled = Some(true);
        config.environment.capabilities.thread_project_from_scratch = Some(true);
        config.environment.capabilities.thread_auto_settle_opt_out = Some(true);
        config.scratch_workspace_root = Some("/fixture/b".into());
        let thread = state
            .shell
            .snapshot
            .as_mut()
            .unwrap()
            .threads
            .iter_mut()
            .find(|thread| thread.id == "menu-thread")
            .unwrap();
        thread.branch = Some("feature/captured".into());
        thread.worktree_path = Some("/fixture/worktree".into());
        state.sync_server_config(cx);
    });
    let grant = |operate| {
        serde_json::from_value(json!({"authenticated":true,"permissions":if operate {vec!["access:write","orchestration:operate"]} else {vec!["access:write"]}})).unwrap()
    };
    fixture.grants.send(Some(grant(true))).unwrap();
    let mut handles = None;
    let window = cx
        .open_window(size(px(1000.), px(820.)), |window, cx| {
            let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
            let guest =
                cx.new(|cx| crate::guest_share::GuestShareDialog::new(state.clone(), window, cx));
            let outside = cx.focus_handle();
            window.focus(&outside, cx);
            handles = Some((menu.clone(), outside.clone()));
            cx.new(|_| MenuFrame {
                menu,
                outside,
                guest: Some(guest),
            })
        })
        .unwrap();
    let (menu, outside) = handles.unwrap();
    let capture = |cx: &mut HeadlessAppContext, name: &str| {
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();
        cx.capture_screenshot(window.into())
            .unwrap()
            .save(out.join(format!("{name}.png")))
            .unwrap();
    };
    for dark in [false, true] {
        let theme = if dark { "dark" } else { "light" };
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
        cx.update_window(window.into(), |_, window, cx| {
            menu.update(cx, |menu, cx| {
                menu.open_for_thread("menu-thread", point(px(20.), px(30.)), window, cx)
            })
        })
        .unwrap();
        capture(&mut cx, &format!("{theme}-sidebar-menu"));
        cx.update_window(window.into(), |_, window, cx| {
            let items = &menu.read(cx).open.as_ref().unwrap().items;
            let count = ContextMenu::enabled(items)
                .iter()
                .position(|index| items[*index].action == MenuAction::Thread(ThreadAction::Copy))
                .unwrap();
            for _ in 0..=count {
                window.dispatch_keystroke(Keystroke::parse("down").unwrap(), cx);
            }
            window.dispatch_keystroke(Keystroke::parse("right").unwrap(), cx);
            assert!(menu.read(cx).open.as_ref().unwrap().submenu.is_some());
        })
        .unwrap();
        capture(&mut cx, &format!("{theme}-copy-submenu"));
        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
            assert!(menu.read(cx).is_open());
            assert!(menu.read(cx).open.as_ref().unwrap().submenu.is_none());
            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
            assert!(!menu.read(cx).is_open());
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, _| {
            assert!(outside.is_focused(window))
        })
        .unwrap();
        fixture.grants.send(Some(grant(false))).unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            menu.update(cx, |menu, cx| {
                menu.open_for_thread("menu-thread", point(px(20.), px(30.)), window, cx);
                let items = &menu.open.as_ref().unwrap().items;
                assert!(
                    !items
                        .iter()
                        .find(|item| item.action == MenuAction::Thread(ThreadAction::ShareGuest))
                        .unwrap()
                        .disabled
                );
                assert!(
                    items
                        .iter()
                        .find(|item| item.action == MenuAction::Thread(ThreadAction::AutoSettle))
                        .unwrap()
                        .children
                        .iter()
                        .all(|child| child.disabled)
                );
            })
        })
        .unwrap();
        capture(&mut cx, &format!("{theme}-read-only"));
        cx.update_window(window.into(), |_, _, cx| {
            menu.update(cx, |menu, cx| {
                menu.activate(MenuAction::Thread(ThreadAction::AutoSettleEnabled), cx)
            })
        })
        .unwrap();
        assert!(
            !fixture
                .take_commands()
                .contains(&t3_core::commands::method::DISPATCH_COMMAND)
        );
        fixture.grants.send(Some(grant(true))).unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            menu.update(cx, |menu, cx| {
                menu.open_for_thread("menu-thread", point(px(20.), px(30.)), window, cx)
            })
        })
        .unwrap();
        capture(&mut cx, &format!("{theme}-pointer-before-cancel"));
        cx.update_window(window.into(), |_, window, cx| {
            let position = point(px(900.), px(760.));
            window.simulate_mouse_move(position, cx);
            window.dispatch_event(
                PlatformInput::MouseDown(gpui::MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(gpui::MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                }),
                cx,
            );
            assert!(!menu.read(cx).is_open());
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            assert!(outside.is_focused(window));
            menu.update(cx, |menu, cx| {
                menu.open_for_thread("menu-thread", point(px(20.), px(30.)), window, cx);
                menu.activate(MenuAction::Thread(ThreadAction::ShareGuest), cx);
            });
        })
        .unwrap();
        capture(&mut cx, &format!("{theme}-guest-from-sidebar"));
        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, _| {
            assert!(outside.is_focused(window))
        })
        .unwrap();
    }
    let commands = fixture.take_commands();
    assert!(
        commands
            .iter()
            .all(|method| *method == t3_core::commands::method::SUBSCRIBE_THREAD),
        "menu navigation/cancel produced commands: {commands:?}"
    );
    state.update(&mut cx, |state, _| state.stop_pump());
}

#[test]
fn branch_menu_preserves_an_open_draft_and_its_restored_workspace() {
    use gpui::{HeadlessAppContext, size};
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        std::sync::Arc::new(crate::icons::Assets),
        gpui_platform::current_headless_renderer,
    );
    use gpui::AppContext;
    cx.allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, mut fixture) = cx.update(|cx| {
        let installed = project_filter_tests::install_state(cx, runtime.handle().clone());
        crate::composer::init(cx);
        crate::keymap::init(cx);
        installed
    });
    cx.update(|_cx| t3_client::store::save_ui_state(t3_core::composer::drafts::STORE_NAME, &serde_json::json!({"version":1, "drafts":{"draft:checkout-b":{"prompt":"Keep this draft", "updatedAt":1}}})));
    state.update(&mut cx, |state, cx| state.start_draft("checkout-b", cx));
    let mut handles = None;
    let window = cx
        .open_window(size(px(720.), px(760.)), |window, cx| {
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
            handles = Some((composer, menu.clone()));
            cx.new(|cx| MenuFrame {
                menu,
                outside: cx.focus_handle(),
                guest: None,
            })
        })
        .unwrap();
    let (composer, menu) = handles.unwrap();
    for path in [None, Some("/fixture/worktree")] {
        state.update(&mut cx, |state, cx| {
            let thread = state
                .shell
                .snapshot
                .as_mut()
                .unwrap()
                .threads
                .iter_mut()
                .find(|thread| thread.id == "menu-thread")
                .unwrap();
            thread.branch = Some("feature/captured".into());
            thread.worktree_path = path.map(str::to_owned);
            state.rebuild_sidebar(cx);
        });
        cx.update_window(window.into(), |_, window, cx| {
            menu.update(cx, |menu, cx| {
                menu.open_for_thread("menu-thread", point(px(10.), px(10.)), window, cx);
                menu.activate(MenuAction::Thread(ThreadAction::NewThreadOnBranch), cx);
            })
        })
        .unwrap();
        let audit = composer.read_with(&cx, |composer, cx| composer.snapshot_audit(0, cx));
        assert_eq!(audit["prompt"], "Keep this draft");
        let choice: t3_core::workspace::DraftWorkspace =
            serde_json::from_value(audit["workspace"].clone()).unwrap();
        assert_eq!(choice.branch.as_deref(), Some("feature/captured"));
        assert_eq!(
            choice
                .previous
                .as_ref()
                .map(|seed| seed.worktree_path.as_str()),
            path
        );
        assert_eq!(
            choice.mode,
            if path.is_some() {
                t3_core::server_config::ThreadEnvMode::Worktree
            } else {
                t3_core::server_config::ThreadEnvMode::Local
            }
        );
        assert_eq!(
            choice.strategy().unwrap().to_json()["branch"],
            "feature/captured"
        );
        state.read_with(&cx, |state, _| {
            assert_eq!(state.draft_project.as_deref(), Some("checkout-b"));
            assert!(state.selected_thread.is_none());
        });
    }
    assert!(
        !fixture.take_commands().iter().any(|method| matches!(
            *method,
            "orchestration.launchThread" | "orchestration.dispatchCommand"
        )),
        "opening a branch draft never launches a turn"
    );
    state.update(&mut cx, |state, _| state.stop_pump());
}

#[test]
fn restored_menu_seed_never_falls_through_launch_defaults() {
    use futures::{FutureExt, select};
    use gpui::{AppContext, HeadlessAppContext, size};
    use serde_json::json;
    for mode in [
        "entry-environment",
        "entry-project",
        "await-environment",
        "await-project",
        "valid-root",
        "valid-worktree",
        "ordinary",
    ] {
        if std::env::var("T3_MENU_LAUNCH_CASE").as_deref() == Ok("await")
            && !mode.starts_with("await-")
        {
            continue;
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut cx = HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(),
            std::sync::Arc::new(crate::icons::Assets),
            gpui_platform::current_headless_renderer,
        );
        cx.allow_parking();
        let executor = cx.update(|cx| cx.foreground_executor().clone());
        let (state, mut fixture) = cx.update(|cx| {
            let installed = project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            installed
        });
        let across_await = mode.starts_with("await-");
        let workspace = json!({"environmentId":"saved-fixture","projectRoot":"/fixture/b", "destination":["fixture-env","2026-01-01T00:00:00Z"], "branch":if across_await {None} else {Some("feature/captured")}, "mode":if across_await || mode == "valid-worktree" {"worktree"} else {"local"}, "previous":if mode == "valid-worktree" {json!({"branch":"feature/captured","worktreePath":"/fixture/worktree"})} else {json!(null)}});
        let workspace = if mode == "ordinary" {
            json!(null)
        } else {
            workspace
        };
        t3_client::store::save_ui_state(
            t3_core::composer::drafts::STORE_NAME,
            &json!({"version":1,"drafts":{"draft:checkout-b":{"prompt":"Keep restored draft", "workspace":workspace, "updatedAt":1}}}),
        );
        state.update(&mut cx, |state, cx| {
            let config = state.server_config.config.as_mut().unwrap();
            config.providers = vec![serde_json::from_value(json!({"instanceId":"replay","driver":"replay","enabled":true,"installed":true,"status":"ready","models":[{"slug":"replay","name":"Replay","isDefault":true}]})).unwrap()];
            config.settings.provider_instances = serde_json::from_value(json!({"replay":{"driver":"replay","enabled":true}})).unwrap();
            config.settings.default_thread_env_mode = Some(t3_core::server_config::ThreadEnvMode::Local);
            if mode.starts_with("entry-") {
                if mode.ends_with("environment") { config.environment.environment_id = "replacement".into(); }
                else { state.shell.snapshot.as_mut().unwrap().projects.iter_mut().find(|project| project.id == "checkout-b").unwrap().created_at = "replacement".into(); }
            }
            state.start_draft("checkout-b", cx);
        });
        let mut composer = None;
        let window = cx
            .open_window(size(px(720.), px(760.)), |window, cx| {
                composer =
                    Some(cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx)));
                let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
                cx.new(|cx| MenuFrame {
                    menu,
                    outside: cx.focus_handle(),
                    guest: None,
                })
            })
            .unwrap();
        let composer = composer.unwrap();
        let (send, done) = tokio::sync::oneshot::channel();
        let mut send = Some(send);
        let _observer = state.update(&mut cx, |_, cx| {
            cx.observe(&composer, move |_, composer, cx| {
                let (sending, error) = composer.read(cx).menu_launch_outcome();
                if !sending && error.is_some() {
                    if let Some(send) = send.take() {
                        let _ = send.send(());
                    }
                }
            })
        });
        cx.update_window(window.into(), |_, window, cx| {
            composer.update(cx, |composer, cx| composer.exercise_menu_launch(window, cx))
        })
        .unwrap();
        let mut changed = !across_await;
        let mut done = done.fuse();
        let escaped = executor.block_test(async {
            loop {
                select! {
                    _ = done => break None,
                    request = fixture.next_request().fuse() => {
                        let (method, payload, reply) = request;
                        if method == t3_core::commands::method::LAUNCH_THREAD {
                            let _ = reply.send(Err(t3_client::session::RpcError::Closed));
                            break Some(payload);
                        }
                        assert!(matches!(method, t3_core::commands::method::VCS_LIST_REFS | t3_core::commands::method::PROJECTS_READ_FILE));
                        if across_await && !changed {
                            state.update(&mut cx, |state, _| {
                                if mode.ends_with("environment") { state.server_config.config.as_mut().unwrap().environment.environment_id = "replacement".into(); }
                                else { state.shell.snapshot.as_mut().unwrap().projects.iter_mut().find(|project| project.id == "checkout-b").unwrap().created_at = "replacement".into(); }
                            });
                            changed = true;
                        }
                        let value = if method == t3_core::commands::method::PROJECTS_READ_FILE { json!({"exists":false,"truncated":false,"contents":""}) } else { json!({"isRepo":true,"refs":[{"name":"main","isDefault":true,"current":true}]}) };
                        let _ = reply.send(Ok(value));
                    }
                }
            }
        });
        if mode.starts_with("valid-") || mode == "ordinary" {
            let payload = escaped.expect("valid workspace must reach the in-memory launch queue");
            let strategy = &payload["workspaceStrategy"];
            assert_eq!(
                strategy["type"],
                if mode == "valid-worktree" {
                    "existing_worktree"
                } else {
                    "root"
                }
            );
            assert_eq!(
                strategy["branch"],
                if mode == "ordinary" {
                    json!(null)
                } else {
                    json!("feature/captured")
                }
            );
            assert!(strategy.get("startFromOrigin").is_none());
            executor.block_test(done).unwrap();
        } else {
            assert!(
                escaped.is_none(),
                "{mode}: rejected restored seed fell through to actual launch payload {escaped:?}"
            );
        }
        let audit = composer.read_with(&cx, |composer, cx| {
            assert!(!composer.menu_launch_outcome().0);
            assert!(
                composer.menu_launch_outcome().1.as_ref().unwrap().contains(
                    if mode.starts_with("valid-") || mode == "ordinary" {
                        "connection closed"
                    } else {
                        "destination changed"
                    }
                ),
                "unexpected outcome: {:?}",
                composer.menu_launch_outcome()
            );
            composer.snapshot_audit(0, cx)
        });
        assert_eq!(audit["prompt"], "Keep restored draft");
        assert!(changed);
        assert!(
            !fixture
                .take_commands()
                .contains(&t3_core::commands::method::LAUNCH_THREAD)
        );
        state.update(&mut cx, |state, _| state.stop_pump());
    }
}

#[gpui::test]
async fn archive_completion_preserves_undo_after_the_shell_removes_its_row(
    cx: &mut gpui::TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, mut fixture) =
        cx.update(|cx| project_filter_tests::install_state(cx, runtime.handle().clone()));
    let (send, done) = tokio::sync::oneshot::channel();
    let mut send = Some(send);
    let _subscription = state.update(cx, |_, cx| {
        cx.subscribe(
            &state,
            move |state, _, event: &crate::app_state::AppEvent, _| {
                if *event == crate::app_state::AppEvent::Notice {
                    if let Some(notice) = state.sidebar.undo.notice() {
                        if let Some(send) = send.take() {
                            let _ = send.send(notice);
                        }
                    }
                }
            },
        )
    });
    state.update(cx, |state, cx| state.archive_thread("menu-thread", cx));
    let (method, payload, reply) = fixture.next_request().await;
    assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
    assert_eq!(payload["type"], "thread.archive");
    state.update(cx, |state, cx| {
        state
            .shell
            .snapshot
            .as_mut()
            .unwrap()
            .threads
            .retain(|thread| thread.id != "menu-thread");
        state.rebuild_sidebar(cx);
    });
    assert!(reply.send(Ok(serde_json::json!({"sequence":1}))).is_ok());
    let notice = done.await.unwrap();
    assert_eq!(notice.action, t3_core::thread_actions::UndoAction::Archived);
    assert_eq!(notice.count, 1);
    state.update(cx, |state, cx| assert!(state.undo_latest(cx)));
    let (_, payload, reply) = fixture.next_request().await;
    assert_eq!(payload["type"], "thread.unarchive");
    assert_eq!(payload["threadId"], "menu-thread");
    assert!(
        reply
            .send(Err(t3_client::session::RpcError::Closed))
            .is_ok()
    );
}
