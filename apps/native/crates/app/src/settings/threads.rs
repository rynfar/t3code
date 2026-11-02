//! Threads: `ThreadsSettingsPanel` in `web/src/components/settings/SettingsPanels.tsx` with the
//! general rows of `ProjectDefaultsSettings.tsx` and `SnoozePresetsSettings.tsx`, at environment
//! scope. Every row is server-backed. The text generation model, effort traits and thread title
//! instructions are not ported yet.

use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveTime, TimeZone};
use gpui::{App, AppContext, Context, Entity, Focusable, IntoElement, ParentElement, SharedString, Styled, Subscription, Window, div};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use t3_core::client_settings::TimestampFormat;
use t3_core::composer::{runtime_mode_description, runtime_mode_label};
use t3_core::model::ModelSelection;
use t3_core::providers;
use t3_core::server_config::{RuntimeMode, ServerConfig, ThreadEnvMode};
use t3_core::settings::snooze::{self, DurationUnit, SnoozeDay, SnoozeDraft, SnoozePreset};
use t3_core::settings::{
    DEFAULT_AUTO_SETTLE_DAYS, ENV_MODES, MAX_AUTO_SETTLE_DAYS, MIN_AUTO_SETTLE_DAYS, ResponseStreamingMode, ServerSettingChange,
    effective_env_mode,
};

use super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, muted_text, r, section};
use super::{Block, SettingsView, store};
use crate::composer::runtime_mode_icon;
use crate::theme::{hsla, palette};

const SELECTION_SEPARATOR: char = '\u{1f}';

type SnoozeEdit = Box<dyn FnOnce(&mut Vec<SnoozePreset>)>;

pub(crate) struct ThreadsState {
    pub(crate) base_directory: Entity<InputState>,
    pub(crate) auto_settle_days: Entity<InputState>,
    time_inputs: Vec<(Entity<InputState>, Subscription)>,
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
        let subscriptions = vec![
            cx.subscribe_in(&base_directory, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_base_directory(cx);
                }
            }),
            cx.subscribe_in(&auto_settle_days, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let days = input.read(cx).value().trim().parse::<u32>().ok().filter(|days| (MIN_AUTO_SETTLE_DAYS..=MAX_AUTO_SETTLE_DAYS).contains(days));
                    let saved = this.thread_settings(cx).and_then(|config| config.settings.threads.sidebar_auto_settle_after_days);
                    if let Some(days) = days.filter(|days| Some(*days) != saved) {
                        store::update_server(&this.state, ServerSettingChange::AutoSettleAfterDays(Some(days)), cx);
                    }
                }
                InputEvent::Blur => {
                    let saved = this.thread_settings(cx).and_then(|config| config.settings.threads.sidebar_auto_settle_after_days);
                    if let Some(days) = saved {
                        input.update(cx, |input, cx| input.set_value(days.to_string(), window, cx));
                    }
                }
                _ => {}
            }),
        ];
        ThreadsState { base_directory, auto_settle_days, time_inputs: Vec::new(), snooze: None, _subscriptions: subscriptions }
    }
}

