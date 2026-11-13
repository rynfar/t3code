use super::*;
use crate::context_menu::project_filter_tests::{install_state, session};
use gpui::{AppContext, StyleRefinement};
use serde_json::json;
use t3_client::supervisor::{ClientFixture, ConnectionStatus};

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("t3-drag-test-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &std::path::Path { &self.0 }
}
impl Drop for TempDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

struct DragHost {
    pane: Entity<SidebarPane>,
    composer: Entity<crate::composer::Composer>,
    menu: Entity<ContextMenu>,
    height: Pixels,
}

impl Render for DragHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let _ = &self.composer;
        div()
            .w(px(320.))
            .h(self.height)
            .child(self.pane.clone().cached(StyleRefinement::default().size_full()))
            .child(self.menu.clone())
    }
}

fn thread_json(id: &str, title: &str, state: &str, updated_at: &str) -> serde_json::Value {
    let run = (state == "running").then_some("fixture-run");
    let started = (state == "running").then_some(updated_at);
    json!({
        "id": id,
        "projectId": "proj-1",
        "title": title,
        "providerInstanceId": "inst-1",
        "modelSelection": {"instanceId": "inst-1", "model": "gpt-5"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "lineage": {"rootThreadId": id},
        "branch": null,
        "worktreePath": null,
        "latestTurn": null,
        "createdAt": "2026-01-01T00:00:00.000Z",
        "updatedAt": updated_at,
        "archivedAt": null,
        "deletedAt": null,
        "status": state,
        "latestRunId": run,
        "activeRunId": run,
        "activityRunStartedAt": started,
        "latestUserAuthoredMessageAt": started,
        "hasActionableProposedPlan": false,
        "itemCount": 0,
        "visibleItemCount": 0,
        "pinnedAt": null,
        "settledAt": null,
        "snoozedUntil": null,
        "lastVisitedAt": updated_at,
        "pinOrderKey": null,
        "activeOrderKey": null,
    })
}

fn setup_app(cx: &mut gpui::TestAppContext, runtime: &tokio::runtime::Runtime) -> (Entity<AppState>, ClientFixture) {
    cx.update(|cx| {
        gpui_component::init(cx);
        let installed = install_state(cx, runtime.handle().clone());
        crate::composer::init(cx);
        crate::keymap::init(cx);
        crate::settings::init(installed.0.clone(), cx);
        crate::updates::init(cx);
        installed
    })
}

#[gpui::test]
async fn thread_drag_permissions_supports_settlement_and_mid_drag_cancellation(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, mut fixture) = setup_app(cx, &runtime);
    state.update(cx, |s, cx| {
        let snap = s.shell.snapshot.as_mut().unwrap();
        snap.projects = vec![serde_json::from_value(json!({
            "id": "proj-1",
            "title": "App",
            "workspaceRoot": "/app",
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z"
        })).unwrap()];
        snap.threads = vec![
            serde_json::from_value(thread_json("th-pin", "Pinned", "idle", "2026-01-02T00:00:00.000Z")).unwrap(),
            serde_json::from_value(thread_json("th-act-1", "Active One", "idle", "2026-01-02T00:01:00.000Z")).unwrap(),
            serde_json::from_value(thread_json("th-act-2", "Active Two", "idle", "2026-01-02T00:02:00.000Z")).unwrap(),
            serde_json::from_value(thread_json("th-set", "Settled", "idle", "2026-01-01T00:00:00.000Z")).unwrap(),
        ];
        snap.threads[0].pinned_at = Some("2026-01-02T00:00:00.000Z".into());
        snap.threads[0].pin_order_key = Some("m".into());
        snap.threads[3].settled_at = Some("2026-01-02T00:00:00.000Z".into());
        s.sidebar.capabilities.can_operate = false;
        s.sidebar.capabilities.config_loaded = true;
        s.rebuild_sidebar(cx);
    });

    let (root, vcx) = cx.add_window_view(|window, cx| {
        let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
        let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
        let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
        let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer.clone(), menu.clone(), confirm, snooze, window, cx));
        DragHost { pane, composer, menu, height: px(600.) }
    });
    let pane = root.read_with(vcx, |host, _| host.pane.clone());
    vcx.run_until_parked();
    let _ = fixture.take_commands();

    // Read-only environment must refuse thread drag planning & start.
    state.update(vcx, |s, cx| {
        s.start_thread_drag("th-act-1", cx);
        assert!(s.sidebar.drag.is_none(), "read-only environment must not start thread drag");
        let slot = s.sidebar.model.rows.iter().position(|row| matches!(row, t3_core::sidebar::SidebarRow::Marker(t3_core::sidebar::drag::Marker::SettledPlaceholder | t3_core::sidebar::drag::Marker::SettledHeader))).unwrap_or(s.sidebar.model.rows.len() - 1);
        assert!(s.thread_drop_target(slot).is_none(), "read-only thread_drop_target must be None");
    });

    // Grant operate permission, start drag in pane, then revoke permission mid-drag -> must cancel both controller and pane drag.
    state.update(vcx, |s, cx| {
        s.sidebar.capabilities.can_operate = true;
        s.rebuild_sidebar(cx);
    });
    pane.update(vcx, |p, cx| {
        let preview = cx.new(|_| drag::ThreadDragPreview::new("Active One".into(), None, Default::default()));
        p.begin_drag("th-act-1", None, preview, cx);
    });
    vcx.run_until_parked();
    assert!(state.read_with(vcx, |s, _| s.sidebar.drag.is_some()));
    assert!(pane.read_with(vcx, |p, _| p.drag.is_some()));

    // Pressing Escape cancels active thread drag without dispatching commands.
    assert!(vcx.update(|window, cx| pane.update(cx, |p, cx| p.handle_escape(false, window, cx))));
    vcx.run_until_parked();
    assert!(state.read_with(vcx, |s, _| s.sidebar.drag.is_none()));
    assert!(pane.read_with(vcx, |p, _| p.drag.is_none()));

    // Start again, then make dragged row enter Working mid-drag -> drag must cancel automatically.
    pane.update(vcx, |p, cx| {
        let preview = cx.new(|_| drag::ThreadDragPreview::new("Active One".into(), None, Default::default()));
        p.begin_drag("th-act-1", None, preview, cx);
    });
    vcx.run_until_parked();
    state.update(vcx, |s, cx| {
        s.shell.snapshot.as_mut().unwrap().threads[1] = serde_json::from_value(thread_json("th-act-1", "Active One", "running", "2026-01-02T00:01:00.000Z")).unwrap();
        s.rebuild_sidebar(cx);
        cx.emit(AppEvent::Shell);
    });
    vcx.run_until_parked();
    assert!(state.read_with(vcx, |s, _| s.sidebar.drag.is_none()), "moving dragged row to Working must cancel controller drag");
    assert!(pane.read_with(vcx, |p, _| p.drag.is_none()), "moving dragged row to Working must cancel pane drag");

    // Start on th-act-2, then drop can_operate mid-drag -> must cancel automatically.
    pane.update(vcx, |p, cx| {
        let preview = cx.new(|_| drag::ThreadDragPreview::new("Active Two".into(), None, Default::default()));
        p.begin_drag("th-act-2", None, preview, cx);
    });
    vcx.run_until_parked();
    state.update(vcx, |s, cx| {
        s.sidebar.capabilities.can_operate = false;
        s.rebuild_sidebar(cx);
        cx.emit(AppEvent::Shell);
    });
    vcx.run_until_parked();
    assert!(state.read_with(vcx, |s, _| s.sidebar.drag.is_none()), "losing can_operate mid-drag must cancel controller drag");
    assert!(pane.read_with(vcx, |p, _| p.drag.is_none()), "losing can_operate mid-drag must cancel pane drag");
    assert!(fixture.take_commands().is_empty());

    // Scoped fleet: supports_settlement is gated by env.config.config.is_some(), and read-only env refuses drag.
    let (c_op, _f_op) = ClientFixture::handle(session(true));
    let (c_ro, _f_ro) = ClientFixture::handle(session(false));
    let mut shell_op = state.read_with(vcx, |s, _| s.shell.clone());
    shell_op.snapshot.as_mut().unwrap().threads = vec![
        serde_json::from_value(thread_json("scoped-act", "Scoped Active", "idle", "2026-01-02T00:05:00.000Z")).unwrap(),
        serde_json::from_value(thread_json("scoped-set", "Scoped Settled", "idle", "2026-01-01T00:00:00.000Z")).unwrap(),
    ];
    shell_op.snapshot.as_mut().unwrap().threads[1].settled_at = Some("2026-01-02T00:00:00.000Z".into());
    shell_op.synchronized = true;
    let shell_ro = shell_op.clone();
    let mut cfg_op = state.read_with(vcx, |s, _| s.server_config.clone());
    cfg_op.config.as_mut().unwrap().environment.environment_id = "env-op-srv".into();
    let cfg_empty = t3_core::server_config::ServerConfigState::default();
    let i1 = state.update(vcx, |s, _| s.connections.install_fixture("op-no-cfg", c_op, shell_op, cfg_empty, ConnectionStatus::Connected));
    let i2 = state.update(vcx, |s, _| s.connections.install_fixture("ro-with-cfg", c_ro, shell_ro, cfg_op, ConnectionStatus::Connected));
    let _e1 = runtime.spawn(i1).await.unwrap();
    let _e2 = runtime.spawn(i2).await.unwrap();
    vcx.run_until_parked();

    let ro_key = t3_core::sidebar::tag_filter::ThreadKey::new("ro-with-cfg", "scoped-act");
    let op_key = t3_core::sidebar::tag_filter::ThreadKey::new("op-no-cfg", "scoped-act");
    state.update(vcx, |s, cx| {
        s.start_scoped_thread_drag(&ro_key, cx);
        assert!(s.sidebar.fleet_drag.is_none(), "read-only scoped row must not start drag");
        s.start_scoped_thread_drag(&op_key, cx);
        // Without config loaded on op-no-cfg, supports_settlement must be false (so Settle slot returns None).
        if let Some(slot) = s.sidebar.fleet.rows.iter().position(|r| matches!(r, t3_core::sidebar::fleet::ScopedSidebarRow::Marker(t3_core::sidebar::drag::Marker::SettledHeader))) {
            assert!(s.scoped_thread_drop_target(slot, cx).is_none(), "missing server config must disable settlement drop target");
        }
        s.end_thread_drag(cx);
        s.stop_pump();
    });
}

