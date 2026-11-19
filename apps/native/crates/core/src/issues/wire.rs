//! Issue tracking wire contracts and forward-compatible decoders, mirroring
//! `packages/contracts/src/issue.ts`, `packages/contracts/src/rpc.ts`, and
//! `packages/contracts/src/clientRpcPermissions.ts`.
//!
//! Unknown enum literals decode to `Unknown` (and `IssueTrackerKind` preserves any
//! unrecognized tracker slug), optional fields default cleanly, and array fields drop
//! malformed entries rather than failing an entire listing or detail read.

use std::collections::HashMap;
use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

pub use crate::pull_requests::wire::{
    Actor as IssueActor, Label as IssueLabel, PrState, Reaction as IssueReaction,
    ReactionContent as IssueReactionContent, lenient,
};

/// `WS_METHODS` issue-tracking RPC tags (`packages/contracts/src/rpc.ts`).
pub mod method {
    pub const LIST: &str = "issues.list";
    pub const DETAIL: &str = "issues.detail";
    pub const COMMENTS: &str = "issues.comments";
    pub const TEMPLATES: &str = "issues.templates";
    pub const CANDIDATES: &str = "issues.candidates";
    pub const CREATE: &str = "issues.create";
    pub const COMMENT: &str = "issues.comment";
    pub const UPDATE: &str = "issues.update";
    pub const SET_STATE: &str = "issues.setState";
    pub const REACT: &str = "issues.react";
    pub const LINKED_THREADS: &str = "issues.linkedThreads";
    pub const TRACKERS_LIST: &str = "issueTrackers.list";
    pub const TRACKERS_CONNECT: &str = "issueTrackers.connect";
    pub const TRACKERS_BIND_PROJECT: &str = "issueTrackers.bindProject";
    pub const SUBSCRIBE_LINKED_THREAD_REFRESHES: &str =
        "pullRequests.subscribeLinkedThreadRefreshes";
}

/// Environment permission scopes guarding issue and tracker mutations
/// (`packages/contracts/src/clientRpcPermissions.ts`).
pub const SOURCE_CONTROL_WRITE_SCOPE: &str = "source-control:write";
pub const SETTINGS_WRITE_SCOPE: &str = "settings:write";

/// Returns the environment permission scope required for an issue or issue-tracker RPC method,
/// or `None` for read methods (`CLIENT_GUARDED_RPC_SCOPES`).
pub fn required_scope(rpc_method: &str) -> Option<&'static str> {
    match rpc_method {
        method::CREATE
        | method::COMMENT
        | method::UPDATE
        | method::SET_STATE
        | method::REACT => Some(SOURCE_CONTROL_WRITE_SCOPE),
        method::TRACKERS_CONNECT | method::TRACKERS_BIND_PROJECT => Some(SETTINGS_WRITE_SCOPE),
        _ => None,
    }
}

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

/// `IssueTrackerKind` (`packages/contracts/src/issue.ts`): open string union covering git hosts
/// (`github`, `gitlab`, `azure-devops`, `forgejo`) and standalone trackers (`linear`, `jira`),
/// preserving any future tracker slug in `Unknown(String)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IssueTrackerKind {
    GitHub,
    GitLab,
    AzureDevOps,
    Forgejo,
    Linear,
    Jira,
    Unknown(String),
}

impl IssueTrackerKind {
    pub fn from_wire(raw: &str) -> Self {
        match raw {
            "github" => Self::GitHub,
            "gitlab" => Self::GitLab,
            "azure-devops" => Self::AzureDevOps,
            "forgejo" => Self::Forgejo,
            "linear" => Self::Linear,
            "jira" => Self::Jira,
            other => Self::Unknown(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
            Self::AzureDevOps => "azure-devops",
            Self::Forgejo => "forgejo",
            Self::Linear => "linear",
            Self::Jira => "jira",
            Self::Unknown(other) => other.as_str(),
        }
    }

    /// Human-facing provider name (`issueTrackerPresentation`).
    pub fn display_name(&self) -> &str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
            Self::AzureDevOps => "Azure DevOps",
            Self::Forgejo => "Forgejo",
            Self::Linear => "Linear",
            Self::Jira => "Jira",
            Self::Unknown(other) => other.as_str(),
        }
    }
}

impl fmt::Display for IssueTrackerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for IssueTrackerKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for IssueTrackerKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_wire(&raw))
    }
}

literals!(
    /// `IssueStateCategory`.
    IssueStateCategory {
        Open = "open",
        Closed = "closed",
    }
);

