//! Thread-level notices around the transcript. The banner lane above it: the thread error
//! (`threadErrorSummary`, `latestRootProviderFailure`, `usageLimitRunPresentedAsLatest` in
//! `shared/src/orchestrationV2ThreadError.ts`, `getThreadErrorBannerMessage` in
//! `ThreadErrorBanner.tsx`) and the provider status (`ProviderStatusBanner.tsx`). The usage-limit
//! recovery banner over the composer (`UsageLimitRecoveryBanner.tsx`). The lines after the last
//! row (`ThreadActivityBanners.tsx`, `useThreadLifecycle.tsx`): Stop, goal, and lifecycle status
//! (`presentRunStop`, `presentProviderGoal` in `client-runtime/src/state/threadExecution.ts`).

use serde::Deserialize;
use serde_json::{Value, json};

use super::{TimestampLocale, format_duration, format_upcoming, parse_iso_ms};
use crate::client_settings::TimestampFormat;
use crate::sidebar::time::Zone;
use crate::providers::driver_label;
use crate::server_config::ServerProvider;
use crate::shell::ThreadShell;
use crate::sidebar::settled::{effective_snoozed, is_woke_unseen, snooze_wake_label, woke_at};
use crate::sidebar::thread::SidebarThread;
use crate::thread::{Run, ThreadProjection};
use crate::turn_item::{ProviderFailure, TurnItemKind};

pub const CHATGPT_USAGE_LIMIT_CODE: &str = "subscription_sharing_usage_limit_exceeded";
pub const CHATGPT_WORKSPACE_USAGE_LIMIT_CODE: &str = "subscription_sharing_workplace_individual_usage_limit_exceeded";
pub const CHATGPT_WORKSPACE_ADMIN_REQUEST: &str = "I've reached the limit for using my ChatGPT plan in T3 Code. Could you increase our workspace's usage limits so I can continue?\n\nGo to admin.openai.com → Workspace → Usage limits. Thank you!";
pub const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/#settings/Usage";

const ACTIVITY_RUN_STATUSES: [&str; 4] = ["preparing", "starting", "running", "waiting"];
const INTERRUPTIBLE_RUN_STATUSES: [&str; 3] = ["preparing", "starting", "running"];

fn run_end(run: &Run) -> i64 {
    run.completed_at.as_deref().and_then(parse_iso_ms).unwrap_or(i64::MAX)
}

/// `runRanAfter`: held queues can run out of ordinal order; unfinished runs count as latest.
fn run_ran_after(run: &Run, other: &Run) -> bool {
    let (a, b) = (run_end(run), run_end(other));
    if a == b { run.ordinal > other.ordinal } else { a > b }
}

/// `latestExecutedRun`.
fn latest_executed_run(runs: &[Run]) -> Option<&Run> {
    runs.iter()
        .filter(|run| run.status != "queued" && !(run.status == "cancelled" && run.started_at.is_none()))
        .fold(None, |latest: Option<&Run>, run| match latest {
            Some(latest) if !run_ran_after(run, latest) => Some(latest),
            _ => Some(run),
        })
}

/// `latestUnheldRun`.
fn latest_unheld_run(runs: &[Run]) -> Option<&Run> {
    runs.iter().filter(|run| !(run.status == "queued" && run.queue_held == Some(true))).max_by_key(|run| run.ordinal)
}

fn latest_matching_run<'a>(projection: &'a ThreadProjection, statuses: &[&str]) -> Option<&'a Run> {
    projection.runs.iter().filter(|run| statuses.contains(&run.status.as_str())).max_by_key(|run| run.ordinal)
}

/// `latestRootProviderFailure`.
fn latest_root_provider_failure<'a>(run: Option<&Run>, projection: &'a ThreadProjection) -> Option<&'a ProviderFailure> {
    let run = run.filter(|run| run.status == "failed")?;
    projection
        .turn_items
        .iter()
        .filter(|item| item.status == "failed" && item.run_id.as_deref() == Some(run.id.as_str()) && item.node_id == run.root_node_id)
        .filter_map(|item| match &item.kind {
            TurnItemKind::Error { failure, .. } => Some((item, failure)),
            _ => None,
        })
        .max_by(|(a, _), (b, _)| (parse_iso_ms(&a.updated_at), a.ordinal, a.id.as_str()).cmp(&(parse_iso_ms(&b.updated_at), b.ordinal, b.id.as_str())))
        .map(|(_, failure)| failure)
}

