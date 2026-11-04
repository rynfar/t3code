//! The expanded image view (`ExpandedImageDialog.tsx`, `ZoomableImage.tsx`): a black 75% scrim
//! over the whole window, the image fitted to 92% of it, a caption with its position in the
//! gallery, and arrows between a message's images. Click toggles 2x at the pointer, scroll
//! zooms up to 8x, drag pans; Enter, + / -, 0 and the arrow keys do the same from the keyboard.
//! Escape or a click on the scrim closes it and gives focus back.

use gpui::{
    App, Context, FocusHandle, FontWeight, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, Styled, Window, anchored, deferred, div, hsla as gpui_hsla, img, point, prelude::*, px,
};
use t3_core::media::MediaSource;

use super::{ImageState, Variant};
use crate::icons::Icon;

const MAX_ZOOM: f32 = 8.;
const CAPTION_HEIGHT: f32 = 24.;

#[derive(Clone)]
pub struct ViewerItem {
    pub source: MediaSource,
    pub name: SharedString,
}

pub struct MediaViewer {
    items: Vec<ViewerItem>,
    index: usize,
    zoom: f32,
    /// Offset of the zoomed image's visible window from its top-left corner.
    pan: Point<Pixels>,
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
    dragged: bool,
    focus: FocusHandle,
    restore_focus: Option<FocusHandle>,
}

struct Frame {
    /// The fitted image at the current zoom.
    width: f32,
    height: f32,
    /// The visible box: the image, clipped to the space the dialog allows.
    box_width: f32,
    box_height: f32,
    origin: Point<Pixels>,
}

impl MediaViewer {
    pub fn new(cx: &mut Context<Self>) -> Self {
        MediaViewer { items: Vec::new(), index: 0, zoom: 1., pan: Point::default(), drag: None, dragged: false, focus: cx.focus_handle(), restore_focus: None }
    }

