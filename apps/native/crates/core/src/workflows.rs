//! What a thread's runs allow right now: switching providers, steering, and what Stop ends.
//! Ports `threadSupportsProviderHandoff` (`client-runtime/src/state/threadWorkflows.ts`), the
//! compaction gate in `web/src/components/ChatView.tsx`, and the run search `interruptThreadTurn`
//! does without `thread.stop` (`client-runtime/src/operations/commands.ts`), with
//! `derivePendingBackgroundWork` from `shared/src/orchestrationV2PendingBackgroundWork.ts`.

use serde_json::Value;

use crate::thread::{Run, ThreadProjection};
use crate::turn_item::TurnItemKind;

/// `ACTIVE_RUN_STATUSES`.
fn is_active(run: &Run) -> bool {
    matches!(run.status.as_str(), "preparing" | "starting" | "running" | "waiting")
}

/// `resolveActiveThreadRun`.
pub fn active_run(projection: &ThreadProjection) -> Option<&Run> {
    projection.runs.iter().rev().find(|run| is_active(run))
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

/// `resolveThreadProviderSession`: the session of the run's provider thread, else of the thread's
/// attached provider thread, else the newest session still alive.
fn provider_session(projection: &ThreadProjection) -> Option<&Value> {
    let provider_thread_id = active_run(projection)
        .and_then(|run| run.provider_thread_id.as_deref())
        .or(projection.thread.active_provider_thread_id.as_deref());
    let threads = projection.provider_threads.iter().map(|entity| &entity.value);
    let attached = provider_thread_id
        .and_then(|id| threads.clone().find(|thread| text(thread, "id") == Some(id)))
        .or_else(|| {
            threads.clone().find(|thread| {
                text(thread, "appThreadId") == Some(projection.thread.id.as_str()) && text(thread, "providerSessionId").is_some()
            })
        });
    if let Some(session_id) = attached.and_then(|thread| text(thread, "providerSessionId")) {
        return projection.provider_sessions.iter().map(|entity| &entity.value).find(|session| text(session, "id") == Some(session_id));
    }
    projection
        .provider_sessions
        .iter()
        .rev()
        .map(|entity| &entity.value)
        .find(|session| !matches!(text(session, "status"), Some("stopped" | "error")))
}

/// `threadSupportsProviderHandoff`: whether the next turn may run on another provider.
pub fn supports_provider_handoff(projection: &ThreadProjection) -> bool {
    if let Some(session) = provider_session(projection) {
        return session.pointer("/capabilities/sessions/supportsProviderSwitchingViaHandoff") == Some(&Value::Bool(true));
    }
    if active_run(projection).is_some() {
        return false;
    }
    if projection.thread.history_origin.as_deref() == Some("v1_import") || projection.runs.is_empty() {
        return true;
    }
    // A detached stopped session leaves its native thread behind for the next turn's handoff.
    projection.provider_threads.iter().map(|entity| &entity.value).any(|thread| {
        text(thread, "id").is_some()
            && text(thread, "id") == projection.thread.active_provider_thread_id.as_deref()
            && text(thread, "appThreadId") == Some(projection.thread.id.as_str())
            && text(thread, "providerInstanceId") == Some(projection.thread.model_selection.instance_id.as_str())
            && thread.get("nativeThreadRef").is_some_and(|reference| !reference.is_null())
    })
}

fn is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "interrupted" | "cancelled")
}

/// `isCompacting` in `ChatView.tsx` from server state: the active run is a `/compact` turn whose
/// compaction has not settled, or a compaction is running. Steering into it is rejected, so a send
/// queues behind it instead.
pub fn is_compacting(projection: &ThreadProjection, active_run_id: Option<&str>) -> bool {
    let Some(run_id) = active_run_id else { return false };
    let items = || projection.visible_turn_items.iter().map(|row| &row.item);
    let compaction_running = items().any(|item| {
        matches!(item.kind, TurnItemKind::Compaction { .. }) && item.run_id.as_deref() == Some(run_id) && item.status == "running"
    });
    if compaction_running {
        return true;
    }
    let run_unfinished = projection.runs.iter().any(|run| run.id == run_id && run.is_unfinished());
    let compact_request = items().rev().find(|item| matches!(item.kind, TurnItemKind::UserMessage { .. })).is_some_and(|item| {
        matches!(&item.kind, TurnItemKind::UserMessage { text, attachments, .. }
            if text.trim().eq_ignore_ascii_case("/compact") && attachments.is_empty())
            && item.run_id.as_deref() == Some(run_id)
            && run_unfinished
    });
    let settled = items().any(|item| {
        matches!(item.kind, TurnItemKind::Compaction { .. }) && item.run_id.as_deref() == Some(run_id) && is_terminal(&item.status)
    });
    compact_request && !settled
}

/// `derivePendingBackgroundWork(...).length > 0` for a thread's own projection, pull request
/// watches aside: work the latest settled run left running, which Stop still has to end.
pub fn has_pending_background_work(projection: &ThreadProjection) -> bool {
    if projection.runs.iter().any(|run| matches!(run.status.as_str(), "preparing" | "starting" | "running")) {
        return false;
    }
    let Some(latest) = projection.runs.last() else { return false };
    if !matches!(latest.status.as_str(), "cancelled" | "completed" | "failed" | "interrupted" | "waiting") {
        return false;
    }
    let active_thread = projection.thread.active_provider_thread_id.as_deref();
    let provider_tasks = projection
        .provider_threads
        .iter()
        .map(|entity| &entity.value)
        .filter(|thread| active_thread.is_none_or(|id| text(thread, "id") == Some(id)))
        .filter_map(|thread| thread.get("pendingBackgroundTasks")?.as_array())
        .flatten()
        .any(|task| text(task, "taskId").is_some_and(|id| !id.is_empty()));
    if provider_tasks {
        return true;
    }
    let rolled_back: Vec<&str> = projection.runs.iter().filter(|run| run.status == "rolled_back").map(|run| run.id.as_str()).collect();
    projection.turn_items.iter().any(|item| {
        let background = match &item.kind {
            TurnItemKind::CommandExecution { .. } | TurnItemKind::Subagent { .. } => true,
            TurnItemKind::DynamicTool { input, .. } => input.get("persistent") != Some(&Value::Bool(true)),
            _ => false,
        };
        background
            && matches!(item.status.as_str(), "pending" | "running" | "waiting")
            && item.run_id.as_deref().is_none_or(|run_id| !rolled_back.contains(&run_id))
    })
}

/// What Stop sends, as `interruptThreadTurn` decides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopTarget {
    /// `thread.stop`: the server finds every run and command itself.
    Thread,
    /// `run.interrupt` for servers without `thread.stop`.
    Run(String),
    /// Nothing is running.
    Nothing,
}

pub fn stop_target(projection: Option<&ThreadProjection>, thread_stop: bool) -> StopTarget {
    if thread_stop {
        return StopTarget::Thread;
    }
    let Some(projection) = projection else { return StopTarget::Nothing };
    let running = projection
        .runs
        .iter()
        .rev()
        .find(|run| matches!(run.status.as_str(), "preparing" | "starting" | "running" | "waiting"));
    if let Some(run) = running {
        return StopTarget::Run(run.id.clone());
    }
    match projection.runs.last() {
        Some(latest) if has_pending_background_work(projection) => StopTarget::Run(latest.id.clone()),
        _ => StopTarget::Nothing,
    }
}

#[cfg(test)]
mod tests;
