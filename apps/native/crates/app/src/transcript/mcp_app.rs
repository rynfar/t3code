//! Inline MCP apps, ported from `McpAppFrame.tsx` and `McpAppTimelineRow`.
//! WebKit only transports messages; the protocol host lives in `t3_core::mcp_apps`.

use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt, channel::mpsc};
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Context, Element, ElementId, Entity, Global, GlobalElementId, InspectorElementId, LayoutId, Pixels, Task, Window,
    div, prelude::*, px, relative,
};
use gpui_component::ActiveTheme;
use serde_json::{Value, json};
use t3_core::mcp_apps::{Action, DEFAULT_HEIGHT, Host, MAX_HEIGHT, Operation, TEARDOWN_TIMEOUT_MS};
use t3_core::timeline::McpAppEntry;
use t3_core::turn_item::{TurnItem, TurnItemKind};

use crate::app_state::AppState;
use crate::theme::{hsla, palette, rem};
use crate::thread_view::ThreadView;
use crate::webview::{self, Layer, Page, WebEvent, WebView};

const MAX_PAGES: usize = 24;
const MAX_VIEWS: usize = 6;
const TRANSPORT: &str = include_str!("mcp_app_transport.js");
type Key = (String, String, String, String, gpui::EntityId);

struct Frames {
    state: Entity<AppState>,
    pages: HashMap<Key, (u64, Entity<McpFrame>)>,
    views: Vec<Weak<WebView>>,
    clock: u64,
}
impl Global for Frames {}

pub fn init(state: &Entity<AppState>, cx: &mut App) {
    if !cx.has_global::<Frames>() {
        cx.set_global(Frames {
            state: state.clone(),
            pages: HashMap::new(),
            views: Vec::new(),
            clock: 0,
        });
    }
}

pub fn frame_row(_: &str, entry: &Arc<McpAppEntry>, cx: &mut Context<ThreadView>) -> AnyElement {
    let Some(frames) = cx.try_global::<Frames>() else {
        return div().into_any_element();
    };
    let Some(environment) = frames.state.read(cx).environment() else {
        return div().into_any_element();
    };
    let key = (
        environment.to_owned(),
        entry.source_thread_id.clone(),
        entry.source_item_id.clone(),
        entry.reference.attachment_id.clone(),
        cx.entity_id(),
    );
    let state = frames.state.clone();
    let frame = if let Some((_, frame)) = frames.pages.get(&key) {
        frame.clone()
    } else {
        let entry = entry.clone();
        let Some(client) = state.read(cx).client_handle() else {
            return div().into_any_element();
        };
        let frame = cx.new(|cx| McpFrame::new(state, &client, entry, cx));
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
            .filter(|(k, (_, f))| *k != &key && f.read(cx).view.strong_count() == 0)
            .min_by_key(|(_, (used, _))| *used)
            .map(|(k, _)| k.clone());
        let Some(oldest) = oldest else { break };
        cx.global_mut::<Frames>().pages.remove(&oldest);
    }
    frame.update(cx, |frame, cx| frame.update_entry(entry, cx));
    div().w_full().px(rem(4.)).child(frame).into_any_element()
}

struct McpFrame {
    connection: t3_client::mcp_apps::AppConnection,
    state: Entity<AppState>,
    runtime: tokio::runtime::Handle,
    entry: Arc<McpAppEntry>,
    html: Option<Arc<str>>,
    failed: bool,
    height: u32,
    width: f32,
    context: Value,
    tool: Option<Value>,
    detail: Option<Arc<TurnItem>>,
    host: Option<Host>,
    view: Weak<WebView>,
    generation: u64,
    closed: bool,
    load: Option<Task<()>>,
    detail_task: Option<Task<()>>,
    events: Option<Task<()>>,
    teardown_timer: Option<Task<()>>,
}

impl McpFrame {
    fn new(state: Entity<AppState>, client: &t3_client::supervisor::ClientHandle, entry: Arc<McpAppEntry>, cx: &mut Context<Self>) -> Self {
        let runtime = state.read(cx).runtime().clone();
        let mut frame = Self {
            connection: t3_client::mcp_apps::AppConnection::new(client),
            state,
            runtime,
            entry,
            html: None,
            failed: false,
            height: DEFAULT_HEIGHT,
            width: 0.,
            context: host_context(cx),
            tool: None,
            detail: None,
            host: None,
            view: Weak::new(),
            generation: 0,
            closed: false,
            load: None,
            detail_task: None,
            events: None,
            teardown_timer: None,
        };
        frame.load(cx);
        frame.fetch_detail(cx);
        #[cfg(feature = "snapshot")]
        if webview::is_offscreen() && fixture::page(&frame.entry.reference.attachment_id).is_some() && fixture::audit_enabled() {
            frame.audit_teardown(cx);
        }
        frame
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
        self.context["containerDimensions"]["width"] = json!(self.width.round());
        if let Some(tool) = &self.tool {
            self.context["toolInfo"] = json!({"tool":tool});
        }
        if let Some(host) = &mut self.host {
            let actions = host.update_context(self.context.clone());
            self.actions(actions, cx);
        }
    }

