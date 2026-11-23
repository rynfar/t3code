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
    #[serde(default)]
    pub has_custom_title: Option<bool>,
    pub workspace_root: String,
    #[serde(default)]
    pub repository_identity: Option<serde_json::Value>,
    #[serde(default)]
    pub default_model_selection: Option<ModelSelection>,
    #[serde(default)]
    pub pinned_at: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    /// A workspace image chosen as the icon; the server serves it as the project favicon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon_path: Option<String>,
    /// `ProjectIconOverride`, kept raw (`settings::projects::ProjectIcon` reads it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_icon: Option<serde_json::Value>,
    /// `ProjectScript[]`, kept raw: the thread details decode them (`project_scripts`).
    #[serde(default)]
    pub scripts: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked_folders: Vec<serde_json::Value>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingBackgroundTask {
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub child_thread_id: Option<String>,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub source_thread_id: Option<String>,
    #[serde(default)]
    pub native_item_id: Option<String>,
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

/// `OrchestrationV2LinkOrigin`: when a delegated thread was started over a paired link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkOrigin {
    pub session_id: String,
    pub label: String,
    #[serde(default)]
    pub access: Option<String>,
}

impl LinkOrigin {
    /// `linkOriginIsFenced`: only explicit `"full-access"` links are unfenced.
    pub fn is_fenced(&self) -> bool {
        self.access.as_deref() != Some("full-access")
    }
}

/// Result of the most recent title-regeneration request for a thread shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastTitleRegeneration {
    pub request_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub completed_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "ThreadShellWire")]
pub struct ThreadShell {
    pub id: String,
    pub project_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub provider_instance_id: String,
    /// Instances the thread ran on before, oldest first (`providerInstanceHistory`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_instance_history: Vec<String>,
    pub model_selection: ModelSelection,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub lineage: Lineage,
    /// `{ type: "run", threadId, runId }` for a fork from a run; other sources name no thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<serde_json::Value>,
    #[serde(default)]
    pub creation_source: Option<String>,
    #[serde(default)]
    pub delegated_from: Option<serde_json::Value>,
    #[serde(default)]
    pub link_origin: Option<LinkOrigin>,
    #[serde(default)]
    pub active_provider_thread_id: Option<String>,
    pub latest_run_id: Option<String>,
    /// Stop was requested for the latest run; its end is quiet (no completion notification).
    #[serde(default)]
    pub latest_run_stop_requested: Option<bool>,
    /// The latest run answers a background-work progress wake, like a Monitor line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_background_wake: Option<bool>,
    #[serde(default)]
    pub latest_run_requested_at: Option<String>,
    #[serde(default)]
    pub latest_run_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_completed_at: Option<Option<String>>,
    pub active_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_run_started_at: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_run_status: Option<Option<String>>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_title_regeneration: Option<LastTitleRegeneration>,
    #[serde(default)]
    pub pinned_at: Option<String>,
    #[serde(default)]
    pub pin_order_key: Option<String>,
    #[serde(default)]
    pub active_order_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_visited_at: Option<Option<String>>,
    pub deleted_at: Option<String>,
    /// Linked pull requests; links that do not decode are dropped. Falls back to legacy single
    /// `linkedPullRequest` only when `pullRequests` is absent (`threadPullRequestsOf`).
    #[serde(default, skip_serializing)]
    pub pull_requests: Vec<crate::pull_requests::link::ThreadLink>,
    /// Issues linked to this thread (`OrchestrationV2ThreadShellJson.issues`); malformed entries are
    /// dropped leniently so pre-linking servers and future fields never fail the shell frame.
    #[serde(default, skip_serializing)]
    pub issues: Vec<crate::issues::wire::ThreadIssueLink>,
}

/// Legacy single-link payload (`ThreadLinkedPullRequest` in `packages/contracts/src/threadPullRequest.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyLinkedPullRequest {
    repository: String,
    number: u64,
    url: String,
}

