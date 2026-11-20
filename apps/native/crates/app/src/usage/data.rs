//! What the page reads and how it asks: every connected environment's summary for the selected
//! window (`web/src/state/usage.ts` `useUsage`), kept per window per environment while the page is
//! open, the last answer shown dimmed while another window loads, explicit refresh (`refreshUsage`),
//! and the Limits refresh through `server.refreshProviders` (`refreshUsageLimits`, at most every
//! five minutes per environment on its own). Requests run and decode on the network runtime.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use futures::future::join_all;
use gpui::{App, Context, Entity, Task};
use serde_json::json;
use t3_client::supervisor::{ClientHandle, ConnectionStatus};
use t3_core::auth::AuthSessionState;
use t3_core::server_config::ServerConfig;
use t3_core::usage::format::make_window;
use t3_core::usage::limits::{self, ExternalUsageLink, LimitEnvironment, LimitPool, Redeem};
use t3_core::usage::merge::{AnsweredUsage, MergedUsage, merge_usage};
use t3_core::usage::{
    self, ConnectionPhase, EnvironmentUsage, PREFERENCES_STORE, USAGE_CONTRACT_VERSION, UsageAccess, UsageMetric, UsageModelFilter,
    UsagePagePreferences, UsageProvider, UsageSummary, UsageSummaryInput, decode_summary, method, resolve_usage_access,
};

use super::{Host, UsageView};
use crate::app_state::{AppState, Phase};
use crate::sidebar::controller::{SystemZone, now_ms};

const CACHE_LIMIT: usize = 8;
const STALE_MS: i64 = 60_000;
const LIMITS_REFRESH_MS: i64 = 5 * 60_000;
const FALLBACK_ACTIVE_ID: &str = "__active__";

/// The last answer, shown dimmed while another window loads.
#[derive(Clone)]
pub(super) struct Kept {
    pub window: UsageSummaryInput,
    pub merged: Arc<MergedUsage>,
    selected_ids: Option<BTreeSet<String>>,
    hidden: BTreeSet<UsageProvider>,
}

pub(super) struct LimitsData {
    pub pools: Vec<LimitPool>,
    pub notices: Vec<String>,
    pub links: Vec<ExternalUsageLink>,
}

pub(super) struct EnvUsage {
    pub saved_id: String,
    pub environment_id: String,
    pub label: String,
    pub phase: ConnectionPhase,
    pub session: Option<AuthSessionState>,
    pub access: UsageAccess,
    pub summary: Option<Arc<UsageSummary>>,
    pub pending: bool,
    pub error: Option<String>,
    pub model_filter: Option<UsageModelFilter>,
    pub config: Option<ServerConfig>,
    cache: Vec<(UsageSummaryInput, Arc<UsageSummary>, i64)>,
    load_gen: u64,
    load_task: Option<Task<()>>,
}

impl EnvUsage {
    fn new(saved_id: String, environment_id: String, label: String, phase: ConnectionPhase, session: Option<AuthSessionState>, config: Option<ServerConfig>) -> Self {
        let access = resolve_usage_access(phase, session.as_ref(), false);
        let model_filter = config.as_ref().and_then(|c| c.settings.usage_model_filter.clone());
        Self {
            saved_id,
            environment_id,
            label,
            phase,
            session,
            access,
            summary: None,
            pending: false,
            error: None,
            model_filter,
            config,
            cache: Vec::new(),
            load_gen: 0,
            load_task: None,
        }
    }

    fn cached(&self, window: &UsageSummaryInput, now: i64) -> Option<Arc<UsageSummary>> {
        self.cache.iter().find(|(key, _, at)| key.same_window(window) && now - at < STALE_MS).map(|(_, summary, _)| summary.clone())
    }

    fn remember(&mut self, window: &UsageSummaryInput, summary: Arc<UsageSummary>) {
        self.cache.retain(|(key, _, _)| !key.same_window(window));
        self.cache.push((window.clone(), summary, now_ms()));
        if self.cache.len() > CACHE_LIMIT {
            self.cache.remove(0);
        }
    }

