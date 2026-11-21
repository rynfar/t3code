//! The thread terminal drawer's state: every thread's layout (persisted), the server's terminal
//! list from `subscribeTerminalMetadata`, and the panes of the selected thread. Ports
//! `PersistentThreadTerminalDrawer` (`components/PersistentThreadTerminals.tsx`),
//! `components/terminal/useThreadTerminalActions.ts`, and the toggle, new, split and close
//! handlers of `components/chat/useChatTerminalActions.ts`. Creating and closing terminals
//! needs `terminal:operate`; the drawer follows the device's grants as they change.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext, Context, Entity, Focusable, PromptLevel, Subscription, Task, Window};
use serde_json::json;
use t3_client::session::RpcError;
use t3_client::store;
use t3_core::composer::context::TerminalSelection;
use t3_core::composer::drafts::DraftRef;
use t3_core::files::metadata::MetadataCache;
use t3_core::terminal::{
    self, LaunchLocation, MetadataEvent, SnapshotId, SplitDirection, TerminalAccess, TerminalFocusQueue, TerminalLayouts, TerminalSummary,
    ThreadEnterFocusDecision, ThreadTerminalLayout, method,
};
use tokio::sync::mpsc;

use super::pane::{PaneEvent, TerminalPane};
use super::session::Target;
use crate::app_state::{AppEvent, AppState, Phase};

const SAVE_DELAY: Duration = Duration::from_millis(300);
/// The web keeps this in `localStorage` as `t3code:terminal-state:v1`.
const STATE_NAME: &str = "terminal-state";
const AUTHORIZATION_ERROR: &str = "EnvironmentAuthorizationError";

/// Each thread's terminals with a running process on the current connection, in metadata order
/// (`useThreadRunningTerminalIds`), for the sidebar's terminal indicator.
#[derive(Default)]
pub struct RunningTerminals {
    pub by_thread: std::collections::BTreeMap<String, Vec<String>>,
}

impl gpui::Global for RunningTerminals {}

/// The workspace's live terminal drawer, so `runProjectScript` (`GT-SCRIPTS`) and right-panel
/// terminal tabs can share metadata, grants, ID allocation, and focus tracking without holding a `Window`.
struct ActiveDrawer(gpui::WeakEntity<TerminalDrawer>);

impl gpui::Global for ActiveDrawer {}

/// Shared pending-focus queue (`apps/web/src/lib/terminalFocus.ts`) coordinating drawer and
/// right-panel terminal viewports.
#[derive(Default)]
struct FocusQueue(TerminalFocusQueue);

impl gpui::Global for FocusQueue {}

pub(crate) fn active_drawer(cx: &App) -> Option<Entity<TerminalDrawer>> {
    cx.try_global::<ActiveDrawer>().and_then(|active| active.0.upgrade())
}

pub(crate) fn request_terminal_focus(thread_id: &str, terminal_id: &str, cx: &mut App) {
    cx.default_global::<FocusQueue>().0.request(thread_id, terminal_id);
}

pub(crate) fn request_terminal_focus_on_navigation(thread_id: &str, terminal_id: &str, cx: &mut App) {
    cx.default_global::<FocusQueue>().0.request_on_navigation(thread_id, terminal_id);
}

pub(crate) fn cancel_terminal_focus(thread_id: &str, cx: &mut App) {
    if cx.has_global::<FocusQueue>() {
        cx.global_mut::<FocusQueue>().0.cancel(thread_id);
    }
}

pub(crate) fn consume_terminal_focus(thread_id: &str, terminal_id: &str, visible: bool, can_operate: bool, cx: &mut App) -> bool {
    cx.has_global::<FocusQueue>()
        && cx.global_mut::<FocusQueue>().0.consume_if_ready(thread_id, terminal_id, visible, can_operate)
}

pub(crate) fn record_terminal_focus(thread_id: &str, terminal_id: &str, cx: &mut App) {
    cx.default_global::<FocusQueue>().0.record_terminal_focus(thread_id, terminal_id);
}

/// `focusPanelTerminalAfterClose` (`apps/web/src/lib/terminalFocus.ts:107-116`): after a right
/// panel surface closes, focuses a newly exposed terminal surface; a terminal surface that was
/// already active keeps its focus as is.
pub(crate) fn focus_panel_terminal_after_close(
    state: &AppState,
    thread_id: &str,
    previous_active_surface_id: Option<&str>,
    cx: &mut App,
) {
    let panel = state.panels.panels.panel(thread_id);
    if !panel.is_open || panel.active_surface_id.as_deref() == previous_active_surface_id {
        return;
    }
    if let Some(t3_core::right_panel::SurfaceKind::Terminal { active_terminal_id, .. }) =
        panel.active().map(|surface| &surface.kind)
    {
        request_terminal_focus(thread_id, active_terminal_id, cx);
    }
}

/// Label for a terminal tab or pane (`session_label`), reading server metadata when available.
pub(crate) fn terminal_label(thread_id: &str, terminal_id: &str, cx: &App) -> String {
    cx.try_global::<ActiveDrawer>()
        .and_then(|active| active.0.upgrade())
        .map(|drawer| drawer.read(cx).label(thread_id, terminal_id))
        .unwrap_or_else(|| terminal::session_label(terminal_id, None))
}

pub(crate) fn can_operate_terminals(cx: &App) -> bool {
    cx.try_global::<ActiveDrawer>()
        .and_then(|active| active.0.upgrade())
        .is_some_and(|drawer| drawer.read(cx).can_operate())
}

