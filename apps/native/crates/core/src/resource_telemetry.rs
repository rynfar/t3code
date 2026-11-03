//! Wire and view model ported from `contracts/src/resourceTelemetry.ts` and
//! `web/src/components/settings/ResourceTelemetryDiagnostics{,.logic}.tsx`.

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub const SUBSCRIBE: &str = "subscribeResourceTelemetry";
pub const HISTORY: &str = "server.getResourceTelemetryHistory";
pub const RETRY: &str = "server.retryResourceTelemetry";

macro_rules! literals {
    ($name:ident { $($variant:ident => $wire:literal),* $(,)? }) => {
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant,)*
            #[default]
            #[serde(other)]
            Unknown,
        }
        impl $name {
            pub fn label(self) -> &'static str {
                match self { $(Self::$variant => $wire,)* Self::Unknown => "unknown" }
            }
        }
    };
}

literals!(Category {
    Server => "server", ServerChild => "server-child", ProviderRoot => "provider-root",
    TerminalRoot => "terminal-root", ElectronMain => "electron-main", ElectronRenderer => "electron-renderer",
    ElectronGpu => "electron-gpu", ElectronUtility => "electron-utility", ResourceMonitor => "resource-monitor", UnknownT3 => "unknown-t3"
});
literals!(IoSemantics { Storage => "storage", Logical => "logical", AllIo => "all-io", Unavailable => "unavailable" });
literals!(SourceStatus { Starting => "starting", Healthy => "healthy", Degraded => "degraded", Unavailable => "unavailable", Stopped => "stopped" });
literals!(BooleanState { True => "true", False => "false" });
literals!(ThermalState { Nominal => "nominal", Fair => "fair", Serious => "serious", Critical => "critical" });
literals!(PowerSource { MacosShell => "node-macos-shell", MacosNative => "node-macos-native", Linux => "node-linux", Windows => "node-windows", Electron => "electron-main" });
literals!(ElectronType { Browser => "Browser", Tab => "Tab", Utility => "Utility", Zygote => "Zygote", SandboxHelper => "Sandbox helper", Gpu => "GPU", PepperPlugin => "Pepper Plugin", PepperPluginBroker => "Pepper Plugin Broker" });

