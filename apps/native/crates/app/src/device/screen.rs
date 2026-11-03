//! An open device's live screen, as `DeviceWorkspace.tsx` and `DeviceStreamView.tsx` show it in
//! the flat view: the newest frame fitted to the screen's aspect beside the controls rail
//! (`DeviceControlsRail.tsx`: Home, Rotate, and More with Close device panel and Power off), a
//! touch per left-button drag, HID keys while the screen has focus, the loading view until video
//! arrives (Reconnect after a failure), and a bar while input reconnects. Frames decode and turn
//! upright on the network runtime; only the newest waits, and the view repaints once per decoded
//! frame, never on a timer. It streams only while its tab shows.

use std::sync::Arc;

use gpui::{
    App, Bounds, Context, Corners, DispatchPhase, Entity, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, RenderImage, RenderOnce, SharedString, Styled, Task,
    Window, canvas, div, point, prelude::*, px, size,
};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{Selectable, v_flex};
use t3_client::device_stream::{DeviceInput, DeviceStream, DeviceStreamEvent, DeviceTarget, Frame, InputSender};
use t3_core::device::stream::{
    CONNECTING_VIDEO, HardwareButton, KeyPhase, Screen, StreamStatus, TouchPhase, fit_frame, input_disconnected_label, screen_aspect, screen_rotation,
    transport,
};
use t3_core::device::{Device, HostKind, Platform};
use t3_core::preview::stream::{dom_key, fit};

use super::pane::{Stage, loading_view};
use super::{Abort, Devices};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::settings::controls::{Button as ActionButton, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

/// `CONTROLS_RAIL_WIDTH`: the screen keeps clear of the rail.
const RAIL_WIDTH: f32 = 56.;

// DropdownMenu forwards its open state through Selectable before rendering the trigger.
#[derive(IntoElement)]
struct MoreButton(Button);

impl Selectable for MoreButton {
    fn selected(self, selected: bool) -> Self {
        Self(self.0.selected(selected))
    }

    fn is_selected(&self) -> bool {
        self.0.is_selected()
    }
}

impl Styled for MoreButton {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        self.0.style()
    }
}

impl InteractiveElement for MoreButton {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.0.interactivity()
    }
}

impl DropdownMenu for MoreButton {}

impl RenderOnce for MoreButton {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let open = self.is_selected();
        self.0.when(!open, |button| button.tooltip("More device actions"))
    }
}

struct Decoded {
    image: Arc<RenderImage>,
    size: (u32, u32),
}

/// Decodes a JPEG and turns serve-sim's raw portrait framebuffer upright (`screenRotation`).
fn decode(bytes: &[u8], rotation: i32) -> Option<Decoded> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?;
    let image = match rotation {
        90 => image.rotate90(),
        -90 => image.rotate270(),
        180 => image.rotate180(),
        _ => image,
    };
    let size = (image.width(), image.height());
    let mut pixels = image.into_rgba8();
    // GPUI's atlas takes BGRA.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Decoded { image: Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])), size })
}

/// The open stream; dropping it stops video, input and decoding.
struct Live {
    input: InputSender,
    rotation: tokio::sync::watch::Sender<i32>,
    _decoder: Abort,
    _frames: Task<()>,
    _events: Task<()>,
}

pub struct DeviceScreen {
    state: Entity<AppState>,
    devices: Entity<Devices>,
    thread_id: String,
    surface_id: String,
    device: Device,
    visible: bool,
    live: Option<Live>,
    status: StreamStatus,
    input_connected: bool,
    input_detail: Option<String>,
    screen: Option<Screen>,
    image: Option<Arc<RenderImage>>,
    /// The frame before `image`, freed once the next one replaces it so the atlas never drops a
    /// tile the last drawn scene still uses.
    retired: Option<Arc<RenderImage>>,
    frame_size: (u32, u32),
    /// The screen's box in window coordinates, from the last layout.
    frame: Option<Bounds<Pixels>>,
    /// A left-button drag is touching the screen.
    touching: bool,
    /// Modifier keys sent down, as the web sends each modifier key's own events.
    held: Modifiers,
    focus: FocusHandle,
}

