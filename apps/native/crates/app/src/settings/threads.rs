//! Threads: `ThreadsSettingsPanel` in `web/src/components/settings/SettingsPanels.tsx` with the
//! general rows of `ProjectDefaultsSettings.tsx`, `SnoozePresetsSettings.tsx`, and
//! `ThreadTitleInstructionsSettings.tsx`, at environment scope. Every row is server-backed.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveTime, Timelike, TimeZone};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, Focusable, IntoElement, ParentElement, SharedString, Styled,
    Subscription, Window, div,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{Sizable, h_flex, v_flex};
use t3_core::client_settings::TimestampFormat;
use t3_core::composer::{runtime_mode_description, runtime_mode_label};
use t3_core::model::ModelSelection;
use t3_core::providers;
use t3_core::server_config::{OptionDescriptor, RuntimeMode, ServerConfig, ThreadEnvMode};
use t3_core::settings::providers::text_generators::{
    LIST_TEXT_GENERATOR_MODELS, ListModelsResult, TextGeneratorModel, decode_text_generators, list_models_payload,
};
use t3_core::settings::snooze::{self, DurationUnit, SnoozeDay, SnoozeDraft, SnoozePreset};
use t3_core::settings::{
    AutoSettleDaysField, DEFAULT_AUTO_SETTLE_DAYS, ENV_MODES, ResponseStreamingMode, ServerSettingChange,
    effective_env_mode, resolve_default_text_generation_selection, text_generation_options,
    text_generation_selection_label,
};
use t3_core::sidebar::time::LocalTime;
use t3_core::timeline::TimestampLocale;
use t3_core::traits::{self, OptionValue};

use super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, muted_text, r, section};
use super::{Block, SettingsView, store};
use crate::composer::runtime_mode_icon;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::timestamp::SystemTimestampLocale;

const SELECTION_SEPARATOR: char = '\u{1f}';

type SnoozeEdit = Box<dyn FnOnce(&mut Vec<SnoozePreset>)>;

#[derive(Clone)]
struct SnoozeRowMemory {
    minutes: u32,
    time: String,
    chosen_unit: Option<DurationUnit>,
}

impl Default for SnoozeRowMemory {
    fn default() -> Self {
        Self { minutes: 60, time: "09:00".to_owned(), chosen_unit: None }
    }
}

pub(crate) struct ThreadsState {
    pub(crate) base_directory: Entity<InputState>,
    pub(crate) auto_settle_days: Entity<InputState>,
    auto_settle_field: AutoSettleDaysField,
    pub(crate) title_instructions: Entity<TextareaState>,
    title_instructions_saved: Option<String>,
    title_instructions_pending: Option<(String, u64)>,
    title_instructions_revision: u64,
    text_gen_advanced: bool,
    pub(super) text_gen_models: HashMap<String, Vec<TextGeneratorModel>>,
    text_gen_fetch_keys: HashMap<String, String>,
    time_inputs: Vec<(Entity<InputState>, Subscription)>,
    row_memory: Vec<SnoozeRowMemory>,
    /// The snooze editor's local copy, created from the saved options on first render.
    snooze: Option<SnoozeDraft>,
    _subscriptions: Vec<Subscription>,
}

impl ThreadsState {
    pub(super) fn time_inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        self.time_inputs.iter().map(|(input, _)| input)
    }

    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let base_directory = cx.new(|cx| InputState::new(window, cx).placeholder("~/"));
        let auto_settle_days = cx.new(|cx| InputState::new(window, cx));
        let title_instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 8)
                .placeholder("Keep titles to a few words. Lead with the main subject.")
        });
        let subscriptions = vec![
            cx.subscribe_in(&base_directory, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_base_directory(cx);
                }
            }),
            cx.subscribe_in(&auto_settle_days, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let value = input.read(cx).value().to_string();
                    if this.threads.auto_settle_field.draft != value {
                        this.threads.auto_settle_field.edit(value);
                    }
                }
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    this.commit_auto_settle_days(window, cx);
                }
                _ => {}
            }),
            cx.subscribe_in(&title_instructions, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Blur) {
                    this.commit_title_instructions(cx);
                }
            }),
        ];
        ThreadsState {
            base_directory,
            auto_settle_days,
            auto_settle_field: AutoSettleDaysField::new(DEFAULT_AUTO_SETTLE_DAYS),
            title_instructions,
            title_instructions_saved: None,
            title_instructions_pending: None,
            title_instructions_revision: 0,
            text_gen_advanced: false,
            text_gen_models: HashMap::new(),
            text_gen_fetch_keys: HashMap::new(),
            time_inputs: Vec::new(),
            row_memory: Vec::new(),
            snooze: None,
            _subscriptions: subscriptions,
        }
    }
}

/// The models a default-model select offers: every selectable provider instance, grouped. A
/// project leaves out the instances it turned off.
pub(super) fn model_options(config: &ServerConfig, project_id: Option<&str>) -> Vec<SelectOption> {
    providers::entries(config, project_id)
        .into_iter()
        .filter(|entry| entry.enabled && entry.provider.is_available())
        .flat_map(|entry| {
            let group: SharedString = entry.display_name.clone().into();
            providers::model_options(&config.settings, entry.provider).into_iter().map(move |model| {
                SelectOption::new(format!("{}{SELECTION_SEPARATOR}{}", entry.instance_id(), model.slug), providers::model_name(model).to_owned())
                    .group(group.clone())
            })
        })
        .collect()
}

pub(super) fn selection_value(selection: &ModelSelection) -> String {
    format!("{}{SELECTION_SEPARATOR}{}", selection.instance_id, selection.model)
}

pub(super) fn parse_selection(value: &str) -> Option<ModelSelection> {
    let (instance_id, model) = value.split_once(SELECTION_SEPARATOR)?;
    Some(ModelSelection { instance_id: instance_id.to_owned(), model: model.to_owned(), options: None })
}

pub(super) fn selection_label(config: &ServerConfig, selection: &ModelSelection) -> String {
    config
        .provider(&selection.instance_id)
        .and_then(|provider| provider.models.iter().find(|model| model.slug == selection.model))
        .map(|model| providers::trigger_model_name(model).to_owned())
        .unwrap_or_else(|| selection.model.clone())
}

/// `formatShortTimestamp` for a wake time, honoring the host locale when `Locale` is selected.
fn format_time(time: &DateTime<Local>, format: TimestampFormat) -> String {
    let local = LocalTime {
        year: i64::from(time.year()),
        month: i64::from(time.month()),
        day: i64::from(time.day()),
        hour: i64::from(time.hour()),
        minute: i64::from(time.minute()),
        second: i64::from(time.second()),
        millis: 0,
        weekday: i64::from(time.weekday().num_days_from_sunday()),
    };
    SystemTimestampLocale.time(local, format)
}

