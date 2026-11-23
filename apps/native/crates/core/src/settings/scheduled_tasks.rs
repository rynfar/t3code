//! Scheduled tasks: the list, labels, destination thread modes, webhook settings & deliveries,
//! and editor draft of `web/src/components/settings/ScheduledTasksSettings.tsx` and
//! `scheduledTasksSettings.logic.ts`, over `contracts/src/scheduledTask.ts`.

use std::collections::BTreeMap;

use chrono::{Offset, TimeZone};
use chrono_tz::{OffsetName, Tz};
use serde_json::{Map, Value, json};

use crate::client_settings::TimestampFormat;
use crate::model::ModelSelection;
use crate::settings::archive::relative_label;
use crate::shell::Lineage;
use crate::sidebar::time::{LocalTime, parse_iso};
use crate::timeline::TimestampLocale;

pub mod method {
    pub const SUBSCRIBE: &str = "scheduledTasks.subscribe";
    pub const UPSERT: &str = "scheduledTasks.upsert";
    pub const SET_ENABLED: &str = "scheduledTasks.setEnabled";
    pub const DELETE: &str = "scheduledTasks.delete";
    pub const RUN_NOW: &str = "scheduledTasks.runNow";
    pub const ROTATE_WEBHOOK_TOKEN: &str = "scheduledTasks.rotateWebhookToken";
    pub const LIST_WEBHOOK_DELIVERIES: &str = "scheduledTasks.listWebhookDeliveries";
    pub const GET_WEBHOOK_DELIVERY: &str = "scheduledTasks.getWebhookDelivery";
}

pub const MIN_INTERVAL_MS: u64 = 60_000;
/// `MAX_WEBHOOK_DELIVERY_AGE_MINUTES` (24 hours).
pub const MAX_WEBHOOK_DELIVERY_AGE_MINUTES: i64 = 24 * 60;
/// `MAX_WEBHOOK_FILTERS`.
pub const MAX_WEBHOOK_FILTERS: usize = 16;
/// `DEFAULT_WEBHOOK_PROMPT`.
pub const DEFAULT_WEBHOOK_PROMPT: &str = "Handle this webhook:\n{{body}}";
/// `WEEKDAY_LABELS`, Sunday first as the schedule numbers them.
pub const WEEKDAY_LABELS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
/// The editor's day toggles, Monday first, with the day number each saves.
pub const DAY_TOGGLES: [(u8, &str); 7] = [(1, "Mo"), (2, "Tu"), (3, "We"), (4, "Th"), (5, "Fr"), (6, "Sa"), (0, "Su")];

pub const DEFAULT_SIGNATURE_HEADER: &str = "x-hub-signature-256";
pub const DEFAULT_SIGNATURE_PREFIX: &str = "sha256=";

/// `ScheduledTaskThreadMode` (`pingdotgg/t3code#30326`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThreadMode {
    #[default]
    NewThread,
    Continue,
    FreshSession,
}

impl ThreadMode {
    pub const ALL: [ThreadMode; 3] = [ThreadMode::NewThread, ThreadMode::Continue, ThreadMode::FreshSession];

    pub fn as_str(self) -> &'static str {
        match self {
            ThreadMode::NewThread => "new_thread",
            ThreadMode::Continue => "continue",
            ThreadMode::FreshSession => "fresh_session",
        }
    }

    pub fn parse(value: &str) -> Option<ThreadMode> {
        ThreadMode::ALL.into_iter().find(|mode| mode.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            ThreadMode::NewThread => "New thread each run",
            ThreadMode::Continue => "One thread, keep conversation",
            ThreadMode::FreshSession => "One thread, fresh context each run",
        }
    }

    pub fn detail(self, has_thread: bool) -> &'static str {
        match self {
            ThreadMode::NewThread => "Each run starts a new thread with the workspace and model below.",
            ThreadMode::Continue if has_thread => "Each run continues this conversation, in its workspace and with its current model.",
            ThreadMode::Continue => "The first run starts a thread with the workspace and model below; later runs continue that conversation.",
            ThreadMode::FreshSession if has_thread => {
                "Each run posts to this thread in its workspace, starting a fresh agent session without carrying over prior turns."
            }
            ThreadMode::FreshSession => {
                "The first run starts a thread with the workspace and model below; later runs stay in that thread with a fresh agent session each time."
            }
        }
    }
}

/// `ScheduledTaskWebhookSignature.encoding`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SignatureEncoding {
    #[default]
    Hex,
    Base64,
}

impl SignatureEncoding {
    pub const ALL: [SignatureEncoding; 2] = [SignatureEncoding::Hex, SignatureEncoding::Base64];

    pub fn as_str(self) -> &'static str {
        match self {
            SignatureEncoding::Hex => "hex",
            SignatureEncoding::Base64 => "base64",
        }
    }

    pub fn parse(value: &str) -> Option<SignatureEncoding> {
        match value {
            "hex" => Some(SignatureEncoding::Hex),
            "base64" => Some(SignatureEncoding::Base64),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SignatureEncoding::Hex => "Hex",
            SignatureEncoding::Base64 => "Base64",
        }
    }
}

/// `ScheduledTaskWebhookFilterOp`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WebhookFilterOp {
    #[default]
    Equals,
    NotEquals,
    In,
    StartsWith,
    Exists,
}

impl WebhookFilterOp {
    pub const ALL: [WebhookFilterOp; 5] = [
        WebhookFilterOp::Equals,
        WebhookFilterOp::NotEquals,
        WebhookFilterOp::In,
        WebhookFilterOp::StartsWith,
        WebhookFilterOp::Exists,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WebhookFilterOp::Equals => "equals",
            WebhookFilterOp::NotEquals => "notEquals",
            WebhookFilterOp::In => "in",
            WebhookFilterOp::StartsWith => "startsWith",
            WebhookFilterOp::Exists => "exists",
        }
    }

    pub fn parse(value: &str) -> Option<WebhookFilterOp> {
        WebhookFilterOp::ALL.into_iter().find(|op| op.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            WebhookFilterOp::Equals => "equals",
            WebhookFilterOp::NotEquals => "does not equal",
            WebhookFilterOp::In => "is one of",
            WebhookFilterOp::StartsWith => "starts with",
            WebhookFilterOp::Exists => "exists",
        }
    }
}