    pub fn to_environment_usage(&self) -> EnvironmentUsage {
        EnvironmentUsage {
            environment_id: self.environment_id.clone(),
            label: self.label.clone(),
            is_connected: self.phase == ConnectionPhase::Connected,
            is_pending: self.pending,
            can_read_diagnostics: self.access.can_read_diagnostics,
            error: self.error.clone().or_else(|| self.access.error.map(str::to_owned)),
            summary: self.summary.clone(),
        }
    }

    pub fn can_manage_providers(&self) -> bool {
        self.session.as_ref().is_some_and(|session| session.grants(usage::PROVIDERS_MANAGE_SCOPE))
    }
}

type MergedKey = (Vec<(String, usize)>, BTreeSet<UsageProvider>, Option<BTreeSet<String>>);

pub(super) struct UsageData {
    pub preferences: UsagePagePreferences,
    pub window: UsageSummaryInput,
    pub envs: Vec<EnvUsage>,
    /// `None` means "All environments" (automatically tracks newly connected environments).
    pub selected_ids: Option<BTreeSet<String>>,
    pub refreshing: bool,
    pub active_session: Option<AuthSessionState>,
    pub limits_now: i64,
    /// `resolveHour12`: the clock the timestamp setting picks; the system default reads 12-hour.
    pub hour12: bool,
    pub merged: Arc<MergedUsage>,
    pub kept: Option<Kept>,
    limits: Option<Arc<LimitsData>>,
    merged_for: Option<MergedKey>,
    time_zone: String,
    refresh_task: Option<Task<()>>,
    action: Option<Task<()>>,
}

fn hour12() -> bool {
    let doc = t3_core::client_settings::ClientSettingsDoc::from_json(&t3_client::store::load_client_settings().unwrap_or_default());
    doc.timestamp_format() != t3_core::client_settings::TimestampFormat::TwentyFourHour
}

fn system_time_zone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_owned())
}

fn phase_of(phase: &Phase) -> ConnectionPhase {
    match phase {
        Phase::Online(status) => connection_phase_of(status),
        Phase::Unpaired => ConnectionPhase::Disconnected,
    }
}

fn connection_phase_of(status: &ConnectionStatus) -> ConnectionPhase {
    match status {
        ConnectionStatus::Connected => ConnectionPhase::Connected,
        ConnectionStatus::Connecting => ConnectionPhase::Connecting,
        ConnectionStatus::Reconnecting { .. } => ConnectionPhase::Reconnecting,
        _ => ConnectionPhase::Disconnected,
    }
}

impl UsageData {
    pub fn new() -> Self {
        let preferences = UsagePagePreferences::from_json(t3_client::store::load_ui_state(PREFERENCES_STORE).as_ref());
        let time_zone = system_time_zone();
        let period = preferences.period;
        UsageData {
            window: make_window(period.days(), now_ms(), period.resolution(), &time_zone, &SystemZone),
            preferences,
            envs: Vec::new(),
            selected_ids: None,
            refreshing: false,
            active_session: None,
            limits_now: now_ms(),
            hour12: hour12(),
            merged: Arc::new(MergedUsage::default()),
            kept: None,
            limits: None,
            merged_for: None,
            time_zone,
            refresh_task: None,
            action: None,
        }
    }

    pub fn save_preferences(&self) {
        t3_client::store::save_ui_state(PREFERENCES_STORE, &self.preferences.to_json());
    }

    pub fn set_window(&mut self, now: i64) {
        let period = self.preferences.period;
        self.window = make_window(period.days(), now, period.resolution(), &self.time_zone, &SystemZone);
        for env in &mut self.envs {
            env.summary = env.cached(&self.window, now);
            env.error = None;
            env.pending = false;
            env.load_task = None;
        }
        self.merged_for = None;
    }

    pub fn hidden(&self) -> &BTreeSet<UsageProvider> {
        &self.preferences.hidden_providers
    }

    pub fn config_changed(&mut self) {
        self.limits = None;
    }

