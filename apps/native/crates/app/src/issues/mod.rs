//! Issues page and issue detail panel (`routes/_chat.issues.tsx`, `IssuesPage.tsx`, `IssueRow.tsx`,
//! `IssueListFilterPickers.tsx`, `SavedViewsMenu.tsx`, `IssueDetailPanel.tsx`, `IssueDetailGhost.tsx`,
//! `IssueCommentComposer.tsx`, `IssueThreadLinks.tsx`, `ThreadLinkPicker.tsx`, `IssueLinkPreview.tsx`).
//! Domain logic lives in `t3_core::issues`; reads and mutations run on the network runtime.

mod detail;
mod ghost;
mod page;
mod preview;
mod row;
mod thread_links;
mod toolbar;

use std::collections::{HashMap, HashSet, VecDeque};

use gpui::{App, Context, Entity, Global, Hsla, actions};
use serde_json::{Value, json};
use t3_client::environments::DestinationError;
use t3_client::session::RpcError;
use t3_core::auth::{ORCHESTRATION_OPERATE_SCOPE, ORCHESTRATION_READ_SCOPE};
use t3_core::commands::method::DISPATCH_COMMAND;
use t3_core::issues::links::IssueLinkProject;
use t3_core::issues::presentation::{IssueStateTone, issue_state_tone};
use t3_core::issues::wire::{
    IssueDetail, IssueListEntry, IssueRef, IssueState, IssueTrackerConnectionList,
    IssueTrackerKind, SOURCE_CONTROL_WRITE_SCOPE, decode, failure_message,
    is_issues_unsupported_error, method,
};
use t3_core::sidebar::grouping::EnvironmentOwner;
use tokio::task::JoinHandle;

pub use detail::IssueDetailPanel;
pub use page::IssuesPage;

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

actions!(issues, [OpenIssues, CloseIssues, CopyIssueKey]);

/// Maximum number of recently viewed issues held in memory for immediate repaint.
const HELD_ISSUES_LIMIT: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldIssueKey {
    pub environment: String,
    pub project_id: String,
    pub tracker: String,
    pub host: String,
    pub scope: String,
    pub key: String,
}

impl HeldIssueKey {
    pub(crate) fn of(
        state: &AppState,
        environment_id: Option<&str>,
        reference: &IssueRef,
    ) -> Option<Self> {
        let environment = environment_id
            .or_else(|| state.environment())
            .or(state.environments.active.as_deref())?
            .to_owned();
        Some(Self {
            environment,
            project_id: reference.project_id.clone(),
            tracker: reference.tracker.as_str().to_owned(),
            host: reference
                .host
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase(),
            scope: reference.scope.trim().to_ascii_lowercase(),
            key: reference.key.trim().to_owned(),
        })
    }
}

#[derive(Default)]
struct HeldIssues {
    details: VecDeque<(HeldIssueKey, IssueDetail)>,
    seeds: VecDeque<(HeldIssueKey, IssueListEntry)>,
}

impl Global for HeldIssues {}

pub(crate) fn held_issue_detail(key: &HeldIssueKey, cx: &App) -> Option<IssueDetail> {
    cx.try_global::<HeldIssues>()
        .and_then(|held| held.details.iter().find(|(k, _)| k == key))
        .map(|(_, detail)| detail.clone())
}

pub(crate) fn hold_issue_detail(key: HeldIssueKey, detail: &IssueDetail, cx: &mut App) {
    let held = cx.default_global::<HeldIssues>();
    if let Some(pos) = held.details.iter().position(|(k, _)| *k == key) {
        held.details.remove(pos);
    }
    held.details.push_back((key, detail.clone()));
    while held.details.len() > HELD_ISSUES_LIMIT {
        held.details.pop_front();
    }
}

pub(crate) fn held_issue_seed(key: &HeldIssueKey, cx: &App) -> Option<IssueListEntry> {
    cx.try_global::<HeldIssues>()
        .and_then(|held| held.seeds.iter().find(|(k, _)| k == key))
        .map(|(_, seed)| seed.clone())
}

pub(crate) fn hold_issue_seed(key: HeldIssueKey, entry: &IssueListEntry, cx: &mut App) {
    let held = cx.default_global::<HeldIssues>();
    if let Some(pos) = held.seeds.iter().position(|(k, _)| *k == key) {
        held.seeds.remove(pos);
    }
    held.seeds.push_back((key, entry.clone()));
    while held.seeds.len() > HELD_ISSUES_LIMIT {
        held.seeds.pop_front();
    }
}