/// `WebhookFilterDraft` (`packages/client-runtime/src/scheduledTaskWebhook.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookFilterDraft {
    pub id: u64,
    pub path: String,
    pub op: WebhookFilterOp,
    pub value: String,
}

/// `isWebhookDedupeTemplate` (`packages/contracts/src/scheduledTask.ts`).
pub fn is_webhook_dedupe_template(template: &str) -> bool {
    let mut rest = template;
    let mut count = 0usize;
    while let Some(start) = rest.find("{{") {
        let before = &rest[..start];
        if before.contains("}}") {
            return false;
        }
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            return false;
        };
        let inner = after_open[..end].trim();
        if !is_valid_placeholder_expr(inner) {
            return false;
        }
        count += 1;
        rest = &after_open[end + 2..];
    }
    count > 0 && !rest.contains("{{") && !rest.contains("}}")
}

fn is_valid_placeholder_expr(inner: &str) -> bool {
    if inner == "request" {
        return true;
    }
    let mut parts = inner.split('.');
    let Some(root) = parts.next() else { return false };
    if !matches!(root, "body" | "headers" | "query") {
        return false;
    }
    parts.all(|segment| !segment.is_empty() && !segment.chars().any(|c| c.is_whitespace() || matches!(c, '{' | '}' | '.')))
}

fn is_valid_filter_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return false;
    }
    matches!(trimmed, "body" | "headers" | "query")
        || trimmed.starts_with("body.")
        || trimmed.starts_with("headers.")
        || trimmed.starts_with("query.")
}

/// Blank means `Ok(None)` ("no limit"); invalid values return `Err(message)`.
pub fn parse_max_delivery_age(value: &str) -> Result<Option<i64>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let minutes = trimmed.parse::<i64>().map_err(|_| {
        format!("Enter whole minutes from 1 to {MAX_WEBHOOK_DELIVERY_AGE_MINUTES}, or leave it blank.")
    })?;
    if (1..=MAX_WEBHOOK_DELIVERY_AGE_MINUTES).contains(&minutes) {
        Ok(Some(minutes))
    } else {
        Err(format!("Enter whole minutes from 1 to {MAX_WEBHOOK_DELIVERY_AGE_MINUTES}, or leave it blank."))
    }
}

pub fn decode_webhook_filter_drafts(schedule: &Value) -> Vec<WebhookFilterDraft> {
    let Some(filters) = schedule.get("filters").and_then(Value::as_array) else {
        return Vec::new();
    };
    filters
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let path = item.get("path")?.as_str()?.to_owned();
            let op = WebhookFilterOp::parse(item.get("op")?.as_str()?)?;
            let value = match op {
                WebhookFilterOp::Exists => String::new(),
                WebhookFilterOp::In => item
                    .get("value")?
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => item.get("value")?.as_str()?.to_owned(),
            };
            Some(WebhookFilterDraft { id: (idx as u64) + 1, path, op, value })
        })
        .collect()
}

pub fn parse_webhook_filter_drafts(drafts: &[WebhookFilterDraft]) -> Result<Vec<Value>, String> {
    if drafts.len() > MAX_WEBHOOK_FILTERS {
        return Err(format!("Use at most {MAX_WEBHOOK_FILTERS} filters."));
    }
    let mut out = Vec::with_capacity(drafts.len());
    for (index, draft) in drafts.iter().enumerate() {
        let path = draft.path.trim();
        let row_num = index + 1;
        if !is_valid_filter_path(path) {
            return Err(format!(
                "Filter {row_num} needs a path under body, headers or query{}",
                match draft.op {
                    WebhookFilterOp::Exists => ".",
                    WebhookFilterOp::In => ", and at least one value.",
                    _ => ", and a value.",
                }
            ));
        }
        match draft.op {
            WebhookFilterOp::Exists => out.push(json!({ "path": path, "op": "exists" })),
            WebhookFilterOp::In => {
                let items: Vec<String> = draft
                    .value
                    .split(',')
                    .map(|item| item.trim().to_owned())
                    .filter(|item| !item.is_empty())
                    .collect();
                if items.is_empty() {
                    return Err(format!("Filter {row_num} needs a path under body, headers or query, and at least one value."));
                }
                out.push(json!({ "path": path, "op": "in", "value": items }));
            }
            op => {
                if draft.value.is_empty() {
                    return Err(format!("Filter {row_num} needs a path under body, headers or query, and a value."));
                }
                out.push(json!({ "path": path, "op": op.as_str(), "value": draft.value }));
            }
        }
    }
    Ok(out)
}

pub fn webhook_filter_drafts_changed(saved: &[WebhookFilterDraft], current: &[WebhookFilterDraft]) -> bool {
    saved.len() != current.len()
        || saved.iter().zip(current).any(|(left, right)| {
            left.path != right.path || left.op != right.op || (left.op != WebhookFilterOp::Exists && left.value != right.value)
        })
}

/// `scheduleTimeZoneLabel` (`packages/client-runtime/src/scheduledTaskTimeZone.ts`):
/// returns the short zone name (e.g. `PDT`, `UTC`) for an IANA time zone at `reference_iso` (or `now_ms`).
pub fn schedule_time_zone_label(time_zone: Option<&str>, reference_iso: Option<&str>, now_ms: i64) -> Option<String> {
    let tz_name = time_zone?.trim();
    if tz_name.is_empty() {
        return None;
    }
    if matches!(tz_name, "UTC" | "Etc/UTC" | "GMT" | "Etc/GMT") {
        return Some("UTC".to_owned());
    }
    let tz: Tz = tz_name.parse().ok()?;
    let epoch_ms = reference_iso.and_then(parse_iso).unwrap_or(now_ms);
    let dt = tz.timestamp_millis_opt(epoch_ms).single()?;
    if let Some(abbr) = dt.offset().abbreviation()
        && !abbr.is_empty()
    {
        return Some(abbr.to_owned());
    }
    let offset_secs = dt.offset().fix().local_minus_utc();
    let sign = if offset_secs >= 0 { '+' } else { '-' };
    let abs = offset_secs.abs();
    let hours = abs / 3600;
    let mins = (abs % 3600) / 60;
    if mins == 0 {
        Some(format!("UTC{sign}{hours}"))
    } else {
        Some(format!("UTC{sign}{hours}:{mins:02}"))
    }
}

