//! The Code tab (`PullRequestCodeTab.tsx`): the patch from `POST /api/pull-requests/diff`, read in
//! slices (`nextCursor`) as the reader nears the end of what is loaded, each slice parsed once on
//! the network runtime (`sliceParses`) and the parses joined there for the diff panel. A link
//! naming a file's diff, by the host's digest of its path, reveals that file and its lines once
//! a loaded slice holds it (`useCodeViewFileReveal`).

use std::sync::Arc;

use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Task, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_core::composer::review::Side as DiffSide;
use t3_core::diff::{FileDiff, FileRevealTarget, ParsedPatch};
use t3_core::pull_requests::anchor::{Range, Side, diff_path_matches};
use t3_core::pull_requests::wire::{DiffResult, decode};
use t3_core::pull_requests::{PrRef, failure_message};

use super::controls::{Button, Size, Variant};
use crate::app_state::AppState;
use crate::diff::{DiffPanel, External, Parsed};
use crate::theme::{hsla, palette};

/// The sentinel's `rootMargin`: the next slice is read once the end is this close.
const NEAR_END: f32 = 240.;

pub struct CodeTab {
    state: Entity<AppState>,
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
    reveals: u64,
    _fetch: Option<Task<()>>,
}

impl CodeTab {
    pub fn new(state: Entity<AppState>, reference: PrRef, cx: &mut Context<Self>) -> Self {
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
        let mut tab = CodeTab { state, reference, panel, slices: Vec::new(), next_cursor: None, pending: false, error: None, generation: 0, near_end_check: false, commit: None, reveal: None, reveals: 0, _fetch: None };
        tab.fetch(None, cx);
        tab
    }

    pub fn set_commit(&mut self, commit: Option<String>, cx: &mut Context<Self>) {
        if self.commit != commit {
            self.commit = commit;
            self.reload(cx);
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.slices.clear();
        self.next_cursor = None;
        self.fetch(None, cx);
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

    fn try_reveal(&mut self, cx: &mut Context<Self>) {
        let Some((digest, range)) = &self.reveal else { return };
        let path = self.slices.iter().flat_map(|slice| slice.files().iter()).map(|file| file.path().to_owned()).find(|path| diff_path_matches(digest, path));
        let Some(path) = path else { return };
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
        v_flex()
            .size_full()
            .min_h_0()
            .children(commit_banner)
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