/// Renders effort/trait controls for a selected model (`TraitsPicker` in `SettingsPanels.tsx`).
pub(super) fn model_trait_controls(
    id_prefix: &'static str,
    config: &ServerConfig,
    selection: &ModelSelection,
    state: &gpui::Entity<crate::app_state::AppState>,
    change: fn(Option<ModelSelection>) -> ServerSettingChange,
) -> Vec<AnyElement> {
    let Some(provider) = config.provider(&selection.instance_id) else {
        return Vec::new();
    };
    let model = provider.models.iter().find(|candidate| candidate.slug == selection.model);
    let explicit = traits::selections(selection.options.as_ref());
    let resolved = traits::composer_options(&provider.driver, model, false, &explicit);
    let descriptors = resolved.descriptors;
    let mut controls = Vec::new();
    for descriptor in &descriptors {
        let OptionDescriptor::Select(select) = descriptor else {
            continue;
        };
        let options: Vec<SelectOption> = traits::selectable_choices(select)
            .map(|choice| {
                let mut option = SelectOption::new(choice.id.clone(), choice.label.clone());
                if let Some(description) = &choice.description {
                    option = option.description(description.clone());
                }
                option
            })
            .collect();
        if options.is_empty() {
            continue;
        }
        let selected = select
            .current_value
            .clone()
            .or_else(|| options.first().map(|option| option.value.to_string()))
            .unwrap_or_default();
        let all_descriptors = descriptors.clone();
        let option_id = select.id.clone();
        let instance_id = selection.instance_id.clone();
        let model_slug = selection.model.clone();
        let state = state.clone();
        controls.push(
            Select::new(SharedString::from(format!("{id_prefix}-trait-{}", select.id)), options)
                .selected(selected)
                .width(124.)
                .popup_width(180.)
                .show_check()
                .on_change(move |value, _, cx| {
                    if let Some(next_selections) =
                        traits::with_value(&all_descriptors, &option_id, OptionValue::Str(value.to_string()))
                    {
                        let next = traits::model_selection(&instance_id, &model_slug, &next_selections);
                        store::update_server(&state, change(Some(next)), cx);
                    }
                })
                .into_any_element(),
        );
    }
    controls
}

/// `resolveSnoozePreset` against the local clock: (id, label, when, wake minute).
fn resolve(preset: &SnoozePreset, now: DateTime<Local>, format: TimestampFormat) -> Option<(String, String, String, i64)> {
    let days_ahead = preset.days_ahead(now.weekday().num_days_from_sunday());
    let wake = match preset {
        SnoozePreset::After { minutes } => now + ChronoDuration::minutes(i64::from(*minutes)),
        SnoozePreset::At { time, .. } => {
            let (hour, minute) = snooze::parse_time(time)?;
            let date = now.date_naive() + ChronoDuration::days(i64::from(days_ahead));
            let wake = Local.from_local_datetime(&date.and_time(NaiveTime::from_hms_opt(hour, minute, 0)?)).earliest()?;
            if wake <= now {
                return None;
            }
            wake
        }
    };
    let label = preset.label(days_ahead);
    let names_day = matches!(preset, SnoozePreset::At { .. }) && label != "Next week";
    let time = format_time(&wake, format);
    let when = if names_day || wake.date_naive() == now.date_naive() { time } else { format!("{} {time}", wake.format("%a")) };
    Some((preset.id(), label, when, wake.timestamp() / 60))
}

