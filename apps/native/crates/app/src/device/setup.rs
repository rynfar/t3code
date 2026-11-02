//! Device setup, as `components/device/DeviceSetup.tsx` walks it inside its wizard dialog: turn
//! the device hub on, check simulator support per platform, then decide whether agents may control
//! devices, and Done marks the environment set up. It shows when the launcher's "Device" entry
//! asks for it (`DeviceSetupDialogHost`), or over a device tab while devices are off or not set up
//! (`DevicePanel`'s dialog); dismissing it there closes that tab.

use gpui::{
    AnyElement, App, BoxShadow, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, Styled,
    Subscription, Window, div, point, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use serde_json::{Value, json};
use t3_core::device::{AGENT_DESCRIPTION, HUB_DESCRIPTION, HostStatus, Platform, PlatformStatus, ProbeGate, SETUP_STEPS, ServiceState, SetupStatus, configure_hub_payload};
use t3_core::right_panel::{Surface, SurfaceKind};

use super::{HostRetry, host_updates};
use super::{Devices, DevicesEvent, devices};
use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, Switch};
use crate::theme::{hsla, palette};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Hub,
    Check,
    Agent,
    Complete,
}

/// Why the wizard shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shown {
    /// The launcher asked for it on this thread.
    Requested(String),
    /// A device tab of this thread is in front while devices are off or not set up.
    Tab { thread_id: String, surface_id: String },
}

pub struct DeviceSetupDialog {
    state: Entity<AppState>,
    devices: Option<Entity<Devices>>,
    shown: Option<Shown>,
    probe: ProbeGate,
    step: usize,
    pending: Option<Pending>,
    retrying: Option<String>,
    focus: FocusHandle,
    focus_pending: bool,
    _subscriptions: Vec<Subscription>,
}

impl HostRetry for DeviceSetupDialog {
    fn set_retrying(&mut self, host: Option<String>) {
        self.retrying = host;
    }
}

