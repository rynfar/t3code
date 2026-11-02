//! Pull request wire types, mirroring `packages/contracts/src/pullRequest.ts`. Only the fields the
//! native client reads are decoded; unknown literals decode to `Unknown`, and an entry that does
//! not decode is dropped rather than failing its whole list.

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

macro_rules! literals {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $wire:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant,)*
            #[serde(other)]
            Unknown,
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $wire,)*
                    $name::Unknown => "unknown",
                }
            }
        }
    };
}

literals!(PrState { Open = "open", Closed = "closed", Merged = "merged" });
literals!(ReviewDecision { Approved = "approved", ChangesRequested = "changes-requested", ReviewRequired = "review-required" });
literals!(ChecksState { Passing = "passing", Failing = "failing", Pending = "pending" });
literals!(Mergeability { Mergeable = "mergeable", Conflicting = "conflicting" });
literals!(MergeMethod { Merge = "merge", Squash = "squash", Rebase = "rebase" });
literals!(
    /// `PullRequestAction`.
    Action {
        Merge = "merge",
        Ready = "ready",
        Draft = "draft",
        Close = "close",
        Reopen = "reopen",
        DeleteBranch = "delete-branch",
        UpdateBranch = "update-branch",
        EnableAutoMerge = "enable-auto-merge",
        DisableAutoMerge = "disable-auto-merge",
        Revert = "revert",
        ApproveWorkflows = "approve-workflows",
    }
);
literals!(CheckStatus {
    Pending = "pending",
    ActionRequired = "action-required",
    Success = "success",
    Failure = "failure",
    Skipped = "skipped",
    Neutral = "neutral",
    Cancelled = "cancelled",
});
literals!(CommentKind { IssueComment = "issue-comment", ReviewComment = "review-comment", Review = "review" });

/// Decodes an array element by element, dropping elements that do not decode.
pub fn lenient<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let values = Option::<Vec<Value>>::deserialize(deserializer)?.unwrap_or_default();
    Ok(values.into_iter().filter_map(|value| serde_json::from_value(value).ok()).collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    #[serde(default)]
    pub is_bot: Option<bool>,
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StackMembership {
    pub number: u64,
    pub position: u64,
    pub size: u64,
    pub base: String,
}

/// `PullRequestListEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListEntry {
    #[serde(default)]
    pub stack: Option<StackMembership>,
    pub provider: String,
    pub host: String,
    pub project_id: String,
    pub project_title: String,
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<Actor>,
    pub head_branch: String,
    pub base_branch: String,
    pub state: PrState,
    pub is_draft: bool,
    pub mergeability: Mergeability,
    pub additions: u64,
    pub deletions: u64,
    pub created_at: String,
    pub updated_at: String,
    pub viewer_review_requested: bool,
    #[serde(default, deserialize_with = "lenient")]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub review_decision: Option<ReviewDecision>,
    #[serde(default)]
    pub checks_state: Option<ChecksState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub host: String,
    pub kind: String,
    pub searches_on_host: bool,
    #[serde(default)]
    pub actions: Option<Vec<Action>>,
    pub project_count: u64,
    pub configured: bool,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectError {
    pub project_id: String,
    pub project_title: String,
    pub message: String,
}

/// `PullRequestListResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    #[serde(default)]
    pub viewers: HashMap<String, String>,
    #[serde(default, deserialize_with = "lenient")]
    pub providers: Vec<ProviderSummary>,
    #[serde(default, deserialize_with = "lenient")]
    pub entries: Vec<ListEntry>,
    #[serde(default, deserialize_with = "lenient")]
    pub errors: Vec<ProjectError>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub next_cursors: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStat {
    pub project_id: String,
    pub repository: String,
    pub number: u64,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ListStats {
    #[serde(default, deserialize_with = "lenient")]
    pub stats: Vec<DiffStat>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct FilterOptions {
    #[serde(default, deserialize_with = "lenient")]
    pub labels: Vec<Label>,
    #[serde(default, deserialize_with = "lenient")]
    pub authors: Vec<Actor>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub diff: bool,
    #[serde(default)]
    pub comment: bool,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub merge_methods: Vec<MergeMethod>,
    #[serde(default)]
    pub labels: Option<bool>,
    #[serde(default)]
    pub stacks: Option<bool>,
    #[serde(default)]
    pub stack_actions: Option<bool>,
}

/// Unreported host permissions are granted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerPermissions {
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub comment: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct MergeCapabilities {
    #[serde(default)]
    pub merge: bool,
    #[serde(default)]
    pub squash: bool,
    #[serde(default)]
    pub rebase: bool,
}

impl MergeCapabilities {
    pub fn allows(self, method: MergeMethod) -> bool {
        match method {
            MergeMethod::Merge => self.merge,
            MergeMethod::Squash => self.squash,
            MergeMethod::Rebase => self.rebase,
            MergeMethod::Unknown => false,
        }
    }
}

/// `PullRequestDetail`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    pub provider: String,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub viewer_permissions: ViewerPermissions,
    pub project_id: String,
    pub project_title: String,
    #[serde(default)]
    pub workspace_root: String,
    pub repository: String,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub url: String,
    pub author: Option<Actor>,
    pub state: PrState,
    pub is_draft: bool,
    pub mergeability: Mergeability,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub head_branch: String,
    pub base_branch: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub merged_at: Option<String>,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub reviewers: Vec<Actor>,
    #[serde(default, deserialize_with = "lenient")]
    pub labels: Vec<Label>,
    #[serde(default, deserialize_with = "lenient")]
    pub checks: Vec<Check>,
    #[serde(default)]
    pub merge_capabilities: MergeCapabilities,
    #[serde(default)]
    pub viewer: Option<String>,
    #[serde(default)]
    pub behind_by: Option<u64>,
    /// Absent means the host does not report auto-merge state, not that it is off.
    #[serde(default)]
    pub auto_merge_enabled: Option<bool>,
    #[serde(default)]
    pub workflow_approvals_required: Option<u64>,
}

impl Detail {
    /// `canPerformPullRequestAction`: the host offers it and the viewer may run it.
    pub fn can(&self, action: Action) -> bool {
        self.capabilities.actions.contains(&action) && self.viewer_permissions.actions.contains(&action)
    }

    /// `allowedPullRequestMergeMethods`.
    pub fn allowed_merge_methods(&self) -> Vec<MergeMethod> {
        self.capabilities.merge_methods.iter().copied().filter(|method| self.merge_capabilities.allows(*method)).collect()
    }
}

/// One layer of `PullRequestStack`, bottom first.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackLayer {
    pub number: u64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub is_draft: Option<bool>,
    #[serde(default)]
    pub head_sha: Option<String>,
    pub head_branch: String,
    pub state: PrState,
}

/// `pullRequests.stack`: null when the pull request is in no stack.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Stack {
    pub id: String,
    pub number: u64,
    pub url: String,
    pub base: String,
    pub layers: Vec<StackLayer>,
}

