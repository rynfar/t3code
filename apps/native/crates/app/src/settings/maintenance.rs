//! Maintenance: `MaintenanceSettingsPanel` in `web/src/components/settings/SettingsPanels.tsx`
//! for the connected environment: provider update checks and restart continuation, background
//! activity with its advanced dialog (`BackgroundActivitySettings.tsx`), the storage sections of
//! `StorageSettings.tsx` (database notice, artifact and log retention, Delete now and the last
//! run's report), and the link to Diagnostics. Rules live in `t3_core::settings::maintenance`.

use std::{collections::{HashMap, HashSet}, time::Duration};

use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div, list, prelude::FluentBuilder,
};
use gpui_component::input::{Input, InputEvent, InputState, NumberInputEvent, StepAction};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::json;
use t3_client::session::RpcError;
use t3_client::supervisor::ConnectionStatus;
use t3_core::i18n::IcuArg;
use t3_core::device::SETTINGS_WRITE_SCOPE;
use t3_core::sidebar::grouping::EnvironmentOwner;
use crate::i18n::{t, t_fmt};
use t3_core::server_config::{DEFAULT_USAGE_HISTORY_RETENTION_DAYS, ServerConfig, ServerSettings, UsageHistoryRetentionDays};
use t3_core::settings::ServerSettingChange;
use t3_core::settings::diagnostics::ENVIRONMENT_MAINTAIN_SCOPE;
use t3_core::settings::maintenance::{
    self as model, BackgroundEdit, CleanupReport, DatabaseStorage, INTERVAL_FIELDS, IntervalField, LastRun, MaintenanceChange, Outcome, Override, PROFILES, PauseKey,
    ProfileOption, ReportEntry, Retention, RetentionCommit, RetentionDraft, RetentionToggle,
};
use t3_core::settings::search::Section;

use super::controls::{Button, ButtonSize, ButtonVariant, DialogFrame, NumberField, Row, Select, SelectOption, SettingInheritance, SettingInheritanceLayer, SettingSource, Switch, dialog_footer, group, icon, muted_text, r, section, section_title};
use super::providers::failure_text;
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

#[derive(Clone, PartialEq, Eq)]
struct StorageConnection {
    owner: Option<EnvironmentOwner>,
    active_connection: Option<u64>,
    cleanup: bool,
    database: bool,
}

struct StorageTarget {
    id: String,
    label: String,
    connection: StorageConnection,
    has_rules: bool,
    can_clean: bool,
    can_reclaim: bool,
}

struct StorageReports {
    connection: StorageConnection,
    last_run: LastRun,
    database: Option<DatabaseStorage>,
    reclaim_requested: bool,
    results: ListState,
    _streams: Vec<Task<()>>,
}

pub(crate) struct MaintenanceState {
    artifacts_days: Entity<InputState>,
    logs_days: Entity<InputState>,
    usage_history_days: Entity<InputState>,
    retention_drafts: [RetentionDraft; 3],
    retention_switch_pressed: [bool; 3],
    retention_seq: u64,
    intervals: [Entity<InputState>; 4],
    storage: HashMap<String, StorageReports>,
    cleanup_pending: bool,
    reclaim_confirming: Option<(String, FocusHandle)>,
    last_run_open: bool,
    background_dialog: Option<FocusHandle>,
    ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

const DATABASE_NOTICE_DISMISSALS: &str = "dismissed-database-storage";

#[derive(Default)]
struct DismissedDatabaseNotice {
    environments: HashSet<String>,
    loaded: bool,
    save_task: Option<Task<()>>,
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
            storage: HashMap::new(),
            cleanup_pending: false,
            reclaim_confirming: None,
            last_run_open: false,
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
        self.storage.clear();
        self.last_run_open = false;
        self.ticker = None;
        self.background_dialog = None;
        self.reclaim_confirming = None;
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