/// `formatTimeOfDay` (`apps/web/src/timestampFormat.ts`): formats `H:MM` / `HH:MM` wall clock.
pub fn format_time_of_day(
    time_of_day: &str,
    format: TimestampFormat,
    locale: &dyn TimestampLocale,
) -> String {
    let trimmed = time_of_day.trim();
    let Some((h_str, m_str)) = trimmed.split_once(':') else {
        return time_of_day.to_owned();
    };
    if m_str.len() != 2
        || !(1..=2).contains(&h_str.len())
        || !h_str.bytes().all(|b| b.is_ascii_digit())
        || !m_str.bytes().all(|b| b.is_ascii_digit())
    {
        return time_of_day.to_owned();
    }
    let (Ok(hour), Ok(minute)) = (h_str.parse::<i64>(), m_str.parse::<i64>()) else {
        return time_of_day.to_owned();
    };
    if hour > 23 || minute > 59 {
        return time_of_day.to_owned();
    }
    locale.time(
        LocalTime { year: 2000, month: 1, day: 1, hour, minute, second: 0, millis: 0, weekday: 6 },
        format,
    )
}

/// An empty base is automatic: the project's default branch, else `main` or `master`
/// (`scheduledTaskWorktreeBaseRef` in `packages/client-runtime/src/scheduledTaskWorktree.ts`).
pub fn scheduled_task_worktree_base_ref(base_ref: &str, refs: &[crate::vcs::VcsRef]) -> String {
    let picked = base_ref.trim();
    if !picked.is_empty() {
        return picked.to_owned();
    }
    if let Some(default_ref) = refs.iter().find(|r| r.is_default) {
        return default_ref.name.clone();
    }
    for name in ["main", "master"] {
        let origin_name = format!("origin/{name}");
        if let Some(found) = refs.iter().find(|r| r.name == name || r.name == origin_name) {
            return found.name.clone();
        }
    }
    "main".to_owned()
}

/// `ScheduledTaskSchedule`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    Interval { every_ms: u64 },
    FixedTime { time_of_day: String, weekdays: Vec<u8> },
    Once { run_at: String },
    Webhook,
    /// A schedule type this client does not know; the task lists but cannot be edited here.
    Unknown,
}

impl Schedule {
    fn decode(value: &Value) -> Schedule {
        match value.get("type").and_then(Value::as_str) {
            Some("interval") => value.get("everyMs").and_then(Value::as_u64).map_or(Schedule::Unknown, |every_ms| Schedule::Interval { every_ms }),
            Some("fixed_time") => match value.get("timeOfDay").and_then(Value::as_str) {
                Some(time) => Schedule::FixedTime {
                    time_of_day: time.to_owned(),
                    weekdays: value
                        .get("weekdays")
                        .and_then(Value::as_array)
                        .map(|days| days.iter().filter_map(Value::as_u64).filter(|day| *day < 7).map(|day| day as u8).collect())
                        .unwrap_or_default(),
                },
                None => Schedule::Unknown,
            },
            Some("once") => value.get("runAt").and_then(Value::as_str).map_or(Schedule::Unknown, |run_at| Schedule::Once { run_at: run_at.to_owned() }),
            Some("webhook") => Schedule::Webhook,
            _ => Schedule::Unknown,
        }
    }

    /// `scheduleLabel` without server time zone suffix.
    pub fn label(&self, once_label: impl Fn(i64) -> String) -> String {
        self.label_with_timezone(None, None, 0, once_label)
    }

    /// `scheduleLabel` with optional server IANA time zone suffix and user `TimestampFormat`.
    pub fn label_with_timezone(
        &self,
        time_zone: Option<&str>,
        next_run_at: Option<&str>,
        now_ms: i64,
        once_label: impl Fn(i64) -> String,
    ) -> String {
        self.format_label(None, time_zone, next_run_at, now_ms, once_label)
    }

    /// `scheduleLabel` following `TimestampFormat` (`pingdotgg/t3code#37314`).
    pub fn label_with_format(
        &self,
        format: TimestampFormat,
        locale: &dyn TimestampLocale,
        time_zone: Option<&str>,
        next_run_at: Option<&str>,
        now_ms: i64,
        once_label: impl Fn(i64) -> String,
    ) -> String {
        self.format_label(Some((format, locale)), time_zone, next_run_at, now_ms, once_label)
    }

    fn format_label(
        &self,
        clock: Option<(TimestampFormat, &dyn TimestampLocale)>,
        time_zone: Option<&str>,
        next_run_at: Option<&str>,
        now_ms: i64,
        once_label: impl Fn(i64) -> String,
    ) -> String {
        match self {
            Schedule::Webhook => "On webhook".to_owned(),
            Schedule::Once { run_at } => format!("Once, {}", parse_iso(run_at).map(&once_label).unwrap_or_else(|| run_at.clone())),
            Schedule::Interval { every_ms } if every_ms % 60_000 == 0 => format!("Every {} min", every_ms / 60_000),
            Schedule::Interval { every_ms } => format!("Every {} sec", (*every_ms as f64 / 1000.).round()),
            Schedule::FixedTime { time_of_day, weekdays } => {
                let days = if weekdays.is_empty() {
                    "Daily".to_owned()
                } else if weekdays.len() == 5 && weekdays.iter().all(|day| (1..=5).contains(day)) {
                    "Weekdays".to_owned()
                } else {
                    weekdays.iter().map(|day| WEEKDAY_LABELS[usize::from(*day)]).collect::<Vec<_>>().join(", ")
                };
                let time = match clock {
                    Some((format, locale)) => format_time_of_day(time_of_day, format, locale),
                    None => time_of_day.clone(),
                };
                match schedule_time_zone_label(time_zone, next_run_at, now_ms) {
                    Some(zone) => format!("{days} at {time} {zone}"),
                    None => format!("{days} at {time}"),
                }
            }
            Schedule::Unknown => "Unknown schedule".to_owned(),
        }
    }
}

/// `ScheduledTask.lastRunStatus`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunStatus {
    #[default]
    Never,
    Running,
    Succeeded,
    Failed,
}

impl RunStatus {
    fn parse(value: Option<&str>) -> RunStatus {
        match value {
            Some("running") => RunStatus::Running,
            Some("succeeded") => RunStatus::Succeeded,
            Some("failed") => RunStatus::Failed,
            _ => RunStatus::Never,
        }
    }

