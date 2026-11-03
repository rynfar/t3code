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

/// Context chips use the same detail selection as linked pull requests.
pub(crate) fn open_context_link(state: &Entity<AppState>, url: &str, cx: &mut App) {
    let link = t3_core::pull_requests::link::parse_change_request_url(url);
    let project = {
        let state = state.read(cx);
        state.selected_thread.as_deref().and_then(|id| state.thread_shell(id)).map(|t| t.project_id.clone()).or_else(|| state.draft_project.clone())
    };
    let (Some(link), Some(project_id)) = (link, project) else { cx.open_url(url); return };
    cx.set_global(thread_panel::PendingSelection(Some(t3_core::pull_requests::PrRef {
        project_id, host: Some(link.host), repository: link.repository, number: link.number,
    })));
    state.update(cx, |state, cx| {
        state.set_pull_requests_open(true, cx);
        cx.emit(AppEvent::Selection);
    });
}
