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
        runtime.block_on(runtime.spawn_blocking(|| t3_client::store::commit_client_settings(serde_json::Map::new())))
            .expect("fixture writer barrier task").expect("fixture drafts committed before Composer reads them");
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

#[gpui::test]
fn scoped_menu_keeps_background_owner_and_rejects_same_id_replacement(cx: &mut gpui::TestAppContext) {
    use std::sync::Arc;
    use t3_core::sidebar::fleet::EnvironmentSummary;
    use t3_core::sidebar::grouping::{EnvironmentOwner, GroupConsumer};
    use t3_core::sidebar::tag_filter::ThreadKey;
    use t3_client::environments::{EnvironmentView, FleetView};
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let (state, mut client) = cx.update(|cx| project_filter_tests::install_state(cx, runtime.handle().clone()));
    state.update(cx, |state, cx| {
        let mut environments = Vec::new();
        for (saved, canonical) in [("saved-fixture", "fixture-env"), ("background", "other-env")] {
            let owner = EnvironmentOwner { saved_id:saved.into(), canonical_id:Some(canonical.into()), generation:1, connection_generation:1, shell_generation:state.shell.generation };
            let mut config = state.server_config.clone();
            config.config.as_mut().unwrap().scratch_workspace_root = Some("/fixture/b".into());
            environments.push(Arc::new(EnvironmentView { summary:Arc::new(EnvironmentSummary::from_shell(owner, &state.shell, &config)), status:t3_client::supervisor::ConnectionStatus::Connected, config, grants:Some(project_filter_tests::session(true)), settings_revision:0, keybindings_revision:0 }));
        }
        state.fleet = Arc::new(FleetView { revision:1, environments });
        state.sidebar.grouping_projects = state.fleet.environments.iter().flat_map(|entry| entry.summary.projects.iter().cloned()).collect();
        state.sidebar.grouping_threads = state.fleet.environments.iter().flat_map(|entry| entry.summary.threads.iter().cloned()).collect();
        state.rebuild_sidebar(cx);
        assert_eq!(state.project_groups_for(GroupConsumer::Sidebar,cx).iter().flat_map(|group| &group.registrations).count(), 6);
        let key = ThreadKey::new("background", "menu-thread");
        let owner = ThreadMenuOwner::capture_scoped(state,&key).unwrap();
        assert!(owner.current(state));
        assert_eq!(state.environments.active.as_deref(),Some("saved-fixture"));
        let mut replaced = (*state.fleet).clone();
        let entry = Arc::make_mut(&mut replaced.environments[1]);
        Arc::make_mut(&mut entry.summary).threads.iter_mut().find(|row| row.thread.id=="menu-thread").unwrap().created_at="replacement".into();
        state.fleet=Arc::new(replaced);
        assert!(!owner.current(state));
        assert_eq!(state.selected_thread,None);
        assert_eq!(state.environments.active.as_deref(),Some("saved-fixture"));
    });
    let (menu, cx) = cx.add_window_view(|_, cx| ContextMenu::new(state.clone(), cx));
    let route = state.read_with(cx, |state, _| (state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()));
    state.update(cx, |state, cx| {
        let groups = state.project_groups_for(GroupConsumer::Sidebar, cx);
        let scratch = groups.iter().find(|group| group.is_scratch).unwrap();
        state.project_preferences.scope_keys = vec![scratch.members[0].physical_key.clone()];
        assert_eq!(state.project_preferences.resolved_scope(&groups, true), vec![scratch.key.clone()]);
        state.rebuild_sidebar(cx);
    });
    cx.update(|window, cx| menu.update(cx, |menu, cx| {
        menu.open_for_scoped_thread(&ThreadKey::new("background", "menu-thread"), point(px(10.), px(10.)), window, cx);
        assert!(menu.is_open());
        menu.activate(MenuAction::Thread(ThreadAction::FilterByProject), cx);
        assert!(!menu.is_open());
    }));
    state.read_with(cx, |state, cx| {
        let groups = state.project_groups_for(GroupConsumer::Sidebar, cx);
        assert!(state.project_preferences.resolved_scope(&groups, true).is_empty(), "the checked scratch alias must clear through the actual menu action");
        assert_eq!((state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()), route);
    });
    state.update(cx, |state, cx| {
        let delegated = ThreadKey::new("background", "menu-thread");
        state.notifications.visibility.delegated_under_parent.insert(delegated.clone());
        state.rebuild_sidebar(cx);
        assert!(state.sidebar.fleet.thread(&delegated).is_none(), "delegated rows are hidden under their parent across the fleet");
        assert!(state.sidebar.fleet.thread(&ThreadKey::new("saved-fixture", "menu-thread")).is_some(), "same raw id in another owner must stay visible");
    });
    assert!(client.take_commands().is_empty());
}