literals!(
    /// `IssueListState`: what a listing asks for.
    IssueListState {
        All = "all",
        Open = "open",
        Closed = "closed",
    }
);

#[allow(clippy::derivable_impls)]
impl Default for IssueListState {
    fn default() -> Self {
        Self::Open
    }
}

impl IssueListState {
    pub const ALL: [Self; 3] = [Self::Open, Self::Closed, Self::All];

    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Closed => "Closed",
            Self::All => "All",
            Self::Unknown => "Unknown",
        }
    }

    pub fn holds(self, category: IssueStateCategory) -> bool {
        match self {
            Self::All => true,
            Self::Open => category == IssueStateCategory::Open,
            Self::Closed => category == IssueStateCategory::Closed,
            Self::Unknown => true,
        }
    }
}

literals!(
    /// `IssueInvolvement`.
    IssueInvolvement {
        All = "all",
        Assigned = "assigned",
        Authored = "authored",
        Mentioned = "mentioned",
    }
);

#[allow(clippy::derivable_impls)]
impl Default for IssueInvolvement {
    fn default() -> Self {
        Self::All
    }
}

impl IssueInvolvement {
    pub const ALL: [Self; 4] = [Self::All, Self::Assigned, Self::Authored, Self::Mentioned];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "Everyone",
            Self::Assigned => "Assigned to me",
            Self::Authored => "Opened by me",
            Self::Mentioned => "Mentioning me",
            Self::Unknown => "Unknown",
        }
    }
}

literals!(
    /// `IssueListSort`: newest first by the named date.
    IssueListSort {
        Updated = "updated",
        Created = "created",
    }
);

#[allow(clippy::derivable_impls)]
impl Default for IssueListSort {
    fn default() -> Self {
        Self::Updated
    }
}

impl IssueListSort {
    pub const ALL: [Self; 2] = [Self::Updated, Self::Created];

    pub fn label(self) -> &'static str {
        match self {
            Self::Updated => "Recently updated",
            Self::Created => "Newest",
            Self::Unknown => "Recently updated",
        }
    }
}

literals!(
    /// `IssueEventKind`.
    IssueEventKind {
        Closed = "closed",
        Reopened = "reopened",
        Labeled = "labeled",
        Unlabeled = "unlabeled",
        Assigned = "assigned",
        Unassigned = "unassigned",
        Renamed = "renamed",
        Referenced = "referenced",
        StateChanged = "state-changed",
    }
);

literals!(
    /// `IssueUnavailableReason`.
    IssueUnavailableReason {
        ToolMissing = "tool-missing",
        Unauthenticated = "unauthenticated",
        TrackerUnsupported = "tracker-unsupported",
    }
);

literals!(
    /// `IssueOperationError.reason`.
    IssueOperationReason {
        NotFound = "not-found",
        ReadOnly = "read-only",
        RateLimited = "rate-limited",
    }
);

literals!(
    /// `ThreadIssueLinkSource`.
    ThreadIssueLinkSource {
        Manual = "manual",
        Agent = "agent",
    }
);

literals!(
    /// `IssueTrackerConnectionError.reason`.
    IssueTrackerConnectionErrorReason {
        UnknownTracker = "unknown-tracker",
        Rejected = "rejected",
        Failed = "failed",
    }
);

/// `IssueState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueState {
    pub category: IssueStateCategory,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
}

/// `IssueRef`: addresses one issue read through a project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRef {
    pub project_id: String,
    pub tracker: IssueTrackerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub scope: String,
    pub key: String,
}

impl IssueRef {
    pub fn of(entry: &IssueListEntry) -> Self {
        Self {
            project_id: entry.project_id.clone(),
            tracker: entry.tracker.clone(),
            host: Some(entry.host.clone()),
            scope: entry.scope.clone(),
            key: entry.key.clone(),
        }
    }

    pub fn payload(&self) -> Value {
        let mut map = Map::new();
        map.insert("projectId".into(), json!(self.project_id));
        map.insert("tracker".into(), json!(self.tracker));
        if let Some(host) = &self.host {
            map.insert("host".into(), json!(host));
        }
        map.insert("scope".into(), json!(self.scope));
        map.insert("key".into(), json!(self.key));
        Value::Object(map)
    }

    pub fn entry_key(&self) -> String {
        format!(
            "{}\0{}\0{}\0{}",
            self.project_id,
            self.host.as_deref().unwrap_or("").to_lowercase(),
            self.scope.to_lowercase(),
            self.key
        )
    }
}

/// `IssueScopeRef`: addresses a tracker container (repository, Linear team, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueScopeRef {
    pub project_id: String,
    pub tracker: IssueTrackerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub scope: String,
}