pub(crate) fn link_metadata_cache(cx: &App) -> Option<SharedMetadataCache> {
    cx.try_global::<ActiveDrawer>()
        .and_then(|active| active.0.upgrade())
        .map(|drawer| drawer.read(cx).link_metadata.clone())
}

pub(crate) fn terminal_location(state: &AppState, thread_id: &str, terminal_id: Option<&str>, cx: &App) -> Option<LaunchLocation> {
    if let Some(drawer) = cx.try_global::<ActiveDrawer>().and_then(|active| active.0.upgrade()) {
        return drawer.read(cx).location_with_state(state, thread_id, terminal_id);
    }
    let (root, worktree) = terminal_workspace(state, thread_id)?;
    Some(LaunchLocation::new(root, worktree))
}

fn terminal_workspace<'a>(state: &'a AppState, owner: &str) -> Option<(&'a str, Option<&'a str>)> {
    if AppState::is_utility_panel_key(owner) {
        return None;
    }
    if let Some(thread) = state.thread_shell(owner) {
        return Some((&state.shell.project(&thread.project_id)?.workspace_root, thread.worktree_path.as_deref()));
    }
    let draft = DraftRef::from_key(owner)?;
    if draft.environment_id.as_deref().is_some_and(|environment| state.environments.active.as_deref() != Some(environment)) {
        return None;
    }
    let root = &state.shell.project(&draft.project_id)?.workspace_root;
    (!root.is_empty()).then_some((root, None))
}

fn legacy_terminal_owner(draft: &DraftRef, summaries: &[TerminalSummary]) -> Option<String> {
    if draft.id != DraftRef::legacy(&draft.project_id).id {
        return None;
    }
    let legacy = format!("draft:{}", draft.project_id);
    summaries.iter().any(|terminal| {
        terminal.thread_id == legacy
            && matches!(terminal.status, terminal::SessionStatus::Starting | terminal::SessionStatus::Running)
    }).then_some(legacy)
}

pub(crate) fn allocate_panel_terminal_id(state: &AppState, thread_id: &str, cx: &App) -> String {
    if let Some(drawer) = cx.try_global::<ActiveDrawer>().and_then(|active| active.0.upgrade()) {
        return drawer.read(cx).allocate_id_with_state(state, thread_id);
    }
    let panel_ids = state.panels.panels.panel_terminal_ids(thread_id);
    let suffix = uuid::Uuid::new_v4().to_string();
    terminal::next_terminal_id(panel_ids.iter().map(String::as_str), Some(&suffix))
}

/// Launches a PTY session for a right-panel terminal tab (`terminal.open`).
pub(crate) fn launch_panel_terminal(state: &AppState, thread_id: &str, terminal_id: &str, cx: &App) -> Option<LaunchLocation> {
    let location = terminal_location(state, thread_id, None, cx)?;
    let client = state.client_handle()?;
    let open = client.request(method::OPEN, terminal::open_input(thread_id, terminal_id, &location, None));
    state.runtime().spawn(open);
    Some(location)
}

/// Ends a terminal session and deletes its history; a failed close falls back to typing `exit`.
pub(crate) fn close_terminal_session(state: &AppState, thread_id: &str, terminal_id: &str) {
    if let Some(client) = state.client_handle() {
        let (thread_id, terminal_id) = (thread_id.to_owned(), terminal_id.to_owned());
        state.runtime().spawn(async move {
            let closed = client.request(method::CLOSE, terminal::close_input(&thread_id, &terminal_id, true)).await;
            if let Err(RpcError::Failure(_)) = closed {
                let _ = client.request(method::WRITE, terminal::write_input(&thread_id, &terminal_id, "exit\n")).await;
            }
        });
    }
}

/// Opens and focuses `terminal_id` in `thread_id`'s terminal drawer (`useChatTerminalActions.ts`).
pub fn open_script_terminal(thread_id: String, terminal_id: String, cx: &mut App) {
    let Some(drawer) = cx.try_global::<ActiveDrawer>().and_then(|active| active.0.upgrade()) else {
        return;
    };
    if let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) {
        let _ = window.update(cx, |_, window, cx| {
            drawer.update(cx, |drawer, cx| drawer.open_terminal_for_thread(&thread_id, &terminal_id, window, cx));
        });
    } else {
        drawer.update(cx, |drawer, cx| {
            drawer.layouts.ensure(&thread_id, &terminal_id, true, Some(true));
            drawer.schedule_save(cx);
            cx.notify();
        });
    }
}

pub(super) type SharedMetadataCache = Rc<RefCell<(Instant, MetadataCache)>>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DrawerAnimation {
    pub(super) seq: u64,
    pub(super) duration_ms: u32,
    pub(super) opening: bool,
    pub(super) from_height: f32,
}

