//! `AgentsStep`, `ConnectedAgentsStep` and `AgentCard` of `WelcomeWizard.tsx`: Codex and Claude
//! on the connected computer with their readiness, refreshed when the step opens. Install and
//! Sign in show the command the web types into its setup terminal, to run on that computer;
//! closing it checks the agents again, as closing the terminal does.

use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, px};
use gpui_component::{h_flex, v_flex};
use t3_core::onboarding::{AgentState, OnboardingAgent, agent_display_name, agent_state, install_command, login_command, onboarding_agents};
use t3_core::providers::accent_color;
use t3_core::server_config::ServerConfig;
use t3_core::settings::providers::{self as model, SERVER_REFRESH_PROVIDERS, refresh_payload};

use super::{AgentSetup, WelcomeWizard, row_card, step_title};
use crate::app_state::AppEvent;
use crate::icons::Icon;
use crate::provider_icon::{ProviderIcon, ProviderMark};
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, r};
use crate::theme::{hsla, palette};

/// One `AgentCard`, read from the server config before rendering.
struct AgentRow {
    id: SharedString,
    driver: &'static str,
    name: String,
    mark: ProviderMark,
    state: AgentState,
    /// "Ready to code.", or the summary's headline and detail.
    line: String,
    headline: String,
    /// What Install or Sign in runs: the login command once installed, else the installer.
    command: Option<String>,
}

impl AgentRow {
    fn of(agent: &OnboardingAgent, config: Option<&ServerConfig>) -> AgentRow {
        let provider = agent.provider;
        let name = agent_display_name(agent.driver, provider);
        let state = agent_state(provider);
        let summary = model::summary(provider, provider.is_none_or(|provider| provider.enabled));
        let line = match (state, &summary.detail) {
            (AgentState::Ready, _) => "Ready to code.".to_owned(),
            (_, Some(detail)) => format!("{} · {detail}", summary.headline),
            (_, None) => summary.headline.clone(),
        };
        let os = config.and_then(|config| config.environment.platform.as_ref()).map_or("unknown", |platform| platform.os.as_str());
        let command = match (provider, config) {
            (Some(provider), Some(config)) if provider.installed => Some(login_command(provider, &config.settings, os)),
            (Some(_), Some(_)) => install_command(agent.driver, os).map(str::to_owned),
            _ => None,
        };
        AgentRow {
            id: format!("welcome-agent-setup-{}", provider.map_or(agent.driver, |provider| provider.instance_id.as_str())).into(),
            driver: agent.driver,
            mark: ProviderMark::configured(agent.driver, &name, provider.and_then(accent_color)),
            name,
            state,
            line,
            headline: summary.headline,
            command,
        }
    }
}

/// A reply describes the environment it was asked on: after a switch to another, it is dropped.
fn answers_current(asked_on: Option<u64>, now: Option<u64>) -> bool {
    asked_on.is_some() && asked_on == now
}

impl WelcomeWizard {
    /// `serverEnvironment.refreshProviders`; the reply replaces the provider list of the
    /// environment it came from.
    pub(super) fn refresh_providers(&mut self, cx: &mut Context<Self>) {
        let app = self.state.read(cx);
        let asked_on = app.connection_id();
        let task = app.rpc(SERVER_REFRESH_PROVIDERS, refresh_payload());
        let state = self.state.clone();
        cx.spawn(async move |_, cx| {
            let providers = match task.await {
                Ok(reply) => model::decode_providers_reply(reply),
                Err(error) => {
                    tracing::warn!(%error, "failed to refresh providers");
                    None
                }
            };
            state.update(cx, |state, cx| {
                if !answers_current(asked_on, state.connection_id()) {
                    return;
                }
                if providers.is_some_and(|providers| state.server_config.apply_providers(providers)) {
                    cx.emit(AppEvent::ServerConfig);
                }
            });
        })
        .detach();
    }

