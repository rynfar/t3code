//! Queued runs: the messages waiting behind the active turn and what each may do now. Ports
//! `deriveThreadQueueWorkflowState` and `getUserQueuedThreadRuns`
//! (`client-runtime/src/state/threadWorkflows.ts`), the queue commands of
//! `client-runtime/src/operations/commands.ts` (`queue.resume`, `queued-run.start`,
//! `queued-message.promote-to-steer`, `queued-run.cancel`, `queued-run.edit`), the row text of
//! `web/src/components/chat/QueuedRunsControl.tsx`, and `queuedRunEditBlockedReason`
//! (`client-runtime/src/queuedRunEdit.ts`).

use serde_json::{Value, json};

use super::attachments::{ATTACHMENT_ONLY_PROMPT, is_attachment_only_prompt};
use super::menu::{context_references, sanitize_context_label};
use crate::thread::{Run, ThreadProjection};
use crate::workflows;

/// `QUEUE_VISIBLE_ROWS`: more than this collapses behind "Show all".
pub const VISIBLE_ROWS: usize = 3;

/// One queued message.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedRun {
    pub run_id: String,
    pub message_id: String,
    pub text: String,
    pub attachments: Vec<Value>,
    /// A guest's message can be cancelled, not rewritten.
    pub editable: bool,
    pub fresh_context: bool,
    pub created_at: Option<String>,
    pub attribution: Option<crate::timeline::Attribution>,
    pub scheduled_task_id: Option<String>,
    pub sender_thread_id: Option<String>,
    pub linked_sender: Option<crate::turn_item::LinkedSender>,
    pub guest_name: Option<String>,
}

impl QueuedRun {
    pub fn image_count(&self) -> usize {
        self.attachments.iter().filter(|attachment| attachment.get("type").and_then(Value::as_str) == Some("image")).count()
    }
}

/// `ThreadQueueWorkflowState`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueueState {
    pub active_run_id: Option<String>,
    pub queued: Vec<QueuedRun>,
    pub is_held: bool,
    /// No turn runs, so a queued message starts one.
    pub can_send_now: bool,
    /// The running turn takes a queued message as a steer.
    pub can_promote_to_steer: bool,
    pub promote_skips_compaction: bool,
}

impl QueueState {
    /// "Send now" works for a run: start a turn when idle, else steer the running one unless
    /// `fresh_context` forbids steering into a non-compaction turn (`QueuedRunsControl.tsx`).
    pub fn can_send_run(&self, fresh_context: bool) -> bool {
        self.can_send_now || (self.can_promote_to_steer && (!fresh_context || self.promote_skips_compaction))
    }

    /// "Send now" works: start a turn, or steer the running one.
    pub fn can_send(&self) -> bool {
        self.can_send_run(false)
    }

    /// The Send now tooltip.
    pub fn send_tooltip(&self) -> &'static str {
        if self.can_send_now {
            "Start a turn with this message"
        } else if self.promote_skips_compaction {
            "Skip compaction and send this message"
        } else {
            "Send into the current turn"
        }
    }
}

fn message<'a>(projection: &'a ThreadProjection, id: &str) -> Option<&'a Value> {
    projection.messages.iter().find(|entity| entity.id == id).map(|entity| &entity.value)
}

/// `isCompactionCommandMessage`.
fn is_compaction_command(message: &Value) -> bool {
    message.get("text").and_then(Value::as_str).is_some_and(|text| text.trim().eq_ignore_ascii_case("/compact"))
        && message.get("attachments").and_then(Value::as_array).is_none_or(Vec::is_empty)
}

/// `getUserQueuedThreadRuns`: queued runs, without automatic completion notices.
fn user_queued_runs(projection: &ThreadProjection) -> Vec<&Run> {
    projection
        .runs
        .iter()
        .filter(|run| run.status == "queued")
        .filter(|run| {
            message(projection, &run.user_message_id)
                .is_none_or(|message| message.get("delegatedCompletion").is_none_or(Value::is_null) && message.get("notification").is_none_or(Value::is_null))
        })
        .collect()
}

