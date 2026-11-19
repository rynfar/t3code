//! Pull request review submission, inline review drafts, comment creation/editing/replies,
//! and adapters bridging PR wire types to `t3_core::diff` (`InlineReviewThread`, `FileViewedState`).
//! Ports `apps/web/src/components/pullRequest/pullRequestReviewStore.ts`, `PullRequestReviewForm.tsx`,
//! `PullRequestCodeTab.tsx` review position mapping, and `filesViewedStore.ts`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::composer::review::{ReviewComment, Selection, Side};
use crate::diff::{
    FileDiff, FileViewedState as DiffFileViewedState, InlineReviewComment, InlineReviewThread, LineKind, ReviewActor,
};

use super::PrRef;
use super::detail::capitalize;
use super::wire::{Activity, CommentKind, FileViewedState, FilesViewedResult, ReviewVerdict};

/// `PullRequestReviewPosition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ReviewPosition {
    #[serde(rename_all = "camelCase")]
    Added { new_line: u32 },
    #[serde(rename_all = "camelCase")]
    Deleted { old_line: u32 },
    #[serde(rename_all = "camelCase")]
    Context { old_line: u32, new_line: u32, side: String },
}

/// One pending inline comment queued in a pull request review draft (`PendingReviewComment`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReviewComment {
    pub id: String,
    pub path: String,
    pub old_path: Option<String>,
    pub position: ReviewPosition,
    pub start_position: Option<ReviewPosition>,
    pub range_label: String,
    pub body: String,
}

impl PendingReviewComment {
    pub fn to_wire(&self) -> Value {
        let mut payload = json!({
            "path": self.path,
            "position": self.position,
            "body": self.body,
        });
        if let Some(old_path) = &self.old_path {
            payload["oldPath"] = json!(old_path);
        }
        if let Some(start_position) = &self.start_position {
            payload["startPosition"] = json!(start_position);
        }
        payload
    }
}

/// Per-PR in-progress review state (`pullRequestReviewStore.ts`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewDraft {
    pub summary: String,
    pub verdict: Option<ReviewVerdict>,
    pub comments: Vec<PendingReviewComment>,
}

impl ReviewDraft {
    pub fn add_comment(&mut self, comment: PendingReviewComment) {
        self.comments.push(comment);
    }

    pub fn remove_comment(&mut self, id: &str) {
        self.comments.retain(|comment| comment.id != id);
    }

    /// Removes only the comment IDs that were submitted and clears the summary only if it was not
    /// edited while the submission was in flight (`PullRequestReviewForm.tsx:134-139`).
    pub fn finish_submission(&mut self, submitted_ids: &[String], submitted_summary: &str) {
        self.comments.retain(|comment| !submitted_ids.iter().any(|id| id == &comment.id));
        if self.summary == submitted_summary {
            self.summary.clear();
        }
    }
}

/// `canSubmit` in `PullRequestReviewForm.tsx`: Forgejo requires a summary when requesting changes.
pub fn can_submit_review(verdict: ReviewVerdict, summary: &str, comment_count: usize, request_changes_summary_required: bool) -> bool {
    if verdict == ReviewVerdict::RequestChanges && request_changes_summary_required {
        !summary.trim().is_empty()
    } else {
        verdict == ReviewVerdict::Approve || !summary.trim().is_empty() || comment_count > 0
    }
}

pub fn review_verdict_label(verdict: ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::Comment => "Comment",
        ReviewVerdict::Approve => "Approve",
        ReviewVerdict::RequestChanges => "Request changes",
        ReviewVerdict::Unknown => "Comment",
    }
}

pub fn review_verdict_sent_label(verdict: ReviewVerdict, term: &str) -> String {
    match verdict {
        ReviewVerdict::Comment | ReviewVerdict::Unknown => "Review submitted".into(),
        ReviewVerdict::Approve => format!("{} approved", capitalize(term)),
        ReviewVerdict::RequestChanges => "Changes requested".into(),
    }
}

/// `PullRequestSubmitReviewInput`.
pub fn submit_review_input(reference: &PrRef, verdict: ReviewVerdict, body: &str, comments: &[PendingReviewComment]) -> Value {
    let mut payload = reference.payload();
    payload["verdict"] = json!(verdict.as_str());
    payload["body"] = json!(body);
    payload["comments"] = json!(comments.iter().map(PendingReviewComment::to_wire).collect::<Vec<_>>());
    payload
}

/// `PullRequestCommentInput`.
pub fn post_comment_input(reference: &PrRef, body: &str) -> Value {
    let mut payload = reference.payload();
    payload["body"] = json!(body);
    payload
}