    pub fn open(&mut self, items: Vec<ViewerItem>, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if items.is_empty() {
            return;
        }
        self.restore_focus = window.focused(cx);
        self.items = items;
        self.index = index.min(self.items.len() - 1);
        self.reset_zoom();
        self.pin(cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.items.clear();
        self.drag = None;
        if let Some(cache) = cx.try_global::<super::Media>().map(|media| media.cache.clone()) {
            cache.update(cx, |cache, _| cache.unpin_all());
        }
        if let Some(focus) = self.restore_focus.take() {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn pin(&self, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(self.index) else { return };
        if let Some(cache) = cx.try_global::<super::Media>().map(|media| media.cache.clone()) {
            let source = item.source.clone();
            cache.update(cx, |cache, cx| {
                cache.unpin_all();
                cache.pin(&source, Variant::Full, cx);
                cache.pin(&source, Variant::Thumb, cx);
            });
        }
    }

    fn reset_zoom(&mut self) {
        self.zoom = 1.;
        self.pan = Point::default();
    }

    fn navigate(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.items.len() as isize;
        if count <= 1 {
            return;
        }
        self.index = (self.index as isize + step).rem_euclid(count) as usize;
        self.reset_zoom();
        self.pin(cx);
        cx.notify();
    }

    /// `fit`: the image's own size, scaled down to 92% of the window (less the arrows' gutters)
    /// and the height the caption leaves.
    fn frame(&self, natural: (u32, u32), window: &Window) -> Frame {
        let viewport = window.viewport_size();
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let max_width = (vw * 0.92 - if vw >= 640. { 96. } else { 0. }).max(1.);
        let max_height = (vh * 0.86).min(vh - 160.).max(1.);
        let (nw, nh) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
        let fit = (max_width / nw).min(max_height / nh).min(1.);
        let (width, height) = (nw * fit * self.zoom, nh * fit * self.zoom);
        let (box_width, box_height) = (width.min(max_width), height.min(max_height));
        let origin = point(px((vw - box_width) / 2.), px((vh - box_height - CAPTION_HEIGHT) / 2.));
        Frame { width, height, box_width, box_height, origin }
    }

    fn clamp_pan(&mut self, frame: &Frame) {
        self.pan.x = self.pan.x.clamp(px(0.), px((frame.width - frame.box_width).max(0.)));
        self.pan.y = self.pan.y.clamp(px(0.), px((frame.height - frame.box_height).max(0.)));
    }

    /// Zooms keeping the image point under `anchor` (window coordinates) in place.
    fn zoom_to(&mut self, next: f32, anchor: Option<Point<Pixels>>, natural: (u32, u32), window: &Window, cx: &mut Context<Self>) {
        let next = next.clamp(1., MAX_ZOOM);
        if (next - self.zoom).abs() < f32::EPSILON {
            return;
        }
        let before = self.frame(natural, window);
        let anchor = anchor.unwrap_or(point(before.origin.x + px(before.box_width / 2.), before.origin.y + px(before.box_height / 2.)));
        let (local_x, local_y) = (f32::from(anchor.x - before.origin.x), f32::from(anchor.y - before.origin.y));
        let (image_x, image_y) = ((f32::from(self.pan.x) + local_x) / self.zoom, (f32::from(self.pan.y) + local_y) / self.zoom);
        self.zoom = next;
        let after = self.frame(natural, window);
        let (local_x, local_y) = (f32::from(anchor.x - after.origin.x), f32::from(anchor.y - after.origin.y));
        self.pan = point(px(image_x * next - local_x), px(image_y * next - local_y));
        self.clamp_pan(&after);
        cx.notify();
    }

    fn natural(&self, cx: &App) -> Option<(u32, u32)> {
        super::dimensions(&self.items.get(self.index)?.source, cx)
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let natural = self.natural(cx);
        let key = event.keystroke.key.as_str();
        let pan_step = px(64.);
        match (key, natural) {
            ("escape", _) => self.close(window, cx),
            ("left" | "right" | "up" | "down", Some(natural)) if self.zoom > 1. => {
                match key {
                    "left" => self.pan.x -= pan_step,
                    "right" => self.pan.x += pan_step,
                    "up" => self.pan.y -= pan_step,
                    _ => self.pan.y += pan_step,
                }
                let frame = self.frame(natural, window);
                self.clamp_pan(&frame);
                cx.notify();
            }
            ("left", _) => self.navigate(-1, cx),
            ("right", _) => self.navigate(1, cx),
            ("enter" | "space", Some(natural)) => self.zoom_to(if self.zoom > 1. { 1. } else { 2. }, None, natural, window, cx),
            ("+" | "=", Some(natural)) => self.zoom_to(self.zoom * 1.5, None, natural, window, cx),
            ("-", Some(natural)) => self.zoom_to(self.zoom / 1.5, None, natural, window, cx),
            ("0", Some(natural)) => self.zoom_to(1., None, natural, window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

/// `media-close` / `media-navigation` buttons: white glyphs on the scrim.
fn media_button(id: &'static str, icon: Icon, size: f32, filled: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .when(filled, |this| this.bg(gpui_hsla(0., 0., 0., 0.65)).border_1().border_color(gpui_hsla(0., 0., 1., 0.2)).hover(|this| this.bg(gpui_hsla(0., 0., 0., 0.8))))
        .when(!filled, |this| this.hover(|this| this.bg(gpui_hsla(0., 0., 1., 0.1))))
        .child(icon.render(px(if filled { 16. } else { 20. }), gpui_hsla(0., 0., 1., if filled { 1. } else { 0.9 })))
}

impl Render for MediaViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(item) = self.items.get(self.index).cloned() else { return div().into_any_element() };
        let owner = cx.entity_id();
        let full = super::image(&item.source, Variant::Full, owner, cx);
        let shown = match full {
            ImageState::Ready(image) => Ok(Some(image)),
            ImageState::Failed(message) => Err(message),
            ImageState::Loading => match super::image(&item.source, Variant::Thumb, owner, cx) {
                ImageState::Ready(image) => Ok(Some(image)),
                _ => Ok(None),
            },
        };
        let natural = self.natural(cx);
        let viewport = window.viewport_size();
        let white = |alpha: f32| gpui_hsla(0., 0., 1., alpha);
        let count = self.items.len();
        let caption: SharedString = if count > 1 { format!("{} ({}/{count})", item.name, self.index + 1).into() } else { item.name.clone() };
        let state_box = |text: SharedString| {
            div()
                .w(px(512.0_f32.min(f32::from(viewport.width) * 0.92)))
                .h(px(192.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .p(px(24.))
                .rounded(px(8.))
                .border_1()
                .border_color(white(0.15))
                .bg(gpui_hsla(0., 0., 0., 1.))
                .text_sm()
                .text_color(white(1.))
                .child(text)
                .into_any_element()
        };
        let media = match (shown, natural) {
            (Err(message), _) => state_box(message),
            (Ok(Some(image)), Some(natural)) => {
                let frame = self.frame(natural, window);
                self.clamp_pan(&frame);
                let zoomed = self.zoom > 1.;
                div()
                    .id("media-viewer-image")
                    .relative()
                    .w(px(frame.box_width))
                    .h(px(frame.box_height))
                    .overflow_hidden()
                    .rounded(px(8.))
                    .cursor(if zoomed { if self.drag.is_some() { gpui::CursorStyle::ClosedHand } else { gpui::CursorStyle::OpenHand } } else { gpui::CursorStyle::PointingHand })
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.dragged = false;
                        if this.zoom > 1. {
                            this.drag = Some((event.position, this.pan));
                        }
                    }))
                    .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, window, cx| {
                        let Some((start, pan)) = this.drag else { return };
                        if !event.dragging() {
                            this.drag = None;
                            return;
                        }
                        let delta = event.position - start;
                        if f32::from(delta.x).abs() + f32::from(delta.y).abs() > 3. {
                            this.dragged = true;
                        }
                        this.pan = pan - delta;
                        let frame = this.frame(natural, window);
                        this.clamp_pan(&frame);
                        cx.notify();
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(move |this, event: &gpui::MouseUpEvent, window, cx| {
                        this.drag = None;
                        if std::mem::take(&mut this.dragged) {
                            cx.notify();
                            return;
                        }
                        let next = if this.zoom > 1. { 1. } else { 2. };
                        this.zoom_to(next, Some(event.position), natural, window, cx);
                    }))
                    .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, window, cx| {
                        let delta = f32::from(event.delta.pixel_delta(px(16.)).y);
                        let factor = (delta * if event.modifiers.control { 0.01 } else { 0.002 }).exp();
                        this.zoom_to(this.zoom * factor, Some(event.position), natural, window, cx);
                        cx.stop_propagation();
                    }))
                    .child(img(image).absolute().left(-self.pan.x).top(-self.pan.y).w(px(frame.width)).h(px(frame.height)))
                    .into_any_element()
            }
            _ => state_box("Loading image…".into()),
        };
        let observer = cx.try_global::<super::Media>().and_then(|media| media.scope.clone()).map(|mut scope| {
            scope.owner = owner;
            super::observe_image(item.source.clone(), scope)
        });
        let backdrop = div()
            .id("media-viewer")
            .track_focus(&self.focus)
            .key_context("MediaViewer")
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .w(viewport.width)
            .h(viewport.height)
            .occlude()
            .bg(gpui_hsla(0., 0., 0., 0.75))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(media)
            .children(observer)
            .child(
                div()
                    .h(px(CAPTION_HEIGHT))
                    .pt(px(8.))
                    .max_w(px(f32::from(viewport.width) * 0.92))
                    .truncate()
                    .text_xs()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(white(0.8))
                    .child(caption),
            )
            .child(
                div().absolute().top(px(16.)).right(px(16.)).child(
                    media_button("media-viewer-close", Icon::X, 28., true).on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                ),
            )
            .when(count > 1, |this| {
                this.child(
                    div().absolute().left(px(16.)).top_0().bottom_0().flex().items_center().child(
                        media_button("media-viewer-previous", Icon::ChevronLeft, 36., false)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, _, cx| this.navigate(-1, cx))),
                    ),
                )
                .child(
                    div().absolute().right(px(16.)).top_0().bottom_0().flex().items_center().child(
                        media_button("media-viewer-next", Icon::ChevronRight, 36., false)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, _, cx| this.navigate(1, cx))),
                    ),
                )
            });
        deferred(anchored().position(point(px(0.), px(0.))).child(backdrop)).with_priority(60).into_any_element()
    }
}