/// Environments that rejected `issues.*` with `MethodNotFound` / `RpcNotFound`.
#[derive(Default)]
struct UnsupportedEnvironments(HashSet<String>);

impl Global for UnsupportedEnvironments {}

pub(crate) fn mark_issues_unsupported(env_id: &str, cx: &mut App) {
    cx.default_global::<UnsupportedEnvironments>()
        .0
        .insert(env_id.to_owned());
}

pub(crate) fn issues_supported_for(state: &AppState, env_id: Option<&str>, cx: &App) -> bool {
    let resolved = env_id
        .or_else(|| state.environment())
        .or(state.environments.active.as_deref());
    if let Some(id) = resolved
        && cx
            .try_global::<UnsupportedEnvironments>()
            .is_some_and(|set| set.0.contains(id))
    {
        return false;
    }
    state.server_config.config.is_some()
        || environment_owner_for(state, env_id).is_some()
}

/// Cached `issueTrackers.list` per environment for resolving standalone tracker links (Linear).
#[derive(Default)]
struct IssueTrackerCache {
    by_env: HashMap<String, IssueTrackerConnectionList>,
}

impl Global for IssueTrackerCache {}

pub(crate) fn cached_issue_trackers(
    env_id: &str,
    cx: &App,
) -> Option<IssueTrackerConnectionList> {
    cx.try_global::<IssueTrackerCache>()
        .and_then(|cache| cache.by_env.get(env_id).cloned())
}

pub(crate) fn cache_issue_trackers(
    env_id: String,
    list: IssueTrackerConnectionList,
    cx: &mut App,
) {
    cx.default_global::<IssueTrackerCache>()
        .by_env
        .insert(env_id, list);
}

/// Fetches `issueTrackers.list` for `env_id` (used on demand for Linear links).
pub(crate) fn read_issue_trackers(
    state: &AppState,
    env_id: Option<&str>,
) -> Read<IssueTrackerConnectionList> {
    let owner = environment_owner_for(state, env_id);
    read_for_owner(
        state,
        owner.as_ref(),
        method::TRACKERS_LIST,
        json!({}),
        decode::<IssueTrackerConnectionList>,
    )
}

impl AppState {
    /// Opens or leaves the issues page, which replaces the thread area while open and is
    /// mutually exclusive with the pull requests page.
    pub fn set_issues_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let mut changed = false;
        if open && self.pull_requests_open {
            self.pull_requests_open = false;
            changed = true;
        }
        if self.issues_open != open {
            self.issues_open = open;
            changed = true;
        }
        if changed {
            cx.emit(AppEvent::Selection);
            self.land_home(cx);
        }
    }
}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    let open = state.clone();
    cx.on_action(move |_: &OpenIssues, cx| {
        open.update(cx, |state, cx| state.set_issues_open(true, cx))
    });
    cx.on_action(move |_: &CloseIssues, cx| {
        state.update(cx, |state, cx| state.set_issues_open(false, cx))
    });
}

#[derive(Debug, Clone)]
pub(crate) struct ReadFailure {
    pub message: String,
    pub unsupported: bool,
}

pub(crate) type Read<T> = Option<JoinHandle<Result<T, ReadFailure>>>;

pub(crate) const NOT_CONNECTED: &str = "Not connected to the server.";

fn map_rpc_result<T>(
    result: Result<Value, RpcError>,
    decode: fn(Value) -> Result<T, String>,
) -> Result<T, ReadFailure> {
    match result {
        Ok(value) => decode(value).map_err(|message| ReadFailure {
            message,
            unsupported: false,
        }),
        Err(RpcError::Failure(failure)) => {
            let unsupported =
                is_issues_unsupported_error(failure.tag.as_deref(), &failure.message);
            let message =
                failure_message(failure.tag.as_deref(), &failure.error, &failure.message);
            Err(ReadFailure { message, unsupported })
        }
        Err(RpcError::Closed) => Err(ReadFailure {
            message: NOT_CONNECTED.to_owned(),
            unsupported: false,
        }),
    }
}

