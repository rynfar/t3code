//! The sidebar's view of a thread shell: `presentThreadShell` and `shellRuntime`
//! (`client-runtime/src/state/models.ts`) plus the subagent request roll-up
//! (`client-runtime/src/state/threadShell.ts`). Timestamps are parsed once, here.

use std::collections::HashMap;

use super::time::{Ms, parse_iso, parse_opt};
use crate::shell::{PendingBackgroundTask, ThreadShell};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentRequest {
    Approval,
    UserInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub run_id: String,
    pub status: String,
    pub requested_at: Option<Ms>,
    pub started_at: Option<Ms>,
    pub completed_at: Option<Ms>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSummary {
    /// A run status or `"idle"`.
    pub status: String,
    pub active_run_id: Option<String>,
    /// `None` when the server does not send the field.
    pub activity_started_at: Option<Option<Ms>>,
    pub last_error: Option<String>,
    pub last_error_class: Option<String>,
    pub updated_at: Option<Ms>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettledOverride {
    Settled,
    Active,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SidebarThread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub interaction_mode: String,
    pub listed: bool,
    pub latest_run: Option<RunSummary>,
    pub runtime: Option<RuntimeSummary>,
    pub blocked_on_thread_wait: bool,
    pub latest_user_message_at: Option<Ms>,
    /// `None` when the server predates the field.
    pub latest_user_authored_message_at: Option<Option<Ms>>,
    pub has_pending_approvals: bool,
    pub has_pending_user_input: bool,
    pub pending_user_input_at: Option<Ms>,
    pub has_pending_runtime_request: bool,
    pub pending_subagent_request: Option<SubagentRequest>,
    pub pending_guest_message_count: u64,
    pub has_actionable_proposed_plan: bool,
    pub background_holds_completion: bool,
    pub goal_active: bool,
    pub created_at: Option<Ms>,
    pub updated_at: Option<Ms>,
    pub archived: bool,
    pub deleted: bool,
    pub settled_override: Option<SettledOverride>,
    pub settled_at: Option<Ms>,
    pub unsettled_at: Option<Ms>,
    pub snoozed_until: Option<Ms>,
    pub snoozed_at: Option<Ms>,
    pub pinned: bool,
    pub pin_order_key: Option<String>,
    pub active_order_key: Option<String>,
    pub auto_settle_disabled: bool,
    pub last_visited_at: Option<Ms>,
    pub regenerating_title: bool,
    pub opens: Option<crate::search::ThreadOpenStats>,
}

impl SidebarThread {
    pub fn is_settled(&self) -> bool {
        self.settled_override == Some(SettledOverride::Settled)
    }

    /// `threadRuntimeCanArchive`: archive rejects attached providers.
    pub fn can_archive(&self) -> bool {
        match self.runtime.as_ref().map(|runtime| runtime.status.as_str()) {
            Some("queued") => self.runtime.as_ref().is_some_and(|runtime| runtime.active_run_id.is_none()),
            Some("preparing" | "starting" | "running") => false,
            _ => true,
        }
    }
}

/// `backgroundWorkHoldsCompletion`: commands and monitors hold only when awaited.
pub fn background_work_holds_completion(tasks: &[PendingBackgroundTask]) -> bool {
    tasks.iter().any(|task| task.awaited == Some(true) || matches!(task.kind.as_str(), "subagent" | "background_task"))
}

fn terminal_run_status(status: &str) -> bool {
    matches!(status, "completed" | "interrupted" | "failed" | "cancelled" | "rolled_back")
}

fn shell_runtime(thread: &ThreadShell) -> Option<RuntimeSummary> {
    let park_at_idle = background_work_holds_completion(&thread.pending_background_tasks) && thread.status != "failed";
    if thread.latest_run_id.is_none() && thread.active_provider_thread_id.is_none() && !park_at_idle {
        return None;
    }
    let status = if park_at_idle {
        "idle".to_owned()
    } else {
        thread.activity_run_status.clone().flatten().unwrap_or_else(|| thread.status.clone())
    };
    Some(RuntimeSummary {
        status,
        active_run_id: thread.active_run_id.clone(),
        activity_started_at: thread.activity_run_started_at.as_ref().map(|value| parse_opt(value.as_deref())),
        last_error: thread.last_error.clone(),
        last_error_class: thread.last_error_class.clone(),
        updated_at: parse_iso(&thread.updated_at),
    })
}

pub fn present_thread(thread: &ThreadShell, pending_subagent_request: Option<SubagentRequest>) -> SidebarThread {
    let updated_at = parse_iso(&thread.updated_at);
    let latest_run = thread.latest_run_id.as_ref().map(|run_id| RunSummary {
        run_id: run_id.clone(),
        status: if thread.status == "idle" { "completed".to_owned() } else { thread.status.clone() },
        requested_at: parse_opt(thread.latest_run_requested_at.as_deref()),
        started_at: parse_opt(thread.latest_run_started_at.as_deref()),
        completed_at: match &thread.latest_run_completed_at {
            None if thread.status == "idle" || terminal_run_status(&thread.status) => updated_at,
            None => None,
            Some(value) => parse_opt(value.as_deref()),
        },
    });
    let request_kind = thread.pending_runtime_request.as_ref().map(|request| request.kind.as_str());
    SidebarThread {
        id: thread.id.clone(),
        project_id: thread.project_id.clone(),
        title: thread.title.clone(),
        branch: thread.branch.clone(),
        worktree_path: thread.worktree_path.clone(),
        interaction_mode: thread.interaction_mode.clone(),
        listed: thread.lineage.is_listed(),
        latest_run,
        runtime: shell_runtime(thread),
        blocked_on_thread_wait: thread.blocked_on_thread_wait == Some(true),
        latest_user_message_at: parse_opt(thread.latest_user_message_at.as_deref()),
        latest_user_authored_message_at: thread.latest_user_authored_message_at.as_ref().map(|value| parse_opt(value.as_deref())),
        has_pending_approvals: request_kind.is_some_and(|kind| kind != "user_input" && kind != "auth_refresh"),
        has_pending_user_input: request_kind == Some("user_input"),
        pending_user_input_at: thread.pending_runtime_request.as_ref().filter(|request| request.kind == "user_input").and_then(|request| parse_opt(request.created_at.as_deref())),
        has_pending_runtime_request: request_kind.is_some(),
        pending_subagent_request,
        pending_guest_message_count: thread.pending_guest_message_count.unwrap_or(0),
        has_actionable_proposed_plan: thread.has_actionable_proposed_plan,
        background_holds_completion: background_work_holds_completion(&thread.pending_background_tasks),
        goal_active: thread.goal.as_ref().and_then(|goal| goal.get("status")).and_then(|status| status.as_str()) == Some("active"),
        created_at: parse_iso(&thread.created_at),
        updated_at,
        archived: thread.archived_at.is_some(),
        deleted: thread.deleted_at.is_some(),
        settled_override: match thread.settled_override.as_deref() {
            Some("settled") => Some(SettledOverride::Settled),
            Some("active") => Some(SettledOverride::Active),
            _ => None,
        },
        settled_at: parse_opt(thread.settled_at.as_deref()),
        unsettled_at: parse_opt(thread.unsettled_at.as_deref()),
        snoozed_until: parse_opt(thread.snoozed_until.as_deref()),
        snoozed_at: parse_opt(thread.snoozed_at.as_deref()),
        pinned: thread.pinned_at.is_some(),
        pin_order_key: thread.pin_order_key.clone(),
        active_order_key: thread.active_order_key.clone(),
        auto_settle_disabled: thread.auto_settle_disabled_at.is_some(),
        last_visited_at: parse_opt(thread.last_visited_at.as_deref()),
        regenerating_title: thread.title_regeneration.as_ref().is_some_and(|value| !value.is_null()),
        opens: thread.opens.clone().and_then(|opens| serde_json::from_value(opens).ok()),
    }
}

/// `threadRuntimeIsActive` on the shell's runtime summary.
pub fn runtime_is_active(thread: &ThreadShell) -> bool {
    shell_runtime(thread).is_some_and(|runtime| matches!(runtime.status.as_str(), "preparing" | "queued" | "starting" | "running" | "waiting"))
}

/// `sameApartFromStreaming` (`client-runtime/src/state/threadShell.ts`): while a run streams the
/// server bumps timestamps, item counts and the latest message several times a second; only the
/// counts' zero crossing matters. Such a tick changes nothing the sidebar shows.
pub fn same_apart_from_streaming(previous: &ThreadShell, next: &ThreadShell) -> bool {
    if !runtime_is_active(next) || (previous.item_count > 0) != (next.item_count > 0) || (previous.visible_item_count > 0) != (next.visible_item_count > 0) {
        return false;
    }
    let mut adjusted = previous.clone();
    adjusted.updated_at.clone_from(&next.updated_at);
    adjusted.item_count = next.item_count;
    adjusted.visible_item_count = next.visible_item_count;
    adjusted.latest_visible_message.clone_from(&next.latest_visible_message);
    adjusted == *next
}

fn is_child_run_live(thread: &ThreadShell) -> bool {
    match &thread.activity_run_status {
        Some(status) => status.is_some(),
        None => matches!(thread.status.as_str(), "preparing" | "starting" | "running" | "waiting"),
    }
}

fn request_of(kind: &str) -> SubagentRequest {
    if kind == "user_input" { SubagentRequest::UserInput } else { SubagentRequest::Approval }
}

pub(crate) fn subagent_request(thread: &ThreadShell) -> Option<SubagentRequest> {
    let kind = thread.pending_runtime_request.as_ref()?.kind.as_str();
    let subagent = thread.lineage.relationship_to_parent.as_deref() == Some("subagent");
    let provider_native = subagent && thread.creation_source.as_deref() == Some("provider");
    if kind == "auth_refresh" || (!subagent && thread.delegated_from.is_none()) || provider_native || !is_child_run_live(thread) {
        return None;
    }
    Some(request_of(kind))
}

pub(crate) fn side_chat_request(thread: &ThreadShell) -> Option<SubagentRequest> {
    let kind = thread.pending_runtime_request.as_ref()?.kind.as_str();
    (kind != "auth_refresh" && thread.lineage.side_chat == Some(true)).then(|| request_of(kind))
}

/// `pendingSubagentRequestsByThread`: which listed threads to flag for a child's request.
pub fn pending_subagent_requests(threads: &[ThreadShell]) -> HashMap<String, SubagentRequest> {
    pending_subagent_requests_with_delegated(threads, &[])
}

pub(crate) fn pending_subagent_requests_with_delegated(threads: &[ThreadShell], delegated: &[(&str, SubagentRequest)]) -> HashMap<String, SubagentRequest> {
    let mut requests: HashMap<String, SubagentRequest> = HashMap::new();
    let by_id: HashMap<&str, &ThreadShell> = threads.iter().map(|thread| (thread.id.as_str(), thread)).collect();
    for child in threads {
        let parent_id = child.lineage.parent_thread_id.clone();
        if child.lineage.side_chat == Some(true) {
            if let (Some(parent_id), Some(request)) = (parent_id, side_chat_request(child))
                && requests.get(&parent_id) != Some(&SubagentRequest::Approval)
            {
                requests.insert(parent_id, request);
            }
            continue;
        }
        if child.lineage.relationship_to_parent.as_deref() != Some("subagent") {
            continue;
        }
        // The coordinator answers child questions; only approvals need user attention.
        let Some(SubagentRequest::Approval) = subagent_request(child) else { continue };
        flag_approval(parent_id.as_deref(), &by_id, &mut requests, vec![child.id.as_str()]);
    }
    for (parent_id, request) in delegated {
        if *request == SubagentRequest::Approval { flag_approval(Some(parent_id), &by_id, &mut requests, Vec::new()); }
    }
    requests
}

fn flag_approval<'a>(parent_id: Option<&'a str>, by_id: &HashMap<&str, &'a ThreadShell>, requests: &mut HashMap<String, SubagentRequest>, mut seen: Vec<&'a str>) {
    let mut next = parent_id;
    while let Some(parent_id) = next.filter(|id| !seen.contains(id)) {
        seen.push(parent_id);
        requests.insert(parent_id.to_owned(), SubagentRequest::Approval);
        next = by_id.get(parent_id).and_then(|parent| {
            let nested = parent.lineage.relationship_to_parent.as_deref() == Some("subagent") || parent.lineage.side_chat == Some(true);
            if nested { parent.lineage.parent_thread_id.as_deref() } else { None }
        });
    }
}
