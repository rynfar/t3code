//! Maintenance: `MaintenanceSettingsPanel` in `web/src/components/settings/SettingsPanels.tsx`
//! for the connected environment: provider update checks and restart continuation, background
//! activity with its advanced dialog (`BackgroundActivitySettings.tsx`), the storage sections of
//! `StorageSettings.tsx` (database notice, artifact and log retention, Delete now and the last
//! run's report), and the link to Diagnostics. Rules live in `t3_core::settings::maintenance`.

use std::{collections::HashSet, time::Duration};

use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div, list, prelude::FluentBuilder,
};
use gpui_component::input::{Input, InputEvent, InputState, NumberInputEvent, StepAction};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::json;
use t3_client::supervisor::ConnectionStatus;
use t3_core::device::SETTINGS_WRITE_SCOPE;
use t3_core::sidebar::grouping::EnvironmentOwner;
use crate::i18n::t;
use t3_core::server_config::{DEFAULT_USAGE_HISTORY_RETENTION_DAYS, ServerConfig, ServerSettings, UsageHistoryRetentionDays};
use t3_core::settings::ServerSettingChange;
use t3_core::settings::diagnostics::ENVIRONMENT_MAINTAIN_SCOPE;
use t3_core::settings::maintenance::{
    self as model, BackgroundEdit, CleanupReport, DatabaseStorage, INTERVAL_FIELDS, IntervalField, LastRun, MaintenanceChange, Outcome, Override, PROFILES, PauseKey,
    ProfileOption, ReportEntry, Retention, RetentionCommit, RetentionDraft, RetentionToggle,
};
use t3_core::settings::search::Section;

use super::controls::{Button, ButtonSize, ButtonVariant, DialogFrame, NumberField, Row, Select, SelectOption, SettingInheritance, SettingInheritanceLayer, SettingSource, Switch, dialog_footer, group, icon, muted_text, r, section, section_title};
use super::providers::{Stream, failure_text};
use super::{Block, SettingsView};
use crate::app_state::Phase;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

/// The retention fields on this page (`RetentionControl`, minimum one day).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Artifacts,
    Logs,
    UsageHistory,
}

impl Field {
    const ALL: [Self; 3] = [Self::Artifacts, Self::Logs, Self::UsageHistory];

    fn saved(self, settings: &ServerSettings) -> Option<u32> {
        let retention = Retention::of(settings);
        match self {
            Field::Artifacts => retention.browser_artifacts_after_days,
            Field::Logs => retention.logs_after_days,
            Field::UsageHistory => match settings.usage_history_retention_days {
                UsageHistoryRetentionDays::Days(days) => Some(days),
                UsageHistoryRetentionDays::Unlimited => None,
            },
        }
    }

    fn change(self, days: Option<u32>) -> MaintenanceChange {
        match self {
            Field::Artifacts => MaintenanceChange::BrowserArtifactsAfterDays(days),
            Field::Logs => MaintenanceChange::LogsAfterDays(days),
            Field::UsageHistory => MaintenanceChange::UsageHistoryRetentionDays(
                days.map_or(UsageHistoryRetentionDays::Unlimited, UsageHistoryRetentionDays::Days),
            ),
        }
    }

    fn default_when_enabled(self) -> Option<u32> {
        match self {
            Field::UsageHistory => Some(DEFAULT_USAGE_HISTORY_RETENTION_DAYS),
            Field::Artifacts | Field::Logs => None,
        }
    }

    fn off_label(self) -> &'static str {
        match self {
            Field::UsageHistory => "Unlimited",
            Field::Artifacts | Field::Logs => "Off",
        }
    }

    fn index(self) -> usize {
        match self {
            Field::Artifacts => 0,
            Field::Logs => 1,
            Field::UsageHistory => 2,
        }
    }
}

/// Above this many results the last run's list is virtualized in a fixed height.
const INLINE_RESULTS: usize = 20;

pub(crate) struct MaintenanceState {
    artifacts_days: Entity<InputState>,
    logs_days: Entity<InputState>,
    usage_history_days: Entity<InputState>,
    retention_drafts: [RetentionDraft; 3],
    retention_switch_pressed: [bool; 3],
    retention_seq: u64,
    intervals: [Entity<InputState>; 4],
    last_run: LastRun,
    stream_target: Option<(Option<EnvironmentOwner>, Option<u64>)>,
    report_stream: Option<Stream>,
    database: Option<DatabaseStorage>,
    database_stream: Option<Stream>,
    cleanup_pending: bool,
    reclaim_confirming: Option<FocusHandle>,
    reclaim_requested: bool,
    last_run_open: bool,
    results: ListState,
    background_dialog: Option<FocusHandle>,
    ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

const DATABASE_NOTICE_DISMISSALS: &str = "dismissed-database-storage";

#[derive(Default)]
struct DismissedDatabaseNotice {
    environments: HashSet<String>,
    loaded: bool,
}

impl gpui::Global for DismissedDatabaseNotice {}

impl MaintenanceState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let artifacts_days = cx.new(|cx| InputState::new(window, cx));
        let logs_days = cx.new(|cx| InputState::new(window, cx));
        let usage_history_days = cx.new(|cx| InputState::new(window, cx));
        let intervals = [(); 4].map(|_| cx.new(|cx| InputState::new(window, cx)));
        let commit_on = |event: &InputEvent| matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur);
        let mut subscriptions = Vec::new();
        for (input, field) in [
            (&artifacts_days, Field::Artifacts),
            (&logs_days, Field::Logs),
            (&usage_history_days, Field::UsageHistory),
        ] {
            subscriptions.push(cx.subscribe_in(input, window, move |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.maintenance.retention_drafts[field.index()].mark_edited(),
                InputEvent::PressEnter { .. } => {
                    this.maintenance.retention_switch_pressed[field.index()] = false;
                    this.commit_artifact_retention(field, true, window, cx);
                }
                InputEvent::Blur => {
                    if std::mem::take(&mut this.maintenance.retention_switch_pressed[field.index()]) {
                        return;
                    }
                    this.commit_artifact_retention(field, false, window, cx);
                }
                _ => {}
            }));
        }
        for field in Field::ALL {
            let input = match field {
                Field::Artifacts => &artifacts_days,
                Field::Logs => &logs_days,
                Field::UsageHistory => &usage_history_days,
            };
            subscriptions.push(cx.subscribe_in(input, window, move |this, input, event: &NumberInputEvent, window, cx| {
                let current = input.read(cx).value().trim().parse::<u32>().unwrap_or(1);
                let NumberInputEvent::Step(action) = event;
                let days = match action {
                    StepAction::Increment => current.saturating_add(1),
                    StepAction::Decrement => current.saturating_sub(1).max(1),
                };
                input.update(cx, |input, cx| input.set_value(days.to_string(), window, cx));
                this.maintenance.retention_drafts[field.index()].mark_edited();
                this.commit_artifact_retention(field, true, window, cx);
            }));
        }
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
            usage_history_days,
            retention_drafts: [RetentionDraft::Closed; 3],
            retention_switch_pressed: [false; 3],
            retention_seq: 0,
            intervals,
            last_run: LastRun::Loading,
            stream_target: None,
            report_stream: None,
            database: None,
            database_stream: None,
            cleanup_pending: false,
            reclaim_confirming: None,
            reclaim_requested: false,
            last_run_open: false,
            results: ListState::new(0, ListAlignment::Top, gpui::px(400.)),
            background_dialog: None,
            ticker: None,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        [&self.artifacts_days, &self.logs_days, &self.usage_history_days]
            .into_iter()
            .chain(self.intervals.iter())
    }

    fn input(&self, field: Field) -> &Entity<InputState> {
        match field {
            Field::Artifacts => &self.artifacts_days,
            Field::Logs => &self.logs_days,
            Field::UsageHistory => &self.usage_history_days,
        }
    }

    /// Off the page: its streams and clock stop.
    pub(super) fn leave(&mut self) {
        self.stream_target = None;
        self.report_stream = None;
        self.database_stream = None;
        self.last_run = LastRun::Loading;
        self.database = None;
        self.last_run_open = false;
        self.cleanup_pending = false;
        self.ticker = None;
        self.background_dialog = None;
        self.reclaim_confirming = None;
        self.reclaim_requested = false;
        self.retention_drafts = [RetentionDraft::Closed; 3];
        self.retention_switch_pressed = [false; 3];
    }

    pub(super) fn dismiss(&mut self) -> bool {
        self.background_dialog.take().is_some() | self.reclaim_confirming.take().is_some()
    }
}