    /// The badge text, the raw status; `never` shows none.
    pub fn badge(self) -> Option<&'static str> {
        match self {
            RunStatus::Never => None,
            RunStatus::Running => Some("running"),
            RunStatus::Succeeded => Some("succeeded"),
            RunStatus::Failed => Some("failed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Webhook {
    pub path: String,
    /// The relay's public URL when the environment is linked to T3 Connect.
    pub url: Option<String>,
    /// Whether the server already holds a signing secret.
    pub has_secret: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledTask {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: Schedule,
    /// The schedule as the server sent it.
    pub schedule_raw: Value,
    pub project_id: String,
    pub thread_id: Option<String>,
    pub thread_mode: Option<ThreadMode>,
    pub workspace_strategy: Value,
    pub model_selection: ModelSelection,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub next_run_at: Option<String>,
    pub last_run_status: RunStatus,
    pub last_run_error: Option<String>,
    pub webhook: Option<Webhook>,
    pub blocked_by_policy: Option<String>,
}

/// Decoded `scheduledTasks.subscribe` snapshot including the server's IANA time zone.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskList {
    pub tasks: Vec<ScheduledTask>,
    pub time_zone: Option<String>,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn decode_task(value: &Value) -> Option<ScheduledTask> {
    let schedule_raw = value.get("schedule")?.clone();
    Some(ScheduledTask {
        id: text(value, "id")?,
        title: text(value, "title")?,
        prompt: text(value, "prompt").unwrap_or_default(),
        enabled: value.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        schedule: Schedule::decode(&schedule_raw),
        schedule_raw,
        project_id: text(value, "projectId")?,
        thread_id: text(value, "threadId"),
        thread_mode: value.get("threadMode").and_then(Value::as_str).and_then(ThreadMode::parse),
        workspace_strategy: value.get("workspaceStrategy").cloned().unwrap_or_else(|| json!({ "type": "root" })),
        model_selection: serde_json::from_value(value.get("modelSelection")?.clone()).ok()?,
        runtime_mode: text(value, "runtimeMode").unwrap_or_else(|| "full-access".to_owned()),
        interaction_mode: text(value, "interactionMode").unwrap_or_else(|| "default".to_owned()),
        next_run_at: text(value, "nextRunAt"),
        last_run_status: RunStatus::parse(value.get("lastRunStatus").and_then(Value::as_str)),
        last_run_error: text(value, "lastRunError").filter(|error| !error.is_empty()),
        webhook: value.get("webhook").and_then(|hook| {
            Some(Webhook {
                path: text(hook, "path")?,
                url: text(hook, "url"),
                has_secret: hook.get("hasSecret").and_then(Value::as_bool).unwrap_or(false),
            })
        }),
        blocked_by_policy: text(value, "blockedByPolicy").filter(|reason| !reason.is_empty()),
    })
}

/// One `scheduledTasks.subscribe` item: the whole list and optional server `timeZone`.
pub fn decode_list_with_timezone(item: &Value) -> Option<TaskList> {
    let tasks = item.get("tasks")?.as_array()?.iter().filter_map(decode_task).collect();
    let time_zone = text(item, "timeZone").filter(|tz| !tz.trim().is_empty());
    Some(TaskList { tasks, time_zone })
}

/// One `scheduledTasks.subscribe` item: the whole list, tasks that fail to decode skipped alone.
pub fn decode_list(item: &Value) -> Option<Vec<ScheduledTask>> {
    decode_list_with_timezone(item).map(|list| list.tasks)
}

/// `relativeLabel`: upcoming runs count up from "in under a minute"; overdue ones read like
/// `formatRelativeTimeLabel`.
pub fn relative_run_label(value: Option<&str>, now_ms: i64) -> String {
    let Some(at) = value.and_then(parse_iso) else { return "Not scheduled".to_owned() };
    let diff = at - now_ms;
    if diff <= 0 {
        return relative_label(value.unwrap_or_default(), now_ms);
    }
    let minutes = (diff + 59_999) / 60_000;
    if minutes < 2 {
        return "in under a minute".to_owned();
    }
    if minutes < 60 {
        return format!("in {minutes}m");
    }
    let hours = (minutes as f64 / 60.).round() as i64;
    if hours < 24 {
        return format!("in {hours}h");
    }
    format!("in {}d", (hours as f64 / 24.).round() as i64)
}

impl ScheduledTask {
    /// `scheduledTaskThreadMode` (`contracts/src/scheduledTask.ts`).
    pub fn effective_thread_mode(&self) -> ThreadMode {
        self.thread_mode.unwrap_or(if self.thread_id.is_none() {
            ThreadMode::NewThread
        } else {
            ThreadMode::Continue
        })
    }

    /// The row's state after the schedule label.
    pub fn state_label(&self, now_ms: i64) -> String {
        if let Some(reason) = &self.blocked_by_policy {
            return reason.clone();
        }
        if !self.enabled {
            return "Paused".to_owned();
        }
        if self.schedule == Schedule::Webhook {
            return "Listening".to_owned();
        }
        match &self.next_run_at {
            Some(next) => format!("Next run {}", relative_run_label(Some(next), now_ms)),
            None => "Not scheduled".to_owned(),
        }
    }

    pub fn is_webhook(&self) -> bool {
        self.schedule == Schedule::Webhook
    }
}

pub fn set_enabled_payload(id: &str, enabled: bool) -> Value {
    json!({ "id": id, "enabled": enabled })
}

pub fn id_payload(id: &str) -> Value {
    json!({ "id": id })
}

pub fn list_deliveries_payload(id: &str) -> Value {
    json!({ "id": id, "acceptAnyOutcome": true })
}

pub fn get_delivery_payload(id: &str, delivery_id: &str) -> Value {
    json!({ "id": id, "deliveryId": delivery_id, "acceptAnyOutcome": true })
}

/// `ScheduledTaskWebhookDeliverySummary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDeliverySummary {
    pub id: String,
    pub task_id: String,
    pub received_at: String,
    pub method: String,
    pub content_type: Option<String>,
    pub body_bytes: u64,
    pub outcome: String,
    pub signature_verified: bool,
    pub pending_window: bool,
    pub missing_fields: Vec<String>,
    pub error: Option<String>,
}

/// `ScheduledTaskWebhookDelivery`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDelivery {
    pub summary: WebhookDeliverySummary,
    pub query: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
    pub body_truncated: bool,
    pub rendered_prompt: Option<String>,
}

fn decode_delivery_summary(value: &Value) -> Option<WebhookDeliverySummary> {
    Some(WebhookDeliverySummary {
        id: text(value, "id")?,
        task_id: text(value, "taskId")?,
        received_at: text(value, "receivedAt")?,
        method: text(value, "method")?,
        content_type: text(value, "contentType"),
        body_bytes: value.get("bodyBytes").and_then(Value::as_u64).unwrap_or(0),
        outcome: text(value, "outcome")?,
        signature_verified: value.get("signatureVerified").and_then(Value::as_bool).unwrap_or(false),
        pending_window: value.get("pendingWindow").and_then(Value::as_bool).unwrap_or(false),
        missing_fields: value
            .get("missingFields")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default(),
        error: text(value, "error").filter(|e| !e.is_empty()),
    })
}

pub fn decode_webhook_deliveries(reply: &Value) -> Result<Vec<WebhookDeliverySummary>, String> {
    let list = reply.get("deliveries").and_then(Value::as_array).ok_or("Missing deliveries array")?;
    Ok(list.iter().filter_map(decode_delivery_summary).collect())
}

pub fn decode_webhook_delivery(reply: &Value) -> Result<WebhookDelivery, String> {
    let raw = reply.get("delivery").ok_or("Missing delivery object")?;
    let summary = decode_delivery_summary(raw).ok_or("Invalid delivery summary")?;
    let headers = raw
        .get("headers")
        .and_then(Value::as_object)
        .map(|obj| obj.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned()))).collect())
        .unwrap_or_default();
    Ok(WebhookDelivery {
        summary,
        query: text(raw, "query").unwrap_or_default(),
        headers,
        body: text(raw, "body").unwrap_or_default(),
        body_truncated: raw.get("bodyTruncated").and_then(Value::as_bool).unwrap_or(false),
        rendered_prompt: text(raw, "renderedPrompt"),
    })
}

