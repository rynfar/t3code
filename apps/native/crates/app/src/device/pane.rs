//! A device tab, as `components/device/DevicePanel.tsx` shows it: discovery warnings, host
//! updates (`DeviceHostUpdates.tsx`), devices starting for the thread and the last open or
//! power-off failure along the top; below, the open device's screen (`DeviceScreen`), the loading
//! view (`DeviceLoadingView.tsx`) while devices load or one opens, or the device list grouped by
//! platform with Open or Start on each row. While devices are off or not set up, the tab is empty
//! and `DeviceSetupDialog` shows over it.

use std::collections::HashSet;

use gpui::{AnyElement, App, AppContext, Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, StyleRefinement, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_core::device::{ANDROID_HINT, Device, HOST_FAILED_HINT, ProbeGate, ServiceState, retry_host_payload};
use t3_core::right_panel::{DeviceTarget, SurfaceKind};

use super::screen::DeviceScreen;
use super::{Devices, DevicesEvent};
use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::settings::controls::{Button as ActionButton, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

pub struct DevicePane {
    state: Entity<AppState>,
    devices: Entity<Devices>,
    thread_id: String,
    surface_id: String,
    visible: bool,
    probe: ProbeGate,
    screen: Option<Entity<DeviceScreen>>,
    /// Discovery warnings dismissed while this tab lives, by host and detail.
    dismissed: HashSet<(String, String)>,
    /// The host whose Retry runs.
    retrying: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl DevicePane {
    pub fn new(state: Entity<AppState>, devices: Entity<Devices>, thread_id: String, surface_id: String, cx: &mut Context<Self>) -> Self {
        let device_events = cx.subscribe(&devices, |this, _, _: &DevicesEvent, cx| {
            this.sync_probe(cx);
            this.sync_screen(cx);
            cx.notify();
        });
        let panel_events = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if let AppEvent::RightPanel = event {
                this.sync_screen(cx);
            }
        });
        let mut pane = DevicePane {
            state,
            devices,
            thread_id,
            surface_id,
            visible: false,
            probe: ProbeGate::default(),
            screen: None,
            dismissed: HashSet::new(),
            retrying: None,
            _subscriptions: vec![device_events, panel_events],
        };
        pane.sync_screen(cx);
        pane
    }

    pub fn surface_id(&self) -> &str {
        &self.surface_id
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.sync_probe(cx);
        if let Some(screen) = &self.screen {
            screen.update(cx, |screen, cx| screen.set_visible(visible, cx));
        }
    }

    /// `deviceHostProbeAtom`, mounted while the tab shows a loaded environment with the hub on.
    fn sync_probe(&mut self, cx: &mut Context<Self>) {
        let active = self.visible && self.devices.read(cx).service().is_some_and(ServiceState::hub_enabled);
        if self.probe.update(active) {
            self.devices.update(cx, |devices, cx| devices.list(json!({}), cx)).detach();
        }
    }

    fn target(&self, cx: &App) -> Option<DeviceTarget> {
        let panel = self.state.read(cx).panels.panels.panel(&self.thread_id);
        panel.surfaces.iter().find(|surface| surface.id == self.surface_id).and_then(|surface| match &surface.kind {
            SurfaceKind::Device { target, .. } => target.clone(),
            _ => None,
        })
    }

    /// The tab's device while the thread's session on it lasts.
    fn active(&self, cx: &App) -> Option<Device> {
        let target = self.target(cx)?;
        let service = self.devices.read(cx).service()?;
        let session = service.thread_session(&self.thread_id, &target.host_id, &target.device_id)?;
        service.device(&session.host_id, &session.device_id).cloned()
    }

    /// A screen streams while its device stays open; a different device gets a fresh one.
    fn sync_screen(&mut self, cx: &mut Context<Self>) {
        let active = self.active(cx);
        let same = |screen: &Entity<DeviceScreen>| active.as_ref().is_some_and(|device| screen.read(cx).shows(device));
        if self.screen.as_ref().is_some_and(|screen| !same(screen)) {
            self.screen = None;
        }
        if let (None, Some(device)) = (&self.screen, active) {
            let (state, devices, thread, surface, visible) = (self.state.clone(), self.devices.clone(), self.thread_id.clone(), self.surface_id.clone(), self.visible);
            self.screen = Some(cx.new(|cx| {
                let mut screen = DeviceScreen::new(state, devices, thread, surface, device, cx);
                screen.set_visible(visible, cx);
                screen
            }));
        }
    }

    fn render_list(&self, service: &ServiceState, pending: Option<&Device>, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let grouped = service.grouped();
        let ready = service.hub_readiness().ready;
        let empty = grouped.is_empty();
        let groups: Vec<AnyElement> = if ready {
            grouped
                .iter()
                .map(|(platform, devices)| {
                    let rows: Vec<AnyElement> = devices
                        .iter()
                        .enumerate()
                        .map(|(index, device)| {
                            let device: &Device = device;
                            let opening = pending.is_some_and(|pending| pending.key() == device.key());
                            let label = if device.booted { "Open" } else { "Start" };
                            let open = (self.devices.clone(), self.thread_id.clone(), device.clone());
                            h_flex()
                                .gap(px(12.))
                                .px(px(12.))
                                .py(px(12.))
                                .when(index > 0, |this| this.border_t_1().border_color(hsla(p.border).opacity(0.6)))
                                .child(
                                    div()
                                        .flex()
                                        .flex_none()
                                        .items_center()
                                        .justify_center()
                                        .size(px(32.))
                                        .rounded(px(6.))
                                        .border_1()
                                        .border_color(hsla(p.border).opacity(0.6))
                                        .child(Icon::Smartphone.svg().size(px(16.)).text_color(hsla(p.muted_foreground))),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(device.name.clone()))
                                        .child(div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(service.device_detail(device))),
                                )
                                .child(
                                    ActionButton::new(SharedString::from(format!("device-open-{}-{}", device.host_id, device.id)), ButtonVariant::Outline, ButtonSize::Sm)
                                        .icon(if opening {
                                            Icon::LoaderCircle
                                        } else if device.booted {
                                            Icon::ArrowUpRight
                                        } else {
                                            Icon::Play
                                        })
                                        .label(label)
                                        .tooltip(format!("{label} {}", device.name))
                                        .disabled(pending.is_some())
                                        .on_click(move |_, _, cx| {
                                            let (devices, thread, device) = open.clone();
                                            devices.update(cx, |devices, cx| devices.open_device(&thread, device, cx));
                                        }),
                                )
                                .into_any_element()
                        })
                        .collect();
                    v_flex()
                        .gap(px(12.))
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .text_sm()
                                .text_color(hsla(p.muted_foreground))
                                .child(Icon::Smartphone.svg().size(px(16.)).flex_none().text_color(hsla(p.muted_foreground)))
                                .child(div().font_weight(FontWeight::MEDIUM).child(platform.group_label())),
                        )
                        .child(v_flex().overflow_hidden().rounded(px(12.)).border_1().border_color(hsla(p.border).opacity(0.7)).bg(hsla(p.background)).children(rows))
                        .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };
        let devices = self.devices.clone();
        let refresh = (!service.hub_readiness().busy).then(|| {
            ActionButton::new("device-refresh", ButtonVariant::Outline, ButtonSize::Sm).label("Refresh devices").on_click(move |_, _, cx| {
                devices.update(cx, |devices, cx| devices.list(json!({}), cx)).detach();
            })
        });
        div()
            .id("device-list")
            .size_full()
            .overflow_y_scroll()
            .px(px(20.))
            .py(px(32.))
            .text_sm()
            .text_color(hsla(p.muted_foreground))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(576.))
                    .mx_auto()
                    .gap(px(24.))
                    .when(empty, |this| this.min_h_full().justify_center().items_center().text_center())
                    .when(empty, |this| {
                        this.child(Icon::Smartphone.svg().size(px(24.)).text_color(hsla(p.muted_foreground)).opacity(0.6))
                            .child(div().w_full().max_w(px(384.)).max_h(px(128.)).child(service.empty_message()))
                    })
                    .when(!groups.is_empty(), |this| this.child(v_flex().w_full().gap(px(24.)).children(groups)))
                    .when(service.show_android_hint(), |this| this.child(div().max_w(px(384.)).text_xs().child(ANDROID_HINT)))
                    .when_some(refresh, |this, refresh| this.child(h_flex().when(empty, |this| this.justify_center()).child(refresh))),
            )
            .into_any_element()
    }

    fn render_notices(&self, service: &ServiceState, active: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = palette(cx);
        let mut notices = Vec::new();
        if !active {
            for diagnostic in service.panel_diagnostics() {
                let key = (diagnostic.host_id.clone(), diagnostic.detail.clone());
                if self.dismissed.contains(&key) {
                    continue;
                }
                let body = v_flex()
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("Device discovery warning · {}", diagnostic.label)))
                    .child(diagnostic.detail.clone())
                    .into_any_element();
                let dismiss = cx.listener(move |this: &mut Self, _, _, cx| {
                    this.dismissed.insert(key.clone());
                    cx.notify();
                });
                notices.push(notice(Tone::Warning, &format!("device-warning-{}", diagnostic.host_id), body, dismiss, cx));
            }
        }
        if let Some(updates) = host_updates(service, &self.devices, self.retrying.as_deref(), cx.entity().downgrade(), cx) {
            notices.push(updates);
        }
        let booting = service.booting_names(&self.thread_id);
        if !booting.is_empty() {
            notices.push(
                div()
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .px(px(12.))
                    .py(px(8.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(format!("Starting {}… This can take a minute.", booting.join(", ")))
                    .into_any_element(),
            );
        }
        if let Some(error) = self.devices.read(cx).operation(&self.thread_id).and_then(|operation| operation.error.clone()) {
            let devices = self.devices.clone();
            let thread = self.thread_id.clone();
            let dismiss = move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| devices.update(cx, |devices, cx| devices.dismiss_error(&thread, cx));
            notices.push(notice(Tone::Error, "device-error", div().child(error).into_any_element(), dismiss, cx));
        }
        notices
    }
}

