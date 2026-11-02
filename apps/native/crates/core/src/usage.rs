//! Usage: the summary the Usage page reads (`contracts/src/usage.ts`), its RPCs, and the client
//! logic around them: `client-runtime/src/state/usage.ts`, `usageAccess.ts` and
//! `usageProgress.ts`, plus the page's providers, metrics, periods and saved preferences
//! (`web/src/components/usage/usageProviders.ts`, `usageShortcuts.ts`, `usagePagePreferences.ts`).
//! Arrays the contract marks forward-compatible drop entries that fail to decode, so a bucket
//! from a provider this build does not know is skipped rather than failing the summary.

pub mod chart;
pub mod context_window;
pub mod format;
pub mod limits;
pub mod merge;

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::AuthSessionState;
use crate::server_config::{ServerProvider, lenient_vec};

pub mod method {
    pub const GET_USAGE_SUMMARY: &str = "server.getUsageSummary";
    pub const REFRESH_USAGE_RATES: &str = "server.refreshUsageRates";
    pub const REFRESH_PROVIDERS: &str = "server.refreshProviders";
    pub const CONSUME_RESET_CREDIT: &str = "provider.consumeResetCredit";
}

pub const USAGE_CONTRACT_VERSION: f64 = 6.;
pub const USAGE_MERGE_COMPATIBLE_SINCE: f64 = 4.;
pub const DIAGNOSTICS_READ_SCOPE: &str = "diagnostics:read";
pub const PROVIDERS_MANAGE_SCOPE: &str = "providers:manage";

/// `UsageProviderKind`, in the page's display order (`PROVIDER_ORDER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageProvider {
    Codex,
    Claude,
    Grok,
    Cursor,
    Opencode,
    Antigravity,
    Pi,
    Muse,
}

