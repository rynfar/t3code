//! Diagnostics: `DiagnosticsSettings.tsx` (live processes and their signals, resource history,
//! trace diagnostics and its tables) and `TelemetryExportSettings.tsx` (the OTLP endpoints). Wire
//! shapes are `ServerProcessDiagnosticsResult`, `ServerProcessResourceHistoryResult`,
//! `ServerTraceDiagnosticsResult`, `ServerSignalProcessResult`, `OtlpEndpointCheckResult` and
//! `ServerObservability` in `contracts/src/server.ts`; dates are ISO strings and an Effect
//! `Option` is `{"_tag":"Some","value":…}`.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::archive::relative_label;
use super::maintenance::to_fixed;
use crate::sidebar::time::parse_iso;

pub const GET_PROCESS_DIAGNOSTICS: &str = "server.getProcessDiagnostics";
pub const GET_PROCESS_RESOURCE_HISTORY: &str = "server.getProcessResourceHistory";
pub const GET_TRACE_DIAGNOSTICS: &str = "server.getTraceDiagnostics";
pub const SIGNAL_PROCESS: &str = "server.signalProcess";
pub const CHECK_OTLP_ENDPOINT: &str = "server.checkOtlpEndpoint";
/// Signals need it (`signalServerProcess`).
pub const ENVIRONMENT_MAINTAIN_SCOPE: &str = "environment:maintain";

fn option_value(value: Option<&Value>) -> Option<&Value> {
    let value = value?;
    match value.get("_tag").and_then(Value::as_str) {
        Some("Some") => value.get("value"),
        Some(_) => None,
        None => (!value.is_null()).then_some(value),
    }
}

fn option_message(value: Option<&Value>) -> Option<String> {
    option_value(value)?.get("message")?.as_str().map(str::to_owned)
}

fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.)
}

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn items<T>(value: &Value, key: &str, decode: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    value.get(key).and_then(Value::as_array).map(|items| items.iter().filter_map(decode).collect()).unwrap_or_default()
}

/// `ServerProcessDiagnosticsEntry`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessEntry {
    pub pid: u64,
    pub start_time_ms: u64,
    pub cpu_percent: f64,
    pub rss_bytes: f64,
    pub command: String,
    pub depth: usize,
    pub has_children: bool,
}

/// `ServerProcessDiagnosticsResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessDiagnostics {
    pub server_pid: u64,
    pub read_at: String,
    pub process_count: f64,
    pub total_rss_bytes: f64,
    pub total_cpu_percent: f64,
    pub processes: Vec<ProcessEntry>,
    pub error: Option<String>,
}

pub fn decode_process_diagnostics(value: &Value) -> Option<ProcessDiagnostics> {
    Some(ProcessDiagnostics {
        server_pid: value.get("serverPid")?.as_u64()?,
        read_at: text(value, "readAt"),
        process_count: number(value, "processCount"),
        total_rss_bytes: number(value, "totalRssBytes"),
        total_cpu_percent: number(value, "totalCpuPercent"),
        processes: items(value, "processes", |entry| {
            Some(ProcessEntry {
                pid: entry.get("pid")?.as_u64()?,
                start_time_ms: entry.get("startTimeMs")?.as_u64()?,
                cpu_percent: number(entry, "cpuPercent"),
                rss_bytes: number(entry, "rssBytes"),
                command: text(entry, "command"),
                depth: entry.get("depth").and_then(Value::as_u64).unwrap_or(0) as usize,
                has_children: entry.get("childPids").and_then(Value::as_array).is_some_and(|children| !children.is_empty()),
            })
        }),
        error: option_message(value.get("error")),
    })
}

/// `visibleProcessTree`: rows under a collapsed process are hidden, in the server's order.
pub fn visible_process_tree(processes: &[ProcessEntry], collapsed: &HashSet<u64>) -> Vec<usize> {
    let mut visible = Vec::new();
    let mut hidden_below: Option<usize> = None;
    for (index, process) in processes.iter().enumerate() {
        if let Some(depth) = hidden_below {
            if process.depth > depth {
                continue;
            }
            hidden_below = None;
        }
        visible.push(index);
        if collapsed.contains(&process.pid) {
            hidden_below = Some(process.depth);
        }
    }
    visible
}

