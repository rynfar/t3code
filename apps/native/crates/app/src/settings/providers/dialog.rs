//! Add provider (`AddProviderInstanceDialog.tsx`, `AddProviderInstanceWizardSteps.tsx`): pick a
//! driver or manual/local ACP, name the instance, and configure it with the shared forms.
//! Registry search and sign-in reuse the provider editor account controls.

use std::collections::HashMap;

use gpui::{
    AnyElement, AppContext, Context, Entity, FocusHandle, FontWeight, IntoElement, ParentElement,
    SharedString, Styled, Subscription, Window, div, point, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use t3_core::commands::method;
use t3_core::settings::providers::{self as model, Driver, wizard};

use super::super::SettingsView;
use super::super::controls::{Button, ButtonSize, ButtonVariant, Row, icon, r};
use super::{CloseProviderDialog, failure_text};
use crate::components::badge::{BadgeKind, badge};
use crate::icons::Icon;
use crate::provider_icon::{ProviderIcon, ProviderMark};
use crate::theme::{hsla, palette};

use wizard::AcpSetup;

#[derive(Clone)]
struct IdentityDraft {
    label: String,
    accent: String,
    instance_id: Option<String>,
}

pub(super) struct AddDialog {
    pub(super) token: u64,
    pub(super) connection: Option<u64>,
    pub(super) registry: super::registry::Registry,
    pub(super) selected_acp: Option<model::registry::Agent>,
    pub(super) prepared: Option<model::registry::Agent>,
    pub(super) step: usize,
    pub(super) acp: AcpSetup,
    pub(super) created: bool,
    pub(super) local_form: super::form::FormState,
    pub(super) focus_step: bool,
    pub(super) driver: &'static Driver,
    pub(super) show_more: bool,
    identities: HashMap<&'static str, IdentityDraft>,
    pub(super) configs: HashMap<&'static str, super::dialog_config::ConfigDraft>,
    pub(super) label: Entity<InputState>,
    pub(super) instance_id: Entity<InputState>,
    pub(super) accent: Entity<InputState>,
    /// The user typed their own id, so the label no longer drives it.
    pub(super) id_overridden: bool,
    pub(super) attempted: bool,
    pub(super) saving: bool,
    pub(super) error: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl AddDialog {
    pub(super) fn inputs(&self) -> Vec<&Entity<InputState>> {
        [&self.label, &self.instance_id, &self.accent, self.registry.input()]
            .into_iter()
            .chain(self.configs.values().flat_map(|draft| draft.inputs()))
            .chain(self.local_form.inputs())
            .collect()
    }

    pub(super) fn driver(&self) -> &'static Driver {
        self.driver
    }
    pub(super) fn local_acp(&self) -> bool {
        self.driver.kind == model::ACP_REGISTRY && self.acp.local(self.configs.get(model::ACP_REGISTRY).and_then(|c| c.model.value.as_ref()))
    }
    pub(super) fn saving(&self) -> bool {
        self.saving
    }
}

impl SettingsView {
    pub(super) fn add_driver_denial(&self, driver: &Driver, cx: &gpui::App) -> Option<String> {
        let app = self.state.read(cx);
        t3_core::managed_policy::driver_denial(
            app.server_config
                .config
                .as_ref()
                .and_then(|c| c.managed_policy.as_ref()),
            driver.kind,
            driver.label,
        )
    }

    fn check_dialog_policy(&mut self, cx: &mut Context<Self>) -> bool {
        if self.providers.dialog.as_ref().is_none_or(|d|d.connection!=self.state.read(cx).connection_id()) { return false; }
        if !self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE, cx) {
            if let Some(dialog) = self.providers.dialog.as_mut() {
                dialog.error = Some("This connection is read-only.".into());
            }
            cx.notify();
            return false;
        }
        let denial = self
            .providers
            .dialog
            .as_ref()
            .and_then(|dialog| self.add_driver_denial(dialog.driver, cx));
        if let Some(denial) = denial {
            if let Some(dialog) = self.providers.dialog.as_mut() {
                dialog.error = Some(denial);
            }
            cx.notify();
            return false;
        }
        true
    }

    pub(super) fn open_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let driver = {
            let app = self.state.read(cx);
            let policy = app
                .server_config
                .config
                .as_ref()
                .and_then(|c| c.managed_policy.as_ref());
            model::offered_drivers(true, policy)
                .next()
                .or_else(|| model::offered_drivers(false, policy).next())
                .or_else(|| model::find_driver(model::ACP_REGISTRY).filter(|d| t3_core::managed_policy::driver_allowed(policy, d.kind)))
        };
        let Some(driver) = driver else {
            return;
        };
        let label = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Work"));
        let instance_id =
            cx.new(|cx| InputState::new(window, cx).placeholder(format!("{}_work", driver.kind)));
        let accent = cx.new(|cx| InputState::new(window, cx).placeholder("#2563eb"));
        let subscriptions =
            vec![
                cx.subscribe_in(&label, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::Change => this.sync_dialog_id(window, cx),
                        InputEvent::PressEnter { .. } => this.advance_dialog(window, cx),
                        _ => {}
                    }
                }),
                cx.subscribe_in(
                    &instance_id,
                    window,
                    |this, input, event: &InputEvent, window, cx| match event {
                        InputEvent::Change => {
                            let typed = input.read(cx).value().to_string();
                            let proposed = this.proposed_dialog_id(cx);
                            if let Some(dialog) = this.providers.dialog.as_mut() {
                                dialog.id_overridden = proposed.as_deref() != Some(typed.as_str());
                                cx.notify();
                            }
                        }
                        InputEvent::PressEnter { .. } => this.advance_dialog(window, cx),
                        _ => {}
                    },
                ),
                cx.subscribe_in(
                    &accent,
                    window,
                    |this, _, event: &InputEvent, window, cx| match event {
                        InputEvent::Change => cx.notify(),
                        InputEvent::PressEnter { .. } => this.advance_dialog(window, cx),
                        _ => {}
                    },
                ),
            ];
        let focus = cx.focus_handle();
        static NEXT_DIALOG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let registry = super::registry::Registry::new(window,cx);
        self.providers.dialog = Some(AddDialog {
            token: NEXT_DIALOG.fetch_add(1,std::sync::atomic::Ordering::Relaxed),
            connection: self.state.read(cx).connection_id(),
            registry,
            selected_acp:None,
            prepared:None,
            step: 0,
            acp: AcpSetup::Choices,
            created: false,
            local_form: Default::default(),
            focus_step: false,
            driver,
            show_more: false,
            identities: HashMap::new(),
            configs: HashMap::new(),
            label,
            instance_id,
            accent,
            id_overridden: false,
            attempted: false,
            saving: false,
            error: None,
            focus: focus.clone(),
            _subscriptions: subscriptions,
        });
        self.providers
            .dialog
            .as_ref()
            .unwrap()
            .label
            .update(cx, |input, cx| input.set_value(driver.label, window, cx));
        self.sync_dialog_id(window, cx);
        self.sync_dialog_config(window, cx);
        window.focus(&focus, cx);
        #[cfg(feature = "snapshot")]
        if let Some(fixture) = std::env::var_os("T3_NATIVE_PROVIDER_WIZARD_FIXTURE")
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok()) {
            self.apply_wizard_fixture(fixture,window,cx);
        }
        cx.notify();
    }

    fn close_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.providers.dialog.take().is_some() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Each driver starts from its own label, as the web keeps one identity draft per driver.
    pub(super) fn select_dialog_driver(
        &mut self,
        driver: &'static Driver,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.providers.dialog.as_ref().is_none_or(|d| d.saving || d.created || d.registry.model.preparing.is_some()) || !self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE, cx) || self.add_driver_denial(driver, cx).is_some() {
            return;
        }
        let Some(dialog) = self.providers.dialog.as_mut() else {
            return;
        };
        dialog.identities.insert(
            dialog.driver.kind,
            IdentityDraft {
                label: dialog.label.read(cx).value().to_string(),
                accent: dialog.accent.read(cx).value().to_string(),
                instance_id: dialog
                    .id_overridden
                    .then(|| dialog.instance_id.read(cx).value().to_string()),
            },
        );
        let draft = dialog
            .identities
            .get(driver.kind)
            .cloned()
            .unwrap_or_else(|| IdentityDraft {
                label: driver.label.into(),
                accent: String::new(),
                instance_id: None,
            });
        dialog.driver = driver;
        dialog.acp = AcpSetup::Choices;
        dialog.id_overridden = draft.instance_id.is_some();
        dialog.attempted = false;
        dialog.error = None;
        let (label, accent, id) = (
            dialog.label.clone(),
            dialog.accent.clone(),
            dialog.instance_id.clone(),
        );
        label.update(cx, |input, cx| input.set_value(draft.label, window, cx));
        accent.update(cx, |input, cx| input.set_value(draft.accent, window, cx));
        if let Some(value) = draft.instance_id {
            id.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let placeholder = format!("{}_work", driver.kind);
        if let Some(dialog) = self.providers.dialog.as_ref() {
            dialog.instance_id.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
        self.sync_dialog_id(window, cx);
        self.sync_dialog_config(window, cx);
        cx.notify();
    }

    pub(super) fn start_dialog_acp(&mut self, local: bool, window: &mut Window, cx: &mut Context<Self>) {
        let driver = model::find_driver(model::ACP_REGISTRY).unwrap();
        if self.providers.dialog.as_ref().is_none_or(|d| d.saving || d.created || d.registry.model.preparing.is_some()) || self.add_driver_denial(driver, cx).is_some() || !self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE, cx) || (local && self.form_denial(model::ACP_REGISTRY, "config.source", cx).is_some()) { return; }
        self.select_dialog_driver(driver, window, cx);
        let (mode, config, label) = AcpSetup::start(local);
        let dialog = self.providers.dialog.as_mut().unwrap();
        dialog.registry.stop();
        dialog.selected_acp = None;
        dialog.acp = mode;
        dialog.show_more = true;
        dialog.focus_step = true;
        dialog.id_overridden = false;
        dialog.configs.insert(model::ACP_REGISTRY, super::dialog_config::ConfigDraft { model: config, ..Default::default() });
        if local { dialog.local_form = Default::default(); }
        dialog.label.clone().update(cx, |input, cx| input.set_value(label, window, cx));
        self.sync_dialog_id(window, cx);
        self.sync_dialog_config(window, cx);
        cx.notify();
    }

    pub(super) fn existing_instance_ids(&self, cx: &gpui::App) -> Vec<String> {
        let app = self.state.read(cx);
        app.server_config
            .config
            .as_ref()
            .map(|config| model::reserved_instance_ids(&config.settings))
            .unwrap_or_default()
    }

    fn proposed_dialog_id(&self, cx: &gpui::App) -> Option<String> {
        let dialog = self.providers.dialog.as_ref()?;
        let label = dialog.label.read(cx).value().to_string();
        Some(model::proposed_instance_id(
            dialog.driver,
            &label,
            &self.existing_instance_ids(cx),
        ))
    }

    /// Keeps the id field on the label's proposal until the user types their own.
    fn sync_dialog_id(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(proposed) = self.proposed_dialog_id(cx) else {
            return;
        };
        let Some(dialog) = self.providers.dialog.as_ref() else {
            return;
        };
        if dialog.id_overridden || dialog.instance_id.read(cx).value().as_ref() == proposed {
            return;
        }
        let input = dialog.instance_id.clone();
        input.update(cx, |input, cx| input.set_value(proposed, window, cx));
        cx.notify();
    }

    fn dialog_id_error(&self, cx: &gpui::App) -> Option<String> {
        let dialog = self.providers.dialog.as_ref()?;
        let id = dialog.instance_id.read(cx).value().to_string();
        model::validate_instance_id(&id, &self.existing_instance_ids(cx))
    }

    /// Next, or Add instance on the last step. Moving past the identity step needs a valid id
    /// (`resolveWizardNavigation`).
    pub(super) fn advance_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.providers.dialog.as_ref().is_some_and(|d| d.created) { self.close_add_dialog(window, cx); return; }
        if !self.check_dialog_policy(cx) { return; }
        self.commit_focused_form_field(window, cx);
        let Some(step) = self
            .providers
            .dialog
            .as_ref()
            .filter(|dialog| !dialog.saving && dialog.registry.model.preparing.is_none())
            .map(|dialog| dialog.step)
        else {
            return;
        };
        let dialog = self.providers.dialog.as_ref().unwrap();
        if dialog.created { self.close_add_dialog(window, cx); return; }
        let config = dialog.configs.get(dialog.driver.kind).and_then(|c| c.model.value.as_ref());
        let last = dialog.acp.steps(dialog.driver.kind, config).len() - 1;
        if step < last && !(dialog.driver.kind == model::ACP_REGISTRY && step == 1) {
            self.navigate_dialog(step + 1, window, cx);
        } else {
            self.save_dialog(cx);
        }
    }

    fn navigate_dialog(&mut self, requested: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_focused_form_field(window, cx);
        let error = self.dialog_id_error(cx);
        let Some(dialog) = self.providers.dialog.as_mut().filter(|d| !d.saving && d.registry.model.preparing.is_none()) else {
            return;
        };
        if dialog.created { return; }
        let config = dialog.configs.get(dialog.driver.kind).and_then(|c| c.model.value.as_ref());
        match dialog.acp.navigate(dialog.driver.kind, config, dialog.step, requested, error) {
            Ok(2) if dialog.driver.kind == model::ACP_REGISTRY => { self.save_dialog(cx); return; }
            Ok(step) => dialog.step = step,
            Err((step, _)) => {
                dialog.step = step;
                dialog.attempted = true;
            }
        }
        dialog.focus.focus(window, cx);
        dialog.focus_step = true;
        cx.notify();
    }

    fn save_dialog(&mut self, cx: &mut Context<Self>) {
        if super::fixture_active(cx) {return;}
        if !self.check_dialog_policy(cx) {
            return;
        }
        let error = self.dialog_id_error(cx);
        let environment = self.dialog_environment(cx);
        let Some(dialog) = self.providers.dialog.as_mut() else {
            return;
        };
        if dialog.saving || dialog.created || dialog.registry.model.preparing.is_some() {
            return;
        }
        dialog.attempted = true;
        if error.is_some() {
            cx.notify();
            return;
        }
        let Some(config) = dialog.configs.get(dialog.driver.kind) else {
            return;
        };
        let local = dialog.acp.local(config.model.value.as_ref());
        if dialog.driver.kind == model::ACP_REGISTRY && let Some(error) = dialog.acp.selection_error(config.model.value.as_ref()) {
            dialog.step = 0;
            dialog.error = Some(error.into());
            cx.notify(); return;
        }
        if local && let Err(error) = &environment { dialog.error = Some((*error).into()); cx.notify(); return; }
        if config.model.error().is_some() {
            cx.notify();
            return;
        }
        let instance_id = dialog.instance_id.read(cx).value().to_string();
        let instance = wizard::instance(
            dialog.driver.kind,
            &dialog.label.read(cx).value(),
            &dialog.accent.read(cx).value(),
            config.model.value.clone(),
            environment.unwrap_or_default(),
        );
        let token = dialog.token;
        let connection = dialog.connection;
        dialog.registry.stop();
        dialog.saving = true;
        dialog.error = None;
        cx.notify();
        let (sent_at, task) = {
            let app = self.state.read(cx);
            (
                app.server_config.settings_revision(),
                app.rpc(
                    method::SERVER_UPDATE_SETTINGS,
                    model::create_payload(&instance_id, &instance),
                ),
            )
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.state.read(cx).connection_id() != connection || this.providers.dialog.as_ref().is_none_or(|d|d.token!=token) { return; }
                match result {
                    Ok(reply) => {
                        this.apply_settings_reply(reply, sent_at, cx);
                        this.providers.selected = Some(instance_id.clone());
                        if let Some(dialog) = this.providers.dialog.as_mut().filter(|d| d.driver.kind == model::ACP_REGISTRY && !local) {
                            dialog.saving = false;
                            dialog.created = true;
                            dialog.step = 2;
                            dialog.focus_step = true;
                        } else { this.providers.dialog = None; }
                    }
                    Err(error) => {
                        if let Some(dialog) = this.providers.dialog.as_mut() {
                            dialog.saving = false;
                            dialog.error = Some(format!(
                                "Could not add provider instance: {}",
                                failure_text(&error, "The settings update failed.")
                            ));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_add_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.apply_prepared_registry(window,cx);
        self.sync_registry(cx);
        let focus_step = std::mem::take(&mut self.providers.dialog.as_mut()?.focus_step);
        if focus_step && self.providers.dialog.as_ref()?.created { self.providers.dialog.as_ref()?.focus.focus(window, cx); }
        let target = if focus_step && !self.providers.dialog.as_ref()?.created {
            let dialog = self.providers.dialog.as_ref()?;
            let app = self.state.read(cx);
            wizard::focus_target(
                if dialog.step == 0 && dialog.driver.kind == model::ACP_REGISTRY && dialog.acp == AcpSetup::Manual { 2 } else { dialog.step },
                dialog.driver.kind,
                dialog
                    .configs
                    .get(dialog.driver.kind)
                    .and_then(|c| c.model.value.as_ref()),
                app.server_config
                    .config
                    .as_ref()
                    .and_then(|c| c.managed_policy.as_ref()),
                self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE, cx),
            )
        } else {
            wizard::FocusTarget::Dialog
        };
        if focus_step && target == wizard::FocusTarget::Identity {
            self.providers
                .dialog
                .as_ref()?
                .label
                .update(cx, |input, cx| input.focus(window, cx));
        }
        let dialog = self.providers.dialog.as_ref()?;
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let environment = {
            let app = self.state.read(cx);
            app.server_config
                .config
                .as_ref()
                .map(|config| config.environment.label.trim().to_owned())
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| "this environment".to_owned())
        };
        let step = dialog.step;
        let config = dialog.configs.get(dialog.driver.kind).and_then(|c| c.model.value.as_ref());
        let steps_names = dialog.acp.steps(dialog.driver.kind, config);
        let acp_error = (dialog.driver.kind == model::ACP_REGISTRY && dialog.attempted).then(|| dialog.acp.selection_error(config)).flatten();
        let acp_allowed = self.add_driver_denial(model::find_driver(model::ACP_REGISTRY).unwrap(), cx).is_none();
        let created = dialog.created;
        let driver = dialog.driver;
        let saving = dialog.saving || dialog.registry.model.preparing.is_some();
        let id_error = self.dialog_id_error(cx).filter(|_| dialog.attempted);
        let denial = self.add_driver_denial(driver, cx);

        let steps = h_flex()
            .gap(r(4.))
            .p(r(4.))
            .rounded(r(12.))
            .bg(if dark {
                gpui::white().opacity(0.04)
            } else {
                hsla(p.background)
            })
            .children(steps_names.iter().enumerate().map(|(index, name)| {
                let current = index == step;
                let done = index < step;
                let badge = div()
                    .size(r(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .map(|this| {
                        if done {
                            this.bg(hsla(p.primary))
                                .text_color(hsla(p.primary_foreground))
                                .child(icon(Icon::Check, 16., hsla(p.primary_foreground)))
                        } else if current {
                            this.bg(hsla(p.primary).opacity(0.1))
                                .text_color(hsla(p.primary))
                                .border_1()
                                .border_color(hsla(p.primary).opacity(0.3))
                                .child((index + 1).to_string())
                        } else {
                            this.bg(hsla(p.card))
                                .text_color(hsla(p.muted_foreground))
                                .border_1()
                                .border_color(hsla(p.border))
                                .child((index + 1).to_string())
                        }
                    });
                h_flex()
                    .id(("add-provider-step", index))
                    .flex_1()
                    .min_w_0()
                    .gap(r(8.))
                    .px(r(10.))
                    .py(r(8.))
                    .rounded(r(8.))
                    .when(current, |this| this.bg(hsla(p.card)))
                    .when(!saving && !created, |this| {
                        this.cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.navigate_dialog(index, window, cx)
                            }))
                    })
                    .child(badge)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if current {
                                hsla(p.foreground)
                            } else {
                                hsla(p.muted_foreground)
                            })
                            .child(if done && index == 0 {
                                let summary = if driver.kind == model::ACP_REGISTRY {
                                    if dialog.local_acp() {"Local ACP command".to_owned()} else {dialog.selected_acp.as_ref().map(|a|a.name.clone()).or_else(||config.and_then(|c|c["agentId"].as_str()).map(str::to_owned)).unwrap_or_default()}
                                } else {driver.label.to_owned()};
                                format!("{name}: {summary}")
                            } else if done && index == 1 {
                                let label=dialog.label.read(cx).value().to_string();
                                format!("{name}: {}",if label.trim().is_empty(){format!("{} Workspace",driver.label)}else{label.trim().to_owned()})
                            } else {(*name).to_owned()}),
                    )
            }));

        let registry_body = (step == 0 && acp_allowed && !(driver.kind == model::ACP_REGISTRY && dialog.acp == AcpSetup::Manual)).then(||self.render_registry(cx));
        let auth_body = created.then(||self.render_wizard_auth(&environment,window,cx));
        let config_body = (self.providers.dialog.as_ref()?.step == 2 && driver.kind != model::ACP_REGISTRY).then(|| {
            let key = match &target {
                wizard::FocusTarget::Config(key) => Some(key.as_str()),
                _ => None,
            };
            self.render_dialog_config(key, window, cx)
        });
        let acp_body = if self.providers.dialog.as_ref()?.step == 0 {
            let dialog = self.providers.dialog.as_ref()?;
            let manual = dialog.driver.kind == model::ACP_REGISTRY && dialog.acp == AcpSetup::Manual;
            let local = dialog.acp.local(dialog.configs.get(model::ACP_REGISTRY).and_then(|d| d.model.value.as_ref()));
            if manual {
                self.sync_dialog_config(window, cx);
                self.sync_dialog_local_form(window, cx);
                let key = match &target { wizard::FocusTarget::Config(k) => Some(k.as_str()), _ => None };
                let config = self.render_dialog_config(key, window, cx);
                let mut content = v_flex().gap(r(8.)).child(
                    h_flex().gap(r(8.)).child(div().flex_1().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(if local { "Local ACP command" } else { "Enter manually" }))
                    .child(Button::new("wizard-acp-choices", ButtonVariant::GhostMuted, ButtonSize::Compact).label("Search registry").disabled(saving).on_click(cx.listener(|this, _, window, cx| {
                        if let Some(d) = this.providers.dialog.as_mut().filter(|d| !d.saving && d.registry.model.preparing.is_none()) { d.acp = AcpSetup::Choices; d.attempted = false; d.error = None; d.focus.focus(window, cx); }
                        cx.notify();
                    })))
                ).child(div().text_xs().text_color(hsla(p.muted_foreground)).child(if local { format!("Run an installed ACP executable on {environment}.") } else { "Enter an official registry ID and any local executable or auth override.".into() })).child(config);
                if local { content = content.child(self.render_argument_controls(cx)).children(self.render_environment_controls(model::ACP_REGISTRY, cx)); }
                Some(content.into_any_element())
            } else { None }
        } else { None };
        let dialog = self.providers.dialog.as_ref()?;
        let body = if created {
            div().min_h(r(288.)).child(auth_body.unwrap()).into_any_element()
        } else if step == 0 {
            let app = self.state.read(cx);
            let policy = app
                .server_config
                .config
                .as_ref()
                .and_then(|c| c.managed_policy.as_ref());
            let featured: Vec<&'static Driver> = model::offered_drivers(true, policy).collect();
            let more: Vec<&'static Driver> = model::offered_drivers(false, policy).collect();
            v_flex()
                .gap(r(8.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .child("Provider"),
                )
                .child(self.render_driver_grid(&featured, driver, cx))
                .when(!more.is_empty() || acp_allowed, |this| {
                    let show_more = dialog.show_more;
                    this.child(
                        h_flex().pt(r(8.)).child(
                            Button::new(
                                "add-provider-more",
                                ButtonVariant::GhostMuted,
                                ButtonSize::Compact,
                            )
                            .label(if show_more { "Show less" } else { "Show more" })
                            .icon(Icon::ChevronDown)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(dialog) = this.providers.dialog.as_mut() {
                                    dialog.show_more = !dialog.show_more;
                                    cx.notify();
                                }
                            })),
                        ),
                    )
                    .when(show_more, |this| {
                        this.child(
                            div()
                                .pt(r(8.))
                                .child(self.render_driver_grid(&more, driver, cx))
                                .when(acp_allowed, |v| v.child(div().py(r(12.)).text_xs().text_color(hsla(p.muted_foreground)).child("Or choose from ACP Registry")))
                                .children(acp_body)
                                .children(registry_body)
                        )
                    })
                })
                .into_any_element()
        } else if let Some(config) = config_body {
            config
        } else {
            let accent = model::accent_rgba(&dialog.accent.read(cx).value());
            let swatch = div().size(r(16.)).flex_none().rounded_full().border_1().map(|this| match accent {
                Some(accent) => this.border_color(gpui::rgba(accent)).bg(gpui::rgba(accent)),
                None => this.border_color(hsla(p.border)),
            });
            v_flex()
                .children(dialog.selected_acp.as_ref().filter(|_|driver.kind==model::ACP_REGISTRY).map(|agent|v_flex().pb(r(12.)).child(div().text_sm().font_weight(FontWeight::MEDIUM).child(agent.name.clone())).child(div().text_xs().text_color(hsla(p.muted_foreground)).child(format!("v{} · {}",agent.version,agent.distribution)))))
                .child(Row::new("add-provider-label", "Label").description("Shown in the provider list.").control(div().w(r(224.)).child(Input::new(&dialog.label).small().disabled(saving || !self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE, cx)))))
                .child(
                    div().border_t_1().border_color(hsla(p.border).opacity(0.5)).child(
                        Row::new("add-provider-instance-id", "Instance ID")
                            .description("Letters, digits, '-', or '_'.")
                            .control(div().w(r(224.)).child(Input::new(&dialog.instance_id).small().disabled(saving || !self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE, cx))))
                            .when_some(id_error, |row, error| row.below(div().pb(r(8.)).text_xs().text_color(hsla(p.destructive_foreground)).child(error))),
                    ),
                )
                .child(
                    div().border_t_1().border_color(hsla(p.border).opacity(0.5)).child(
                        Row::new("add-provider-accent", "Accent color")
                            .description("Optional marker shown in the picker.")
                            .control(h_flex().gap(r(8.)).child(swatch).child(div().w(r(112.)).child(Input::new(&dialog.accent).small().disabled(saving || !self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE, cx))))),
                    ),
                )
                .into_any_element()
        };

        let auth_state = self.providers.auth.as_ref().and_then(|s|s.state.as_ref());
        let active_auth = created && auth_state.is_some_and(|s|s.is_active());
        let signed_in = created && model::registry::signed_in(self.state.read(cx).server_config.config.as_ref().and_then(|c|c.provider(dialog.instance_id.read(cx).value().as_ref())),auth_state);
        let back: SharedString = if step == 0 {
            "Cancel".into()
        } else {
            "Back".into()
        };
        let primary: SharedString = if created { if signed_in {"Done".into()} else {"Skip for now".into()} } else if step + 1 < steps_names.len() && !(driver.kind == model::ACP_REGISTRY && step == 1) {
            if step == 0 && driver.kind == "codex" {
                "Configure manually".into()
            } else {
                "Next".into()
            }
        } else if saving {
            "Adding...".into()
        } else if driver.kind == model::ACP_REGISTRY && !dialog.local_acp() {
            "Continue to sign-in".into()
        } else {
            "Add instance".into()
        };
        let footer = h_flex()
            .id("add-provider-footer")
            .h(r(60.))
            .flex_none()
            .justify_end()
            .gap(r(8.))
            .px(r(24.))
            .py(r(16.))
            .when(!created, |footer|footer.child(
                Button::new(
                    "add-provider-back",
                    if step == 0 {
                        ButtonVariant::GhostMuted
                    } else {
                        ButtonVariant::Outline
                    },
                    ButtonSize::Sm,
                )
                .label(back)
                .disabled(saving || created)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if step == 0 {
                        this.close_add_dialog(window, cx);
                    } else {
                        this.navigate_dialog(step - 1, window, cx);
                    }
                })),
            ))
            .child(
                Button::new(
                    "add-provider-primary",
                    if (created && !signed_in) || (step == 0 && driver.kind == "codex") {
                        ButtonVariant::Outline
                    } else {
                        ButtonVariant::Default
                    },
                    ButtonSize::Sm,
                )
                .label(primary)
                .disabled(
                    saving || active_auth
                        || (!created && (denial.is_some()
                        || !self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE, cx))),
                )
                .on_click(cx.listener(|this, _, window, cx| this.advance_dialog(window, cx))),
            )
            .when(
                step == 0
                    && driver.kind == "codex"
                    && self.form_denial("codex", "config.setupMode", cx).is_none(),
                |v| {
                    v.child(
                        Button::new(
                            "add-provider-chatgpt",
                            ButtonVariant::Default,
                            ButtonSize::Sm,
                        )
                        .icon(Icon::OpenAi)
                        .label("Continue with ChatGPT")
                        .on_click(cx.listener(|this, _, window, cx| {
                            let instance = this
                                .state
                                .read(cx)
                                .server_config
                                .config
                                .as_ref()
                                .and_then(|c| c.providers.iter().find(|p| p.driver == "codex"))
                                .map(|p| p.instance_id.clone())
                                .unwrap_or_else(|| "codex".into());
                            let view = cx.new(|cx| {
                                crate::chatgpt_setup::ChatGptSetup::new(
                                    this.state.clone(),
                                    instance.clone(),
                                    false,
                                    window,
                                    cx,
                                )
                            });
                            view.update(cx, |view, cx| view.open_add(cx));
                            this.providers.selected = Some(instance);
                            this.providers.chatgpt =
                                Some((this.state.read(cx).connection_id(), view));
                            this.close_add_dialog(window, cx);
                        })),
                    )
                },
            );

        let viewport = window.viewport_size();
        let popup = v_flex()
            .id("add-provider-dialog")
            .key_context("ProviderDialog")
            .track_focus(&dialog.focus)
            .on_action(cx.listener(|this, _: &CloseProviderDialog, window, cx| this.close_add_dialog(window, cx)))
            .occlude()
            .w_full()
            .max_w(r(768.))
            .max_h(px(f32::from(viewport.height) - 32.))
            .when(step == 0 && driver.kind == model::ACP_REGISTRY && dialog.acp == AcpSetup::Manual && dialog.show_more, |el| el.h(px(f32::from(viewport.height) - 32.)))
            .relative()
            .rounded(r(16.))
            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::black().opacity(0.65),
                offset: point(px(0.), px(24.)),
                blur_radius: px(64.),
                spread_radius: px(-24.),
                inset: false,
            }])
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .flex_none()
                    .p(r(24.))
                    .gap(r(8.))
                    .child(div().text_xl().line_height(r(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child("Add provider"))
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(format!("Add an account or configure a provider on {environment}.")))
                    .child(div().pt(r(8.)).child(steps)),
            )
            .child(div().id("add-provider-panel").flex_1().min_h_0().overflow_y_scroll().px(r(24.)).pt(r(4.)).pb(r(24.)).child(body)
                .children(denial.or_else(|| dialog.error.clone()).or_else(|| acp_error.map(str::to_owned)).map(|error| div().pt(r(8.)).text_xs().text_color(hsla(p.destructive_foreground)).child(error))))
            .child(footer);

        let overlay = div()
            .id("add-provider-backdrop")
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.))
            .bg(hsla(p.dialog_backdrop))
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, window, cx| this.close_add_dialog(window, cx)))
            .child(popup);
        Some(
            gpui::deferred(gpui::anchored().position_mode(gpui::AnchoredPositionMode::Window).position(point(px(0.), px(0.))).child(overlay))
                .with_priority(3)
                .into_any_element(),
        )
    }

    fn render_driver_grid(&self, drivers: &[&'static Driver], current: &'static Driver, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let cards: Vec<AnyElement> = drivers
            .iter()
            .map(|driver| {
                let driver: &'static Driver = driver;
                let selected = driver.kind == current.kind;
                h_flex()
                    .id(SharedString::from(format!("add-provider-driver-{}", driver.kind)))
                    .w_full()
                    .gap(r(12.))
                    .px(r(12.))
                    .py(r(12.))
                    .rounded(r(8.))
                    .cursor_pointer()
                    .border_1()
                    .map(|this| {
                        if selected {
                            this.border_color(hsla(p.primary)).bg(hsla(p.primary).opacity(if dark { 0.15 } else { 0.08 }))
                        } else {
                            this.border_color(if dark { gpui::white().opacity(0.05) } else { gpui::black().opacity(0.05) })
                                .bg(if dark { gpui::white().opacity(0.03) } else { hsla(p.card) })
                                .hover(|this| this.bg(hsla(p.muted)))
                        }
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.select_dialog_driver(driver, window, cx)))
                    .child(ProviderIcon::new(ProviderMark::configured(driver.kind, driver.label, None)).size(px(16.)))
                    .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(driver.label))
                    .when(selected, |this| {
                        this.child(div().size(r(20.)).flex_none().flex().items_center().justify_center().rounded_full().bg(hsla(p.primary)).child(icon(Icon::Check, 14., hsla(p.primary_foreground))))
                    })
                    .children(driver.badge_label.map(|label| badge(label, BadgeKind::Warning, false, cx)))
                    .into_any_element()
            })
            .collect();
        let mut rows = v_flex().gap(r(8.));
        let mut cards = cards.into_iter();
        while let Some(first) = cards.next() {
            let second = cards.next();
            rows = rows.child(h_flex().gap(r(8.)).child(div().flex_1().min_w_0().child(first)).child(div().flex_1().min_w_0().children(second)));
        }
        rows.into_any_element()
    }
}