/// `webhookDeliveryLabel` (`scheduledTasksSettings.logic.ts`).
pub fn webhook_delivery_label(outcome: &str, pending_window: bool) -> String {
    if outcome == "accepted" {
        return if pending_window { "Opened window" } else { "Ran" }.to_owned();
    }
    match outcome {
        "dispatch_failed" => "Run failed".to_owned(),
        "duplicate" => "Duplicate".to_owned(),
        "coalesced" => "Coalesced".to_owned(),
        "filtered" => "Filtered".to_owned(),
        "rejected_signature" => "Bad signature".to_owned(),
        "disabled" => "Task paused".to_owned(),
        "rate_limited" => "Rate limited".to_owned(),
        "expired" => "Too old".to_owned(),
        other => other.replace('_', " "),
    }
}

/// A destination thread candidate for the editor's "Destination thread" picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationThread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub updated_at: String,
}

/// `scheduledTaskDestinationThreads`: filters to non-archived, listed-lineage threads in
/// `project_id` (excluding `current_thread_id`), matched by `query`, sorted newest first.
pub fn destination_threads<'a>(
    threads: impl IntoIterator<Item = (&'a str, &'a str, &'a str, &'a str, bool, &'a Lineage)>,
    project_id: &str,
    current_thread_id: Option<&str>,
    query: &str,
) -> Vec<DestinationThread> {
    let search = query.trim().to_lowercase();
    let mut out: Vec<DestinationThread> = threads
        .into_iter()
        .filter(|(id, pid, title, _, archived, lineage)| {
            *pid == project_id
                && current_thread_id != Some(*id)
                && !*archived
                && lineage.is_listed()
                && (search.is_empty() || title.to_lowercase().contains(&search))
        })
        .map(|(id, pid, title, updated_at, _, _)| DestinationThread {
            id: id.to_owned(),
            project_id: pid.to_owned(),
            title: title.to_owned(),
            updated_at: updated_at.to_owned(),
        })
        .collect();
    out.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    out
}

/// The editor's schedule choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScheduleMode {
    #[default]
    Fixed,
    Once,
    Interval,
    Webhook,
}

impl ScheduleMode {
    pub const ALL: [ScheduleMode; 4] = [ScheduleMode::Fixed, ScheduleMode::Once, ScheduleMode::Interval, ScheduleMode::Webhook];

    pub fn label(self) -> &'static str {
        match self {
            ScheduleMode::Fixed => "At a time",
            ScheduleMode::Once => "Once",
            ScheduleMode::Interval => "Every interval",
            ScheduleMode::Webhook => "On webhook",
        }
    }
}

/// The editor's workspace choice (`WorkspaceStrategy`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkspaceMode {
    #[default]
    Worktree,
    Root,
    ExistingWorktree,
}

impl WorkspaceMode {
    pub const ALL: [WorkspaceMode; 3] = [WorkspaceMode::Worktree, WorkspaceMode::Root, WorkspaceMode::ExistingWorktree];

    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceMode::Worktree => "worktree",
            WorkspaceMode::Root => "root",
            WorkspaceMode::ExistingWorktree => "existing_worktree",
        }
    }

    pub fn parse(value: &str) -> Option<WorkspaceMode> {
        WorkspaceMode::ALL.into_iter().find(|mode| mode.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            WorkspaceMode::Worktree => "Create a new worktree",
            WorkspaceMode::Root => "Use the project checkout",
            WorkspaceMode::ExistingWorktree => "Use a specific checkout",
        }
    }
}

/// The editor form (`ScheduledTaskDraft`). Text fields hold what the user typed.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub editing_id: Option<String>,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule_mode: ScheduleMode,
    pub interval_minutes: String,
    pub time_of_day: String,
    /// `YYYY-MM-DD HH:MM` in local time, as `datetime-local` holds it.
    pub run_at: String,
    /// The stored `runAt`, sent back unchanged while `run_at` still shows it.
    pub saved_run_at: Option<(String, String)>,
    pub weekdays: Vec<u8>,
    pub project_id: String,
    pub thread_id: Option<String>,
    /// The thread the editor opened with, so saving after clearing it emits `unbindThread: true`.
    pub saved_thread_id: Option<String>,
    pub thread_mode: ThreadMode,
    pub workspace_mode: WorkspaceMode,
    pub base_ref: String,
    pub start_from_origin: bool,
    pub existing_worktree_path: String,
    pub model: Option<ModelSelection>,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub signature_enabled: bool,
    pub signature_header: String,
    pub signature_encoding: SignatureEncoding,
    pub signature_prefix: String,
    /// Write-only: empty keeps the secret already stored on the server.
    pub signature_secret: String,
    pub has_saved_signature_secret: bool,
    pub max_delivery_age_minutes: String,
    pub replay_key: String,
    pub coalesce_key: String,
    pub coalesce_window_seconds: String,
    pub filters: Vec<WebhookFilterDraft>,
    pub saved_filters: Vec<WebhookFilterDraft>,
    pub next_filter_id: u64,
}