/// `formatProcessName`: the executable's file name.
pub fn process_name(command: &str) -> String {
    let Some(first) = command.split_whitespace().next() else {
        return command.to_owned();
    };
    let unquoted = first.strip_prefix(['\'', '"']).unwrap_or(first);
    let unquoted = unquoted.strip_suffix(['\'', '"']).unwrap_or(unquoted);
    unquoted.split(['/', '\\']).rfind(|segment| !segment.is_empty()).unwrap_or(unquoted).to_owned()
}

/// `formatShortProcessName`.
pub fn short_process_name(command: &str) -> String {
    let name = process_name(command);
    if name.chars().count() > 42 { format!("{}...", name.chars().take(39).collect::<String>()) } else { name }
}

/// `formatProcessType`: `\b(codex|claude|opencode|cursor)\b`, case-insensitive.
pub fn process_type(process: &ProcessEntry) -> &'static str {
    if process.depth > 0 {
        return "Subprocess";
    }
    let lower = process.command.to_lowercase();
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let agent = ["codex", "claude", "opencode", "cursor"]
        .iter()
        .any(|name| lower.match_indices(name).any(|(start, _)| !word(lower[..start].chars().next_back()) && !word(lower[start + name.len()..].chars().next())));
    if agent { "Agent" } else { "Process" }
}

/// One period the resource history offers (`RESOURCE_HISTORY_WINDOWS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryWindow {
    pub label: &'static str,
    pub window_ms: u64,
    pub bucket_ms: u64,
}

pub const HISTORY_WINDOWS: [HistoryWindow; 4] = [
    HistoryWindow { label: "5m", window_ms: 5 * 60_000, bucket_ms: 30_000 },
    HistoryWindow { label: "15m", window_ms: 15 * 60_000, bucket_ms: 60_000 },
    HistoryWindow { label: "30m", window_ms: 30 * 60_000, bucket_ms: 2 * 60_000 },
    HistoryWindow { label: "1h", window_ms: 60 * 60_000, bucket_ms: 5 * 60_000 },
];
/// The page opens on 15 minutes.
pub const DEFAULT_HISTORY_WINDOW: usize = 1;

impl HistoryWindow {
    pub fn payload(self) -> Value {
        json!({ "windowMs": self.window_ms, "bucketMs": self.bucket_ms })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistoryBucket {
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
}

/// `ServerProcessResourceHistorySummary`.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryProcess {
    pub process_key: String,
    pub pid: u64,
    pub command: String,
    pub depth: usize,
    pub is_server_root: bool,
    pub current_cpu_percent: f64,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
    pub cpu_seconds_approx: f64,
    pub max_rss_bytes: f64,
}

/// `ServerProcessResourceHistoryResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceHistory {
    pub read_at: String,
    pub sample_interval_ms: f64,
    pub retained_sample_count: f64,
    pub total_cpu_seconds_approx: f64,
    pub buckets: Vec<HistoryBucket>,
    pub top_processes: Vec<HistoryProcess>,
    pub error: Option<String>,
}

pub fn decode_resource_history(value: &Value) -> Option<ResourceHistory> {
    Some(ResourceHistory {
        read_at: value.get("readAt")?.as_str()?.to_owned(),
        sample_interval_ms: number(value, "sampleIntervalMs"),
        retained_sample_count: number(value, "retainedSampleCount"),
        total_cpu_seconds_approx: number(value, "totalCpuSecondsApprox"),
        buckets: items(value, "buckets", |bucket| {
            Some(HistoryBucket { avg_cpu_percent: bucket.get("avgCpuPercent")?.as_f64()?, max_cpu_percent: bucket.get("maxCpuPercent")?.as_f64()? })
        }),
        top_processes: items(value, "topProcesses", |process| {
            Some(HistoryProcess {
                process_key: process.get("processKey")?.as_str()?.to_owned(),
                pid: process.get("pid")?.as_u64()?,
                command: text(process, "command"),
                depth: process.get("depth").and_then(Value::as_u64).unwrap_or(0) as usize,
                is_server_root: process.get("isServerRoot").and_then(Value::as_bool).unwrap_or(false),
                current_cpu_percent: number(process, "currentCpuPercent"),
                avg_cpu_percent: number(process, "avgCpuPercent"),
                max_cpu_percent: number(process, "maxCpuPercent"),
                cpu_seconds_approx: number(process, "cpuSecondsApprox"),
                max_rss_bytes: number(process, "maxRssBytes"),
            })
        }),
        error: option_message(value.get("error")),
    })
}

