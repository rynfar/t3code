//! Scheduled tasks: the list, labels and editor draft of
//! `web/src/components/settings/ScheduledTasksSettings.tsx` and `scheduledTasksSettings.logic.ts`,
//! over `ScheduledTask` and `ScheduledTaskUpsertInput` (`contracts/src/scheduledTask.ts`).
//! `scheduledTasks.subscribe` sends the whole list again after every change, so a list item
//! replaces the list. Webhook tasks keep their stored schedule untouched unless switched away;
//! signatures, filters and dedupe keys are edited on the web.

use serde_json::{Map, Value, json};

use crate::model::ModelSelection;
use crate::settings::archive::relative_label;
use crate::sidebar::time::parse_iso;

pub mod method {
    pub const SUBSCRIBE: &str = "scheduledTasks.subscribe";
    pub const UPSERT: &str = "scheduledTasks.upsert";
    pub const SET_ENABLED: &str = "scheduledTasks.setEnabled";
    pub const DELETE: &str = "scheduledTasks.delete";
    pub const RUN_NOW: &str = "scheduledTasks.runNow";
}

pub const MIN_INTERVAL_MS: u64 = 60_000;
/// `DEFAULT_WEBHOOK_PROMPT`.
pub const DEFAULT_WEBHOOK_PROMPT: &str = "Handle this webhook:\n{{body}}";
/// `WEEKDAY_LABELS`, Sunday first as the schedule numbers them.
pub const WEEKDAY_LABELS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
/// The editor's day toggles, Monday first, with the day number each saves.
pub const DAY_TOGGLES: [(u8, &str); 7] = [(1, "Mo"), (2, "Tu"), (3, "We"), (4, "Th"), (5, "Fr"), (6, "Sa"), (0, "Su")];

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

    /// `scheduleLabel`; `once_label` formats an epoch-ms time as `Intl` medium date, short time.
    pub fn label(&self, once_label: impl Fn(i64) -> String) -> String {
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
                format!("{days} at {time_of_day}")
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledTask {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: Schedule,
    /// The schedule as the server sent it, saved back unchanged when the editor leaves it alone.
    pub schedule_raw: Value,
    pub project_id: String,
    pub thread_id: Option<String>,
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
        workspace_strategy: value.get("workspaceStrategy").cloned().unwrap_or_else(|| json!({ "type": "root" })),
        model_selection: serde_json::from_value(value.get("modelSelection")?.clone()).ok()?,
        runtime_mode: text(value, "runtimeMode").unwrap_or_else(|| "full-access".to_owned()),
        interaction_mode: text(value, "interactionMode").unwrap_or_else(|| "default".to_owned()),
        next_run_at: text(value, "nextRunAt"),
        last_run_status: RunStatus::parse(value.get("lastRunStatus").and_then(Value::as_str)),
        last_run_error: text(value, "lastRunError").filter(|error| !error.is_empty()),
        webhook: value.get("webhook").and_then(|hook| Some(Webhook { path: text(hook, "path")?, url: text(hook, "url") })),
        blocked_by_policy: text(value, "blockedByPolicy").filter(|reason| !reason.is_empty()),
    })
}

/// One `scheduledTasks.subscribe` item: the whole list, tasks that fail to decode skipped alone.
pub fn decode_list(item: &Value) -> Option<Vec<ScheduledTask>> {
    Some(item.get("tasks")?.as_array()?.iter().filter_map(decode_task).collect())
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
    /// `YYYY-MM-DDTHH:MM` in local time, as `datetime-local` holds it.
    pub run_at: String,
    /// The stored `runAt`, sent back unchanged while `run_at` still shows it.
    pub saved_run_at: Option<(String, String)>,
    pub weekdays: Vec<u8>,
    pub project_id: String,
    pub thread_id: Option<String>,
    pub workspace_mode: WorkspaceMode,
    pub base_ref: String,
    pub start_from_origin: bool,
    pub existing_worktree_path: String,
    pub model: Option<ModelSelection>,
    pub runtime_mode: String,
    pub interaction_mode: String,
    /// The webhook schedule as stored, kept when the task stays a webhook task.
    pub webhook_schedule: Option<Value>,
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
            workspace_mode: WorkspaceMode::default(),
            base_ref: "main".to_owned(),
            start_from_origin: true,
            existing_worktree_path: String::new(),
            model: None,
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            webhook_schedule: None,
        }
    }
}

/// Why a draft cannot be saved: a toast title and description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub title: &'static str,
    pub description: &'static str,
}

const fn invalid(title: &'static str, description: &'static str) -> Invalid {
    Invalid { title, description }
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
            model: Some(task.model_selection.clone()),
            runtime_mode: task.runtime_mode.clone(),
            interaction_mode: task.interaction_mode.clone(),
            ..Draft::default()
        };
        match &task.schedule {
            Schedule::Interval { every_ms } => {
                draft.schedule_mode = ScheduleMode::Interval;
                draft.interval_minutes = format!("{}", (*every_ms as f64 / 60_000.).max(1.).round());
            }
            Schedule::FixedTime { time_of_day, weekdays } => {
                draft.time_of_day = time_of_day.clone();
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
                draft.webhook_schedule = Some(task.schedule_raw.clone());
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
                draft.base_ref = text(strategy, "baseRef").unwrap_or_else(|| "main".to_owned());
                draft.start_from_origin = strategy.get("startFromOrigin").and_then(Value::as_bool).unwrap_or(true);
            }
        }
        draft
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

    /// `ScheduledTaskUpsertInput`, or why it cannot be sent. `to_iso` turns the local `run_at`
    /// field into an ISO time, `None` when that time does not exist; `once_supported` is the
    /// `oneTimeSchedules` capability.
    pub fn upsert_input(&self, once_supported: bool, to_iso: impl Fn(&str) -> Option<String>) -> Result<Value, Invalid> {
        let model = self.model.as_ref().filter(|_| !self.title.trim().is_empty() && !self.prompt.trim().is_empty() && !self.project_id.is_empty());
        let Some(model) = model else { return Err(invalid("Scheduled task is incomplete", "Add a title, prompt, project, and model.")) };
        let schedule = match self.schedule_mode {
            ScheduleMode::Fixed => {
                if !valid_time_of_day(self.time_of_day.trim()) {
                    return Err(invalid("Invalid run time", "Pick a date and time."));
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
                    _ => to_iso(self.run_at.trim()).ok_or(invalid("Invalid run time", "That time does not exist in your time zone."))?,
                };
                json!({ "type": "once", "runAt": run_at })
            }
            ScheduleMode::Interval => {
                let minutes = self.interval_minutes.trim().parse::<f64>().ok().filter(|minutes| minutes.is_finite());
                let every_ms = minutes.map(|minutes| (minutes * 60_000.).round()).filter(|ms| *ms >= MIN_INTERVAL_MS as f64);
                let Some(every_ms) = every_ms else { return Err(invalid("Invalid interval", "Enter an interval of at least one minute.")) };
                json!({ "type": "interval", "everyMs": every_ms as u64 })
            }
            ScheduleMode::Webhook => self.webhook_schedule.clone().unwrap_or_else(|| json!({ "type": "webhook", "signature": null, "dedupe": {} })),
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
        input.insert("workspaceStrategy".into(), workspace);
        input.insert("modelSelection".into(), json!(model));
        input.insert("runtimeMode".into(), json!(self.runtime_mode));
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
