//! The provider-level server settings under Providers → Advanced: the health check interval
//! (`resolveServerBackgroundActivitySettings` and its presets in
//! `shared/src/backgroundActivitySettings.ts`, `backgroundActivityOverrideSettings` in
//! `web/src/components/settings/SettingsPanels.logic.ts`) and "Compact idle threads"
//! (`globalIdleCompaction` and `idleCompactionPatch` in `contracts/src/settings.ts`). Durations are
//! milliseconds on the wire (`Schema.DurationFromMillis`).

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

/// `PROVIDER_HEALTH_INTERVAL_STEP_SECONDS`.
pub const HEALTH_INTERVAL_STEP_SECONDS: u64 = 30;

const DEFAULT_GIT_FETCH_MS: u64 = 30_000;
const DEFAULT_HEALTH_MS: u64 = 300_000;

/// `BackgroundActivityProfile`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Profile {
    #[default]
    Balanced,
    Performance,
    BatterySaver,
}

impl Profile {
    fn parse(value: &str) -> Option<Profile> {
        match value {
            "balanced" => Some(Profile::Balanced),
            "performance" => Some(Profile::Performance),
            "battery-saver" => Some(Profile::BatterySaver),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Balanced => "balanced",
            Profile::Performance => "performance",
            Profile::BatterySaver => "battery-saver",
        }
    }
}

/// `ResolvedBackgroundActivitySettings`, durations in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub profile: Profile,
    pub automatic_git_fetch_interval: u64,
    pub provider_health_refresh_interval: u64,
    pub host_power_monitor_active_interval: u64,
    pub host_power_monitor_idle_interval: u64,
    pub pause_when_host_locked: bool,
    pub pause_when_host_low_power: bool,
    pub pause_when_client_low_power: bool,
    pub pause_when_on_battery: bool,
}

/// `getBackgroundActivityPresetSettings`.
pub fn preset(profile: Profile) -> Resolved {
    match profile {
        Profile::Performance => Resolved {
            profile,
            automatic_git_fetch_interval: 15_000,
            provider_health_refresh_interval: 60_000,
            host_power_monitor_active_interval: 30_000,
            host_power_monitor_idle_interval: 120_000,
            pause_when_host_locked: true,
            pause_when_host_low_power: false,
            pause_when_client_low_power: false,
            pause_when_on_battery: false,
        },
        Profile::Balanced => Resolved {
            profile,
            automatic_git_fetch_interval: DEFAULT_GIT_FETCH_MS,
            provider_health_refresh_interval: DEFAULT_HEALTH_MS,
            host_power_monitor_active_interval: 30_000,
            host_power_monitor_idle_interval: 300_000,
            pause_when_host_locked: true,
            pause_when_host_low_power: true,
            pause_when_client_low_power: true,
            pause_when_on_battery: false,
        },
        Profile::BatterySaver => Resolved {
            profile,
            automatic_git_fetch_interval: 0,
            provider_health_refresh_interval: 900_000,
            host_power_monitor_active_interval: 60_000,
            host_power_monitor_idle_interval: 600_000,
            pause_when_host_locked: true,
            pause_when_host_low_power: true,
            pause_when_client_low_power: true,
            pause_when_on_battery: true,
        },
    }
}

/// `BackgroundActivitySettings`. Overrides stay a raw map; only the known keys are read.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BackgroundActivity {
    /// A profile, or `custom`; absent means the default profile.
    #[serde(deserialize_with = "lenient")]
    pub profile: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub base_profile: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub overrides: Map<String, Value>,
}

impl BackgroundActivity {
    fn is_custom(&self) -> bool {
        self.profile.as_deref() == Some("custom")
    }

    /// `getBackgroundActivityBaseProfile`.
    pub fn base_profile(&self) -> Profile {
        let named = if self.is_custom() { self.base_profile.as_deref() } else { self.profile.as_deref() };
        named.and_then(Profile::parse).unwrap_or_default()
    }