impl ResourceHistory {
    /// The history table indents children under the server root from the shallowest one.
    pub fn visual_depth(&self, process: &HistoryProcess) -> usize {
        let shallowest = self.top_processes.iter().filter(|process| !process.is_server_root).map(|process| process.depth).min();
        match shallowest {
            Some(shallowest) if !process.is_server_root => (process.depth + 1).saturating_sub(shallowest).max(1),
            _ => 0,
        }
    }

    /// Each bucket's peak and average bar as a percentage of the chart's height (at least 2).
    pub fn bars(&self) -> Vec<(f64, f64)> {
        let max = self.buckets.iter().map(|bucket| bucket.max_cpu_percent).fold(1., f64::max);
        self.buckets.iter().map(|bucket| ((bucket.max_cpu_percent / max * 100.).max(2.), (bucket.avg_cpu_percent / max * 100.).max(2.))).collect()
    }

    pub fn interval_label(&self) -> String {
        if self.sample_interval_ms == 0. { "On demand".to_owned() } else { format_duration(self.sample_interval_ms) }
    }
}

/// `trimTrailingZeros` of `toFixed`: `"2.50"` stays, `"3.00"` becomes `"3"`.
fn fixed(value: f64, digits: usize) -> String {
    let text = to_fixed(value, digits);
    match text.split_once('.') {
        Some((whole, fraction)) if fraction.chars().all(|c| c == '0') => whole.to_owned(),
        _ => text,
    }
}

/// `formatDuration`: milliseconds under a second, then seconds.
pub fn format_duration(ms: f64) -> String {
    if ms < 1000. {
        return format!("{} ms", ms.round());
    }
    format!("{} s", fixed(ms / 1000., if ms >= 10_000. { 1 } else { 2 }))
}

/// `formatCpuTime`: seconds, minutes, then hours.
pub fn format_cpu_time(seconds: f64) -> String {
    if seconds < 60. {
        return format!("{}s", fixed(seconds, if seconds >= 10. { 1 } else { 2 }));
    }
    let minutes = seconds / 60.;
    if minutes < 60. {
        return format!("{}m", fixed(minutes, if minutes >= 10. { 1 } else { 2 }));
    }
    format!("{}h", fixed(minutes / 60., 2))
}

pub fn percent(value: f64) -> String {
    format!("{}%", to_fixed(value, 1))
}

/// `shortenTraceId`.
pub fn shorten_trace_id(trace_id: &str) -> String {
    if trace_id.len() <= 32 {
        return trace_id.to_owned();
    }
    format!("{}...{}", &trace_id[..18], &trace_id[trace_id.len() - 10..])
}

/// `ExpandableText`'s `canExpand` for text shown whole by default.
pub fn can_expand(text: &str) -> bool {
    text.chars().count() > 180 || text.contains('\n')
}

/// `RelativeTimeNoWrap`: "5m\u{a0}ago" or "just now", nothing for a date that does not parse.
pub fn relative_no_wrap(iso: &str, now_ms: i64) -> String {
    if parse_iso(iso).is_none() {
        return String::new();
    }
    relative_label(iso, now_ms).replace(" ago", "\u{a0}ago")
}

