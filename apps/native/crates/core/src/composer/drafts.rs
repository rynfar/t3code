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
const VERSION: u32 = 7;
const SESSION_PREFIX: &str = "draft-session:";

/// The storage key for the composer of an existing thread.
pub fn thread_key(thread_id: &str) -> String {
    format!("thread:{thread_id}")
}

/// The project-based owner key still used by the terminal and panels.
pub fn new_thread_key(project_id: &str) -> String {
    format!("draft:{project_id}")
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DraftId(String);

impl DraftId {
    pub fn new(id: String) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftRef {
    pub id: DraftId,
    pub environment_id: Option<String>,
    pub project_id: String,
}

impl DraftRef {
    pub fn new(id: DraftId, environment_id: &str, project_id: &str) -> Self {
        Self {
            id,
            environment_id: (!environment_id.is_empty()).then(|| environment_id.to_owned()),
            project_id: project_id.to_owned(),
        }
    }

    /// Legacy project keys did not record an environment. Reserve an ID for each old key so
    /// decoding it again before the next save produces the same session.
    pub fn legacy(project_id: &str) -> Self {
        use std::fmt::Write as _;
        let mut id = String::from("legacy-");
        for byte in project_id.bytes() {
            let _ = write!(id, "{byte:02x}");
        }
        Self::new(DraftId::new(id), "", project_id)
    }

    pub fn key(&self) -> String {
        format!("{SESSION_PREFIX}{}:{}:{}", self.id.0, self.environment_id.as_deref().unwrap_or_default(), self.project_id)
    }

    pub fn from_key(key: &str) -> Option<Self> {
        let mut parts = key.strip_prefix(SESSION_PREFIX)?.splitn(3, ':');
        let (id, environment_id, project_id) = (parts.next()?, parts.next()?, parts.next()?);
        (!id.is_empty()).then(|| Self::new(DraftId::new(id.to_owned()), environment_id, project_id))
    }
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
    pub reply_to: Option<Value>,
    #[serde(skip)]
    pub pending_attachment_count: usize,
    pub updated_at: i64,
    pub created_at: Option<i64>,
}

impl Draft {
    pub fn has_user_content(&self) -> bool {
        !self.prompt.trim().is_empty() || !self.attachments.is_empty() || !self.terminals.is_empty() || !self.review_comments.is_empty() || self.reply_to.is_some() || self.pending_attachment_count > 0 || !self.threads.is_empty() || !self.context_records.is_empty()
    }

    /// Nothing worth keeping: `composerDraftHasUserContent` plus the choices.
    pub fn is_empty(&self) -> bool {
        !self.has_user_content()
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
    preferred: BTreeMap<String, DraftRef>,
}

impl DraftStore {
    /// Reads the saved document; a draft that does not decode is dropped alone, and bad fields
    /// within a draft degrade individually rather than wiping valid prompt/draft state (`#225`).
    pub fn from_value(value: Option<&Value>) -> DraftStore {
        let version = value.and_then(|value| value.get("version")).and_then(Value::as_u64);
        let valid_version = matches!(version, Some(1 | 3..=7));
        let drafts = value
            .filter(|_| valid_version)
            .and_then(|value| value.get("drafts"))
            .and_then(Value::as_object)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|(key, draft)| {
                        let key = if version != Some(u64::from(VERSION)) {
                            key.strip_prefix("draft:").map_or_else(|| key.clone(), |project| DraftRef::legacy(project).key())
                        } else {
                            key.clone()
                        };
                        Some((key, decode_draft(draft)?))
                    })
                    .filter(|(_, draft)| !draft.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let preferred = value.filter(|_| version == Some(u64::from(VERSION)))
            .and_then(|value| value.get("preferred"))
            .and_then(Value::as_object)
            .map(|entries| entries.iter().filter_map(|(key, draft)| {
                Some((key.clone(), serde_json::from_value(draft.clone()).ok()?))
            }).collect())
            .unwrap_or_default();
        DraftStore { drafts, preferred, pruned_keys: Vec::new() }
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({ "version": VERSION, "drafts": self.drafts, "preferred": self.preferred })
    }

    pub fn session_drafts(&self) -> impl Iterator<Item = (DraftRef, &Draft)> {
        self.drafts.iter().filter_map(|(key, draft)| Some((DraftRef::from_key(key)?, draft)))
    }

    pub fn reusable_session(
        &self,
        logical_project: &str,
        project_id: &str,
        active_in_project: Option<&DraftRef>,
        mut promoting: impl FnMut(&DraftRef) -> bool,
    ) -> Option<DraftRef> {
        let legacy = DraftRef::legacy(project_id);
        let preferred = self.preferred.get(logical_project)
            .or_else(|| self.drafts.contains_key(&legacy.key()).then_some(&legacy));
        preferred.into_iter().chain(active_in_project).find(|draft| {
            !promoting(draft) && self.get(&draft.key()).is_none_or(|content| !content.has_user_content())
        }).cloned()
    }

    pub fn remember_session(&mut self, logical_project: &str, draft: &DraftRef) -> bool {
        if self.preferred.get(logical_project) == Some(draft) {
            return false;
        }
        self.preferred.retain(|_, existing| existing.id != draft.id);
        self.preferred.insert(logical_project.to_owned(), draft.clone());
        true
    }

    pub fn forget_session(&mut self, draft: &DraftRef) -> Vec<String> {
        let mut forgotten = Vec::new();
        self.preferred.retain(|key, existing| {
            if existing == draft {
                forgotten.push(key.clone());
                false
            } else {
                true
            }
        });
        forgotten
    }

    pub fn restore_session(&mut self, draft: &DraftRef, logical_projects: &[String]) {
        for logical_project in logical_projects {
            self.preferred.entry(logical_project.clone()).or_insert_with(|| draft.clone());
        }
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

    /// Clears user content while keeping choices (`clearComposerContent`), dropping if empty.
    pub fn clear_content(&mut self, key: &str, updated_at: i64) -> bool {
        let Some(existing) = self.drafts.get(key).cloned() else { return false };
        let had_content = existing.has_user_content();
        let cleared = Draft {
            workspace: existing.workspace,
            model_selection: existing.model_selection,
            runtime_mode: existing.runtime_mode,
            interaction_mode: existing.interaction_mode,
            created_at: existing.created_at,
            updated_at,
            ..Draft::default()
        };
        if cleared.is_empty() {
            return self.drafts.remove(key).is_some();
        }
        self.drafts.insert(key.to_owned(), cleared);
        had_content
    }

    pub fn remove(&mut self, key: &str) -> bool {
        self.drafts.remove(key).is_some()
    }

    fn prune(&mut self) {
        while self.drafts.len() > MAX_DRAFTS {
            let Some(oldest) = self.drafts.iter().min_by_key(|(_, draft)| draft.updated_at).map(|(key, _)| key.clone()) else { break };
            self.drafts.remove(&oldest);
            if let Some(draft) = DraftRef::from_key(&oldest) {
                self.forget_session(&draft);
            }
            self.pruned_keys.push(oldest);
        }
    }
}

/// Equal apart from when it was written, so re-saving an unchanged draft writes nothing.
fn same_content(left: &Draft, right: &Draft) -> bool {
    Draft { updated_at: 0, ..left.clone() } == Draft { updated_at: 0, ..right.clone() }
}

fn decode_lenient_vec<T: serde::de::DeserializeOwned>(value: Option<&Value>) -> Vec<T> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|item| serde_json::from_value(item.clone()).ok()).collect())
        .unwrap_or_default()
}

fn trimmed_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Decodes a persisted `modelSelection` entry, upgrading legacy `provider` and object-shaped
/// `options` (`#225`) while preserving instance-scoped custom slugs verbatim (`#226`).
fn decode_model_selection_value(value: &Value, fallback_provider: Option<&str>) -> Option<ModelSelection> {
    let obj = value.as_object()?;
    let model = trimmed_str(obj.get("model"))?;
    let instance_id = trimmed_str(obj.get("instanceId"))
        .or_else(|| trimmed_str(obj.get("provider")))
        .or(fallback_provider.map(str::trim).filter(|s| !s.is_empty()))?;
    let selections = crate::traits::selections(obj.get("options"));
    Some(crate::traits::model_selection(instance_id, model, &selections))
}

fn decode_draft_model_selection(obj: &serde_json::Map<String, Value>) -> Option<ModelSelection> {
    if let Some(raw) = obj.get("modelSelection").filter(|v| !v.is_null())
        && let Some(selection) = decode_model_selection_value(raw, None)
    {
        return Some(selection);
    }
    if let Some(active) = trimmed_str(obj.get("activeProvider"))
        && let Some(by_provider) = obj.get("modelSelectionByProvider").and_then(Value::as_object)
        && let Some(raw) = by_provider.get(active)
        && let Some(selection) = decode_model_selection_value(raw, Some(active))
    {
        return Some(selection);
    }
    let provider = trimmed_str(obj.get("provider"))?;
    let model = trimmed_str(obj.get("model"))?;
    let mut selections = crate::traits::selections(
        obj.get("modelOptions").and_then(Value::as_object).and_then(|map| map.get(provider)),
    );
    let mut push_if_missing = |id: &str, raw: Option<&Value>| {
        if selections.iter().any(|s| s.id == id) {
            return;
        }
        if let Some(value) = raw.and_then(crate::traits::OptionValue::from_json) {
            selections.push(crate::traits::OptionSelection { id: id.to_owned(), value });
        }
    };
    if provider == "codex" {
        let effort = obj.get("effort").filter(|value| value.as_str().is_none_or(|s| !s.is_empty()));
        push_if_missing("reasoningEffort", effort);
        let fast = obj.get("codexFastMode").and_then(Value::as_bool) == Some(true)
            || obj.get("serviceTier").and_then(Value::as_str) == Some("fast");
        if fast {
            push_if_missing("fastMode", Some(&Value::Bool(true)));
        }
    }
    Some(crate::traits::model_selection(provider, model, &selections))
}

fn decode_draft(value: &Value) -> Option<Draft> {
    let obj = value.as_object()?;
    let prompt = obj.get("prompt").and_then(Value::as_str).unwrap_or_default().to_owned();
    let model_selection = decode_draft_model_selection(obj);
    let workspace = obj.get("workspace").and_then(|v| serde_json::from_value(v.clone()).ok());
    let runtime_mode = obj.get("runtimeMode").and_then(|v| serde_json::from_value(v.clone()).ok());
    let interaction_mode = obj.get("interactionMode").and_then(|v| serde_json::from_value(v.clone()).ok());
    let threads = decode_lenient_vec(obj.get("threads"));
    let terminals = decode_lenient_vec(obj.get("terminals"));
    let review_comments = decode_lenient_vec(obj.get("reviewComments"));
    let attachments = decode_lenient_vec(obj.get("attachments"));
    let context_records = obj
        .get("contextRecords")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter(|item| item.is_object()).cloned().map(std::sync::Arc::new).collect())
        .unwrap_or_default();
    let reply_to = obj.get("replyTo").filter(|v| v.is_object()).cloned();
    let updated_at = obj.get("updatedAt").and_then(Value::as_i64).unwrap_or(0);
    let created_at = obj.get("createdAt").and_then(Value::as_i64);
    Some(Draft {
        prompt,
        model_selection,
        workspace,
        runtime_mode,
        interaction_mode,
        threads,
        terminals,
        review_comments,
        attachments,
        context_records,
        reply_to,
        pending_attachment_count: 0,
        updated_at,
        created_at,
    })
}

#[cfg(test)]
mod tests;
