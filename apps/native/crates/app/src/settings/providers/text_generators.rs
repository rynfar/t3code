//! `TextGeneratorsSettings.tsx` (`pingdotgg/t3code#33147`): direct text-generation APIs (OpenAI,
//! OpenRouter, Anthropic, or custom OpenAI-compatible servers) with a 2-step Connect → Use it
//! wizard and a searchable models inspector.

use std::{collections::HashMap, ops::Range};

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, Subscription, Window, div, uniform_list, prelude::FluentBuilder,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::Value;
use t3_core::device::hosts::SecretValue;
use t3_core::model::ModelSelection;
use t3_core::server_config::ServerConfig;
use t3_core::settings::ServerSettingChange;
use t3_core::settings::providers::text_generators::{
    LIST_TEXT_GENERATOR_MODELS, ListModelsResult, TEXT_GENERATOR_PRESETS, TextGeneratorConfig, TextGeneratorKind, TextGeneratorModel, TextTask,
    check_config_payload, decode_text_generators, list_models_payload, next_text_generator_id, remove_text_generator_patch, test_model_payload,
    upsert_text_generator_patch,
};

use super::super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, DialogFrame, dialog_footer, group, muted_text, r, section, section_title};
use super::super::projects::SETTINGS_WRITE_SCOPE;
use super::super::source_control::{Tone, tone_badge};
use super::super::SettingsView;
use super::CloseProviderDialog;
use crate::components::motion::Spinner;
use crate::i18n::{t, t_fmt};
use crate::icons::Icon;
use crate::theme::{ThemeSlot, ThemeSlotExt, hsla, palette};

const STORED_SECRET: &str = "__stored__";
const KEEP_EXISTING_MODEL: &str = "__keep__";

#[derive(Clone, Copy, PartialEq, Eq)]
enum WizardStep {
    Connect,
    UseIt,
}

struct WizardState {
    focus: FocusHandle,
    is_new: bool,
    id: String,
    preset_id: &'static str,
    kind: TextGeneratorKind,
    enabled: bool,
    saved_api_key: SecretValue,
    source: String,
    step: WizardStep,
    label: Entity<InputState>,
    base_url: Entity<InputState>,
    api_key: Entity<InputState>,
    checking: bool,
    saving: bool,
    error: Option<String>,
    models: Vec<TextGeneratorModel>,
    testing_task: Option<TextTask>,
    task_results: HashMap<&'static str, Result<String, String>>,
    seq: u64,
    _subscriptions: Vec<Subscription>,
}

struct ModelsPanel {
    focus: FocusHandle,
    id: String,
    display_name: String,
    filter: Entity<InputState>,
    loading: bool,
    result: Option<Result<Vec<TextGeneratorModel>, String>>,
    testing_model: Option<String>,
    test_result: Option<Result<String, String>>,
    seq: u64,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, PartialEq, Eq)]
struct DialogOwner {
    environment: Option<t3_core::sidebar::grouping::EnvironmentOwner>,
    connection: Option<u64>,
}

#[derive(Default)]
pub(super) struct TextGeneratorsState {
    owner: Option<DialogOwner>,
    wizard: Option<WizardState>,
    models_panel: Option<ModelsPanel>,
}

impl TextGeneratorsState {
    pub(super) fn clear(&mut self) {
        self.owner = None;
        self.wizard = None;
        self.models_panel = None;
    }

    pub(super) fn inputs(&self) -> Vec<&Entity<InputState>> {
        let mut out = Vec::new();
        if let Some(wizard) = &self.wizard {
            out.push(&wizard.label);
            out.push(&wizard.base_url);
            out.push(&wizard.api_key);
        }
        if let Some(panel) = &self.models_panel {
            out.push(&panel.filter);
        }
        out
    }
}

fn secret_source_options(config: &ServerConfig) -> Vec<SelectOption> {
    std::iter::once(SelectOption::new(STORED_SECRET, "Stored secret"))
        .chain(config.settings.secret_providers.0.iter().map(|provider| SelectOption::new(provider.id.clone(), provider.label.clone())))
        .collect()
}

fn brand_label(brand: &str) -> &'static str {
    match brand {
        "openai" => "OpenAI",
        "openrouter" => "OpenRouter",
        "anthropic" => "Anthropic",
        "ollama" => "Ollama",
        _ => "OpenAI-compatible",
    }
}

fn models_button_disabled(enabled: bool) -> bool {
    !enabled
}

fn use_it_keep_label(current: Option<&ModelSelection>, generator_id: &str) -> String {
    match current {
        Some(sel) if sel.instance_id != generator_id => format!("Keep current ({}/{})", sel.instance_id, sel.model),
        _ => "Do not use for this task".to_owned(),
    }
}