pub struct TerminalDrawer {
    pub(super) app: Entity<AppState>,
    pub(super) layouts: TerminalLayouts,
    summaries: Option<Vec<TerminalSummary>>,
    legacy_closes: HashMap<String, u64>,
    metadata_snapshot_id: Option<SnapshotId>,
    ephemeral_snapshot_seq: u64,
    panes: HashMap<String, Entity<TerminalPane>>,
    panes_thread: Option<String>,
    link_metadata: SharedMetadataCache,
    metadata: Option<(tokio::task::JoinHandle<()>, Task<()>)>,
    /// The connection `metadata`, `grants`, and the panes belong to.
    connection: Option<u64>,
    access: TerminalAccess,
    grants: Option<Task<()>>,
    save: Option<Task<()>>,
    pub(super) resize_from: Option<(f32, f32)>,
    /// Tracks `(thread_id, is_open)` across frames so `panelAnimationDurationMs` animates
    /// same-thread drawer open/close transitions without animating thread switches (`ST-APPEARANCE`).
    pub(super) last_open_state: Option<(String, bool)>,
    pub(super) last_rendered_height: f32,
    pub(super) drawer_animation: Option<DrawerAnimation>,
    pub(super) animation_seq: u64,
    pub(super) _close_animation_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Drop for TerminalDrawer {
    fn drop(&mut self) {
        if let Some((driver, _)) = self.metadata.take() {
            driver.abort();
        }
    }
}

impl TerminalDrawer {
    pub fn new(app: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let layouts = TerminalLayouts::from_saved(load_layouts());
        let subscription = cx.subscribe_in(&app, window, |drawer, _, event: &AppEvent, window, cx| match event {
            AppEvent::Phase => drawer.connection_changed(window, cx),
            AppEvent::Selection | AppEvent::Shell | AppEvent::RightPanel => drawer.sync_selection(window, cx),
            _ => {}
        });
        let mut drawer = TerminalDrawer {
            app,
            layouts,
            summaries: None,
            legacy_closes: HashMap::new(),
            metadata_snapshot_id: None,
            ephemeral_snapshot_seq: 0,
            panes: HashMap::new(),
            panes_thread: None,
            link_metadata: Rc::new(RefCell::new((Instant::now(), MetadataCache::default()))),
            metadata: None,
            connection: None,
            access: TerminalAccess::default(),
            grants: None,
            save: None,
            resize_from: None,
            last_open_state: None,
            last_rendered_height: terminal::DEFAULT_DRAWER_HEIGHT,
            drawer_animation: None,
            animation_seq: 0,
            _close_animation_task: None,
            _subscriptions: vec![subscription],
        };
        let weak = cx.entity().downgrade();
        cx.set_global(ActiveDrawer(weak));
        drawer.connection_changed(window, cx);
        drawer
    }

    pub(crate) fn has_saved_layout(key: &str, cx: &App) -> bool {
        cx.try_global::<ActiveDrawer>()
            .and_then(|active| active.0.upgrade())
            .is_some_and(|drawer| drawer.read(cx).layouts.saved().contains_key(key))
    }

    pub(crate) fn is_active(cx: &App) -> bool {
        active_drawer(cx).is_some()
    }

    /// `remember` (`apps/web/src/lib/terminalFocus.ts`): records that the composer holds focus for
    /// `thread_id` (and cancels any pending terminal focus for it) so returning to the thread
    /// restores composer focus even when its terminal drawer or panel is open.
    pub(crate) fn record_composer_focus(thread_id: &str, cx: &mut App) {
        cx.default_global::<FocusQueue>().0.record_composer_focus(thread_id);
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn last_focus_for_test(thread_id: &str, cx: &App) -> Option<t3_core::terminal::LastThreadFocus> {
        cx.try_global::<FocusQueue>().and_then(|q| q.0.last_focus(thread_id).cloned())
    }

    pub(super) fn selected_thread(&self, cx: &App) -> Option<String> {
        let state = self.app.read(cx);
        state
            .selected_thread
            .clone()
            .or_else(|| state.draft_route_key())
    }

    pub(super) fn layout(&self, cx: &App) -> Option<(String, ThreadTerminalLayout)> {
        let thread_id = self.selected_thread(cx)?;
        let layout = self.layouts.get(&thread_id).normalized();
        Some((thread_id, layout))
    }

    pub fn is_open(&self, cx: &App) -> bool {
        self.layout(cx).is_some_and(|(_, layout)| layout.terminal_open)
    }

    pub fn visible_height(&self, viewport_height: f32, cx: &App) -> f32 {
        match self.layout(cx) {
            Some((_, layout)) if layout.terminal_open => terminal::clamp_drawer_height(layout.terminal_height, viewport_height),
            _ => 0.,
        }
    }

    /// Follows the current connection; switching environments drops everything the last one
    /// started, which still holds its client.
    fn connection_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.app.read(cx);
        let current = if matches!(state.phase, Phase::Online(_)) { state.connection_id() } else { None };
        if current != self.connection {
            if let Some((driver, _)) = self.metadata.take() {
                driver.abort();
            }
            self.grants = None;
            self.access = TerminalAccess::default();
            self.summaries = None;
            self.legacy_closes.clear();
            self.metadata_snapshot_id = None;
            self.panes.clear();
            self.panes_thread = None;
            *self.link_metadata.borrow_mut() = (Instant::now(), MetadataCache::default());
            self.connection = current;
            self.publish_running(cx);
            cx.notify();
        }
        if current.is_none() {
            return;
        }
        if self.metadata.is_none()
            && let Some(client) = self.app.read(cx).client_handle()
        {
            self.watch_grants(&client, window, cx);
            self.start_metadata(client, window, cx);
        }
        self.sync_selection(window, cx);
    }

    pub(super) fn can_operate(&self) -> bool {
        self.access.operate
    }

