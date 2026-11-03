//! Pull requests: the page (`routes/_chat.pull-requests.tsx`, `PullRequestsPage.tsx`), its
//! detail panel (`PullRequestDetailPanel.tsx`), the link dialog (`LinkPullRequestDialog.tsx`)
//! and a thread's linked pull requests (`ThreadPullRequestsPanel.tsx`). Logic lives in
//! `t3_core::pull_requests`; reads are decoded on the network runtime.

mod code;
mod controls;
mod detail;
mod link_dialog;
mod mutations;
mod page;
mod row;
mod summary;
mod thread_panel;
mod timeline;
mod toolbar;

use gpui::{App, Context, Entity, Window, actions};
use serde_json::Value;
use t3_client::session::RpcError;
use t3_core::pull_requests::failure_message;
use tokio::task::JoinHandle;

pub use link_dialog::LinkPullRequestDialog;
pub use page::PullRequestsPage;
pub use thread_panel::ThreadPullRequestsPanel;

use crate::app_state::{AppEvent, AppState};

actions!(pull_requests, [OpenPullRequests, ClosePullRequests, LinkPullRequest, ShowLinkedPullRequests, CopyPullRequestNumber]);

impl AppState {
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
    cx.set_global(thread_panel::PendingSelection(Some(target.reference)));
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