impl DeviceSetupDialog {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let devices = devices(cx);
        let mut subscriptions = vec![cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::RightPanel | AppEvent::Selection) {
                this.refresh(cx);
            }
        })];
        if let Some(devices) = &devices {
            subscriptions.push(cx.subscribe(devices, |this, _, _: &DevicesEvent, cx| this.refresh(cx)));
        }
        let mut dialog = DeviceSetupDialog {
            state,
            devices,
            shown: None,
            probe: ProbeGate::default(),
            step: 0,
            pending: None,
            retrying: None,
            focus: cx.focus_handle(),
            focus_pending: false,
            _subscriptions: subscriptions,
        };
        dialog.refresh(cx);
        dialog
    }

    fn compute_shown(&self, cx: &App) -> Option<Shown> {
        let devices = self.devices.as_ref()?.read(cx);
        let state = self.state.read(cx);
        let thread_id = state.selected_thread.clone()?;
        if devices.setup_requested() == Some(thread_id.as_str()) {
            return Some(Shown::Requested(thread_id));
        }
        let surface = state.panels.panels.active_surface(&thread_id)?;
        let needs_setup = devices.service().is_some_and(ServiceState::needs_setup);
        (matches!(surface.kind, SurfaceKind::Device { .. }) && needs_setup).then(|| Shown::Tab { surface_id: surface.id.clone(), thread_id })
    }

    /// Opening starts on the first step. While it shows with the hub on, the hosts are probed
    /// (`deviceHostProbeAtom`), again whenever the hub comes on.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let shown = self.compute_shown(cx);
        if shown != self.shown {
            self.focus_pending = shown.is_some();
            self.shown = shown;
            self.step = 0;
            self.pending = None;
        }
        if let Some(devices) = self.devices.clone() {
            let active = self.shown.is_some() && devices.read(cx).service().is_some_and(ServiceState::hub_enabled);
            if self.probe.update(active) {
                devices.update(cx, |devices, cx| devices.list(json!({}), cx)).detach();
            }
        }
        cx.notify();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        match self.shown.clone() {
            Some(Shown::Requested(_)) => {
                if let Some(devices) = &self.devices {
                    devices.update(cx, |devices, cx| devices.clear_setup_request(cx));
                }
            }
            Some(Shown::Tab { thread_id, surface_id }) => {
                // `onDismissSetup`: the tab closes and the panel stays, on the launcher if it was the last.
                self.state.update(cx, |state, cx| {
                    super::edit_panels(state, &thread_id, cx, |panels, thread| panels.close_surface(thread, &surface_id) | panels.show(thread))
                });
            }
            None => {}
        }
    }

    /// `update`: a configure call while this device may change settings.
    fn configure(&mut self, pending: Pending, input: Value, cx: &mut Context<Self>) {
        let Some(devices) = self.devices.clone() else { return };
        if !devices.read(cx).can_configure() || self.pending.is_some() {
            return;
        }
        self.pending = Some(pending);
        cx.notify();
        let task = devices.update(cx, |devices, cx| devices.configure(input, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.pending = None;
                // The web reports a failed configure only as a console warning; the switch shows the state that stands.
                if let Err(error) = &result {
                    tracing::warn!(%error, "device.configure failed");
                }
                if pending == Pending::Complete
                    && result.is_ok()
                    && let Some(Shown::Requested(thread_id)) = this.shown.clone()
                {
                    // Done from the launcher opens the device list it asked for.
                    if let Some(devices) = &this.devices {
                        devices.update(cx, |devices, cx| devices.clear_setup_request(cx));
                    }
                    this.state.update(cx, |state, cx| super::edit_panels(state, &thread_id, cx, |panels, thread| panels.open(thread, Surface::device(None))));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn check_again(&mut self, cx: &mut Context<Self>) {
        let Some(devices) = self.devices.clone() else { return };
        self.pending = Some(Pending::Check);
        cx.notify();
        let task = devices.update(cx, |devices, cx| devices.list(json!({}), cx));
        cx.spawn(async move |this, cx| {
            let _ = task.await;
            let _ = this.update(cx, |this, cx| {
                this.pending = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn render_steps(&self, busy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let step = self.step;
        let locked = busy || self.pending.is_some();
        h_flex().gap(px(4.)).p(px(4.)).rounded(px(12.)).bg(if dark { gpui::white().opacity(0.04) } else { hsla(p.background) }).children(SETUP_STEPS.iter().enumerate().map(|(index, name)| {
            let current = index == step;
            let done = index < step;
            let badge = div().size(px(20.)).flex_none().flex().items_center().justify_center().rounded_full().text_sm().font_weight(FontWeight::MEDIUM).map(|this| {
                if done {
                    this.bg(hsla(p.primary)).child(Icon::Check.svg().size(px(16.)).text_color(hsla(p.primary_foreground)))
                } else if current {
                    this.bg(hsla(p.primary).opacity(0.1)).text_color(hsla(p.primary)).border_1().border_color(hsla(p.primary).opacity(0.3)).child((index + 1).to_string())
                } else {
                    this.bg(hsla(p.card)).text_color(hsla(p.muted_foreground)).border_1().border_color(hsla(p.border)).child((index + 1).to_string())
                }
            });
            h_flex()
                .id(("device-setup-step", index))
                .flex_1()
                .min_w_0()
                .gap(px(8.))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(8.))
                .when(current, |this| this.bg(hsla(p.card)))
                .when(!locked && index < step, |this| {
                    this.cursor_pointer().hover(|style| style.bg(hsla(p.card))).on_click(cx.listener(move |this, _, _, cx| {
                        this.step = index;
                        cx.notify();
                    }))
                })
                .child(badge)
                .child(div().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(if current { p.foreground } else { p.muted_foreground })).child(*name))
        }))
    }
}

/// `DeviceHubSetupStatus` and `AgentDeviceSetupStatus`: a check or a static spinner and a line.
pub(crate) fn setup_status(status: Option<SetupStatus>, cx: &App) -> Option<AnyElement> {
    let status = status?;
    let p = palette(cx);
    let icon = if status.pending { Icon::LoaderCircle.svg().size(px(12.)).text_color(hsla(p.muted_foreground)) } else { Icon::Check.svg().size(px(12.)).text_color(hsla(p.success)) };
    Some(h_flex().gap(px(8.)).text_xs().text_color(hsla(p.muted_foreground)).child(icon).child(status.text).into_any_element())
}

/// `PlatformStatus`.
fn platform_status(platform: Platform, status: PlatformStatus, cx: &App) -> AnyElement {
    let p = palette(cx);
    let icon = if status.ready { Icon::Check.svg().text_color(hsla(p.success)) } else { Icon::CircleAlert.svg().text_color(hsla(p.muted_foreground)) };
    h_flex()
        .items_start()
        .gap(px(8.))
        .rounded(px(6.))
        .border_1()
        .border_color(hsla(p.border).opacity(0.6))
        .px(px(12.))
        .py(px(8.))
        .child(icon.mt(px(2.)).size(px(16.)).flex_none())
        .child(
            v_flex()
                .min_w_0()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(platform.name()))
                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(status.message)),
        )
        .into_any_element()
}

fn section(title: &'static str, cx: &App) -> gpui::Div {
    let p = palette(cx);
    v_flex().gap(px(12.)).text_sm().child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
}

fn switch_row(description: &'static str, switch: Switch, cx: &App) -> AnyElement {
    let p = palette(cx);
    h_flex().items_start().justify_between().gap(px(16.)).child(div().flex_1().min_w_0().text_color(hsla(p.muted_foreground)).child(description)).child(switch).into_any_element()
}

impl Render for DeviceSetupDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (Some(_), Some(devices)) = (&self.shown, self.devices.clone()) else { return div().into_any_element() };
        if std::mem::take(&mut self.focus_pending) {
            let focus = self.focus.clone();
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let (service, can_configure) = {
            let devices = devices.read(cx);
            (devices.service().cloned(), devices.can_configure())
        };
        let service = service.unwrap_or_else(ServiceState::empty);
        let step = self.step;
        let pending = self.pending;
        let enabled = service.hub_enabled();
        let readiness = service.hub_readiness();
        let locked = readiness.busy || pending.is_some();

        let mut body = v_flex().gap(px(16.));
        if let Some(updates) = host_updates(&service, &devices, self.retrying.as_deref(), cx.entity().downgrade(), cx) {
            body = body.child(updates);
        }
        if step == 0 {
            let toggle = cx.entity().downgrade();
            let switch = Switch::new("device-setup-hub", enabled).disabled(!can_configure || locked).on_change(move |checked, _, cx| {
                let _ = toggle.update(cx, |this, cx| this.configure(Pending::Hub, configure_hub_payload(checked), cx));
            });
            let hub_pending = pending == Some(Pending::Hub) || (readiness.busy && pending != Some(Pending::Agent));
            body = body.child(section("Enable the device hub", cx).child(switch_row(HUB_DESCRIPTION, switch, cx)).children(setup_status(service.hub_setup_status(hub_pending), cx)));
        }
        if step == 1 || (step == 0 && enabled && service.local_platforms_unavailable()) {
            let diagnostics = service.setup_diagnostics();
            let checking = pending == Some(Pending::Check);
            let check = Button::new("device-setup-check", ButtonVariant::Outline, ButtonSize::Compact)
                .label(if checking { "Checking…" } else { "Check again" })
                .disabled(!enabled || locked)
                .on_click(cx.listener(|this, _, _, cx| this.check_again(cx)));
            let check = if checking { check.icon(Icon::LoaderCircle) } else { check };
            body = body.child(
                section("Check simulator support", cx)
                    .child(platform_status(Platform::Ios, service.platform_setup_status(Platform::Ios), cx))
                    .child(platform_status(Platform::Android, service.platform_setup_status(Platform::Android), cx))
                    .when(!diagnostics.is_empty(), |this| {
                        this.child(v_flex().gap(px(4.)).text_xs().text_color(hsla(p.muted_foreground)).children(diagnostics.into_iter().map(|diagnostic| div().child(format!("{}: {}", diagnostic.label, diagnostic.detail)))))
                    })
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("You can use either platform. Fixing a missing platform does not block the other one."))
                    .child(h_flex().child(check)),
            );
        }
        if step == 2 {
            let toggle = cx.entity().downgrade();
            let switch = Switch::new("device-setup-agent", service.agent_access_enabled).disabled(!can_configure || !enabled || locked).on_change(move |checked, _, cx| {
                let _ = toggle.update(cx, |this, cx| this.configure(Pending::Agent, json!({ "agentAccessEnabled": checked }), cx));
            });
            body = body.child(
                section("Allow agent control", cx)
                    .child(switch_row(AGENT_DESCRIPTION, switch, cx))
                    .children(setup_status(service.agent_setup_status(pending == Some(Pending::Agent)), cx))
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Leave this off to keep manual device controls without giving agents access.")),
            );
        }
        if let (HostStatus::Failed, Some(detail)) = (service.host_status, service.host_status_detail.clone()) {
            body = body.child(div().text_xs().text_color(hsla(p.destructive)).child(detail));
        }

        let back = if step == 0 {
            Button::new("device-setup-cancel", ButtonVariant::Outline, ButtonSize::Default).label("Cancel").on_click(cx.listener(|this, _, _, cx| this.dismiss(cx)))
        } else {
            Button::new("device-setup-back", ButtonVariant::Outline, ButtonSize::Default).label("Back").disabled(locked).on_click(cx.listener(|this, _, _, cx| {
                this.step = this.step.saturating_sub(1);
                cx.notify();
            }))
        };
        let primary = if step < SETUP_STEPS.len() - 1 {
            Button::new("device-setup-continue", ButtonVariant::Default, ButtonSize::Default).label("Continue").disabled(!readiness.ready || pending.is_some()).on_click(cx.listener(|this, _, _, cx| {
                this.step += 1;
                cx.notify();
            }))
        } else {
            Button::new("device-setup-done", ButtonVariant::Default, ButtonSize::Default)
                .label(if pending == Some(Pending::Complete) { "Saving…" } else { "Done" })
                .disabled(!can_configure || !readiness.ready || pending.is_some())
                .on_click(cx.listener(|this, _, _, cx| this.configure(Pending::Complete, json!({ "onboardingCompleted": true }), cx)))
        };

        let viewport = window.viewport_size();
        let popup = v_flex()
            .id("device-setup-dialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.dismiss(cx);
                }
            }))
            .occlude()
            .w_full()
            .max_w(px(576.))
            .max_h(px(f32::from(viewport.height) - 32.))
            .relative()
            .rounded(px(16.))
            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .shadow(vec![BoxShadow { color: gpui::black().opacity(0.65), offset: point(px(0.), px(24.)), blur_radius: px(64.), spread_radius: px(-24.), inset: false }])
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .p(px(24.))
                    .gap(px(8.))
                    .child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child("Set up devices"))
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child("Review what runs on this environment before using simulators and emulators."))
                    .child(div().pt(px(8.)).child(self.render_steps(readiness.busy, cx))),
            )
            .child(
                div()
                    .id("device-setup-panel")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(24.))
                    .py(px(20.))
                    .bg(if dark { gpui::white().opacity(0.02) } else { hsla(p.background).opacity(0.8) })
                    .border_t_1()
                    .border_b_1()
                    .border_color(if dark { gpui::white().opacity(0.05) } else { gpui::black().opacity(0.05) })
                    .child(body),
            )
            .child(h_flex().justify_end().gap(px(8.)).px(px(24.)).py(px(16.)).child(back).child(primary));

        let overlay = div()
            .id("device-setup-backdrop")
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.))
            .bg(hsla(p.dialog_backdrop))
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| this.dismiss(cx)))
            .child(popup);
        gpui::deferred(gpui::anchored().position_mode(gpui::AnchoredPositionMode::Window).position(point(px(0.), px(0.))).child(overlay))
            .with_priority(3)
            .into_any_element()
    }
}