impl SettingsView {
    pub(super) fn thread_settings<'a>(&self, cx: &'a App) -> Option<&'a ServerConfig> {
        self.state.read(cx).server_config.config.as_ref()
    }

    /// Ensures enabled text generators on `config` have their model lists fetched via
    /// `server.listTextGeneratorModels` (`useTextGenerationTargets` in `textGenerators.ts`).
    pub(super) fn sync_text_generator_models(&mut self, config: &ServerConfig, cx: &mut Context<Self>) {
        let env_id = &config.environment.environment_id;
        let generators = decode_text_generators(&config.settings.text_generators);
        let active_ids: std::collections::HashSet<&String> =
            generators.iter().filter(|(_, cfg)| cfg.enabled).map(|(id, _)| id).collect();
        self.threads.text_gen_fetch_keys.retain(|id, _| active_ids.contains(id));
        self.threads.text_gen_models.retain(|id, _| active_ids.contains(id));

        for (id, gen_cfg) in generators {
            if !gen_cfg.enabled {
                continue;
            }
            let cache_key = format!(
                "{env_id}\u{1f}{id}\u{1f}{}\u{1f}{}\u{1f}{}",
                gen_cfg.kind.as_str(),
                gen_cfg.base_url,
                gen_cfg.api_key.encode()
            );
            if self.threads.text_gen_fetch_keys.get(&id).is_some_and(|existing| existing == &cache_key) {
                continue;
            }
            self.threads.text_gen_fetch_keys.insert(id.clone(), cache_key.clone());
            let payload = list_models_payload(&id, false);
            let task = self.state.read(cx).rpc(LIST_TEXT_GENERATOR_MODELS, payload);
            let id_owned = id.clone();
            cx.spawn(async move |this, cx| {
                let result = task.await.ok().and_then(|value| ListModelsResult::decode(value).ok());
                let _ = this.update(cx, |this, cx| {
                    if this.threads.text_gen_fetch_keys.get(&id_owned).is_some_and(|k| k == &cache_key)
                        && let Some(decoded) = result
                    {
                        this.threads.text_gen_models.insert(id_owned, decoded.models);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    pub(super) fn text_gen_select_options(
        &self,
        config: &ServerConfig,
        project_id: Option<&str>,
        selections: &[Option<&ModelSelection>],
    ) -> Vec<SelectOption> {
        text_generation_options(config, project_id, &self.threads.text_gen_models, selections)
            .into_iter()
            .map(|opt| {
                SelectOption::new(
                    format!("{}{SELECTION_SEPARATOR}{}", opt.instance_id, opt.model_slug),
                    opt.model_label,
                )
                .group(SharedString::from(opt.group_label))
            })
            .collect()
    }

    pub(super) fn text_gen_selection_label(&self, config: &ServerConfig, selection: &ModelSelection) -> String {
        text_generation_selection_label(config, selection, &self.threads.text_gen_models)
    }

    pub(super) fn sync_input(input: &Entity<InputState>, value: &str, window: &mut Window, cx: &mut App) {
        if !input.focus_handle(cx).is_focused(window) && input.read(cx).value().as_ref() != value {
            let value = value.to_owned();
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
    }

    pub(super) fn threads_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let Some(config) = self.thread_settings(cx).cloned() else {
            return vec![Block::new(&[], muted_text("Reconnect to change this environment's settings.", cx))];
        };
        let settings = &config.settings;
        let threads = &settings.threads;
        let state = self.state.clone();
        let update = move |change: ServerSettingChange| {
            let state = state.clone();
            move |cx: &mut App| store::update_server(&state, change.clone(), cx)
        };

        Self::sync_input(&self.threads.base_directory, &threads.add_project_base_directory, window, cx);
        self.sync_auto_settle_days(threads.sidebar_auto_settle_after_days, window, cx);
        self.sync_title_instructions(&threads.thread_title_instructions, window, cx);

        let ghq_state = self.state.clone();
        let new_projects = section(
            "New projects",
            vec![
                Row::new("add-project-starts-in", "Add project starts in")
                    .description("Leave empty to use \"~/\" when the Add Project browser opens.")
                    .highlighted(self.is_target("add-project-starts-in"))
                    .reset(!threads.add_project_base_directory.is_empty(), {
                        let reset = update(ServerSettingChange::AddProjectBaseDirectory(String::new()));
                        move |_, _, cx| reset(cx)
                    })
                    .control(div().w(r(288.)).child(Input::new(&self.threads.base_directory).small()))
                    .into_any_element(),
                Row::new("ghq-clone-layout", "Clone into host folders")
                    .description("Clones go to github.com/owner/repo under the folder above, like ghq.")
                    .highlighted(self.is_target("ghq-clone-layout"))
                    .reset(threads.ghq_clone_layout, {
                        let reset = update(ServerSettingChange::GhqCloneLayout(false));
                        move |_, _, cx| reset(cx)
                    })
                    .control(Switch::new("ghq-clone-layout-switch", threads.ghq_clone_layout).on_change(move |checked, _, cx| {
                        store::update_server(&ghq_state, ServerSettingChange::GhqCloneLayout(checked), cx);
                    }))
                    .into_any_element(),
            ],
            cx,
        );

        let options = model_options(&config, None);
        let selection = settings
            .default_model_selection
            .clone()
            .filter(|selection| providers::instance_enabled(settings, None, &selection.instance_id))
            .or_else(|| providers::project_default_selection(&providers::entries(&config, None)));
        let automatic = settings.default_model_selection.is_none();
        let model_state = self.state.clone();
        let model_row = Row::new("default-model", "Model")
            .description(if automatic {
                "Default model for new threads, selected automatically. Projects can override it."
            } else {
                "Default model for new threads. Projects can override it."
            })
            .highlighted(self.is_target("default-model"))
            .reset(!automatic, {
                let reset = update(ServerSettingChange::DefaultModelSelection(None));
                move |_, _, cx| reset(cx)
            });
        let model_row = match &selection {
            Some(selection) if !options.is_empty() => {
                let trait_pickers = model_trait_controls(
                    "default-model",
                    &config,
                    selection,
                    &self.state,
                    ServerSettingChange::DefaultModelSelection,
                );
                model_row.control(
                    h_flex()
                        .flex_wrap()
                        .justify_end()
                        .gap(r(6.))
                        .child(
                            Select::new("default-model-select", options.clone())
                                .selected(selection_value(selection))
                                .label(selection_label(&config, selection))
                                .width(220.)
                                .popup_width(260.)
                                .show_check()
                                .on_change(move |value, _, cx| {
                                    if let Some(selection) = parse_selection(&value) {
                                        store::update_server(&model_state, ServerSettingChange::DefaultModelSelection(Some(selection)), cx);
                                    }
                                }),
                        )
                        .children(trait_pickers),
                )
            }
            _ => model_row.control(muted_text("No providers available", cx)),
        };

        let fallback_state = self.state.clone();
        let fallback = settings.fallback_model_selection.clone().or(selection.clone());
        let fallback_row = Row::new("fallback-model", "Fallback model")
            .description("Used for new threads when the default model's provider is out of usage.")
            .highlighted(self.is_target("fallback-model"))
            .reset(settings.fallback_model_selection.is_some(), {
                let reset = update(ServerSettingChange::FallbackModelSelection(None));
                move |_, _, cx| reset(cx)
            });
        let fallback_row = match &fallback {
            Some(shown) if !options.is_empty() => {
                let trait_pickers = settings
                    .fallback_model_selection
                    .as_ref()
                    .map(|explicit| {
                        model_trait_controls(
                            "fallback-model",
                            &config,
                            explicit,
                            &self.state,
                            ServerSettingChange::FallbackModelSelection,
                        )
                    })
                    .unwrap_or_default();
                fallback_row.control(
                    h_flex()
                        .flex_wrap()
                        .justify_end()
                        .gap(r(6.))
                        .child(
                            Select::new("fallback-model-select", options.clone())
                                .selected(selection_value(shown))
                                .label(if settings.fallback_model_selection.is_none() { "None".to_owned() } else { selection_label(&config, shown) })
                                .width(220.)
                                .popup_width(260.)
                                .show_check()
                                .on_change(move |value, _, cx| {
                                    if let Some(selection) = parse_selection(&value) {
                                        store::update_server(&fallback_state, ServerSettingChange::FallbackModelSelection(Some(selection)), cx);
                                    }
                                }),
                        )
                        .children(trait_pickers),
                )
            }
            _ => fallback_row.control(muted_text("No providers available", cx)),
        };

        let mode_state = self.state.clone();
        let permissions_row = Row::new("default-permissions", "Permissions")
            .description("Default permissions for new threads. Projects can override them.")
            .highlighted(self.is_target("default-permissions"))
            .reset(settings.default_runtime_mode != RuntimeMode::default(), {
                let reset = update(ServerSettingChange::DefaultRuntimeMode(RuntimeMode::default()));
                move |_, _, cx| reset(cx)
            })
            .control(
                Select::new(
                    "default-permissions-select",
                    RuntimeMode::ALL
                        .iter()
                        .map(|mode| {
                            SelectOption::new(mode.as_str(), runtime_mode_label(*mode))
                                .description(runtime_mode_description(*mode))
                                .icon(runtime_mode_icon(*mode))
                        })
                        .collect(),
                )
                .selected(settings.default_runtime_mode.as_str())
                .popup_width(256.)
                .on_change(move |value, _, cx| {
                    if let Some(mode) = RuntimeMode::parse(&value) {
                        store::update_server(&mode_state, ServerSettingChange::DefaultRuntimeMode(mode), cx);
                    }
                }),
            );

        let workspace_state = self.state.clone();
        let workspace_row = Row::new("new-threads", "Workspace")
            .description("Where new threads start. Projects and their t3.json can override it.")
            .highlighted(self.is_target("new-threads"))
            .reset(settings.default_thread_env_mode.is_some(), {
                let reset = update(ServerSettingChange::DefaultThreadEnvMode(None));
                move |_, _, cx| reset(cx)
            })
            .control(
                Select::new("default-workspace-select", ENV_MODES.iter().map(|(mode, label)| SelectOption::new(env_mode_value(*mode), *label)).collect())
                    .selected(env_mode_value(effective_env_mode(settings)))
                    .show_check()
                    .on_change(move |value, _, cx| {
                        let mode = match value.as_ref() {
                            "local" => ThreadEnvMode::Local,
                            "worktree" => ThreadEnvMode::Worktree,
                            _ => return,
                        };
                        store::update_server(&workspace_state, ServerSettingChange::DefaultThreadEnvMode(Some(mode)), cx);
                    }),
            );

        let new_threads = section(
            "New threads",
            vec![model_row.into_any_element(), fallback_row.into_any_element(), permissions_row.into_any_element(), workspace_row.into_any_element()],
            cx,
        );

        let snooze_section = section("Snooze", vec![self.snooze_row(&threads.snooze_presets, window, cx).into_any_element()], cx);

        let switch = |id: &'static str, checked: bool, change: fn(bool) -> ServerSettingChange| {
            let state = self.state.clone();
            Switch::new(id, checked).on_change(move |checked, _, cx| store::update_server(&state, change(checked), cx))
        };
        let usage_limits = section(
            "Usage limits",
            vec![
                Row::new("auto-resume-limited-threads", "Resume after usage limits")
                    .description("Automatically resume threads when their usage limits reset. You can cancel auto-resume in each thread.")
                    .highlighted(self.is_target("auto-resume-limited-threads"))
                    .control(switch("auto-resume-switch", threads.auto_resume_limited_threads, ServerSettingChange::AutoResumeLimitedThreads))
                    .into_any_element(),
                Row::new("snooze-limited-threads", "Snooze until limits reset")
                    .description("Snooze threads until their usage limits reset. Enable auto-resume to continue the agent after the reset.")
                    .highlighted(self.is_target("snooze-limited-threads"))
                    .control(switch("snooze-limited-switch", threads.snooze_limited_threads, ServerSettingChange::SnoozeLimitedThreads))
                    .into_any_element(),
            ],
            cx,
        );

        let days = threads.sidebar_auto_settle_after_days;
        let mut auto_settle_rows = vec![
            Row::new("auto-settle-merged-threads", "Auto-settle merged threads")
                .description("Settle a thread when its pull request merges. Closed pull requests still settle automatically.")
                .highlighted(self.is_target("auto-settle-merged-threads"))
                .reset(!threads.sidebar_auto_settle_on_merge, {
                    let reset = update(ServerSettingChange::AutoSettleOnMerge(true));
                    move |_, _, cx| reset(cx)
                })
                .control(switch("auto-settle-merged-switch", threads.sidebar_auto_settle_on_merge, ServerSettingChange::AutoSettleOnMerge))
                .into_any_element(),
            Row::new("auto-settle-inactive-threads", "Auto-settle inactive threads")
                .description("Sidebar threads with no activity for this long settle automatically.")
                .highlighted(self.is_target("auto-settle-inactive-threads"))
                .reset(days != Some(DEFAULT_AUTO_SETTLE_DAYS), {
                    let reset = update(ServerSettingChange::AutoSettleAfterDays(Some(DEFAULT_AUTO_SETTLE_DAYS)));
                    move |_, _, cx| reset(cx)
                })
                .control(switch("auto-settle-inactive-switch", days.is_some(), |checked| {
                    ServerSettingChange::AutoSettleAfterDays(checked.then_some(DEFAULT_AUTO_SETTLE_DAYS))
                }))
                .into_any_element(),
        ];
        if days.is_some() {
            auto_settle_rows.push(
                Row::new("days-before-auto-settle", "Days of inactivity before auto-settle")
                    .description("Any new activity un-settles a thread automatically.")
                    .control(div().w(r(96.)).child(Input::new(&self.threads.auto_settle_days).small()))
                    .into_any_element(),
            );
        }
        if days.is_some() && config.environment.capabilities.thread_auto_settlement_scope == Some(true) {
            auto_settle_rows.push(
                Row::new("keep-threads-with-pull-requests", "Keep threads with a PR")
                    .description("Inactivity does not settle threads with a pull request. Merge and close still do.")
                    .control(switch("keep-pull-requests-switch", threads.sidebar_auto_settle_keep_pull_requests, ServerSettingChange::AutoSettleKeepPullRequests))
                    .into_any_element(),
            );
        }
        let auto_settle = section("Auto-settle", auto_settle_rows, cx);

        self.sync_text_generator_models(&config, cx);
        let saved_text_gen = t3_core::settings::text_generation_model_selection(settings);
        let text_gen_selection =
            resolve_default_text_generation_selection(&config, None, &self.threads.text_gen_models);
        let text_gen_options =
            self.text_gen_select_options(&config, None, &[saved_text_gen.as_ref(), text_gen_selection.as_ref()]);
        let text_gen_state = self.state.clone();
        let text_gen_row = Row::new("text-generation-model", "Text generation model")
            .description("For thread titles and other generated text. Source control has its own setting.")
            .highlighted(self.is_target("text-generation-model"))
            .reset(saved_text_gen.is_some(), {
                let reset = update(ServerSettingChange::TextGenerationModelSelection(None));
                move |_, _, cx| reset(cx)
            });
        let text_gen_row = match &text_gen_selection {
            Some(shown) if !text_gen_options.is_empty() => {
                let trait_pickers = model_trait_controls(
                    "text-generation-model",
                    &config,
                    shown,
                    &self.state,
                    ServerSettingChange::TextGenerationModelSelection,
                );
                let label = self.text_gen_selection_label(&config, shown);
                text_gen_row.control(
                    h_flex()
                        .flex_wrap()
                        .justify_end()
                        .gap(r(6.))
                        .child(
                            Select::new("text-generation-model-select", text_gen_options)
                                .selected(selection_value(shown))
                                .label(label)
                                .width(220.)
                                .popup_width(260.)
                                .show_check()
                                .on_change(move |value, _, cx| {
                                    if let Some(selection) = parse_selection(&value) {
                                        store::update_server(
                                            &text_gen_state,
                                            ServerSettingChange::TextGenerationModelSelection(Some(selection)),
                                            cx,
                                        );
                                    }
                                }),
                        )
                        .children(trait_pickers),
                )
            }
            _ => text_gen_row.control(muted_text("No text generation providers available.", cx)),
        };

        let text_gen_advanced = self.threads.text_gen_advanced
            || self.is_target("thread-title-instructions")
            || self.is_target("response-streaming");
        let weak_advanced = cx.entity().downgrade();
        let mut text_gen_rows = vec![
            text_gen_row.into_any_element(),
            div()
                .px(r(16.))
                .py(r(8.))
                .child(
                    Button::new("threads-text-gen-advanced", ButtonVariant::GhostMuted, ButtonSize::Sm)
                        .icon(if text_gen_advanced { Icon::ChevronDown } else { Icon::ChevronRight })
                        .label("Advanced")
                        .on_click(move |_, _, cx| {
                            let _ = weak_advanced.update(cx, |view, cx| {
                                view.threads.text_gen_advanced = !text_gen_advanced;
                                if view.is_target("thread-title-instructions") || view.is_target("response-streaming") {
                                    view.target = None;
                                }
                                cx.notify();
                            });
                        }),
                )
                .into_any_element(),
        ];
        if text_gen_advanced {
            let streaming = threads.response_streaming_mode;
            let streaming_state = self.state.clone();
            let reset_instructions_view = cx.entity().downgrade();
            text_gen_rows.push(
                Row::new("thread-title-instructions", "Thread title instructions")
                    .description("Appended to the prompt for new and regenerated thread titles.")
                    .highlighted(self.is_target("thread-title-instructions"))
                    .reset(!threads.thread_title_instructions.is_empty(), {
                        let reset = update(ServerSettingChange::ThreadTitleInstructions(String::new()));
                        move |_, _, cx| {
                            let _ = reset_instructions_view.update(cx, |view, _| {
                                view.threads.title_instructions_pending = None;
                                view.threads.title_instructions_saved = None;
                            });
                            reset(cx);
                        }
                    })
                    .below(
                        div()
                            .mt(r(12.))
                            .pb(r(12.))
                            .max_w(r(672.))
                            .child(Textarea::new(&self.threads.title_instructions)),
                    )
                    .into_any_element(),
            );
            text_gen_rows.push(
                Row::new("response-streaming", "Response streaming")
                    .description(streaming.description())
                    .highlighted(self.is_target("response-streaming"))
                    .reset(streaming != ResponseStreamingMode::default(), {
                        let reset = update(ServerSettingChange::ResponseStreamingMode(ResponseStreamingMode::default()));
                        move |_, _, cx| reset(cx)
                    })
                    .control(
                        Select::new(
                            "response-streaming-select",
                            ResponseStreamingMode::ALL.iter().map(|mode| SelectOption::new(streaming_value(*mode), mode.label())).collect(),
                        )
                        .selected(streaming_value(streaming))
                        .width(224.)
                        .on_change(move |value, _, cx| {
                            let mode = match value.as_ref() {
                                "turn" => ResponseStreamingMode::Turn,
                                "paragraph" => ResponseStreamingMode::Paragraph,
                                _ => return,
                            };
                            store::update_server(&streaming_state, ServerSettingChange::ResponseStreamingMode(mode), cx);
                        }),
                    )
                    .into_any_element(),
            );
        }
        let text_generation = section("Text generation", text_gen_rows, cx);

        vec![
            Block::new(&["new-projects", "add-project-starts-in", "ghq-clone-layout"], new_projects),
            Block::new(&["project-defaults", "default-model", "fallback-model", "default-permissions", "new-threads"], new_threads),
            Block::new(&["snooze", "snooze-options"], snooze_section),
            Block::new(&["usage-limits", "auto-resume-limited-threads", "snooze-limited-threads"], usage_limits),
            Block::new(
                &[
                    "auto-settle",
                    "auto-settle-merged-threads",
                    "auto-settle-inactive-threads",
                    "days-before-auto-settle",
                    "keep-threads-with-pull-requests",
                ],
                auto_settle,
            ),
            Block::new(&["text-generation", "text-generation-model", "thread-title-instructions", "response-streaming"], text_generation),
        ]
    }

    fn commit_base_directory(&mut self, cx: &mut Context<Self>) {
        let value = self.threads.base_directory.read(cx).value().trim().to_owned();
        let saved = self.thread_settings(cx).map(|config| config.settings.threads.add_project_base_directory.clone());
        if saved.as_deref() != Some(value.as_str()) {
            store::update_server(&self.state, ServerSettingChange::AddProjectBaseDirectory(value), cx);
        }
    }

    /// Keeps `title_instructions` in step with the server without clobbering an in-flight blur
    /// commit before the `ServerConfig` echo arrives.
    fn sync_title_instructions(&mut self, saved: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.threads.title_instructions_pending.as_ref().is_some_and(|(pending, _)| pending == saved) {
            self.threads.title_instructions_pending = None;
            self.threads.title_instructions_saved = Some(saved.to_owned());
        }
        if self.threads.title_instructions_pending.is_some() {
            return;
        }
        if self.threads.title_instructions_saved.as_deref() != Some(saved)
            && !self.threads.title_instructions.focus_handle(cx).is_focused(window)
        {
            self.threads.title_instructions_saved = Some(saved.to_owned());
            if self.threads.title_instructions.read(cx).value().as_ref() != saved {
                let value = saved.to_owned();
                self.threads.title_instructions.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
    }

    fn commit_title_instructions(&mut self, cx: &mut Context<Self>) {
        let value = self.threads.title_instructions.read(cx).value().trim().to_owned();
        let saved = self.thread_settings(cx).map(|config| config.settings.threads.thread_title_instructions.clone());
        let target = self
            .threads
            .title_instructions_pending
            .as_ref()
            .map(|(pending, _)| pending.as_str())
            .or(saved.as_deref());
        if target == Some(value.as_str()) {
            return;
        }
        self.threads.title_instructions_revision += 1;
        let revision = self.threads.title_instructions_revision;
        self.threads.title_instructions_pending = Some((value.clone(), revision));
        let view = cx.entity().downgrade();
        let on_failed = Box::new(move |cx: &mut App| {
            let _ = view.update(cx, |this, cx| {
                if this.threads.title_instructions_pending.as_ref().is_some_and(|(_, rev)| *rev == revision) {
                    this.threads.title_instructions_pending = None;
                    this.threads.title_instructions_saved = None;
                    cx.notify();
                }
            });
        });
        store::update_server_with(&self.state, ServerSettingChange::ThreadTitleInstructions(value), Some(on_failed), cx);
    }

    fn sync_auto_settle_days(&mut self, saved: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(days) = saved {
            self.threads.auto_settle_field.sync(days);
            if !self.threads.auto_settle_field.dirty
                && self.threads.auto_settle_days.read(cx).value().as_ref() != self.threads.auto_settle_field.draft
            {
                let draft = self.threads.auto_settle_field.draft.clone();
                self.threads.auto_settle_days.update(cx, |input, cx| input.set_value(draft, window, cx));
            }
        }
    }

    /// Commits the deferred auto-settle days draft on Enter, Blur, or modal close (`pingdotgg/t3code#36303`).
    fn commit_auto_settle_days(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (draft, submit) = self.threads.auto_settle_field.commit();
        if self.threads.auto_settle_days.read(cx).value().as_ref() != draft {
            self.threads.auto_settle_days.update(cx, |input, cx| input.set_value(draft, window, cx));
        }
        let Some((days, revision)) = submit else {
            return;
        };
        let view = cx.entity().downgrade();
        let on_failed = Box::new(move |cx: &mut App| {
            let _ = view.update(cx, |this, cx| {
                let _ = this.threads.auto_settle_field.fail(revision);
                cx.notify();
            });
        });
        store::update_server_with(&self.state, ServerSettingChange::AutoSettleAfterDays(Some(days)), Some(on_failed), cx);
    }

    /// Escape while editing auto-settle days cancels the dirty draft without closing Settings.
    pub(super) fn dismiss_auto_settle_days(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.threads.auto_settle_days.focus_handle(cx).is_focused(window) && self.threads.auto_settle_field.dirty {
            let restored = self.threads.auto_settle_field.cancel();
            self.threads.auto_settle_days.update(cx, |input, cx| input.set_value(restored, window, cx));
            cx.notify();
            return true;
        }
        false
    }

    /// Saves a typed wake time, or puts the saved one back when it does not parse.
    fn commit_time_input(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.threads.time_inputs.get(index).map(|(input, _)| input.clone()) else { return };
        let Some(SnoozePreset::At { day, time }) = self.threads.snooze.as_ref().and_then(|draft| draft.presets().get(index).cloned()) else { return };
        let typed = input.read(cx).value().trim().to_owned();
        if snooze::parse_time(&typed).is_some() && typed != time {
            self.edit_snooze(
                |presets| {
                    if let Some(slot) = presets.get_mut(index) {
                        *slot = SnoozePreset::At { day, time: typed };
                    }
                },
                cx,
            );
        } else {
            input.update(cx, |input, cx| input.set_value(time, window, cx));
        }
    }

    /// The Threads page's commit-on-blur fields, committed before the modal closes.
    pub(super) fn commit_focused_thread_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.threads.base_directory.focus_handle(cx).is_focused(window) {
            self.commit_base_directory(cx);
        }
        if self.threads.auto_settle_days.focus_handle(cx).is_focused(window) {
            self.commit_auto_settle_days(window, cx);
        }
        if self.threads.title_instructions.focus_handle(cx).is_focused(window) {
            self.commit_title_instructions(cx);
        }
        if let Some(index) = self.threads.time_inputs.iter().position(|(input, _)| input.focus_handle(cx).is_focused(window)) {
            self.commit_time_input(index, window, cx);
        }
    }

    /// Edits the snooze draft and saves the whole list; a failed save puts the saved list back.
    fn edit_snooze(&mut self, change: impl FnOnce(&mut Vec<SnoozePreset>), cx: &mut Context<Self>) {
        let Some(draft) = self.threads.snooze.as_mut() else { return };
        let mut next = draft.presets().to_vec();
        change(&mut next);
        if !draft.edit(next.clone()) {
            return;
        }
        let view = cx.entity().downgrade();
        let on_failed = Box::new(move |cx: &mut App| {
            let _ = view.update(cx, |this, cx| {
                if let Some(draft) = this.threads.snooze.as_mut() {
                    draft.failed();
                }
                cx.notify();
            });
        });
        store::update_server_with(&self.state, ServerSettingChange::SnoozePresets(next), Some(on_failed), cx);
        cx.notify();
    }

    fn snooze_row(&mut self, saved: &[SnoozePreset], window: &mut Window, cx: &mut Context<Self>) -> Row {
        let p = palette(cx);
        let format = store::client(cx).timestamp_format();
        let now = Local::now();
        let draft = self.threads.snooze.get_or_insert_with(|| SnoozeDraft::new(saved));
        draft.observe(saved);
        let presets = draft.presets().to_vec();
        let presets = presets.as_slice();
        self.threads.row_memory.resize_with(presets.len(), SnoozeRowMemory::default);
        self.sync_time_inputs(presets, window, cx);

        let view = cx.entity().downgrade();
        let save = move |change: SnoozeEdit, cx: &mut App| {
            let _ = view.update(cx, |this, cx| this.edit_snooze(change, cx));
        };
        let mut seen_ids: Vec<String> = Vec::new();
        let mut seen_wakes: Vec<i64> = Vec::new();
        let mut list = v_flex().gap(r(6.)).pt(r(12.)).pb(r(8.));
        for (index, preset) in presets.iter().enumerate() {
            let preview: (bool, SharedString, Option<SharedString>) = match resolve(preset, now, format) {
                None => (false, "Passed for today".into(), None),
                Some((id, ..)) if seen_ids.contains(&id) => (false, "Duplicate of an option above".into(), None),
                Some((_, _, _, wake)) if seen_wakes.contains(&wake) => (false, "Same time as an option above".into(), None),
                Some((id, label, when, wake)) => {
                    seen_ids.push(id);
                    seen_wakes.push(wake);
                    (true, label.into(), Some(when.into()))
                }
            };
            let replace = {
                let save = save.clone();
                move |next: SnoozePreset, cx: &mut App| {
                    save(
                        Box::new(move |presets| {
                            if let Some(slot) = presets.get_mut(index) {
                                *slot = next;
                            }
                        }),
                        cx,
                    )
                }
            };
            let remove = {
                let save = save.clone();
                let view = cx.entity().downgrade();
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ = view.update(cx, |this, _| {
                        if index < this.threads.row_memory.len() {
                            this.threads.row_memory.remove(index);
                        }
                    });
                    save(
                        Box::new(move |presets| {
                            if index < presets.len() {
                                presets.remove(index);
                            }
                        }),
                        cx,
                    )
                }
            };
            list = list.child(
                h_flex()
                    .flex_wrap()
                    .gap(r(8.))
                    .child(self.snooze_fields(index, preset, replace, cx))
                    .child(
                        h_flex()
                            .ml_auto()
                            .min_w_0()
                            .gap(r(8.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(match preview {
                                (true, label, when) => h_flex()
                                    .gap(r(8.))
                                    .child(div().text_color(hsla(p.foreground)).child(label))
                                    .children(when.map(|when| div().child(when))),
                                (false, text, _) => h_flex().child(text),
                            })
                            .child(
                                Button::new(("remove-snooze", index), ButtonVariant::GhostMuted, ButtonSize::IconSm)
                                    .icon(crate::icons::Icon::X)
                                    .tooltip(format!("Remove snooze option {}", index + 1))
                                    .on_click(remove),
                            ),
                    ),
            );
        }
        let mut menu: Vec<(String, String, i64)> =
            presets.iter().filter_map(|preset| resolve(preset, now, format)).map(|(_, label, when, wake)| (label, when, wake)).collect();
        menu.sort_by_key(|(_, _, wake)| *wake);
        menu.dedup_by_key(|(_, _, wake)| *wake);
        let labels: Vec<&str> = menu.iter().map(|(label, ..)| label.as_str()).collect();
        let menu_text = if menu.is_empty() {
            "Menus offer only Custom\u{2026}".to_owned()
        } else {
            let items: Vec<String> = menu
                .iter()
                .map(|(label, when, _)| if labels.iter().filter(|other| **other == label).count() > 1 { format!("{label} {when}") } else { label.clone() })
                .collect();
            format!("Menu now: {}", items.join(" \u{00b7} "))
        };
        let full = presets.len() >= snooze::MAX_PRESETS;
        let add = {
            let save = save.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                save(
                    Box::new(|presets| {
                        if presets.len() < snooze::MAX_PRESETS {
                            presets.push(snooze::next_candidate(presets));
                        }
                    }),
                    cx,
                )
            }
        };
        list = list.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(menu_text)).child(
            h_flex()
                .gap(r(8.))
                .child(Button::new("add-snooze-option", ButtonVariant::GhostMuted, ButtonSize::Compact).icon(crate::icons::Icon::Plus).label("Add option").disabled(full).on_click(add))
                .children(full.then(|| div().text_xs().text_color(hsla(p.muted_foreground)).child(format!("Up to {} options", snooze::MAX_PRESETS)))),
        );
        let defaults = snooze::default_presets();
        let reset_view = cx.entity().downgrade();
        Row::new("snooze-options", "Snooze options")
            .description("Choices in every snooze menu, listed soonest first. Weekdays mean the next one after today.")
            .highlighted(self.is_target("snooze-options"))
            .reset(!snooze::same_presets(presets, &defaults), move |_, _, cx| {
                let _ = reset_view.update(cx, |this, _| this.threads.row_memory.clear());
                let defaults = defaults.clone();
                save(Box::new(move |presets| *presets = defaults), cx)
            })
            .below(list)
    }

    fn snooze_fields(&self, index: usize, preset: &SnoozePreset, replace: impl Fn(SnoozePreset, &mut App) + Clone + 'static, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let memory = self.threads.row_memory.get(index).cloned().unwrap_or_default();
        let when_options = std::iter::once(SelectOption::new("after", "In"))
            .chain(SnoozeDay::ALL.iter().map(|day| SelectOption::new(day.as_str(), day.label())))
            .collect();
        let when_value = match preset {
            SnoozePreset::After { .. } => "after",
            SnoozePreset::At { day, .. } => day.as_str(),
        };
        let when_preset = preset.clone();
        let when_replace = replace.clone();
        let when_view = cx.entity().downgrade();
        let mem_minutes = memory.minutes;
        let mem_time = memory.time.clone();
        let when = Select::new(("snooze-when", index), when_options).selected(when_value).width(128.).on_change(move |value, _, cx| {
            let day = SnoozeDay::ALL.iter().copied().find(|day| day.as_str() == value.as_ref());
            if value.as_ref() != "after" && day.is_none() {
                return;
            }
            let next = snooze::with_when(&when_preset, day, mem_minutes, &mem_time);
            let _ = when_view.update(cx, |this, _| {
                if let Some(slot) = this.threads.row_memory.get_mut(index) {
                    match &when_preset {
                        SnoozePreset::After { minutes } if matches!(next, SnoozePreset::At { .. }) => slot.minutes = *minutes,
                        SnoozePreset::At { time, .. } if matches!(next, SnoozePreset::After { .. }) => slot.time = time.clone(),
                        _ => {}
                    }
                }
            });
            when_replace(next, cx);
        });
        let fields = h_flex().gap(r(8.)).child(when);
        match preset {
            SnoozePreset::After { minutes } => {
                let minutes = *minutes;
                let unit = memory
                    .chosen_unit
                    .filter(|chosen| minutes.is_multiple_of(chosen.minutes()))
                    .unwrap_or_else(|| snooze::split_minutes(minutes).1);
                let amount = minutes / unit.minutes();
                let max_amount = snooze::MAX_PRESET_MINUTES / unit.minutes();
                let (minus, plus, units) = (replace.clone(), replace.clone(), replace);
                let unit_view = cx.entity().downgrade();
                fields
                    .child(
                        h_flex()
                            .h(r(28.))
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.input))
                            .child(
                                Button::new(("snooze-decrease", index), ButtonVariant::Ghost, ButtonSize::IconSm)
                                    .icon(crate::icons::Icon::Minus)
                                    .tooltip("Decrease")
                                    .disabled(amount <= 1)
                                    .on_click(move |_, _, cx| {
                                        let next = amount.saturating_sub(1).max(1) * unit.minutes();
                                        minus(SnoozePreset::After { minutes: next }, cx);
                                    }),
                            )
                            .child(div().w(r(40.)).text_center().text_sm().child(amount.to_string()))
                            .child(
                                Button::new(("snooze-increase", index), ButtonVariant::Ghost, ButtonSize::IconSm)
                                    .icon(crate::icons::Icon::Plus)
                                    .tooltip("Increase")
                                    .disabled(amount >= max_amount)
                                    .on_click(move |_, _, cx| {
                                        let next = (amount + 1).min(max_amount) * unit.minutes();
                                        plus(SnoozePreset::After { minutes: next }, cx);
                                    }),
                            ),
                    )
                    .child(
                        Select::new(
                            ("snooze-unit", index),
                            DurationUnit::ALL
                                .iter()
                                .map(|candidate| SelectOption::new(candidate.plural(), if amount == 1 { candidate.singular() } else { candidate.plural() }))
                                .collect(),
                        )
                        .selected(unit.plural())
                        .width(112.)
                        .on_change(move |value, _, cx| {
                            if let Some(next_unit) = DurationUnit::ALL.iter().copied().find(|u| u.plural() == value.as_ref()) {
                                let _ = unit_view.update(cx, |this, _| {
                                    if let Some(slot) = this.threads.row_memory.get_mut(index) {
                                        slot.chosen_unit = Some(next_unit);
                                    }
                                });
                                let next_minutes = (amount.saturating_mul(next_unit.minutes())).min(snooze::MAX_PRESET_MINUTES);
                                units(SnoozePreset::After { minutes: next_minutes }, cx);
                            }
                        }),
                    )
                    .into_any_element()
            }
            SnoozePreset::At { .. } => fields
                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("at"))
                .children(self.threads.time_inputs.get(index).map(|(input, _)| div().w(r(96.)).child(Input::new(input).small())))
                .into_any_element(),
        }
    }

    /// One time field per `at` row, created as rows appear and kept in step with the saved time.
    fn sync_time_inputs(&mut self, presets: &[SnoozePreset], window: &mut Window, cx: &mut Context<Self>) {
        while self.threads.time_inputs.len() < presets.len() {
            let index = self.threads.time_inputs.len();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("09:00"));
            let subscription = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_time_input(index, window, cx);
                }
            });
            self.threads.time_inputs.push((input, subscription));
        }
        self.threads.time_inputs.truncate(presets.len());
        for (preset, (input, _)) in presets.iter().zip(&self.threads.time_inputs) {
            if let SnoozePreset::At { time, .. } = preset {
                Self::sync_input(input, time, window, cx);
            }
        }
    }
}