impl UsageProvider {
    pub const ORDER: [UsageProvider; 8] = [
        UsageProvider::Codex,
        UsageProvider::Claude,
        UsageProvider::Grok,
        UsageProvider::Cursor,
        UsageProvider::Opencode,
        UsageProvider::Antigravity,
        UsageProvider::Pi,
        UsageProvider::Muse,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            UsageProvider::Codex => "codex",
            UsageProvider::Claude => "claude",
            UsageProvider::Grok => "grok",
            UsageProvider::Cursor => "cursor",
            UsageProvider::Opencode => "opencode",
            UsageProvider::Antigravity => "antigravity",
            UsageProvider::Pi => "pi",
            UsageProvider::Muse => "muse",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            UsageProvider::Codex => "Codex",
            UsageProvider::Claude => "Claude Code",
            UsageProvider::Grok => "Grok Build",
            UsageProvider::Cursor => "Cursor",
            UsageProvider::Opencode => "OpenCode",
            UsageProvider::Antigravity => "Antigravity",
            UsageProvider::Pi => "Pi",
            UsageProvider::Muse => "Muse Code",
        }
    }

    /// The provider driver whose brand mark and limits this provider shows.
    pub fn driver(self) -> &'static str {
        match self {
            UsageProvider::Codex => "codex",
            UsageProvider::Claude => "claudeAgent",
            UsageProvider::Grok => "grok",
            UsageProvider::Cursor => "cursor",
            UsageProvider::Opencode => "opencode",
            UsageProvider::Antigravity => "antigravity",
            UsageProvider::Pi => "pi",
            UsageProvider::Muse => "muse",
        }
    }

    pub fn from_driver(driver: &str) -> Option<UsageProvider> {
        UsageProvider::ORDER.into_iter().find(|provider| provider.driver() == driver)
    }

    pub fn index(self) -> usize {
        UsageProvider::ORDER.iter().position(|provider| *provider == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageResolution {
    #[default]
    Day,
    Hour,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageCostSource {
    ProviderReported,
    ModelPriced,
    Unpriced,
}

/// `UsageTokenTotals`: reasoning is already part of output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTokenTotals {
    pub uncached_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCategoryCost {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    pub day: String,
    #[serde(default)]
    pub hour_start: Option<String>,
    pub provider: UsageProvider,
    pub model: String,
    #[serde(default)]
    pub source_path: Option<String>,
    pub totals: UsageTokenTotals,
    pub cost_usd: f64,
    pub cache_savings_usd: f64,
    #[serde(default)]
    pub category_cost_usd: Option<UsageCategoryCost>,
    #[serde(default)]
    pub fast_cost_usd: Option<f64>,
    #[serde(default)]
    pub ultrafast_cost_usd: Option<f64>,
    #[serde(default)]
    pub speed_premium_usd: Option<f64>,
    pub cost_source: UsageCostSource,
    pub records: u64,
    pub unpriced_records: u64,
    pub sessions: u64,
}

impl UsageBucket {
    /// Processed tokens; reasoning is a subset of output and is not added again.
    pub fn tokens(&self) -> u64 {
        self.totals.uncached_input_tokens + self.totals.cached_input_tokens + self.totals.cache_creation_tokens + self.totals.output_tokens
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSourceFingerprint {
    pub host_id: String,
    pub provider: UsageProvider,
    pub resolved_home_path: String,
    #[serde(default)]
    pub volume_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageSourceStatus {
    Ok,
    Missing,
    Partial,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSource {
    pub fingerprint: UsageSourceFingerprint,
    pub status: UsageSourceStatus,
    #[serde(default)]
    pub scanned_files: u64,
    #[serde(default)]
    pub skipped_files: u64,
    #[serde(default)]
    pub malformed_records: u64,
    #[serde(default)]
    pub distinct_sessions: u64,
    #[serde(default)]
    pub message: Option<String>,
    /// `enableCursorKeychain` when reading Cursor's account usage needs the keychain.
    #[serde(default)]
    pub action: Option<String>,
    /// A cached reply during a refresh; ask again with `awaitRefresh` for fresh data.
    #[serde(default)]
    pub refreshing: bool,
}

pub const ENABLE_CURSOR_KEYCHAIN: &str = "enableCursorKeychain";

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePricing {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub fetched_at: Option<String>,
    #[serde(default)]
    pub known_models: u64,
}

/// `UsageModelSelector`: a provider's original model id; no provider matches any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageModelSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageModelFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<UsageModelSelector>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<UsageModelSelector>>,
}

/// `UsageSummaryInput`: calendar days in an IANA zone, or a rolling window of hours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummaryInput {
    pub since_day: String,
    pub until_day: String,
    pub time_zone: String,
    pub resolution: UsageResolution,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_time: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub await_refresh: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_filter: Option<UsageModelFilter>,
}

impl UsageSummaryInput {
    /// `usageSummaryInput`: the page asks each environment with its configured model filter.
    pub fn with_model_filter(&self, filter: Option<UsageModelFilter>) -> UsageSummaryInput {
        UsageSummaryInput { model_filter: filter, ..self.clone() }
    }

    pub fn awaiting_refresh(&self) -> UsageSummaryInput {
        UsageSummaryInput { await_refresh: true, ..self.clone() }
    }

    /// The same window: what a cached reply may stand in for.
    pub fn same_window(&self, other: &UsageSummaryInput) -> bool {
        self.since_day == other.since_day
            && self.until_day == other.until_day
            && self.time_zone == other.time_zone
            && self.resolution == other.resolution
            && self.since_time == other.since_time
            && self.until_time == other.until_time
    }

    pub fn payload(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub contract_version: f64,
    #[serde(default)]
    pub read_at: String,
    pub time_zone: String,
    pub since_day: String,
    pub until_day: String,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub buckets: Vec<UsageBucket>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub sources: Vec<UsageSource>,
    #[serde(default)]
    pub pricing: UsagePricing,
    #[serde(default)]
    pub scan_duration_ms: u64,
    #[serde(default)]
    pub model_filter: Option<UsageModelFilter>,
}

impl UsageSummary {
    /// Some source answered from cache while it rescans; the page asks again to settle it.
    pub fn is_refreshing(&self) -> bool {
        self.sources.iter().any(|source| source.refreshing)
    }

    /// The same summary without the providers the page hides (`withoutProviders`).
    pub fn without_providers(&self, hidden: &BTreeSet<UsageProvider>) -> UsageSummary {
        if hidden.is_empty() {
            return self.clone();
        }
        UsageSummary {
            buckets: self.buckets.iter().filter(|bucket| !hidden.contains(&bucket.provider)).cloned().collect(),
            sources: self.sources.iter().filter(|source| !hidden.contains(&source.fingerprint.provider)).cloned().collect(),
            ..self.clone()
        }
    }

    /// The same summary with only the buckets `keep` accepts.
    pub fn keep_buckets(&self, keep: impl Fn(&UsageBucket) -> bool) -> UsageSummary {
        UsageSummary { buckets: self.buckets.iter().filter(|bucket| keep(bucket)).cloned().collect(), ..self.clone() }
    }
}

pub fn decode_summary(value: Value) -> Result<UsageSummary, String> {
    serde_json::from_value(value).map_err(|error| format!("Could not read usage: {error}"))
}

/// `needsCursorKeychainAccess`: Cursor's account usage is a keychain grant away.
pub fn needs_cursor_keychain_access(summary: Option<&UsageSummary>, providers: &[ServerProvider]) -> bool {
    summary.is_some_and(|summary| summary.sources.iter().any(|source| source.action.as_deref() == Some(ENABLE_CURSOR_KEYCHAIN)))
        && providers.iter().any(|provider| provider.driver == "cursor" && provider.status == "ready")
}

/// `cursorKeychainAccessEnvironments`: an environment already reading the Cursor account reports
/// every machine's history, so the prompt would only add duplicates.
pub fn cursor_keychain_prompt(summaries: &[(Option<&UsageSummary>, bool)]) -> Vec<usize> {
    let has_account = summaries.iter().any(|(summary, _)| {
        summary.is_some_and(|summary| {
            summary.sources.iter().any(|source| source.fingerprint.provider == UsageProvider::Cursor && source.fingerprint.host_id == "cursor.com")
        })
    });
    if has_account {
        return Vec::new();
    }
    summaries.iter().enumerate().filter(|(_, (_, needs))| *needs).map(|(index, _)| index).collect()
}

pub fn enable_cursor_keychain_patch() -> Value {
    json!({ "patch": { "cursorKeychainUsageEnabled": true } })
}

/// `server.refreshProviders` as the web sends it for Limits.
pub fn refresh_providers_payload() -> Value {
    json!({ "workspaceSnapshotsOnDemand": true })
}

/// Where the connection stands, as `EnvironmentConnectionPhase` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionPhase {
    Connected,
    Connecting,
    Reconnecting,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageAccess {
    pub can_read_diagnostics: bool,
    pub is_pending: bool,
    pub error: Option<&'static str>,
}

/// `resolveUsageAccess`.
pub fn resolve_usage_access(phase: ConnectionPhase, session: Option<&AuthSessionState>, has_session_error: bool) -> UsageAccess {
    if has_session_error {
        return UsageAccess {
            can_read_diagnostics: false,
            is_pending: false,
            error: Some("Could not check this connection's access to diagnostics and usage."),
        };
    }
    let Some(session) = session else {
        // An offline connection cannot finish its session check; keep it out of pending totals.
        let is_pending = matches!(phase, ConnectionPhase::Connected | ConnectionPhase::Connecting | ConnectionPhase::Reconnecting);
        return UsageAccess { can_read_diagnostics: false, is_pending, error: (!is_pending).then_some("This environment is not connected.") };
    };
    let can_read_diagnostics = session.grants(DIAGNOSTICS_READ_SCOPE);
    UsageAccess {
        can_read_diagnostics,
        is_pending: false,
        error: (!can_read_diagnostics).then_some("This connection does not have access to diagnostics and usage."),
    }
}

/// One environment as the page tracks it (`EnvironmentUsageStatus`).
#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentUsage {
    pub environment_id: String,
    pub label: String,
    pub is_connected: bool,
    pub is_pending: bool,
    pub can_read_diagnostics: bool,
    pub error: Option<String>,
    pub summary: Option<std::sync::Arc<UsageSummary>>,
    pub needs_cursor_keychain_access: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentProgress {
    Inactive,
    Loading,
    Stale,
    Partway(Vec<UsageProvider>),
    Ready,
}

/// `usageEnvironmentProgress`.
pub fn environment_progress(environment: &EnvironmentUsage, refreshing: bool) -> EnvironmentProgress {
    if !environment.is_connected || environment.error.is_some() {
        return EnvironmentProgress::Inactive;
    }
    let Some(summary) = &environment.summary else { return EnvironmentProgress::Loading };
    if refreshing {
        return EnvironmentProgress::Stale;
    }
    if !environment.is_pending {
        return EnvironmentProgress::Ready;
    }
    let mut providers = Vec::new();
    for source in summary.sources.iter().filter(|source| source.refreshing) {
        if !providers.contains(&source.fingerprint.provider) {
            providers.push(source.fingerprint.provider);
        }
    }
    if providers.is_empty() { EnvironmentProgress::Stale } else { EnvironmentProgress::Partway(providers) }
}

/// `updatingProvidersLabel`.
pub fn updating_providers_label(providers: &[UsageProvider]) -> String {
    match providers {
        [only] => format!("Updating {}…", only.label()),
        _ => format!("Updating {} providers…", providers.len()),
    }
}

/// `usageEnvironmentStatus` as the environment menu labels it.
pub fn environment_status_label(environment: &EnvironmentUsage, refreshing: bool) -> String {
    if environment.error.is_some() {
        return "Unavailable".to_owned();
    }
    if environment.summary.as_ref().is_some_and(|summary| !merge::is_compatible_contract_version(summary.contract_version, USAGE_CONTRACT_VERSION)) {
        return "Update required".to_owned();
    }
    if !environment.is_connected {
        return "Connecting…".to_owned();
    }
    match environment_progress(environment, refreshing) {
        EnvironmentProgress::Loading => "Scanning…".to_owned(),
        EnvironmentProgress::Stale => "Refreshing…".to_owned(),
        EnvironmentProgress::Partway(providers) => updating_providers_label(&providers),
        EnvironmentProgress::Inactive | EnvironmentProgress::Ready => "Ready".to_owned(),
    }
}

/// `usageLoadingState`, without the providers the page hides (`usageLoading`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadingState {
    pub partial: bool,
    pub every_provider: bool,
    pub providers: BTreeSet<UsageProvider>,
}

impl LoadingState {
    pub fn is_loading(&self, provider: UsageProvider) -> bool {
        self.every_provider || self.providers.contains(&provider)
    }
}

pub fn loading_state(environments: &[EnvironmentUsage], refreshing: bool, showing_kept: bool, hidden: &BTreeSet<UsageProvider>) -> LoadingState {
    if showing_kept {
        return LoadingState { partial: true, every_provider: true, providers: BTreeSet::new() };
    }
    let mut state = LoadingState::default();
    for environment in environments {
        match environment_progress(environment, refreshing) {
            EnvironmentProgress::Loading | EnvironmentProgress::Stale => state.every_provider = true,
            EnvironmentProgress::Partway(providers) => state.providers.extend(providers.into_iter().filter(|provider| !hidden.contains(provider))),
            EnvironmentProgress::Inactive | EnvironmentProgress::Ready => {}
        }
    }
    state.partial = state.every_provider || !state.providers.is_empty();
    state
}

/// The page's metric (`METRIC_OPTIONS`); keys C, T and L.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageMetric {
    Cost,
    Tokens,
    #[default]
    Limits,
}

impl UsageMetric {
    pub const ALL: [UsageMetric; 3] = [UsageMetric::Cost, UsageMetric::Tokens, UsageMetric::Limits];

    pub fn label(self) -> &'static str {
        match self {
            UsageMetric::Cost => "Cost",
            UsageMetric::Tokens => "Tokens",
            UsageMetric::Limits => "Limits",
        }
    }

    pub fn command(self) -> &'static str {
        match self {
            UsageMetric::Cost => "usage.cost",
            UsageMetric::Tokens => "usage.tokens",
            UsageMetric::Limits => "usage.limits",
        }
    }
}

/// The page's period (`WINDOW_OPTIONS`); `cmd-shift-1..4`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UsagePeriod {
    Day,
    Week,
    #[default]
    Month,
    Quarter,
}

impl UsagePeriod {
    pub const ALL: [UsagePeriod; 4] = [UsagePeriod::Day, UsagePeriod::Week, UsagePeriod::Month, UsagePeriod::Quarter];

    pub fn days(self) -> u32 {
        match self {
            UsagePeriod::Day => 1,
            UsagePeriod::Week => 7,
            UsagePeriod::Month => 30,
            UsagePeriod::Quarter => 90,
        }
    }

    pub fn from_days(days: u64) -> Option<UsagePeriod> {
        UsagePeriod::ALL.into_iter().find(|period| u64::from(period.days()) == days)
    }

    pub fn label(self) -> &'static str {
        match self {
            UsagePeriod::Day => "Past 24h",
            UsagePeriod::Week => "7 days",
            UsagePeriod::Month => "30 days",
            UsagePeriod::Quarter => "90 days",
        }
    }

    pub fn command(self) -> &'static str {
        match self {
            UsagePeriod::Day => "usage.period.day",
            UsagePeriod::Week => "usage.period.week",
            UsagePeriod::Month => "usage.period.month",
            UsagePeriod::Quarter => "usage.period.quarter",
        }
    }

    /// The past 24 hours read hourly; every other period reads days.
    pub fn resolution(self) -> UsageResolution {
        if self == UsagePeriod::Day { UsageResolution::Hour } else { UsageResolution::Day }
    }
}