impl HostRetry for DevicePane {
    fn set_retrying(&mut self, host: Option<String>) {
        self.retrying = host;
    }
}

/// Whoever shows host updates remembers which host's Retry runs.
pub(super) trait HostRetry: 'static {
    fn set_retrying(&mut self, host: Option<String>);
}

/// `DeviceHostUpdates`: hosts installing, starting or failed, with Retry where the server can.
pub(super) fn host_updates<V: HostRetry>(service: &ServiceState, devices: &Entity<Devices>, retrying: Option<&str>, owner: gpui::WeakEntity<V>, cx: &App) -> Option<AnyElement> {
    let updates = service.host_updates();
    if updates.is_empty() {
        return None;
    }
    let p = palette(cx);
    let rows = updates.into_iter().map(|update| {
        let retry = update.retry.then(|| {
            let (devices, owner, host) = (devices.clone(), owner.clone(), update.host_id.clone());
            ActionButton::new(SharedString::from(format!("device-host-retry-{}", update.host_id)), ButtonVariant::Outline, ButtonSize::Compact)
                .label(if retrying == Some(update.host_id.as_str()) { "Retrying…" } else { "Retry" })
                .disabled(retrying.is_some())
                .on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |owner, cx| {
                        owner.set_retrying(Some(host.clone()));
                        cx.notify();
                    });
                    let task = devices.update(cx, |devices, cx| devices.list(retry_host_payload(&host), cx));
                    let owner = owner.clone();
                    cx.spawn(async move |cx| {
                        let _ = task.await;
                        let _ = owner.update(cx, |owner, cx| {
                            owner.set_retrying(None);
                            cx.notify();
                        });
                    })
                    .detach();
                })
        });
        h_flex()
            .items_start()
            .gap(px(12.))
            .rounded(px(6.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.6))
            .px(px(12.))
            .py(px(8.))
            .text_xs()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(update.label))
                    .child(div().text_color(hsla(p.muted_foreground)).child(update.detail))
                    .when(update.failed, |this| this.child(div().mt(px(4.)).text_color(hsla(p.muted_foreground)).child(HOST_FAILED_HINT))),
            )
            .children(retry)
    });
    Some(v_flex().gap(px(8.)).children(rows).into_any_element())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Warning,
    Error,
}

