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

use gpui::{App, Context, Entity, FontWeight, IntoElement, ParentElement, Styled, Window, actions, div, prelude::*, px};
use serde_json::Value;
use t3_client::session::RpcError;
use t3_core::pull_requests::failure_message;
use tokio::task::JoinHandle;

pub use link_dialog::LinkPullRequestDialog;
pub use page::PullRequestsPage;
pub use thread_panel::ThreadPullRequestsPanel;

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

actions!(pull_requests, [OpenPullRequests, ClosePullRequests, LinkPullRequest, ShowLinkedPullRequests, CopyPullRequestNumber]);

impl AppState {
    /// Opens or leaves the pull requests page, which replaces the thread area while open.
    pub fn set_pull_requests_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.pull_requests_open != open {
            self.pull_requests_open = open;
            cx.emit(AppEvent::Selection);
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

/// The sidebar footer's utility item (`SidebarUtilityMenu`): an icon button for "Pull requests",
/// or "Back" with its label while the page is open. Sidebar menu buttons (design-system §6.8),
/// a tab stop that Enter and Space press.
pub fn sidebar_item(state: &Entity<AppState>, window: &mut Window, cx: &mut App) -> impl IntoElement {
    let p = palette(cx);
    let open = state.read(cx).pull_requests_open;
    let state = state.clone();
    let button = crate::right_panel::focusable(div().id("sidebar-pull-requests"), "sidebar-pull-requests".into(), true, window, cx)
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(32.))
        .rounded(px(8.))
        .cursor_pointer()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.sidebar_muted_foreground).opacity(0.8))
        .hover(|this| this.bg(hsla(p.sidebar_row_hover)))
        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.set_pull_requests_open(!open, cx)));
    let button = if open {
        button.flex_1().px(px(10.)).child(Icon::ArrowLeft.sized(px(16.)).text_color(hsla(p.sidebar_icon))).child("Back")
    } else {
        button
            .w(px(32.))
            .justify_center()
            .child(Icon::GitPullRequestArrow.sized(px(16.)).text_color(hsla(p.sidebar_icon)))
            .tooltip(|window, cx| crate::right_panel::tooltip_view("Pull requests".into(), window, cx))
    };
    div().flex().flex_none().items_center().px(px(8.)).pb(px(8.)).child(button)
}

/// Opens the link dialog for the open thread (`openLinkPullRequestDialog`).
pub fn open_link_dialog(dialog: &Entity<LinkPullRequestDialog>, window: &mut Window, cx: &mut App) {
    dialog.update(cx, |dialog, cx| dialog.open(window, cx));
}
