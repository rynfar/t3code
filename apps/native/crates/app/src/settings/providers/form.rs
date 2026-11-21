//! Runtime and environment controls from `ProviderSettingsForm.tsx` and `ProviderInstanceCard.tsx`.
use super::super::{
    SettingsView,
    controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, r, section_title},
};
use super::{Selected, flat_section};
use crate::{
    icons::Icon,
    theme::{hsla, palette},
};
use std::collections::HashMap;

use gpui::{AnyElement, App, AppContext, Context, ElementId, Entity, Focusable, IntoElement, ParentElement, SharedString, Styled, Subscription, Window, div, prelude::*};
use gpui_component::{
    Sizable, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use serde_json::{Value, json};
use t3_core::{
    device::SETTINGS_WRITE_SCOPE,
    settings::{
        ServerSettingChange,
        providers::{self as model, form},
        secrets,
    },
    usage::PROVIDERS_MANAGE_SCOPE,
};

struct FieldInput {
    key: String,
    placeholder: String,
    masked: bool,
    last_saved: Option<Value>,
    source: form::SecretSourceDraft,
    input: Entity<InputState>,
    _subscription: Subscription,
}
struct VariableInput {
    id: usize,
    source: form::SecretSourceDraft,
    raw: Value,
    name: Entity<InputState>,
    value: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}
struct ArgumentInput {
    id: usize,
    input: Entity<InputState>,
    _subscription: Subscription,
}
#[derive(Clone, PartialEq, Eq)]
enum SecretCheckStatus {
    Checking,
    Resolved,
    Failed(String),
}
#[derive(Default)]
pub(super) struct FormState {
    instance_id: String,
    generation: usize,
    fields: Vec<FieldInput>,
    arguments: form::ArgumentsDraft,
    argument_inputs: Vec<ArgumentInput>,
    variables: Vec<VariableInput>,
    secret_checks: HashMap<(String, String), SecretCheckStatus>,
    seen: Vec<Value>,
    published: Option<Vec<Value>>,
    next_id: usize,
    runtime_open: bool,
    pub(super) runtime_observer: Option<Subscription>,
    error: Option<String>,
}
impl FormState {
    pub(super) fn reset(&mut self) {
        *self = Self {
            generation: self.generation + 1,
            ..Default::default()
        };
    }

    pub(super) fn inputs(&self) -> Vec<&Entity<InputState>> {
        self.fields
            .iter()
            .map(|f| &f.input)
            .chain(self.variables.iter().flat_map(|v| [&v.name, &v.value]))
            .chain(self.argument_inputs.iter().map(|a| &a.input))
            .collect()
    }
}
fn config_field_placeholder<'a>(stored: bool, secret: bool, saved_value: &Value, default_placeholder: &'a str) -> &'a str {
    if !stored {
        "op://vault/item/field"
    } else if secret && saved_value.as_str().is_some_and(|value| !value.is_empty()) {
        "Stored secret, enter a new value to replace"
    } else if secret {
        "Not set"
    } else {
        default_placeholder
    }
}