    fn storage_targets(&self, cx: &App) -> Vec<StorageTarget> {
        if self.is_project_scope(cx) {
            return Vec::new();
        }
        let app = self.state.read(cx);
        let make_target = |config: &ServerConfig, owner, active_connection, grants: Option<&t3_core::auth::AuthSessionState>| {
            let environment = &config.environment;
            let cleanup = environment.capabilities.storage_cleanup_run == Some(true);
            let database = environment.capabilities.database_storage == Some(true);
            StorageTarget {
                id: environment.environment_id.clone(),
                label: environment.label.clone(),
                connection: StorageConnection { owner, active_connection, cleanup, database },
                has_rules: cleanup && model::has_cleanup_rules(&config.settings),
                can_clean: cleanup && grants.is_some_and(|grants| grants.grants(SETTINGS_WRITE_SCOPE)),
                can_reclaim: database && grants.is_some_and(|grants| grants.grants(ENVIRONMENT_MAINTAIN_SCOPE)),
            }
        };
        let owners = self.scoped_environment_owners(cx);
        if owners.is_empty() && self.scope.machine.is_none() && self.connected_now(cx) {
            let grants = self.session(cx);
            return app.server_config.config.as_ref().map(|config| {
                make_target(config, None, app.connection_id(), grants.as_ref())
            }).into_iter().collect();
        }
        owners.into_iter().filter_map(|owner| {
            let entry = app.fleet.environment(&owner.saved_id)?;
            Some(make_target(entry.config.config.as_ref()?, Some(owner), None, entry.grants.as_ref()))
        }).collect()
    }

