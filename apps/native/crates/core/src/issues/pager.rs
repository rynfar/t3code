//! Backwards comment pagination and refresh-safe state for issue detail panels
//! (`createIssueCommentPager` in `packages/client-runtime/src/state/issues.ts`,
//! `pingdotgg/t3code#38489`, `pingdotgg/t3code#39000`).

use std::collections::HashSet;

use super::wire::{IssueComment, IssueCommentsPage, IssueDetail};

/// Prepends older comments ahead of `current`, dropping any comment whose `id` is already present
/// (`prependIssueComments`).
pub fn prepend_issue_comments(
    current: &[IssueComment],
    older: &[IssueComment],
) -> Vec<IssueComment> {
    if older.is_empty() {
        return current.to_vec();
    }
    let seen: HashSet<&str> = current.iter().map(|comment| comment.id.as_str()).collect();
    let mut out: Vec<IssueComment> = older
        .iter()
        .filter(|comment| !seen.contains(comment.id.as_str()))
        .cloned()
        .collect();
    out.extend(current.iter().cloned());
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OlderComments {
    seed: String,
    comments: Vec<IssueComment>,
    next_cursor: Option<String>,
}

/// Snapshot of comments and pagination controls derived from the current detail + pager state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentPagerView {
    pub comments: Vec<IssueComment>,
    pub next_cursor: Option<String>,
    pub loading_older: bool,
    pub can_load_older: bool,
}

/// Token issued when a `"Load older comments"` RPC starts; passed back on completion so an older
/// page that returns after a successful refresh is discarded (`pingdotgg/t3code#38489`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OlderCommentsRequest {
    pub seed: String,
    pub cursor: String,
    pub refresh_epoch: u64,
}

/// Manages older comment pages alongside `IssueDetail` refreshes (`createIssueCommentPager`):
/// - A successful refresh clears `older` and bumps `refreshes` even if `comments_cursor` is
///   unchanged, so previously loaded older pages do not reappear (`pingdotgg/t3code#38489`).
/// - A failed refresh keeps `older` intact so already-loaded comments remain visible while the
///   refresh error banner is shown (`pingdotgg/t3code#39000`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueCommentPager {
    older: Option<OlderComments>,
    loading_older: bool,
    refreshing: bool,
    refreshes: u64,
}

impl IssueCommentPager {
    pub fn reset(&mut self) {
        self.older = None;
        self.loading_older = false;
        self.refreshing = false;
        self.refreshes = self.refreshes.wrapping_add(1);
    }

    pub fn is_refreshing(&self) -> bool {
        self.refreshing
    }

    pub fn is_loading_older(&self) -> bool {
        self.loading_older
    }

    fn effective_older<'a>(&'a self, seed: Option<&str>) -> Option<&'a OlderComments> {
        let seed = seed?;
        self.older.as_ref().filter(|older| older.seed == seed)
    }

    pub fn view(&self, detail: Option<&IssueDetail>, detail_waiting: bool) -> CommentPagerView {
        let seed = detail.and_then(|d| d.comments_cursor.as_deref());
        let older = self.effective_older(seed);
        let next_cursor = match older {
            Some(page) => page.next_cursor.clone(),
            None => seed.map(str::to_owned),
        };
        let unavailable = detail_waiting || self.refreshing || self.loading_older;
        let comments = match detail {
            Some(d) => prepend_issue_comments(
                &d.comments,
                older.map_or(&[][..], |page| page.comments.as_slice()),
            ),
            None => Vec::new(),
        };
        let can_load_older = next_cursor.is_some() && !unavailable;
        CommentPagerView {
            comments,
            next_cursor,
            loading_older: self.loading_older,
            can_load_older,
        }
    }

    /// Marks the start of a detail refresh. Returns `false` if one is already in flight.
    pub fn begin_refresh(&mut self) -> bool {
        if self.refreshing {
            return false;
        }
        self.refreshing = true;
        true
    }

    /// Completes a detail refresh.
    ///
    /// Only a successful refresh drops `older` and advances `refreshes`; a failed refresh leaves
    /// already-loaded older comments intact (`pingdotgg/t3code#39000`).
    pub fn finish_refresh(&mut self, succeeded: bool) {
        self.refreshing = false;
        if succeeded {
            self.older = None;
            self.refreshes = self.refreshes.wrapping_add(1);
        }
    }

    /// Starts loading the next older page if available and idle.
    pub fn begin_load_older(
        &mut self,
        detail: Option<&IssueDetail>,
        detail_waiting: bool,
    ) -> Option<OlderCommentsRequest> {
        if detail_waiting || self.refreshing || self.loading_older {
            return None;
        }
        let seed = detail?.comments_cursor.as_deref()?.to_owned();
        let cursor = match self.effective_older(Some(&seed)) {
            Some(page) => page.next_cursor.clone()?,
            None => seed.clone(),
        };
        self.loading_older = true;
        Some(OlderCommentsRequest {
            seed,
            cursor,
            refresh_epoch: self.refreshes,
        })
    }

    /// Finishes an older-comments request. Returns `true` if the page was applied (not stale and
    /// not an error).
    pub fn finish_load_older(
        &mut self,
        request: &OlderCommentsRequest,
        result: Result<IssueCommentsPage, ()>,
    ) -> bool {
        self.loading_older = false;
        if self.refreshes != request.refresh_epoch {
            return false;
        }
        let Ok(page) = result else {
            return false;
        };
        let existing = self
            .older
            .as_ref()
            .filter(|older| older.seed == request.seed)
            .map_or(&[][..], |older| older.comments.as_slice());
        let comments = prepend_issue_comments(existing, &page.comments);
        self.older = Some(OlderComments {
            seed: request.seed.clone(),
            comments,
            next_cursor: page.next_cursor,
        });
        true
    }
}
