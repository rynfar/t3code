//! Floating device window ("Float device over chat"): a dedicated GPUI secondary floating
//! window (`WindowKind::Floating`) hosting a live `DeviceScreen` with a compact header bar offering
//! Dock-back and Close controls. Guarded off from spawning OS windows during unit tests and
//! offscreen snapshot runs.

use gpui::{
    App, AppContext, Context, Entity, FontWeight, IntoElement, MouseButton, ParentElement, Render, Styled, Window, div, prelude::*, px,
};
#[cfg(not(any(test, feature = "snapshot")))]
use gpui::{Bounds, TitlebarOptions, WindowBounds, WindowKind, WindowOptions, point, size};
use gpui_component::{h_flex, v_flex};
use t3_core::device::Device;

use super::DeviceScreen;
use crate::app_state::AppState;
use crate::device::Devices;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

/// Compact header height for the floating device window.
const HEADER_HEIGHT: f32 = 34.;

/// Root view of a floating device window: compact draggable header (Dock back + Close) above a
/// live `DeviceScreen`.
pub(crate) struct FloatingDeviceWindow {
    devices: Entity<Devices>,
    thread_id: String,
    device: Device,
    screen: Entity<DeviceScreen>,
}

impl FloatingDeviceWindow {
    pub(crate) fn new(
        state: Entity<AppState>,
        devices: Entity<Devices>,
        thread_id: String,
        surface_id: String,
        device: Device,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = (device.host_id.clone(), device.id.clone());
        let unregister = devices.downgrade();
        cx.on_release(move |_, cx| {
            let _ = unregister.update(cx, |devices, cx| devices.clear_floating_key(&key, cx));
        })
        .detach();
        let screen = cx.new(|cx| {
            let mut screen = DeviceScreen::new_floating(state, devices.clone(), thread_id.clone(), surface_id, device.clone(), cx);
            screen.set_visible(true, cx);
            screen
        });
        Self { devices, thread_id, device, screen }
    }

    pub(crate) fn dock_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.thread_id.clone();
        let device = self.device.clone();
        self.devices.update(cx, |devices, cx| devices.dock_device(&thread, &device, cx));
        window.remove_window();
    }
}

impl Render for FloatingDeviceWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let left_pad = if cfg!(target_os = "macos") { px(72.) } else { px(10.) };
        let header = h_flex()
            .id("device-float-header")
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .pl(left_pad)
            .pr(px(8.))
            .border_b_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.background))
            .on_mouse_down(MouseButton::Left, |_, window, _| {
                window.start_window_move();
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(6.))
                    .child(Icon::Smartphone.svg().size(px(13.)).text_color(hsla(p.muted_foreground)))
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child(self.device.name.clone()),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(px(2.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("device-float-dock")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::PanelRight)
                            .tooltip("Dock back to panel")
                            .on_click(cx.listener(|this, _, window, cx| this.dock_back(window, cx))),
                    )
                    .child(
                        Button::new("device-float-close")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::X)
                            .tooltip("Close floating window")
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            );
        v_flex()
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .child(header)
            .child(div().flex_1().min_h_0().w_full().child(self.screen.clone()))
    }
}

/// Opens a secondary floating GPUI window (`WindowKind::Floating`) for `device`.
/// Guarded off during unit tests and offscreen snapshot builds so tests never spawn OS windows or
/// steal focus.
pub(crate) fn open_floating_window(
    state: Entity<AppState>,
    devices: Entity<Devices>,
    thread_id: String,
    surface_id: String,
    device: Device,
    cx: &mut App,
) -> Option<gpui::AnyWindowHandle> {
    #[cfg(any(test, feature = "snapshot"))]
    {
        let _ = (state, devices, thread_id, surface_id, device, cx);
        None
    }
    #[cfg(not(any(test, feature = "snapshot")))]
    {
        let bounds = Bounds::centered(None, size(px(360.), px(680.)), cx);
        let title = device.name.clone();
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some(title.into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(10.), px(10.))),
                    }),
                    window_min_size: Some(size(px(280.), px(420.))),
                    kind: WindowKind::Floating,
                    is_movable: true,
                    is_resizable: true,
                    ..Default::default()
                },
                |window, cx| {
                    crate::theme::follow_system_appearance(window, cx);
                    let view = cx.new(|cx| FloatingDeviceWindow::new(state, devices, thread_id, surface_id, device, cx));
                    cx.new(|cx| gpui_component::Root::new(view, window, cx))
                },
            )
            .ok()?;
        Some(handle.into())
    }
}
