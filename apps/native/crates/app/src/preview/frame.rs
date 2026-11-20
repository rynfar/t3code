//! One browser tab's stream and the page drawn from it, as `browser/ServerBrowserSurface.tsx`
//! shows it: the control bar (who drives the tab, Take or Release control), the newest frame
//! scaled to fit, pointer, wheel and key input in page pixels while this viewer has control, the
//! agent's cursor (`AgentCursorMark`) while it does not, page dialogs, file picker uploads with
//! stale-picker withdrawal on drop/stop (`pingdotgg/t3code#29655`), file downloads, region/point
//! annotation selection (`pingdotgg/t3code#34796`), and the host setup and refused states.
//! Frames decode on the network runtime; only the newest waits, and the view repaints once per
//! decoded frame, never on a timer.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, Corners, DispatchPhase, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection, ParentElement,
    PathPromptOptions, Pixels, Render, RenderImage, ScrollDelta, ScrollWheelEvent, Styled, Task, Window, canvas, div, point, prelude::*, px, size,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::preview_stream::{Frame, PreviewStream, StreamEvent, StreamSender, StreamTarget, download_file, upload_file_chooser};
use t3_core::preview::annotation::{AnnotationRegion, AnnotationTarget};
use t3_core::preview::stream::{
    AGENT_CURSOR_ACTIVE_MS, Control, Controller, FileChooserOffer, HostSetup, KeyAction, MouseAction, MouseButton as PageButton,
    NO_FRAME_STALLED_MESSAGE, NO_FRAME_TIMEOUT_MS, Pointer, RESIZE_THROTTLE_MS, StreamInput, StreamMessage, UploadFile, Viewport, WHEEL_LINE_PX,
    control_label, cursor_position, dom_key, fit, grow_cap, may_send, modifiers, page_point,
};

use super::annotation::crop_frame_to_png;
use super::{Abort, Previews};
use crate::app_state::AppState;
use crate::composer::controls::{Button as BarButton, ButtonSize as BarSize, ButtonVariant as BarVariant};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

/// The control bar: an `xs` button with `py-1` and its bottom border, held with or without the
/// button so overlays below it never shift.
pub const CONTROL_BAR_HEIGHT: f32 = 33.;

/// Pointer moves and wheel deltas coalesce for about one display frame, as the web batches them
/// per animation frame.
const INPUT_FLUSH_MS: u64 = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum FrameEvent {
    /// Control, connection or the first frame changed; the chrome row follows.
    Changed,
    /// User picked a region or point on the streamed page while annotation mode is active.
    Picked(AnnotationTarget),
    /// User pressed Escape while annotation pick mode is active (`pingdotgg/t3code#34796`).
    CancelPick,
}

struct Decoded {
    image: Arc<RenderImage>,
    size: (u32, u32),
    jpeg: Frame,
}

fn decode(bytes: Frame) -> Option<Decoded> {
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()?;
    let size = (image.width(), image.height());
    let mut pixels = image.into_rgba8();
    // GPUI's atlas takes BGRA.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Decoded { image: Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])), size, jpeg: bytes })
}

/// The open connection; dropping it closes the socket and stops decoding.
struct Live {
    input: StreamSender,
    _decoder: Abort,
    _frames: Task<()>,
    _events: Task<()>,
}

pub struct FrameView {
    state: Entity<AppState>,
    previews: Entity<Previews>,
    thread_id: String,
    tab_id: String,
    focus: FocusHandle,
    visible: bool,
    follow_size: bool,
    live: Option<Live>,
    image: Option<Arc<RenderImage>>,
    latest_jpeg: Option<Frame>,
    /// The frame before `image`, freed once the next one replaces it so the atlas never drops a
    /// tile the last drawn scene still uses.
    retired: Option<Arc<RenderImage>>,
    frame_size: (u32, u32),
    viewport: Option<Viewport>,
    control: Option<Control>,
    file_chooser: Option<FileChooserOffer>,
    host_setup: Option<HostSetup>,
    refused: bool,
    gone: bool,
    /// `framesStalled` (`ServerBrowserSurface.tsx`, `pingdotgg/t3code#38865`): connected without
    /// receiving any frame for `NO_FRAME_TIMEOUT_MS`.
    frames_stalled: bool,
    no_frame_task: Option<Task<()>>,
    /// The page area in window coordinates, from the last layout.
    bounds: Option<Bounds<Pixels>>,
    /// The page area size last sent as `resize`, in logical px.
    sent_size: Option<(u32, u32)>,
    cap: Option<(u32, u32)>,
    resize_task: Option<Task<()>>,
    cap_task: Option<Task<()>>,
    /// Buttons held over the page (left 1, right 2, middle 4).
    pressed: u8,
    pending_move: Option<StreamInput>,
    pending_wheel: Option<StreamInput>,
    flush_task: Option<Task<()>>,
    pointer: Option<(Pointer, bool)>,
    pointer_task: Option<Task<()>>,
    prompt: Entity<InputState>,
    prompt_for: Option<(String, String)>,
    /// Annotation region/point selection mode over the streamed frame.
    pick_mode: bool,
    pick_drag: Option<((f64, f64), (f64, f64))>,
}

