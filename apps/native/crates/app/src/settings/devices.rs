//! Devices: the "Devices" section of `DevicesSettingsPanel` (`DeviceIntegrationControls` in
//! `web/src/components/settings/IntegrationsSettings.tsx`), where the device hub and agent device
//! access stay switchable after setup. The connected environment's device state comes from
//! `crate::device`. Tool version popovers and device sources are not ported.

use gpui::{Context, IntoElement, ParentElement, Styled, Subscription};
use gpui_component::h_flex;
use serde_json::{Value, json};
use t3_core::device::{ServiceState, configure_hub_payload};
use t3_core::settings::search::Section;

use super::controls::{Row, Switch, r, section};
use super::{Block, SettingsView};
use crate::device::{Devices, DevicesEvent, setup_status};
use crate::toast::{Toast, ToastKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Hub,
    Agent,
}

pub(crate) struct DevicesState {
    pending: Option<Pending>,
    _subscription: Option<Subscription>,
}

impl DevicesState {
    pub(super) fn new(cx: &mut Context<SettingsView>) -> Self {
        let subscription = crate::device::devices(cx).map(|devices| {
            cx.subscribe(&devices, |view: &mut SettingsView, _, _: &DevicesEvent, cx| {
                if view.section == Section::Devices {
                    cx.notify();
                }
            })
        });
        DevicesState { pending: None, _subscription: subscription }
    }
}

impl SettingsView {
    pub(super) fn devices_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let Some(devices) = crate::device::devices(cx) else { return Vec::new() };
        let (service, can_configure) = {
            let devices = devices.read(cx);
            (devices.service().cloned(), devices.can_configure())
        };
        let loaded = service.is_some();
        let service = service.unwrap_or_else(ServiceState::empty);
        let pending = self.devices.pending;
        let busy = service.hub_readiness().busy;
        let enabled = service.hub_enabled();
        let view = cx.entity().downgrade();
        let hub = Switch::new("device-hub-switch", enabled).disabled(!can_configure || !loaded || busy || pending.is_some()).on_change({
            let view = view.clone();
            move |on, _, cx| {
                // Turning the hub on here also finishes setup, as the wizard's Done does.
                let input = if on { json!({ "enabled": true, "onboardingCompleted": true }) } else { configure_hub_payload(false) };
                let _ = view.update(cx, |view, cx| view.configure_devices(Pending::Hub, input, cx));
            }
        });
        let agent = Switch::new("agent-device-access-switch", service.agent_access_enabled).disabled(!can_configure || !loaded || !enabled || busy || pending.is_some()).on_change(
            move |on, _, cx| {
                let _ = view.update(cx, |view, cx| view.configure_devices(Pending::Agent, json!({ "agentAccessEnabled": on }), cx));
            },
        );
        // The compact setup status shows beside a switch while its change runs.
        let hub_status = setup_status((pending == Some(Pending::Hub)).then(|| service.hub_setup_status(true)).flatten(), cx);
        let agent_status = setup_status((pending == Some(Pending::Agent)).then(|| service.agent_setup_status(true)).flatten(), cx);
        let rows = vec![
            Row::new("device-hub", "Use devices")
                .description("Open simulators and emulators in the Device panel.")
                .highlighted(self.is_target("device-hub"))
                .control(h_flex().gap(r(8.)).children(hub_status).child(hub))
                .into_any_element(),
            Row::new("agent-device-access", "Agents can use devices")
                .description("New agent sessions can open and drive these devices.")
                .highlighted(self.is_target("agent-device-access"))
                .control(h_flex().gap(r(8.)).children(agent_status).child(agent))
                .into_any_element(),
        ];
        vec![Block::new(&["devices", "device-hub", "agent-device-access"], section("Devices", rows, cx))]
    }

    /// `update`: one `device.configure`; a failure says which environment kept its settings.
    fn configure_devices(&mut self, pending: Pending, input: Value, cx: &mut Context<Self>) {
        let Some(devices) = crate::device::devices(cx) else { return };
        if self.devices.pending.is_some() || !devices.read(cx).can_configure() {
            return;
        }
        self.devices.pending = Some(pending);
        cx.notify();
        let label = self.environment_label(cx);
        let task = devices.update(cx, |devices: &mut Devices, cx| devices.configure(input, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.devices.pending = None;
                if result.is_err() {
                    let toast = Toast::new(ToastKind::Error, "Device settings not saved on all environments").description(format!("Could not update {label}."));
                    this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }
}
