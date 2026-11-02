//! The environment shell: projects plus thread rows, from `orchestration.subscribeShell`.
//! Mirrors `OrchestrationV2ShellStreamItem` and `client-runtime/src/state/shellReducer.ts`.
//! Fields the native client does not read yet are left out; serde ignores them.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::model::ModelSelection;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectShell {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    #[serde(default)]
    pub repository_identity: Option<serde_json::Value>,
    #[serde(default)]
    pub default_model_selection: Option<ModelSelection>,
    #[serde(default)]
    pub pinned_at: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    /// `ProjectScript[]`, kept raw: the thread details decode them (`project_scripts`).
    #[serde(default)]
    pub scripts: Vec<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lineage {
    pub parent_thread_id: Option<String>,
    pub relationship_to_parent: Option<String>,
    pub root_thread_id: String,
    #[serde(default)]
    pub side_chat: Option<bool>,
}

impl Lineage {
    /// Subagents and side chats live inside their parent's view, never in thread lists.
    pub fn is_listed(&self) -> bool {
        self.relationship_to_parent.as_deref() != Some("subagent") && self.side_chat != Some(true)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRuntimeRequestSummary {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub blocking: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestVisibleMessage {
    pub id: String,
    pub role: String,
    pub text: String,
    pub updated_at: String,
}

/// Distinguishes an absent optional field (`None`) from an explicit `null` (`Some(None)`).
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingBackgroundTask {
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub child_thread_id: Option<String>,
    #[serde(default)]
    pub awaited: Option<bool>,
}

/// `OrchestrationV2LimitRecovery`: what to do when a usage limit resets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitRecovery {
    pub run_id: String,
    pub reset_at: String,
    pub auto_resume: bool,
    #[serde(default)]
    pub snooze: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadShell {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub provider_instance_id: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub lineage: Lineage,
    #[serde(default)]
    pub creation_source: Option<String>,
    #[serde(default)]
    pub delegated_from: Option<serde_json::Value>,
    #[serde(default)]
    pub active_provider_thread_id: Option<String>,
    pub latest_run_id: Option<String>,
    /// Stop was requested for the latest run; its end is quiet (no completion notification).
    #[serde(default)]
    pub latest_run_stop_requested: Option<bool>,
    #[serde(default)]
    pub latest_run_requested_at: Option<String>,
    #[serde(default)]
    pub latest_run_started_at: Option<String>,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub latest_run_completed_at: Option<Option<String>>,
    pub active_run_id: Option<String>,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub activity_run_started_at: Option<Option<String>>,
    #[serde(default)]
    pub activity_run_status: Option<String>,
    #[serde(default)]
    pub blocked_on_thread_wait: Option<bool>,
    pub status: String,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_class: Option<String>,
    pub pending_runtime_request: Option<PendingRuntimeRequestSummary>,
    #[serde(default)]
    pub pending_guest_message_count: Option<u64>,
    pub latest_visible_message: Option<LatestVisibleMessage>,
    pub latest_user_message_at: Option<String>,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub latest_user_authored_message_at: Option<Option<String>>,
    pub has_actionable_proposed_plan: bool,
    #[serde(default)]
    pub pending_background_tasks: Vec<PendingBackgroundTask>,
    #[serde(default)]
    pub goal: Option<serde_json::Value>,
    #[serde(default)]
    pub limit_recovery: Option<LimitRecovery>,
    pub item_count: u64,
    pub visible_item_count: u64,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub settled_override: Option<String>,
    pub settled_at: Option<String>,
    #[serde(default)]
    pub unsettled_at: Option<String>,
    #[serde(default)]
    pub snoozed_until: Option<String>,
    #[serde(default)]
    pub snoozed_at: Option<String>,
    #[serde(default)]
    pub auto_settle_disabled_at: Option<String>,
    #[serde(default)]
    pub opens: Option<serde_json::Value>,
    #[serde(default)]
    pub title_regeneration: Option<serde_json::Value>,
    #[serde(default)]
    pub pinned_at: Option<String>,
    #[serde(default)]
    pub pin_order_key: Option<String>,
    #[serde(default)]
    pub active_order_key: Option<String>,
    #[serde(default)]
    pub last_visited_at: Option<String>,
    pub deleted_at: Option<String>,
    /// Linked pull requests; links that do not decode are dropped.
    #[serde(default, deserialize_with = "crate::pull_requests::link::thread_links", skip_serializing)]
    pub pull_requests: Vec<crate::pull_requests::link::ThreadLink>,
}

impl ThreadShell {
    pub fn is_live(&self) -> bool {
        self.activity_run_status.is_some()
            || matches!(self.status.as_str(), "queued" | "preparing" | "starting" | "running" | "waiting")
    }

    pub fn is_settled(&self) -> bool {
        self.settled_override.as_deref() == Some("settled")
    }

    pub fn needs_attention(&self) -> bool {
        self.pending_runtime_request.is_some()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshot {
    pub snapshot_sequence: u64,
    pub projects: Vec<ProjectShell>,
    pub threads: Vec<ThreadShell>,
    #[serde(default)]
    pub archived_threads: Vec<ThreadShell>,
    /// Present when the shell was subscribed with `omitOlderSettled`.
    #[serde(default)]
    pub older_settled: Option<OlderSettledCut>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OlderSettledCount {
    pub project_id: String,
    pub count: u64,
}

/// `OrchestrationV2OlderSettledThreads`: where the shell cut settled threads, and how many it left out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OlderSettledCut {
    pub before: String,
    pub as_of_sequence: u64,
    pub counts: Vec<OlderSettledCount>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
pub enum ShellStreamItem {
    #[serde(rename = "synchronized")]
    Synchronized,
    #[serde(rename = "snapshot")]
    Snapshot {
        snapshot: Box<ShellSnapshot>,
        /// Present on metadata-only enrichment refreshes: same structure and sequence, only
        /// repository identities for these workspace roots are authoritative.
        #[serde(default, rename = "resolvedRepositoryIdentityRoots")]
        resolved_repository_identity_roots: Option<Vec<String>>,
    },
    #[serde(rename = "project.updated")]
    ProjectUpdated { sequence: u64, project: Box<ProjectShell> },
    #[serde(rename = "project.removed")]
    ProjectRemoved {
        sequence: u64,
        #[serde(rename = "projectId")]
        project_id: String,
    },
    #[serde(rename = "thread.updated")]
    ThreadUpdated { sequence: u64, location: String, thread: Box<ThreadShell> },
    #[serde(rename = "thread.removed")]
    ThreadRemoved {
        sequence: u64,
        #[serde(rename = "threadId")]
        thread_id: String,
    },
    #[serde(other)]
    Unknown,
}

/// Client-side shell state. `synchronized` flips once the server has replayed everything after
/// the snapshot, which is when "no threads" means empty rather than loading.
#[derive(Debug, Clone, Default)]
pub struct ShellState {
    pub snapshot: Option<ShellSnapshot>,
    pub synchronized: bool,
    /// Bumped by every authoritative snapshot. Reads of rows the shell omits belong to one
    /// generation (`shell.ts` `generation`), since a new snapshot may predate their changes.
    pub generation: u64,
    /// `thread.removed` sequences since the last authoritative snapshot, so a family or page
    /// read from before a removal cannot bring the thread back (`shell.ts` `removedAt`).
    pub removed_at: HashMap<String, u64>,
    /// A fresh stamp whenever a row is replaced, so observers skip unchanged rows the way the web
    /// compares row identity, without comparing or cloning them.
    revisions: HashMap<String, u64>,
}

/// Process-wide, so stamps from a replaced `ShellState` (a new pairing) never repeat.
static NEXT_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl ShellState {
    pub fn sequence(&self) -> Option<u64> {
        self.snapshot.as_ref().map(|snapshot| snapshot.snapshot_sequence)
    }

    /// The row's current stamp; it changes whenever the shell replaces the row.
    pub fn revision(&self, thread_id: &str) -> Option<u64> {
        self.revisions.get(thread_id).copied()
    }

    fn stamp_all(&mut self) {
        let revision = next_revision();
        let threads = self.snapshot.as_ref().map_or(&[][..], |snapshot| &snapshot.threads);
        self.revisions = threads.iter().map(|thread| (thread.id.clone(), revision)).collect();
    }

    /// Returns true when visible state changed.
    pub fn apply(&mut self, item: ShellStreamItem) -> bool {
        match item {
            ShellStreamItem::Synchronized => !std::mem::replace(&mut self.synchronized, true),
            ShellStreamItem::Snapshot { snapshot, resolved_repository_identity_roots: Some(roots) } => {
                match self.snapshot.as_mut() {
                    Some(previous) => merge_identity_enrichment(previous, &snapshot, &roots),
                    None => {
                        self.snapshot = Some(*snapshot);
                        self.stamp_all();
                        true
                    }
                }
            }
            ShellStreamItem::Snapshot { snapshot, resolved_repository_identity_roots: None } => {
                let mut snapshot = *snapshot;
                if let Some(previous) = &self.snapshot {
                    for project in &mut snapshot.projects {
                        let prior = previous.projects.iter().find(|p| p.id == project.id);
                        retain_repository_identity(prior, project);
                    }
                }
                self.snapshot = Some(snapshot);
                self.generation += 1;
                self.removed_at.clear();
                self.stamp_all();
                true
            }
            ShellStreamItem::Unknown => false,
            delta => {
                let (touched, removed) = match &delta {
                    ShellStreamItem::ThreadUpdated { thread, location, .. } => (Some((thread.id.clone(), location == "active")), None),
                    ShellStreamItem::ThreadRemoved { thread_id, sequence } => (Some((thread_id.clone(), false)), Some(*sequence)),
                    _ => (None, None),
                };
                let applied = self.snapshot.as_mut().is_some_and(|snapshot| apply_delta(snapshot, delta));
                if applied && let Some((thread_id, listed)) = touched {
                    if let Some(sequence) = removed {
                        self.removed_at.insert(thread_id.clone(), sequence);
                    }
                    if listed {
                        self.revisions.insert(thread_id, next_revision());
                    } else {
                        self.revisions.remove(&thread_id);
                    }
                }
                applied
            }
        }
    }

    pub fn project(&self, id: &str) -> Option<&ProjectShell> {
        self.snapshot.as_ref()?.projects.iter().find(|project| project.id == id)
    }

    pub fn thread(&self, id: &str) -> Option<&ThreadShell> {
        self.snapshot.as_ref()?.threads.iter().find(|thread| thread.id == id)
    }
}

fn apply_delta(snapshot: &mut ShellSnapshot, item: ShellStreamItem) -> bool {
    let sequence = match &item {
        ShellStreamItem::ProjectUpdated { sequence, .. }
        | ShellStreamItem::ProjectRemoved { sequence, .. }
        | ShellStreamItem::ThreadUpdated { sequence, .. }
        | ShellStreamItem::ThreadRemoved { sequence, .. } => *sequence,
        _ => return false,
    };
    if sequence <= snapshot.snapshot_sequence {
        return false;
    }
    snapshot.snapshot_sequence = sequence;
    match item {
        ShellStreamItem::ProjectUpdated { project, .. } => {
            let mut project = *project;
            retain_repository_identity(snapshot.projects.iter().find(|p| p.id == project.id), &mut project);
            upsert(&mut snapshot.projects, project, |p| &p.id);
        }
        ShellStreamItem::ProjectRemoved { project_id, .. } => {
            snapshot.projects.retain(|project| project.id != project_id);
        }
        ShellStreamItem::ThreadUpdated { location, thread, .. } => {
            let thread = *thread;
            if location == "active" {
                if snapshot.threads.contains(&thread) {
                    return false;
                }
                upsert(&mut snapshot.threads, thread.clone(), |t| &t.id);
            } else {
                snapshot.threads.retain(|existing| existing.id != thread.id);
            }
            // The archive has its own bounded query; archive-located deltas leave the shell.
            snapshot.archived_threads.retain(|existing| existing.id != thread.id);
        }
        ShellStreamItem::ThreadRemoved { thread_id, .. } => {
            snapshot.threads.retain(|thread| thread.id != thread_id);
            snapshot.archived_threads.retain(|thread| thread.id != thread_id);
        }
        _ => {}
    }
    true
}

/// Enrichment is async: a project can arrive without the identity an earlier snapshot resolved for
/// the same workspace root. Keep it so multi-environment grouping does not split.
fn retain_repository_identity(previous: Option<&ProjectShell>, next: &mut ProjectShell) {
    if next.repository_identity.is_none()
        && let Some(previous) = previous.filter(|p| p.workspace_root == next.workspace_root)
    {
        next.repository_identity = previous.repository_identity.clone();
    }
}

/// `mergeShellSnapshotProjects` with enrichment options: only repository identities change.
fn merge_identity_enrichment(previous: &mut ShellSnapshot, next: &ShellSnapshot, roots: &[String]) -> bool {
    let mut changed = false;
    for project in &mut previous.projects {
        let Some(candidate) = next.projects.iter().find(|c| c.id == project.id) else { continue };
        if candidate.workspace_root != project.workspace_root {
            continue;
        }
        let authoritative = roots.contains(&project.workspace_root);
        let adopt = if authoritative {
            project.repository_identity != candidate.repository_identity
        } else {
            project.repository_identity.is_none() && candidate.repository_identity.is_some()
        };
        if adopt {
            project.repository_identity = candidate.repository_identity.clone();
            changed = true;
        }
    }
    changed
}

fn upsert<T>(items: &mut Vec<T>, item: T, id: impl Fn(&T) -> &String) {
    match items.iter().position(|candidate| id(candidate) == id(&item)) {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
}

#[cfg(test)]
mod tests;