/// `deriveThreadQueueWorkflowState`.
pub fn queue_state(projection: &ThreadProjection) -> QueueState {
    let active = workflows::active_run(projection);
    let capabilities = workflows::provider_session(projection).and_then(|session| session.pointer("/capabilities/turns"));
    let promote_skips_compaction = active.and_then(|run| message(projection, &run.user_message_id)).is_some_and(is_compaction_command);
    let steerable_turn = active.is_some_and(|run| {
        run.status == "running"
            && run.active_attempt_id.as_ref().is_some_and(|attempt| {
                projection.provider_turns.iter().any(|turn| {
                    turn.value.get("runAttemptId").and_then(Value::as_str) == Some(attempt.as_str())
                        && turn.value.get("status").and_then(Value::as_str) == Some("running")
                })
            })
    });
    let capable = |key: &str| capabilities.and_then(|turns| turns.get(key)) == Some(&Value::Bool(true));
    let mut runs = user_queued_runs(projection);
    runs.sort_by_key(|run| (run.queue_position.unwrap_or(run.ordinal), run.ordinal));
    let queued = runs
        .into_iter()
        .map(|run| {
            let message = message(projection, &run.user_message_id);
            let guest = message.and_then(|message| message.pointer("/context/guest")).filter(|guest| !guest.is_null());
            let guest_name = guest
                .and_then(|guest| guest.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
            let guest_text = guest.and_then(|guest| guest.get("text")).and_then(Value::as_str);
            let raw_text = message.and_then(|message| message.get("text")).and_then(Value::as_str).unwrap_or("Queued message");
            let linked_sender = message
                .and_then(|message| message.get("linkedSender"))
                .filter(|value| !value.is_null())
                .and_then(|value| serde_json::from_value::<crate::turn_item::LinkedSender>(value.clone()).ok());
            let meta_task_id = message.and_then(|message| message.get("scheduledTaskId")).and_then(Value::as_str);
            let scheduled_task_id = crate::timeline::scheduled_task_id_from_fields(&run.user_message_id, meta_task_id);
            let created_by = message.and_then(|message| message.get("createdBy")).and_then(Value::as_str);
            let creation_source = message.and_then(|message| message.get("creationSource")).and_then(Value::as_str);
            let attribution = crate::timeline::attribution_for_fields(
                &run.user_message_id,
                raw_text,
                guest_name.is_some() || guest_text.is_some(),
                meta_task_id,
                linked_sender.is_some(),
                created_by,
                creation_source,
            );
            let stripped_text = if attribution == Some(crate::timeline::Attribution::Automation) {
                crate::timeline::strip_legacy_automation_prefix(raw_text).unwrap_or(raw_text)
            } else {
                raw_text
            };
            let text = guest_text.unwrap_or(stripped_text).to_owned();
            let created_at = message
                .and_then(|message| message.get("createdAt"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let sender_thread_id = message
                .and_then(|message| message.get("senderThreadId"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            QueuedRun {
                run_id: run.id.clone(),
                message_id: run.user_message_id.clone(),
                text,
                attachments: message.and_then(|message| message.get("attachments")).and_then(Value::as_array).cloned().unwrap_or_default(),
                editable: guest.is_none(),
                fresh_context: run.fresh_context == Some(true),
                created_at,
                attribution,
                scheduled_task_id,
                sender_thread_id,
                linked_sender,
                guest_name,
            }
        })
        .collect();
    QueueState {
        active_run_id: active.map(|run| run.id.clone()),
        queued,
        is_held: projection.runs.iter().any(|run| run.status == "queued" && run.queue_held == Some(true)),
        can_send_now: active.is_none() && projection.thread.archived_at.is_none(),
        can_promote_to_steer: promote_skips_compaction
            || (steerable_turn && (capable("supportsActiveSteering") || capable("supportsSteeringByInterruptRestart"))),
        promote_skips_compaction,
    }
}

/// What a queued row reads: links as their labels, images dropped when thumbnails show, and
/// nothing for the attachment-only prompt.
pub fn preview_text(run: &QueuedRun) -> String {
    if is_attachment_only_prompt(&run.text) {
        return String::new();
    }
    let text = run.text.as_str();
    let mut out = String::new();
    let mut cursor = 0;
    for reference in context_references(text) {
        out.push_str(&text[cursor..reference.start]);
        if !(reference.kind == "image" && run.image_count() > 0) {
            out.push_str(&sanitize_context_label(reference.label, reference.kind));
        }
        cursor = reference.end;
    }
    out.push_str(&text[cursor..]);
    crate::assistant_citations::to_plain_text(&out).trim().to_owned()
}

/// `stripAttachmentOnlyBootstrapPrompt`: the text an edit starts from.
pub fn editable_text(text: &str) -> String {
    match is_attachment_only_prompt(text) {
        true => text[..text.len() - ATTACHMENT_ONLY_PROMPT.len()].trim_end().to_owned(),
        false => text.to_owned(),
    }
}

/// `queuedRunEditBlockedReason`.
pub fn edit_blocked_reason(open_questions: usize) -> Option<&'static str> {
    (open_questions > 0).then_some("Answer the question to edit queued messages")
}

/// `resumeThreadQueue` for one run: start it now.
pub fn start_queued_run(command_id: &str, thread_id: &str, run_id: &str) -> Value {
    json!({ "type": "queued-run.start", "commandId": command_id, "threadId": thread_id, "runId": run_id })
}

/// `promoteQueuedRun`: steer the running turn with a queued message.
pub fn promote_queued_run(command_id: &str, thread_id: &str, queued_run_id: &str, target_run_id: &str) -> Value {
    json!({
        "type": "queued-message.promote-to-steer",
        "commandId": command_id,
        "threadId": thread_id,
        "queuedRunId": queued_run_id,
        "targetRunId": target_run_id,
    })
}

/// "Send now": start a turn when idle, else steer the active one unless blocked by `fresh_context`.
pub fn send_now(state: &QueueState, command_id: &str, thread_id: &str, run_id: &str) -> Option<Value> {
    let fresh_context = state.queued.iter().any(|run| run.run_id == run_id && run.fresh_context);
    if !state.can_send_run(fresh_context) {
        return None;
    }
    if state.can_send_now {
        return Some(start_queued_run(command_id, thread_id, run_id));
    }
    let active = state.active_run_id.as_deref().filter(|_| state.can_promote_to_steer)?;
    Some(promote_queued_run(command_id, thread_id, run_id, active))
}

pub fn cancel_queued_run(command_id: &str, thread_id: &str, run_id: &str) -> Value {
    json!({ "type": "queued-run.cancel", "commandId": command_id, "threadId": thread_id, "runId": run_id })
}

/// `editQueuedRun` for the text alone: the server keeps the attachments when they are absent.
pub fn edit_queued_run(command_id: &str, thread_id: &str, run_id: &str, text: &str) -> Value {
    json!({ "type": "queued-run.edit", "commandId": command_id, "threadId": thread_id, "runId": run_id, "text": text })
}

#[cfg(test)]
mod tests;
