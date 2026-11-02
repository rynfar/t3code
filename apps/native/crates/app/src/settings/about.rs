//! Environments and About. The native app holds one paired environment, so Environments shows
//! that connection (label, address, server and protocol versions, status) with Disconnect; the
//! web's saved, linked and remote environment management is not ported. About shows the app
//! version (`AboutSettingsPanel` in `SettingsPanels.tsx`).

use gpui::{Context, IntoElement, ParentElement, div};
use t3_client::auth::ORCHESTRATION_PROTOCOL_VERSION;
use t3_client::supervisor::ConnectionStatus;

use super::controls::{Button, ButtonSize, ButtonVariant, Row, muted_text, section};
use super::{Block, SettingsView, close};
use crate::app_state::Phase;

impl SettingsView {
    pub(super) fn environments(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let state = self.state.read(cx);
        let environment = state.server_config.config.as_ref().map(|config| config.environment.clone()).unwrap_or_default();
        let address = self.server_address.clone();
        let status = match &state.phase {
            Phase::Online(ConnectionStatus::Connected) => "Connected".to_owned(),
            Phase::Online(ConnectionStatus::Connecting) => "Connecting\u{2026}".to_owned(),
            Phase::Online(ConnectionStatus::Reconnecting { reason, .. }) => format!("Reconnecting: {reason}"),
            Phase::Online(ConnectionStatus::NeedsPairing { reason }) => reason.clone(),
            Phase::Pairing => "Pairing\u{2026}".to_owned(),
            Phase::Unpaired { .. } => "Not connected".to_owned(),
        };
        let label = if environment.label.is_empty() { "Environment".to_owned() } else { environment.label.clone() };
        let protocol = environment.orchestration_protocol_version.unwrap_or(ORCHESTRATION_PROTOCOL_VERSION);
        let value = |text: String| muted_text(text, cx);
        let disconnect_state = self.state.clone();
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
                Row::new("disconnect", "Disconnect from server")
                    .description("Forget this server on this device. Pair again with a new pairing link.")
                    .control(
                        Button::new("disconnect-server", ButtonVariant::DestructiveOutline, ButtonSize::Sm).label("Disconnect").on_click(move |_, window, cx| {
                            close(window, cx);
                            disconnect_state.update(cx, |state, cx| state.forget_connection(cx));
                        }),
                    )
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
