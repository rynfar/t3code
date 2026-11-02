//! One browser tab's stream and the page drawn from it, as `browser/ServerBrowserSurface.tsx`
//! shows it: the control bar (who drives the tab, Take or Release control), the newest frame
//! scaled to fit, pointer, wheel and key input in page pixels while this viewer has control, the
//! agent's cursor (`AgentCursorMark`) while it does not, page dialogs, and the host setup and
//! refused states. Frames decode on the network runtime; only the newest waits, and the view
//! repaints once per decoded frame, never on a timer.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, Corners, DispatchPhase, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection, ParentElement, Pixels,
    Render, RenderImage, ScrollDelta, ScrollWheelEvent, Styled, Task, Window, canvas, div, point, prelude::*, px, size,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::preview_stream::{Frame, PreviewStream, StreamEvent, StreamSender, StreamTarget};
use t3_core::preview::stream::{
    AGENT_CURSOR_ACTIVE_MS, Control, Controller, HostSetup, KeyAction, MouseAction, MouseButton as PageButton, Pointer, RESIZE_THROTTLE_MS, StreamInput,
    StreamMessage, Viewport, WHEEL_LINE_PX, control_label, cursor_position, dom_key, fit, grow_cap, may_send, modifiers, page_point,
};

use super::{Abort, Previews};
use crate::app_state::AppState;
use crate::composer::controls::{Button as BarButton, ButtonSize as BarSize, ButtonVariant as BarVariant};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

/// Pointer moves and wheel deltas coalesce for about one display frame, as the web batches them
/// per animation frame.
const INPUT_FLUSH_MS: u64 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameEvent {
    /// Control, connection or the first frame changed; the chrome row follows.
    Changed,
}

struct Decoded {
    image: Arc<RenderImage>,
    size: (u32, u32),
}

fn decode(bytes: &[u8]) -> Option<Decoded> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?;
    let size = (image.width(), image.height());
    let mut pixels = image.into_rgba8();
    // GPUI's atlas takes BGRA.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Decoded { image: Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])), size })
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
    live: Option<Live>,
    image: Option<Arc<RenderImage>>,
    /// The frame before `image`, freed once the next one replaces it so the atlas never drops a
    /// tile the last drawn scene still uses.
    retired: Option<Arc<RenderImage>>,
    frame_size: (u32, u32),
    viewport: Option<Viewport>,
    control: Option<Control>,
    host_setup: Option<HostSetup>,
    refused: bool,
    gone: bool,
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
            live: None,
            image: None,
            retired: None,
            frame_size: (0, 0),
            viewport: None,
            control: None,
            host_setup: None,
            refused: false,
            gone: false,
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
        }
    }

    pub fn controlled(&self) -> bool {
        self.control.as_ref().is_some_and(Control::is_yours)
    }

    pub fn has_frame(&self) -> bool {
        self.image.is_some()
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
                let Ok(Some(image)) = tokio::task::spawn_blocking(move || decode(&bytes)).await else { continue };
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
        self.clear_input();
        if self.control.take().is_some() {
            cx.emit(FrameEvent::Changed);
        }
        cx.notify();
    }

    fn reconnect(&mut self, cx: &mut Context<Self>) {
        self.live = None;
        self.connect(cx);
    }

    fn show_frame(&mut self, frame: &Decoded, cx: &mut Context<Self>) {
        let first = self.image.is_none();
        if let Some(stale) = self.retired.take() {
            cx.drop_image(stale, None);
        }
        self.retired = self.image.replace(frame.image.clone());
        self.frame_size = frame.size;
        if first {
            cx.emit(FrameEvent::Changed);
        }
        cx.notify();
    }

    fn on_event(&mut self, event: StreamEvent, cx: &mut Context<Self>) {
        match event {
            StreamEvent::Connected(connected) => {
                if !connected {
                    self.control = None;
                    self.clear_input();
                    cx.emit(FrameEvent::Changed);
                    cx.notify();
                }
            }
            StreamEvent::Message(message) => self.on_message(message, cx),
            StreamEvent::Gone => {
                self.gone = true;
                self.live = None;
            }
            StreamEvent::HostSetup(setup) => {
                self.host_setup = Some(setup);
                self.live = None;
                cx.emit(FrameEvent::Changed);
                cx.notify();
            }
            StreamEvent::Refused => {
                self.refused = true;
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
            StreamMessage::Download { .. } | StreamMessage::FileChooser { .. } | StreamMessage::FileChooserClosed { .. } | StreamMessage::Unknown => {}
        }
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
        let (bounds, viewport) = (self.bounds?, self.viewport?);
        if self.image.is_none() || !self.controlled() {
            return None;
        }
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
        if !self.controlled() {
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
            key_code: Some(dom.key_code),
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
            .justify_between()
            .gap(px(8.))
            .px(px(8.))
            .py(px(4.))
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

    fn render_blocker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = palette(cx);
        let base = || v_flex().absolute().inset_0().items_center().justify_center().gap(px(12.)).p(px(16.)).bg(hsla(p.background)).text_center();
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
                    .children(self.render_dialog(cx))
                    .children(self.render_blocker(cx)),
            )
    }
}
