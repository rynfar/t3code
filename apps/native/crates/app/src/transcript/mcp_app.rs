//! Inline MCP apps, ported from `McpAppFrame.tsx` and `McpAppTimelineRow`.
//! WebKit only transports messages; the protocol host lives in `t3_core::mcp_apps`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt, channel::mpsc};
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Context, Element, ElementId, Entity, FocusHandle, Focusable, Global, GlobalElementId, InspectorElementId,
    KeyDownEvent, LayoutId, MouseButton, PathPromptOptions, Pixels, SharedString, Subscription, Task, Window, div, prelude::*, px, relative,
};
use gpui_component::ActiveTheme;
use serde_json::{Value, json};
use t3_core::commands::{self, DispatchMode, method};
use t3_core::html_render::routing::PageOrigin;
use t3_core::mcp_apps::{Action, DEFAULT_HEIGHT, DownloadItem, Host, MAX_HEIGHT, Operation, TEARDOWN_TIMEOUT_MS};
use t3_core::timeline::McpAppEntry;
use t3_core::turn_item::{TurnItem, TurnItemKind};

use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::icons::Icon;
use crate::theme::{hsla, palette, rem};
use crate::thread_view::ThreadView;
use crate::webview::{self, Layer, Page, WebEvent, WebView};

const MAX_PAGES: usize = 24;
const MAX_VIEWS: usize = 6;
const FULLSCREEN_BAR_HEIGHT: f32 = 40.;
const TRANSPORT: &str = include_str!("mcp_app_transport.js");
type Key = (String, String, String, String, String, gpui::EntityId);

#[derive(Clone)]
struct FrameOwner {
    origin: PageOrigin,
    embedded: bool,
    view: gpui::WeakEntity<ThreadView>,
}

