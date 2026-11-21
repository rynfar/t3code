//! The Guide tab (`PullRequestGuideTab.tsx`, `guide/GuideTour.tsx`, `guide/GuideRail.tsx`,
//! `guide/GuideStop.tsx`, `guide/GuideExcerpt.tsx`): fetches `pullRequests.reviewGuide`,
//! triggers `pullRequests.generateReviewGuide`, renders the stop rail + stop walkthrough with
//! attention callouts, sliced diff excerpts, lettered notes (A/B/C), clickable file jumps to the
//! Code tab, review questions, and `j`/`k` stepping.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, ScrollHandle, SharedString, Styled, Task, WeakEntity,
    Window, div, prelude::*, px,
};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::auth::ORCHESTRATION_READ_SCOPE;
use t3_core::composer::review::{Selection, Side as DiffSide, build_diff_comment};
use t3_core::diff::{FileDiff, LineKind};
use t3_core::pull_requests::editing::allowed_review_verdicts;
use t3_core::pull_requests::guide::{TourProgress, excerpt_rows, letter_stop_notes, stop_subline};
use t3_core::pull_requests::review::{PendingReviewComment, ReviewPosition, pending_comment_from_diff};
use t3_core::pull_requests::wire::{DiffResult, ReviewGuide, ReviewGuideResult, ReviewGuideRun, ReviewGuideStop, decode};
use t3_core::pull_requests::{PrRef, failure_message, method};

use super::controls::{BadgeKind, Button, Size, Variant, badge};
use super::detail::DetailView;
use super::{destination_error_message, owner_for_pr, read_for_owner, settle};
use crate::app_state::{AppState, uuid_v4};
use crate::diff::highlight::{DiffHighlightCache, display_text};
use crate::icons::Icon;
use crate::selectable_text::SelectableText;
use crate::theme::{hsla, palette};
use crate::transcript::markdown::{MarkdownStore, prose};

const FAILURE_HINT: &str = "Check the review guide model in Settings → Source control.";
const MAX_DIFF_SLICES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
struct GuideCommentTarget {
    path: String,
    line: u32,
    side: DiffSide,
}

pub struct GuideTab {
    state: Entity<AppState>,
    detail_view: WeakEntity<DetailView>,
    environment_id: Option<String>,
    reference: PrRef,
    result: Option<ReviewGuideResult>,
    loading: bool,
    generating: bool,
    error: Option<String>,
    show_failure_details: bool,
    progress: TourProgress,
    diff_files: Vec<FileDiff>,
    highlighted_lines: RefCell<DiffHighlightCache>,
    diff_loaded_sha: Option<String>,
    diff_error: Option<String>,
    /// Files expanded inline under "Also in this stop" (`pingdotgg/t3code#38858`).
    expanded_files: HashSet<String>,
    /// Active inline review comment target inside an excerpt or expanded stop file (`pingdotgg/t3code#38858`).
    comment_target: Option<GuideCommentTarget>,
    comment_input: Option<Entity<TextareaState>>,
    focus: FocusHandle,
    needs_focus: bool,
    scroll: ScrollHandle,
    markdown: MarkdownStore,
    _fetch: Option<Task<()>>,
    _diff: Option<Task<()>>,
}

impl Focusable for GuideTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn wanted_guide_paths(guide: &ReviewGuide) -> HashSet<String> {
    guide
        .stops
        .iter()
        .flat_map(|stop| {
            stop.excerpts
                .iter()
                .map(|e| e.path.clone())
                .chain(stop.files.iter().map(|f| f.path.clone()))
        })
        .collect()
}

/// Paginates diff slices (`useGuideDiffFiles.ts:44-63`) until every excerpt path in `wanted` has
/// arrived or `next_cursor` is exhausted (bounded by `MAX_DIFF_SLICES`).
async fn fetch_guide_diff_slices<F, Fut>(wanted: &HashSet<String>, mut fetch_slice: F) -> Result<Vec<FileDiff>, String>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<DiffResult, String>>,
{
    let mut files = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_DIFF_SLICES {
        let diff = fetch_slice(cursor.clone()).await?;
        if let t3_core::diff::ParsedPatch::Files(parsed) = t3_core::diff::parse_patch(&diff.patch) {
            for file in parsed {
                if seen_paths.insert(file.path().to_owned()) {
                    files.push(file);
                }
            }
        }
        if !wanted.is_empty() && wanted.iter().all(|path| seen_paths.contains(path)) {
            break;
        }
        match diff.next_cursor {
            Some(next) if !next.is_empty() && Some(next.as_str()) != cursor.as_deref() => {
                cursor = Some(next);
            }
            _ => break,
        }
    }
    Ok(files)
}

impl GuideTab {
    pub fn new(
        state: Entity<AppState>,
        detail_view: WeakEntity<DetailView>,
        environment_id: Option<String>,
        reference: PrRef,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut tab = GuideTab {
            state,
            detail_view,
            environment_id,
            reference,
            result: None,
            loading: false,
            generating: false,
            error: None,
            show_failure_details: false,
            progress: TourProgress::default(),
            diff_files: Vec::new(),
            highlighted_lines: RefCell::default(),
            diff_loaded_sha: None,
            diff_error: None,
            expanded_files: HashSet::new(),
            comment_target: None,
            comment_input: None,
            focus: cx.focus_handle(),
            needs_focus: true,
            scroll: ScrollHandle::new(),
            markdown: MarkdownStore::new(cx.entity_id()),
            _fetch: None,
            _diff: None,
        };
        tab.reload(cx);
        tab
    }

    pub fn request_focus(&mut self) {
        self.needs_focus = true;
    }