fn destination_error_message(error: DestinationError) -> String {
    match error {
        DestinationError::Denied => "Permission required for this environment.".to_owned(),
        DestinationError::Disconnected | DestinationError::Unsynchronized => {
            NOT_CONNECTED.to_owned()
        }
        DestinationError::Missing | DestinationError::Stale => {
            "This environment is no longer available.".to_owned()
        }
    }
}

/// Resolves an optional environment id (saved or canonical) to a fleet `EnvironmentOwner`.
pub(crate) fn environment_owner_for(
    state: &AppState,
    env_id: Option<&str>,
) -> Option<EnvironmentOwner> {
    let target = env_id
        .or(state.environments.active.as_deref())
        .or_else(|| state.environment())?;
    state
        .fleet
        .environments
        .iter()
        .find(|env| {
            env.summary.owner.saved_id == target
                || env.summary.owner.canonical_id.as_deref() == Some(target)
                || env
                    .config
                    .config
                    .as_ref()
                    .is_some_and(|c| c.environment.environment_id == target)
        })
        .map(|env| env.summary.owner.clone())
}

/// Builds the project descriptors for `resolve_issue_link_target` in `env_id` (or the active
/// environment when `None`).
pub(crate) fn issue_link_projects_for(
    state: &AppState,
    env_id: Option<&str>,
) -> Vec<IssueLinkProject> {
    let target = env_id
        .or(state.environments.active.as_deref())
        .or_else(|| state.environment());
    if let Some(target_id) = target
        && let Some(env) = state.fleet.environments.iter().find(|env| {
            env.summary.owner.saved_id == target_id
                || env.summary.owner.canonical_id.as_deref() == Some(target_id)
                || env
                    .config
                    .config
                    .as_ref()
                    .is_some_and(|c| c.environment.environment_id == target_id)
        })
    {
        return env
            .summary
            .projects
            .iter()
            .filter(|scoped| scoped.project.archived_at.is_none())
            .map(|scoped| IssueLinkProject {
                id: scoped.project.id.clone(),
                environment_id: env.summary.owner.saved_id.clone(),
                repository_identity: scoped.project.repository_identity.clone(),
            })
            .collect();
    }
    let resolved_env = target.unwrap_or("local").to_owned();
    let Some(snapshot) = &state.shell.snapshot else {
        return Vec::new();
    };
    snapshot
        .projects
        .iter()
        .filter(|project| project.archived_at.is_none())
        .map(|project| IssueLinkProject {
            id: project.id.clone(),
            environment_id: resolved_env.clone(),
            repository_identity: project.repository_identity.clone(),
        })
        .collect()
}

/// Checks whether the viewer holds `scope` on `env_id` (or active session); unknown grants return
/// `true` so the server remains authoritative (matching `pull_requests/detail.rs`).
fn session_grants_for(state: &AppState, env_id: Option<&str>, scope: &str) -> bool {
    if let Some(target) = env_id
        && let Some(env) = state.fleet.environments.iter().find(|env| {
            env.summary.owner.saved_id == target
                || env.summary.owner.canonical_id.as_deref() == Some(target)
                || env
                    .config
                    .config
                    .as_ref()
                    .is_some_and(|c| c.environment.environment_id == target)
        })
    {
        return env
            .grants
            .as_ref()
            .is_none_or(|grants| grants.grants(scope));
    }
    state
        .client_handle()
        .and_then(|handle| handle.session_state().borrow().clone())
        .is_none_or(|session| session.grants(scope))
}

pub(crate) fn can_write_issues(state: &AppState, env_id: Option<&str>) -> bool {
    session_grants_for(state, env_id, SOURCE_CONTROL_WRITE_SCOPE)
}

pub(crate) fn can_operate_threads(state: &AppState, env_id: Option<&str>) -> bool {
    session_grants_for(state, env_id, ORCHESTRATION_OPERATE_SCOPE)
}