fn side_chat_visible(state: &AppState, thread_id: &str) -> bool {
    state
        .panel_thread()
        .as_deref()
        .and_then(|parent| state.panels.panels.active_surface(parent))
        .is_some_and(|surface| surface.side_chat_thread() == Some(thread_id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayMode {
    Inline,
    Fullscreen,
}

impl DisplayMode {
    fn as_str(self) -> &'static str {
        match self {
            DisplayMode::Inline => "inline",
            DisplayMode::Fullscreen => "fullscreen",
        }
    }
}

struct Frames {
    state: Entity<AppState>,
    pages: HashMap<Key, (u64, Entity<McpFrame>)>,
    views: Vec<Weak<WebView>>,
    fullscreen: Option<Entity<McpFullscreen>>,
    clock: u64,
}
impl Global for Frames {}

pub fn init(state: &Entity<AppState>, cx: &mut App) {
    if !cx.has_global::<Frames>() {
        cx.set_global(Frames {
            state: state.clone(),
            pages: HashMap::new(),
            views: Vec::new(),
            fullscreen: None,
            clock: 0,
        });
    }
}

pub fn is_fullscreen_open(cx: &App) -> bool {
    let Some(frames) = cx.try_global::<Frames>() else { return false };
    let Some(fullscreen) = &frames.fullscreen else { return false };
    fullscreen.read(cx).is_valid(cx)
}

pub fn fullscreen(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let fullscreen = cx.try_global::<Frames>()?.fullscreen.clone()?;
    if !fullscreen.read(cx).is_valid(cx) {
        let frame = fullscreen.read(cx).frame.clone();
        cx.defer(move |cx| {
            frame.update(cx, |frame, cx| frame.exit_fullscreen(cx));
            if let Some(frames) = cx.try_global::<Frames>()
                && frames.fullscreen.as_ref().is_some_and(|fs| fs == &fullscreen)
            {
                cx.global_mut::<Frames>().fullscreen = None;
                cx.refresh_windows();
            }
        });
        return None;
    }
    let viewport = window.viewport_size();
    let width = f32::from(viewport.width);
    let height = (f32::from(viewport.height) - FULLSCREEN_BAR_HEIGHT).max(0.);
    let frame = fullscreen.read(cx).frame.clone();
    frame.update(cx, |frame, cx| {
        frame.set_fullscreen_size(width, height, cx);
        frame.sync_theme(cx);
    });
    Some(fullscreen.into_any_element())
}

pub fn frame_row(_: &str, entry: &Arc<McpAppEntry>, thread: Option<&str>, embedded: bool, cx: &mut Context<ThreadView>) -> AnyElement {
    let Some(frames) = cx.try_global::<Frames>() else {
        return div().into_any_element();
    };
    let state = frames.state.clone();
    let Some(environment) = state.read(cx).environment().map(str::to_owned) else {
        return div().into_any_element();
    };
    let conversation_thread = thread.unwrap_or(&entry.source_thread_id).to_owned();
    let owner = PageOrigin::for_view(Some(&environment), Some(&conversation_thread), None).map(|origin| FrameOwner {
        origin,
        embedded,
        view: cx.entity().downgrade(),
    });
    let key = (
        environment.clone(),
        conversation_thread.clone(),
        entry.source_thread_id.clone(),
        entry.source_item_id.clone(),
        entry.reference.attachment_id.clone(),
        cx.entity_id(),
    );
    let frame = if let Some((_, frame)) = frames.pages.get(&key) {
        frame.clone()
    } else {
        let entry = entry.clone();
        let Some(client) = state.read(cx).client_handle() else {
            return div().into_any_element();
        };
        let frame = cx.new(|cx| McpFrame::new(environment, conversation_thread, owner.clone(), state, &client, entry, cx));
        cx.global_mut::<Frames>().pages.insert(key.clone(), (0, frame.clone()));
        frame
    };
    let clock = {
        let frames = cx.global_mut::<Frames>();
        frames.clock += 1;
        frames.clock
    };
    cx.global_mut::<Frames>().pages.get_mut(&key).unwrap().0 = clock;
    while cx.global::<Frames>().pages.len() > MAX_PAGES {
        let oldest = cx
            .global::<Frames>()
            .pages
            .iter()
            .filter(|(k, (_, f))| {
                let f = f.read(cx);
                *k != &key && f.view.is_none() && f.teardown_timer.is_none() && f.display_mode != DisplayMode::Fullscreen
            })
            .min_by_key(|(_, (used, _))| *used)
            .map(|(k, _)| k.clone());
        let Some(oldest) = oldest else { break };
        cx.global_mut::<Frames>().pages.remove(&oldest);
    }
    frame.update(cx, |frame, cx| frame.update_entry(entry, cx));
    div().w_full().px(rem(4.)).child(frame).into_any_element()
}

struct McpFrame {
    environment: String,
    thread_id: String,
    owner: Option<FrameOwner>,
    connection: t3_client::mcp_apps::AppConnection,
    state: Entity<AppState>,
    runtime: tokio::runtime::Handle,
    entry: Arc<McpAppEntry>,
    html: Option<Arc<str>>,
    failed: bool,
    height: u32,
    width: f32,
    fullscreen_size: Option<(f32, f32)>,
    display_mode: DisplayMode,
    theme_revision: usize,
    context: Value,
    tool: Option<Value>,
    detail: Option<Arc<TurnItem>>,
    host: Option<Host>,
    view: Option<Rc<WebView>>,
    generation: u64,
    closed: bool,
    load: Option<Task<()>>,
    detail_task: Option<Task<()>>,
    events: Option<Task<()>>,
    teardown_timer: Option<Task<()>>,
    _state_sub: Subscription,
    _event_sub: Subscription,
}

impl McpFrame {
    fn new(
        environment: String,
        thread_id: String,
        owner: Option<FrameOwner>,
        state: Entity<AppState>,
        client: &t3_client::supervisor::ClientHandle,
        entry: Arc<McpAppEntry>,
        cx: &mut Context<Self>,
    ) -> Self {
        let runtime = state.read(cx).runtime().clone();
        let state_sub = cx.observe(&state, |frame, _, cx| {
            if frame.display_mode == DisplayMode::Fullscreen && !frame.fullscreen_still_valid(cx) {
                frame.exit_fullscreen(cx);
            }
        });
        let event_sub = cx.subscribe(&state, |frame, _, event, cx| {
            if matches!(
                event,
                AppEvent::Phase | AppEvent::Shell | AppEvent::Selection | AppEvent::Thread | AppEvent::RightPanel | AppEvent::SideThread
            ) && frame.display_mode == DisplayMode::Fullscreen
                && !frame.fullscreen_still_valid(cx)
            {
                frame.exit_fullscreen(cx);
            }
        });
        let theme_revision = crate::theme::revision(cx);
        let context = host_context(DisplayMode::Inline, 0., None, None, cx);
        let mut frame = Self {
            environment,
            thread_id,
            owner,
            connection: t3_client::mcp_apps::AppConnection::new(client),
            state,
            runtime,
            entry,
            html: None,
            failed: false,
            height: DEFAULT_HEIGHT,
            width: 0.,
            fullscreen_size: None,
            display_mode: DisplayMode::Inline,
            theme_revision,
            context,
            tool: None,
            detail: None,
            host: None,
            view: None,
            generation: 0,
            closed: false,
            load: None,
            detail_task: None,
            events: None,
            teardown_timer: None,
            _state_sub: state_sub,
            _event_sub: event_sub,
        };
        frame.load(cx);
        frame.fetch_detail(cx);
        #[cfg(feature = "snapshot")]
        if webview::is_offscreen() && fixture::page(&frame.entry.reference.attachment_id).is_some() && fixture::audit_enabled() {
            frame.audit_teardown(cx);
        }
        frame
    }

    fn conversation_thread_id(&self) -> &str {
        &self.thread_id
    }

    fn awaiting_user(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        let tid = self.conversation_thread_id();
        state.thread_shell(tid).is_some_and(|shell| shell.pending_runtime_request.is_some())
            || state
                .thread_state(tid)
                .projection
                .as_ref()
                .is_some_and(|projection| !t3_core::requests::pending_requests(projection).is_empty())
    }

    fn fullscreen_still_valid(&self, cx: &App) -> bool {
        if self.display_mode != DisplayMode::Fullscreen || self.closed || self.awaiting_user(cx) {
            return false;
        }
        let state = self.state.read(cx);
        if state.environment() != Some(self.environment.as_str()) {
            return false;
        }
        let Some(owner) = &self.owner else {
            return true;
        };
        if owner.view.upgrade().is_none() {
            return false;
        }
        let current_origin = PageOrigin::for_view(
            state.environment(),
            state.selected_thread.as_deref(),
            owner.embedded.then_some(owner.origin.thread.as_str()),
        );
        if current_origin.as_ref() != Some(&owner.origin) {
            return false;
        }
        !owner.embedded || side_chat_visible(state, &owner.origin.thread)
    }

    fn client(&self, cx: &App) -> Option<t3_client::supervisor::ClientHandle> {
        let state = self.state.read(cx);
        let current = matches!(state.phase, crate::app_state::Phase::Online(t3_client::supervisor::ConnectionStatus::Connected))
            .then(|| state.client_handle())
            .flatten();
        self.connection.resolve(current)
    }

    fn target(&self) -> t3_client::mcp_apps::Target {
        t3_client::mcp_apps::Target {
            thread_id: self.entry.source_thread_id.clone(),
            item_id: self.entry.source_item_id.clone(),
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "snapshot")]
        if let Some(path) = fixture::page(&self.entry.reference.attachment_id) {
            self.html = std::fs::read_to_string(path).ok().map(Arc::from);
            self.failed = self.html.is_none();
            return;
        }
        let Some(client) = self.client(cx) else {
            self.failed = true;
            return;
        };
        let (reference, target) = (self.entry.reference.clone(), self.target());
        let job = self.runtime.spawn(async move {
            let (html, info) = tokio::join!(
                t3_client::mcp_apps::load_page(&client, &reference),
                t3_client::mcp_apps::tool_info(&client, &target, &reference.tool)
            );
            (html, info.ok().and_then(|i| i.tool))
        });
        self.load = Some(cx.spawn(async move |this, cx| {
            let Ok((html, tool)) = job.await else { return };
            let _ = this.update(cx, |frame, cx| {
                frame.html = html.ok().map(Arc::from);
                frame.failed = frame.html.is_none();
                frame.tool = tool;
                frame.update_context(cx);
                cx.notify();
            });
        }));
    }

    fn update_entry(&mut self, entry: &Arc<McpAppEntry>, cx: &mut Context<Self>) {
        if Arc::ptr_eq(&self.entry, entry) {
            return;
        }
        let changed = t3_core::timeline::detail_revision(&self.entry.item) != t3_core::timeline::detail_revision(&entry.item);
        self.entry = entry.clone();
        if changed {
            self.detail = None;
            self.fetch_detail(cx);
        }
        self.deliver_tool(cx);
    }

    fn fetch_detail(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "snapshot")]
        if fixture::page(&self.entry.reference.attachment_id).is_some() {
            return;
        }
        let Some(client) = self.client(cx) else { return };
        let target = self.target();
        let revision = t3_core::timeline::detail_revision(&self.entry.item);
        let job = self.runtime.spawn(async move {
            let value = client
                .request(
                    t3_core::timeline::GET_TURN_ITEM,
                    json!({"threadId":target.thread_id,"itemId":target.item_id,"revision":revision}),
                )
                .await
                .ok()?;
            t3_core::timeline::decode_detail(value).ok().flatten().map(Arc::new)
        });
        self.detail_task = Some(cx.spawn(async move |this, cx| {
            let Ok(detail) = job.await else { return };
            let _ = this.update(cx, |frame, cx| {
                frame.detail = detail;
                frame.deliver_tool(cx);
            });
        }));
    }

    fn deliver_tool(&mut self, cx: &mut Context<Self>) {
        let item = self.detail.as_ref().unwrap_or(&self.entry.item);
        let TurnItemKind::DynamicTool {
            input, output, output_omitted, ..
        } = &item.kind
        else {
            return;
        };
        if *output_omitted && self.detail.is_none() {
            return;
        }
        if let Some(host) = &mut self.host {
            let actions = host.set_tool_call(input.clone(), output.as_ref().and_then(|o| o.get("result")).cloned());
            self.actions(actions, cx);
        }
    }

    fn update_context(&mut self, cx: &mut Context<Self>) {
        self.theme_revision = crate::theme::revision(cx);
        self.context = host_context(self.display_mode, self.width, self.fullscreen_size, self.tool.as_ref(), cx);
        if let Some(view) = &self.view {
            view.set_dark(self.context["theme"] == "dark");
        }
        if let Some(host) = &mut self.host {
            let actions = host.update_context(self.context.clone());
            self.actions(actions, cx);
        }
    }

    fn sync_theme(&mut self, cx: &mut Context<Self>) {
        if self.theme_revision != crate::theme::revision(cx) {
            self.update_context(cx);
        }
    }

    fn set_fullscreen_size(&mut self, width: f32, height: f32, cx: &mut Context<Self>) {
        let next = Some((width, height));
        if self.fullscreen_size != next {
            self.fullscreen_size = next;
            if self.display_mode == DisplayMode::Fullscreen {
                self.update_context(cx);
            }
        }
    }

    fn dispose_host(&mut self) {
        self.generation += 1;
        self.teardown_timer = None;
        self.events = None;
        self.view = None;
        self.display_mode = DisplayMode::Inline;
        self.fullscreen_size = None;
        if let Some(mut host) = self.host.take() {
            host.dispose();
        }
    }

    fn clear_global_fullscreen(&self, my_id: gpui::EntityId, cx: &mut App) {
        if let Some(frames) = cx.try_global::<Frames>()
            && frames.fullscreen.as_ref().is_some_and(|fs| fs.read(cx).frame.entity_id() == my_id)
        {
            cx.global_mut::<Frames>().fullscreen = None;
            cx.refresh_windows();
        }
    }

    fn exit_fullscreen(&mut self, cx: &mut Context<Self>) {
        if self.display_mode != DisplayMode::Fullscreen {
            if let Some(view) = &self.view {
                view.set_dismiss_on_escape(false);
            }
            self.clear_global_fullscreen(cx.entity_id(), cx);
            return;
        }
        self.display_mode = DisplayMode::Inline;
        self.fullscreen_size = None;
        if let Some(view) = &self.view {
            view.set_dismiss_on_escape(false);
            view.release_focus();
            view.set_layer(Layer::Transcript);
        }
        let restore = if let Some(frames) = cx.try_global::<Frames>()
            && frames.fullscreen.as_ref().is_some_and(|fs| fs.read(cx).frame.entity_id() == cx.entity_id())
        {
            cx.global_mut::<Frames>().fullscreen.take().and_then(|fs| fs.read(cx).restore.clone())
        } else {
            None
        };
        if let Some(previous) = restore
            && let Some(window) = cx.active_window()
        {
            let _ = window.update(cx, |_, window, cx| {
                previous.focus(window, cx);
            });
        }
        self.update_context(cx);
        cx.notify();
        cx.refresh_windows();
    }

    fn handle_escape(&mut self, cx: &mut Context<Self>) {
        if self.display_mode == DisplayMode::Fullscreen {
            self.exit_fullscreen(cx);
        }
    }

    fn try_enter_fullscreen(&mut self, cx: &mut Context<Self>) -> bool {
        if self.closed || self.awaiting_user(cx) || crate::dialog::is_open(cx) {
            return false;
        }
        let my_id = cx.entity_id();
        if let Some(frames) = cx.try_global::<Frames>()
            && frames.fullscreen.as_ref().is_some_and(|fs| fs.read(cx).frame.entity_id() != my_id)
        {
            return false;
        }
        if !self.view.as_ref().is_some_and(|v| v.take_activation()) {
            return false;
        }
        let Some(window_handle) = cx.active_window() else { return false };
        self.enter_fullscreen_on_window(window_handle, cx)
    }

    fn enter_fullscreen_on_window(&mut self, window_handle: gpui::AnyWindowHandle, cx: &mut Context<Self>) -> bool {
        if let Some(view) = &self.view {
            view.set_layer(Layer::Modal);
            view.set_dismiss_on_escape(true);
            view.release_focus();
        }
        self.display_mode = DisplayMode::Fullscreen;
        let frame_entity = cx.entity();
        let restore = window_handle
            .update(cx, |_, window, cx| {
                let viewport = window.viewport_size();
                self.fullscreen_size = Some((f32::from(viewport.width), (f32::from(viewport.height) - FULLSCREEN_BAR_HEIGHT).max(0.)));
                window.focused(cx)
            })
            .ok()
            .flatten();
        let overlay = cx.new(|cx| McpFullscreen {
            frame: frame_entity,
            focus: cx.focus_handle(),
            restore,
        });
        let focus = overlay.read(cx).focus.clone();
        cx.global_mut::<Frames>().fullscreen = Some(overlay);
        self.update_context(cx);
        cx.notify();
        cx.refresh_windows();
        let _ = window_handle.update(cx, |_, window, cx| {
            window.focus(&focus, cx);
        });
        true
    }

    fn install_host(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.teardown_timer = None;
        self.host = Some(Host::new(self.entry.reference.clone(), self.context.clone(), env!("CARGO_PKG_VERSION")));
        self.deliver_tool(cx);
    }

    fn detach_host(&mut self, cx: &mut Context<Self>) {
        if self.display_mode == DisplayMode::Fullscreen {
            self.exit_fullscreen(cx);
        } else {
            if let Some(view) = &self.view {
                view.set_dismiss_on_escape(false);
            }
            self.clear_global_fullscreen(cx.entity_id(), cx);
        }
        if self.teardown_timer.is_some() {
            self.closed = true;
            cx.notify();
        }
        self.dispose_host();
    }

    fn reopen(&mut self, cx: &mut Context<Self>) {
        self.closed = false;
        self.height = DEFAULT_HEIGHT;
        cx.notify();
    }

    fn receive_message(&mut self, generation: u64, text: &str, cx: &mut Context<Self>) {
        if self.generation == generation
            && let Some(host) = &mut self.host
        {
            let actions = host.receive(text);
            self.actions(actions, cx);
        }
    }

    fn complete_request(&mut self, generation: u64, token: u64, result: Result<Value, String>, cx: &mut Context<Self>) {
        if self.generation == generation
            && let Some(host) = &mut self.host
        {
            let actions = host.complete(token, result);
            self.actions(actions, cx);
        }
    }

    fn teardown_timeout(&mut self, generation: u64, cx: &mut Context<Self>) {
        if self.generation == generation
            && let Some(host) = &mut self.host
        {
            let actions = host.teardown_timeout();
            self.actions(actions, cx);
        }
    }

    fn actions(&mut self, actions: Vec<Action>, cx: &mut Context<Self>) {
        for action in actions {
            match action {
                Action::Post(message) => {
                    #[cfg(feature = "snapshot")]
                    if webview::is_offscreen() && fixture::audit_enabled() {
                        eprintln!("MCP bridge host -> {message}");
                    }
                    if let Some(view) = &self.view {
                        view.evaluate(&format!("window.__t3McpReceive({message});"));
                    }
                }
                Action::Height(height) => {
                    if self.display_mode != DisplayMode::Fullscreen && self.height != height {
                        self.height = height;
                        cx.notify();
                    }
                }
                Action::ExitFullscreen => self.exit_fullscreen(cx),
                Action::Close => {
                    self.closed = true;
                    self.clear_global_fullscreen(cx.entity_id(), cx);
                    self.dispose_host();
                    cx.notify();
                }
                Action::StartTeardownTimer => {
                    let generation = self.generation;
                    self.teardown_timer = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(TEARDOWN_TIMEOUT_MS)).await;
                        let _ = this.update(cx, |frame, cx| frame.teardown_timeout(generation, cx));
                    }));
                }
                Action::Request { token, operation } => self.handle_request(token, operation, cx),
            }
        }
    }

    fn ask_user(
        &mut self,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
        on_decision: impl FnOnce(&mut Self, Result<bool, String>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.exit_fullscreen(cx);
        let generation = self.generation;
        let frame = cx.weak_entity();
        let on_decision = std::cell::RefCell::new(Some(on_decision));
        let confirm_frame = frame.clone();
        let cancel_frame = frame;
        let confirm_cb = on_decision.borrow_mut().take().expect("decision callback");
        let shared_cb = Rc::new(std::cell::RefCell::new(Some(confirm_cb)));
        let cancel_cb = shared_cb.clone();
        let fallback_cb = shared_cb.clone();
        let asked = crate::dialog::ask_available_cancellable(
            title,
            description,
            confirm_label,
            move |_, cx| {
                let Some(cb) = shared_cb.borrow_mut().take() else { return };
                let _ = confirm_frame.update(cx, |frame, cx| {
                    if frame.generation == generation {
                        cb(frame, Ok(true), cx);
                    }
                });
            },
            move |_, cx| {
                let Some(cb) = cancel_cb.borrow_mut().take() else { return };
                let _ = cancel_frame.update(cx, |frame, cx| {
                    if frame.generation == generation {
                        cb(frame, Ok(false), cx);
                    }
                });
            },
            cx,
        );
        if !asked
            && let Some(cb) = fallback_cb.borrow_mut().take()
        {
            cb(self, Err("T3 could not ask for approval here.".into()), cx);
        }
    }

    fn handle_request(&mut self, token: u64, operation: Operation, cx: &mut Context<Self>) {
        let generation = self.generation;
        match operation {
            Operation::OpenLink { url } => {
                let result = if self.view.as_ref().is_some_and(|v| v.take_activation()) {
                    cx.open_url(&url);
                    Ok(json!({}))
                } else {
                    Err("Links open only from a click in the app.".into())
                };
                self.complete_request(generation, token, result, cx);
            }
            Operation::RequestDisplayMode { mode } => {
                let next = match mode.as_str() {
                    "inline" => {
                        self.exit_fullscreen(cx);
                        "inline"
                    }
                    "fullscreen" => {
                        if self.try_enter_fullscreen(cx) {
                            "fullscreen"
                        } else {
                            self.display_mode.as_str()
                        }
                    }
                    _ => self.display_mode.as_str(),
                };
                self.complete_request(generation, token, Ok(json!({ "mode": next })), cx);
            }
            Operation::CallTool { name, arguments } => {
                let Some(client) = self.client(cx) else {
                    self.complete_request(generation, token, Err("The app's environment is not connected.".into()), cx);
                    return;
                };
                let target = self.target();
                let preflight_client = client.clone();
                let preflight_target = target.clone();
                let preflight_name = name.clone();
                let job = self.runtime.spawn(async move {
                    t3_client::mcp_apps::tool_info(&preflight_client, &preflight_target, &preflight_name).await
                });
                cx.spawn(async move |this, cx| {
                    let info = job.await.unwrap_or_else(|e| Err(e.to_string()));
                    let _ = this.update(cx, |frame, cx| {
                        if frame.generation != generation {
                            return;
                        }
                        let info = match info {
                            Ok(info) => info,
                            Err(err) => {
                                frame.complete_request(generation, token, Err(err), cx);
                                return;
                            }
                        };
                        if !info.callable {
                            frame.complete_request(generation, token, Err("This app cannot call that tool.".into()), cx);
                            return;
                        }
                        let label = info.title.as_deref().unwrap_or(&name);
                        let args_text = serde_json::to_string_pretty(&arguments).unwrap_or_else(|_| arguments.to_string());
                        let title = format!("Allow {} to run {}?", frame.entry.reference.server, label);
                        let run_call = move |frame: &mut McpFrame, cx: &mut Context<McpFrame>| {
                            let call_client = client.clone();
                            let call_target = target.clone();
                            let call_name = name.clone();
                            let call_args = arguments.clone();
                            let job = frame.runtime.spawn(async move {
                                t3_client::mcp_apps::call_tool_unchecked(&call_client, &call_target, &call_name, call_args).await
                            });
                            cx.spawn(async move |this, cx| {
                                let result = job.await.unwrap_or_else(|e| Err(e.to_string()));
                                let _ = this.update(cx, |frame, cx| frame.complete_request(generation, token, result, cx));
                            })
                            .detach();
                        };
                        if info.read_only {
                            run_call(frame, cx);
                            return;
                        }
                        frame.ask_user(
                            title,
                            args_text,
                            "Allow",
                            move |frame, decision, cx| match decision {
                                Ok(true) => run_call(frame, cx),
                                Ok(false) => frame.complete_request(generation, token, Err("Declined by the user.".into()), cx),
                                Err(err) => frame.complete_request(generation, token, Err(err), cx),
                            },
                            cx,
                        );
                    });
                })
                .detach();
            }
            Operation::ReadResource { uri } => {
                let Some(client) = self.client(cx) else {
                    self.complete_request(generation, token, Err("The app's environment is not connected.".into()), cx);
                    return;
                };
                let target = self.target();
                let job = self.runtime.spawn(async move { t3_client::mcp_apps::read_resource(&client, &target, &uri).await });
                cx.spawn(async move |this, cx| {
                    let result = job.await.unwrap_or_else(|e| Err(e.to_string()));
                    let _ = this.update(cx, |frame, cx| frame.complete_request(generation, token, result, cx));
                })
                .detach();
            }
            Operation::SendMessage { text } => {
                if self.client(cx).is_none() {
                    self.complete_request(generation, token, Err("Messages are not available here.".into()), cx);
                    return;
                }
                let conv_id = self.conversation_thread_id().to_owned();
                let has_thread = {
                    let state = self.state.read(cx);
                    state.thread_shell(&conv_id).is_some() || state.thread_state(&conv_id).projection.is_some()
                };
                if !has_thread {
                    self.complete_request(generation, token, Err("Messages are not available here.".into()), cx);
                    return;
                }
                let title = format!("Send this message from {}?", self.entry.reference.server);
                self.ask_user(
                    title,
                    text.clone(),
                    "Send",
                    move |frame, decision, cx| match decision {
                        Ok(true) => {
                            let cmd = commands::send_message(&uuid_v4(), &conv_id, &text, &DispatchMode::QueueAfterActive);
                            let rpc = frame.state.read(cx).rpc(method::DISPATCH_COMMAND, cmd);
                            cx.spawn(async move |this, cx| {
                                let result = rpc.await.map(|_| json!({})).map_err(|e| e.to_string());
                                let _ = this.update(cx, |frame, cx| frame.complete_request(generation, token, result, cx));
                            })
                            .detach();
                        }
                        Ok(false) => frame.complete_request(generation, token, Err("Declined by the user.".into()), cx),
                        Err(err) => frame.complete_request(generation, token, Err(err), cx),
                    },
                    cx,
                );
            }
            Operation::UpdateModelContext { content, structured_content } => {
                let Some(client) = self.client(cx) else {
                    self.complete_request(generation, token, Err("The app's environment is not connected.".into()), cx);
                    return;
                };
                let target = self.target();
                let conv_id = self.conversation_thread_id().to_owned();
                let job = self.runtime.spawn(async move {
                    t3_client::mcp_apps::update_model_context(&client, &target, &conv_id, content, structured_content).await
                });
                cx.spawn(async move |this, cx| {
                    let result = job.await.unwrap_or_else(|e| Err(e.to_string())).map(|_| json!({}));
                    let _ = this.update(cx, |frame, cx| frame.complete_request(generation, token, result, cx));
                })
                .detach();
            }
            Operation::DownloadFile { files } => {
                let Some(client) = self.client(cx) else {
                    self.complete_request(generation, token, Err("The app's environment is not connected.".into()), cx);
                    return;
                };
                let names = files
                    .iter()
                    .map(|file| match file {
                        DownloadItem::Embedded { name, .. } | DownloadItem::Link { name, .. } => name.as_str(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let title = format!("Save {} from {}?", names, self.entry.reference.server);
                let target = self.target();
                self.ask_user(
                    title,
                    "",
                    "Save",
                    move |frame, decision, cx| {
                        if !matches!(decision, Ok(true)) {
                            frame.complete_request(generation, token, Ok(json!({ "isError": true })), cx);
                            return;
                        }
                        let job = frame.runtime.spawn(async move {
                            t3_client::mcp_apps::resolve_downloads(&client, &target, &files).await
                        });
                        cx.spawn(async move |this, cx| {
                            let resolved = match job.await {
                                Ok(Ok(files)) => files,
                                Ok(Err(t3_client::mcp_apps::DownloadError::Refusal(_))) => {
                                    let _ = this.update(cx, |frame, cx| {
                                        frame.complete_request(generation, token, Ok(json!({ "isError": true })), cx);
                                    });
                                    return;
                                }
                                Err(err) => {
                                    let _ = this.update(cx, |frame, cx| frame.complete_request(generation, token, Err(err.to_string()), cx));
                                    return;
                                }
                            };
                            let _ = this.update(cx, |frame, cx| {
                                if frame.generation != generation {
                                    return;
                                }
                                frame.save_downloads(generation, token, resolved, cx);
                            });
                        })
                        .detach();
                    },
                    cx,
                );
            }
        }
    }

    fn save_downloads(
        &mut self,
        generation: u64,
        token: u64,
        mut resolved: Vec<t3_client::mcp_apps::ResolvedDownload>,
        cx: &mut Context<Self>,
    ) {
        let default_dir = std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Downloads"))
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from("/"));
        if resolved.len() == 1 {
            let file = resolved.remove(0);
            let rx = cx.prompt_for_new_path(&default_dir, Some(&file.name));
            cx.spawn(async move |this, cx| {
                let Ok(Ok(Some(dest))) = rx.await else {
                    let _ = this.update(cx, |frame, cx| {
                        frame.complete_request(generation, token, Ok(json!({ "isError": true })), cx);
                    });
                    return;
                };
                let write = cx.background_executor().spawn(async move { std::fs::write(&dest, &file.bytes) }).await;
                let result = write.map(|_| json!({})).map_err(|e| e.to_string());
                let _ = this.update(cx, |frame, cx| frame.complete_request(generation, token, result, cx));
            })
            .detach();
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose folder to save MCP app files".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut dirs))) = rx.await else {
                let _ = this.update(cx, |frame, cx| {
                    frame.complete_request(generation, token, Ok(json!({ "isError": true })), cx);
                });
                return;
            };
            let Some(dir) = dirs.pop() else {
                let _ = this.update(cx, |frame, cx| {
                    frame.complete_request(generation, token, Ok(json!({ "isError": true })), cx);
                });
                return;
            };
            let write = cx
                .background_executor()
                .spawn(async move {
                    for file in resolved {
                        std::fs::write(dir.join(&file.name), &file.bytes)?;
                    }
                    Ok::<_, std::io::Error>(())
                })
                .await;
            let result = write.map(|_| json!({})).map_err(|e| e.to_string());
            let _ = this.update(cx, |frame, cx| frame.complete_request(generation, token, result, cx));
        })
        .detach();
    }

    fn inline_view(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> Option<Rc<WebView>> {
        if (self.width - width).abs() > 0.5 {
            self.width = width;
            self.update_context(cx);
        }
        if let Some(view) = self.view.clone() {
            return Some(view);
        }
        if self.host.is_some() {
            self.detach_host(cx);
            if self.closed {
                return None;
            }
        }
        if !webview::available(window) || !window.is_window_active() {
            return None;
        }
        let my_id = cx.entity_id();
        loop {
            {
                let frames = cx.global_mut::<Frames>();
                frames.views.retain(|v| v.strong_count() > 0);
                if frames.views.len() < MAX_VIEWS {
                    break;
                }
            }
            let reclaim = cx
                .global::<Frames>()
                .pages
                .values()
                .filter(|(_, f)| {
                    let guard = f.read(cx);
                    f.entity_id() != my_id
                        && guard.teardown_timer.is_none()
                        && guard.display_mode != DisplayMode::Fullscreen
                        && guard.view.as_ref().is_some_and(|v| !v.is_shown())
                })
                .min_by_key(|(used, _)| *used)
                .map(|(_, f)| f.clone());
            let reclaim = reclaim?;
            reclaim.update(cx, |frame, cx| frame.detach_host(cx));
        }
        let html = self.html.clone()?;
        self.update_context(cx);
        let dark = self.context["theme"] == "dark";
        let page = Page {
            html,
            dark,
            scroll_through: false,
            script: TRANSPORT.into(),
            allow_popups: false,
            content_security_policy: Some(self.entry.reference.content_security_policy()),
        };
        let (events, received) = mpsc::unbounded();
        let view = WebView::new(page, Layer::Transcript, events, window, cx)?;
        let escapes = view.watch_escape();
        self.view = Some(view.clone());
        self.install_host(cx);
        let generation = self.generation;
        cx.global_mut::<Frames>().views.push(Rc::downgrade(&view));
        self.events = Some(cx.spawn(async move |this, cx| {
            enum PumpEvent {
                Web(WebEvent),
                Escape,
            }
            let mut stream = futures::stream::select(received.map(PumpEvent::Web), escapes.map(|()| PumpEvent::Escape));
            while let Some(event) = stream.next().await {
                if this
                    .update(cx, |frame, cx| {
                        if frame.generation != generation {
                            return;
                        }
                        match event {
                            PumpEvent::Web(WebEvent::Message(text)) => frame.receive_message(generation, &text, cx),
                            PumpEvent::Web(WebEvent::OpenUrl(_)) => {}
                            PumpEvent::Escape => frame.handle_escape(cx),
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = this.update(cx, |frame, cx| {
                if frame.generation == generation {
                    frame.detach_host(cx);
                }
            });
        }));
        Some(view)
    }

    #[cfg(feature = "snapshot")]
    fn audit_teardown(&mut self, cx: &mut Context<Self>) {
        let initialized = r#"{"jsonrpc":"2.0","method":"ui/notifications/initialized"}"#;
        let teardown = r#"{"jsonrpc":"2.0","method":"ui/notifications/request-teardown"}"#;
        let ack = r#"{"jsonrpc":"2.0","id":"t3-host-0","result":{}}"#;
        let mut old_generation = self.generation;
        for response in [
            Some(ack),
            Some(r#"{"jsonrpc":"2.0","id":"t3-host-0","error":{"code":-32000,"message":"fixture"}}"#),
            None,
        ] {
            self.reopen(cx);
            self.install_host(cx);
            let generation = self.generation;
            self.receive_message(generation, initialized, cx);
            self.receive_message(generation, teardown, cx);
            assert!(!self.closed && self.host.is_some() && self.teardown_timer.is_some());
            self.receive_message(old_generation, ack, cx);
            self.teardown_timeout(old_generation, cx);
            assert!(!self.closed && self.teardown_timer.is_some());
            eprintln!("MCP bridge audit: awaiting acknowledgement; stale message/deadline ignored");
            if let Some(response) = response {
                self.receive_message(generation, response, cx);
            } else {
                self.teardown_timeout(generation, cx);
            }
            assert!(self.closed && self.host.is_none() && self.teardown_timer.is_none());
            old_generation = generation;
            eprintln!(
                "MCP bridge audit: {} -> closed, host/deadline released",
                if response.is_some() { "response" } else { "explicit deadline" }
            );
        }
        self.reopen(cx);
        self.install_host(cx);
        let generation = self.generation;
        self.receive_message(generation, initialized, cx);
        self.receive_message(generation, teardown, cx);
        self.detach_host(cx);
        assert!(self.closed && self.host.is_none() && self.teardown_timer.is_none());
        eprintln!("MCP bridge audit: detach disposes immediately; graceful detached cleanup remains a gap");
        eprintln!("MCP bridge audit: offscreen only; WebKit transport not exercised");
    }
}

impl Render for McpFrame {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_theme(cx);
        if self.display_mode == DisplayMode::Fullscreen && self.awaiting_user(cx) {
            self.exit_fullscreen(cx);
        }
        let p = palette(cx);
        if self.closed {
            return div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .rounded(rem(8.))
                .border_1()
                .border_color(hsla(p.border))
                .px(rem(12.))
                .py(rem(8.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(format!("The {} app was closed", self.entry.reference.server))
                .child(super::parts::ghost_text_button(
                    "mcp-reopen",
                    "Show app",
                    None,
                    cx.listener(|this, _, _, cx| {
                        this.reopen(cx);
                    }),
                    cx,
                ))
                .into_any_element();
        }
        if self.display_mode == DisplayMode::Fullscreen {
            // Preserve inline row height during fullscreen to prevent timeline jumps.
            return div().w_full().h(px(self.height as f32)).into_any_element();
        }
        let body = if self.html.is_some() {
            FrameElement { frame: cx.entity() }.into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .when(self.failed, |d| d.child(format!("Unable to load the {} app", self.entry.reference.server)))
                .into_any_element()
        };
        div()
            .w_full()
            .h(px(self.height as f32))
            .overflow_hidden()
            .when(self.entry.reference.prefers_border, |d| {
                d.rounded(rem(8.)).border_1().border_color(hsla(p.border))
            })
            .child(body)
            .into_any_element()
    }
}

struct McpFullscreen {
    frame: Entity<McpFrame>,
    focus: FocusHandle,
    restore: Option<FocusHandle>,
}

impl Focusable for McpFullscreen {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl McpFullscreen {
    fn is_valid(&self, cx: &App) -> bool {
        self.frame.read(cx).fullscreen_still_valid(cx)
    }

    fn close(&self, _: Option<&mut Window>, cx: &mut Context<Self>) {
        self.frame.update(cx, |frame, cx| frame.exit_fullscreen(cx));
    }
}

impl Render for McpFullscreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let (server, view) = {
            let frame = self.frame.read(cx);
            (frame.entry.reference.server.clone(), frame.view.clone())
        };
        let page = match view {
            Some(view) => webview::web_view("mcp-app-fullscreen-page", view, dark).into_any_element(),
            None => div().size_full().into_any_element(),
        };
        div()
            .id("mcp-app-fullscreen")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(Some(window), cx);
                }
            }))
            .absolute()
            .inset_0()
            .size_full()
            .occlude()
            .bg(hsla(p.background))
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(webview::occluder("mcp-app-fullscreen-cover", Layer::Modal))
            .child(
                div()
                    .h(px(FULLSCREEN_BAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(rem(12.))
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .child(div().min_w_0().truncate().text_sm().text_color(hsla(p.foreground)).child(server))
                    .child(
                        div()
                            .id("mcp-app-fullscreen-exit")
                            .h(px(28.))
                            .w(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(rem(6.))
                            .cursor_pointer()
                            .hover(|this| this.bg(hsla(p.accent)))
                            .on_click(cx.listener(|this, _, window, cx| this.close(Some(window), cx)))
                            .child(Icon::Minimize2.render(px(16.), hsla(p.foreground))),
                    ),
            )
            .child(div().flex_1().min_h_0().w_full().child(page))
    }
}

struct FrameElement {
    frame: Entity<McpFrame>,
}
impl IntoElement for FrameElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for FrameElement {
    type RequestLayoutState = ();
    type PrepaintState = AnyElement;
    fn id(&self) -> Option<ElementId> {
        Some("mcp-page".into())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = gpui::Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let view = self.frame.update(cx, |frame, cx| frame.inline_view(f32::from(bounds.size.width), window, cx));
        let mut child = if let Some(view) = view {
            webview::web_view("app", view, self.frame.read(cx).context["theme"] == "dark").into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .flex_col()
                .gap(rem(4.))
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hsla(palette(cx).muted_foreground))
                .child(format!("{} app", self.frame.read(cx).entry.reference.server))
                .child("MCP apps need an active macOS window.")
                .into_any_element()
        };
        child.prepaint_as_root(bounds.origin, bounds.size.map(AvailableSpace::Definite), window, cx);
        child
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        child: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        child.paint(window, cx);
    }
}

fn host_context(display_mode: DisplayMode, inline_width: f32, fullscreen_size: Option<(f32, f32)>, tool: Option<&Value>, cx: &App) -> Value {
    let p = palette(cx);
    let pairs = [
        ("--color-background-primary", p.background),
        ("--color-background-secondary", p.card),
        ("--color-background-tertiary", p.muted),
        ("--color-background-inverse", p.foreground),
        ("--color-background-info", p.info),
        ("--color-background-danger", p.error_surface),
        ("--color-background-warning", p.warning_surface),
        ("--color-text-primary", p.foreground),
        ("--color-text-secondary", p.muted_foreground),
        ("--color-text-tertiary", p.muted_foreground),
        ("--color-text-inverse", p.background),
        ("--color-text-info", p.info_foreground),
        ("--color-text-danger", p.destructive_foreground),
        ("--color-text-success", p.success_foreground),
        ("--color-text-warning", p.warning_foreground),
        ("--color-border-primary", p.border),
        ("--color-border-secondary", p.input),
        ("--color-border-danger", p.destructive),
        ("--color-ring-primary", p.ring),
    ];
    let mut variables: serde_json::Map<String, Value> = pairs.into_iter().map(|(key, color)| (key.into(), json!(format!("#{color:08x}")))).collect();
    variables.insert("--font-sans".into(), json!(cx.theme().font_family.as_ref()));
    variables.insert("--font-mono".into(), json!(cx.theme().mono_font_family.as_ref()));
    variables.insert(
        "--border-radius-md".into(),
        json!(format!(
            "{}px",
            crate::theme::metrics::RADIUS * crate::theme::preferences(cx).interface_font_size / crate::theme::metrics::ROOT_FONT_SIZE
        )),
    );
    let dimensions = match (display_mode, fullscreen_size) {
        (DisplayMode::Fullscreen, Some((width, height))) => json!({"width": width.round(), "height": height.round()}),
        _ => json!({"width": inline_width.round(), "maxHeight": MAX_HEIGHT}),
    };
    let mut context = json!({
        "theme": if cx.theme().is_dark() { "dark" } else { "light" },
        "styles": { "variables": variables },
        "displayMode": display_mode.as_str(),
        "availableDisplayModes": ["inline", "fullscreen"],
        "containerDimensions": dimensions,
        "platform": "desktop",
        "userAgent": format!("t3-code/{}", env!("CARGO_PKG_VERSION")),
        "deviceCapabilities": { "touch": false, "hover": true },
        "safeAreaInsets": { "top": 0, "right": 0, "bottom": 0, "left": 0 },
    });
    if let Some(tool) = tool {
        context["toolInfo"] = json!({ "tool": tool });
    }
    context
}

#[cfg(feature = "snapshot")]
pub mod fixture {
    use std::borrow::Cow;
    use std::path::PathBuf;
    use std::sync::{Arc, OnceLock};

    use t3_core::thread::{ProjectedTurnItem, Visibility};
    use t3_core::turn_item::TurnItem;

    const ATTACHMENT_ID: &str = "native-mcp-fixture.html";
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    static ITEM: OnceLock<Option<Arc<TurnItem>>> = OnceLock::new();

    pub fn set(path: PathBuf) {
        let _ = PATH.set(Some(path));
    }

    fn path() -> Option<&'static PathBuf> {
        PATH.get_or_init(|| std::env::var_os("T3_NATIVE_MCP_FIXTURE").map(PathBuf::from)).as_ref()
    }

    pub(super) fn page(attachment_id: &str) -> Option<&'static PathBuf> {
        path().filter(|_| attachment_id == ATTACHMENT_ID)
    }

    pub(super) fn audit_enabled() -> bool {
        std::env::var("T3_NATIVE_MCP_TEARDOWN_AUDIT").as_deref() == Ok("1")
    }

    fn item() -> Option<Arc<TurnItem>> {
        path()?;
        ITEM.get_or_init(|| {
            let value = serde_json::json!({
                "id": "native-mcp-fixture", "threadId": "fixture", "runId": null, "nodeId": null, "parentItemId": null,
                "ordinal": u32::MAX, "status": "completed", "title": null,
                "startedAt": "2026-01-01T00:00:00.000Z", "completedAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
                "type": "dynamic_tool", "toolName": "weather.get_weather", "input": {"city":"Oslo"}, "outputOmitted": false,
                "output": { "t3McpApp": { "attachmentId": ATTACHMENT_ID, "server": "weather", "tool": "get_weather", "resourceUri": "ui://weather/dashboard", "prefersBorder": true }, "result": {"content":[{"type":"text","text":"Sunny"}]} },
            });
            serde_json::from_value(value).ok().map(Arc::new)
        })
        .clone()
    }

    /// The thread's visible items, with the fixture page last when one is set.
    pub fn with_page(items: &[ProjectedTurnItem]) -> Cow<'_, [ProjectedTurnItem]> {
        let Some(item) = item() else { return Cow::Borrowed(items) };
        let mut items = items.to_vec();
        items.push(ProjectedTurnItem {
            position: items.len(),
            visibility: Visibility::Local,
            source_thread_id: item.thread_id.clone(),
            source_item_id: item.id.clone(),
            item,
        });
        Cow::Owned(items)
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::size;
    use t3_core::client_settings::AppearanceMode;

    fn test_entry(id: &str) -> Arc<McpAppEntry> {
        let item: TurnItem = serde_json::from_value(json!({
            "id": id,
            "threadId": "thread-1",
            "runId": null,
            "nodeId": null,
            "parentItemId": null,
            "ordinal": 1,
            "status": "completed",
            "title": null,
            "startedAt": "2026-01-01T00:00:00.000Z",
            "completedAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z",
            "type": "dynamic_tool",
            "toolName": "weather.get_weather",
            "input": {"city": "Oslo"},
            "outputOmitted": false,
            "output": {
                "t3McpApp": {
                    "attachmentId": format!("{id}.html"),
                    "server": "weather",
                    "tool": "get_weather",
                    "resourceUri": "ui://weather/dashboard",
                    "prefersBorder": true
                }
            }
        }))
        .unwrap();
        let reference = t3_core::mcp_apps::read_reference(&json!({
            "attachmentId": format!("{id}.html"),
            "server": "weather",
            "tool": "get_weather",
            "resourceUri": "ui://weather/dashboard",
            "prefersBorder": true
        }))
        .unwrap();
        Arc::new(McpAppEntry {
            run_id: None,
            reference,
            source_thread_id: "thread-1".into(),
            source_item_id: id.into(),
            item: Arc::new(item),
        })
    }

    fn setup_test_state(
        cx: &mut gpui::TestAppContext,
    ) -> (
        tokio::runtime::Runtime,
        Entity<AppState>,
        t3_client::supervisor::ClientHandle,
        String,
        t3_client::supervisor::ClientFixture,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, env) = cx.update(|cx| {
            init(&state, cx);
            let client = state.read(cx).client_handle().unwrap();
            let env = client.environment().to_owned();
            (client, env)
        });
        (runtime, state, client, env, fixture)
    }

    #[gpui::test]
    async fn fullscreen_guards_step_aside_teardown_and_live_theme_updates(cx: &mut gpui::TestAppContext) {
        let (_runtime, state, client, env, _fixture) = setup_test_state(cx);
        cx.update(|cx| {
            state.update(cx, |s, _| {
                s.selected_thread = Some("thread-1".into());
                let projection: t3_core::thread::ThreadProjection = serde_json::from_value(json!({
                    "thread": {
                        "id": "thread-1", "projectId": "p-1", "title": "T",
                        "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "codex", "modelSelection": { "instanceId": "codex", "model": "gpt-5" },
                        "runtimeMode": "full-access", "interactionMode": "default", "lineage": { "rootThreadId": "thread-1" }
                    },
                    "runs": [], "attempts": [], "nodes": [], "subagents": [],
                    "providerSessions": [], "providerThreads": [], "providerTurns": [],
                    "runtimeRequests": [], "messages": [], "plans": [], "turnItems": [],
                    "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [],
                    "contextTransfers": [], "visibleTurnItems": [],
                    "updatedAt": "2026-01-01T00:00:00.000Z"
                }))
                .unwrap();
                s.thread.projection = Some(projection);
            });
        });
        let window = cx.open_window(size(px(1024.), px(768.)), |_, cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));

        let (frame, other) = cx.update(|cx| {
            assert!(!is_fullscreen_open(cx));
            let frame = cx.new(|cx| McpFrame::new(env.clone(), "thread-1".into(), None, state.clone(), &client, test_entry("item-1"), cx));
            let other = cx.new(|cx| McpFrame::new(env.clone(), "thread-1".into(), None, state.clone(), &client, test_entry("item-2"), cx));
            (frame, other)
        });

        // Cannot enter fullscreen without live WebView activation.
        cx.update(|cx| {
            frame.update(cx, |f, cx| {
                assert!(!f.try_enter_fullscreen(cx));
            });
            assert!(!is_fullscreen_open(cx));
        });

        // Simulate entering fullscreen on the window and initializing the host bridge.
        cx.update(|cx| {
            frame.update(cx, |f, cx| {
                f.install_host(cx);
                let generation = f.generation;
                f.receive_message(generation, r#"{"jsonrpc":"2.0","method":"ui/notifications/initialized"}"#, cx);
                assert!(f.enter_fullscreen_on_window(window.into(), cx));
                assert_eq!(f.display_mode, DisplayMode::Fullscreen);
                assert_eq!(f.context["displayMode"], "fullscreen");
                assert!(f.context["containerDimensions"].get("height").is_some());
                assert!(f.context["containerDimensions"].get("maxHeight").is_none());

                // Size notifications are ignored in fullscreen.
                f.receive_message(generation, r#"{"jsonrpc":"2.0","method":"ui/notifications/size-changed","params":{"height":120}}"#, cx);
                assert_eq!(f.height, DEFAULT_HEIGHT);
            });
            assert!(is_fullscreen_open(cx));

            // Second frame cannot take fullscreen while the first is open.
            other.update(cx, |o, cx| {
                assert!(!o.try_enter_fullscreen(cx));
            });
        });

        // `request-teardown` in fullscreen exits back to inline instead of closing.
        cx.update(|cx| {
            frame.update(cx, |f, cx| {
                let generation = f.generation;
                f.receive_message(generation, r#"{"jsonrpc":"2.0","method":"ui/notifications/request-teardown"}"#, cx);
                assert!(!f.closed);
                assert!(f.host.is_some());
                assert_eq!(f.display_mode, DisplayMode::Inline);
                assert_eq!(f.context["displayMode"], "inline");
            });
            assert!(!is_fullscreen_open(cx));
        });

        // Pending approval/input causes reactive step-aside from fullscreen back to inline.
        cx.update(|cx| {
            frame.update(cx, |f, cx| {
                assert!(f.enter_fullscreen_on_window(window.into(), cx));
            });
            assert!(is_fullscreen_open(cx));
            state.update(cx, |s, cx| {
                let proj = s.thread.projection.as_mut().unwrap();
                let req: t3_core::thread::Entity = serde_json::from_value(json!({
                    "id": "req-1",
                    "kind": "command_approval",
                    "status": "pending",
                    "responseCapability": { "type": "live" },
                    "createdAt": "2026-01-01T00:00:00.000Z"
                }))
                .unwrap();
                proj.runtime_requests.push(req);
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!is_fullscreen_open(cx));
            let f = frame.read(cx);
            assert_eq!(f.display_mode, DisplayMode::Inline);
            assert!(!f.closed);
        });

        // Live theme revision updates sync context.
        cx.update(|cx| {
            let mut prefs = crate::theme::preferences(cx).clone();
            prefs.mode = if frame.read(cx).context["theme"] == "dark" {
                AppearanceMode::Light
            } else {
                AppearanceMode::Dark
            };
            crate::theme::set_preferences(prefs, cx);
            frame.update(cx, |f, cx| f.sync_theme(cx));
            let expected = if cx.theme().is_dark() { "dark" } else { "light" };
            assert_eq!(frame.read(cx).context["theme"], expected);
        });
    }

    #[gpui::test]
    async fn confirmation_dialog_steps_aside_cancels_and_guards_reentry(cx: &mut gpui::TestAppContext) {
        let (_runtime, state, client, env, _fixture) = setup_test_state(cx);
        let window = cx.open_window(size(px(800.), px(600.)), |_, cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
        let dialog_entity = window.entity(cx).unwrap();

        let frame = cx.update(|cx| cx.new(|cx| McpFrame::new(env, "thread-1".into(), None, state.clone(), &client, test_entry("item-dlg"), cx)));
        let decisions = Rc::new(std::cell::RefCell::new(Vec::new()));
        let d1 = decisions.clone();
        let d2 = decisions.clone();

        cx.update(|cx| {
            frame.update(cx, |f, cx| {
                assert!(f.enter_fullscreen_on_window(window.into(), cx));
                f.ask_user(
                    "Save report.csv from weather?",
                    "",
                    "Save",
                    move |_, res, _| d1.borrow_mut().push(res),
                    cx,
                );
                assert_eq!(f.display_mode, DisplayMode::Inline);
            });
            assert!(!is_fullscreen_open(cx));
            assert!(crate::dialog::is_open(cx));

            // Re-entrant confirmation returns refusal immediately without clobbering open dialog.
            frame.update(cx, |f, cx| {
                f.ask_user(
                    "Second?",
                    "",
                    "Allow",
                    move |_, res, _| d2.borrow_mut().push(res),
                    cx,
                );
            });
        });

        let _ = cx.update_window(window.into(), |_, window, cx| {
            dialog_entity.update(cx, |d, cx| d.resolve_for_test(false, window, cx));
        });
        cx.update(|cx| {
            assert!(!crate::dialog::is_open(cx));
        });
        assert_eq!(
            *decisions.borrow(),
            vec![
                Err("T3 could not ask for approval here.".into()),
                Ok(false),
            ]
        );
    }

    #[gpui::test]
    async fn side_chat_routing_and_shell_runtime_request_step_aside(cx: &mut gpui::TestAppContext) {
        let (_runtime, state, client, env, _fixture) = setup_test_state(cx);
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                let mut shell = s.shell.clone();
                shell.snapshot.as_mut().unwrap().threads = vec![
                    serde_json::from_value(json!({
                        "id": "main-1", "projectId": "checkout-a", "title": "Main",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"main-1"},
                        "status": "idle", "hasActionableProposedPlan": false, "itemCount": 1, "visibleItemCount": 1
                    }))
                    .unwrap(),
                    serde_json::from_value(json!({
                        "id": "side-1", "projectId": "checkout-a", "title": "Side",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"main-1","parentThreadId":"main-1"},
                        "status": "idle", "hasActionableProposedPlan": false, "itemCount": 1, "visibleItemCount": 1
                    }))
                    .unwrap(),
                ];
                s.install_active_environment(
                    client.clone(),
                    shell,
                    s.server_config.clone(),
                    t3_client::supervisor::ConnectionStatus::Connected,
                    cx,
                );
                s.selected_thread = Some("main-1".into());
                s.panels.panels = serde_json::from_value(json!({
                    "threads": {
                        "main-1": {
                            "isOpen": true,
                            "activeSurfaceId": "side-1",
                            "surfaces": [
                                { "id": "side-1", "kind": "side-chat", "threadId": "side-1" },
                                { "id": "diff", "kind": "diff" }
                            ]
                        }
                    }
                }))
                .unwrap();
            });
        });

        let side_window = cx.open_window(size(px(900.), px(700.)), |window, cx| ThreadView::embedded(state.clone(), "side-1".into(), window, cx));
        let side_view = side_window.entity(cx).unwrap();
        let side_frame = cx.update(|cx| {
            let owner = side_view.read(cx).html_page_origin(cx).map(|origin| FrameOwner {
                origin,
                embedded: true,
                view: side_view.downgrade(),
            });
            cx.new(|cx| McpFrame::new(env, "side-1".into(), owner, state.clone(), &client, test_entry("item-side"), cx))
        });

        cx.update(|cx| {
            assert_eq!(side_frame.read(cx).conversation_thread_id(), "side-1");
            side_frame.update(cx, |f, cx| {
                // Inline handle_escape is a no-op.
                f.handle_escape(cx);
                assert_eq!(f.display_mode, DisplayMode::Inline);
                assert!(f.enter_fullscreen_on_window(side_window.into(), cx));
                assert_eq!(f.display_mode, DisplayMode::Fullscreen);
            });
            assert!(is_fullscreen_open(cx));

            // Escape exits fullscreen back to inline.
            side_frame.update(cx, |f, cx| f.handle_escape(cx));
            assert_eq!(side_frame.read(cx).display_mode, DisplayMode::Inline);
            assert!(!is_fullscreen_open(cx));

            // Re-enter fullscreen for request routing test.
            side_frame.update(cx, |f, cx| assert!(f.enter_fullscreen_on_window(side_window.into(), cx)));
            assert!(is_fullscreen_open(cx));

            // Pending shell runtime request on parent `main-1` does NOT dismiss `side-1` fullscreen.
            state.update(cx, |s, cx| {
                s.shell.snapshot.as_mut().unwrap().threads[0].pending_runtime_request =
                    Some(serde_json::from_value(json!({"id":"secret-main","kind":"secret"})).unwrap());
                cx.emit(AppEvent::Shell);
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(is_fullscreen_open(cx));
            assert_eq!(side_frame.read(cx).display_mode, DisplayMode::Fullscreen);

            // Shell-only pending_runtime_request (e.g. secret/link prompt) on `side-1` steps fullscreen aside.
            state.update(cx, |s, cx| {
                s.shell.snapshot.as_mut().unwrap().threads[1].pending_runtime_request =
                    Some(serde_json::from_value(json!({"id":"secret-side","kind":"secret"})).unwrap());
                cx.emit(AppEvent::Shell);
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!is_fullscreen_open(cx));
            assert_eq!(side_frame.read(cx).display_mode, DisplayMode::Inline);
        });
    }

    #[gpui::test]
    async fn fullscreen_exits_on_owner_thread_switch_and_side_chat_hide(cx: &mut gpui::TestAppContext) {
        let (_runtime, state, client, env, _fixture) = setup_test_state(cx);
        cx.update(|cx| {
            state.update(cx, |s, _| {
                s.selected_thread = Some("main-1".into());
                s.panels.panels = serde_json::from_value(json!({
                    "threads": {
                        "main-1": {
                            "isOpen": true,
                            "activeSurfaceId": "side-1",
                            "surfaces": [
                                { "id": "side-1", "kind": "side-chat", "threadId": "side-1" },
                                { "id": "diff", "kind": "diff" }
                            ]
                        }
                    }
                }))
                .unwrap();
            });
        });

        let main_window = cx.open_window(size(px(900.), px(700.)), |window, cx| ThreadView::new(state.clone(), window, cx));
        let main_view = main_window.entity(cx).unwrap();
        let main_frame = cx.update(|cx| {
            let owner = main_view.read(cx).html_page_origin(cx).map(|origin| FrameOwner {
                origin,
                embedded: false,
                view: main_view.downgrade(),
            });
            cx.new(|cx| McpFrame::new(env.clone(), "main-1".into(), owner, state.clone(), &client, test_entry("item-main"), cx))
        });

        cx.update(|cx| {
            main_frame.update(cx, |f, cx| assert!(f.enter_fullscreen_on_window(main_window.into(), cx)));
            assert!(is_fullscreen_open(cx));
            state.update(cx, |s, cx| {
                s.selected_thread = Some("main-2".into());
                cx.emit(AppEvent::Selection);
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!is_fullscreen_open(cx));
            assert_eq!(main_frame.read(cx).display_mode, DisplayMode::Inline);
        });

        // Switch back to main-1 and test embedded side-chat fullscreen invalidation on tab switch & panel close.
        let side_window = cx.open_window(size(px(900.), px(700.)), |window, cx| ThreadView::embedded(state.clone(), "side-1".into(), window, cx));
        let side_view = side_window.entity(cx).unwrap();
        let side_frame = cx.update(|cx| {
            state.update(cx, |s, _| s.selected_thread = Some("main-1".into()));
            let owner = side_view.read(cx).html_page_origin(cx).map(|origin| FrameOwner {
                origin,
                embedded: true,
                view: side_view.downgrade(),
            });
            cx.new(|cx| McpFrame::new(env, "side-1".into(), owner, state.clone(), &client, test_entry("item-side-2"), cx))
        });

        cx.update(|cx| {
            side_frame.update(cx, |f, cx| assert!(f.enter_fullscreen_on_window(side_window.into(), cx)));
            assert!(is_fullscreen_open(cx));
            state.update(cx, |s, cx| {
                s.panels.panels = serde_json::from_value(json!({
                    "threads": {
                        "main-1": {
                            "isOpen": true,
                            "activeSurfaceId": "diff",
                            "surfaces": [
                                { "id": "side-1", "kind": "side-chat", "threadId": "side-1" },
                                { "id": "diff", "kind": "diff" }
                            ]
                        }
                    }
                }))
                .unwrap();
                cx.emit(AppEvent::RightPanel);
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!is_fullscreen_open(cx));
            assert_eq!(side_frame.read(cx).display_mode, DisplayMode::Inline);

            // Re-activate side-1, enter fullscreen, then close right panel -> fullscreen exits.
            state.update(cx, |s, _| {
                s.panels.panels = serde_json::from_value(json!({
                    "threads": {
                        "main-1": {
                            "isOpen": true,
                            "activeSurfaceId": "side-1",
                            "surfaces": [
                                { "id": "side-1", "kind": "side-chat", "threadId": "side-1" },
                                { "id": "diff", "kind": "diff" }
                            ]
                        }
                    }
                }))
                .unwrap();
            });
            side_frame.update(cx, |f, cx| assert!(f.enter_fullscreen_on_window(side_window.into(), cx)));
            assert!(is_fullscreen_open(cx));
            state.update(cx, |s, cx| {
                s.panels.panels = serde_json::from_value(json!({
                    "threads": {
                        "main-1": {
                            "isOpen": false,
                            "activeSurfaceId": "side-1",
                            "surfaces": [
                                { "id": "side-1", "kind": "side-chat", "threadId": "side-1" },
                                { "id": "diff", "kind": "diff" }
                            ]
                        }
                    }
                }))
                .unwrap();
                cx.emit(AppEvent::RightPanel);
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!is_fullscreen_open(cx));
            assert_eq!(side_frame.read(cx).display_mode, DisplayMode::Inline);
        });
    }
}
