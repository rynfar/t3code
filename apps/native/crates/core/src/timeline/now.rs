//! The "what the thread is doing now" lines after the last row that are not about the run itself
//! (`ThreadNow.tsx`, `ThreadActivityBanners.tsx`): subagents and side chats waiting on the user
//! (`childThreadInputsAtom`, `childThreadInputsTitle` in `client-runtime/src/state/threadShell.ts`),
//! background work (`presentPendingBackgroundWork` in `threadExecution.ts`), and the answer to
//! Force stop (`useForceStopThread.ts`).

use serde_json::{Value, json};

use super::command_display_text;
use crate::shell::{PendingBackgroundTask, ThreadShell};
use crate::sidebar::thread::{side_chat_request, subagent_request};

/// `WS_METHODS.forceStopThread`.
pub const FORCE_STOP_THREAD: &str = "orchestration.forceStopThread";

/// `BACKGROUND_WORK_KINDS`: display order, singular and plural nouns.
fn kind_nouns(kind: &str) -> (u8, &'static str, &'static str) {
    match kind {
        "subagent" => (0, "subagent", "subagents"),
        "command" => (1, "command", "commands"),
        "monitor" => (2, "monitor", "monitors"),
        _ => (3, "background task", "background tasks"),
    }
}

/// `formatSubagentDisplayTitle`: drops the `Subagent:` prefix, and names `/root/…/x_y` paths
/// by their last segment in title case.
pub fn format_subagent_display_title(title: &str) -> String {
    let display = title.strip_prefix("Subagent:").or_else(|| title.strip_prefix("subagent:")).map(str::trim_start).unwrap_or(title);
    let Some(path) = display.strip_prefix("/root/") else { return display.to_owned() };
    let Some(last) = path.trim_end_matches('/').rsplit('/').next().filter(|segment| !segment.is_empty()) else { return display.to_owned() };
    let spaced = last.replace('_', " ");
    let words: Vec<String> = spaced
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect();
    let name = words.join(" ");
    if name.is_empty() { display.to_owned() } else { name }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundItem {
    pub task_id: String,
    pub kind: String,
    pub label: String,
    /// The label is a command line, drawn in mono.
    pub command: bool,
    pub child_thread_id: Option<String>,
}

/// `PendingBackgroundWorkPresentation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundWork {
    pub title: String,
    pub items: Vec<BackgroundItem>,
    /// The agent resumes when this work ends (`backgroundWorkHoldsCompletion`).
    pub waiting: bool,
}

impl BackgroundWork {
    /// The line's detail: every label when there are several; a single task's title says it all.
    pub fn description(&self) -> Option<String> {
        match self.items.as_slice() {
            [_] => None,
            items => Some(items.iter().map(|item| item.label.as_str()).collect::<Vec<_>>().join(", ")),
        }
    }

    /// The child thread a single subagent task runs in, for its Open action.
    pub fn only_child_thread(&self) -> Option<&str> {
        match self.items.as_slice() {
            [only] => only.child_thread_id.as_deref(),
            _ => None,
        }
    }

    pub fn all_commands(&self) -> bool {
        self.items.iter().all(|item| item.kind != "subagent")
    }
}

fn join_with_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

pub fn background_work(tasks: &[PendingBackgroundTask]) -> Option<BackgroundWork> {
    if tasks.is_empty() {
        return None;
    }
    let waiting = crate::sidebar::thread::background_work_holds_completion(tasks);
    let mut items: Vec<BackgroundItem> = tasks
        .iter()
        .map(|task| {
            let (_, singular, _) = kind_nouns(&task.kind);
            let description = task.description.as_deref().map(str::trim).filter(|text| !text.is_empty());
            let command = matches!(task.kind.as_str(), "command" | "monitor");
            let label = description.map(|text| match task.kind.as_str() {
                "subagent" => format_subagent_display_title(text).trim().to_owned(),
                _ if command => command_display_text(text),
                _ => text.to_owned(),
            });
            let label = label.filter(|label| !label.is_empty());
            BackgroundItem {
                task_id: task.task_id.clone(),
                kind: task.kind.clone(),
                command: command && label.is_some(),
                label: label.unwrap_or_else(|| singular.to_owned()),
                child_thread_id: (task.kind == "subagent").then(|| task.child_thread_id.clone()).flatten(),
            }
        })
        .collect();
    items.sort_by_key(|item| kind_nouns(&item.kind).0);
    if let [only] = items.as_slice() {
        let (_, noun, _) = kind_nouns(&only.kind);
        let title = if only.label != noun {
            format!("{} · {}", if waiting { "Waiting" } else { "Running" }, only.label)
        } else if waiting {
            format!("Waiting on a {noun}")
        } else {
            format!("Running a {noun}")
        };
        return Some(BackgroundWork { title, items, waiting });
    }
    let mut groups: Vec<(&str, &str, usize)> = Vec::new();
    for item in &items {
        let (_, singular, plural) = kind_nouns(&item.kind);
        match groups.iter_mut().find(|(noun, _, _)| *noun == singular) {
            Some(group) => group.2 += 1,
            None => groups.push((singular, plural, 1)),
        }
    }
    let parts: Vec<String> = groups.iter().map(|(singular, plural, count)| format!("{count} {}", if *count == 1 { singular } else { plural })).collect();
    Some(BackgroundWork { title: format!("{} {}", if waiting { "Waiting on" } else { "Running" }, join_with_and(&parts)), items, waiting })
}