fn session_error(projection: &ThreadProjection) -> Option<&str> {
    projection
        .provider_sessions
        .iter()
        .rev()
        .find(|session| session.value.get("providerInstanceId").and_then(Value::as_str) == Some(projection.thread.provider_instance_id.as_str()))
        .and_then(|session| session.value.get("lastError"))
        .and_then(Value::as_str)
}

/// `threadErrorSummary`: the session's error wins; the failure's class only when it is that error.
fn error_summary(failure: Option<&ProviderFailure>, session_error: Option<&str>) -> Option<ThreadError> {
    let current = failure.filter(|failure| session_error.is_none_or(|error| error == failure.message));
    let message = session_error.map(str::to_owned).or_else(|| failure.map(|failure| failure.message.clone()))?;
    Some(ThreadError {
        message,
        class: current.map(|failure| failure.class.clone()),
        code: current.and_then(|failure| failure.code.clone()),
        reset_at: current.filter(|failure| failure.class == "usage_limit").and_then(|failure| failure.reset_at.clone()),
        runtime_failed: false,
    })
}

/// `usageLimitRunPresentedAsLatest`: a limited run stays the latest while later runs never started.
fn usage_limit_presented_run(projection: &ThreadProjection) -> Option<&Run> {
    let executed = latest_executed_run(&projection.runs).filter(|run| run.status == "failed")?;
    let summary = error_summary(latest_root_provider_failure(Some(executed), projection), session_error(projection))?;
    (summary.class.as_deref() == Some("usage_limit") && projection.runs.iter().any(|run| run.ordinal > executed.ordinal)).then_some(executed)
}

/// `presentedLatestRun`.
pub fn presented_latest_run(projection: &ThreadProjection) -> Option<&Run> {
    usage_limit_presented_run(projection).or_else(|| latest_unheld_run(&projection.runs))
}

/// The thread's runtime error, as `deriveThreadStructureRuntime` summarizes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadError {
    pub message: String,
    pub class: Option<String>,
    pub code: Option<String>,
    /// `usageLimitResetAt`.
    pub reset_at: Option<String>,
    /// The runtime's status is `failed`.
    pub runtime_failed: bool,
}

impl ThreadError {
    pub fn is_usage_limit(&self) -> bool {
        self.class.as_deref() == Some("usage_limit") || self.chatgpt_limit()
    }

    pub fn chatgpt_limit(&self) -> bool {
        matches!(self.code.as_deref(), Some(CHATGPT_USAGE_LIMIT_CODE | CHATGPT_WORKSPACE_USAGE_LIMIT_CODE))
    }

    pub fn workspace_limit(&self) -> bool {
        self.code.as_deref() == Some(CHATGPT_WORKSPACE_USAGE_LIMIT_CODE)
    }

    /// The failed-on-a-usage-limit state the recovery banner takes over from the error banner.
    pub fn limit_blocked(&self) -> bool {
        self.runtime_failed && self.class.as_deref() == Some("usage_limit")
    }

    /// `getThreadErrorBannerKey`; also the session dismissal key.
    pub fn banner_key(&self, thread_id: &str) -> String {
        format!("{thread_id}\u{0}{}", self.message)
    }
}

pub fn thread_error(projection: &ThreadProjection) -> Option<ThreadError> {
    let latest = presented_latest_run(projection);
    let mut error = error_summary(latest_root_provider_failure(latest, projection), session_error(projection))?;
    let activity = latest_matching_run(projection, &ACTIVITY_RUN_STATUSES).or(latest);
    error.runtime_failed = usage_limit_presented_run(projection).is_some() || activity.is_some_and(|run| run.status == "failed");
    Some(error)
}

/// The error banner over the transcript, unless the usage-limit recovery banner covers it
/// (`timelineThreadError` in `ChatView.tsx`).
pub fn timeline_thread_error(error: Option<ThreadError>, shell: Option<&ThreadShell>) -> Option<ThreadError> {
    error.filter(|error| !(error.limit_blocked() && shell.is_some_and(|shell| shell.latest_run_id.is_some())))
}

/// `getThreadErrorBannerMessage`: a provider's JSON error body shows its `error.message`.
pub fn error_banner_message(error: &str) -> String {
    serde_json::from_str::<Value>(error)
        .ok()
        .and_then(|payload| payload.get("error")?.get("message")?.as_str().map(str::to_owned))
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| error.to_owned())
}

