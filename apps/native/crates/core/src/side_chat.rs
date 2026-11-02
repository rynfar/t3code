//! Side chats: a fork of a thread's latest stable point that lives in its parent's right panel.
//! Ports the commands `useSideChat.ts` and `SideChatPane.tsx` dispatch, the gates in
//! `ChatView.tsx` (`sideChatsOffered`, `sideChatDisabledReason`), Bring back's
//! `bringBackBlockedReason`, and `sideChatTabState` from `RightPanelTabs.tsx`.

use serde_json::{Value, json};

use crate::commands::CREATION_SOURCE;
use crate::shell::ThreadShell;
use crate::thread::Run;

pub const SIDE_CHAT_TITLE: &str = "Side chat";

/// `thread.fork` from the latest stable point into a hidden side chat; works while the parent runs.
pub fn fork_command(command_id: &str, source_thread_id: &str, target_thread_id: &str) -> Value {
    json!({
        "type": "thread.fork",
        "commandId": command_id,
        "createdBy": "user",
        "creationSource": CREATION_SOURCE,
        "sourceThreadId": source_thread_id,
        "targetThreadId": target_thread_id,
        "sourcePoint": { "type": "latest_stable" },
        "sideChat": true,
        "title": SIDE_CHAT_TITLE,
    })
}

/// The side chat's first message, as `startThreadTurn` sends it without a model override.
pub fn first_message_command(command_id: &str, thread_id: &str, message_id: &str, text: &str) -> Value {
    json!({
        "type": "message.dispatch",
        "commandId": command_id,
        "createdBy": "user",
        "creationSource": CREATION_SOURCE,
        "threadId": thread_id,
        "messageId": message_id,
        "text": text,
        "attachments": [],
        "titleSeed": crate::composer::title_seed(text),
        "deliveryIntent": "auto",
        "dispatchMode": { "type": "start_immediately" },
    })
}

/// `thread.delete`. With `only_if_unused` the server keeps a side chat that has a run or message,
/// which is how closing a tab drops only an empty one.
pub fn delete_command(command_id: &str, thread_id: &str, only_if_unused: bool) -> Value {
    let mut command = json!({ "type": "thread.delete", "commandId": command_id, "threadId": thread_id });
    if only_if_unused {
        command["onlyIfUnused"] = Value::Bool(true);
    }
    command
}

/// What the parent thread is, for the side chat gates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parent<'a> {
    pub is_side_chat: bool,
    pub is_subagent: bool,
    /// `None` until the server config arrives.
    pub capability: Option<bool>,
    /// The thread exists on the server (not a draft).
    pub is_server_thread: bool,
    pub worktree_path: Option<&'a str>,
    pub runs: &'a [Run],
}

impl<'a> Parent<'a> {
    pub fn of(thread: &'a ThreadShell, runs: &'a [Run], capability: Option<bool>) -> Parent<'a> {
        Parent {
            is_side_chat: thread.lineage.side_chat == Some(true),
            is_subagent: thread.lineage.relationship_to_parent.as_deref() == Some("subagent") || thread.delegated_from.is_some(),
            capability,
            is_server_thread: true,
            worktree_path: thread.worktree_path.as_deref(),
            runs,
        }
    }
}

/// `sideChatsOffered`: side chats do not nest or start from subagents, and older servers would
/// fork a listed thread.
pub fn offered(parent: &Parent) -> bool {
    !parent.is_side_chat && !parent.is_subagent && parent.capability == Some(true)
}

/// `orchestrationV2WorktreePending`: the thread waits on a worktree that is not set up yet.
pub fn worktree_pending(worktree_path: Option<&str>, runs: &[Run]) -> bool {
    worktree_path.is_none()
        && runs.iter().any(|run| {
            matches!(run.status.as_str(), "preparing" | "failed" | "interrupted")
                && run.workspace_preparation.as_ref().and_then(|preparation| preparation.get("type")).and_then(Value::as_str) == Some("worktree")
        })
}

/// `sideChatDisabledReason`: why the parent cannot start one, shown on its entry points.
pub fn disabled_reason(parent: &Parent) -> Option<&'static str> {
    if parent.is_side_chat {
        return Some("Side chats cannot start side chats.");
    }
    if parent.is_subagent {
        return Some("Subagent threads cannot start side chats.");
    }
    match parent.capability {
        None => return Some("Side chats are not ready yet. Try again in a moment."),
        Some(false) => return Some("Update this environment's T3 Code server to use side chats."),
        Some(true) => {}
    }
    if !parent.is_server_thread {
        return Some("Side chats start after the first message.");
    }
    worktree_pending(parent.worktree_path, parent.runs).then_some("Available once the thread's worktree is set up.")
}

/// `bringBackBlockedReason`: a pending merge-back would fail the main thread's queued message.
pub fn bring_back_blocked_reason(parent_active: bool, merge_run: Option<&Run>) -> Option<&'static str> {
    if parent_active {
        Some("Available when the main thread is idle.")
    } else if merge_run.is_none() {
        Some("Available after a finished reply.")
    } else {
        None
    }
}

pub const BRING_BACK_HINT: &str = "Adds this side chat to the main thread's next message.";

/// `sideChatTabState`: what a side chat tab's dot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabState {
    NeedsApproval,
    NeedsInput,
    Running,
}

impl TabState {
    pub fn of(thread: &ThreadShell) -> Option<TabState> {
        match thread.pending_runtime_request.as_ref().map(|request| request.kind.as_str()) {
            Some("user_input") => Some(TabState::NeedsInput),
            Some("auth_refresh") | None => thread.is_live().then_some(TabState::Running),
            Some(_) => Some(TabState::NeedsApproval),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TabState::NeedsApproval => "needs approval",
            TabState::NeedsInput => "needs input",
            TabState::Running => "running",
        }
    }
}

/// `openSideChatTab`: whether `thread` is a side chat of `parent_id`, so it opens in a tab there.
pub fn is_side_chat_of(thread: &ThreadShell, parent_id: &str) -> bool {
    thread.lineage.side_chat == Some(true) && thread.lineage.parent_thread_id.as_deref() == Some(parent_id)
}

#[cfg(test)]
mod tests;
