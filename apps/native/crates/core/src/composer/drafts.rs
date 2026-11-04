//! Per-thread composer drafts that survive thread switches and restarts: the prompt, the model,
//! permission and mode choices, mentioned threads, terminal excerpts, and uploaded attachments. Ports the persisted
//! shape of `web/src/composerDraftStore.ts` (one entry per `composerTargetKey`, empty drafts
//! dropped, writes debounced by `COMPOSER_PERSIST_DEBOUNCE_MS`). Attachments persist only once
//! uploaded: their server copy outlives a restart (pending uploads are kept a day), their bytes
//! do not.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::attachments::{Attachment, AttachmentKind, Upload};
use super::context::TerminalContext;
use super::menu::ThreadContext;
use crate::model::ModelSelection;
use crate::server_config::{InteractionMode, RuntimeMode};

/// `COMPOSER_PERSIST_DEBOUNCE_MS`.
pub const PERSIST_DEBOUNCE_MS: u64 = 300;
/// The file under `T3_NATIVE_HOME` the drafts live in.
pub const STORE_NAME: &str = "composer-drafts";
/// Old drafts beyond this many are dropped, oldest first, so the file stays small.
pub const MAX_DRAFTS: usize = 200;
const VERSION: u32 = 1;

/// The key a draft is stored under: an existing thread, or a project's new-thread draft.
pub fn thread_key(thread_id: &str) -> String {
    format!("thread:{thread_id}")
}

