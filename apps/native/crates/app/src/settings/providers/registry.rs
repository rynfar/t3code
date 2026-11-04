//! `AcpRegistrySearchStep.tsx`: dialog-owned, cancellable RPCs and a bounded virtual results list.
use super::super::{
    SettingsView,
    controls::{Button, ButtonSize, ButtonVariant, Row, r},
};
use super::{AbortOnDrop, Stream, dialog::AddDialog, dialog_config::ConfigDraft};
use crate::{
    icons::Icon,
    provider_icon::{ProviderIcon, ProviderMark},
    theme::{hsla, palette},
};
use gpui::{
    AnyElement, AppContext, Context, Entity, FontWeight, IntoElement, ListAlignment, ListState,
    ParentElement, Styled, Subscription, Task, Window, div, list, px,
};
use gpui_component::{
    Sizable, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use std::time::Duration;
use t3_core::settings::providers::{
    self as model, registry as logic,
    wizard::{self, AcpSetup},
};

pub(super) struct Registry {
    pub model: logic::Search,
    input: Entity<InputState>,
    list: ListState,
    enabled: bool,
    debounce: Option<Task<()>>,
    work: Option<Stream>,
    _subscription: Subscription,
}
impl Registry {
    pub fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search agents…"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if !this.registry_visible(cx) {
                        return;
                    }
                    let dialog = this.providers.dialog.as_mut().unwrap();
                    if dialog.registry.input != *input {
                        return;
                    }
                    let token = dialog.token;
                    if dialog.registry.model.preparing.is_some() {
                        return;
                    }
                    let text = input.read(cx).value().to_string();
                    if dialog.registry.model.query == text {
                        return;
                    }
                    dialog.registry.model.typed(text);
                    dialog.registry.debounce = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(logic::DEBOUNCE_MS))
                            .await;
                        let _ = this.update(cx, |this, cx| {
                            if this
                                .providers
                                .dialog
                                .as_ref()
                                .is_some_and(|d| d.token == token)
                            {
                                this.search_registry(cx);
                            }
                        });
                    }));
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.submit_registry(window, cx),
                _ => {}
            },
        );
        Self {
            model: Default::default(),
            input,
            list: ListState::new(0, ListAlignment::Top, px(128.)),
            enabled: false,
            debounce: None,
            work: None,
            _subscription: subscription,
        }
    }
    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }
    pub fn stop(&mut self) {
        self.enabled = false;
        self.debounce = None;
        self.work = None;
        self.model.retire();
    }
}
impl AddDialog {
    pub(super) fn registry_step(&self) -> bool {
        self.step == 0
            && !self.created
            && !(self.driver.kind == model::ACP_REGISTRY && self.acp == AcpSetup::Manual)
    }
}
impl SettingsView {
    fn registry_visible(&self, cx: &gpui::App) -> bool {
        if self
            .add_driver_denial(model::find_driver(model::ACP_REGISTRY).unwrap(), cx)
            .is_some()
        {
            return false;
        }
        self.providers.dialog.as_ref().is_some_and(|d| {
            d.registry_step() && d.connection == self.state.read(cx).connection_id()
        })
    }
    pub(super) fn sync_registry(&mut self, cx: &mut Context<Self>) {
        let visible = self.registry_visible(cx);
        let Some(dialog) = self.providers.dialog.as_mut() else {
            return;
        };
        if !visible {
            if dialog.registry.enabled {
                dialog.registry.stop();
            }
            return;
        }
        if !dialog.registry.enabled {
            dialog.registry.enabled = true;
            self.search_registry(cx);
        }
    }
    fn registry_allowed(&self, cx: &gpui::App) -> bool {
        self.registry_visible(cx)
            && self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE, cx)
            && self
                .add_driver_denial(model::find_driver(model::ACP_REGISTRY).unwrap(), cx)
                .is_none()
    }
    fn registry_configured(&self, id: &str, cx: &gpui::App) -> bool {
        self.state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .is_some_and(|c| {
                logic::configured(c.settings.provider_instances.iter().map(|(_, i)| i), id)
            })
    }
    fn submit_registry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.registry_visible(cx) {
            return;
        }
        let d = self.providers.dialog.as_mut().unwrap();
        if d.registry.model.preparing.is_some() {
            return;
        }
        let query = d.registry.model.query.trim().to_owned();
        d.registry.model.query = query.clone();
        d.registry
            .input
            .update(cx, |input, cx| input.set_value(query, window, cx));
        self.search_registry(cx);
    }
    fn search_registry(&mut self, cx: &mut Context<Self>) {
        if !self.registry_visible(cx) || super::fixture_active(cx) {
            return;
        }
        let (client, runtime) = {
            let app = self.state.read(cx);
            let Some(client) = app.client_handle() else {
                return;
            };
            (client, app.runtime().clone())
        };
        let dialog = self.providers.dialog.as_mut().unwrap();
        if dialog.registry.model.preparing.is_some() {
            return;
        }
        dialog.registry.debounce = None;
        dialog.registry.work = None;
        let epoch = dialog.registry.model.begin_search();
        let query = dialog.registry.model.submitted.clone();
        let token = dialog.token;
        let connection = dialog.connection;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let driver = runtime.spawn(async move {
            let _ = tx.send(t3_client::acp_registry::search(client, &query).await);
        });
        let pump = cx.spawn(async move |this, cx| {
            let Ok(result) = rx.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if !this.registry_visible(cx) {
                    return;
                }
                if let Some(d) = this
                    .providers
                    .dialog
                    .as_mut()
                    .filter(|d| d.token == token && d.connection == connection)
                    && d.registry.model.searched(epoch, result)
                {
                    d.registry
                        .list
                        .reset(d.registry.model.agents.as_ref().map_or(0, Vec::len));
                    cx.notify();
                }
            });
        });
        self.providers.dialog.as_mut().unwrap().registry.work = Some(Stream {
            _driver: AbortOnDrop(driver),
            _pump: pump,
        });
        cx.notify();
    }
    fn prepare_registry(&mut self, agent: logic::Agent, cx: &mut Context<Self>) {
        if !self.registry_allowed(cx)
            || self.registry_configured(&agent.id, cx)
            || super::fixture_active(cx)
        {
            return;
        }
        let (client, runtime) = {
            let app = self.state.read(cx);
            let Some(client) = app.client_handle() else {
                return;
            };
            (client, app.runtime().clone())
        };
        let d = self.providers.dialog.as_mut().unwrap();
        let Some(epoch) = d.registry.model.begin_prepare(&agent.id) else {
            return;
        };
        d.registry.debounce = None;
        d.registry.work = None;
        let (token, connection) = (d.token, d.connection);
        let request_agent = agent.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let driver = runtime.spawn(async move {
            let _ = tx.send(t3_client::acp_registry::prepare(client, &request_agent).await);
        });
        let pump = cx.spawn(async move |this, cx| {
            let Ok(result) = rx.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if !this.registry_allowed(cx) {
                    if let Some(d) = this.providers.dialog.as_mut().filter(|d| d.token == token) {
                        d.registry.stop();
                        cx.notify();
                    }
                    return;
                }
                let selected = this
                    .providers
                    .dialog
                    .as_mut()
                    .filter(|d| d.token == token && d.connection == connection)
                    .and_then(|d| d.registry.model.prepared(epoch, agent, result));
                if let Some(agent) = selected
                    && !this.registry_configured(&agent.id, cx)
                {
                    // Input entity updates need a Window; apply the prepared draft on the next render.
                    this.providers.dialog.as_mut().unwrap().prepared = Some(agent);
                }
                cx.notify();
            });
        });
        self.providers.dialog.as_mut().unwrap().registry.work = Some(Stream {
            _driver: AbortOnDrop(driver),
            _pump: pump,
        });
        cx.notify();
    }
    pub(super) fn apply_prepared_registry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self
            .providers
            .dialog
            .as_mut()
            .and_then(|d| d.prepared.take());
        let Some(agent) =
            agent.filter(|a| self.registry_allowed(cx) && !self.registry_configured(&a.id, cx))
        else {
            return;
        };
        self.select_dialog_driver(model::find_driver(model::ACP_REGISTRY).unwrap(), window, cx);
        let id = logic::instance_id(&agent.name, &self.existing_instance_ids(cx));
        let d = self.providers.dialog.as_mut().unwrap();
        d.acp = AcpSetup::Registry;
        d.configs.insert(
            model::ACP_REGISTRY,
            ConfigDraft {
                model: wizard::ConfigDraft::from_value(agent.config()),
                ..Default::default()
            },
        );
        d.label.update(cx, |input, cx| {
            input.set_value(agent.name.clone(), window, cx)
        });
        d.instance_id
            .update(cx, |input, cx| input.set_value(id, window, cx));
        d.id_overridden = true;
        d.selected_acp = Some(agent);
        d.step = 1;
        d.focus_step = true;
        d.registry.stop();
    }
    pub(super) fn render_registry(&self, cx: &mut Context<Self>) -> AnyElement {
        let d = self.providers.dialog.as_ref().unwrap();
        let search = &d.registry.model;
        let preparing = search.preparing.is_some();
        let p = palette(cx);
        let mut content = v_flex().gap(r(12.)).child(
            h_flex()
                .gap(r(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w(r(160.))
                        .child(Input::new(&d.registry.input).small().disabled(preparing)),
                )
                .child(
                    Button::new(
                        "registry-refresh",
                        ButtonVariant::GhostMuted,
                        ButtonSize::IconSm,
                    )
                    .icon(Icon::RefreshCw)
                    .tooltip("Refresh search")
                    .disabled(preparing)
                    .on_click(cx.listener(|this, _, window, cx| this.submit_registry(window, cx))),
                )
                .child(
                    Button::new(
                        "wizard-manual-acp",
                        ButtonVariant::GhostMuted,
                        ButtonSize::Sm,
                    )
                    .label("Enter manually")
                    .disabled(preparing || !self.registry_allowed(cx))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.start_dialog_acp(false, window, cx)),
                    ),
                )
                .child(
                    Button::new("wizard-local-acp", ButtonVariant::Outline, ButtonSize::Sm)
                        .label("Local ACP command")
                        .disabled(
                            preparing
                                || self
                                    .form_denial(model::ACP_REGISTRY, "config.source", cx)
                                    .is_some(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.start_dialog_acp(true, window, cx)
                        })),
                ),
        );
        if let Some(error) = search.prepare_error.as_ref().or(search.error.as_ref()) {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(hsla(p.destructive_foreground))
                    .child(error.clone()),
            );
        }
        if search.pending {
            content = content.child(div().text_sm().text_color(hsla(p.muted_foreground)).child(
                if search.agents.is_some() {
                    "Refreshing ACP Registry results."
                } else {
                    "Searching the registry..."
                },
            ));
        }
        if let Some(agents) = &search.agents {
            if agents.is_empty() {
                content = content.child(
                    v_flex()
                        .min_h(r(112.))
                        .justify_center()
                        .items_center()
                        .border_1()
                        .border_color(hsla(p.border))
                        .rounded(r(16.))
                        .child(div().text_sm().child("No compatible agents found"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child("Try a broader search."),
                        ),
                );
            } else {
                content = content.child(
                    list(
                        d.registry.list.clone(),
                        cx.processor(|this, index, _, cx| this.render_registry_agent(index, cx)),
                    )
                    .h(r((agents.len() as f32 * 64.).min(256.)))
                    .w_full(),
                );
            }
        }
        content.into_any_element()
    }
    fn render_registry_agent(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let d = self.providers.dialog.as_ref().unwrap();
        let Some(agent) = d
            .registry
            .model
            .agents
            .as_ref()
            .and_then(|a| a.get(index))
            .cloned()
        else {
            return div().into_any_element();
        };
        let p = palette(cx);
        let added = self.registry_configured(&agent.id, cx);
        let preparing = d.registry.model.preparing.as_deref() == Some(&agent.id);
        let label = if added {
            "Added"
        } else if preparing && agent.distribution == "binary" {
            "Downloading"
        } else if preparing {
            "Preparing"
        } else {
            "Add"
        };
        let about = agent.website.clone().or(agent.repository.clone());
        h_flex()
            .w_full()
            .h(r(64.))
            .gap(r(12.))
            .py(r(10.))
            .child(
                ProviderIcon::new(ProviderMark::configured(
                    model::ACP_REGISTRY,
                    &agent.name,
                    None,
                ))
                .size(px(
                    24. * crate::theme::preferences(cx).interface_font_size / 16.
                )),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(r(2.))
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child(agent.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(agent.description.clone()),
                    ),
            )
            .children(about.map(|url| {
                Button::new(
                    ("registry-about", index),
                    ButtonVariant::GhostMuted,
                    ButtonSize::IconXs,
                )
                .icon(Icon::ExternalLink)
                .tooltip(format!("About {}", agent.name))
                .on_click(move |_, _, cx| {
                    if !super::fixture_active(cx) {
                        cx.open_url(&url);
                    }
                })
            }))
            .child(
                Button::new(
                    ("registry-add", index),
                    ButtonVariant::Outline,
                    ButtonSize::Xs,
                )
                .label(label)
                .disabled(
                    added || d.registry.model.preparing.is_some() || !self.registry_allowed(cx),
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| this.prepare_registry(agent.clone(), cx)),
                ),
            )
            .into_any_element()
    }
    pub(super) fn render_wizard_auth(
        &mut self,
        environment: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = self
            .providers
            .dialog
            .as_ref()
            .unwrap()
            .instance_id
            .read(cx)
            .value()
            .to_string();
        let provider = self
            .state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .and_then(|c| c.provider(&id))
            .cloned();
        let session = self.providers.auth.as_ref().filter(|s| s.instance_id == id);
        let auth = session.and_then(|s| s.state.as_ref());
        let signed = logic::signed_in(provider.as_ref(), auth);
        if let Some(mut provider) = provider.clone().filter(|_| {
            signed || auth.is_some_and(|a| a.methods.as_ref().is_some_and(|m| !m.is_empty()))
        }) {
            let setup = provider.setup.get_or_insert_with(Default::default);
            setup.can_authenticate =
                auth.is_some_and(|a| a.methods.as_ref().is_some_and(|m| !m.is_empty()));
            let selected = self.page_data(cx).and_then(|p| p.selected);
            if let Some(selected) = selected.filter(|s| s.row.instance_id == id) {
                return self.render_account(&selected, &provider, environment, window, cx);
            }
        }
        let error = session.and_then(|s| s.load_error.as_deref());
        let description = if logic::discovering(signed, auth, error) {
            "Discovering sign-in methods…"
        } else {
            error
                .or_else(|| auth.and_then(|a| a.message.as_deref()))
                .unwrap_or(
                    "No in-app sign-in advertised. Follow the provider's docs to finish setup.",
                )
        };
        let mut row = Row::new("wizard-account", "Account").description(description.to_owned());
        if logic::discovering(signed, auth, error) {
            row = row.control(
                Button::new("wizard-discover", ButtonVariant::Outline, ButtonSize::Sm)
                    .label("Sign in")
                    .disabled(true),
            );
        } else if let Some(url) = provider
            .and_then(|p| p.setup)
            .and_then(|s| s.documentation_url)
        {
            row = row.control(
                Button::new("wizard-docs", ButtonVariant::Outline, ButtonSize::Sm)
                    .label("Open docs")
                    .on_click(move |_, _, cx| {
                        if !super::fixture_active(cx) {
                            cx.open_url(&url);
                        }
                    }),
            );
        }
        row.into_any_element()
    }
}