/// `UsagePagePreferences` under `t3code:usage-page-preferences:v1`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsagePagePreferences {
    pub metric: UsageMetric,
    pub period: UsagePeriod,
    pub hidden_providers: BTreeSet<UsageProvider>,
}

pub const PREFERENCES_STORE: &str = "usage-page-preferences";

impl UsagePagePreferences {
    /// An invalid value falls back to the defaults as a whole, as the schema decode does.
    pub fn from_json(value: Option<&Value>) -> UsagePagePreferences {
        let parse = || -> Option<UsagePagePreferences> {
            let value = value?;
            let metric = serde_json::from_value(value.get("metric")?.clone()).ok()?;
            let period = UsagePeriod::from_days(value.get("windowDays")?.as_u64()?)?;
            let hidden_providers = match value.get("hiddenProviders") {
                None => BTreeSet::new(),
                Some(hidden) => serde_json::from_value::<Vec<UsageProvider>>(hidden.clone()).ok()?.into_iter().collect(),
            };
            Some(UsagePagePreferences { metric, period, hidden_providers })
        };
        parse().unwrap_or_default()
    }

    pub fn to_json(&self) -> Value {
        let hidden: Vec<UsageProvider> = UsageProvider::ORDER.into_iter().filter(|provider| self.hidden_providers.contains(provider)).collect();
        let mut value = json!({ "metric": self.metric, "windowDays": self.period.days() });
        if !hidden.is_empty() {
            value["hiddenProviders"] = json!(hidden);
        }
        value
    }
}

/// `UsageProviderFilter`'s trigger label.
pub fn provider_filter_label(hidden: &BTreeSet<UsageProvider>) -> String {
    let visible: Vec<UsageProvider> = UsageProvider::ORDER.into_iter().filter(|provider| !hidden.contains(provider)).collect();
    match visible.as_slice() {
        all if all.len() == UsageProvider::ORDER.len() => "All providers".to_owned(),
        [] => "No providers".to_owned(),
        [only] => only.label().to_owned(),
        many => format!("{} providers", many.len()),
    }
}

/// `providersWithUsage`: providers with tokens or cost, in display order.
pub fn providers_with_usage(totals: &[merge::ProviderTotals]) -> Vec<UsageProvider> {
    UsageProvider::ORDER
        .into_iter()
        .filter(|provider| totals.iter().any(|entry| entry.provider == *provider && (entry.total_tokens > 0 || entry.cost_usd > 0.)))
        .collect()
}

#[cfg(test)]
mod tests;
