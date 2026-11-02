//! "Edit from here": which user messages can rewind the thread, the checkpoint and command that
//! do it, when the rewind has landed, and the prompt that goes back into the composer. Ports
//! `deriveRevertTurnCountByUserMessageId` and `findRevertCheckpoint` (`apps/web/src/session-logic.ts`),
//! `deriveAvailableRevertTurnCounts` and `waitForRevertedMessage` (`ChatView.logic.ts`),
//! `revertThreadCheckpoint` (`client-runtime/src/operations/commands.ts`), `rewindToMessage`
//! (`chatViewSend.ts`), `recallableComposerPrompt` (`composerPromptHistory.ts`) and the guards of
//! `useTimelineRewind.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Value, json};

use super::{EntryKind, Role, TimelineEntry};
use crate::diff::turn_diff_summaries;
use crate::thread::{Entity, ThreadProjection};

pub const EDIT_FROM_HERE: &str = "Edit from here";
pub const CONFIRM_TITLE: &str = "Edit from here?";
pub const CONFIRM_DESCRIPTION: &str =
    "Rewind chat to before this message. Your prompt and attachments return to the composer. \u{201c}Revert files too\u{201d} also undoes your own edits.";
pub const REVERT_FILES: &str = "Revert files too";
pub const KEEP_CHANGES: &str = "Revert and keep changes";
pub const NO_ROLLBACK: &str = "This provider does not support reverting conversation history. Start a new thread instead.";
pub const INTERRUPT_FIRST: &str = "Interrupt the current turn before reverting checkpoints.";
pub const THREAD_CHANGED: &str = "The thread has changed. Select a message to rewind again.";
pub const TIMED_OUT: &str = "Timed out waiting for the thread to rewind.";
pub const FAILED: &str = "Failed to revert thread state.";
/// `waitForRevertedMessage`'s default timeout.
pub const TIMEOUT_SECS: u64 = 120;

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// `deriveAvailableRevertTurnCounts`: the run ordinals the active provider conversation can
/// return to. A ready file checkpoint alone cannot restore the conversation.
pub fn available_revert_turn_counts(projection: &ThreadProjection) -> HashSet<u64> {
    let mut available = HashSet::new();
    let thread = &projection.thread;
    let Some(provider_thread) = projection.provider_threads.iter().find(|candidate| Some(&candidate.id) == thread.active_provider_thread_id.as_ref()) else {
        return available;
    };
    if text(&provider_thread.value, "providerSessionId").is_none()
        || text(&provider_thread.value, "providerInstanceId") != Some(thread.model_selection.instance_id.as_str())
    {
        return available;
    }
    available.insert(0);
    let mut latest_by_attempt: HashMap<&str, (u64, &Value)> = HashMap::new();
    for turn in &projection.provider_turns {
        let Some(attempt) = text(&turn.value, "runAttemptId") else { continue };
        let ordinal = turn.value.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
        if latest_by_attempt.get(attempt).is_none_or(|(latest, _)| ordinal > *latest) {
            latest_by_attempt.insert(attempt, (ordinal, &turn.value));
        }
    }
    for run in &projection.runs {
        let Some(attempt) = run.active_attempt_id.as_deref() else { continue };
        if latest_by_attempt.get(attempt).is_some_and(|(_, turn)| text(turn, "providerThreadId") == Some(provider_thread.id.as_str())) {
            available.insert(run.ordinal);
        }
    }
    available
}

/// A checkpoint a rewind targets (`OrchestrationV2Checkpoint` id and scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertCheckpoint {
    pub id: String,
    pub scope_id: String,
}

/// `findRevertCheckpoint`: the last ready checkpoint for `turn_count`; zero is the thread start.
pub fn find_revert_checkpoint(checkpoints: &[Entity], turn_count: u64) -> Option<RevertCheckpoint> {
    checkpoints.iter().rev().find_map(|checkpoint| {
        let value = &checkpoint.value;
        if text(value, "status") != Some("ready") {
            return None;
        }
        let ordinal = value.get("appRunOrdinal").and_then(Value::as_u64);
        let matches = if turn_count == 0 {
            value.get("ordinalWithinScope").and_then(Value::as_u64) == Some(0) && ordinal.is_none()
        } else {
            ordinal == Some(turn_count)
        };
        matches.then(|| RevertCheckpoint { id: checkpoint.id.clone(), scope_id: text(value, "scopeId").unwrap_or_default().to_owned() })
    })
}

