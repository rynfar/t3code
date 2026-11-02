//! What the page reads and how it asks: the environment's summary for the selected window
//! (`web/src/state/usage.ts` `useUsage`), kept per window while the page is open, the answer
//! shown while a new window loads, the explicit refresh (`refreshUsage`), and the Limits
//! refresh through `server.refreshProviders` (`refreshUsageLimits`, at most every five minutes
//! on its own). Requests run and decode on the network runtime.

use std::collections::BTreeSet;
use std::sync::Arc;

use gpui::{App, Context, Entity, Task};
use serde_json::json;
use t3_client::supervisor::ConnectionStatus;
use t3_core::auth::AuthSessionState;
use t3_core::commands::method as commands_method;
use t3_core::usage::format::make_window;
use t3_core::usage::limits::{self, ExternalUsageLink, LimitEnvironment, LimitPool, Redeem};
use t3_core::usage::merge::{AnsweredUsage, MergedUsage, merge_usage};
use t3_core::usage::{
    self, ConnectionPhase, EnvironmentUsage, PREFERENCES_STORE, USAGE_CONTRACT_VERSION, UsageAccess, UsageMetric, UsagePagePreferences, UsageProvider,
    UsageSummary, UsageSummaryInput, decode_summary, method, resolve_usage_access,
};

use super::{Host, UsageView};
use crate::app_state::{AppState, Phase};
use crate::sidebar::controller::{SystemZone, now_ms};

const CACHE_LIMIT: usize = 8;
const STALE_MS: i64 = 60_000;
const LIMITS_REFRESH_MS: i64 = 5 * 60_000;

/// The last answer, shown dimmed while another window loads.
#[derive(Clone)]
pub(super) struct Kept {
    pub window: UsageSummaryInput,
    pub merged: Arc<MergedUsage>,
    hidden: BTreeSet<UsageProvider>,
}

pub(super) struct LimitsData {
    pub pools: Vec<LimitPool>,
    pub notices: Vec<String>,
    pub links: Vec<ExternalUsageLink>,
}

pub(super) struct UsageData {
    pub preferences: UsagePagePreferences,
    pub window: UsageSummaryInput,
    pub summary: Option<Arc<UsageSummary>>,
    pub pending: bool,
    pub error: Option<String>,
    pub refreshing: bool,
    pub selected: bool,
    pub session: Option<AuthSessionState>,
    pub access: UsageAccess,
    pub phase: ConnectionPhase,
    pub label: String,
    pub environment_id: String,
    pub limits_now: i64,
    pub keychain_pending: bool,
    /// `resolveHour12`: the clock the timestamp setting picks; the system default reads 12-hour.
    pub hour12: bool,
    pub merged: Arc<MergedUsage>,
    pub kept: Option<Kept>,
    limits: Option<Arc<LimitsData>>,
    merged_for: Option<(usize, BTreeSet<UsageProvider>, bool)>,
    cache: Vec<(UsageSummaryInput, Arc<UsageSummary>, i64)>,
    time_zone: String,
    load: Option<Task<()>>,
    action: Option<Task<()>>,
}

fn hour12() -> bool {
    let doc = t3_core::client_settings::ClientSettingsDoc::from_json(&t3_client::store::load_client_settings().unwrap_or_default());
    doc.timestamp_format() != t3_core::client_settings::TimestampFormat::TwentyFourHour
}

fn system_time_zone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_owned())
}

impl UsageData {
    pub fn new() -> Self {
        let preferences = UsagePagePreferences::from_json(t3_client::store::load_ui_state(PREFERENCES_STORE).as_ref());
        let time_zone = system_time_zone();
        let period = preferences.period;
        UsageData {
            window: make_window(period.days(), now_ms(), period.resolution(), &time_zone, &SystemZone),
            preferences,
            summary: None,
            pending: false,
            error: None,
            refreshing: false,
            selected: true,
            session: None,
            access: UsageAccess { can_read_diagnostics: false, is_pending: true, error: None },
            phase: ConnectionPhase::Connecting,
            label: String::new(),
            environment_id: String::new(),
            limits_now: now_ms(),
            keychain_pending: false,
            hour12: hour12(),
            merged: Arc::new(MergedUsage::default()),
            kept: None,
            limits: None,
            merged_for: None,
            cache: Vec::new(),
            time_zone,
            load: None,
            action: None,
        }
    }

    pub fn save_preferences(&self) {
        t3_client::store::save_ui_state(PREFERENCES_STORE, &self.preferences.to_json());
    }

