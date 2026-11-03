//! Visible turn items to timeline entries: `deriveTimelineEntriesFromVisibleTurnItems` and
//! `projectedWorkEntry` (`apps/web/src/session-logic.ts`), `extractToolActivityPresentation`
//! (`packages/client-runtime/src/work-log/toolPresentation.ts`). MCP-app tool results stay work
//! entries until the native client renders their frames.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Value, json};

use super::presentation::context_compaction_label;
use super::tool_activity::{ToolActivityAction, classify_tool_activity, collect_tool_file_paths, dynamic_tool_surface, format_read_tool_label, format_search_tool_label};
use super::{ChatMessage, EntryKind, HtmlRenderEntry, MessageMeta, ProposedPlan, Role, SourceActivity, TimelineAttempt, TimelineEntry, Tone, ToolSource, ToolStatus, WorkLogEntry};
use crate::thread::{Entity, ProjectedTurnItem, RunAttempt};
use crate::turn_item::{TurnItem, TurnItemKind};

const WORKSPACE_PREPARATION_INPUT: &str = "Preparing workspace";
const WORKSPACE_PREPARATION_FAILURE_CODE: &str = "workspace_preparation_failed";
const IMAGE_PREVIEW_EXTENSIONS: [&str; 13] =
    [".avif", ".bmp", ".gif", ".ico", ".heic", ".heif", ".jpeg", ".jpg", ".png", ".svg", ".tif", ".tiff", ".webp"];

/// The wire `type` of an item.
pub fn item_type_name(kind: &TurnItemKind) -> &'static str {
    match kind {
        TurnItemKind::Notification { .. } => "notification",
        TurnItemKind::UserMessage { .. } => "user_message",
        TurnItemKind::AssistantMessage { .. } => "assistant_message",
        TurnItemKind::Reasoning { .. } => "reasoning",
        TurnItemKind::ProposedPlan { .. } => "proposed_plan",
        TurnItemKind::TodoList { .. } => "todo_list",
        TurnItemKind::UserInputRequest { .. } => "user_input_request",
        TurnItemKind::FileChange { .. } => "file_change",
        TurnItemKind::CommandExecution { .. } => "command_execution",
        TurnItemKind::FileSearch { .. } => "file_search",
        TurnItemKind::WebSearch { .. } => "web_search",
        TurnItemKind::ApprovalRequest { .. } => "approval_request",
        TurnItemKind::Checkpoint { .. } => "checkpoint",
        TurnItemKind::RunInterruptRequest { .. } => "run_interrupt_request",
        TurnItemKind::RunInterruptResult { .. } => "run_interrupt_result",
        TurnItemKind::SystemNotice { .. } => "system_notice",
        TurnItemKind::Error { .. } => "error",
        TurnItemKind::Compaction { .. } => "compaction",
        TurnItemKind::Handoff { .. } => "handoff",
        TurnItemKind::Fork { .. } => "fork",
        TurnItemKind::ThreadCreated { .. } => "thread_created",
        TurnItemKind::SecretRequest { .. } => "secret_request",
        TurnItemKind::LinkRequest { .. } => "link_request",
        TurnItemKind::Subagent { .. } => "subagent",
        TurnItemKind::DynamicTool { .. } => "dynamic_tool",
        TurnItemKind::Unknown => "unknown",
    }
}

fn is_workspace_preparation(item: &TurnItem) -> bool {
    match &item.kind {
        TurnItemKind::CommandExecution { input, .. } => input == WORKSPACE_PREPARATION_INPUT,
        TurnItemKind::Error { failure, .. } => {
            item.status == "cancelled" && failure.code.as_deref() == Some(WORKSPACE_PREPARATION_FAILURE_CODE)
        }
        _ => false,
    }
}

fn created_at(item: &TurnItem) -> String {
    item.started_at.clone().unwrap_or_else(|| item.updated_at.clone())
}

fn status_of(item: &TurnItem) -> ToolStatus {
    match item.status.as_str() {
        "pending" | "running" | "waiting" => ToolStatus::InProgress,
        "idle" => ToolStatus::Idle,
        "failed" => ToolStatus::Failed,
        "cancelled" | "interrupted" => ToolStatus::Stopped,
        _ => ToolStatus::Completed,
    }
}