#[gpui::test]
async fn drag_scroll_stability_and_conditional_settled_overflow_reveal(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _fixture) = setup_app(cx, &runtime);
    state.update(cx, |s, cx| {
        let snap = s.shell.snapshot.as_mut().unwrap();
        snap.projects = vec![serde_json::from_value(json!({
            "id": "proj-1",
            "title": "App",
            "workspaceRoot": "/app",
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z"
        })).unwrap()];
        // No pinned threads initially; enough active threads + collapsed settled shelf to require vertical scroll in 400px viewport.
        let mut threads = Vec::new();
        for idx in 0..20 {
            let ts = format!("2026-01-02T00:{idx:02}:00.000Z");
            threads.push(serde_json::from_value(thread_json(&format!("act-{idx}"), &format!("Thread {idx}"), "idle", &ts)).unwrap());
        }
        snap.threads = threads;
        s.sidebar.capabilities.can_operate = true;
        s.sidebar.capabilities.config_loaded = true;
        s.rebuild_sidebar(cx);
    });

    let (root, vcx) = cx.add_window_view(|window, cx| {
        let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
        let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
        let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
        let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer.clone(), menu.clone(), confirm, snooze, window, cx));
        DragHost { pane, composer, menu, height: px(400.) }
    });
    let pane = root.read_with(vcx, |host, _| host.pane.clone());
    vcx.run_until_parked();
    vcx.update(|window, cx| { window.draw(cx).clear(); });

    // Start drag at scroll top 0 when Pinned is empty: splicing PinnedHeader/PinnedDivider at index 0 must keep scroll top at item_ix == 0.
    let initial_top = pane.read_with(vcx, |p, _| p.list.logical_scroll_top());
    assert_eq!(initial_top.item_ix, 0);
    pane.update(vcx, |p, cx| {
        let preview = cx.new(|_| drag::ThreadDragPreview::new("Thread 19".into(), None, Default::default()));
        p.begin_drag("act-19", None, preview, cx);
    });
    vcx.run_until_parked();
    pane.update(vcx, |_, cx| cx.notify());
    vcx.update(|window, cx| { window.draw(cx).clear(); });
    assert!(pane.read_with(vcx, |p, _| p.drag.is_some()), "begin_drag must activate pane drag");
    assert!(pane.read_with(vcx, |p, _| matches!(
        p.row_keys.first(),
        Some(PaneRowKey::Thread(t3_core::sidebar::RowKey::Marker(t3_core::sidebar::drag::Marker::PinnedHeader)))
    )));
    let drag_top = pane.read_with(vcx, |p, _| p.list.logical_scroll_top());
    assert_eq!(drag_top.item_ix, 0, "top marker splice at scroll top 0 must keep PinnedHeader in view (#293)");
    assert!(vcx.debug_bounds("sidebar-drag-marker-pinned-header").is_some(), "PinnedHeader bounds missing; act-19={:?}", vcx.debug_bounds("thread-row-None-act-19"));

    // Hovering PinnedHeader (valid non-Settled target -> Pinned) must NOT scroll SettledPlaceholder into view.
    pane.update(vcx, |p, cx| {
        p.set_drag_position_for_test(0, false, cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| { window.draw(cx).clear(); });
    assert_eq!(pane.read_with(vcx, |p, _| p.drop_target()), Some(t3_core::sidebar::Section::Pinned));
    assert_eq!(pane.read_with(vcx, |p, _| p.list.logical_scroll_top().item_ix), 0, "Pinned target must not jump scroll to SettledPlaceholder");

    // Hovering SettledPlaceholder (switching target -> Settled) MUST reveal SettledPlaceholder in the scrollable list (#293).
    let settled_idx = pane.read_with(vcx, |p, _| {
        p.row_keys.iter().position(|k| matches!(k, PaneRowKey::Thread(t3_core::sidebar::RowKey::Marker(t3_core::sidebar::drag::Marker::SettledPlaceholder)))).unwrap()
    });
    pane.update(vcx, |p, cx| {
        p.set_drag_position_for_test(settled_idx, false, cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| { window.draw(cx).clear(); });
    let after_settled_top = pane.read_with(vcx, |p, _| p.list.logical_scroll_top());
    assert!(after_settled_top.item_ix > 0, "Settled target must reveal SettledPlaceholder (#293), got {after_settled_top:?}");

    pane.update(vcx, |p, cx| p.finish_drag(false, cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| { window.draw(cx).clear(); });

    // Start and end drag while already scrolled down (item_ix > 0, offset > 0) when both Pinned (top) and Settled (bottom) insert markers (#293).
    pane.update(vcx, |p, _| {
        p.list.scroll_to(gpui::ListOffset { item_ix: 8, offset_in_item: px(12.) });
    });
    vcx.update(|window, cx| { window.draw(cx).clear(); });
    let scrolled_before = pane.read_with(vcx, |p, _| p.list.logical_scroll_top());
    assert_eq!(scrolled_before.item_ix, 8);
    assert_eq!(scrolled_before.offset_in_item, px(12.));
    let anchor_key = pane.read_with(vcx, |p, _| p.row_keys[scrolled_before.item_ix].clone());

    pane.update(vcx, |p, cx| {
        let preview = cx.new(|_| drag::ThreadDragPreview::new("Thread 10".into(), None, Default::default()));
        p.begin_drag("act-10", None, preview, cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| { window.draw(cx).clear(); });
    let scrolled_during = pane.read_with(vcx, |p, _| p.list.logical_scroll_top());
    let expected_during_ix = pane.read_with(vcx, |p, _| p.row_keys.iter().position(|k| k == &anchor_key).unwrap());
    assert_eq!(scrolled_during.item_ix, expected_during_ix, "starting drag while scrolled must re-anchor instead of jumping to top (#293)");
    assert_eq!(scrolled_during.offset_in_item, px(12.));

    pane.update(vcx, |p, cx| p.finish_drag(false, cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| { window.draw(cx).clear(); });
    let scrolled_after = pane.read_with(vcx, |p, _| p.list.logical_scroll_top());
    assert_eq!(scrolled_after.item_ix, 8, "ending drag while scrolled must re-anchor back without jumping to top (#293)");
    assert_eq!(scrolled_after.offset_in_item, px(12.));
    state.update(vcx, |s, _| s.stop_pump());
}

#[gpui::test]
async fn project_manual_reorder_drag_and_drop_persists_within_same_band(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _fixture) = setup_app(cx, &runtime);
    cx.update(|cx| {
        crate::settings::store::install_committed_client(
            &state,
            serde_json::from_value(json!({"sidebarProjectGroupingMode": "separate", "sidebarProjectSortOrder": "manual"})).unwrap(),
            cx,
        );
    });
    let (client, _fix) = ClientFixture::handle(session(true));
    let mut shell = t3_core::shell::ShellState::default();
    shell.apply(serde_json::from_value(json!({
        "kind": "snapshot",
        "snapshot": {
            "snapshotSequence": 1,
            "projects": [
                {"id": "p-pin", "title": "PinnedProj", "workspaceRoot": "/pin", "pinnedAt": "2026-01-02T00:00:00.000Z", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"},
                {"id": "p-alpha", "title": "Alpha", "workspaceRoot": "/alpha", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"},
                {"id": "p-beta", "title": "Beta", "workspaceRoot": "/beta", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"},
                {"id": "p-scratch", "title": "Scratch", "workspaceRoot": "/scratch", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"},
                {"id": "p-arch", "title": "Archived", "workspaceRoot": "/arch", "archivedAt": "2026-01-02T00:00:00.000Z", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"}
            ],
            "threads": []
        }
    })).unwrap());
    shell.synchronized = true;
    let mut config = t3_core::server_config::ServerConfigState::default();
    config.apply(serde_json::from_value(json!({
        "type": "snapshot",
        "config": {
            "environment": {"environmentId": "srv-1", "label": "Local", "platform": {"os": "macos", "arch": "arm64"}, "capabilities": {"projectOrganization": true}},
            "cwd": "/",
            "scratchWorkspaceRoot": "/scratch",
            "settings": {}
        }
    })).unwrap());
    let install = state.update(cx, |s, _| s.connections.install_fixture("env-1", client, shell, config, ConnectionStatus::Connected));
    let _ev = runtime.spawn(install).await.unwrap();
    cx.run_until_parked();

    let (root, vcx) = cx.add_window_view(|window, cx| {
        let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
        let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
        let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
        let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer.clone(), menu.clone(), confirm, snooze, window, cx));
        DragHost { pane, composer, menu, height: px(600.) }
    });
    let pane = root.read_with(vcx, |host, _| host.pane.clone());
    vcx.run_until_parked();

    let keys = state.read_with(vcx, |s, cx| {
        s.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx)
            .into_iter()
            .map(|g| (g.label, g.key))
            .collect::<std::collections::HashMap<_, _>>()
    });
    let alpha = keys["Alpha"].clone();
    let beta = keys["Beta"].clone();
    let pin = keys["PinnedProj"].clone();
    let arch = keys["Archived"].clone();

    // Cross-band drop must be refused.
    assert!(!state.update(vcx, |s, cx| s.reorder_scoped_project(&alpha, &pin, false, cx)));
    assert!(!state.update(vcx, |s, cx| s.reorder_scoped_project(&alpha, &arch, false, cx)));

    // Same-band manual drop moves Alpha after Beta, persists preferences, and Escape cancels an in-progress folder drag cleanly.
    pane.update(vcx, |p, cx| {
        p.begin_project_drag(alpha.clone(), "Alpha".into(), Default::default(), cx);
    });
    assert!(pane.read_with(vcx, |p, _| p.project_drag.is_some()));
    assert!(vcx.update(|window, cx| pane.update(cx, |p, cx| p.handle_escape(false, window, cx))));
    assert!(pane.read_with(vcx, |p, _| p.project_drag.is_none()));

    assert!(state.update(vcx, |s, cx| s.reorder_scoped_project(&alpha, &beta, true, cx)));
    vcx.run_until_parked();
    let labels: Vec<String> = state.read_with(vcx, |s, cx| {
        s.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx)
            .into_iter()
            .filter(|g| !g.pinned && !g.archived && !g.is_scratch)
            .map(|g| g.label)
            .collect()
    });
    assert_eq!(labels, vec!["Beta".to_owned(), "Alpha".to_owned()]);
    assert!(!state.read_with(vcx, |s, _| s.project_preferences.project_order.is_empty()));

    // Non-manual sort order refuses drag reorder.
    vcx.update(|_, cx| {
        crate::settings::store::install_committed_client(
            &state,
            serde_json::from_value(json!({"sidebarProjectGroupingMode": "separate", "sidebarProjectSortOrder": "updated-at"})).unwrap(),
            cx,
        );
    });
    vcx.run_until_parked();
    assert!(!state.update(vcx, |s, cx| s.reorder_scoped_project(&beta, &alpha, true, cx)));
    state.update(vcx, |s, _| s.stop_pump());
}

#[gpui::test]
async fn external_file_drop_on_sidebar_and_search_navigates_with_key_isolation(cx: &mut gpui::TestAppContext) {
    use t3_client::environments::{EnvironmentEvent, Environments};
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _fixture) = setup_app(cx, &runtime);
    let (connections, mut output) = Environments::start(runtime.handle(), false);
    state.update(cx, |s, _| {
        s.stop_pump();
        s.connections = connections;
        for (saved, srv) in [("env-a", "srv-a"), ("env-b", "srv-b")] {
            s.environments.environments.push(serde_json::from_value(json!({
                "id": saved,
                "environmentId": srv,
                "label": saved,
                "httpBase": "http://fixture.invalid",
                "accessToken": "",
                "disabled": false
            })).unwrap());
        }
    });
    let mut view = state.read_with(cx, |s, _| s.connections.view());
    let tmp = TempDir::new("ext-drop");
    let file_a = tmp.path().join("notes.txt");
    let file_b = tmp.path().join("spec.md");
    std::fs::write(&file_a, "hello").unwrap();
    std::fs::write(&file_b, "world").unwrap();

    let rw_session: t3_core::auth::AuthSessionState = serde_json::from_value(json!({
        "authenticated": true,
        "permissions": [t3_core::auth::ORCHESTRATION_READ_SCOPE, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE]
    })).unwrap();
    let mut _client_events = Vec::new();
    let mut _client_fixtures = Vec::new();
    for (saved, srv) in [("env-a", "srv-a"), ("env-b", "srv-b")] {
        let (client, fix) = ClientFixture::handle(rw_session.clone());
        fix.grants.send_replace(Some(rw_session.clone()));
        _client_fixtures.push(fix);
        let mut shell = t3_core::shell::ShellState::default();
        shell.apply(serde_json::from_value(json!({
            "kind": "snapshot",
            "snapshot": {
                "snapshotSequence": 1,
                "projects": [
                    {"id": "proj-1", "title": "App", "workspaceRoot": "/app", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"}
                ],
                "threads": [
                    thread_json("shared-id", &format!("Thread on {saved}"), "idle", "2026-01-02T00:00:00.000Z"),
                    thread_json("other-id", &format!("Other on {saved}"), "idle", "2026-01-02T00:01:00.000Z"),
                    thread_json("row-target", &format!("Row target on {saved}"), "idle", "2026-01-02T00:02:00.000Z")
                ]
            }
        })).unwrap());
        shell.synchronized = true;
        let mut config = t3_core::server_config::ServerConfigState::default();
        config.apply(serde_json::from_value(json!({
            "type": "snapshot",
            "config": {
                "environment": {"environmentId": srv, "label": saved, "platform": {"os": "macos", "arch": "arm64"}, "capabilities": {"fileAttachments": {"maxUploadBytes": 1000000}}},
                "cwd": "/",
                "settings": {}
            }
        })).unwrap());
        let install = state.update(cx, |s, _| s.connections.install_fixture(saved, client, shell, config, ConnectionStatus::Connected));
        _client_events.push(runtime.spawn(install).await.unwrap());
    }
    let fleet_view = view.borrow_and_update().clone();
    state.update(cx, |s, cx| {
        s.apply_fleet_view(fleet_view, cx);
        for saved in ["env-a", "env-b"] {
            let owner = s.scoped_environment(saved).expect("scoped env installed").summary.owner.clone();
            assert!(s.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE), "{saved} must be scoped_ready");
        }
        s.environments.active = Some("env-a".into());
        s.connections.activate(Some("env-a"));
    });
    let (ev_a, mut output) = runtime.spawn(async move {
        loop {
            let ev = output.recv().await.unwrap();
            if matches!(&ev, EnvironmentEvent::Activated { id, .. } if id == "env-a") {
                return (ev, output);
            }
        }
    }).await.unwrap();
    state.update(cx, |s, cx| s.apply_environment_event(ev_a, cx));
    cx.run_until_parked();

    let (root, vcx) = cx.add_window_view(|window, cx| {
        let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
        let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
        let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
        let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer.clone(), menu.clone(), confirm, snooze, window, cx));
        DragHost { pane, composer, menu, height: px(600.) }
    });
    let composer = root.read_with(vcx, |host, _| host.composer.clone());
    let pane = root.read_with(vcx, |host, _| host.pane.clone());
    state.update(vcx, |s, cx| s.select_thread("shared-id", cx));
    vcx.run_until_parked();

    // Queue pending drop targeting (env-b, shared-id): env-a's identically named "shared-id" MUST NOT consume it!
    let key_b = t3_core::sidebar::tag_filter::ThreadKey::new("env-b", "shared-id");
    state.update(vcx, |s, cx| {
        s.drop_files_on_sidebar_thread(key_b.clone(), vec![file_a.clone()], cx);
    });
    vcx.run_until_parked();
    assert_eq!(composer.read_with(vcx, |c, cx| c.snapshot_audit(0, cx)["regularAttachmentCount"].clone()), json!(0), "env-a must never consume env-b pending drop even when raw thread.id matches");

    // Switching / completing navigation to env-b selects (env-b, shared-id) and consumes file_a once.
    let (ev_b, mut output) = runtime.spawn(async move {
        loop {
            let ev = output.recv().await.unwrap();
            if matches!(&ev, EnvironmentEvent::Activated { id, .. } if id == "env-b") {
                return (ev, output);
            }
        }
    }).await.unwrap();
    state.update(vcx, |s, cx| s.apply_environment_event(ev_b, cx));
    vcx.run_until_parked();
    assert_eq!(state.read_with(vcx, |s, _| s.environments.active.as_deref().map(str::to_owned)), Some("env-b".into()));
    assert_eq!(state.read_with(vcx, |s, _| s.selected_thread.clone()), Some("shared-id".into()));
    assert!(!state.read_with(vcx, |s, _| s.has_pending_composer_file_drop()), "delivered drop must be cleared");
    assert_eq!(composer.read_with(vcx, |c, cx| c.snapshot_audit(0, cx)["regularAttachmentCount"].clone()), json!(1));

    // Dropping via a search result row delivers once to target thread.
    pane.update(vcx, |p, cx| {
        p.search.set_query("Other on env-b".into(), &p.state, cx);
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| { window.draw(cx).clear(); });
    let search_key = t3_core::sidebar::tag_filter::ThreadKey::new("env-b", "other-id");
    let search_row_id: &'static str = Box::leak(format!("search-result-{:?}", search_key).into_boxed_str());
    let search_pos = vcx.debug_bounds(search_row_id).expect("search result row rendered").center();
    vcx.simulate_event(gpui::FileDropEvent::Entered { position: search_pos, paths: gpui::ExternalPaths(vec![file_b.clone()].into()) });
    vcx.simulate_event(gpui::FileDropEvent::Submit { position: search_pos });
    vcx.run_until_parked();
    assert_eq!(state.read_with(vcx, |s, _| s.selected_thread.clone()), Some("other-id".into()));
    assert_eq!(composer.read_with(vcx, |c, cx| c.snapshot_audit(0, cx)["regularAttachmentCount"].clone()), json!(1));

    // Clear search before dropping via GPUI FileDropEvent directly onto a rendered sidebar thread row (row.rs).
    pane.update(vcx, |p, cx| {
        p.search.set_query(String::new(), &p.state, cx);
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| { window.draw(cx).clear(); });
    let row_key_a = t3_core::sidebar::tag_filter::ThreadKey::new("env-a", "row-target");
    let row_id_a: &'static str = Box::leak(format!("thread-row-{:?}-row-target", Some(&row_key_a)).into_boxed_str());
    let row_pos_a = vcx.debug_bounds(row_id_a).expect("rendered env-a sidebar row").center();
    vcx.simulate_event(gpui::FileDropEvent::Entered { position: row_pos_a, paths: gpui::ExternalPaths(vec![file_a.clone(), file_b].into()) });
    vcx.simulate_event(gpui::FileDropEvent::Submit { position: row_pos_a });
    let (ev_a2, _output) = runtime.spawn(async move {
        loop {
            let ev = output.recv().await.unwrap();
            if matches!(&ev, EnvironmentEvent::Activated { id, .. } if id == "env-a") {
                return (ev, output);
            }
        }
    }).await.unwrap();
    state.update(vcx, |s, cx| s.apply_environment_event(ev_a2, cx));
    vcx.run_until_parked();
    assert_eq!(state.read_with(vcx, |s, _| s.environments.active.as_deref().map(str::to_owned)), Some("env-a".into()));
    assert_eq!(state.read_with(vcx, |s, _| s.selected_thread.clone()), Some("row-target".into()));
    assert_eq!(composer.read_with(vcx, |c, cx| c.snapshot_audit(0, cx)["regularAttachmentCount"].clone()), json!(2));

    // Failed navigation (unready/non-existent environment) clears pending drop immediately so opening the thread later won't attach stale files.
    let missing_key = t3_core::sidebar::tag_filter::ThreadKey::new("missing-env", "shared-id");
    state.update(vcx, |s, cx| {
        s.drop_files_on_sidebar_thread(missing_key, vec![file_a.clone()], cx);
        assert!(!s.has_pending_composer_file_drop(), "failed navigation must clear pending drop");
        // Cancel mid-flight via start_draft: also clears pending drop.
        s.drop_files_on_sidebar_thread(key_b, vec![file_a], cx);
        s.start_draft("proj-1", cx);
        assert!(!s.has_pending_composer_file_drop(), "leaving route must clear pending drop");
        s.stop_pump();
    });
}

#[gpui::test]
async fn omitted_thread_file_drop_selects_over_active_thread_or_draft_and_is_env_scoped(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, mut fixture) = setup_app(cx, &runtime);
    let tmp = TempDir::new("omitted-drop");
    let file_a = tmp.path().join("drop.txt");
    std::fs::write(&file_a, "omitted").unwrap();

    state.update(cx, |s, cx| {
        s.environments.active = Some("env-a".into());
        if let Some(cfg) = s.server_config.config.as_mut() {
            cfg.shell_thread_families = true;
            cfg.settings.hide_tagged_threads = true;
            cfg.settings.hidden_thread_tags = vec!["review".into()];
            cfg.environment.capabilities.file_attachments = Some(t3_core::server_config::FileAttachmentsCapability {
                max_upload_bytes: 1_000_000,
            });
        }
        let snap = s.shell.snapshot.as_mut().unwrap();
        snap.projects = vec![serde_json::from_value(json!({
            "id": "proj-1",
            "title": "App",
            "workspaceRoot": "/app",
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z"
        })).unwrap()];
        snap.threads = vec![
            serde_json::from_value(thread_json("act-1", "Active One", "idle", "2026-01-02T00:00:00.000Z")).unwrap(),
        ];
        s.rebuild_sidebar(cx);
        s.select_thread("act-1", cx);
        assert_eq!(s.selected_thread.as_deref(), Some("act-1"));
    });

    let (root, vcx) = cx.add_window_view(|window, cx| {
        let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
        let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
        let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
        let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer.clone(), menu.clone(), confirm, snooze, window, cx));
        DragHost { pane, composer, menu, height: px(600.) }
    });
    let composer = root.read_with(vcx, |host, _| host.composer.clone());
    vcx.run_until_parked();
    let _ = fixture.take_commands();
    assert_eq!(state.read_with(vcx, |s, _| s.selected_thread.clone()), Some("act-1".into()), "act-1 must stay open before drop");

    // 1) Drop onto an unloaded omitted thread while another thread (`act-1`) is already open.
    let (tx1, rx1) = tokio::sync::oneshot::channel();
    let mut tx1 = Some(tx1);
    let sub1 = state.update(vcx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == crate::app_state::AppEvent::Shell && state.omitted.mounts.generation_of(&t3_core::families::family_key("omitted-1")).is_some() {
            if let Some(tx) = tx1.take() { let _ = tx.send(()); }
        }
    }));
    state.update(vcx, |s, cx| {
        s.drop_files_on_sidebar_thread(t3_core::sidebar::tag_filter::ThreadKey::new("", "omitted-1"), vec![file_a.clone()], cx);
    });
    let ((method, payload, reply), mut fixture) = runtime.spawn(async move {
        let req = fixture.next_request().await;
        (req, fixture)
    }).await.unwrap();
    assert_eq!(method, t3_core::families::GET_THREAD_FAMILY);
    assert_eq!(payload, json!({"threadId": "omitted-1"}));
    reply.send(Ok(json!({
        "snapshotSequence": 2,
        "threads": [thread_json("omitted-1", "Omitted One", "idle", "2026-01-01T00:00:00.000Z")]
    }))).unwrap();
    rx1.await.unwrap();
    drop(sub1);
    vcx.run_until_parked();
    assert_eq!(state.read_with(vcx, |s, _| s.selected_thread.clone()), Some("omitted-1".into()), "omitted thread drop must select over active thread");
    assert!(!state.read_with(vcx, |s, _| s.has_pending_composer_file_drop()), "drop must be consumed");
    assert_eq!(composer.read_with(vcx, |c, cx| c.snapshot_audit(0, cx)["regularAttachmentCount"].clone()), json!(1));

    // 2) Drop onto another unloaded omitted thread while a draft is open.
    let (tx2, rx2) = tokio::sync::oneshot::channel();
    let mut tx2 = Some(tx2);
    let sub2 = state.update(vcx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
        if *event == crate::app_state::AppEvent::Shell && state.omitted.mounts.generation_of(&t3_core::families::family_key("omitted-2")).is_some() {
            if let Some(tx) = tx2.take() { let _ = tx.send(()); }
        }
    }));
    state.update(vcx, |s, cx| {
        s.start_draft("proj-1", cx);
        assert_eq!(s.draft_project.as_deref(), Some("proj-1"));
        s.drop_files_on_sidebar_thread(t3_core::sidebar::tag_filter::ThreadKey::new("", "omitted-2"), vec![file_a.clone()], cx);
    });
    assert_eq!(state.read_with(vcx, |s, _| s.draft_project.clone()), Some("proj-1".into()), "draft must stay open while family read is in flight");
    let ((method2, payload2, reply2), fixture) = runtime.spawn(async move {
        loop {
            let (m, p, r) = fixture.next_request().await;
            if m == t3_core::families::GET_THREAD_FAMILY {
                return ((m, p, r), fixture);
            }
        }
    }).await.unwrap();
    assert_eq!(method2, t3_core::families::GET_THREAD_FAMILY);
    assert_eq!(payload2, json!({"threadId": "omitted-2"}));
    reply2.send(Ok(json!({
        "snapshotSequence": 3,
        "threads": [thread_json("omitted-2", "Omitted Two", "idle", "2026-01-01T00:00:00.000Z")]
    }))).unwrap();
    rx2.await.unwrap();
    drop(sub2);
    vcx.run_until_parked();
    assert_eq!(state.read_with(vcx, |s, _| s.selected_thread.clone()), Some("omitted-2".into()), "omitted thread drop must select over open draft");
    assert_eq!(state.read_with(vcx, |s, _| s.draft_project.clone()), None);
    assert!(!state.read_with(vcx, |s, _| s.has_pending_composer_file_drop()));
    assert_eq!(composer.read_with(vcx, |c, cx| c.snapshot_audit(0, cx)["regularAttachmentCount"].clone()), json!(1));

    // 3) Environment-scoped clear must not wipe another environment's pending drop with the same raw thread_id.
    state.update(vcx, |s, _cx| {
        s.set_pending_composer_file_drop_for_test(t3_core::sidebar::tag_filter::ThreadKey::new("env-b", "shared-id"), vec![file_a]);
        s.clear_pending_composer_file_drop_for(Some("env-a"), "shared-id");
        assert!(s.has_pending_composer_file_drop(), "env-a clear must not wipe env-b pending drop");
        s.clear_pending_composer_file_drop_for(Some("env-b"), "shared-id");
        assert!(!s.has_pending_composer_file_drop(), "env-b clear must clear env-b pending drop");
        s.stop_pump();
    });
    drop(fixture);
}

