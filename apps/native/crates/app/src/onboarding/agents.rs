//! `ConnectedAgentsStep` and `AgentCard` from `web/src/components/onboarding/WelcomeWizard.tsx`.
//! The embedded setup pane is `agent_terminal`; commands and readiness come from core.

use gpui::prelude::FluentBuilder;
use gpui::{AppContext, AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, px};
use gpui_component::{h_flex, v_flex};
use t3_core::onboarding::{
    AgentState, OnboardingAgent, agent_display_name, agent_state, group_welcome_agents, install_command, is_broken_version, login_command, onboarding_agents,
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
    driver: String,
    name: String,
    mark: ProviderMark,
    state: AgentState,
    /// "Ready to code.", or the summary's headline and detail.
    line: String,
    broken: bool,
    /// What Install or Sign in runs: the login command once installed, else the installer.
    command: Option<String>,
}

impl AgentRow {
    fn of(agent: &OnboardingAgent, config: Option<&ServerConfig>) -> AgentRow {
        Self::for_driver(agent.driver, agent.provider, config)
    }

    fn for_driver(driver: &str, provider: Option<&t3_core::server_config::ServerProvider>, config: Option<&ServerConfig>) -> Self {
        let name = agent_display_name(driver, provider);
        let state = provider.map_or(AgentState::Checking, |provider| welcome_agent_state(provider, config.map(|config| &config.settings)));
        let summary = model::summary(provider, state != AgentState::Disabled);
        let line = match (state, &summary.detail) {
            (AgentState::Ready, _) => "Ready to code.".to_owned(),
            (_, Some(detail)) => format!("{} · {detail}", summary.headline),
            (_, None) => summary.headline,
        };
        let os = config.and_then(|config| config.environment.platform.as_ref()).map_or("unknown", |platform| platform.os.as_str());
        let command = match (driver, provider, config) {
            ("codex" | "claudeAgent", Some(provider), Some(config)) if provider.installed => Some(login_command(provider, &config.settings, os)),
            (_, Some(_), Some(_)) => install_command(driver, os).map(str::to_owned),
            _ => None,
        };
        Self {
            instance_id: provider.map(|provider| provider.instance_id.clone()),
            id: format!("welcome-agent-setup-{}", provider.map_or(driver, |provider| provider.instance_id.as_str())).into(),
            driver: driver.to_owned(),
            mark: ProviderMark::configured(driver, &name, provider.and_then(accent_color)),
            name,
            state,
            line,
            broken: provider.is_some_and(is_broken_version),
            command,
        }
    }

    fn action(&self) -> AgentAction {
        match self.state {
            AgentState::Disabled => AgentAction::Enable,
            _ if self.broken => AgentAction::Settings("Update"),
            AgentState::Ready | AgentState::Checking => AgentAction::Status,
            AgentState::Install | AgentState::SignIn if self.command.is_some() => AgentAction::Terminal,
            AgentState::Attention => AgentAction::Settings("Details"),
            AgentState::SignIn => AgentAction::Settings("Sign in"),
            AgentState::Install => AgentAction::Settings("Set up"),
        }
    }

}