/// `DiagnosticsLastChecked`.
pub fn last_checked(read_at: Option<&str>, now_ms: i64) -> String {
    match read_at {
        None => "Checking".to_owned(),
        Some(iso) if parse_iso(iso).is_none() => "Checked unavailable".to_owned(),
        Some(iso) => format!("Checked {}", relative_label(iso, now_ms)),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpanSummary {
    pub name: String,
    pub count: f64,
    pub failure_count: f64,
    pub average_duration_ms: f64,
    pub max_duration_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FailureSummary {
    pub name: String,
    pub cause: String,
    pub count: f64,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecentFailure {
    pub name: String,
    pub cause: String,
    pub duration_ms: f64,
    pub ended_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpanOccurrence {
    pub name: String,
    pub duration_ms: f64,
    pub ended_at: String,
    pub trace_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEvent {
    pub span_name: String,
    pub level: String,
    pub message: String,
    pub seen_at: String,
    pub trace_id: String,
}

/// `ServerTraceDiagnosticsResult`, the parts the page shows.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceDiagnostics {
    pub read_at: String,
    pub record_count: f64,
    pub parse_error_count: f64,
    pub failure_count: f64,
    pub slow_span_threshold_ms: f64,
    pub slow_span_count: f64,
    pub top_spans_by_count: Vec<SpanSummary>,
    pub slowest_spans: Vec<SpanOccurrence>,
    pub common_failures: Vec<FailureSummary>,
    pub latest_failures: Vec<RecentFailure>,
    pub latest_warning_and_error_logs: Vec<LogEvent>,
    pub partial_failure: bool,
    pub error: Option<String>,
}

pub fn decode_trace_diagnostics(value: &Value) -> Option<TraceDiagnostics> {
    Some(TraceDiagnostics {
        read_at: value.get("readAt")?.as_str()?.to_owned(),
        record_count: number(value, "recordCount"),
        parse_error_count: number(value, "parseErrorCount"),
        failure_count: number(value, "failureCount"),
        slow_span_threshold_ms: number(value, "slowSpanThresholdMs"),
        slow_span_count: number(value, "slowSpanCount"),
        top_spans_by_count: items(value, "topSpansByCount", |span| {
            Some(SpanSummary {
                name: span.get("name")?.as_str()?.to_owned(),
                count: number(span, "count"),
                failure_count: number(span, "failureCount"),
                average_duration_ms: number(span, "averageDurationMs"),
                max_duration_ms: number(span, "maxDurationMs"),
            })
        }),
        slowest_spans: items(value, "slowestSpans", |span| {
            Some(SpanOccurrence {
                name: span.get("name")?.as_str()?.to_owned(),
                duration_ms: number(span, "durationMs"),
                ended_at: text(span, "endedAt"),
                trace_id: text(span, "traceId"),
            })
        }),
        common_failures: items(value, "commonFailures", |failure| {
            Some(FailureSummary {
                name: failure.get("name")?.as_str()?.to_owned(),
                cause: text(failure, "cause"),
                count: number(failure, "count"),
                last_seen_at: text(failure, "lastSeenAt"),
            })
        }),
        latest_failures: items(value, "latestFailures", |failure| {
            Some(RecentFailure {
                name: failure.get("name")?.as_str()?.to_owned(),
                cause: text(failure, "cause"),
                duration_ms: number(failure, "durationMs"),
                ended_at: text(failure, "endedAt"),
            })
        }),
        latest_warning_and_error_logs: items(value, "latestWarningAndErrorLogs", |event| {
            Some(LogEvent {
                span_name: event.get("spanName")?.as_str()?.to_owned(),
                level: text(event, "level"),
                message: text(event, "message"),
                seen_at: text(event, "seenAt"),
                trace_id: text(event, "traceId"),
            })
        }),
        partial_failure: option_value(value.get("partialFailure")).and_then(Value::as_bool).unwrap_or(false),
        error: option_message(value.get("error")),
    })
}

impl TraceDiagnostics {
    /// The Slow Spans tooltip.
    pub fn slow_span_tooltip(&self) -> String {
        format!("Spans with a duration of {} or longer.", format_duration(self.slow_span_threshold_ms))
    }

    /// The trace error line: a partial failure warns that diagnostics may be incomplete.
    pub fn error_text(&self) -> Option<String> {
        let error = self.error.as_ref()?;
        Some(if self.partial_failure { format!("Some trace files could not be read, so diagnostics may be incomplete. {error}") } else { error.clone() })
    }
}

/// `ServerProcessSignal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Int,
    Kill,
}

impl Signal {
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::Int => "SIGINT",
            Signal::Kill => "SIGKILL",
        }
    }

    pub fn payload(self, process: &ProcessEntry) -> Value {
        json!({ "pid": process.pid, "startTimeMs": process.start_time_ms, "signal": self.as_str() })
    }
}

/// The SIGKILL confirmation (`dialogs.confirm`, split as `ConfirmDialogHost` splits it).
pub fn kill_confirmation(pid: u64) -> (String, &'static str) {
    (format!("Send SIGKILL to process {pid}?"), "This cannot be handled by the process.")
}

/// What a `server.signalProcess` reply means for the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalOutcome {
    Signaled,
    /// The server no longer counts it as a child: an info toast, not an error.
    AlreadyExited,
    Failed(String),
}

pub fn signal_outcome(reply: &Value, signal: Signal) -> SignalOutcome {
    if reply.get("signaled").and_then(Value::as_bool) == Some(true) {
        return SignalOutcome::Signaled;
    }
    let message = option_value(reply.get("message")).and_then(Value::as_str).map(str::to_owned);
    match message {
        Some(message) if message.contains("not a live descendant") => SignalOutcome::AlreadyExited,
        Some(message) => SignalOutcome::Failed(message),
        None => SignalOutcome::Failed(format!("Failed to send {}.", signal.as_str())),
    }
}

pub const ALREADY_EXITED_TITLE: &str = "Process already exited";
pub const ALREADY_EXITED_DESCRIPTION: &str = "The process is not a child of the T3 Server. It might already have exited.";

/// `ServerObservability`: what the server runs with, where its exports come from, and its logs.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerObservability {
    pub logs_directory_path: Option<String>,
    pub otlp_traces_url: Option<String>,
    pub otlp_traces_source: Option<String>,
    pub otlp_traces_variable: Option<String>,
    pub otlp_metrics_url: Option<String>,
    pub otlp_metrics_source: Option<String>,
    pub otlp_metrics_variable: Option<String>,
    pub otlp_logs_url: Option<String>,
    pub otlp_logs_source: Option<String>,
    pub otlp_logs_variable: Option<String>,
}

