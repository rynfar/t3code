//! Maintenance: `MaintenanceSettingsPanel` in `web/src/components/settings/SettingsPanels.tsx`
//! for the connected environment: provider update checks and restart continuation, background
//! activity with its advanced dialog (`BackgroundActivitySettings.tsx`), the storage sections of
//! `StorageSettings.tsx` (database notice, artifact and log retention, Delete now and the last
//! run's report), and the link to Diagnostics. Rules live in `t3_core::settings::maintenance`.

use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div, list, prelude::FluentBuilder,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::json;
use t3_client::supervisor::ConnectionStatus;
use t3_core::device::SETTINGS_WRITE_SCOPE;
use t3_core::server_config::ServerConfig;
use t3_core::settings::ServerSettingChange;
use t3_core::settings::maintenance::{
    self as model, CleanupReport, DatabaseStorage, INTERVAL_FIELDS, IntervalField, LastRun, MaintenanceChange, Outcome, Override, PROFILES, PauseKey, ProfileOption, ReportEntry,
    Retention,
};
use t3_core::settings::search::Section;
use t3_core::settings::worktrees::{DEFAULT_RETENTION_DAYS, clamp_retention};

use super::controls::{Button, ButtonSize, ButtonVariant, DialogFrame, Row, Select, SelectOption, Switch, dialog_footer, group, icon, muted_text, r, section, section_title};
use super::providers::{Stream, failure_text};
use super::{Block, SettingsView, store};
use crate::app_state::Phase;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

/// The two retention fields on this page (`RetentionControl`, minimum one day).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Artifacts,
    Logs,
}

impl Field {
    fn saved(self, retention: &Retention) -> Option<u32> {
        match self {
            Field::Artifacts => retention.browser_artifacts_after_days,
            Field::Logs => retention.logs_after_days,
        }
    }

    fn change(self, days: Option<u32>) -> MaintenanceChange {
        match self {
            Field::Artifacts => MaintenanceChange::BrowserArtifactsAfterDays(days),
            Field::Logs => MaintenanceChange::LogsAfterDays(days),
        }
    }
}

/// Above this many results the last run's list is virtualized in a fixed height.
const INLINE_RESULTS: usize = 20;

pub(crate) struct MaintenanceState {
    artifacts_days: Entity<InputState>,
    logs_days: Entity<InputState>,
    intervals: [Entity<InputState>; 4],
    last_run: LastRun,
    report_stream: Option<(u64, Stream)>,
    database: Option<DatabaseStorage>,
    database_stream: Option<(u64, Stream)>,
    cleanup_pending: bool,
    last_run_open: bool,
    results: ListState,
    background_dialog: Option<FocusHandle>,
    ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// `DatabaseStorageNotice`'s Dismiss lasts for the app session; the web keeps it in browser
/// storage.
struct DismissedDatabaseNotice(bool);

impl gpui::Global for DismissedDatabaseNotice {}

impl MaintenanceState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let artifacts_days = cx.new(|cx| InputState::new(window, cx));
        let logs_days = cx.new(|cx| InputState::new(window, cx));
        let intervals = [(); 4].map(|_| cx.new(|cx| InputState::new(window, cx)));
        let commit_on = |event: &InputEvent| matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur);
        let mut subscriptions = vec![
            cx.subscribe_in(&artifacts_days, window, move |this, _, event: &InputEvent, window, cx| {
                if commit_on(event) {
                    this.commit_artifact_retention(Field::Artifacts, window, cx);
                }
            }),
            cx.subscribe_in(&logs_days, window, move |this, _, event: &InputEvent, window, cx| {
                if commit_on(event) {
                    this.commit_artifact_retention(Field::Logs, window, cx);
                }
            }),
        ];
        for (input, field) in intervals.iter().zip(INTERVAL_FIELDS) {
            subscriptions.push(cx.subscribe_in(input, window, move |this, _, event: &InputEvent, window, cx| {
                if commit_on(event) {
                    this.commit_interval(field, window, cx);
                }
            }));
        }
        MaintenanceState {
            artifacts_days,
            logs_days,
            intervals,
            last_run: LastRun::Loading,
            report_stream: None,
            database: None,
            database_stream: None,
            cleanup_pending: false,
            last_run_open: false,
            results: ListState::new(0, ListAlignment::Top, gpui::px(400.)),
            background_dialog: None,
            ticker: None,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        [&self.artifacts_days, &self.logs_days].into_iter().chain(self.intervals.iter())
    }

    fn input(&self, field: Field) -> &Entity<InputState> {
        match field {
            Field::Artifacts => &self.artifacts_days,
            Field::Logs => &self.logs_days,
        }
    }

    /// Off the page: its streams and clock stop.
    pub(super) fn leave(&mut self) {
        self.report_stream = None;
        self.database_stream = None;
        self.ticker = None;
        self.background_dialog = None;
    }

    pub(super) fn dismiss(&mut self) -> bool {
        self.background_dialog.take().is_some()
    }
}