    fn override_ms(&self, key: &str) -> Option<u64> {
        self.overrides.get(key).and_then(Value::as_f64).filter(|ms| *ms >= 0.).map(|ms| ms.round() as u64)
    }

    fn override_bool(&self, key: &str) -> Option<bool> {
        self.overrides.get(key).and_then(Value::as_bool)
    }

    /// `resolveBackgroundActivitySettings`.
    pub fn resolve(&self) -> Resolved {
        let base = preset(self.base_profile());
        if !self.is_custom() {
            return base;
        }
        Resolved {
            profile: base.profile,
            automatic_git_fetch_interval: self.override_ms("automaticGitFetchInterval").unwrap_or(base.automatic_git_fetch_interval),
            provider_health_refresh_interval: self
                .override_ms("providerHealthRefreshInterval")
                .unwrap_or(base.provider_health_refresh_interval),
            host_power_monitor_active_interval: self
                .override_ms("hostPowerMonitorActiveInterval")
                .unwrap_or(base.host_power_monitor_active_interval),
            host_power_monitor_idle_interval: self
                .override_ms("hostPowerMonitorIdleInterval")
                .unwrap_or(base.host_power_monitor_idle_interval),
            pause_when_host_locked: self.override_bool("pauseWhenHostLocked").unwrap_or(base.pause_when_host_locked),
            pause_when_host_low_power: self.override_bool("pauseWhenHostLowPower").unwrap_or(base.pause_when_host_low_power),
            pause_when_client_low_power: self.override_bool("pauseWhenClientLowPower").unwrap_or(base.pause_when_client_low_power),
            pause_when_on_battery: self.override_bool("pauseWhenOnBattery").unwrap_or(base.pause_when_on_battery),
        }
    }
}

/// `IDLE_COMPACTION_MODES`, in the web's order with its labels.
pub const IDLE_COMPACTION_MODES: [(&str, &str); 3] =
    [("onReturn", "When you return"), ("beforeCacheExpires", "Before the cache expires (beta)"), ("off", "Never")];

/// The `ServerSettings` keys Providers → Advanced reads. Absent or invalid keys take the
/// contract's defaults one at a time.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderPanelSettings {
    #[serde(deserialize_with = "lenient")]
    pub background_activity: BackgroundActivity,
    /// The legacy fields from before `backgroundActivity`, still read when it is unset.
    #[serde(deserialize_with = "lenient")]
    pub automatic_git_fetch_interval: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    pub provider_health_refresh_interval: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    pub background_activity_profile: Option<String>,
    /// A boolean on servers from before idle compaction modes, else a mode.
    #[serde(deserialize_with = "lenient")]
    pub compact_idle_threads: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    pub idle_compaction: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub text_generation_model_selection: Option<Value>,
}

fn ms(value: Option<f64>, default: u64) -> u64 {
    value.filter(|ms| ms.is_finite() && *ms >= 0.).map_or(default, |ms| ms.round() as u64)
}

impl ProviderPanelSettings {
    /// The instance that writes titles and commit messages; the contract's default is Codex.
    pub fn text_generation_instance(&self) -> &str {
        self.text_generation_model_selection.as_ref().and_then(|selection| selection.get("instanceId")).and_then(Value::as_str).unwrap_or("codex")
    }

    /// `resolveServerBackgroundActivitySettings`: a default `backgroundActivity` defers to the
    /// legacy profile and intervals when those were changed.
    pub fn resolved_background_activity(&self) -> Resolved {
        let current = &self.background_activity;
        let is_default = current.profile.as_deref().is_none_or(|profile| profile == Profile::default().as_str())
            && current.base_profile.is_none()
            && current.overrides.is_empty();
        let legacy = self.background_activity_profile.as_deref().and_then(Profile::parse).unwrap_or_default();
        let git = ms(self.automatic_git_fetch_interval, DEFAULT_GIT_FETCH_MS);
        let health = ms(self.provider_health_refresh_interval, DEFAULT_HEALTH_MS);
        let has_legacy = legacy != Profile::default() || git != DEFAULT_GIT_FETCH_MS || health != DEFAULT_HEALTH_MS;
        if !(is_default && has_legacy) {
            return current.resolve();
        }
        let legacy_preset = preset(legacy);
        let mut overrides = Map::new();
        if git != legacy_preset.automatic_git_fetch_interval {
            overrides.insert("automaticGitFetchInterval".into(), json!(git));
        }
        if health != legacy_preset.provider_health_refresh_interval {
            overrides.insert("providerHealthRefreshInterval".into(), json!(health));
        }
        let profile = if overrides.is_empty() { legacy.as_str() } else { "custom" };
        BackgroundActivity { profile: Some(profile.into()), base_profile: Some(legacy.as_str().into()), overrides }.resolve()
    }

