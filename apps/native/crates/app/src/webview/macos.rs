//! The AppKit side of a web view: a clipping container added beneath GPUI's overlay plane, holding
//! a wry `WKWebView` that loads the page from memory.

use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use block2::RcBlock;
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use gpui::{Bounds, Pixels, Window};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType,
    NSResponder, NSView, NSWindowOrderingMode,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};
use objc2_web_kit::{WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationType, WKWebView};
use wry::http::{Request, Response, header::CONTENT_TYPE};
use wry::raw_window_handle::{AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use wry::{NewWindowResponse, WebViewBuilder, WebViewExtMacOS, WryWebView};

use super::{Page, RectF, ScrollArea, WebEvent, page_consumes_vertical_scroll, parse_scroll_areas, subtract_cutouts};

/// WebKit opens script windows without a click on macOS by default; with this off, only a
/// reader's click (a link's `target=_blank`) asks for a window, which becomes a link open.
fn block_script_windows(page: &WryWebView) {
    // SAFETY: a web view's configuration and its preferences are live objects; the setter takes a BOOL.
    unsafe {
        let configuration: *mut AnyObject = msg_send![page, configuration];
        let preferences: *mut AnyObject = msg_send![configuration, preferences];
        if !preferences.is_null() {
            let _: () = msg_send![preferences, setJavaScriptCanOpenWindowsAutomatically: false];
        }
    }
}

/// HTML matches `BrowserDocumentFrame`; MCP retains its existing native popup policy.
fn content_policy(allow_popups: bool, additional: Option<&str>) -> String {
    let sandbox = if allow_popups { "sandbox allow-scripts allow-forms allow-popups" } else { t3_core::html_render::routing::SANDBOX };
    additional.map(|csp| format!("{sandbox}; {csp}")).unwrap_or_else(|| sandbox.into())
}
const MAX_MESSAGE_BYTES: usize = 2 * t3_core::mcp_apps::MAX_MESSAGE_BYTES;

#[repr(C)]
struct CGColor {
    _private: [u8; 0],
}

// SAFETY: CoreGraphics `CGColorRef` maps to `^{CGColor=}` in Objective-C encodings.
unsafe impl objc2::RefEncode for CGColor {
    const ENCODING_REF: objc2::Encoding = objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGColor", &[]));
}

struct ContainerIvars {
    cutouts: RefCell<Vec<NSRect>>,
}

define_class!(
    /// Container view whose cutout rectangles pass pointer hit-testing straight through to GPUI.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ContainerIvars]
    struct T3WebContainerView;

    unsafe impl NSObjectProtocol for T3WebContainerView {}

    impl T3WebContainerView {
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut AnyObject {
            // SAFETY: reading our own superview while AppKit hit-tests this view.
            if let Some(superview) = unsafe { self.superview() } {
                let local = self.convertPoint_fromView(point, Some(&superview));
                if self.ivars().cutouts.borrow().iter().any(|r| {
                    local.x >= r.origin.x
                        && local.x < r.origin.x + r.size.width
                        && local.y >= r.origin.y
                        && local.y < r.origin.y + r.size.height
                }) {
                    return std::ptr::null_mut();
                }
            }
            // SAFETY: `[super hitTest:point]` expects `point` in the receiver's superview coordinates.
            unsafe { msg_send![super(self), hitTest: point] }
        }
    }
);

fn apply_cutout_mask(container: &T3WebContainerView, width: f64, height: f64, cutouts: &[NSRect]) {
    container.ivars().cutouts.replace(cutouts.to_vec());
    // SAFETY: a layer-backed view owns a `CALayer` and AppKit/CoreAnimation classes exist on macOS.
    unsafe {
        let layer: *mut AnyObject = msg_send![container, layer];
        if layer.is_null() {
            return;
        }
        if cutouts.is_empty() {
            let _: () = msg_send![layer, setMask: std::ptr::null::<AnyObject>()];
            return;
        }
        let Some(tx) = AnyClass::get(c"CATransaction") else { return };
        let Some(layer_cls) = AnyClass::get(c"CALayer") else { return };
        let Some(color_cls) = AnyClass::get(c"NSColor") else { return };
        let rects: Vec<RectF> = cutouts
            .iter()
            .map(|r| RectF { x: r.origin.x, y: r.origin.y, w: r.size.width, h: r.size.height })
            .collect();
        let pieces = subtract_cutouts(width, height, &rects);
        let _: () = msg_send![tx, begin];
        let _: () = msg_send![tx, setDisableActions: true];
        let mask: Retained<AnyObject> = msg_send![layer_cls, layer];
        let _: () = msg_send![&*mask, setFrame: rect(0., 0., width, height)];
        let white: *mut AnyObject = msg_send![color_cls, whiteColor];
        let cg_white: *const CGColor = msg_send![white, CGColor];
        for piece in pieces {
            let sub: Retained<AnyObject> = msg_send![layer_cls, layer];
            let _: () = msg_send![&*sub, setFrame: rect(piece.x, piece.y, piece.w, piece.h)];
            let _: () = msg_send![&*sub, setBackgroundColor: cg_white];
            let _: () = msg_send![&*mask, addSublayer: &*sub];
        }
        let _: () = msg_send![layer, setMask: &*mask];
        let _: () = msg_send![tx, commit];
    }
}

pub(super) struct NativeView {
    host: Retained<NSView>,
    container: Retained<T3WebContainerView>,
    page: Retained<WryWebView>,
    webview: wry::WebView,
    /// The page's navigation delegate; the web view holds it weakly.
    _policy: Retained<PagePolicy>,
    scroll: Option<Rc<ScrollTarget>>,
    hover: Rc<HoverTarget>,
    zoom: Rc<Cell<f64>>,
    activation: Activation,
}

/// wry builds a child web view in the view a window handle names; this one names the container.
struct Container<'a>(&'a NSView);