fn now_ms() -> i64 {
    crate::sidebar::controller::now_ms()
}

impl SettingsView {
    fn connected_now(&self, cx: &App) -> bool {
        matches!(self.state.read(cx).phase, Phase::Online(ConnectionStatus::Connected))
    }

    /// Why a server-backed row cannot change (`SettingsRow`'s inert control), if it cannot.
    fn settings_unavailable(&self, cx: &App) -> Option<String> {
        if !self.connected_now(cx) {
            return Some("Reconnect the selected environment to change this setting.".to_owned());
        }
        (!self.may(SETTINGS_WRITE_SCOPE, cx)).then(|| t3_core::settings::diagnostics::telemetry_no_permission(&self.environment_label(cx)))
    }

    fn save_maintenance(&self, change: MaintenanceChange, cx: &mut App) {
        store::update_server(&self.state, ServerSettingChange::Prepared(change.payload()), cx);
    }

    fn save_prepared(state: &Entity<crate::app_state::AppState>, payload: serde_json::Value, cx: &mut App) {
        store::update_server(state, ServerSettingChange::Prepared(payload), cx);
    }

    /// Follows the last cleanup report and the database's reclaimable space while the page shows.
    fn ensure_maintenance_streams(&mut self, config: &ServerConfig, cx: &mut Context<Self>) {
        let Some(connection) = self.state.read(cx).connection_id() else {
            self.maintenance.leave();
            return;
        };
        let capabilities = &config.environment.capabilities;
        if capabilities.storage_cleanup_run == Some(true) && !self.maintenance.report_stream.as_ref().is_some_and(|(id, _)| *id == connection) {
            self.maintenance.last_run = LastRun::Loading;
            let stream = self.subscribe(model::GET_STORAGE_CLEANUP_REPORT, json!({}), cx, |this, item, _| {
                this.maintenance.last_run = match item {
                    Ok(serde_json::Value::Null) => LastRun::None,
                    Ok(value) => match model::decode_report(&value) {
                        Some(report) => LastRun::Report(report),
                        None => return,
                    },
                    Err(_) => LastRun::Failed,
                };
            });
            self.maintenance.report_stream = stream.map(|stream| (connection, stream));
        }
        if capabilities.database_storage == Some(true) && !self.maintenance.database_stream.as_ref().is_some_and(|(id, _)| *id == connection) {
            let stream = self.subscribe(model::GET_DATABASE_STORAGE, json!({}), cx, |this, item, _| {
                if let Ok(value) = item {
                    this.maintenance.database = model::compaction_recommendation(&value);
                }
            });
            self.maintenance.database_stream = stream.map(|stream| (connection, stream));
        }
        if self.maintenance.ticker.is_none() {
            self.maintenance.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(30)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        }
    }

    /// `RetentionControl`'s commit: a number saves clamped; anything else puts the saved value back.
    fn commit_artifact_retention(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.maintenance.input(field).clone();
        let Some(saved) = self.thread_settings(cx).and_then(|config| field.saved(&Retention::of(&config.settings))) else {
            return;
        };
        match input.read(cx).value().trim().parse::<f64>().ok().filter(|days| days.is_finite()) {
            Some(days) => {
                let days = clamp_retention(days, 1);
                input.update(cx, |input, cx| input.set_value(days.to_string(), window, cx));
                if days != saved {
                    self.save_maintenance(field.change(Some(days)), cx);
                }
            }
            None => input.update(cx, |input, cx| input.set_value(saved.to_string(), window, cx)),
        }
    }

