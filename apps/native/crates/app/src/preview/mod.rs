//! The preview browser: a right panel tab showing a page the server's headless Chromium renders,
//! streamed as frames. This controller follows the selected thread's preview sessions as
//! `components/preview/usePreviewSession.ts` and `agentOpenedPreviews.ts` do
//! (`followPreviewSessions`, forced `revealServerTabs`; there is no floating player here), opens
//! and closes sessions (`addBrowserSurface.ts`, `openPreviewSession.ts`, `closePreviewSession.ts`,
//! the browser part of `hooks/useRightPanelSurfaceClose.ts`) with default zoom and appearance
//! (`pingdotgg/t3code#34943`, `pingdotgg/t3code#25874`), keeps the per-project recently used list
//! and closed-tab history excluding Incognito (`browserHistoryStore.ts`, `closedViewStore.ts`,
//! `reopenClosedView.ts`, `pingdotgg/t3code#34859`), and decides availability as
//! `PreviewPanel.tsx` does. The tab itself is `PreviewPane`; its stream and input are
//! `frame::FrameView`.

mod annotation;
mod device_toolbar;
mod frame;
mod pane;
#[cfg(all(test, feature = "snapshot"))]
mod tests;

use std::collections::HashMap;
use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task, actions};
use serde_json::{Value, json};
use t3_client::session::RpcError;
use t3_client::store;
use t3_core::preview::address::{BrowserHistory, ClosedPreviewHistory, HistoryEntry, is_history_profile};
use t3_core::preview::viewport::{BrowserDefaults, OpenPreviewOverrides, ViewportSetting, open_session_payload};
use t3_core::preview::{ListResult, PREVIEW_OPERATE_SCOPE, PreviewEvent, PreviewThreads, SessionSnapshot, TabsEdit, method};

pub use pane::PreviewPane;

use crate::app_state::{AppEvent, AppState};
use crate::toast::{Toast, ToastKind};

actions!(preview, [TogglePreview, Refresh, FocusUrl, ZoomIn, ZoomOut, ResetZoom, ReopenClosedTab]);

const AUTHORIZATION_ERROR: &str = "EnvironmentAuthorizationError";

/// Whether this connection can show a browser tab, in the web's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// The server hosts no browser for clients without one (`capabilities.serverBrowser`).
    DesktopOnly,
    /// This device was paired without `preview:operate`.
    NeedsScope,
}