    pub fn invalidate_limits(&mut self) {
        self.limits = None;
    }

    pub fn is_env_selected(&self, saved_id: &str) -> bool {
        self.selected_ids.as_ref().is_none_or(|ids| ids.contains(saved_id))
    }

    pub fn selected_envs(&self) -> Vec<&EnvUsage> {
        self.envs.iter().filter(|env| self.is_env_selected(&env.saved_id)).collect()
    }

    pub fn selected_environment_usages(&self) -> Vec<EnvironmentUsage> {
        self.selected_envs().into_iter().map(EnvUsage::to_environment_usage).collect()
    }

    pub fn any_can_read_diagnostics(&self) -> bool {
        self.selected_envs().iter().any(|env| env.access.can_read_diagnostics)
    }

    pub fn any_connected(&self) -> bool {
        self.envs.iter().any(|env| env.phase == ConnectionPhase::Connected)
    }

    pub fn answered_count(&self) -> usize {
        self.selected_envs().iter().filter(|env| env.summary.is_some()).count()
    }

    pub fn still_reporting_count(&self) -> usize {
        self.selected_envs()
            .iter()
            .filter(|env| env.summary.is_none() && env.error.is_none() && env.access.error.is_none())
            .count()
    }

    pub fn is_pending(&self) -> bool {
        self.answered_count() == 0 && self.still_reporting_count() > 0
    }

    pub fn is_partial(&self) -> bool {
        self.answered_count() > 0 && self.still_reporting_count() > 0
    }

    pub fn is_settled(&self) -> bool {
        let selected = self.selected_envs();
        if selected.is_empty() {
            return true;
        }
        selected.iter().all(|env| !env.pending && (env.summary.is_some() || env.error.is_some() || !env.access.can_read_diagnostics))
    }

    pub fn all_selected(&self) -> bool {
        self.selected_ids.is_none()
    }

    pub fn environment_filter_label(&self, cx: &gpui::App) -> String {
        if self.all_selected() {
            return crate::i18n::t(cx, "All environments").to_owned();
        }
        let selected = self.selected_envs();
        if selected.len() == 1 {
            selected[0].label.clone()
        } else {
            crate::i18n::t_fmt(
                cx,
                "{count, plural, one {{count} environments} other {{count} environments}}",
                &[("count", selected.len().into())],
            )
        }
    }