/// `pullRequests.checks`: null when the host has nothing to say.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Checks {
    pub state: PrState,
    #[serde(default, deserialize_with = "lenient")]
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub kind: CommentKind,
    pub author: Option<Actor>,
    #[serde(default)]
    pub body: String,
    pub created_at: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub review_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadComment {
    pub id: String,
    pub author: Option<Actor>,
    #[serde(default)]
    pub body: String,
    pub created_at: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub line: Option<u64>,
    #[serde(default)]
    pub is_resolved: bool,
    #[serde(default)]
    pub is_outdated: bool,
    #[serde(default, deserialize_with = "lenient")]
    pub comments: Vec<ThreadComment>,
    #[serde(default)]
    pub comment_count: Option<u64>,
    #[serde(default)]
    pub next_comments_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub oid: String,
    #[serde(default)]
    pub message_headline: String,
    pub committed_date: String,
    #[serde(default)]
    pub additions: Option<u64>,
    #[serde(default)]
    pub deletions: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub authors: Vec<Actor>,
}

/// `PullRequestActivity`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    #[serde(default, deserialize_with = "lenient")]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub comment_count: u64,
    #[serde(default)]
    pub comments_truncated: bool,
    #[serde(default, deserialize_with = "lenient")]
    pub review_threads: Vec<ReviewThread>,
    #[serde(default, deserialize_with = "lenient")]
    pub commits: Vec<Commit>,
}

/// `PullRequestDiffResult`, from `POST /api/pull-requests/diff`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffResult {
    #[serde(default)]
    pub patch: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

pub fn decode<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// `pullRequests.checks` answers null when the host reports nothing.
pub fn decode_checks(value: Value) -> Result<Option<Checks>, String> {
    if value.is_null() { Ok(None) } else { decode(value).map(Some) }
}
