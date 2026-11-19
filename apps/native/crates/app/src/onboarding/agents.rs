//! `ConnectedAgentsStep` and `AgentCard` from `web/src/components/onboarding/WelcomeWizard.tsx`.
//! The embedded setup pane is `agent_terminal`; commands and readiness come from core.

use gpui::prelude::FluentBuilder;
use gpui::{AppContext, AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, px};
use gpui_component::{h_flex, v_flex};
use t3_core::onboarding::{
    AgentState, OnboardingAgent, agent_display_name, agent_state, group_welcome_agents, install_command, login_command, onboarding_agents,
    welcome_agent_state,
};
use t3_core::providers::accent_color;
use t3_core::server_config::ServerConfig;
use t3_core::settings::providers::{self as model, SERVER_REFRESH_PROVIDERS, refresh_payload};

use super::{AgentSetup, WelcomeWizard, row_card, section_heading};
use crate::app_state::AppEvent;
use crate::icons::Icon;
use crate::provider_icon::{ProviderIcon, ProviderMark};
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, r};
use crate::theme::{hsla, palette};

/// One `AgentCard`, read from the server config before rendering.
struct AgentRow {
    id: SharedString,
    instance_id: Option<String>,
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
            instance_id: provider.map(|provider| provider.instance_id.clone()),
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
        self.refresh_providers_for(None, cx);
    }

    /// Refreshes one provider instance (after its install/login command exits, `pingdotgg/t3code#36477`)
    /// or all providers when `instance_id` is `None`.
    pub(super) fn refresh_providers_for(&mut self, instance_id: Option<&str>, cx: &mut Context<Self>) {
        if crate::chatgpt_setup::fixture_active(cx) { return; }
        let app = self.state.read(cx);
        let asked_on = app.connection_id();
        let payload = match instance_id {
            Some(id) => serde_json::json!({ "instanceId": id }),
            None => refresh_payload(),
        };
        let task = app.rpc(SERVER_REFRESH_PROVIDERS, payload);
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
                let (driver, instance_id) = (row.driver, row.instance_id);
                Button::new(row.id, ButtonVariant::Ghost, ButtonSize::Xs)
                    .icon(Icon::Terminal)
                    .label(if row.state == AgentState::SignIn { "Sign in" } else { "Install" })
                    .disabled(open || row.command.is_none() || !self.terminal_access)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(instance_id) = &instance_id {
                            this.open_agent_terminal(driver, instance_id, cx);
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

    pub(super) fn watch_terminal_grants(&mut self, cx: &mut Context<Self>) {
        let app = self.state.read(cx);
        let connection = app.connection_id();
        if self.terminal_grants.as_ref().map(|(id, _)| *id) == connection {
            return;
        }
        self.setup = None;
        self.terminal_grants = None;
        self.terminal_access = false;
        let (Some(connection), Some(client)) = (connection, app.client_handle()) else {
            return;
        };
        let mut grants = client.session_state();
        self.terminal_access =
            t3_core::terminal::TerminalAccess::of(grants.borrow_and_update().as_ref()).operate;
        self.terminal_grants = Some((
            connection,
            cx.spawn(async move |this, cx| {
                while grants.changed().await.is_ok() {
                    let access =
                        t3_core::terminal::TerminalAccess::of(grants.borrow_and_update().as_ref())
                            .operate;
                    if this
                        .update(cx, |this, cx| {
                            this.terminal_access = access;
                            if let Some(setup) = &this.setup {
                                setup.terminal.update(cx, |_, cx| cx.notify());
                            }
                            cx.notify();
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }),
        ));
    }

    fn open_agent_terminal(
        &mut self,
        driver: &'static str,
        instance_id: &str,
        cx: &mut Context<Self>,
    ) {
        use super::agent_terminal::{AgentInstallTerminal, Closed, CommandFinished, can_operate};
        let app = self.state.read(cx);
        let Some(client) = app.client_handle().filter(can_operate) else {
            return;
        };
        let Some(config) = app.server_config.config.as_ref() else {
            return;
        };
        let agent = config
            .providers
            .iter()
            .find(|provider| provider.driver == driver && provider.instance_id == instance_id)
            .map(|provider| OnboardingAgent { driver, provider: Some(provider) })
            .or_else(|| {
                onboarding_agents(&config.providers, config.managed_policy.as_ref())
                    .into_iter()
                    .find(|agent| {
                        agent.driver == driver
                            && agent
                                .provider
                                .is_some_and(|provider| provider.instance_id == instance_id)
                    })
            });
        let Some(row) = agent.map(|agent| AgentRow::of(&agent, Some(config))) else {
            return;
        };
        let Some(command) = row.command else { return };
        let (cwd, runtime) = (config.cwd.clone(), app.runtime().clone());
        let instance_id = instance_id.to_owned();
        let session = t3_core::onboarding::terminal::Session {
            driver: driver.to_owned(),
            command,
            cwd,
            provider_instance_id: instance_id,
        };
        let terminal = cx.new(|cx| AgentInstallTerminal::new(session, client, runtime, cx));
        let closed = cx.subscribe(&terminal, |this, _, _: &Closed, cx| {
            this.setup = None;
            this.refresh_providers(cx);
            cx.notify();
        });
        let finished = cx.subscribe(&terminal, |this, _, event: &CommandFinished, cx| {
            this.refresh_providers_for(event.instance_id.as_deref(), cx);
        });
        self.setup = Some(AgentSetup {
            driver,
            terminal,
            _closed: closed,
            _finished: Some(finished),
        });
        cx.notify();
    }

    pub(super) fn render_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let (machine, ready_count, mut primary_rows, more_rows, can_add, existing) = {
            let app = self.state.read(cx);
            let config = app.server_config.config.as_ref();
            let machine = app
                .active_environment()
                .map(|environment| environment.label.clone())
                .or_else(|| config.map(|config| config.environment.label.clone()))
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| "Computer".to_owned());
            let providers = config.map(|c| c.providers.as_slice());
            let settings = config.map(|c| &c.settings);
            let policy = config.and_then(|c| c.managed_policy.as_ref());
            let grouped = group_welcome_agents(providers, settings, policy);

            // Primary drivers fallback while providers haven't loaded yet, else WelcomePage found + available groups.
            let mut primary_rows: Vec<AgentRow> = if grouped.found.is_empty() && grouped.available.is_empty() {
                onboarding_agents(providers.unwrap_or_default(), policy)
                    .iter()
                    .map(|agent| AgentRow::of(agent, config))
                    .collect()
            } else {
                grouped
                    .found
                    .iter()
                    .map(|provider| {
                        let driver: &'static str = match provider.driver.as_str() {
                            "codex" => "codex",
                            "claudeAgent" => "claudeAgent",
                            _ => "other",
                        };
                        let mut row = AgentRow::of(&OnboardingAgent { driver, provider: Some(provider) }, config);
                        row.state = welcome_agent_state(provider, settings);
                        row
                    })
                    .collect()
            };

            let more_rows: Vec<AgentRow> = grouped
                .available
                .iter()
                .map(|provider| {
                    let driver: &'static str = match provider.driver.as_str() {
                        "codex" => "codex",
                        "claudeAgent" => "claudeAgent",
                        _ => "other",
                    };
                    let mut row = AgentRow::of(&OnboardingAgent { driver, provider: Some(provider) }, config);
                    row.state = welcome_agent_state(provider, settings);
                    row
                })
                .collect();

            if let Some(id) = &self.created_chatgpt {
                let row = primary_rows.iter().position(|r| r.instance_id.as_ref() == Some(id)).map(|index| primary_rows.remove(index));
                let mut row = row.unwrap_or_else(|| AgentRow::of(&OnboardingAgent { driver: "codex", provider: None }, config));
                row.instance_id = Some(id.clone());
                primary_rows.insert(0, row);
            }
            let can_add = providers.unwrap_or_default().iter().any(|p| p.driver == "codex" && agent_state(Some(p)) == AgentState::Ready);
            let existing = config.map(|c| c.settings.provider_instances.iter().filter(|(_, p)| t3_core::settings::providers::chatgpt::existing_chosen(p.config.as_ref())).map(|(id, _)| id.to_owned()).collect::<Vec<_>>()).unwrap_or_default();
            (machine, grouped.ready_count, primary_rows, more_rows, can_add, existing)
        };
        self.chatgpt.retain(|id, _| !existing.contains(id));

        let mut cards = Vec::new();
        let mut add_view = None;
        for row in primary_rows.drain(..) {
            let id = row.instance_id.clone().unwrap_or_else(|| "codex".into());
            if row.driver == "codex" && !existing.contains(&id) {
                let view = if let Some(view) = self.chatgpt.get(&id) {
                    view.clone()
                } else {
                    let view = cx.new(|cx| crate::chatgpt_setup::ChatGptSetup::new(self.state.clone(), id.clone(), true, window, cx));
                    self.chatgpt_subscriptions.push(cx.subscribe(&view, |this, _, created: &crate::chatgpt_setup::AccountCreated, cx| {
                        this.created_chatgpt = Some(created.0.clone());
                        cx.notify();
                    }));
                    if self.created_chatgpt.as_ref() == Some(&id) {
                        view.update(cx, |view, cx| view.start_automatically(cx));
                    }
                    self.chatgpt.insert(id, view.clone());
                    view
                };
                cards.push(div().child(view.clone()).into_any_element());
                if can_add {
                    add_view = Some(view);
                }
            } else {
                cards.push(self.render_agent(row, cx));
            }
        }
        if let Some(view) = add_view {
            let enabled = view.read(cx).can_manage(cx);
            let denial = view.read(cx).managed_denial(cx);
            cards.push(
                crate::components::managed::ManagedControl::new(
                    "managed-welcome-chatgpt-another",
                    denial,
                    Button::new("welcome-chatgpt-another", ButtonVariant::GhostMuted, ButtonSize::Xs)
                        .label("Connect another ChatGPT account")
                        .disabled(!enabled)
                        .on_click(move |_, _, cx| view.update(cx, |view, cx| view.open_add(cx))),
                )
                .into_any_element(),
            );
        }

        let more_count = more_rows.len();
        let more_open = self.more_agents_open || cards.is_empty();
        let more_cards: Vec<AnyElement> = if more_open {
            more_rows.into_iter().map(|row| self.render_agent(row, cx)).collect()
        } else {
            Vec::new()
        };

        let setup = self.setup.as_ref().map(|setup| setup.terminal.clone());
        let subtitle = if ready_count > 0 {
            format!("{} ready on {machine}. You can add more anytime in Settings.", if ready_count == 1 { "1 agent".to_owned() } else { format!("{} agents", ready_count) })
        } else {
            format!("Choose an agent on {machine} to start coding. You can add more later.")
        };
        v_flex()
            .gap(r(10.))
            .child(section_heading("Agents", &subtitle, cx))
            .when(!self.terminal_access, |section| {
                section.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(t3_core::onboarding::terminal::PERMISSION_MESSAGE))
            })
            .when(!cards.is_empty(), |section| section.child(v_flex().gap(r(6.)).children(cards)))
            .when(more_count > 0, |section| {
                section.child(
                    v_flex()
                        .gap(r(6.))
                        .child(
                            Button::new("welcome-more-agents", ButtonVariant::GhostMuted, ButtonSize::Xs)
                                .icon(if more_open { Icon::ChevronDown } else { Icon::ChevronRight })
                                .label(format!("More agents ({more_count})"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.more_agents_open = !this.more_agents_open;
                                    cx.notify();
                                })),
                        )
                        .when(more_open, |list| list.child(v_flex().gap(r(6.)).children(more_cards))),
                )
            })
            .children(setup)
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