impl DeviceScreen {
    pub fn new(state: Entity<AppState>, devices: Entity<Devices>, thread_id: String, surface_id: String, device: Device, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this: &mut DeviceScreen, cx| {
            for image in [this.image.take(), this.retired.take()].into_iter().flatten() {
                cx.drop_image(image, None);
            }
        })
        .detach();
        DeviceScreen {
            state,
            devices,
            thread_id,
            surface_id,
            device,
            visible: false,
            live: None,
            status: StreamStatus::Connecting,
            input_connected: false,
            input_detail: None,
            screen: None,
            image: None,
            retired: None,
            frame_size: (0, 0),
            frame: None,
            touching: false,
            held: Modifiers::default(),
            focus: cx.focus_handle(),
        }
    }

    pub fn shows(&self, device: &Device) -> bool {
        self.device.key() == device.key()
    }

    fn host_kind(&self, cx: &App) -> Option<HostKind> {
        self.devices.read(cx).service()?.host(&self.device.host_id).map(|host| host.kind)
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible { self.connect(cx) } else { self.disconnect(cx) }
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if !self.visible || self.live.is_some() {
            return;
        }
        self.status = StreamStatus::Connecting;
        self.input_connected = false;
        self.input_detail = None;
        if let Err(detail) = transport(self.device.platform, self.host_kind(cx)) {
            self.status = StreamStatus::Error(detail.to_owned());
            cx.notify();
            return;
        }
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let runtime = state.runtime().clone();
        let target = DeviceTarget { host_id: self.device.host_id.clone(), device_id: self.device.id.clone() };
        let DeviceStream { input, mut frames, mut events } = client.device_stream(&runtime, target);
        let (rotation, mut rotations) = tokio::sync::watch::channel(screen_rotation(self.device.platform, self.screen.as_ref()));
        let (decoded_tx, mut decoded) = tokio::sync::watch::channel::<Option<Arc<Decoded>>>(None);
        let decoder = runtime.spawn(async move {
            loop {
                tokio::select! {
                    changed = frames.changed() => if changed.is_err() { return },
                    changed = rotations.changed() => if changed.is_err() { return },
                }
                let Some(bytes): Option<Frame> = frames.borrow_and_update().clone() else { continue };
                let rotation = *rotations.borrow_and_update();
                let Ok(Some(image)) = tokio::task::spawn_blocking(move || decode(&bytes, rotation)).await else { continue };
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
                        this.on_event(event);
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        self.live = Some(Live { input, rotation, _decoder: Abort(decoder), _frames: frames, _events: events });
        cx.notify();
    }

    /// A hidden tab streams nothing and keeps no frame.
    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.live = None;
        self.touching = false;
        self.held = Modifiers::default();
        for image in [self.image.take(), self.retired.take()].into_iter().flatten() {
            cx.drop_image(image, None);
        }
        cx.notify();
    }

    fn reconnect(&mut self, cx: &mut Context<Self>) {
        self.live = None;
        self.connect(cx);
    }

    fn show_frame(&mut self, frame: &Decoded, cx: &mut Context<Self>) {
        if let Some(stale) = self.retired.take() {
            cx.drop_image(stale, None);
        }
        self.retired = self.image.replace(frame.image.clone());
        self.frame_size = frame.size;
        cx.notify();
    }

    fn on_event(&mut self, event: DeviceStreamEvent) {
        match event {
            DeviceStreamEvent::Status(status) => self.status = status,
            DeviceStreamEvent::Screen(screen) => {
                self.screen = Some(screen);
                if let Some(live) = &self.live {
                    live.rotation.send_if_modified(|rotation| {
                        let next = screen_rotation(self.device.platform, Some(&screen));
                        std::mem::replace(rotation, next) != next
                    });
                }
            }
            DeviceStreamEvent::Input { connected, detail } => {
                self.input_connected = connected;
                self.input_detail = detail;
                if !connected {
                    self.touching = false;
                }
            }
        }
    }

    fn send(&self, input: DeviceInput) {
        if let Some(live) = &self.live {
            live.input.send(input);
        }
    }

    /// `normalizedPoint`: a window position as 0..1 in the screen's box, clamped.
    fn normalized(&self, position: gpui::Point<Pixels>) -> Option<(f64, f64)> {
        let frame = self.frame?;
        let (width, height) = (f64::from(f32::from(frame.size.width)), f64::from(f32::from(frame.size.height)));
        if width <= 0. || height <= 0. {
            return None;
        }
        let x = f64::from(f32::from(position.x - frame.origin.x)) / width;
        let y = f64::from(f32::from(position.y - frame.origin.y)) / height;
        Some((x.clamp(0., 1.), y.clamp(0., 1.)))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if !self.input_connected || self.touching || !self.frame.is_some_and(|frame| frame.contains(&event.position)) {
            return;
        }
        let Some((x, y)) = self.normalized(event.position) else { return };
        cx.stop_propagation();
        self.touching = true;
        self.send(DeviceInput::Touch { phase: TouchPhase::Begin, x, y });
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent) {
        if !self.touching {
            return;
        }
        if let Some((x, y)) = self.normalized(event.position) {
            self.send(DeviceInput::Touch { phase: TouchPhase::Move, x, y });
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent) {
        if !self.touching || event.button != MouseButton::Left {
            return;
        }
        self.touching = false;
        if let Some((x, y)) = self.normalized(event.position) {
            self.send(DeviceInput::Touch { phase: TouchPhase::End, x, y });
        }
    }

    /// `sendKey`: the key's DOM code as HID. Command shortcuts stay the app's, except Command-R.
    fn key(&mut self, phase: KeyPhase, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) || (keystroke.modifiers.platform && keystroke.key != "r") {
            return;
        }
        let shortcut = keystroke.modifiers.control || keystroke.modifiers.platform;
        let Some(dom) = dom_key(&keystroke.key, keystroke.key_char.as_deref(), shortcut) else { return };
        if dom.code.is_empty() {
            return;
        }
        cx.stop_propagation();
        self.send(DeviceInput::Key { phase, code: dom.code });
    }

    /// The browser sends Shift, Control and Option as keys of their own; GPUI reports them as
    /// modifier changes.
    fn modifiers_changed(&mut self, event: &ModifiersChangedEvent, window: &mut Window) {
        if !self.focus.is_focused(window) {
            return;
        }
        let next = event.modifiers;
        for (before, now, code) in [(self.held.shift, next.shift, "ShiftLeft"), (self.held.control, next.control, "ControlLeft"), (self.held.alt, next.alt, "AltLeft")] {
            if before != now {
                self.send(DeviceInput::Key { phase: if now { KeyPhase::Down } else { KeyPhase::Up }, code: code.to_owned() });
            }
        }
        self.held = next;
    }

    fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let disabled = !self.input_connected;
        let ios = self.device.platform == Platform::Ios;
        let simlock = self.host_kind(cx) == Some(HostKind::Simlock);
        let (devices, thread, surface) = (self.devices.clone(), self.thread_id.clone(), self.surface_id.clone());
        let session = (self.device.host_id.clone(), self.device.id.clone());
        let more = MoreButton(Button::new("device-more").variant(Variant::Ghost).size(Size::IconSm).icon(Icon::Ellipsis)).dropdown_menu(move |menu, _, _| {
            let (close_devices, close_thread, close_surface) = (devices.clone(), thread.clone(), surface.clone());
            let (power_devices, power_thread, power_surface, power_session) = (devices.clone(), thread.clone(), surface.clone(), session.clone());
            menu.item(PopupMenuItem::new("Close device panel").on_click(move |_, _, cx| {
                close_devices.update(cx, |devices, cx| devices.close_device(&close_thread, &close_surface, None, false, cx));
            }))
            .separator()
            .item(PopupMenuItem::new(if simlock { "Return device to the pool" } else { "Power off device" }).on_click(move |_, _, cx| {
                let session = Some(power_session.clone());
                power_devices.update(cx, |devices, cx| devices.close_device(&power_thread, &power_surface, session, true, cx));
            }))
        });
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(RAIL_WIDTH))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .py(px(12.))
            .pr(px(8.))
            .child(
                v_flex()
                    .items_center()
                    .gap(px(4.))
                    .p(px(8.))
                    .rounded_full()
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.5))
                    .bg(hsla(p.background).opacity(0.8))
                    .shadow_sm()
                    .child(
                        Button::new("device-home")
                            .variant(Variant::Ghost)
                            .size(Size::IconSm)
                            .icon(Icon::House)
                            .tooltip("Home")
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, _, _| this.send(DeviceInput::Button(HardwareButton::Home)))),
                    )
                    .when(ios && !simlock, |this| {
                        this.child(
                            Button::new("device-rotate")
                                .variant(Variant::Ghost)
                                .size(Size::IconSm)
                                .icon(Icon::RotateCcw)
                                .tooltip("Rotate device")
                                .disabled(disabled)
                                .on_click(cx.listener(|this, _, _, _| this.send(DeviceInput::Rotate))),
                        )
                    })
                    .child(more),
            )
    }
}

