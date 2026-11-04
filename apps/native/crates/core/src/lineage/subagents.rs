//! A thread's subagents and workflows. Ports `client-runtime/src/state/threadSubagents.ts`,
//! the workflow parts of `subagentRuntime.ts`, `subagentDisplay.ts`, and
//! `deriveProviderSubagentStatus` / `formatProviderSubagentStatus` from `threadExecution.ts`.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::thread::{Run, ThreadProjection};
use crate::timeline::{format_duration, parse_iso_ms};

/// `OrchestrationV2Subagent`, as far as the client reads it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subagent {
    pub id: String,
    pub thread_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub child_thread_id: Option<String>,
    #[serde(default)]
    pub remote_child: Option<Value>,
    #[serde(default)]
    pub native_task_ref: Option<Value>,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub status: String,
    #[serde(default)]
    pub progress: Option<String>,
    #[serde(default)]
    pub workflow: Option<Workflow>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub phases: Vec<WorkflowPhase>,
    #[serde(default)]
    pub agents: Vec<WorkflowAgent>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WorkflowPhase {
    pub index: u32,
    pub title: String,
}

/// `OrchestrationV2WorkflowAgent`; its timestamps are epoch milliseconds.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgent {
    pub index: u32,
    pub label: String,
    #[serde(default)]
    pub child_thread_id: Option<String>,
    pub state: String,
    #[serde(default)]
    pub phase_index: Option<u32>,
    #[serde(default)]
    pub phase_title: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
}

/// The projection's subagents; a record this build cannot read is skipped.
pub fn subagents_of(projection: &ThreadProjection) -> Vec<Subagent> {
    projection.subagents.iter().filter_map(|entity| Subagent::deserialize(&entity.value).ok()).collect()
}

/// `isActiveSubagentStatus` (`isOrchestrationV2WorkActive`).
pub fn is_active_status(status: &str) -> bool {
    matches!(status, "pending" | "running" | "waiting")
}

/// `isTerminalSubagentStatus`.
pub fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

fn millis(value: Option<&str>) -> i64 {
    value.and_then(parse_iso_ms).unwrap_or(0)
}

/// `ThreadTurnSubagents`: the agents of the turn a thread shows.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSubagents {
    pub run_id: Option<String>,
    pub turn_active: bool,
    /// No later turn has run since these agents' turn.
    pub latest_turn: bool,
    pub subagents: Vec<Subagent>,
    pub live_count: usize,
    pub settled_count: usize,
}

