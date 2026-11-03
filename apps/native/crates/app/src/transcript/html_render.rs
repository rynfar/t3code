//! Agent HTML in the transcript: `HtmlRenderFrame.tsx`, its full-screen `HtmlRenderDialog.tsx`,
//! and the page host of `HtmlRenderDocument` (`BrowserDocumentFrame.tsx`). The page is fetched
//! as an inline attachment on the network runtime and shown in a [`WebView`] while its row is on
//! screen; the frame fits the page's reported height (`htmlRenderFrameHeight`).
//!
//! The page's own bootstrap only reports its height to a parent frame, so the view adds a
//! document-start script that reports it the same way through `window.ipc`. The bootstrap's
//! default theme is the shared T3 palette, which the web overrides with the app's canvas
//! (`getStandardThemeColors`); the script sets the canvas the same way, and the view's appearance
//! picks light or dark. A view's control cannot draw over the page, so "Open full screen" sits
//! under the frame instead of on it.

use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, App, AppContext, AvailableSpace, Bounds, Context, Element, ElementId, Entity, EntityId, FocusHandle, Focusable, FontWeight, Global, GlobalElementId,
    InspectorElementId, IntoElement, KeyDownEvent, LayoutId, MouseButton, ParentElement, Pixels, Render, SharedString, Styled, Task, Window, div, hsla as gpui_hsla,
    prelude::*, px, relative,
};
use gpui_component::ActiveTheme;
use t3_core::html_render::{self, HtmlRenderReference};
use t3_core::media::MediaSource;
use t3_core::timeline::HtmlRenderEntry;

use super::parts::{outline_button, tooltip_icon_button};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::theme::{hsla, palette, rem};
use crate::thread_view::ThreadView;
use crate::webview::{self, Layer, Page, WebEvent, WebView};

/// Inline pages alive at once; past it a frame shows its card until one scrolls away.
const MAX_INLINE_VIEWS: usize = 6;
/// Fetched pages kept for rows that scroll back.
const MAX_PAGES: usize = 32;
const RETRY_AFTER: Duration = Duration::from_secs(30);
const ZOOM_STEP: f32 = 0.25;
const ZOOM_RANGE: (f32, f32) = (0.5, 2.);

/// The bootstrap's height report (`injectHtmlRenderBootstrap`), for a page with no parent frame.
const CONTENT_HEIGHT_SCRIPT: &str = r#"(function(){if(window.parent!==window||!window.ipc)return;var h,o,z=function(){var r=document.documentElement;if(!r)return;var v=Math.ceil(r.scrollHeight>r.clientHeight?r.scrollHeight:r.getBoundingClientRect().height);if(v===h)return;h=v;window.ipc.postMessage(JSON.stringify({jsonrpc:"2.0",method:"ui/notifications/size-changed",params:{height:v}}));};if(window.ResizeObserver){o=new ResizeObserver(z);if(document.documentElement)o.observe(document.documentElement);}document.addEventListener("DOMContentLoaded",function(){if(o){o.observe(document.documentElement);if(document.body)o.observe(document.body);}z();});window.addEventListener("load",z);})();"#;

/// Puts `--background` right after the bootstrap's theme sheet, so the page's own rules still win.
fn canvas_script(canvas: u32) -> String {
    let canvas = serde_json::to_string(&css(canvas)).unwrap_or_default();
    format!(
        r#"(function(){{var c={canvas};function p(){{var s=document.getElementById("t3-theme");if(!s)return false;var e=document.createElement("style");e.id="t3-host-canvas";e.textContent=":root{{--background:"+c+"}}";s.after(e);return true;}}if(p())return;var o=new MutationObserver(function(){{if(p())o.disconnect();}});o.observe(document,{{childList:true,subtree:true}});document.addEventListener("DOMContentLoaded",function(){{o.disconnect();}});}})();"#
    )
}