impl Render for DeviceScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let view = cx.entity();
        let platform = self.device.platform;
        let aspect = screen_aspect(platform, self.screen.as_ref());
        let page = {
            let (view, paint_view) = (view.clone(), view.clone());
            canvas(
                move |bounds, _, cx| {
                    // The screen's box: the screen's aspect, centered left of the rail.
                    let (width, height) = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
                    let (frame_width, frame_height) = fit_frame(aspect, width, height, f64::from(RAIL_WIDTH));
                    let left = (width - f64::from(RAIL_WIDTH) - frame_width).max(0.) / 2.;
                    let top = (height - frame_height) / 2.;
                    let frame = Bounds::new(bounds.origin + point(px(left as f32), px(top as f32)), size(px(frame_width as f32), px(frame_height as f32)));
                    view.update(cx, |this, _| this.frame = Some(frame));
                },
                move |_, (), window, cx| {
                    let this = paint_view.read(cx);
                    if let (Some(image), Some(frame)) = (this.image.clone(), this.frame) {
                        let box_size = (f64::from(f32::from(frame.size.width)), f64::from(f32::from(frame.size.height)));
                        if let Some(fit) = fit(box_size, this.frame_size) {
                            let drawn = Bounds::new(frame.origin + point(px(fit.left as f32), px(fit.top as f32)), size(px(fit.width as f32), px(fit.height as f32)));
                            let _ = window.paint_image(drawn, Corners::default(), image, 0, false);
                        }
                    }
                    // A drag keeps touching outside the screen; window-wide listeners see it.
                    let moves = paint_view.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            moves.update(cx, |this, _| this.mouse_move(event));
                        }
                    });
                    let ups = paint_view.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            ups.update(cx, |this, _| this.mouse_up(event));
                        }
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        let streaming = self.status == StreamStatus::Streaming;
        let loading = (!streaming).then(|| {
            let (message, error): (SharedString, bool) = match &self.status {
                StreamStatus::Error(detail) => (detail.clone().into(), true),
                _ => (CONNECTING_VIDEO.into(), false),
            };
            let host = self.devices.read(cx).service().and_then(|service| service.host(&self.device.host_id)).map_or_else(|| "Device host".to_owned(), |host| host.label.clone());
            let reconnect = error.then(|| {
                ActionButton::new("device-reconnect", ButtonVariant::Outline, ButtonSize::Sm)
                    .label("Reconnect")
                    .on_click(cx.listener(|this, _, _, cx| this.reconnect(cx)))
                    .into_any_element()
            });
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(loading_view(self.device.name.clone().into(), format!("{host} · {}", self.device.version).into(), Stage::Stream, message, error, reconnect, cx))
        });
        let input_bar = (streaming && !self.input_connected).then(|| {
            div().absolute().left_0().right_0().bottom_0().flex().justify_center().p(px(8.)).child(
                div()
                    .rounded(px(6.))
                    .bg(hsla(p.background).opacity(0.85))
                    .px(px(8.))
                    .py(px(4.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(input_disconnected_label(self.input_detail.as_deref())),
            )
        });
        div()
            .id("device-screen")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(hsla(p.background))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.key(KeyPhase::Down, &event.keystroke, window, cx)))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| this.key(KeyPhase::Up, &event.keystroke, window, cx)))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, window, _| this.modifiers_changed(event, window)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .child(page)
            .children(input_bar)
            .children(loading)
            .child(self.render_rail(cx))
    }
}
