//! The expanded image view (`ExpandedImageDialog.tsx`, `ZoomableImage.tsx`): a black 75% scrim
//! over the whole window, the image fitted to 92% of it, a caption with its position in the
//! gallery, and arrows between a message's images. Click toggles 2x at the pointer, scroll
//! zooms up to 8x, drag pans; Enter, + / -, 0 and the arrow keys do the same from the keyboard.
//! Escape or a click on the scrim closes it and gives focus back.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext, Context, Entity, FocusHandle, FontWeight, Global, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels,
    Point, Render, ScrollWheelEvent, SharedString, Styled, Task, WeakEntity, Window, anchored, deferred, div, hsla as gpui_hsla, img, point, prelude::*, px,
};
use t3_core::media::{AssetResource, MediaHints, MediaKind, MediaSource};
use gpui_component::tooltip::Tooltip;
use gpui_component::h_flex;

use super::video::{VideoPlayer, VideoPlayerOptions};
use super::{ImageState, MediaScope, Variant};
use crate::i18n::t;
use crate::icons::Icon;
use crate::toast::{Toast, ToastKind};

const MAX_ZOOM: f32 = 8.;
const CAPTION_HEIGHT: f32 = 24.;

#[derive(Default)]
struct FullscreenViewer(Option<WeakEntity<MediaViewer>>);

impl Global for FullscreenViewer {}

#[derive(Clone)]
pub struct ViewerItem {
    pub source: MediaSource,
    pub name: SharedString,
    pub kind: MediaKind,
}

impl ViewerItem {
    pub fn image(source: MediaSource, name: impl Into<SharedString>) -> Self {
        Self { source, name: name.into(), kind: MediaKind::Image }
    }

    pub fn video(source: MediaSource, name: impl Into<SharedString>) -> Self {
        Self { source, name: name.into(), kind: MediaKind::Video }
    }
}