/// `ExpandableText`: whether "Show full error" is offered.
pub fn error_can_expand(full: &str, collapsed: &str) -> bool {
    collapsed != full || full.chars().count() > 180 || full.contains('\n')
}

/// `usageLimitRecoveryBannerItem` over the composer while the latest run is blocked on a limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitRecovery {
    pub run_id: String,
    pub reset_at: Option<String>,
    /// Resume and snooze are offered only when the reset is after the stop.
    pub can_schedule: bool,
    pub scheduled: bool,
    pub snoozed: bool,
    pub reset_passed: bool,
}

impl LimitRecovery {
    pub const TITLE: &str = "Usage limit reached";

    pub fn id(&self) -> String {
        format!("usage-limit-recovery:{}", self.run_id)
    }

    pub fn description(&self, now_ms: i64, zone: &dyn Zone, format: TimestampFormat, locale: &dyn TimestampLocale) -> String {
        match self.reset_at.as_deref().filter(|reset| !reset.is_empty()).map(|reset| format_upcoming(reset, now_ms, zone, format, locale)) {
            Some(reset) => format!("Resets {reset}"),
            None => "Reset time unavailable; retry manually".to_owned(),
        }
    }

    /// The `thread.metadata.update` for Resume at reset / Cancel auto-resume.
    pub fn toggle_resume(&self, command_id: &str, thread_id: &str) -> Option<Value> {
        let reset_at = self.reset_at.as_deref()?;
        Some(meta_update(command_id, thread_id, json!({ "runId": self.run_id, "resetAt": reset_at, "autoResume": !self.scheduled })))
    }

    /// Snooze until reset / Wake now. Snoozing past the reset is refused with an error toast.
    pub fn toggle_snooze(&self, command_id: &str, thread_id: &str, now_ms: i64) -> Result<Value, SnoozeRefused> {
        let reset_at = self.reset_at.as_deref().ok_or(SnoozeRefused)?;
        if !self.snoozed && parse_iso_ms(reset_at).is_none_or(|reset| reset <= now_ms) {
            return Err(SnoozeRefused);
        }
        Ok(meta_update(command_id, thread_id, json!({ "runId": self.run_id, "resetAt": reset_at, "snooze": !self.snoozed })))
    }

    /// When the snooze button next changes on its own.
    pub fn deadline(&self) -> Option<i64> {
        self.reset_at.as_deref().and_then(parse_iso_ms).filter(|_| !self.reset_passed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnoozeRefused;

fn meta_update(command_id: &str, thread_id: &str, limit_recovery: Value) -> Value {
    json!({ "type": "thread.metadata.update", "commandId": command_id, "threadId": thread_id, "limitRecovery": limit_recovery })
}

pub fn limit_recovery(error: Option<&ThreadError>, shell: &ThreadShell, now_ms: i64) -> Option<LimitRecovery> {
    let error = error.filter(|error| error.limit_blocked())?;
    let run_id = shell.latest_run_id.clone()?;
    let reset_at = error.reset_at.clone();
    let stopped_at = shell.latest_run_completed_at.clone().flatten().unwrap_or_else(|| shell.updated_at.clone());
    let reset_ms = reset_at.as_deref().and_then(parse_iso_ms);
    let can_schedule = reset_ms.zip(parse_iso_ms(&stopped_at)).is_some_and(|(reset, stopped)| reset > stopped);
    let recovery = shell.limit_recovery.as_ref().filter(|recovery| recovery.run_id == run_id && Some(recovery.reset_at.as_str()) == reset_at.as_deref());
    let reset_passed = reset_ms.is_some_and(|reset| reset <= now_ms);
    let snoozed_until = shell.snoozed_until.as_deref().and_then(parse_iso_ms);
    Some(LimitRecovery {
        scheduled: recovery.is_some_and(|recovery| recovery.auto_resume),
        snoozed: recovery.is_some_and(|recovery| recovery.snooze == Some(true)) && reset_ms.is_some() && snoozed_until == reset_ms && !reset_passed,
        run_id,
        reset_at,
        can_schedule,
        reset_passed,
    })
}

/// `presentRunStop`: a Stop the provider has not finished yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStop {
    pub run_id: String,
    pub requested_at: String,
    /// The provider did not confirm the Stop in time, so Force stop is offered.
    pub unresponsive: bool,
    /// The run already ended; the Stop targeted its background work.
    pub background: bool,
}

impl RunStop {
    pub fn title(&self) -> &'static str {
        if self.unresponsive { "Agent not responding to Stop" } else { "Stopping" }
    }

    pub fn description(&self) -> Option<&'static str> {
        self.unresponsive.then_some("It may be short on memory. Force stop kills its processes.")
    }
}