#[gpui::test]
async fn pending_file_drop_is_consumed_when_target_thread_cannot_attach(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, _fixture) = setup_app(cx, &runtime);
    let tmp = TempDir::new("blocked-drop");
    let file_a = tmp.path().join("blocked.txt");
    std::fs::write(&file_a, "blocked").unwrap();

    state.update(cx, |s, cx| {
        if let Some(cfg) = s.server_config.config.as_mut() {
            cfg.environment.capabilities.file_attachments = Some(t3_core::server_config::FileAttachmentsCapability {
                max_upload_bytes: 1_000_000,
            });
        }
        let snap = s.shell.snapshot.as_mut().unwrap();
        snap.projects = vec![serde_json::from_value(json!({
            "id": "proj-1",
            "title": "App",
            "workspaceRoot": "/app",
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z"
        })).unwrap()];
        snap.threads = vec![
            serde_json::from_value(thread_json("th-blocked", "Blocked Thread", "idle", "2026-01-02T00:00:00.000Z")).unwrap(),
        ];
        s.rebuild_sidebar(cx);
        s.select_thread("th-blocked", cx);
        assert_eq!(s.selected_thread.as_deref(), Some("th-blocked"));
        let projection: t3_core::thread::ThreadProjection = serde_json::from_value(json!({
            "thread": {
                "id": "th-blocked",
                "projectId": "proj-1",
                "title": "Blocked Thread",
                "providerInstanceId": "inst-1",
                "modelSelection": {"instanceId": "inst-1", "model": "gpt-5"},
                "runtimeMode": "full-access",
                "interactionMode": "default",
                "lineage": {"rootThreadId": "th-blocked"},
                "createdAt": "2026-01-01T00:00:00.000Z",
                "updatedAt": "2026-01-02T00:00:00.000Z"
            },
            "runs": [],
            "attempts": [],
            "nodes": [],
            "subagents": [],
            "providerSessions": [],
            "providerThreads": [],
            "providerTurns": [],
            "runtimeRequests": [{
                "id": "req-1",
                "kind": "user_input",
                "status": "pending",
                "createdAt": "2026-01-02T00:00:00.000Z",
                "responseCapability": {"type": "form"}
            }],
            "messages": [],
            "plans": [],
            "turnItems": [{
                "id": "item-1",
                "threadId": "th-blocked",
                "runId": null,
                "nodeId": null,
                "parentItemId": null,
                "ordinal": 1,
                "status": "pending",
                "title": null,
                "startedAt": null,
                "completedAt": null,
                "updatedAt": "2026-01-02T00:00:00.000Z",
                "type": "user_input_request",
                "requestId": "req-1",
                "questions": [],
                "responseMode": "form"
            }],
            "checkpointScopes": [],
            "checkpoints": [],
            "contextHandoffs": [],
            "contextTransfers": [],
            "visibleTurnItems": [],
            "updatedAt": "2026-01-02T00:00:00.000Z"
        })).unwrap();
        s.thread.projection = Some(projection);
        cx.emit(crate::app_state::AppEvent::Thread);
    });

    let (root, vcx) = cx.add_window_view(|window, cx| {
        let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
        let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
        let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
        let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer.clone(), menu.clone(), confirm, snooze, window, cx));
        DragHost { pane, composer, menu, height: px(600.) }
    });
    let composer = root.read_with(vcx, |host, _| host.composer.clone());
    vcx.run_until_parked();

    // Drop files while `!can_attach(cx)` (pending user_input with no question attachment support).
    state.update(vcx, |s, cx| {
        s.drop_files_on_sidebar_thread(t3_core::sidebar::tag_filter::ThreadKey::new("", "th-blocked"), vec![file_a], cx);
    });
    vcx.run_until_parked();
    assert!(!state.read_with(vcx, |s, _| s.has_pending_composer_file_drop()), "unattachable drop must still be consumed on thread arrival");
    assert_eq!(composer.read_with(vcx, |c, cx| c.snapshot_audit(0, cx)["regularAttachmentCount"].clone()), json!(0));

    // Clearing pending user_inputs later must NOT retroactively attach the old consumed drop.
    state.update(vcx, |s, cx| {
        if let Some(proj) = s.thread.projection.as_mut() {
            proj.runtime_requests.clear();
            proj.turn_items.clear();
        }
        cx.emit(crate::app_state::AppEvent::Thread);
        s.stop_pump();
    });
    vcx.run_until_parked();
    assert_eq!(composer.read_with(vcx, |c, cx| c.snapshot_audit(0, cx)["regularAttachmentCount"].clone()), json!(0));
}
