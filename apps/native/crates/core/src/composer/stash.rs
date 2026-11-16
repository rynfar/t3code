//! Prompt stash and prompt history. The stash (`web/src/promptStashStore.ts`, `useComposerStash.ts`,
//! `ComposerStashMenu.tsx`) parks a prompt with its mentions, terminal excerpts and uploaded attachments under
//! Mod+S, twenty deep, newest first. History (`composerPromptHistory.ts`) recalls what the user
//! sent in this thread with Up and Down at the prompt's edges.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::context::TerminalContext;
use super::drafts::{SavedAttachment, SavedThread};
use crate::thread::ThreadProjection;
use crate::turn_item::TurnItemKind;

/// `MAX_STASH_ENTRIES`.
pub const MAX_ENTRIES: usize = 20;
pub const STORE_NAME: &str = "prompt-stash";
const VERSION: u32 = 1;
const SNIPPET_MAX_CHARS: usize = 90;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StashEntry {
    pub id: String,
    /// Epoch ms.
    pub created_at: i64,
    pub prompt: String,
    #[serde(default)]
    pub attachments: Vec<SavedAttachment>,
    #[serde(default)]
    pub threads: Vec<SavedThread>,
    #[serde(default)]
    pub terminals: Vec<TerminalContext>,
    #[serde(default)]
    pub review_comments: Vec<super::review::ReviewComment>,
    #[serde(default)]
    pub context_records: Vec<std::sync::Arc<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Value>,
}

impl StashEntry {
    /// `stashEntrySnippet`.
    pub fn snippet(&self) -> String {
        let plain = crate::assistant_citations::to_plain_text(&self.prompt);
        let trimmed = plain.split_whitespace().collect::<Vec<_>>().join(" ");
        if !trimmed.is_empty() {
            let chars: Vec<char> = trimmed.chars().collect();
            return if chars.len() > SNIPPET_MAX_CHARS { format!("{}…", chars[..SNIPPET_MAX_CHARS].iter().collect::<String>()) } else { trimmed };
        }
        let images = self.attachments.iter().filter(|attachment| attachment.image).count();
        let files = self.attachments.len() - images;
        let count = self.attachments.len();
        if count == 0 {
            return "(empty)".to_owned();
        }
        let label = if images > 0 && files > 0 { "attachment" } else if files > 0 { "file" } else { "image" };
        format!("({count} {label}{})", if count == 1 { "" } else { "s" })
    }
}

/// Every stashed prompt, newest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stash {
    pub entries: Vec<StashEntry>,
}

impl Stash {
    pub fn from_value(value: Option<&Value>) -> Stash {
        let entries = value
            .filter(|value| value.get("version").and_then(Value::as_u64) == Some(u64::from(VERSION)))
            .and_then(|value| value.get("entries"))
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(|entry| serde_json::from_value(entry.clone()).ok()).take(MAX_ENTRIES).collect())
            .unwrap_or_default();
        Stash { entries }
    }

    pub fn to_value(&self) -> Value {
        json!({ "version": VERSION, "entries": self.entries })
    }

    /// Adds an entry on top; returns the oldest one when the stash overflows.
    pub fn push(&mut self, entry: StashEntry) -> Option<StashEntry> {
        self.entries.insert(0, entry);
        (self.entries.len() > MAX_ENTRIES).then(|| self.entries.pop()).flatten()
    }

    pub fn take(&mut self, id: &str) -> Option<StashEntry> {
        let index = self.entries.iter().position(|entry| entry.id == id)?;
        Some(self.entries.remove(index))
    }
}

/// A restored prompt joins what is already typed after a blank line.
pub fn merge_prompt(current: &str, restored: &str) -> String {
    match (restored.is_empty(), current.trim().is_empty()) {
        (true, _) => current.to_owned(),
        (false, true) => restored.to_owned(),
        (false, false) => format!("{}\n\n{restored}", current.trim_end()),
    }
}