/// `has_background_work`: a background Stop matters only while that work is still pending.
pub fn run_stop(projection: &ThreadProjection, has_background_work: bool) -> Option<RunStop> {
    let active = latest_matching_run(projection, &INTERRUPTIBLE_RUN_STATUSES);
    let latest = projection.runs.iter().filter(|run| run.status != "queued").max_by_key(|run| run.ordinal);
    let run = active.or(latest)?;
    let mut request = None;
    for item in projection.turn_items.iter().filter(|item| item.run_id.as_deref() == Some(run.id.as_str())) {
        match &item.kind {
            TurnItemKind::RunInterruptResult { .. } => return None,
            TurnItemKind::RunInterruptRequest { unresponsive_at, .. } => request = Some((item, unresponsive_at.is_some())),
            _ => {}
        }
    }
    let (request, unresponsive) = request?;
    if active.is_none() && !unresponsive {
        return None;
    }
    let stop = RunStop {
        run_id: run.id.clone(),
        requested_at: request.started_at.clone().unwrap_or_else(|| request.updated_at.clone()),
        unresponsive,
        background: active.is_none(),
    };
    (!stop.background || has_background_work).then_some(stop)
}

/// `formatDuration(max(1s, now - requestedAt))`, the Stop line's ticking label.
pub fn elapsed_since(started_at: &str, now_ms: i64) -> Option<String> {
    parse_iso_ms(started_at).map(|start| format_duration((now_ms - start).max(1_000) as f64))
}

/// `OrchestrationV2ProviderGoal`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderGoal {
    pub objective: String,
    pub status: String,
    #[serde(default)]
    pub tokens_used: Option<u64>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub time_used_seconds: Option<u64>,
    #[serde(default)]
    pub checks: Option<u64>,
}

/// `presentProviderGoal`, with the title and usage joined as `goalNowItem` shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalLine {
    pub title: String,
    pub objective: String,
    pub can_resume: bool,
}

fn format_goal_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    let thousands = (tokens as f64 / 1_000.).round() as u64;
    if thousands < 1_000 {
        return format!("{thousands}k");
    }
    let millions = format!("{:.1}", tokens as f64 / 1_000_000.);
    format!("{}m", millions.strip_suffix(".0").unwrap_or(&millions))
}

pub fn goal_line(goal: &Value, working: bool) -> Option<GoalLine> {
    let goal: ProviderGoal = serde_json::from_value(goal.clone()).ok()?;
    let mut usage = Vec::new();
    if let Some(used) = goal.tokens_used.filter(|used| *used > 0) {
        usage.push(match goal.token_budget {
            Some(budget) => format!("{} / {} tokens", format_goal_tokens(used), format_goal_tokens(budget)),
            None => format!("{} tokens", format_goal_tokens(used)),
        });
    }
    if let Some(seconds) = goal.time_used_seconds.filter(|seconds| *seconds >= 60) {
        usage.push(format_duration(seconds as f64 * 1_000.));
    }
    if let Some(checks) = goal.checks.filter(|checks| *checks > 0) {
        usage.push(format!("{checks} {}", if checks == 1 { "check" } else { "checks" }));
    }
    let title = match goal.status.as_str() {
        "active" if !working => "Goal set",
        "active" => "Pursuing goal",
        "paused" => "Goal paused",
        "blocked" => "Goal blocked",
        "usage_limited" => "Goal hit a usage limit",
        "budget_limited" => "Goal reached its token budget",
        "complete" => "Goal complete",
        _ => return None,
    };
    Some(GoalLine {
        title: if usage.is_empty() { title.to_owned() } else { format!("{title} · {}", usage.join(" · ")) },
        can_resume: goal.status != "active" && goal.status != "complete",
        objective: goal.objective,
    })
}

/// `formatRelativeTimeLabel`.
pub fn relative_time_label(elapsed_ms: i64) -> String {
    let minutes = elapsed_ms.max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes}m ago"),
        _ if minutes < 24 * 60 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / (24 * 60)),
    }
}

/// The line under the last row while the thread is snoozed, settled or just woke
/// (`threadStatusLine` in `useThreadLifecycle.tsx`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusLine {
    Snoozed { label: String },
    Settled { label: String },
    Woke { woke_at: i64 },
}