    /// Synchronizes `self.envs` from `state.fleet` (or the active single connection when the fleet
    /// is empty). Returns `saved_id`s that are connected + readable and have no summary/load yet.
    pub fn sync_environments(&mut self, state: &AppState) -> Vec<String> {
        let mut previous: HashMap<String, EnvUsage> = self.envs.drain(..).map(|env| (env.saved_id.clone(), env)).collect();
        let mut next: Vec<EnvUsage> = Vec::new();
        let mut need_load = Vec::new();

        if !state.fleet.environments.is_empty() {
            let mut ordered: Vec<&t3_client::environments::EnvironmentView> = Vec::new();
            for saved in &state.environments.environments {
                if let Some(view) = state.fleet.environment(&saved.id) {
                    ordered.push(view);
                }
            }
            for view in &state.fleet.environments {
                if !ordered.iter().any(|existing| existing.summary.owner.saved_id == view.summary.owner.saved_id) {
                    ordered.push(view.as_ref());
                }
            }
            for view in ordered {
                let saved_id = view.summary.owner.saved_id.clone();
                let config = view.config.config.clone();
                let environment_id = config
                    .as_ref()
                    .map(|c| c.environment.environment_id.clone())
                    .filter(|id| !id.is_empty())
                    .unwrap_or_else(|| saved_id.clone());
                let saved_label = state.environments.get(&saved_id).map(|e| e.label.trim()).filter(|l| !l.is_empty());
                let config_label = config.as_ref().map(|c| c.environment.label.trim()).filter(|l| !l.is_empty());
                let label = saved_label.or(config_label).unwrap_or("This environment").to_owned();
                let phase = connection_phase_of(&view.status);
                let is_active = state.environments.active.as_deref() == Some(saved_id.as_str());
                let session = view.grants.clone().or_else(|| is_active.then(|| self.active_session.clone()).flatten());
                let entry = Self::merge_env_entry(previous.remove(&saved_id), saved_id, environment_id, label, phase, session, config);
                if entry.access.can_read_diagnostics && entry.phase == ConnectionPhase::Connected && entry.summary.is_none() && !entry.pending && entry.error.is_none() {
                    need_load.push(entry.saved_id.clone());
                }
                next.push(entry);
            }
        } else if state.client_handle().is_some() || state.server_config.config.is_some() {
            let saved_id = state.environments.active.clone().unwrap_or_else(|| FALLBACK_ACTIVE_ID.to_owned());
            let config = state.server_config.config.clone();
            let environment_id = config
                .as_ref()
                .map(|c| c.environment.environment_id.clone())
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| saved_id.clone());
            let saved_label = state.environments.active().map(|e| e.label.trim()).filter(|l| !l.is_empty());
            let config_label = config.as_ref().map(|c| c.environment.label.trim()).filter(|l| !l.is_empty());
            let label = saved_label.or(config_label).unwrap_or("This environment").to_owned();
            let phase = phase_of(&state.phase);
            let session = self.active_session.clone();
            let entry = Self::merge_env_entry(previous.remove(&saved_id), saved_id, environment_id, label, phase, session, config);
            if entry.access.can_read_diagnostics && entry.phase == ConnectionPhase::Connected && entry.summary.is_none() && !entry.pending && entry.error.is_none() {
                need_load.push(entry.saved_id.clone());
            }
            next.push(entry);
        }