    pub fn set_window(&mut self, now: i64) {
        let period = self.preferences.period;
        self.window = make_window(period.days(), now, period.resolution(), &self.time_zone, &SystemZone);
        self.summary = self.cached(&self.window, now);
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

    pub fn hidden(&self) -> &BTreeSet<UsageProvider> {
        &self.preferences.hidden_providers
    }

    pub fn config_changed(&mut self) {
        self.limits = None;
    }

    pub fn invalidate_limits(&mut self) {
        self.limits = None;
    }

    /// Brings derived state up to date before a render; cheap when nothing changed.
    pub fn prepare(&mut self, state: &Entity<AppState>, cx: &App) {
        let state = state.read(cx);
        if let Some(config) = state.server_config.config.as_ref() {
            self.environment_id.clone_from(&config.environment.environment_id);
            self.label = if config.environment.label.is_empty() { "This environment".to_owned() } else { config.environment.label.clone() };
        }
        let key = (self.summary.as_ref().map_or(0, |summary| Arc::as_ptr(summary) as usize), self.hidden().clone(), self.selected);
        if self.merged_for.as_ref() != Some(&key) {
            self.merged = Arc::new(match self.summary.as_ref().filter(|_| self.selected) {
                Some(summary) => merge_usage(
                    &[AnsweredUsage {
                        environment_id: self.environment_id.clone(),
                        label: self.label.clone(),
                        summary: Arc::new(summary.without_providers(self.hidden())),
                    }],
                    USAGE_CONTRACT_VERSION,
                ),
                None => MergedUsage::default(),
            });
            if self.summary.is_some() && self.selected {
                self.kept = Some(Kept { window: self.window.clone(), merged: self.merged.clone(), hidden: self.hidden().clone() });
            }
            self.merged_for = Some(key);
        }
    }

    /// What the page shows: this window's answer, the last one dimmed, or nothing yet.
    pub fn shown(&self) -> Option<(UsageSummaryInput, Arc<MergedUsage>, bool)> {
        if self.summary.is_some() {
            return Some((self.window.clone(), self.merged.clone(), false));
        }
        if let Some(kept) = self.kept.as_ref().filter(|kept| kept.hidden == *self.hidden()) {
            return Some((kept.window.clone(), kept.merged.clone(), true));
        }
        if self.error.is_none() && self.access.error.is_none() {
            return None;
        }
        Some((self.window.clone(), self.merged.clone(), false))
    }

    /// The environment as the page's helpers read it.
    pub fn environment(&self, state: &AppState) -> EnvironmentUsage {
        let providers = state.server_config.config.as_ref().map(|config| config.providers.as_slice()).unwrap_or_default();
        EnvironmentUsage {
            environment_id: self.environment_id.clone(),
            label: self.label.clone(),
            is_connected: self.phase == ConnectionPhase::Connected,
            is_pending: self.pending,
            can_read_diagnostics: self.access.can_read_diagnostics,
            error: self.error.clone().or_else(|| self.access.error.map(str::to_owned)),
            summary: self.summary.clone(),
            needs_cursor_keychain_access: usage::needs_cursor_keychain_access(self.summary.as_deref(), providers),
        }
    }

    pub fn limits(&mut self, state: &AppState) -> Arc<LimitsData> {
        if let Some(limits) = &self.limits {
            return limits.clone();
        }
        let config = state.server_config.config.as_ref();
        let hidden: Vec<&str> = self.hidden().iter().map(|provider| provider.driver()).collect();
        let (providers, sources) = match config.filter(|_| self.selected) {
            Some(config) => limits::hide_drivers(&config.providers, &config.usage_limit_sources, &hidden),
            None => (Vec::new(), Vec::new()),
        };
        let environments = [LimitEnvironment { environment_id: &self.environment_id, label: &self.label, providers: &providers, sources: &sources }];
        let data = Arc::new(LimitsData {
            pools: limits::collect_limit_pools(limits::collect_limit_accounts(&environments), self.limits_now),
            notices: limits::collect_limit_notices(&environments),
            links: limits::collect_external_usage_links(&environments),
        });
        self.limits = Some(data.clone());
        data
    }
}

fn phase_of(phase: &Phase) -> ConnectionPhase {
    match phase {
        Phase::Online(ConnectionStatus::Connected) => ConnectionPhase::Connected,
        Phase::Online(ConnectionStatus::Connecting) => ConnectionPhase::Connecting,
        Phase::Online(ConnectionStatus::Reconnecting { .. }) => ConnectionPhase::Reconnecting,
        _ => ConnectionPhase::Disconnected,
    }
}

impl UsageView {
    /// Re-derives access from the connection and grants, and loads once usage is readable.
    pub(super) fn sync_access(&mut self, cx: &mut Context<Self>) {
        let phase = phase_of(&self.state.read(cx).phase);
        self.data.phase = phase;
        self.data.access = resolve_usage_access(phase, self.data.session.as_ref(), false);
        if self.data.access.can_read_diagnostics && phase == ConnectionPhase::Connected && self.data.summary.is_none() && !self.data.pending {
            self.load(false, cx);
        }
    }