#[gpui::test]
fn scoped_keyboard_order_and_undo_keep_same_raw_ids_in_separate_owners(cx: &mut gpui::TestAppContext) {
    // The scope setter persists preferences on the real UI-state writer thread.
    cx.executor().allow_parking();
    use std::sync::Arc;
    use t3_core::sidebar::fleet::EnvironmentSummary;
    use t3_core::sidebar::grouping::EnvironmentOwner;
    use t3_core::sidebar::tag_filter::ThreadKey;
    use t3_client::environments::{EnvironmentView,FleetView};
    let runtime=tokio::runtime::Builder::new_current_thread().build().unwrap();
    let (state,mut fixture)=cx.update(|cx| project_filter_tests::install_state(cx,runtime.handle().clone()));
    state.update(cx,|state,cx| {
        let environments=["first","second"].into_iter().map(|saved| {
            let owner=EnvironmentOwner { saved_id:saved.into(),canonical_id:Some(saved.into()),generation:1,connection_generation:1,shell_generation:state.shell.generation };
            Arc::new(EnvironmentView { summary:Arc::new(EnvironmentSummary::from_shell(owner,&state.shell,&state.server_config)),status:t3_client::supervisor::ConnectionStatus::Connected,config:state.server_config.clone(),grants:Some(project_filter_tests::session(true)),settings_revision:0,keybindings_revision:0 })
        }).collect();
        state.fleet=Arc::new(FleetView { revision:1,environments });
        state.sidebar.grouping_projects=state.fleet.environments.iter().flat_map(|entry| entry.summary.projects.iter().cloned()).collect();
        state.sidebar.grouping_threads=state.fleet.environments.iter().flat_map(|entry| entry.summary.threads.iter().cloned()).collect();
        state.rebuild_sidebar(cx);
        let model=&state.sidebar.fleet;
        let keys=model.ordered.iter().map(|index| model.threads[*index].key.clone()).collect::<Vec<_>>();
        assert_eq!(keys.len(),6);
        assert_ne!(keys[0],keys[1]);
        assert_eq!(model.adjacent_thread(None,true),keys.first().cloned());
        assert_eq!(model.jump_target(1),Some(keys[1].clone()));
        for saved in ["first","second"] {
            let owner=ThreadMenuOwner::capture_scoped(state,&ThreadKey::new(saved,"menu-thread")).unwrap();
            let (claim,action,plan)=state.scoped_undo_claim(&owner,"thread.settle").unwrap();
            state.sidebar.undo.show(action,claim,plan,crate::sidebar::controller::now_ms());
        }
        assert_eq!(state.sidebar.undo.notice().unwrap().count,2);
        assert!(state.selected_thread.is_none());
    });
    for folders in [false, true] {
        cx.update(|cx| crate::settings::store::install_committed_client(&state, serde_json::from_value(serde_json::json!({"boringSidebarEnabled":folders,"sidebarWorkingShelfEnabled":false})).unwrap(), cx));
        state.update(cx, |state, cx| {
            state.rebuild_sidebar(cx);
            let active=state.sidebar.fleet.partition.active.iter().map(|index| state.sidebar.fleet.threads[*index].key.clone()).collect::<Vec<_>>();
            assert!(active.len()>1);
            let route = (state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone());
            let ordered = state.sidebar.fleet.ordered.iter().map(|index| state.sidebar.fleet.threads[*index].key.clone()).collect::<Vec<_>>();
            assert!(ordered.contains(&ThreadKey::new("first", "menu-thread")));
            assert!(ordered.contains(&ThreadKey::new("second", "menu-thread")), "visible order retains same raw ids in different owners");
            let anchor = ordered.first().unwrap().clone();
            let target = ordered.last().unwrap().clone();
            let modifier = gpui::Modifiers { platform:cfg!(target_os="macos"), control:!cfg!(target_os="macos"), ..Default::default() };
            let shift = gpui::Modifiers { shift:true, ..Default::default() };
            state.click_scoped_sidebar_thread(&anchor, modifier, cx);
            state.click_scoped_sidebar_thread(&target, shift, cx);
            assert_eq!(state.sidebar.fleet_selection, ordered.iter().cloned().collect(), "modifier-add then Shift selects the actual inclusive scoped visible order");
            assert_eq!(state.sidebar.fleet_anchor.as_ref(), Some(&anchor));
            assert!(state.clear_thread_selection(cx));
            assert!(state.sidebar.fleet_selection.is_empty());
            assert!(state.sidebar.fleet_anchor.is_none(), "clear also removes the scoped range anchor");
            state.click_scoped_sidebar_thread(&target, shift, cx);
            assert_eq!(state.sidebar.fleet_selection, [target.clone()].into_iter().collect(), "Shift without an anchor selects its target alone");
            assert_eq!(state.sidebar.fleet_anchor.as_ref(), Some(&target));
            assert!(state.clear_thread_selection(cx));
            state.click_scoped_sidebar_thread(&anchor, modifier, cx);
            state.notifications.visibility.hidden.insert(anchor.clone());
            state.rebuild_sidebar(cx);
            assert!(!state.sidebar.fleet.ordered.iter().any(|index| state.sidebar.fleet.threads[*index].key == anchor), "the captured anchor has left the visible order");
            let mut expected = state.sidebar.fleet_selection.clone();
            expected.insert(target.clone());
            state.click_scoped_sidebar_thread(&target, shift, cx);
            assert_eq!(state.sidebar.fleet_selection, expected, "Shift with an unavailable anchor still adds the visible target");
            assert_eq!(state.sidebar.fleet_anchor.as_ref(), Some(&target));
            state.notifications.visibility.hidden.remove(&anchor);
            state.rebuild_sidebar(cx);
            assert!(state.clear_thread_selection(cx));
            assert!(state.sidebar.fleet_anchor.is_none());
            assert_eq!((state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()), route, "selection gestures do not navigate");
            let dragged=active.last().unwrap().clone();
            let target=active[0].clone();
            state.start_scoped_thread_drag(&dragged, cx);
            let slot=state.sidebar.fleet.rows.iter().position(|row| matches!(row, t3_core::sidebar::fleet::ScopedSidebarRow::Thread { index, section:t3_core::sidebar::Section::Active } if state.sidebar.fleet.threads[*index].key==target)).unwrap();
            let plan=state.scoped_thread_drop_target(slot, cx);
            if folders { assert!(plan.is_none(), "folder Active rows are time-ordered, not manually reorderable"); }
            else { assert!(matches!(plan, Some((_, t3_core::sidebar::drag::DropPlan::MoveActive { assignments, .. })) if !assignments.is_empty()), "normal Active mode permits a real reorder"); }
            state.end_thread_drag(cx);
        });
    }
    state.update(cx, |state, cx| {
        let key = state.sidebar.fleet.ordered.first().map(|index| state.sidebar.fleet.threads[*index].key.clone()).unwrap();
        let scope = state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx).into_iter().find(|group| !group.archived).unwrap().key;
        let modifier = gpui::Modifiers { platform:cfg!(target_os="macos"), control:!cfg!(target_os="macos"), ..Default::default() };
        state.click_scoped_sidebar_thread(&key, modifier, cx);
        assert_eq!(state.sidebar.fleet_selection, [key.clone()].into_iter().collect());
        assert_eq!(state.sidebar.fleet_anchor.as_ref(), Some(&key));
        let route = (state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone());
        state.set_scoped_project_scope(vec![scope.clone()], cx);
        assert_eq!(state.project_preferences.scope_keys, vec![scope]);
        assert!(state.sidebar.fleet_selection.is_empty());
        assert!(state.sidebar.fleet_anchor.is_none(), "explicit project scope changes clear the scoped selection anchor");
        assert_eq!((state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()), route, "explicit project scope changes do not navigate");
    });
    assert!(fixture.take_commands().is_empty());
    runtime.block_on(runtime.spawn_blocking(|| t3_client::store::commit_client_settings(serde_json::Map::new())))
        .expect("fixture writer barrier task").expect("prior fixture writes committed before teardown");
    cx.run_until_parked();
}

    #[gpui::test]
    fn undo_contexts_preserve_interleaved_order_and_share_header_invalidation(cx:&mut gpui::TestAppContext) {
    use std::sync::Arc;
    use t3_core::sidebar::fleet::EnvironmentSummary;
    use t3_core::sidebar::grouping::EnvironmentOwner;
    use t3_core::sidebar::tag_filter::ThreadKey;
    use t3_client::environments::{EnvironmentView,FleetView};
    use t3_client::supervisor::ConnectionStatus;
        let runtime=tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (state,mut fixture)=cx.update(|cx| project_filter_tests::install_state(cx,runtime.handle().clone()));
        state.update(cx,|state,cx| {
            let entries=["saved-fixture","other"].into_iter().map(|saved| {
                let owner=EnvironmentOwner { saved_id:saved.into(),canonical_id:Some(saved.into()),generation:1,connection_generation:1,shell_generation:state.shell.generation };
                Arc::new(EnvironmentView { summary:Arc::new(EnvironmentSummary::from_shell(owner,&state.shell,&state.server_config)),status:ConnectionStatus::Connected,config:state.server_config.clone(),grants:Some(crate::context_menu::project_filter_tests::session(true)),settings_revision:0,keybindings_revision:0 })
            }).collect();
            state.fleet=Arc::new(FleetView { revision:1,environments:entries });
            state.rebuild_sidebar(cx);
            let first=crate::context_menu::ThreadMenuOwner::capture_scoped(state,&ThreadKey::new("saved-fixture","menu-thread")).unwrap();
            let other=crate::context_menu::ThreadMenuOwner::capture_scoped(state,&ThreadKey::new("other","menu-thread")).unwrap();
            for (owner,command) in [(first.clone(),"thread.settle"),(other.clone(),"thread.unpin"),(first.clone(),"thread.snooze")] {
                let (claim,action,plan)=state.scoped_undo_claim(&owner,command).unwrap(); state.sidebar.undo.show(action,claim,plan,0);
            }
            for expected in [t3_core::thread_actions::UndoAction::Snoozed,t3_core::thread_actions::UndoAction::Unpinned,t3_core::thread_actions::UndoAction::Settled] {
                assert_eq!(state.sidebar.undo.notice().unwrap().action,expected);
                assert_eq!(state.sidebar.undo.take_latest_group().len(),1);
            }
            let (first_claim,_,_)=state.scoped_undo_claim(&first,"thread.snooze").unwrap();
            let (other_claim,action,plan)=state.scoped_undo_claim(&other,"thread.snooze").unwrap();
            state.rebuild_sidebar(cx);
            state.sidebar.undo.invalidate(t3_core::thread_actions::UndoKind::Snooze,"menu-thread");
            assert!(!state.sidebar.undo.is_current(&first_claim));
            assert!(state.sidebar.undo.is_current(&other_claim));
            state.sidebar.undo.show(action,other_claim,plan,0);
            assert!(state.sidebar.undo.expire(t3_core::thread_actions::UNDO_NOTICE_MS));
            assert!(state.sidebar.undo.notice().is_none());
        });
        assert!(fixture.take_commands().is_empty());
    }