/// Normalizes a prompt for stashing: trims outer whitespace while keeping a trailing space after
/// an inline context or citation link so restoring doesn't glue text onto the chip (`useComposerStash.ts`).
pub fn stash_prompt_text(raw: &str) -> String {
    let trimmed_start = raw.trim_start();
    let trimmed = trimmed_start.trim_end();
    if trimmed.is_empty() {
        return String::new();
    }
    let ends_with_link = (trimmed.ends_with(')')
        && (super::menu::context_references(trimmed).last().is_some_and(|r| r.end == trimmed.len())
            || crate::assistant_citations::collect(trimmed).last().is_some_and(|(range, _)| range.end == trimmed.len())
            || (trimmed.contains("](file://") && trimmed.rfind('[').is_some())))
        || crate::markdown::mentions::collect_composer_model_mentions(trimmed).last().is_some_and(|m| m.end == trimmed.len());
    if trimmed_start.len() > trimmed.len() && ends_with_link {
        format!("{trimmed} ")
    } else {
        trimmed.to_owned()
    }
}

// --- Prompt history (`composerPromptHistory.ts`) ---

/// One recallable prompt and the message it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: String,
    pub prompt: String,
}

/// `ComposerPromptHistoryPosition`: the entry recalled and the text it put in the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPosition {
    pub entry_id: String,
    pub recalled: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryStep {
    pub position: Option<HistoryPosition>,
    pub prompt: String,
}

/// `recallableComposerPrompt`: the words the user typed, without links whose records are gone,
/// the ultrathink prefix, trailing review/legacy context blocks, or the prompts the app writes itself.
pub fn recallable_prompt(text: &str) -> String {
    crate::timeline::recallable_prompt(text)
}

/// `buildComposerPromptHistoryEntries` from the thread's user messages: only what the user
/// typed (no guests, automations or agents), consecutive repeats collapsed.
pub fn history_entries(projection: &ThreadProjection) -> Vec<HistoryEntry> {
    let mut entries: Vec<HistoryEntry> = Vec::new();
    for projected in &projection.visible_turn_items {
        let TurnItemKind::UserMessage { message_id, text, scheduled_task_id, created_by, context, .. } = &projected.item.kind else {
            continue;
        };
        let attributed = scheduled_task_id.is_some()
            || created_by.as_deref() == Some("agent")
            || context.as_ref().and_then(|context| context.get("guest")).is_some_and(|guest| !guest.is_null());
        if attributed {
            continue;
        }
        let prompt = recallable_prompt(text);
        if prompt.is_empty() {
            continue;
        }
        match entries.last_mut() {
            Some(previous) if previous.prompt == prompt => previous.id = message_id.clone(),
            _ => entries.push(HistoryEntry { id: message_id.clone(), prompt }),
        }
    }
    entries
}

/// `stepComposerPromptHistory`.
pub fn step_history(backward: bool, entries: &[HistoryEntry], position: Option<&HistoryPosition>, current: &str) -> Option<HistoryStep> {
    let active = position.filter(|position| position.recalled == current).and_then(|position| {
        entries.iter().position(|entry| entry.id == position.entry_id).or_else(|| entries.iter().rposition(|entry| entry.prompt == position.recalled))
    });
    let recall = |entry: &HistoryEntry| HistoryStep {
        position: Some(HistoryPosition { entry_id: entry.id.clone(), recalled: entry.prompt.clone() }),
        prompt: entry.prompt.clone(),
    };
    if backward {
        let index = match active {
            None if !current.is_empty() => return None,
            None => entries.len().checked_sub(1)?,
            Some(index) => index.checked_sub(1)?,
        };
        return entries.get(index).map(recall);
    }
    let index = active?;
    Some(entries.get(index + 1).map(recall).unwrap_or(HistoryStep { position: None, prompt: String::new() }))
}

/// Whether the caret sits on the prompt's first line (Up) or last line (Down), so the key
/// leaves the text for history instead of moving the caret.
pub fn caret_on_edge(text: &str, cursor: usize, start: bool) -> bool {
    let cursor = cursor.min(text.len());
    match start {
        true => text.get(..cursor).is_some_and(|before| !before.contains('\n')),
        false => text.get(cursor..).is_some_and(|after| !after.contains('\n')),
    }
}

#[cfg(test)]
mod tests;