    fn storage_rpc(
        &self, target: &StorageTarget, scope: &'static str, method: &'static str, cx: &App,
    ) -> impl Future<Output = Result<serde_json::Value, RpcError>> + use<> {
        let app = self.state.read(cx);
        let destination = target.connection.owner.as_ref().map(|owner| app.connections.destination(owner, scope));
        let active = app.client_handle();
        async move {
            let client = if let Some(destination) = destination {
                destination.await.map_err(|_| RpcError::Closed)?.client
            } else {
                active.ok_or(RpcError::Closed)?
            };
            client.request(method, json!({})).await
        }
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
            cx.update(|cx| cx.set_global(DismissedDatabaseNotice { loaded: true, environments, save_task: None }));
            let _ = this.update(cx, |_, cx| cx.notify());
        }).detach();
    }

    fn storage_stream(
        &self, target: &StorageTarget, method: &'static str, cx: &mut Context<Self>,
        on_item: impl Fn(&mut StorageReports, Result<serde_json::Value, RpcError>) + 'static,
    ) -> Task<()> {
        let app = self.state.read(cx);
        let lookup = target.connection.owner.as_ref().map(|owner| app.connections.client(&owner.saved_id));
        let active = app.client_handle();
        let id = target.id.clone();
        let connection = target.connection.clone();
        cx.spawn(async move |this, cx| {
            let client = if let Some(lookup) = lookup { lookup.await } else { active };
            let update = |item, cx: &mut gpui::AsyncApp| {
                this.update(cx, |this, cx| {
                    let Some(reports) = this.maintenance.storage.get_mut(&id).filter(|entry| entry.connection == connection) else { return false };
                    on_item(reports, item);
                    cx.notify();
                    true
                }).unwrap_or(false)
            };
            let Some(client) = client else {
                update(Err(RpcError::Closed), cx);
                return;
            };
            let mut delay = Duration::from_millis(250);
            loop {
                match client.stream(method, json!({})).await {
                    Ok(mut stream) => {
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(value) => {
                                    delay = Duration::from_millis(250);
                                    if !update(Ok(value), cx) { return; }
                                }
                                Err(RpcError::Closed) => break,
                                Err(error) => {
                                    update(Err(error), cx);
                                    return;
                                }
                            }
                        }
                    }
                    Err(RpcError::Closed) => {}
                    Err(error) => {
                        update(Err(error), cx);
                        return;
                    }
                }
                cx.background_executor().timer(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        })
    }

    /// Follows cleanup and database reports for the selected environments while the page shows.
    fn ensure_maintenance_streams(&mut self, targets: &[StorageTarget], cx: &mut Context<Self>) {
        self.maintenance.storage.retain(|id, entry| {
            targets.iter().any(|target| &target.id == id && target.connection == entry.connection)
        });
        if let Some((id, _)) = &self.maintenance.reclaim_confirming
            && !targets.iter().any(|target| &target.id == id && target.can_reclaim)
        {
            self.maintenance.reclaim_confirming = None;
        }
        if targets.iter().any(|target| target.connection.database) {
            self.ensure_database_dismissals(cx);
        }
        for target in targets {
            if self.maintenance.storage.contains_key(&target.id) || !(target.connection.cleanup || target.connection.database) {
                continue;
            }
            let mut streams = Vec::new();
            if target.connection.cleanup {
                streams.push(self.storage_stream(target, model::GET_STORAGE_CLEANUP_REPORT, cx, |reports, item| {
                    reports.last_run = match item {
                        Ok(serde_json::Value::Null) => LastRun::None,
                        Ok(value) => match model::decode_report(&value) {
                            Some(report) => LastRun::Report(report),
                            None => return,
                        },
                        Err(_) => LastRun::Failed,
                    };
                }));
            }
            if target.connection.database {
                streams.push(self.storage_stream(target, model::GET_DATABASE_STORAGE, cx, |reports, item| {
                    if let Ok(value) = item {
                        reports.database = model::compaction_recommendation(&value);
                        if reports.database.as_ref().is_none_or(|storage| storage.reclaim_failure.is_some()) {
                            reports.reclaim_requested = false;
                        }
                    }
                }));
            }
            self.maintenance.storage.insert(target.id.clone(), StorageReports {
                connection: target.connection.clone(),
                last_run: LastRun::Loading,
                database: None,
                reclaim_requested: false,
                results: ListState::new(0, ListAlignment::Top, gpui::px(400.)),
                _streams: streams,
            });
        }
        if targets.iter().any(|target| target.connection.cleanup) && self.maintenance.ticker.is_none() {
            self.maintenance.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(30)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        } else if !targets.iter().any(|target| target.connection.cleanup) {
            self.maintenance.ticker = None;
        }
    }

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
        let storage_targets = self.storage_targets(cx);
        self.ensure_maintenance_streams(&storage_targets, cx);
        let Some(config) = self.scoped_representative_config(cx) else {
            return vec![Block::new(&[], muted_text("Reconnect to change this environment's settings.", cx))];
        };
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
            blocks.extend(self.storage_blocks(&storage_targets, &config, env_only, window, cx));
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
    fn storage_blocks(&mut self, targets: &[StorageTarget], config: &ServerConfig, unavailable: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let mut blocks = Vec::new();
        if let Some(notice) = self.database_notice(targets, cx) {
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
        let cleanup_targets: Vec<_> = targets.iter().filter(|target| target.connection.cleanup).collect();
        if !cleanup_targets.is_empty() {
            blocks.push(Block::new(&["storage-delete-now"], self.storage_cleanup_section(&cleanup_targets, targets.len(), cx)));
        }
        blocks
    }

    fn open_reclaim_dialog(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.storage_targets(cx).into_iter().find(|target| target.id == id && target.can_reclaim) else { return };
        if !self.maintenance.storage.get(id).is_some_and(|entry| {
            entry.connection == target.connection && !entry.reclaim_requested
                && entry.database.as_ref().is_some_and(|database| database.can_reclaim)
        }) {
            return;
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.maintenance.reclaim_confirming = Some((id.to_owned(), focus));
        cx.notify();
    }

    fn close_reclaim_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.maintenance.reclaim_confirming.take().is_some() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    fn start_database_reclaim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, _)) = self.maintenance.reclaim_confirming.take() else { return };
        self.focus.focus(window, cx);
        cx.notify();
        let Some(target) = self.storage_targets(cx).into_iter().find(|target| target.id == id && target.can_reclaim) else { return };
        let Some(reports) = self.maintenance.storage.get_mut(&id).filter(|reports| reports.connection == target.connection && !reports.reclaim_requested) else { return };
        let Some(storage) = reports.database.as_ref().filter(|storage| storage.can_reclaim) else { return };
        reports.reclaim_requested = true;
        let amount = storage.amount();
        let minutes = storage.reclaim_minutes();
        let task = self.storage_rpc(&target, ENVIRONMENT_MAINTAIN_SCOPE, model::RECLAIM_DATABASE_SPACE, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let environment = IcuArg::from(&target.label);
                let toast = match result {
                    Ok(_) => Toast::new(ToastKind::Success, t_fmt(cx, "Reclaiming {amount} on {environment}", &[
                        ("amount", IcuArg::from(&amount)), ("environment", environment),
                    ])).description(t_fmt(cx, "T3 Code there restarts and is back in about {minutes} min.", &[("minutes", IcuArg::Int(minutes as i64))])),
                    Err(error) => {
                        if let Some(reports) = this.maintenance.storage.get_mut(&id).filter(|entry| entry.connection == target.connection) {
                            reports.reclaim_requested = false;
                        }
                        Toast::new(ToastKind::Error, t_fmt(cx, "Could not reclaim space on {environment}", &[("environment", environment)]))
                            .description(failure_text(&error, t(cx, "An error occurred.")))
                    }
                };
                this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                cx.notify();
            });
        }).detach();
    }

    fn reclaim_dialog_overlay(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, focus) = self.maintenance.reclaim_confirming.clone()?;
        let storage = self.maintenance.storage.get(&id)?.database.as_ref()?;
        let target = self.storage_targets(cx).into_iter().find(|target| target.id == id && target.can_reclaim)?;
        let (title, description) = storage.reclaim_confirmation(&target.label);
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

    fn dismiss_database_notice(&self, id: &str, cx: &mut Context<Self>) {
        let dismissed = cx.global_mut::<DismissedDatabaseNotice>();
        dismissed.environments.insert(id.to_owned());
        let environments = dismissed.environments.clone();
        let previous = dismissed.save_task.take();
        let save = cx.background_executor().spawn(async move {
            // Preserve click order when several machine notices are dismissed in quick succession.
            if let Some(previous) = previous {
                previous.await;
            }
            t3_client::store::save_ui_state(DATABASE_NOTICE_DISMISSALS, &environments);
        });
        cx.global_mut::<DismissedDatabaseNotice>().save_task = Some(save);
        cx.notify();
    }

    /// `DatabaseStorageNotice` shows each machine's current recommendation.
    fn database_notice(&self, targets: &[StorageTarget], cx: &mut Context<Self>) -> Option<AnyElement> {
        let dismissed = cx.try_global::<DismissedDatabaseNotice>()?;
        if !dismissed.loaded {
            return None;
        }
        let mut rows = Vec::new();
        let mut all_reclaimable = true;
        for target in targets.iter().filter(|target| !dismissed.environments.contains(&target.id)) {
            let Some(reports) = self.maintenance.storage.get(&target.id) else { continue };
            let Some(storage) = &reports.database else { continue };
            let reclaimable = storage.can_reclaim && target.can_reclaim;
            all_reclaimable &= reclaimable;
            let restarting = reports.reclaim_requested && storage.reclaim_failure.is_none();
            let row_id = SharedString::from(format!("storage-database-machine-{}", target.id));
            let id = target.id.clone();
            let dismiss = Button::new((gpui::ElementId::from(row_id.clone()), "dismiss"), ButtonVariant::Ghost, ButtonSize::Xs)
                .label(t(cx, "Dismiss"))
                .on_click(cx.listener(move |this, _, _, cx| this.dismiss_database_notice(&id, cx)));
            let row = Row::new(row_id.clone(), storage.title(&target.label));
            let control = |name, variant| Button::new((gpui::ElementId::from(row_id.clone()), name), variant, ButtonSize::Xs);
            let machine_row = if reclaimable {
                let id = target.id.clone();
                row.description(storage.reclaim_description()).control(
                    h_flex().gap(r(6.)).child(control("reclaim", ButtonVariant::Outline)
                        .label(t(cx, if restarting { "Restarting…" } else { "Reclaim" }))
                        .disabled(restarting)
                        .on_click(cx.listener(move |this, _, window, cx| this.open_reclaim_dialog(&id, window, cx))))
                        .child(dismiss),
                )
            } else {
                let command = storage.compact_command.clone();
                row.description_content(div().font_family(cx.theme().mono_font_family.clone()).child(command.clone()))
                    .control(h_flex().gap(r(6.)).child(control("copy", ButtonVariant::Outline)
                        .label(t(cx, "Copy command"))
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(command.clone()))))
                        .child(dismiss))
            };
            rows.push(machine_row.into_any_element());
        }
        if rows.is_empty() {
            return None;
        }
        rows.insert(0, Row::new("storage-database-intro", t(cx, "Reclaim freed database space"))
            .description(t(cx, model::database_description(all_reclaimable)))
            .into_any_element());
        Some(section(t(cx, "Database"), rows, cx).into_any_element())
    }

    /// `CleanupSection`: Delete now with the last run, and the last run's results folded below.
    fn storage_cleanup_section(&self, targets: &[&StorageTarget], machine_count: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let has_rules = targets.iter().any(|target| target.has_rules);
        let allowed = targets.iter().any(|target| target.can_clean);
        let pending = self.maintenance.cleanup_pending;
        let now = now_ms();
        let description = if has_rules {
            v_flex().children(targets.iter().map(|target| {
                let run = self.maintenance.storage.get(&target.id).map_or(&LastRun::Loading, |entry| &entry.last_run);
                let text = cleanup_description(run, now, cx);
                div().child(if targets.len() > 1 { format!("{}: {text}", target.label) } else { text })
            })).into_any_element()
        } else {
            div().child(t(cx, model::NO_RULES)).into_any_element()
        };
        let delete_now = Row::new("storage-delete-now", t(cx, "Delete now")).description_content(description)
            .highlighted(self.is_target("storage-delete-now")).control(
                Button::new("storage-delete-now-button", ButtonVariant::Outline, ButtonSize::Sm)
                    .label(t(cx, if pending { "Deleting…" } else { "Delete now" }))
                    .disabled(pending || !has_rules || !allowed)
                    .on_click(cx.listener(|this, _, _, cx| this.run_storage_cleanup(cx))),
            );
        let mut content = v_flex().gap(r(10.)).child(section_title(t(cx, "Cleanup"))).child(group(vec![delete_now.into_any_element()], cx));
        let reports: Vec<_> = targets.iter().filter_map(|target| {
            let entry = self.maintenance.storage.get(&target.id)?;
            Some((*target, entry.last_run.report()?, &entry.results))
        }).collect();
        if let Some((_, first, _)) = reports.first() {
            let open = self.maintenance.last_run_open;
            let counts = cleanup_counts(first, cx);
            let summary = if machine_count <= 1 {
                counts
            } else if reports.len() == machine_count && reports.iter().all(|(_, report, _)| (report.removed, report.kept, report.failed) == (first.removed, first.kept, first.failed)) {
                t_fmt(cx, "{counts} on every machine", &[("counts", IcuArg::from(&counts))])
            } else {
                reports.iter().map(|(target, report, _)| format!("{}: {}", target.label, cleanup_counts(report, cx))).collect::<Vec<_>>().join(" · ")
            };
            let header = h_flex()
                .id("storage-cleanup-last-run")
                .min_h(r(44.))
                .px(r(16.))
                .py(r(8.))
                .gap(r(8.))
                .cursor_pointer()
                .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 16., hsla(p.muted_foreground)))
                .child(div().flex_none().text_sm().font_weight(FontWeight::MEDIUM).child(t(cx, "Last run")))
                .child(div().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(summary))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.maintenance.last_run_open = !this.maintenance.last_run_open;
                    cx.notify();
                }));
            let folded = v_flex().child(header).when(open, |this| this.children(reports.into_iter().map(|(target, report, list_state)| {
                div().id(SharedString::from(format!("storage-cleanup-results-{}", target.id)))
                    .border_t_1().border_color(hsla(p.border).opacity(0.5))
                    .child(self.cleanup_results(report, &target.label, list_state, cx))
            })));
            content = content.child(group(vec![folded.into_any_element()], cx));
        }
        content.into_any_element()
    }

    /// `CleanupResults`: one row per entry, then what was left out or that nothing needed it.
    fn cleanup_results(&self, report: &CleanupReport, label: &str, list_state: &ListState, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let note = |text: String| div().px(r(16.)).py(r(10.)).text_xs().text_color(hsla(p.muted_foreground)).child(text).into_any_element();
        let footer = if report.omitted_count > 0 {
            let text = t_fmt(cx, "{omitted, plural, one {{environment}: {count} more results.} other {{environment}: {count} more results.}}", &[
                ("environment", IcuArg::from(label)),
                ("omitted", IcuArg::Int(report.omitted_count as i64)),
                ("count", IcuArg::from(model::format_count(report.omitted_count as f64))),
            ]);
            Some(note(text))
        } else if report.entries.is_empty() {
            Some(note(t_fmt(cx, "{environmentLabel}: Nothing needed cleanup.", &[("environmentLabel", IcuArg::from(label))])))
        } else {
            None
        };
        let body = if report.entries.len() <= INLINE_RESULTS {
            v_flex().children(report.entries.iter().enumerate().map(|(index, entry)| self.cleanup_result(index, entry, label, cx))).into_any_element()
        } else {
            let entries = report.entries.clone();
            if list_state.item_count() != entries.len() {
                list_state.reset(entries.len());
            }
            let label = label.to_owned();
            list(
                list_state.clone(),
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

    /// A manual cleanup runs on every permitted environment in the selected scope.
    fn run_storage_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.maintenance.cleanup_pending {
            return;
        }
        let targets = self.storage_targets(cx);
        if !targets.iter().any(|target| target.has_rules) {
            return;
        }
        let requests: Vec<_> = targets.into_iter().filter(|target| target.can_clean).map(|target| {
            let request = self.storage_rpc(&target, SETTINGS_WRITE_SCOPE, model::RUN_STORAGE_CLEANUP, cx);
            async move { (target, request.await) }
        }).collect();
        if requests.is_empty() {
            return;
        }
        self.maintenance.cleanup_pending = true;
        cx.spawn(async move |this, cx| {
            for (target, result) in futures::future::join_all(requests).await {
                let _ = this.update(cx, |this, cx| {
                    let toast = match result {
                        Ok(reply) => model::decode_report(&reply).and_then(|report| {
                            report.failures().next().map(|entry| {
                                Toast::new(ToastKind::Error, t_fmt(cx, "Cleanup failed on {label}", &[("label", IcuArg::from(&target.label))]))
                                    .description(entry.reason.clone())
                            })
                        }),
                        Err(error) => Some(Toast::new(ToastKind::Error, t_fmt(cx, "Could not run cleanup on {label}", &[("label", IcuArg::from(&target.label))]))
                            .description(failure_text(&error, t(cx, "Try again.")))),
                    };
                    if let Some(toast) = toast {
                        this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                    }
                });
            }
            let _ = this.update(cx, |this, cx| {
                this.maintenance.cleanup_pending = false;
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
}

fn cleanup_counts(report: &CleanupReport, cx: &App) -> String {
    let mut args = vec![
        ("removed", IcuArg::Int(report.removed as i64)),
        ("kept", IcuArg::Int(report.kept as i64)),
    ];
    let message = if report.failed > 0 {
        args.push(("failed", IcuArg::Int(report.failed as i64)));
        "{removed} removed, {kept} kept, {failed} failed"
    } else {
        "{removed} removed, {kept} kept"
    };
    t_fmt(cx, message, &args)
}

fn cleanup_description(run: &LastRun, now: i64, cx: &App) -> String {
    let report = match run {
        LastRun::Loading => return t(cx, "Loading last run…").to_owned(),
        LastRun::Failed => return t(cx, "Could not load the last run.").to_owned(),
        LastRun::None => return t(cx, "Cleanup hasn't run yet.").to_owned(),
        LastRun::Report(report) => report,
    };
    let mut args = vec![
        ("when", IcuArg::from(t3_core::settings::archive::relative_label(&report.finished_at, now))),
        ("bytes", IcuArg::from(model::format_bytes(report.bytes_freed))),
    ];
    let message = if report.failed > 0 {
        args.push(("count", IcuArg::Int(report.failed as i64)));
        "{count, plural, one {Last run {when}. {bytes} recovered. {count} item couldn't be removed.} other {Last run {when}. {bytes} recovered. {count} items couldn't be removed.}}"
    } else {
        "Last run {when}. {bytes} recovered."
    };
    t_fmt(cx, message, &args)
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
    use gpui::{AppContext, IntoElement, ParentElement, Render, Styled, div, px, size};
    use serde_json::json;
    use t3_core::settings::search::Section;

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().bg(hsla(palette(cx).background)).children(super::super::overlay(window, cx))
        }
    }

    #[gpui::test]
    async fn cleanup_reads_and_runs_on_both_selected_machines(cx: &mut gpui::TestAppContext) {
        use t3_client::environments::Environments;
        use t3_client::session::StreamHandle;
        use t3_client::supervisor::ClientFixture;
        use t3_core::server_config::ServerConfigState;
        use t3_core::shell::ShellState;

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _active_fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
        });
        let (connections, _events) = Environments::start(runtime.handle(), false);
        state.update(cx, |state, _| {
            state.stop_pump();
            state.connections = connections;
        });
        let grants: t3_core::auth::AuthSessionState = serde_json::from_value(json!({
            "authenticated": true,
            "permissions": ["orchestration:read", "settings:write"]
        })).unwrap();
        let mut fixtures = Vec::new();
        for (id, label, logs) in [("saved-fixture", "Studio Mac", None), ("saved-linux", "Linux Box", Some(14))] {
            let (client, fixture) = ClientFixture::handle(grants.clone());
            let mut shell = ShellState::default();
            shell.synchronized = true;
            let mut config = ServerConfigState::default();
            config.config = Some(serde_json::from_value(json!({
                "environment": {
                    "environmentId": id,
                    "label": label,
                    "capabilities": {"storageCleanup": true, "storageCleanupRun": true}
                },
                "settings": {"storageCleanup": {"logsAfterDays": logs}}
            })).unwrap());
            let install = state.update(cx, |state, _| {
                state.connections.install_fixture(id, client, shell, config, ConnectionStatus::Connected)
            });
            fixtures.push((fixture, install.await));
        }
        state.update(cx, |state, cx| {
            let fleet = state.connections.view().borrow().clone();
            state.apply_fleet_view(fleet, cx);
        });
        let (view, vcx) = cx.add_window_view(|window, cx| SettingsView::new(state.clone(), Section::Maintenance, None, window, cx));
        view.update(vcx, |view, cx| {
            let targets = view.storage_targets(cx);
            assert_eq!(targets.len(), 2);
            assert_eq!(targets.iter().filter(|target| target.has_rules).count(), 1);
            view.ensure_maintenance_streams(&targets, cx);
        });
        let mut reports = Vec::new();
        for (removed, (fixture, _)) in fixtures.iter_mut().enumerate() {
            let (tag, reply) = fixture.next_stream().await;
            assert_eq!(tag, model::GET_STORAGE_CLEANUP_REPORT);
            let (handle, sender) = StreamHandle::fixture();
            reply.send(Ok(handle)).ok().unwrap();
            let report = json!({
                "finishedAt": "2026-09-03T12:00:00.000Z",
                "counts": {"removed": removed + 1, "kept": 0, "failed": 0},
                "entries": []
            });
            sender.send(Ok(report.clone())).await.unwrap();
            reports.push((sender, report));
        }
        vcx.run_until_parked();
        view.read_with(vcx, |view, _| {
            let removed = |id: &str| view.maintenance.storage[id].last_run.report().unwrap().removed;
            assert_eq!(removed("saved-fixture"), 1);
            assert_eq!(removed("saved-linux"), 2);
        });
        view.update(vcx, |view, cx| view.run_storage_cleanup(cx));
        for ((fixture, _), (_, report)) in fixtures.iter_mut().zip(&reports) {
            let (tag, input, reply) = fixture.next_request().await;
            assert_eq!(tag, model::RUN_STORAGE_CLEANUP);
            assert_eq!(input, json!({}));
            reply.send(Ok(report.clone())).unwrap();
        }
        vcx.run_until_parked();
        view.read_with(vcx, |view, _| assert!(!view.maintenance.cleanup_pending));
    }

    #[test]
    fn reclaim_keep_awake_and_deferred_retention_parity() {
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
            assert!(view.maintenance.storage.values().any(|entry| entry.database.as_ref().is_some_and(|db| db.can_reclaim)));
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
            view.update(cx, |view, cx| view.open_reclaim_dialog("fixture-env", window, cx));
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
        view.read_with(&cx, |view, _| assert!(view.maintenance.storage["fixture-env"].reclaim_requested));

        fixture.grants.send(Some(crate::context_menu::project_filter_tests::session(false))).unwrap();
        drive(&mut cx);
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| view.open_reclaim_dialog("fixture-env", window, cx));
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