/// The models a default-model select offers: every selectable provider instance, grouped.
fn model_options(config: &ServerConfig) -> Vec<SelectOption> {
    providers::entries(config, None)
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

fn selection_value(selection: &ModelSelection) -> String {
    format!("{}{SELECTION_SEPARATOR}{}", selection.instance_id, selection.model)
}

fn parse_selection(value: &str) -> Option<ModelSelection> {
    let (instance_id, model) = value.split_once(SELECTION_SEPARATOR)?;
    Some(ModelSelection { instance_id: instance_id.to_owned(), model: model.to_owned(), options: None })
}

fn selection_label(config: &ServerConfig, selection: &ModelSelection) -> String {
    config
        .provider(&selection.instance_id)
        .and_then(|provider| provider.models.iter().find(|model| model.slug == selection.model))
        .map(|model| providers::trigger_model_name(model).to_owned())
        .unwrap_or_else(|| selection.model.clone())
}

/// `formatShortTimestamp` for a wake time.
fn format_time(time: &DateTime<Local>, format: TimestampFormat) -> String {
    match format {
        TimestampFormat::TwentyFourHour => time.format("%H:%M").to_string(),
        TimestampFormat::Locale | TimestampFormat::TwelveHour => time.format("%-I:%M %p").to_string(),
    }
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

    fn sync_input(input: &Entity<InputState>, value: &str, window: &mut Window, cx: &mut App) {
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
        if let Some(days) = threads.sidebar_auto_settle_after_days {
            Self::sync_input(&self.threads.auto_settle_days, &days.to_string(), window, cx);
        }

        let new_projects = section(
            "New projects",
            vec![Row::new("add-project-starts-in", "Add project starts in")
                .description("Leave empty to use \"~/\" when the Add Project browser opens.")
                .highlighted(self.is_target("add-project-starts-in"))
                .reset(!threads.add_project_base_directory.is_empty(), {
                    let reset = update(ServerSettingChange::AddProjectBaseDirectory(String::new()));
                    move |_, _, cx| reset(cx)
                })
                .control(div().w(r(288.)).child(Input::new(&self.threads.base_directory).small()))
                .into_any_element()],
            cx,
        );

        let options = model_options(&config);
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
            Some(selection) if !options.is_empty() => model_row.control(
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
            ),
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
            Some(shown) if !options.is_empty() => fallback_row.control(
                Select::new("fallback-model-select", options)
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
            ),
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

        let streaming = threads.response_streaming_mode;
        let streaming_state = self.state.clone();
        let text_generation = section(
            "Text generation",
            vec![Row::new("response-streaming", "Response streaming")
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
                .into_any_element()],
            cx,
        );

        vec![
            Block::new(&["new-projects", "add-project-starts-in"], new_projects),
            Block::new(&["project-defaults", "default-model", "fallback-model", "default-permissions", "new-threads"], new_threads),
            Block::new(&["snooze", "snooze-options"], snooze_section),
            Block::new(&["usage-limits", "auto-resume-limited-threads", "snooze-limited-threads"], usage_limits),
            Block::new(&["auto-settle", "auto-settle-merged-threads", "auto-settle-inactive-threads"], auto_settle),
            Block::new(&["text-generation", "response-streaming"], text_generation),
        ]
    }

    fn commit_base_directory(&mut self, cx: &mut Context<Self>) {
        let value = self.threads.base_directory.read(cx).value().trim().to_owned();
        let saved = self.thread_settings(cx).map(|config| config.settings.threads.add_project_base_directory.clone());
        if saved.as_deref() != Some(value.as_str()) {
            store::update_server(&self.state, ServerSettingChange::AddProjectBaseDirectory(value), cx);
        }
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
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
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
        Row::new("snooze-options", "Snooze options")
            .description("Choices in every snooze menu, listed soonest first. Weekdays mean the next one after today.")
            .highlighted(self.is_target("snooze-options"))
            .reset(!snooze::same_presets(presets, &defaults), move |_, _, cx| {
                let defaults = defaults.clone();
                save(Box::new(move |presets| *presets = defaults), cx)
            })
            .below(list)
    }

    fn snooze_fields(&self, index: usize, preset: &SnoozePreset, replace: impl Fn(SnoozePreset, &mut App) + Clone + 'static, cx: &App) -> impl IntoElement {
        let p = palette(cx);
        let when_options = std::iter::once(SelectOption::new("after", "In"))
            .chain(SnoozeDay::ALL.iter().map(|day| SelectOption::new(day.as_str(), day.label())))
            .collect();
        let when_value = match preset {
            SnoozePreset::After { .. } => "after",
            SnoozePreset::At { day, .. } => day.as_str(),
        };
        let when_preset = preset.clone();
        let when_replace = replace.clone();
        let when = Select::new(("snooze-when", index), when_options).selected(when_value).width(128.).on_change(move |value, _, cx| {
            let day = SnoozeDay::ALL.iter().copied().find(|day| day.as_str() == value.as_ref());
            if value.as_ref() != "after" && day.is_none() {
                return;
            }
            when_replace(snooze::with_when(&when_preset, day, 60, "09:00"), cx);
        });
        let fields = h_flex().gap(r(8.)).child(when);
        match preset {
            SnoozePreset::After { minutes } => {
                let (amount, unit) = snooze::split_minutes(*minutes);
                let minutes = *minutes;
                let (minus, plus, units) = (replace.clone(), replace.clone(), replace);
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
                                    .on_click(move |_, _, cx| minus(SnoozePreset::After { minutes: snooze::step_amount(minutes, -1) }, cx)),
                            )
                            .child(div().w(r(40.)).text_center().text_sm().child(amount.to_string()))
                            .child(
                                Button::new(("snooze-increase", index), ButtonVariant::Ghost, ButtonSize::IconSm)
                                    .icon(crate::icons::Icon::Plus)
                                    .tooltip("Increase")
                                    .on_click(move |_, _, cx| plus(SnoozePreset::After { minutes: snooze::step_amount(minutes, 1) }, cx)),
                            ),
                    )
                    .child(
                        Select::new(
                            ("snooze-unit", index),
                            DurationUnit::ALL
                                .iter()
                                .map(|unit| SelectOption::new(unit.plural(), if amount == 1 { unit.singular() } else { unit.plural() }))
                                .collect(),
                        )
                        .selected(unit.plural())
                        .width(112.)
                        .on_change(move |value, _, cx| {
                            if let Some(unit) = DurationUnit::ALL.iter().copied().find(|unit| unit.plural() == value.as_ref()) {
                                units(SnoozePreset::After { minutes: snooze::with_unit(minutes, unit) }, cx);
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

fn env_mode_value(mode: ThreadEnvMode) -> &'static str {
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