fn canvas_update(canvas: u32) -> String {
    let canvas = serde_json::to_string(&css(canvas)).unwrap_or_default();
    format!(r#"(function(){{var e=document.getElementById("t3-host-canvas");if(e)e.textContent=":root{{--background:"+{canvas}+"}}";}})();"#)
}

/// The theme's canvas: `background` (design system §2.6).
fn canvas(cx: &App) -> u32 {
    palette(cx).background
}

fn css(color: u32) -> String {
    if color & 0xff == 0xff { format!("#{:06x}", color >> 8) } else { format!("#{color:08x}") }
}

type PageKey = (Arc<str>, String);

enum Html {
    Loading { _task: Task<()> },
    Ready(Arc<str>),
    Failed(Instant),
}

struct PageState {
    html: Html,
    content_height: Option<f32>,
    /// The frame's last laid-out width; the web starts at the column width too.
    width: f32,
    owners: Vec<EntityId>,
    used: u64,
}

/// The frame drawing a view holds it (its lease and the presented frame), so a view goes as soon
/// as no frame draws it: a row scrolled away, a thread switched, a transcript closed.
struct InlineView {
    key: PageKey,
    owner: EntityId,
    view: Weak<WebView>,
    canvas: u32,
    _events: Task<()>,
}

/// Fetched pages and the inline views showing them, shared by every transcript.
pub struct HtmlFrames {
    state: Entity<AppState>,
    pages: HashMap<PageKey, PageState>,
    views: Vec<InlineView>,
    fullscreen: Option<Entity<HtmlFullscreen>>,
    clock: u64,
}

struct Frames(Entity<HtmlFrames>);

impl Global for Frames {}

pub fn init(state: &Entity<AppState>, cx: &mut App) {
    if !cx.has_global::<Frames>() {
        let frames = cx.new(|_| HtmlFrames { state: state.clone(), pages: HashMap::new(), views: Vec::new(), fullscreen: None, clock: 0 });
        cx.set_global(Frames(frames));
    }
}

fn frames(cx: &App) -> Option<Entity<HtmlFrames>> {
    cx.try_global::<Frames>().map(|frames| frames.0.clone())
}

/// What a row draws this frame.
struct FrameState {
    html: Option<Arc<str>>,
    failed: bool,
    content_height: Option<f32>,
    width: f32,
}

impl HtmlFrames {
    fn key(&self, attachment_id: &str, cx: &App) -> Option<PageKey> {
        Some((Arc::from(self.state.read(cx).environment()?), attachment_id.to_owned()))
    }

    fn frame(&mut self, key: &PageKey, reference: &HtmlRenderReference, owner: EntityId, cx: &mut Context<Self>) -> FrameState {
        self.clock += 1;
        let clock = self.clock;
        let retry = self.pages.get(key).is_some_and(|page| matches!(page.html, Html::Failed(at) if at.elapsed() >= RETRY_AFTER));
        if (retry || !self.pages.contains_key(key))
            && let Some(html) = self.load(key.clone(), reference, cx)
        {
            let page = self.pages.entry(key.clone()).or_insert_with(|| PageState { html: Html::Failed(Instant::now()), content_height: None, width: html_render::COLUMN_WIDTH, owners: Vec::new(), used: clock });
            page.html = html;
            self.evict();
        }
        let Some(page) = self.pages.get_mut(key) else { return FrameState { html: None, failed: false, content_height: None, width: html_render::COLUMN_WIDTH } };
        page.used = clock;
        if !page.owners.contains(&owner) {
            page.owners.push(owner);
        }
        FrameState {
            html: match &page.html {
                Html::Ready(html) => Some(html.clone()),
                _ => None,
            },
            failed: matches!(page.html, Html::Failed(_)),
            content_height: page.content_height,
            width: page.width,
        }
    }

    /// Starts fetching the page; `None` while there is no connection to fetch it over.
    fn load(&mut self, key: PageKey, reference: &HtmlRenderReference, cx: &mut Context<Self>) -> Option<Html> {
        #[cfg(feature = "snapshot")]
        if let Some(path) = fixture::page(&key.1) {
            return Some(match std::fs::read(path) {
                Ok(bytes) => Html::Ready(Arc::from(String::from_utf8_lossy(&bytes).as_ref())),
                Err(_) => Html::Failed(Instant::now()),
            });
        }
        let state = self.state.read(cx);
        let client = state.client_handle()?;
        let source = MediaSource::Asset(reference.resource());
        let job = state.runtime().spawn(async move {
            let fetched = t3_client::assets::fetch_media(&client, &source).await?;
            Ok::<_, String>(Arc::<str>::from(String::from_utf8_lossy(&fetched.bytes).as_ref()))
        });
        let task = cx.spawn(async move |this, cx| {
            let result = job.await.unwrap_or_else(|_| Err(String::new()));
            let _ = this.update(cx, |frames, cx| {
                let Some(page) = frames.pages.get_mut(&key) else { return };
                page.html = match result {
                    Ok(html) => Html::Ready(html),
                    Err(error) => {
                        tracing::debug!(%error, "html render page unavailable");
                        Html::Failed(Instant::now())
                    }
                };
                frames.notify_owners(&key, cx);
            });
        });
        Some(Html::Loading { _task: task })
    }

    /// Past the budget, the pages used longest ago and shown by no view go.
    fn evict(&mut self) {
        while self.pages.len() > MAX_PAGES {
            let shown = |key: &PageKey| self.views.iter().any(|inline| &inline.key == key);
            let Some(oldest) = self.pages.iter().filter(|(key, _)| !shown(key)).min_by_key(|(_, page)| page.used).map(|(key, _)| key.clone()) else { break };
            self.pages.remove(&oldest);
        }
    }

    fn notify_owners(&self, key: &PageKey, cx: &mut Context<Self>) {
        let owners = self.pages.get(key).map(|page| page.owners.clone()).unwrap_or_default();
        let app: &mut App = cx;
        for owner in owners {
            app.notify(owner);
        }
    }

    fn set_width(&mut self, key: &PageKey, width: f32, cx: &mut Context<Self>) {
        let Some(page) = self.pages.get_mut(key) else { return };
        if (page.width - width).abs() > 0.5 {
            page.width = width;
            if page.content_height.is_none() {
                self.notify_owners(key, cx);
            }
        }
    }

    fn on_event(&mut self, key: &PageKey, event: WebEvent, cx: &mut Context<Self>) {
        match event {
            WebEvent::Message(text) => {
                let Some(height) = serde_json::from_str(&text).ok().and_then(|message| html_render::read_content_height(&message)) else { return };
                let Some(page) = self.pages.get_mut(key) else { return };
                let height = height as f32;
                if page.content_height.is_none_or(|current| (current - height).abs() >= 0.5) {
                    page.content_height = Some(height);
                    self.notify_owners(key, cx);
                }
            }
            WebEvent::OpenUrl(url) => cx.open_url(&url),
        }
    }

    /// The view showing `key` for `owner`, made when there is room and the window is in front.
    /// Making one in a background window would bring the app forward on macOS 13.
    fn inline_view(&mut self, key: &PageKey, owner: EntityId, html: Arc<str>, dark: bool, window: &mut Window, cx: &mut Context<Self>) -> Slot {
        let canvas = canvas(cx);
        self.views.retain(|inline| inline.view.strong_count() > 0);
        if let Some(inline) = self.views.iter_mut().find(|inline| &inline.key == key && inline.owner == owner)
            && let Some(view) = inline.view.upgrade()
        {
            if inline.canvas != canvas {
                view.evaluate(&canvas_update(canvas));
                inline.canvas = canvas;
            }
            return Slot::View(view);
        }
        if !webview::available(window) {
            return Slot::Card;
        }
        if !window.is_window_active() {
            return Slot::Pending;
        }
        if self.views.len() >= MAX_INLINE_VIEWS {
            return Slot::Card;
        }
        let (events, mut received) = mpsc::unbounded();
        let page = Page { html, dark, content_security_policy: None, scroll_through: true, script: format!("{CONTENT_HEIGHT_SCRIPT}{}", canvas_script(canvas)) };
        let Some(view) = WebView::new(page, Layer::Transcript, events, window, cx) else { return Slot::Card };
        let event_key = key.clone();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(event) = received.next().await {
                if this.update(cx, |frames, cx| frames.on_event(&event_key, event, cx)).is_err() {
                    return;
                }
            }
        });
        self.views.push(InlineView { key: key.clone(), owner, view: Rc::downgrade(&view), canvas, _events: pump });
        Slot::View(view)
    }
}