pub struct RevertInput<'a> {
    pub entries: &'a [Arc<TimelineEntry>],
    pub checkpoints: &'a [Entity],
    /// The thread-start checkpoint is ready and no earlier history is left to page in.
    pub thread_start_ready: bool,
    pub available: &'a HashSet<u64>,
    pub unsettled_run_id: Option<&'a str>,
    /// Items inherited from a source thread: outside this thread's checkpoint scope.
    pub inherited: &'a HashSet<String>,
}

/// `deriveRevertTurnCountByUserMessageId`: each turn-starting user message rewinds to the ready
/// checkpoint of the run before it. Run ordinals keep increasing after a rewind, so only the
/// runs visible here define predecessors.
pub fn revert_turn_counts(input: &RevertInput) -> HashMap<String, u64> {
    let ready: HashMap<String, u64> =
        turn_diff_summaries(input.checkpoints).into_iter().filter(|summary| summary.status == "ready").map(|summary| (summary.run_id, summary.turn_count)).collect();
    let mut targets: HashMap<String, Option<u64>> = HashMap::new();
    let mut by_message = HashMap::new();
    let mut previous: Option<String> = None;
    for entry in input.entries {
        let (run_id, item_id) = match &entry.kind {
            EntryKind::Message(message) => (message.run_id.clone(), message.item.as_ref().map(|item| item.id.as_str())),
            EntryKind::Work(work) => (work.run_id.clone().filter(|_| work.item_type == Some("notification")), work.item.as_ref().map(|item| item.id.as_str())),
            _ => continue,
        };
        if item_id.is_some_and(|id| input.inherited.contains(id)) {
            continue;
        }
        let Some(run_id) = run_id else { continue };
        if !targets.contains_key(&run_id) {
            let target = match &previous {
                None => input.thread_start_ready.then_some(0),
                Some(previous) => ready.get(previous).copied(),
            };
            targets.insert(run_id.clone(), target);
            previous = Some(run_id.clone());
        }
        let EntryKind::Message(message) = &entry.kind else { continue };
        if message.role != Role::User || input.unsettled_run_id == Some(run_id.as_str()) {
            continue;
        }
        if !matches!(message.meta.input_intent.as_deref(), Some("turn_start" | "queued_turn")) {
            continue;
        }
        if let Some(target) = targets.get(&run_id).copied().flatten().filter(|target| input.available.contains(target)) {
            by_message.insert(message.id.clone(), target);
        }
    }
    by_message
}

/// `checkpoint.rollback`, as `revertThreadCheckpoint` dispatches it once the checkpoint is known.
pub fn rollback_command(command_id: &str, thread_id: &str, checkpoint: &RevertCheckpoint, restore_files: bool) -> Value {
    json!({
        "type": "checkpoint.rollback",
        "restoreFiles": restore_files,
        "commandId": command_id,
        "threadId": thread_id,
        "scopeId": checkpoint.scope_id,
        "checkpointId": checkpoint.id,
    })
}

/// What `waitForRevertedMessage` captures before dispatching: the provider conversation in use
/// and the newest settled run past the target, which the rollback marks `rolled_back`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindWatch {
    pub thread_id: String,
    pub turn_count: u64,
    pub request_id: String,
    provider_thread_id: Option<String>,
    provider_instance_id: Option<String>,
    provider_thread_ids: HashSet<String>,
    witness_run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindProgress {
    Pending,
    Done,
    Failed(String),
}