impl EventEmitter<FrameEvent> for FrameView {}

impl Focusable for FrameView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn page_button(button: MouseButton) -> Option<PageButton> {
    match button {
        MouseButton::Left => Some(PageButton::Left),
        MouseButton::Right => Some(PageButton::Right),
        MouseButton::Middle => Some(PageButton::Middle),
        MouseButton::Navigate(_) => None,
    }
}

/// `pressedButtonOf`: the first held button, as DOM `buttons` orders them.
fn held_button(buttons: u8) -> PageButton {
    if buttons & 1 != 0 {
        PageButton::Left
    } else if buttons & 2 != 0 {
        PageButton::Right
    } else if buttons & 4 != 0 {
        PageButton::Middle
    } else {
        PageButton::None
    }
}

fn modifier_bits(held: &gpui::Modifiers) -> u8 {
    modifiers(held.alt, held.control, held.platform, held.shift)
}

impl FrameView {
    pub fn new(state: Entity<AppState>, previews: Entity<Previews>, thread_id: String, tab_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this: &mut FrameView, cx| {
            for image in [this.image.take(), this.retired.take()].into_iter().flatten() {
                cx.drop_image(image, None);
            }
        })
        .detach();
        FrameView {
            state,
            previews,
            thread_id,
            tab_id,
            focus: cx.focus_handle(),
            visible: false,
            follow_size: true,
            live: None,
            image: None,
            latest_jpeg: None,
            retired: None,
            frame_size: (0, 0),
            viewport: None,
            control: None,
            file_chooser: None,
            host_setup: None,
            refused: false,
            gone: false,
            frames_stalled: false,
            no_frame_task: None,
            bounds: None,
            sent_size: None,
            cap: None,
            resize_task: None,
            cap_task: None,
            pressed: 0,
            pending_move: None,
            pending_wheel: None,
            flush_task: None,
            pointer: None,
            pointer_task: None,
            prompt: cx.new(|cx| InputState::new(window, cx)),
            prompt_for: None,
            pick_mode: false,
            pick_drag: None,
        }
    }

    pub fn controlled(&self) -> bool {
        self.control.as_ref().is_some_and(Control::is_yours)
    }

    pub fn has_frame(&self) -> bool {
        self.image.is_some()
    }

    pub fn frames_stalled(&self) -> bool {
        self.frames_stalled
    }

    pub fn panel_size(&self) -> Option<(f64, f64)> {
        let bounds = self.bounds?;
        Some((f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height))))
    }

    pub fn set_follow_size(&mut self, follow_size: bool) {
        if self.follow_size != follow_size {
            self.follow_size = follow_size;
            if follow_size {
                self.sent_size = None;
                self.send_resize();
            }
        }
    }

    pub fn set_pick_mode(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.pick_mode == active {
            return;
        }
        self.pick_mode = active;
        self.pick_drag = None;
        if active {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// Crops the selected annotation region (or full frame) from the latest streamed JPEG into PNG.
    pub fn capture_annotation_png(&self, target: &AnnotationTarget) -> Option<Vec<u8>> {
        let jpeg = self.latest_jpeg.as_ref()?;
        crop_frame_to_png(jpeg, self.viewport, target)
    }

    pub fn capture_full_png(&self) -> Option<Vec<u8>> {
        let jpeg = self.latest_jpeg.as_ref()?;
        let image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?;
        let mut png = Vec::new();
        image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
        Some(png)
    }

    /// A state the page cannot show through (host setup, refused): drawn over the empty state.
    pub fn blocks(&self) -> bool {
        self.host_setup.is_some() || self.refused
    }

    /// Streams only while its tab shows, as the web connects only while visible.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.connect(cx);
        } else {
            self.disconnect(cx);
        }
    }

    /// Sends `input` when this viewer may; whether it went out.
    pub fn send(&mut self, input: StreamInput) -> bool {
        let Some(live) = &self.live else { return false };
        if !may_send(self.control.as_ref(), &input) {
            return false;
        }
        live.input.send(input);
        true
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if !self.visible || self.gone || self.refused || self.host_setup.is_some() || self.live.is_some() {
            return;
        }
        let Some((max_width, max_height)) = self.cap else { return };
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let runtime = state.runtime().clone();
        let target = StreamTarget { thread_id: self.thread_id.clone(), tab_id: self.tab_id.clone(), max_width, max_height };
        let PreviewStream { input, mut frames, mut events } = client.preview_stream(&runtime, target);
        let (decoded_tx, mut decoded) = tokio::sync::watch::channel::<Option<Arc<Decoded>>>(None);
        let decoder = runtime.spawn(async move {
            while frames.changed().await.is_ok() {
                let Some(bytes): Option<Frame> = frames.borrow_and_update().clone() else { continue };
                let Ok(Some(image)) = tokio::task::spawn_blocking(move || decode(bytes)).await else { continue };
                if decoded_tx.send(Some(Arc::new(image))).is_err() {
                    return;
                }
            }
        });
        let frames = cx.spawn(async move |this, cx| {
            while decoded.changed().await.is_ok() {
                let Some(frame) = decoded.borrow_and_update().clone() else { continue };
                if this.update(cx, |this, cx| this.show_frame(&frame, cx)).is_err() {
                    return;
                }
            }
        });
        let events = cx.spawn(async move |this, cx| {
            while let Some(first) = events.recv().await {
                let mut batch = vec![first];
                while let Ok(event) = events.try_recv() {
                    batch.push(event);
                }
                let applied = this.update(cx, |this, cx| {
                    for event in batch {
                        this.on_event(event, cx);
                    }
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        self.live = Some(Live { input, _decoder: Abort(decoder), _frames: frames, _events: events });
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.live = None;
        self.file_chooser = None;
        self.no_frame_task = None;
        let cleared_stall = std::mem::take(&mut self.frames_stalled);
        self.clear_input();
        if self.control.take().is_some() || cleared_stall {
            cx.emit(FrameEvent::Changed);
        }
        cx.notify();
    }

    fn reconnect(&mut self, cx: &mut Context<Self>) {
        self.live = None;
        self.file_chooser = None;
        self.no_frame_task = None;
        self.frames_stalled = false;
        self.connect(cx);
    }

    fn show_frame(&mut self, frame: &Decoded, cx: &mut Context<Self>) {
        let first = self.image.is_none();
        self.no_frame_task = None;
        let cleared_stall = std::mem::take(&mut self.frames_stalled);
        if let Some(stale) = self.retired.take() {
            cx.drop_image(stale, None);
        }
        self.retired = self.image.replace(frame.image.clone());
        self.latest_jpeg = Some(frame.jpeg.clone());
        self.frame_size = frame.size;
        if first || cleared_stall {
            cx.emit(FrameEvent::Changed);
        }
        cx.notify();
    }

    fn on_event(&mut self, event: StreamEvent, cx: &mut Context<Self>) {
        match event {
            StreamEvent::Connected(connected) => {
                self.no_frame_task = None;
                if connected {
                    if self.image.is_none() {
                        self.no_frame_task = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(Duration::from_millis(NO_FRAME_TIMEOUT_MS)).await;
                            let _ = this.update(cx, |this, cx| {
                                this.no_frame_task = None;
                                if this.image.is_none() {
                                    this.frames_stalled = true;
                                    cx.emit(FrameEvent::Changed);
                                    cx.notify();
                                }
                            });
                        }));
                    }
                    return;
                }
                self.frames_stalled = false;
                self.file_chooser = None;
                self.control = None;
                self.clear_input();
                cx.emit(FrameEvent::Changed);
                cx.notify();
            }
            StreamEvent::FileChooser(offer) => {
                if self.file_chooser != offer {
                    self.file_chooser = offer;
                    cx.notify();
                }
            }
            StreamEvent::Message(message) => self.on_message(message, cx),
            StreamEvent::Gone => {
                self.gone = true;
                self.no_frame_task = None;
                self.frames_stalled = false;
                self.file_chooser = None;
                self.live = None;
                cx.notify();
            }
            StreamEvent::HostSetup(setup) => {
                self.host_setup = Some(setup);
                self.no_frame_task = None;
                self.frames_stalled = false;
                self.file_chooser = None;
                self.live = None;
                cx.emit(FrameEvent::Changed);
                cx.notify();
            }
            StreamEvent::Refused => {
                self.refused = true;
                self.no_frame_task = None;
                self.frames_stalled = false;
                self.file_chooser = None;
                self.live = None;
                cx.emit(FrameEvent::Changed);
                cx.notify();
            }
        }
    }

    fn on_message(&mut self, message: StreamMessage, cx: &mut Context<Self>) {
        match message {
            StreamMessage::Viewport(viewport) => {
                self.viewport = Some(viewport);
            }
            StreamMessage::Control(next) => {
                let gained = next.is_yours() && !self.controlled();
                if !next.is_yours() {
                    self.clear_input();
                }
                self.control = Some(next);
                if gained {
                    // A new controller's size wins (`client.send({ type: "resize" })`).
                    self.sent_size = None;
                    self.send_resize();
                }
                cx.emit(FrameEvent::Changed);
                cx.notify();
            }
            StreamMessage::Pointer(pointer) => {
                self.pointer = Some((pointer, true));
                self.pointer_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(AGENT_CURSOR_ACTIVE_MS)).await;
                    let _ = this.update(cx, |this, cx| {
                        if let Some((_, active)) = &mut this.pointer {
                            *active = false;
                            cx.notify();
                        }
                    });
                }));
                cx.notify();
            }
            StreamMessage::Clipboard { text } => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            StreamMessage::Popup { tab_id } => {
                let thread = self.thread_id.clone();
                self.previews.update(cx, |previews, cx| previews.popup(&thread, &tab_id, cx));
            }
            StreamMessage::Download { id, file_name, .. } => {
                self.save_download(id, file_name, cx);
            }
            StreamMessage::FileChooser { .. } | StreamMessage::FileChooserClosed { .. } | StreamMessage::Unknown => {}
        }
    }

    /// Prompts for a local destination and downloads a preview artifact from the server.
    pub fn save_download(&self, download_id: String, file_name: String, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let connection = state.connection_id();
        let Some(client) = state.client_handle() else { return };
        let credentials = client.credentials().clone();
        let runtime = state.runtime().clone();
        let thread_id = self.thread_id.clone();
        let tab_id = self.tab_id.clone();
        let dir = std::env::var_os("HOME")
            .map(|home| std::path::PathBuf::from(home).join("Downloads"))
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let suggested = if file_name.trim().is_empty() { "download" } else { file_name.as_str() };
        let prompt = cx.prompt_for_new_path(&dir, Some(suggested));
        let app_state = self.state.clone();
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(path))) = prompt.await else { return };
            let fetched = runtime.spawn(async move { download_file(&credentials, &thread_id, &tab_id, &download_id).await }).await;
            let result: Result<(), String> = match fetched {
                Ok(Ok(bytes)) => {
                    cx.background_executor()
                        .spawn(async move { std::fs::write(&path, bytes).map_err(|err| err.to_string()) })
                        .await
                }
                Ok(Err(err)) => Err(err),
                Err(err) => Err(err.to_string()),
            };
            cx.update(|cx| {
                app_state.update(cx, |state, cx| {
                    if state.connection_id() != connection {
                        return;
                    }
                    match result {
                        Ok(()) => state.show_toast(Toast::new(ToastKind::Success, format!("Downloaded {file_name}")), cx),
                        Err(err) => state.show_toast(Toast::new(ToastKind::Error, "Could not save download").description(err), cx),
                    }
                });
            });
        })
        .detach();
    }

    fn answer_file_chooser(&mut self, files: Vec<UploadFile>, cx: &mut Context<Self>) {
        let Some(offer) = self.file_chooser.take() else { return };
        cx.notify();
        let state = self.state.read(cx);
        let connection = state.connection_id();
        let Some(client) = state.client_handle() else { return };
        let credentials = client.credentials().clone();
        let runtime = state.runtime().clone();
        let thread_id = self.thread_id.clone();
        let tab_id = self.tab_id.clone();
        let app_state = self.state.clone();
        cx.spawn(async move |_, cx| {
            let result = runtime
                .spawn(async move { upload_file_chooser(&credentials, &thread_id, &tab_id, &offer.id, &files).await })
                .await;
            let err = match result {
                Ok(Ok(())) => return,
                Ok(Err(err)) => err,
                Err(err) => err.to_string(),
            };
            cx.update(|cx| {
                app_state.update(cx, |state, cx| {
                    if state.connection_id() != connection {
                        return;
                    }
                    state.show_toast(Toast::new(ToastKind::Error, "Could not send the files to the page").description(err), cx);
                });
            });
        })
        .detach();
    }

    fn open_file_chooser_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.file_chooser.clone() else { return };
        let connection = self.state.read(cx).connection_id();
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: offer.multiple,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else { return };
            if paths.is_empty() {
                return;
            }
            let files: Vec<UploadFile> = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .filter_map(|path| {
                            let bytes = std::fs::read(&path).ok()?;
                            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_owned();
                            Some(UploadFile { name, mime_type: "application/octet-stream".to_owned(), bytes })
                        })
                        .collect()
                })
                .await;
            if !files.is_empty() {
                let _ = this.update(cx, |this, cx| {
                    if this.state.read(cx).connection_id() == connection
                        && this.file_chooser.as_ref().is_some_and(|current| current.id == offer.id)
                    {
                        this.answer_file_chooser(files, cx);
                    }
                });
            }
        })
        .detach();
    }

    fn clear_input(&mut self) {
        self.pressed = 0;
        self.pending_move = None;
        self.pending_wheel = None;
        self.flush_task = None;
    }

    /// Lays out the page area: a new size goes to the page after `RESIZE_THROTTLE_MS`, and a
    /// larger frame cap reconnects once resizing settles.
    fn measured(&mut self, bounds: Bounds<Pixels>, scale: f32, cx: &mut Context<Self>) {
        let first = self.bounds.is_none();
        let changed = self.bounds.is_none_or(|old| old.size != bounds.size);
        self.bounds = Some(bounds);
        if !changed || f32::from(bounds.size.width) < 1. || f32::from(bounds.size.height) < 1. {
            return;
        }
        let throttle = if first { 0 } else { RESIZE_THROTTLE_MS };
        self.resize_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(throttle)).await;
            let _ = this.update(cx, |this, _| this.send_resize());
        }));
        let size = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
        if let Some(cap) = grow_cap(size, f64::from(scale), self.cap) {
            let delay = if self.cap.is_none() { 0 } else { RESIZE_THROTTLE_MS * 2 };
            self.cap_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(delay)).await;
                let _ = this.update(cx, |this, cx| {
                    let first = this.cap.is_none();
                    this.cap = Some(cap);
                    if first { this.connect(cx) } else if this.live.is_some() { this.reconnect(cx) }
                });
            }));
        }
    }

    fn send_resize(&mut self) {
        if !self.follow_size {
            return;
        }
        let Some(bounds) = self.bounds else { return };
        let size = (f32::from(bounds.size.width).round() as u32, f32::from(bounds.size.height).round() as u32);
        if self.sent_size == Some(size) {
            return;
        }
        if self.send(StreamInput::Resize { width: size.0, height: size.1 }) {
            self.sent_size = Some(size);
        }
    }

    /// A window position as a page point, `None` outside the frame unless `clamp`.
    fn page_at(&self, position: gpui::Point<Pixels>, clamp: bool) -> Option<t3_core::preview::stream::PagePoint> {
        if !self.controlled() {
            return None;
        }
        self.frame_point(position, clamp)
    }

    /// Maps window coordinates into page CSS px regardless of controller ownership (used for
    /// annotation selection even when watching an agent's tab).
    fn frame_point(&self, position: gpui::Point<Pixels>, clamp: bool) -> Option<t3_core::preview::stream::PagePoint> {
        let (bounds, viewport) = (self.bounds?, self.viewport?);
        self.image.as_ref()?;
        let box_size = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
        let at = (f64::from(f32::from(position.x - bounds.origin.x)), f64::from(f32::from(position.y - bounds.origin.y)));
        page_point(box_size, self.frame_size, viewport, at, clamp)
    }

    fn flush(&mut self) {
        self.flush_task = None;
        for input in [self.pending_move.take(), self.pending_wheel.take()].into_iter().flatten() {
            self.send(input);
        }
    }

    fn schedule_flush(&mut self, cx: &mut Context<Self>) {
        if self.flush_task.is_none() {
            self.flush_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(INPUT_FLUSH_MS)).await;
                let _ = this.update(cx, |this, _| this.flush());
            }));
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.pick_mode {
            if event.button == MouseButton::Left
                && let Some(at) = self.frame_point(event.position, false)
            {
                cx.stop_propagation();
                self.pick_drag = Some(((at.x, at.y), (at.x, at.y)));
                cx.notify();
            }
            return;
        }
        if let MouseButton::Navigate(direction) = event.button {
            // Thumb buttons drive this tab's history, never the page.
            self.send(StreamInput::History { delta: if direction == NavigationDirection::Back { -1 } else { 1 } });
            return;
        }
        let Some(button) = page_button(event.button) else { return };
        let Some(at) = self.page_at(event.position, false) else { return };
        cx.stop_propagation();
        self.flush();
        self.pressed |= button.bit();
        let modifiers = modifier_bits(&event.modifiers);
        self.send(StreamInput::Mouse {
            action: MouseAction::Down,
            x: at.x,
            y: at.y,
            button,
            buttons: self.pressed,
            click_count: event.click_count.max(1) as u32,
            modifiers,
        });
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.pick_mode {
            if event.button == MouseButton::Left
                && let Some((start, last)) = self.pick_drag.take()
            {
                let end = self.frame_point(event.position, true).map_or(last, |at| (at.x, at.y));
                let target = self
                    .viewport
                    .and_then(|viewport| AnnotationRegion::from_drag(start, end, viewport))
                    .map_or(AnnotationTarget::Point { x: end.0, y: end.1 }, AnnotationTarget::Region);
                self.pick_mode = false;
                cx.emit(FrameEvent::Picked(target));
                cx.notify();
            }
            return;
        }
        let Some(button) = page_button(event.button) else { return };
        if self.pressed & button.bit() == 0 {
            return;
        }
        self.pressed &= !button.bit();
        let Some(at) = self.page_at(event.position, true) else { return };
        self.flush();
        let _ = cx;
        self.send(StreamInput::Mouse {
            action: MouseAction::Up,
            x: at.x,
            y: at.y,
            button,
            buttons: self.pressed,
            click_count: event.click_count.max(1) as u32,
            modifiers: modifier_bits(&event.modifiers),
        });
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.pick_mode {
            if let Some((start, _)) = self.pick_drag
                && let Some(at) = self.frame_point(event.position, true)
            {
                self.pick_drag = Some((start, (at.x, at.y)));
                cx.notify();
            }
            return;
        }
        let Some(at) = self.page_at(event.position, self.pressed != 0) else { return };
        self.pending_move = Some(StreamInput::Mouse {
            action: MouseAction::Move,
            x: at.x,
            y: at.y,
            button: held_button(self.pressed),
            buttons: self.pressed,
            click_count: 0,
            modifiers: modifier_bits(&event.modifiers),
        });
        self.schedule_flush(cx);
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.pick_mode {
            return;
        }
        let Some(at) = self.page_at(event.position, false) else { return };
        cx.stop_propagation();
        // GPUI reports content motion; DOM deltas are scroll motion, the other way.
        let (dx, dy) = match event.delta {
            ScrollDelta::Pixels(delta) => (-f64::from(f32::from(delta.x)), -f64::from(f32::from(delta.y))),
            ScrollDelta::Lines(lines) => (-f64::from(lines.x) * WHEEL_LINE_PX, -f64::from(lines.y) * WHEEL_LINE_PX),
        };
        let (dx, dy) = (dx * at.scale, dy * at.scale);
        let (before_x, before_y) = match &self.pending_wheel {
            Some(StreamInput::Wheel { delta_x, delta_y, .. }) => (*delta_x, *delta_y),
            _ => (0., 0.),
        };
        self.pending_wheel = Some(StreamInput::Wheel { x: at.x, y: at.y, delta_x: before_x + dx, delta_y: before_y + dy, modifiers: modifier_bits(&event.modifiers) });
        self.schedule_flush(cx);
    }

    fn key(&mut self, action: KeyAction, keystroke: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        if self.pick_mode {
            if action == KeyAction::Down && keystroke.key == "escape" {
                cx.stop_propagation();
                self.pick_mode = false;
                self.pick_drag = None;
                cx.emit(FrameEvent::CancelPick);
                cx.notify();
            }
            return;
        }
        // Only the page's own focus types into it; a dialog's prompt field inside this view keeps its keys.
        if !self.controlled() || !self.focus.is_focused(window) {
            return;
        }
        let modifiers = &keystroke.modifiers;
        if keystroke.key == "escape" && modifiers.shift {
            // Shift+Escape leaves the page, as the web's textarea describes.
            cx.stop_propagation();
            if action == KeyAction::Down {
                window.blur();
            }
            return;
        }
        let shortcut = modifiers.control || modifiers.platform;
        if shortcut && keystroke.key == "v" {
            cx.stop_propagation();
            if action == KeyAction::Down
                && let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
                && !text.is_empty()
            {
                self.send(StreamInput::Text { text });
            }
            return;
        }
        let Some(dom) = dom_key(&keystroke.key, keystroke.key_char.as_deref(), shortcut) else { return };
        cx.stop_propagation();
        self.flush();
        self.send(StreamInput::Key {
            action,
            key: dom.key,
            code: dom.code,
            key_code: dom.key_code,
            text: if action == KeyAction::Down { dom.text } else { None },
            modifiers: modifier_bits(modifiers),
        });
    }

    fn sync_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = self.control.as_ref().and_then(|control| control.dialog.as_ref()).map(|dialog| (dialog.message.clone(), dialog.default_value.clone()));
        if dialog != self.prompt_for {
            if let Some((_, default)) = &dialog {
                let default = default.clone();
                self.prompt.update(cx, |input, cx| input.set_value(default, window, cx));
            }
            self.prompt_for = dialog;
        }
    }

    fn render_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let control = self.control.clone();
        h_flex()
            .flex_none()
            .h(px(CONTROL_BAR_HEIGHT))
            .justify_between()
            .gap(px(8.))
            .px(px(8.))
            .border_b_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.background))
            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(control_label(control.as_ref())))
            .when_some(control.filter(|control| control.can_operate), |this, control| {
                let yours = control.is_yours();
                this.child(
                    BarButton::new("preview-control", BarVariant::Outline, BarSize::Xs)
                        .label(if yours { "Release control" } else { "Take control" })
                        .disabled(control.controller == Controller::AnotherViewer)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(if yours { StreamInput::ReleaseControl } else { StreamInput::TakeControl });
                            cx.notify();
                        })),
                )
            })
    }

    fn render_cursor(&self, cx: &App) -> Option<impl IntoElement> {
        let (pointer, active) = self.pointer.as_ref()?;
        let control = self.control.as_ref()?;
        if control.is_yours() || self.image.is_none() {
            return None;
        }
        let bounds = self.bounds?;
        let box_size = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
        let (left, top) = cursor_position(box_size, self.frame_size, self.viewport?, pointer)?;
        let p = palette(cx);
        // `agentBrowserCursorOpacity`: bright for a moment after each move, dim between.
        let opacity = if *active { 1. } else { 0.35 };
        Some(
            div()
                .absolute()
                .left(px(left as f32 - 2.))
                .top(px(top as f32 - 2.))
                .size(px(20.))
                .opacity(opacity)
                .when(pointer.click && *active, |this| {
                    this.child(div().absolute().left(px(2.)).top(px(2.)).size(px(16.)).rounded_full().bg(hsla(p.primary).opacity(0.25)))
                })
                .child(Icon::AgentPointerFill.svg().absolute().size(px(20.)).text_color(hsla(p.background)))
                .child(Icon::AgentPointer.svg().absolute().size(px(20.)).text_color(hsla(p.primary))),
        )
    }

    fn render_pick_overlay(&self, cx: &App) -> Option<impl IntoElement> {
        if !self.pick_mode {
            return None;
        }
        let p = palette(cx);
        let drag_box = self.pick_drag.and_then(|(start, end)| {
            let bounds = self.bounds?;
            let viewport = self.viewport?;
            let box_size = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
            let fit = fit(box_size, self.frame_size)?;
            if !(viewport.width > 0. && viewport.height > 0.) {
                return None;
            }
            let x0 = start.0.min(end.0).clamp(0., viewport.width);
            let y0 = start.1.min(end.1).clamp(0., viewport.height);
            let x1 = start.0.max(end.0).clamp(0., viewport.width);
            let y1 = start.1.max(end.1).clamp(0., viewport.height);
            let left = fit.left + x0 * fit.width / viewport.width;
            let top = fit.top + y0 * fit.height / viewport.height;
            let width = ((x1 - x0) * fit.width / viewport.width).max(2.);
            let height = ((y1 - y0) * fit.height / viewport.height).max(2.);
            Some(
                div()
                    .absolute()
                    .left(px(left as f32))
                    .top(px(top as f32))
                    .w(px(width as f32))
                    .h(px(height as f32))
                    .rounded(px(4.))
                    .border_2()
                    .border_color(hsla(p.primary))
                    .bg(hsla(p.primary).opacity(0.16)),
            )
        });
        Some(
            div()
                .absolute()
                .inset_0()
                .cursor_crosshair()
                .child(
                    div()
                        .absolute()
                        .top(px(10.))
                        .left(px(10.))
                        .px(px(10.))
                        .py(px(5.))
                        .rounded(px(999.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.popover))
                        .shadow_sm()
                        .text_xs()
                        .text_color(hsla(p.foreground))
                        .child("Click or drag a region to annotate · Esc to cancel"),
                )
                .children(drag_box),
        )
    }

    fn render_file_chooser(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let offer = self.file_chooser.as_ref()?;
        if !self.controlled() {
            return None;
        }
        let p = palette(cx);
        let multiple = offer.multiple;
        Some(
            v_flex()
                .id("preview-file-chooser")
                .absolute()
                .top(px(8.))
                .left(px(8.))
                .right(px(8.))
                .gap(px(8.))
                .p(px(12.))
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(p.border))
                .bg(hsla(p.background))
                .shadow_lg()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .text_sm()
                        .text_color(hsla(p.foreground))
                        .child(if multiple { "The page asks for files." } else { "The page asks for a file." }),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            Button::new("preview-file-chooser-cancel", ButtonVariant::Outline, ButtonSize::Sm)
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| this.answer_file_chooser(Vec::new(), cx))),
                        )
                        .child(
                            Button::new("preview-file-chooser-choose", ButtonVariant::Default, ButtonSize::Sm)
                                .label(if multiple { "Choose files" } else { "Choose file" })
                                .on_click(cx.listener(|this, _, _, cx| this.open_file_chooser_dialog(cx))),
                        ),
                ),
        )
    }

    fn render_dialog(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let control = self.control.as_ref()?;
        let dialog = control.dialog.as_ref()?;
        let p = palette(cx);
        let yours = control.is_yours();
        let prompt = dialog.kind == "prompt";
        let card = v_flex()
            .absolute()
            .top(px(8.))
            .left(px(8.))
            .right(px(8.))
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.background))
            .shadow_lg()
            // Clicks on the card stay off the page, which would take focus from the prompt field.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_sm().text_color(hsla(p.foreground)).child(dialog.message.clone()));
        Some(if yours {
            card.when(prompt, |this| this.child(Input::new(&self.prompt)))
                .child(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(Button::new("preview-dialog-dismiss", ButtonVariant::Outline, ButtonSize::Sm).label("Dismiss").on_click(cx.listener(|this, _, _, _| {
                            this.send(StreamInput::Dialog { accept: false, prompt_text: None });
                        })))
                        .child(Button::new("preview-dialog-accept", ButtonVariant::Default, ButtonSize::Sm).label("Accept").on_click(cx.listener(move |this, _, _, cx| {
                            let prompt_text = prompt.then(|| this.prompt.read(cx).value().to_string());
                            this.send(StreamInput::Dialog { accept: true, prompt_text });
                        }))),
                )
        } else {
            card.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Take control to respond."))
        })
    }

    fn render_stalled_frames(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        if !self.visible || !self.frames_stalled || self.blocks() {
            return None;
        }
        let p = palette(cx);
        Some(
            v_flex()
                .id("preview-no-frames")
                .absolute()
                .inset_0()
                .items_center()
                .justify_center()
                .p(px(16.))
                .bg(hsla(p.background))
                .text_center()
                .child(div().max_w(px(384.)).text_sm().text_color(hsla(p.muted_foreground)).child(NO_FRAME_STALLED_MESSAGE)),
        )
    }

    fn render_blocker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = palette(cx);
        let base = || {
            v_flex()
                .absolute()
                .inset_0()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .p(px(16.))
                .bg(hsla(p.background))
                .text_center()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        if let Some(setup) = &self.host_setup {
            return Some(
                base()
                    .child(div().max_w(px(384.)).text_sm().text_color(hsla(p.muted_foreground)).child(setup.message()))
                    .child(
                        div()
                            .w_full()
                            .max_w(px(448.))
                            .px(px(12.))
                            .py(px(8.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(hsla(p.border))
                            .bg(hsla(p.muted).opacity(0.4))
                            .text_left()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(hsla(p.foreground))
                            .child(setup.command.clone()),
                    )
                    .child(Button::new("preview-host-retry", ButtonVariant::Outline, ButtonSize::Sm).label("Try again").on_click(cx.listener(|this, _, _, cx| {
                        this.host_setup = None;
                        this.connect(cx);
                        cx.emit(FrameEvent::Changed);
                        cx.notify();
                    }))),
            );
        }
        self.refused.then(|| {
            base()
                .gap(px(8.))
                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Browser connection was refused."))
                .child(Button::new("preview-reconnect", ButtonVariant::Outline, ButtonSize::Sm).label("Reconnect").on_click(cx.listener(|this, _, _, cx| {
                    this.refused = false;
                    this.connect(cx);
                    cx.emit(FrameEvent::Changed);
                    cx.notify();
                })))
        })
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn file_chooser_for_test(&self) -> Option<&FileChooserOffer> {
        self.file_chooser.as_ref()
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn inject_stream_event_for_test(&mut self, event: StreamEvent, cx: &mut Context<Self>) {
        self.on_event(event, cx);
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn seed_jpeg_for_test(&mut self, jpeg: Vec<u8>) {
        self.latest_jpeg = Some(jpeg.into());
    }
}

impl Render for FrameView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_prompt(window, cx);
        let view = cx.entity();
        let page = {
            let view = view.clone();
            let paint_view = view.clone();
            canvas(
                move |bounds, window, cx| {
                    let scale = window.scale_factor();
                    view.update(cx, |this, cx| this.measured(bounds, scale, cx));
                },
                move |bounds, (), window, cx| {
                    let this = paint_view.read(cx);
                    if let Some(image) = this.image.clone() {
                        let box_size = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
                        if let Some(fit) = fit(box_size, this.frame_size) {
                            let drawn = Bounds::new(bounds.origin + point(px(fit.left as f32), px(fit.top as f32)), size(px(fit.width as f32), px(fit.height as f32)));
                            let _ = window.paint_image(drawn, Corners::default(), image, 0, false);
                        }
                    }
                    // Drags keep going outside the page; window-wide listeners see them.
                    let moves = paint_view.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            moves.update(cx, |this, cx| this.mouse_move(event, cx));
                        }
                    });
                    let ups = paint_view.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            ups.update(cx, |this, cx| this.mouse_up(event, cx));
                        }
                    });
                },
            )
            .size_full()
        };
        let p = palette(cx);
        v_flex()
            .id("preview-frame")
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.key(KeyAction::Down, &event.keystroke, window, cx)))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| this.key(KeyAction::Up, &event.keystroke, window, cx)))
            .child(self.render_bar(cx))
            .child(
                div()
                    .id("preview-page")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .bg(hsla(p.background))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Navigate(NavigationDirection::Back), cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Navigate(NavigationDirection::Forward), cx.listener(Self::mouse_down))
                    .on_scroll_wheel(cx.listener(Self::scroll))
                    .child(page)
                    .children(self.render_cursor(cx))
                    .children(self.render_pick_overlay(cx))
                    .children(self.render_stalled_frames(cx))
                    .children(self.render_file_chooser(cx))
                    .children(self.render_dialog(cx))
                    .children(self.render_blocker(cx)),
            )
    }
}
