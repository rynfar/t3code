//! Native web views inside a GPUI window, for pages the app cannot draw itself: agent HTML now,
//! MCP app frames later. A page is loaded from memory through a private URL scheme into its own
//! non-persistent WebKit store, so it never sees the app's session, token or server credentials
//! (the web's opaque-origin `sandbox="allow-scripts allow-forms"` frame, `BrowserDocumentFrame.tsx`).
//!
//! A web view is an AppKit view, so it draws above everything GPUI paints in the window's base
//! plane. The window's scene overlay (zui's `enable_scene_overlay`) moves deferred content —
//! menus, popovers, dialogs, the palette, toasts and tooltips — onto a plane above native views
//! and routes input to it while one is open. GPUI content painted in the base plane over a web
//! view (the transcript's find bar and banners, the full-screen page's backdrop) registers an
//! [`occluder`] instead: a view is clipped away from any occluder of a higher [`Layer`].
//!
//! A view shows only while an element draws it ([`web_view`]): the element's state holds a lease,
//! and the frame it last appeared in places it when presented (`Window::on_present`, replayed
//! with cached paint), so a row that scrolls away or a view that stops drawing hides it the same
//! frame.

#[cfg(target_os = "macos")]
mod macos;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use futures::channel::mpsc::UnboundedSender;
use gpui::{
    App, Bounds, DispatchPhase, Element, ElementId, Global, GlobalElementId, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseDownEvent, Pixels, Style,
    Window, WindowId, point, px, relative, size,
};

/// Where any frame of a page may navigate: the page itself and inline content. Unlike the web's
/// sandboxed iframe, a WebKit document keeps no sandbox across navigation, and every main-frame
/// document gets the IPC bridge, so a remote site never loads; the reader's links open in the
/// browser instead.
pub(crate) fn navigable(url: &str) -> bool {
    ["t3-page:", "about:", "data:", "blob:"].iter().any(|scheme| url.get(..scheme.len()).is_some_and(|head| head.eq_ignore_ascii_case(scheme)))
}

/// Paint order among base-plane content: an occluder hides views of lower layers only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Transcript,
    TranscriptChrome,
    Modal,
}

/// What a view loads.
pub struct Page {
    pub html: Arc<str>,
    pub dark: bool,
    /// Vertical wheel scrolls go to the window instead of the page, so a frame sized to its page
    /// never takes the transcript's scroll.
    pub scroll_through: bool,
    /// Runs at document start in the main frame, after `window.ipc` exists.
    pub script: String,
}

/// What a page sends the app.
#[derive(Debug, Clone, PartialEq)]
pub enum WebEvent {
    /// A string the page posted with `window.ipc.postMessage`.
    Message(String),
    /// An `http(s)` link the reader opened.
    OpenUrl(String),
}

#[cfg(target_os = "macos")]
use macos::NativeView;

#[cfg(not(target_os = "macos"))]
struct NativeView;

#[cfg(not(target_os = "macos"))]
impl NativeView {
    fn new(_: &mut Window, _: &Page, _: UnboundedSender<WebEvent>) -> Option<Self> {
        None
    }
    fn place(&self, _: Bounds<Pixels>, _: Bounds<Pixels>) {}
    fn hide(&self) {}
    fn set_dark(&self, _: bool) {}
    fn set_zoom(&self, _: f64) {}
    fn evaluate(&self, _: &str) {}
    fn release_focus(&self) {}
}

pub struct WebView {
    native: NativeView,
    layer: Layer,
    occluders: Rc<Occluders>,
    leases: Cell<usize>,
    /// The frame and visible part last applied; `None` while hidden.
    applied: Cell<Option<(Bounds<Pixels>, Bounds<Pixels>)>>,
    dark: Cell<bool>,
}

