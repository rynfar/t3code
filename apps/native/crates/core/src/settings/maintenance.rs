//! Maintenance: `MaintenanceSettingsPanel` in `web/src/components/settings/SettingsPanels.tsx`
//! (updates, restart continuation, the diagnostics link), `BackgroundActivitySettings.tsx` with
//! `resolveBackgroundActivityProfileOption`, `backgroundActivitySharedPolicySettings` and
//! `backgroundActivityOverrideSettings` (`SettingsPanels.logic.ts`) over
//! `shared/src/backgroundActivitySettings.ts`, and `StorageCleanupSections` in
//! `StorageSettings.tsx` (artifact and log retention, Delete now and the last run's report).
//! `formatBytes` is `web/src/lib/formatBytes.ts`.

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use super::providers::background::{Profile, ProviderPanelSettings, Resolved, preset};
use super::worktrees::{CleanupRules, MAX_RETENTION_DAYS};
use crate::server_config::ServerSettings;

pub const GET_STORAGE_CLEANUP_REPORT: &str = "server.getStorageCleanupReport";
pub const RUN_STORAGE_CLEANUP: &str = "server.runStorageCleanup";
pub const GET_DATABASE_STORAGE: &str = "server.getDatabaseStorage";

/// The `ServerSettings` keys the Updates section edits; both default on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MaintenanceSettings {
    #[serde(deserialize_with = "bool_or_true")]
    pub enable_provider_update_checks: bool,
    #[serde(deserialize_with = "bool_or_true")]
    pub continue_threads_after_server_update: bool,
}

impl Default for MaintenanceSettings {
    fn default() -> Self {
        MaintenanceSettings { enable_provider_update_checks: true, continue_threads_after_server_update: true }
    }
}

fn bool_or_true<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(Value::deserialize(d)?.as_bool().unwrap_or(true))
}

/// One Maintenance page edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceChange {
    ProviderUpdateChecks(bool),
    ContinueThreads(bool),
    BrowserArtifactsAfterDays(Option<u32>),
    LogsAfterDays(Option<u32>),
}

impl MaintenanceChange {
    pub fn payload(self) -> Value {
        let clamp = |days: Option<u32>| days.map(|days| days.clamp(1, MAX_RETENTION_DAYS));
        let patch = match self {
            MaintenanceChange::ProviderUpdateChecks(on) => {
                json!({ "enableProviderUpdateChecks": on })
            }
            MaintenanceChange::ContinueThreads(on) => {
                json!({ "continueThreadsAfterServerUpdate": on })
            }
            MaintenanceChange::BrowserArtifactsAfterDays(days) => {
                json!({ "storageCleanup": { "browserArtifactsAfterDays": clamp(days) } })
            }
            MaintenanceChange::LogsAfterDays(days) => {
                json!({ "storageCleanup": { "logsAfterDays": clamp(days) } })
            }
        };
        json!({ "patch": patch })
    }
}

/// `BackgroundActivityProfileOption`: a named profile, or the custom intervals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileOption {
    Profile(Profile),
    Advanced,
}

pub const PROFILES: [Profile; 3] = [Profile::Balanced, Profile::Performance, Profile::BatterySaver];

/// `BACKGROUND_ACTIVITY_PROFILE_LABELS`.
pub fn profile_label(profile: Profile) -> &'static str {
    match profile {
        Profile::Balanced => "Balanced",
        Profile::Performance => "Performance",
        Profile::BatterySaver => "Battery saver",
    }
}

pub fn parse_profile(value: &str) -> Option<Profile> {
    PROFILES.into_iter().find(|profile| profile.as_str() == value)
}

/// `BACKGROUND_ACTIVITY_PROFILE_DESCRIPTIONS`.
pub fn profile_description(profile: Profile) -> &'static str {
    match profile {
        Profile::Balanced => "Pauses probes for idle clients, locked hosts, or low power mode.",
        Profile::Performance => "Allows scoped background probes while any subscribed client remains connected.",
        Profile::BatterySaver => "Also pauses background probes when the host or client is on battery.",
    }
}