impl IssueScopeRef {
    pub fn payload(&self) -> Value {
        let mut map = Map::new();
        map.insert("projectId".into(), json!(self.project_id));
        map.insert("tracker".into(), json!(self.tracker));
        if let Some(host) = &self.host {
            map.insert("host".into(), json!(host));
        }
        map.insert("scope".into(), json!(self.scope));
        Value::Object(map)
    }
}

/// `IssueSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSummary {
    pub tracker: IssueTrackerKind,
    pub host: String,
    pub scope: String,
    pub key: String,
    pub display_key: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub author: Option<IssueActor>,
    pub state: IssueState,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub assignees: Vec<IssueActor>,
    #[serde(default, deserialize_with = "lenient")]
    pub labels: Vec<IssueLabel>,
    #[serde(default)]
    pub comment_count: u64,
}

/// `IssueListEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListEntry {
    pub tracker: IssueTrackerKind,
    pub host: String,
    pub scope: String,
    pub key: String,
    pub display_key: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub author: Option<IssueActor>,
    pub state: IssueState,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub assignees: Vec<IssueActor>,
    #[serde(default, deserialize_with = "lenient")]
    pub labels: Vec<IssueLabel>,
    #[serde(default)]
    pub comment_count: u64,
    pub project_id: String,
    pub project_title: String,
}

impl IssueListEntry {
    pub fn summary(&self) -> IssueSummary {
        IssueSummary {
            tracker: self.tracker.clone(),
            host: self.host.clone(),
            scope: self.scope.clone(),
            key: self.key.clone(),
            display_key: self.display_key.clone(),
            title: self.title.clone(),
            url: self.url.clone(),
            author: self.author.clone(),
            state: self.state.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            closed_at: self.closed_at.clone(),
            assignees: self.assignees.clone(),
            labels: self.labels.clone(),
            comment_count: self.comment_count,
        }
    }
}

/// `IssueComment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueComment {
    pub id: String,
    #[serde(default)]
    pub author: Option<IssueActor>,
    #[serde(default)]
    pub body: String,
    pub created_at: String,
    #[serde(default)]
    pub url: Option<String>,
    /// Absent from a tracker with no reactions, which differs from `Some(vec![])`.
    #[serde(default, deserialize_with = "lenient_option")]
    pub reactions: Option<Vec<IssueReaction>>,
}

/// `IssueEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueEvent {
    pub id: String,
    pub kind: IssueEventKind,
    #[serde(default)]
    pub actor: Option<IssueActor>,
    pub created_at: String,
    #[serde(default)]
    pub detail: Option<String>,
}

/// `IssueLinkedPullRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLinkedPullRequest {
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PrState,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub closes_issue: bool,
}

/// `IssueCapabilities`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCapabilities {
    #[serde(default)]
    pub create: bool,
    #[serde(default)]
    pub edit: bool,
    #[serde(default)]
    pub comment: bool,
    #[serde(default)]
    pub labels: bool,
    #[serde(default)]
    pub assignees: bool,
    #[serde(default)]
    pub reactions: bool,
    #[serde(default)]
    pub templates: bool,
    #[serde(default)]
    pub linked_pull_requests: bool,
    #[serde(default)]
    pub events: bool,
    #[serde(default)]
    pub search: bool,
}

fn default_true() -> bool {
    true
}

/// `IssueViewerPermissions`. A permission the tracker reports nothing about is granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueViewerPermissions {
    #[serde(default = "default_true")]
    pub set_state: bool,
    #[serde(default = "default_true")]
    pub comment: bool,
    #[serde(default = "default_true")]
    pub edit: bool,
    #[serde(default = "default_true")]
    pub labels: bool,
    #[serde(default = "default_true")]
    pub assignees: bool,
}

impl Default for IssueViewerPermissions {
    fn default() -> Self {
        Self {
            set_state: true,
            comment: true,
            edit: true,
            labels: true,
            assignees: true,
        }
    }
}

