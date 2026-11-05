//! Runtime and environment controls from `ProviderSettingsForm.tsx` and `ProviderInstanceCard.tsx`.
use super::super::{
    SettingsView,
    controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, r, section_title},
    store,
};
use super::{Selected, flat_section};
use crate::{
    icons::Icon,
    theme::{hsla, palette},
};
use gpui::{AnyElement, App, AppContext, Context, Entity, Focusable, IntoElement, ParentElement, SharedString, Styled, Subscription, Window, div, prelude::*};
use gpui_component::{
    Sizable, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use serde_json::{Value, json};
use t3_core::{
    settings::{
        ServerSettingChange,
        providers::{self as model, form},
    },
    usage::PROVIDERS_MANAGE_SCOPE,
};

struct FieldInput {
    key: String,
    placeholder: String,
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
#[derive(Default)]
pub(super) struct FormState {
    instance_id: String,
    generation: usize,
    fields: Vec<FieldInput>,
    arguments: form::ArgumentsDraft,
    argument_inputs: Vec<ArgumentInput>,
    variables: Vec<VariableInput>,
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
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(field.placeholder.clone()).masked(field.secret));
            let generation = self.provider_form().generation;
            let subscription = cx.subscribe_in(&input, window, move |this, source, event: &InputEvent, window, cx| {
                if !this.form_event_current(generation, cx) || !this.provider_form().fields.iter().any(|field| field.key == key && &field.input == source) { return; }
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_config_field(&key, window, cx);
                }
            });
            self.provider_form_mut().fields.push(FieldInput {
                key: field.key.clone(),
                placeholder: field.placeholder.clone(),
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
            let saved = saved_value.as_str().unwrap_or("");
            let value = if field.secret { "" } else { saved };
            let placeholder = if field.secret && !saved.is_empty() {
                "Stored secret, enter a new value to replace"
            } else if field.secret {
                "Not set"
            } else {
                &field.placeholder
            };
            let changed = input.placeholder != placeholder;
            input.placeholder = placeholder.into();
            if changed || input.input.read(cx).value().as_ref() != value {
                input.input.update(cx, |input, cx| {
                    if input.value().as_ref() != value {
                        input.set_value(value, window, cx);
                    }
                    if changed {
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
        let Some(input) = self.provider_form().fields.iter().find(|f| f.key == key).map(|f| f.input.clone()) else {
            return;
        };
        let text = input.read(cx).value().to_string();
        let secret = form::fields(&instance.driver, instance.config.as_ref())
            .iter()
            .any(|f| f.key == key && f.secret);
        if secret && text.is_empty() {
            return;
        }
        let value = json!(text);
        if value.as_str().unwrap_or_default()
            == form::field_value(&instance, key)
                .as_str()
                .unwrap_or_default()
        {
            return;
        }
        if !form::validate_field(&instance.driver, key, &value) {
            self.provider_form_mut().error = Some("Compact after 100,000 to 1,000,000 tokens. Leave empty to use Claude's default.".into());
            cx.notify();
            return;
        }
        self.provider_form_mut().error = None;
        self.update_instance(&id, model::InstanceEdit::ConfigField { key: key.into(), value }, cx);
        if secret {
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
            store::update_server_then(
                &self.state,
                ServerSettingChange::ProviderInstance {
                    instance_id: id.clone(),
                    edit: model::InstanceEdit::ConfigField {
                        key: "commandArgs".into(),
                        value: json!(publication.values),
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
                                this.state.read(cx).server_config.config.as_ref().and_then(|c| c.settings.provider_instances.get(&id)).map(form::command_args)
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
                    let Some(input) = self.provider_form().fields.iter().find(|f| f.key == key).map(|f| f.input.clone()) else {
                        continue;
                    };
                    let clear_key = key.clone();
                    h_flex()
                        .gap(r(8.))
                        .child(div().w(r(224.)).child(Input::new(&input).small().disabled(disabled)))
                        .when(field.secret && saved.as_str().is_some_and(|v| !v.is_empty()), |el| {
                            el.child(
                                Button::new(SharedString::from(format!("clear-config-{key}")), ButtonVariant::GhostMuted, ButtonSize::IconSm)
                                    .icon(Icon::X)
                                    .tooltip(format!("Clear {}", field.label))
                                    .disabled(disabled)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.update_instance(
                                            &id,
                                            model::InstanceEdit::ConfigField {
                                                key: clear_key.clone(),
                                                value: json!(""),
                                            },
                                            cx,
                                        )
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

    fn variable_source(&self, index: usize, driver: &str, disabled: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let v = &self.provider_form().variables[index];
        if !form::variable_sensitive(driver, &v.raw) {
            return None;
        }
        let app = self.state.read(cx);
        let config = app.server_config.config.as_ref()?;
        let mut sources = if config.secret_references {
            config
                .settings
                .secret_providers
                .0
                .iter()
                .filter(|p| !matches!(t3_core::settings::secrets::classify(p), t3_core::settings::secrets::Classified::Unsupported))
                .map(|p| (p.id.clone(), p.label.clone()))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if sources.is_empty() && v.source.provider(&v.raw).is_none() {
            return None;
        }
        if let Some(source) = v.source.provider(&v.raw)
            && !sources.iter().any(|(id, _)| id == source)
        {
            sources.push((source.to_owned(), source.to_owned()));
        }
        let options = std::iter::once(SelectOption::new("__stored__", "Value"))
            .chain(sources.into_iter().map(|(id, label)| SelectOption::new(id, label)))
            .collect();
        let source = v.source.provider(&v.raw).unwrap_or("__stored__").to_owned();
        if disabled {
            return Some(div().text_xs().opacity(0.5).child(source).into_any_element());
        }
        let id = v.id;
        let driver = driver.to_owned();
        let view = cx.entity().downgrade();
        Some(
            Select::new(("provider-env-source", id), options)
                .selected(source)
                .width(128.)
                .on_change(move |source, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        let name = this.provider_form().variables.iter().find(|v| v.id == id).and_then(|v| v.raw["name"].as_str()).unwrap_or("");
                        if this.form_denial(&driver, &format!("environment.{name}"), cx).is_some() { return; }
                        let Some(v) = this.provider_form_mut().variables.iter_mut().find(|v| v.id == id) else {
                            return;
                        };
                        let stored = source.as_ref() == "__stored__";
                        let changed = v.source.select(&mut v.raw, (!stored).then(|| source.to_string()));
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
                    });
                })
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
            return h_flex()
                .gap(r(8.))
                .children(source)
                .child(div().w(r(224.)).child(value))
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
            .child(h_flex().gap(r(6.)).child(div().flex_1().min_w_0().child(value)).children(lock))
            .children(denial.map(|reason| div().text_xs().text_color(hsla(palette(cx).muted_foreground)).child(reason)))
            .into_any_element()
    }
}
