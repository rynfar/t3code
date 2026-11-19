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
use crate::server_config::lenient_vec;

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
    /// A cached reply during a refresh; ask again with `awaitRefresh` for fresh data.
    #[serde(default)]
    pub refreshing: bool,
}

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

    /// `provisional: true` payload (`pingdotgg/t3code#33993`): asks the server for an immediate
    /// answer from the last scan before running the full read.
    pub fn provisional_payload(&self) -> Value {
        let mut value = self.payload();
        if let Some(obj) = value.as_object_mut() {
            obj.insert("provisional".to_owned(), Value::Bool(true));
            obj.remove("awaitRefresh");
        }
        value
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

/// `server.refreshProviders` as the web sends it for Limits.
pub fn refresh_providers_payload() -> Value {
    crate::providers::RefreshProviders::Limits.payload()
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

/// Model price overrides and model alias mappings (`UsagePriceOverrides.tsx`,
/// `usagePriceForm.ts`, `usagePriceTable.ts`, `usagePriceTargets.ts`).
pub mod price_overrides {
    use std::collections::{BTreeMap, BTreeSet};

    use serde::{Deserialize, Serialize};
    use serde_json::{Map, Value, json};

    pub const GET_CONFIG: &str = "server.getConfig";

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct UsageModelPriceOverride {
        pub input_cost_per_million_tokens: f64,
        pub output_cost_per_million_tokens: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cache_read_cost_per_million_tokens: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cache_write_cost_per_million_tokens: Option<f64>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum UsagePriceField {
        Input,
        Output,
        CacheRead,
        CacheWrite,
    }

    impl UsagePriceField {
        pub const ALL: [UsagePriceField; 4] = [
            UsagePriceField::Input,
            UsagePriceField::Output,
            UsagePriceField::CacheRead,
            UsagePriceField::CacheWrite,
        ];

        pub fn label(self) -> &'static str {
            match self {
                UsagePriceField::Input => "Input",
                UsagePriceField::Output => "Output",
                UsagePriceField::CacheRead => "Cache read",
                UsagePriceField::CacheWrite => "Cache write",
            }
        }

        pub fn optional(self) -> bool {
            matches!(self, UsagePriceField::CacheRead | UsagePriceField::CacheWrite)
        }
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct UsagePriceForm {
        pub model: String,
        pub input: String,
        pub output: String,
        pub cache_read: String,
        pub cache_write: String,
    }

    impl UsagePriceForm {
        pub fn from_override(model: impl Into<String>, price: Option<&UsageModelPriceOverride>) -> Self {
            Self {
                model: model.into(),
                input: price.map(|p| p.input_cost_per_million_tokens.to_string()).unwrap_or_default(),
                output: price.map(|p| p.output_cost_per_million_tokens.to_string()).unwrap_or_default(),
                cache_read: price.and_then(|p| p.cache_read_cost_per_million_tokens).map(|v| v.to_string()).unwrap_or_default(),
                cache_write: price.and_then(|p| p.cache_write_cost_per_million_tokens).map(|v| v.to_string()).unwrap_or_default(),
            }
        }

        pub fn field(&self, field: UsagePriceField) -> &str {
            match field {
                UsagePriceField::Input => &self.input,
                UsagePriceField::Output => &self.output,
                UsagePriceField::CacheRead => &self.cache_read,
                UsagePriceField::CacheWrite => &self.cache_write,
            }
        }

        pub fn set_field(&mut self, field: UsagePriceField, value: String) {
            match field {
                UsagePriceField::Input => self.input = value,
                UsagePriceField::Output => self.output = value,
                UsagePriceField::CacheRead => self.cache_read = value,
                UsagePriceField::CacheWrite => self.cache_write = value,
            }
        }
    }

    /// `parseUsagePriceForm`: blank cache prices inherit input rate; explicit zero means free.
    pub fn parse_usage_price_form(form: &UsagePriceForm) -> Option<(String, UsageModelPriceOverride)> {
        let model = form.model.trim();
        if model.is_empty() {
            return None;
        }
        let parse_rate = |raw: &str, optional: bool| -> Option<Option<f64>> {
            let clean = raw.trim().replace(',', ".");
            if clean.is_empty() {
                return optional.then_some(None);
            }
            let value = clean.parse::<f64>().ok()?;
            (value.is_finite() && value >= 0.0).then_some(Some(value))
        };
        let input = parse_rate(&form.input, false)??;
        let output = parse_rate(&form.output, false)??;
        let cache_read = parse_rate(&form.cache_read, true)?;
        let cache_write = parse_rate(&form.cache_write, true)?;
        Some((
            model.to_owned(),
            UsageModelPriceOverride {
                input_cost_per_million_tokens: input,
                output_cost_per_million_tokens: output,
                cache_read_cost_per_million_tokens: cache_read,
                cache_write_cost_per_million_tokens: cache_write,
            },
        ))
    }

    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct UsagePriceSettings {
        pub prices: BTreeMap<String, UsageModelPriceOverride>,
        pub aliases: BTreeMap<String, String>,
        pub aliases_supported: bool,
        pub ghq_clone_layout: bool,
    }

    /// Reads `settings.usagePriceOverrides`, `settings.usageModelAliases`, and
    /// `settings.ghqCloneLayout` from a `server.getConfig` reply.
    pub fn decode_usage_price_settings(config: &Value) -> UsagePriceSettings {
        let settings = config.get("settings").unwrap_or(&Value::Null);
        let prices = settings
            .get("usagePriceOverrides")
            .and_then(|v| serde_json::from_value::<BTreeMap<String, UsageModelPriceOverride>>(v.clone()).ok())
            .unwrap_or_default();
        let aliases_field = settings.get("usageModelAliases");
        let aliases_supported = aliases_field.is_some();
        let aliases = aliases_field
            .and_then(|v| serde_json::from_value::<BTreeMap<String, String>>(v.clone()).ok())
            .unwrap_or_default();
        let ghq_clone_layout = settings.get("ghqCloneLayout").and_then(Value::as_bool).unwrap_or(false);
        UsagePriceSettings { prices, aliases, aliases_supported, ghq_clone_layout }
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct UsagePriceRowDraft {
        pub id: String,
        pub model: String,
        pub is_new: bool,
        pub form: UsagePriceForm,
        pub alias: String,
        pub removed: bool,
    }

    impl UsagePriceRowDraft {
        pub fn is_empty(&self) -> bool {
            self.is_new
                && self.model.trim().is_empty()
                && self.alias.trim().is_empty()
                && self.form.input.trim().is_empty()
                && self.form.output.trim().is_empty()
                && self.form.cache_read.trim().is_empty()
                && self.form.cache_write.trim().is_empty()
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum UsagePriceChange {
        Price { model: String, price: Option<UsageModelPriceOverride> },
        Alias { model: String, alias: Option<String> },
    }

    /// Validates row drafts and builds minimal `UsagePriceChange` list plus per-row error messages
    /// (`usagePriceTableChanges` in `usagePriceTable.ts`, including alias cycle detection).
    pub fn usage_price_table_changes(
        label: &str,
        current: &UsagePriceSettings,
        drafts: &[UsagePriceRowDraft],
    ) -> (Vec<UsagePriceChange>, BTreeMap<String, String>) {
        let mut changes = Vec::new();
        let mut errors = BTreeMap::new();
        for draft in drafts {
            if draft.is_empty() {
                continue;
            }
            let model = draft.model.trim();
            let current_price = current.prices.get(model);
            let current_alias = current.aliases.get(model).map(String::as_str);
            if draft.removed {
                if current_price.is_some() {
                    changes.push(UsagePriceChange::Price { model: model.to_owned(), price: None });
                }
                if current_alias.is_some() {
                    changes.push(UsagePriceChange::Alias { model: model.to_owned(), alias: None });
                }
                continue;
            }
            let alias = draft.alias.trim();
            if !alias.is_empty() {
                if model.is_empty() {
                    errors.insert(draft.id.clone(), "Enter a model ID.".to_owned());
                } else if alias == model {
                    errors.insert(draft.id.clone(), "Map to a different model.".to_owned());
                } else {
                    if current_alias != Some(alias) {
                        changes.push(UsagePriceChange::Alias { model: model.to_owned(), alias: Some(alias.to_owned()) });
                    }
                    if current_price.is_some() {
                        changes.push(UsagePriceChange::Price { model: model.to_owned(), price: None });
                    }
                }
                continue;
            }
            if current_alias.is_some() {
                changes.push(UsagePriceChange::Alias { model: model.to_owned(), alias: None });
                if current_price.is_none()
                    && draft.form.input.trim().is_empty()
                    && draft.form.output.trim().is_empty()
                    && draft.form.cache_read.trim().is_empty()
                    && draft.form.cache_write.trim().is_empty()
                {
                    continue;
                }
            }
            let mut form = draft.form.clone();
            form.model = model.to_owned();
            match parse_usage_price_form(&form) {
                None => {
                    let message = if model.is_empty() {
                        "Enter a model ID.".to_owned()
                    } else if form.input.trim().is_empty() {
                        format!("Input is required on {label}.")
                    } else if form.output.trim().is_empty() {
                        format!("Output is required on {label}.")
                    } else {
                        "Use non-negative numbers for prices.".to_owned()
                    };
                    errors.insert(draft.id.clone(), message);
                }
                Some((model_name, parsed)) => {
                    if current_price != Some(&parsed) {
                        changes.push(UsagePriceChange::Price { model: model_name, price: Some(parsed) });
                    }
                }
            }
        }
        // Cycle detection over the resulting alias map.
        let mut final_aliases = current.aliases.clone();
        for change in &changes {
            if let UsagePriceChange::Alias { model, alias } = change {
                match alias {
                    Some(target) => {
                        final_aliases.insert(model.clone(), target.clone());
                    }
                    None => {
                        final_aliases.remove(model);
                    }
                }
            }
        }
        for draft in drafts {
            if draft.removed || errors.contains_key(&draft.id) {
                continue;
            }
            let mut cursor = draft.model.trim();
            let mut seen = BTreeSet::new();
            while let Some(next) = final_aliases.get(cursor) {
                if !seen.insert(cursor.to_owned()) {
                    errors.insert(draft.id.clone(), format!("Model mappings cannot form a cycle on {label}."));
                    break;
                }
                cursor = next.as_str();
            }
        }
        (changes, errors)
    }

    /// Builds the `server.updateSettings` payload for `changes` (`writeUsagePrices`), or `None`
    /// if there are no changes to save.
    pub fn usage_price_patch_payload(changes: &[UsagePriceChange]) -> Option<Value> {
        let mut prices = Map::new();
        let mut aliases = Map::new();
        for change in changes {
            match change {
                UsagePriceChange::Price { model, price } => {
                    prices.insert(
                        model.clone(),
                        price.as_ref().map_or(Value::Null, |p| serde_json::to_value(p).unwrap_or(Value::Null)),
                    );
                }
                UsagePriceChange::Alias { model, alias } => {
                    aliases.insert(model.clone(), alias.as_ref().map_or(Value::Null, |a| Value::String(a.clone())));
                }
            }
        }
        if prices.is_empty() && aliases.is_empty() {
            return None;
        }
        let mut patch = Map::new();
        if !prices.is_empty() {
            patch.insert("usagePriceOverrides".to_owned(), Value::Object(prices));
        }
        if !aliases.is_empty() {
            patch.insert("usageModelAliases".to_owned(), Value::Object(aliases));
        }
        Some(json!({ "patch": Value::Object(patch) }))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_prices_detects_alias_cycles_and_builds_settings_patch() {
            let form = UsagePriceForm {
                model: " custom-model ".into(),
                input: "3,5".into(),
                output: "15".into(),
                cache_read: "0".into(),
                cache_write: "".into(),
            };
            let (model, parsed) = parse_usage_price_form(&form).expect("valid price form");
            assert_eq!(model, "custom-model");
            assert_eq!(parsed.input_cost_per_million_tokens, 3.5);
            assert_eq!(parsed.output_cost_per_million_tokens, 15.0);
            assert_eq!(parsed.cache_read_cost_per_million_tokens, Some(0.0));
            assert_eq!(parsed.cache_write_cost_per_million_tokens, None);

            let current = decode_usage_price_settings(&json!({
                "settings": {
                    "usagePriceOverrides": {
                        "old-model": { "inputCostPerMillionTokens": 1.0, "outputCostPerMillionTokens": 2.0 }
                    },
                    "usageModelAliases": { "b": "a" },
                    "ghqCloneLayout": true
                }
            }));
            assert!(current.aliases_supported);
            assert!(current.ghq_clone_layout);
            assert!(current.prices.contains_key("old-model"));

            // Cycle a -> b -> a is caught.
            let cycle_draft = UsagePriceRowDraft {
                id: "1".into(),
                model: "a".into(),
                is_new: true,
                form: UsagePriceForm::default(),
                alias: "b".into(),
                removed: false,
            };
            let (_, errs) = usage_price_table_changes("Local", &current, &[cycle_draft]);
            assert_eq!(errs.get("1").map(String::as_str), Some("Model mappings cannot form a cycle on Local."));

            // Valid add + remove builds patch.
            let add = UsagePriceRowDraft {
                id: "2".into(),
                model: "custom-model".into(),
                is_new: true,
                form,
                alias: String::new(),
                removed: false,
            };
            let remove = UsagePriceRowDraft {
                id: "3".into(),
                model: "old-model".into(),
                is_new: false,
                form: UsagePriceForm::default(),
                alias: String::new(),
                removed: true,
            };
            let (changes, errs) = usage_price_table_changes("Local", &current, &[add, remove]);
            assert!(errs.is_empty());
            let payload = usage_price_patch_payload(&changes).expect("patch payload");
            assert_eq!(payload["patch"]["usagePriceOverrides"]["old-model"], Value::Null);
            assert_eq!(payload["patch"]["usagePriceOverrides"]["custom-model"]["inputCostPerMillionTokens"], 3.5);
        }
    }
}

#[cfg(test)]
mod tests;