    /// Applies a loaded or newly generated guide, clearing stale diff files whenever the guide's
    /// `head_sha` changes so a rewritten guide never shows code from a previous push (`pingdotgg/t3code#37864`).
    fn apply_guide_result(&mut self, result: ReviewGuideResult, cx: &mut Context<Self>) {
        let diff_target = result.guide.as_ref().map(|g| (g.head_sha.clone(), wanted_guide_paths(g)));
        let sha_changed = match (&diff_target, &self.diff_loaded_sha) {
            (Some((sha, _)), Some(loaded)) => sha != loaded,
            (None, Some(_)) => true,
            _ => false,
        };
        if sha_changed {
            self.diff_files.clear();
            self.diff_loaded_sha = None;
            self.diff_error = None;
            self.expanded_files.clear();
            self.comment_target = None;
        }
        self.result = Some(result);
        self.error = None;
        if let Some((sha, wanted)) = diff_target
            && self.diff_loaded_sha.as_deref() != Some(sha.as_str())
        {
            self.load_diff(sha, wanted, cx);
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let app = self.state.read(cx);
        let owner = owner_for_pr(app, self.environment_id.as_deref(), &self.reference.project_id);
        let task = read_for_owner(app, owner.as_ref(), method::REVIEW_GUIDE, self.reference.payload(), decode::<ReviewGuideResult>);
        self.loading = true;
        self.error = None;
        cx.notify();
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |tab, cx| {
                tab.loading = false;
                match res {
                    Ok(result) => tab.apply_guide_result(result, cx),
                    Err(err) => tab.error = Some(err),
                }
                cx.notify();
            });
        }));
    }

    fn load_diff(&mut self, head_sha: String, wanted: HashSet<String>, cx: &mut Context<Self>) {
        let app = self.state.read(cx);
        let owner = owner_for_pr(app, self.environment_id.as_deref(), &self.reference.project_id);
        let base_payload = self.reference.payload();
        let task = if let Some(owner) = owner {
            let dest_fut = app.connections.destination(&owner, ORCHESTRATION_READ_SCOPE);
            app.runtime().spawn(async move {
                let client = dest_fut.await.map_err(destination_error_message)?.client;
                fetch_guide_diff_slices(&wanted, move |cursor| {
                    let client = client.clone();
                    let mut payload = base_payload.clone();
                    if let Some(cursor) = cursor {
                        payload["cursor"] = serde_json::json!(cursor);
                    }
                    async move {
                        let value = client
                            .pull_request_diff(payload)
                            .await
                            .map_err(|failure| failure_message(failure.tag.as_deref(), &failure.error, &failure.message))?;
                        decode::<DiffResult>(value)
                    }
                })
                .await
            })
        } else {
            let Some(client) = app.client_handle() else { return };
            app.runtime().spawn(async move {
                fetch_guide_diff_slices(&wanted, move |cursor| {
                    let client = client.clone();
                    let mut payload = base_payload.clone();
                    if let Some(cursor) = cursor {
                        payload["cursor"] = serde_json::json!(cursor);
                    }
                    async move {
                        let value = client
                            .pull_request_diff(payload)
                            .await
                            .map_err(|failure| failure_message(failure.tag.as_deref(), &failure.error, &failure.message))?;
                        decode::<DiffResult>(value)
                    }
                })
                .await
            })
        };
        self._diff = Some(cx.spawn(async move |this, cx| {
            let res = task.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update(cx, |tab, cx| {
                match res {
                    Ok(files) => {
                        tab.diff_files = files;
                        tab.diff_loaded_sha = Some(head_sha);
                        tab.diff_error = None;
                    }
                    Err(err) => tab.diff_error = Some(err),
                }
                cx.notify();
            });
        }));
    }

    pub fn generate(&mut self, cx: &mut Context<Self>) {
        if self.generating {
            return;
        }
        let app = self.state.read(cx);
        let owner = owner_for_pr(app, self.environment_id.as_deref(), &self.reference.project_id);
        let task = read_for_owner(app, owner.as_ref(), method::GENERATE_REVIEW_GUIDE, self.reference.payload(), decode::<ReviewGuideResult>);
        self.generating = true;
        cx.notify();
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |tab, cx| {
                tab.generating = false;
                match res {
                    Ok(result) => tab.apply_guide_result(result, cx),
                    Err(err) => {
                        tab.state.update(cx, |state, cx| {
                            state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Error, "Could not start review guide").description(err), cx);
                        });
                    }
                }
                cx.notify();
            });
        }));
    }

    pub fn go_to(&mut self, index: usize, cx: &mut Context<Self>) {
        let count = self.result.as_ref().and_then(|r| r.guide.as_ref()).map_or(0, |g| g.stops.len());
        self.progress.go_to(index, count);
        self.scroll.scroll_to_top_of_item(0);
        cx.notify();
    }

    pub fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.result.as_ref().and_then(|r| r.guide.as_ref()).map_or(0, |g| g.stops.len());
        self.progress.step(delta, count);
        self.scroll.scroll_to_top_of_item(0);
        cx.notify();
    }

    /// Handles `j` / `k` keyboard tour stepping (`GuideTour.tsx:86-94`).
    pub fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.comment_target.is_some() {
            return;
        }
        let m = &event.keystroke.modifiers;
        if m.platform || m.control || m.alt || m.shift {
            return;
        }
        match event.keystroke.key.as_str() {
            "j" => self.step(1, cx),
            "k" => self.step(-1, cx),
            _ => {}
        }
    }

    pub(crate) fn can_comment_inline(&self, cx: &App) -> bool {
        self.detail_view.upgrade().is_some_and(|view| {
            let v = view.read(cx);
            v.can_write_source_control(cx)
                && v.detail.as_ref().is_some_and(|d| d.capabilities.review.inline_comment && d.viewer_permissions.comment)
        })
    }

    pub fn toggle_stop_file(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.expanded_files.remove(&path) {
            self.expanded_files.insert(path);
        }
        cx.notify();
    }

    pub fn open_line_comment(&mut self, path: String, line: u32, side: DiffSide, window: &mut Window, cx: &mut Context<Self>) {
        self.comment_target = Some(GuideCommentTarget { path, line, side });
        let input = match &self.comment_input {
            Some(existing) => existing.clone(),
            None => {
                let created = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder("Write an inline review comment…"));
                self.comment_input = Some(created.clone());
                created
            }
        };
        input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub fn cancel_line_comment(&mut self, cx: &mut Context<Self>) {
        if self.comment_target.take().is_some() {
            cx.notify();
        }
    }

    pub fn submit_line_comment(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.comment_target.clone() else { return };
        let Some(input) = self.comment_input.as_ref() else { return };
        let body = input.read(cx).value().trim().to_owned();
        if body.is_empty() {
            return;
        }
        let file = self.diff_files.iter().find(|f| f.path() == target.path);
        let range_comments = self
            .detail_view
            .upgrade()
            .and_then(|v| v.read(cx).detail.as_ref().and_then(|d| d.capabilities.review.range_comment))
            == Some(true);
        let selection = Selection {
            start: target.line,
            end: target.line,
            side: target.side,
            end_side: target.side,
        };
        let id = uuid_v4();
        let pending = file
            .and_then(|f| build_diff_comment(id.clone(), self.reference.key(), "Guide".into(), f, selection, &body))
            .and_then(|comment| pending_comment_from_diff(&comment, file, range_comments))
            .unwrap_or(PendingReviewComment {
                id,
                path: target.path.clone(),
                old_path: file.map(|f| f.previous_path().to_owned()).filter(|old| old != &target.path),
                position: match target.side {
                    DiffSide::Deletions => ReviewPosition::Deleted { old_line: target.line },
                    DiffSide::Additions => ReviewPosition::Added { new_line: target.line },
                },
                start_position: None,
                range_label: format!("L{}", target.line),
                body,
            });
        if let Some(detail_view) = self.detail_view.upgrade() {
            detail_view.update(cx, |view, cx| view.add_pending_review_comment(pending, cx));
        }
        self.comment_target = None;
        cx.notify();
    }

    fn pending_comments_for(&self, path: &str, cx: &App) -> Vec<PendingReviewComment> {
        self.detail_view
            .upgrade()
            .map(|view| {
                view.read(cx)
                    .review_draft
                    .comments
                    .iter()
                    .filter(|c| c.path == path)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn remove_pending_comment(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(view) = self.detail_view.upgrade() {
            view.update(cx, |view, cx| view.remove_pending_review_comment(&id, cx));
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn set_result_for_test(&mut self, result: ReviewGuideResult, diff_files: Vec<FileDiff>, cx: &mut Context<Self>) {
        self.diff_loaded_sha = result.guide.as_ref().map(|g| g.head_sha.clone());
        self.result = Some(result);
        self.diff_files = diff_files;
        self.loading = false;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn current_position(&self) -> usize {
        self.progress.position
    }

    fn can_write(&self, cx: &App) -> bool {
        self.detail_view.upgrade().is_some_and(|view| view.read(cx).can_write_source_control(cx))
    }

    /// Mirrors `PullRequestDetailPanel.tsx:1918-1924`: only offer `"Finish review"` when the viewer
    /// has write access and at least one review verdict is permitted on this pull request.
    pub(crate) fn can_finish_review(&self, cx: &App) -> bool {
        self.detail_view.upgrade().is_some_and(|view| {
            let v = view.read(cx);
            v.can_write_source_control(cx) && v.detail.as_ref().is_some_and(|detail| !allowed_review_verdicts(detail).is_empty())
        })
    }

    fn open_file(&self, path: String, cx: &mut Context<Self>) {
        if let Some(view) = self.detail_view.upgrade() {
            view.update(cx, |view, cx| view.open_code_file(path, cx));
        }
    }

    fn finish_review(&mut self, cx: &mut Context<Self>) {
        self.progress.finish_current();
        if let Some(view) = self.detail_view.upgrade() {
            view.update(cx, |view, cx| view.open_review_form(cx));
        }
        cx.notify();
    }

    fn render_empty(&self, running: bool, failure: Option<&str>, can_write: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        if self.loading && self.result.is_none() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p(px(24.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child("Loading review guide…")
                .into_any_element();
        }
        let heading = if running {
            "Writing a review guide"
        } else if failure.is_none() {
            "No review guide yet"
        } else {
            "Could not write a review guide"
        };
        let description = if running {
            "An agent is reading the change. A large one takes a few minutes. You can leave; you will be told when it is ready.".to_owned()
        } else if failure.is_none() {
            self.error.clone().unwrap_or_else(|| {
                "A guide is a tour of the change: stops through the code in reading order, with the calls only a maintainer can make.".to_owned()
            })
        } else {
            FAILURE_HINT.to_owned()
        };
        let button_label = if running {
            "Reading the change..."
        } else if failure.is_none() {
            "Write guide"
        } else {
            "Try again"
        };
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .px(px(24.))
            .py(px(32.))
            .text_center()
            .child(Icon::FileText.sized(px(22.)).text_color(hsla(p.muted_foreground).opacity(0.6)))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(heading))
            .child(div().max_w(px(400.)).text_xs().text_color(hsla(p.muted_foreground)).child(description))
            .when(can_write, |this| {
                this.child(
                    Button::new("pr-guide-write", Variant::Outline, Size::Sm)
                        .icon(Icon::Sparkles)
                        .label(button_label)
                        .busy(running)
                        .disabled(running)
                        .on_click(cx.listener(|this, _, _, cx| this.generate(cx))),
                )
            })
            .when_some(failure.filter(|_| !running).map(str::to_owned), |this, detail| {
                let open = self.show_failure_details;
                this.child(
                    v_flex()
                        .max_w(px(460.))
                        .gap(px(6.))
                        .items_center()
                        .child(
                            Button::new("pr-guide-failure-details", Variant::GhostMuted, Size::Xs)
                                .label(if open { "Hide technical details" } else { "Technical details" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_failure_details = !this.show_failure_details;
                                    cx.notify();
                                })),
                        )
                        .when(open, |this| {
                            this.child(
                                div()
                                    .w_full()
                                    .p(px(10.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(hsla(p.border))
                                    .bg(hsla(p.muted))
                                    .text_left()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(detail),
                            )
                        }),
                )
            })
            .into_any_element()
    }

    fn render_tour(&mut self, guide: &ReviewGuide, banner: Option<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let stop_count = guide.stops.len();
        let position = if stop_count == 0 { 0 } else { self.progress.position.min(stop_count - 1) };
        let rail = v_flex()
            .flex_none()
            .w(px(210.))
            .border_r_1()
            .border_color(hsla(p.border))
            .p(px(10.))
            .gap(px(4.))
            .child(div().px(px(6.)).pb(px(4.)).text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child(format!("Tour stops ({stop_count})")))
            .children(guide.stops.iter().enumerate().map(|(index, stop)| {
                let active = index == position;
                let seen = self.progress.seen.contains(&index);
                v_flex()
                    .id(SharedString::from(format!("pr-guide-stop-{index}")))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(active, |this| this.bg(hsla(p.accent)))
                    .when(!active, |this| this.hover(|style| style.bg(hsla(p.accent).opacity(0.6))))
                    .on_click(cx.listener(move |this, _, _, cx| this.go_to(index, cx)))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .min_w_0()
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.))
                                    .text_color(if seen { hsla(p.pill_completed) } else { hsla(p.muted_foreground) })
                                    .child(if seen { "✓".to_owned() } else { format!("{}.", index + 1) }),
                            )
                            .child(div().min_w_0().truncate().text_xs().font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL }).text_color(hsla(p.foreground)).child(stop.title.clone())),
                    )
                    .child(div().pl(px(16.)).text_size(px(10.)).text_color(hsla(p.muted_foreground)).child(stop_subline(stop)))
            }));

        let main = match guide.stops.get(position) {
            None => v_flex()
                .flex_1()
                .p(px(20.))
                .children(banner)
                .child(div().text_sm().text_color(hsla(p.muted_foreground)).child("This change has no files to tour."))
                .into_any_element(),
            Some(stop) => self.render_stop(stop, position, stop_count, guide.stops.get(position + 1).map(|s| s.title.clone()), banner, cx),
        };

        h_flex()
            .id("pr-guide-tour")
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus.focus(window, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.handle_key_down(event, cx)))
            .child(rail)
            .child(main)
            .into_any_element()
    }

    fn render_stop(
        &mut self,
        stop: &ReviewGuideStop,
        index: usize,
        stop_count: usize,
        next_title: Option<String>,
        banner: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.highlighted_lines.borrow_mut().begin_bounded_frame();
        let p = palette(cx);
        let notes = letter_stop_notes(stop);
        let summary_text: Arc<str> = Arc::from(stop.summary.as_str());
        let summary_md = self.markdown.state(&format!("guide-stop-{index}"), &summary_text, cx);
        let summary_el = prose(&summary_md, cx).into_any_element();
        let mono = cx.theme().mono_font_family.clone();

        let can_comment = self.can_comment_inline(cx);
        let excerpts: Vec<AnyElement> = stop
            .excerpts
            .iter()
            .enumerate()
            .map(|(ex_ix, excerpt)| {
                let file = self.diff_files.iter().find(|f| f.path() == excerpt.path);
                let rows = file.map(|f| excerpt_rows(f, excerpt)).unwrap_or_default();
                let line_markers = notes.by_excerpt.get(ex_ix).cloned().unwrap_or_default();
                let jump_path = excerpt.path.clone();
                let pending_for_file = self.pending_comments_for(&excerpt.path, cx);
                let side_for_excerpt = if excerpt.side == "old" { DiffSide::Deletions } else { DiffSide::Additions };
                v_flex()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hsla(p.border))
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .justify_between()
                            .gap(px(8.))
                            .px(px(10.))
                            .py(px(6.))
                            .bg(hsla(p.muted))
                            .border_b_1()
                            .border_color(hsla(p.border))
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap(px(6.))
                                    .child(Icon::FileDiff.sized(px(13.)).text_color(hsla(p.muted_foreground)))
                                    .child(div().min_w_0().truncate().font_family(mono.clone()).text_xs().text_color(hsla(p.foreground)).child(format!("{}:{}-{}", excerpt.path, excerpt.start, excerpt.end))),
                            )
                            .child(
                                Button::new(SharedString::from(format!("pr-guide-jump-ex-{index}-{ex_ix}")), Variant::Ghost, Size::Xs)
                                    .label("Open in Code")
                                    .on_click(cx.listener(move |this, _, _, cx| this.open_file(jump_path.clone(), cx))),
                            ),
                    )
                    .when(!excerpt.caption.trim().is_empty(), |this| {
                        this.child(div().px(px(10.)).py(px(6.)).text_xs().text_color(hsla(p.muted_foreground)).child(excerpt.caption.clone()))
                    })
                    .child(if rows.is_empty() {
                        div().px(px(10.)).py(px(8.)).text_xs().text_color(hsla(p.muted_foreground)).child("Open in Code to inspect this file.").into_any_element()
                    } else {
                        v_flex()
                            .bg(hsla(p.code_background))
                            .py(px(4.))
                            .children(rows.into_iter().enumerate().map(|(row_ix, row)| {
                                let line_no = if excerpt.side == "old" { row.old_line } else { row.new_line };
                                let markers = line_no.and_then(|n| line_markers.iter().find(|(line, _)| *line == n)).map(|(_, m)| m.clone()).unwrap_or_default();
                                let line_side = match row.kind {
                                    LineKind::Deletion => DiffSide::Deletions,
                                    LineKind::Addition => DiffSide::Additions,
                                    LineKind::Context => side_for_excerpt,
                                };
                                let id = format!("ex-{index}-{ex_ix}-{row_ix}");
                                let styled = file.and_then(|file| {
                                    let lines = &file.hunks.get(row.hunk)?.lines;
                                    let line = lines.iter().position(|line| line.old_line == row.old_line && line.new_line == row.new_line)?;
                                    self.highlighted_lines.borrow_mut().line(file, row.hunk, line, line_side == DiffSide::Deletions, cx)
                                });
                                let text_id = SharedString::from(id.clone());
                                let text = styled.map_or_else(
                                    || SelectableText::new(text_id.clone(), row.text),
                                    |(text, runs)| SelectableText::new(text_id.clone(), text).highlights(runs),
                                );
                                self.render_guide_diff_line(
                                    id,
                                    &excerpt.path,
                                    line_no,
                                    line_side,
                                    row.kind,
                                    text,
                                    markers,
                                    &pending_for_file,
                                    can_comment,
                                    &mono,
                                    cx,
                                )
                            }))
                            .into_any_element()
                    })
                    .into_any_element()
            })
            .collect();

        let files_section = (!stop.files.is_empty()).then(|| {
            let heading = if stop.excerpts.is_empty() { "Files in this stop" } else { "Also in this stop" };
            v_flex()
                .gap(px(6.))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child(heading))
                .children(stop.files.iter().enumerate().map(|(file_ix, file)| {
                    let path = file.path.clone();
                    let jump_path = file.path.clone();
                    let expanded = self.expanded_files.contains(&file.path);
                    let diff_file = expanded.then(|| self.diff_files.iter().find(|f| f.path() == file.path).cloned()).flatten();
                    let pending_for_file = if expanded { self.pending_comments_for(&file.path, cx) } else { Vec::new() };
                    v_flex()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(hsla(p.border).opacity(if expanded { 1.0 } else { 0.6 }))
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .id(SharedString::from(format!("pr-guide-file-{index}-{file_ix}")))
                                .gap(px(6.))
                                .px(px(8.))
                                .py(px(5.))
                                .cursor_pointer()
                                .when(expanded, |this| this.bg(hsla(p.muted)))
                                .hover(|style| style.bg(hsla(p.accent).opacity(0.6)))
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle_stop_file(path.clone(), cx)))
                                .child((if expanded { Icon::ChevronDown } else { Icon::ChevronRight }).sized(px(13.)).text_color(hsla(p.muted_foreground)))
                                .child(Icon::FileText.sized(px(13.)).text_color(hsla(p.muted_foreground)))
                                .child(div().font_family(mono.clone()).text_xs().text_color(hsla(p.foreground)).child(file.path.clone()))
                                .children(file.note.as_ref().filter(|n| !n.trim().is_empty()).map(|note| div().flex_1().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(note.clone())))
                                .when(file.note.as_ref().is_none_or(|n| n.trim().is_empty()), |this| this.child(div().flex_1()))
                                .child(
                                    Button::new(SharedString::from(format!("pr-guide-file-jump-{index}-{file_ix}")), Variant::Ghost, Size::Xs)
                                        .label("Open in Code")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.open_file(jump_path.clone(), cx);
                                        })),
                                ),
                        )
                        .when(expanded, |this| {
                            let body: AnyElement = match diff_file {
                                Some(df) if !df.hunks.is_empty() => {
                                    let mut line_els: Vec<AnyElement> = Vec::new();
                                    for (hunk_ix, hunk) in df.hunks.iter().enumerate() {
                                        for (line_ix, line) in hunk.lines.iter().enumerate() {
                                            let (line_no, side) = match line.kind {
                                                LineKind::Deletion => (line.old_line, DiffSide::Deletions),
                                                LineKind::Addition | LineKind::Context => (line.new_line.or(line.old_line), DiffSide::Additions),
                                            };
                                            let id = format!("file-{index}-{file_ix}-{hunk_ix}-{line_ix}");
                                            let text_id = SharedString::from(id.clone());
                                            let styled = self.highlighted_lines.borrow_mut().line(&df, hunk_ix, line_ix, side == DiffSide::Deletions, cx);
                                            let text = styled.map_or_else(
                                                || SelectableText::new(text_id.clone(), display_text(line).to_owned()),
                                                |(text, runs)| SelectableText::new(text_id.clone(), text).highlights(runs),
                                            );
                                            line_els.push(self.render_guide_diff_line(
                                                id,
                                                &file.path,
                                                line_no,
                                                side,
                                                line.kind,
                                                text,
                                                Vec::new(),
                                                &pending_for_file,
                                                can_comment,
                                                &mono,
                                                cx,
                                            ));
                                        }
                                    }
                                    v_flex().bg(hsla(p.code_background)).py(px(4.)).children(line_els).into_any_element()
                                }
                                Some(_) => div().px(px(10.)).py(px(8.)).text_xs().text_color(hsla(p.muted_foreground)).child("No text diff available here. Open in Code to inspect.").into_any_element(),
                                None => div().px(px(10.)).py(px(8.)).text_xs().text_color(hsla(p.muted_foreground)).child("Loading file diff…").into_any_element(),
                            };
                            this.child(body)
                        })
                }))
        });

        let notes_section = (!notes.lettered.is_empty()).then(|| {
            v_flex()
                .gap(px(6.))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Notes"))
                .children(notes.lettered.iter().enumerate().map(|(note_ix, entry)| {
                    let path = entry.note.path.clone();
                    v_flex()
                        .gap(px(2.))
                        .p(px(8.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .child(
                            h_flex()
                                .gap(px(6.))
                                .child(badge(entry.marker.clone(), BadgeKind::Info, false, cx))
                                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(entry.note.title.clone()))
                                .child(div().flex_1())
                                .child(
                                    Button::new(SharedString::from(format!("pr-guide-note-jump-{index}-{note_ix}")), Variant::GhostMuted, Size::Xs)
                                        .label(format!("{}:{}", entry.note.path, entry.note.line))
                                        .on_click(cx.listener(move |this, _, _, cx| this.open_file(path.clone(), cx))),
                                ),
                        )
                        .when(!entry.note.body.trim().is_empty(), |this| {
                            this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(entry.note.body.clone()))
                        })
                }))
        });

        let questions_section = (!stop.questions.is_empty()).then(|| {
            v_flex()
                .gap(px(4.))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Questions to verify"))
                .children(stop.questions.iter().map(|question| {
                    h_flex()
                        .gap(px(6.))
                        .items_start()
                        .text_xs()
                        .text_color(hsla(p.foreground))
                        .child(div().text_color(hsla(p.muted_foreground)).child("•"))
                        .child(div().flex_1().child(question.clone()))
                }))
        });

        let is_last = index + 1 >= stop_count;
        let can_finish = self.can_finish_review(cx);
        let next_control: Option<AnyElement> = if is_last {
            can_finish.then(|| {
                Button::new("pr-guide-finish", Variant::Default, Size::Xs)
                    .icon(Icon::Check)
                    .label("Finish review")
                    .on_click(cx.listener(|this, _, _, cx| this.finish_review(cx)))
                    .into_any_element()
            })
        } else {
            let next_label = match next_title {
                Some(title) => format!("Next: {title}"),
                None => "Next stop".to_owned(),
            };
            Some(
                Button::new("pr-guide-next", Variant::Outline, Size::Xs)
                    .icon(Icon::ChevronRight)
                    .label(next_label)
                    .on_click(cx.listener(|this, _, _, cx| this.step(1, cx)))
                    .into_any_element(),
            )
        };
        let nav_footer = h_flex()
            .pt(px(12.))
            .border_t_1()
            .border_color(hsla(p.border))
            .justify_between()
            .child(
                Button::new("pr-guide-prev", Variant::Outline, Size::Xs)
                    .icon(Icon::ChevronLeft)
                    .label("Previous")
                    .disabled(index == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.step(-1, cx))),
            )
            .children(next_control);

        v_flex()
            .id("pr-guide-main")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(16.))
            .gap(px(12.))
            .children(banner)
            .children(self.diff_error.as_ref().map(|err| {
                div().text_xs().text_color(hsla(p.muted_foreground)).child(format!("The diff could not be loaded, so excerpts are missing: {err}"))
            }))
            .child(
                v_flex()
                    .gap(px(4.))
                    .child(div().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(format!("Stop {} of {stop_count} · Press j/k to step", index + 1)))
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(stop.title.clone())),
            )
            .children(stop.attention.as_ref().map(|attention| {
                h_flex()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hsla(p.warning).opacity(0.35))
                    .bg(hsla(p.warning_surface))
                    .text_xs()
                    .child(badge(attention.kind.clone(), BadgeKind::Outline, false, cx))
                    .child(div().flex_1().text_color(hsla(p.foreground)).child(attention.note.clone()))
            }))
            .child(summary_el)
            .children(excerpts)
            .children(notes_section)
            .children(files_section)
            .children(questions_section)
            .child(nav_footer)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_guide_diff_line(
        &self,
        id_prefix: String,
        path: &str,
        line_no: Option<u32>,
        side: DiffSide,
        kind: LineKind,
        text: SelectableText,
        markers: Vec<t3_core::pull_requests::guide::ExcerptLineMarker>,
        pending_for_file: &[PendingReviewComment],
        can_comment: bool,
        mono: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let (prefix, row_bg) = match kind {
            LineKind::Addition => ("+", Some(hsla(p.diff_row_addition))),
            LineKind::Deletion => ("-", Some(hsla(p.diff_row_deletion))),
            LineKind::Context => (" ", None),
        };
        let matching_pending: Vec<&PendingReviewComment> = line_no
            .map(|n| {
                pending_for_file
                    .iter()
                    .filter(|c| match &c.position {
                        ReviewPosition::Added { new_line } => side == DiffSide::Additions && *new_line == n,
                        ReviewPosition::Deleted { old_line } => side == DiffSide::Deletions && *old_line == n,
                        ReviewPosition::Context { old_line, new_line, side: s } => {
                            if side == DiffSide::Deletions {
                                s == "left" && *old_line == n
                            } else {
                                s != "left" && *new_line == n
                            }
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let composer_open = line_no.is_some_and(|n| {
            self.comment_target
                .as_ref()
                .is_some_and(|t| t.path == path && t.line == n && t.side == side)
        });
        let comment_btn = (can_comment && line_no.is_some()).then(|| {
            let target_path = path.to_owned();
            let target_line = line_no.unwrap_or(1);
            Button::new(SharedString::from(format!("pr-guide-comment-{id_prefix}")), Variant::GhostMuted, Size::Xs)
                .icon(Icon::Plus)
                .tooltip("Add review comment on this line")
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_line_comment(target_path.clone(), target_line, side, window, cx);
                }))
        });
        let line_row = h_flex()
            .px(px(8.))
            .gap(px(6.))
            .when_some(row_bg, |this, bg| this.bg(bg))
            .font_family(mono.clone())
            .text_size(px(12.))
            .line_height(px(18.))
            .child(div().w(px(36.)).flex_none().text_color(hsla(p.muted_foreground)).child(line_no.map_or(String::new(), |n| n.to_string())))
            .children(comment_btn)
            .child(div().flex_none().text_color(hsla(p.muted_foreground)).child(prefix))
            .child(div().flex_1().min_w_0().text_color(hsla(p.code_foreground)).child(text))
            .children(markers.into_iter().map(|m| badge(m.marker, BadgeKind::Info, false, cx)));

        if matching_pending.is_empty() && !composer_open {
            return line_row.into_any_element();
        }
        v_flex()
            .child(line_row)
            .children(matching_pending.into_iter().enumerate().map(|(c_ix, comment)| {
                let id = comment.id.clone();
                h_flex()
                    .mx(px(12.))
                    .my(px(4.))
                    .px(px(8.))
                    .py(px(6.))
                    .gap(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(hsla(p.primary).opacity(0.4))
                    .bg(hsla(p.muted))
                    .text_xs()
                    .child(badge("Pending", BadgeKind::Info, false, cx))
                    .child(div().flex_1().min_w_0().text_color(hsla(p.foreground)).child(comment.body.clone()))
                    .child(
                        Button::new(SharedString::from(format!("pr-guide-rm-{id_prefix}-{c_ix}")), Variant::GhostMuted, Size::Xs)
                            .icon(Icon::X)
                            .tooltip("Remove pending comment")
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_pending_comment(id.clone(), cx))),
                    )
            }))
            .when(composer_open, |this| {
                let Some(input) = self.comment_input.clone() else { return this };
                this.child(
                    v_flex()
                        .mx(px(12.))
                        .my(px(6.))
                        .p(px(8.))
                        .gap(px(6.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.background))
                        .child(Textarea::new(&input))
                        .child(
                            h_flex()
                                .justify_end()
                                .gap(px(6.))
                                .child(
                                    Button::new(SharedString::from(format!("pr-guide-cancel-{id_prefix}")), Variant::Ghost, Size::Xs)
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| this.cancel_line_comment(cx))),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("pr-guide-add-{id_prefix}")), Variant::Default, Size::Xs)
                                        .label("Add review comment")
                                        .on_click(cx.listener(|this, _, _, cx| this.submit_line_comment(cx))),
                                ),
                        ),
                )
            })
            .into_any_element()
    }
}