    fn render_agent(&self, row: AgentRow, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let quiet = |text: String| div().text_xs().text_color(hsla(p.muted_foreground)).child(text).into_any_element();
        let status = match row.state {
            AgentState::Ready => h_flex()
                .gap(r(6.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.success_foreground))
                .child(Icon::Check.render(px(14.), hsla(p.success_foreground)))
                .child("Ready")
                .into_any_element(),
            AgentState::Checking => quiet("Checking...".to_owned()),
            AgentState::Disabled => quiet("Disabled".to_owned()),
            AgentState::Attention => quiet(row.headline),
            AgentState::Install | AgentState::SignIn => {
                let open = self.setup.as_ref().is_some_and(|setup| setup.driver == row.driver);
                let (driver, command) = (row.driver, row.command.clone());
                Button::new(row.id, ButtonVariant::Ghost, ButtonSize::Xs)
                    .icon(Icon::Terminal)
                    .label(if row.state == AgentState::SignIn { "Sign in" } else { "Install" })
                    .disabled(open || command.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(command) = command.clone() {
                            this.setup = Some(AgentSetup { driver, command });
                            cx.notify();
                        }
                    }))
                    .into_any_element()
            }
        };
        row_card(cx)
            .gap(r(12.))
            .px(r(16.))
            .py(r(16.))
            .child(ProviderIcon::new(row.mark).size(px(20.)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(row.name))
                    .child(div().mt(r(2.)).text_xs().line_height(r(19.5)).text_color(hsla(p.muted_foreground)).child(row.line)),
            )
            .child(div().flex_none().child(status))
            .into_any_element()
    }

    /// Where the web's setup terminal sits: the command and where to run it.
    fn render_setup(&self, command: &str, machine: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        v_flex()
            .mt(r(16.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.7))
            .bg(hsla(p.background))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(r(12.))
                    .px(r(12.))
                    .py(r(6.))
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .rounded_t(r(7.))
                    .bg(hsla(p.background).opacity(0.6))
                    .child(div().text_size(r(11.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child(format!("Run this command in a terminal on {machine}.")))
                    .child(Button::new("welcome-agent-setup-close", ButtonVariant::GhostMuted, ButtonSize::Xs).label("Close").on_click(cx.listener(|this, _, _, cx| {
                        this.setup = None;
                        this.refresh_providers(cx);
                        cx.notify();
                    }))),
            )
            .child(div().p(r(12.)).child(self.command_block("welcome-copy-setup", command, cx)))
            .into_any_element()
    }

    pub(super) fn render_agents(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let app = self.state.read(cx);
        let config = app.server_config.config.as_ref();
        let machine = app
            .active_environment()
            .map(|environment| environment.label.clone())
            .or_else(|| config.map(|config| config.environment.label.clone()))
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "Computer".to_owned());
        let providers = config.map_or(&[][..], |config| &config.providers);
        let rows: Vec<AgentRow> =
            onboarding_agents(providers, config.and_then(|config| config.managed_policy.as_ref())).iter().map(|agent| AgentRow::of(agent, config)).collect();
        let cards: Vec<AnyElement> = rows.into_iter().map(|row| self.render_agent(row, cx)).collect();
        let setup = self.setup.as_ref().map(|setup| setup.command.clone()).map(|command| self.render_setup(&command, &machine, cx));
        v_flex()
            .child(step_title("Connect your agents", "Choose an agent to start coding. You can add more later.", cx))
            .child(
                v_flex()
                    .mt(r(20.))
                    .child(div().mb(r(8.)).text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(machine))
                    .child(v_flex().gap(r(6.)).children(cards))
                    .children(setup),
            )
            .child(
                h_flex().mt(r(24.)).justify_end().child(
                    Button::new("welcome-agents-continue", ButtonVariant::Default, ButtonSize::Default)
                        .label("Next")
                        .trailing(Icon::ArrowRight)
                        .on_click(cx.listener(|this, _, _, cx| this.go_to(super::Step::Projects, cx))),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::answers_current;

    #[test]
    fn a_provider_reply_after_a_switch_is_dropped() {
        assert!(answers_current(Some(3), Some(3)));
        // Asked on A, answered after switching to B or disconnecting.
        assert!(!answers_current(Some(3), Some(4)));
        assert!(!answers_current(Some(3), None));
        assert!(!answers_current(None, None));
    }
}