impl StatusLine {
    pub fn label(&self) -> &str {
        match self {
            StatusLine::Snoozed { label } | StatusLine::Settled { label } => label,
            StatusLine::Woke { .. } => "Woke from snooze",
        }
    }

    /// The action, and its label while it runs.
    pub fn action(&self) -> (&'static str, &'static str) {
        match self {
            StatusLine::Snoozed { .. } => ("Wake now", "Waking..."),
            StatusLine::Settled { .. } => ("Un-settle", "Un-settling..."),
            StatusLine::Woke { .. } => ("Dismiss", "Dismiss"),
        }
    }
}

pub fn status_line(thread: &SidebarThread, now: i64) -> Option<StatusLine> {
    if effective_snoozed(thread, now) {
        let wake = thread.snoozed_until.map(|until| snooze_wake_label(until, now)).filter(|wake| wake != "now");
        return Some(StatusLine::Snoozed { label: wake.map_or_else(|| "Snoozed".to_owned(), |wake| format!("Snoozed, {wake} left")) });
    }
    if thread.is_settled() {
        let label = thread.settled_at.map_or_else(|| "Settled".to_owned(), |at| format!("Settled {}", relative_time_label(now - at)));
        return Some(StatusLine::Settled { label });
    }
    let woke = woke_at(thread, now)?;
    let completed = thread.latest_run.as_ref().and_then(|run| run.completed_at);
    is_woke_unseen(Some(woke), false, thread.last_visited_at, completed).then_some(StatusLine::Woke { woke_at: woke })
}

/// `ProviderStatusBanner`: availability and compatibility notices for the thread's provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderBanner {
    pub instance_id: String,
    pub refresh_tooltip: String,
    pub key: String,
    pub title: String,
    pub message: String,
    pub warning: bool,
}

pub fn provider_banner(provider: &ServerProvider) -> Option<ProviderBanner> {
    if provider.status == "disabled" {
        return None;
    }
    let unauthenticated = provider.status == "error" && provider.auth.status == "unauthenticated";
    let incompatible = provider.compatibility_advisory.as_ref().filter(|advisory| {
        !unauthenticated && (advisory.status == "broken" || (provider.status == "ready" && advisory.status == "unsupported"))
    });
    if incompatible.is_none() && (provider.status == "ready" || (provider.driver == "antigravity" && provider.installed && provider.status == "warning" && provider.auth.status == "unknown")) {
        return None;
    }
    let key = match incompatible {
        Some(advisory) => [provider.instance_id.as_str(), &advisory.status, provider.version.as_deref().unwrap_or_default(), advisory.message.as_deref().unwrap_or_default()].join("\u{0}"),
        None => [provider.instance_id.as_str(), &provider.status, &provider.auth.status, provider.message.as_deref().unwrap_or_default()].join("\u{0}"),
    };
    let name = provider.display_name.as_deref().map(str::trim).filter(|name| !name.is_empty()).map_or_else(|| driver_label(&provider.driver), str::to_owned);
    let setup = provider.driver == "antigravity";
    let message = if let Some(message) = incompatible.and_then(|advisory| advisory.message.as_ref()) {
        message.clone()
    } else if let Some(message) = provider.message.as_ref().filter(|message| !message.is_empty()) {
        message.clone()
    } else if !provider.installed && setup {
        format!("Open provider setup to install {} on this environment.", driver_label(&provider.driver))
    } else if provider.auth.status == "unauthenticated" {
        if setup { "Open provider setup to sign in with Google.".into() } else { "Sign in via the CLI to authenticate again.".into() }
    } else if provider.status == "ready" {
        "No models are available for this provider.".into()
    } else if provider.status == "error" {
        format!("{name} provider is unavailable.")
    } else {
        format!("{name} provider has limited availability.")
    };
    Some(ProviderBanner {
        instance_id: provider.instance_id.clone(),
        refresh_tooltip: format!("Refresh {name} provider status"),
        key,
        title: if unauthenticated {
            format!("{name} is unauthenticated")
        } else if let Some(advisory) = incompatible {
            let version = provider.version.as_deref().unwrap_or_default();
            let status = if advisory.status == "broken" { "known to be broken" } else { "unsupported" };
            format!("{name} {version} is {status}")
        } else {
            format!("{name} provider status")
        },
        message,
        warning: incompatible.map_or(provider.status == "warning", |advisory| advisory.status != "broken"),
    })
}