    fn dispose_host(&mut self) {
        self.generation += 1;
        self.teardown_timer = None;
        self.events = None;
        self.view = Weak::new();
        if let Some(mut host) = self.host.take() {
            host.dispose();
        }
    }

    fn install_host(&mut self, cx: &mut Context<Self>) {
        self.dispose_host();
        self.host = Some(Host::new(self.entry.reference.clone(), self.context.clone(), env!("CARGO_PKG_VERSION")));
        self.deliver_tool(cx);
    }

    fn detach_host(&mut self, cx: &mut Context<Self>) {
        if self.teardown_timer.is_some() {
            self.closed = true;
            cx.notify();
        }
        // The viewport owns the WebView; a detached document cannot acknowledge cleanup.
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
                    if let Some(view) = self.view.upgrade() {
                        view.evaluate(&format!("window.__t3McpReceive({message});"));
                    }
                }
                Action::Height(height) => {
                    if self.height != height {
                        self.height = height;
                        cx.notify();
                    }
                }
                Action::Close => {
                    self.closed = true;
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
                Action::Request {
                    token,
                    operation: Operation::OpenLink { url },
                } => {
                    let result = if self.view.upgrade().is_some_and(|v| v.take_activation()) {
                        cx.open_url(&url);
                        Ok(json!({}))
                    } else {
                        Err("Links open only from a click in the app.".into())
                    };
                    if let Some(host) = &mut self.host {
                        let actions = host.complete(token, result);
                        self.actions(actions, cx);
                    }
                }
                Action::Request { token, operation } => {
                    let Some(client) = self.client(cx) else {
                        if let Some(host) = &mut self.host {
                            let actions = host.complete(token, Err("The app's environment is not connected.".into()));
                            self.actions(actions, cx);
                        }
                        continue;
                    };
                    let target = self.target();
                    let generation = self.generation;
                    let job = self
                        .runtime
                        .spawn(async move { t3_client::mcp_apps::execute(&client, &target, operation).await });
                    cx.spawn(async move |this, cx| {
                        let result = job.await.unwrap_or_else(|e| Err(e.to_string()));
                        let _ = this.update(cx, |frame, cx| {
                            if frame.generation == generation
                                && let Some(host) = &mut frame.host
                            {
                                let actions = host.complete(token, result);
                                frame.actions(actions, cx);
                            }
                        });
                    })
                    .detach();
                }
            }
        }
    }

    fn inline_view(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> Option<Rc<WebView>> {
        if (self.width - width).abs() > 0.5 {
            self.width = width;
            self.update_context(cx);
        }
        if let Some(view) = self.view.upgrade() {
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
        let frames = cx.global_mut::<Frames>();
        frames.views.retain(|v| v.strong_count() > 0);
        if frames.views.len() >= MAX_VIEWS {
            return None;
        }
        let html = self.html.clone()?;
        self.context = host_context(cx);
        self.update_context(cx);
        let dark = self.context["theme"] == "dark";
        let page = Page {
            html,
            dark,
            scroll_through: false,
            script: TRANSPORT.into(),
            allow_popups: true,
            content_security_policy: Some(self.entry.reference.content_security_policy()),
        };
        let (events, mut received) = mpsc::unbounded();
        let view = WebView::new(page, Layer::Transcript, events, window, cx)?;
        self.install_host(cx);
        let generation = self.generation;
        self.view = Rc::downgrade(&view);
        cx.global_mut::<Frames>().views.push(self.view.clone());
        self.events = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = received.next().await {
                if this
                    .update(cx, |frame, cx| {
                        if frame.generation != generation {
                            return;
                        }
                        match event {
                            WebEvent::Message(text) => frame.receive_message(generation, &text, cx),
                            WebEvent::OpenUrl(url) => cx.open_url(&url),
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

fn host_context(cx: &App) -> Value {
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
    json!({"theme":if cx.theme().is_dark() {"dark"} else {"light"},"styles":{"variables":variables},"displayMode":"inline","availableDisplayModes":["inline"],"containerDimensions":{"width":0,"maxHeight":MAX_HEIGHT},"platform":"desktop","userAgent":format!("t3-code/{}",env!("CARGO_PKG_VERSION")),"deviceCapabilities":{"touch":false,"hover":true},"safeAreaInsets":{"top":0,"right":0,"bottom":0,"left":0}})
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