    /// Follows this device's grants; the last answer holds while reconnecting.
    fn watch_grants(&mut self, client: &t3_client::supervisor::ClientHandle, window: &mut Window, cx: &mut Context<Self>) {
        let mut grants = client.session_state();
        self.access = TerminalAccess::of(grants.borrow_and_update().as_ref());
        self.grants = Some(cx.spawn_in(window, async move |this, cx| {
            while grants.changed().await.is_ok() {
                let access = TerminalAccess::of(grants.borrow_and_update().as_ref());
                let updated = this.update_in(cx, |drawer, window, cx| {
                    if drawer.access != access {
                        let gained_operate = !drawer.access.operate && access.operate;
                        drawer.access = access;
                        if gained_operate {
                            drawer.close_legacy_draft_terminals(cx);
                            drawer.ensure_panes(window, cx);
                        }
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    return;
                }
            }
        }));
    }

    fn start_metadata(&mut self, client: t3_client::supervisor::ClientHandle, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, mut rx) = mpsc::unbounded_channel::<MetadataEvent>();
        let driver = self.app.read(cx).runtime().spawn(async move {
            let mut attempt = 0u32;
            loop {
                match client.stream(method::SUBSCRIBE_METADATA, json!({})).await {
                    Ok(mut stream) => {
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(value) => {
                                    attempt = 0;
                                    match serde_json::from_value::<MetadataEvent>(value) {
                                        Ok(event) => {
                                            if tx.send(event).is_err() {
                                                return;
                                            }
                                        }
                                        Err(error) => tracing::warn!(%error, "undecodable terminal metadata"),
                                    }
                                }
                                Err(RpcError::Failure(failure)) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => return,
                                Err(_) => break,
                            }
                        }
                    }
                    Err(RpcError::Failure(failure)) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => return,
                    Err(_) => {}
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_millis((250u64 << attempt.min(5)).min(5_000))).await;
            }
        });
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(event) = rx.try_recv() {
                    batch.push(event);
                }
                let applied = this.update_in(cx, |drawer, window, cx| {
                    let summaries = drawer.summaries.get_or_insert_with(Vec::new);
                    for event in batch {
                        if let MetadataEvent::Snapshot { manager_id, .. } = &event {
                            drawer.metadata_snapshot_id = Some(match manager_id {
                                Some(id) => SnapshotId::Manager(id.clone()),
                                None => {
                                    drawer.ephemeral_snapshot_seq = drawer.ephemeral_snapshot_seq.wrapping_add(1);
                                    SnapshotId::Ephemeral(drawer.ephemeral_snapshot_seq)
                                }
                            });
                        }
                        terminal::apply_metadata(summaries, event);
                    }
                    drawer.publish_running(cx);
                    drawer.sync_selection(window, cx);
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        self.metadata = Some((driver, pump));
    }

    /// Panes belong to the selected thread and attach as soon as their group is active, drawn
    /// or not, as the web keeps the active thread's drawer mounted while it is closed.
    fn sync_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_thread(cx);
        if self.panes_thread != selected {
            self.panes.clear();
            let mut cache = self.link_metadata.borrow_mut();
            let previous = self.panes_thread.take();
            if let Some(previous) = &previous {
                cache.1.release_thread(previous);
            }
            if let Some(thread_id) = &selected {
                cache.1.retain_thread(thread_id);
            }
            drop(cache);
            if let Some(previous) = &previous {
                cx.default_global::<FocusQueue>().0.leave_thread(previous);
            }
            self.panes_thread = selected.clone();
            if let Some(thread_id) = &selected {
                let layout = self.layouts.get(thread_id).normalized();
                let active_group_ids = layout.visible_terminal_ids();
                let active_drawer_tid = (!layout.active_terminal_id.is_empty()).then_some(layout.active_terminal_id.as_str());
                let panel_tids: Option<Vec<String>> = self
                    .app
                    .read(cx)
                    .panels
                    .panels
                    .active_surface(thread_id)
                    .and_then(|surface| match &surface.kind {
                        t3_core::right_panel::SurfaceKind::Terminal { terminal_ids, .. } => Some(terminal_ids.clone()),
                        _ => None,
                    });
                let decision = cx.default_global::<FocusQueue>().0.enter_thread(
                    thread_id,
                    layout.terminal_open,
                    &active_group_ids,
                    active_drawer_tid,
                    panel_tids.as_deref(),
                );
                if matches!(decision, ThreadEnterFocusDecision::FocusComposer) {
                    cancel_terminal_focus(thread_id, cx);
                    crate::composer::focus_for_thread(thread_id, window, cx);
                }
            }
        }
        self.reconcile(cx);
        self.close_legacy_draft_terminals(cx);
        self.ensure_panes(window, cx);
        cx.notify();
    }

    fn close_legacy_draft_terminals(&mut self, cx: &App) {
        if !self.access.operate {
            return;
        }
        let app = self.app.read(cx);
        let Some(draft) = app.active_draft() else { return };
        if terminal_workspace(app, &draft.key()).is_none() {
            return;
        }
        let Some(legacy) = self.summaries.as_deref().and_then(|summaries| legacy_terminal_owner(&draft, summaries)) else { return };
        let Some(client) = app.client_handle() else { return };
        let generation = *client.connection_generation().borrow();
        if self.legacy_closes.insert(legacy.clone(), generation) == Some(generation) {
            return;
        }
        app.runtime().spawn(async move {
            if let Err(error) = client.request(method::CLOSE, json!({ "threadId": legacy })).await {
                tracing::warn!(%error, "could not close legacy draft terminals");
            }
        });
    }