/// `deriveThreadTurnSubagents`.
pub fn turn_subagents(runs: &[Run], subagents: &[Subagent]) -> Option<TurnSubagents> {
    let rolled_back: Vec<&str> = runs.iter().filter(|run| run.status == "rolled_back").map(|run| run.id.as_str()).collect();
    let eligible: Vec<&Subagent> = subagents.iter().filter(|agent| agent.run_id.as_deref().is_none_or(|id| !rolled_back.contains(&id))).collect();
    let first = *eligible.first()?;
    let active_run = runs.iter().rev().find(|run| matches!(run.status.as_str(), "preparing" | "starting" | "running" | "waiting"));
    let latest_updated = eligible.iter().fold(first, |latest, agent| if millis(Some(&agent.updated_at)) > millis(Some(&latest.updated_at)) { agent } else { latest });
    // Ordinal, not position: a reordered queue can leave runs out of order.
    let latest_started = runs
        .iter()
        .filter(|run| run.started_at.is_some() && !rolled_back.contains(&run.id.as_str()))
        .fold(None::<&Run>, |latest, run| if latest.is_none_or(|latest| run.ordinal > latest.ordinal) { Some(run) } else { latest });
    let latest_started_has_agents = latest_started.is_some_and(|run| eligible.iter().any(|agent| agent.run_id.as_deref() == Some(run.id.as_str())));
    let run_id = match (active_run, latest_started) {
        (Some(run), _) => Some(run.id.clone()),
        (None, Some(run)) if latest_started_has_agents => Some(run.id.clone()),
        _ => latest_updated.run_id.clone(),
    };
    let mut members: Vec<Subagent> = eligible
        .into_iter()
        .filter(|agent| agent.run_id == run_id || is_active_status(&agent.status) || agent.workflow.is_some())
        .cloned()
        .collect();
    if members.is_empty() {
        return None;
    }
    let order = |agent: &Subagent| millis(agent.started_at.as_deref().or(Some(&agent.updated_at)));
    members.sort_by(|left, right| order(left).cmp(&order(right)).then_with(|| left.id.cmp(&right.id)));
    let live_count = members.iter().filter(|agent| is_active_status(&agent.status)).count();
    let settled_count = members.iter().filter(|agent| is_terminal_status(&agent.status)).count();
    Some(TurnSubagents {
        latest_turn: run_id.is_none() || latest_started.is_none_or(|run| run_id.as_deref() == Some(run.id.as_str())),
        run_id,
        turn_active: active_run.is_some(),
        subagents: members,
        live_count,
        settled_count,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PillSegment {
    pub label: String,
    pub accessibility_label: String,
}

/// `resolveSubagentPillSegment`: finished agents stay reachable until a later turn replaces
/// them; workflows always stay.
pub fn subagent_pill(turn: Option<&TurnSubagents>) -> Option<PillSegment> {
    let turn = turn?;
    if !turn.turn_active && !turn.latest_turn && turn.live_count == 0 && !turn.subagents.iter().any(|agent| agent.workflow.is_some()) {
        return None;
    }
    let total = turn.subagents.len();
    if turn.live_count > 0 {
        return Some(PillSegment { label: format!("{}/{total}", turn.live_count), accessibility_label: format!("{} of {total} agents working", turn.live_count) });
    }
    let noun = if turn.settled_count == 1 { "agent" } else { "agents" };
    Some(PillSegment { label: format!("{} done", turn.settled_count), accessibility_label: format!("{} {noun} done", turn.settled_count) })
}

/// A workflow member as `workflowMembersToRuntime` sees it: its status follows a coordinator
/// that ended while it was still queued or running.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowMember {
    pub index: u32,
    pub label: String,
    pub status: String,
    pub phase_index: Option<u32>,
    pub child_thread_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseState {
    Pending,
    Running,
    Done,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowPhaseGroup {
    pub index: u32,
    pub title: String,
    pub members: Vec<WorkflowMember>,
    pub state: PhaseState,
    pub settled_count: usize,
}

/// `SubagentWorkflowGroup` for one coordinator.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowGroup {
    pub coordinator_id: String,
    pub name: Option<String>,
    pub status: String,
    pub phases: Vec<WorkflowPhaseGroup>,
    pub unphased: Vec<WorkflowMember>,
}

/// `deriveWorkflowGroups(projectedSubagentsToRuntime([coordinator]))` for a coordinator.
pub fn workflow_group(coordinator: &Subagent) -> Option<WorkflowGroup> {
    let workflow = coordinator.workflow.as_ref()?;
    let members: Vec<WorkflowMember> = workflow
        .agents
        .iter()
        .map(|agent| {
            let status = if matches!(agent.state.as_str(), "queued" | "running") && is_terminal_status(&coordinator.status) {
                coordinator.status.clone()
            } else if agent.state == "queued" {
                "pending".to_owned()
            } else {
                agent.state.clone()
            };
            WorkflowMember { index: agent.index, label: agent.label.clone(), status, phase_index: agent.phase_index, child_thread_id: agent.child_thread_id.clone() }
        })
        .collect();
    // The declared plan lags its members: members seed the titles, declared ones overwrite.
    let mut titles: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
    for agent in &workflow.agents {
        if let Some(index) = agent.phase_index {
            let title = agent.phase_title.clone().or_else(|| titles.get(&index).cloned()).unwrap_or_else(|| format!("Phase {}", index + 1));
            titles.insert(index, title);
        }
    }
    for phase in &workflow.phases {
        titles.insert(phase.index, phase.title.clone());
    }
    let phases = titles
        .into_iter()
        .map(|(index, title)| {
            let mut phase_members: Vec<WorkflowMember> = members.iter().filter(|member| member.phase_index == Some(index)).cloned().collect();
            phase_members.sort_by_key(|member| member.index);
            // An idle member is resumable, so its phase is not done.
            let live = phase_members.iter().any(|member| is_active_status(&member.status) || member.status == "idle");
            let settled_count = phase_members.iter().filter(|member| is_terminal_status(&member.status)).count();
            let state = if phase_members.is_empty() {
                PhaseState::Pending
            } else if live {
                PhaseState::Running
            } else {
                PhaseState::Done
            };
            WorkflowPhaseGroup { index, title, members: phase_members, state, settled_count }
        })
        .collect();
    let mut unphased: Vec<WorkflowMember> = members.into_iter().filter(|member| member.phase_index.is_none()).collect();
    unphased.sort_by_key(|member| member.index);
    Some(WorkflowGroup { coordinator_id: coordinator.id.clone(), name: workflow.name.clone(), status: coordinator.status.clone(), phases, unphased })
}

/// `formatSubagentDisplayTitle`: drops a `Subagent:` prefix and names a `/root/...` path by its
/// last segment.
pub fn subagent_display_title(title: &str) -> String {
    let display = match title.get(..9) {
        Some(prefix) if prefix.eq_ignore_ascii_case("subagent:") => title[9..].trim_start(),
        _ => title,
    };
    let Some(path) = display.strip_prefix("/root/") else { return display.to_owned() };
    let path = path.strip_suffix('/').unwrap_or(path);
    let Some(name) = path.rsplit('/').next().filter(|name| !name.is_empty()) else { return display.to_owned() };
    let words: Vec<String> = name
        .split(|c: char| c == '_' || c.is_whitespace())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        })
        .collect();
    if words.is_empty() { display.to_owned() } else { words.join(" ") }
}

/// `ProviderSubagentStatus`: a provider-native subagent thread's runless root turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSubagentStatus {
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// `isProviderNativeSubagentThread`.
pub fn is_provider_native_subagent(lineage: &crate::shell::Lineage, creation_source: Option<&str>) -> bool {
    lineage.relationship_to_parent.as_deref() == Some("subagent") && creation_source == Some("provider")
}

/// `deriveProviderSubagentStatus`.
pub fn provider_subagent_status(projection: &ThreadProjection) -> Option<ProviderSubagentStatus> {
    let thread = &projection.thread;
    if !is_provider_native_subagent(&thread.lineage, thread.creation_source.as_deref()) {
        return None;
    }
    let node = projection.nodes.iter().rev().map(|entity| &entity.value).find(|node| {
        node.get("kind").and_then(Value::as_str) == Some("root_turn") && node.get("runId").is_none_or(Value::is_null)
    })?;
    let text = |key: &str| node.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(ProviderSubagentStatus { status: text("status")?, started_at: text("startedAt"), completed_at: text("completedAt") })
}

fn status_label(status: &str) -> &'static str {
    match status {
        "idle" => "Idle",
        "pending" | "running" => "Working",
        "waiting" => "Waiting",
        "completed" => "Completed",
        "interrupted" => "Interrupted",
        "failed" => "Failed",
        _ => "Cancelled",
    }
}