fn now_ms() -> i64 {
    crate::sidebar::controller::now_ms()
}

impl SettingsView {
    pub(super) fn connected_now(&self, cx: &App) -> bool {
        matches!(self.state.read(cx).phase, Phase::Online(ConnectionStatus::Connected))
    }

    /// Why a server-backed row cannot change (`SettingsRow`'s inert control), if it cannot.
    pub(super) fn settings_unavailable(&self, cx: &App) -> Option<String> {
        let owners = self.scoped_environment_owners(cx);
        let app = self.state.read(cx);
        let explicit = self.scope.machine.is_some() || self.scope.project.is_some();
        if (!self.scoped_connected_now(cx) && owners.is_empty()) || (explicit && owners.is_empty()) {
            return Some(t(cx, "Reconnect the selected environment to change this setting.").to_owned());
        }
        let can_edit = if owners.is_empty() {
            self.may(SETTINGS_WRITE_SCOPE, cx)
        } else {
            owners.iter().all(|owner| app.scoped_ready(owner, SETTINGS_WRITE_SCOPE))
        };
        (!can_edit).then(|| t3_core::settings::diagnostics::telemetry_no_permission(&self.environment_label(cx)))
    }

    pub(super) fn switch_inheritance(
        &self, key: &'static str, value: bool, default: bool,
        read: fn(&ServerSettings) -> bool, cx: &Context<Self>,
    ) -> (SettingInheritance, bool) {
        let targets = self.scoped_model_targets(cx);
        let mixed = targets.iter().any(|(_, config, project)| {
            let effective = project.as_ref().map(|project| t3_core::settings::resolve_project_settings(&config.settings, project));
            read(effective.as_ref().unwrap_or(&config.settings)) != value
        });
        let raw = self.scoped_raw_representative_config(cx).map_or(default, |config| read(&config.settings));
        let project = self.is_project_scope(cx);
        if !project {
            return (environment_switch_inheritance(raw, default, mixed, cx), mixed);
        }
        let overridden = self.has_project_override(&[key], cx);
        let environment_set = raw != default;
        let flag = |on| t(cx, if on { "On" } else { "Off" }).into();
        let summary = if mixed { "Mixed values" } else if overridden { "Project" } else if environment_set { "Environment" } else { "Default" };
        let mut inheritance = SettingInheritance::new(t(cx, summary), vec![
            SettingInheritanceLayer::new(SettingSource::Project, overridden.then(|| flag(value)), overridden && !mixed),
            SettingInheritanceLayer::new(SettingSource::Environment, environment_set.then(|| flag(raw)), !overridden && !mixed && environment_set),
            SettingInheritanceLayer::new(SettingSource::Default, Some(flag(default)), !overridden && !mixed && !environment_set),
        ]).mixed(mixed);
        if overridden && self.settings_unavailable(cx).is_none() && self.managed_setting_lock(&[key], cx).is_none() {
            inheritance = inheritance.on_reset_overrides(cx.listener(move |this, _, _, cx| this.clear_project_overrides(&[key], cx)));
        }
        (inheritance, mixed)
    }