/// One issue RPC read targeting a fleet environment via `connections.destination`, or falling
/// back to the active `client_handle` when `owner` is `None`.
pub(crate) fn read_for_owner<T: Send + 'static>(
    state: &AppState,
    owner: Option<&EnvironmentOwner>,
    method: &'static str,
    payload: Value,
    decode: fn(Value) -> Result<T, String>,
) -> Read<T> {
    if let Some(owner) = owner {
        let dest_fut = state.connections.destination(owner, ORCHESTRATION_READ_SCOPE);
        return Some(state.runtime().spawn(async move {
            let client = match dest_fut.await {
                Ok(dest) => dest.client,
                Err(error) => {
                    return Err(ReadFailure {
                        message: destination_error_message(error),
                        unsupported: false,
                    });
                }
            };
            map_rpc_result(client.request(method, payload).await, decode)
        }));
    }
    let client = state.client_handle()?;
    Some(state.runtime().spawn(async move {
        map_rpc_result(client.request(method, payload).await, decode)
    }))
}

/// One issue RPC mutation guarded by `source-control:write`.
pub(crate) fn write_for_owner<T: Send + 'static>(
    state: &AppState,
    owner: Option<&EnvironmentOwner>,
    method: &'static str,
    payload: Value,
    decode: fn(Value) -> Result<T, String>,
) -> Read<T> {
    if let Some(owner) = owner {
        let dest_fut = state
            .connections
            .destination(owner, SOURCE_CONTROL_WRITE_SCOPE);
        return Some(state.runtime().spawn(async move {
            let client = match dest_fut.await {
                Ok(dest) => dest.client,
                Err(error) => {
                    return Err(ReadFailure {
                        message: destination_error_message(error),
                        unsupported: false,
                    });
                }
            };
            map_rpc_result(client.request(method, payload).await, decode)
        }));
    }
    let client = state.client_handle()?;
    Some(state.runtime().spawn(async move {
        map_rpc_result(client.request(method, payload).await, decode)
    }))
}

/// Dispatches an orchestration command (`thread.issue.link`, `thread.issue.unlink`) guarded by
/// `orchestration:operate`.
pub(crate) fn dispatch_command_for_owner(
    state: &AppState,
    owner: Option<&EnvironmentOwner>,
    command: Value,
) -> Read<Value> {
    if let Some(owner) = owner {
        let dest_fut = state
            .connections
            .destination(owner, ORCHESTRATION_OPERATE_SCOPE);
        return Some(state.runtime().spawn(async move {
            let client = match dest_fut.await {
                Ok(dest) => dest.client,
                Err(error) => {
                    return Err(ReadFailure {
                        message: destination_error_message(error),
                        unsupported: false,
                    });
                }
            };
            map_rpc_result(client.request(DISPATCH_COMMAND, command).await, Ok)
        }));
    }
    let client = state.client_handle()?;
    Some(state.runtime().spawn(async move {
        map_rpc_result(client.request(DISPATCH_COMMAND, command).await, Ok)
    }))
}

pub(crate) async fn settle<T>(task: Read<T>) -> Result<T, ReadFailure> {
    match task {
        Some(task) => task.await.unwrap_or_else(|error| {
            Err(ReadFailure {
                message: error.to_string(),
                unsupported: false,
            })
        }),
        None => Err(ReadFailure {
            message: NOT_CONNECTED.to_owned(),
            unsupported: false,
        }),
    }
}

/// Maps `IssueState` to its glyph and semantic theme token (`issueStatePresentation`).
pub(crate) fn state_glyph(state: &IssueState, cx: &App) -> (Icon, Hsla) {
    let p = palette(cx);
    match issue_state_tone(state) {
        IssueStateTone::Open => (Icon::CircleDot, hsla(p.pill_completed)),
        IssueStateTone::Completed => (Icon::CircleCheck, hsla(p.pill_plan)),
        IssueStateTone::Dismissed => (Icon::CircleSlash, hsla(p.muted_foreground)),
        IssueStateTone::Unknown => (Icon::CircleDot, hsla(p.muted_foreground)),
    }
}

/// Maps `IssueTrackerKind` to a vendored icon (`issueTrackerPresentation`).
pub(crate) fn tracker_icon(tracker: &IssueTrackerKind) -> Icon {
    match tracker {
        IssueTrackerKind::GitHub
        | IssueTrackerKind::GitLab
        | IssueTrackerKind::AzureDevOps
        | IssueTrackerKind::Forgejo
        | IssueTrackerKind::TanStackSource => Icon::GitBranch,
        IssueTrackerKind::Linear | IssueTrackerKind::Jira | IssueTrackerKind::Unknown(_) => {
            Icon::Ticket
        }
    }
}
