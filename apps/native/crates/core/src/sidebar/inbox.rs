//! The Working shelf: `isThreadWorking` and `createInboxReturnTracker`
//! (`client-runtime/src/state/threadInbox.ts`). The tracker is stateful and owns no timer;
//! callers re-observe at `next_deadline`.

use std::collections::HashMap;

use super::settled::effective_snoozed;
use super::thread::SidebarThread;
use super::time::Ms;

/// How long a thread that stopped working keeps reading as working.
pub const WORKING_EXIT_GRACE_MS: Ms = 2_000;

fn runtime_is_active(thread: &SidebarThread) -> bool {
    thread.runtime.as_ref().is_some_and(|runtime| matches!(runtime.status.as_str(), "preparing" | "queued" | "starting" | "running" | "waiting"))
}

pub fn is_thread_working(thread: &SidebarThread) -> bool {
    if thread.has_pending_approvals || thread.has_pending_user_input || thread.pending_subagent_request.is_some() {
        return false;
    }
    let idle = thread.runtime.as_ref().is_some_and(|runtime| runtime.status == "idle");
    if !runtime_is_active(thread) && !idle {
        return false;
    }
    let run_settled = thread.latest_run.as_ref().is_some_and(|run| {
        !matches!(run.status.as_str(), "preparing" | "queued" | "starting" | "running" | "waiting")
            && thread.runtime.as_ref().and_then(|runtime| runtime.active_run_id.as_deref()) != Some(run.run_id.as_str())
    });
    !(thread.interaction_mode == "plan" && thread.has_actionable_proposed_plan && run_settled)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedState {
    working: bool,
    deadline: Option<Ms>,
    placement: (bool, bool, bool, Option<u8>, Option<Ms>, Option<Ms>, Option<Ms>, Option<String>),
}

#[derive(Debug, Default)]
pub struct InboxReturnTracker {
    states: HashMap<String, TrackedState>,
    returns: HashMap<String, Ms>,
}

/// What one observation decided: who reads as working, and when each returned to Active.
#[derive(Debug, Default, Clone)]
pub struct InboxObservation {
    working: HashMap<String, bool>,
    returns: HashMap<String, Ms>,
}

impl InboxObservation {
    pub fn is_working(&self, thread: &SidebarThread) -> bool {
        self.working.get(&thread.id).copied().unwrap_or_else(|| is_thread_working(thread))
    }

    pub fn returned_at(&self, thread: &SidebarThread) -> Option<Ms> {
        self.returns.get(&thread.id).copied()
    }
}

impl InboxReturnTracker {
    pub fn clear(&mut self) {
        self.states.clear();
        self.returns.clear();
    }

    /// Re-run at `next_deadline()` so held rows leave the shelf on time.
    pub fn observe(&mut self, threads: &[SidebarThread], now: Ms) -> InboxObservation {
        for (key, state) in self.states.iter_mut() {
            if let Some(deadline) = state.deadline.filter(|deadline| *deadline <= now) {
                state.working = false;
                state.deadline = None;
                self.returns.insert(key.clone(), deadline);
            }
        }
        let mut present = std::collections::HashSet::with_capacity(threads.len());
        for thread in threads {
            present.insert(thread.id.as_str());
            let placement = (
                thread.archived,
                thread.deleted,
                thread.pinned,
                thread.settled_override.map(|o| o as u8),
                thread.settled_at,
                thread.snoozed_at,
                thread.snoozed_until,
                thread.active_order_key.clone(),
            );
            let previous = self.states.get(&thread.id);
            let actionable = thread.has_pending_approvals
                || thread.has_pending_user_input
                || thread.pending_subagent_request.is_some()
                || thread.pending_guest_message_count > 0
                || (thread.interaction_mode == "plan" && thread.has_actionable_proposed_plan)
                || thread.runtime.as_ref().is_some_and(|runtime| runtime.last_error.is_some())
                || thread.latest_run.as_ref().is_some_and(|run| run.status == "failed");
            let moved = previous.is_some_and(|previous| previous.placement != placement);
            let excluded = thread.archived || thread.deleted || thread.pinned || thread.is_settled() || effective_snoozed(thread, now);
            let actual = is_thread_working(thread);
            let was_working = previous.is_some_and(|previous| previous.working);
            // Only shelf placement is held: execution state and attention requests stay authoritative.
            let deadline = (!actual && was_working && !actionable && !moved && !excluded)
                .then(|| previous.and_then(|previous| previous.deadline).unwrap_or(now + WORKING_EXIT_GRACE_MS));
            let working = actual || deadline.is_some_and(|deadline| deadline > now);
            if was_working && !working {
                self.returns.insert(thread.id.clone(), now);
            }
            self.states.insert(thread.id.clone(), TrackedState { working, deadline: if working { deadline } else { None }, placement });
        }
        self.states.retain(|key, _| present.contains(key.as_str()));
        self.returns.retain(|key, _| present.contains(key.as_str()));
        InboxObservation {
            working: self.states.iter().map(|(key, state)| (key.clone(), state.working)).collect(),
            returns: self.returns.clone(),
        }
    }

    pub fn next_deadline(&self) -> Option<Ms> {
        self.states.values().filter_map(|state| state.deadline).min()
    }
}