        if let Some(selected) = &mut self.selected_ids {
            selected.retain(|id| next.iter().any(|env| env.saved_id == *id));
        }
        self.envs = next;
        self.limits = None;
        need_load
    }

    fn merge_env_entry(
        existing: Option<EnvUsage>,
        saved_id: String,
        environment_id: String,
        label: String,
        phase: ConnectionPhase,
        session: Option<AuthSessionState>,
        config: Option<ServerConfig>,
    ) -> EnvUsage {
        match existing {
            Some(mut entry) => {
                let model_filter = config.as_ref().and_then(|c| c.settings.usage_model_filter.clone());
                if entry.model_filter != model_filter {
                    entry.cache.clear();
                    entry.summary = None;
                    entry.error = None;
                }
                if entry.phase != phase && phase == ConnectionPhase::Connected {
                    entry.error = None;
                }
                entry.environment_id = environment_id;
                entry.label = label;
                entry.phase = phase;
                entry.session = session;
                entry.access = resolve_usage_access(phase, entry.session.as_ref(), false);
                entry.model_filter = model_filter;
                entry.config = config;
                entry
            }
            None => EnvUsage::new(saved_id, environment_id, label, phase, session, config),
        }
    }

    /// Brings derived state up to date before a render; cheap when nothing changed.
    pub fn prepare(&mut self, state: &Entity<AppState>, cx: &App) {
        let state = state.read(cx);
        // Update lightweight metadata (labels/configs) without starting tasks during render.
        let _ = self.sync_environments(state);
        let pointers: Vec<(String, usize)> = self
            .selected_envs()
            .into_iter()
            .map(|env| (env.environment_id.clone(), env.summary.as_ref().map_or(0, |s| Arc::as_ptr(s) as usize)))
            .collect();
        let key = (pointers, self.hidden().clone(), self.selected_ids.clone());
        if self.merged_for.as_ref() != Some(&key) {
            let answered: Vec<AnsweredUsage> = self
                .selected_envs()
                .into_iter()
                .filter_map(|env| {
                    let summary = env.summary.as_ref()?;
                    Some(AnsweredUsage {
                        environment_id: env.environment_id.clone(),
                        label: env.label.clone(),
                        summary: Arc::new(summary.without_providers(self.hidden())),
                    })
                })
                .collect();
            self.merged = Arc::new(merge_usage(&answered, USAGE_CONTRACT_VERSION));
            if !answered.is_empty() {
                self.kept = Some(Kept {
                    window: self.window.clone(),
                    merged: self.merged.clone(),
                    selected_ids: self.selected_ids.clone(),
                    hidden: self.hidden().clone(),
                });
            }
            self.merged_for = Some(key);
        }
    }

    /// What the page shows: answered summary, the last one dimmed, or skeleton (`None`).
    pub fn shown(&self) -> Option<(UsageSummaryInput, Arc<MergedUsage>, bool)> {
        if self.answered_count() > 0 {
            return Some((self.window.clone(), self.merged.clone(), false));
        }
        if let Some(kept) = self
            .kept
            .as_ref()
            .filter(|kept| kept.selected_ids == self.selected_ids && kept.hidden == *self.hidden())
        {
            return Some((kept.window.clone(), kept.merged.clone(), true));
        }
        if self.is_pending() {
            return None;
        }
        Some((self.window.clone(), self.merged.clone(), false))
    }

    pub fn limits(&mut self, _state: &AppState) -> Arc<LimitsData> {
        if let Some(limits) = &self.limits {
            return limits.clone();
        }
        let hidden: Vec<&str> = self.hidden().iter().map(|provider| provider.driver()).collect();
        let owned: Vec<(String, String, Vec<t3_core::server_config::ServerProvider>, Vec<t3_core::server_config::UsageLimitSource>)> = self
            .selected_envs()
            .into_iter()
            .filter_map(|env| {
                let config = env.config.as_ref()?;
                let (providers, sources) = limits::hide_drivers(&config.providers, &config.usage_limit_sources, &hidden);
                Some((env.environment_id.clone(), env.label.clone(), providers, sources))
            })
            .collect();
        let environments: Vec<LimitEnvironment<'_>> = owned
            .iter()
            .map(|(environment_id, label, providers, sources)| LimitEnvironment {
                environment_id,
                label,
                providers,
                sources,
            })
            .collect();
        let data = Arc::new(LimitsData {
            pools: limits::collect_limit_pools(limits::collect_limit_accounts(&environments), self.limits_now),
            notices: limits::collect_limit_notices(&environments),
            links: limits::collect_external_usage_links(&environments),
        });
        self.limits = Some(data.clone());
        data
    }

    pub fn any_chatgpt_sharing(&self) -> bool {
        !self.hidden().contains(&UsageProvider::Codex)
            && self
                .selected_envs()
                .into_iter()
                .any(|env| env.config.as_ref().is_some_and(|config| config.providers.iter().any(limits::uses_chatgpt_sharing)))
    }

    /// The primary target label for model price validation error messages.
    pub fn primary_label(&self) -> String {
        self.selected_envs()
            .first()
            .copied()
            .or_else(|| self.envs.first())
            .map(|env| env.label.clone())
            .unwrap_or_else(|| "This environment".to_owned())
    }
}

pub(super) fn client_future(state: &AppState, saved_id: &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ClientHandle>> + Send + 'static>> {
    if saved_id == FALLBACK_ACTIVE_ID || state.fleet.environments.is_empty() || state.environments.active.as_deref() == Some(saved_id) {
        let handle = state.client_handle();
        Box::pin(async move { handle })
    } else {
        let lookup = state.connections.client(saved_id);
        Box::pin(lookup)
    }
}

async fn request_summary(
    runtime: &tokio::runtime::Handle,
    client: &ClientHandle,
    payload: serde_json::Value,
) -> Result<UsageSummary, String> {
    let client = client.clone();
    runtime
        .spawn(async move {
            let reply = client.request(method::GET_USAGE_SUMMARY, payload).await.map_err(|e| e.to_string())?;
            decode_summary(reply)
        })
        .await
        .unwrap_or_else(|_| Err("closed".to_owned()))
}