impl HasWindowHandle for Container<'_> {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let view = NonNull::from(self.0).cast();
        // SAFETY: the container outlives the borrow and is a view in a window.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(view))) })
    }
}

fn set_appearance(view: &NSView, dark: bool) {
    // SAFETY: AppKit's appearance names are immutable statics.
    let name = unsafe { if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua } };
    view.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

/// Every response carries the sandbox, a 404 included.
fn serve(html: &str, policy: &str, request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let sandboxed = Response::builder().header("Content-Security-Policy", policy);
    let response = if request.uri() == super::PAGE_URL {
        sandboxed.header(CONTENT_TYPE, "text/html; charset=utf-8").body(Cow::Owned(html.as_bytes().to_vec()))
    } else {
        sandboxed.status(404).body(Cow::Borrowed(&[][..]))
    };
    response.unwrap_or_else(|_| Response::builder().status(500).header("Content-Security-Policy", policy).body(Cow::Borrowed(&[][..])).unwrap_or_default())
}

impl NativeView {
    pub(super) fn new(window: &mut Window, page: &Page, events: UnboundedSender<WebEvent>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw() else { return None };
        // SAFETY: GPUI's handle names its live content view.
        let host: Retained<NSView> = unsafe { Retained::retain(handle.ns_view.as_ptr().cast()) }?;
        // SAFETY: `T3WebContainerView` inherits `[NSView initWithFrame:]`.
        let container: Retained<T3WebContainerView> = unsafe {
            msg_send![super(mtm.alloc::<T3WebContainerView>().set_ivars(ContainerIvars { cutouts: RefCell::new(Vec::new()) })), initWithFrame: rect(0., 0., 0., 0.)]
        };
        container.setWantsLayer(true);
        // SAFETY: a layer-backed view has a layer; `masksToBounds` takes a BOOL.
        unsafe {
            let layer: *mut AnyObject = msg_send![&*container, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
        }
        container.setHidden(true);
        set_appearance(&container, page.dark);
        // Beneath every other child, so the overlay plane and its backdrops stay on top.
        host.addSubview_positioned_relativeTo(&container, NSWindowOrderingMode::Below, None);

        let html = page.html.clone();
        let policy = content_policy(page.allow_popups, page.content_security_policy.as_deref());
        let allow_popups = page.allow_popups;
        let links = events.clone();
        let link_events = events.clone();
        let scroll_areas = Rc::new(RefCell::new(Vec::new()));
        let ipc_scroll_areas = scroll_areas.clone();
        let scroll_through = page.scroll_through;
        let builder = WebViewBuilder::new()
            .with_custom_protocol(super::PAGE_SCHEME.into(), move |_, request| serve(&html, &policy, &request))
            .with_url(super::PAGE_URL)
            .with_initialization_script_for_main_only(page.script.as_str(), true)
            .with_ipc_handler(move |request: Request<String>| {
                // wry names the posting frame's URL; any frame can reach the handler.
                if !super::from_page(&request.uri().to_string()) {
                    return;
                }
                let body = request.into_body();
                if body.len() <= MAX_MESSAGE_BYTES {
                    if scroll_through && let Some(areas) = parse_scroll_areas(&body) {
                        *ipc_scroll_areas.borrow_mut() = areas;
                        return;
                    }
                    let _ = events.unbounded_send(WebEvent::Message(body));
                }
            })
            // `window.open` after a click (link clicks go through `PagePolicy`).
            .with_new_window_req_handler(move |url, _| {
                if allow_popups && super::web_link(&url) {
                    let _ = links.unbounded_send(WebEvent::OpenUrl(url));
                }
                NewWindowResponse::Deny
            })
            .with_incognito(true)
            .with_transparent(true)
            .with_autoplay(false)
            .with_back_forward_navigation_gestures(false);
        let webview = match builder.build_as_child(&Container(&container)) {
            Ok(webview) => webview,
            Err(error) => {
                tracing::warn!(%error, "web view unavailable");
                container.removeFromSuperview();
                return None;
            }
        };
        let page_view = webview.webview();
        block_script_windows(&page_view);
        let policy = PagePolicy::install(&page_view, link_events, allow_popups, mtm);
        let zoom = Rc::new(Cell::new(1.0));
        let scroll = page.scroll_through.then(|| {
            let target = Rc::new(ScrollTarget {
                host: host.clone(),
                frame: Cell::new(None),
                page_top_left: Cell::new(None),
                areas: scroll_areas,
                zoom: zoom.clone(),
            });
            ScrollTarget::register(&target);
            target
        });
        let hover = Rc::new(HoverTarget {
            container: container.clone(),
            host: host.clone(),
            hovered: Cell::new(false),
            senders: RefCell::new(Vec::new()),
        });
        HoverTarget::register(&hover);
        let activation = Activation::new(&container);
        Some(NativeView { host, container, page: page_view, webview, _policy: policy, scroll, hover, zoom, activation })
    }

    pub(super) fn is_focused(&self) -> bool {
        !self.container.isHidden() && self.container.window().is_some_and(|window| window.isKeyWindow()
            && window.firstResponder().is_some_and(|r| r.downcast_ref::<NSView>().is_some_and(|v| v.isDescendantOf(&self.container))))
    }

    pub(super) fn take_activation(&self) -> bool {
        !self.container.isHidden()
            && self.container.window().is_some_and(|window| window.isKeyWindow())
            && self.activation.at.take().is_some_and(|at| at.elapsed() <= Duration::from_secs(5))
    }

    /// Shows the page at `frame`, cut to `visible` with `cutouts` masked out; all in window points from the top left.
    pub(super) fn place(&self, frame: Bounds<Pixels>, visible: Bounds<Pixels>, cutouts: &[Bounds<Pixels>]) {
        let px = |value: Pixels| f64::from(f32::from(value));
        let host_height = self.host.frame().size.height;
        let container = rect(px(visible.left()), host_height - px(visible.bottom()), px(visible.size.width), px(visible.size.height));
        self.container.setFrame(container);
        self.page.setFrame(rect(
            px(frame.left() - visible.left()),
            px(visible.bottom() - frame.bottom()),
            px(frame.size.width),
            px(frame.size.height),
        ));
        let local_cutouts: Vec<NSRect> = cutouts
            .iter()
            .filter_map(|cutout| {
                let hit = cutout.intersect(&visible);
                if hit.size.width <= gpui::px(0.) || hit.size.height <= gpui::px(0.) {
                    return None;
                }
                Some(rect(
                    px(hit.left() - visible.left()),
                    px(visible.bottom() - hit.bottom()),
                    px(hit.size.width),
                    px(hit.size.height),
                ))
            })
            .collect();
        apply_cutout_mask(&self.container, px(visible.size.width), px(visible.size.height), &local_cutouts);
        self.container.setHidden(false);
        if let Some(scroll) = &self.scroll {
            scroll.frame.set(Some(container));
            scroll.page_top_left.set(Some(NSPoint::new(px(frame.left()), host_height - px(frame.top()))));
        }
    }

    pub(super) fn hide(&self) {
        self.container.setHidden(true);
        self.hover.set(false);
        if let Some(scroll) = &self.scroll {
            scroll.frame.set(None);
            scroll.page_top_left.set(None);
        }
    }

    pub(super) fn set_dark(&self, dark: bool) {
        set_appearance(&self.container, dark);
    }

    pub(super) fn set_zoom(&self, zoom: f64) {
        self.zoom.set(zoom.max(0.1));
        let _ = self.webview.zoom(zoom);
    }

    pub(super) fn watch_hover(&self) -> UnboundedReceiver<bool> {
        self.hover.watch()
    }

    pub(super) fn evaluate(&self, script: &str) {
        let _ = self.webview.evaluate_script(script);
    }

    /// Hands keyboard focus back to GPUI when the page holds it. AppKit leaves the window, not
    /// GPUI's view, as first responder when a focused view goes away.
    pub(super) fn release_focus(&self) {
        let Some(window) = self.host.window() else { return };
        let inside = window.firstResponder().is_some_and(|responder| responder.downcast_ref::<NSView>().is_some_and(|view| view.isDescendantOf(&self.container)));
        if inside {
            let host: &NSResponder = &self.host;
            window.makeFirstResponder(Some(host));
        }
    }
}

impl Drop for NativeView {
    fn drop(&mut self) {
        self.release_focus();
        self.container.removeFromSuperview();
    }
}

struct PolicyIvars {
    allow_popups: bool,
    events: UnboundedSender<WebEvent>,
    /// wry's own delegate: it still runs scripts queued before the page committed and reports
    /// page loads.
    wry: Option<Retained<ProtocolObject<dyn WKNavigationDelegate>>>,
}

define_class!(
    /// Decides navigations by frame, which wry's URL-only handler cannot: the main frame keeps
    /// the page and subframes inherit its sandbox. MCP may forward clicked new-window links;
    /// HTML forbids them. WebKit asks here before `createWebView`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PolicyIvars]
    struct PagePolicy;

    unsafe impl NSObjectProtocol for PagePolicy {}

    unsafe impl WKNavigationDelegate for PagePolicy {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(&self, _webview: &WKWebView, action: &WKNavigationAction, handler: &block2::Block<dyn Fn(WKNavigationActionPolicy)>) {
            let allowed = self.allows(action);
            handler.call((if allowed { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, webview: &WKWebView, navigation: &WKNavigation) {
            if let Some(wry) = &self.ivars().wry {
                // SAFETY: wry's delegate implements this method with these argument types.
                let _: () = unsafe { msg_send![&**wry, webView: webview, didCommitNavigation: navigation] };
            }
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, webview: &WKWebView, navigation: &WKNavigation) {
            if let Some(wry) = &self.ivars().wry {
                // SAFETY: as above; wry reports the page load from here.
                let _: () = unsafe { msg_send![&**wry, webView: webview, didFinishNavigation: navigation] };
            }
        }
    }
);

impl PagePolicy {
    fn install(page: &WryWebView, events: UnboundedSender<WebEvent>, allow_popups: bool, mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: reading and replacing a live web view's delegate on the main thread.
        let wry = unsafe { page.navigationDelegate() };
        let policy: Retained<Self> = unsafe { msg_send![super(mtm.alloc::<Self>().set_ivars(PolicyIvars { events, wry, allow_popups })), init] };
        unsafe { page.setNavigationDelegate(Some(ProtocolObject::from_ref(&*policy))) };
        policy
    }

    fn allows(&self, action: &WKNavigationAction) -> bool {
        if unsafe { action.respondsToSelector(objc2::sel!(shouldPerformDownload)) && action.shouldPerformDownload() } {
            return false;
        }
        // SAFETY: a navigation action's request and target frame are plain getters.
        let (url, target, kind) = unsafe {
            let url = action.request().URL().and_then(|url| url.absoluteString()).map(|url| url.to_string()).unwrap_or_default();
            (url, action.targetFrame(), action.navigationType())
        };
        match target {
            Some(frame) => super::navigable(&url, unsafe { frame.isMainFrame() }),
            None => {
                if self.ivars().allow_popups && kind == WKNavigationType::LinkActivated && super::web_link(&url) {
                    let _ = self.ivars().events.unbounded_send(WebEvent::OpenUrl(url));
                }
                false
            }
        }
    }
}

/// An inline page's visible frame, in its host view's coordinates, while it shows.
struct ScrollTarget {
    host: Retained<NSView>,
    frame: Cell<Option<NSRect>>,
    page_top_left: Cell<Option<NSPoint>>,
    areas: Rc<RefCell<Vec<ScrollArea>>>,
    zoom: Rc<Cell<f64>>,
}

thread_local! {
    static SCROLL_TARGETS: RefCell<Vec<Weak<ScrollTarget>>> = const { RefCell::new(Vec::new()) };
    static SCROLL_MONITOR: OnceCell<Option<Retained<AnyObject>>> = const { OnceCell::new() };
    static HOVER_TARGETS: RefCell<Vec<Weak<HoverTarget>>> = const { RefCell::new(Vec::new()) };
    static HOVER_MONITOR: OnceCell<Option<Retained<AnyObject>>> = const { OnceCell::new() };
}

impl ScrollTarget {
    /// WebKit keeps every wheel event over a page, even one that cannot scroll, so a local monitor
    /// hands vertical scrolls over inline pages to GPUI's view unless an inner container can scroll
    /// in that direction.
    fn register(target: &Rc<ScrollTarget>) {
        SCROLL_TARGETS.with(|targets| {
            let mut targets = targets.borrow_mut();
            targets.retain(|target| target.strong_count() > 0);
            targets.push(Rc::downgrade(target));
        });
        SCROLL_MONITOR.with(|monitor| {
            monitor.get_or_init(|| {
                let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
                    // SAFETY: AppKit passes a live event.
                    let passed = ScrollTarget::pass_through(unsafe { event.as_ref() });
                    if passed { std::ptr::null_mut() } else { event.as_ptr() }
                });
                // SAFETY: the block returns the event it was given, or null to consume it.
                unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::ScrollWheel, &block) }
            });
        });
    }