fn tone_of(kind: &TurnItemKind) -> Tone {
    match kind {
        TurnItemKind::Reasoning { .. } => Tone::Thinking,
        TurnItemKind::CommandExecution { .. }
        | TurnItemKind::FileChange { .. }
        | TurnItemKind::FileSearch { .. }
        | TurnItemKind::WebSearch { .. }
        | TurnItemKind::DynamicTool { .. }
        | TurnItemKind::Subagent { .. }
        | TurnItemKind::ThreadCreated { .. }
        | TurnItemKind::UserInputRequest { .. }
        | TurnItemKind::ApprovalRequest { .. } => Tone::Tool,
        _ => Tone::Info,
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

fn tool_source(value: Option<&Value>) -> Option<ToolSource> {
    let value = value?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let kind = text("kind").filter(|k| matches!(k.as_str(), "browser" | "computer" | "integration"))?;
    Some(ToolSource { key: text("key")?, name: text("name")?, kind })
}

/// `providerErrorPresentation`: (label, detail).
fn provider_error_presentation(item: &TurnItem) -> (String, String) {
    let TurnItemKind::Error { failure, retry } = &item.kind else { return (String::new(), String::new()) };
    let usage = failure.class == "usage_limit";
    let Some(retry) = retry else {
        let label = if usage {
            "Usage limit reached".to_owned()
        } else {
            item.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).unwrap_or("Provider error").to_owned()
        };
        return (label, failure.message.clone());
    };
    let progress = match retry.max_attempts {
        Some(max) => format!("{}/{max}", retry.attempt),
        None => retry.attempt.to_string(),
    };
    let label = match item.status.as_str() {
        "running" => format!("Retrying provider ({progress})"),
        "completed" => format!("Provider recovered ({progress} retries)"),
        "failed" => format!("{} after {progress} retries", if usage { "Usage limit reached" } else { "Provider error" }),
        _ => format!("Provider retry stopped ({progress})"),
    };
    let delay = match retry.retry_delay_ms {
        Some(ms) if item.status == "running" && ms > 0 && ms < 1_000 => format!(" Retrying in {ms}ms."),
        Some(ms) if item.status == "running" && ms > 0 => {
            let seconds = format!("{:.1}", ms as f64 / 1_000.);
            format!(" Retrying in {}s.", seconds.strip_suffix(".0").unwrap_or(&seconds))
        }
        _ => String::new(),
    };
    (label, format!("{}{delay}", failure.message))
}

/// `projectedWorkEntry`.
pub(super) fn projected_work_entry(row: &ProjectedTurnItem) -> WorkLogEntry {
    let item = &row.item;
    let title = item.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned);
    let mut source = tool_source(item.tool_source.as_ref());
    let surface = match item.tool_surface.as_deref() {
        Some(surface @ ("browser" | "computer")) => Some(surface.to_owned()),
        _ => match source.as_ref().map(|s| s.kind.as_str()) {
            Some(kind @ ("browser" | "computer")) => Some(kind.to_owned()),
            _ => match &item.kind {
                TurnItemKind::DynamicTool { tool_name, input, .. } => dynamic_tool_surface(tool_name.as_deref(), input).map(str::to_owned),
                _ => None,
            },
        },
    };
    if surface.as_deref() == Some("computer") && source.as_ref().is_none_or(|s| s.kind != "computer") {
        source = Some(ToolSource { key: "computer-use".into(), name: "Computer Use".into(), kind: "computer".into() });
    }
    let viewed_image_path = match &item.kind {
        TurnItemKind::DynamicTool { viewed_image_path: Some(path), .. } => {
            let path = path.trim();
            let lower = path.to_lowercase();
            (!path.contains(['\r', '\n']) && IMAGE_PREVIEW_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))).then(|| path.to_owned())
        }
        _ => None,
    };
    let mut entry = WorkLogEntry {
        id: item.id.clone(),
        created_at: created_at(item),
        run_id: item.run_id.clone(),
        tone: tone_of(&item.kind),
        item_type: Some(item_type_name(&item.kind)),
        status: Some(status_of(item)),
        item: Some(item.clone()),
        visibility: Some(row.visibility),
        tool_surface: surface,
        tool_source: source,
        viewed_image_path,
        ..WorkLogEntry::default()
    };
    match &item.kind {
        TurnItemKind::ThreadCreated { .. } => entry.label = "Created thread".into(),
        TurnItemKind::Compaction { summary, before_token_count, after_token_count } => {
            entry.label = context_compaction_label(&item.status, *before_token_count, *after_token_count);
            entry.source_activity = Some(SourceActivity::Compaction);
            entry.detail = summary.as_deref().and_then(non_empty);
        }
        TurnItemKind::Reasoning { text, .. } => {
            entry.label = title.unwrap_or_else(|| "Thinking".into());
            entry.detail = non_empty(text);
        }
        TurnItemKind::CommandExecution { input, .. } => {
            entry.label = title.clone().unwrap_or_else(|| "Ran command".into());
            entry.command = Some(input.clone());
            entry.raw_command = Some(input.clone());
            entry.tool_title = Some(title.unwrap_or_else(|| "Command".into()));
        }
        TurnItemKind::FileChange { file_name, changes, .. } => {
            let changes = changes.as_deref().unwrap_or_default();
            entry.label = title.clone().unwrap_or_else(|| {
                if changes.len() > 1 { format!("Changed {} files", changes.len()) } else { format!("Changed {file_name}") }
            });
            entry.changed_files =
                if changes.is_empty() { vec![file_name.clone()] } else { changes.iter().map(|c| c.path.clone()).collect() };
            entry.tool_title = Some(title.unwrap_or_else(|| "File change".into()));
        }
        TurnItemKind::FileSearch { pattern, .. } => {
            let search = pattern.as_ref().and_then(|p| format_search_tool_label(&json!({ "pattern": p })));
            entry.label = title.clone().or(search).unwrap_or_else(|| "Searched files".into());
            entry.detail = pattern.as_deref().and_then(non_empty);
            entry.tool_title = Some(title.unwrap_or_else(|| "File search".into()));
        }
        TurnItemKind::WebSearch { patterns, .. } => {
            entry.label = title.clone().unwrap_or_else(|| "Searched the web".into());
            entry.detail = patterns.as_ref().filter(|p| !p.is_empty()).map(|p| p.join(", "));
            entry.tool_title = Some(title.unwrap_or_else(|| "Web search".into()));
        }
        TurnItemKind::Checkpoint { files, .. } => {
            entry.label = title.unwrap_or_else(|| "Checkpoint captured".into());
            entry.changed_files = files.iter().map(|f| f.path.clone()).collect();
        }
        TurnItemKind::SystemNotice { message } => {
            entry.label = message.clone();
            entry.source_activity = Some(SourceActivity::RuntimeWarning);
        }
        TurnItemKind::Error { failure, retry } => {
            let (label, detail) = provider_error_presentation(item);
            entry.label = label;
            entry.detail = Some(detail);
            entry.source_activity = if failure.class == "usage_limit" && item.status != "completed" {
                Some(SourceActivity::RuntimeWarning)
            } else if retry.is_none() {
                Some(SourceActivity::RuntimeError)
            } else {
                None
            };
        }
        TurnItemKind::DynamicTool { tool_name, input, .. } => {
            let data = json!({ "toolName": tool_name, "input": input });
            let classified = classify_tool_activity(Some("dynamic_tool_call"), None, Some(&data));
            let read_path = collect_tool_file_paths(&json!({ "input": input })).into_iter().next();
            let fallback = || tool_name.clone().unwrap_or_else(|| "Tool call".into());
            entry.label = title.clone().unwrap_or_else(|| match classified {
                ToolActivityAction::Read => format_read_tool_label(read_path.as_deref().unwrap_or_default(), 0),
                ToolActivityAction::Search => format_search_tool_label(&json!({ "input": input })).unwrap_or_else(fallback),
                _ => fallback(),
            });
            entry.tool_title = Some(title.or_else(|| tool_name.clone()).unwrap_or_else(|| "Tool".into()));
        }
        TurnItemKind::TodoList { steps, .. } => {
            let done = steps.iter().filter(|s| s.status == "completed").count();
            let current = steps.iter().find(|s| s.status == "running").or_else(|| steps.iter().find(|s| s.status == "pending"));
            entry.label = format!("Tasks {done}/{}{}", steps.len(), current.map(|s| format!(" · {}", s.text)).unwrap_or_default());
        }
        TurnItemKind::ApprovalRequest { prompt, request_kind, .. } => {
            entry.label = title.unwrap_or_else(|| "Approval requested".into());
            entry.detail = Some(prompt.clone().unwrap_or_else(|| request_kind.clone()));
        }
        TurnItemKind::UserInputRequest { question_answer, .. } => {
            entry.label = title.unwrap_or_else(|| if question_answer.is_some() { "Answered questions".into() } else { "Input requested".into() });
            entry.question_answer = question_answer.clone();
        }
        TurnItemKind::Notification { summary, .. } => {
            entry.label = summary.clone();
            entry.tone = Tone::Info;
            entry.status = None;
            entry.tool_surface = None;
            entry.tool_source = None;
        }
        other => entry.label = title.unwrap_or_else(|| item_type_name(other).replace('_', " ")),
    }
    entry
}