fn effect_option<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(deserializer: D) -> Result<Option<T>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    let value = match value.get("_tag").and_then(Value::as_str) {
        Some("Some") => value.get("value").cloned().unwrap_or(Value::Null),
        Some(_) => Value::Null,
        None => value,
    };
    if value.is_null() { Ok(None) } else { serde_json::from_value(value).map(Some).map_err(serde::de::Error::custom) }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub pid: u64,
    pub start_time_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Process {
    pub identity: Identity,
    pub ppid: u64,
    pub child_pids: Vec<u64>,
    pub depth: usize,
    pub name: String,
    pub command: String,
    pub status: String,
    pub category: Category,
    pub electron_type: Option<ElectronType>,
    pub electron_service_name: Option<String>,
    pub cpu_percent: f64,
    pub cpu_time_ms: f64,
    pub resident_bytes: f64,
    pub peak_resident_bytes: f64,
    pub virtual_bytes: f64,
    pub io_read_bytes: f64,
    pub io_write_bytes: f64,
    pub io_read_bytes_per_second: f64,
    pub io_write_bytes_per_second: f64,
    pub io_semantics: IoSemantics,
    pub idle_wakeups_per_second: Option<f64>,
    pub run_time_ms: f64,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Aggregate {
    pub process_count: u64,
    pub current_cpu_percent: f64,
    pub cpu_time_ms: f64,
    pub current_rss_bytes: f64,
    pub peak_rss_bytes: f64,
    pub io_read_bytes: f64,
    pub io_write_bytes: f64,
    pub io_read_bytes_per_second: f64,
    pub io_write_bytes_per_second: f64,
    pub process_starts: u64,
    pub process_exits: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Groups {
    pub backend: Aggregate,
    pub electron: Aggregate,
    pub monitor: Aggregate,
    pub all_t3: Aggregate,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SourceHealth {
    pub status: SourceStatus,
    #[serde(deserialize_with = "effect_option")]
    pub last_sample_at: Option<String>,
    #[serde(deserialize_with = "effect_option")]
    pub last_error: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Health {
    pub native: SourceHealth,
    pub desktop: SourceHealth,
    #[serde(deserialize_with = "effect_option")]
    pub sidecar_version: Option<String>,
    #[serde(deserialize_with = "effect_option")]
    pub sidecar_pid: Option<u64>,
    pub restart_count: u64,
    pub collection_duration_micros: f64,
    pub scanned_process_count: u64,
    pub retained_process_count: u64,
    pub inaccessible_process_count: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Power {
    pub source: PowerSource,
    pub idle: BooleanState,
    pub idle_seconds: Option<f64>,
    pub locked: BooleanState,
    pub suspended: bool,
    pub on_battery: BooleanState,
    pub low_power_mode: BooleanState,
    pub thermal_state: ThermalState,
    pub stale: bool,
    pub updated_at: String,
}
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AttributionEntry {
    pub component: String,
    pub operation: String,
    pub logical_read_bytes: f64,
    pub logical_write_bytes: f64,
    pub count: u64,
    pub duration_ms: f64,
}
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Attribution {
    pub read_at: String,
    pub entries: Vec<AttributionEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub read_at: String,
    pub sample_interval_ms: u64,
    pub processes: Vec<Process>,
    pub groups: Groups,
    pub power: Power,
    #[serde(deserialize_with = "effect_option")]
    pub speed_limit_percent: Option<f64>,
    pub attribution: Attribution,
    pub health: Health,
}
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Bucket {
    pub started_at: String,
    pub ended_at: String,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
    pub max_rss_bytes: f64,
    pub io_read_bytes: f64,
    pub io_write_bytes: f64,
    pub max_process_count: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProcessSummary {
    pub identity: Identity,
    pub ppid: u64,
    pub depth: usize,
    pub name: String,
    pub command: String,
    pub category: Category,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub current_cpu_percent: f64,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
    pub cpu_time_ms: f64,
    pub current_rss_bytes: f64,
    pub peak_rss_bytes: f64,
    pub io_read_bytes: f64,
    pub io_write_bytes: f64,
    pub io_semantics: IoSemantics,
    pub sample_count: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct History {
    pub read_at: String,
    pub window_ms: u64,
    pub bucket_ms: u64,
    pub sample_interval_ms: u64,
    pub retained_sample_count: u64,
    pub buckets: Vec<Bucket>,
    pub top_processes: Vec<ProcessSummary>,
    pub health: Health,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RetryResult {
    pub accepted: bool,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, Copy)]
pub struct HistoryWindow {
    pub label: &'static str,
    pub window_ms: u64,
    pub bucket_ms: u64,
}
pub const HISTORY_WINDOWS: [HistoryWindow; 4] = [
    HistoryWindow { label: "5m", window_ms: 300_000, bucket_ms: 15_000 },
    HistoryWindow { label: "15m", window_ms: 900_000, bucket_ms: 30_000 },
    HistoryWindow { label: "30m", window_ms: 1_800_000, bucket_ms: 60_000 },
    HistoryWindow { label: "1h", window_ms: 3_600_000, bucket_ms: 120_000 },
];
impl HistoryWindow {
    pub fn payload(self) -> Value {
        json!({"windowMs": self.window_ms, "bucketMs": self.bucket_ms})
    }
}

#[derive(Debug)]
pub struct HistoryState {
    pub query: crate::settings::diagnostics::Query<std::sync::Arc<History>>,
    pub selected: usize,
}
impl Default for HistoryState {
    fn default() -> Self {
        Self { query: Default::default(), selected: 1 }
    }
}
impl HistoryState {
    pub fn select(&mut self, selected: usize) {
        if self.selected != selected {
            self.query.reset();
            self.selected = selected;
        }
    }
    pub fn reset(&mut self) {
        self.query.reset();
        self.selected = 1;
    }
}

pub fn visible_processes(processes: &[Process], collapsed: &HashSet<Identity>) -> Vec<usize> {
    let mut children: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, p) in processes.iter().enumerate() {
        children.entry(p.ppid).or_default().push(i);
    }
    let mut hidden = HashSet::new();
    let mut pending: Vec<u64> = processes.iter().filter(|p| collapsed.contains(&p.identity)).map(|p| p.identity.pid).collect();
    while let Some(pid) = pending.pop() {
        for &i in children.get(&pid).into_iter().flatten() {
            if hidden.insert(i) {
                pending.push(processes[i].identity.pid);
            }
        }
    }
    (0..processes.len()).filter(|i| !hidden.contains(i)).collect()
}

pub fn should_show_retry(status: Option<SourceStatus>, error: Option<&str>) -> bool {
    matches!(status, Some(SourceStatus::Degraded | SourceStatus::Unavailable | SourceStatus::Stopped)) || (status.is_none() && error.is_some())
}
pub fn history_bar_height(value: f64, max: f64, minimum: f64) -> f64 {
    if value <= 0. { 0. } else { minimum.max(value / max.max(1.) * 100.) }
}
pub fn history_cpu_scale_max(buckets: &[Bucket]) -> f64 {
    buckets.iter().fold(1_f64, |max, b| max.max(b.avg_cpu_percent))
}
pub fn category_label(category: Category) -> &'static str {
    match category {
        Category::Server => "Server",
        Category::ServerChild => "Backend child",
        Category::ProviderRoot => "Provider",
        Category::TerminalRoot => "Terminal",
        Category::ElectronMain => "Electron main",
        Category::ElectronRenderer => "Renderer",
        Category::ElectronGpu => "GPU",
        Category::ElectronUtility => "Electron utility",
        Category::ResourceMonitor => "Monitor",
        Category::UnknownT3 => "T3 process",
        Category::Unknown => "Unknown",
    }
}
pub fn io_label(io: IoSemantics) -> &'static str {
    match io {
        IoSemantics::Storage => "Storage bytes",
        IoSemantics::Logical => "Logical bytes",
        IoSemantics::AllIo => "All I/O bytes",
        IoSemantics::Unavailable => "Unavailable",
        IoSemantics::Unknown => "Unknown",
    }
}
pub fn process_name(process: &Process) -> String {
    if !process.name.trim().is_empty() {
        return process.name.clone();
    }
    let token = process.command.split_whitespace().next().unwrap_or_default().trim_matches(['\'', '"']);
    token.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(token).to_owned()
}
fn fixed(value: f64, digits: usize) -> String {
    let result = format!("{value:.digits$}");
    if result.split_once('.').is_some_and(|(_, rest)| rest.chars().all(|c| c == '0')) { result.split('.').next().unwrap_or_default().to_owned() } else { result }
}
pub fn cpu_time(ms: f64) -> String {
    let seconds = ms / 1000.;
    if seconds < 60. {
        format!("{}s", fixed(seconds, if seconds >= 10. { 1 } else { 2 }))
    } else if seconds < 3600. {
        let minutes = seconds / 60.;
        format!("{}m", fixed(minutes, if minutes >= 10. { 1 } else { 2 }))
    } else {
        format!("{}h", fixed(seconds / 3600., 2))
    }
}
pub fn attribution_time(ms: f64) -> String {
    format!("{}s", fixed(ms / 1000., 2))
}
pub fn duration_micros(value: f64) -> String {
    if value < 1000. {
        format!("{} µs", value.round())
    } else if value < 1_000_000. {
        format!("{} ms", fixed(value / 1000., 2))
    } else {
        format!("{} s", fixed(value / 1_000_000., 2))
    }
}
pub fn sample_interval(ms: u64) -> String {
    if ms < 1000 { format!("{ms} ms") } else { format!("{} {}", fixed(ms as f64 / 1000., 1), if ms == 1000 { "second" } else { "seconds" }) }
}
pub fn bytes(value: f64) -> String {
    if value.round() < 1024. {
        return format!("{} B", value.round());
    }
    let mut next = value / 1024.;
    let units = ["KB", "MB", "GB", "TB"];
    let mut i = 0;
    let digits = |n: f64| {
        if n >= 100. {
            0
        } else if n >= 10. {
            1
        } else {
            2
        }
    };
    while format!("{:.prec$}", next, prec = digits(next)).parse::<f64>().unwrap_or(0.) >= 1024. && i < 3 {
        next /= 1024.;
        i += 1;
    }
    format!("{:.prec$} {}", next, units[i], prec = digits(next))
}
pub fn rate(value: f64) -> String {
    format!("{}/s", bytes(value))
}
pub fn boolean_label(state: BooleanState, yes: &'static str, no: &'static str) -> &'static str {
    match state {
        BooleanState::True => yes,
        BooleanState::False => no,
        BooleanState::Unknown => "Unknown",
    }
}

#[cfg(test)]
mod tests;