    fn ensure_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread_id, layout)) = self.layout(cx) else { return };
        let visible_ids = layout.visible_terminal_ids();
        for terminal_id in &visible_ids {
            self.pane(&thread_id, terminal_id, window, cx);
        }
        let can_operate = self.access.operate;
        let mut focus_target = None;
        for (terminal_id, pane) in &self.panes {
            let visible = layout.terminal_open && visible_ids.iter().any(|id| id == terminal_id);
            pane.update(cx, |pane, cx| pane.set_visible(visible, cx));
            if visible && consume_terminal_focus(&thread_id, terminal_id, true, can_operate, cx) {
                focus_target = Some(pane.clone());
            }
        }
        if let Some(pane) = focus_target {
            window.focus(&pane.focus_handle(cx), cx);
        }
    }

    /// Adopts the server's terminal list for the selected thread (the drawer's reconcile effect),
    /// excluding terminals currently owned by right-panel terminal tabs
    /// (`PersistentThreadTerminals.tsx`), dropping background terminals closed by another client
    /// while keeping reopened/newly placed terminals (`pingdotgg/t3code#38998`, `pingdotgg/t3code#39260`).
    fn reconcile(&mut self, cx: &mut Context<Self>) {
        let (Some(thread_id), Some(summaries), Some(snapshot_id)) =
            (self.selected_thread(cx), self.summaries.as_ref(), self.metadata_snapshot_id.clone())
        else {
            return;
        };
        let panel_ids = self.app.read(cx).panels.panels.panel_terminal_ids(&thread_id);
        let server_ids: Vec<String> = terminal::thread_terminals(summaries, &thread_id)
            .into_iter()
            .filter(|t| !panel_ids.contains(&t.terminal_id))
            .map(|t| t.terminal_id.clone())
            .collect();
        if self.layouts.reconcile_with_snapshot(&thread_id, &server_ids, &snapshot_id) {
            self.drop_stale_panes(&thread_id);
            self.schedule_save(cx);
        }
    }

    fn drop_stale_panes(&mut self, thread_id: &str) {
        let ids = self.layouts.get(thread_id).terminal_ids;
        self.panes.retain(|terminal_id, _| ids.contains(terminal_id));
    }

    /// Publishes the current connection's running terminals; observers wake only on a change.
    fn publish_running(&self, cx: &mut App) {
        let by_thread = self.summaries.as_deref().map(terminal::running_by_thread).unwrap_or_default();
        if cx.try_global::<RunningTerminals>().is_none_or(|current| current.by_thread != by_thread) {
            cx.set_global(RunningTerminals { by_thread });
        }
    }

    pub(super) fn summary(&self, thread_id: &str, terminal_id: &str) -> Option<&TerminalSummary> {
        self.summaries.as_ref()?.iter().find(|t| t.thread_id == thread_id && t.terminal_id == terminal_id)
    }

    pub(super) fn label(&self, thread_id: &str, terminal_id: &str) -> String {
        terminal::session_label(terminal_id, self.summary(thread_id, terminal_id).map(|t| t.label.as_str()))
    }

    /// Where a terminal runs: its server session's cwd when known (reopening elsewhere would
    /// restart it), otherwise the thread's worktree or the thread/draft's project root.
    fn location(&self, thread_id: &str, terminal_id: Option<&str>, cx: &App) -> Option<LaunchLocation> {
        self.location_with_state(self.app.read(cx), thread_id, terminal_id)
    }

    fn location_with_state(&self, app: &AppState, thread_id: &str, terminal_id: Option<&str>) -> Option<LaunchLocation> {
        let (root, thread_worktree) = terminal_workspace(app, thread_id)?;
        let known = terminal_id.and_then(|id| self.summary(thread_id, id)).filter(|summary| !summary.cwd.is_empty());
        Some(match known {
            Some(summary) => LaunchLocation {
                cwd: summary.cwd.clone(),
                ..LaunchLocation::new(root, summary.worktree_path.as_deref())
            },
            None => LaunchLocation::new(root, thread_worktree),
        })
    }

    pub(super) fn has_project(&self, cx: &App) -> bool {
        self.selected_thread(cx).and_then(|thread_id| self.location(&thread_id, None, cx)).is_some()
    }

    pub(super) fn pane(&mut self, thread_id: &str, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TerminalPane>> {
        if self.panes_thread.as_deref() != Some(thread_id) {
            return None;
        }
        if let Some(pane) = self.panes.get(terminal_id) {
            return Some(pane.clone());
        }
        let location = self.location(thread_id, Some(terminal_id), cx)?;
        let client = self.app.read(cx).client_handle()?;
        let runtime = self.app.read(cx).runtime().clone();
        let target = Target { thread_id: thread_id.to_owned(), terminal_id: terminal_id.to_owned(), location };
        let app = self.app.clone();
        let link_metadata = self.link_metadata.clone();
        let pane = cx.new(|cx| {
            let mut pane = TerminalPane::new(target, client, runtime, window, cx);
            pane.set_link_context(app, link_metadata);
            pane
        });
        let (thread, terminal) = (thread_id.to_owned(), terminal_id.to_owned());
        cx.subscribe_in(&pane, window, move |drawer, _, event: &PaneEvent, window, cx| match event {
            PaneEvent::Exited => drawer.close_terminal(&thread, &terminal, window, cx),
            PaneEvent::Activated => {
                record_terminal_focus(&thread, &terminal, cx);
                drawer.activate(&terminal, window, cx);
            }
            PaneEvent::AddToChat(excerpt) => drawer.add_to_chat(&thread, &terminal, excerpt.clone(), cx),
        })
        .detach();
        self.panes.insert(terminal_id.to_owned(), pane.clone());
        Some(pane)
    }

    /// `handleAddTerminalContext`: names the excerpt after its terminal and hands it to the
    /// thread's composer.
    fn add_to_chat(&mut self, thread_id: &str, terminal_id: &str, excerpt: super::pane::Excerpt, cx: &mut Context<Self>) {
        let selection = TerminalSelection {
            terminal_id: terminal_id.to_owned(),
            terminal_label: self.label(thread_id, terminal_id),
            line_start: excerpt.line_start,
            line_end: excerpt.line_end,
            text: excerpt.text,
        };
        self.app.update(cx, |state, cx| state.add_terminal_context(thread_id, selection, cx));
    }

    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread_id, layout)) = self.layout(cx) else { return };
        if !layout.terminal_open || layout.active_terminal_id.is_empty() {
            cancel_terminal_focus(&thread_id, cx);
            return;
        }
        request_terminal_focus(&thread_id, &layout.active_terminal_id, cx);
        let can_operate = self.access.operate;
        if let Some(pane) = self.pane(&thread_id, &layout.active_terminal_id, window, cx)
            && consume_terminal_focus(&thread_id, &layout.active_terminal_id, true, can_operate, cx)
        {
            window.focus(&pane.focus_handle(cx), cx);
        }
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(thread_id) = self.selected_thread(cx) {
            self.drop_stale_panes(&thread_id);
        }
        self.ensure_panes(window, cx);
        self.focus_active(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = this.read_with(cx, |drawer, _| store::save_ui_state(STATE_NAME, drawer.layouts.saved()));
        }));
    }

    /// `terminal.toggle`. Opening an empty drawer reuses a terminal the server already runs for
    /// the thread before starting a new shell.
    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread_id, layout)) = self.layout(cx) else { return };
        let was_open = layout.terminal_open;
        let opening = !was_open;
        if opening && !self.access.read && !self.access.operate {
            return;
        }
        if opening && self.access.operate && layout.terminal_ids.is_empty() {
            let existing = self.summaries.as_ref().and_then(|summaries| terminal::pick_existing_terminal(&terminal::thread_terminals(summaries, &thread_id)));
            match existing {
                Some(terminal_id) => {
                    self.layouts.ensure(&thread_id, &terminal_id, true, Some(true));
                }
                None => {
                    self.launch(|layouts, thread_id, terminal_id| layouts.ensure(thread_id, terminal_id, true, None), cx);
                }
            }
        } else {
            self.layouts.set_open(&thread_id, !layout.terminal_open);
        }
        self.changed(window, cx);
        if was_open && !self.is_open(cx) {
            crate::composer::focus_for_thread(&thread_id, window, cx);
        }
    }

    /// Opens and focuses one of the selected thread's terminals, such as the worktree setup
    /// script's (`openSetupTerminal`).
    pub fn open_terminal(&mut self, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.selected_thread(cx) else { return };
        self.open_terminal_for_thread(&thread_id, terminal_id, window, cx);
    }

    /// Opens `terminal_id` in `thread_id`'s drawer layout and focuses it when `thread_id` is open.
    pub fn open_terminal_for_thread(&mut self, thread_id: &str, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.layouts.ensure(thread_id, terminal_id, true, Some(true));
        request_terminal_focus_on_navigation(thread_id, terminal_id, cx);
        if self.selected_thread(cx).as_deref() == Some(thread_id) {
            self.changed(window, cx);
        } else {
            self.schedule_save(cx);
            cx.notify();
        }
    }

    fn open_if_closed(&mut self, cx: &App) {
        if let Some((thread_id, layout)) = self.layout(cx)
            && !layout.terminal_open
        {
            self.layouts.set_open(&thread_id, true);
        }
    }

    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.access.operate {
            return;
        }
        self.open_if_closed(cx);
        self.launch(|layouts, thread_id, terminal_id| layouts.new_terminal(thread_id, terminal_id), cx);
        self.changed(window, cx);
    }

    pub fn split(&mut self, direction: SplitDirection, window: &mut Window, cx: &mut Context<Self>) {
        if !self.access.operate {
            return;
        }
        self.open_if_closed(cx);
        if self.layout(cx).is_none_or(|(_, layout)| layout.has_reached_split_limit()) {
            return;
        }
        self.launch(move |layouts, thread_id, terminal_id| layouts.split(thread_id, terminal_id, direction), cx);
        self.changed(window, cx);
    }

    /// Allocates an id, places it, and opens the shell (`launchTerminal`). The pane attaches
    /// with the same launch location, so the two never disagree about where it runs.
    fn launch(&mut self, place: impl FnOnce(&mut TerminalLayouts, &str, &str) -> bool, cx: &mut Context<Self>) -> Option<String> {
        if !self.access.operate {
            return None;
        }
        let thread_id = self.selected_thread(cx)?;
        let location = self.location(&thread_id, None, cx)?;
        let client = self.app.read(cx).client_handle()?;
        let terminal_id = self.allocate_id(&thread_id, cx);
        place(&mut self.layouts, &thread_id, &terminal_id);
        let open = client.request(method::OPEN, terminal::open_input(&thread_id, &terminal_id, &location, None));
        self.app.read(cx).runtime().spawn(open);
        Some(terminal_id)
    }

    /// The lowest free `term-N` across server sessions, drawer layouts, and right-panel terminal
    /// tabs; without the server's list, suffixed so it cannot collide with an unseen session.
    pub(super) fn allocate_id(&self, thread_id: &str, cx: &App) -> String {
        self.allocate_id_with_state(self.app.read(cx), thread_id)
    }

    fn allocate_id_with_state(&self, state: &AppState, thread_id: &str) -> String {
        let layout_ids = self.layouts.get(thread_id).terminal_ids;
        let panel_ids = state.panels.panels.panel_terminal_ids(thread_id);
        let server_ids: Vec<&str> = self
            .summaries
            .as_ref()
            .map(|summaries| summaries.iter().filter(|t| t.thread_id == thread_id).map(|t| t.terminal_id.as_str()).collect())
            .unwrap_or_default();
        let suffix = self.summaries.is_none().then(|| uuid::Uuid::new_v4().to_string());
        terminal::next_terminal_id(
            server_ids
                .into_iter()
                .chain(layout_ids.iter().map(String::as_str))
                .chain(panel_ids.iter().map(String::as_str)),
            suffix.as_deref(),
        )
    }

    pub fn activate(&mut self, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.selected_thread(cx) else { return };
        if self.layouts.set_active(&thread_id, terminal_id) {
            self.changed(window, cx);
        }
    }

    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, layout)) = self.layout(cx).filter(|(_, layout)| layout.terminal_open && !layout.active_terminal_id.is_empty()) {
            self.request_close(&layout.active_terminal_id, window, cx);
        }
    }

    /// Asks first when `confirmTerminalClose` is enabled (`pingdotgg/t3code#35305`).
    pub fn request_close(&mut self, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.selected_thread(cx) else { return };
        if !self.access.operate {
            return;
        }
        if !crate::settings::store::try_client(cx).is_none_or(|doc| doc.confirm_terminal_close()) {
            self.close_terminal(&thread_id, terminal_id, window, cx);
            return;
        }
        let (message, detail) = terminal::close_confirmation(&[self.label(&thread_id, terminal_id)]);
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &["Close", "Cancel"], cx);
        let terminal_id = terminal_id.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |drawer, window, cx| drawer.close_terminal(&thread_id, &terminal_id, window, cx));
            }
        })
        .detach();
    }

    /// Ends the session and deletes its history; a failed close falls back to typing `exit`.
    fn close_terminal(&mut self, thread_id: &str, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(client) = self.app.read(cx).client_handle() {
            let (thread_id, terminal_id) = (thread_id.to_owned(), terminal_id.to_owned());
            self.app.read(cx).runtime().spawn(async move {
                let closed = client.request(method::CLOSE, terminal::close_input(&thread_id, &terminal_id, true)).await;
                if let Err(RpcError::Failure(_)) = closed {
                    let _ = client.request(method::WRITE, terminal::write_input(&thread_id, &terminal_id, "exit\n")).await;
                }
            });
        }
        let was_open = self.layouts.get(thread_id).normalized().terminal_open;
        self.layouts.close(thread_id, terminal_id);
        self.panes.remove(terminal_id);
        self.changed(window, cx);
        if was_open && self.selected_thread(cx).as_deref() == Some(thread_id) && !self.is_open(cx) {
            crate::composer::focus_for_thread(thread_id, window, cx);
        }
    }

    pub(super) fn begin_resize(&mut self, pointer_y: f32, height: f32) {
        self.resize_from = Some((pointer_y, height));
    }

    pub(super) fn resize_to(&mut self, pointer_y: f32, viewport_height: f32, cx: &mut Context<Self>) {
        let (Some((start_y, start_height)), Some(thread_id)) = (self.resize_from, self.selected_thread(cx)) else { return };
        let height = terminal::clamp_drawer_height(start_height + (start_y - pointer_y), viewport_height);
        if self.layouts.set_height(&thread_id, height) {
            self.schedule_save(cx);
            cx.notify();
        }
    }
}