#[derive(Debug, PartialEq, Eq)]
enum AgentAction {
    Status,
    Enable,
    Terminal,
    Settings(&'static str),
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
        let t = crate::i18n::t;
        let status = match row.action() {
            AgentAction::Status if row.state == AgentState::Ready => h_flex()
                .gap(r(6.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.success_foreground))
                .child(Icon::Check.render(px(14.), hsla(p.success_foreground)))
                .child(t(cx, "Ready"))
                .into_any_element(),
            AgentAction::Status => div().text_xs().text_color(hsla(p.muted_foreground))
                .child(t(cx, "Checking...")).into_any_element(),
            AgentAction::Enable => {
                let instance_id = row.instance_id;
                let disabled = instance_id.as_ref().is_none_or(|id| {
                    self.enabling_agents.contains(id) || !self.can_enable_agent(id, &row.driver, cx)
                });
                Button::new(row.id, ButtonVariant::Outline, ButtonSize::Xs)
                    .label("Turn on")
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(id) = &instance_id { this.enable_agent(id, &row.driver, cx); }
                    }))
                    .into_any_element()
            }
            AgentAction::Terminal => {
                let instance_id = row.instance_id;
                let open = self.setup.as_ref().is_some_and(|setup| Some(&setup.instance_id) == instance_id.as_ref());
                Button::new(row.id, ButtonVariant::Ghost, ButtonSize::Xs)
                    .icon(Icon::Terminal)
                    .label(if row.state == AgentState::SignIn { "Sign in" } else { "Install" })
                    .disabled(open || instance_id.is_none() || !self.terminal_access)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(instance_id) = &instance_id { this.open_agent_terminal(instance_id, cx); }
                    }))
                    .into_any_element()
            }
            AgentAction::Settings(label) => {
                let instance_id = row.instance_id;
                Button::new(row.id, ButtonVariant::Outline, ButtonSize::Xs)
                    .label(label)
                    .disabled(instance_id.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(instance_id) = &instance_id { this.open_agent_settings(instance_id, cx); }
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

    fn can_manage_agents(&self, cx: &gpui::App) -> bool {
        self.state.read(cx).client_handle().is_some_and(|client| {
            client.session_state().borrow().as_ref().is_some_and(|session| {
                session.grants(t3_core::usage::PROVIDERS_MANAGE_SCOPE)
            })
        })
    }

    fn can_enable_agent(&self, instance_id: &str, driver: &str, cx: &gpui::App) -> bool {
        self.can_manage_agents(cx) && self.state.read(cx).server_config.config.as_ref().is_some_and(|config| {
            t3_core::onboarding::managed_driver_allowed(config.managed_policy.as_ref(), driver)
                && !t3_core::managed_policy::setting_locked(config.managed_policy.as_ref(), &format!("providerInstances.{instance_id}"))
        })
    }

    fn enable_agent(&mut self, instance_id: &str, driver: &str, cx: &mut Context<Self>) {
        if !self.can_enable_agent(instance_id, driver, cx) || !self.enabling_agents.insert(instance_id.to_owned()) {
            return;
        }
        let app = self.state.read(cx);
        let config = app.server_config.config.as_ref().expect("provider settings available");
        let previous = config.settings.provider_instances.get(instance_id).cloned().unwrap_or_else(|| {
            t3_core::server_config::ProviderInstanceConfig { driver: driver.to_owned(), ..Default::default() }
        });
        let mut next = model::with_enabled(&previous, true);
        if next.config.as_ref().and_then(|config| config.get("enabled")) == Some(&serde_json::Value::Bool(false)) {
            next = model::form::with_field(&next, "enabled", serde_json::Value::Bool(true));
        }
        let payload = model::upsert_payload(&config.settings, instance_id, &previous, &next);
        let asked_on = app.connection_id();
        let instance = instance_id.to_owned();
        let wizard = cx.entity().downgrade();
        crate::settings::store::update_server_then(
            &self.state,
            t3_core::settings::ServerSettingChange::Prepared(payload),
            move |error, cx| {
                let _ = wizard.update(cx, |this, cx| {
                    if !answers_current(asked_on, this.state.read(cx).connection_id()) { return; }
                    this.enabling_agents.remove(&instance);
                    if error.is_none() { this.refresh_providers_for(Some(&instance), cx); }
                    cx.notify();
                });
            },
            cx,
        );
        cx.notify();
    }

    fn open_agent_settings(&mut self, instance_id: &str, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let target = crate::composer::ProviderTarget {
            environment: crate::composer::ProviderTarget::environment(state),
            connection: state.connection_id(),
            thread: None,
            instance: Some(instance_id.to_owned()),
        };
        self.dismiss_for_settings(cx);
        crate::settings::open_provider_setup(target, cx);
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
        self.can_read_relay = false;
        self.can_write_relay = false;
        let (Some(connection), Some(client)) = (connection, app.client_handle()) else {
            return;
        };
        let mut grants = client.session_state();
        let read_grants = |session: Option<&t3_core::auth::AuthSessionState>| {
            (
                t3_core::terminal::TerminalAccess::of(session).operate,
                session.is_some_and(|s| s.grants(t3_core::auth::RELAY_READ_SCOPE)),
                session.is_some_and(|s| s.grants(t3_core::auth::RELAY_WRITE_SCOPE)),
            )
        };
        let (terminal_access, can_read_relay, can_write_relay) =
            read_grants(grants.borrow_and_update().as_ref());
        self.terminal_access = terminal_access;
        self.can_read_relay = can_read_relay;
        self.can_write_relay = can_write_relay;
        self.terminal_grants = Some((
            connection,
            cx.spawn(async move |this, cx| {
                while grants.changed().await.is_ok() {
                    let (access, relay_read, relay_write) =
                        read_grants(grants.borrow_and_update().as_ref());
                    if this
                        .update(cx, |this, cx| {
                            this.terminal_access = access;
                            this.can_read_relay = relay_read;
                            this.can_write_relay = relay_write;
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
        let Some(provider) = config.providers.iter().find(|provider| provider.instance_id == instance_id) else { return };
        let row = AgentRow::for_driver(&provider.driver, Some(provider), Some(config));
        let Some(command) = row.command else { return };
        let (cwd, runtime) = (config.cwd.clone(), app.runtime().clone());
        let instance_id = instance_id.to_owned();
        let session = t3_core::onboarding::terminal::Session {
            driver: provider.driver.clone(),
            command,
            cwd,
            provider_instance_id: instance_id.clone(),
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
            instance_id,
            terminal,
            _closed: closed,
            _finished: Some(finished),
        });
        cx.notify();
    }

    pub(super) fn render_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let (machine, ready_count, mut primary_rows, more_rows, can_add, existing, registry) = {
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
                    .map(|provider| AgentRow::for_driver(&provider.driver, Some(provider), config))
                    .collect()
            };

            let more_rows: Vec<AgentRow> = grouped
                .available
                .iter()
                .map(|provider| AgentRow::for_driver(&provider.driver, Some(provider), config))
                .collect();

            if let Some(id) = &self.created_chatgpt {
                let row = primary_rows.iter().position(|r| r.instance_id.as_ref() == Some(id)).map(|index| primary_rows.remove(index));
                let mut row = row.unwrap_or_else(|| AgentRow::of(&OnboardingAgent { driver: "codex", provider: None }, config));
                row.instance_id = Some(id.clone());
                primary_rows.insert(0, row);
            }
            let can_add = providers.unwrap_or_default().iter().any(|p| p.driver == "codex" && agent_state(Some(p)) == AgentState::Ready);
            let existing = config.map(|c| c.settings.provider_instances.iter().filter(|(_, p)| t3_core::settings::providers::chatgpt::existing_chosen(p.config.as_ref())).map(|(id, _)| id.to_owned()).collect::<Vec<_>>()).unwrap_or_default();
            let registry = t3_core::onboarding::managed_driver_allowed(policy, model::ACP_REGISTRY);
            (machine, grouped.ready_count, primary_rows, more_rows, can_add, existing, registry)
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
            .when(registry, |section| section.child(
                h_flex().gap(r(8.)).child(Icon::PackagePlus.render(px(16.), hsla(p.icon_muted)))
                    .child(div().flex_1().min_w_0().text_sm().child(crate::i18n::t(cx, "More agents in the ACP Registry")))
                    .child(Button::new("welcome-acp-registry", ButtonVariant::Outline, ButtonSize::Xs)
                        .label("Browse")
                        .disabled(!self.can_manage_agents(cx))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.dismiss_for_settings(cx);
                            crate::settings::open_onboarding_registry(cx);
                        }))),
            ))
            .children(setup)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::answers_current;

    #[test]
    fn non_primary_agents_keep_their_driver_and_setup_actions() {
        use super::{AgentAction, AgentRow};
        let row = |enabled, installed, advisory| {
            let provider: t3_core::server_config::ServerProvider = serde_json::from_value(serde_json::json!({
                "driver": "pi", "instanceId": "pi-work", "enabled": enabled, "installed": installed,
                "status": "ready", "compatibilityAdvisory": advisory,
            })).unwrap();
            AgentRow::for_driver(&provider.driver, Some(&provider), None)
        };
        let disabled = row(false, true, serde_json::Value::Null);
        assert_eq!(disabled.driver, "pi");
        assert_eq!(disabled.action(), AgentAction::Enable);
        assert_eq!(row(true, false, serde_json::Value::Null).action(), AgentAction::Settings("Set up"));
        let advisory = serde_json::json!({"status": "broken", "targetVersion": "1.2.3"});
        assert_eq!(row(true, true, advisory).action(), AgentAction::Settings("Update"));
    }

    #[test]
    fn a_provider_reply_after_a_switch_is_dropped() {
        assert!(answers_current(Some(3), Some(3)));
        // Asked on A, answered after switching to B or disconnecting.
        assert!(!answers_current(Some(3), Some(4)));
        assert!(!answers_current(Some(3), None));
        assert!(!answers_current(None, None));
    }
}