/// Computes the server patch for a `UseIt` task picker change.
/// Returns `None` (no RPC) when keeping an assignment owned by a different provider so we never
/// overwrite another provider's task model with `null`.
fn use_it_task_change(current: Option<&ModelSelection>, generator_id: &str, picked: &str) -> Option<Option<ModelSelection>> {
    if picked == KEEP_EXISTING_MODEL {
        if current.is_some_and(|sel| sel.instance_id == generator_id) {
            Some(None)
        } else {
            None
        }
    } else {
        Some(Some(ModelSelection { instance_id: generator_id.to_owned(), model: picked.to_owned(), options: None }))
    }
}

/// Only offers a test model when the task actually targets this text generator.
fn use_it_test_model(selected_model: &str) -> Option<String> {
    (selected_model != KEEP_EXISTING_MODEL).then(|| selected_model.to_owned())
}

fn use_it_test_disabled(testing_any: bool, test_model: Option<&str>) -> bool {
    testing_any || test_model.is_none()
}

impl SettingsView {
    fn text_generator_owner(&self, cx: &App) -> DialogOwner {
        DialogOwner {
            environment: self.scoped_primary_owner(cx),
            connection: self.state.read(cx).connection_id(),
        }
    }

    fn text_generator_dialog_current(&self, cx: &App) -> bool {
        self.providers.text_generators.owner.as_ref() == Some(&self.text_generator_owner(cx))
            && self.section == t3_core::settings::search::Section::Providers
    }

    fn text_generator_request(&self, payload: Value, cx: &App) -> impl std::future::Future<Output = Result<ListModelsResult, String>> + use<> {
        let request = self.scoped_rpc(SETTINGS_WRITE_SCOPE, LIST_TEXT_GENERATOR_MODELS, payload, cx);
        let fallback = t(cx, "Could not load text generator models.").to_owned();
        async move {
            ListModelsResult::decode(request.await.map_err(|error| super::failure_text(&error, &fallback))?)
        }
    }