    fn pass_through(event: &NSEvent) -> bool {
        let delta_y = event.scrollingDeltaY();
        if delta_y.abs() < event.scrollingDeltaX().abs() {
            return false;
        }
        let Some(window) = MainThreadMarker::new().and_then(|mtm| event.window(mtm)) else { return false };
        let matched = SCROLL_TARGETS.with(|targets| {
            targets.borrow().iter().filter_map(Weak::upgrade).find_map(|target| {
                let frame = target.frame.get()?;
                if target.host.window().is_none_or(|host| Retained::as_ptr(&host) != Retained::as_ptr(&window)) {
                    return None;
                }
                let point = target.host.convertPoint_fromView(event.locationInWindow(), None);
                if point.x >= frame.origin.x
                    && point.x < frame.origin.x + frame.size.width
                    && point.y >= frame.origin.y
                    && point.y < frame.origin.y + frame.size.height
                {
                    Some((target, point))
                } else {
                    None
                }
            })
        });
        let Some((target, point)) = matched else { return false };
        if let Some(top_left) = target.page_top_left.get() {
            let zoom = target.zoom.get();
            let css_x = (point.x - top_left.x) / zoom;
            let css_y = (top_left.y - point.y) / zoom;
            if page_consumes_vertical_scroll(&target.areas.borrow(), css_x, css_y, delta_y) {
                return false;
            }
        }
        target.host.scrollWheel(event);
        true
    }
}

