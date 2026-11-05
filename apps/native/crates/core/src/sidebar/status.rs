//! Row status: `resolveSidebarThreadStatus`, `shouldRecedeSidebarThread`, `hasUnseenCompletion`,
//! `resolveThreadStatusPill` (`apps/web/src/components/Sidebar.logic.ts`) and the row's top
//! status label (`apps/web/src/components/sidebar/SidebarThreadRow.tsx`).

use super::thread::{SidebarThread, SubagentRequest};
use super::time::Ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatus {
    Approval,
    Input,
    Request,
    Preparing,
    Working,
    Waiting,
    Failed,
    Limited,
    Ready,
}

pub fn resolve_status(thread: &SidebarThread) -> ThreadStatus {
    if thread.has_pending_approvals || thread.pending_subagent_request == Some(SubagentRequest::Approval) {
        return ThreadStatus::Approval;
    }
    if thread.has_pending_user_input || thread.pending_subagent_request == Some(SubagentRequest::UserInput) {
        return ThreadStatus::Input;
    }
    if thread.pending_guest_message_count > 0 {
        return ThreadStatus::Request;
    }
    let Some(runtime) = &thread.runtime else { return ThreadStatus::Ready };
    match runtime.status.as_str() {
        "running" if thread.blocked_on_thread_wait => ThreadStatus::Waiting,
        "preparing" => ThreadStatus::Preparing,
        "queued" | "starting" | "running" | "waiting" => ThreadStatus::Working,
        "idle" => ThreadStatus::Waiting,
        "failed" if runtime.last_error_class.as_deref() == Some("usage_limit") => ThreadStatus::Limited,
        "failed" => ThreadStatus::Failed,
        _ => ThreadStatus::Ready,
    }
}

pub fn has_unseen_completion(thread: &SidebarThread) -> bool {
    match (thread.latest_run.as_ref().and_then(|run| run.completed_at), thread.last_visited_at) {
        (Some(completed_at), Some(visited_at)) => completed_at > visited_at,
        _ => false,
    }
}

/// Unopened threads' failures still count as unseen, unlike completions.
pub fn has_unseen_failure(thread: &SidebarThread) -> bool {
    let Some(run) = thread.latest_run.as_ref().filter(|run| run.status == "failed") else { return false };
    match (run.completed_at, thread.last_visited_at) {
        (Some(completed_at), Some(visited_at)) => completed_at > visited_at,
        _ => true,
    }
}

pub fn should_recede(status: ThreadStatus, is_unread: bool, is_woke: bool, is_active: bool) -> bool {
    if is_active || matches!(status, ThreadStatus::Input | ThreadStatus::Request) {
        return false;
    }
    match status {
        ThreadStatus::Preparing | ThreadStatus::Working | ThreadStatus::Waiting => true,
        ThreadStatus::Ready | ThreadStatus::Approval => !is_unread && !is_woke,
        _ => false,
    }
}

/// The design-system §7.3 icon for a status label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusIcon {
    Working,
    Waiting,
    Input,
    Request,
    Approval,
    Failed,
    Done,
    Woke,
}

/// The token family for a status label; the app maps it to the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Info,
    Muted,
    Warning,
    Input,
    Guest,
    Error,
    Success,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopStatus {
    pub label: &'static str,
    pub icon: StatusIcon,
    pub tone: StatusTone,
}

pub fn top_status(status: ThreadStatus, goal_active: bool, is_woke: bool, is_unread: bool) -> Option<TopStatus> {
    let make = |label, icon, tone| Some(TopStatus { label, icon, tone });
    match status {
        ThreadStatus::Preparing => make("Setting up", StatusIcon::Working, StatusTone::Info),
        ThreadStatus::Working => make(if goal_active { "Goal" } else { "Working" }, StatusIcon::Working, StatusTone::Info),
        ThreadStatus::Waiting => make("Waiting", StatusIcon::Waiting, StatusTone::Muted),
        ThreadStatus::Approval => make("Approval", StatusIcon::Approval, StatusTone::Warning),
        ThreadStatus::Input => make("Input", StatusIcon::Input, StatusTone::Input),
        ThreadStatus::Request => make("Request", StatusIcon::Request, StatusTone::Guest),
        ThreadStatus::Limited => make("Limited", StatusIcon::Failed, StatusTone::Warning),
        ThreadStatus::Failed => make("Failed", StatusIcon::Failed, StatusTone::Error),
        ThreadStatus::Ready if is_woke => make("Woke", StatusIcon::Woke, StatusTone::Warning),
        ThreadStatus::Ready if is_unread => make("Done", StatusIcon::Done, StatusTone::Success),
        ThreadStatus::Ready => None,
    }
}

/// `resolveThreadWorkingStartedAt`: timers count from the activity-owning run's work start.
pub fn working_started_at(thread: &SidebarThread) -> Option<Ms> {
    if let Some(runtime) = &thread.runtime
        && let Some(started) = runtime.activity_started_at
    {
        return started;
    }
    let run = thread.latest_run.as_ref()?;
    let active = thread.runtime.as_ref().and_then(|runtime| runtime.active_run_id.as_deref());
    (run.completed_at.is_none() && active == Some(run.run_id.as_str())).then_some(run.started_at.or(run.requested_at)).flatten()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillTone {
    Approval,
    Input,
    Guest,
    Working,
    Waiting,
    Failed,
    Plan,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusPill {
    pub label: &'static str,
    pub tone: PillTone,
    pub pulse: bool,
}

/// `isLatestRunSettled` (`apps/web/src/session-logic.ts`).
fn latest_run_settled(thread: &SidebarThread) -> bool {
    let Some(run) = &thread.latest_run else { return false };
    if matches!(run.status.as_str(), "preparing" | "queued" | "starting" | "running" | "waiting") {
        return false;
    }
    thread.runtime.as_ref().and_then(|runtime| runtime.active_run_id.as_deref()) != Some(run.run_id.as_str())
}

/// `resolveThreadStatusPill`, for the command palette.
pub fn status_pill(thread: &SidebarThread) -> Option<StatusPill> {
    let pill = |label, tone, pulse| Some(StatusPill { label, tone, pulse });
    let runtime = thread.runtime.as_ref().map(|runtime| runtime.status.as_str());
    if thread.has_pending_approvals || thread.pending_subagent_request == Some(SubagentRequest::Approval) {
        return pill("Pending Approval", PillTone::Approval, false);
    }
    if thread.has_pending_user_input || thread.pending_subagent_request == Some(SubagentRequest::UserInput) {
        return pill("Awaiting Input", PillTone::Input, false);
    }
    if thread.pending_guest_message_count > 0 {
        return pill("Guest Request", PillTone::Guest, false);
    }
    if (runtime == Some("running") && !thread.blocked_on_thread_wait) || runtime == Some("waiting") {
        return pill("Working", PillTone::Working, true);
    }
    if matches!(runtime, Some("preparing" | "starting" | "queued")) {
        return pill("Connecting", PillTone::Working, true);
    }
    if (runtime == Some("running") && thread.blocked_on_thread_wait) || thread.background_holds_completion {
        return pill("Waiting", PillTone::Waiting, false);
    }
    if has_unseen_failure(thread) {
        return pill("Failed", PillTone::Failed, false);
    }
    if !thread.has_pending_user_input && thread.interaction_mode == "plan" && latest_run_settled(thread) && thread.has_actionable_proposed_plan {
        return pill("Plan Ready", PillTone::Plan, false);
    }
    if has_unseen_completion(thread) {
        return pill("Completed", PillTone::Completed, false);
    }
    None
}
