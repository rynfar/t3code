//! Environments and About. Environments shows the connected environment (label, address, server
//! and protocol versions, status) with Switch environment, Disconnect, and Remove; the saved list
//! lives on the connect screen the switcher opens, alongside the T3 Connect account.
//! About shows the app version (`AboutSettingsPanel` in `SettingsPanels.tsx`).

use gpui::{Context, IntoElement, ParentElement, div};
use t3_client::auth::ORCHESTRATION_PROTOCOL_VERSION;
use t3_client::store::SavedEnvironment;

use super::controls::{Button, ButtonSize, ButtonVariant, Row, muted_text, section};
use super::{Block, SettingsView, close};
use crate::app_state::Phase;

impl SettingsView {
    pub(super) fn environments(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let state = self.state.read(cx);
        let environment = state.server_config.config.as_ref().map(|config| config.environment.clone()).unwrap_or_default();
        let saved = state.active_environment().cloned();
        let address = saved.as_ref().map(SavedEnvironment::address).unwrap_or_default();
        let status = match &state.phase {
            Phase::Online(status) => status.summary().0,
            Phase::Unpaired => "Not connected".to_owned(),
        };
        let label = [environment.label.as_str(), saved.as_ref().map_or("", |saved| saved.label.as_str())]
            .into_iter()
            .find(|label| !label.trim().is_empty())
            .unwrap_or("Environment")
            .to_owned();
        let protocol = environment.orchestration_protocol_version.unwrap_or(ORCHESTRATION_PROTOCOL_VERSION);
        let value = |text: String| muted_text(text, cx);
        let (switch_state, disconnect_state, remove_state) = (self.state.clone(), self.state.clone(), self.state.clone());
        let cloud_state = self.state.clone();
        let remove_id = saved.as_ref().map(|saved| saved.id.clone());
        let connection = section(
            "Environments",
            vec![
                Row::new("remote-environments", label)
                    .description(if address.is_empty() { "This device's paired environment.".to_owned() } else { address })
                    .highlighted(self.is_target("remote-environments"))
                    .control(value(status))
                    .into_any_element(),
                Row::new("server-version", "Server version")
                    .control(value(if environment.server_version.is_empty() { "Unknown".to_owned() } else { environment.server_version.clone() }))
                    .into_any_element(),
                Row::new("orchestration-protocol", "Orchestration protocol").control(value(protocol.to_string())).into_any_element(),
                Row::new("cloud-account", "T3 Connect")
                    .description("Sign in to connect to your environments from anywhere.")
                    .control(Button::new("cloud-account-button", ButtonVariant::Outline, ButtonSize::Sm).label("Manage…").on_click(move |_, window, cx| {
                        close(window, cx);
                        cloud_state.update(cx, |state, cx| state.open_switcher(cx));
                    }))
                    .into_any_element(),
                Row::new("switch-environment", "Switch environment")
                    .description("Connect to another saved environment, or pair a new one.")
                    .control(Button::new("switch-environment-button", ButtonVariant::Outline, ButtonSize::Sm).label("Switch\u{2026}").on_click(move |_, window, cx| {
                        close(window, cx);
                        switch_state.update(cx, |state, cx| state.open_switcher(cx));
                    }))
                    .into_any_element(),
                Row::new("disconnect", "Disconnect")
                    .description("Stop using this environment for now. It stays saved on this device.")
                    .control(Button::new("disconnect-server", ButtonVariant::Outline, ButtonSize::Sm).label("Disconnect").on_click(move |_, window, cx| {
                        close(window, cx);
                        disconnect_state.update(cx, |state, cx| state.disconnect(cx));
                    }))
                    .into_any_element(),
                Row::new("remove-environment", "Remove from this device")
                    .description("Forget this environment and its credential. Pair again with a new pairing link.")
                    .control(Button::new("remove-environment-button", ButtonVariant::DestructiveOutline, ButtonSize::Sm).label("Remove").disabled(remove_id.is_none()).on_click(
                        move |_, window, cx| {
                            let Some(id) = remove_id.clone() else { return };
                            close(window, cx);
                            remove_state.update(cx, |state, cx| state.remove_environment(&id, cx));
                        },
                    ))
                    .into_any_element(),
            ],
            cx,
        );
        vec![Block::new(&["remote-environments", "connections"], connection)]
    }

    pub(super) fn about(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let app = section(
            "App",
            vec![Row::new("app-version", "Version")
                .description("T3 Code Native")
                .highlighted(self.is_target("app-version"))
                .control(div().child(muted_text(env!("CARGO_PKG_VERSION"), cx)))
                .into_any_element()],
            cx,
        );
        vec![Block::new(&["about", "app-version"], app)]
    }
}