/// `DeviceNotice`: a dismissable warning or error strip.
fn notice(tone: Tone, id: &str, body: AnyElement, dismiss: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
    let p = palette(cx);
    let (fill, text) = match tone {
        Tone::Warning => (hsla(p.warning_surface), hsla(p.warning_foreground)),
        Tone::Error => (hsla(p.destructive).opacity(0.05), hsla(p.destructive)),
    };
    let label = if tone == Tone::Warning { "Dismiss device warning" } else { "Dismiss device error" };
    h_flex()
        .items_start()
        .gap(px(8.))
        .border_b_1()
        .border_color(hsla(p.border))
        .px(px(12.))
        .py(px(8.))
        .text_xs()
        .bg(fill)
        .text_color(text)
        .child(Icon::CircleAlert.svg().mt(px(4.)).size(px(14.)).flex_none().text_color(text))
        .child(div().id(SharedString::from(format!("{id}-body"))).flex_1().min_w_0().max_h(px(128.)).overflow_y_scroll().child(body))
        .child(Button::new(SharedString::from(format!("{id}-dismiss"))).variant(Variant::Ghost).size(Size::IconXs).icon(Icon::X).tooltip(label).on_click(dismiss))
        .into_any_element()
}

/// Which step a loading view stands for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    Opening,
    Stream,
}