#[cfg(feature = "snapshot")]
impl SettingsView {
    pub(super) fn apply_wizard_fixture(
        &mut self,
        fixture: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use serde_json::json;
        if fixture.get("registry").is_some() {
            let d = self.providers.dialog.as_mut().unwrap();
            d.show_more = true;
            d.registry.enabled = true;
            if let Ok(results) = serde_json::from_value(fixture["registry"].clone()) {
                let epoch = d.registry.model.begin_search();
                d.registry.model.searched(epoch, Ok(results));
                d.registry
                    .list
                    .reset(d.registry.model.agents.as_ref().map_or(0, Vec::len));
            }
            d.registry.model.pending = fixture["pending"] == true;
            d.registry.model.error = fixture["error"].as_str().map(str::to_owned);
            if let Some(id) = fixture["preparing"].as_str() {
                d.registry.model.begin_prepare(id);
            }
            if let Ok(selected) = serde_json::from_value(fixture["selected"].clone()) {
                d.prepared = Some(selected);
            }
            self.apply_prepared_registry(window, cx);
        } else {
            self.start_dialog_acp(fixture["local"] == true, window, cx);
            if let Some(config) = fixture.get("config") {
                self.providers.dialog.as_mut().unwrap().configs.insert(
                    model::ACP_REGISTRY,
                    ConfigDraft {
                        model: wizard::ConfigDraft::from_value(config.clone()),
                        ..Default::default()
                    },
                );
            }
            self.sync_dialog_config(window, cx);
            if let Some(rows) = fixture["environment"].as_array() {
                for row in rows {
                    self.add_variable_input(row.clone(), window, cx);
                }
            }
        }
        if let Some(step) = fixture["step"].as_u64() {
            self.providers.dialog.as_mut().unwrap().step = step as usize;
        }
        if let Some(provider) = fixture.get("provider").and_then(|v| {
            serde_json::from_value::<t3_core::server_config::ServerProvider>(v.clone()).ok()
        }) {
            let id = provider.instance_id.clone();
            let label = provider
                .display_name
                .clone()
                .unwrap_or_else(|| "Fixture Agent".into());
            self.state.update(cx, |app, _| {
                if let Some(config) = app.server_config.config.as_mut() {
                    config.providers.retain(|p| p.instance_id != id);
                    config.providers.push(provider);
                    let mut instances = config
                        .settings
                        .provider_instances
                        .iter()
                        .map(|(id, i)| (id.to_owned(), i.clone()))
                        .collect::<Vec<_>>();
                    instances.push((
                        id.clone(),
                        t3_core::server_config::ProviderInstanceConfig {
                            driver: model::ACP_REGISTRY.into(),
                            display_name: Some(label.clone()),
                            config: Some(json!({"agentId":"fixture"})),
                            ..Default::default()
                        },
                    ));
                    config.settings.provider_instances = instances.into_iter().collect();
                }
            });
            self.providers.selected = Some(id.clone());
            let d = self.providers.dialog.as_mut().unwrap();
            d.driver = model::find_driver(model::ACP_REGISTRY).unwrap();
            d.created = true;
            d.step = 2;
            d.instance_id
                .update(cx, |input, cx| input.set_value(id.clone(), window, cx));
            self.providers.auth = Some(super::AuthSession {
                instance_id: id,
                state: model::auth::decode_auth_state(fixture["auth"].clone()),
                load_error: fixture["authError"].as_str().map(str::to_owned),
                pending: false,
                error: None,
                _stream: None,
            });
        }
    }
}