impl Availability {
    /// `PreviewPanel`'s message in place of the browser.
    pub fn message(self) -> Option<&'static str> {
        match self {
            Availability::Available => None,
            Availability::DesktopOnly => Some("Preview is only available in the T3 Code desktop app."),
            Availability::NeedsScope => Some("Pair this client again with preview access to control browser previews."),
        }
    }

    /// The launcher's disabled hint (`SURFACE_UNAVAILABLE_HINTS.browser`).
    pub fn launcher_hint(self) -> Option<&'static str> {
        match self {
            Availability::Available => None,
            Availability::DesktopOnly => Some("Browser previews are only available in the T3 Code desktop app."),
            Availability::NeedsScope => self.message(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewsEvent {
    /// A thread's sessions, or the recently used list, changed.
    Thread(String),
}

/// Aborts a network task with its owner.
struct Abort(tokio::task::JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum Feed {
    Event(Box<PreviewEvent>),
    /// The subscription reopened; events in the gap are lost, so the thread lists again.
    Resubscribed,
}

pub struct Previews {
    state: Entity<AppState>,
    threads: PreviewThreads,
    /// Browser tabs each thread's panel last showed, to tell a user's close from the server's.
    panel_tabs: HashMap<String, Vec<String>>,
    history: BrowserHistory,
    history_store: Option<String>,
    closed_tabs: ClosedPreviewHistory,
    closed_store: Option<String>,
    /// `None` until the session state answers; unknown counts as granted.
    can_operate: Option<bool>,
    /// `None` until the server config arrives.
    server_browser: Option<bool>,
    connection: Option<u64>,
    events: Option<(Abort, Task<()>)>,
    grants: Option<Task<()>>,
    listing: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<PreviewsEvent> for Previews {}

struct PreviewsGlobal(Entity<Previews>);

impl Global for PreviewsGlobal {}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    let previews = cx.new(|cx| Previews::new(state.clone(), cx));
    cx.set_global(PreviewsGlobal(previews));
    let toggle = state.clone();
    cx.on_action(move |_: &TogglePreview, cx| toggle.update(cx, toggle_preview));
}

pub fn previews(cx: &App) -> Option<Entity<Previews>> {
    cx.try_global::<PreviewsGlobal>().map(|global| global.0.clone())
}

/// The launcher's "Browser" entry (`addBrowserSurface`): a new session in a new tab.
pub fn add_browser_surface(state: &mut AppState, cx: &mut Context<AppState>) {
    let Some(thread_id) = state.panel_thread() else { return };
    cx.defer(move |cx| {
        if let Some(previews) = previews(cx) {
            previews.update(cx, |previews, cx| previews.open_session(thread_id, None, cx));
        }
    });
}

/// `preview.toggle` (`togglePreviewPanel`): hides a showing browser, else shows the active
/// session's tab or starts one.
pub fn toggle_preview(state: &mut AppState, cx: &mut Context<AppState>) {
    let Some(thread_id) = state.panel_thread() else { return };
    let showing = state.panels.panels.active_surface(&thread_id).is_some_and(|surface| surface.preview_tab().is_some() || surface.id == "browser:new");
    if showing {
        state.mutate_panels(cx, |panels, thread| {
            let unmaximized = panels.panels.set_maximized(thread, false);
            panels.panels.hide(thread) || unmaximized
        });
        return;
    }
    cx.defer(move |cx| {
        let Some(previews) = previews(cx) else { return };
        previews.update(cx, |previews, cx| {
            if previews.availability() != Availability::Available {
                return;
            }
            match previews.threads.thread(&thread_id).and_then(|thread| thread.active_tab_id.clone()) {
                Some(tab) => previews.state.update(cx, |state, cx| state.mutate_panels(cx, |panels, thread| panels.panels.open_browser(thread, Some(&tab)))),
                None => previews.open_session(thread_id, None, cx),
            }
        });
    });
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}

impl Previews {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
            AppEvent::Phase => this.sync_connection(cx),
            AppEvent::ServerConfig => this.sync_server_browser(cx),
            AppEvent::Selection => {
                this.panel_changed(cx);
                let selected = this.state.read(cx).panel_thread();
                let edit = this.threads.show(selected.as_deref());
                this.apply_edit(selected.as_deref(), edit, cx);
                this.list_selected(cx);
            }
            AppEvent::RightPanel => this.panel_changed(cx),
            _ => {}
        });
        let mut previews = Previews {
            state,
            threads: PreviewThreads::default(),
            panel_tabs: HashMap::new(),
            history: BrowserHistory::default(),
            history_store: None,
            closed_tabs: ClosedPreviewHistory::default(),
            closed_store: None,
            can_operate: None,
            server_browser: None,
            connection: None,
            events: None,
            grants: None,
            listing: None,
            _subscription: subscription,
        };
        previews.sync_connection(cx);
        previews
    }

    pub fn availability(&self) -> Availability {
        if self.can_operate == Some(false) {
            Availability::NeedsScope
        } else if self.server_browser == Some(false) {
            Availability::DesktopOnly
        } else {
            Availability::Available
        }
    }

    pub fn session(&self, thread_id: &str, tab_id: &str) -> Option<&SessionSnapshot> {
        self.threads.thread(thread_id)?.session(tab_id)
    }

    pub fn browser_defaults(&self, cx: &App) -> BrowserDefaults {
        crate::settings::store::try_client(cx)
            .map(|doc| BrowserDefaults::from_settings_entries(doc.entries()))
            .unwrap_or_default()
    }

    pub fn has_closed_tab(&self, thread_id: &str) -> bool {
        self.closed_tabs.has_for_thread(thread_id)
    }

    /// A new connection starts over: its sessions, grants and history are its own.
    fn sync_connection(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let current = state.connection_id();
        if current == self.connection {
            return;
        }
        self.connection = current;
        self.threads.clear();
        self.threads.show(state.panel_thread().as_deref());
        self.panel_tabs.clear();
        self.events = None;
        self.grants = None;
        self.listing = None;
        self.can_operate = None;
        self.server_browser = None;
        self.history_store = state.environments.active.as_deref().map(|environment| format!("preview-history-{environment}"));
        self.history = self
            .history_store
            .as_deref()
            .and_then(store::load_ui_state)
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        self.closed_store = state.environments.active.as_deref().map(|environment| format!("preview-closed-tabs-{environment}"));
        self.closed_tabs = ClosedPreviewHistory::from_persisted_value(self.closed_store.as_deref().and_then(store::load_ui_state).as_ref());
        if let Some(client) = state.client_handle() {
            self.watch_grants(&client, cx);
        }
        self.sync_server_browser(cx);
    }

    fn watch_grants(&mut self, client: &t3_client::supervisor::ClientHandle, cx: &mut Context<Self>) {
        let mut grants = client.session_state();
        self.can_operate = grants.borrow_and_update().as_ref().map(|state| state.grants(PREVIEW_OPERATE_SCOPE));
        self.grants = Some(cx.spawn(async move |this, cx| {
            while grants.changed().await.is_ok() {
                let can_operate = grants.borrow_and_update().as_ref().map(|state| state.grants(PREVIEW_OPERATE_SCOPE));
                let updated = this.update(cx, |this, cx| {
                    if this.can_operate != can_operate {
                        this.can_operate = can_operate;
                        this.emit_all(cx);
                    }
                });
                if updated.is_err() {
                    return;
                }
            }
        }));
    }

    fn emit_all(&self, cx: &mut Context<Self>) {
        if let Some(thread) = self.state.read(cx).panel_thread() {
            cx.emit(PreviewsEvent::Thread(thread));
        }
    }

    fn sync_server_browser(&mut self, cx: &mut Context<Self>) {
        let config = self.state.read(cx).server_config.config.as_ref().map(|config| config.environment.capabilities.server_browser == Some(true));
        if config.is_none() || config == self.server_browser {
            return;
        }
        self.server_browser = config;
        if config == Some(true) {
            self.start_events(cx);
            self.list_selected(cx);
        } else {
            self.events = None;
        }
        self.emit_all(cx);
    }

    fn start_events(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Feed>();
        let driver = state.runtime().spawn(async move {
            let mut attempt = 0u32;
            let mut opened_before = false;
            loop {
                match client.stream(method::SUBSCRIBE_EVENTS, json!({})).await {
                    Ok(mut stream) => {
                        if opened_before && tx.send(Feed::Resubscribed).is_err() {
                            return;
                        }
                        opened_before = true;
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(value) => {
                                    attempt = 0;
                                    match PreviewEvent::decode(value) {
                                        Ok(event) => {
                                            if tx.send(Feed::Event(Box::new(event))).is_err() {
                                                return;
                                            }
                                        }
                                        Err(error) => tracing::warn!(%error, "undecodable preview event"),
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
                if tx.is_closed() {
                    return;
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_millis((250u64 << attempt.min(5)).min(5_000))).await;
            }
        });
        let pump = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(item) = rx.try_recv() {
                    batch.push(item);
                }
                if this.update(cx, |this, cx| this.apply_feed(batch, cx)).is_err() {
                    return;
                }
            }
        });
        self.events = Some((Abort(driver), pump));
    }

    fn apply_feed(&mut self, batch: Vec<Feed>, cx: &mut Context<Self>) {
        let mut resubscribed = false;
        let events: Vec<PreviewEvent> = batch
            .into_iter()
            .filter_map(|item| match item {
                Feed::Event(event) => Some(*event),
                Feed::Resubscribed => {
                    resubscribed = true;
                    None
                }
            })
            .collect();
        let applied = self.threads.apply(events);
        for thread in &applied.changed {
            self.changed(thread, cx);
        }
        let shown = self.state.read(cx).panel_thread();
        self.apply_edit(shown.as_deref(), applied.edit, cx);
        if resubscribed || applied.relist {
            self.list_selected(cx);
        }
    }

    fn list_selected(&mut self, cx: &mut Context<Self>) {
        if self.server_browser != Some(true) {
            return;
        }
        let Some(thread_id) = self.state.read(cx).panel_thread() else { return };
        self.threads.thread_mut(&thread_id);
        let Some(task) = self.state.read(cx).request(method::LIST, json!({ "threadId": thread_id }), ListResult::decode) else { return };
        self.listing = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Disconnected.".to_owned()));
            let _ = this.update(cx, |this, cx| match result {
                Ok(list) => {
                    let (changed, relist, edit) = this.threads.listed(&thread_id, &list);
                    if changed {
                        this.changed(&thread_id, cx);
                    }
                    this.apply_edit(Some(&thread_id), edit, cx);
                    if relist {
                        // An event overtook the list; ask again before trusting it for tab cleanup.
                        cx.spawn(async move |this, cx| this.update(cx, |this, cx| this.list_selected(cx))).detach();
                    }
                }
                Err(error) => tracing::warn!(%error, "preview.list failed"),
            });
        }));
    }

    /// A thread's sessions changed: views follow, and non-Incognito loaded pages name their
    /// history entries (`pingdotgg/t3code#34859`).
    fn changed(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        cx.emit(PreviewsEvent::Thread(thread_id.to_owned()));
        let project = self.project_of(thread_id, cx);
        let (Some(project), Some(thread)) = (project, self.threads.thread(thread_id)) else { return };
        if self.history.record_settled_titles(&project, &thread.sessions) {
            self.save_history();
        }
    }

    /// Applies the shown thread's tab edit to its panel.
    fn apply_edit(&mut self, thread_id: Option<&str>, edit: Option<TabsEdit>, cx: &mut Context<Self>) {
        let (Some(thread_id), Some(edit)) = (thread_id, edit) else { return };
        if self.state.read(cx).panel_thread().as_deref() != Some(thread_id) {
            return;
        }
        self.state.update(cx, |state, cx| state.mutate_panels(cx, |panels, thread| edit.apply(&mut panels.panels, thread)));
    }

    /// A browser tab the user closed (a tab whose session still lives) closes its session.
    fn panel_changed(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(thread_id) = state.panel_thread() else { return };
        let now: Vec<String> = state.panels.panels.panel(&thread_id).surfaces.iter().filter_map(|surface| surface.preview_tab().map(str::to_owned)).collect();
        let before = self.panel_tabs.insert(thread_id.clone(), now.clone()).unwrap_or_default();
        if self.availability() != Availability::Available {
            return;
        }
        for tab in before.into_iter().filter(|tab| !now.contains(tab)) {
            let live = self.threads.thread(&thread_id).is_none_or(|thread| !thread.list_loaded || thread.session(&tab).is_some());
            if live {
                self.close_session(&thread_id, &tab, cx);
            }
        }
    }

    /// `openPreviewSession` on the server's browser, then its tab in front.
    pub fn open_session(&mut self, thread_id: String, url: Option<String>, cx: &mut Context<Self>) {
        self.open_session_with(thread_id, OpenPreviewOverrides { url, ..Default::default() }, false, cx);
    }

    pub fn open_link(&mut self, thread_id: String, url: String, cx: &mut Context<Self>) {
        self.open_session_with(thread_id, OpenPreviewOverrides { url: Some(url), ..Default::default() }, true, cx);
    }

    /// Opens a server browser tab with default zoom, appearance, viewport, and profile
    /// (`pingdotgg/t3code#34943` & `pingdotgg/t3code#25874`), honoring explicit `overrides` (e.g.
    /// when reopening a closed tab).
    pub fn open_session_with(
        &mut self,
        thread_id: String,
        overrides: OpenPreviewOverrides,
        external_fallback: bool,
        cx: &mut Context<Self>,
    ) {
        let url = overrides.url.clone();
        if self.availability() != Availability::Available {
            if external_fallback && let Some(url) = &url {
                cx.open_url(url);
            }
            return;
        }
        let defaults = self.browser_defaults(cx);
        let payload = open_session_payload(&thread_id, &defaults, &overrides);
        let Some(task) = self.state.read(cx).request(method::OPEN, payload, SessionSnapshot::decode) else {
            if external_fallback && let Some(url) = &url {
                cx.open_url(url);
            }
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Disconnected.".to_owned()));
            let _ = this.update(cx, |this, cx| match result {
                Ok(snapshot) => {
                    let tab = snapshot.tab_id.clone();
                    let opened_profile = snapshot.profile_id.clone();
                    let opened_url = snapshot.nav_status.url().map(str::to_owned).or_else(|| url.clone());
                    let thread = this.threads.thread_mut(&thread_id);
                    if let Some(opened_url) = &opened_url {
                        thread.remember_url(opened_url);
                    }
                    thread.apply_snapshot(snapshot);
                    if let Some(opened_url) = &opened_url
                        && is_history_profile(opened_profile.as_deref())
                        && let Some(project) = this.project_of(&thread_id, cx)
                        && this.history.record(&project, opened_url, now_ms())
                    {
                        this.save_history();
                    }
                    cx.emit(PreviewsEvent::Thread(thread_id.clone()));
                    this.apply_edit(Some(&thread_id), Some(TabsEdit { tabs: None, fronted: vec![tab] }), cx);
                }
                Err(error) => {
                    if external_fallback && let Some(url) = &url {
                        cx.open_url(url);
                    } else {
                        this.toast("Unable to open browser", error, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// `closePreviewSession`: remembers the tab in closed-view history (Incognito stays in-memory
    /// only, never persisted — `pingdotgg/t3code#34859`), hides at once, restores if the server refuses.
    pub fn close_session(&mut self, thread_id: &str, tab_id: &str, cx: &mut Context<Self>) {
        let snapshot = self.session(thread_id, tab_id).cloned();
        if let Some(snapshot) = &snapshot {
            self.closed_tabs.remember(snapshot);
            self.save_closed_tabs();
        }
        self.threads.thread_mut(thread_id).begin_close(tab_id);
        cx.emit(PreviewsEvent::Thread(thread_id.to_owned()));
        let task = self.state.read(cx).rpc(method::CLOSE, json!({ "threadId": thread_id, "tabId": tab_id }));
        let (thread_id, tab_id) = (thread_id.to_owned(), tab_id.to_owned());
        cx.spawn(async move |this, cx| {
            let Err(error) = task.await else { return };
            let _ = this.update(cx, |this, cx| {
                let thread = this.threads.thread_mut(&thread_id);
                let before = thread.clone();
                if thread.cancel_close(snapshot, &tab_id) {
                    this.changed(&thread_id, cx);
                    let edit = this.threads.changed(&thread_id, &before);
                    this.apply_edit(Some(&thread_id), edit, cx);
                }
                this.toast("Unable to close the browser tab", error.to_string(), cx);
            });
        })
        .detach();
    }

    /// Reopens the most recently closed preview tab for `thread_id`, preserving its URL, viewport,
    /// profile, zoom, and appearance (`reopenClosedView.ts`, `pingdotgg/t3code#25874`).
    pub fn reopen_last_closed(&mut self, thread_id: &str, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.closed_tabs.pop_for_thread(thread_id) else {
            return false;
        };
        self.save_closed_tabs();
        self.open_session_with(thread_id.to_owned(), entry.reopen_overrides(), false, cx);
        cx.emit(PreviewsEvent::Thread(thread_id.to_owned()));
        true
    }

    /// `preview.resize`: commits a viewport change from the device toolbar (`browserViewportActions.ts`).
    pub fn resize(&mut self, thread_id: &str, tab_id: &str, viewport: ViewportSetting, cx: &mut Context<Self>) {
        let payload = json!({
            "threadId": thread_id,
            "tabId": tab_id,
            "viewport": viewport.to_value(),
        });
        let Some(task) = self.state.read(cx).request(method::RESIZE, payload, SessionSnapshot::decode) else { return };
        let thread_id = thread_id.to_owned();
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Disconnected.".to_owned()));
            let _ = this.update(cx, |this, cx| match result {
                Ok(snapshot) => {
                    if this.threads.thread_mut(&thread_id).update_snapshot(snapshot) {
                        cx.emit(PreviewsEvent::Thread(thread_id));
                    }
                }
                Err(error) => this.toast("Unable to resize browser viewport", error, cx),
            });
        })
        .detach();
    }

    /// `preview.adjust` (zoom, appearance, hard reload, clear cookies/cache): needs no control of the tab.
    pub fn adjust(&mut self, thread_id: &str, tab_id: &str, change: Value, cx: &mut Context<Self>) {
        let mut payload = json!({ "threadId": thread_id, "tabId": tab_id });
        if let (Some(payload), Some(change)) = (payload.as_object_mut(), change.as_object()) {
            payload.extend(change.clone());
        }
        let Some(task) = self.state.read(cx).request(method::ADJUST, payload, SessionSnapshot::decode) else { return };
        let thread_id = thread_id.to_owned();
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Disconnected.".to_owned()));
            let _ = this.update(cx, |this, cx| match result {
                Ok(snapshot) => {
                    if this.threads.thread_mut(&thread_id).update_snapshot(snapshot) {
                        cx.emit(PreviewsEvent::Thread(thread_id));
                    }
                }
                Err(error) => this.toast("Unable to change the browser tab", error, cx),
            });
        })
        .detach();
    }

    /// The page opened a popup; it gets a tab once its session is known.
    pub fn popup(&mut self, thread_id: &str, tab_id: &str, cx: &mut Context<Self>) {
        let edit = self.threads.popup(thread_id, tab_id);
        self.apply_edit(Some(thread_id), edit, cx);
    }

    /// `setActivePreviewTab`: the tab `preview.toggle` brings back.
    pub fn set_active(&mut self, thread_id: &str, tab_id: &str) {
        self.threads.thread_mut(thread_id).set_active(tab_id);
    }

    fn project_of(&self, thread_id: &str, cx: &App) -> Option<String> {
        self.state
            .read(cx)
            .thread_shell(thread_id)
            .map(|thread| thread.project_id.clone())
            .or_else(|| thread_id.strip_prefix("draft:").map(str::to_owned))
    }

    /// The thread's project's recently used addresses, newest first.
    pub fn recent(&self, thread_id: &str, cx: &App) -> Vec<HistoryEntry> {
        self.project_of(thread_id, cx).map(|project| self.history.recent(&project).to_vec()).unwrap_or_default()
    }

    /// `rememberPreviewUrl` and `recordVisitForThread`: skips persisted history for Incognito
    /// tabs (`pingdotgg/t3code#34859`).
    pub fn record_visit(&mut self, thread_id: &str, profile_id: Option<&str>, url: &str, cx: &mut Context<Self>) {
        self.threads.thread_mut(thread_id).remember_url(url);
        if let Some(project) = self.project_of(thread_id, cx)
            && self.history.record_for_profile(&project, profile_id, url, now_ms())
        {
            self.save_history();
            cx.emit(PreviewsEvent::Thread(thread_id.to_owned()));
        }
    }

    pub fn remove_recent(&mut self, thread_id: &str, url: &str, cx: &mut Context<Self>) {
        if let Some(project) = self.project_of(thread_id, cx)
            && self.history.remove(&project, url)
        {
            self.save_history();
            cx.emit(PreviewsEvent::Thread(thread_id.to_owned()));
        }
    }

    fn save_history(&self) {
        if let Some(name) = &self.history_store {
            store::save_ui_state(name, &self.history);
        }
    }

    fn save_closed_tabs(&self) {
        if let Some(name) = &self.closed_store {
            store::save_ui_state(name, &self.closed_tabs.to_persisted_value());
        }
    }

    fn toast(&self, title: &'static str, description: String, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, title).description(description), cx));
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn seed_snapshot_for_test(&mut self, snapshot: SessionSnapshot, cx: &mut Context<Self>) {
        let thread_id = snapshot.thread_id.clone();
        self.threads.thread_mut(&thread_id).apply_snapshot(snapshot);
        self.changed(&thread_id, cx);
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn set_server_browser_for_test(&mut self, enabled: bool) {
        self.server_browser = Some(enabled);
        self.can_operate = Some(true);
    }
}