/// `IssueDetail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueDetail {
    pub tracker: IssueTrackerKind,
    pub host: String,
    pub scope: String,
    pub key: String,
    pub display_key: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub author: Option<IssueActor>,
    pub state: IssueState,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub assignees: Vec<IssueActor>,
    #[serde(default, deserialize_with = "lenient")]
    pub labels: Vec<IssueLabel>,
    #[serde(default)]
    pub comment_count: u64,
    pub project_id: String,
    pub project_title: String,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub capabilities: IssueCapabilities,
    #[serde(default)]
    pub viewer_permissions: IssueViewerPermissions,
    #[serde(default, deserialize_with = "lenient")]
    pub states: Vec<IssueState>,
    #[serde(default)]
    pub viewer: Option<String>,
    #[serde(default)]
    pub body: String,
    #[serde(default, deserialize_with = "lenient_option")]
    pub reactions: Option<Vec<IssueReaction>>,
    #[serde(default, deserialize_with = "lenient")]
    pub comments: Vec<IssueComment>,
    #[serde(default)]
    pub comments_cursor: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub events: Vec<IssueEvent>,
    #[serde(default, deserialize_with = "lenient")]
    pub linked_pull_requests: Vec<IssueLinkedPullRequest>,
}

/// `IssueCommentsPageInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCommentsPageInput {
    #[serde(flatten)]
    pub reference: IssueRef,
    pub cursor: String,
}

/// `IssueCommentsPage`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCommentsPage {
    #[serde(default, deserialize_with = "lenient")]
    pub comments: Vec<IssueComment>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// `IssueListCursors`: keyed `"<host> <scope>"`.
pub type IssueListCursors = HashMap<String, String>;

/// `IssueListInput`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListInput {
    pub state: IssueListState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub involvement: Option<IssueInvolvement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursors: Option<IssueListCursors>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<IssueListSort>,
}

/// `IssueTrackerSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerSummary {
    pub tracker: IssueTrackerKind,
    pub host: String,
    #[serde(default)]
    pub project_count: u64,
    #[serde(default)]
    pub capabilities: Option<IssueCapabilities>,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub detail: Option<String>,
}

/// `IssueListProjectError`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListProjectError {
    pub project_id: String,
    pub project_title: String,
    pub message: String,
}

/// `IssueListResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListResult {
    #[serde(default)]
    pub viewers: HashMap<String, String>,
    #[serde(default, deserialize_with = "lenient")]
    pub trackers: Vec<IssueTrackerSummary>,
    #[serde(default, deserialize_with = "lenient")]
    pub entries: Vec<IssueListEntry>,
    #[serde(default, deserialize_with = "lenient")]
    pub errors: Vec<IssueListProjectError>,
    #[serde(default)]
    pub next_cursors: IssueListCursors,
    #[serde(default, deserialize_with = "lenient_option")]
    pub scopes: Option<Vec<IssueScopeRef>>,
}

/// `IssueCreateInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCreateInput {
    #[serde(flatten)]
    pub scope: IssueScopeRef,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
}

/// `IssueCreateResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCreateResult {
    pub key: String,
    pub display_key: String,
    pub url: String,
}

/// `IssueCommentInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCommentInput {
    #[serde(flatten)]
    pub reference: IssueRef,
    pub body: String,
}

/// `IssueUpdateInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueUpdateInput {
    #[serde(flatten)]
    pub reference: IssueRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignees: Option<Vec<String>>,
}

/// `IssueSetStateInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSetStateInput {
    #[serde(flatten)]
    pub reference: IssueRef,
    pub state: String,
}

/// `IssueReactionInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueReactionInput {
    #[serde(flatten)]
    pub reference: IssueRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    pub content: IssueReactionContent,
    pub reacted: bool,
}

/// `IssueTemplate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTemplate {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub about: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
}

/// `IssueExternalTemplate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueExternalTemplate {
    pub name: String,
    #[serde(default)]
    pub about: String,
    pub url: String,
}

/// `IssueTemplateList`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTemplateList {
    #[serde(default, deserialize_with = "lenient")]
    pub templates: Vec<IssueTemplate>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub external_templates: Option<Vec<IssueExternalTemplate>>,
    #[serde(default = "default_true")]
    pub blank_issues_enabled: bool,
}

/// `IssueLabelCandidate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLabelCandidate {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// `IssueAssigneeCandidate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueAssigneeCandidate {
    #[serde(default)]
    pub is_bot: Option<bool>,
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    pub id: String,
}

/// `IssueCandidates`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCandidates {
    #[serde(default, deserialize_with = "lenient")]
    pub labels: Vec<IssueLabelCandidate>,
    #[serde(default, deserialize_with = "lenient")]
    pub assignees: Vec<IssueAssigneeCandidate>,
    #[serde(default)]
    pub truncated: bool,
}

/// `ThreadIssueKey`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIssueKey {
    pub tracker: IssueTrackerKind,
    pub host: String,
    pub scope: String,
    pub key: String,
}

