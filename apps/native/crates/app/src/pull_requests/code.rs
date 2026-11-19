//! The Code tab (`PullRequestCodeTab.tsx`): the patch from `POST /api/pull-requests/diff`, read in
//! slices (`nextCursor`) as the reader nears the end of what is loaded, each slice parsed once on
//! the network runtime (`sliceParses`) and the parses joined there for the diff panel. A link
//! naming a file's diff, by the host's digest of its path, reveals that file and its lines once
//! a loaded slice holds it (`useCodeViewFileReveal`). Wires `DiffPanel::set_review_threads`
//! (`pingdotgg/t3code#34644`), `DiffPanel::set_viewed_target` (`pingdotgg/t3code#35114`),
//! `DiffPanel::set_external_comment_handler` (inline PR review drafts), and
//! `DiffPanel::set_completion_scope` (PR `@mention` / `#ref` completion).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Task, WeakEntity, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_core::composer::review::{ReviewComment, Side as DiffSide};
use t3_core::diff::{CompletionScope, CompletionSupport, FileDiff, FileRevealTarget, ParsedPatch, ReferenceSigil};
use t3_core::pull_requests::anchor::{Range, Side, diff_path_matches};
use t3_core::pull_requests::review::{files_viewed_map, pending_comment_from_diff, to_inline_review_threads};
use t3_core::pull_requests::wire::{Activity, Detail, DiffResult, FilesViewedResult, decode};
use t3_core::pull_requests::{PrRef, failure_message, method};

use super::controls::{Button, Size, Variant};
use super::detail::DetailView;
use super::{read, settle};
use crate::app_state::AppState;
use crate::diff::{DiffPanel, External, Parsed, ViewedTarget};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// The sentinel's `rootMargin`: the next slice is read once the end is this close.
const NEAR_END: f32 = 240.;

pub struct CodeTab {
    state: Entity<AppState>,
    detail_view: WeakEntity<DetailView>,
    reference: PrRef,
    panel: Entity<DiffPanel>,
    /// Each slice's own parse, in order.
    slices: Vec<Arc<Parsed>>,
    /// Where the next slice starts, read once the reader nears the end.
    next_cursor: Option<String>,
    pending: bool,
    error: Option<String>,
    /// Counts reloads, so a slice read before one is dropped.
    generation: u64,
    near_end_check: bool,
    /// Optional single-commit filter when opened from a PR commit link (`pullRequestLinkCommit`).
    commit: Option<String>,
    /// A linked file's digest and lines, until a loaded slice has the file.
    reveal: Option<(String, Option<Range>)>,
    /// A direct path reveal (from GuideTab file jump), until a loaded slice holds the file.
    reveal_path: Option<String>,
    reveals: u64,
    viewed_supported: bool,
    viewed_can_write: bool,
    inline_allowed: bool,
    range_comments: bool,
    /// PR-head inline review threads, cleared on `DiffPanel` while viewing a single commit and
    /// restored when returning to the full PR diff (`PullRequestCodeTab.tsx:595, 637-638`).
    head_review_threads: Vec<t3_core::diff::InlineReviewThread>,
    _fetch: Option<Task<()>>,
    _viewed: Option<Task<()>>,
    #[cfg(test)]
    last_requested_cursor: Option<String>,
}