pub struct MediaViewer {
    items: Vec<ViewerItem>,
    index: usize,
    scope: Option<MediaScope>,
    window_handle: Option<AnyWindowHandle>,
    zoom: f32,
    /// Offset of the zoomed image's visible window from its top-left corner.
    pan: Point<Pixels>,
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
    dragged: bool,
    video_player: Option<(MediaSource, Entity<VideoPlayer>)>,
    focus: FocusHandle,
    restore_focus: Option<FocusHandle>,
    mount_count: Arc<AtomicUsize>,
    unmount_tx: mpsc::UnboundedSender<()>,
    _unmount_task: Task<()>,
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
        let mount_count = Arc::new(AtomicUsize::new(0));
        let (unmount_tx, mut unmount_rx) = mpsc::unbounded();
        let _unmount_task = cx.spawn(async move |this, cx| {
            while unmount_rx.next().await.is_some() {
                let _ = this.update(cx, |viewer, cx| {
                    if viewer.mount_count.load(Ordering::Acquire) == 0 && viewer.is_open() {
                        viewer.dismiss(cx);
                    }
                });
            }
        });
        MediaViewer {
            items: Vec::new(),
            index: 0,
            scope: None,
            window_handle: None,
            zoom: 1.,
            pan: Point::default(),
            drag: None,
            dragged: false,
            video_player: None,
            focus: cx.focus_handle(),
            restore_focus: None,
            mount_count,
            unmount_tx,
            _unmount_task,
        }
    }

    pub fn is_open(&self) -> bool { !self.items.is_empty() }

    pub fn open(&mut self, items: Vec<ViewerItem>, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.open_with_scope(items, index, None, window, cx);
    }

    pub fn open_with_scope(&mut self, items: Vec<ViewerItem>, index: usize, scope: Option<MediaScope>, window: &mut Window, cx: &mut Context<Self>) {
        if items.is_empty() {
            return;
        }
        self.restore_focus = window.focused(cx);
        self.scope = scope.or_else(|| cx.try_global::<super::Media>().and_then(|media| media.scope.clone()));
        self.window_handle = Some(window.window_handle());
        self.items = items;
        self.index = index.min(self.items.len() - 1);
        self.reset_zoom();
        if let Some((_, player)) = self.video_player.take() {
            player.update(cx, |p, cx| p.release_webview(cx));
        }
        self.pin(cx);
        window.focus(&self.focus, cx);
        self.sync_fullscreen_with(crate::webview::available(window), cx);
        window.refresh();
        cx.notify();
    }

    fn sync_fullscreen_with(&self, can_webview: bool, cx: &mut Context<Self>) {
        let active_video = self.items.get(self.index).is_some_and(|item| item.kind == MediaKind::Video);
        if active_video && can_webview {
            cx.default_global::<FullscreenViewer>().0 = Some(cx.entity().downgrade());
        } else {
            clear_fullscreen_for(cx.entity_id(), cx);
        }
    }

    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) {
        let had_state = !self.items.is_empty() || self.video_player.is_some() || self.scope.is_some();
        self.items.clear();
        self.drag = None;
        self.scope = None;
        self.window_handle = None;
        if let Some((_, player)) = self.video_player.take() {
            player.update(cx, |p, cx| p.release_webview(cx));
        }
        clear_fullscreen_for(cx.entity_id(), cx);
        if let Some(cache) = cx.try_global::<super::Media>().map(|media| media.cache.clone()) {
            cache.update(cx, |cache, _| cache.unpin_all());
        }
        if had_state {
            cx.refresh_windows();
            cx.notify();
        }
    }

    pub(crate) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(cx);
        if let Some(focus) = self.restore_focus.take() {
            window.focus(&focus, cx);
        }
        window.refresh();
    }

    fn pin(&self, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(self.index) else { return };
        if item.kind != MediaKind::Image {
            if let Some(cache) = cx.try_global::<super::Media>().map(|media| media.cache.clone()) {
                cache.update(cx, |cache, _| cache.unpin_all());
            }
            return;
        }
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

    fn navigate(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.items.len() as isize;
        if count <= 1 {
            return;
        }
        self.index = (self.index as isize + step).rem_euclid(count) as usize;
        self.reset_zoom();
        if let Some((_, player)) = self.video_player.take() {
            player.update(cx, |p, cx| p.release_webview(cx));
        }
        self.pin(cx);
        self.sync_fullscreen_with(crate::webview::available(window), cx);
        window.refresh();
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

    fn apply_scroll_wheel(&mut self, event: &ScrollWheelEvent, natural: (u32, u32), window: &Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(16.));
        if event.modifiers.control {
            let factor = (f32::from(delta.y) * 0.01).exp();
            self.zoom_to(self.zoom * factor, Some(event.position), natural, window, cx);
            return;
        }
        if self.zoom > 1. {
            self.pan -= delta;
            let frame = self.frame(natural, window);
            self.clamp_pan(&frame);
            cx.notify();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let kind = self.items.get(self.index).map_or(MediaKind::Image, |item| item.kind);
        let natural = if kind == MediaKind::Image { self.natural(cx) } else { None };
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
            ("left", _) => self.navigate(-1, window, cx),
            ("right", _) => self.navigate(1, window, cx),
            ("enter" | "space", Some(natural)) => self.zoom_to(if self.zoom > 1. { 1. } else { 2. }, None, natural, window, cx),
            ("+" | "=", Some(natural)) => self.zoom_to(self.zoom * 1.5, None, natural, window, cx),
            ("-", Some(natural)) => self.zoom_to(self.zoom / 1.5, None, natural, window, cx),
            ("0", Some(natural)) => self.zoom_to(1., None, natural, window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

fn clear_fullscreen_for(id: gpui::EntityId, cx: &mut App) {
    if let Some(slot) = cx.try_global::<FullscreenViewer>().and_then(|g| g.0.as_ref())
        && slot.entity_id() == id
    {
        cx.global_mut::<FullscreenViewer>().0 = None;
    }
}

/// Fullscreen modal video layer rendered in `workspace.rs` base plane so native `WKWebView`
/// sits above GPUI overlays and occludes transcript webviews underneath.
pub fn fullscreen(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let viewer = cx.try_global::<FullscreenViewer>()?.0.as_ref()?.upgrade()?;
    let guard = viewer.read(cx);
    if guard.window_handle != Some(window.window_handle()) {
        return None;
    }
    let active_video = guard.items.get(guard.index).is_some_and(|item| item.kind == MediaKind::Video);
    if !active_video || !crate::webview::available(window) {
        return None;
    }
    Some(viewer.update(cx, |this, cx| this.render_modal(true, window, cx)))
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

fn video_copy_reference(source: &MediaSource, scope: Option<&MediaScope>) -> Option<(&'static str, String)> {
    match source {
        MediaSource::Direct(url) | MediaSource::Asset(AssetResource::SourceControlMedia { url, .. })
            if url.starts_with("https://") || url.starts_with("http://") => Some(("Copy URL", url.clone())),
        MediaSource::Asset(AssetResource::MediaFile { path, .. } | AssetResource::RemoteMediaFile { path, .. }) => {
            if path.starts_with('/') || t3_core::media::is_windows_absolute_path(path) {
                Some(("Copy full path", path.clone()))
            } else if let Some(root) = scope.and_then(|scope| scope.workspace_root.as_deref()) {
                let separator = if t3_core::media::is_windows_absolute_path(root) { '\\' } else { '/' };
                Some(("Copy full path", format!("{}{separator}{path}", root.trim_end_matches(['/', '\\']))))
            } else {
                Some(("Copy relative path", path.clone()))
            }
        }
        _ => None,
    }
}

fn save_video(
    source: MediaSource,
    name: &str,
    state: Entity<crate::app_state::AppState>,
    cx: &mut App,
) {
    let client = state.read(cx).client_handle();
    let runtime = state.read(cx).runtime().clone();
    let dir = std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join("Downloads"))
        .unwrap_or_else(std::env::temp_dir);
    let prompt = cx.prompt_for_new_path(dir.as_path(), Some(name));
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(path))) = prompt.await else { return };
        let result = runtime.spawn(async move {
            let fetched = if let Some(client) = client {
                t3_client::assets::fetch_media(&client, &source).await?
            } else if let MediaSource::Direct(url) = &source {
                t3_client::assets::fetch_data_media(url)?
            } else {
                return Err("Video unavailable.".to_owned());
            };
            tokio::fs::write(path, fetched.bytes).await.map_err(|error| error.to_string())
        }).await.unwrap_or_else(|error| Err(error.to_string()));
        if let Err(error) = result {
            state.update(cx, |app, cx| {
                app.show_toast(Toast::new(ToastKind::Error, t(cx, "Could not save video")).description(error), cx);
            });
        }
    }).detach();
}