pub(super) fn env_mode_value(mode: ThreadEnvMode) -> &'static str {
    match mode {
        ThreadEnvMode::Local => "local",
        ThreadEnvMode::Worktree => "worktree",
    }
}

fn streaming_value(mode: ResponseStreamingMode) -> &'static str {
    match mode {
        ResponseStreamingMode::Turn => "turn",
        ResponseStreamingMode::Paragraph => "paragraph",
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use gpui::{Render, div, prelude::*};
    use serde_json::json;
    use t3_core::settings::search::Section;

    use super::*;

    struct EmptyFrame;
    impl Render for EmptyFrame {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn auto_settle_days_defers_save_cancels_on_escape_and_rolls_back_on_failure(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let drive = |vcx: &mut gpui::VisualTestContext| {
            vcx.run_until_parked();
            runtime.block_on(async {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            });
            vcx.run_until_parked();
        };
        cx.executor().allow_parking();
        let (state, mut fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "settings:write"]
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
                        "environment": { "environmentId": "fixture-env", "label": "Mac" },
                        "settings": { "sidebarAutoSettleAfterDays": 7 }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx.add_window(|_, _| EmptyFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let view = vcx.update(|window, cx| cx.new(|cx| SettingsView::new(state.clone(), Section::Threads, None, window, cx)));
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.sync_auto_settle_days(Some(7), window, cx);
            });
        });

        // Editing the draft must not send an immediate RPC.
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let focus = view.threads.auto_settle_days.focus_handle(cx);
                window.focus(&focus, cx);
                view.threads.auto_settle_days.update(cx, |input, cx| input.set_value("14", window, cx));
                view.threads.auto_settle_field.edit("14".into());
            });
        });
        drive(&mut vcx);
        assert!(fixture.take_requests().0.into_iter().all(|(m, _, _)| m != "server.updateSettings"));

        // Escape cancels dirty draft without losing view state.
        vcx.update(|window, cx| {
            let handled = view.update(cx, |view, cx| view.dismiss_auto_settle_days(window, cx));
            assert!(handled);
            assert_eq!(view.read(cx).threads.auto_settle_days.read(cx).value().as_ref(), "7");
        });

        // Re-edit and commit emits server.updateSettings; failure rolls back to saved "7".
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.threads.auto_settle_days.update(cx, |input, cx| input.set_value("14", window, cx));
                view.threads.auto_settle_field.edit("14".into());
                view.commit_auto_settle_days(window, cx);
            });
        });
        drive(&mut vcx);
        let (requests, _) = fixture.take_requests();
        let (_, payload, reply) = requests.into_iter().find(|(m, _, _)| *m == "server.updateSettings").expect("auto-settle save sent");
        assert_eq!(payload["patch"]["sidebarAutoSettleAfterDays"], json!(14));
        let _ = reply.send(Err(t3_client::session::RpcError::Closed));
        drive(&mut vcx);
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.sync_auto_settle_days(Some(7), window, cx);
            });
        });
        view.read_with(&vcx, |view, cx| {
            assert_eq!(view.threads.auto_settle_field.draft, "7");
            assert_eq!(view.threads.auto_settle_days.read(cx).value().as_ref(), "7");
        });

        // Thread title instructions blur commit must not be overwritten before the server echo lands,
        // and must roll back to the saved server value when the save fails.
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.sync_title_instructions("Old style", window, cx);
                assert_eq!(view.threads.title_instructions.read(cx).value().as_ref(), "Old style");
                view.threads.title_instructions.update(cx, |input, cx| input.set_value("Use conventional style", window, cx));
                view.commit_title_instructions(cx);
                // Simulated unfocused re-render before ServerConfig echo arrives:
                view.sync_title_instructions("Old style", window, cx);
                assert_eq!(view.threads.title_instructions.read(cx).value().as_ref(), "Use conventional style");
            });
        });
        drive(&mut vcx);
        let (requests, _) = fixture.take_requests();
        let (_, payload, reply) = requests
            .into_iter()
            .find(|(m, _, _)| *m == "server.updateSettings")
            .expect("title instructions save sent");
        assert_eq!(payload["patch"]["threadTitleInstructions"], json!("Use conventional style"));
        let _ = reply.send(Err(t3_client::session::RpcError::Closed));
        drive(&mut vcx);
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.sync_title_instructions("Old style", window, cx);
                assert_eq!(view.threads.title_instructions.read(cx).value().as_ref(), "Old style");
            });
        });

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(view);
        vcx.run_until_parked();
        drop(vcx);
        drop(fixture);
        drop(runtime);
    }

    #[gpui::test]
    fn text_generation_picker_filters_supports_text_generation_and_loads_text_generators(
        cx: &mut gpui::TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let drive = |vcx: &mut gpui::VisualTestContext| {
            vcx.run_until_parked();
            runtime.block_on(async {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            });
            vcx.run_until_parked();
        };
        cx.executor().allow_parking();
        let (state, mut fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.run_until_parked();
        let server_cfg: ServerConfig = serde_json::from_value(json!({
            "environment": { "environmentId": "fixture-env", "label": "Mac" },
            "providers": [
                {
                    "instanceId": "codex",
                    "driver": "codex",
                    "displayName": "Codex",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "models": [
                        { "slug": "gpt-5", "name": "GPT-5" },
                        { "slug": "no-text", "name": "No Text", "supportsTextGeneration": false }
                    ]
                },
                {
                    "instanceId": "fx",
                    "driver": "fx",
                    "displayName": "fx",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "supportsTextGeneration": false,
                    "models": [{ "slug": "fx-1", "name": "fx 1" }]
                }
            ],
            "settings": {
                "providerInstances": {
                    "codex": { "driver": "codex", "enabled": true },
                    "fx": { "driver": "fx", "enabled": true }
                },
                "textGenerators": {
                    "or-fast": {
                        "kind": "openaiCompatible",
                        "label": "OpenRouter Fast",
                        "baseUrl": "https://openrouter.ai/api/v1",
                        "apiKey": "__REDACTED__",
                        "enabled": true
                    }
                }
            }
        }))
        .unwrap();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(server_cfg.clone());
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx.add_window(|_, _| EmptyFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let view = vcx.update(|window, cx| cx.new(|cx| SettingsView::new(state.clone(), Section::Threads, None, window, cx)));

        vcx.update(|_, cx| {
            view.update(cx, |view, cx| view.sync_text_generator_models(&server_cfg, cx));
        });
        drive(&mut vcx);
        let (requests, _) = fixture.take_requests();
        let (_, payload, reply) = requests
            .into_iter()
            .find(|(m, _, _)| *m == LIST_TEXT_GENERATOR_MODELS)
            .expect("listTextGeneratorModels RPC sent");
        assert_eq!(payload["id"], json!("or-fast"));
        let _ = reply.send(Ok(json!({
            "models": [{ "id": "openai/gpt-5-nano", "name": "GPT-5 Nano" }]
        })));
        drive(&mut vcx);

        view.read_with(&vcx, |view, _| {
            let sel = ModelSelection {
                instance_id: "or-fast".into(),
                model: "openai/gpt-5-nano".into(),
                options: None,
            };
            let opts = view.text_gen_select_options(&server_cfg, None, &[Some(&sel)]);
            let values: Vec<&str> = opts.iter().map(|o| o.value.as_ref()).collect();
            assert_eq!(
                values,
                vec![
                    format!("codex{SELECTION_SEPARATOR}gpt-5"),
                    format!("or-fast{SELECTION_SEPARATOR}openai/gpt-5-nano"),
                ]
            );
            assert_eq!(view.text_gen_selection_label(&server_cfg, &sel), "GPT-5 Nano");
        });

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(view);
        vcx.run_until_parked();
        drop(vcx);
        drop(fixture);
        drop(runtime);
    }
}