pub fn watch_rewind(projection: &ThreadProjection, turn_count: u64, request_id: &str) -> Result<RewindWatch, String> {
    let witness = projection
        .runs
        .iter()
        .filter(|run| run.ordinal > turn_count && matches!(run.status.as_str(), "completed" | "interrupted" | "failed" | "cancelled"))
        .max_by_key(|run| run.ordinal)
        .ok_or_else(|| THREAD_CHANGED.to_owned())?;
    let provider_thread_id = projection.thread.active_provider_thread_id.clone();
    let provider_instance_id = projection
        .provider_threads
        .iter()
        .find(|thread| Some(&thread.id) == provider_thread_id.as_ref())
        .and_then(|thread| text(&thread.value, "providerInstanceId"))
        .map(str::to_owned);
    Ok(RewindWatch {
        thread_id: projection.thread.id.clone(),
        turn_count,
        request_id: request_id.to_owned(),
        provider_thread_id,
        provider_instance_id,
        provider_thread_ids: projection.provider_threads.iter().map(|thread| thread.id.clone()).collect(),
        witness_run_id: witness.id.clone(),
    })
}

/// Whether the rewind landed: the provider conversation is back at the target (Claude starts a
/// fresh root session when rewinding to the thread start) and the witness run rolled back.
pub fn rewind_progress(watch: &RewindWatch, projection: &ThreadProjection) -> RewindProgress {
    if let Some(failure) = projection.thread.rollback_failure.as_ref().filter(|failure| failure.request_id == watch.request_id) {
        return RewindProgress::Failed(failure.message.clone());
    }
    let target = (watch.turn_count != 0).then_some(watch.turn_count);
    let conversation_reset = projection.provider_threads.iter().any(|thread| {
        let value = &thread.value;
        let same = Some(&thread.id) == watch.provider_thread_id.as_ref();
        let fresh_root = watch.turn_count == 0
            && value.get("ownerNodeId").is_none_or(Value::is_null)
            && text(value, "appThreadId") == Some(watch.thread_id.as_str())
            && text(value, "providerInstanceId") == watch.provider_instance_id.as_deref()
            && !watch.provider_thread_ids.contains(&thread.id);
        (same || fresh_root) && value.get("lastRunOrdinal").and_then(Value::as_u64) == target
    });
    let rolled_back = projection.runs.iter().any(|run| run.id == watch.witness_run_id && run.status == "rolled_back");
    if conversation_reset && rolled_back { RewindProgress::Done } else { RewindProgress::Pending }
}

const ULTRATHINK_PREFIX: &str = "Ultrathink:\n";
const ATTACHMENT_ONLY_PROMPT: &str =
    "[User attached one or more files without additional text. Respond using the conversation context and the attached files.]";
const PLAN_IMPLEMENTATION_PREFIX: &str = "PLEASE IMPLEMENT THIS PLAN:\n";
const LEGACY_CONTEXT_TAGS: [&str; 3] = ["terminal_context", "element_context", "preview_annotation"];

fn strip_trailing_review_comment(prompt: &str) -> Option<&str> {
    let trimmed = prompt.trim_end();
    if !trimmed.ends_with("</review_comment>") {
        return None;
    }
    let start = trimmed.rmatch_indices("<review_comment").map(|(index, _)| index).find(|index| {
        trimmed[index + "<review_comment".len()..].starts_with([' ', '>', '\t', '\n'])
    })?;
    Some(trimmed[..start].trim_end())
}

/// A trailing `<tag>\n…\n</tag>` block of an older client: its start, and its `- Header:` lines.
fn trailing_legacy_block(prompt: &str) -> Option<(usize, &'static str, Vec<String>, bool)> {
    let trimmed = prompt.trim_end();
    LEGACY_CONTEXT_TAGS.iter().find_map(|tag| {
        let (open, close) = (format!("<{tag}>\n"), format!("\n</{tag}>"));
        if !trimmed.ends_with(&close) {
            return None;
        }
        let start = trimmed.find(&open)?;
        let body_start = start + open.len();
        let body_end = trimmed.len() - close.len();
        if body_start > body_end {
            return None;
        }
        let body = &trimmed[body_start..body_end];
        let headers = body.lines().filter_map(|line| line.strip_prefix("- ")?.strip_suffix(':').map(str::to_owned)).filter(|header| !header.is_empty()).collect();
        let annotation_without_fields = *tag == "preview_annotation"
            && body.lines().any(|line| line.trim_end() == "<preview_annotation>")
            && !body.lines().any(|line| ["Id: ", "Page: ", "Comment: ", "Targets: "].iter().any(|field| line.starts_with(field)));
        Some((start, *tag, headers, annotation_without_fields))
    })
}