    /// The health check interval in whole seconds (`durationToSeconds`).
    pub fn health_interval_seconds(&self) -> u64 {
        seconds(self.resolved_background_activity().provider_health_refresh_interval)
    }

    /// The interval the profile gives when none is set; the reset button shows while it differs.
    pub fn default_health_interval_seconds(&self) -> u64 {
        seconds(preset(self.resolved_background_activity().profile).provider_health_refresh_interval)
    }

    /// `backgroundActivityOverrideSettings` with `providerHealthRefreshInterval` set, or removed
    /// to reset it: every other resolved value is pinned so nothing else moves.
    pub fn health_interval_payload(&self, seconds: Option<u64>) -> Value {
        let resolved = self.resolved_background_activity();
        let mut overrides = Map::new();
        overrides.insert("automaticGitFetchInterval".into(), json!(resolved.automatic_git_fetch_interval));
        if let Some(seconds) = seconds {
            overrides.insert("providerHealthRefreshInterval".into(), json!(seconds * 1000));
        }
        overrides.insert("hostPowerMonitorActiveInterval".into(), json!(resolved.host_power_monitor_active_interval));
        overrides.insert("hostPowerMonitorIdleInterval".into(), json!(resolved.host_power_monitor_idle_interval));
        overrides.insert("pauseWhenHostLocked".into(), json!(resolved.pause_when_host_locked));
        overrides.insert("pauseWhenHostLowPower".into(), json!(resolved.pause_when_host_low_power));
        overrides.insert("pauseWhenClientLowPower".into(), json!(resolved.pause_when_client_low_power));
        overrides.insert("pauseWhenOnBattery".into(), json!(resolved.pause_when_on_battery));
        json!({ "patch": { "backgroundActivity": {
            "schemaVersion": 1,
            "profile": "custom",
            "baseProfile": self.background_activity.base_profile().as_str(),
            "overrides": overrides,
        } } })
    }

    /// `globalIdleCompaction`.
    pub fn idle_compaction(&self) -> &'static str {
        if let Some(mode) = self.idle_compaction.as_deref().and_then(known_mode) {
            return mode;
        }
        match &self.compact_idle_threads {
            Some(Value::String(mode)) => known_mode(mode).unwrap_or("onReturn"),
            Some(Value::Bool(true)) => "beforeCacheExpires",
            _ => "onReturn",
        }
    }

    /// `idleCompactionPatch`: the mode in whichever field the server shows it reads, or the
    /// boolean switch for a server from before modes.
    pub fn idle_compaction_payload(&self, mode: &str) -> Value {
        let patch = if self.idle_compaction.as_deref().and_then(known_mode).is_some() {
            json!({ "idleCompaction": mode })
        } else if matches!(self.compact_idle_threads, Some(Value::String(_))) {
            json!({ "compactIdleThreads": mode })
        } else {
            json!({ "compactIdleThreads": mode == "beforeCacheExpires" })
        };
        json!({ "patch": patch })
    }
}

fn known_mode(mode: &str) -> Option<&'static str> {
    IDLE_COMPACTION_MODES.iter().map(|(value, _)| *value).find(|value| *value == mode)
}

fn seconds(ms: u64) -> u64 {
    (ms + 500) / 1000
}

fn lenient<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(d)?).unwrap_or_default())
}