fn is_standalone(kind: &TurnItemKind) -> bool {
    matches!(
        kind,
        TurnItemKind::Fork { .. }
            | TurnItemKind::Handoff { .. }
            | TurnItemKind::RunInterruptRequest { .. }
            | TurnItemKind::RunInterruptResult { .. }
            | TurnItemKind::SecretRequest { .. }
            | TurnItemKind::LinkRequest { .. }
            | TurnItemKind::Subagent { .. }
    )
}

fn chat_message(item: &Arc<TurnItem>) -> Option<ChatMessage> {
    let (id, role, text, streaming, meta) = match &item.kind {
        TurnItemKind::UserMessage {
            message_id, text, input_intent, model_switch, scheduled_task_id, sender_thread_id, created_by, creation_source, idle_compaction, context, ..
        } => {
            let guest = context.as_ref().and_then(|c| c.get("guest"));
            let meta = MessageMeta {
                input_intent: Some(input_intent.clone()),
                created_by: created_by.clone(),
                creation_source: creation_source.clone(),
                scheduled_task_id: scheduled_task_id.clone(),
                sender_thread_id: sender_thread_id.clone(),
                idle_compaction: idle_compaction.clone(),
                guest_name: guest.and_then(|g| g.get("name")).and_then(Value::as_str).map(str::to_owned),
                guest_text: guest.and_then(|g| g.get("text")).and_then(Value::as_str).map(str::to_owned),
                model_switch: model_switch.clone(),
            };
            (message_id, Role::User, text, false, meta)
        }
        TurnItemKind::AssistantMessage { message_id, text, streaming, .. } => {
            (message_id, Role::Assistant, text, *streaming, MessageMeta::default())
        }
        _ => return None,
    };
    Some(ChatMessage {
        id: id.clone(),
        role,
        text: Arc::from(text.as_str()),
        run_id: item.run_id.clone(),
        streaming,
        created_at: created_at(item),
        updated_at: item.updated_at.clone(),
        meta,
        item: Some(item.clone()),
    })
}