/// Each thread decodes on its own, so one unreadable entry never costs the others.
fn load_layouts() -> HashMap<String, ThreadTerminalLayout> {
    let Some(serde_json::Value::Object(entries)) = store::load_ui_state(STATE_NAME) else { return HashMap::new() };
    entries.into_iter().filter_map(|(thread_id, value)| Some((thread_id, serde_json::from_value(value).ok()?))).collect()
}

#[cfg(test)]
#[test]
fn legacy_terminal_cleanup_only_targets_live_sessions_of_the_migrated_project() {
    let draft = DraftRef::legacy("project:1");
    let other_draft = DraftRef::legacy("project:2");
    let fresh = DraftRef::new(t3_core::composer::drafts::DraftId::new("fresh".into()), "", "project:1");
    let summary = |thread_id: &str, status: &str| {
        serde_json::from_value::<TerminalSummary>(json!({
            "threadId": thread_id, "terminalId": "default", "status": status,
        })).unwrap()
    };
    let mut summaries = vec![
        summary(&draft.key(), "running"),
        summary("normal-thread", "running"),
        summary("draft:project:1", "exited"),
    ];
    assert!(legacy_terminal_owner(&draft, &summaries).is_none());
    summaries.push(summary("draft:project:1", "running"));
    assert!(legacy_terminal_owner(&fresh, &summaries).is_none());
    assert!(legacy_terminal_owner(&other_draft, &summaries).is_none());
    assert_eq!(legacy_terminal_owner(&draft, &summaries).as_deref(), Some("draft:project:1"));
}

