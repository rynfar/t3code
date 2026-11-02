//! Thread-level inputs for row derivation, from `packages/client-runtime/src/state/threadExecution.ts`
//! (`deriveThreadActivityRun`, `deriveThreadStructureRuntime`), `threadRequests.ts`
//! (pending approvals), plus `shouldCollapseUserMessage` (`MessagesTimeline.logic.ts`),
//! `resolveUserMessagePresentation` (`userMessage.ts`) and `proposedPlanText.ts`.

use std::collections::HashSet;

use serde_json::Value;

use super::ChatMessage;
use crate::thread::{Entity, Run, ThreadProjection};

/// `TimelineLatestRun`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LatestRun {
    pub run_id: String,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    /// The run's latest provider turn finished while the run itself is still open.
    pub turn_ended: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThreadActivity {
    pub latest_run: Option<LatestRun>,
    pub running_run_id: Option<String>,
    pub is_working: bool,
    /// A provider-native subagent thread working through its runless root turn.
    pub runless_work_active: bool,
    pub active_turn_started_at: Option<String>,
    pub pending_approval_ids: HashSet<String>,
}

const ACTIVITY: [&str; 4] = ["preparing", "starting", "running", "waiting"];
const INTERRUPTIBLE: [&str; 3] = ["preparing", "starting", "running"];

fn latest_matching(runs: &[Run], predicate: impl Fn(&Run) -> bool) -> Option<&Run> {
    runs.iter().filter(|run| predicate(run)).max_by_key(|run| run.ordinal)
}

/// `summarizeThreadRun`: an open run whose latest provider turn has stopped reports `turnEnded`.
fn summarize(run: &Run, provider_turns: &[Entity]) -> LatestRun {
    let turn_status = ACTIVITY.contains(&run.status.as_str()).then(|| {
        let attempt = run.active_attempt_id.as_deref()?;
        provider_turns
            .iter()
            .filter(|turn| turn.value.get("runAttemptId").and_then(Value::as_str) == Some(attempt))
            .max_by_key(|turn| turn.value.get("ordinal").and_then(Value::as_u64).unwrap_or(0))
            .and_then(|turn| turn.value.get("status").and_then(Value::as_str))
    });
    LatestRun {
        run_id: run.id.clone(),
        status: run.status.clone(),
        started_at: run.started_at.clone(),
        completed_at: run.completed_at.clone(),
        turn_ended: matches!(turn_status.flatten(), Some(status) if status != "pending" && status != "running"),
    }
}

/// `deriveRunlessWorkStartedAt`: provider-native subagent threads have no app runs; their work is
/// a runless root turn whose status follows the subagent.
fn runless_work_started_at(projection: &ThreadProjection) -> Option<String> {
    let thread = &projection.thread;
    if thread.lineage.relationship_to_parent.as_deref() != Some("subagent") || thread.creation_source.as_deref() != Some("provider") {
        return None;
    }
    let node = projection.nodes.iter().rev().find(|node| {
        node.value.get("kind").and_then(Value::as_str) == Some("root_turn") && node.value.get("runId").is_none_or(Value::is_null)
    })?;
    let active = matches!(node.value.get("status").and_then(Value::as_str), Some("pending" | "running" | "waiting"));
    active.then(|| node.value.get("startedAt").and_then(Value::as_str).unwrap_or_default().to_owned())
}