impl CodeTab {
    pub fn new(
        state: Entity<AppState>,
        detail_view: WeakEntity<DetailView>,
        reference: PrRef,
        detail: Option<&Detail>,
        activity: Option<&Activity>,
        can_write: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let panel = cx.new(|cx| DiffPanel::detached(state.clone(), cx));
        let this = cx.entity().downgrade();
        let rows = panel.downgrade();
        // The list holds its state borrowed while it reports a scroll, so it is read afterwards.
        panel.read(cx).on_rows_scrolled(move |_, _, cx| {
            let (this, rows) = (this.clone(), rows.clone());
            cx.defer(move |cx| {
                let near = rows.upgrade().is_some_and(|rows| rows.read(cx).near_end(px(NEAR_END)));
                if near && let Some(this) = this.upgrade() {
                    this.update(cx, |tab, cx| tab.load_next(cx));
                }
            });
        });
        let mut tab = CodeTab {
            state,
            detail_view,
            reference,
            panel,
            slices: Vec::new(),
            next_cursor: None,
            pending: false,
            error: None,
            generation: 0,
            near_end_check: false,
            commit: None,
            reveal: None,
            reveal_path: None,
            reveals: 0,
            viewed_supported: false,
            viewed_can_write: false,
            inline_allowed: false,
            range_comments: false,
            head_review_threads: Vec::new(),
            _fetch: None,
            _viewed: None,
            #[cfg(test)]
            last_requested_cursor: None,
        };
        tab.sync_state(detail, activity, can_write, cx);
        tab.fetch(None, cx);
        tab
    }

    /// Synchronizes host capabilities (inline commenting, completion scope, viewed files) and
    /// inline review conversations (`DiffPanel::set_review_threads`) from `DetailView`.
    pub fn sync_state(&mut self, detail: Option<&Detail>, activity: Option<&Activity>, can_write: bool, cx: &mut Context<Self>) {
        if let Some(activity) = activity {
            self.sync_review_threads(activity, cx);
        }
        if let Some(detail) = detail {
            self.sync_capabilities(detail, can_write, cx);
        }
    }

    pub fn sync_review_threads(&mut self, activity: &Activity, cx: &mut Context<Self>) {
        self.head_review_threads = to_inline_review_threads(activity);
        self.apply_commit_review_state(cx);
    }

