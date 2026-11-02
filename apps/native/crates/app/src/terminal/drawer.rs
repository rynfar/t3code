//! The thread terminal drawer's state: every thread's layout (persisted), the server's terminal
//! list from `subscribeTerminalMetadata`, and the panes of the selected thread. Ports
//! `PersistentThreadTerminalDrawer` (`components/PersistentThreadTerminals.tsx`),
//! `components/terminal/useThreadTerminalActions.ts`, and the toggle, new, split and close
//! handlers of `components/chat/useChatTerminalActions.ts`. Creating and closing terminals
//! needs `terminal:operate`; the drawer follows the device's grants as they change.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, Focusable, PromptLevel, Subscription, Task, Window};
use serde_json::json;
use t3_client::session::RpcError;
use t3_client::store;
use t3_core::terminal::{
    self, LaunchLocation, MetadataEvent, SplitDirection, TerminalAccess, TerminalLayouts, TerminalSummary, ThreadTerminalLayout,
    method,
};
use tokio::sync::mpsc;

use super::pane::{PaneEvent, TerminalPane};
use super::session::Target;
use crate::app_state::{AppEvent, AppState, Phase};

const SAVE_DELAY: Duration = Duration::from_millis(300);
/// The web keeps this in `localStorage` as `t3code:terminal-state:v1`.
const STATE_NAME: &str = "terminal-state";
const AUTHORIZATION_ERROR: &str = "EnvironmentAuthorizationError";