fn media_actions(item: &ViewerItem, scope: Option<&MediaScope>, cx: &App) -> Option<AnyElement> {
    let media = cx.try_global::<super::Media>()?;
    let cache = media.cache.read(cx);
    let bytes = (item.kind == MediaKind::Image).then(|| cache.original_bytes(&item.source, cx)).flatten();
    let image = bytes.as_ref().and_then(|bytes| {
        let format = image::guess_format(bytes).ok()?;
        gpui::ImageFormat::from_mime_type(format.to_mime_type())
    });
    let source = item.source.clone();
    let can_open = match &source {
        MediaSource::Asset(_) => true,
        MediaSource::Direct(url) => url.starts_with("https://") || url.starts_with("http://"),
    };
    let video = item.kind == MediaKind::Video;
    let copy_reference = video.then(|| video_copy_reference(&source, scope)).flatten();
    if bytes.is_none() && !can_open && !video {
        return None;
    }

    let app = cache.state.clone();
    let name = std::path::Path::new(item.name.as_ref()).file_name().and_then(|name| name.to_str()).unwrap_or(if video { "video" } else { "image" }).to_owned();
    let action = |id, glyph, label| {
        let label = t(cx, label);
        media_button(id, glyph, 28., true).aria_label(label)
            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
    };
    let video_save = video.then(|| {
        let source = source.clone();
        let name = name.clone();
        let app = app.clone();
        action("media-viewer-save-video", Icon::Download, "Save video").on_click(move |_, _, cx| {
            save_video(source.clone(), &name, app.clone(), cx);
        })
    });
    Some(h_flex()
        .id("media-viewer-actions")
        .absolute().top(px(16.)).left(px(16.))
        .gap(px(6.))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .children(bytes.as_ref().zip(image).map(|(bytes, format)| {
            let bytes = bytes.clone();
            action("media-viewer-copy", Icon::Copy, "Copy image").on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(format, bytes.to_vec())));
            })
        }))
        .children(bytes.map(|bytes| {
            action("media-viewer-save", Icon::Download, "Save image").on_click(move |_, _, cx| {
                let dir = std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join("Downloads"))
                    .unwrap_or_else(std::env::temp_dir);
                let prompt = cx.prompt_for_new_path(dir.as_path(), Some(&name));
                let bytes = bytes.clone();
                let app = app.clone();
                cx.spawn(async move |cx| {
                    let Ok(Ok(Some(path))) = prompt.await else { return };
                    let result = cx.background_executor().spawn(async move { std::fs::write(path, bytes.as_ref()) }).await;
                    if let Err(error) = result {
                        app.update(cx, |app, cx| {
                            app.show_toast(Toast::new(ToastKind::Error, t(cx, "Could not save image")).description(error.to_string()), cx);
                        });
                    }
                }).detach();
            })
        }))
        .children(copy_reference.map(|(label, reference)| {
            action("media-viewer-copy-video-reference", Icon::Copy, label).on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(reference.clone()));
            })
        }))
        .children(video_save)
        .when(can_open, |this| this.child(action("media-viewer-open", Icon::ExternalLink, "Open in browser")
            .on_click(move |_, _, cx| match &source {
                MediaSource::Asset(resource) => super::open_asset(resource, cx),
                MediaSource::Direct(url) => cx.open_url(url),
            })))
        .into_any_element())
}