impl LegacyLinkedPullRequest {
    /// Mirrors `legacyThreadPullRequestKey` + `threadPullRequestsOf`
    /// (`packages/source-control-core/src/shared/threadPullRequests.ts`).
    fn into_thread_link(self) -> Option<crate::pull_requests::link::ThreadLink> {
        let repository = self.repository.trim();
        let url = self.url.trim();
        if repository.is_empty() || url.is_empty() || self.number == 0 {
            return None;
        }
        let (host, repo, number) = match crate::pull_requests::link::parse_change_request_url(url) {
            Some(parsed) if parsed.number == self.number => (parsed.host, parsed.repository, parsed.number),
            _ => {
                let host = url::Url::parse(url)
                    .ok()
                    .and_then(|u| u.host_str().map(|h| h.trim().to_ascii_lowercase()))
                    .filter(|h| !h.is_empty())
                    .unwrap_or_else(|| "unknown".to_owned());
                (host, repository.to_ascii_lowercase(), self.number)
            }
        };
        Some(crate::pull_requests::link::ThreadLink {
            host,
            repository: repo,
            number,
            url: url.to_owned(),
            source: "manual".to_owned(),
            linked_at: "1970-01-01T00:00:00.000Z".to_owned(),
            snapshot: None,
            stack: None,
            watch: None,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadShellWire {
    id: String,
    project_id: String,
    title: String,
    #[serde(default, deserialize_with = "stored_tags")]
    tags: Vec<String>,
    provider_instance_id: String,
    #[serde(default)]
    provider_instance_history: Vec<String>,
    model_selection: ModelSelection,
    runtime_mode: String,
    interaction_mode: String,
    branch: Option<String>,
    worktree_path: Option<String>,
    lineage: Lineage,
    #[serde(default)]
    forked_from: Option<serde_json::Value>,
    #[serde(default)]
    creation_source: Option<String>,
    #[serde(default)]
    delegated_from: Option<serde_json::Value>,
    #[serde(default)]
    link_origin: Option<LinkOrigin>,
    #[serde(default)]
    active_provider_thread_id: Option<String>,
    latest_run_id: Option<String>,
    #[serde(default)]
    latest_run_stop_requested: Option<bool>,
    #[serde(default)]
    latest_run_background_wake: Option<bool>,
    #[serde(default)]
    latest_run_requested_at: Option<String>,
    #[serde(default)]
    latest_run_started_at: Option<String>,
    #[serde(default, deserialize_with = "present")]
    latest_run_completed_at: Option<Option<String>>,
    active_run_id: Option<String>,
    #[serde(default, deserialize_with = "present")]
    activity_run_started_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    activity_run_status: Option<Option<String>>,
    #[serde(default)]
    blocked_on_thread_wait: Option<bool>,
    status: String,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    last_error_class: Option<String>,
    pending_runtime_request: Option<PendingRuntimeRequestSummary>,
    #[serde(default)]
    pending_guest_message_count: Option<u64>,
    latest_visible_message: Option<LatestVisibleMessage>,
    latest_user_message_at: Option<String>,
    #[serde(default, deserialize_with = "present")]
    latest_user_authored_message_at: Option<Option<String>>,
    has_actionable_proposed_plan: bool,
    #[serde(default)]
    pending_background_tasks: Vec<PendingBackgroundTask>,
    #[serde(default)]
    goal: Option<serde_json::Value>,
    #[serde(default)]
    limit_recovery: Option<LimitRecovery>,
    item_count: u64,
    visible_item_count: u64,
    created_at: String,
    updated_at: String,
    archived_at: Option<String>,
    settled_override: Option<String>,
    settled_at: Option<String>,
    #[serde(default)]
    unsettled_at: Option<String>,
    #[serde(default)]
    snoozed_until: Option<String>,
    #[serde(default)]
    snoozed_at: Option<String>,
    #[serde(default)]
    auto_settle_disabled_at: Option<String>,
    #[serde(default)]
    opens: Option<serde_json::Value>,
    #[serde(default)]
    title_regeneration: Option<serde_json::Value>,
    #[serde(default)]
    last_title_regeneration: Option<LastTitleRegeneration>,
    #[serde(default)]
    pinned_at: Option<String>,
    #[serde(default)]
    pin_order_key: Option<String>,
    #[serde(default)]
    active_order_key: Option<String>,
    #[serde(default, deserialize_with = "present")]
    last_visited_at: Option<Option<String>>,
    deleted_at: Option<String>,
    /// `Some(links)` when `pullRequests` was explicitly sent (even `[]`), `None` when omitted.
    #[serde(default, deserialize_with = "present_thread_links")]
    pull_requests: Option<Vec<crate::pull_requests::link::ThreadLink>>,
    /// Legacy single `linkedPullRequest` from older V2 payloads.
    #[serde(default, deserialize_with = "lenient_legacy_pull_request")]
    linked_pull_request: Option<LegacyLinkedPullRequest>,
    /// Linked issues (`ThreadIssueLink[]`); absent on pre-linking servers.
    #[serde(default, deserialize_with = "lenient_thread_issue_links")]
    issues: Vec<crate::issues::wire::ThreadIssueLink>,
}

impl From<ThreadShellWire> for ThreadShell {
    fn from(wire: ThreadShellWire) -> Self {
        let pull_requests = match wire.pull_requests {
            Some(links) => links,
            None => wire
                .linked_pull_request
                .and_then(LegacyLinkedPullRequest::into_thread_link)
                .into_iter()
                .collect(),
        };
        ThreadShell {
            id: wire.id,
            project_id: wire.project_id,
            title: wire.title,
            tags: wire.tags,
            provider_instance_id: wire.provider_instance_id,
            provider_instance_history: wire.provider_instance_history,
            model_selection: wire.model_selection,
            runtime_mode: wire.runtime_mode,
            interaction_mode: wire.interaction_mode,
            branch: wire.branch,
            worktree_path: wire.worktree_path,
            lineage: wire.lineage,
            forked_from: wire.forked_from,
            creation_source: wire.creation_source,
            delegated_from: wire.delegated_from,
            link_origin: wire.link_origin,
            active_provider_thread_id: wire.active_provider_thread_id,
            latest_run_id: wire.latest_run_id,
            latest_run_stop_requested: wire.latest_run_stop_requested,
            latest_run_background_wake: wire.latest_run_background_wake,
            latest_run_requested_at: wire.latest_run_requested_at,
            latest_run_started_at: wire.latest_run_started_at,
            latest_run_completed_at: wire.latest_run_completed_at,
            active_run_id: wire.active_run_id,
            activity_run_started_at: wire.activity_run_started_at,
            activity_run_status: wire.activity_run_status,
            blocked_on_thread_wait: wire.blocked_on_thread_wait,
            status: wire.status,
            last_error: wire.last_error,
            last_error_class: wire.last_error_class,
            pending_runtime_request: wire.pending_runtime_request,
            pending_guest_message_count: wire.pending_guest_message_count,
            latest_visible_message: wire.latest_visible_message,
            latest_user_message_at: wire.latest_user_message_at,
            latest_user_authored_message_at: wire.latest_user_authored_message_at,
            has_actionable_proposed_plan: wire.has_actionable_proposed_plan,
            pending_background_tasks: wire.pending_background_tasks,
            goal: wire.goal,
            limit_recovery: wire.limit_recovery,
            item_count: wire.item_count,
            visible_item_count: wire.visible_item_count,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
            archived_at: wire.archived_at,
            settled_override: wire.settled_override,
            settled_at: wire.settled_at,
            unsettled_at: wire.unsettled_at,
            snoozed_until: wire.snoozed_until,
            snoozed_at: wire.snoozed_at,
            auto_settle_disabled_at: wire.auto_settle_disabled_at,
            opens: wire.opens,
            title_regeneration: wire.title_regeneration,
            last_title_regeneration: wire.last_title_regeneration,
            pinned_at: wire.pinned_at,
            pin_order_key: wire.pin_order_key,
            active_order_key: wire.active_order_key,
            last_visited_at: wire.last_visited_at,
            deleted_at: wire.deleted_at,
            pull_requests,
            issues: wire.issues,
        }
    }
}

fn lenient_thread_issue_links<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<crate::issues::wire::ThreadIssueLink>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    let Some(items) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(items
        .iter()
        .filter_map(|item| serde_json::from_value(item.clone()).ok())
        .collect())
}

fn present_thread_links<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<crate::pull_requests::link::ThreadLink>>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    let Some(items) = value.as_array() else {
        return Ok(None);
    };
    Ok(Some(
        items
            .iter()
            .filter_map(|item| serde_json::from_value(item.clone()).ok())
            .collect(),
    ))
}

fn lenient_legacy_pull_request<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<LegacyLinkedPullRequest>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}

impl ThreadShell {
    pub fn visited_at_iso(&self) -> Option<&str> {
        self.last_visited_at.as_ref().and_then(|value| value.as_deref())
    }