/// `stripInlineTerminalLabels`: the `@name:12-20` labels a terminal context block put inline.
fn strip_inline_terminal_labels(mut prompt: String, headers: &[String]) -> String {
    for header in headers {
        let Some((name, range)) = header.rsplit_once(" lines ").or_else(|| header.rsplit_once(" line ")) else { continue };
        if range.is_empty() || !range.chars().all(|c| c.is_ascii_digit() || c == '-') || name.trim().is_empty() {
            continue;
        }
        let label = format!("@{}:{range}", name.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join("-"));
        let found = prompt.match_indices(&label).map(|(index, _)| index).find(|&index| {
            let before = prompt[..index].chars().next_back();
            let after = prompt[index + label.len()..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '@' | '.' | '-')) && !after.is_some_and(|c| c.is_ascii_digit() || c == '-')
        });
        let Some(index) = found else { continue };
        let (mut start, mut end) = (index, index + label.len());
        if prompt[end..].starts_with(' ') {
            end += 1;
        } else if prompt[..start].ends_with(' ') {
            start -= 1;
        }
        prompt.replace_range(start..end, "");
    }
    prompt
}

/// Context chips (`[label](t3-context:…)`, optionally `!`-prefixed): recall drops them, since
/// their backing records are not restored.
fn strip_context_references(prompt: &str) -> String {
    let mut out = prompt.to_owned();
    while let Some(marker) = out.rfind("](t3-context:") {
        let Some(open) = out[..marker].rfind('[') else { break };
        let Some(close) = out[marker..].find(')').map(|offset| marker + offset) else { break };
        let label = &out[open + 1..marker];
        let href = &out[marker + 2..close];
        if label.contains(['\n', ']']) || href.contains(char::is_whitespace) {
            break;
        }
        let mut start = if out[..open].ends_with('!') { open - 1 } else { open };
        let mut end = close + 1;
        if out[end..].starts_with(' ') {
            end += 1;
        } else if out[..start].ends_with(' ') {
            start -= 1;
        }
        out.replace_range(start..end, "");
    }
    out
}

/// `recallableComposerPrompt`: the words the user typed, without the effort prefix, trailing
/// review comments and legacy context blocks, or context chips.
pub fn recallable_prompt(message_text: &str) -> String {
    let mut prompt = message_text.trim().to_owned();
    if let Some(rest) = prompt.strip_prefix(ULTRATHINK_PREFIX) {
        prompt = rest.to_owned();
    }
    loop {
        if let Some(stripped) = strip_trailing_review_comment(&prompt) {
            prompt = stripped.to_owned();
            continue;
        }
        let Some((start, tag, headers, annotation_without_fields)) = trailing_legacy_block(&prompt) else { break };
        if annotation_without_fields || (tag != "preview_annotation" && headers.is_empty()) {
            break;
        }
        prompt = prompt[..start].trim_end().to_owned();
        if tag == "terminal_context" {
            prompt = strip_inline_terminal_labels(prompt, &headers);
        }
    }
    let prompt = strip_context_references(&prompt);
    let trimmed = prompt.trim();
    if trimmed == ATTACHMENT_ONLY_PROMPT || trimmed.starts_with(PLAN_IMPLEMENTATION_PREFIX) { String::new() } else { trimmed.to_owned() }
}

/// `rewindToMessage`: the restored prompt joins what the composer already holds.
pub fn merge_restored_prompt(current: &str, restored: &str) -> String {
    match (current.is_empty(), restored.is_empty()) {
        (_, true) => current.to_owned(),
        (true, false) => restored.to_owned(),
        (false, false) => format!("{current}\n\n{restored}"),
    }
}