pub const ADVANCED_OPTION: &str = "advanced";
pub const POLICY_TOOLTIP: &str = "This shared policy gates background work such as Git refreshes and provider health probes after their individual intervals elapse.";

fn same_values(left: &Resolved, right: &Resolved) -> bool {
    Resolved { profile: right.profile, ..*left } == *right
}

/// `normalizeBackgroundActivitySettings` over what the settings resolve to: the first preset
/// they match (their own profile first), else custom values over their profile.
fn matching_preset(resolved: &Resolved) -> Option<Profile> {
    [resolved.profile, Profile::Balanced, Profile::Performance, Profile::BatterySaver].into_iter().find(|profile| same_values(resolved, &preset(*profile)))
}

/// `resolveBackgroundActivityProfileOption`.
pub fn profile_option(settings: &ProviderPanelSettings) -> ProfileOption {
    match matching_preset(&settings.resolved_background_activity()) {
        Some(profile) => ProfileOption::Profile(profile),
        None => ProfileOption::Advanced,
    }
}

/// The row's description: the profile's, or the custom intervals and their shared policy.
pub fn background_description(settings: &ProviderPanelSettings) -> String {
    let resolved = settings.resolved_background_activity();
    match profile_option(settings) {
        ProfileOption::Advanced => format!("Uses custom intervals. Shared policy: {}.", profile_label(resolved.profile)),
        ProfileOption::Profile(_) => profile_description(resolved.profile).to_owned(),
    }
}

/// The reset button shows while `backgroundActivity` is not the default.
pub fn background_customized(settings: &ProviderPanelSettings) -> bool {
    let current = &settings.background_activity;
    current.profile.as_deref().is_some_and(|profile| profile != Profile::default().as_str()) || current.base_profile.is_some() || !current.overrides.is_empty()
}

/// `resetBackgroundActivitySettings`: the contract's default.
pub fn background_reset_payload() -> Value {
    json!({ "patch": { "backgroundActivity": { "schemaVersion": 1, "profile": Profile::default().as_str(), "overrides": {} } } })
}

/// `backgroundActivityProfileSettings`: a named profile, overrides dropped.
pub fn background_profile_payload(profile: Profile) -> Value {
    json!({ "patch": { "backgroundActivity": { "schemaVersion": 1, "profile": profile.as_str(), "overrides": {} } } })
}

/// The overrides that differ from `base` (`normalizeBackgroundActivitySettings`'s custom branch).
fn differing_overrides(resolved: &Resolved, base: &Resolved) -> Map<String, Value> {
    let mut overrides = Map::new();
    let mut put = |key: &str, value: Value, differs: bool| {
        if differs {
            overrides.insert(key.to_owned(), value);
        }
    };
    put("automaticGitFetchInterval", json!(resolved.automatic_git_fetch_interval), resolved.automatic_git_fetch_interval != base.automatic_git_fetch_interval);
    put("providerHealthRefreshInterval", json!(resolved.provider_health_refresh_interval), resolved.provider_health_refresh_interval != base.provider_health_refresh_interval);
    put(
        "hostPowerMonitorActiveInterval",
        json!(resolved.host_power_monitor_active_interval),
        resolved.host_power_monitor_active_interval != base.host_power_monitor_active_interval,
    );
    put("hostPowerMonitorIdleInterval", json!(resolved.host_power_monitor_idle_interval), resolved.host_power_monitor_idle_interval != base.host_power_monitor_idle_interval);
    put("pauseWhenHostLocked", json!(resolved.pause_when_host_locked), resolved.pause_when_host_locked != base.pause_when_host_locked);
    put("pauseWhenHostLowPower", json!(resolved.pause_when_host_low_power), resolved.pause_when_host_low_power != base.pause_when_host_low_power);
    put("pauseWhenClientLowPower", json!(resolved.pause_when_client_low_power), resolved.pause_when_client_low_power != base.pause_when_client_low_power);
    put("pauseWhenOnBattery", json!(resolved.pause_when_on_battery), resolved.pause_when_on_battery != base.pause_when_on_battery);
    overrides
}