/// Set by the offscreen renderer (`snapshot.rs`), whose hidden windows never show native views.
#[cfg(feature = "snapshot")]
static OFFSCREEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "snapshot")]
pub fn set_offscreen() {
    OFFSCREEN.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the window can host native views: not on other platforms or the offscreen renderer.
pub fn available(window: &Window) -> bool {
    #[cfg(feature = "snapshot")]
    if OFFSCREEN.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    cfg!(target_os = "macos") && window.enable_scene_overlay().is_ok()
}

impl WebView {
    /// A hidden view loading `page`, or `None` where the window cannot host one.
    pub fn new(page: Page, layer: Layer, events: UnboundedSender<WebEvent>, window: &mut Window, cx: &mut App) -> Option<Rc<Self>> {
        if !available(window) {
            return None;
        }
        let native = NativeView::new(window, &page, events)?;
        Some(Rc::new(WebView { native, layer, occluders: occluders(window, cx), leases: Cell::new(0), applied: Cell::new(None), dark: Cell::new(page.dark) }))
    }

    pub fn set_zoom(&self, zoom: f64) {
        self.native.set_zoom(zoom);
    }

    pub fn evaluate(&self, script: &str) {
        self.native.evaluate(script);
    }

    fn set_dark(&self, dark: bool) {
        if self.dark.replace(dark) != dark {
            self.native.set_dark(dark);
        }
    }

    fn hide(&self) {
        if self.applied.take().is_some() {
            self.native.release_focus();
            self.native.hide();
        }
    }

    /// Places the view for the frame being presented: `bounds` clipped to `clip`, minus what
    /// higher layers cover.
    fn present(&self, bounds: Bounds<Pixels>, clip: Bounds<Pixels>) {
        let visible = self.occluders.visible(bounds.intersect(&clip), self.layer);
        if visible.size.width <= px(0.) || visible.size.height <= px(0.) {
            return self.hide();
        }
        if self.applied.get() != Some((bounds, visible)) {
            self.applied.set(Some((bounds, visible)));
            self.native.place(bounds, visible);
        }
    }
}

/// Held in the drawing element's state: the view stays shown while some element draws it.
struct Lease(Rc<WebView>);

impl Lease {
    fn new(view: Rc<WebView>) -> Self {
        view.leases.set(view.leases.get() + 1);
        Lease(view)
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let left = self.0.leases.get().saturating_sub(1);
        self.0.leases.set(left);
        if left == 0 {
            self.0.hide();
        }
    }
}

/// Draws `view` over the element's bounds. `id` must be stable for the view while it shows.
pub fn web_view(id: impl Into<ElementId>, view: Rc<WebView>, dark: bool) -> WebViewElement {
    WebViewElement { id: id.into(), view, dark }
}

pub struct WebViewElement {
    id: ElementId,
    view: Rc<WebView>,
    dark: bool,
}

impl IntoElement for WebViewElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn fill() -> Style {
    let mut style = Style::default();
    style.size.width = relative(1.).into();
    style.size.height = relative(1.).into();
    style
}

impl Element for WebViewElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (window.request_layout(fill(), [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, _: &mut App) {
        // Pointer input over the page goes to the native view; the hitbox keeps hover states
        // beneath it from staying lit, and lets wheel scrolls passed through reach the list.
        window.insert_hitbox(bounds, HitboxBehavior::BlockMouseExceptScroll);
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) {
        let Some(id) = id else { return };
        let view = self.view.clone();
        window.with_element_state::<Lease, _>(id, |lease, _| match lease {
            Some(lease) if Rc::ptr_eq(&lease.0, &view) => ((), lease),
            _ => ((), Lease::new(view.clone())),
        });
        view.set_dark(self.dark);
        let clip = window.content_mask().bounds;
        let presented = view.clone();
        window.on_present(move || presented.present(bounds, clip));
        // A click on GPUI content takes keyboard focus back from the page.
        window.on_mouse_event(move |_: &MouseDownEvent, phase, _, _| {
            if phase == DispatchPhase::Capture {
                view.native.release_focus();
            }
        });
    }
}

struct Occluder {
    layer: Layer,
    bounds: Cell<Bounds<Pixels>>,
}

#[derive(Default)]
struct Occluders(RefCell<Vec<Weak<Occluder>>>);

impl Occluders {
    fn add(&self, occluder: &Rc<Occluder>) {
        let mut list = self.0.borrow_mut();
        list.retain(|weak| weak.strong_count() > 0);
        list.push(Rc::downgrade(occluder));
    }

    /// What is left of `visible` once the occluders above `layer` are cut away, each by the edge
    /// that keeps the most.
    fn visible(&self, mut visible: Bounds<Pixels>, layer: Layer) -> Bounds<Pixels> {
        for occluder in self.0.borrow().iter().filter_map(Weak::upgrade).filter(|occluder| occluder.layer > layer) {
            visible = cut(visible, occluder.bounds.get());
        }
        visible
    }
}

/// `area` minus `cover`, as the largest rectangle that keeps one side of it.
fn cut(area: Bounds<Pixels>, cover: Bounds<Pixels>) -> Bounds<Pixels> {
    if !area.intersects(&cover) {
        return area;
    }
    let (left, top, right, bottom) = (area.left(), area.top(), area.right(), area.bottom());
    [
        Bounds::from_corners(point(left, top), point(right, cover.top().max(top))),
        Bounds::from_corners(point(left, cover.bottom().min(bottom)), point(right, bottom)),
        Bounds::from_corners(point(left, top), point(cover.left().max(left), bottom)),
        Bounds::from_corners(point(cover.right().min(right), top), point(right, bottom)),
    ]
    .into_iter()
    .max_by(|a, b| area_of(a).total_cmp(&area_of(b)))
    .unwrap_or(Bounds { origin: area.origin, size: size(px(0.), px(0.)) })
}

fn area_of(bounds: &Bounds<Pixels>) -> f32 {
    f32::from(bounds.size.width) * f32::from(bounds.size.height)
}

#[derive(Default)]
struct WindowOccluders(HashMap<WindowId, Rc<Occluders>>);

impl Global for WindowOccluders {}

fn occluders(window: &Window, cx: &mut App) -> Rc<Occluders> {
    cx.default_global::<WindowOccluders>().0.entry(window.window_handle().window_id()).or_default().clone()
}

/// Base-plane content drawn over web views of lower layers: fills its parent, which must be
/// positioned, and keeps those views out of its bounds while it is drawn.
pub fn occluder(id: impl Into<ElementId>, layer: Layer) -> OccluderElement {
    OccluderElement { id: id.into(), layer }
}

pub struct OccluderElement {
    id: ElementId,
    layer: Layer,
}

impl IntoElement for OccluderElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for OccluderElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = fill();
        style.position = gpui::Position::Absolute;
        style.inset.top = px(0.).into();
        style.inset.left = px(0.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, id: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        let Some(id) = id else { return };
        let registry = occluders(window, cx);
        let layer = self.layer;
        window.with_element_state::<Rc<Occluder>, _>(id, |occluder, _| {
            let occluder = occluder.filter(|occluder| occluder.layer == layer).unwrap_or_else(|| {
                let occluder = Rc::new(Occluder { layer, bounds: Cell::new(bounds) });
                registry.add(&occluder);
                occluder
            });
            occluder.bounds.set(bounds);
            ((), occluder)
        });
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), _: &mut Window, _: &mut App) {}
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, point, px, size};

    use super::cut;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<gpui::Pixels> {
        Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) }
    }

    /// A WebKit document keeps no sandbox across navigation and every main-frame document gets
    /// the IPC bridge, so a frame may only load the page itself or inline content.
    #[test]
    fn pages_never_navigate_to_remote_documents() {
        for url in ["t3-page://page/index.html", "about:blank", "about:srcdoc", "data:text/html,<p>", "blob:null/1", "T3-PAGE://page/index.html"] {
            assert!(super::navigable(url), "{url}");
        }
        for url in ["https://attacker.example/", "http://127.0.0.1:3773/api", "HTTPS://x.example", "file:///etc/hosts", "javascript:alert(1)", "t3-pag"] {
            assert!(!super::navigable(url), "{url}");
        }
    }

    #[test]
    fn occluders_cut_the_side_that_keeps_most() {
        let frame = rect(0., 0., 700., 400.);
        // A find bar over the top-right corner takes a strip off the top.
        assert_eq!(cut(frame, rect(400., -20., 300., 60.)), rect(0., 40., 700., 360.));
        // A banner along the bottom takes the bottom.
        assert_eq!(cut(frame, rect(-50., 350., 800., 100.)), rect(0., 0., 700., 350.));
        // Covered whole: nothing left.
        let covered = cut(frame, rect(-10., -10., 800., 500.));
        assert_eq!(f32::from(covered.size.width) * f32::from(covered.size.height), 0.);
        // Apart: untouched.
        assert_eq!(cut(frame, rect(800., 0., 10., 10.)), frame);
    }
}