/// What an inline frame shows.
enum Slot {
    View(Rc<WebView>),
    /// Nothing until the window comes forward.
    Pending,
    Card,
}

/// An `html-render` row: the page, or "Unable to load", at the frame's height, with the
/// full-screen control under it.
pub fn frame_row(row_id: &str, entry: &HtmlRenderEntry, cx: &mut Context<ThreadView>) -> AnyElement {
    let owner = cx.entity_id();
    let reference = &entry.reference;
    let Some(store) = frames(cx) else { return div().into_any_element() };
    let Some((key, frame)) = store.update(cx, |frames, cx| {
        let key = frames.key(&reference.attachment_id, cx)?;
        let frame = frames.frame(&key, reference, owner, cx);
        Some((key, frame))
    }) else {
        return div().into_any_element();
    };
    let height = html_render::frame_height(reference, frame.width, frame.content_height);
    let p = palette(cx);
    let body = match &frame.html {
        Some(html) => InlineFrame {
            id: ElementId::Name(format!("html-render-{row_id}").into()),
            store: store.clone(),
            key: key.clone(),
            owner,
            html: html.clone(),
            reference: reference.clone(),
            width: frame.width,
            dark: cx.theme().is_dark(),
        }
        .into_any_element(),
        None if frame.failed => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(hsla(p.muted_foreground))
            .child(format!("Unable to load {}", reference.title))
            .into_any_element(),
        None => div().into_any_element(),
    };
    let expand = frame.html.clone().map(|html| {
        let (store, title) = (store.clone(), SharedString::from(reference.title.clone()));
        let open = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| open_fullscreen(&store, title.clone(), html.clone(), window, cx);
        div().flex().justify_end().pt(px(4.)).child(tooltip_icon_button(SharedString::from(format!("html-render-expand-{row_id}")), Icon::Maximize2, "Open full screen", false, open, cx))
    });
    div().w_full().min_w_0().child(div().w_full().h(px(height as f32)).relative().child(body)).children(expand).into_any_element()
}

