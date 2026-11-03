//! The AppKit side of a web view: a clipping container added beneath GPUI's overlay plane, holding
//! a wry `WKWebView` that loads the page from memory.

use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::ptr::NonNull;
use std::rc::{Rc, Weak};

use block2::RcBlock;
use futures::channel::mpsc::UnboundedSender;
use gpui::{Bounds, Pixels, Window};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSEvent, NSEventMask, NSResponder, NSView, NSWindowOrderingMode,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use wry::http::{Request, Response, header::CONTENT_TYPE};
use wry::raw_window_handle::{AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use wry::{NewWindowResponse, WebViewBuilder, WebViewExtMacOS, WryWebView};

use super::{Page, WebEvent};

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

const SCHEME: &str = "t3-page";
const PAGE_URL: &str = "t3-page://page/index.html";
/// What the server sends with agent HTML (`http.ts`), less downloads: an opaque origin with
/// scripts and forms. Popups reach the app as link opens and never as windows.
const CONTENT_SECURITY_POLICY: &str = "sandbox allow-scripts allow-forms allow-popups";
const MAX_MESSAGE_BYTES: usize = 4096;

pub(super) struct NativeView {
    host: Retained<NSView>,
    container: Retained<NSView>,
    page: Retained<WryWebView>,
    webview: wry::WebView,
    scroll: Option<Rc<ScrollTarget>>,
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

fn serve(html: &str, request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let response = if request.uri().path() == "/index.html" {
        Response::builder()
            .header(CONTENT_TYPE, "text/html; charset=utf-8")
            .header("Content-Security-Policy", CONTENT_SECURITY_POLICY)
            .body(Cow::Owned(html.as_bytes().to_vec()))
    } else {
        Response::builder().status(404).body(Cow::Borrowed(&[][..]))
    };
    response.unwrap_or_default()
}

impl NativeView {
    pub(super) fn new(window: &mut Window, page: &Page, events: UnboundedSender<WebEvent>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw() else { return None };
        // SAFETY: GPUI's handle names its live content view.
        let host: Retained<NSView> = unsafe { Retained::retain(handle.ns_view.as_ptr().cast()) }?;
        let container = NSView::initWithFrame(mtm.alloc(), rect(0., 0., 0., 0.));
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
        let links = events.clone();
        let builder = WebViewBuilder::new()
            .with_custom_protocol(SCHEME.into(), move |_, request| serve(&html, &request))
            .with_url(PAGE_URL)
            .with_initialization_script_for_main_only(page.script.as_str(), true)
            .with_ipc_handler(move |request: Request<String>| {
                let body = request.into_body();
                if body.len() <= MAX_MESSAGE_BYTES {
                    let _ = events.unbounded_send(WebEvent::Message(body));
                }
            })
            .with_navigation_handler(|url: String| super::navigable(&url))
            .with_new_window_req_handler(move |url, _| {
                if url.starts_with("http://") || url.starts_with("https://") {
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
        let scroll = page.scroll_through.then(|| {
            let target = Rc::new(ScrollTarget { host: host.clone(), frame: Cell::new(None) });
            ScrollTarget::register(&target);
            target
        });
        Some(NativeView { host, container, page: page_view, webview, scroll })
    }

    /// Shows the page at `frame`, cut to `visible`; both in window points from the top left.
    pub(super) fn place(&self, frame: Bounds<Pixels>, visible: Bounds<Pixels>) {
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
        self.container.setHidden(false);
        if let Some(scroll) = &self.scroll {
            scroll.frame.set(Some(container));
        }
    }

    pub(super) fn hide(&self) {
        self.container.setHidden(true);
        if let Some(scroll) = &self.scroll {
            scroll.frame.set(None);
        }
    }

    pub(super) fn set_dark(&self, dark: bool) {
        set_appearance(&self.container, dark);
    }

    pub(super) fn set_zoom(&self, zoom: f64) {
        let _ = self.webview.zoom(zoom);
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

/// An inline page's visible frame, in its host view's coordinates, while it shows.
struct ScrollTarget {
    host: Retained<NSView>,
    frame: Cell<Option<NSRect>>,
}

thread_local! {
    static SCROLL_TARGETS: RefCell<Vec<Weak<ScrollTarget>>> = const { RefCell::new(Vec::new()) };
    static SCROLL_MONITOR: OnceCell<Option<Retained<AnyObject>>> = const { OnceCell::new() };
}

impl ScrollTarget {
    /// WebKit keeps every wheel event over a page, even one that cannot scroll, so a local monitor
    /// hands vertical scrolls over inline pages to GPUI's view first.
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
        if event.scrollingDeltaY().abs() < event.scrollingDeltaX().abs() {
            return false;
        }
        let Some(window) = MainThreadMarker::new().and_then(|mtm| event.window(mtm)) else { return false };
        let target = SCROLL_TARGETS.with(|targets| {
            targets.borrow().iter().filter_map(Weak::upgrade).find(|target| {
                let Some(frame) = target.frame.get() else { return false };
                if target.host.window().is_none_or(|host| Retained::as_ptr(&host) != Retained::as_ptr(&window)) {
                    return false;
                }
                let point = target.host.convertPoint_fromView(event.locationInWindow(), None);
                point.x >= frame.origin.x && point.x < frame.origin.x + frame.size.width && point.y >= frame.origin.y && point.y < frame.origin.y + frame.size.height
            })
        });
        let Some(target) = target else { return false };
        target.host.scrollWheel(event);
        true
    }
}