/// `PullRequestCommentUpdateInput`.
pub fn update_comment_input(reference: &PrRef, comment_id: &str, kind: CommentKind, body: &str) -> Value {
    let mut payload = reference.payload();
    payload["commentId"] = json!(comment_id);
    payload["kind"] = json!(kind.as_str());
    payload["body"] = json!(body);
    payload
}

/// `PullRequestThreadReplyInput`.
pub fn reply_to_thread_input(reference: &PrRef, thread_id: &str, body: &str) -> Value {
    let mut payload = reference.payload();
    payload["threadId"] = json!(thread_id);
    payload["body"] = json!(body);
    payload
}

/// `PullRequestThreadCommentsInput`.
pub fn thread_comments_input(reference: &PrRef, thread_id: &str, cursor: &str) -> Value {
    let mut payload = reference.payload();
    payload["threadId"] = json!(thread_id);
    payload["cursor"] = json!(cursor);
    payload
}

fn line_position(file: &FileDiff, number: u32, side: Side) -> Option<ReviewPosition> {
    let found = file.hunks.iter().flat_map(|hunk| &hunk.lines).find(|line| match side {
        Side::Additions => line.new_line == Some(number),
        Side::Deletions => line.old_line == Some(number),
    });
    let Some(line) = found else {
        return Some(match side {
            Side::Additions => ReviewPosition::Added { new_line: number.max(1) },
            Side::Deletions => ReviewPosition::Deleted { old_line: number.max(1) },
        });
    };
    match line.kind {
        LineKind::Addition => Some(ReviewPosition::Added { new_line: line.new_line.unwrap_or(number).max(1) }),
        LineKind::Deletion => Some(ReviewPosition::Deleted { old_line: line.old_line.unwrap_or(number).max(1) }),
        LineKind::Context => Some(ReviewPosition::Context {
            old_line: line.old_line.unwrap_or(number).max(1),
            new_line: line.new_line.unwrap_or(number).max(1),
            side: match side {
                Side::Additions => "right".into(),
                Side::Deletions => "left".into(),
            },
        }),
    }
}

/// Converts an inline diff `ReviewComment` emitted by `DiffPanel::set_external_comment_handler`
/// into a `PendingReviewComment` ready for `pullRequests.submitReview`.
pub fn pending_comment_from_diff(comment: &ReviewComment, file: Option<&FileDiff>, range_comments: bool) -> Option<PendingReviewComment> {
    let body = comment.text.trim();
    if body.is_empty() {
        return None;
    }
    let selection = comment.selection.unwrap_or(Selection {
        start: 1,
        side: Side::Additions,
        end: 1,
        end_side: Side::Additions,
    });
    let (start_num, start_side, end_num, end_side) = if selection.start <= selection.end {
        (selection.start, selection.side, selection.end, selection.end_side)
    } else {
        (selection.end, selection.end_side, selection.start, selection.side)
    };
    let (position, start_position) = match file {
        Some(diff) => {
            let end_pos = line_position(diff, end_num, end_side)?;
            let start_pos = if range_comments && (start_num != end_num || start_side != end_side) {
                line_position(diff, start_num, start_side)
            } else {
                None
            };
            (end_pos, start_pos)
        }
        None => {
            let make = |n: u32, side: Side| match side {
                Side::Additions => ReviewPosition::Added { new_line: n.max(1) },
                Side::Deletions => ReviewPosition::Deleted { old_line: n.max(1) },
            };
            let end_pos = make(end_num, end_side);
            let start_pos = (range_comments && (start_num != end_num || start_side != end_side)).then(|| make(start_num, start_side));
            (end_pos, start_pos)
        }
    };
    let old_path = file.and_then(|diff| {
        let old = diff.old_path.as_deref()?;
        (old != diff.path()).then(|| old.to_owned())
    });
    Some(PendingReviewComment {
        id: comment.id.clone(),
        path: comment.file_path.clone(),
        old_path,
        position,
        start_position,
        range_label: comment.range_label.clone(),
        body: body.to_owned(),
    })
}

/// Converts `Activity::review_threads` into `DiffPanel::set_review_threads` inputs (`pingdotgg/t3code#34644`).
pub fn to_inline_review_threads(activity: &Activity) -> Vec<InlineReviewThread> {
    activity
        .review_threads
        .iter()
        .filter_map(|thread| {
            let path = thread.path.clone()?;
            let line = thread.line?.min(u32::MAX as u64) as u32;
            let side = match thread.side.as_deref() {
                Some("left") => Side::Deletions,
                _ => Side::Additions,
            };
            let comments = thread
                .comments
                .iter()
                .map(|comment| InlineReviewComment {
                    id: comment.id.clone(),
                    author: comment.author.as_ref().map(|actor| ReviewActor {
                        login: actor.login.clone(),
                        avatar_url: actor.avatar_url.clone(),
                    }),
                    body: comment.body.clone(),
                    created_at: Some(comment.created_at.clone()),
                })
                .collect();
            Some(InlineReviewThread {
                id: thread.id.clone(),
                path,
                side,
                line,
                is_resolved: thread.is_resolved,
                comments,
            })
        })
        .collect()
}