impl UsageView {
    /// Re-derives access and environment list from `AppState`, loading any newly readable environments.
    pub(super) fn sync_access(&mut self, cx: &mut Context<Self>) {
        let need_load = self.data.sync_environments(self.state.read(cx));
        for saved_id in need_load {
            self.load_env(saved_id, false, cx);
        }
    }

    /// Loads the current window for every readable environment (or refreshes selected ones).
    pub(super) fn load(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let targets: Vec<String> = if refresh {
            self.data
                .selected_envs()
                .into_iter()
                .filter(|env| env.access.can_read_diagnostics && env.phase == ConnectionPhase::Connected)
                .map(|env| env.saved_id.clone())
                .collect()
        } else {
            self.data
                .envs
                .iter()
                .filter(|env| env.access.can_read_diagnostics && env.phase == ConnectionPhase::Connected)
                .map(|env| env.saved_id.clone())
                .collect()
        };
        if targets.is_empty() {
            if refresh {
                self.set_refreshing(false, cx);
            }
            return;
        }
        if !refresh {
            for saved_id in targets {
                self.load_env(saved_id, false, cx);
            }
            return;
        }

        // Explicit refresh: fan out `refreshUsageRates` + fresh `getUsageSummary` across all
        // selected readable environments in parallel and stop spinning once every one settles.
        self.set_refreshing(true, cx);
        let window = self.data.window.clone();
        let runtime = self.state.read(cx).runtime().clone();
        let mut jobs = Vec::with_capacity(targets.len());
        for saved_id in targets {
            let Some(env) = self.data.envs.iter_mut().find(|e| e.saved_id == saved_id) else { continue };
            env.load_gen += 1;
            let load_gen = env.load_gen;
            env.pending = true;
            env.error = None;
            env.load_task = None;
            let input = window.with_model_filter(env.model_filter.clone());
            let client_fut = client_future(self.state.read(cx), &saved_id);
            jobs.push((saved_id, load_gen, input, client_fut));
        }
        self.data.refresh_task = Some(cx.spawn(async move |this, cx| {
            let mut tasks = Vec::with_capacity(jobs.len());
            for (saved_id, load_gen, input, client_fut) in jobs {
                let runtime = runtime.clone();
                let window = window.clone();
                let this = this.clone();
                let cx = cx.clone();
                tasks.push(async move {
                    let Some(client) = client_fut.await else {
                        let _ = this.update(&mut cx.clone(), |view, cx| {
                            view.apply_env(&saved_id, load_gen, &window, Err("not connected".to_owned()), false, cx);
                        });
                        return;
                    };
                    let rates_client = client.clone();
                    let _ = runtime.spawn(async move { rates_client.request(method::REFRESH_USAGE_RATES, json!({})).await }).await;
                    let plain = request_summary(&runtime, &client, input.payload()).await;
                    let settling = matches!(&plain, Ok(summary) if summary.is_refreshing());
                    if this
                        .update(&mut cx.clone(), |view, cx| view.apply_env(&saved_id, load_gen, &window, plain, settling, cx))
                        .is_err()
                        || !settling
                    {
                        return;
                    }
                    let awaited = request_summary(&runtime, &client, input.awaiting_refresh().payload()).await;
                    let _ = this.update(&mut cx.clone(), |view, cx| match awaited {
                        Ok(summary) => view.apply_env(&saved_id, load_gen, &window, Ok(summary), false, cx),
                        Err(error) => {
                            tracing::warn!(%error, %saved_id, "could not refresh slow usage sources");
                            view.settle_env_on_slow_error(&saved_id, load_gen, &window, cx);
                        }
                    });
                });
            }
            join_all(tasks).await;
            let _ = this.update(cx, |view, cx| {
                view.set_refreshing(false, cx);
            });
        }));
    }