    pub fn tracks_visits(&self) -> bool {
        self.last_visited_at.is_some()
    }
    pub fn is_live(&self) -> bool {
        self.activity_run_status.as_ref().is_some_and(Option::is_some)
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

/// `StoredThreadTags`: a future malformed list must not fail the shell frame.
fn stored_tags<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

#[cfg(test)]
mod legacy_pr_tests {
    use super::*;
    use serde_json::json;

    fn base_thread(extra: serde_json::Map<String, serde_json::Value>) -> ThreadShell {
        let mut obj = json!({
            "id": "thr-1",
            "projectId": "proj-1",
            "title": "Fix bug",
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": "main",
            "worktreePath": null,
            "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thr-1" },
            "latestRunId": null,
            "activeRunId": null,
            "status": "idle",
            "pendingRuntimeRequest": null,
            "latestVisibleMessage": null,
            "latestUserMessageAt": null,
            "hasActionableProposedPlan": false,
            "itemCount": 0,
            "visibleItemCount": 0,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "deletedAt": null
        })
        .as_object()
        .unwrap()
        .clone();
        obj.extend(extra);
        serde_json::from_value(serde_json::Value::Object(obj)).expect("valid ThreadShell")
    }

    #[test]
    fn decodes_legacy_single_linked_pull_request_when_pull_requests_absent() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "linkedPullRequest".into(),
            json!({
                "projectId": "proj-1",
                "repository": "PingDotGG/T3Code",
                "number": 42,
                "url": "https://github.com/PingDotGG/T3Code/pull/42"
            }),
        );
        let thread = base_thread(extra);
        assert_eq!(thread.pull_requests.len(), 1);
        let link = &thread.pull_requests[0];
        assert_eq!(link.host, "github.com");
        assert_eq!(link.repository, "pingdotgg/t3code");
        assert_eq!(link.number, 42);
        assert_eq!(link.url, "https://github.com/PingDotGG/T3Code/pull/42");
        assert_eq!(link.source, "manual");
        assert_eq!(link.linked_at, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn explicit_empty_pull_requests_array_overrides_legacy_linked_pull_request() {
        let mut extra = serde_json::Map::new();
        extra.insert("pullRequests".into(), json!([]));
        extra.insert(
            "linkedPullRequest".into(),
            json!({
                "projectId": "proj-1",
                "repository": "pingdotgg/t3code",
                "number": 42,
                "url": "https://github.com/pingdotgg/t3code/pull/42"
            }),
        );
        let thread = base_thread(extra);
        assert!(thread.pull_requests.is_empty(), "explicit empty pullRequests means unlinked");
    }

    #[test]
    fn malformed_legacy_linked_pull_request_does_not_fail_shell_frame() {
        let mut extra = serde_json::Map::new();
        extra.insert("linkedPullRequest".into(), json!({ "number": "not-a-number" }));
        let thread = base_thread(extra);
        assert!(thread.pull_requests.is_empty());

        // Self-hosted / custom URL host falls back to hostname.
        let mut custom = serde_json::Map::new();
        custom.insert(
            "linkedPullRequest".into(),
            json!({
                "projectId": "proj-1",
                "repository": "Org/Repo",
                "number": 9,
                "url": "https://git.corp.example/custom/9"
            }),
        );
        let thread = base_thread(custom);
        assert_eq!(thread.pull_requests.len(), 1);
        assert_eq!(thread.pull_requests[0].host, "git.corp.example");
        assert_eq!(thread.pull_requests[0].repository, "org/repo");
    }

    #[test]
    fn decodes_thread_issues_and_drops_malformed_entries() {
        // Pre-linking server omits `issues` -> defaults to empty.
        let bare = base_thread(serde_json::Map::new());
        assert!(bare.issues.is_empty());

        let mut extra = serde_json::Map::new();
        extra.insert(
            "issues".into(),
            json!([
                {
                    "tracker": "github",
                    "host": "github.com",
                    "scope": "acme/web",
                    "key": "19",
                    "projectId": "proj-1",
                    "url": "https://github.com/acme/web/issues/19",
                    "source": "manual",
                    "linkedAt": "2026-01-03T00:00:00Z",
                    "displayKey": "#19",
                    "title": "Fix hover preview"
                },
                { "broken": true },
                {
                    "tracker": "linear",
                    "host": "linear.app",
                    "scope": "ENG",
                    "key": "ENG-7",
                    "projectId": "proj-1",
                    "url": "https://linear.app/acme/issue/ENG-7",
                    "source": "agent",
                    "linkedAt": "2026-01-04T00:00:00Z",
                    "displayKey": "ENG-7",
                    "title": "Linear task"
                }
            ]),
        );
        let thread = base_thread(extra);
        assert_eq!(thread.issues.len(), 2);
        assert_eq!(thread.issues[0].display_key, "#19");
        assert_eq!(
            thread.issues[0].source,
            crate::issues::wire::ThreadIssueLinkSource::Manual
        );
        assert_eq!(thread.issues[1].display_key, "ENG-7");
        assert_eq!(
            thread.issues[1].tracker,
            crate::issues::wire::IssueTrackerKind::Linear
        );
        assert_eq!(
            thread.issues[1].source,
            crate::issues::wire::ThreadIssueLinkSource::Agent
        );
    }
}