/// `ObservabilitySettings`: the saved endpoints, empty for off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ObservabilitySettings {
    pub otlp_traces_url: String,
    pub otlp_metrics_url: String,
    pub otlp_logs_url: String,
}

/// One OTLP signal (`SIGNALS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OtlpSignal {
    Traces,
    Metrics,
    Logs,
}

impl OtlpSignal {
    pub const ALL: [OtlpSignal; 3] = [OtlpSignal::Traces, OtlpSignal::Metrics, OtlpSignal::Logs];

    pub fn key(self) -> &'static str {
        match self {
            OtlpSignal::Traces => "otlpTracesUrl",
            OtlpSignal::Metrics => "otlpMetricsUrl",
            OtlpSignal::Logs => "otlpLogsUrl",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            OtlpSignal::Traces => "Traces",
            OtlpSignal::Metrics => "Metrics",
            OtlpSignal::Logs => "Logs",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OtlpSignal::Traces => "traces",
            OtlpSignal::Metrics => "metrics",
            OtlpSignal::Logs => "logs",
        }
    }

    pub fn placeholder(self) -> String {
        format!("http://localhost:4318/v1/{}", self.as_str())
    }

    pub fn saved(self, settings: &ObservabilitySettings) -> &str {
        match self {
            OtlpSignal::Traces => &settings.otlp_traces_url,
            OtlpSignal::Metrics => &settings.otlp_metrics_url,
            OtlpSignal::Logs => &settings.otlp_logs_url,
        }
    }

    /// What the server runs with now, empty when export is off.
    pub fn running(self, running: Option<&ServerObservability>) -> String {
        let url = running.and_then(|running| match self {
            OtlpSignal::Traces => running.otlp_traces_url.clone(),
            OtlpSignal::Metrics => running.otlp_metrics_url.clone(),
            OtlpSignal::Logs => running.otlp_logs_url.clone(),
        });
        url.unwrap_or_default()
    }

    pub fn check_payload(self, url: &str) -> Value {
        json!({ "signal": self.as_str(), "url": url })
    }
}

/// `runningOverride`: an endpoint the environment or the desktop shell sets, which the saved one
/// cannot change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningOverride {
    pub url: String,
    pub label: &'static str,
    pub variable: Option<String>,
}

pub fn running_override(running: Option<&ServerObservability>, signal: OtlpSignal) -> Option<RunningOverride> {
    let running = running?;
    let (url, source, variable) = match signal {
        OtlpSignal::Traces => (&running.otlp_traces_url, &running.otlp_traces_source, &running.otlp_traces_variable),
        OtlpSignal::Metrics => (&running.otlp_metrics_url, &running.otlp_metrics_source, &running.otlp_metrics_variable),
        OtlpSignal::Logs => (&running.otlp_logs_url, &running.otlp_logs_source, &running.otlp_logs_variable),
    };
    let label = match source.as_deref() {
        Some("environment") => "Set by environment",
        Some("bootstrap") => "Set by desktop",
        _ => return None,
    };
    Some(RunningOverride { url: url.clone()?, label, variable: variable.clone() })
}