impl Default for Draft {
    fn default() -> Self {
        Draft {
            editing_id: None,
            title: String::new(),
            prompt: String::new(),
            enabled: true,
            schedule_mode: ScheduleMode::default(),
            interval_minutes: "15".to_owned(),
            time_of_day: "09:00".to_owned(),
            run_at: String::new(),
            saved_run_at: None,
            weekdays: vec![1, 2, 3, 4, 5],
            project_id: String::new(),
            thread_id: None,
            saved_thread_id: None,
            thread_mode: ThreadMode::NewThread,
            workspace_mode: WorkspaceMode::default(),
            base_ref: String::new(),
            start_from_origin: true,
            existing_worktree_path: String::new(),
            model: None,
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            signature_enabled: false,
            signature_header: DEFAULT_SIGNATURE_HEADER.to_owned(),
            signature_encoding: SignatureEncoding::Hex,
            signature_prefix: DEFAULT_SIGNATURE_PREFIX.to_owned(),
            signature_secret: String::new(),
            has_saved_signature_secret: false,
            max_delivery_age_minutes: String::new(),
            replay_key: String::new(),
            coalesce_key: String::new(),
            coalesce_window_seconds: String::new(),
            filters: Vec::new(),
            saved_filters: Vec::new(),
            next_filter_id: 1,
        }
    }
}

/// Why a draft cannot be saved: a toast title and description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub title: String,
    pub description: String,
}

fn invalid(title: impl Into<String>, description: impl Into<String>) -> Invalid {
    Invalid { title: title.into(), description: description.into() }
}

/// `String(Math.max(1, everyMs / 60000))`: whole minutes print without a fraction.
fn minutes_text(every_ms: u64) -> String {
    let minutes = (every_ms as f64 / 60_000.).max(1.);
    if minutes.fract() == 0. { format!("{}", minutes as u64) } else { format!("{minutes}") }
}

const RUNTIME_MODES: [&str; 4] = ["approval-required", "auto-accept-edits", "auto", "full-access"];

/// `clampManagedRuntimeMode`: a mode above the ceiling drops to the highest one allowed; with
/// no ceiling, or none allowed, the mode stays (tasks have no mode picker to fix it).
pub fn clamp_runtime_mode(mode: &str, ceiling: Option<&str>) -> String {
    let rank = |mode: &str| RUNTIME_MODES.iter().position(|candidate| *candidate == mode);
    match (rank(mode), ceiling.and_then(rank)) {
        (Some(mode_rank), Some(ceiling_rank)) if mode_rank > ceiling_rank => RUNTIME_MODES[ceiling_rank].to_owned(),
        _ => mode.to_owned(),
    }
}

/// `managedPolicy.limits.maxRuntimeMode` from the server config, kept raw.
pub fn max_runtime_mode(policy: Option<&Value>) -> Option<&str> {
    policy?.get("limits")?.get("maxRuntimeMode")?.as_str()
}

/// `HH:MM` or `H:MM`, 00:00 to 23:59.
pub fn valid_time_of_day(value: &str) -> bool {
    let Some((hour, minute)) = value.split_once(':') else { return false };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    digits(hour) && hour.len() <= 2 && minute.len() == 2 && digits(minute) && hour.parse::<u8>().is_ok_and(|h| h < 24) && minute.parse::<u8>().is_ok_and(|m| m < 60)
}

impl Draft {
    /// `taskToDraft`; `local_run_at` turns a stored ISO time into the local field text.
    pub fn from_task(task: &ScheduledTask, local_run_at: impl Fn(&str) -> Option<String>) -> Draft {
        let mut draft = Draft {
            editing_id: Some(task.id.clone()),
            title: task.title.clone(),
            prompt: task.prompt.clone(),
            enabled: task.enabled,
            project_id: task.project_id.clone(),
            thread_id: task.thread_id.clone(),
            saved_thread_id: task.thread_id.clone(),
            thread_mode: task.effective_thread_mode(),
            model: Some(task.model_selection.clone()),
            runtime_mode: task.runtime_mode.clone(),
            interaction_mode: task.interaction_mode.clone(),
            has_saved_signature_secret: task.webhook.as_ref().is_some_and(|hook| hook.has_secret),
            ..Draft::default()
        };
        match &task.schedule {
            Schedule::Interval { every_ms } => {
                draft.schedule_mode = ScheduleMode::Interval;
                draft.interval_minutes = minutes_text(*every_ms);
            }
            Schedule::FixedTime { time_of_day, weekdays } => {
                draft.time_of_day = if time_of_day.len() == 4 { format!("0{time_of_day}") } else { time_of_day.clone() };
                draft.weekdays = if weekdays.is_empty() { (0..7).collect() } else { weekdays.clone() };
            }
            Schedule::Once { run_at } => {
                draft.schedule_mode = ScheduleMode::Once;
                if let Some(local) = local_run_at(run_at) {
                    draft.saved_run_at = Some((local.clone(), run_at.clone()));
                    draft.run_at = local;
                }
            }
            Schedule::Webhook => {
                draft.schedule_mode = ScheduleMode::Webhook;
                if let Some(sig) = task.schedule_raw.get("signature").and_then(Value::as_object) {
                    draft.signature_enabled = true;
                    draft.signature_header = sig.get("header").and_then(Value::as_str).unwrap_or(DEFAULT_SIGNATURE_HEADER).to_owned();
                    draft.signature_encoding = sig
                        .get("encoding")
                        .and_then(Value::as_str)
                        .and_then(SignatureEncoding::parse)
                        .unwrap_or(SignatureEncoding::Hex);
                    draft.signature_prefix = sig.get("prefix").and_then(Value::as_str).unwrap_or("").to_owned();
                }
                if let Some(age) = task.schedule_raw.get("maxDeliveryAgeMinutes").and_then(Value::as_i64) {
                    draft.max_delivery_age_minutes = age.to_string();
                }
                if let Some(dedupe) = task.schedule_raw.get("dedupe").and_then(Value::as_object) {
                    draft.replay_key = dedupe.get("replayKey").and_then(Value::as_str).unwrap_or_default().to_owned();
                    draft.coalesce_key = dedupe.get("coalesceKey").and_then(Value::as_str).unwrap_or_default().to_owned();
                    if let Some(win) = dedupe.get("coalesceWindowSeconds").and_then(Value::as_i64) {
                        draft.coalesce_window_seconds = win.to_string();
                    }
                }
                let filters = decode_webhook_filter_drafts(&task.schedule_raw);
                draft.next_filter_id = filters.iter().map(|f| f.id).max().unwrap_or(0) + 1;
                draft.saved_filters = filters.clone();
                draft.filters = filters;
            }
            Schedule::Unknown => {}
        }
        let strategy = &task.workspace_strategy;
        match strategy.get("type").and_then(Value::as_str) {
            Some("root") => draft.workspace_mode = WorkspaceMode::Root,
            Some("existing_worktree") => {
                draft.workspace_mode = WorkspaceMode::ExistingWorktree;
                draft.existing_worktree_path = text(strategy, "worktreePath").unwrap_or_default();
            }
            _ => {
                draft.base_ref = text(strategy, "baseRef").unwrap_or_default();
                draft.start_from_origin = strategy.get("startFromOrigin").and_then(Value::as_bool).unwrap_or(false);
            }
        }
        draft
    }