/// `backgroundActivitySharedPolicySettings`: a new shared policy that keeps any custom intervals.
pub fn shared_policy_payload(settings: &ProviderPanelSettings, profile: Profile) -> Value {
    let resolved = settings.resolved_background_activity();
    let overrides = match matching_preset(&resolved) {
        Some(_) => Map::new(),
        None => differing_overrides(&resolved, &preset(resolved.profile)),
    };
    json!({ "patch": { "backgroundActivity": { "schemaVersion": 1, "profile": "custom", "baseProfile": profile.as_str(), "overrides": overrides } } })
}

/// One value the advanced dialog sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Override {
    GitFetchSeconds(u64),
    ProviderHealthSeconds(u64),
    HostActiveSeconds(u64),
    HostIdleSeconds(u64),
    Pause(PauseKey, bool),
}

/// `BACKGROUND_ACTIVITY_BOOLEAN_OVERRIDES` with their labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseKey {
    HostLocked,
    HostLowPower,
    ClientLowPower,
    OnBattery,
}

impl PauseKey {
    pub const ALL: [PauseKey; 4] = [PauseKey::HostLocked, PauseKey::HostLowPower, PauseKey::ClientLowPower, PauseKey::OnBattery];

    pub fn label(self) -> &'static str {
        match self {
            PauseKey::HostLocked => "Pause when host is locked",
            PauseKey::HostLowPower => "Pause on host low power",
            PauseKey::ClientLowPower => "Pause on client low power",
            PauseKey::OnBattery => "Pause on battery",
        }
    }

    pub fn value(self, resolved: &Resolved) -> bool {
        match self {
            PauseKey::HostLocked => resolved.pause_when_host_locked,
            PauseKey::HostLowPower => resolved.pause_when_host_low_power,
            PauseKey::ClientLowPower => resolved.pause_when_client_low_power,
            PauseKey::OnBattery => resolved.pause_when_on_battery,
        }
    }
}

/// `backgroundActivityOverrideSettings`: every resolved value pinned, then `change` applied, as a
/// custom profile over the current base profile.
pub fn override_payload(settings: &ProviderPanelSettings, change: Override) -> Value {
    let mut next = settings.resolved_background_activity();
    match change {
        Override::GitFetchSeconds(seconds) => next.automatic_git_fetch_interval = seconds * 1000,
        Override::ProviderHealthSeconds(seconds) => next.provider_health_refresh_interval = seconds * 1000,
        Override::HostActiveSeconds(seconds) => next.host_power_monitor_active_interval = seconds * 1000,
        Override::HostIdleSeconds(seconds) => next.host_power_monitor_idle_interval = seconds * 1000,
        Override::Pause(PauseKey::HostLocked, on) => next.pause_when_host_locked = on,
        Override::Pause(PauseKey::HostLowPower, on) => next.pause_when_host_low_power = on,
        Override::Pause(PauseKey::ClientLowPower, on) => next.pause_when_client_low_power = on,
        Override::Pause(PauseKey::OnBattery, on) => next.pause_when_on_battery = on,
    }
    json!({ "patch": { "backgroundActivity": {
        "schemaVersion": 1,
        "profile": "custom",
        "baseProfile": settings.background_activity.base_profile().as_str(),
        "overrides": {
            "automaticGitFetchInterval": next.automatic_git_fetch_interval,
            "providerHealthRefreshInterval": next.provider_health_refresh_interval,
            "hostPowerMonitorActiveInterval": next.host_power_monitor_active_interval,
            "hostPowerMonitorIdleInterval": next.host_power_monitor_idle_interval,
            "pauseWhenHostLocked": next.pause_when_host_locked,
            "pauseWhenHostLowPower": next.pause_when_host_low_power,
            "pauseWhenClientLowPower": next.pause_when_client_low_power,
            "pauseWhenOnBattery": next.pause_when_on_battery,
        },
    } } })
}

/// One interval field in the advanced dialog: its bounds and what it sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntervalField {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub min: u64,
    pub step: u64,
}

pub const GIT_FETCH: IntervalField =
    IntervalField { id: "git-fetch-interval", title: "Git fetch interval", description: "Refresh remote branch status in the background.", min: 0, step: 5 };