/// The page's view, made once the row is on screen (a row only measured never gets one), or the
/// frame's card where no view can be made.
struct InlineFrame {
    id: ElementId,
    store: Entity<HtmlFrames>,
    key: PageKey,
    owner: EntityId,
    html: Arc<str>,
    reference: HtmlRenderReference,
    width: f32,
    dark: bool,
}

impl IntoElement for InlineFrame {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InlineFrame {
    type RequestLayoutState = ();
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let width = f32::from(bounds.size.width);
        if (width - self.width).abs() > 0.5 {
            let (store, key) = (self.store.clone(), self.key.clone());
            cx.defer(move |cx| store.update(cx, |frames, cx| frames.set_width(&key, width, cx)));
        }
        let (key, owner, html, dark) = (self.key.clone(), self.owner, self.html.clone(), self.dark);
        let slot = self.store.update(cx, |frames, cx| frames.inline_view(&key, owner, html, dark, window, cx));
        let mut child = match slot {
            Slot::View(view) => webview::web_view("page", view, self.dark).into_any_element(),
            Slot::Card => page_card(&self.reference, cx),
            Slot::Pending => return None,
        };
        child.prepaint_as_root(bounds.origin, bounds.size.map(AvailableSpace::Definite), window, cx);
        Some(child)
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), child: &mut Option<AnyElement>, window: &mut Window, cx: &mut App) {
        if let Some(child) = child {
            child.paint(window, cx);
        }
    }
}

/// Where no view can show the page (other platforms, the offscreen renderer, too many at once):
/// its title and file name, and the page opened in the browser.
fn page_card(reference: &HtmlRenderReference, cx: &App) -> AnyElement {
    let p = palette(cx);
    let resource = reference.resource();
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.border))
        .bg(hsla(p.card))
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(reference.title.clone()))
        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(reference.file_name()))
        .child(div().pt(px(8.)).child(outline_button("html-render-open-browser", "Open in browser", move |_, _, cx| crate::media::open_asset(&resource, cx), cx)))
        .into_any_element()
}

fn open_fullscreen(store: &Entity<HtmlFrames>, title: SharedString, html: Arc<str>, window: &mut Window, cx: &mut App) {
    let restore = window.focused(cx);
    let view = cx.new(|cx| HtmlFullscreen { title, html, zoom: 1., view: None, canvas: 0, unavailable: false, focus: cx.focus_handle(), restore, _events: None });
    view.read(cx).focus.clone().focus(window, cx);
    store.update(cx, |frames, _| frames.fullscreen = Some(view));
    window.refresh();
}

