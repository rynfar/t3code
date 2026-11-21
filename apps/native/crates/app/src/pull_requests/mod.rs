//! Pull requests: the page (`routes/_chat.pull-requests.tsx`, `PullRequestsPage.tsx`), its
//! detail panel (`PullRequestDetailPanel.tsx`), the link dialog (`LinkPullRequestDialog.tsx`)
//! and a thread's linked pull requests (`ThreadPullRequestsPanel.tsx`). Logic lives in
//! `t3_core::pull_requests`; reads are decoded on the network runtime.

mod code;
pub(crate) mod controls;
mod detail;
mod guide;
mod interactions;
mod link_dialog;
pub(crate) mod mutations;
mod page;
mod preview;
mod row;
mod summary;
mod thread_panel;
mod timeline;
mod toolbar;

use gpui::{App, Context, Entity, Window, actions};
use serde_json::Value;
use t3_client::environments::DestinationError;
use t3_client::session::RpcError;
use t3_core::auth::ORCHESTRATION_READ_SCOPE;
use t3_core::pull_requests::failure_message;
use t3_core::sidebar::grouping::EnvironmentOwner;
use tokio::task::JoinHandle;

pub(crate) use detail::{DetailView, Tab as DetailTab};
pub use link_dialog::LinkPullRequestDialog;
pub use page::PullRequestsPage;
pub use thread_panel::ThreadPullRequestsPanel;

use crate::app_state::{AppEvent, AppState};

actions!(pull_requests, [OpenPullRequests, ClosePullRequests, LinkPullRequest, ShowLinkedPullRequests, CopyPullRequestNumber]);

pub(crate) const PULL_REQUESTS_UTILITY_PANEL_KEY: &str = "pull-requests-panel";

impl AppState {
    pub(crate) fn details_utility_panel_key(&self) -> Option<&'static str> {
        if self.pull_requests_open {
            Some(PULL_REQUESTS_UTILITY_PANEL_KEY)
        } else if self.issues_open {
            Some(t3_core::right_panel::ISSUES_PANEL_THREAD_KEY)
        } else {
            None
        }
    }

    pub(crate) fn uses_shared_utility_panel(&self) -> bool {
        self.details_utility_panel_key().is_some_and(|key| self.panel_thread().as_deref() == Some(key))
    }

    pub(crate) fn open_utility_pull_request(&mut self, target: t3_core::right_panel::PullRequestTarget, cx: &mut Context<Self>) -> bool {
        let Some(key) = self.details_utility_panel_key().filter(|_| self.uses_shared_utility_panel()) else { return false };
        if self.panels.panels.open_pull_request(key, target) {
            self.save_panels(cx);
        }
        true
    }

    /// Opens or leaves the pull requests page, which replaces the thread area while open.
    pub fn set_pull_requests_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.pull_requests_open != open {
            self.pull_requests_open = open;
            cx.emit(AppEvent::Selection);
            self.land_home(cx);
        }
    }
}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    mutations::init(state.read(cx).runtime().clone(), cx);
    let open = state.clone();
    cx.on_action(move |_: &OpenPullRequests, cx| open.update(cx, |state, cx| state.set_pull_requests_open(true, cx)));
    cx.on_action(move |_: &ClosePullRequests, cx| state.update(cx, |state, cx| state.set_pull_requests_open(false, cx)));
}

/// The snapshot tool's `--pull-request <number>`: selected once the list holds it, on `tab`.
#[cfg(feature = "snapshot")]
pub struct SnapshotSelection {
    pub number: u64,
    pub code: bool,
}

#[cfg(feature = "snapshot")]
impl gpui::Global for SnapshotSelection {}

pub(crate) type Read<T> = Option<JoinHandle<Result<T, String>>>;

/// One read, run and decoded on the network runtime, with tagged errors worded as the page
/// words them. `None` while disconnected.
pub(crate) fn read<T: Send + 'static>(state: &AppState, method: &'static str, payload: Value, decode: fn(Value) -> Result<T, String>) -> Read<T> {
    let client = state.client_handle()?;
    Some(state.runtime().spawn(async move {
        match client.request(method, payload).await {
            Ok(value) => decode(value),
            Err(RpcError::Failure(failure)) => Err(failure_message(failure.tag.as_deref(), &failure.error, &failure.message)),
            Err(RpcError::Closed) => Err(NOT_CONNECTED.to_owned()),
        }
    }))
}

pub(crate) fn destination_error_message(error: DestinationError) -> String {
    match error {
        DestinationError::Denied => "Permission required for this environment.".to_owned(),
        DestinationError::Disconnected | DestinationError::Unsynchronized => NOT_CONNECTED.to_owned(),
        DestinationError::Missing | DestinationError::Stale => "This environment is no longer available.".to_owned(),
    }
}

/// Finds the fleet `EnvironmentOwner` matching `env_id` (either `saved_id` or `canonical_id`).
pub(crate) fn owner_for_env(state: &AppState, env_id: &str) -> Option<EnvironmentOwner> {
    state
        .fleet
        .environments
        .iter()
        .find(|env| env.summary.owner.saved_id == env_id || env.summary.owner.canonical_id.as_deref() == Some(env_id))
        .map(|env| env.summary.owner.clone())
}