/// `DeviceLoadingView`: the device, a static spinner and what is happening, or the failure.
pub(super) fn loading_view(name: SharedString, description: SharedString, stage: Stage, message: SharedString, error: bool, action: Option<AnyElement>, cx: &App) -> AnyElement {
    let p = palette(cx);
    let bar = |filled: bool| div().h(px(4.)).flex_1().rounded_full().bg(if filled { hsla(p.foreground).opacity(0.6) } else { hsla(p.muted) });
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(hsla(p.background))
        .px(px(24.))
        .py(px(40.))
        .child(
            v_flex()
                .max_w(px(384.))
                .items_center()
                .gap(px(12.))
                .text_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(48.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.muted).opacity(0.3))
                        .child(Icon::Smartphone.svg().size(px(24.)).text_color(hsla(p.muted_foreground))),
                )
                .child(
                    v_flex()
                        .gap(px(4.))
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(name))
                        .when(!description.is_empty(), |this| this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(description))),
                )
                .child(
                    h_flex()
                        .gap(px(8.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .when(!error, |this| this.child(Icon::LoaderCircle.svg().size(px(12.)).text_color(hsla(p.muted_foreground))))
                        .child(message),
                )
                .children(action)
                .when(!error, |this| this.child(h_flex().w(px(96.)).gap(px(4.)).child(bar(true)).child(bar(stage == Stage::Stream)))),
        )
        .into_any_element()
}

impl Render for DevicePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let devices = self.devices.read(cx);
        let Some(service) = devices.service().cloned() else {
            let error = devices.load_error().map(str::to_owned);
            let retry = error.is_some().then(|| {
                let devices = self.devices.clone();
                ActionButton::new("device-load-retry", ButtonVariant::Outline, ButtonSize::Sm)
                    .label("Retry")
                    .on_click(move |_, _, cx| devices.update(cx, |devices, cx| devices.retry_load(cx)))
                    .into_any_element()
            });
            let message: SharedString = error.clone().unwrap_or_else(|| "Finding devices…".to_owned()).into();
            return loading_view("Devices".into(), SharedString::default(), Stage::Opening, message, error.is_some(), retry, cx);
        };
        if service.needs_setup() {
            return div().size_full().bg(hsla(p.background)).into_any_element();
        }
        let pending = devices.operation(&self.thread_id).and_then(|operation| operation.pending.clone());
        let screen = self.screen.clone();
        let loading = screen.is_none() && (pending.is_some() || service.hub_readiness().busy);
        let notices = self.render_notices(&service, screen.is_some(), cx);
        let body = match (&screen, loading) {
            (Some(screen), _) => screen.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            (None, true) => {
                let (name, description): (SharedString, SharedString) = match &pending {
                    Some(device) => (device.name.clone().into(), service.device_description(device).into()),
                    None => ("Devices".into(), SharedString::default()),
                };
                loading_view(name, description, Stage::Opening, service.loading_message(pending.as_ref()).into(), false, None, cx)
            }
            (None, false) => self.render_list(&service, pending.as_ref(), cx),
        };
        let top = v_flex().flex_none().bg(hsla(p.background)).children(notices);
        let body = div().relative().flex_1().min_h_0().flex().child(body);
        let root = v_flex().relative().size_full().min_h_0().bg(hsla(p.background));
        // While loading, notices float over the top of the loading view instead of pushing it.
        if loading {
            root.child(body).child(top.absolute().top_0().left_0().right_0()).into_any_element()
        } else {
            root.child(top).child(body).into_any_element()
        }
    }
}