/// `formatProviderSubagentStatus`: "Working 1m 5s" while live, "Completed in 42s" once done.
pub fn format_provider_subagent_status(status: Option<&ProviderSubagentStatus>, now_ms: i64) -> String {
    let Some(status) = status else { return "Starting".to_owned() };
    let label = status_label(&status.status);
    let live = is_active_status(&status.status);
    if !live && status.status != "completed" {
        return label.to_owned();
    }
    let start = status.started_at.as_deref().and_then(parse_iso_ms);
    let end = if live { Some(now_ms) } else { status.completed_at.as_deref().and_then(parse_iso_ms) };
    let (Some(start), Some(end)) = (start, end) else { return label.to_owned() };
    let elapsed = format_duration(((end - start) / 1_000 * 1_000).max(1_000) as f64);
    if live { format!("{label} {elapsed}") } else { format!("{label} in {elapsed}") }
}

pub const GET_NATIVE_CHILD_WORK: &str = "orchestration.getNativeChildWork";

pub fn native_child_work_request(thread_id: &str) -> Value {
    json!({ "threadId": thread_id })
}

/// `OrchestrationV2NativeChildStep`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeChildStep {
    pub index: u32,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<NativeToolCall>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NativeToolCall {
    pub name: String,
    pub input: String,
}