pub const PROVIDER_HEALTH: IntervalField = IntervalField {
    id: "provider-health-interval",
    title: "Provider health interval",
    description: "Refresh provider availability, versions, and auth state. Model catalog updates follow Provider update checks.",
    min: 0,
    step: 30,
};
pub const HOST_ACTIVE: IntervalField =
    IntervalField { id: "host-power-active-interval", title: "Host power monitor", description: "Poll host power state while clients are active.", min: 5, step: 5 };
pub const HOST_IDLE: IntervalField =
    IntervalField { id: "host-power-idle-interval", title: "Idle host monitor", description: "Poll host power state when no foreground client is active.", min: 5, step: 30 };
pub const INTERVAL_FIELDS: [IntervalField; 4] = [GIT_FETCH, PROVIDER_HEALTH, HOST_ACTIVE, HOST_IDLE];

impl IntervalField {
    /// The field's value in whole seconds (`durationToSeconds`).
    pub fn seconds(self, resolved: &Resolved) -> u64 {
        let ms = match self.id {
            "git-fetch-interval" => resolved.automatic_git_fetch_interval,
            "provider-health-interval" => resolved.provider_health_refresh_interval,
            "host-power-active-interval" => resolved.host_power_monitor_active_interval,
            _ => resolved.host_power_monitor_idle_interval,
        };
        (ms + 500) / 1000
    }

    pub fn change(self, seconds: u64) -> Override {
        match self.id {
            "git-fetch-interval" => Override::GitFetchSeconds(seconds),
            "provider-health-interval" => Override::ProviderHealthSeconds(seconds),
            "host-power-active-interval" => Override::HostActiveSeconds(seconds),
            _ => Override::HostIdleSeconds(seconds),
        }
    }

    /// `normalizeIntervalSeconds`: rounded and at least the field's minimum; `None` for text that
    /// is not a number, which puts the saved value back.
    pub fn normalize(self, text: &str) -> Option<u64> {
        let value = text.trim().parse::<f64>().ok().filter(|value| value.is_finite())?;
        Some(value.round().max(self.min as f64) as u64)
    }
}

/// `StorageCleanupSettings`' artifact and log retention.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Retention {
    pub browser_artifacts_after_days: Option<u32>,
    pub logs_after_days: Option<u32>,
}

impl Retention {
    pub fn of(settings: &ServerSettings) -> Retention {
        let storage = &settings.worktrees.storage_cleanup;
        Retention { browser_artifacts_after_days: storage.browser_artifacts_after_days, logs_after_days: storage.logs_after_days }
    }
}

fn worktree_rules_enabled(rules: &CleanupRules) -> bool {
    rules.worktree_after_days.is_some() || rules.worktree_settled_after_days.is_some() || rules.worktree_on_merge || rules.worktree_on_delete || rules.worktree_unchanged
}