    /// `IntervalSecondsField`'s commit: rounded, at least the field's minimum; text that is not a
    /// number puts the saved value back.
    fn commit_interval(&mut self, field: IntervalField, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = INTERVAL_FIELDS.iter().position(|candidate| *candidate == field) else {
            return;
        };
        let input = self.maintenance.intervals[index].clone();
        let Some(settings) = self.thread_settings(cx).map(|config| config.settings.provider_panel.clone()) else {
            return;
        };
        let saved = field.seconds(&settings.resolved_background_activity());
        let seconds = field.normalize(&input.read(cx).value()).unwrap_or(saved);
        input.update(cx, |input, cx| input.set_value(seconds.to_string(), window, cx));
        if seconds != saved && self.may(SETTINGS_WRITE_SCOPE, cx) {
            Self::save_prepared(&self.state, model::override_payload(&settings, field.change(seconds)), cx);
        }
    }

    pub(super) fn commit_focused_maintenance_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for field in [Field::Artifacts, Field::Logs] {
            if self.maintenance.input(field).focus_handle(cx).is_focused(window) {
                self.commit_artifact_retention(field, window, cx);
            }
        }
        for (index, field) in INTERVAL_FIELDS.into_iter().enumerate() {
            if self.maintenance.intervals[index].focus_handle(cx).is_focused(window) {
                self.commit_interval(field, window, cx);
            }
        }
    }

    pub(super) fn maintenance_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let Some(config) = self.thread_settings(cx).cloned() else {
            return vec![Block::new(&[], muted_text("Reconnect to change this environment's settings.", cx))];
        };
        self.ensure_maintenance_streams(&config, cx);
        let settings = &config.settings;
        let unavailable = self.settings_unavailable(cx);
        let connected = self.connected_now(cx);
        let state = self.state.clone();
        let updates = section(
            "Updates",
            vec![
                Row::new("provider-update-checks", "Provider update checks")
                    .description("Check installed provider CLIs for newer available versions and fetch model catalog updates. When off, new models may not appear.")
                    .highlighted(self.is_target("provider-update-checks"))
                    .unavailable(unavailable.clone())
                    .reset(!settings.maintenance.enable_provider_update_checks, {
                        let state = state.clone();
                        move |_, _, cx| Self::save_prepared(&state, MaintenanceChange::ProviderUpdateChecks(true).payload(), cx)
                    })
                    .control(Switch::new("provider-update-checks-switch", settings.maintenance.enable_provider_update_checks).on_change({
                        let state = state.clone();
                        move |on, _, cx| Self::save_prepared(&state, MaintenanceChange::ProviderUpdateChecks(on).payload(), cx)
                    }))
                    .into_any_element(),
                Row::new("continue-threads-after-server-update", "Continue threads after restarts")
                    .description("Automatically resume interrupted threads after an update, crash, or machine restart on the selected environments.")
                    .when(!connected, |row| row.status("All selected connected environments must support restart continuation."))
                    .highlighted(self.is_target("continue-threads-after-server-update"))
                    .unavailable(unavailable.clone())
                    .reset(connected && !settings.maintenance.continue_threads_after_server_update, {
                        let state = state.clone();
                        move |_, _, cx| Self::save_prepared(&state, MaintenanceChange::ContinueThreads(true).payload(), cx)
                    })
                    .control(Switch::new("continue-threads-switch", settings.maintenance.continue_threads_after_server_update).disabled(!connected).on_change({
                        let state = state.clone();
                        move |on, _, cx| Self::save_prepared(&state, MaintenanceChange::ContinueThreads(on).payload(), cx)
                    }))
                    .into_any_element(),
            ],
            cx,
        );
        let background = section("Background activity", vec![self.background_row(&config, unavailable.clone(), cx)], cx);
        let mut blocks =
            vec![Block::new(&["provider-update-checks", "continue-threads-after-server-update"], updates), Block::new(&["background-activity", "git-fetch-interval"], background)];
        blocks.extend(self.storage_blocks(&config, unavailable, window, cx));
        let diagnostics = section(
            "Diagnostics",
            vec![
                Row::new("diagnostics", "Diagnostics")
                    .description(if connected {
                        "Inspect processes, resource use, and logs on this environment."
                    } else {
                        "Inspect processes, resource use, and logs on one environment at a time."
                    })
                    .highlighted(self.is_target("diagnostics"))
                    .control(
                        Button::new("view-diagnostics", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("View diagnostics")
                            .on_click(cx.listener(|this, _, _, cx| this.navigate(Section::Diagnostics, None, cx))),
                    )
                    .into_any_element(),
            ],
            cx,
        );
        blocks.push(Block::new(&["diagnostics"], diagnostics));
        blocks
    }

    /// `BackgroundActivitySettingsRow`.
    fn background_row(&mut self, config: &ServerConfig, unavailable: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        let settings = &config.settings.provider_panel;
        let option = model::profile_option(settings);
        let selected = match option {
            ProfileOption::Profile(profile) => profile.as_str(),
            ProfileOption::Advanced => model::ADVANCED_OPTION,
        };
        let mut options: Vec<SelectOption> = PROFILES.iter().map(|profile| SelectOption::new(profile.as_str(), model::profile_label(*profile))).collect();
        options.push(SelectOption::new(model::ADVANCED_OPTION, "Advanced"));
        let state = self.state.clone();
        let view = cx.entity().downgrade();
        let select = Select::new("background-activity-select", options).selected(selected).width(160.).on_change(move |value, window, cx| {
            if value.as_ref() == model::ADVANCED_OPTION {
                let _ = view.update(cx, |this, cx| this.open_background_dialog(window, cx));
            } else if let Some(profile) = model::parse_profile(&value) {
                Self::save_prepared(&state, model::background_profile_payload(profile), cx);
            }
        });
        let configure = (option == ProfileOption::Advanced).then(|| {
            Button::new("background-activity-configure", ButtonVariant::Outline, ButtonSize::IconSm)
                .icon(Icon::Settings)
                .tooltip("Configure background activity")
                .on_click(cx.listener(|this, _, window, cx| this.open_background_dialog(window, cx)))
        });
        let reset_state = self.state.clone();
        Row::new("background-activity", "Background activity")
            .title_tooltip(model::POLICY_TOOLTIP)
            .description(model::background_description(settings))
            .highlighted(self.is_target("background-activity") || self.is_target("git-fetch-interval"))
            .unavailable(unavailable)
            .reset(model::background_customized(settings), move |_, _, cx| Self::save_prepared(&reset_state, model::background_reset_payload(), cx))
            .control(h_flex().gap(r(8.)).child(select).children(configure))
            .into_any_element()
    }

    fn open_background_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.may(SETTINGS_WRITE_SCOPE, cx) {
            return;
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.maintenance.background_dialog = Some(focus);
        cx.notify();
    }

    fn close_background_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_focused_maintenance_field(window, cx);
        if self.maintenance.dismiss() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// `BackgroundActivityAdvancedDialog`.
    pub(super) fn background_dialog_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let focus = self.maintenance.background_dialog.clone()?;
        let config = self.thread_settings(cx).cloned()?;
        let p = palette(cx);
        let settings = config.settings.provider_panel.clone();
        let resolved = settings.resolved_background_activity();
        let can_write = self.may(SETTINGS_WRITE_SCOPE, cx);
        for (index, field) in INTERVAL_FIELDS.into_iter().enumerate() {
            Self::sync_input(&self.maintenance.intervals[index], &field.seconds(&resolved).to_string(), window, cx);
        }
        let divider = hsla(p.border);
        let text_block = |title: &'static str, description: &'static str| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(r(4.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.card_foreground)).child(title))
                .child(div().text_xs().line_height(r(18.)).text_color(hsla(p.muted_foreground)).child(description))
        };
        let row = |content: gpui::Div| content.px(r(16.)).py(r(12.)).gap(r(12.)).items_center();
        let state = self.state.clone();
        let policy_settings = settings.clone();
        let policy = row(h_flex()).child(text_block("Shared policy", "Controls whether background work may run after a subscribed interval fires.")).child(
            Select::new("background-shared-policy", PROFILES.iter().map(|profile| SelectOption::new(profile.as_str(), model::profile_label(*profile))).collect())
                .selected(resolved.profile.as_str())
                .width(160.)
                .on_change(move |value, _, cx| {
                    if let Some(profile) = model::parse_profile(&value) {
                        Self::save_prepared(&state, model::shared_policy_payload(&policy_settings, profile), cx);
                    }
                }),
        );
        let mut card = v_flex().rounded(r(12.)).border_1().border_color(divider).bg(hsla(p.card)).overflow_hidden().when(!can_write, |this| this.opacity(0.64)).child(policy);
        for (index, field) in INTERVAL_FIELDS.into_iter().enumerate() {
            card = card.child(
                row(h_flex()).border_t_1().border_color(divider).child(text_block(field.title, field.description)).child(
                    h_flex()
                        .flex_none()
                        .gap(r(8.))
                        .child(div().w(r(96.)).child(Input::new(&self.maintenance.intervals[index]).small().disabled(!can_write)))
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("seconds")),
                ),
            );
        }
        let switches = PauseKey::ALL.map(|key| {
            let state = self.state.clone();
            let settings = settings.clone();
            h_flex()
                .flex_1()
                .justify_between()
                .gap(r(12.))
                .px(r(16.))
                .py(r(12.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(key.label()))
                .child(Switch::new(SharedString::from(format!("background-{}", key.label())), key.value(&resolved)).disabled(!can_write).on_change(move |on, _, cx| {
                    Self::save_prepared(&state, model::override_payload(&settings, Override::Pause(key, on)), cx);
                }))
                .into_any_element()
        });
        let [a, b, c, d] = switches;
        card = card
            .child(h_flex().border_t_1().border_color(divider).child(div().flex_1().border_r_1().border_color(divider).child(a)).child(div().flex_1().child(b)))
            .child(h_flex().border_t_1().border_color(divider).child(div().flex_1().border_r_1().border_color(divider).child(c)).child(div().flex_1().child(d)));
        let reset_state = self.state.clone();
        let footer = dialog_footer(
            false,
            vec![
                Button::new("background-reset-all", ButtonVariant::Outline, ButtonSize::Default)
                    .label("Reset all")
                    .disabled(!can_write)
                    .on_click(move |_, _, cx| Self::save_prepared(&reset_state, model::background_reset_payload(), cx))
                    .into_any_element(),
                Button::new("background-done", ButtonVariant::Default, ButtonSize::Default)
                    .label("Done")
                    .on_click(cx.listener(|this, _, window, cx| this.close_background_dialog(window, cx)))
                    .into_any_element(),
            ],
            cx,
        );
        let view = cx.entity().downgrade();
        let frame = DialogFrame {
            id: "background-activity-dialog",
            title: "Background Activity".into(),
            description: "Tune the shared power policy and the background intervals that feed it.".into(),
            max_width: 576.,
            body: card.into_any_element(),
            footer: footer.into_any_element(),
            focus,
        };
        Some(frame.render(
            window,
            move |window, cx| {
                let _ = view.update(cx, |this, cx| this.close_background_dialog(window, cx));
            },
            cx,
        ))
    }

    /// `RetentionControl`: a day count while on, "Off" while off, and a switch that turns it on
    /// at eight days.
    fn maintenance_retention(&self, field: Field, id: &'static str, value: Option<u32>, cx: &App) -> impl IntoElement {
        let p = palette(cx);
        let state = self.state.clone();
        h_flex()
            .gap(r(12.))
            .map(|this| match value {
                Some(_) => this.child(
                    h_flex()
                        .gap(r(6.))
                        .child(div().w(r(72.)).child(Input::new(self.maintenance.input(field)).small()))
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("days")),
                ),
                None => this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Off")),
            })
            .child(Switch::new(id, value.is_some()).on_change(move |on, _, cx| {
                Self::save_prepared(&state, field.change(on.then_some(DEFAULT_RETENTION_DAYS)).payload(), cx);
            }))
    }

    /// `StorageCleanupSections`: the database notice, retention, and the cleanup run.
    fn storage_blocks(&mut self, config: &ServerConfig, unavailable: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let capabilities = &config.environment.capabilities;
        let mut blocks = Vec::new();
        if let Some(notice) = self.database_notice(config, cx) {
            blocks.push(Block::new(&["storage-database"], notice));
        }
        if capabilities.storage_cleanup != Some(true) {
            blocks.push(Block::new(
                &["storage-artifacts"],
                v_flex()
                    .gap(r(10.))
                    .child(section_title("Artifacts and logs"))
                    .child(div().px(r(16.)).child(muted_text("Update the selected environments to use storage cleanup, or choose a machine that supports it.", cx))),
            ));
            return blocks;
        }
        let retention = Retention::of(&config.settings);
        for (field, days) in [(Field::Artifacts, retention.browser_artifacts_after_days), (Field::Logs, retention.logs_after_days)] {
            if let Some(days) = days {
                Self::sync_input(self.maintenance.input(field), &days.to_string(), window, cx);
            }
        }
        let artifacts = section(
            "Artifacts and logs",
            vec![
                Row::new("storage-browser-artifacts", "Delete old browser artifacts")
                    .description("Delete saved browser captures after this many days. Older capture links will no longer open.")
                    .unavailable(unavailable.clone())
                    .control(self.maintenance_retention(Field::Artifacts, "storage-artifacts-switch", retention.browser_artifacts_after_days, cx))
                    .into_any_element(),
                Row::new("storage-logs", "Delete old rotated logs")
                    .description("Delete inactive rotated log files after this many days. Current logs are kept.")
                    .unavailable(unavailable)
                    .control(self.maintenance_retention(Field::Logs, "storage-logs-switch", retention.logs_after_days, cx))
                    .into_any_element(),
            ],
            cx,
        );
        let highlighted = self.is_target("storage-artifacts");
        let p = palette(cx);
        blocks.push(Block::new(
            &["storage-artifacts"],
            div().id("storage-artifacts").rounded(r(12.)).when(highlighted, |this| this.bg(hsla(p.accent).opacity(0.3))).child(artifacts),
        ));
        if capabilities.storage_cleanup_run == Some(true) {
            blocks.push(Block::new(&["storage-delete-now"], self.storage_cleanup_section(config, cx)));
        }
        blocks
    }

    /// `DatabaseStorageNotice`: suggests `t3 doctor` while the database holds a GiB of freed pages.
    fn database_notice(&self, config: &ServerConfig, cx: &mut Context<Self>) -> Option<AnyElement> {
        if config.environment.capabilities.database_storage != Some(true) || cx.try_global::<DismissedDatabaseNotice>().is_some_and(|dismissed| dismissed.0) {
            return None;
        }
        let storage = self.maintenance.database.clone()?;
        let command = storage.compact_command.clone();
        let rows = vec![
            Row::new("storage-database-intro", "Reclaim freed database space").description(model::DATABASE_DESCRIPTION).into_any_element(),
            Row::new("storage-database-machine", storage.title(&self.environment_label(cx)))
                .description_content(div().font_family(cx.theme().mono_font_family.clone()).child(command.clone()))
                .control(
                    h_flex()
                        .gap(r(6.))
                        .child(Button::new("storage-database-copy", ButtonVariant::Outline, ButtonSize::Xs).label("Copy command").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                        }))
                        .child(Button::new("storage-database-dismiss", ButtonVariant::Ghost, ButtonSize::Xs).label("Dismiss").on_click(cx.listener(|_, _, _, cx| {
                            cx.set_global(DismissedDatabaseNotice(true));
                            cx.notify();
                        }))),
                )
                .into_any_element(),
        ];
        Some(section("Database", rows, cx).into_any_element())
    }

    /// `CleanupSection`: Delete now with the last run, and the last run's results folded below.
    fn storage_cleanup_section(&mut self, config: &ServerConfig, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let has_rules = model::has_cleanup_rules(&config.settings);
        let allowed = self.may(SETTINGS_WRITE_SCOPE, cx);
        let pending = self.maintenance.cleanup_pending;
        let description = if has_rules { self.maintenance.last_run.description(now_ms()) } else { model::NO_RULES.to_owned() };
        let delete_now = Row::new("storage-delete-now", "Delete now").description(description).highlighted(self.is_target("storage-delete-now")).control(
            Button::new("storage-delete-now-button", ButtonVariant::Outline, ButtonSize::Sm)
                .label(if pending { "Deleting\u{2026}" } else { "Delete now" })
                .disabled(pending || !has_rules || !allowed)
                .on_click(cx.listener(|this, _, _, cx| this.run_storage_cleanup(cx))),
        );
        let mut content = v_flex().gap(r(10.)).child(section_title("Cleanup")).child(group(vec![delete_now.into_any_element()], cx));
        if let Some(report) = self.maintenance.last_run.report().cloned() {
            let open = self.maintenance.last_run_open;
            let header = h_flex()
                .id("storage-cleanup-last-run")
                .min_h(r(44.))
                .px(r(16.))
                .py(r(8.))
                .gap(r(8.))
                .cursor_pointer()
                .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 16., hsla(p.muted_foreground)))
                .child(div().flex_none().text_sm().font_weight(FontWeight::MEDIUM).child("Last run"))
                .child(div().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(report.counts()))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.maintenance.last_run_open = !this.maintenance.last_run_open;
                    cx.notify();
                }));
            let folded =
                v_flex().child(header).when(open, |this| this.child(div().border_t_1().border_color(hsla(p.border).opacity(0.5)).child(self.cleanup_results(&report, cx))));
            content = content.child(group(vec![folded.into_any_element()], cx));
        }
        content.into_any_element()
    }

    /// `CleanupResults`: one row per entry, then what was left out or that nothing needed it.
    fn cleanup_results(&mut self, report: &CleanupReport, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let label = self.environment_label(cx);
        let note = |text: String| div().px(r(16.)).py(r(10.)).text_xs().text_color(hsla(p.muted_foreground)).child(text).into_any_element();
        let footer = if report.omitted_count > 0 {
            Some(note(format!("{label}: {} more results.", model::format_count(report.omitted_count as f64))))
        } else if report.entries.is_empty() {
            Some(note(format!("{label}: Nothing needed cleanup.")))
        } else {
            None
        };
        let entries = std::rc::Rc::new(report.entries.clone());
        let body = if entries.len() <= INLINE_RESULTS {
            v_flex().children(entries.iter().enumerate().map(|(index, entry)| self.cleanup_result(index, entry, &label, cx))).into_any_element()
        } else {
            if self.maintenance.results.item_count() != entries.len() {
                self.maintenance.results.reset(entries.len());
            }
            let label = label.clone();
            list(
                self.maintenance.results.clone(),
                cx.processor(move |this, index: usize, _, cx| match entries.get(index) {
                    Some(entry) => this.cleanup_result(index, entry, &label, cx),
                    None => div().into_any_element(),
                }),
            )
            .h(r(560.))
            .into_any_element()
        };
        v_flex().child(body).children(footer).into_any_element()
    }

    fn cleanup_result(&self, index: usize, entry: &ReportEntry, environment: &str, cx: &App) -> AnyElement {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let detail = |name: &'static str, value: String, code: bool| {
            h_flex()
                .gap(r(12.))
                .items_start()
                .child(div().w(r(56.)).flex_none().child(name))
                .child(div().min_w_0().text_color(hsla(p.foreground).opacity(0.8)).when(code, |this| this.font_family(mono.clone())).child(value))
        };
        let description =
            v_flex().gap(r(8.)).child(div().when(entry.outcome == Outcome::Failed, |this| this.text_color(hsla(p.destructive_foreground))).child(entry.reason.clone())).child(
                v_flex()
                    .gap(r(4.))
                    .pl(r(12.))
                    .border_l_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .child(detail("Machine", environment.to_owned(), false))
                    .children(entry.project_path.clone().map(|path| detail("Project", path, true)))
                    .children(entry.path.clone().map(|path| detail("Folder", path, true)))
                    .children(entry.thread_title.clone().map(|title| detail("Thread", title, false))),
            );
        let size = entry.size().filter(|size| !size.is_empty()).map(|size| div().text_xs().text_color(hsla(p.muted_foreground)).child(size));
        div()
            .when(index > 0, |this| this.border_t_1().border_color(hsla(p.border).opacity(0.5)))
            .child(
                Row::new(SharedString::from(format!("storage-cleanup-result-{index}")), entry.name())
                    .description_content(description)
                    .when_some(size, |row, size| row.control(size)),
            )
            .into_any_element()
    }

    /// Delete now: one manual run; failures it reports, or its own failure, show as toasts.
    fn run_storage_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.maintenance.cleanup_pending {
            return;
        }
        self.maintenance.cleanup_pending = true;
        let label = self.environment_label(cx);
        let task = self.state.read(cx).rpc(model::RUN_STORAGE_CLEANUP, json!({}));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.maintenance.cleanup_pending = false;
                let toast = match result {
                    Ok(reply) => model::decode_report(&reply).and_then(|report| {
                        let failure = report.failures().next().map(|entry| entry.reason.clone());
                        this.maintenance.last_run = LastRun::Report(report);
                        failure.map(|reason| Toast::new(ToastKind::Error, format!("Cleanup failed on {label}")).description(reason))
                    }),
                    Err(error) => Some(Toast::new(ToastKind::Error, format!("Could not run cleanup on {label}")).description(failure_text(&error, "Try again."))),
                };
                if let Some(toast) = toast {
                    this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