pub fn new_thread_key(project_id: &str) -> String {
    format!("draft:{project_id}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAttachment {
    pub id: String,
    pub image: bool,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub pasted_text: bool,
    pub attachment_id: String,
}

impl SavedAttachment {
    pub fn of(attachment: &Attachment) -> Option<SavedAttachment> {
        Some(SavedAttachment {
            id: attachment.id.clone(),
            image: attachment.kind == AttachmentKind::Image,
            name: attachment.name.clone(),
            mime_type: attachment.mime_type.clone(),
            size_bytes: attachment.size_bytes,
            pasted_text: attachment.pasted_text,
            attachment_id: attachment.attachment_id()?.to_owned(),
        })
    }

    pub fn restore(&self) -> Attachment {
        Attachment {
            id: self.id.clone(),
            kind: if self.image { AttachmentKind::Image } else { AttachmentKind::File },
            name: self.name.clone(),
            mime_type: self.mime_type.clone(),
            size_bytes: self.size_bytes,
            pasted_text: self.pasted_text,
            upload: Upload::Ready { attachment_id: self.attachment_id.clone() },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedThread {
    pub environment_id: String,
    pub thread_id: String,
    pub title: String,
}

/// One target's draft.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Draft {
    pub prompt: String,
    pub model_selection: Option<ModelSelection>,
    pub workspace: Option<crate::workspace::DraftWorkspace>,
    pub runtime_mode: Option<RuntimeMode>,
    pub interaction_mode: Option<InteractionMode>,
    pub threads: Vec<SavedThread>,
    /// Terminal excerpts the prompt links, with their text (`PersistedTerminalContextDraft`).
    pub terminals: Vec<TerminalContext>,
    pub review_comments: Vec<super::review::ReviewComment>,
    pub attachments: Vec<SavedAttachment>,
    pub context_records: Vec<std::sync::Arc<Value>>,
    #[serde(skip)]
    pub pending_attachment_count: usize,
    pub updated_at: i64,
    pub created_at: Option<i64>,
}

impl Draft {
    pub fn has_user_content(&self) -> bool {
        !self.prompt.trim().is_empty() || !self.attachments.is_empty() || !self.terminals.is_empty() || !self.review_comments.is_empty() || self.pending_attachment_count > 0
    }

    /// Nothing worth keeping: `composerDraftHasUserContent` plus the choices.
    pub fn is_empty(&self) -> bool {
        self.prompt.trim().is_empty()
            && self.attachments.is_empty()
            && self.pending_attachment_count == 0
            && self.terminals.is_empty()
            && self.review_comments.is_empty()
            && self.workspace.is_none()
            && self.model_selection.is_none()
            && self.runtime_mode.is_none()
            && self.interaction_mode.is_none()
    }

    pub fn thread_contexts(&self) -> Vec<ThreadContext> {
        self.threads.iter().map(|thread| ThreadContext::new(&thread.environment_id, &thread.thread_id, &thread.title)).collect()
    }

    /// The excerpts a draft keeps: those its prompt links.
    pub fn saved_terminals(terminals: &[TerminalContext], prompt: &str) -> Vec<TerminalContext> {
        super::context::referenced(prompt, terminals).into_iter().cloned().collect()
    }

    /// The prompt as restored: drafts from before inline links get their excerpts linked
    /// (`migrateLegacyTerminalContextPlaceholders`).
    pub fn restored_prompt(&self) -> String {
        super::context::migrate_legacy_placeholders(&self.prompt, &self.terminals)
    }

    pub fn saved_threads(threads: &[ThreadContext], prompt: &str) -> Vec<SavedThread> {
        let referenced = super::menu::referenced_context_ids(prompt);
        threads
            .iter()
            .filter(|thread| referenced.contains(&thread.context_id.as_str()))
            .map(|thread| SavedThread { environment_id: thread.environment_id.clone(), thread_id: thread.thread_id.clone(), title: thread.label.clone() })
            .collect()
    }
}

/// Every draft, as one JSON document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DraftStore {
    drafts: BTreeMap<String, Draft>,
    pruned_keys: Vec<String>,
}

impl DraftStore {
    /// Reads the saved document; a draft that does not decode is dropped alone.
    pub fn from_value(value: Option<&Value>) -> DraftStore {
        let drafts = value
            .filter(|value| value.get("version").and_then(Value::as_u64) == Some(u64::from(VERSION)))
            .and_then(|value| value.get("drafts"))
            .and_then(Value::as_object)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|(key, draft)| Some((key.clone(), serde_json::from_value::<Draft>(draft.clone()).ok()?)))
                    .filter(|(_, draft)| !draft.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        DraftStore { drafts, pruned_keys: Vec::new() }
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({ "version": VERSION, "drafts": self.drafts })
    }

    pub fn get(&self, key: &str) -> Option<&Draft> {
        self.drafts.get(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.drafts.keys()
    }

    /// Stores a draft, or drops it when empty. Returns whether anything changed.
    pub fn set(&mut self, key: &str, mut draft: Draft) -> bool {
        draft.created_at = self.drafts.get(key).and_then(|held| held.created_at).or(draft.created_at).or(Some(draft.updated_at));
        if draft.is_empty() {
            return self.drafts.remove(key).is_some();
        }
        let changed = self.drafts.get(key).is_none_or(|current| !same_content(current, &draft));
        if changed {
            self.drafts.insert(key.to_owned(), draft);
            self.prune();
        }
        changed
    }

    pub fn restore(&mut self, key: &str, draft: Draft, live: &Draft) -> bool {
        if live.has_user_content() || self.get(key).is_some_and(Draft::has_user_content) { return false; }
        self.drafts.remove(key);
        self.set(key, draft);
        true
    }

    pub fn take_pruned_keys(&mut self) -> Vec<String> { std::mem::take(&mut self.pruned_keys) }

    pub fn remove(&mut self, key: &str) -> bool {
        self.drafts.remove(key).is_some()
    }

    fn prune(&mut self) {
        while self.drafts.len() > MAX_DRAFTS {
            let Some(oldest) = self.drafts.iter().min_by_key(|(_, draft)| draft.updated_at).map(|(key, _)| key.clone()) else { break };
            self.drafts.remove(&oldest);
            self.pruned_keys.push(oldest);
        }
    }
}

/// Equal apart from when it was written, so re-saving an unchanged draft writes nothing.
fn same_content(left: &Draft, right: &Draft) -> bool {
    Draft { updated_at: 0, ..left.clone() } == Draft { updated_at: 0, ..right.clone() }
}

#[cfg(test)]
mod tests;