    /// Loads one environment for the current window (provisional -> plain -> awaitRefresh).
    pub(super) fn load_env(&mut self, saved_id: String, refresh: bool, cx: &mut Context<Self>) {
        let now = now_ms();
        let window = self.data.window.clone();
        let Some(env) = self.data.envs.iter_mut().find(|e| e.saved_id == saved_id) else { return };
        if !env.access.can_read_diagnostics || env.phase != ConnectionPhase::Connected {
            return;
        }
        if !refresh && let Some(summary) = env.cached(&window, now) {
            env.summary = Some(summary);
            env.pending = false;
            env.error = None;
            return;
        }
        let has_previous = env.summary.is_some();
        let input = window.with_model_filter(env.model_filter.clone());
        env.load_gen += 1;
        let load_gen = env.load_gen;
        env.pending = true;
        env.error = None;
        let state = self.state.read(cx);
        let runtime = state.runtime().clone();
        let client_fut = client_future(state, &saved_id);
        let saved_for_task = saved_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let Some(client) = client_fut.await else {
                let _ = this.update(cx, |view, cx| {
                    view.apply_env(&saved_for_task, load_gen, &window, Err("not connected".to_owned()), false, cx);
                });
                return;
            };
            if refresh {
                let rates_client = client.clone();
                let _ = runtime.spawn(async move { rates_client.request(method::REFRESH_USAGE_RATES, json!({})).await }).await;
            }
            if !refresh
                && !has_previous
                && let Ok(saved) = request_summary(&runtime, &client, input.provisional_payload()).await
            {
                let settling = saved.is_refreshing();
                if this.update(cx, |view, cx| view.apply_env(&saved_for_task, load_gen, &window, Ok(saved), settling, cx)).is_err() || !settling {
                    return;
                }
            }
            let plain = request_summary(&runtime, &client, input.payload()).await;
            let settling = matches!(&plain, Ok(summary) if summary.is_refreshing());
            if this.update(cx, |view, cx| view.apply_env(&saved_for_task, load_gen, &window, plain, settling, cx)).is_err() || !settling {
                return;
            }
            let awaited = request_summary(&runtime, &client, input.awaiting_refresh().payload()).await;
            let _ = this.update(cx, |view, cx| match awaited {
                Ok(summary) => view.apply_env(&saved_for_task, load_gen, &window, Ok(summary), false, cx),
                Err(error) => {
                    tracing::warn!(%error, saved_id = %saved_for_task, "could not refresh slow usage sources");
                    view.settle_env_on_slow_error(&saved_for_task, load_gen, &window, cx);
                }
            });
        });
        if let Some(env) = self.data.envs.iter_mut().find(|e| e.saved_id == saved_id) {
            env.load_task = Some(task);
        }
    }

    fn apply_env(
        &mut self,
        saved_id: &str,
        load_gen: u64,
        window: &UsageSummaryInput,
        reply: Result<UsageSummary, String>,
        settling: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.data.window.same_window(window) {
            return;
        }
        let Some(env) = self.data.envs.iter_mut().find(|e| e.saved_id == saved_id && e.load_gen == load_gen) else {
            return;
        };
        match reply {
            Ok(summary) => {
                let summary = Arc::new(summary);
                if !settling {
                    env.remember(window, summary.clone());
                }
                env.summary = Some(summary);
                env.error = None;
            }
            Err(error) => {
                tracing::warn!(%error, %saved_id, "could not read usage");
                env.summary = None;
                env.error = Some("This environment could not report usage.".to_owned());
            }
        }
        env.pending = settling;
        self.data.merged_for = None;
        cx.notify();
    }

    fn settle_env_on_slow_error(&mut self, saved_id: &str, load_gen: u64, window: &UsageSummaryInput, cx: &mut Context<Self>) {
        if !self.data.window.same_window(window) {
            return;
        }
        let Some(env) = self.data.envs.iter_mut().find(|e| e.saved_id == saved_id && e.load_gen == load_gen) else {
            return;
        };
        if let Some(summary) = env.summary.clone() {
            env.remember(window, summary);
        }
        env.pending = false;
        cx.notify();
    }

    /// The header's Refresh: provider limits on Limits, else a fresh window and new prices.
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.data.refreshing {
            return;
        }
        if self.data.preferences.metric == UsageMetric::Limits {
            self.refresh_limits(false, cx);
            return;
        }
        self.data.set_window(now_ms());
        self.load(true, cx);
        cx.notify();
    }

    pub(super) fn auto_refresh_limits(&mut self, cx: &mut Context<Self>) {
        if self.data.preferences.metric == UsageMetric::Limits {
            self.refresh_limits(true, cx);
        }
    }

    fn refresh_limits(&mut self, automatic: bool, cx: &mut Context<Self>) {
        if cx.global::<Host>().read_only {
            return;
        }
        let now = now_ms();
        let targets: Vec<String> = {
            let cooldowns = &cx.global::<Host>().limits_refresh_after;
            self.data
                .selected_envs()
                .into_iter()
                .filter(|env| {
                    env.phase == ConnectionPhase::Connected
                        && env.config.is_some()
                        && (!automatic || now >= cooldowns.get(&env.saved_id).copied().unwrap_or(0))
                })
                .map(|env| env.saved_id.clone())
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        for id in &targets {
            cx.global_mut::<Host>().limits_refresh_after.insert(id.clone(), i64::MAX);
        }
        let runtime = self.state.read(cx).runtime().clone();
        let jobs: Vec<_> = targets
            .iter()
            .map(|saved_id| (saved_id.clone(), client_future(self.state.read(cx), saved_id)))
            .collect();
        if !automatic {
            self.set_refreshing(true, cx);
        }
        self.data.action = Some(cx.spawn(async move |this, cx| {
            let tasks = jobs.into_iter().map(|(saved_id, client_fut)| {
                let runtime = runtime.clone();
                async move {
                    if let Some(client) = client_fut.await {
                        let _ = runtime.spawn(async move { client.request(method::REFRESH_PROVIDERS, usage::refresh_providers_payload()).await }).await;
                    }
                    saved_id
                }
            });
            let finished = join_all(tasks).await;
            let _ = this.update(cx, |view, cx| {
                let next_after = now_ms() + LIMITS_REFRESH_MS;
                let cooldowns = &mut cx.global_mut::<Host>().limits_refresh_after;
                for id in finished {
                    cooldowns.insert(id, next_after);
                }
                if !automatic {
                    view.set_refreshing(false, cx);
                }
                view.data.limits_now = now_ms();
                view.data.invalidate_limits();
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Spends one reset credit (`provider.consumeResetCredit`) on the environment that owns it.
    pub(super) fn redeem_reset(&mut self, redeem: Redeem, account: String, cx: &mut Context<Self>) {
        if !self.can_manage_providers_for(&redeem.environment_id) {
            return;
        }
        let saved_id = self
            .data
            .envs
            .iter()
            .find(|env| env.environment_id == redeem.environment_id || env.saved_id == redeem.environment_id)
            .map(|env| env.saved_id.clone());
        let Some(saved_id) = saved_id else { return };
        let runtime = self.state.read(cx).runtime().clone();
        let client_fut = client_future(self.state.read(cx), &saved_id);
        let payload = redeem.input.payload();
        self.reset_status = Some((account.clone(), None));
        self.data.action = Some(cx.spawn(async move |this, cx| {
            let reply = match client_fut.await {
                Some(client) => runtime
                    .spawn(async move { client.request(method::CONSUME_RESET_CREDIT, payload).await })
                    .await
                    .unwrap_or(Err(t3_client::session::RpcError::Closed)),
                None => Err(t3_client::session::RpcError::Closed),
            };
            let status = match reply {
                Ok(reply) => limits::reset_outcome_text(&reply),
                Err(error) => {
                    let message = error.to_string();
                    if message.is_empty() { "Could not use the reset credit.".to_owned() } else { message }
                }
            };
            let _ = this.update(cx, |view, cx| {
                view.reset_status = Some((account, Some(status)));
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn can_manage_providers_for(&self, environment_id: &str) -> bool {
        self.data
            .envs
            .iter()
            .find(|env| env.environment_id == environment_id || env.saved_id == environment_id)
            .is_some_and(EnvUsage::can_manage_providers)
    }
}