impl MediaViewer {
    fn video_entity(&mut self, item: &ViewerItem, viewport: gpui::Size<Pixels>, cx: &mut Context<Self>) -> Entity<VideoPlayer> {
        let scope = self.scope.clone().or_else(|| cx.try_global::<super::Media>().and_then(|media| media.scope.clone()));
        let revision = scope.as_ref().and_then(|scope| super::revision_for(&item.source, scope).map(str::to_owned));
        let viewer_handle = cx.entity().downgrade();
        let options = VideoPlayerOptions {
            source: item.source.clone(),
            name: item.name.clone(),
            revision,
            hints: MediaHints::default(),
            modal: true,
            max_width: (f32::from(viewport.width) - if self.items.len() > 1 { 136. } else { 64. }).clamp(240., f32::from(viewport.width) * 0.88),
            max_height: (f32::from(viewport.height) * 0.78).max(200.),
            on_expand: None,
            on_open_external: None,
            on_close_modal: Some(std::sync::Arc::new(move |window, cx| {
                if let Some(viewer) = viewer_handle.upgrade() {
                    viewer.update(cx, |this, cx| this.close(window, cx));
                }
            })),
        };
        match &self.video_player {
            Some((source, entity)) if *source == item.source => {
                entity.update(cx, |player, cx| player.update_options(options, scope, cx));
                entity.clone()
            }
            _ => {
                let entity = cx.new(|cx| VideoPlayer::new(options, scope, cx));
                self.video_player = Some((item.source.clone(), entity.clone()));
                entity
            }
        }
    }