    /// `moveDraftToThread`: binds to a thread (and its project), switching out of `NewThread`.
    pub fn move_to_thread(&mut self, thread_id: String, project_id: String) {
        self.thread_id = Some(thread_id);
        self.project_id = project_id;
        if self.thread_mode == ThreadMode::NewThread {
            self.thread_mode = ThreadMode::Continue;
        }
    }

    /// `unbindDraftFromThread`: leaves the thread while keeping the mode.
    pub fn unbind_from_thread(&mut self) {
        if self.thread_id.is_none() {
            return;
        }
        self.thread_id = None;
        if self.workspace_mode == WorkspaceMode::Root {
            self.workspace_mode = WorkspaceMode::Worktree;
            self.start_from_origin = true;
        }
    }

    /// `changeDraftProject`: changing project unbinds from any thread in the old project.
    pub fn change_project(&mut self, project_id: String) {
        if self.project_id == project_id {
            return;
        }
        self.unbind_from_thread();
        self.project_id = project_id;
    }

    /// Changing `threadMode` to `NewThread` unbinds any attached thread.
    pub fn set_thread_mode(&mut self, mode: ThreadMode) {
        self.thread_mode = mode;
        if mode == ThreadMode::NewThread {
            self.unbind_from_thread();
        }
    }

    pub fn add_filter(&mut self) {
        if self.filters.len() >= MAX_WEBHOOK_FILTERS {
            return;
        }
        let id = self.next_filter_id;
        self.next_filter_id += 1;
        self.filters.push(WebhookFilterDraft {
            id,
            path: String::new(),
            op: WebhookFilterOp::Equals,
            value: String::new(),
        });
    }

    pub fn remove_filter(&mut self, id: u64) {
        self.filters.retain(|filter| filter.id != id);
    }

    /// Picking a schedule: Once with no time suggests the next full hour; a webhook fills an
    /// empty prompt with the default template.
    pub fn choose_schedule(&mut self, mode: ScheduleMode, next_full_hour: impl FnOnce() -> String) {
        self.schedule_mode = mode;
        if mode == ScheduleMode::Once && self.run_at.trim().is_empty() {
            self.run_at = next_full_hour();
        }
        if mode == ScheduleMode::Webhook && self.prompt.trim().is_empty() {
            self.prompt = DEFAULT_WEBHOOK_PROMPT.to_owned();
        }
    }

    /// A day toggle; the last day stays on.
    pub fn toggle_weekday(&mut self, day: u8) {
        if let Some(index) = self.weekdays.iter().position(|candidate| *candidate == day) {
            if self.weekdays.len() > 1 {
                self.weekdays.remove(index);
            }
        } else {
            self.weekdays.push(day);
            self.weekdays.sort_unstable();
        }
    }

    /// `ScheduledTaskUpsertInput`, or why it cannot be sent (omitting `threadMode` for older servers).
    pub fn upsert_input(
        &self,
        once_supported: bool,
        max_runtime_mode: Option<&str>,
        to_iso: impl Fn(&str) -> Option<String>,
    ) -> Result<Value, Invalid> {
        self.upsert_input_with_thread_modes(once_supported, false, max_runtime_mode, to_iso)
    }