struct HoverTarget {
    container: Retained<T3WebContainerView>,
    host: Retained<NSView>,
    hovered: Cell<bool>,
    senders: RefCell<Vec<UnboundedSender<bool>>>,
}

impl HoverTarget {
    fn watch(&self) -> UnboundedReceiver<bool> {
        let (tx, rx) = mpsc::unbounded();
        if self.hovered.get() {
            let _ = tx.unbounded_send(true);
        }
        let mut senders = self.senders.borrow_mut();
        senders.retain(|sender| !sender.is_closed());
        senders.push(tx);
        rx
    }

    fn set(&self, hovered: bool) {
        if self.hovered.replace(hovered) == hovered {
            return;
        }
        self.senders.borrow_mut().retain(|sender| sender.unbounded_send(hovered).is_ok());
    }

    fn register(target: &Rc<HoverTarget>) {
        HOVER_TARGETS.with(|targets| {
            let mut targets = targets.borrow_mut();
            targets.retain(|target| target.strong_count() > 0);
            targets.push(Rc::downgrade(target));
        });
        HOVER_MONITOR.with(|monitor| {
            monitor.get_or_init(|| {
                let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
                    // SAFETY: AppKit passes a live event.
                    HoverTarget::handle(unsafe { event.as_ref() });
                    event.as_ptr()
                });
                let mask = NSEventMask::MouseMoved | NSEventMask::MouseExited;
                // SAFETY: the block always returns the input event unchanged.
                unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) }
            });
        });
    }

    fn handle(event: &NSEvent) {
        let Some(window) = MainThreadMarker::new().and_then(|mtm| event.window(mtm)) else { return };
        let exited = event.r#type() == NSEventType::MouseExited;
        HOVER_TARGETS.with(|targets| {
            for target in targets.borrow().iter().filter_map(Weak::upgrade) {
                let Some(host_window) = target.container.window() else {
                    target.set(false);
                    continue;
                };
                if Retained::as_ptr(&host_window) != Retained::as_ptr(&window) || target.container.isHidden() {
                    target.set(false);
                    continue;
                }
                if exited {
                    let p_host = target.host.convertPoint_fromView(event.locationInWindow(), None);
                    let host_size = target.host.frame().size;
                    if p_host.x <= 0. || p_host.y <= 0. || p_host.x >= host_size.width || p_host.y >= host_size.height {
                        target.set(false);
                        continue;
                    }
                }
                let p = target.container.convertPoint_fromView(event.locationInWindow(), None);
                let size = target.container.frame().size;
                target.set(p.x >= 0. && p.y >= 0. && p.x < size.width && p.y < size.height);
            }
        });
    }
}