#[cfg(all(test, feature = "snapshot"))]
mod focus_tests {
    use super::*;
    use gpui::{IntoElement, Render, div};
    use t3_core::terminal::LastThreadFocus;

    struct Host {
        composer: Entity<crate::composer::Composer>,
        drawer: Entity<TerminalDrawer>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn composer_and_terminal_focus_restoration_across_thread_switches(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) =
            cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:operate", "terminal:read", "terminal:operate"]
                }))
                .unwrap(),
            ))
            .unwrap();
        state.update(cx, |state, cx| {
            state.selected_thread = Some("menu-thread".into());
            cx.emit(AppEvent::Selection);
        });

        let (host, vcx) = cx.add_window_view(|window, cx| {
            // Same construction order as workspace.rs: Composer before TerminalDrawer.
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let drawer = cx.new(|cx| TerminalDrawer::new(state.clone(), window, cx));
            Host { composer, drawer }
        });
        vcx.run_until_parked();

        // Pre-open drawers on both menu-thread (and focus term-1) and sibling (fresh).
        let (composer, drawer) = host.read_with(vcx, |h, _| (h.composer.clone(), h.drawer.clone()));
        drawer.update_in(vcx, |drawer, window, cx| {
            drawer.access = TerminalAccess { read: true, operate: true };
            drawer.layouts.ensure("menu-thread", "term-1", true, Some(true));
            drawer.layouts.ensure("sibling", "term-2", true, Some(true));
            drawer.sync_selection(window, cx);
            drawer.focus_active(window, cx);
        });
        vcx.run_until_parked();

        // 1. Fresh entry into sibling with an open drawer focuses term-2, not the composer.
        state.update(vcx, |state, cx| state.select_thread("sibling", cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            assert!(!composer.read(cx).has_focus(window, cx), "fresh open drawer should take focus");
            let pane = drawer.read(cx).panes.get("term-2").expect("term-2 pane mounted");
            assert!(pane.focus_handle(cx).is_focused(window));
            assert_eq!(TerminalDrawer::last_focus_for_test("sibling", cx), Some(LastThreadFocus::Terminal("term-2".into())));
        });

        // 2. Focusing the composer on sibling records composer focus; switching away and back restores composer.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| composer.focus(window, cx));
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            assert!(composer.read(cx).has_focus(window, cx));
            assert_eq!(TerminalDrawer::last_focus_for_test("sibling", cx), Some(LastThreadFocus::Composer));
        });

        state.update(vcx, |state, cx| state.select_thread("menu-thread", cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let pane = drawer.read(cx).panes.get("term-1").expect("term-1 pane mounted");
            assert!(pane.focus_handle(cx).is_focused(window));
        });

        state.update(vcx, |state, cx| state.select_thread("sibling", cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            assert!(composer.read(cx).has_focus(window, cx), "returning to sibling must restore composer focus over open drawer");
            let pane = drawer.read(cx).panes.get("term-2").expect("term-2 pane mounted");
            assert!(!pane.focus_handle(cx).is_focused(window));
        });

        // 3. Explicit terminal navigation overrides remembered composer focus.
        state.update(vcx, |state, cx| state.select_thread("menu-thread", cx));
        vcx.run_until_parked();
        vcx.update(|_, cx| request_terminal_focus_on_navigation("sibling", "term-2", cx));
        state.update(vcx, |state, cx| state.select_thread("sibling", cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let pane = drawer.read(cx).panes.get("term-2").expect("term-2 pane mounted");
            assert!(pane.focus_handle(cx).is_focused(window), "explicit terminal navigation must win over remembered composer");
            assert!(!composer.read(cx).has_focus(window, cx));
        });

        // 4. Toggling the drawer closed returns focus to the composer.
        drawer.update_in(vcx, |drawer, window, cx| drawer.toggle(window, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            assert!(!drawer.read(cx).is_open(cx));
            assert!(composer.read(cx).has_focus(window, cx), "closing the drawer must return focus to composer");
            assert_eq!(TerminalDrawer::last_focus_for_test("sibling", cx), Some(LastThreadFocus::Composer));
        });

        // 5. Draft entry focuses the composer and records focus for that draft session.
        state.update(vcx, |state, cx| state.start_draft("checkout-a", cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            assert!(composer.read(cx).has_focus(window, cx));
            let draft = state.read(cx).draft_route_key().expect("active draft");
            assert_eq!(TerminalDrawer::last_focus_for_test(&draft, cx), Some(LastThreadFocus::Composer));
        });

        // 6. Optional `confirmTerminalClose = false` (`pingdotgg/t3code#35305`) closes immediately
        // without opening a confirmation prompt.
        state.update(vcx, |state, cx| state.select_thread("menu-thread", cx));
        vcx.update(|_, cx| {
            crate::settings::store::init(cx);
            crate::settings::store::install_committed_client(
                &state,
                serde_json::from_value(json!({
                    "confirmTerminalClose": false,
                    "panelAnimationDurationMs": 180
                }))
                .unwrap(),
                cx,
            );
        });
        vcx.run_until_parked();
        drawer.update_in(vcx, |drawer, window, cx| {
            assert!(drawer.layouts.get("menu-thread").terminal_ids.contains(&"term-1".to_owned()));
            drawer.request_close("term-1", window, cx);
            assert!(
                !drawer.layouts.get("menu-thread").terminal_ids.contains(&"term-1".to_owned()),
                "confirmTerminalClose=false must close terminal without waiting on prompt"
            );
        });

        // 7. `panelAnimationDurationMs` (`#1`) drives drawer open/close height transitions on same thread.
        drawer.update_in(vcx, |drawer, window, cx| {
            // Seed closed state on menu-thread via render, then open and close.
            drawer.layouts.set_open("menu-thread", false);
            let _ = gpui::Render::render(drawer, window, cx);
            assert!(drawer.drawer_animation.is_none());

            drawer.layouts.ensure("menu-thread", "term-3", true, Some(true));
            let _ = gpui::Render::render(drawer, window, cx);
            assert!(
                drawer.drawer_animation.is_some_and(|a| a.opening && a.duration_ms == 180),
                "opening drawer with panelAnimationDurationMs=180 should start opening animation"
            );

            drawer.layouts.set_open("menu-thread", false);
            let _ = gpui::Render::render(drawer, window, cx);
            assert!(
                drawer.drawer_animation.is_some_and(|a| !a.opening && a.duration_ms == 180 && a.from_height > 0.),
                "closing drawer with panelAnimationDurationMs=180 should start closing animation"
            );
        });
        vcx.executor().advance_clock(Duration::from_millis(210));
        vcx.run_until_parked();
        drawer.read_with(vcx, |drawer, _| {
            assert!(drawer.drawer_animation.is_none(), "closing animation clears after duration elapses");
        });

        state.update(vcx, |state, _| state.stop_pump());
        runtime.shutdown_background();
    }
}