/// Reuses the entry of every item whose `Arc` is unchanged since the last derive, so a
/// streaming token rebuilds one entry, not the transcript: `reuseTimelineEntries`.
#[derive(Default)]
pub struct EntriesCache {
    by_item: HashMap<usize, Arc<TimelineEntry>>,
    attempts: Vec<(String, String, Option<String>)>,
    nodes: usize,
    /// Per visible item: its `Arc` address and the index of the entry it produced.
    positions: Vec<(usize, Option<usize>)>,
    entries: Vec<Arc<TimelineEntry>>,
}

impl EntriesCache {
    pub fn entries(&self) -> &[Arc<TimelineEntry>] {
        &self.entries
    }
}

struct Lookups<'a> {
    attempt_by_root: HashMap<&'a str, &'a RunAttempt>,
    nodes: HashMap<&'a str, (Option<&'a str>, Option<&'a str>)>,
}

impl<'a> Lookups<'a> {
    fn new(attempts: &'a [RunAttempt], nodes: &'a [Entity]) -> Self {
        let node = |entity: &'a Entity| {
            let get = |key: &str| entity.value.get(key).and_then(Value::as_str);
            (entity.id.as_str(), (get("rootNodeId"), get("parentNodeId")))
        };
        Lookups {
            attempt_by_root: attempts.iter().map(|a| (a.root_node_id.as_str(), a)).collect(),
            nodes: nodes.iter().map(node).collect(),
        }
    }

    fn attempt(&self, item: &TurnItem) -> Option<TimelineAttempt> {
        let (Some(run_id), Some(mut node_id)) = (item.run_id.as_deref(), item.node_id.as_deref()) else { return None };
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(node_id) {
                return None;
            }
            let found = self.attempt_by_root.get(node_id).filter(|a| a.run_id == run_id).or_else(|| {
                let (root, _) = self.nodes.get(node_id)?;
                self.attempt_by_root.get((*root)?).filter(|a| a.run_id == run_id)
            });
            if let Some(attempt) = found {
                return Some(TimelineAttempt {
                    id: attempt.id.clone(),
                    run_id: attempt.run_id.clone(),
                    status: attempt.status.clone(),
                    completed_at: attempt.completed_at.clone(),
                });
            }
            let (_, parent) = self.nodes.get(node_id)?;
            node_id = (*parent)?;
        }
    }
}