/// The run the timeline treats as current and whether the thread is working. Held queues are
/// not on the native wire model yet, so every run counts as unheld.
pub fn derive_activity(projection: &ThreadProjection) -> ThreadActivity {
    let runs = &projection.runs;
    let activity_run = latest_matching(runs, |run| ACTIVITY.contains(&run.status.as_str()))
        .or_else(|| latest_matching(runs, |_| true));
    let running_run_id = latest_matching(runs, |run| INTERRUPTIBLE.contains(&run.status.as_str())).map(|run| run.id.clone());
    let live = latest_matching(runs, |run| ACTIVITY.contains(&run.status.as_str()));
    let runless = runless_work_started_at(projection);
    let is_working = live.is_some() || runless.is_some();
    let active_turn_started_at =
        live.map(|run| run.started_at.clone().unwrap_or_else(|| run.requested_at.clone())).or_else(|| runless.clone().filter(|at| !at.is_empty()));
    let pending_approval_ids = projection
        .runtime_requests
        .iter()
        .filter(|request| {
            let get = |key: &str| request.value.get(key).and_then(Value::as_str);
            get("status") == Some("pending") && !matches!(get("kind"), Some("user_input" | "auth_refresh" | "dynamic_tool_call"))
        })
        .map(|request| request.id.clone())
        .collect();
    ThreadActivity {
        latest_run: activity_run.map(|run| summarize(run, &projection.provider_turns)),
        running_run_id,
        is_working,
        runless_work_active: runless.is_some(),
        active_turn_started_at,
        pending_approval_ids,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    Automation,
    Agent,
    T3Code,
    Guest,
}

/// `resolveUserMessagePresentation(...).attribution`.
pub fn user_message_attribution(message: &ChatMessage) -> Option<Attribution> {
    if message.meta.guest_name.is_some() || message.meta.guest_text.is_some() {
        return Some(Attribution::Guest);
    }
    if message.meta.scheduled_task_id.is_some() || message.text.starts_with("[Triggered by schedule task: ") {
        return Some(Attribution::Automation);
    }
    match (message.meta.created_by.as_deref(), message.meta.creation_source.as_deref()) {
        (Some("agent"), Some("server")) => Some(Attribution::T3Code),
        (Some("agent"), _) => Some(Attribution::Agent),
        _ => None,
    }
}

const MAX_COLLAPSED_LINES: usize = 8;
const MAX_COLLAPSED_LENGTH: usize = 600;

/// Markdown links count by their label: `visibleUserMessageText`.
fn visible_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let label_start = open + 1;
        let Some(close) = rest[label_start..].find(']').map(|i| label_start + i) else { break };
        let label = &rest[label_start..close];
        let after = &rest[close + 1..];
        let target_end = after.strip_prefix('(').and_then(|target| target.find(')').filter(|end| !target[..*end].contains([' ', '\n'])));
        match target_end {
            Some(end) if !label.contains('\n') => {
                let prefix = rest[..open].strip_suffix('!').unwrap_or(&rest[..open]);
                out.push_str(prefix);
                out.push_str(label);
                rest = &after[end + 2..];
            }
            _ => {
                out.push_str(&rest[..label_start]);
                rest = &rest[label_start..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `shouldCollapseUserMessage`.
pub fn should_collapse_user_message(text: &str) -> bool {
    let visible = visible_text(text);
    !visible.trim().is_empty() && (visible.chars().count() > MAX_COLLAPSED_LENGTH || visible.split('\n').count() > MAX_COLLAPSED_LINES)
}

fn heading_text(line: &str) -> Option<&str> {
    let trimmed = line.strip_prefix("   ").or_else(|| line.strip_prefix("  ")).or_else(|| line.strip_prefix(' ')).unwrap_or(line);
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &trimmed[hashes..];
    rest.starts_with([' ', '\t']).then(|| rest.trim()).filter(|text| !text.is_empty())
}

/// `proposedPlanTitle`.
pub fn plan_title(markdown: &str) -> Option<String> {
    markdown.lines().find_map(heading_text).map(str::to_owned)
}

/// `stripDisplayedPlanMarkdown`: drops the title heading and a leading "Summary" heading.
pub fn strip_displayed_plan_markdown(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.trim_end().lines().collect();
    let mut start = usize::from(lines.first().is_some_and(|line| heading_text(line).is_some()));
    while lines.get(start).is_some_and(|line| line.trim().is_empty()) {
        start += 1;
    }
    if lines.get(start).and_then(|line| heading_text(line)).is_some_and(|text| text.eq_ignore_ascii_case("summary")) {
        start += 1;
        while lines.get(start).is_some_and(|line| line.trim().is_empty()) {
            start += 1;
        }
    }
    lines.get(start..).map(|rest| rest.join("\n")).unwrap_or_default()
}