    fn render_modal(&mut self, base_plane: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.items.get(self.index).cloned() else { return div().into_any_element() };
        let owner = cx.entity_id();
        let viewport = window.viewport_size();
        let white = |alpha: f32| gpui_hsla(0., 0., 1., alpha);
        let count = self.items.len();
        let caption: SharedString = if count > 1 { format!("{} ({}/{count})", item.name, self.index + 1).into() } else { item.name.clone() };
        let (media, observer) = if item.kind == MediaKind::Video {
            let player = self.video_entity(&item, viewport, cx);
            (
                div()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(player)
                    .into_any_element(),
                None,
            )
        } else {
            let full = super::image_in_scope(&item.source, Variant::Full, owner, self.scope.as_ref(), cx);
            let shown = match full {
                ImageState::Ready(image) => Ok(Some(image)),
                ImageState::Failed(message) => Err(message),
                ImageState::Loading => match super::image_in_scope(&item.source, Variant::Thumb, owner, self.scope.as_ref(), cx) {
                    ImageState::Ready(image) => Ok(Some(image)),
                    _ => Ok(None),
                },
            };
            let natural = self.natural(cx);
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
                            this.apply_scroll_wheel(event, natural, window, cx);
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
            (media, observer)
        };
        let backdrop = div()
            .id("media-viewer")
            .track_focus(&self.focus)
            .key_context("MediaViewer")
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .when(base_plane, |el| el.size_full())
            .when(!base_plane, |el| el.w(viewport.width).h(viewport.height))
            .occlude()
            .bg(gpui_hsla(0., 0., 0., 0.75))
            .when(base_plane, |el| el.child(crate::webview::occluder("media-viewer-fullscreen-cover", crate::webview::Layer::Modal)))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(media)
            .children(observer)
            .children(media_actions(&item, self.scope.as_ref(), cx))
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
                            .on_click(cx.listener(|this, _, window, cx| this.navigate(-1, window, cx))),
                    ),
                )
                .child(
                    div().absolute().right(px(16.)).top_0().bottom_0().flex().items_center().child(
                        media_button("media-viewer-next", Icon::ChevronRight, 36., false)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, window, cx| this.navigate(1, window, cx))),
                    ),
                )
            });
        if base_plane {
            div().absolute().inset_0().child(backdrop).into_any_element()
        } else {
            deferred(anchored().position(point(px(0.), px(0.))).child(backdrop)).with_priority(60).into_any_element()
        }
    }
}