fn plan_status<'a>(plans: &'a [Entity], plan_id: &str) -> &'a str {
    plans
        .iter()
        .find(|p| p.id == plan_id && p.value.get("kind").and_then(Value::as_str) == Some("proposed_plan"))
        .and_then(|p| p.value.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("active")
}

/// Whether a cached entry still stands for its item: plans also read the plan's status.
fn still_current(entry: &TimelineEntry, plans: &[Entity]) -> bool {
    match &entry.kind {
        EntryKind::ProposedPlan(plan) => plan.status == plan_status(plans, &plan.id),
        _ => true,
    }
}

fn build_entry(row: &ProjectedTurnItem, lookups: &Lookups, plans: &[Entity]) -> Arc<TimelineEntry> {
    let item = &row.item;
    let kind = if let Some(message) = chat_message(item) {
        EntryKind::Message(Arc::new(message))
    } else if let TurnItemKind::ProposedPlan { plan_id, markdown, .. } = &item.kind {
        EntryKind::ProposedPlan(Arc::new(ProposedPlan {
            id: plan_id.clone(),
            run_id: item.run_id.clone(),
            markdown: Arc::from(markdown.as_str()),
            status: plan_status(plans, plan_id).to_owned(),
            updated_at: item.updated_at.clone(),
        }))
    } else if let Some(reference) = html_render_reference(item) {
        EntryKind::HtmlRender(Arc::new(HtmlRenderEntry { run_id: item.run_id.clone(), reference, item: item.clone() }))
    } else if let crate::turn_item::TurnItemKind::DynamicTool { tool_name, output, .. } = &item.kind
        && let Some(reference) = crate::mcp_apps::from_tool_item(tool_name.as_deref(), output.as_ref())
    {
        EntryKind::McpApp(Arc::new(super::McpAppEntry { run_id: item.run_id.clone(), reference, source_thread_id: row.source_thread_id.clone(), source_item_id: row.source_item_id.clone(), item: item.clone() }))
    } else if is_standalone(&item.kind) {
        EntryKind::Event(item.clone())
    } else {
        EntryKind::Work(Arc::new(projected_work_entry(row)))
    };
    let id = match &kind {
        EntryKind::Message(message) => message.id.clone(),
        _ => item.id.clone(),
    };
    Arc::new(TimelineEntry { id, created_at: created_at(item), attempt: lookups.attempt(item), kind })
}

fn entry_item(entry: &TimelineEntry) -> Option<&Arc<TurnItem>> {
    match &entry.kind {
        EntryKind::Message(message) => message.item.as_ref(),
        EntryKind::Work(work) => work.item.as_ref(),
        EntryKind::Event(item) => Some(item),
        EntryKind::HtmlRender(render) => Some(&render.item),
        EntryKind::McpApp(app) => Some(&app.item),
        EntryKind::ProposedPlan(_) => None,
    }
}

/// A completed `html_render` call's page (`htmlRenderFromToolItem`).
fn html_render_reference(item: &TurnItem) -> Option<crate::html_render::HtmlRenderReference> {
    let TurnItemKind::DynamicTool { tool_name, output, .. } = &item.kind else { return None };
    if item.status != "completed" {
        return None;
    }
    crate::html_render::from_tool_item(tool_name.as_deref(), output.as_ref())
}

/// An item that changed in place maps to the same single entry, unless it takes part in
/// cross-item rules (user messages and question answers dedupe) or starts or stops being shown:
/// `canRederiveInPlace`.
fn rederives_in_place(previous: &TurnItem, next: &TurnItem) -> bool {
    previous.id == next.id
        && std::mem::discriminant(&previous.kind) == std::mem::discriminant(&next.kind)
        && !matches!(next.kind, TurnItemKind::UserMessage { .. } | TurnItemKind::UserInputRequest { .. } | TurnItemKind::ProposedPlan { .. })
        && !is_workspace_preparation(next)
        && previous.started_at == next.started_at
}