/// `threadIssueKeyOf`.
pub fn thread_issue_key_of(key: &ThreadIssueKey) -> String {
    format!(
        "{}\0{}\0{}\0{}",
        key.tracker.as_str(),
        key.host.trim().to_lowercase(),
        key.scope.trim().to_lowercase(),
        key.key.trim()
    )
}

/// `threadIssueKeysEqual`.
pub fn thread_issue_keys_equal(left: &ThreadIssueKey, right: &ThreadIssueKey) -> bool {
    thread_issue_key_of(left) == thread_issue_key_of(right)
}

/// `ThreadIssueLink`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIssueLink {
    pub tracker: IssueTrackerKind,
    pub host: String,
    pub scope: String,
    pub key: String,
    pub project_id: String,
    pub url: String,
    pub source: ThreadIssueLinkSource,
    pub linked_at: String,
    pub display_key: String,
    pub title: String,
}

impl ThreadIssueLink {
    pub fn issue_key(&self) -> ThreadIssueKey {
        ThreadIssueKey {
            tracker: self.tracker.clone(),
            host: self.host.clone(),
            scope: self.scope.clone(),
            key: self.key.clone(),
        }
    }
}

/// One row of `IssueLinkedThreadsResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLinkedThread {
    pub id: String,
    pub project_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub settled: bool,
}

/// `IssueLinkedThreadsResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLinkedThreadsResult {
    #[serde(default, deserialize_with = "lenient")]
    pub threads: Vec<IssueLinkedThread>,
}

/// `StandaloneIssueTrackerDescriptor.credential`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandaloneIssueTrackerCredential {
    pub label: String,
    #[serde(default)]
    pub help_url: Option<String>,
}

/// `IssueTrackerProjectBinding`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerProjectBinding {
    pub tracker: IssueTrackerKind,
    pub scope: String,
}

/// `IssueTrackerScope`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerScope {
    pub key: String,
    pub name: String,
}

/// `IssueTrackerConnection`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerConnection {
    pub tracker: IssueTrackerKind,
    pub display_name: String,
    pub host: String,
    pub scope_noun: String,
    pub credential: StandaloneIssueTrackerCredential,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub scopes: Vec<IssueTrackerScope>,
}

/// `IssueTrackerConnectionList`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerConnectionList {
    #[serde(default, deserialize_with = "lenient")]
    pub trackers: Vec<IssueTrackerConnection>,
    #[serde(default)]
    pub bindings: HashMap<String, IssueTrackerProjectBinding>,
}

/// `IssueTrackerConnectInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerConnectInput {
    pub tracker: IssueTrackerKind,
    pub token: Option<String>,
}

/// `IssueTrackerBindProjectInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerBindProjectInput {
    pub project_id: String,
    pub binding: Option<IssueTrackerProjectBinding>,
}

/// Decodes an optional array element-by-element, distinguishing absent/null (`None`) from present.
fn lenient_option<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let raw = Option::<Vec<Value>>::deserialize(deserializer)?;
    Ok(raw.map(|values| {
        values
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect()
    }))
}

pub fn decode<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// Formats tagged issue RPC failures (`IssueUnavailableError`, `IssueOperationError`,
/// `IssueTrackerConnectionError`, `EnvironmentAuthorizationError`) into user-facing text.
pub fn failure_message(tag: Option<&str>, error: &Value, fallback: &str) -> String {
    match tag {
        Some("IssueUnavailableError") => {
            match error.get("reason").and_then(Value::as_str) {
                Some("tool-missing") => {
                    "The tool this tracker is read through is not installed or set up.".to_owned()
                }
                Some("unauthenticated") => {
                    "This tracker has no working credentials.".to_owned()
                }
                Some("tracker-unsupported") => {
                    "Issues cannot be browsed for this project yet.".to_owned()
                }
                _ => fallback.to_owned(),
            }
        }
        Some("IssueOperationError") => error
            .get("detail")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|detail| !detail.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| fallback.to_owned()),
        Some("IssueTrackerConnectionError") => error
            .get("detail")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|detail| !detail.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| fallback.to_owned()),
        Some("EnvironmentAuthorizationError") => error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| "Permission required for this environment.".to_owned()),
        _ => fallback.to_owned(),
    }
}

/// Returns `true` when an RPC error indicates the connected server does not implement `issues.*`.
pub fn is_issues_unsupported_error(tag: Option<&str>, message: &str) -> bool {
    let lower = message.to_lowercase();
    matches!(tag, Some("RpcNotFound" | "MethodNotFound"))
        || (lower.contains("issues.list") && (lower.contains("not found") || lower.contains("unknown")))
        || lower.contains("method not found")
}
