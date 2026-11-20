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
literals!(MergeMethod { Merge = "merge", Squash = "squash", Rebase = "rebase", FastForward = "fast-forward" });
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
literals!(ReactionContent {
    ThumbsUp = "thumbs-up",
    ThumbsDown = "thumbs-down",
    Laugh = "laugh",
    Hooray = "hooray",
    Confused = "confused",
    Heart = "heart",
    Rocket = "rocket",
    Eyes = "eyes",
});
literals!(ReviewVerdict { Comment = "comment", Approve = "approve", RequestChanges = "request-changes" });
literals!(ReviewerKind { User = "user", Team = "team" });
literals!(ViewedFilesStore { Host = "host", Environment = "environment" });
literals!(FileViewedState { Unviewed = "unviewed", Viewed = "viewed", Dismissed = "dismissed" });

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reaction {
    pub content: ReactionContent,
    pub count: u64,
    #[serde(default)]
    pub actors: Vec<String>,
    #[serde(default)]
    pub viewer_has_reacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewerCandidate {
    pub id: String,
    pub kind: ReviewerKind,
    #[serde(default)]
    pub is_requested: bool,
    #[serde(default)]
    pub is_bot: Option<bool>,
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerCandidates {
    #[serde(default, deserialize_with = "lenient")]
    pub candidates: Vec<ReviewerCandidate>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub project_id: String,
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub author: Option<Actor>,
    pub state: PrState,
    #[serde(default)]
    pub is_draft: bool,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub observed_at: Option<serde_json::Number>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileViewed {
    pub path: String,
    pub state: FileViewedState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesViewedResult {
    #[serde(default, deserialize_with = "lenient")]
    pub files: Vec<FileViewed>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCommentsPage {
    #[serde(default, deserialize_with = "lenient")]
    pub comments: Vec<ThreadComment>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub hidden_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGuideAttention {
    pub kind: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGuideExcerpt {
    pub path: String,
    pub side: String,
    pub start: u32,
    pub end: u32,
    #[serde(default)]
    pub caption: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGuideNote {
    pub path: String,
    pub line: u32,
    pub title: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGuideFile {
    pub path: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGuideStop {
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub attention: Option<ReviewGuideAttention>,
    #[serde(default, deserialize_with = "lenient")]
    pub excerpts: Vec<ReviewGuideExcerpt>,
    #[serde(default, deserialize_with = "lenient")]
    pub files: Vec<ReviewGuideFile>,
    #[serde(default, deserialize_with = "lenient")]
    pub notes: Vec<ReviewGuideNote>,
    #[serde(default)]
    pub questions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGuide {
    pub head_sha: String,
    pub generated_at: String,
    #[serde(default, deserialize_with = "lenient")]
    pub stops: Vec<ReviewGuideStop>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum ReviewGuideRun {
    #[serde(rename_all = "camelCase")]
    Running { head_sha: String, started_at: String },
    #[serde(rename_all = "camelCase")]
    Failed { head_sha: String, failed_at: String, detail: String },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGuideResult {
    #[serde(default)]
    pub guide: Option<ReviewGuide>,
    #[serde(default)]
    pub run: Option<ReviewGuideRun>,
    #[serde(default)]
    pub current_head_sha: Option<String>,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackMembership {
    pub number: u64,
    pub position: u64,
    pub size: u64,
    pub base: String,
}

/// `PullRequestListEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub observed_at: Option<serde_json::Number>,
    pub viewer_review_requested: bool,
    #[serde(default, deserialize_with = "lenient")]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub review_decision: Option<ReviewDecision>,
    #[serde(default)]
    pub checks_state: Option<ChecksState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub host: String,
    pub kind: String,
    pub searches_on_host: bool,
    #[serde(default)]
    pub actions: Option<Vec<Action>>,
    pub project_count: u64,
    #[serde(default)]
    pub project_ids: Option<Vec<String>>,
    pub configured: bool,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectError {
    pub project_id: String,
    pub project_title: String,
    pub message: String,
}

/// `PullRequestListResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStat {
    pub project_id: String,
    pub repository: String,
    pub number: u64,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

/// `PullRequestEditCapabilities`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditCapabilities {
    #[serde(default)]
    pub change_request: bool,
    #[serde(default)]
    pub comment: bool,
    #[serde(default)]
    pub maintainer_edits: Option<bool>,
}

/// `PullRequestReviewCapabilities`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCapabilities {
    #[serde(default)]
    pub inline_comment: bool,
    #[serde(default)]
    pub range_comment: Option<bool>,
    #[serde(default)]
    pub reply: bool,
    #[serde(default)]
    pub resolve: bool,
    #[serde(default, deserialize_with = "lenient")]
    pub verdicts: Vec<ReviewVerdict>,
}

/// `PullRequestReviewerCapabilities`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewerCapabilities {
    #[serde(default)]
    pub request: bool,
    #[serde(default)]
    pub list_candidates: bool,
    #[serde(default)]
    pub search_candidates: Option<bool>,
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
    pub reactions: Option<bool>,
    #[serde(default)]
    pub viewed_files: Option<ViewedFilesStore>,
    #[serde(default)]
    pub labels: Option<bool>,
    #[serde(default)]
    pub stacks: Option<bool>,
    #[serde(default)]
    pub stack_actions: Option<bool>,
    #[serde(default)]
    pub edit: Option<EditCapabilities>,
    #[serde(default)]
    pub review: ReviewCapabilities,
    #[serde(default)]
    pub reviewers: ReviewerCapabilities,
    #[serde(default)]
    pub mentions: Option<bool>,
    #[serde(default)]
    pub reference_sigils: Option<Vec<String>>,
    #[serde(default)]
    pub checks_always_report: Option<bool>,
    /// The diff can read one file the paged diff withheld as too large (`pingdotgg/t3code#37007`).
    #[serde(default)]
    pub withheld_file_diff: Option<bool>,
}

/// Unreported host permissions are granted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerPermissions {
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub comment: bool,
    #[serde(default)]
    pub resolve: bool,
    #[serde(default, deserialize_with = "lenient")]
    pub verdicts: Vec<ReviewVerdict>,
    #[serde(default)]
    pub request_reviewers: bool,
    #[serde(default)]
    pub labels: Option<bool>,
    #[serde(default)]
    pub edit_change_request: Option<bool>,
    #[serde(default)]
    pub maintainer_edits: Option<bool>,
}

/// `PullRequestMergeQueueRemoval`: the host's merge queue letting go of a pull request without
/// merging it. Decoded so the detail keeps it; the web client does not show it either.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeQueueRemoval {
    pub id: String,
    pub removed_at: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `PullRequestLabelCandidate`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelCandidate {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_applied: bool,
}

/// `PullRequestLabelCandidateList`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct LabelCandidates {
    #[serde(default, deserialize_with = "lenient")]
    pub candidates: Vec<LabelCandidate>,
    #[serde(default)]
    pub truncated: bool,
}

/// A field that is either absent (`None`), null (`Some(None)`) or set, as `pickDefined` tells
/// `undefined` from `null`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// `PullRequestSummary`: what `pullRequests.runAction` replies with when asked
/// (`replyWithSummary`). Absent fields were not in the host's reply.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub state: PrState,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub author: Option<Option<Actor>>,
    #[serde(default)]
    pub is_draft: Option<bool>,
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default)]
    pub base_branch: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub closed_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub merged_at: Option<Option<String>>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub observed_at: Option<f64>,
    #[serde(default)]
    pub additions: Option<u64>,
    #[serde(default)]
    pub deletions: Option<u64>,
    #[serde(default)]
    pub changed_files: Option<u64>,
    #[serde(default)]
    pub mergeability: Option<Mergeability>,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub auto_merge_enabled: Option<bool>,
    #[serde(default)]
    pub auto_merge_method: Option<MergeMethod>,
}

/// `runAction`'s reply: a summary when the host's reply carried one, else nothing.
pub fn decode_summary(value: Value) -> Result<Option<Summary>, String> {
    if value.is_null() { Ok(None) } else { Ok(serde_json::from_value(value).ok()) }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct MergeCapabilities {
    #[serde(default)]
    pub merge: bool,
    #[serde(default)]
    pub squash: bool,
    #[serde(default)]
    pub rebase: bool,
    #[serde(default, rename = "fast-forward")]
    pub fast_forward: bool,
}

impl MergeCapabilities {
    pub fn allows(self, method: MergeMethod) -> bool {
        match method {
            MergeMethod::Merge => self.merge,
            MergeMethod::Squash => self.squash,
            MergeMethod::Rebase => self.rebase,
            MergeMethod::FastForward => self.fast_forward,
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
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub auto_merge_method: Option<MergeMethod>,
    /// When the server read this from the host, in epoch milliseconds; kept as the wire number
    /// so equal readings compare equal. Absent from servers that predate it.
    #[serde(default)]
    pub observed_at: Option<serde_json::Number>,
    #[serde(default)]
    pub merge_queue_removal: Option<MergeQueueRemoval>,
    #[serde(default)]
    pub maintainer_can_modify: Option<bool>,
}

impl Detail {
    pub fn observed_at_ms(&self) -> Option<f64> {
        self.observed_at.as_ref().and_then(serde_json::Number::as_f64)
    }
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
    pub edited_at: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub review_state: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub can_edit: Option<bool>,
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
    pub edited_at: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// The review that posted the comment, so a link to a review with no text finds its comments.
    #[serde(default)]
    pub review_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub can_edit: Option<bool>,
}

/// `PullRequestReviewThread`, or an overview thread (`PullRequestOverviewThread`) on the change
/// request as a whole, which has no path or line.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThread {
    pub id: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub line: Option<u64>,
    #[serde(default)]
    pub side: Option<String>,
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
    /// Can only narrow repository-wide resolve permission. Absent when unreported.
    #[serde(default)]
    pub can_resolve: Option<bool>,
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
    #[serde(default)]
    pub review_threads_truncated: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    pub review_threads: Vec<ReviewThread>,
    /// Apart from `review_threads` on the wire so clients that require a path still decode.
    #[serde(default, deserialize_with = "lenient")]
    pub overview_threads: Vec<ReviewThread>,
    #[serde(default, deserialize_with = "lenient")]
    pub commits: Vec<Commit>,
    #[serde(default, deserialize_with = "lenient")]
    pub reactions: Vec<Reaction>,
}

/// `PullRequestOmittedFileStat`: line counts for a file whose diff the server withheld as too large.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmittedFileStat {
    pub path: String,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
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
    #[serde(default, deserialize_with = "lenient")]
    pub omitted_file_stats: Vec<OmittedFileStat>,
    /// On the first page: files this diff withholds can be read one at a time with `path`
    /// (`pingdotgg/t3code#37007`).
    #[serde(default)]
    pub withheld_files_readable: Option<bool>,
}

pub fn decode<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// `pullRequests.checks` answers null when the host reports nothing.
pub fn decode_checks(value: Value) -> Result<Option<Checks>, String> {
    if value.is_null() { Ok(None) } else { decode(value).map(Some) }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn fast_forward_merge_method_and_capability_decode() {
        let method: MergeMethod = serde_json::from_value(json!("fast-forward")).unwrap();
        assert_eq!(method, MergeMethod::FastForward);
        assert_eq!(method.as_str(), "fast-forward");

        let capabilities: MergeCapabilities =
            serde_json::from_value(json!({ "merge": false, "squash": false, "rebase": false, "fast-forward": true }))
                .unwrap();
        assert!(capabilities.allows(MergeMethod::FastForward));
        assert!(!capabilities.allows(MergeMethod::Merge));
    }
}