    /// Commit-scoped diffs use commit-local coordinates, so PR-head inline review threads and
    /// inline review comment drafting are disabled while `self.commit.is_some()` and restored when
    /// viewing the full PR diff (`PullRequestCodeTab.tsx:595, 637-638`).
    fn apply_commit_review_state(&self, cx: &mut Context<Self>) {
        if self.commit.is_some() {
            self.panel.update(cx, |panel, cx| {
                panel.set_review_threads(Vec::new(), cx);
                panel.set_external_comment_handler(None, cx);
            });
            return;
        }
        let threads = self.head_review_threads.clone();
        let handler = self.inline_allowed.then(|| {
            let this = cx.entity().downgrade();
            Rc::new(move |comment: ReviewComment, cx: &mut gpui::App| {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |tab, cx| tab.handle_inline_comment(comment, cx));
                }
            }) as Rc<dyn Fn(ReviewComment, &mut gpui::App)>
        });
        self.panel.update(cx, |panel, cx| {
            panel.set_review_threads(threads, cx);
            panel.set_external_comment_handler(handler, cx);
        });
    }

    fn sync_capabilities(&mut self, detail: &Detail, can_write: bool, cx: &mut Context<Self>) {
        self.range_comments = detail.capabilities.review.range_comment == Some(true);
        self.inline_allowed = can_write && detail.capabilities.review.inline_comment && detail.viewer_permissions.comment;
        self.apply_commit_review_state(cx);

        let reference_sigils: Vec<ReferenceSigil> = detail
            .capabilities
            .reference_sigils
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter_map(|sigil| match sigil.as_str() {
                "#" => Some(ReferenceSigil::Hash),
                "!" => Some(ReferenceSigil::Bang),
                _ => None,
            })
            .collect();
        let support = CompletionSupport {
            mentions: detail.capabilities.mentions == Some(true),
            reference_sigils,
        };
        let scope = support.is_enabled().then(|| CompletionScope {
            scope_key: self.reference.key(),
            project_id: self.reference.project_id.clone(),
            support,
        });
        self.panel.update(cx, |panel, cx| panel.set_completion_scope(scope, cx));

        let viewed_now = detail.capabilities.viewed_files.is_some();
        let target = viewed_now.then(|| self.viewed_target(can_write));
        if viewed_now != self.viewed_supported || (viewed_now && can_write != self.viewed_can_write) {
            self.viewed_supported = viewed_now;
            self.viewed_can_write = can_write;
            if viewed_now {
                self.panel.update(cx, |panel, cx| panel.set_viewed_target(target, HashMap::new(), cx));
                self.fetch_files_viewed(can_write, cx);
            } else {
                self.panel.update(cx, |panel, cx| panel.set_viewed_target(None, HashMap::new(), cx));
            }
        }
    }

    fn viewed_target(&self, enabled: bool) -> ViewedTarget {
        ViewedTarget {
            project_id: self.reference.project_id.clone(),
            number: self.reference.number,
            host: self.reference.host.clone(),
            repository: Some(self.reference.repository.clone()),
            commit: self.commit.clone(),
            enabled,
        }
    }

    fn fetch_files_viewed(&mut self, can_write: bool, cx: &mut Context<Self>) {
        if !self.viewed_supported {
            return;
        }
        let target = self.viewed_target(can_write);
        let task = read(self.state.read(cx), method::FILES_VIEWED, self.reference.payload(), decode::<FilesViewedResult>);
        self._viewed = Some(cx.spawn(async move |this, cx| {
            let Ok(result) = settle(task).await else { return };
            let states = files_viewed_map(&result);
            let _ = this.update(cx, |tab, cx| {
                tab.panel.update(cx, |panel, cx| panel.set_viewed_target(Some(target), states, cx));
            });
        }));
    }

    fn handle_inline_comment(&mut self, comment: ReviewComment, cx: &mut Context<Self>) {
        let file = self
            .slices
            .iter()
            .flat_map(|slice| slice.files().iter())
            .find(|file| file.path() == comment.file_path);
        let Some(pending) = pending_comment_from_diff(&comment, file, self.range_comments) else {
            return;
        };
        if let Some(detail_view) = self.detail_view.upgrade() {
            detail_view.update(cx, |view, cx| view.add_pending_review_comment(pending, cx));
        }
    }

    pub fn set_commit(&mut self, commit: Option<String>, cx: &mut Context<Self>) {
        if self.commit != commit {
            self.commit = commit;
            self.apply_commit_review_state(cx);
            if self.viewed_supported {
                let can_write = self.viewed_can_write;
                let target = self.viewed_target(can_write);
                self.panel.update(cx, |panel, cx| panel.set_viewed_target(Some(target), HashMap::new(), cx));
                self.fetch_files_viewed(can_write, cx);
            }
            self.reload(cx);
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.slices.clear();
        self.next_cursor = None;
        self.fetch(None, cx);
        if self.viewed_supported {
            self.fetch_files_viewed(self.viewed_can_write, cx);
        }
    }

    /// `canLoadNextSlice`: never after a failure, which would retry without end.
    fn can_load_next(&self) -> bool {
        self.next_cursor.is_some() && !self.pending && self.error.is_none()
    }

    fn load_next(&mut self, cx: &mut Context<Self>) {
        if self.can_load_next() {
            let cursor = self.next_cursor.clone();
            self.fetch(cursor, cx);
        }
    }

    fn fetch(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.last_requested_cursor = cursor.clone();
        }
        let Some(client) = self.state.read(cx).client_handle() else {
            self.error = Some(super::NOT_CONNECTED.to_owned());
            return self.show(cx);
        };
        let mut payload = self.reference.payload();
        if let Some(cursor) = &cursor {
            payload["cursor"] = json!(cursor);
        }
        if let Some(commit) = &self.commit {
            payload["commit"] = json!(commit);
        }
        let held = self.slices.clone();
        let task = self.state.read(cx).runtime().spawn(async move {
            let value = client.pull_request_diff(payload).await.map_err(|failure| failure_message(failure.tag.as_deref(), &failure.error, &failure.message))?;
            let diff: DiffResult = decode(value)?;
            let slice = Parsed::new(&diff.patch, diff.truncated, None, None, None);
            let joined = join(&held, &slice, diff.truncated || diff.next_cursor.is_some());
            Ok::<_, String>((diff.next_cursor, slice, joined))
        });
        let generation = self.generation;
        self.pending = true;
        self.error = None;
        self.show(cx);
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.pending = false;
                match result {
                    Ok((next_cursor, slice, joined)) => {
                        this.slices.push(slice);
                        this.next_cursor = next_cursor;
                        this.show_parsed(joined, cx);
                        this.try_reveal(cx);
                    }
                    Err(error) => {
                        this.error = Some(error);
                        this.show(cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Reveals the file whose path the host's `digest` names, at `range` when given.
    pub fn reveal_diff(&mut self, digest: String, range: Option<Range>, cx: &mut Context<Self>) {
        self.reveal = Some((digest, range));
        self.try_reveal(cx);
    }

    /// Reveals a file directly by repo-relative path (used by `GuideTab` file jumps).
    pub fn reveal_file_path(&mut self, path: String, cx: &mut Context<Self>) {
        self.reveal_path = Some(path);
        self.try_reveal(cx);
    }

    fn try_reveal(&mut self, cx: &mut Context<Self>) {
        if let Some(wanted_path) = self.reveal_path.clone() {
            let found = self.slices.iter().flat_map(|slice| slice.files().iter()).any(|file| file.path() == wanted_path);
            if found {
                self.reveal_path = None;
                self.reveals += 1;
                let request = self.reveals;
                self.panel.update(cx, |panel, cx| panel.reveal_file_path(request, FileRevealTarget::file(wanted_path), cx));
            }
        }
        if let Some((digest, range)) = &self.reveal {
            let path = self
                .slices
                .iter()
                .flat_map(|slice| slice.files().iter())
                .map(|file| file.path().to_owned())
                .find(|path| diff_path_matches(digest, path));
            if let Some(path) = path {
                let target = match range {
                    Some(range) => {
                        let side = match range.side {
                            Side::Left => DiffSide::Deletions,
                            Side::Right => DiffSide::Additions,
                        };
                        FileRevealTarget::range(path, range.start.min(u32::MAX as u64) as u32, Some(range.end.min(u32::MAX as u64) as u32), Some(side), true)
                    }
                    None => FileRevealTarget::file(path),
                };
                self.reveal = None;
                self.reveals += 1;
                let request = self.reveals;
                self.panel.update(cx, |panel, cx| panel.reveal_file_path(request, target, cx));
            }
        }
        // If a pending path or anchor reveal is not in loaded slices yet, pull the next slice
        // automatically (`PullRequestCodeTab.tsx:1518-1521, 1545-1548`).
        if (self.reveal_path.is_some() || self.reveal.is_some()) && self.can_load_next() {
            self.load_next(cx);
        }
    }

    fn key(&self) -> String {
        match &self.commit {
            Some(commit) => format!("pull-request:{}:commit:{commit}", self.reference.key()),
            None => format!("pull-request:{}", self.reference.key()),
        }
    }

    fn show_parsed(&mut self, parsed: Arc<Parsed>, cx: &mut Context<Self>) {
        let key = self.key();
        self.panel.update(cx, |panel, cx| panel.show_external(External { key, parsed: Some(parsed), pending: false, error: None }, cx));
    }

    fn show(&mut self, cx: &mut Context<Self>) {
        if !self.slices.is_empty() && self.error.is_none() {
            return cx.notify();
        }
        let external = External { key: self.key(), parsed: None, pending: self.pending, error: self.error.clone() };
        self.panel.update(cx, |panel, cx| panel.show_external(external, cx));
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn diff_panel(&self) -> &Entity<DiffPanel> {
        &self.panel
    }

    #[cfg(test)]
    pub(crate) fn commit_review_active(&self) -> (bool, usize) {
        if self.commit.is_some() {
            (false, 0)
        } else {
            (self.inline_allowed, self.head_review_threads.len())
        }
    }
}

/// The held slices and a new one as one patch for the panel. Each slice keeps its own parse, so
/// a new slice costs one parse plus copies of the files before it, all off the UI thread.
fn join(held: &[Arc<Parsed>], slice: &Arc<Parsed>, truncated: bool) -> Arc<Parsed> {
    if held.is_empty() && slice.truncated == truncated {
        return slice.clone();
    }
    let slices = || held.iter().chain(std::iter::once(slice));
    let files: Vec<FileDiff> = slices().flat_map(|slice| slice.files().iter().cloned()).collect();
    let patch = if files.is_empty() {
        slices()
            .find_map(|slice| match &slice.patch {
                ParsedPatch::Raw { text, reason } => Some(ParsedPatch::Raw { text: text.clone(), reason }),
                _ => None,
            })
            .unwrap_or(ParsedPatch::Empty)
    } else {
        ParsedPatch::Files(files)
    };
    let max_line_len = slices().map(|slice| slice.max_line_len).max().unwrap_or(0);
    Arc::new(Parsed { patch, truncated, totals: None, base_ref: None, head_ref: None, max_line_len })
}

impl Render for CodeTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        // A slice that does not fill the panel leaves the end in view with nothing to scroll, so
        // the next one is read once this frame's rows are laid out.
        if self.can_load_next() && !self.near_end_check {
            self.near_end_check = true;
            let this = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = this.update(cx, |tab, cx| {
                    tab.near_end_check = false;
                    if tab.panel.read(cx).near_end(px(NEAR_END)) {
                        tab.load_next(cx);
                    }
                });
            });
        }
        let failed = self.error.is_some();
        let first = self.slices.is_empty();
        let partial = (!first && self.pending) || failed;
        let pending_count = self.detail_view.upgrade().map_or(0, |view| view.read(cx).pending_review_comment_count());
        let commit_banner = self.commit.as_deref().map(|sha| {
            let short = t3_core::pull_requests::detail::short_sha(sha).to_owned();
            h_flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .px(px(16.))
                .py(px(6.))
                .border_b_1()
                .border_color(hsla(p.border))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(format!("Showing commit {short}"))
                .child(Button::new("pr-code-all-commits", Variant::Ghost, Size::Xs).label("Show full diff").on_click(cx.listener(|this, _, _, cx| this.set_commit(None, cx))))
        });
        let review_banner = (pending_count > 0).then(|| {
            let detail_view = self.detail_view.clone();
            h_flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .px(px(16.))
                .py(px(6.))
                .border_b_1()
                .border_color(hsla(p.border))
                .bg(hsla(p.muted).opacity(0.6))
                .text_xs()
                .text_color(hsla(p.foreground))
                .child(h_flex().gap(px(6.)).child(Icon::MessageSquare.sized(px(13.)).text_color(hsla(p.primary))).child(format!("{pending_count} pending review {}", if pending_count == 1 { "comment" } else { "comments" })))
                .child(
                    Button::new("pr-code-finish-review", Variant::Default, Size::Xs)
                        .label("Finish review")
                        .on_click(move |_, _, cx| {
                            if let Some(view) = detail_view.upgrade() {
                                view.update(cx, |view, cx| view.open_review_form(cx));
                            }
                        }),
                )
        });
        v_flex()
            .size_full()
            .min_h_0()
            .children(commit_banner)
            .children(review_banner)
            .pt(px(12.))
            .child(div().flex_1().min_h_0().child(self.panel.clone()))
            .when(partial, |this| {
                this.child(
                    h_flex()
                        .flex_none()
                        .justify_center()
                        .gap(px(8.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(hsla(p.border))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .when(!failed, |this| this.child("Loading more files..."))
                        .when(failed, |this| {
                            let cursor = self.next_cursor.clone();
                            this.when(!first, |this| this.child("The rest of this diff could not be loaded.")).child(
                                Button::new("pr-code-retry", Variant::Outline, Size::Xs).label("Retry").on_click(cx.listener(move |this, _, _, cx| this.fetch(cursor.clone(), cx))),
                            )
                        }),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use t3_core::pull_requests::wire::{Activity, Detail, decode};

    use super::*;

    #[gpui::test]
    fn code_tab_reveal_paginates_into_next_slice_and_commit_scope_disables_head_review_state(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let detail: Detail = decode(json!({
            "provider": "github",
            "capabilities": {
                "actions": [], "mergeMethods": ["merge"],
                "edit": { "changeRequest": false, "comment": false },
                "review": { "resolve": true, "inlineComment": true, "rangeComment": true },
                "labels": false
            },
            "viewerPermissions": { "actions": [], "comment": true, "resolve": true, "labels": false },
            "projectId": "p1", "projectTitle": "Repo", "workspaceRoot": "/tmp/repo", "repository": "acme/app", "number": 42,
            "title": "PR", "body": "", "url": "https://github.com/acme/app/pull/42", "author": { "login": "alice" },
            "viewer": "alice", "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 2, "deletions": 0, "changedFiles": 2,
            "headBranch": "feat", "baseBranch": "main", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "observedAt": 100, "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
            "mergeCapabilities": { "merge": true, "squash": false, "rebase": false }
        }))
        .unwrap();
        let activity: Activity = decode(json!({
            "comments": [],
            "commentCount": 0,
            "commentsTruncated": false,
            "reviewThreads": [{
                "id": "rt1", "path": "src/slice1.rs", "line": 1, "side": "right", "isResolved": false, "isOutdated": false,
                "comments": [{ "id": "rc1", "author": { "login": "bob" }, "body": "Nit", "createdAt": "2026-01-02T01:00:00Z" }]
            }],
            "overviewThreads": [],
            "commits": []
        }))
        .unwrap();
        let reference = PrRef { project_id: "p1".into(), host: Some("github.com".into()), repository: "acme/app".into(), number: 42 };
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            let code = cx.new(|cx| CodeTab::new(state, WeakEntity::new_invalid(), reference, Some(&detail), Some(&activity), true, cx));

            // Full PR diff keeps inline comment drafting and PR-head review threads active.
            assert_eq!(code.read(cx).commit_review_active(), (true, 1));

            // Switching to a single commit disables inline drafting and clears head review threads.
            code.update(cx, |tab, cx| tab.set_commit(Some("deadbeef".into()), cx));
            assert_eq!(code.read(cx).commit_review_active(), (false, 0));

            // Returning to full PR diff restores both.
            code.update(cx, |tab, cx| tab.set_commit(None, cx));
            assert_eq!(code.read(cx).commit_review_active(), (true, 1));

            // Revealing a file in slice 2 triggers pagination to `next_cursor` instead of stalling.
            code.update(cx, |tab, cx| {
                let slice1 = Parsed::new("diff --git a/src/slice1.rs b/src/slice1.rs\n--- a/src/slice1.rs\n+++ b/src/slice1.rs\n@@ -1 +1 @@\n-a\n+b\n", false, None, None, None);
                tab.slices = vec![slice1];
                tab.next_cursor = Some("cursor-2".into());
                tab.pending = false;
                tab.error = None;
                tab.last_requested_cursor = None;

                tab.reveal_file_path("src/slice2.rs".into(), cx);
                assert_eq!(tab.last_requested_cursor.as_deref(), Some("cursor-2"));
                assert_eq!(tab.reveal_path.as_deref(), Some("src/slice2.rs"));

                // Once slice 2 arrives, `try_reveal` resolves the pending path and bumps `reveals`.
                let slice2 = Parsed::new("diff --git a/src/slice2.rs b/src/slice2.rs\n--- a/src/slice2.rs\n+++ b/src/slice2.rs\n@@ -1 +1 @@\n-x\n+y\n", false, None, None, None);
                tab.slices.push(slice2);
                tab.next_cursor = None;
                tab.error = None;
                tab.try_reveal(cx);
                assert!(tab.reveal_path.is_none());
                assert_eq!(tab.reveals, 1);
            });
        });
    }
}
