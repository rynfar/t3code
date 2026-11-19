//! Issues page (`routes/_chat.issues.tsx`, `IssuesPage.tsx`, `IssueRow.tsx`,
//! `IssueListFilterPickers.tsx`, `SavedViewsMenu.tsx`). Domain logic lives in `t3_core::issues`;
//! reads are decoded on the network runtime.

mod page;
mod row;
mod toolbar;

use gpui::{App, Context, Entity, Hsla, actions};
use serde_json::Value;
use t3_client::session::RpcError;
use t3_core::issues::wire::{
    IssueState, IssueStateCategory, IssueTrackerKind, failure_message,
    is_issues_unsupported_error,
};
use tokio::task::JoinHandle;

pub use page::IssuesPage;

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

actions!(issues, [OpenIssues, CloseIssues, CopyIssueKey]);

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

/// One issue RPC read, run and decoded on the network runtime.
pub(crate) fn read<T: Send + 'static>(
    state: &AppState,
    method: &'static str,
    payload: Value,
    decode: fn(Value) -> Result<T, String>,
) -> Read<T> {
    let client = state.client_handle()?;
    Some(state.runtime().spawn(async move {
        match client.request(method, payload).await {
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
    match state.category {
        IssueStateCategory::Open => (Icon::CircleDot, hsla(p.pill_completed)),
        IssueStateCategory::Closed => {
            if state
                .close_reason
                .as_deref()
                .is_some_and(|reason| reason != "completed")
            {
                (Icon::CircleSlash, hsla(p.muted_foreground))
            } else {
                (Icon::CircleCheck, hsla(p.pill_plan))
            }
        }
        IssueStateCategory::Unknown => (Icon::CircleDot, hsla(p.muted_foreground)),
    }
}

/// Maps `IssueTrackerKind` to a vendored icon (`issueTrackerPresentation`).
pub(crate) fn tracker_icon(tracker: &IssueTrackerKind) -> Icon {
    match tracker {
        IssueTrackerKind::GitHub
        | IssueTrackerKind::GitLab
        | IssueTrackerKind::AzureDevOps
        | IssueTrackerKind::Forgejo => Icon::GitBranch,
        IssueTrackerKind::Linear | IssueTrackerKind::Jira | IssueTrackerKind::Unknown(_) => {
            Icon::Ticket
        }
    }
}