impl Render for MediaViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mount_id: SharedString = format!("media-viewer-mount-{}", cx.entity_id().as_u64()).into();
        let tracker = super::video::mount_tracker(mount_id, self.mount_count.clone(), self.unmount_tx.clone());
        let Some(item) = self.items.get(self.index).cloned() else {
            clear_fullscreen_for(cx.entity_id(), cx);
            return div().child(tracker).into_any_element();
        };
        if item.kind == MediaKind::Video && crate::webview::available(window) {
            self.window_handle = Some(window.window_handle());
            cx.default_global::<FullscreenViewer>().0 = Some(cx.entity().downgrade());
            return div().child(tracker).into_any_element();
        }
        clear_fullscreen_for(cx.entity_id(), cx);
        div().child(tracker).child(self.render_modal(false, window, cx)).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn viewer_navigates_mixed_image_and_video_gallery(cx: &mut gpui::TestAppContext) {
        let (viewer, cx) = cx.add_window_view(|_, cx| MediaViewer::new(cx));
        let items = vec![
            ViewerItem::image(MediaSource::Direct("data:image/png;base64,iVBORw0KGgo=".into()), "one.png"),
            ViewerItem::video(MediaSource::Direct("https://example.com/clip.mp4".into()), "clip.mp4"),
        ];
        cx.update(|window, cx| {
            viewer.update(cx, |v, cx| {
                v.open(items, 0, window, cx);
                assert!(v.is_open());
                assert_eq!(v.index, 0);
                v.navigate(1, window, cx);
                assert_eq!(v.index, 1);
                assert_eq!(v.items[1].kind, MediaKind::Video);
                v.navigate(-1, window, cx);
                assert_eq!(v.index, 0);
                v.close(window, cx);
                assert!(!v.is_open());
            });
        });
    }

    #[gpui::test]
    fn open_with_scope_syncs_fullscreen_preserves_surface_scope_and_dismisses_cleanly(cx: &mut gpui::TestAppContext) {
        let (viewer, cx) = cx.add_window_view(|_, cx| MediaViewer::new(cx));
        let global_viewer = cx.update(|_, cx| cx.new(MediaViewer::new));
        let global_scope = MediaScope {
            owner: global_viewer.entity_id(),
            thread_id: Some("thread-global".into()),
            workspace_root: Some("/global".into()),
            workspace_mutation_id: Some("mut-global".into()),
            source_control_media: false,
            source_control_media_repository: None,
            viewer: global_viewer.downgrade(),
        };
        let preview_scope = MediaScope {
            owner: viewer.entity_id(),
            thread_id: Some("thread-preview".into()),
            workspace_root: Some("/preview/docs".into()),
            workspace_mutation_id: Some("mut-preview".into()),
            source_control_media: false,
            source_control_media_repository: None,
            viewer: viewer.downgrade(),
        };
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let items = vec![
            ViewerItem::video(MediaSource::Direct("https://example.com/clip.mp4".into()), "clip.mp4"),
            ViewerItem::image(MediaSource::Direct("data:image/png;base64,iVBORw0KGgo=".into()), "two.png"),
        ];
        cx.update(|window, cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let state = cx.new(|cx| crate::app_state::AppState::new(runtime.handle().clone(), cx));
            crate::media::cache(&state, cx);
            crate::media::set_scope(global_scope, cx);

            viewer.update(cx, |v, cx| {
                v.open_with_scope(items, 0, Some(preview_scope.clone()), window, cx);
                assert_eq!(v.scope.as_ref().and_then(|s| s.workspace_root.as_deref()), Some("/preview/docs"));
                assert_eq!(v.scope.as_ref().and_then(|s| s.workspace_mutation_id.as_deref()), Some("mut-preview"));
                assert_eq!(v.window_handle, Some(window.window_handle()));
                let viewport = window.viewport_size();
                let player = v.video_entity(&v.items[0].clone(), viewport, cx);
                assert_eq!(
                    player.read(cx).scope_for_test().and_then(|s| s.workspace_root.as_deref()),
                    Some("/preview/docs"),
                    "modal VideoPlayer must inherit the opening surface scope rather than global Media::scope",
                );
                // Even before render(), opening a video updates FullscreenViewer when webviews are enabled.
                v.sync_fullscreen_with(true, cx);
                assert!(cx.try_global::<FullscreenViewer>().and_then(|g| g.0.as_ref()).is_some());
                // Stepping from video -> image clears FullscreenViewer.
                v.navigate(1, window, cx);
                assert!(cx.try_global::<FullscreenViewer>().and_then(|g| g.0.as_ref()).is_none());
                // Stepping back to video restores FullscreenViewer, and dismiss() clears it without a Window.
                v.navigate(-1, window, cx);
                v.sync_fullscreen_with(true, cx);
                assert!(cx.try_global::<FullscreenViewer>().and_then(|g| g.0.as_ref()).is_some());
                v.dismiss(cx);
                assert!(!v.is_open());
                assert!(v.scope.is_none());
                assert!(cx.try_global::<FullscreenViewer>().and_then(|g| g.0.as_ref()).is_none());
            });
        });
    }

    #[gpui::test]
    fn wheel_zooms_only_with_control_and_plain_wheel_pans(cx: &mut gpui::TestAppContext) {
        let (viewer, cx) = cx.add_window_view(|_, cx| MediaViewer::new(cx));
        let natural = (2000, 1200);
        cx.update(|window, cx| {
            viewer.update(cx, |v, cx| {
                v.open(vec![ViewerItem::image(MediaSource::Direct("data:image/png;base64,iVBORw0KGgo=".into()), "big.png")], 0, window, cx);
                let plain = ScrollWheelEvent {
                    delta: gpui::ScrollDelta::Pixels(point(px(-20.), px(-40.))),
                    modifiers: gpui::Modifiers::none(),
                    ..Default::default()
                };
                v.apply_scroll_wheel(&plain, natural, window, cx);
                assert!((v.zoom - 1.).abs() < f32::EPSILON, "plain wheel must not zoom");

                let pinch = ScrollWheelEvent {
                    delta: gpui::ScrollDelta::Pixels(point(px(0.), px(60.))),
                    modifiers: gpui::Modifiers { control: true, ..Default::default() },
                    ..Default::default()
                };
                v.apply_scroll_wheel(&pinch, natural, window, cx);
                assert!(v.zoom > 1.2, "ctrl/pinch wheel should zoom");

                let before_pan = v.pan;
                v.apply_scroll_wheel(&plain, natural, window, cx);
                assert!(v.pan.x > before_pan.x || v.pan.y > before_pan.y, "plain wheel while zoomed should pan");
            });
        });
    }
}
