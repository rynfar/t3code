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
    pub(super) fn commit_focused_form_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self
            .providers
            .form
            .fields
            .iter()
            .filter(|f| f.input.focus_handle(cx).is_focused(window))
            .map(|f| f.key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            self.commit_config_field(&key, window, cx);
        }
        let arguments = self
            .providers
            .form
            .argument_inputs
            .iter()
            .filter(|a| a.input.focus_handle(cx).is_focused(window))
            .map(|a| a.id)
            .collect::<Vec<_>>();
        for id in arguments {
            self.commit_argument(id, cx);
        }
        let variables = self
            .providers
            .form
            .variables
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

    fn form_denial(&self, driver: &str, path: &str, cx: &App) -> Option<String> {
        if !self.may(PROVIDERS_MANAGE_SCOPE, cx) {
            return Some("This connection is read-only.".into());
        }
        let app = self.state.read(cx);
        let policy = app.server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref());
        policy
            .filter(|p| t3_core::managed_policy::instance_field_locked(Some(p), driver, path))
            .map(t3_core::managed_policy::instance_reason)
    }

    pub(super) fn sync_provider_form(&mut self, selected: &Selected, window: &mut Window, cx: &mut Context<Self>) {
        let instance = selected.row.instance();
        let switched = self.providers.form.instance_id != selected.row.instance_id;
        if switched {
            self.providers.form = FormState {
                instance_id: selected.row.instance_id.clone(),
                generation: self.providers.form.generation + 1,
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
            if !field.options.is_empty() || self.providers.form.fields.iter().any(|input| input.key == field.key) {
                continue;
            }
            let key = field.key.clone();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(field.placeholder.clone()).masked(field.secret));
            let subscription = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_config_field(&key, window, cx);
                }
            });
            self.providers.form.fields.push(FieldInput {
                key: field.key.clone(),
                placeholder: field.placeholder.clone(),
                input,
                _subscription: subscription,
            });
        }
        for input in &mut self.providers.form.fields {
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
        self.providers.form.arguments.reconcile(form::command_args(&instance));
        let rows = self.providers.form.arguments.rows.clone();
        self.providers.form.argument_inputs.retain(|input| rows.iter().any(|row| row.id == input.id));
        for row in rows {
            if let Some(input) = self.providers.form.argument_inputs.iter().find(|a| a.id == row.id) {
                if !input.input.focus_handle(cx).is_focused(window) && input.input.read(cx).value().as_ref() != row.value {
                    input.input.update(cx, |input, cx| input.set_value(row.value, window, cx));
                }
            } else {
                let id = row.id;
                let input = cx.new(|cx| InputState::new(window, cx).default_value(row.value));
                let subscription = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.commit_argument(id, cx);
                    }
                });
                self.providers.form.argument_inputs.push(ArgumentInput {
                    id,
                    input,
                    _subscription: subscription,
                });
            }
        }
        let incoming = form::environment(&instance);
        if switched || self.providers.form.seen != incoming {
            let echo = self.providers.form.published.as_ref() == Some(&incoming);
            self.providers.form.seen = incoming.clone();
            self.providers.form.published = None;
            if !echo {
                self.providers.form.variables.clear();
                for row in incoming {
                    self.add_variable_input(row, window, cx);
                }
            }
        }
        if let Some((name, _, _)) = form::dedicated_field(&instance.driver)
            && !self.providers.form.variables.iter().any(|v| v.raw["name"] == name)
        {
            self.add_variable_input(json!({"name":name,"value":"","sensitive":true}), window, cx);
        }
    }

    fn add_variable_input(&mut self, raw: Value, window: &mut Window, cx: &mut Context<Self>) {
        let driver = self.current_form_instance(cx).map(|i| i.driver).unwrap_or_default();
        let id = self.providers.form.next_id;
        self.providers.form.next_id += 1;
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
        let subscriptions = [(&name, false), (&value, true)]
            .into_iter()
            .map(|(input, is_value)| {
                cx.subscribe_in(input, window, move |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.commit_variable(id, is_value, window, cx);
                    }
                })
            })
            .collect();
        let source = form::SecretSourceDraft::default();
        self.providers.form.variables.push(VariableInput {
            id,
            source,
            raw,
            name,
            value,
            _subscriptions: subscriptions,
        });
    }

    fn commit_config_field(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected_instance_id(cx) else { return };
        if id != self.providers.form.instance_id {
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
        let Some(input) = self.providers.form.fields.iter().find(|f| f.key == key).map(|f| f.input.clone()) else {
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
        if !form::validate_field(&instance.driver, key, &value) {
            self.providers.form.error = Some("Compact after 100,000 to 1,000,000 tokens. Leave empty to use Claude's default.".into());
            cx.notify();
            return;
        }
        self.providers.form.error = None;
        self.update_instance(&id, model::InstanceEdit::ConfigField { key: key.into(), value }, cx);
        if secret {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    fn commit_argument(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(input) = self.providers.form.argument_inputs.iter().find(|a| a.id == id) else {
            return;
        };
        let value = input.input.read(cx).value().to_string();
        self.edit_arguments(form::ArgumentEdit::Value(id, value), cx);
    }
    fn edit_arguments(&mut self, edit: form::ArgumentEdit, cx: &mut Context<Self>) {
        let Some(instance) = self.current_form_instance(cx) else {
            return;
        };
        if self
            .form_denial(&instance.driver, "config.commandArgs", cx)
            .is_some()
        {
            return;
        }
        if let Some(publication) = self.providers.form.arguments.edit(edit) {
            let id = self.providers.form.instance_id.clone();
            let generation = self.providers.form.generation;
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
                                this.current_form_instance(cx)
                                    .map(|instance| form::command_args(&instance))
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

    fn current_form_instance(&self, cx: &App) -> Option<t3_core::server_config::ProviderInstanceConfig> {
        let app = self.state.read(cx);
        let c = app.server_config.config.as_ref()?;
        let id = &self.providers.form.instance_id;
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
        let Some(instance) = self.current_form_instance(cx) else { return };
        let Some(index) = self.providers.form.variables.iter().position(|v| v.id == id) else {
            return;
        };
        let variable = &self.providers.form.variables[index];
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
        let variable = &mut self.providers.form.variables[index];
        if is_value {
            if variable.source.provider(&variable.raw).is_some() {
                match variable.source.commit(&mut variable.raw, &next, one_password) {
                    Ok(false) => return,
                    Err(error) => {
                        self.providers.form.error = Some(error.into());
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
        let rows = self.providers.form.variables.iter().map(|v| v.raw.clone()).collect::<Vec<_>>();
        let Some(instance) = self.current_form_instance(cx) else { return };
        match form::publish_environment(&instance.driver, rows) {
            Ok(rows) => {
                self.providers.form.error = None;
                self.providers.form.published = Some(rows.clone());
                let id = self.providers.form.instance_id.clone();
                self.update_instance(&id, model::InstanceEdit::Environment(rows), cx);
            }
            Err(error) => self.providers.form.error = Some(error.into()),
        }
        cx.notify();
    }

    pub(super) fn render_runtime_form(&mut self, selected: &Selected, cx: &mut Context<Self>) -> Option<AnyElement> {
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
        let folded = managed && !self.providers.form.runtime_open;
        let mut rows = Vec::new();
        let header = managed.then(|| {
            h_flex().px(r(16.)).gap(r(8.)).child(div().flex_1().child(section_title("Runtime"))).child(
                Button::new("provider-runtime-expand", ButtonVariant::GhostMuted, ButtonSize::Compact)
                    .label(if folded { "Show more" } else { "Show less" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.providers.form.runtime_open = !this.providers.form.runtime_open;
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
                    let Some(input) = self.providers.form.fields.iter().find(|f| f.key == key).map(|f| f.input.clone()) else {
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
            let args = self.providers.form.arguments.rows.clone();
            let disabled = self.form_denial(&instance.driver, "config.commandArgs", cx).is_some();
            let mut controls = Vec::new();
            for (index, row) in args.into_iter().enumerate() {
                let id = row.id;
                let Some(input) = self.providers.form.argument_inputs.iter().find(|f| f.id == id).map(|f| f.input.clone()) else {
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
            rows.push(
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
                    .into_any_element(),
            );
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

    pub(super) fn render_dedicated_environment(&mut self, selected: &Selected, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (name, label, description) = form::dedicated_field(&selected.row.driver)?;
        let index = self.providers.form.variables.iter().position(|v| v.raw["name"] == name)?;
        let control = self.render_variable(index, true, &selected.row.driver, cx);
        Some(Row::new("provider-api-key", label).description(description).control(control).into_any_element())
    }

    pub(super) fn render_environment_form(&mut self, selected: &Selected, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut sections = Vec::new();
        let dedicated = form::dedicated_field(&selected.row.driver);
        let disabled = self.form_denial(&selected.row.driver, "environment", cx).is_some();
        let mut children = Vec::new();
        for index in 0..self.providers.form.variables.len() {
            if dedicated.is_some_and(|(name, _, _)| self.providers.form.variables[index].raw["name"] == name) {
                continue;
            }
            children.push(self.render_variable(index, false, &selected.row.driver, cx));
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
                            this.add_variable_input(json!({"name":"","value":"","sensitive":true}), window, cx);
                            cx.notify();
                        })),
                )
                .below(v_flex().gap(r(8.)).children(children))
                .into_any_element(),
        ];
        sections.push(flat_section("Environment", rows, cx).into_any_element());
        if let Some(error) = &self.providers.form.error {
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
        let v = &self.providers.form.variables[index];
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
                        let Some(v) = this.providers.form.variables.iter_mut().find(|v| v.id == id) else {
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
        let v = &self.providers.form.variables[index];
        let id = v.id;
        let redacted = v.raw["valueRedacted"] == true;
        let sensitive = v.raw["sensitive"] == true;
        let reference = v.raw["value"].is_object();
        let name = v.raw["name"].as_str().unwrap_or("").to_owned();
        let denial = self.form_denial(driver, &format!("environment.{name}"), cx);
        let disabled = denial.is_some();
        let name_control = (!dedicated).then(|| div().flex_1().min_w_0().child(Input::new(&v.name).small().disabled(disabled || redacted)));
        let source = self.variable_source(index, driver, disabled, cx);
        let v = &self.providers.form.variables[index];
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
                this.providers.form.variables.retain(|v| v.id != id);
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
                    if let Some(v) = this.providers.form.variables.iter_mut().find(|v| v.id == id) {
                        let next = v.raw["sensitive"] != true;
                        v.raw = form::edit_variable(&v.raw, form::VariableEdit::Sensitive(next));
                        v.value.update(cx, |input, cx| input.set_masked(next, window, cx));
                    }
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