/// `hasRules`: Delete now does something only once a cleanup rule is on, here or in a project.
pub fn has_cleanup_rules(settings: &ServerSettings) -> bool {
    let retention = Retention::of(settings);
    retention.browser_artifacts_after_days.is_some()
        || retention.logs_after_days.is_some()
        || worktree_rules_enabled(&settings.worktrees.cleanup())
        || settings.project_settings_overrides.values().any(|entry| {
            let policy = entry.raw.get("worktreeCleanup");
            policy.and_then(|policy| policy.get("mode")).and_then(Value::as_str) == Some("custom")
                && policy
                    .and_then(|policy| policy.get("rules"))
                    .and_then(|rules| serde_json::from_value::<CleanupRules>(rules.clone()).ok())
                    .is_some_and(|rules| worktree_rules_enabled(&rules))
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Worktree,
    BrowserArtifacts,
    Logs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Removed,
    Kept,
    Failed,
}

/// `StorageCleanupReportEntry`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportEntry {
    pub kind: EntryKind,
    pub outcome: Outcome,
    pub reason: String,
    pub path: Option<String>,
    pub project_path: Option<String>,
    pub thread_id: Option<String>,
    pub thread_title: Option<String>,
    pub bytes: Option<f64>,
    pub files: Option<f64>,
}

/// `StorageCleanupReport`.
#[derive(Debug, Clone, PartialEq)]
pub struct CleanupReport {
    pub finished_at: String,
    pub entries: Vec<ReportEntry>,
    pub removed: u64,
    pub kept: u64,
    pub failed: u64,
    pub omitted_count: u64,
    pub bytes_freed: f64,
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn decode_entry(value: &Value) -> Option<ReportEntry> {
    let kind = match value.get("kind")?.as_str()? {
        "worktree" => EntryKind::Worktree,
        "browser-artifacts" => EntryKind::BrowserArtifacts,
        "logs" => EntryKind::Logs,
        _ => return None,
    };
    let outcome = match value.get("outcome")?.as_str()? {
        "removed" => Outcome::Removed,
        "kept" => Outcome::Kept,
        "failed" => Outcome::Failed,
        _ => return None,
    };
    Some(ReportEntry {
        kind,
        outcome,
        reason: string(value, "reason").unwrap_or_default(),
        path: string(value, "path"),
        project_path: string(value, "projectPath"),
        thread_id: string(value, "threadId"),
        thread_title: string(value, "threadTitle"),
        bytes: value.get("bytes").and_then(Value::as_f64),
        files: value.get("files").and_then(Value::as_f64),
    })
}

/// A cleanup report, from `server.runStorageCleanup` or one item of
/// `server.getStorageCleanupReport` (null until cleanup first runs). `None` when it does not decode.
pub fn decode_report(value: &Value) -> Option<CleanupReport> {
    let counts = value.get("counts")?;
    let count = |key: &str| counts.get(key).and_then(Value::as_f64).map_or(0, |count| count.max(0.) as u64);
    Some(CleanupReport {
        finished_at: string(value, "finishedAt")?,
        entries: value.get("entries")?.as_array()?.iter().filter_map(decode_entry).collect(),
        removed: count("removed"),
        kept: count("kept"),
        failed: count("failed"),
        omitted_count: value.get("omittedCount").and_then(Value::as_f64).map_or(0, |count| count.max(0.) as u64),
        bytes_freed: value.get("bytesFreed").and_then(Value::as_f64).unwrap_or(0.),
    })
}

impl CleanupReport {
    /// "3 removed, 1 kept", with failures when there were any.
    pub fn counts(&self) -> String {
        let failed = if self.failed > 0 { format!(", {} failed", self.failed) } else { String::new() };
        format!("{} removed, {} kept{failed}", self.removed, self.kept)
    }

    /// The Delete now row's description of the last run.
    pub fn description(&self, now_ms: i64) -> String {
        let failed = match self.failed {
            0 => String::new(),
            1 => " 1 item couldn't be removed.".to_owned(),
            count => format!(" {count} items couldn't be removed."),
        };
        format!("Last run {}. {} recovered.{failed}", super::archive::relative_label(&self.finished_at, now_ms), format_bytes(self.bytes_freed))
    }

    pub fn failures(&self) -> impl Iterator<Item = &ReportEntry> {
        self.entries.iter().filter(|entry| entry.outcome == Outcome::Failed)
    }
}

/// What the Delete now row says while the last run is not in.
#[derive(Debug, Clone, PartialEq)]
pub enum LastRun {
    Loading,
    Failed,
    None,
    Report(CleanupReport),
}

impl LastRun {
    pub fn description(&self, now_ms: i64) -> String {
        match self {
            LastRun::Loading => "Loading last run\u{2026}".to_owned(),
            LastRun::Failed => "Could not load the last run.".to_owned(),
            LastRun::None => "Cleanup hasn't run yet.".to_owned(),
            LastRun::Report(report) => report.description(now_ms),
        }
    }

    pub fn report(&self) -> Option<&CleanupReport> {
        match self {
            LastRun::Report(report) => Some(report),
            _ => None,
        }
    }
}

pub const NO_RULES: &str = "Turn on a cleanup rule to use this.";

impl ReportEntry {
    /// The result row's title.
    pub fn name(&self) -> String {
        match self.kind {
            EntryKind::Worktree => {
                let name = match self.thread_id {
                    None => self.path.clone(),
                    Some(_) => self.path.as_deref().and_then(|path| path.split(['/', '\\']).rfind(|segment| !segment.is_empty())).map(str::to_owned),
                };
                name.unwrap_or_else(|| "Worktrees".to_owned())
            }
            EntryKind::Logs => "Rotated logs".to_owned(),
            EntryKind::BrowserArtifacts => "Browser artifacts".to_owned(),
        }
    }

    /// The result row's size, for removed entries and kept ones that know theirs.
    pub fn size(&self) -> Option<String> {
        if !(self.outcome == Outcome::Removed || (self.outcome == Outcome::Kept && self.bytes.is_some())) {
            return None;
        }
        let files = self.files.filter(|_| self.kind != EntryKind::Worktree).map(|files| {
            let noun = if files == 1. { "file" } else { "files" };
            format!("{} {noun}", format_count(files))
        });
        let parts: Vec<String> = files.into_iter().chain(self.bytes.map(format_bytes)).collect();
        Some(parts.join(" \u{b7} "))
    }
}

/// `Number.prototype.toFixed`: like `{:.N}`, except an exact tie rounds away from zero.
pub fn to_fixed(value: f64, digits: usize) -> String {
    let probe = format!("{:.*}", digits + 24, value.abs());
    let tail = &probe[probe.len() - 24..];
    let tie = tail.starts_with('5') && tail[1..].bytes().all(|b| b == b'0');
    let nudged = if tie { value + value.signum() * 10f64.powi(-(digits as i32) - 6) } else { value };
    format!("{nudged:.digits$}")
}

/// `formatBytes`: bytes under 1 KiB, then the unit picked after rounding.
pub fn format_bytes(value: f64) -> String {
    if value.round() < 1024. {
        return format!("{} B", value.round());
    }
    let scaled = |value: f64| {
        let digits = if value >= 100. {
            0
        } else if value >= 10. {
            1
        } else {
            2
        };
        to_fixed(value, digits)
    };
    let units = ["KB", "MB", "GB", "TB"];
    let mut next = value / 1024.;
    let mut unit = 0;
    while scaled(next).parse::<f64>().unwrap_or(0.) >= 1024. && unit < units.len() - 1 {
        next /= 1024.;
        unit += 1;
    }
    format!("{} {}", scaled(next), units[unit])
}

/// `Intl.NumberFormat` in English: whole numbers with thousands separators.
pub fn format_count(value: f64) -> String {
    let rounded = value.round();
    let digits = format!("{}", rounded.abs());
    let mut grouped = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    if rounded < 0. { format!("-{grouped}") } else { grouped }
}

/// `DatabaseStorageStatus`: reclaimable space worth suggesting `t3 doctor` for.
#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseStorage {
    pub reclaimable_bytes: f64,
    pub compact_command: String,
}

/// `DATABASE_COMPACTION_RECOMMENDED_BYTES`.
const COMPACTION_RECOMMENDED_BYTES: f64 = 1024. * 1024. * 1024.;

/// An item of `server.getDatabaseStorage`, kept only when `shouldRecommendDatabaseCompaction`.
pub fn compaction_recommendation(value: &Value) -> Option<DatabaseStorage> {
    let reclaimable = value.get("reclaimableBytes")?.as_f64()?;
    let incremental = value.get("incrementalVacuumEnabled")?.as_bool()?;
    (!incremental && reclaimable >= COMPACTION_RECOMMENDED_BYTES)
        .then(|| DatabaseStorage { reclaimable_bytes: reclaimable, compact_command: string(value, "compactCommand").unwrap_or_default() })
}

impl DatabaseStorage {
    pub fn title(&self, environment: &str) -> String {
        format!("{} GiB on {environment}", to_fixed(self.reclaimable_bytes / COMPACTION_RECOMMENDED_BYTES, 1))
    }
}

pub const DATABASE_DESCRIPTION: &str = "Old history is cleaned up automatically, but these databases still hold the freed space. Run the command on each machine to return it to disk; it pauses and restarts the background service. Quit the desktop app first if it hosts the environment.";

#[cfg(test)]
mod tests;