/// `isHttpUrl`.
pub fn is_http_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["http://", "https://"].iter().any(|scheme| lower.strip_prefix(scheme).is_some_and(|rest| rest.chars().next().is_some_and(|c| c != '\n')))
}

/// `OtlpEndpointCheckResult`.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckResult {
    Accepted { latency_ms: f64 },
    Rejected { status: f64 },
    Unreachable { timed_out: bool },
}

pub fn decode_check(value: &Value) -> Option<CheckResult> {
    Some(match value.get("_tag")?.as_str()? {
        "Accepted" => CheckResult::Accepted { latency_ms: number(value, "latencyMs") },
        "Rejected" => CheckResult::Rejected { status: number(value, "status") },
        "Unreachable" => CheckResult::Unreachable { timed_out: value.get("timedOut").and_then(Value::as_bool).unwrap_or(false) },
        _ => return None,
    })
}

/// An endpoint test: the URL it tested, and its result once in.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointCheck {
    pub url: String,
    pub result: Option<CheckResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    Muted,
    Warning,
    Success,
    Destructive,
}

/// `endpointStatus`.
pub fn endpoint_status(url: &str, check: Option<&EndpointCheck>) -> (String, Dot) {
    if url.is_empty() {
        return ("Off".into(), Dot::Muted);
    }
    let Some(check) = check.filter(|check| check.url == url) else {
        return ("Not checked".into(), Dot::Muted);
    };
    match &check.result {
        None => ("Checking\u{2026}".into(), Dot::Warning),
        Some(CheckResult::Accepted { latency_ms }) => (format!("Connected \u{b7} {} ms", latency_ms.round()), Dot::Success),
        Some(CheckResult::Rejected { status }) => (format!("Rejected \u{b7} HTTP {status}"), Dot::Destructive),
        Some(CheckResult::Unreachable { timed_out }) => ((if *timed_out { "Timed out" } else { "Unreachable" }).into(), Dot::Destructive),
    }
}

/// A signal's status line: the override's source with its check, or the check alone.
pub fn signal_status(running_override: Option<&RunningOverride>, check: Option<(String, Dot)>) -> Option<(String, Dot)> {
    match running_override {
        Some(found) => Some(match check {
            Some((label, dot)) => (format!("{} \u{b7} {label}", found.label), dot),
            None => (found.label.to_owned(), Dot::Muted),
        }),
        None => check,
    }
}

/// The "Running:" line shows while the saved endpoint differs from the running one, or the
/// running one comes from a variable.
pub fn running_line(signal: OtlpSignal, saved: &ObservabilitySettings, running: Option<&ServerObservability>) -> Option<(String, Option<String>)> {
    let running_url = signal.running(running);
    let variable = running_override(running, signal).and_then(|found| found.variable);
    if signal.saved(saved) == running_url && variable.is_none() {
        return None;
    }
    Some((if running_url.is_empty() { "Disabled".to_owned() } else { running_url }, variable))
}

/// The signals whose field differs from what is saved, trimmed.
pub fn changed_signals(values: &[(OtlpSignal, String)], saved: &ObservabilitySettings) -> Vec<OtlpSignal> {
    values.iter().filter(|(signal, value)| value.trim() != signal.saved(saved)).map(|(signal, _)| *signal).collect()
}

/// Save: the changed endpoints only, as typed.
pub fn telemetry_save_payload(values: &[(OtlpSignal, String)], saved: &ObservabilitySettings) -> Value {
    let changed = changed_signals(values, saved);
    let observability: Map<String, Value> = values.iter().filter(|(signal, _)| changed.contains(signal)).map(|(signal, value)| (signal.key().to_owned(), json!(value))).collect();
    json!({ "patch": { "observability": observability } })
}

pub const TELEMETRY_SAVED: &str = "Saved. Restart the server to apply.";
pub const TELEMETRY_DESCRIPTION: &str = "Send traces, metrics, and logs to an OTLP HTTP receiver. Restart the server to apply. Environment variables override these settings.";

pub fn telemetry_no_permission(environment: &str) -> String {
    format!("This connection lacks permission to change settings on {environment}.")
}

/// The error toast's title when a signal fails.
pub fn signal_failure_title(signal: Signal) -> String {
    format!("Could not send {}", signal.as_str())
}

#[cfg(test)]
mod tests;