/// The open full-screen page, drawn over the window by the workspace.
pub fn fullscreen(_: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let view = frames(cx)?.read(cx).fullscreen.clone()?;
    Some(view.into_any_element())
}

/// `HtmlRenderDialog`: the page at 92% of the window over a dimmed backdrop, its title, and zoom
/// from 50% to 200%. Drawn in the base plane so the page's view can sit above it; the backdrop
/// keeps inline pages out of sight while it is open.
pub struct HtmlFullscreen {
    title: SharedString,
    html: Arc<str>,
    zoom: f32,
    view: Option<Rc<WebView>>,
    canvas: u32,
    unavailable: bool,
    focus: FocusHandle,
    restore: Option<FocusHandle>,
    _events: Option<Task<()>>,
}

impl Focusable for HtmlFullscreen {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl HtmlFullscreen {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view = None;
        if let Some(frames) = frames(cx) {
            frames.update(cx, |frames, _| frames.fullscreen = None);
        }
        if let Some(previous) = self.restore.take() {
            previous.focus(window, cx);
        }
        window.refresh();
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.zoom = zoom.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
        if let Some(view) = &self.view {
            view.set_zoom(f64::from(self.zoom));
        }
        cx.notify();
    }

    fn ensure_view(&mut self, dark: bool, window: &mut Window, cx: &mut Context<Self>) {
        let canvas = canvas(cx);
        if let Some(view) = &self.view
            && self.canvas != canvas
        {
            view.evaluate(&canvas_update(canvas));
            self.canvas = canvas;
        }
        if self.view.is_some() || self.unavailable {
            return;
        }
        let (events, mut received) = mpsc::unbounded();
        let page = Page { html: self.html.clone(), dark, content_security_policy: None, scroll_through: false, script: canvas_script(canvas) };
        self.canvas = canvas;
        self.view = WebView::new(page, Layer::Modal, events, window, cx);
        self.unavailable = self.view.is_none();
        self._events = Some(cx.spawn(async move |_, cx| {
            while let Some(event) = received.next().await {
                if let WebEvent::OpenUrl(url) = event {
                    cx.update(|cx| cx.open_url(&url));
                }
            }
        }));
    }
}

/// The media viewer's black and white alphas (`overlay`, `media-close`; design system §6.1).
fn white(alpha: f32) -> gpui::Hsla {
    gpui_hsla(0., 0., 1., alpha)
}

fn black(alpha: f32) -> gpui::Hsla {
    gpui_hsla(0., 0., 0., alpha)
}

/// The media dialog's buttons (`button.tsx` `overlay` and `media-close`), in rem so they follow
/// the interface size.
#[derive(Clone, Copy)]
enum MediaButton {
    /// `overlay` at `icon-sm`.
    Icon(Icon),
    /// `overlay` at `sm`.
    Label,
    /// `media-close` at `icon-xs`.
    Close,
}

impl MediaButton {
    fn build(self, id: &'static str, disabled: bool, window: &Window) -> gpui::Stateful<gpui::Div> {
        let size = |value: f32| rem(value).to_pixels(window.rem_size());
        let (height, fill, hover) = match self {
            MediaButton::Close => (24., 0.65, 0.8),
            _ => (28., 0.7, 0.9),
        };
        let button = div()
            .id(id)
            .h(rem(height))
            .min_w(rem(height))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(rem(8.))
            .bg(black(fill))
            .when(disabled, |this| this.opacity(0.64))
            .when(!disabled, |this| this.cursor_pointer().hover(|this| this.bg(black(hover))));
        match self {
            MediaButton::Icon(icon) => button.child(icon.render(size(16.), white(0.65))),
            MediaButton::Label => button.px(rem(9.)).text_sm().font_weight(FontWeight::MEDIUM).text_color(white(0.65)),
            MediaButton::Close => button.border_1().border_color(white(0.2)).child(Icon::X.render(size(14.), white(1.))),
        }
    }
}