    /// Asks for the current window. A refresh first asks the server to refresh model prices.
    /// A reply that is still rescanning shows at once, then settles with `awaitRefresh`.
    pub(super) fn load(&mut self, refresh: bool, cx: &mut Context<Self>) {
        if !self.data.access.can_read_diagnostics || !self.data.selected {
            return;
        }
        let now = now_ms();
        if !refresh && let Some(summary) = self.data.cached(&self.data.window, now) {
            self.data.summary = Some(summary);
            self.data.pending = false;
            return;
        }
        let state = self.state.read(cx);
        let filter = state.server_config.config.as_ref().and_then(|config| config.settings.usage_model_filter.clone());
        let window = self.data.window.clone();
        let input = window.with_model_filter(filter);
        let rates = refresh.then(|| state.rpc(method::REFRESH_USAGE_RATES, json!({})));
        self.data.pending = true;
        self.data.error = None;
        self.data.refreshing = refresh;
        self.data.load = Some(cx.spawn(async move |this, cx| {
            if let Some(rates) = rates {
                let _ = rates.await;
            }
            for (attempt, payload) in [input.payload(), input.awaiting_refresh().payload()].into_iter().enumerate() {
                let request = this.update(cx, |view, cx| view.state.read(cx).request(method::GET_USAGE_SUMMARY, payload, decode_summary));
                let reply = match request {
                    Ok(Some(task)) => task.await.unwrap_or_else(|_| Err("closed".to_owned())),
                    Ok(None) => Err("not connected".to_owned()),
                    Err(_) => return,
                };
                let settling = attempt == 0 && matches!(&reply, Ok(summary) if summary.is_refreshing());
                if this.update(cx, |view, cx| view.apply(&window, reply, settling, cx)).is_err() || !settling {
                    return;
                }
            }
        }));
    }

    fn apply(&mut self, window: &UsageSummaryInput, reply: Result<UsageSummary, String>, settling: bool, cx: &mut Context<Self>) {
        if !self.data.window.same_window(window) {
            return;
        }
        match reply {
            Ok(summary) => {
                let summary = Arc::new(summary);
                self.data.remember(window, summary.clone());
                self.data.summary = Some(summary);
                self.data.error = None;
            }
            Err(error) => {
                tracing::warn!(%error, "could not read usage");
                self.data.summary = None;
                self.data.error = Some("This environment could not report usage.".to_owned());
            }
        }
        self.data.pending = settling;
        if !settling {
            self.data.refreshing = false;
        }
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
        if self.data.preferences.metric == UsageMetric::Limits && self.data.phase == ConnectionPhase::Connected {
            self.refresh_limits(true, cx);
        }
    }

    fn refresh_limits(&mut self, automatic: bool, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        if state.server_config.config.is_none() || (automatic && now_ms() < cx.global::<Host>().limits_refresh_after) {
            return;
        }
        let request = state.rpc(method::REFRESH_PROVIDERS, usage::refresh_providers_payload());
        cx.global_mut::<Host>().limits_refresh_after = i64::MAX;
        self.data.refreshing = !automatic;
        self.data.action = Some(cx.spawn(async move |this, cx| {
            let _ = request.await;
            let _ = this.update(cx, |view, cx| {
                cx.global_mut::<Host>().limits_refresh_after = now_ms() + LIMITS_REFRESH_MS;
                view.data.refreshing = false;
                view.data.limits_now = now_ms();
                view.data.invalidate_limits();
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// `CursorEnableButton`: lets the server read Cursor's account usage from the keychain.
    pub(super) fn enable_cursor_keychain(&mut self, cx: &mut Context<Self>) {
        if self.data.keychain_pending {
            return;
        }
        self.data.keychain_pending = true;
        let request = self.state.read(cx).rpc(commands_method::SERVER_UPDATE_SETTINGS, usage::enable_cursor_keychain_patch());
        self.data.action = Some(cx.spawn(async move |this, cx| {
            let enabled = request.await.is_ok();
            let _ = this.update(cx, |view, cx| {
                view.data.keychain_pending = false;
                if enabled {
                    view.load(true, cx);
                    cx.global_mut::<Host>().limits_refresh_after = 0;
                    view.refresh_limits(false, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Spends one reset credit (`provider.consumeResetCredit`) and reports the outcome.
    pub(super) fn redeem_reset(&mut self, redeem: Redeem, account: String, cx: &mut Context<Self>) {
        let allowed = self.data.session.as_ref().is_some_and(|session| session.grants(usage::PROVIDERS_MANAGE_SCOPE));
        if !allowed {
            return;
        }
        self.reset_status = Some((account.clone(), None));
        let request = self.state.read(cx).rpc(method::CONSUME_RESET_CREDIT, redeem.input.payload());
        self.data.action = Some(cx.spawn(async move |this, cx| {
            let status = match request.await {
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

    pub(super) fn can_manage_providers(&self) -> bool {
        self.data.session.as_ref().is_some_and(|session| session.grants(usage::PROVIDERS_MANAGE_SCOPE))
    }
}