impl EntriesCache {
    /// Patches only the entries whose items changed in place; `None` when the shape changed.
    fn patch<'a>(
        &mut self,
        visible: &[ProjectedTurnItem],
        lookups: &mut Option<Lookups<'a>>,
        attempts: &'a [RunAttempt],
        nodes: &'a [Entity],
        plans: &[Entity],
    ) -> Option<()> {
        if visible.len() != self.positions.len() {
            return None;
        }
        let mut changed = Vec::new();
        for (index, row) in visible.iter().enumerate() {
            let key = Arc::as_ptr(&row.item) as usize;
            let (previous_key, entry_index) = self.positions[index];
            if key == previous_key {
                if let Some(entry_index) = entry_index
                    && !still_current(&self.entries[entry_index], plans)
                {
                    return None;
                }
                continue;
            }
            let entry_index = entry_index?;
            let previous = entry_item(&self.entries[entry_index])?;
            if !rederives_in_place(previous, &row.item) {
                return None;
            }
            changed.push((index, entry_index));
        }
        for (index, entry_index) in changed {
            let row = &visible[index];
            let lookups = lookups.get_or_insert_with(|| Lookups::new(attempts, nodes));
            let entry = build_entry(row, lookups, plans);
            self.by_item.remove(&self.positions[index].0);
            let key = Arc::as_ptr(&row.item) as usize;
            self.by_item.insert(key, entry.clone());
            self.positions[index].0 = key;
            self.entries[entry_index] = entry;
        }
        Some(())
    }
}

/// `deriveTimelineEntriesFromVisibleTurnItems` over the projection's visible items.
pub fn derive_entries(
    cache: &mut EntriesCache,
    visible: &[ProjectedTurnItem],
    attempts: &[RunAttempt],
    nodes: &[Entity],
    plans: &[Entity],
) -> Vec<Arc<TimelineEntry>> {
    update_entries(cache, visible, attempts, nodes, plans);
    cache.entries.clone()
}

/// Brings `cache.entries()` up to date with the visible items.
pub fn update_entries(cache: &mut EntriesCache, visible: &[ProjectedTurnItem], attempts: &[RunAttempt], nodes: &[Entity], plans: &[Entity]) {
    let attempt_key: Vec<(String, String, Option<String>)> =
        attempts.iter().map(|a| (a.id.clone(), a.status.clone(), a.completed_at.clone())).collect();
    let attempts_changed = attempt_key != cache.attempts;
    if attempts_changed {
        cache.by_item.clear();
        cache.attempts = attempt_key;
    }
    // A node's root and parent never change, so only a new node can resolve an attempt that
    // was missing; entries that already resolved keep theirs.
    let new_nodes = nodes.len() != cache.nodes;
    cache.nodes = nodes.len();
    let mut lookups: Option<Lookups> = None;
    if !attempts_changed && !new_nodes && cache.patch(visible, &mut lookups, attempts, nodes, plans).is_some() {
        return;
    }
    let folded_answers: HashSet<String> = visible
        .iter()
        .filter_map(|row| match &row.item.kind {
            TurnItemKind::UserInputRequest { request_id, question_answer: Some(_), .. } => Some(format!("async-answer:{request_id}")),
            _ => None,
        })
        .collect();
    let mut next = HashMap::with_capacity(visible.len());
    let mut entries = Vec::with_capacity(visible.len());
    let mut positions = Vec::with_capacity(visible.len());
    for row in visible {
        let item = &row.item;
        let key = Arc::as_ptr(item) as usize;
        let shown = !is_workspace_preparation(item)
            && match &item.kind {
                TurnItemKind::Checkpoint { .. } => false,
                TurnItemKind::UserMessage { input_intent, .. } if input_intent == "queued_turn" && item.status == "pending" => false,
                TurnItemKind::UserMessage { message_id, .. } => !folded_answers.contains(message_id),
                _ => true,
            };
        if !shown {
            positions.push((key, None));
            continue;
        }
        let reusable = cache.by_item.get(&key).filter(|entry| still_current(entry, plans));
        let entry = match reusable {
            Some(entry) => match (new_nodes, &entry.attempt) {
                (true, None) => match lookups.get_or_insert_with(|| Lookups::new(attempts, nodes)).attempt(item) {
                    Some(attempt) => Arc::new(TimelineEntry { attempt: Some(attempt), ..(**entry).clone() }),
                    None => entry.clone(),
                },
                _ => entry.clone(),
            },
            None => build_entry(row, lookups.get_or_insert_with(|| Lookups::new(attempts, nodes)), plans),
        };
        next.insert(key, entry.clone());
        positions.push((key, Some(entries.len())));
        entries.push(entry);
    }
    cache.by_item = next;
    cache.positions = positions;
    cache.entries = entries;
}