pub struct TerminalDrawer {
    pub(super) app: Entity<AppState>,
    pub(super) layouts: TerminalLayouts,
    summaries: Option<Vec<TerminalSummary>>,
    panes: HashMap<String, Entity<TerminalPane>>,
    panes_thread: Option<String>,
    metadata: Option<(tokio::task::JoinHandle<()>, Task<()>)>,
    access: TerminalAccess,
    grants: Option<Task<()>>,
    save: Option<Task<()>>,
    pub(super) resize_from: Option<(f32, f32)>,
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
            AppEvent::Selection | AppEvent::Shell => drawer.sync_selection(window, cx),
            _ => {}
        });
        let mut drawer = TerminalDrawer {
            app,
            layouts,
            summaries: None,
            panes: HashMap::new(),
            panes_thread: None,
            metadata: None,
            access: TerminalAccess::default(),
            grants: None,
            save: None,
            resize_from: None,
            _subscriptions: vec![subscription],
        };
        drawer.connection_changed(window, cx);
        drawer
    }

    pub(super) fn selected_thread(&self, cx: &App) -> Option<String> {
        self.app.read(cx).selected_thread.clone()
    }

    pub(super) fn layout(&self, cx: &App) -> Option<(String, ThreadTerminalLayout)> {
        let thread_id = self.selected_thread(cx)?;
        let layout = self.layouts.get(&thread_id).normalized();
        Some((thread_id, layout))
    }

    pub fn is_open(&self, cx: &App) -> bool {
        self.layout(cx).is_some_and(|(_, layout)| layout.terminal_open)
    }

    fn connection_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let online = matches!(self.app.read(cx).phase, Phase::Online(_));
        if !online {
            if let Some((driver, _)) = self.metadata.take() {
                driver.abort();
            }
            self.grants = None;
            self.summaries = None;
            self.panes.clear();
            cx.notify();
            return;
        }
        if self.metadata.is_none()
            && let Some(client) = self.app.read(cx).client_handle()
        {
            self.watch_grants(&client, cx);
            self.start_metadata(client, window, cx);
        }
        self.sync_selection(window, cx);
    }

    pub(super) fn can_operate(&self) -> bool {
        self.access.operate
    }

    /// Follows this device's grants; the last answer holds while reconnecting.
    fn watch_grants(&mut self, client: &t3_client::supervisor::ClientHandle, cx: &mut Context<Self>) {
        let mut grants = client.session_state();
        self.access = TerminalAccess::of(grants.borrow_and_update().as_ref());
        self.grants = Some(cx.spawn(async move |this, cx| {
            while grants.changed().await.is_ok() {
                let access = TerminalAccess::of(grants.borrow_and_update().as_ref());
                let updated = this.update(cx, |drawer, cx| {
                    if drawer.access != access {
                        drawer.access = access;
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
                        terminal::apply_metadata(summaries, event);
                    }
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
            self.panes_thread = selected;
        }
        self.reconcile(cx);
        self.ensure_panes(window, cx);
        cx.notify();
    }

    fn ensure_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread_id, layout)) = self.layout(cx) else { return };
        for terminal_id in layout.visible_terminal_ids() {
            self.pane(&thread_id, &terminal_id, window, cx);
        }
    }

    /// Adopts the server's terminal list for the selected thread (the drawer's reconcile effect).
    fn reconcile(&mut self, cx: &mut Context<Self>) {
        let (Some(thread_id), Some(summaries)) = (self.selected_thread(cx), self.summaries.as_ref()) else { return };
        let server_ids: Vec<String> = terminal::thread_terminals(summaries, &thread_id).iter().map(|t| t.terminal_id.clone()).collect();
        let client_ids = self.layouts.get(&thread_id).normalized().terminal_ids;
        if terminal::should_reconcile(&server_ids, &client_ids) && self.layouts.reconcile(&thread_id, &server_ids) {
            self.drop_stale_panes(&thread_id);
            self.schedule_save(cx);
        }
    }

    fn drop_stale_panes(&mut self, thread_id: &str) {
        let ids = self.layouts.get(thread_id).terminal_ids;
        self.panes.retain(|terminal_id, _| ids.contains(terminal_id));
    }

    pub(super) fn summary(&self, thread_id: &str, terminal_id: &str) -> Option<&TerminalSummary> {
        self.summaries.as_ref()?.iter().find(|t| t.thread_id == thread_id && t.terminal_id == terminal_id)
    }

    pub(super) fn label(&self, thread_id: &str, terminal_id: &str) -> String {
        terminal::session_label(terminal_id, self.summary(thread_id, terminal_id).map(|t| t.label.as_str()))
    }

    /// Where a terminal runs: its server session's cwd when known (reopening elsewhere would
    /// restart it), otherwise the thread's worktree or project root.
    fn location(&self, thread_id: &str, terminal_id: Option<&str>, cx: &App) -> Option<LaunchLocation> {
        let shell = &self.app.read(cx).shell;
        let thread = shell.thread(thread_id)?;
        let root = &shell.project(&thread.project_id)?.workspace_root;
        let known = terminal_id.and_then(|id| self.summary(thread_id, id)).filter(|summary| !summary.cwd.is_empty());
        Some(match known {
            Some(summary) => LaunchLocation {
                cwd: summary.cwd.clone(),
                ..LaunchLocation::new(root, summary.worktree_path.as_deref())
            },
            None => LaunchLocation::new(root, thread.worktree_path.as_deref()),
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
        let pane = cx.new(|cx| TerminalPane::new(target, client, runtime, window, cx));
        let (thread, terminal) = (thread_id.to_owned(), terminal_id.to_owned());
        cx.subscribe_in(&pane, window, move |drawer, _, event: &PaneEvent, window, cx| match event {
            PaneEvent::Exited => drawer.close_terminal(&thread, &terminal, window, cx),
            PaneEvent::Activated => drawer.activate(&terminal, window, cx),
        })
        .detach();
        self.panes.insert(terminal_id.to_owned(), pane.clone());
        Some(pane)
    }

    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread_id, layout)) = self.layout(cx) else { return };
        if !layout.terminal_open || layout.active_terminal_id.is_empty() {
            return;
        }
        if let Some(pane) = self.pane(&thread_id, &layout.active_terminal_id, window, cx) {
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
        let opening = !layout.terminal_open;
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
        let terminal_id = self.allocate_id(&thread_id);
        place(&mut self.layouts, &thread_id, &terminal_id);
        let open = client.request(method::OPEN, terminal::open_input(&thread_id, &terminal_id, &location, None));
        self.app.read(cx).runtime().spawn(open);
        Some(terminal_id)
    }

    /// The lowest free `term-N`; without the server's list, suffixed so it cannot collide with
    /// a session this client has not seen.
    fn allocate_id(&self, thread_id: &str) -> String {
        let layout_ids = self.layouts.get(thread_id).terminal_ids;
        let server_ids: Vec<&str> = self
            .summaries
            .as_ref()
            .map(|summaries| summaries.iter().filter(|t| t.thread_id == thread_id).map(|t| t.terminal_id.as_str()).collect())
            .unwrap_or_default();
        let suffix = self.summaries.is_none().then(|| uuid::Uuid::new_v4().to_string());
        terminal::next_terminal_id(server_ids.into_iter().chain(layout_ids.iter().map(String::as_str)), suffix.as_deref())
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

    /// Asks first, like `confirmTerminalClose`: closing stops the process and clears history.
    pub fn request_close(&mut self, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.selected_thread(cx) else { return };
        if !self.access.operate {
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
        self.layouts.close(thread_id, terminal_id);
        self.panes.remove(terminal_id);
        self.changed(window, cx);
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