/// User activation comes from AppKit events in this view, never from the document's IPC data.
struct Activation {
    at: Rc<Cell<Option<Instant>>>,
    monitor: Option<Retained<AnyObject>>,
}

impl Activation {
    fn new(container: &NSView) -> Self {
        let at = Rc::new(Cell::new(None));
        let noted = at.clone();
        let container = container.retain();
        let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit passes the live event to its local monitor.
            let event_ref = unsafe { event.as_ref() };
            let mut consume = false;
            let inside = MainThreadMarker::new().and_then(|mtm| event_ref.window(mtm)).is_some_and(|window| {
                if container.window().is_none_or(|host| Retained::as_ptr(&host) != Retained::as_ptr(&window)) {
                    return false;
                }
                if event_ref.r#type() == NSEventType::KeyDown {
                    let focused = window
                        .firstResponder()
                        .is_some_and(|r| r.downcast_ref::<NSView>().is_some_and(|v| v.isDescendantOf(&container)));
                    if focused {
                        let mods = event_ref.modifierFlags()
                            & (NSEventModifierFlags::Command
                                | NSEventModifierFlags::Control
                                | NSEventModifierFlags::Option
                                | NSEventModifierFlags::Shift);
                        if mods == NSEventModifierFlags::Command
                            && let Some(chars) = event_ref.charactersIgnoringModifiers()
                            && let Some(mtm) = MainThreadMarker::new()
                        {
                            let sel = match chars.to_string().to_ascii_lowercase().as_str() {
                                "c" => Some(objc2::sel!(copy:)),
                                "x" => Some(objc2::sel!(cut:)),
                                "v" => Some(objc2::sel!(paste:)),
                                "a" => Some(objc2::sel!(selectAll:)),
                                _ => None,
                            };
                            if let Some(sel) = sel
                                && unsafe { NSApplication::sharedApplication(mtm).sendAction_to_from(sel, None, None) }
                            {
                                consume = true;
                            }
                        }
                    }
                    return focused;
                }
                let p = container.convertPoint_fromView(event_ref.locationInWindow(), None);
                let size = container.frame().size;
                !container.isHidden() && p.x >= 0. && p.y >= 0. && p.x < size.width && p.y < size.height
            });
            noted.set(inside.then(Instant::now));
            if consume { std::ptr::null_mut() } else { event.as_ptr() }
        });
        let mask = NSEventMask::LeftMouseDown | NSEventMask::LeftMouseUp | NSEventMask::RightMouseDown | NSEventMask::RightMouseUp | NSEventMask::KeyDown;
        // SAFETY: the block returns its input event unchanged unless an edit selector was consumed.
        let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
        Self { at, monitor }
    }
}

impl Drop for Activation {
    fn drop(&mut self) {
        if let Some(monitor) = &self.monitor {
            // SAFETY: this is the monitor token returned by AppKit for this view.
            unsafe { NSEvent::removeMonitor(monitor); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_response_keeps_sandbox_and_app_restrictions() {
        for allow_popups in [false, true] {
            let expected = if allow_popups { "sandbox allow-scripts allow-forms allow-popups" } else { "sandbox allow-scripts allow-forms" };
            let restrictions = "default-src 'none'; connect-src https://api.example.test";
            let policy = content_policy(allow_popups, Some(restrictions));
            for (uri, status) in [(super::super::PAGE_URL, 200), ("t3-page://page/missing", 404)] {
                let request = Request::builder().uri(uri).body(Vec::new()).unwrap();
                let response = serve("<p>app</p>", &policy, &request);
                assert_eq!(response.status(), status);
                assert_eq!(response.headers()["Content-Security-Policy"], format!("{expected}; {restrictions}"));
            }
        }
    }
}