/// Resolves the fleet `EnvironmentOwner` for a pull request: prefers an explicit `env_id`,
/// otherwise finds the enabled fleet environment owning `project_id`. Returns `None` when no
/// fleet environments are loaded (single-host mode).
pub(crate) fn owner_for_pr(state: &AppState, env_id: Option<&str>, project_id: &str) -> Option<EnvironmentOwner> {
    if let Some(id) = env_id
        && let Some(owner) = owner_for_env(state, id)
    {
        return Some(owner);
    }
    state
        .fleet
        .environments
        .iter()
        .filter(|env| {
            !state
                .environments
                .environments
                .iter()
                .any(|saved| saved.id == env.summary.owner.saved_id && saved.disabled)
        })
        .find(|env| env.summary.projects.iter().any(|scoped| scoped.project.id == project_id))
        .map(|env| env.summary.owner.clone())
}

/// One PR RPC read targeting a fleet environment via `connections.destination`, or falling back
/// to the active `client_handle` when `owner` is `None`.
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
            let client = dest_fut.await.map_err(destination_error_message)?.client;
            match client.request(method, payload).await {
                Ok(value) => decode(value),
                Err(RpcError::Failure(failure)) => Err(failure_message(failure.tag.as_deref(), &failure.error, &failure.message)),
                Err(RpcError::Closed) => Err(NOT_CONNECTED.to_owned()),
            }
        }));
    }
    read(state, method, payload, decode)
}

/// One PR diff HTTP read (`POST /api/pull-requests/diff`) targeting `owner` when set, or the
/// active `client_handle` in single-host mode.
pub(crate) fn diff_for_owner(state: &AppState, owner: Option<&EnvironmentOwner>, payload: Value) -> Read<Value> {
    if let Some(owner) = owner {
        let dest_fut = state.connections.destination(owner, ORCHESTRATION_READ_SCOPE);
        return Some(state.runtime().spawn(async move {
            let client = dest_fut.await.map_err(destination_error_message)?.client;
            client
                .pull_request_diff(payload)
                .await
                .map_err(|failure| failure_message(failure.tag.as_deref(), &failure.error, &failure.message))
        }));
    }
    let client = state.client_handle()?;
    Some(state.runtime().spawn(async move {
        client
            .pull_request_diff(payload)
            .await
            .map_err(|failure| failure_message(failure.tag.as_deref(), &failure.error, &failure.message))
    }))
}

pub(crate) async fn settle<T>(task: Read<T>) -> Result<T, String> {
    match task {
        Some(task) => task.await.unwrap_or_else(|error| Err(error.to_string())),
        None => Err(NOT_CONNECTED.to_owned()),
    }
}

pub(crate) const NOT_CONNECTED: &str = "Not connected to the server.";

/// Opens the link dialog for the open thread (`openLinkPullRequestDialog`).
pub fn open_link_dialog(dialog: &Entity<LinkPullRequestDialog>, window: &mut Window, cx: &mut App) {
    dialog.update(cx, |dialog, cx| dialog.open(window, cx));
}

pub(crate) fn link_projects(state: &AppState) -> Vec<t3_core::pull_requests::target::Project<'_>> {
    let Some(environment) = state.environment() else { return Vec::new() };
    state.shell.snapshot.as_ref().into_iter().flat_map(|s| &s.projects).map(|project| t3_core::pull_requests::target::Project {
        id: &project.id, environment, identity: project.repository_identity.as_ref(), config_loaded: state.server_config.config.is_some(),
    }).collect()
}

pub(crate) fn open_resolved_link(state: &Entity<AppState>, target: t3_core::pull_requests::target::Target, cx: &mut App) {
    if state.read(cx).environment() != Some(target.environment.as_str()) { return; }
    let open_in_right_panel = {
        let app = state.read(cx);
        !app.pull_requests_open && app.selected_thread.is_some()
    };
    if open_in_right_panel {
        state.update(cx, |state, cx| {
            state.open_pull_request_surface(
                t3_core::right_panel::PullRequestTarget {
                    environment_id: Some(target.environment),
                    project_id: target.reference.project_id,
                    host: target.reference.host,
                    repository: target.reference.repository,
                    number: target.reference.number,
                    url: Some(target.url.clone()),
                    reveal_url: Some(target.url),
                    detail_tab: None,
                },
                cx,
            );
        });
        return;
    }
    cx.set_global(thread_panel::PendingSelection(Some((target.reference, Some(target.url)))));
    state.update(cx, |state, cx| {
        state.set_pull_requests_open(true, cx);
        cx.emit(AppEvent::Selection);
    });
}

/// Context chips use the same readable-project resolver as HTML page links.
pub(crate) fn open_context_link(state: &Entity<AppState>, url: &str, cx: &mut App) {
    let app = state.read(cx);
    let environment = app.environment();
    let project = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id)).map(|t| t.project_id.as_str()).or(app.draft_project.as_deref());
    let target = t3_core::pull_requests::target::resolve(url, &link_projects(app), environment, project, environment, true);
    match target {
        Some(target) => open_resolved_link(state, target, cx),
        None => cx.open_url(url),
    }
}