    fn close_text_generator_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.providers.text_generators.wizard.as_ref().is_some_and(|wizard| wizard.saving) {
            return;
        }
        self.providers.text_generators.clear();
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn open_text_generator_wizard(&mut self, existing: Option<(String, TextGeneratorConfig)>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.scoped_raw_representative_config(cx) else { return };
        let (is_new, id, preset_id, kind, enabled, saved_api_key, label_val, base_url_val) = match existing {
            Some((id, cfg)) => {
                let preset_id = TEXT_GENERATOR_PRESETS
                    .iter()
                    .find(|p| !p.base_url.is_empty() && p.base_url == cfg.base_url && p.kind == cfg.kind)
                    .map_or("custom", |p| p.id);
                (
                    false,
                    id,
                    preset_id,
                    cfg.kind,
                    cfg.enabled,
                    cfg.api_key,
                    cfg.label.unwrap_or_default(),
                    cfg.base_url,
                )
            }
            None => {
                let preset = TEXT_GENERATOR_PRESETS[0];
                let id = next_text_generator_id(preset.id, &config.settings, &config.providers);
                (
                    true,
                    id,
                    preset.id,
                    preset.kind,
                    true,
                    SecretValue::default(),
                    String::new(),
                    preset.base_url.to_owned(),
                )
            }
        };
        let (source, key_val, masked) = match &saved_api_key {
            SecretValue::Reference { provider, reference } => (provider.clone(), reference.clone(), false),
            SecretValue::Literal(_) => (STORED_SECRET.to_owned(), String::new(), true),
        };
        let label = cx.new(|cx| InputState::new(window, cx).default_value(label_val).placeholder("Optional display name"));
        let base_url = cx.new(|cx| InputState::new(window, cx).default_value(base_url_val).placeholder("https://api.openai.com/v1"));
        let api_key = cx.new(|cx| {
            let input = InputState::new(window, cx).default_value(key_val).placeholder(if !is_new && !saved_api_key.is_empty() && !saved_api_key.is_reference() {
                "Saved — leave blank to keep current key"
            } else {
                "API key (optional for local servers)"
            });
            if masked { input.masked(true) } else { input }
        });
        let subscriptions = [&label, &base_url, &api_key]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        let focus = cx.focus_handle();
        self.providers.text_generators.owner = Some(self.text_generator_owner(cx));
        window.defer(cx, {
            let focus = focus.clone();
            move |window, cx| focus.focus(window, cx)
        });
        self.providers.text_generators.models_panel = None;
        self.providers.text_generators.wizard = Some(WizardState {
            focus,
            is_new,
            id,
            preset_id,
            kind,
            enabled,
            saved_api_key,
            source,
            step: WizardStep::Connect,
            label,
            base_url,
            api_key,
            checking: false,
            saving: false,
            error: None,
            models: Vec::new(),
            testing_task: None,
            task_results: HashMap::new(),
            seq: 0,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn read_wizard_config(wizard: &WizardState, cx: &App) -> Result<TextGeneratorConfig, String> {
        let raw_key = wizard.api_key.read(cx).value().trim().to_owned();
        let api_key = if wizard.source == STORED_SECRET {
            if raw_key.is_empty() && !wizard.is_new && !wizard.saved_api_key.is_reference() {
                wizard.saved_api_key.clone()
            } else {
                SecretValue::Literal(raw_key)
            }
        } else {
            SecretValue::reference(&wizard.source, &raw_key).ok_or_else(|| "Enter a valid secret reference.".to_owned())?
        };
        let label = wizard.label.read(cx).value().trim().to_owned();
        TextGeneratorConfig {
            kind: wizard.kind,
            label: (!label.is_empty()).then_some(label),
            base_url: wizard.base_url.read(cx).value().to_string(),
            api_key,
            enabled: wizard.enabled,
        }
        .validate()
    }

    fn check_and_save_text_generator(&mut self, skip_check: bool, cx: &mut Context<Self>) {
        if !self.text_generator_dialog_current(cx) { return; }
        let Some(wizard) = self.providers.text_generators.wizard.as_mut() else { return };
        if wizard.checking || wizard.saving {
            return;
        }
        let cfg = match Self::read_wizard_config(wizard, cx) {
            Ok(cfg) => cfg,
            Err(err) => {
                wizard.error = Some(err);
                return cx.notify();
            }
        };
        wizard.error = None;
        let id = wizard.id.clone();
        if skip_check {
            self.save_text_generator_config(id, cfg, Vec::new(), cx);
            return;
        }
        wizard.seq += 1;
        let seq = wizard.seq;
        let focus = wizard.focus.clone();
        wizard.checking = true;
        cx.notify();
        let rpc = self.text_generator_request(check_config_payload(&id, &cfg), cx);
        cx.spawn(async move |this, cx| {
            let result = rpc.await;
            let _ = this.update(cx, |this, cx| {
                if !this.text_generator_dialog_current(cx) { return; }
                let Some(w) = this.providers.text_generators.wizard.as_mut() else { return };
                if w.seq != seq || w.focus != focus {
                    return;
                }
                w.checking = false;
                match result {
                    Ok(res) => {
                        this.save_text_generator_config(id, cfg, res.models, cx);
                    }
                    Err(err) => {
                        w.error = Some(err);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn save_text_generator_config(&mut self, id: String, cfg: TextGeneratorConfig, models: Vec<TextGeneratorModel>, cx: &mut Context<Self>) {
        let Some(wizard) = self.providers.text_generators.wizard.as_mut() else { return };
        wizard.saving = true;
        let focus = wizard.focus.clone();
        cx.notify();
        let view = cx.entity().downgrade();
        self.update_scoped_single_env_then(
            ServerSettingChange::Prepared(upsert_text_generator_patch(&id, &cfg)),
            move |error, cx| {
                cx.defer(move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        if !this.text_generator_dialog_current(cx) { return; }
                        let Some(w) = this.providers.text_generators.wizard.as_mut().filter(|w| w.focus == focus) else { return };
                        w.saving = false;
                        match error {
                            None => {
                                w.is_new = false;
                                w.saved_api_key = cfg.api_key;
                                w.models = models;
                                w.step = WizardStep::UseIt;
                            }
                            Some(err) => {
                                w.error = Some(err);
                            }
                        }
                        cx.notify();
                    });
                });
            },
            cx,
        );
    }

    fn test_wizard_task(&mut self, task: TextTask, model_slug: Option<String>, cx: &mut Context<Self>) {
        if !self.text_generator_dialog_current(cx) { return; }
        let Some(model_slug) = model_slug else { return };
        let Some(wizard) = self.providers.text_generators.wizard.as_mut() else { return };
        if wizard.testing_task.is_some() {
            return;
        }
        let id = wizard.id.clone();
        wizard.seq += 1;
        let seq = wizard.seq;
        let focus = wizard.focus.clone();
        wizard.testing_task = Some(task);
        cx.notify();
        let rpc = self.text_generator_request(test_model_payload(&id, Some(&model_slug)), cx);
        cx.spawn(async move |this, cx| {
            let result = rpc.await;
            let _ = this.update(cx, |this, cx| {
                if !this.text_generator_dialog_current(cx) { return; }
                let Some(w) = this.providers.text_generators.wizard.as_mut() else { return };
                if w.seq != seq || w.focus != focus {
                    return;
                }
                w.testing_task = None;
                let entry = match result {
                    Ok(res) => Ok(format!("Answered by {}", res.tested_model.unwrap_or_else(|| "model".to_owned()))),
                    Err(err) => Err(err),
                };
                w.task_results.insert(task.setting_key(), entry);
                cx.notify();
            });
        })
        .detach();
    }

    fn open_text_generator_models(&mut self, id: String, display_name: String, window: &mut Window, cx: &mut Context<Self>) {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter models…"));
        let subscriptions = vec![cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
        let focus = cx.focus_handle();
        self.providers.text_generators.owner = Some(self.text_generator_owner(cx));
        window.defer(cx, {
            let filter = filter.clone();
            move |window, cx| filter.update(cx, |input, cx| input.focus(window, cx))
        });
        self.providers.text_generators.wizard = None;
        self.providers.text_generators.models_panel = Some(ModelsPanel {
            focus,
            id,
            display_name,
            filter,
            loading: false,
            result: None,
            testing_model: None,
            test_result: None,
            seq: 0,
            _subscriptions: subscriptions,
        });
        self.fetch_text_generator_models(false, cx);
    }

    fn fetch_text_generator_models(&mut self, refresh: bool, cx: &mut Context<Self>) {
        if !self.text_generator_dialog_current(cx) { return; }
        let Some(panel) = self.providers.text_generators.models_panel.as_mut() else { return };
        panel.seq += 1;
        let seq = panel.seq;
        let focus = panel.focus.clone();
        let id = panel.id.clone();
        panel.loading = true;
        cx.notify();
        let rpc = self.text_generator_request(list_models_payload(&id, refresh), cx);
        cx.spawn(async move |this, cx| {
            let result = rpc.await;
            let _ = this.update(cx, |this, cx| {
                if !this.text_generator_dialog_current(cx) { return; }
                let Some(p) = this.providers.text_generators.models_panel.as_mut() else { return };
                if p.seq != seq || p.focus != focus {
                    return;
                }
                p.loading = false;
                p.result = Some(result.map(|r| r.models));
                cx.notify();
            });
        })
        .detach();
    }

    fn test_models_panel_model(&mut self, model_id: String, cx: &mut Context<Self>) {
        if !self.text_generator_dialog_current(cx) { return; }
        let Some(panel) = self.providers.text_generators.models_panel.as_mut() else { return };
        if panel.testing_model.is_some() { return; }
        let id = panel.id.clone();
        let focus = panel.focus.clone();
        panel.testing_model = Some(model_id.clone());
        panel.test_result = None;
        cx.notify();
        let rpc = self.text_generator_request(test_model_payload(&id, Some(&model_id)), cx);
        cx.spawn(async move |this, cx| {
            let result = rpc.await;
            let _ = this.update(cx, |this, cx| {
                if !this.text_generator_dialog_current(cx) { return; }
                let Some(p) = this.providers.text_generators.models_panel.as_mut().filter(|p| p.focus == focus) else { return };
                p.testing_model = None;
                p.test_result = Some(match result {
                    Ok(res) => Ok(format!("Answered by {}", res.tested_model.unwrap_or(model_id))),
                    Err(err) => Err(err),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_remove_text_generator(&mut self, id: String, label: String, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        crate::dialog::ask(
            format!("Remove \"{label}\"?"),
            "Any background text task using this generator will fall back to your default provider.",
            "Remove",
            move |_, cx| {
                let id = id.clone();
                let _ = view.update(cx, |this, cx| {
                    this.update_scoped_single_env(ServerSettingChange::Prepared(remove_text_generator_patch(&id)), cx);
                });
            },
            cx,
        );
    }

    pub(super) fn render_text_generators_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(config) = self.scoped_raw_representative_config(cx) else {
            return div().into_any_element();
        };
        let can_write = self.scoped_session(cx).is_none_or(|session| session.grants(SETTINGS_WRITE_SCOPE));
        if !self.text_generator_dialog_current(cx) {
            self.providers.text_generators.clear();
        }
        let modal = self.render_text_generator_wizard(&config, can_write, cx)
            .or_else(|| self.render_text_generator_models_panel(cx))
            .map(|frame| {
                let view = cx.entity().downgrade();
                div().key_context("ProviderDialog")
                    .on_action(cx.listener(|this, _: &CloseProviderDialog, window, cx| this.close_text_generator_dialog(window, cx)))
                    .child(frame.render_modal(window, 4, move |window, cx| {
                        let _ = view.update(cx, |this, cx| this.close_text_generator_dialog(window, cx));
                    }, cx))
            });

        let generators = decode_text_generators(&config.settings.text_generators);
        let add_btn = Button::new("add-text-generator", ButtonVariant::Outline, ButtonSize::Sm)
            .icon(Icon::Plus)
            .label("Add")
            .disabled(!can_write)
            .on_click(cx.listener(|this, _, window, cx| this.open_text_generator_wizard(None, window, cx)));

        let header = h_flex().justify_between().pr(r(8.)).child(section_title("Text generators")).child(add_btn);

        let body: AnyElement = if generators.is_empty() {
            group(
                vec![
                    div()
                        .px(r(16.))
                        .py(r(14.))
                        .child(muted_text(
                            "Connect a direct OpenAI, OpenRouter, Anthropic, or OpenAI-compatible API for fast thread titles, commit messages, and voice cleanup.",
                            cx,
                        ))
                        .into_any_element(),
                ],
                cx,
            )
            .into_any_element()
        } else {
            let rows: Vec<AnyElement> = generators
                .into_iter()
                .map(|(id, cfg)| {
                    let display = cfg.display_name();
                    let brand = brand_label(cfg.brand());
                    let desc = format!("{brand} · {}", cfg.base_url);
                    let toggle_id = id.clone();
                    let toggle_cfg = cfg.clone();
                    let toggle_view = cx.entity().downgrade();
                    let models_id = id.clone();
                    let models_name = display.clone();
                    let edit_id = id.clone();
                    let edit_cfg = cfg.clone();
                    let rm_id = id.clone();
                    let rm_label = display.clone();

                    Row::new(SharedString::from(format!("text-generator-{id}")), display)
                        .description(desc)
                        .control(
                            h_flex()
                                .gap(r(6.))
                                .child(
                                    Button::new(SharedString::from(format!("tg-models-{id}")), ButtonVariant::GhostMuted, ButtonSize::Compact)
                                        .label("Models")
                                        .disabled(!can_write || models_button_disabled(cfg.enabled))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_text_generator_models(models_id.clone(), models_name.clone(), window, cx)
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("tg-edit-{id}")), ButtonVariant::Outline, ButtonSize::Compact)
                                        .label("Edit")
                                        .disabled(!can_write)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_text_generator_wizard(Some((edit_id.clone(), edit_cfg.clone())), window, cx)
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("tg-rm-{id}")), ButtonVariant::GhostMuted, ButtonSize::Compact)
                                        .icon(Icon::Trash2)
                                        .tooltip("Remove text generator")
                                        .disabled(!can_write)
                                        .on_click(cx.listener(move |this, _, _, cx| this.confirm_remove_text_generator(rm_id.clone(), rm_label.clone(), cx))),
                                )
                                .child(
                                    Switch::new(SharedString::from(format!("tg-enabled-{id}")), cfg.enabled)
                                        .disabled(!can_write)
                                        .on_change(move |enabled, _, cx| {
                                            let next = TextGeneratorConfig { enabled, ..toggle_cfg.clone() };
                                            let _ = toggle_view.update(cx, |this, cx| {
                                                this.update_scoped_single_env(
                                                    ServerSettingChange::Prepared(upsert_text_generator_patch(&toggle_id, &next)),
                                                    cx,
                                                );
                                            });
                                        }),
                                ),
                        )
                        .into_any_element()
                })
                .collect();
            group(rows, cx).into_any_element()
        };

        v_flex().gap(r(10.)).child(header).child(body).children(modal).into_any_element()
    }

    fn render_text_generator_wizard(&self, config: &ServerConfig, can_write: bool, cx: &mut Context<Self>) -> Option<DialogFrame> {
        let wizard = self.providers.text_generators.wizard.as_ref()?;
        let p = palette(cx);
        let has_secret_providers = !config.settings.secret_providers.0.is_empty();
        let title = if wizard.is_new { "Add text generator" } else { "Edit text generator" };

        match wizard.step {
            WizardStep::Connect => {
                let presets = h_flex().flex_wrap().gap(r(6.)).children(TEXT_GENERATOR_PRESETS.into_iter().map(|preset| {
                    let active = wizard.preset_id == preset.id;
                    Button::new(
                        SharedString::from(format!("tg-preset-{}", preset.id)),
                        if active { ButtonVariant::Default } else { ButtonVariant::Outline },
                        ButtonSize::Compact,
                    )
                    .label(preset.label)
                    .disabled(wizard.checking || wizard.saving)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let Some(cfg) = this.scoped_raw_representative_config(cx) else { return };
                        if let Some(w) = this.providers.text_generators.wizard.as_mut() {
                            w.preset_id = preset.id;
                            w.kind = preset.kind;
                            if w.is_new {
                                w.id = next_text_generator_id(preset.id, &cfg.settings, &cfg.providers);
                            }
                            w.base_url.update(cx, |input, cx| input.set_value(preset.base_url.to_owned(), window, cx));
                            w.error = None;
                            cx.notify();
                        }
                    }))
                }));

                let view_source = cx.entity().downgrade();
                let source_picker = has_secret_providers.then(|| {
                    Select::new("tg-key-source", secret_source_options(config))
                        .selected(wizard.source.clone())
                        .width(156.)
                        .disabled(wizard.checking || wizard.saving)
                        .on_change(move |next, window, cx| {
                            let next_str = next.to_string();
                            let _ = view_source.update(cx, |this, cx| {
                                if let Some(w) = this.providers.text_generators.wizard.as_mut() {
                                    let is_stored = next_str == STORED_SECRET;
                                    w.source = next_str;
                                    w.api_key.update(cx, |input, cx| {
                                        input.set_masked(is_stored, window, cx);
                                        input.set_value(String::new(), window, cx);
                                    });
                                    cx.notify();
                                }
                            });
                        })
                });

                let rows = vec![
                    Row::new("tg-step-preset", "Provider").control(presets).into_any_element(),
                    Row::new("tg-step-label", "Name")
                        .description("Optional label shown in model pickers.")
                        .control(div().w(r(260.)).child(Input::new(&wizard.label).small().disabled(!can_write || wizard.checking || wizard.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                    Row::new("tg-step-url", "Base URL")
                        .description("API root, e.g. https://api.openai.com/v1 or http://127.0.0.1:11434/v1.")
                        .control(div().w(r(300.)).child(Input::new(&wizard.base_url).small().disabled(!can_write || wizard.checking || wizard.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                    Row::new("tg-step-key", "API key")
                        .description("Stored securely on the server or read from a secret provider.")
                        .below(
                            h_flex()
                                .mt(r(8.))
                                .pb(r(10.))
                                .gap(r(6.))
                                .children(source_picker)
                                .child(div().flex_1().min_w_0().child(Input::new(&wizard.api_key).small().disabled(!can_write || wizard.checking || wizard.saving).theme_slot(ThemeSlot::Input, cx))),
                        )
                        .into_any_element(),
                ];

                let busy = wizard.checking || wizard.saving;
                let has_error = wizard.error.is_some();
                let footer = h_flex()
                    .justify_between()
                    .gap(r(8.))
                    .child(div().text_xs().text_color(hsla(p.error_foreground)).children(wizard.error.clone()))
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(
                                Button::new("tg-cancel", ButtonVariant::Ghost, ButtonSize::Sm)
                                    .label("Cancel")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| this.close_text_generator_dialog(window, cx))),
                            )
                            .when(has_error, |row| {
                                row.child(
                                    Button::new("tg-save-anyway", ButtonVariant::Outline, ButtonSize::Sm)
                                        .label("Save without checking")
                                        .disabled(!can_write || busy)
                                        .on_click(cx.listener(|this, _, _, cx| this.check_and_save_text_generator(true, cx))),
                                )
                            })
                            .child(
                                Button::new("tg-check-save", ButtonVariant::Default, ButtonSize::Sm)
                                    .label(if wizard.checking {
                                        "Checking…"
                                    } else if wizard.saving {
                                        "Saving…"
                                    } else {
                                        "Check & Save"
                                    })
                                    .disabled(!can_write || busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.check_and_save_text_generator(false, cx))),
                            ),
                    );
                Some(DialogFrame {
                    id: "text-generator-wizard",
                    title: t(cx, title).into(),
                    description: "".into(),
                    max_width: 600.,
                    body: group(rows, cx).into_any_element(),
                    footer: dialog_footer(true, vec![div().w_full().child(footer).into_any_element()], cx).into_any_element(),
                    focus: wizard.focus.clone(),
                })
            }
            WizardStep::UseIt => {
                let id = wizard.id.clone();
                let mut rows: Vec<AnyElement> = Vec::new();
                for task in TextTask::ALL {
                    let current = task.current_selection(&config.settings);
                    let selected_model = current
                        .as_ref()
                        .filter(|sel| sel.instance_id == id)
                        .map(|sel| sel.model.clone())
                        .unwrap_or_else(|| KEEP_EXISTING_MODEL.to_owned());
                    let mut options = vec![SelectOption::new(KEEP_EXISTING_MODEL, use_it_keep_label(current.as_ref(), &id))];
                    for model in &wizard.models {
                        options.push(SelectOption::new(model.id.clone(), model.name.clone()));
                    }
                    if selected_model != KEEP_EXISTING_MODEL && !options.iter().any(|o| o.value.as_ref() == selected_model) {
                        options.push(SelectOption::new(selected_model.clone(), selected_model.clone()));
                    }
                    let task_view = cx.entity().downgrade();
                    let task_id = id.clone();
                    let current_for_change = current.clone();
                    let test_model = use_it_test_model(&selected_model);
                    let test_disabled = use_it_test_disabled(wizard.testing_task.is_some(), test_model.as_deref());
                    let testing = wizard.testing_task == Some(task);
                    let test_status = wizard.task_results.get(task.setting_key()).map(|res| match res {
                        Ok(msg) => tone_badge(msg.clone(), Tone::Success, cx),
                        Err(err) => tone_badge(err.clone(), Tone::Error, cx),
                    });
                    rows.push(
                        Row::new(SharedString::from(format!("tg-task-{}", task.setting_key())), task.label())
                            .description(task.description())
                            .control(
                                h_flex()
                                    .gap(r(6.))
                                    .children(test_status)
                                    .child(
                                        Select::new(SharedString::from(format!("tg-task-select-{}", task.setting_key())), options)
                                            .selected(selected_model)
                                            .width(220.)
                                            .disabled(!can_write)
                                            .on_change(move |value, _, cx| {
                                                let Some(selection) = use_it_task_change(current_for_change.as_ref(), &task_id, value.as_ref()) else {
                                                    return;
                                                };
                                                let _ = task_view.update(cx, |this, cx| {
                                                    this.update_scoped_single_env(
                                                        ServerSettingChange::Prepared(task.patch(selection.as_ref())),
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!("tg-task-test-{}", task.setting_key())), ButtonVariant::Outline, ButtonSize::Compact)
                                            .label(if testing { "Testing…" } else { "Test" })
                                            .disabled(test_disabled)
                                            .on_click(cx.listener(move |this, _, _, cx| this.test_wizard_task(task, test_model.clone(), cx))),
                                    ),
                            )
                            .into_any_element(),
                    );
                }
                let footer = h_flex()
                    .justify_end()
                    .gap(r(8.))
                    .child(
                        Button::new("tg-wizard-back", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(w) = this.providers.text_generators.wizard.as_mut() {
                                    w.step = WizardStep::Connect;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("tg-wizard-done", ButtonVariant::Default, ButtonSize::Sm)
                            .label("Done")
                            .on_click(cx.listener(|this, _, window, cx| this.close_text_generator_dialog(window, cx))),
                    );
                Some(DialogFrame {
                    id: "text-generator-wizard",
                    title: t_fmt(cx, "Use {name} for background tasks", &[("name", wizard.id.as_str().into())]).into(),
                    description: "".into(),
                    max_width: 620.,
                    body: section(t(cx, "Use it"), rows, cx).into_any_element(),
                    footer: dialog_footer(true, vec![footer.into_any_element()], cx).into_any_element(),
                    focus: wizard.focus.clone(),
                })
            }
        }
    }

    fn render_text_generator_models_panel(&self, cx: &mut Context<Self>) -> Option<DialogFrame> {
        let panel = self.providers.text_generators.models_panel.as_ref()?;
        let p = palette(cx);
        let query = panel.filter.read(cx).value().trim().to_lowercase();
        let status = panel.test_result.as_ref().map(|result| match result {
            Ok(message) => div().text_xs().text_color(hsla(p.success_foreground)).child(message.clone()),
            Err(error) => div().text_xs().text_color(hsla(p.error_foreground)).child(error.clone()),
        });
        let list = match &panel.result {
            None => div().py(r(16.)).child(Spinner::new("tg-models-loading", gpui::px(16.), hsla(p.muted_foreground))).into_any_element(),
            Some(Err(error)) => div().py(r(12.)).text_xs().text_color(hsla(p.error_foreground)).child(error.clone()).into_any_element(),
            Some(Ok(models)) => {
                let filtered: Vec<_> = models.iter().enumerate()
                    .filter(|(_, model)| query.is_empty() || model.id.to_lowercase().contains(&query) || model.name.to_lowercase().contains(&query))
                    .map(|(index, _)| index)
                    .collect();
                let count = filtered.len();
                if count == 0 {
                    div().py(r(14.)).child(muted_text(t(cx, "No matching models."), cx)).into_any_element()
                } else {
                    uniform_list("text-generator-models-list", count, cx.processor(
                        move |this, range: Range<usize>, _, cx| {
                            range.filter_map(|index| this.text_generator_model_row(*filtered.get(index)?, cx)).collect::<Vec<_>>()
                        },
                    ))
                    .h(r((count.min(8) * 40) as f32))
                    .into_any_element()
                }
            }
        };
        let footer = dialog_footer(true, vec![
            Button::new("tg-models-refresh", ButtonVariant::Outline, ButtonSize::Sm)
                .icon(Icon::RefreshCw)
                .label(t(cx, if panel.loading { "Refreshing…" } else { "Refresh" }))
                .disabled(panel.loading)
                .on_click(cx.listener(|this, _, _, cx| this.fetch_text_generator_models(true, cx)))
                .into_any_element(),
            Button::new("tg-models-done", ButtonVariant::Default, ButtonSize::Sm)
                .label(t(cx, "Done"))
                .on_click(cx.listener(|this, _, window, cx| this.close_text_generator_dialog(window, cx)))
                .into_any_element(),
        ], cx);
        Some(DialogFrame {
            id: "text-generator-models-dialog",
            title: t_fmt(cx, "{name} models", &[("name", panel.display_name.as_str().into())]).into(),
            description: "".into(),
            max_width: 448.,
            body: v_flex().gap(r(10.))
                .child(Input::new(&panel.filter).small().theme_slot(ThemeSlot::Input, cx))
                .children(status).child(list).into_any_element(),
            footer: footer.into_any_element(),
            focus: panel.focus.clone(),
        })
    }

    fn text_generator_model_row(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.providers.text_generators.models_panel.as_ref()?;
        let model = panel.result.as_ref()?.as_ref().ok()?.get(index)?;
        let id = model.id.clone();
        let testing = panel.testing_model.as_deref() == Some(id.as_str());
        let p = palette(cx);
        Some(h_flex()
            .id(("tg-model-row", index))
            .h(r(40.))
            .px(r(6.))
            .gap(r(10.))
            .child(v_flex().flex_1().min_w_0()
                .child(div().truncate().text_xs().font_weight(FontWeight::MEDIUM).child(model.name.clone()))
                .child(div().truncate().font_family(cx.theme().mono_font_family.clone())
                    .text_size(r(11.)).text_color(hsla(p.muted_foreground)).child(model.id.clone())))
            .child(Button::new(("tg-test-model", index), ButtonVariant::Outline, ButtonSize::Compact)
                .label(t(cx, if testing { "Testing…" } else { "Test" }))
                .disabled(panel.testing_model.is_some())
                .on_click(cx.listener(move |this, _, _, cx| this.test_models_panel_model(id.clone(), cx))))
            .into_any_element())
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_button_is_disabled_when_generator_is_disabled() {
        assert!(models_button_disabled(false));
        assert!(!models_button_disabled(true));
    }

    #[test]
    fn custom_preset_clears_base_url_to_empty() {
        let custom = TEXT_GENERATOR_PRESETS.iter().find(|p| p.id == "custom").expect("custom preset");
        assert_eq!(custom.base_url, "");
    }

    #[test]
    fn use_it_keep_option_preserves_other_provider_and_only_clears_owned_selection() {
        let other = ModelSelection { instance_id: "claudeAgent".into(), model: "claude-haiku-4-5".into(), options: None };
        let own = ModelSelection { instance_id: "openai".into(), model: "gpt-4.1-mini".into(), options: None };

        // Keeping while another provider owns the task sends no RPC (never overwrites with null).
        assert_eq!(use_it_task_change(Some(&other), "openai", KEEP_EXISTING_MODEL), None);
        assert_eq!(use_it_keep_label(Some(&other), "openai"), "Keep current (claudeAgent/claude-haiku-4-5)");

        // Keeping while nothing is set also sends no RPC.
        assert_eq!(use_it_task_change(None, "openai", KEEP_EXISTING_MODEL), None);
        assert_eq!(use_it_keep_label(None, "openai"), "Do not use for this task");

        // Unselecting when this generator owns the task sends Some(None).
        assert_eq!(use_it_task_change(Some(&own), "openai", KEEP_EXISTING_MODEL), Some(None));

        // Picking a model assigns this generator.
        assert_eq!(
            use_it_task_change(Some(&other), "openai", "gpt-4.1-mini"),
            Some(Some(ModelSelection { instance_id: "openai".into(), model: "gpt-4.1-mini".into(), options: None }))
        );
    }

    #[test]
    fn use_it_test_is_disabled_when_unassigned_or_no_model_selected() {
        let unassigned = use_it_test_model(KEEP_EXISTING_MODEL);
        assert_eq!(unassigned, None);
        assert!(use_it_test_disabled(false, unassigned.as_deref()));

        let assigned = use_it_test_model("gpt-4.1-mini");
        assert_eq!(assigned.as_deref(), Some("gpt-4.1-mini"));
        assert!(!use_it_test_disabled(false, assigned.as_deref()));
        assert!(use_it_test_disabled(true, assigned.as_deref()));
    }
}