impl Render for HtmlFullscreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = cx.theme().is_dark();
        self.ensure_view(dark, window, cx);
        let p = palette(cx);
        let viewport = window.viewport_size();
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        let box_height = (height * 0.86).min(height - 160.).max(120.);
        let page = match &self.view {
            Some(view) => webview::web_view("html-render-fullscreen-page", view.clone(), dark).into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child(if self.unavailable { "Page unavailable" } else { "" })
                .into_any_element(),
        };
        let zoom = self.zoom;
        let zoom_out = zoom > ZOOM_RANGE.0;
        let zoom_in = zoom < ZOOM_RANGE.1;
        let controls = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(rem(4.))
            .child(
                MediaButton::Icon(Icon::Minus)
                    .build("html-render-zoom-out", !zoom_out, window)
                    .when(zoom_out, |this| this.on_click(cx.listener(move |this, _, _, cx| this.set_zoom(zoom - ZOOM_STEP, cx)))),
            )
            .child(
                MediaButton::Label
                    .build("html-render-zoom-reset", false, window)
                    .child(format!("{}%", (zoom * 100.).round()))
                    .on_click(cx.listener(|this, _, _, cx| this.set_zoom(1., cx))),
            )
            .child(
                MediaButton::Icon(Icon::Plus)
                    .build("html-render-zoom-in", !zoom_in, window)
                    .when(zoom_in, |this| this.on_click(cx.listener(move |this, _, _, cx| this.set_zoom(zoom + ZOOM_STEP, cx)))),
            );
        let close = div()
            .absolute()
            .right_0()
            .top(rem(-40.))
            .child(MediaButton::Close.build("html-render-fullscreen-close", false, window).on_click(cx.listener(|this, _, window, cx| this.close(window, cx))));
        div()
            .id("html-render-fullscreen")
            .track_focus(&self.focus)
            .key_context("HtmlRenderFullscreen")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(window, cx);
                }
            }))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .bg(black(0.75))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(webview::occluder("html-render-fullscreen-cover", Layer::Modal))
            .child(
                div()
                    .relative()
                    .w(px(width * 0.92))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(close)
                    .child(
                        div()
                            .h(px(box_height))
                            .p(rem(16.))
                            .rounded(rem(8.))
                            .bg(hsla(p.background))
                            .border_1()
                            .border_color(hsla(p.border).opacity(0.7))
                            .shadow_2xl()
                            .child(page),
                    )
                    .child(
                        div()
                            .mt(rem(8.))
                            .flex()
                            .min_w_0()
                            .items_center()
                            .justify_center()
                            .gap(rem(12.))
                            .text_xs()
                            .text_color(white(0.8))
                            .child(div().min_w_0().truncate().child(self.title.clone()))
                            .child(controls),
                    ),
            )
    }
}

/// `--html-fixture <page.html>` (or `T3_NATIVE_HTML_FIXTURE`): a completed `html_render` result
/// appended to the open thread, its page read from the file, for screenshots without an agent.
#[cfg(feature = "snapshot")]
pub mod fixture {
    use std::borrow::Cow;
    use std::path::PathBuf;
    use std::sync::{Arc, OnceLock};

    use t3_core::thread::{ProjectedTurnItem, Visibility};
    use t3_core::turn_item::TurnItem;

    const ATTACHMENT_ID: &str = "native-html-fixture.html";
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    static ITEM: OnceLock<Option<Arc<TurnItem>>> = OnceLock::new();

    pub fn set(path: PathBuf) {
        let _ = PATH.set(Some(path));
    }

    fn path() -> Option<&'static PathBuf> {
        PATH.get_or_init(|| std::env::var_os("T3_NATIVE_HTML_FIXTURE").map(PathBuf::from)).as_ref()
    }

    pub(super) fn page(attachment_id: &str) -> Option<&'static PathBuf> {
        path().filter(|_| attachment_id == ATTACHMENT_ID)
    }

    fn item() -> Option<Arc<TurnItem>> {
        path()?;
        ITEM.get_or_init(|| {
            let value = serde_json::json!({
                "id": "native-html-fixture", "threadId": "fixture", "runId": null, "nodeId": null, "parentItemId": null,
                "ordinal": u32::MAX, "status": "completed", "title": null,
                "startedAt": "2026-01-01T00:00:00.000Z", "completedAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
                "type": "dynamic_tool", "toolName": "mcp__t3-code__html_render", "input": {}, "outputOmitted": true,
                "output": { "htmlRender": { "attachmentId": ATTACHMENT_ID, "title": "Fixture page", "height": 320 } },
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