impl Render for GuideTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.markdown.begin_frame();
        if self.needs_focus {
            self.needs_focus = false;
            self.focus.focus(window, cx);
        }
        let p = palette(cx);
        let can_write = self.can_write(cx);
        let data = self.result.clone();
        let run = data.as_ref().and_then(|d| d.run.as_ref());
        let current_sha = data.as_ref().and_then(|d| d.current_head_sha.as_deref());
        let running = self.generating || matches!(run, Some(ReviewGuideRun::Running { .. }));
        let failure = match run {
            Some(ReviewGuideRun::Failed { head_sha, detail, .. }) if current_sha.is_none_or(|sha| sha == head_sha) => Some(detail.as_str()),
            _ => None,
        };
        let Some(guide) = data.as_ref().and_then(|d| d.guide.as_ref()) else {
            return self.render_empty(running, failure, can_write, cx);
        };
        let stale = current_sha.is_some_and(|sha| sha != guide.head_sha);
        let banner = (stale || failure.is_some() || running).then(|| {
            let text = if running {
                "Rewriting this guide. You will be told when the new one is ready.".to_owned()
            } else if let Some(detail) = failure {
                format!("Could not rewrite this guide. {FAILURE_HINT} ({detail})")
            } else {
                "Written before the latest push.".to_owned()
            };
            h_flex()
                .justify_between()
                .gap(px(8.))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(p.warning).opacity(0.35))
                .bg(hsla(p.warning_surface))
                .text_xs()
                .child(div().flex_1().child(text))
                .when(!running && can_write, |this| {
                    this.child(Button::new("pr-guide-rewrite", Variant::Outline, Size::Xs).label("Rewrite").on_click(cx.listener(|this, _, _, cx| this.generate(cx))))
                })
                .into_any_element()
        });
        self.render_tour(guide, banner, cx)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use t3_core::pull_requests::wire::{Detail, DiffResult, ReviewGuideResult, decode};

    use super::*;

    fn sample_guide_result() -> ReviewGuideResult {
        decode(json!({
            "guide": {
                "headSha": "abc1234",
                "generatedAt": "2026-01-02T02:00:00Z",
                "stops": [
                    {
                        "title": "Stop one",
                        "summary": "Core logic",
                        "attention": null,
                        "files": [{ "path": "src/a.rs" }],
                        "excerpts": [{ "path": "src/a.rs", "side": "new", "start": 1, "end": 1, "caption": "a" }],
                        "notes": [],
                        "questions": []
                    },
                    {
                        "title": "Stop two",
                        "summary": "Second slice file",
                        "attention": null,
                        "files": [{ "path": "src/b.rs" }],
                        "excerpts": [{ "path": "src/b.rs", "side": "new", "start": 1, "end": 1, "caption": "b" }],
                        "notes": [],
                        "questions": []
                    }
                ]
            },
            "run": null,
            "currentHeadSha": "abc1234"
        }))
        .unwrap()
    }

    #[gpui::test]
    fn guide_tab_focuses_on_open_steps_on_real_jk_keystrokes_and_gates_finish_review(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let reference = PrRef { project_id: "p1".into(), host: Some("github.com".into()), repository: "acme/app".into(), number: 42 };
        let readonly_detail: Detail = decode(json!({
            "provider": "github",
            "capabilities": {
                "actions": [], "mergeMethods": ["merge"],
                "edit": { "changeRequest": false, "comment": false },
                "review": { "resolve": false, "verdicts": [] },
                "labels": false
            },
            "viewerPermissions": { "actions": [], "comment": false, "resolve": false, "labels": false },
            "projectId": "p1", "projectTitle": "Repo", "workspaceRoot": "/tmp/repo", "repository": "acme/app", "number": 42,
            "title": "PR", "body": "", "url": "https://github.com/acme/app/pull/42", "author": { "login": "alice" },
            "viewer": "alice", "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 1, "deletions": 0, "changedFiles": 1,
            "headBranch": "feat", "baseBranch": "main", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "observedAt": 100, "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
            "mergeCapabilities": { "merge": true, "squash": false, "rebase": false }
        }))
        .unwrap();
        let (state, detail_view) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            let detail_view = cx.new(|cx| DetailView::new(state.clone(), WeakEntity::new_invalid(), reference.clone(), None, cx));
            detail_view.update(cx, |v, _| v.detail = Some(readonly_detail));
            (state, detail_view)
        });
        let (guide, cx) = cx.add_window_view(|_window, cx| GuideTab::new(state.clone(), detail_view.downgrade(), None, reference.clone(), cx));
        guide.update(cx, |guide, cx| {
            guide.set_result_for_test(sample_guide_result(), Vec::new(), cx);
            // Viewer with no allowed review verdicts cannot finish review.
            assert!(!guide.can_finish_review(cx));
        });
        cx.run_until_parked();

        // Dispatch real GPUI j/k keystrokes through the focused GuideTab window.
        assert_eq!(guide.read_with(cx, |g, _| g.current_position()), 0);
        cx.simulate_keystrokes("j");
        assert_eq!(guide.read_with(cx, |g, _| g.current_position()), 1);
        cx.simulate_keystrokes("k");
        assert_eq!(guide.read_with(cx, |g, _| g.current_position()), 0);
    }

    #[test]
    fn guide_diff_pagination_fetches_multiple_slices_until_all_wanted_paths_arrive() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let guide = sample_guide_result().guide.unwrap();
        let wanted = wanted_guide_paths(&guide);
        assert_eq!(wanted.len(), 2);

        let mut seen_cursors: Vec<Option<String>> = Vec::new();
        let files = runtime
            .block_on(fetch_guide_diff_slices(&wanted, |cursor| {
                seen_cursors.push(cursor.clone());
                let slice = match cursor.as_deref() {
                    None => DiffResult {
                        patch: "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n".into(),
                        next_cursor: Some("slice-2".into()),
                        truncated: false,
                        omitted_file_stats: Vec::new(),
                        withheld_files_readable: None,
                    },
                    Some("slice-2") => DiffResult {
                        patch: "diff --git a/src/b.rs b/src/b.rs\n--- a/src/b.rs\n+++ b/src/b.rs\n@@ -1 +1 @@\n-old2\n+new2\n".into(),
                        next_cursor: Some("slice-3".into()),
                        truncated: false,
                        omitted_file_stats: Vec::new(),
                        withheld_files_readable: None,
                    },
                    _ => panic!("should stop paginating once all wanted excerpt paths have arrived"),
                };
                async move { Ok(slice) }
            }))
            .unwrap();

        assert_eq!(seen_cursors, vec![None, Some("slice-2".into())]);
        let paths: Vec<&str> = files.iter().map(|f| f.path()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/b.rs"]);
    }

    #[gpui::test]
    fn guide_also_in_this_stop_inline_comment_and_rewritten_head_sha_clears_stale_diff(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let reference = PrRef { project_id: "p1".into(), host: Some("github.com".into()), repository: "acme/app".into(), number: 42 };
        let writable_detail: Detail = decode(json!({
            "provider": "github",
            "capabilities": {
                "actions": [], "mergeMethods": ["merge"],
                "edit": { "changeRequest": false, "comment": false },
                "review": { "resolve": true, "inlineComment": true, "rangeComment": true, "verdicts": ["comment"] },
                "labels": false
            },
            "viewerPermissions": { "actions": [], "comment": true, "resolve": true, "labels": false },
            "projectId": "p1", "projectTitle": "Repo", "workspaceRoot": "/tmp/repo", "repository": "acme/app", "number": 42,
            "title": "PR", "body": "", "url": "https://github.com/acme/app/pull/42", "author": { "login": "alice" },
            "viewer": "alice", "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 1, "deletions": 0, "changedFiles": 1,
            "headBranch": "feat", "baseBranch": "main", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "observedAt": 100, "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
            "mergeCapabilities": { "merge": true, "squash": false, "rebase": false }
        }))
        .unwrap();
        let (state, detail_view) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            let detail_view = cx.new(|cx| DetailView::new(state.clone(), WeakEntity::new_invalid(), reference.clone(), None, cx));
            detail_view.update(cx, |v, _| v.detail = Some(writable_detail));
            (state, detail_view)
        });
        let (guide, vcx) = cx.add_window_view(|_window, cx| GuideTab::new(state.clone(), detail_view.downgrade(), None, reference.clone(), cx));
        let t3_core::diff::ParsedPatch::Files(files) = t3_core::diff::parse_patch(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        ) else {
            panic!("files");
        };
        guide.update_in(vcx, |guide, window, cx| {
            guide.set_result_for_test(sample_guide_result(), files, cx);
            assert!(guide.can_comment_inline(cx));
            guide.toggle_stop_file("src/a.rs".into(), cx);
            assert!(guide.expanded_files.contains("src/a.rs"));
            guide.open_line_comment("src/a.rs".into(), 1, DiffSide::Additions, window, cx);
            guide.comment_input.as_ref().unwrap().update(cx, |input, cx| {
                input.set_value("Check edge case here", window, cx);
            });
            guide.submit_line_comment(cx);
            assert!(guide.comment_target.is_none());
        });
        assert_eq!(detail_view.read_with(vcx, |v, _| v.pending_review_comment_count()), 1);

        // Rewritten guide for a new head_sha must clear stale diff_files immediately (`pingdotgg/t3code#37864`).
        let rewritten: ReviewGuideResult = decode(json!({
            "guide": {
                "headSha": "def5678",
                "generatedAt": "2026-01-03T02:00:00Z",
                "stops": []
            },
            "run": null,
            "currentHeadSha": "def5678"
        }))
        .unwrap();
        guide.update(vcx, |guide, cx| {
            guide.apply_guide_result(rewritten, cx);
            assert!(guide.diff_files.is_empty());
            assert!(guide.diff_loaded_sha.is_none());
            assert!(guide.expanded_files.is_empty());
        });
    }
}