/// `OrchestrationV2NativeChildWork`: read only; native children cannot take messages.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeChildWork {
    pub conversation_id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub steps: Option<Vec<NativeChildStep>>,
    #[serde(default)]
    pub truncated: bool,
}

pub fn decode_native_child_work(value: Value) -> Result<Vec<NativeChildWork>, String> {
    #[derive(Deserialize)]
    struct Reply {
        children: Vec<NativeChildWork>,
    }
    serde_json::from_value::<Reply>(value).map(|reply| reply.children).map_err(|error| error.to_string())
}

/// `stepLabel` in `NativeChildWorkDialog.tsx`: `TOOL_CALL` reads "Tool call".
pub fn native_step_label(kind: &str) -> String {
    let words = kind.to_lowercase().replace('_', " ");
    let mut chars = words.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

impl NativeChildStep {
    /// "Tool call", or "Tool call · Running" while not done.
    pub fn heading(&self) -> String {
        match self.status.as_deref() {
            Some(status) if status != "DONE" => format!("{} · {}", native_step_label(&self.kind), native_step_label(status)),
            _ => native_step_label(&self.kind),
        }
    }
}

const EFFORT_OPTION_IDS: [&str; 6] = ["reasoningEffort", "effort", "reasoning", "variant", "reasoning_effort", "thinking_level"];

/// `formatModelSelectionEffort`: the effort a subagent runs at, from the selection's explicit
/// choice, else what the provider reported for the same model. A value the catalog lacks is
/// not shown as its default (`getProviderOptionCurrentLabel`).
pub fn effort_label(
    selection: &crate::model::ModelSelection,
    model: Option<&crate::server_config::ProviderModel>,
    reported: Option<&crate::model::ModelSelection>,
) -> Option<String> {
    use crate::server_config::OptionDescriptor;
    use crate::traits::{OptionValue, selections};
    let descriptors = &model?.capabilities.as_ref()?.option_descriptors;
    let explicit = selections(selection.options.as_ref());
    let reported = reported.filter(|reported| reported.instance_id == selection.instance_id && reported.model == selection.model).map(|reported| selections(reported.options.as_ref()));
    for id in EFFORT_OPTION_IDS {
        let Some(OptionDescriptor::Select(descriptor)) = descriptors.iter().find(|descriptor| descriptor.id() == id) else { continue };
        let chosen = explicit.iter().find(|option| option.id == id).map(|option| &option.value);
        // `getReportedOptionValue`: only for options the selection leaves unstated.
        let reported = reported.as_ref().filter(|_| chosen.is_none()).and_then(|options| options.iter().find(|option| option.id == id)).map(|option| &option.value);
        let value = match chosen.or(reported) {
            Some(OptionValue::Bool(_)) => continue,
            Some(OptionValue::Str(value)) => Some(value.as_str()),
            None if id == "variant" => None,
            None => continue,
        };
        let current = match reported {
            Some(OptionValue::Str(value)) => Some(value.as_str()),
            _ if id == "variant" && chosen.is_none() => None,
            _ => value.filter(|value| !value.is_empty()).or_else(|| descriptor.options.iter().find(|option| option.is_default).map(|option| option.id.as_str())),
        };
        let label = descriptor.options.iter().find(|option| Some(option.id.as_str()) == current).map(|option| option.label.clone()).or_else(|| {
            if matches!(reported, Some(OptionValue::Str(value)) if value == "default") {
                Some("Default".to_owned())
            } else {
                (id == "variant").then(|| "Unknown".to_owned())
            }
        });
        if label.is_some() {
            return label;
        }
    }
    None
}

/// `deriveReportedModelSelection`: what the provider says the thread runs on.
pub fn reported_model_selection(projection: &ThreadProjection) -> Option<crate::model::ModelSelection> {
    let thread = &projection.thread;
    projection
        .provider_threads
        .iter()
        .map(|entity| &entity.value)
        .find(|provider_thread| {
            provider_thread.get("id").and_then(Value::as_str) == thread.active_provider_thread_id.as_deref()
                && provider_thread.get("providerInstanceId").and_then(Value::as_str) == Some(thread.model_selection.instance_id.as_str())
        })
        .and_then(|provider_thread| provider_thread.pointer("/nativeMetadata/modelSelection"))
        .and_then(|selection| serde_json::from_value(selection.clone()).ok())
}

impl Subagent {
    /// `currentSubagent` in `ThreadRelationshipsControl.tsx`: a finished task whose child thread
    /// runs again, or ran since, takes that run's status and times.
    pub fn current(&self, child: Option<&crate::shell::ThreadShell>) -> Subagent {
        let Some(child) = child.filter(|_| !is_active_status(&self.status)) else { return self.clone() };
        let live = child.activity_run_status.clone().flatten().or_else(|| (child.status == "queued").then(|| "queued".to_owned()));
        let since = self.completed_at.as_deref().or(self.started_at.as_deref()).unwrap_or(&self.updated_at);
        let newer = child.latest_run_requested_at.as_deref().and_then(parse_iso_ms).zip(parse_iso_ms(since)).is_some_and(|(run, task)| run > task);
        if live.is_none() && !newer {
            return self.clone();
        }
        let (started_at, completed_at) = match live {
            Some(_) => (child.activity_run_started_at.clone().flatten(), None),
            None => (child.latest_run_started_at.clone(), child.latest_run_completed_at.clone().flatten()),
        };
        let status = match live.as_deref().unwrap_or(&child.status) {
            "preparing" | "starting" | "queued" => "pending",
            "rolled_back" => "interrupted",
            status => status,
        };
        Subagent { status: status.to_owned(), started_at, completed_at, progress: None, result: None, ..self.clone() }
    }

    /// `canStop` in `ThreadRelationshipsControl.tsx`: app-owned tasks, and Claude's native tasks
    /// with a strong reference, once started and while they work. `child` is the task's thread,
    /// which may run again after the task finished.
    pub fn can_stop(&self, child: Option<&crate::shell::ThreadShell>) -> bool {
        let current = self.current(child);
        let strong = self.native_task_ref.as_ref().is_some_and(|reference| {
            reference.get("strength").and_then(Value::as_str) == Some("strong") && reference.get("nativeId").is_some_and(|id| !id.is_null())
        });
        let stoppable = self.origin.as_deref() == Some("app_owned")
            || (self.origin.as_deref() == Some("provider_native")
                && self.driver.as_deref() == Some("claudeAgent")
                && self.run_id.is_some()
                && strong
                && is_active_status(&self.status));
        stoppable && current.started_at.is_some() && is_active_status(&current.status)
    }

    /// `canStopWorkflow`: a running Claude workflow coordinator.
    pub fn can_stop_workflow(&self) -> bool {
        self.workflow.is_some() && self.status == "running" && self.origin.as_deref() == Some("provider_native") && self.driver.as_deref() == Some("claudeAgent")
    }
}