/// Converts `FilesViewedResult` from `pullRequests.filesViewed` into `DiffPanel::set_viewed_target`'s map.
pub fn files_viewed_map(result: &FilesViewedResult) -> HashMap<String, DiffFileViewedState> {
    result
        .files
        .iter()
        .map(|entry| {
            let state = match entry.state {
                FileViewedState::Viewed => DiffFileViewedState::Viewed,
                FileViewedState::Dismissed => DiffFileViewedState::Stale,
                FileViewedState::Unviewed | FileViewedState::Unknown => DiffFileViewedState::Unviewed,
            };
            (entry.path.clone(), state)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::diff::{ParsedPatch, parse_patch};

    use super::*;

    #[test]
    fn review_submission_rules_match_web_verdicts() {
        assert!(can_submit_review(ReviewVerdict::Approve, "", 0, false));
        assert!(!can_submit_review(ReviewVerdict::Comment, "  ", 0, false));
        assert!(can_submit_review(ReviewVerdict::Comment, "", 1, false));
        assert!(can_submit_review(ReviewVerdict::RequestChanges, "", 1, false));
        assert!(!can_submit_review(ReviewVerdict::RequestChanges, "", 1, true), "Forgejo requires summary");
        assert!(can_submit_review(ReviewVerdict::RequestChanges, "Needs tests", 0, true));
    }

    #[test]
    fn pending_comment_maps_diff_lines_and_preserves_concurrent_edits_on_finish() {
        let patch = "--- a/old.rs\n+++ b/new.rs\n@@ -10,3 +20,3 @@\n ctx\n-old\n+new\n tail\n";
        let ParsedPatch::Files(files) = parse_patch(patch) else { panic!("files") };
        let diff = &files[0];
        let comment = ReviewComment {
            id: "d1".into(),
            section_id: "s".into(),
            section_title: "s".into(),
            file_path: "new.rs".into(),
            start_index: 0,
            end_index: 2,
            range_label: "L20-L21".into(),
            text: Arc::from("Please check"),
            diff: Arc::from(""),
            fence_language: None,
            selection: Some(Selection { start: 20, side: Side::Additions, end: 21, end_side: Side::Additions }),
        };
        let pending = pending_comment_from_diff(&comment, Some(diff), true).unwrap();
        assert_eq!(pending.old_path.as_deref(), Some("old.rs"));
        assert_eq!(pending.position, ReviewPosition::Added { new_line: 21 });
        assert_eq!(
            pending.start_position,
            Some(ReviewPosition::Context { old_line: 10, new_line: 20, side: "right".into() })
        );

        let mut draft = ReviewDraft { summary: "Looks good".into(), verdict: Some(ReviewVerdict::Comment), comments: vec![pending] };
        let submitted_ids = vec!["d1".to_owned()];
        draft.summary = "Edited while submitting".into();
        draft.finish_submission(&submitted_ids, "Looks good");
        assert!(draft.comments.is_empty());
        assert_eq!(draft.summary, "Edited while submitting");
    }

    #[test]
    fn converts_review_threads_and_files_viewed_for_diff_panel() {
        let activity: Activity = serde_json::from_value(json!({
            "reviewThreads": [{
                "id": "t1", "path": "src/a.rs", "line": 12, "side": "left", "isResolved": true, "isOutdated": false,
                "comments": [{ "id": "c1", "author": { "login": "alice" }, "body": "Fix", "createdAt": "2026-01-01T00:00:00Z" }]
            }, {
                "id": "t2", "path": null, "line": null, "side": "right", "isResolved": false, "isOutdated": false,
                "comments": []
            }]
        }))
        .unwrap();
        let inline = to_inline_review_threads(&activity);
        assert_eq!(inline.len(), 1);
        assert_eq!((inline[0].path.as_str(), inline[0].side, inline[0].line, inline[0].is_resolved), ("src/a.rs", Side::Deletions, 12, true));

        let viewed: FilesViewedResult = serde_json::from_value(json!({
            "files": [
                { "path": "a.rs", "state": "viewed" },
                { "path": "b.rs", "state": "dismissed" }
            ],
            "truncated": false
        }))
        .unwrap();
        let map = files_viewed_map(&viewed);
        assert_eq!(map.get("a.rs"), Some(&DiffFileViewedState::Viewed));
        assert_eq!(map.get("b.rs"), Some(&DiffFileViewedState::Stale));
    }
}