/// A subagent or side chat waiting on the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildInput {
    pub thread_id: String,
    pub title: String,
    pub approval: bool,
    pub side_chat: bool,
}

/// The child-input line: who is waiting, and on what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildInputs {
    pub children: Vec<ChildInput>,
}

impl ChildInputs {
    /// `childThreadInputsTitle`.
    pub fn title(&self) -> String {
        let count = self.children.len();
        let approvals = self.children.iter().filter(|child| child.approval).count();
        let need = if approvals == 0 { "input" } else if approvals == count { "approval" } else { "you" };
        let side_chats = self.children.iter().filter(|child| child.side_chat).count();
        let noun = if side_chats == count { "side chat" } else if side_chats == 0 { "subagent" } else { "thread" };
        if count == 1 { format!("{} needs {need}", if noun == "side chat" { "Side chat" } else { "Subagent" }) } else { format!("{count} {noun}s need {need}") }
    }

    /// The first child's action: `Open approval` or `Open question`.
    pub fn action(&self) -> &'static str {
        if self.children.first().is_some_and(|child| child.approval) { "Open approval" } else { "Open question" }
    }

    pub fn approval_icon(&self) -> bool {
        self.children.first().is_some_and(|child| child.approval)
    }
}

fn delegated_parent(thread: &ThreadShell) -> Option<&str> {
    thread.delegated_from.as_ref()?.get("threadId")?.as_str()
}

/// `childThreadInputsAtom`: descendants (subagents, side chats, delegated tasks) holding a
/// request, unless the parent is itself waiting on the user (`childInputNowItem`).
pub fn child_inputs(threads: &[ThreadShell], thread_id: &str, parent_awaiting: bool) -> Option<ChildInputs> {
    if parent_awaiting {
        return None;
    }
    let parent_of = |thread: &ThreadShell| -> Option<String> {
        let lineage = &thread.lineage;
        match lineage.parent_thread_id.as_deref() {
            Some(parent) if lineage.relationship_to_parent.as_deref() == Some("subagent") || lineage.side_chat == Some(true) => Some(parent.to_owned()),
            _ => delegated_parent(thread).map(str::to_owned),
        }
    };
    let mut seen = vec![thread_id.to_owned()];
    let mut pending = vec![thread_id.to_owned()];
    let mut children = Vec::new();
    while let Some(parent) = pending.pop() {
        for thread in threads.iter().filter(|thread| thread.archived_at.is_none() && parent_of(thread).as_deref() == Some(parent.as_str())) {
            if seen.contains(&thread.id) {
                continue;
            }
            seen.push(thread.id.clone());
            pending.push(thread.id.clone());
            let request = subagent_request(thread).or_else(|| side_chat_request(thread));
            if let Some(request) = request {
                children.push(ChildInput {
                    thread_id: thread.id.clone(),
                    title: thread.title.clone(),
                    approval: request == crate::sidebar::thread::SubagentRequest::Approval,
                    side_chat: thread.lineage.side_chat == Some(true),
                });
            }
        }
    }
    (!children.is_empty()).then_some(ChildInputs { children })
}

/// `OrchestrationV2ForceStopThreadInput`: the run that ignored Stop, so a Force stop left open
/// cannot hit a newer run.
pub fn force_stop_request(thread_id: &str, run_id: &str) -> Value {
    json!({ "threadId": thread_id, "runId": run_id })
}

/// The follow-up notice for a Force stop that did not end everything, as `useForceStopThread` words it.
pub fn force_stop_notice(result: &Value) -> Option<(String, Option<String>)> {
    let others = result.get("sharedWithThreadIds").and_then(Value::as_array).map_or(0, Vec::len);
    match result.get("outcome").and_then(Value::as_str) {
        Some("releasing") => Some(("Turn stopped; the agent is still shutting down".into(), None)),
        Some("shared") => {
            let count = if others == 0 { "other".to_owned() } else { others.to_string() };
            let threads = if others == 1 { "thread" } else { "threads" };
            Some(("Turn stopped; the agent process kept running".into(), Some(format!("It is shared with {count} working {threads}. Stop those to free it."))))
        }
        _ => None,
    }
}