#[gpui::test]
async fn scoped_settle_and_undo_use_captured_actor_without_navigation(cx:&mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let runtime=tokio::runtime::Runtime::new().unwrap();
    let (state,mut active_fixture)=cx.update(|cx| project_filter_tests::install_state(cx,runtime.handle().clone()));
    let before=state.read_with(cx,|state,_| (state.environments.active.clone(),state.selected_thread.clone(),state.draft_project.clone()));
    let (client,mut background)=t3_client::supervisor::ClientFixture::handle(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read","orchestration:operate"]})).unwrap());
    let (shell,mut config)=state.read_with(cx,|state,_| (state.shell.clone(),state.server_config.clone()));
    config.config.as_mut().unwrap().environment.environment_id="background-env".into();
    config.config.as_mut().unwrap().environment.capabilities.project_organization=Some(true);
    let (ready,loaded)=tokio::sync::oneshot::channel(); let mut ready=Some(ready);
    let _loaded=state.update(cx,|_,cx| cx.subscribe(&state,move |state,_,event,_| {
        if *event==crate::app_state::AppEvent::Shell && state.scoped_environment("background").is_some() { if let Some(ready)=ready.take() { let _=ready.send(()); } }
    }));
    let install=state.update(cx,|state,_| state.connections.install_fixture("background",client,shell,config,t3_client::supervisor::ConnectionStatus::Connected));
    let events=runtime.spawn(install).await.unwrap(); loaded.await.unwrap();
    let (shown,notice)=tokio::sync::oneshot::channel(); let mut shown=Some(shown);
    let _notice=state.update(cx,|_,cx| cx.subscribe(&state,move |state,_,event,_| {
        if *event==crate::app_state::AppEvent::Notice && state.sidebar.undo.notice().is_some() { if let Some(shown)=shown.take() { let _=shown.send(()); } }
    }));
    let key=t3_core::sidebar::tag_filter::ThreadKey::new("background","menu-thread");
    state.update(cx,|state,cx| state.run_scoped_thread_action(&key,ThreadAction::Settle,&[],cx));
    let (method,payload,reply)=background.next_request().await;
    assert_eq!(method,t3_core::commands::method::DISPATCH_COMMAND); assert_eq!(payload["type"],"thread.settle"); assert_eq!(payload["threadId"],"menu-thread");
    reply.send(Ok(serde_json::json!({"sequence":2}))).unwrap(); notice.await.unwrap();
    assert!(state.update(cx,|state,cx| state.undo_latest(cx)));
    let (method,payload,reply)=background.next_request().await;
    assert_eq!(method,t3_core::commands::method::DISPATCH_COMMAND); assert_eq!(payload["type"],"thread.unsettle"); assert_eq!(payload["threadId"],"menu-thread");
    reply.send(Ok(serde_json::json!({"sequence":3}))).unwrap();
    for mode in ["selected-anchor", "unselected-anchor", "untouched-anchor", "failure"] {
        let other = t3_core::sidebar::tag_filter::ThreadKey::new("background", "sibling");
        let modifier = gpui::Modifiers { platform:cfg!(target_os="macos"), control:!cfg!(target_os="macos"), ..Default::default() };
        let (selection, anchor, owner) = state.update(cx, |state, cx| {
            state.clear_thread_selection(cx);
            state.click_scoped_sidebar_thread(&other, modifier, cx);
            state.click_scoped_sidebar_thread(&key, modifier, cx);
            if mode == "unselected-anchor" { state.click_scoped_sidebar_thread(&key, modifier, cx); }
            if mode == "untouched-anchor" {
                state.click_scoped_sidebar_thread(&other, modifier, cx);
                state.click_scoped_sidebar_thread(&other, modifier, cx);
            }
            let owner = ThreadMenuOwner::capture_scoped(state, &key).unwrap();
            assert!(owner.current(state) && owner.can_operate_owner(state));
            assert_eq!(state.sidebar.fleet_selection.contains(&key), mode != "unselected-anchor");
            assert_eq!(state.sidebar.fleet_anchor.as_ref(), Some(if mode == "untouched-anchor" { &other } else { &key }));
            (state.sidebar.fleet_selection.clone(), state.sidebar.fleet_anchor.clone(), owner)
        });
        let (sent, finished) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
        let failed = mode == "failure";
        let _finished = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if (!failed && *event == crate::app_state::AppEvent::SidebarSelection)
                || (failed && *event == crate::app_state::AppEvent::Notice && state.toasts.latest().is_some_and(|toast| toast.title == "Thread action stopped")) {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        state.update(cx, |state, cx| state.run_scoped_drop_steps(vec![(owner, t3_core::thread_actions::settle(&crate::sidebar::controller::uuid(), &key.thread_id))], cx));
        let (method, payload, reply) = background.next_request().await;
        assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(payload["type"], "thread.settle");
        assert_eq!(payload["threadId"], key.thread_id);
        if failed {
            reply.send(Err(t3_client::session::RpcError::Failure(t3_core::rpc::RpcFailure { tag:Some("OperationError".into()), message:"cannot settle".into(), error:serde_json::json!({"_tag":"OperationError"}) }))).unwrap();
        } else { reply.send(Ok(serde_json::json!({"sequence":3}))).unwrap(); }
        finished.await.unwrap();
        state.read_with(cx, |state, _| {
            let mut expected = selection.clone();
            if !failed { expected.remove(&key); }
            assert_eq!(state.sidebar.fleet_selection, expected, "{mode}");
            let expected_anchor = if !failed && selection.contains(&key) && anchor.as_ref() == Some(&key) { None } else { anchor.as_ref() };
            assert_eq!(state.sidebar.fleet_anchor.as_ref(), expected_anchor, "valid placement removes only an actually selected anchor: {mode}");
        });
    }
    state.update(cx, |state, cx| { state.clear_thread_selection(cx); });
    state.read_with(cx,|state,_| assert_eq!((state.environments.active.clone(),state.selected_thread.clone(),state.draft_project.clone()),before));
    assert!(active_fixture.take_commands().is_empty());
    let captured = state.read_with(cx, |state, cx| state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx).into_iter().find(|group| group.member_refs.contains(&t3_core::sidebar::grouping::ProjectRef { environment_id:"background".into(), project_id:"checkout-b".into() })).unwrap());
    let original_owner = captured.registrations.iter().find(|project| project.owner.saved_id=="background" && project.project.id=="checkout-b").unwrap().owner.clone();
    let (menu, cx) = cx.add_window_view(|_, cx| ContextMenu::new(state.clone(), cx));
    cx.update(|window, cx| menu.update(cx, |menu, cx| {
        menu.open_scoped_project(captured, point(px(10.), px(10.)), window, cx);
        assert!(menu.open.as_ref().unwrap().items.iter().any(|item| item.action == MenuAction::Project(t3_core::thread_actions::ProjectAction::Pin) && !item.disabled));
    }));
    let group = menu.read_with(cx, |menu, _| menu.open.as_ref().unwrap().scoped_group.clone().unwrap());
    cx.update(|window, cx| menu.update(cx, |menu, cx| {
        menu.open_scoped_project(group.clone(), point(px(10.), px(10.)), window, cx);
        menu.activate(MenuAction::Project(t3_core::thread_actions::ProjectAction::Pin), cx);
    }));
    let mut changed = std::collections::HashSet::new();
    for _ in 0..group.mutation_refs().len() {
        let (method, payload, reply) = background.next_request().await;
        assert_eq!(method, t3_core::projects::PROJECTS_MUTATE);
        assert_eq!(payload["type"], "project.update");
        assert_eq!(payload["pinned"], true);
        changed.insert(payload["projectId"].as_str().unwrap().to_owned());
        reply.send(Ok(serde_json::json!({"sequence":2}))).unwrap();
    }
    assert_eq!(changed, std::collections::HashSet::from(["checkout-a".to_owned(), "checkout-b".to_owned()]));
    cx.update(|window, cx| menu.update(cx, |menu, cx| menu.open_scoped_project(group.clone(), point(px(10.), px(10.)), window, cx)));
    let owner_before = state.read_with(cx, |state, _| state.scoped_environment("background").unwrap().summary.owner.clone());
    for supported in [false, true] {
        let mut changed_config = state.read_with(cx, |state, _| state.scoped_environment("background").unwrap().config.config.clone().unwrap());
        changed_config.environment.capabilities.project_organization = Some(supported);
        let (ready, applied) = tokio::sync::oneshot::channel();
        let mut ready = Some(ready);
        let _applied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("background").and_then(|entry| entry.config.config.as_ref()).is_some_and(|config| config.environment.capabilities.project_organization == Some(supported)) {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        let changed_events = events.clone();
        runtime.spawn(async move { changed_events.send(t3_client::supervisor::ClientEvent::ServerConfig(Box::new(t3_core::server_config::ServerConfigStreamEvent::Snapshot { config: Box::new(changed_config) }))).await.unwrap(); }).await.unwrap();
        applied.await.unwrap();
        state.read_with(cx, |state, _| {
            assert_eq!(state.scoped_environment("background").unwrap().summary.owner, owner_before);
            assert!(state.scoped_ready(&owner_before, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
            for captured in &group.registrations {
                let current = state.scoped_project(&captured.reference()).unwrap();
                assert_eq!(current.project.created_at, captured.project.created_at);
                assert_eq!(current.project.workspace_root, captured.project.workspace_root);
            }
        });
        if !supported {
            cx.update(|_, cx| menu.update(cx, |menu, cx| menu.activate(MenuAction::Project(t3_core::thread_actions::ProjectAction::Pin), cx)));
            let barrier = state.read_with(cx, |state, _| state.connections.destination(&owner_before, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
            runtime.spawn(barrier).await.unwrap().unwrap();
            cx.run_until_parked();
            assert!(!background.take_commands().contains(&t3_core::projects::PROJECTS_MUTATE), "a captured project menu must refuse pin after projectOrganization is removed");
            state.read_with(cx, |state, _| {
                let toast = state.toasts.latest().expect("a project capability refusal must be visible");
                assert_eq!(toast.kind, crate::toast::ToastKind::Error);
                assert!(toast.description.as_ref().is_some_and(|description| !description.is_empty()));
                assert_eq!((state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()), before);
            });
        }
    }
    cx.update(|window, cx| menu.update(cx, |menu, cx| menu.open_scoped_project(group.clone(), point(px(10.), px(10.)), window, cx)));
    let mut replacement = state.read_with(cx, |state, _| state.scoped_project(&t3_core::sidebar::grouping::ProjectRef { environment_id:"background".into(), project_id:"checkout-b".into() }).unwrap().project.clone());
    replacement.created_at="replacement".into();
    let (send, replaced)=tokio::sync::oneshot::channel(); let mut send=Some(send);
    let _replaced=state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event==crate::app_state::AppEvent::Shell && state.scoped_environment("background").is_some_and(|entry| entry.summary.projects.iter().any(|project| project.project.id=="checkout-b" && project.project.created_at=="replacement")) {
            if let Some(send)=send.take() { let _=send.send(()); }
        }
    }));
    let replacement_events = events.clone();
    runtime.spawn(async move { replacement_events.send(t3_client::supervisor::ClientEvent::Shell(t3_core::shell::ShellStreamItem::ProjectUpdated { sequence:2, project:Box::new(replacement) })).await.unwrap(); }).await.unwrap();
    replaced.await.unwrap();
    state.read_with(cx, |state, _| assert_eq!(state.scoped_environment("background").unwrap().summary.owner, original_owner));
    background.take_commands();
    cx.update(|_, cx| menu.update(cx, |menu, cx| menu.activate(MenuAction::Project(t3_core::thread_actions::ProjectAction::Pin), cx)));
    let barrier=state.read_with(cx, |state, _| state.connections.destination(&original_owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
    runtime.spawn(barrier).await.unwrap().unwrap();
    cx.run_until_parked();
    assert!(!background.take_commands().contains(&t3_core::projects::PROJECTS_MUTATE), "a captured folder menu must not pin a same-id replacement project");
    state.read_with(cx, |state, _| assert_eq!((state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()), before));
    state.update(cx, |state, cx| {
        let steps=["menu-thread", "sibling"].into_iter().map(|id| {
            let owner=ThreadMenuOwner::capture_scoped(state, &t3_core::sidebar::tag_filter::ThreadKey::new("background", id)).unwrap();
            (owner, t3_core::thread_actions::mark_unread(&crate::sidebar::controller::uuid(), id))
        }).collect();
        state.run_scoped_drop_steps(steps, cx);
    });
    let (method, payload, reply)=background.next_request().await;
    assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
    assert_eq!(payload["threadId"], "menu-thread");
    cx.update(|_, _| {
        reply.send(Ok(serde_json::json!({"sequence":3}))).unwrap();
        background.grants.send_replace(Some(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":[]})).unwrap()));
    });
    let barrier=state.read_with(cx, |state, _| state.connections.destination(&original_owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
    let _=runtime.spawn(barrier).await.unwrap();
    cx.run_until_parked();
    assert!(!background.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND), "grant drift after one saved action must stop before the second send");
    state.read_with(cx, |state, _| {
        let toast=state.toasts.latest().expect("partial completion must report why the action stopped");
        assert_eq!(toast.kind, crate::toast::ToastKind::Error);
        assert!(toast.description.as_ref().is_some_and(|description| description.starts_with("1 change(s) saved")));
    });
    background.grants.send_replace(Some(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read","orchestration:operate"]})).unwrap()));
    let barrier = state.read_with(cx, |state, _| state.connections.destination(&original_owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
    runtime.spawn(barrier).await.unwrap().unwrap();
    cx.run_until_parked();
    let stable = state.read_with(cx, |state, cx| state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx).into_iter().find(|group| group.member_refs.contains(&t3_core::sidebar::grouping::ProjectRef { environment_id:"background".into(), project_id:"checkout-a".into() })).unwrap());
    let representative = stable.representative.clone();
    state.read_with(cx, |state, cx| assert_eq!(state.scoped_project_action_target(&stable, cx).unwrap().project.reference(), representative));
    let mut title_only = state.read_with(cx, |state, _| state.scoped_project(&representative).unwrap().project.clone());
    title_only.title = "Harmless title change".into();
    let (ready, renamed) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
    let renamed_ref = representative.clone();
    let _renamed = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == crate::app_state::AppEvent::Shell && state.scoped_project(&renamed_ref).is_some_and(|project| project.project.title == "Harmless title change") {
            if let Some(ready) = ready.take() { let _ = ready.send(()); }
        }
    }));
    let title_events = events.clone();
    runtime.spawn(async move { title_events.send(t3_client::supervisor::ClientEvent::Shell(t3_core::shell::ShellStreamItem::ProjectUpdated { sequence:3, project:Box::new(title_only) })).await.unwrap(); }).await.unwrap();
    renamed.await.unwrap();
    state.read_with(cx, |state, cx| assert_eq!(state.scoped_project_action_target(&stable, cx).unwrap().project.reference(), representative));
    cx.update(|window, cx| menu.update(cx, |menu, cx| {
        menu.open_scoped_project(stable.clone(), point(px(10.), px(10.)), window, cx);
        menu.activate(MenuAction::Project(t3_core::thread_actions::ProjectAction::Pin), cx);
    }));
    let mut changed = std::collections::HashSet::new();
    for _ in 0..stable.mutation_refs().len() {
        let (method, payload, reply) = background.next_request().await;
        assert_eq!(method, t3_core::projects::PROJECTS_MUTATE);
        assert_eq!(payload["pinned"], true);
        changed.insert(payload["projectId"].as_str().unwrap().to_owned());
        reply.send(Ok(serde_json::json!({"sequence":4}))).unwrap();
    }
    assert_eq!(changed, std::collections::HashSet::from(["checkout-a".to_owned(), "checkout-b".to_owned()]), "harmless title changes must not invalidate a captured group");
    let mut next_sequence = 4;
    for reference in &stable.member_refs {
        let mut pinned = state.read_with(cx, |state, _| state.scoped_project(reference).unwrap().project.clone());
        pinned.pinned_at = Some("2026-11-06T00:00:00Z".into());
        let reference = reference.clone();
        let (ready, applied) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _applied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Shell && state.scoped_project(&reference).is_some_and(|project| project.project.pinned_at.is_some()) {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        let pin_events = events.clone();
        let sequence = next_sequence;
        next_sequence += 1;
        runtime.spawn(async move { pin_events.send(t3_client::supervisor::ClientEvent::Shell(t3_core::shell::ShellStreamItem::ProjectUpdated { sequence, project:Box::new(pinned) })).await.unwrap(); }).await.unwrap();
        applied.await.unwrap();
    }
    let pinned_group = state.read_with(cx, |state, cx| state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx).into_iter().find(|group| group.key == stable.key).unwrap());
    cx.update(|window, cx| menu.update(cx, |menu, cx| {
        menu.open_scoped_project(pinned_group, point(px(10.), px(10.)), window, cx);
        assert!(menu.open.as_ref().unwrap().items.iter().any(|item| item.action == MenuAction::Project(t3_core::thread_actions::ProjectAction::Unpin) && item.label == "Unpin project" && !item.disabled));
        menu.activate(MenuAction::Project(t3_core::thread_actions::ProjectAction::Unpin), cx);
    }));
    let mut unpinned = std::collections::HashSet::new();
    for _ in 0..stable.mutation_refs().len() {
        let (method, payload, reply) = background.next_request().await;
        assert_eq!(method, t3_core::projects::PROJECTS_MUTATE);
        assert_eq!(payload["type"], "project.update");
        assert_eq!(payload["pinned"], false);
        unpinned.insert(payload["projectId"].as_str().unwrap().to_owned());
        reply.send(Ok(serde_json::json!({"sequence":4}))).unwrap();
    }
    assert_eq!(unpinned, changed, "Unpin reaches each original physical checkout exactly once");
    for completed in [0, 1] {
        let (ready, stopped) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _stopped = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Notice && state.toasts.latest().is_some_and(|toast| toast.title == "Project pin stopped" && toast.description.as_ref().is_some_and(|description| description.starts_with(&format!("{completed} checkout(s) changed;")))) {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        state.update(cx, |state, cx| state.pin_scoped_project_group(&stable, true, cx));
        for _ in 0..completed {
            let (method, payload, reply) = background.next_request().await;
            assert_eq!(method, t3_core::projects::PROJECTS_MUTATE);
            assert_eq!(payload["pinned"], true);
            reply.send(Ok(serde_json::json!({"sequence":4}))).unwrap();
        }
        let (method, payload, reply) = background.next_request().await;
        assert_eq!(method, t3_core::projects::PROJECTS_MUTATE);
        assert_eq!(payload["pinned"], true);
        background.take_commands();
        reply.send(Err(t3_client::session::RpcError::Closed)).unwrap();
        stopped.await.unwrap();
        let barrier = state.read_with(cx, |state, _| state.connections.destination(&original_owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
        runtime.spawn(barrier).await.unwrap().unwrap();
        cx.run_until_parked();
        assert!(!background.take_commands().contains(&t3_core::projects::PROJECTS_MUTATE), "a failed project write must not continue to later targets");
        state.read_with(cx, |state, _| assert_eq!(state.toasts.latest().unwrap().kind, crate::toast::ToastKind::Error));
    }
    state.update(cx, |state, cx| state.pin_scoped_project_group(&stable, true, cx));
    let (method, payload, first_reply) = background.next_request().await;
    assert_eq!(method, t3_core::projects::PROJECTS_MUTATE);
    assert_eq!(payload["pinned"], true);
    background.take_commands();
    let mut first_reply = Some(first_reply);
    for supported in [false, true] {
        let mut changed_config = state.read_with(cx, |state, _| state.scoped_environment("background").unwrap().config.config.clone().unwrap());
        changed_config.environment.capabilities.project_organization = Some(supported);
        let (ready, applied) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _applied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("background").and_then(|entry| entry.config.config.as_ref()).is_some_and(|config| config.environment.capabilities.project_organization == Some(supported)) {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        let config_events = events.clone();
        runtime.spawn(async move { config_events.send(t3_client::supervisor::ClientEvent::ServerConfig(Box::new(t3_core::server_config::ServerConfigStreamEvent::Snapshot { config:Box::new(changed_config) }))).await.unwrap(); }).await.unwrap();
        applied.await.unwrap();
        if !supported {
            let (ready, stopped) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
            let _stopped = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                if *event == crate::app_state::AppEvent::Notice && state.toasts.latest().is_some_and(|toast| toast.description.as_ref().is_some_and(|description| description.starts_with("1 checkout(s) changed before"))) {
                    if let Some(ready) = ready.take() { let _ = ready.send(()); }
                }
            }));
            first_reply.take().unwrap().send(Ok(serde_json::json!({"sequence":4}))).unwrap();
            stopped.await.unwrap();
            let barrier = state.read_with(cx, |state, _| state.connections.destination(&original_owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
            runtime.spawn(barrier).await.unwrap().unwrap();
            cx.run_until_parked();
            assert!(!background.take_commands().contains(&t3_core::projects::PROJECTS_MUTATE), "capability loss while first write waits must stop before second write");
            state.read_with(cx, |state, _| assert_eq!(state.toasts.latest().unwrap().kind, crate::toast::ToastKind::Error));
        }
    }
    for action in [t3_core::thread_actions::ProjectAction::NewThread, t3_core::thread_actions::ProjectAction::Settings] {
        cx.update(|window, cx| menu.update(cx, |menu, cx| menu.open_scoped_project(stable.clone(), point(px(10.), px(10.)), window, cx)));
        let denied_owner = original_owner.clone();
        let (ready, denied) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _denied = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("background").is_some_and(|entry| entry.summary.owner == denied_owner && entry.grants.as_ref().is_some_and(|grants| !grants.grants(t3_core::auth::ORCHESTRATION_READ_SCOPE) && grants.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))) {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        background.grants.send_replace(Some(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:operate"]})).unwrap()));
        let barrier = state.read_with(cx, |state, _| state.connections.destination(&original_owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
        assert!(matches!(runtime.spawn(barrier).await.unwrap(), Err(t3_client::environments::DestinationError::Denied)));
        cx.run_until_parked();
        denied.await.unwrap();
        state.read_with(cx, |state, cx| {
            assert!(!state.scoped_ready(&original_owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
            assert!(state.scoped_ready(&original_owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
            assert!(state.scoped_project_action_target(&stable, cx).is_ok(), "read refusal must be independent of captured group identity");
            for captured in &stable.registrations {
                let current = state.scoped_project(&captured.reference()).unwrap();
                assert_eq!(current.owner, captured.owner);
                assert_eq!(current.project.created_at, captured.project.created_at);
                assert_eq!(current.project.workspace_root, captured.project.workspace_root);
            }
        });
        background.take_commands();
        cx.update(|_, cx| menu.update(cx, |menu, cx| menu.activate(MenuAction::Project(action), cx)));
        let barrier = state.read_with(cx, |state, _| state.connections.destination(&original_owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
        runtime.spawn(barrier).await.unwrap().unwrap();
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_eq!((state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()), before);
            let toast = state.toasts.latest().expect("read grant refusal must be visible");
            assert_eq!(toast.kind, crate::toast::ToastKind::Error);
            assert_eq!(toast.title, "Project action stopped", "captured project navigation must visibly refuse current read-grant loss");
            assert!(toast.description.as_ref().is_some_and(|description| !description.is_empty()));
        });
        assert!(background.take_commands().is_empty(), "read-refused project navigation must not send RPCs");
        let restored_owner = original_owner.clone();
        let (ready, restored) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
        let _restored = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("background").is_some_and(|entry| entry.summary.owner == restored_owner && entry.grants.as_ref().is_some_and(|grants| grants.grants(t3_core::auth::ORCHESTRATION_READ_SCOPE) && grants.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))) {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        background.grants.send_replace(Some(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read","orchestration:operate"]})).unwrap()));
        let barrier = state.read_with(cx, |state, _| state.connections.destination(&original_owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
        runtime.spawn(barrier).await.unwrap().unwrap();
        cx.run_until_parked();
        restored.await.unwrap();
    }
    cx.update(|window, cx| menu.update(cx, |menu, cx| menu.open_scoped_project(stable.clone(), point(px(10.), px(10.)), window, cx)));
    let split_ref = stable.member_refs.iter().find(|reference| **reference != representative).unwrap().clone();
    let mut split = state.read_with(cx, |state, _| state.scoped_project(&split_ref).unwrap().project.clone());
    split.repository_identity = Some(serde_json::json!({"canonicalKey":"different-repository"}));
    let (ready, split_applied) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
    let split_key = split_ref.clone();
    let _split = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == crate::app_state::AppEvent::Shell && state.scoped_project(&split_key).is_some_and(|project| project.project.repository_identity.as_ref().is_some_and(|identity| identity["canonicalKey"] == "different-repository")) {
            if let Some(ready) = ready.take() { let _ = ready.send(()); }
        }
    }));
    let split_events = events.clone();
    let sequence = next_sequence;
    next_sequence += 1;
    runtime.spawn(async move { split_events.send(t3_client::supervisor::ClientEvent::Shell(t3_core::shell::ShellStreamItem::ProjectUpdated { sequence, project:Box::new(split) })).await.unwrap(); }).await.unwrap();
    split_applied.await.unwrap();
    state.read_with(cx, |state, cx| {
        let current = state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx);
        assert!(current.iter().find(|group| group.key == stable.key).is_some_and(|group| group.member_refs.len() < stable.member_refs.len()));
        for captured in &stable.registrations {
            let current = state.scoped_project(&captured.reference()).unwrap();
            assert_eq!(current.owner, captured.owner);
            assert_eq!(current.project.created_at, captured.project.created_at);
            assert_eq!(current.project.workspace_root, captured.project.workspace_root);
        }
    });
    cx.update(|_, cx| menu.update(cx, |menu, cx| menu.activate(MenuAction::Project(t3_core::thread_actions::ProjectAction::Pin), cx)));
    let barrier = state.read_with(cx, |state, _| state.connections.destination(&original_owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE));
    runtime.spawn(barrier).await.unwrap().unwrap();
    cx.run_until_parked();
    let membership_refused = !background.take_commands().contains(&t3_core::projects::PROJECTS_MUTATE);
    state.read_with(cx, |state, cx| assert!(state.scoped_project_action_target(&stable, cx).is_err(), "Copy-path planning must refuse changed membership without touching the clipboard"));
    cx.update(|window, cx| menu.update(cx, |menu, cx| menu.open_scoped_project(stable.clone(), point(px(10.), px(10.)), window, cx)));
    let mut replaced_representative = state.read_with(cx, |state, _| state.scoped_project(&representative).unwrap().project.clone());
    replaced_representative.created_at = "replaced-representative".into();
    let (ready, representative_applied) = tokio::sync::oneshot::channel(); let mut ready = Some(ready);
    let replaced_ref = representative.clone();
    let _representative = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == crate::app_state::AppEvent::Shell && state.scoped_project(&replaced_ref).is_some_and(|project| project.project.created_at == "replaced-representative") {
            if let Some(ready) = ready.take() { let _ = ready.send(()); }
        }
    }));
    runtime.spawn(async move { events.send(t3_client::supervisor::ClientEvent::Shell(t3_core::shell::ShellStreamItem::ProjectUpdated { sequence:next_sequence, project:Box::new(replaced_representative) })).await.unwrap(); }).await.unwrap();
    representative_applied.await.unwrap();
    state.read_with(cx, |state, cx| assert!(state.scoped_project_action_target(&stable, cx).is_err(), "Copy-path planning must refuse a replaced representative without touching the clipboard"));
    cx.update(|_, cx| menu.update(cx, |menu, cx| menu.activate(MenuAction::Project(t3_core::thread_actions::ProjectAction::NewThread), cx)));
    let barrier = state.read_with(cx, |state, _| state.connections.destination(&original_owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
    runtime.spawn(barrier).await.unwrap().unwrap();
    cx.run_until_parked();
    let route_retained = state.read_with(cx, |state, _| (state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()) == before);
    assert!(membership_refused && route_retained, "captured project menu must refuse changed logical membership and replaced representative navigation: membership_refused={membership_refused}, route_retained={route_retained}");
    let (client, mut local) = t3_client::supervisor::ClientFixture::handle(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read","orchestration:operate"]})).unwrap());
    let (shell, config, connection) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone(), state.connection_id()));
    let (sent, loaded) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
    let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("saved-fixture").is_some() {
            if let Some(sent) = sent.take() { let _ = sent.send(()); }
        }
    }));
    let install = state.update(cx, |state, _| state.connections.install_fixture("saved-fixture", client, shell, config, t3_client::supervisor::ConnectionStatus::Connected));
    let local_events = runtime.spawn(install).await.unwrap(); loaded.await.unwrap();
    let (sent, activated) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
    let _activated = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == crate::app_state::AppEvent::Shell && state.connection_id() != connection && state.thread_shell("other-thread").is_some() {
            if let Some(sent) = sent.take() { let _ = sent.send(()); }
        }
    }));
    state.read_with(cx, |state, _| state.connections.activate(Some("saved-fixture")));
    activated.await.unwrap();
    let (owner, anchor) = state.update(cx, |state, cx| {
        let owner = state.scoped_environment("saved-fixture").unwrap().summary.owner.clone();
        assert_eq!(state.environments.active.as_deref(), Some(owner.saved_id.as_str()));
        assert_eq!(state.shell.generation, owner.shell_generation);
        assert_eq!(*state.client_handle().unwrap().connection_generation().borrow(), owner.connection_generation);
        let reference = t3_core::sidebar::grouping::ProjectRef { environment_id:owner.saved_id.clone(), project_id:"checkout-a".into() };
        let scope = state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx).into_iter().find(|group| group.member_refs.contains(&reference)).unwrap().key;
        state.set_scoped_project_scope(vec![scope], cx);
        let anchor = t3_core::sidebar::tag_filter::ThreadKey::new(&owner.saved_id, "sibling");
        assert!(state.sidebar.fleet.ordered.iter().any(|index| state.sidebar.fleet.threads[*index].key == anchor));
        state.click_scoped_sidebar_thread(&anchor, gpui::Modifiers { platform:cfg!(target_os="macos"), control:!cfg!(target_os="macos"), ..Default::default() }, cx);
        assert_eq!(state.sidebar.fleet_selection, [anchor.clone()].into_iter().collect());
        assert_eq!(state.sidebar.fleet_anchor.as_ref(), Some(&anchor));
        (owner, anchor)
    });
    let (sent, selected) = tokio::sync::oneshot::channel(); let mut sent = Some(sent);
    let _selected = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == crate::app_state::AppEvent::Selection && state.environments.active.as_deref() == Some("saved-fixture") && state.selected_thread.as_deref() == Some("other-thread") {
            if let Some(sent) = sent.take() { let _ = sent.send(()); }
        }
    }));
    state.update(cx, |state, cx| state.navigate_scoped(owner.clone(), crate::app_state::ScopedRouteAction::Thread("other-thread".into()), cx));
    selected.await.unwrap();
    let (method, payload, reply) = local.next_request().await;
    assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
    assert_eq!(payload["type"], "thread.visit"); assert_eq!(payload["threadId"], "other-thread");
    reply.send(Ok(serde_json::json!({"sequence":1}))).unwrap();
    state.read_with(cx, |state, _| {
        assert_eq!(state.scoped_environment("saved-fixture").unwrap().summary.owner, owner);
        assert!(state.project_preferences.scope_keys.is_empty());
        assert!(state.sidebar.fleet_selection.is_empty());
        assert!(state.sidebar.fleet_anchor.is_none(), "automatic route widening clears the previous scoped anchor: {anchor:?}");
        assert!(state.sidebar.fleet.thread(&t3_core::sidebar::tag_filter::ThreadKey::new("saved-fixture", "other-thread")).is_some());
    });
    runtime.spawn_blocking(|| t3_client::store::commit_client_settings(serde_json::Map::new())).await
        .expect("fixture writer barrier task").expect("route reveal preferences committed before teardown");
    cx.run_until_parked();
    state.update(cx, |state, _| state.stop_pump());
    drop(local_events);
}

#[gpui::test]
async fn scoped_preferences_follow_canonical_arrival(cx: &mut gpui::TestAppContext) {
    use t3_core::sidebar::grouping::{GroupConsumer, ProjectPreferences, ProjectRef};
    use t3_client::supervisor::{ClientEvent, ClientFixture, ConnectionStatus};
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, mut active) = cx.update(|cx| project_filter_tests::install_state(cx, runtime.handle().clone()));
    let route = state.read_with(cx, |state, _| (state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()));
    let (client, mut arriving) = ClientFixture::handle(serde_json::from_value(serde_json::json!({"authenticated":true,"permissions":["orchestration:read","orchestration:operate"]})).unwrap());
    let (mut shell, mut config) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
    for project in &mut shell.snapshot.as_mut().unwrap().projects { project.repository_identity = None; }
    config.config.as_mut().unwrap().environment.environment_id.clear();
    let (ready, loaded) = tokio::sync::oneshot::channel();
    let mut ready = Some(ready);
    let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("arriving").is_some_and(|entry| entry.summary.owner.canonical_id.is_none()) {
            if let Some(ready) = ready.take() { let _ = ready.send(()); }
        }
    }));
    let install = state.update(cx, |state, _| state.connections.install_fixture("arriving", client, shell, config.clone(), ConnectionStatus::Connected));
    let events = runtime.spawn(install).await.unwrap();
    loaded.await.unwrap();
    let reference = ProjectRef { environment_id:"arriving".into(), project_id:"checkout-b".into() };
    state.update(cx, |state, cx| {
        let key = state.scoped_project(&reference).unwrap().physical_key();
        state.project_preferences = ProjectPreferences { scope_keys:vec![key.clone(), "missing-intent".into()], project_order:vec![key.clone()], project_expanded:std::collections::HashMap::from([(key, false)]) };
        state.rebuild_sidebar(cx);
    });
    let (ready, changed) = tokio::sync::oneshot::channel();
    let mut ready = Some(ready);
    let _changed = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("arriving").is_some_and(|entry| entry.summary.owner.canonical_id.as_deref() == Some("arrived-env")) {
            if let Some(ready) = ready.take() { let _ = ready.send(()); }
        }
    }));
    config.config.as_mut().unwrap().environment.environment_id = "arrived-env".into();
    runtime.spawn(async move { events.send(ClientEvent::ServerConfig(Box::new(t3_core::server_config::ServerConfigStreamEvent::Snapshot { config:Box::new(config.config.unwrap()) }))).await.unwrap(); }).await.unwrap();
    changed.await.unwrap();
    state.read_with(cx, |state, cx| {
        let key = state.scoped_project(&reference).unwrap().physical_key();
        assert!(state.project_preferences.scope_keys.contains(&key), "canonical arrival must migrate the selected physical project");
        assert_eq!(state.project_preferences.project_order, vec![key.clone()]);
        assert_eq!(state.project_preferences.project_expanded.get(&key), Some(&false));
        assert!(state.project_preferences.scope_keys.contains(&"missing-intent".to_owned()));
        let groups = state.project_groups_for(GroupConsumer::Sidebar, cx);
        assert_eq!(state.project_preferences.resolved_scope(&groups, true), vec![key]);
        assert_eq!((state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()), route);
    });
    assert!(active.take_commands().is_empty());
    assert!(arriving.take_commands().is_empty());
    let incompatible = state.update(cx, |state, cx| {
        let key=state.scoped_project(&ProjectRef { environment_id:"arriving".into(), project_id:"checkout-a".into() }).unwrap().physical_key();
        state.set_scoped_project_scope(vec![key.clone()], cx);
        state.sidebar.fleet_selection.insert(t3_core::sidebar::tag_filter::ThreadKey::new("arriving", "sibling"));
        key
    });
    let (menu, cx)=cx.add_window_view(|_, cx| ContextMenu::new(state.clone(), cx));
    cx.update(|window, cx| menu.update(cx, |menu, cx| {
        menu.open_for_scoped_thread(&t3_core::sidebar::tag_filter::ThreadKey::new("arriving", "menu-thread"), point(px(10.), px(10.)), window, cx);
        menu.close(cx);
    }));
    state.read_with(cx, |state, _| {
        assert_eq!(state.project_preferences.scope_keys, vec![incompatible.clone()], "merely opening a menu must not widen the scope");
        assert_eq!((state.environments.active.clone(),state.selected_thread.clone(),state.draft_project.clone()), route);
    });
    let (send, selected)=tokio::sync::oneshot::channel(); let mut send=Some(send);
    let _selected=state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event==crate::app_state::AppEvent::Selection && state.environments.active.as_deref()==Some("arriving") && state.selected_thread.as_deref()==Some("menu-thread") {
            if let Some(send)=send.take() { let _=send.send(()); }
        }
    }));
    state.update(cx, |state, cx| {
        let owner=state.scoped_environment("arriving").unwrap().summary.owner.clone();
        state.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Thread("menu-thread".into()), cx);
    });
    selected.await.unwrap();
    let (method, payload, reply)=arriving.next_request().await;
    assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
    assert_eq!(payload["type"], "thread.visit");
    assert_eq!(payload["threadId"], "menu-thread");
    reply.send(Ok(serde_json::json!({"sequence":2}))).unwrap();
    state.read_with(cx, |state, _| {
        assert!(state.project_preferences.scope_keys.is_empty(), "an actual new route must reveal the out-of-scope destination");
        assert!(state.sidebar.fleet_selection.is_empty());
        assert!(state.sidebar.fleet.thread(&t3_core::sidebar::tag_filter::ThreadKey::new("arriving", "menu-thread")).is_some());
    });
    runtime.spawn_blocking(|| t3_client::store::commit_client_settings(serde_json::Map::new())).await.unwrap().unwrap();
    assert!(t3_client::store::load_project_preferences().scope_keys.is_empty(), "the widened scope must be persisted");
    state.update(cx, |state, cx| {
        state.set_scoped_project_scope(vec![incompatible.clone()], cx);
        state.rebuild_sidebar(cx);
        state.select_thread("menu-thread", cx);
        state.rebuild_sidebar(cx);
        assert_eq!(state.project_preferences.scope_keys, vec![incompatible], "a deliberate scope on the same route must survive rebuilds and reselect");
    });
    assert!(!arriving.take_commands().iter().any(|method| matches!(*method, t3_core::commands::method::DISPATCH_COMMAND | t3_core::projects::PROJECTS_MUTATE)));
    let search=cx.update(|window, cx| {
        let input=cx.new(|cx| gpui_component::input::InputState::new(window, cx));
        input.update(cx, |input, cx| input.set_value("pending search", window, cx));
        state.update(cx, |state, _| state.sidebar.search_input=Some(input.downgrade()));
        input
    });
    let chosen=state.read_with(cx, |state, _| state.project_preferences.scope_keys.clone());
    let pending=t3_core::sidebar::tag_filter::ThreadKey::new("arriving", "other-thread");
    state.update(cx, |state, cx| {
        let owner=state.scoped_environment("arriving").unwrap().summary.owner.clone();
        state.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Thread("other-thread".into()), cx);
        assert_eq!(state.selected_thread.as_deref(), Some("other-thread"));
        assert_eq!(state.sidebar.route_pending.as_ref(), Some(&pending));
        assert_eq!(state.project_preferences.scope_keys, chosen);
    });
    let (method, payload, reply)=arriving.next_request().await;
    assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
    assert_eq!(payload["type"], "thread.visit");
    assert_eq!(payload["threadId"], "other-thread");
    reply.send(Ok(serde_json::json!({"sequence":3}))).unwrap();
    state.update(cx, |state, cx| {
        state.start_draft("checkout-a", cx);
        assert!(state.selected_thread.is_none());
        assert_eq!(state.sidebar.route_pending.as_ref(), Some(&pending));
        assert_eq!(state.project_preferences.scope_keys, chosen);
    });
    cx.update(|window, cx| search.update(cx, |input, cx| input.set_value("", window, cx)));
    state.update(cx, |state, cx| {
        state.rebuild_sidebar(cx);
        assert!(state.selected_thread.is_none());
        assert_eq!(state.draft_project.as_deref(), Some("checkout-a"));
        assert_eq!(state.project_preferences.scope_keys, chosen, "clearing search on a draft must not reveal a stale thread route");
    });
    state.update(cx, |state, _| state.stop_pump());
}