impl SettingsView {
    fn form_event_current(&self, generation: usize, cx: &App) -> bool {
        self.provider_form().generation == generation && self.current_form_instance(cx).is_some()
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn assert_form_queue_safety(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.provider_form().generation;
        assert!(self.form_event_current(generation, cx));
        self.providers.selected = Some("other".into());
        assert!(
            !self.form_event_current(generation, cx),
            "queued runtime/argument/environment blur cannot address a new selection before render"
        );
        let selected = self.page_data(cx).unwrap().selected.unwrap();
        self.sync_editor_inputs(Some(&selected), window, cx);
        self.sync_provider_form(&selected, window, cx);
        assert!(
            !self.form_event_current(generation, cx),
            "queued callbacks cannot read the replacement form's values"
        );
        self.providers.selected = Some("codex".into());
        let selected = self.page_data(cx).unwrap().selected.unwrap();
        self.sync_editor_inputs(Some(&selected), window, cx);
        self.sync_provider_form(&selected, window, cx);
        assert!(
            !self.form_event_current(generation, cx),
            "a selection round trip must not revive queued callbacks"
        );
    }

    fn provider_form(&self) -> &FormState {
        self.providers.dialog.as_ref().map_or(&self.providers.form, |d| &d.local_form)
    }
    fn provider_form_mut(&mut self) -> &mut FormState {
        self.providers.dialog.as_mut().map_or(&mut self.providers.form, |d| &mut d.local_form)
    }
    pub(super) fn dialog_environment(&self, cx: &App) -> Result<Vec<Value>, &'static str> {
        for variable in &self.provider_form().variables {
            if let Some(provider) = variable.source.provider(&variable.raw) {
                let one_password = self.state.read(cx).server_config.config.as_ref().and_then(|c| c.settings.secret_providers.get(provider)).is_some_and(|p| p.kind == t3_core::settings::secrets::ONE_PASSWORD);
                variable.source.validate_draft(&variable.raw, &variable.value.read(cx).value(), one_password)?;
            }
        }
        form::publish_environment("acpRegistry", self.provider_form().variables.iter().map(|v| v.raw.clone()).collect())
    }
    pub(super) fn sync_dialog_local_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(instance) = self.current_form_instance(cx) else { return };
        self.sync_argument_inputs(&instance, window, cx);
    }
    pub(super) fn commit_focused_form_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.provider_form().fields
            .iter()
            .filter(|f| f.input.focus_handle(cx).is_focused(window))
            .map(|f| f.key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            self.commit_config_field(&key, window, cx);
        }
        let arguments = self.provider_form().argument_inputs
            .iter()
            .filter(|a| a.input.focus_handle(cx).is_focused(window))
            .map(|a| a.id)
            .collect::<Vec<_>>();
        for id in arguments {
            self.commit_argument(id, cx);
        }
        let variables = self.provider_form().variables
            .iter()
            .flat_map(|v| {
                [(&v.name, false), (&v.value, true)]
                    .into_iter()
                    .filter(|(input, _)| input.focus_handle(cx).is_focused(window))
                    .map(|(_, value)| (v.id, value))
            })
            .collect::<Vec<_>>();
        for (id, value) in variables {
            self.commit_variable(id, value, window, cx);
        }
    }

    pub(super) fn form_denial(&self, driver: &str, path: &str, cx: &App) -> Option<String> {
        if !self.may(PROVIDERS_MANAGE_SCOPE, cx) {
            return Some("This connection is read-only.".into());
        }
        if self.providers.dialog.as_ref().is_some_and(|d| d.saving()) {
            return Some("Adding provider instance.".into());
        }
        let app = self.state.read(cx);
        let policy = app.server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref());
        if let Some(reason) = t3_core::managed_policy::driver_denial(policy, driver, driver) { return Some(reason); }
        policy
            .filter(|p| t3_core::managed_policy::instance_field_locked(Some(p), driver, path))
            .map(t3_core::managed_policy::instance_reason)
    }

    pub(super) fn sync_provider_form(&mut self, selected: &Selected, window: &mut Window, cx: &mut Context<Self>) {
        if self.providers.dialog.is_some() { return; }
        let instance = selected.row.instance();
        let switched = self.provider_form().instance_id != selected.row.instance_id;
        if switched {
            *self.provider_form_mut() = FormState {
                instance_id: selected.row.instance_id.clone(),
                generation: self.provider_form().generation + 1,
                ..Default::default()
            };
        }
        let managed = instance.driver == "codex" && model::chatgpt::managed(instance.config.as_ref());
        let executable = self
            .providers
            .chatgpt
            .as_ref()
            .and_then(|(_, view)| view.read(cx).state.install.as_ref())
            .and_then(|i| i.executable_path.as_deref());
        let failed = self.providers.chatgpt.as_ref().is_some_and(|(_, view)| view.read(cx).state.install_load_error);
        let runtime = managed.then(|| form::managed_runtime(&instance, selected.live.as_ref(), executable, failed));
        let fields = runtime.as_ref().map_or_else(
            || form::fields(&instance.driver, instance.config.as_ref()),
            |fields| fields.iter().map(|(f, _)| f.clone()).collect(),
        );
        for field in &fields {
            if !field.options.is_empty() || self.provider_form().fields.iter().any(|input| input.key == field.key) {
                continue;
            }
            let key = field.key.clone();
            let accepts_secret_reference = field.accepts_secret_reference;
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(field.placeholder.clone()).masked(field.secret));
            let generation = self.provider_form().generation;
            let subscription = cx.subscribe_in(&input, window, move |this, source, event: &InputEvent, window, cx| {
                if !this.form_event_current(generation, cx) || !this.provider_form().fields.iter().any(|field| field.key == key && &field.input == source) { return; }
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_config_field(&key, window, cx);
                } else if accepts_secret_reference
                    && matches!(event, InputEvent::Change)
                    && this.current_form_instance(cx).is_some_and(|inst| {
                        let row = json!({ "value": form::field_value(&inst, &key) });
                        this.provider_form().fields.iter().any(|f| f.key == key && f.source.provider(&row).is_some())
                    })
                {
                    cx.notify();
                }
            });
            self.provider_form_mut().fields.push(FieldInput {
                key: field.key.clone(),
                placeholder: field.placeholder.clone(),
                masked: field.secret,
                last_saved: None,
                source: form::SecretSourceDraft::default(),
                input,
                _subscription: subscription,
            });
        }
        for input in &mut self.provider_form_mut().fields {
            if input.input.focus_handle(cx).is_focused(window) {
                continue;
            }
            let Some(field) = fields.iter().find(|f| f.key == input.key) else { continue };
            let saved_value = runtime
                .as_ref()
                .and_then(|fields| fields.iter().find(|(f, _)| f.key == input.key))
                .map_or_else(|| form::field_value(&instance, &input.key), |(_, value)| json!(value));
            if input.last_saved.as_ref() == Some(&saved_value) {
                continue;
            }
            input.last_saved = Some(saved_value.clone());
            let (value, placeholder, masked): (String, String, bool) = if field.accepts_secret_reference {
                let row = json!({ "value": saved_value });
                let stored = input.source.provider(&row).is_none();
                let value = if stored && field.secret { "" } else { input.source.input_value(&row) }.to_owned();
                let masked = stored && field.secret;
                let placeholder = config_field_placeholder(stored, field.secret, &saved_value, &field.placeholder);
                (value, placeholder.to_owned(), masked)
            } else {
                let saved = saved_value.as_str().unwrap_or("");
                let value = if field.secret { "" } else { saved }.to_owned();
                let placeholder = if field.secret && !saved.is_empty() {
                    "Stored secret, enter a new value to replace"
                } else if field.secret {
                    "Not set"
                } else {
                    &field.placeholder
                };
                (value, placeholder.to_owned(), field.secret)
            };
            let placeholder_changed = input.placeholder != placeholder;
            let mask_changed = input.masked != masked;
            input.placeholder = placeholder.clone();
            input.masked = masked;
            if placeholder_changed || mask_changed || input.input.read(cx).value().as_ref() != value {
                input.input.update(cx, |input, cx| {
                    if input.value().as_ref() != value {
                        input.set_value(value, window, cx);
                    }
                    if mask_changed {
                        input.set_masked(masked, window, cx);
                    }
                    if placeholder_changed {
                        input.set_placeholder(placeholder, window, cx);
                    }
                });
            }
        }
        self.sync_argument_inputs(&instance, window, cx);
        let incoming = form::environment(&instance);
        if switched || self.provider_form().seen != incoming {
            let echo = self.provider_form().published.as_ref() == Some(&incoming);
            self.provider_form_mut().seen = incoming.clone();
            self.provider_form_mut().published = None;
            if !echo {
                self.provider_form_mut().variables.clear();
                for row in incoming {
                    self.add_variable_input(row, window, cx);
                }
            }
        }
        if let Some((name, _, _)) = form::dedicated_field(&instance.driver)
            && !self.provider_form().variables.iter().any(|v| v.raw["name"] == name)
        {
            self.add_variable_input(json!({"name":name,"value":"","sensitive":true}), window, cx);
        }
    }

    fn sync_argument_inputs(&mut self, instance: &t3_core::server_config::ProviderInstanceConfig, window: &mut Window, cx: &mut Context<Self>) {
        self.provider_form_mut().arguments.reconcile(form::command_args(instance));
        let rows = self.provider_form().arguments.rows.clone();
        self.provider_form_mut().argument_inputs.retain(|input| rows.iter().any(|row| row.id == input.id));
        for row in rows {
            if let Some(input) = self.provider_form().argument_inputs.iter().find(|a| a.id == row.id) {
                if !input.input.focus_handle(cx).is_focused(window) && input.input.read(cx).value().as_ref() != row.value {
                    input.input.update(cx, |input, cx| input.set_value(row.value, window, cx));
                }
            } else {
                let id = row.id;
                let input = cx.new(|cx| InputState::new(window, cx).default_value(row.value));
                let generation = self.provider_form().generation;
                let subscription = cx.subscribe_in(&input, window, move |this, source, event: &InputEvent, _, cx| {
                    if !this.form_event_current(generation, cx) || !this.provider_form().argument_inputs.iter().any(|field| field.id == id && &field.input == source) { return; }
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) || (this.providers.dialog.is_some() && matches!(event, InputEvent::Change)) {
                        this.commit_argument(id, cx);
                    }
                });
                if self.providers.dialog.is_some() { input.update(cx, |input, cx| input.focus(window, cx)); }
                self.provider_form_mut().argument_inputs.push(ArgumentInput {
                    id,
                    input,
                    _subscription: subscription,
                });
            }
        }
    }

    pub(super) fn add_variable_input(&mut self, raw: Value, window: &mut Window, cx: &mut Context<Self>) {
        let driver = self.current_form_instance(cx).map(|i| i.driver).unwrap_or_default();
        let id = self.provider_form().next_id;
        self.provider_form_mut().next_id += 1;
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("VARIABLE_NAME")
                .default_value(raw["name"].as_str().unwrap_or(""))
        });
        let redacted = raw["valueRedacted"] == true;
        let reference = raw["value"].is_object();
        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(if redacted {
                    "Stored secret, enter a new value to replace"
                } else if matches!(raw["name"].as_str(), Some("KIRO_API_KEY" | "CURSOR_API_KEY")) {
                    "Paste API key"
                } else {
                    "value"
                })
                .masked(!reference && form::variable_sensitive(&driver, &raw))
                .default_value(if redacted {
                    ""
                } else if reference {
                    raw["value"]["reference"].as_str().unwrap_or("")
                } else {
                    raw["value"].as_str().unwrap_or("")
                })
        });
        let generation = self.provider_form().generation;
        let subscriptions = [(&name, false), (&value, true)]
            .into_iter()
            .map(|(input, is_value)| {
                cx.subscribe_in(input, window, move |this, source, event: &InputEvent, window, cx| {
                    if !this.form_event_current(generation, cx) || !this.provider_form().variables.iter().any(|field| field.id == id && if is_value { &field.value == source } else { &field.name == source }) { return; }
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) || (this.providers.dialog.is_some() && matches!(event, InputEvent::Change)) {
                        this.commit_variable(id, is_value, window, cx);
                    } else if is_value && matches!(event, InputEvent::Change) && this.provider_form().variables.iter().any(|field| field.id == id && field.source.provider(&field.raw).is_some()) {
                        cx.notify();
                    }
                })
            })
            .collect();
        if self.providers.dialog.is_some() && raw["name"] == "" { name.update(cx, |input, cx| input.focus(window, cx)); }
        let source = form::SecretSourceDraft::default();
        self.provider_form_mut().variables.push(VariableInput {
            id,
            source,
            raw,
            name,
            value,
            _subscriptions: subscriptions,
        });
    }

    fn commit_config_field(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.providers.dialog.is_some() { return; }
        let Some(id) = self.editor_instance_id(cx) else { return };
        if id != self.provider_form().instance_id {
            return;
        }
        let Some(instance) = self.current_form_instance(cx) else { return };
        if !form::allows_edit(&instance, form::EditTarget::Runtime) {
            return;
        }
        if self
            .form_denial(&instance.driver, &format!("config.{}", key.split(':').next().unwrap_or(key)), cx)
            .is_some()
        {
            return;
        }
        let (input, text) = {
            let Some(field) = self.provider_form().fields.iter().find(|f| f.key == key) else {
                return;
            };
            (field.input.clone(), field.input.read(cx).value().to_string())
        };
        let meta = form::fields(&instance.driver, instance.config.as_ref()).into_iter().find(|f| f.key == key);
        let secret = meta.as_ref().is_some_and(|f| f.secret);
        let accepts_secret_ref = meta.as_ref().is_some_and(|f| f.accepts_secret_reference);
        let saved = form::field_value(&instance, key);
        let active_provider = accepts_secret_ref.then(|| {
            let row = json!({ "value": saved });
            self.provider_form()
                .fields
                .iter()
                .find(|f| f.key == key)
                .and_then(|f| f.source.provider(&row).map(str::to_owned))
        }).flatten();
        let one_password = active_provider.as_deref().is_some_and(|source| self.is_one_password_provider(source, cx));
        let mut clear_masked_input = false;
        let value = if active_provider.is_some() {
            let mut row = json!({ "value": saved });
            let Some(field) = self.provider_form_mut().fields.iter_mut().find(|f| f.key == key) else {
                return;
            };
            match field.source.commit(&mut row, &text, one_password) {
                Ok(false) => return,
                Err(error) => {
                    self.provider_form_mut().error = Some(error.into());
                    cx.notify();
                    return;
                }
                Ok(true) => {
                    if field.source.provider(&row).is_none() {
                        let default_placeholder = meta.as_ref().map_or("", |f| f.placeholder.as_str());
                        let placeholder = config_field_placeholder(true, secret, &row["value"], default_placeholder).to_owned();
                        field.masked = secret;
                        field.placeholder = placeholder.clone();
                        input.update(cx, |input, cx| {
                            input.set_masked(secret, window, cx);
                            input.set_placeholder(placeholder, window, cx);
                        });
                    }
                    if row["value"] == saved {
                        return;
                    }
                    row["value"].clone()
                }
            }
        } else {
            if secret && text.is_empty() {
                return;
            }
            let value = json!(text);
            if value.as_str().unwrap_or_default() == saved.as_str().unwrap_or_default() {
                return;
            }
            if !form::validate_field(&instance.driver, key, &value) {
                self.provider_form_mut().error = Some("Compact after 100,000 to 1,000,000 tokens. Leave empty to use Claude's default.".into());
                cx.notify();
                return;
            }
            clear_masked_input = secret;
            value
        };
        self.provider_form_mut().error = None;
        self.update_instance(&id, model::InstanceEdit::ConfigField { key: key.into(), value }, cx);
        if clear_masked_input {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    fn commit_argument(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(input) = self.provider_form().argument_inputs.iter().find(|a| a.id == id) else {
            return;
        };
        let value = input.input.read(cx).value().to_string();
        self.edit_arguments(form::ArgumentEdit::Value(id, value), cx);
    }
    fn edit_arguments(&mut self, edit: form::ArgumentEdit, cx: &mut Context<Self>) {
        if self.providers.dialog.as_ref().is_some_and(|d| !d.local_acp()) { return; }
        let Some(instance) = self.current_form_instance(cx) else {
            return;
        };
        if self
            .form_denial(&instance.driver, "config.commandArgs", cx)
            .is_some()
        {
            return;
        }
        if let Some(publication) = self.provider_form_mut().arguments.edit(edit) {
            if let Some(dialog) = self.providers.dialog.as_mut() {
                dialog.configs.entry(dialog.driver().kind).or_default().model.edit("acpRegistry", "commandArgs", json!(publication.values));
                self.provider_form_mut().arguments.settle(publication.id, Some(publication.values));
                cx.notify();
                return;
            }
            let id = self.provider_form().instance_id.clone();
            let generation = self.provider_form().generation;
            let publication_id = publication.id;
            let view = cx.entity().downgrade();
            self.update_scoped_single_env_then(
                ServerSettingChange::ProviderInstance {
                    instance_id: id.clone(),
                    edit: model::InstanceEdit::CommandArgs {
                        base: publication.base,
                        values: publication.values,
                    },
                },
                move |error, cx| {
                    cx.defer(move |cx| {
                        let _ = view.update(cx, |this, cx| {
                            if this.providers.form.instance_id != id
                                || this.providers.form.generation != generation
                            {
                                return;
                            }
                            let saved = if error.is_none() {
                                this.scoped_raw_representative_config(cx)
                                    .and_then(|c| c.settings.provider_instances.get(&id).map(form::command_args))
                            } else {
                                None
                            };
                            this.providers.form.arguments.settle(publication_id, saved);
                            cx.notify();
                        });
                    });
                },
                cx,
            );
            cx.notify();
        }
    }

    pub(super) fn current_form_instance(
        &self,
        cx: &App,
    ) -> Option<t3_core::server_config::ProviderInstanceConfig> {
        if let Some(dialog) = &self.providers.dialog {
            return Some(t3_core::settings::providers::wizard::instance(dialog.driver().kind, "", "", dialog.configs.get(dialog.driver().kind).and_then(|c| c.model.value.clone()), vec![]));
        }
        let id = self.editor_instance_id(cx)?;
        if id != self.provider_form().instance_id {
            return None;
        }
        let app = self.state.read(cx);
        let c = app.server_config.config.as_ref()?;
        let id = &id;
        Some(
            c.settings
                .provider_instances
                .get(id)
                .cloned()
                .unwrap_or_else(|| t3_core::server_config::ProviderInstanceConfig {
                    driver: id.clone(),
                    ..Default::default()
                }),
        )
    }

    fn commit_variable(&mut self, id: usize, is_value: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.providers.dialog.as_ref().is_some_and(|d| !d.local_acp()) { return; }
        let Some(instance) = self.current_form_instance(cx) else { return };
        let Some(index) = self.provider_form().variables.iter().position(|v| v.id == id) else {
            return;
        };
        let variable = &self.provider_form().variables[index];
        let name = variable.name.read(cx).value().trim().to_owned();
        let old = variable.raw["name"].as_str().unwrap_or("");
        if !form::allows_edit(&instance, form::EditTarget::Environment) {
            return;
        }
        if self.form_denial(&instance.driver, &format!("environment.{old}"), cx).is_some()
            || self.form_denial(&instance.driver, &format!("environment.{name}"), cx).is_some()
        {
            return;
        }
        let next = if is_value { variable.value.read(cx).value().to_string() } else { name };
        let previous = if is_value {
            if variable.source.provider(&variable.raw).is_some() {
                Some(variable.source.input_value(&variable.raw))
            } else {
                variable.raw["value"].as_str()
            }
        } else {
            variable.raw["name"].as_str()
        };
        if previous == Some(next.as_str()) {
            return;
        }
        if is_value && variable.raw["valueRedacted"] == true && next.is_empty() {
            return;
        }
        if !is_value && variable.raw["valueRedacted"] == true {
            return;
        }
        let one_password = variable.source.provider(&variable.raw).is_some_and(|source| {
            self.state
                .read(cx)
                .server_config
                .config
                .as_ref()
                .and_then(|c| c.settings.secret_providers.get(source))
                .is_some_and(|p| p.kind == t3_core::settings::secrets::ONE_PASSWORD)
        });
        let variable = &mut self.provider_form_mut().variables[index];
        if is_value {
            if variable.source.provider(&variable.raw).is_some() {
                match variable.source.commit(&mut variable.raw, &next, one_password) {
                    Ok(false) => return,
                    Err(error) => {
                        self.provider_form_mut().error = Some(error.into());
                        cx.notify();
                        return;
                    }
                    Ok(true) => {
                        if variable.source.provider(&variable.raw).is_none() {
                            variable.value.update(cx, |input, cx| {
                                input.set_masked(true, window, cx);
                                input.set_placeholder("value", window, cx);
                            });
                        }
                    }
                }
            } else {
                variable.raw = form::edit_variable(&variable.raw, form::VariableEdit::Value(next));
            }
        } else {
            variable.raw = form::edit_variable(&variable.raw, form::VariableEdit::Name(next));
            let masked = form::variable_sensitive(&instance.driver, &variable.raw) && variable.source.provider(&variable.raw).is_none();
            variable.value.update(cx, |input, cx| input.set_masked(masked, window, cx));
        }
        self.publish_variables(cx);
    }

    fn publish_variables(&mut self, cx: &mut Context<Self>) {
        if self.providers.dialog.as_ref().is_some_and(|d| !d.local_acp()) { return; }
        let rows = self.provider_form().variables.iter().map(|v| v.raw.clone()).collect::<Vec<_>>();
        let Some(instance) = self.current_form_instance(cx) else { return };
        match form::publish_environment(&instance.driver, rows) {
            Ok(rows) => {
                self.provider_form_mut().error = None;
                self.provider_form_mut().published = Some(rows.clone());
                if self.providers.dialog.is_some() { cx.notify(); return; }
                let id = self.provider_form().instance_id.clone();
                self.update_instance(&id, model::InstanceEdit::Environment(rows), cx);
            }
            Err(error) => self.provider_form_mut().error = Some(error.into()),
        }
        cx.notify();
    }

    pub(super) fn render_runtime_form(&mut self, selected: &Selected, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.providers.dialog.is_some() { return None; }
        let instance = selected.row.instance();
        let managed = instance.driver == "codex" && model::chatgpt::managed(instance.config.as_ref());
        let executable = self
            .providers
            .chatgpt
            .as_ref()
            .and_then(|(_, view)| view.read(cx).state.install.as_ref())
            .and_then(|i| i.executable_path.as_deref());
        let failed = self.providers.chatgpt.as_ref().is_some_and(|(_, view)| view.read(cx).state.install_load_error);
        let fields = if managed {
            form::managed_runtime(&instance, selected.live.as_ref(), executable, failed)
                .into_iter()
                .map(|(f, _)| f)
                .collect()
        } else {
            form::fields(&instance.driver, instance.config.as_ref())
        };
        if fields.is_empty() {
            return None;
        }
        let folded = managed && !self.provider_form().runtime_open;
        let mut rows = Vec::new();
        let header = managed.then(|| {
            h_flex().px(r(16.)).gap(r(8.)).child(div().flex_1().child(section_title("Runtime"))).child(
                Button::new("provider-runtime-expand", ButtonVariant::GhostMuted, ButtonSize::Compact)
                    .label(if folded { "Show more" } else { "Show less" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.provider_form_mut().runtime_open = !this.provider_form().runtime_open;
                        cx.notify();
                    })),
            )
        });
        if !folded {
            for field in fields {
                let key = field.key.clone();
                let id = selected.row.instance_id.clone();
                let denial = self.form_denial(&instance.driver, &format!("config.{key}"), cx);
                let disabled = managed || denial.is_some();
                let saved = instance.config.as_ref().and_then(|c| c.get(&key)).cloned().unwrap_or(Value::Null);
                let control = if disabled && !field.options.is_empty() {
                    let text = saved.as_str().filter(|s| !s.is_empty()).unwrap_or(&field.options[0].0);
                    let label = field.options.iter().find(|(v, _)| v == text).map_or(text, |(_, l)| l.as_str());
                    div().w(r(224.)).opacity(0.5).child(label.to_owned()).into_any_element()
                } else if !field.options.is_empty() {
                    let current = saved.as_str().filter(|s| !s.is_empty()).unwrap_or(&field.options[0].0).to_owned();
                    let label = field
                        .options
                        .iter()
                        .find(|(value, _)| value == &current)
                        .map_or(current.clone(), |(_, label)| label.clone());
                    let fallback = field.options[0].0.clone();
                    let view = cx.entity().downgrade();
                    let edit_key = key.clone();
                    Select::new(
                        SharedString::from(format!("provider-config-{key}")),
                        field.options.into_iter().map(|(v, l)| SelectOption::new(v, l)).collect(),
                    )
                    .selected(current)
                    .label(label)
                    .width(224.)
                    .on_change(move |value, _, cx| {
                        let value = if value.as_ref() == fallback { "" } else { value.as_ref() };
                        let _ = view.update(cx, |this, cx| {
                            this.update_instance(
                                &id,
                                model::InstanceEdit::ConfigField {
                                    key: edit_key.clone(),
                                    value: json!(value),
                                },
                                cx,
                            )
                        });
                    })
                    .into_any_element()
                } else {
                    let Some(field_input) = self.provider_form().fields.iter().find(|f| f.key == key) else {
                        continue;
                    };
                    let input = field_input.input.clone();
                    let row = json!({ "value": saved });
                    let active_provider = field_input.source.provider(&row).map(str::to_owned);
                    let current_text = input.read(cx).value().to_string();
                    let source = field.accepts_secret_reference.then(|| {
                        let source_key = key.clone();
                        let source_id = id.clone();
                        self.secret_source_select(
                            SharedString::from(format!("provider-config-source-{key}")).into(),
                            active_provider.as_deref(),
                            disabled,
                            move |next_provider, this, window, cx| {
                                this.select_config_secret_source(&source_id, &source_key, next_provider, window, cx);
                            },
                            cx,
                        )
                    }).flatten();
                    let secret_check = field
                        .accepts_secret_reference
                        .then(|| {
                            let provider = saved.get("provider").and_then(Value::as_str)?;
                            let reference = saved.get("reference").and_then(Value::as_str)?;
                            (active_provider.as_deref() == Some(provider) && current_text == reference)
                                .then(|| self.secret_check_button(SharedString::from(format!("secret-check-config-{key}")).into(), provider, reference, disabled, cx))
                                .flatten()
                        })
                        .flatten();
                    let configured = saved.is_object() || saved.as_str().is_some_and(|v| !v.is_empty());
                    let has_source = source.is_some();
                    let clear_key = key.clone();
                    let clear_secret = field.secret;
                    let clear_placeholder = field.placeholder.clone();
                    h_flex()
                        .gap(r(6.))
                        .children(source)
                        .child(div().w(r(if has_source { 208. } else { 224. })).child(Input::new(&input).small().disabled(disabled)))
                        .children(secret_check)
                        .when((field.secret || field.accepts_secret_reference) && configured, |el| {
                            el.child(
                                Button::new(SharedString::from(format!("clear-config-{key}")), ButtonVariant::GhostMuted, ButtonSize::IconSm)
                                    .icon(Icon::X)
                                    .tooltip(format!("Clear {}", field.label))
                                    .disabled(disabled)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(fi) = this.provider_form_mut().fields.iter_mut().find(|f| f.key == clear_key) {
                                            fi.source = form::SecretSourceDraft::default();
                                            fi.masked = clear_secret;
                                            let placeholder = if clear_secret { "Not set" } else { clear_placeholder.as_str() }.to_owned();
                                            fi.placeholder = placeholder.clone();
                                            fi.input.update(cx, |input, cx| {
                                                input.set_value("", window, cx);
                                                input.set_masked(clear_secret, window, cx);
                                                input.set_placeholder(placeholder, window, cx);
                                            });
                                        }
                                        this.provider_form_mut().error = None;
                                        this.update_instance(
                                            &id,
                                            model::InstanceEdit::ConfigField {
                                                key: clear_key.clone(),
                                                value: json!(""),
                                            },
                                            cx,
                                        );
                                    })),
                            )
                        })
                        .into_any_element()
                };
                rows.push(
                    Row::new(SharedString::from(format!("provider-field-{key}")), field.label)
                        .description(denial.unwrap_or(field.description))
                        .control(control)
                        .into_any_element(),
                );
            }
        }
        if !folded && instance.driver == "acpRegistry" && instance.config.as_ref().is_some_and(|c| c["source"] == "local") {
            rows.push(self.render_argument_controls(cx));
        }
        Some(if let Some(header) = header {
            let border = hsla(palette(cx).border).opacity(0.5);
            v_flex()
                .gap(r(4.))
                .child(header)
                .children(
                    rows.into_iter()
                        .enumerate()
                        .map(|(index, row)| div().when(index > 0, |el| el.border_t_1().border_color(border)).child(row)),
                )
                .into_any_element()
        } else {
            flat_section("Runtime", rows, cx).into_any_element()
        })
    }

    pub(super) fn render_argument_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
            let args = self.provider_form().arguments.rows.clone();
            let disabled = self.form_denial("acpRegistry", "config.commandArgs", cx).is_some();
            let mut controls = Vec::new();
            for (index, row) in args.into_iter().enumerate() {
                let id = row.id;
                let Some(input) = self.provider_form().argument_inputs.iter().find(|f| f.id == id).map(|f| f.input.clone()) else {
                    continue;
                };
                controls.push(
                    h_flex()
                        .gap(r(6.))
                        .child(div().flex_1().min_w_0().child(Input::new(&input).small().disabled(disabled)))
                        .child(
                            Button::new(("remove-provider-argument", id), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                                .icon(Icon::X)
                                .tooltip(format!("Remove argument {}", index + 1))
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _, _, cx| this.edit_arguments(form::ArgumentEdit::Remove(id), cx))),
                        )
                        .into_any_element(),
                );
            }
                Row::new("provider-arguments", "Arguments")
                    .description("One literal argument per row, in launch order.")
                    .control(
                        Button::new("provider-add-argument", ButtonVariant::Outline, ButtonSize::Sm)
                            .icon(Icon::Plus)
                            .label("Add argument")
                            .disabled(disabled)
                            .on_click(cx.listener(move |this, _, _, cx| this.edit_arguments(form::ArgumentEdit::Add, cx))),
                    )
                    .below(v_flex().gap(r(8.)).children(controls))
                    .into_any_element()
    }

    pub(super) fn render_dedicated_environment(&mut self, selected: &Selected, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.providers.dialog.is_some() { return None; }
        let (name, label, description) = form::dedicated_field(&selected.row.driver)?;
        let index = self.provider_form().variables.iter().position(|v| v.raw["name"] == name)?;
        let control = self.render_variable(index, true, &selected.row.driver, cx);
        Some(Row::new("provider-api-key", label).description(description).control(control).into_any_element())
    }

    pub(super) fn render_environment_form(&mut self, selected: &Selected, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.providers.dialog.is_some() { return Vec::new(); }
        self.render_environment_controls(&selected.row.driver, cx)
    }
    pub(super) fn render_environment_controls(&mut self, driver: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut sections = Vec::new();
        let dedicated = form::dedicated_field(driver);
        let disabled = self.form_denial(driver, "environment", cx).is_some();
        let mut children = Vec::new();
        for index in 0..self.provider_form().variables.len() {
            if dedicated.is_some_and(|(name, _, _)| self.provider_form().variables[index].raw["name"] == name) {
                continue;
            }
            children.push(self.render_variable(index, false, driver, cx));
        }
        let rows = vec![
            Row::new("provider-variables", "Variables")
                .description("API keys, base URLs, and other per-instance CLI settings.")
                .control(
                    Button::new("provider-add-variable", ButtonVariant::Outline, ButtonSize::Sm)
                        .icon(Icon::Plus)
                        .label("Add variable")
                        .disabled(disabled)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let Some(instance) = this.current_form_instance(cx) else { return };
                            if this.form_denial(&instance.driver, "environment", cx).is_some() { return; }
                            this.add_variable_input(json!({"name":"","value":"","sensitive":true}), window, cx);
                            cx.notify();
                        })),
                )
                .below(v_flex().gap(r(8.)).children(children))
                .into_any_element(),
        ];
        sections.push(flat_section("Environment", rows, cx).into_any_element());
        if let Some(error) = &self.provider_form().error {
            sections.push(
                div()
                    .text_xs()
                    .text_color(hsla(palette(cx).destructive))
                    .child(error.clone())
                    .into_any_element(),
            );
        }
        sections
    }

    fn is_one_password_provider(&self, source: &str, cx: &App) -> bool {
        self.state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .and_then(|c| c.settings.secret_providers.get(source))
            .is_some_and(|p| p.kind == secrets::ONE_PASSWORD)
    }

    fn secret_source_select(
        &self,
        id: ElementId,
        current: Option<&str>,
        disabled: bool,
        on_select: impl Fn(Option<String>, &mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let config = self.state.read(cx).server_config.config.as_ref()?;
        let mut sources = if config.secret_references {
            config
                .settings
                .secret_providers
                .0
                .iter()
                .filter(|p| !matches!(t3_core::settings::secrets::classify(p), t3_core::settings::secrets::Classified::Unsupported))
                .map(|p| (p.id.clone(), p.label.clone(), p.kind == secrets::ONE_PASSWORD))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if sources.is_empty() && current.is_none() {
            return None;
        }
        if let Some(source) = current
            && !sources.iter().any(|(id, _, _)| id == source)
        {
            sources.push((source.to_owned(), source.to_owned(), false));
        }
        let selected = current.unwrap_or("__stored__").to_owned();
        if disabled {
            let label = sources
                .iter()
                .find(|(id, _, _)| id == &selected)
                .map_or_else(|| if current.is_none() { "Value".to_owned() } else { selected.clone() }, |(_, label, _)| label.clone());
            return Some(div().text_xs().opacity(0.5).child(label).into_any_element());
        }
        let options = std::iter::once(SelectOption::new("__stored__", "Value").icon(Icon::FileText))
            .chain(sources.into_iter().map(|(id, label, one_password)| {
                SelectOption::new(id, label).icon(if one_password { Icon::OnePassword } else { Icon::KeyRound })
            }))
            .collect();
        let view = cx.entity().downgrade();
        Some(
            Select::new(id, options)
                .selected(selected)
                .label("")
                .popup_width(192.)
                .on_change(move |source, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        let next = (source.as_ref() != "__stored__").then(|| source.to_string());
                        on_select(next, this, window, cx);
                    });
                })
                .into_any_element(),
        )
    }

    fn variable_source(&self, index: usize, driver: &str, disabled: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let v = &self.provider_form().variables[index];
        if !form::variable_sensitive(driver, &v.raw) {
            return None;
        }
        let id = v.id;
        let driver = driver.to_owned();
        let current = v.source.provider(&v.raw).map(str::to_owned);
        self.secret_source_select(
            ("provider-env-source", id).into(),
            current.as_deref(),
            disabled,
            move |next_provider, this, window, cx| {
                let name = this.provider_form().variables.iter().find(|v| v.id == id).and_then(|v| v.raw["name"].as_str()).unwrap_or("");
                if this.form_denial(&driver, &format!("environment.{name}"), cx).is_some() {
                    return;
                }
                let Some(v) = this.provider_form_mut().variables.iter_mut().find(|v| v.id == id) else {
                    return;
                };
                let stored = next_provider.is_none();
                let changed = v.source.select(&mut v.raw, next_provider);
                let value = v.source.input_value(&v.raw);
                v.value.update(cx, |input, cx| {
                    input.set_value(value, window, cx);
                    input.set_masked(stored && form::variable_sensitive(&driver, &v.raw), window, cx);
                    input.set_placeholder(
                        if stored && v.raw["valueRedacted"] == true {
                            "Stored secret, enter a new value to replace"
                        } else if stored {
                            "value"
                        } else {
                            "op://vault/item/field"
                        },
                        window,
                        cx,
                    );
                });
                if changed {
                    this.publish_variables(cx);
                }
                cx.notify();
            },
            cx,
        )
    }

    fn select_config_secret_source(&mut self, instance_id: &str, key: &str, next_provider: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(instance) = self.current_form_instance(cx) else {
            return;
        };
        if self.form_denial(&instance.driver, &format!("config.{key}"), cx).is_some() {
            return;
        }
        let Some(meta) = form::fields(&instance.driver, instance.config.as_ref()).into_iter().find(|field| field.key == key) else {
            return;
        };
        let saved_now = form::field_value(&instance, key);
        let mut row = json!({ "value": saved_now });
        let Some(fi) = self.provider_form_mut().fields.iter_mut().find(|f| f.key == key) else {
            return;
        };
        let stored = next_provider.is_none();
        let changed = fi.source.select(&mut row, next_provider);
        let value = if stored && meta.secret { "" } else { fi.source.input_value(&row) }.to_owned();
        let masked = stored && meta.secret;
        let placeholder = config_field_placeholder(
            stored,
            meta.secret,
            if changed { &Value::Null } else { &saved_now },
            &meta.placeholder,
        )
        .to_owned();
        fi.masked = masked;
        fi.placeholder = placeholder.clone();
        fi.input.update(cx, |input, cx| {
            input.set_value(value, window, cx);
            input.set_masked(masked, window, cx);
            input.set_placeholder(placeholder, window, cx);
        });
        self.provider_form_mut().error = None;
        if changed {
            self.update_instance(
                instance_id,
                model::InstanceEdit::ConfigField {
                    key: key.to_owned(),
                    value: json!(""),
                },
                cx,
            );
        }
        cx.notify();
    }

    fn check_secret_source(&mut self, provider: String, reference: String, cx: &mut Context<Self>) {
        if self.state.read(cx).server_config.config.as_ref().is_none_or(|c| c.environment.capabilities.secret_source_check != Some(true))
            || !self.may(SETTINGS_WRITE_SCOPE, cx)
        {
            return;
        }
        let key = (provider.clone(), reference.clone());
        if matches!(self.provider_form().secret_checks.get(&key), Some(SecretCheckStatus::Checking)) {
            return;
        }
        let generation = self.provider_form().generation;
        self.provider_form_mut().secret_checks.insert(key.clone(), SecretCheckStatus::Checking);
        let task = self.scoped_rpc(SETTINGS_WRITE_SCOPE, secrets::CHECK_SECRET_SOURCE, secrets::check_secret_source_payload(&provider, &reference), cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let next = match result {
                Ok(reply) => match secrets::decode_check_secret_source_result(&reply) {
                    secrets::SecretSourceCheckResult::Resolved => SecretCheckStatus::Resolved,
                    secrets::SecretSourceCheckResult::Failed(detail) => SecretCheckStatus::Failed(detail),
                },
                Err(_) => SecretCheckStatus::Failed("Could not reach the server".to_owned()),
            };
            let _ = this.update(cx, |this, cx| {
                if !this.form_event_current(generation, cx) {
                    return;
                }
                this.provider_form_mut().secret_checks.insert(key, next);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// `SecretCheckButton` (`SecretSourceInput.tsx`): verifies a committed secret reference on demand.
    fn secret_check_button(&self, button_id: ElementId, provider: &str, reference: &str, disabled: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.state.read(cx).server_config.config.as_ref().is_none_or(|c| c.environment.capabilities.secret_source_check != Some(true)) {
            return None;
        }
        let p = palette(cx);
        let can_check = self.may(SETTINGS_WRITE_SCOPE, cx);
        let status = self.provider_form().secret_checks.get(&(provider.to_owned(), reference.to_owned()));
        let checking = matches!(status, Some(SecretCheckStatus::Checking));
        let (glyph, color) = match status {
            Some(SecretCheckStatus::Resolved) => (Icon::Check, hsla(p.success_foreground)),
            Some(SecretCheckStatus::Failed(_)) => (Icon::TriangleAlert, hsla(p.destructive)),
            Some(SecretCheckStatus::Checking) | None => (Icon::RefreshCw, hsla(p.muted_foreground)),
        };
        let description: SharedString = if !can_check {
            "You cannot check secrets on this environment".into()
        } else {
            match status {
                None => "Check that the secret resolves".into(),
                Some(SecretCheckStatus::Checking) => "Checking the secret".into(),
                Some(SecretCheckStatus::Resolved) => "Secret resolved".into(),
                Some(SecretCheckStatus::Failed(detail)) => detail.clone().into(),
            }
        };
        let provider = provider.to_owned();
        let reference = reference.to_owned();
        let debug_id = match &button_id {
            ElementId::Name(name) => Some(name.to_string()),
            _ => None,
        };
        Some(
            div()
                .flex_none()
                .when_some(debug_id, |el, id| el.debug_selector(move || id.clone()))
                .child(
                    Button::new(button_id, ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                        .icon(glyph)
                        .icon_color(color)
                        .loading(checking)
                        .tooltip(description)
                        .disabled(disabled || !can_check || checking)
                        .on_click(cx.listener(move |this, _, _, cx| this.check_secret_source(provider.clone(), reference.clone(), cx))),
                )
                .into_any_element(),
        )
    }

    fn render_variable(&mut self, index: usize, dedicated: bool, driver: &str, cx: &mut Context<Self>) -> AnyElement {
        let v = &self.provider_form().variables[index];
        let id = v.id;
        let redacted = v.raw["valueRedacted"] == true;
        let sensitive = v.raw["sensitive"] == true;
        let reference = v.raw["value"].is_object();
        let name = v.raw["name"].as_str().unwrap_or("").to_owned();
        let denial = self.form_denial(driver, &format!("environment.{name}"), cx);
        let disabled = denial.is_some();
        let committed_ref = v.raw["value"]["provider"]
            .as_str()
            .zip(v.raw["value"]["reference"].as_str())
            .filter(|(provider, reference)| {
                v.source.provider(&v.raw) == Some(*provider) && v.value.read(cx).value().as_ref() == *reference
            })
            .map(|(provider, reference)| (provider.to_owned(), reference.to_owned()));
        let check_button = committed_ref.as_ref().and_then(|(provider, reference)| {
            self.secret_check_button(("secret-check", id).into(), provider, reference, disabled, cx)
        });
        let name_control = (!dedicated).then(|| div().flex_1().min_w_0().child(Input::new(&v.name).small().disabled(disabled || redacted)));
        let source = self.variable_source(index, driver, disabled, cx);
        let v = &self.provider_form().variables[index];
        let value = Input::new(&v.value).small().disabled(disabled);
        let remove = Button::new(("remove-provider-env", id), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
            .icon(Icon::X)
            .tooltip(if dedicated {
                format!("Clear {name}")
            } else {
                format!("Remove environment variable {name}")
            })
            .disabled(disabled)
            .on_click(cx.listener(move |this, _, _, cx| {
                let Some(instance) = this.current_form_instance(cx) else { return };
                let name = this.provider_form().variables.iter().find(|v| v.id == id).and_then(|v| v.raw["name"].as_str()).unwrap_or("");
                if this.form_denial(&instance.driver, &format!("environment.{name}"), cx).is_some() { return; }
                this.provider_form_mut().variables.retain(|v| v.id != id);
                this.publish_variables(cx);
            }));
        if dedicated {
            let configured = redacted || reference || v.raw["value"].as_str().is_some_and(|s| !s.is_empty());
            let has_source = source.is_some();
            return h_flex()
                .gap(r(6.))
                .children(source)
                .child(div().w(r(if has_source { 208. } else { 224. })).child(value))
                .children(check_button)
                .children(configured.then_some(remove))
                .children(denial.map(|reason| div().text_xs().text_color(hsla(palette(cx).muted_foreground)).child(reason)))
                .into_any_element();
        }
        let lock = (!dedicated && !reference).then(|| {
            Button::new(("sensitive-provider-env", id), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                .icon(if sensitive { Icon::Lock } else { Icon::LockOpen })
                .tooltip(if sensitive { "Sensitive, stored separately" } else { "Plain text" })
                .disabled(disabled || redacted)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let Some(instance) = this.current_form_instance(cx) else { return };
                    let name = this.provider_form().variables.iter().find(|v| v.id == id).and_then(|v| v.raw["name"].as_str()).unwrap_or("");
                    if this.form_denial(&instance.driver, &format!("environment.{name}"), cx).is_some() { return; }
                    let Some(v) = this.provider_form_mut().variables.iter_mut().find(|v| v.id == id) else { return };
                    let next = v.raw["sensitive"] != true;
                    if !v.source.set_sensitive(&mut v.raw, next) { return; }
                    let value = v.source.input_value(&v.raw).to_owned();
                    let masked = form::variable_sensitive(&instance.driver, &v.raw);
                    v.value.update(cx, |input, cx| {
                        input.set_value(value, window, cx);
                        input.set_placeholder("value", window, cx);
                        input.set_masked(masked, window, cx);
                    });
                    if let Some(dialog) = this.providers.dialog.as_mut() { dialog.error = None; }
                    this.publish_variables(cx);
                }))
        });
        v_flex()
            .gap(r(6.))
            .when(!dedicated, |el| el.p(r(8.)).rounded(r(8.)).border_1().border_color(hsla(palette(cx).border)))
            .child(h_flex().gap(r(6.)).children(name_control).children(source).child(remove))
            .child(h_flex().gap(r(6.)).child(div().flex_1().min_w_0().child(value)).children(check_button).children(lock))
            .children(denial.map(|reason| div().text_xs().text_color(hsla(palette(cx).muted_foreground)).child(reason)))
            .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod secret_check_tests {
    use super::*;
    use gpui::{AppContext, IntoElement, ParentElement, Render, Styled, div, px, size};
    use serde_json::json;
    use t3_core::settings::search::Section;

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().bg(hsla(palette(cx).background)).children(super::super::super::overlay(window, cx))
        }
    }

    #[test]
    fn secret_check_button_verifies_committed_reference_and_respects_gates() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let drive = |cx: &mut gpui::HeadlessAppContext| {
            cx.run_until_parked();
            runtime.block_on(async {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            });
            cx.run_until_parked();
        };
        let mut cx = crate::snapshot::test_context();
        cx.allow_parking();
        let (state, mut fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "orchestration:operate", "settings:write", "providers:manage", "environment:maintain"]
                }))
                .unwrap(),
            ))
            .unwrap();
        drive(&mut cx);
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": { "environmentId": "fixture-env", "label": "Studio Mac", "capabilities": { "secretProviderSettings": true, "secretSourceCheck": true } },
                        "secretProviderSettings": true,
                        "secretReferences": true,
                        "settings": {
                            "secretProviders": {
                                "op-main": { "kind": "one_password", "label": "Work 1Password", "config": { "account": "ops.1password.com" } }
                            },
                            "providerInstances": {
                                "cursor": {
                                    "driver": "cursor",
                                    "enabled": true,
                                    "displayName": "Cursor Main",
                                    "environment": [
                                        { "name": "CURSOR_API_KEY", "value": { "kind": "secret", "provider": "op-main", "reference": "op://Dev/OpenAI/api-key" }, "sensitive": true },
                                        { "name": "CUSTOM_SECRET", "value": { "kind": "secret", "provider": "op-main", "reference": "op://Dev/Custom/token" }, "sensitive": true }
                                    ]
                                },
                                "antigravity": {
                                    "driver": "antigravity",
                                    "enabled": true,
                                    "displayName": "Antigravity Main",
                                    "config": {
                                        "apiKey": { "provider": "op-main", "reference": "op://Dev/Gemini/api-key" }
                                    }
                                }
                            }
                        },
                        "providers": [
                            { "instanceId": "cursor", "driver": "cursor", "enabled": true, "installed": true, "status": "ready" },
                            { "instanceId": "antigravity", "driver": "antigravity", "enabled": true, "installed": true, "status": "ready" }
                        ]
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx
            .open_window(size(px(1280.), px(900.)), |_, cx| cx.new(|_| SettingsFrame))
            .unwrap();
        cx.update(|cx| super::super::super::open(Section::Providers, cx));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();

        let view = cx.update(|cx| cx.global::<super::super::super::Host>().view.clone().expect("settings view open"));
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.providers.selected = Some("cursor".into());
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();
        cx.update_window(window.into(), |_, _, cx| {
            view.update(cx, |view, cx| {
                assert!(
                    view.secret_check_button("probe-check".into(), "op-main", "op://Dev/OpenAI/api-key", false, cx).is_some(),
                    "capability secretSourceCheck enables check button"
                );
                view.check_secret_source("op-main".into(), "op://Dev/OpenAI/api-key".into(), cx);
                assert!(matches!(
                    view.provider_form().secret_checks.get(&("op-main".into(), "op://Dev/OpenAI/api-key".into())),
                    Some(SecretCheckStatus::Checking)
                ));
            });
        })
        .unwrap();
        drive(&mut cx);

        let (requests, _) = fixture.take_requests();
        let (method, payload, reply) = requests
            .into_iter()
            .find(|(method, _, _)| *method == secrets::CHECK_SECRET_SOURCE)
            .expect("checkSecretSource RPC sent");
        assert_eq!(method, secrets::CHECK_SECRET_SOURCE);
        assert_eq!(payload, json!({ "source": { "provider": "op-main", "reference": "op://Dev/OpenAI/api-key" } }));
        reply.send(Ok(json!({ "_tag": "Resolved" }))).unwrap();
        drive(&mut cx);
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();

        view.read_with(&cx, |view, _| {
            assert!(matches!(
                view.provider_form().secret_checks.get(&("op-main".into(), "op://Dev/OpenAI/api-key".into())),
                Some(SecretCheckStatus::Resolved)
            ));
        });

        if let Some(out) = std::env::var_os("T3_PARITY_386_SHOTS_DIR") {
            let out = std::path::PathBuf::from(out);
            std::fs::create_dir_all(&out).unwrap();
            cx.capture_screenshot(window.into()).unwrap().save(out.join("providers-secret-check.png")).unwrap();
        }

        // Config fields with acceptsSecretReference=true (e.g. antigravity.apiKey) load saved
        // references unmasked, check them, reject invalid 1Password refs, and commit new refs.
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.providers.selected = Some("antigravity".into());
                view.provider_form_mut().runtime_open = true;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                let api_key = view.provider_form().fields.iter().find(|f| f.key == "apiKey").expect("antigravity apiKey field synced");
                assert!(!api_key.masked, "saved secret reference config field must stay unmasked");
                assert_eq!(api_key.input.read(cx).value().as_ref(), "op://Dev/Gemini/api-key");
                api_key.input.update(cx, |input, cx| input.set_value("not-a-1p-ref", window, cx));
                view.commit_config_field("apiKey", window, cx);
                assert!(view.provider_form().error.is_some(), "invalid 1Password reference must surface error");
            });
        })
        .unwrap();
        assert!(fixture.take_requests().0.into_iter().all(|(m, _, _)| m != "server.updateSettings"));
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                let api_key = view.provider_form().fields.iter().find(|f| f.key == "apiKey").expect("apiKey field");
                api_key.input.update(cx, |input, cx| input.set_value("op://Dev/Gemini/next-key", window, cx));
                view.commit_config_field("apiKey", window, cx);
                assert!(view.provider_form().error.is_none());
            });
        })
        .unwrap();
        drive(&mut cx);
        let (requests, _) = fixture.take_requests();
        let (_, payload, reply) = requests.into_iter().find(|(m, _, _)| *m == "server.updateSettings").expect("updateSettings sent for apiKey secret ref");
        assert_eq!(payload["providerInstanceMutation"]["instanceId"], json!("antigravity"));
        assert_eq!(
            payload["providerInstanceMutation"]["instance"]["config"]["apiKey"],
            json!({ "provider": "op-main", "reference": "op://Dev/Gemini/next-key" })
        );
        let _ = reply.send(Ok(json!({})));
        drive(&mut cx);

        // Missing settings:write / legacy orchestration:operate blocks checking.
        fixture.grants.send(Some(crate::context_menu::project_filter_tests::session(false))).unwrap();
        drive(&mut cx);
        cx.update_window(window.into(), |_, _, cx| {
            view.update(cx, |view, cx| {
                view.check_secret_source("op-main".into(), "op://Dev/Other/token".into(), cx);
            });
        })
        .unwrap();
        drive(&mut cx);
        assert!(fixture.take_requests().0.into_iter().all(|(m, _, _)| m != secrets::CHECK_SECRET_SOURCE));

        // Missing capability hides check button and blocks checking even with permission.
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "orchestration:operate", "settings:write", "providers:manage", "environment:maintain"]
                }))
                .unwrap(),
            ))
            .unwrap();
        drive(&mut cx);
        state.update(&mut cx, |state, cx| {
            state.server_config.config.as_mut().unwrap().environment.capabilities.secret_source_check = Some(false);
            cx.emit(crate::app_state::AppEvent::ServerConfig);
        });
        cx.run_until_parked();
        cx.update_window(window.into(), |_, _, cx| {
            view.update(cx, |view, cx| {
                assert!(view.secret_check_button("probe-check".into(), "op-main", "op://Dev/OpenAI/api-key", false, cx).is_none());
                view.check_secret_source("op-main".into(), "op://Dev/Other/token".into(), cx);
            });
        })
        .unwrap();
        assert!(fixture.take_requests().0.into_iter().all(|(m, _, _)| m != secrets::CHECK_SECRET_SOURCE));

        state.update(&mut cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn config_secret_check_hides_on_uncommitted_change_and_keeps_stored_placeholder(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "orchestration:operate", "settings:write", "providers:manage", "environment:maintain"]
                }))
                .unwrap(),
            ))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": { "environmentId": "fixture-env", "label": "Studio Mac", "capabilities": { "secretProviderSettings": true, "secretSourceCheck": true } },
                        "secretProviderSettings": true,
                        "secretReferences": true,
                        "settings": {
                            "secretProviders": {
                                "op-main": { "kind": "one_password", "label": "Work 1Password", "config": { "account": "ops.1password.com" } }
                            },
                            "providerInstances": {
                                "antigravity": {
                                    "driver": "antigravity",
                                    "enabled": true,
                                    "displayName": "Antigravity Main",
                                    "config": { "apiKey": { "kind": "secret", "provider": "op-main", "reference": "op://Dev/Gemini/api-key" } }
                                },
                                "opencode": {
                                    "driver": "opencode",
                                    "enabled": true,
                                    "displayName": "OpenCode Main",
                                    "config": { "serverPassword": "stored-password" }
                                }
                            }
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx.open_window(size(px(1280.), px(900.)), |_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|_, cx| super::super::super::open(Section::Providers, cx));
        vcx.run_until_parked();
        let view = vcx.read(|cx| cx.global::<super::super::super::Host>().view.clone().expect("settings view open"));
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.providers.selected = Some("antigravity".into());
                view.provider_form_mut().runtime_open = true;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("secret-check-config-apiKey").is_some(), "saved config reference renders check button");

        // Uncommitted edit hides secret-check-config-apiKey immediately on InputEvent::Change without Blur/Enter.
        vcx.update(|window, cx| {
            let input = view.read(cx).provider_form().fields.iter().find(|f| f.key == "apiKey").unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("op://Dev/Gemini/unsaved", window, cx));
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("secret-check-config-apiKey").is_none(), "uncommitted config reference edit must hide check button immediately");

        // Reverting back to the committed reference restores secret-check-config-apiKey without Blur/Enter.
        vcx.update(|window, cx| {
            let input = view.read(cx).provider_form().fields.iter().find(|f| f.key == "apiKey").unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("op://Dev/Gemini/api-key", window, cx));
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("secret-check-config-apiKey").is_some(), "reverting uncommitted edit restores check button");

        // Switching stored secret -> 1Password -> Value restores the stored-secret placeholder.
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.providers.selected = Some("opencode".into());
                view.provider_form_mut().runtime_open = true;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear();
            view.update(cx, |view, cx| {
                view.select_config_secret_source("opencode", "serverPassword", Some("op-main".to_owned()), window, cx);
                assert_eq!(
                    view.provider_form().fields.iter().find(|f| f.key == "serverPassword").unwrap().placeholder,
                    "op://vault/item/field"
                );
                view.select_config_secret_source("opencode", "serverPassword", None, window, cx);
                assert_eq!(
                    view.provider_form().fields.iter().find(|f| f.key == "serverPassword").unwrap().placeholder,
                    "Stored secret, enter a new value to replace"
                );
            });
        });
        vcx.run_until_parked();
        state.update(&mut vcx, |state, _| state.stop_pump());
    }
}