    /// `ScheduledTaskUpsertInput`, or why it cannot be sent.
    /// `thread_modes_supported` gates emitting `"threadMode"` (`pingdotgg/t3code#30326`).
    pub fn upsert_input_with_thread_modes(
        &self,
        once_supported: bool,
        thread_modes_supported: bool,
        max_runtime_mode: Option<&str>,
        to_iso: impl Fn(&str) -> Option<String>,
    ) -> Result<Value, Invalid> {
        let model = self.model.as_ref().filter(|_| !self.title.trim().is_empty() && !self.prompt.trim().is_empty() && !self.project_id.is_empty());
        let Some(model) = model else { return Err(invalid("Scheduled task is incomplete", "Add a title, prompt, project, and model.")) };
        let schedule = match self.schedule_mode {
            ScheduleMode::Fixed => {
                if self.time_of_day.trim().is_empty() || !valid_time_of_day(self.time_of_day.trim()) {
                    return Err(invalid("Invalid run time", "Pick a time."));
                }
                let mut schedule = json!({ "type": "fixed_time", "timeOfDay": self.time_of_day.trim() });
                if !self.weekdays.is_empty() && self.weekdays.len() < 7 {
                    schedule["weekdays"] = json!(self.weekdays);
                }
                schedule
            }
            ScheduleMode::Once => {
                if !once_supported {
                    return Err(invalid(
                        "Once is not available here",
                        "This environment runs an older T3 Code. Update it to schedule a one-time task, or pick another schedule.",
                    ));
                }
                let run_at = match &self.saved_run_at {
                    Some((local, stored)) if *local == self.run_at => stored.clone(),
                    _ if self.run_at.trim().is_empty() => return Err(invalid("Invalid run time", "Pick a date and time.")),
                    _ => to_iso(self.run_at.trim()).ok_or_else(|| invalid("Invalid run time", "That time does not exist in your time zone."))?,
                };
                json!({ "type": "once", "runAt": run_at })
            }
            ScheduleMode::Interval => {
                let minutes = self.interval_minutes.trim().parse::<f64>().ok().filter(|minutes| minutes.is_finite());
                let every_ms = minutes.map(|minutes| (minutes * 60_000.).round()).filter(|ms| *ms >= MIN_INTERVAL_MS as f64);
                let Some(every_ms) = every_ms else { return Err(invalid("Invalid interval", "Enter an interval of at least one minute.")) };
                json!({ "type": "interval", "everyMs": every_ms as u64 })
            }
            ScheduleMode::Webhook => {
                let max_age = parse_max_delivery_age(&self.max_delivery_age_minutes)
                    .map_err(|msg| invalid("Invalid age limit", msg))?;
                let replay_key = self.replay_key.trim();
                if !replay_key.is_empty() && !is_webhook_dedupe_template(replay_key) {
                    return Err(invalid("Invalid webhook settings", "Duplicate request key must contain a valid request placeholder."));
                }
                let coalesce_key = self.coalesce_key.trim();
                if !coalesce_key.is_empty() && !is_webhook_dedupe_template(coalesce_key) {
                    return Err(invalid("Invalid webhook settings", "Coalesce key must contain a valid request placeholder."));
                }
                let coalesce_window = if self.coalesce_window_seconds.trim().is_empty() {
                    None
                } else {
                    let secs = self
                        .coalesce_window_seconds
                        .trim()
                        .parse::<i64>()
                        .ok()
                        .filter(|s| (10..=3600).contains(s))
                        .ok_or_else(|| invalid("Invalid webhook settings", "Coalesce window must be whole seconds from 10 to 3600."))?;
                    Some(secs)
                };
                let signature = if self.signature_enabled {
                    let header = self.signature_header.trim();
                    if header.is_empty() {
                        return Err(invalid("Invalid webhook signature", "Enter the signature header name."));
                    }
                    let secret = self.signature_secret.trim();
                    if secret.is_empty() && !self.has_saved_signature_secret {
                        return Err(invalid("Invalid webhook signature", "Enter a signing secret."));
                    }
                    let mut sig = json!({
                        "header": header,
                        "encoding": self.signature_encoding.as_str(),
                        "prefix": self.signature_prefix,
                    });
                    if !secret.is_empty() {
                        sig["secret"] = json!(secret);
                    }
                    sig
                } else {
                    Value::Null
                };
                let parsed_filters = parse_webhook_filter_drafts(&self.filters)
                    .map_err(|err| invalid("Invalid webhook filter", err))?;
                let mut dedupe = Map::new();
                if !replay_key.is_empty() {
                    dedupe.insert("replayKey".into(), json!(replay_key));
                }
                if !coalesce_key.is_empty() {
                    dedupe.insert("coalesceKey".into(), json!(coalesce_key));
                }
                if let Some(secs) = coalesce_window {
                    dedupe.insert("coalesceWindowSeconds".into(), json!(secs));
                }
                let mut sched = Map::new();
                sched.insert("type".into(), json!("webhook"));
                sched.insert("signature".into(), signature);
                sched.insert("maxDeliveryAgeMinutes".into(), json!(max_age));
                sched.insert("dedupe".into(), Value::Object(dedupe));
                if webhook_filter_drafts_changed(&self.saved_filters, &self.filters) {
                    sched.insert("filters".into(), Value::Array(parsed_filters));
                }
                Value::Object(sched)
            }
        };
        let workspace = match self.workspace_mode {
            WorkspaceMode::Root => json!({ "type": "root" }),
            WorkspaceMode::ExistingWorktree if self.existing_worktree_path.trim().is_empty() => {
                return Err(invalid("Checkout path is required", "Enter the path of the checkout to run in."));
            }
            WorkspaceMode::ExistingWorktree => json!({ "type": "existing_worktree", "worktreePath": self.existing_worktree_path.trim() }),
            WorkspaceMode::Worktree => json!({
                "type": "worktree",
                "baseRef": if self.base_ref.trim().is_empty() { "main" } else { self.base_ref.trim() },
                "startFromOrigin": self.start_from_origin,
            }),
        };
        let mut input = Map::new();
        if let Some(id) = &self.editing_id {
            input.insert("id".into(), json!(id));
            input.insert("requireExisting".into(), json!(true));
        }
        input.insert("title".into(), json!(self.title.trim()));
        input.insert("prompt".into(), json!(self.prompt.trim()));
        input.insert("enabled".into(), json!(self.enabled));
        input.insert("schedule".into(), schedule);
        input.insert("projectId".into(), json!(self.project_id));
        input.insert("threadId".into(), json!(self.thread_id));
        if self.thread_id.is_none() && self.saved_thread_id.is_some() {
            input.insert("unbindThread".into(), json!(true));
        }
        if thread_modes_supported {
            input.insert("threadMode".into(), json!(self.thread_mode.as_str()));
        }
        input.insert("workspaceStrategy".into(), workspace);
        input.insert("modelSelection".into(), json!(model));
        input.insert("runtimeMode".into(), json!(clamp_runtime_mode(&self.runtime_mode, max_runtime_mode)));
        input.insert("interactionMode".into(), json!(self.interaction_mode));
        input.insert("creationSource".into(), json!(crate::commands::CREATION_SOURCE));
        Ok(Value::Object(input))
    }
}

/// `webhookAddress`: what the editor shows for a saved webhook task, whether it can be copied,
/// and the note under it.
pub fn webhook_address(webhook: &Webhook, http_base: Option<&str>) -> (String, bool, &'static str) {
    if let Some(url) = &webhook.url {
        return (url.clone(), true, "");
    }
    let Some(base) = http_base.map(|base| base.trim_end_matches('/')) else {
        return (webhook.path.clone(), false, "Link this environment to T3 Connect for a public URL.");
    };
    let authority = base.split("://").nth(1).unwrap_or(base).split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    let loopback = matches!(host, "localhost" | "::1") || host.starts_with("127.");
    let note = if loopback {
        "Only this computer can call this address. Link T3 Connect for a public URL."
    } else {
        "Works wherever this environment's address is reachable, for example over Tailscale or your own proxy. Link T3 Connect for a public URL."
    };
    (format!("{base}{}", webhook.path), true, note)
}

#[cfg(test)]
mod tests;