    pub(super) fn save_retention_setting(
        &self,
        change: ServerSettingChange,
        rollback: impl FnOnce(&mut Self) -> bool + 'static,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        self.update_scoped_server_with(
            change,
            Some(Box::new(move |cx| {
                cx.spawn(async move |cx| {
                    cx.update(|cx| {
                        let _ = view.update(cx, |this, cx| {
                            let _ = rollback(this);
                            cx.notify();
                        });
                    });
                })
                .detach();
            })),
            cx,
        );
    }

    pub(super) fn retention_switch(
        &self,
        id: &'static str,
        on: bool,
        disabled: bool,
        set_pressed: impl Fn(&mut Self, bool) + Copy + 'static,
        on_toggle: impl Fn(&mut Self, bool, &mut Window, &mut Context<Self>) + 'static,
        cx: &Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        div()
            .debug_selector(move || id.to_owned())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, _| {
                    if disabled {
                        return;
                    }
                    window.prevent_default();
                    set_pressed(this, true);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, _| {
                    set_pressed(this, false);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _, _, _| {
                    set_pressed(this, false);
                }),
            )
            .child(Switch::new(id, on).disabled(disabled).on_change(move |next, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    set_pressed(this, false);
                    on_toggle(this, next, window, cx);
                });
            }))
            .into_any_element()
    }

    #[cfg(all(test, feature = "snapshot"))]
    fn save_prepared(state: &Entity<crate::app_state::AppState>, payload: serde_json::Value, cx: &mut App) {
        super::store::update_server(state, ServerSettingChange::Prepared(payload), cx);
    }

    fn maintenance_storage_target(&self, cx: &App) -> Option<(Option<EnvironmentOwner>, Option<u64>)> {
        if self.is_project_scope(cx) || self.scoped_environment_owners(cx).len() > 1 || !self.scoped_connected_now(cx) {
            return None;
        }
        let owner = self.scoped_primary_owner(cx);
        if owner.is_none() && self.scope.machine.is_some() {
            return None;
        }
        Some((owner, self.state.read(cx).connection_id()))
    }

    fn maintenance_may(&self, scope: &str, cx: &App) -> bool {
        self.maintenance_storage_target(cx).is_some()
            && self.scoped_session(cx).is_none_or(|session| session.grants(scope))
    }

    fn ensure_database_dismissals(&self, cx: &mut Context<Self>) {
        if cx.try_global::<DismissedDatabaseNotice>().is_some() {
            return;
        }
        cx.set_global(DismissedDatabaseNotice::default());
        let load = cx.background_executor().spawn(async {
            t3_client::store::load_ui_state(DATABASE_NOTICE_DISMISSALS)
                .and_then(|value| serde_json::from_value::<HashSet<String>>(value).ok())
                .unwrap_or_default()
        });
        cx.spawn(async move |this, cx| {
            let environments = load.await;
            cx.update(|cx| cx.set_global(DismissedDatabaseNotice { loaded: true, environments }));
            let _ = this.update(cx, |_, cx| cx.notify());
        }).detach();
    }

    /// Follows the last cleanup report and the database's reclaimable space while the page shows.
    fn ensure_maintenance_streams(&mut self, config: &ServerConfig, cx: &mut Context<Self>) {
        let target = self.maintenance_storage_target(cx);
        if self.maintenance.stream_target != target {
            self.maintenance.leave();
            self.maintenance.stream_target = target.clone();
        }
        let Some(target) = target else { return };
        let capabilities = &config.environment.capabilities;
        if capabilities.storage_cleanup_run == Some(true) && self.maintenance.report_stream.is_none() {
            self.maintenance.last_run = LastRun::Loading;
            let report_target = target.clone();
            self.maintenance.report_stream = self.subscribe(model::GET_STORAGE_CLEANUP_REPORT, json!({}), cx, move |this, item, cx| {
                if this.maintenance_storage_target(cx).as_ref() != Some(&report_target) { return; }
                this.maintenance.last_run = match item {
                    Ok(serde_json::Value::Null) => LastRun::None,
                    Ok(value) => match model::decode_report(&value) {
                        Some(report) => LastRun::Report(report),
                        None => return,
                    },
                    Err(_) => LastRun::Failed,
                };
            });
        }
        if capabilities.database_storage == Some(true) {
            self.ensure_database_dismissals(cx);
            if self.maintenance.database_stream.is_none() {
                self.maintenance.database_stream = self.subscribe(model::GET_DATABASE_STORAGE, json!({}), cx, move |this, item, cx| {
                    if this.maintenance_storage_target(cx).as_ref() != Some(&target) { return; }
                    if let Ok(value) = item {
                        this.maintenance.database = model::compaction_recommendation(&value);
                        if this.maintenance.database.as_ref().is_some_and(|d| d.reclaim_failure.is_some()) {
                            this.maintenance.reclaim_requested = false;
                        }
                    }
                });
            }
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

    /// `RetentionControl`'s commit: explicit Enter or edited blur saves clamped; untouched blur
    /// discards a draft without saving.
    fn commit_artifact_retention(&mut self, field: Field, explicit: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_project_scope(cx) {
            return;
        }
        let Some(saved) = self.scoped_representative_config(cx).map(|config| field.saved(&config.settings)) else {
            return;
        };
        let input = self.maintenance.input(field).clone();
        let text = input.read(cx).value().to_string();
        let idx = field.index();
        let next_seq = self.maintenance.retention_seq.wrapping_add(1);
        match self.maintenance.retention_drafts[idx].commit(saved, &text, 1, explicit, next_seq) {
            RetentionCommit::Save(days) => {
                self.maintenance.retention_seq = next_seq;
                input.update(cx, |input, cx| input.set_value(days.to_string(), window, cx));
                self.save_retention_setting(
                    ServerSettingChange::Prepared(field.change(Some(days)).payload()),
                    move |this| this.maintenance.retention_drafts[idx].fail_submit(next_seq),
                    cx,
                );
                cx.notify();
            }
            RetentionCommit::Restore(days) => {
                input.update(cx, |input, cx| input.set_value(days.to_string(), window, cx));
                cx.notify();
            }
            RetentionCommit::Discarded => {
                cx.notify();
            }
            RetentionCommit::Unchanged => {}
        }
    }

    pub(super) fn dismiss_maintenance_retention(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        for field in Field::ALL {
            let input = self.maintenance.input(field).clone();
            if self.maintenance.retention_drafts[field.index()].cancel() {
                if input.focus_handle(cx).is_focused(window) {
                    self.focus.focus(window, cx);
                }
                return true;
            }
            if input.focus_handle(cx).is_focused(window)
                && let Some(saved) = self.scoped_representative_config(cx).and_then(|config| field.saved(&config.settings))
            {
                input.update(cx, |input, cx| input.set_value(saved.to_string(), window, cx));
                self.focus.focus(window, cx);
                return true;
            }
        }
        false
    }

    /// `IntervalSecondsField`'s commit: rounded, at least the field's minimum; text that is not a
    /// number puts the saved value back.
    fn commit_interval(&mut self, field: IntervalField, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = INTERVAL_FIELDS.iter().position(|candidate| *candidate == field) else {
            return;
        };
        let input = self.maintenance.intervals[index].clone();
        let Some(settings) = self.scoped_representative_config(cx).map(|config| config.settings.provider_panel) else {
            return;
        };
        let saved = field.seconds(&settings.resolved_background_activity());
        let seconds = field.normalize(&input.read(cx).value()).unwrap_or(saved);
        input.update(cx, |input, cx| input.set_value(seconds.to_string(), window, cx));
        if seconds != saved && self.may(SETTINGS_WRITE_SCOPE, cx) && !self.is_project_scope(cx) {
            self.update_scoped_server(ServerSettingChange::BackgroundActivity(BackgroundEdit::Override(field.change(seconds))), cx);
        }
    }

    pub(super) fn commit_focused_maintenance_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for field in Field::ALL {
            if self.maintenance.input(field).focus_handle(cx).is_focused(window) {
                self.commit_artifact_retention(field, false, window, cx);
            }
        }
        for (index, field) in INTERVAL_FIELDS.into_iter().enumerate() {
            if self.maintenance.intervals[index].focus_handle(cx).is_focused(window) {
                self.commit_interval(field, window, cx);
            }
        }
    }

    pub(super) fn maintenance_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let Some(config) = self.scoped_representative_config(cx) else {
            return vec![Block::new(&[], muted_text("Reconnect to change this environment's settings.", cx))];
        };
        self.ensure_maintenance_streams(&config, cx);
        let scoped_configs = self.scoped_all_configs(cx);
        let settings = &config.settings;
        let unavailable = self.settings_unavailable(cx);
        let env_only = self.env_only_unavailable(cx);
        let connected = self.scoped_connected_now(cx);
        let view = cx.entity().downgrade();
        let provider_checks = settings.maintenance.enable_provider_update_checks;
        let provider_checks_mixed = scoped_configs.iter().any(|cfg| cfg.settings.maintenance.enable_provider_update_checks != provider_checks);
        let continue_threads = settings.maintenance.continue_threads_after_server_update;
        let keep_awake = settings.maintenance.keep_awake_while_agents_work;
        let keep_awake_mixed = scoped_configs.iter().any(|cfg| cfg.settings.maintenance.keep_awake_while_agents_work != keep_awake);
        let (continue_inheritance, continue_mixed) = self.switch_inheritance("continueThreadsAfterServerUpdate", continue_threads, true,
            |settings| settings.maintenance.continue_threads_after_server_update, cx);
        let updates = section(
            "Updates",
            vec![
                {
                    let lock = self.managed_setting_lock(&["enableProviderUpdateChecks"], cx);
                    let row_unavail = lock.clone().or_else(|| env_only.clone());
                    Row::new("provider-update-checks", "Provider update checks")
                        .description("Check installed provider CLIs for newer available versions and fetch model catalog updates. When off, new models may not appear.")
                        .highlighted(self.is_target("provider-update-checks"))
                        .unavailable(row_unavail.clone())
                        .inheritance(environment_switch_inheritance(provider_checks, true, provider_checks_mixed, cx))
                        .reset(lock.is_none() && (!provider_checks || provider_checks_mixed), {
                            let view = view.clone();
                            move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.update_scoped_server(ServerSettingChange::Prepared(MaintenanceChange::ProviderUpdateChecks(true).payload()), cx);
                                });
                            }
                        })
                        .control(Switch::new("provider-update-checks-switch", provider_checks).mixed(provider_checks_mixed).disabled(row_unavail.is_some()).on_change({
                            let view = view.clone();
                            move |on, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.update_scoped_server(ServerSettingChange::Prepared(MaintenanceChange::ProviderUpdateChecks(on).payload()), cx);
                                });
                            }
                        }))
                        .into_any_element()
                },
                {
                    let continue_locked = self.managed_setting_lock(&["continueThreadsAfterServerUpdate"], cx).is_some();
                    self.apply_scoped_reset(
                        Row::new("continue-threads-after-server-update", "Continue threads after restarts")
                            .description("Automatically resume interrupted threads after an update, crash, or machine restart on the selected environments.")
                            .when(!connected, |row| row.status("All selected connected environments must support restart continuation."))
                            .highlighted(self.is_target("continue-threads-after-server-update"))
                            .inheritance(continue_inheritance)
                            .unavailable(unavailable.clone()),
                        &["continueThreadsAfterServerUpdate"],
                        connected && (!continue_threads || continue_mixed),
                        ServerSettingChange::Prepared(MaintenanceChange::ContinueThreads(true).payload()),
                        cx,
                    )
                    .control(Switch::new("continue-threads-switch", continue_threads).mixed(continue_mixed).disabled(!connected || continue_locked || unavailable.is_some()).on_change({
                        let view = view.clone();
                        move |on, _, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.update_scoped_server(ServerSettingChange::Prepared(MaintenanceChange::ContinueThreads(on).payload()), cx);
                            });
                        }
                    }))
                    .into_any_element()
                },
            ],
            cx,
        );
        let background = section(
            "Background activity",
            vec![
                self.background_row(&config, env_only.clone(), cx),
                {
                    let lock = self.managed_setting_lock(&["keepAwakeWhileAgentsWork"], cx);
                    let row_unavail = lock.clone().or_else(|| env_only.clone());
                    Row::new("keep-awake-while-agents-work", "Keep awake while agents work")
                        .description("Stop the computer from going to sleep while agents are working, and let it sleep once they finish. Closing a laptop lid still puts it to sleep.")
                        .highlighted(self.is_target("keep-awake-while-agents-work"))
                        .unavailable(row_unavail.clone())
                        .inheritance(environment_switch_inheritance(keep_awake, true, keep_awake_mixed, cx))
                        .reset(lock.is_none() && (!keep_awake || keep_awake_mixed), {
                            let view = view.clone();
                            move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.update_scoped_server(ServerSettingChange::Prepared(MaintenanceChange::KeepAwakeWhileAgentsWork(true).payload()), cx);
                                });
                            }
                        })
                        .control(
                            Switch::new("keep-awake-while-agents-work-switch", keep_awake).mixed(keep_awake_mixed).disabled(row_unavail.is_some()).on_change({
                                let view = view.clone();
                                move |on, _, cx| {
                                    let _ = view.update(cx, |this, cx| {
                                        this.update_scoped_server(ServerSettingChange::Prepared(MaintenanceChange::KeepAwakeWhileAgentsWork(on).payload()), cx);
                                    });
                                }
                            }),
                        )
                        .into_any_element()
                },
            ],
            cx,
        );
        let usage_retention_days = Field::UsageHistory.saved(settings);
        self.maintenance.retention_drafts[Field::UsageHistory.index()].reconcile_saved(usage_retention_days);
        if let Some(days) = usage_retention_days {
            Self::sync_input(self.maintenance.input(Field::UsageHistory), &days.to_string(), window, cx);
        }
        let usage_lock = self.managed_setting_lock(&["usageHistoryRetentionDays"], cx);
        let usage_unavail = usage_lock.clone().or_else(|| env_only.clone());
        let usage_history = section(
            "Usage history",
            vec![
                Row::new("usage-history-retention", "Keep usage history")
                    .description("How many days of per-turn token usage history to keep on this server. Turn off to keep all history indefinitely.")
                    .highlighted(self.is_target("usage-history-retention"))
                    .unavailable(usage_unavail.clone())
                    .reset(
                        usage_lock.is_none()
                            && settings.usage_history_retention_days != UsageHistoryRetentionDays::default(),
                        {
                            let view = view.clone();
                            move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.update_scoped_server(
                                        ServerSettingChange::Prepared(
                                            MaintenanceChange::UsageHistoryRetentionDays(
                                                UsageHistoryRetentionDays::default(),
                                            )
                                            .payload(),
                                        ),
                                        cx,
                                    );
                                });
                            }
                        },
                    )
                    .control(self.maintenance_retention(
                        Field::UsageHistory,
                        "usage-history-retention-switch",
                        usage_retention_days,
                        usage_unavail.is_some(),
                        cx,
                    ))
                    .into_any_element(),
            ],
            cx,
        );
        let mut blocks = vec![
            Block::new(&["provider-update-checks", "continue-threads-after-server-update"], updates),
            Block::new(&["background-activity", "git-fetch-interval", "keep-awake-while-agents-work"], background),
            Block::new(&["usage-history-retention"], usage_history),
        ];
        if !self.is_project_scope(cx) {
            blocks.extend(self.storage_blocks(&config, env_only, window, cx));
        }
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
        let lock = self.managed_setting_lock(&["backgroundActivity"], cx);
        let row_unavail = lock.clone().or(unavailable);
        let disabled = row_unavail.is_some();
        let option = model::profile_option(settings);
        let selected = match option {
            ProfileOption::Profile(profile) => profile.as_str(),
            ProfileOption::Advanced => model::ADVANCED_OPTION,
        };
        let mut options: Vec<SelectOption> = PROFILES.iter().map(|profile| SelectOption::new(profile.as_str(), model::profile_label(*profile))).collect();
        options.push(SelectOption::new(model::ADVANCED_OPTION, "Advanced"));
        let view = cx.entity().downgrade();
        let select_view = view.clone();
        let select = Select::new("background-activity-select", options).selected(selected).width(160.).disabled(disabled).on_change(move |value, window, cx| {
            if value.as_ref() == model::ADVANCED_OPTION {
                let _ = select_view.update(cx, |this, cx| this.open_background_dialog(window, cx));
            } else if let Some(profile) = model::parse_profile(&value) {
                let _ = select_view.update(cx, |this, cx| {
                    this.update_scoped_server(ServerSettingChange::Prepared(model::background_profile_payload(profile)), cx);
                });
            }
        });
        let configure = (option == ProfileOption::Advanced).then(|| {
            Button::new("background-activity-configure", ButtonVariant::Outline, ButtonSize::IconSm)
                .icon(Icon::Settings)
                .tooltip("Configure background activity")
                .disabled(disabled)
                .on_click(cx.listener(|this, _, window, cx| this.open_background_dialog(window, cx)))
        });
        let reset_view = view.clone();
        Row::new("background-activity", "Background activity")
            .title_tooltip(model::POLICY_TOOLTIP)
            .description(model::background_description(settings))
            .highlighted(self.is_target("background-activity") || self.is_target("git-fetch-interval"))
            .unavailable(row_unavail)
            .reset(lock.is_none() && model::background_customized(settings), move |_, _, cx| {
                let _ = reset_view.update(cx, |this, cx| {
                    this.update_scoped_server(ServerSettingChange::Prepared(model::background_reset_payload()), cx);
                });
            })
            .control(h_flex().gap(r(8.)).child(select).children(configure))
            .into_any_element()
    }

    fn open_background_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.may(SETTINGS_WRITE_SCOPE, cx) || self.managed_setting_lock(&["backgroundActivity"], cx).is_some() {
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

    /// `BackgroundActivityAdvancedDialog` and `DatabaseStorageNotice`'s Reclaim confirmation.
    pub(super) fn background_dialog_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(overlay) = self.reclaim_dialog_overlay(window, cx) {
            return Some(overlay);
        }
        let focus = self.maintenance.background_dialog.clone()?;
        let config = self.scoped_representative_config(cx)?;
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
        let policy_view = cx.entity().downgrade();
        let policy = row(h_flex()).child(text_block("Shared policy", "Controls whether background work may run after a subscribed interval fires.")).child(
            Select::new("background-shared-policy", PROFILES.iter().map(|profile| SelectOption::new(profile.as_str(), model::profile_label(*profile))).collect())
                .selected(resolved.profile.as_str())
                .width(160.)
                .on_change(move |value, _, cx| {
                    if let Some(profile) = model::parse_profile(&value) {
                        let _ = policy_view.update(cx, |this, cx| {
                            this.update_scoped_server(ServerSettingChange::BackgroundActivity(BackgroundEdit::SharedPolicy(profile)), cx);
                        });
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
            let switch_view = cx.entity().downgrade();
            h_flex()
                .flex_1()
                .justify_between()
                .gap(r(12.))
                .px(r(16.))
                .py(r(12.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(key.label()))
                .child(Switch::new(SharedString::from(format!("background-{}", key.label())), key.value(&resolved)).disabled(!can_write).on_change(move |on, _, cx| {
                    let _ = switch_view.update(cx, |this, cx| {
                        this.update_scoped_server(ServerSettingChange::BackgroundActivity(BackgroundEdit::Override(Override::Pause(key, on))), cx);
                    });
                }))
                .into_any_element()
        });
        let [a, b, c, d] = switches;
        card = card
            .child(h_flex().border_t_1().border_color(divider).child(div().flex_1().border_r_1().border_color(divider).child(a)).child(div().flex_1().child(b)))
            .child(h_flex().border_t_1().border_color(divider).child(div().flex_1().border_r_1().border_color(divider).child(c)).child(div().flex_1().child(d)));
        let reset_view = cx.entity().downgrade();
        let footer = dialog_footer(
            false,
            vec![
                Button::new("background-reset-all", ButtonVariant::Outline, ButtonSize::Default)
                    .label("Reset all")
                    .disabled(!can_write)
                    .on_click(move |_, _, cx| {
                        let _ = reset_view.update(cx, |this, cx| {
                            this.update_scoped_server(ServerSettingChange::Prepared(model::background_reset_payload()), cx);
                        });
                    })
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

    /// `RetentionControl`: a day count while on, "Off" while off, and a switch that opens a
    /// draft at eight days without saving until Enter or blur after editing.
    fn maintenance_retention(&self, field: Field, id: &'static str, value: Option<u32>, disabled: bool, cx: &Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let idx = field.index();
        let on = self.maintenance.retention_drafts[idx].is_on(value);
        h_flex()
            .gap(r(12.))
            .map(|this| {
                if on {
                    this.child(
                        h_flex()
                            .gap(r(6.))
                            .child(div().w(r(116.)).child(NumberField::new(self.maintenance.input(field)).disabled(disabled)))
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("days")),
                    )
                } else {
                    this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(field.off_label()))
                }
            })
            .child(self.retention_switch(
                id,
                on,
                disabled,
                move |this, pressed| this.maintenance.retention_switch_pressed[idx] = pressed,
                move |this, next, window, cx| {
                    let saved = this.scoped_representative_config(cx).and_then(|config| field.saved(&config.settings));
                    match this.maintenance.retention_drafts[idx].toggle(saved, next) {
                        RetentionToggle::OpenDraft(days) => {
                            let initial = field.default_when_enabled().unwrap_or(days);
                            this.maintenance.input(field).update(cx, |input, cx| {
                                input.set_value(initial.to_string(), window, cx);
                                input.focus(window, cx);
                            });
                            cx.notify();
                        }
                        RetentionToggle::SaveOff => {
                            this.update_scoped_server(ServerSettingChange::Prepared(field.change(None).payload()), cx);
                            cx.notify();
                        }
                        RetentionToggle::DiscardDraft => {
                            cx.notify();
                        }
                        RetentionToggle::Noop => {}
                    }
                },
                cx,
            ))
    }

    /// `StorageCleanupSections`: the database notice, retention, and the cleanup run.
    fn storage_blocks(&mut self, config: &ServerConfig, unavailable: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let capabilities = &config.environment.capabilities;
        let mut blocks = Vec::new();
        if let Some(notice) = self.database_notice(config, cx) {
            blocks.push(Block::new(&["storage-database"], notice));
        }
        if self.scoped_all_configs(cx).iter().any(|cfg| cfg.environment.capabilities.storage_cleanup != Some(true)) {
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
            self.maintenance.retention_drafts[field.index()].reconcile_saved(days);
            if let Some(days) = days {
                Self::sync_input(self.maintenance.input(field), &days.to_string(), window, cx);
            }
        }
        let artifacts_unavail = self.scoped_unavailable(&["storageCleanup.browserArtifactsAfterDays"], unavailable.clone(), cx);
        let logs_unavail = self.scoped_unavailable(&["storageCleanup.logsAfterDays"], unavailable, cx);
        let artifacts = section(
            "Artifacts and logs",
            vec![
                Row::new("storage-browser-artifacts", "Delete old browser artifacts")
                    .description("Delete saved browser captures after this many days. Older capture links will no longer open.")
                    .unavailable(artifacts_unavail.clone())
                    .control(self.maintenance_retention(
                        Field::Artifacts,
                        "storage-artifacts-switch",
                        retention.browser_artifacts_after_days,
                        artifacts_unavail.is_some(),
                        cx,
                    ))
                    .into_any_element(),
                Row::new("storage-logs", "Delete old rotated logs")
                    .description("Delete inactive rotated log files after this many days. Current logs are kept.")
                    .unavailable(logs_unavail.clone())
                    .control(self.maintenance_retention(
                        Field::Logs,
                        "storage-logs-switch",
                        retention.logs_after_days,
                        logs_unavail.is_some(),
                        cx,
                    ))
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
            let cleanup = if self.maintenance_storage_target(cx).is_some() {
                self.storage_cleanup_section(config, cx)
            } else {
                section(t(cx, "Cleanup"), vec![Row::new("storage-delete-now", t(cx, "Delete now"))
                    .description(t(cx, "Select one connected environment to run cleanup.")).into_any_element()], cx).into_any_element()
            };
            blocks.push(Block::new(&["storage-delete-now"], cleanup));
        }
        blocks
    }

    fn open_reclaim_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.maintenance_may(ENVIRONMENT_MAINTAIN_SCOPE, cx) {
            return;
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.maintenance.reclaim_confirming = Some(focus);
        cx.notify();
    }

    fn close_reclaim_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.maintenance.reclaim_confirming.take().is_some() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    fn start_database_reclaim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.may(ENVIRONMENT_MAINTAIN_SCOPE, cx) {
            self.close_reclaim_dialog(window, cx);
            return;
        }
        let Some(storage) = self.maintenance.database.clone() else {
            self.close_reclaim_dialog(window, cx);
            return;
        };
        self.maintenance.reclaim_confirming = None;
        self.maintenance.reclaim_requested = true;
        self.focus.focus(window, cx);
        let amount = storage.amount();
        let label = self.scoped_raw_representative_config(cx).map(|cfg| cfg.environment.label).unwrap_or_default();
        let minutes = storage.reclaim_minutes();
        let target = self.maintenance_storage_target(cx);
        let task = self.scoped_rpc(ENVIRONMENT_MAINTAIN_SCOPE, model::RECLAIM_DATABASE_SPACE, json!({}), cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.maintenance_storage_target(cx) != target { return; }
                let toast = match result {
                    Ok(_) => Toast::new(ToastKind::Success, format!("Reclaiming {amount} on {label}"))
                        .description(format!("T3 Code there restarts and is back in about {minutes} min.")),
                    Err(error) => {
                        this.maintenance.reclaim_requested = false;
                        Toast::new(ToastKind::Error, format!("Could not reclaim space on {label}"))
                            .description(failure_text(&error, "An error occurred."))
                    }
                };
                this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn reclaim_dialog_overlay(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let focus = self.maintenance.reclaim_confirming.clone()?;
        let storage = self.maintenance.database.as_ref()?;
        let label = self.scoped_raw_representative_config(cx)?.environment.label;
        let (title, description) = storage.reclaim_confirmation(&label);
        let footer = dialog_footer(
            false,
            vec![
                Button::new("storage-database-reclaim-cancel", ButtonVariant::Outline, ButtonSize::Default)
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, window, cx| this.close_reclaim_dialog(window, cx)))
                    .into_any_element(),
                Button::new("storage-database-reclaim-confirm", ButtonVariant::Default, ButtonSize::Default)
                    .label("Restart and reclaim")
                    .on_click(cx.listener(|this, _, window, cx| this.start_database_reclaim(window, cx)))
                    .into_any_element(),
            ],
            cx,
        );
        let view = cx.entity().downgrade();
        let frame = DialogFrame {
            id: "storage-database-reclaim-dialog",
            title: title.into(),
            description: description.into(),
            max_width: 448.,
            body: div().into_any_element(),
            footer: footer.into_any_element(),
            focus,
        };
        Some(frame.render(
            window,
            move |window, cx| {
                let _ = view.update(cx, |this, cx| this.close_reclaim_dialog(window, cx));
            },
            cx,
        ))
    }

    /// `DatabaseStorageNotice`: offers in-app Reclaim when supported & permitted, else `t3 doctor`.
    fn database_notice(&self, config: &ServerConfig, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dismissed = cx.try_global::<DismissedDatabaseNotice>()?;
        let environment = &config.environment;
        if environment.capabilities.database_storage != Some(true) || !dismissed.loaded
            || dismissed.environments.contains(&environment.environment_id)
        {
            return None;
        }
        let storage = self.maintenance.database.clone()?;
        let reclaimable = storage.can_reclaim && self.maintenance_may(ENVIRONMENT_MAINTAIN_SCOPE, cx);
        let restarting = self.maintenance.reclaim_requested && storage.reclaim_failure.is_none();
        let command = storage.compact_command.clone();
        let env_id = environment.environment_id.clone();
        let dismiss = Button::new("storage-database-dismiss", ButtonVariant::Ghost, ButtonSize::Xs)
            .label(t(cx, "Dismiss"))
            .on_click(cx.listener(move |_, _, _, cx| {
                let dismissed = cx.global_mut::<DismissedDatabaseNotice>();
                dismissed.environments.insert(env_id.clone());
                t3_client::store::save_ui_state(DATABASE_NOTICE_DISMISSALS, &dismissed.environments);
                cx.notify();
            }));
        let machine_row = if reclaimable {
            Row::new("storage-database-machine", storage.title(&environment.label))
                .description(storage.reclaim_description())
                .control(
                    h_flex()
                        .gap(r(6.))
                        .child(
                            Button::new("storage-database-reclaim", ButtonVariant::Outline, ButtonSize::Xs)
                                .label(if restarting { "Restarting\u{2026}" } else { "Reclaim" })
                                .disabled(restarting)
                                .on_click(cx.listener(|this, _, window, cx| this.open_reclaim_dialog(window, cx))),
                        )
                        .child(dismiss),
                )
                .into_any_element()
        } else {
            Row::new("storage-database-machine", storage.title(&environment.label))
                .description_content(div().font_family(cx.theme().mono_font_family.clone()).child(command.clone()))
                .control(
                    h_flex()
                        .gap(r(6.))
                        .child(Button::new("storage-database-copy", ButtonVariant::Outline, ButtonSize::Xs).label("Copy command").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                        }))
                        .child(dismiss),
                )
                .into_any_element()
        };
        let rows = vec![
            Row::new("storage-database-intro", "Reclaim freed database space")
                .description(model::database_description(reclaimable))
                .into_any_element(),
            machine_row,
        ];
        Some(section("Database", rows, cx).into_any_element())
    }

    /// `CleanupSection`: Delete now with the last run, and the last run's results folded below.
    fn storage_cleanup_section(&mut self, config: &ServerConfig, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let has_rules = model::has_cleanup_rules(&config.settings);
        let allowed = self.maintenance_may(SETTINGS_WRITE_SCOPE, cx);
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
        if self.maintenance.cleanup_pending || !self.maintenance_may(SETTINGS_WRITE_SCOPE, cx) {
            return;
        }
        self.maintenance.cleanup_pending = true;
        let label = self.scoped_raw_representative_config(cx).map(|cfg| cfg.environment.label).unwrap_or_default();
        let target = self.maintenance_storage_target(cx);
        let task = self.scoped_rpc(SETTINGS_WRITE_SCOPE, model::RUN_STORAGE_CLEANUP, json!({}), cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.maintenance_storage_target(cx) != target { return; }
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

fn environment_switch_inheritance(value: bool, default: bool, mixed: bool, cx: &App) -> SettingInheritance {
    let provided = value != default;
    let flag = |on| t(cx, if on { "On" } else { "Off" }).into();
    let summary = if mixed { "Mixed across selected machines" } else if provided { "Environment" } else { "Default" };
    SettingInheritance::new(t(cx, summary), vec![
        SettingInheritanceLayer::new(SettingSource::Environment, provided.then(|| flag(value)), provided && !mixed),
        SettingInheritanceLayer::new(SettingSource::Default, Some(flag(default)), !provided && !mixed),
    ]).mixed(mixed)
}

#[cfg(all(test, feature = "snapshot"))]
mod parity_tests {
    use super::*;
    use gpui::{AppContext, HeadlessAppContext, IntoElement, ParentElement, Render, Styled, div, px, size};
    use serde_json::json;
    use std::sync::Arc;
    use t3_core::settings::search::Section;

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().bg(hsla(palette(cx).background)).children(super::super::overlay(window, cx))
        }
    }

    #[test]
    fn reclaim_keep_awake_and_deferred_retention_parity() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let drive = |cx: &mut HeadlessAppContext| {
            cx.run_until_parked();
            runtime.block_on(async {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            });
            cx.run_until_parked();
        };
        let mut cx = HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(),
            Arc::new(crate::icons::Assets),
            gpui_platform::current_headless_renderer,
        );
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
                        "environment": {
                            "environmentId": "fixture-env",
                            "label": "Studio Mac",
                            "capabilities": { "storageCleanup": true, "storageCleanupRun": true, "databaseStorage": true }
                        },
                        "settings": {
                            "keepAwakeWhileAgentsWork": true,
                            "storageCleanup": { "browserArtifactsAfterDays": 30, "logsAfterDays": null }
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx
            .open_window(size(px(1280.), px(900.)), |_, cx| cx.new(|_| SettingsFrame))
            .unwrap();
        cx.update(|cx| super::super::open(Section::Maintenance, cx));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();
        cx.run_until_parked();
        runtime.block_on(async {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        });

        // Fulfill stream requests (`server.getDatabaseStorage` + `server.getStorageCleanupReport`).
        let (_, streams) = fixture.take_requests();
        let mut db_tx = None;
        for (method, reply) in streams {
            let (handle, tx) = t3_client::session::StreamHandle::fixture();
            let _ = reply.send(Ok(handle));
            if method == model::GET_DATABASE_STORAGE {
                db_tx = Some(tx);
            } else {
                let _ = tx.try_send(Ok(serde_json::Value::Null));
                std::mem::forget(tx);
            }
        }
        let db_tx = db_tx.expect("database storage stream subscribed");
        runtime.block_on(async {
            db_tx
                .send(Ok(json!({
                    "databaseBytes": 4_294_967_296.0,
                    "reclaimableBytes": 2_147_483_648.0,
                    "incrementalVacuumEnabled": false,
                    "compactCommand": "t3 doctor compact",
                    "canReclaim": true,
                    "reclaimFailure": null
                })))
                .await
                .unwrap();
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        });
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();

        let view = cx.update(|cx| cx.global::<super::super::Host>().view.clone().expect("settings view open"));
        view.read_with(&cx, |view, _| {
            assert!(view.maintenance.database.as_ref().is_some_and(|db| db.can_reclaim));
        });

        if let Some(out) = std::env::var_os("T3_PARITY_386_SHOTS_DIR") {
            let out = std::path::PathBuf::from(out);
            std::fs::create_dir_all(&out).unwrap();
            cx.capture_screenshot(window.into()).unwrap().save(out.join("maintenance-overview.png")).unwrap();
        }

        // 1. Keep awake while agents work toggle writes server.updateSettings.
        cx.update_window(window.into(), |_, _, cx| {
            view.update(cx, |view, cx| {
                SettingsView::save_prepared(&view.state, MaintenanceChange::KeepAwakeWhileAgentsWork(false).payload(), cx);
            });
        })
        .unwrap();
        drive(&mut cx);
        let (requests, _) = fixture.take_requests();
        let (method, payload, reply) = requests.into_iter().find(|(m, _, _)| *m == "server.updateSettings").expect("updateSettings sent");
        assert_eq!(method, "server.updateSettings");
        assert_eq!(payload["patch"]["keepAwakeWhileAgentsWork"], json!(false));
        let _ = reply.send(Ok(json!({})));
        drive(&mut cx);

        // 2. RetentionControl: toggling Logs on opens draft (8) without saving; untouched blur discards it.
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.maintenance.retention_drafts[Field::Logs.index()].toggle(None, true), RetentionToggle::OpenDraft(8));
                view.maintenance.input(Field::Logs).update(cx, |input, cx| {
                    input.set_value("8", window, cx);
                });
                view.commit_artifact_retention(Field::Logs, false, window, cx);
                assert_eq!(view.maintenance.retention_drafts[Field::Logs.index()], RetentionDraft::Closed);
            });
        })
        .unwrap();
        drive(&mut cx);
        assert!(fixture.take_requests().0.into_iter().all(|(m, _, _)| m != "server.updateSettings"), "untouched draft blur must not save");

        // Escape cancels open draft without closing Settings.
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, _cx| {
                assert_eq!(view.maintenance.retention_drafts[Field::Logs.index()].toggle(None, true), RetentionToggle::OpenDraft(8));
                view.maintenance.retention_drafts[Field::Logs.index()].mark_edited();
            });
            window.dispatch_action(Box::new(super::super::CloseSettings), cx);
        })
        .unwrap();
        drive(&mut cx);
        assert!(cx.update(|cx| cx.global::<super::super::Host>().view.is_some()), "Escape cancels retention draft without closing Settings");
        view.read_with(&cx, |view, _| assert_eq!(view.maintenance.retention_drafts[Field::Logs.index()], RetentionDraft::Closed));

        // Failed initial retention enable rolls back Submitting -> Closed so the switch reverts to Off and retries cleanly.
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.maintenance.retention_drafts[Field::Logs.index()].toggle(None, true), RetentionToggle::OpenDraft(8));
                view.maintenance.retention_drafts[Field::Logs.index()].mark_edited();
                view.maintenance.input(Field::Logs).update(cx, |input, cx| input.set_value("14", window, cx));
                view.commit_artifact_retention(Field::Logs, true, window, cx);
                assert!(view.maintenance.retention_drafts[Field::Logs.index()].is_on(None));
            });
        })
        .unwrap();
        drive(&mut cx);
        let (requests, _) = fixture.take_requests();
        let (_, _, reply) = requests.into_iter().find(|(m, _, _)| *m == "server.updateSettings").expect("initial retention save sent");
        let _ = reply.send(Err(t3_client::session::RpcError::Closed));
        drive(&mut cx);
        view.read_with(&cx, |view, _| {
            assert_eq!(view.maintenance.retention_drafts[Field::Logs.index()], RetentionDraft::Closed);
            assert!(!view.maintenance.retention_drafts[Field::Logs.index()].is_on(None), "failed enable must flip switch back Off");
        });

        // Re-enabling after failure retries saving via Enter cleanly.
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.maintenance.retention_drafts[Field::Logs.index()].toggle(None, true), RetentionToggle::OpenDraft(8));
                view.maintenance.retention_drafts[Field::Logs.index()].mark_edited();
                view.maintenance.input(Field::Logs).update(cx, |input, cx| input.set_value("14", window, cx));
                view.commit_artifact_retention(Field::Logs, true, window, cx);
            });
        })
        .unwrap();
        drive(&mut cx);
        let (requests, _) = fixture.take_requests();
        let (_, payload, reply) = requests.into_iter().find(|(m, _, _)| *m == "server.updateSettings").expect("retry after failed enable saves");
        assert_eq!(payload["patch"]["storageCleanup"]["logsAfterDays"], json!(14));
        let _ = reply.send(Ok(json!({
            "keepAwakeWhileAgentsWork": false,
            "storageCleanup": { "browserArtifactsAfterDays": 30, "logsAfterDays": 14 }
        })));
        drive(&mut cx);
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();
        view.read_with(&cx, |view, _| assert_eq!(view.maintenance.retention_drafts[Field::Logs.index()], RetentionDraft::Closed));

        // 3. Reclaim confirmation dialog opens, captures, and confirms RPC; permission missing blocks it.
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| view.open_reclaim_dialog(window, cx));
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();
        view.read_with(&cx, |view, _| assert!(view.maintenance.reclaim_confirming.is_some()));

        if let Some(out) = std::env::var_os("T3_PARITY_386_SHOTS_DIR") {
            let out = std::path::PathBuf::from(out);
            cx.capture_screenshot(window.into()).unwrap().save(out.join("maintenance-reclaim-dialog.png")).unwrap();
        }

        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| view.start_database_reclaim(window, cx));
        })
        .unwrap();
        drive(&mut cx);
        let (requests, _) = fixture.take_requests();
        let (method, payload, reply) = requests
            .into_iter()
            .find(|(m, _, _)| *m == model::RECLAIM_DATABASE_SPACE)
            .expect("reclaimDatabaseSpace RPC sent");
        assert_eq!(method, model::RECLAIM_DATABASE_SPACE);
        assert_eq!(payload, json!({}));
        let _ = reply.send(Ok(json!({})));
        drive(&mut cx);
        view.read_with(&cx, |view, _| assert!(view.maintenance.reclaim_requested));

        fixture.grants.send(Some(crate::context_menu::project_filter_tests::session(false))).unwrap();
        drive(&mut cx);
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| view.open_reclaim_dialog(window, cx));
        })
        .unwrap();
        view.read_with(&cx, |view, _| assert!(view.maintenance.reclaim_confirming.is_none()));

        drop(db_tx);
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
    fn retention_switch_drag_off_resets_pressed_guard(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
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
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": {
                            "environmentId": "fixture-env",
                            "label": "Studio Mac",
                            "capabilities": { "storageCleanup": true }
                        },
                        "settings": {
                            "storageCleanup": { "browserArtifactsAfterDays": null, "logsAfterDays": null }
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|_, cx| super::super::open(Section::Maintenance, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let view = vcx.read(|cx| cx.global::<super::super::Host>().view.clone().expect("settings view open"));
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.maintenance.retention_drafts[Field::Logs.index()].toggle(None, true), RetentionToggle::OpenDraft(8));
                view.maintenance.retention_drafts[Field::Logs.index()].mark_edited();
                view.maintenance.input(Field::Logs).update(cx, |input, cx| input.set_value("14", window, cx));
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear();
        });
        let switch_center = vcx.debug_bounds("storage-logs-switch").expect("logs retention switch bounds").center();
        vcx.simulate_mouse_move(switch_center, None::<MouseButton>, Default::default());
        vcx.simulate_mouse_down(switch_center, MouseButton::Left, Default::default());
        vcx.run_until_parked();
        assert!(
            view.read_with(&vcx, |view, _| view.maintenance.retention_switch_pressed[Field::Logs.index()]),
            "pressing switch sets pressed guard"
        );
        let outside = gpui::point(px(4.), px(4.));
        vcx.simulate_mouse_move(outside, Some(MouseButton::Left), Default::default());
        vcx.simulate_mouse_up(outside, MouseButton::Left, Default::default());
        vcx.run_until_parked();
        assert!(
            !view.read_with(&vcx, |view, _| view.maintenance.retention_switch_pressed[Field::Logs.index()]),
            "releasing outside switch clears pressed guard"
        );
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.commit_artifact_retention(Field::Logs, false, window, cx);
            });
        });
        vcx.run_until_parked();
        runtime.block_on(async {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        });
        vcx.run_until_parked();
        let (requests, _) = fixture.take_requests();
        let (_, payload, reply) = requests.into_iter().find(|(m, _, _)| *m == "server.updateSettings").expect("blur after drag-off commits");
        assert_eq!(payload["patch"]["storageCleanup"]["logsAfterDays"], json!(14));
        let _ = reply.send(Ok(json!({})));
        vcx.run_until_parked();
        state.update(&mut vcx, |state, _| state.stop_pump());
    }
}
