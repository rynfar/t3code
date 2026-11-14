//! Gutter selection and inline drafts from `AnnotatableCodeView.tsx`, Pierre's
//! `InteractionManager`, `DiffCommentAnnotation.tsx`, and `commentSubmitShortcut.ts`.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Duration,
};

use gpui::{
    AnyElement, App, Context, Div, Entity, IntoElement, MouseButton, MouseMoveEvent, ParentElement, Pixels, Point, Styled, Subscription, Task, Window,
    div, prelude::*, px,
};
use gpui_component::{
    h_flex,
    input::{Enter, Escape, IndentInline, InputEvent, MoveDown, MoveUp, Textarea, TextareaState},
    v_flex,
};
use t3_core::{
    composer::review::{self, CommentDictationState, ReviewComment, Selection, Side},
    diff::{
        CompletionItem, CompletionOptions, CompletionScope, CompletionSearchDecision,
        DiffLine, DiffRow, DiffScope, LineKind, PrCompletionState, ReferenceKind,
        ReferenceState, completion_query_cache_key, debounced_pull_request_completion_search,
        filter_options_params, insert_pull_request_completion, nudge_completion,
        pull_request_completions,
    },
    pull_requests::method,
};

use super::DiffPanel;
use crate::{
    app_state::uuid_v4,
    composer::controls::{Button, ButtonSize, ButtonVariant},
    icons::Icon,
    theme::{hsla, palette},
};

pub type ExternalCommentHandler = Rc<dyn Fn(ReviewComment, &mut App)>;
pub type CommentDictationControlsBuilder =
    Rc<dyn Fn(CommentDictationState, &mut Window, &mut Context<DiffPanel>) -> AnyElement>;
pub type CommentDictationCancelHandler = Rc<dyn Fn(&mut App) -> bool>;

/// Host-level hooks for inline diff comments that survive thread/scope switches (`DF-COMMENTS`).
#[derive(Default)]
pub(super) struct CommentsConfig {
    pub completion_scope: Option<CompletionScope>,
    pub completion_options: CompletionOptions,
    pub completion_cache: HashMap<String, CompletionOptions>,
    pub completion_last_options: HashMap<(String, char), CompletionOptions>,
    pub external_comment_handler: Option<ExternalCommentHandler>,
    pub dictation_state: CommentDictationState,
    pub dictation_controls: Option<CommentDictationControlsBuilder>,
    pub dictation_cancel_handler: Option<CommentDictationCancelHandler>,
}

impl CommentsConfig {
    pub(super) fn allows_external_comments(&self) -> bool {
        self.external_comment_handler.is_some()
    }
}

#[derive(Default)]
pub(super) struct Comments {
    pub scope: String,
    selected: Option<(String, Selection)>,
    dragging: bool,
    utility: bool,
    utility_range: Option<Selection>,
    highlight: HashSet<(Side, u32)>,
    pub editor: Option<Editor>,
    completion: PrCompletionState,
    completion_loading: bool,
    completion_error: Option<String>,
    completion_query_key: Option<String>,
    last_caret_snapshot: Option<(usize, usize)>,
    _completion_task: Option<Task<()>>,
}

pub(super) struct Editor {
    pub file_key: String,
    comment: ReviewComment,
    input: Entity<TextareaState>,
    _subscription: Subscription,
}

impl DiffPanel {
    pub(super) fn comment_gutter(
        &self,
        gutter: Div,
        file: usize,
        line: Option<&DiffLine>,
        new_side: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(line) = line.filter(|_| (self.external.is_none() || self.comments_config.allows_external_comments()) && self.comments.editor.is_none()) else {
            return gutter.into_any_element();
        };
        let side = if line.kind == LineKind::Deletion || !new_side { Side::Deletions } else { Side::Additions };
        let number = match side {
            Side::Additions => line.new_line,
            Side::Deletions => line.old_line,
        };
        let Some(number) = number else { return gutter.into_any_element() };
        let p = palette(cx);
        let selected = self.selected_range(file).is_some() && self.comments.highlight.contains(&(side, number));
        let pinned = self.selected_range(file).is_some()
            && self.comments.utility_range.is_some_and(|range| range.end == number && range.end_side == side);
        let show_utility = self.selected_range(file).is_none() || pinned;
        let group = format!("diff-gutter-{file}-{new_side}-{number}");
        gutter
            .id(group.clone())
            .group(group.clone())
            .cursor_pointer()
            .when(selected, |gutter| gutter.bg(hsla(p.primary).opacity(0.15)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    let previous = this.selected_range(file);
                    let selection = if event.modifiers.shift {
                        previous.and_then(|range| {
                            let view = this.view.as_ref()?;
                            view.parsed.files().get(file).map(|diff| review::extend_selection(diff, range, number, side, view.layout))
                        })
                    } else {
                        None
                    }
                    .unwrap_or(Selection { start: number, side, end: number, end_side: side });
                    this.select_range(file, selection);
                    this.comments.dragging = true;
                    this.comments.utility = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .when(show_utility, |gutter| {
                gutter.child(
                    div()
                        .id(format!("diff-comment-add-{file}-{new_side}-{number}"))
                        .absolute()
                        .left(px(4.))
                        .top_0()
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.))
                        .bg(hsla(p.primary))
                        .text_color(hsla(p.primary_foreground))
                        .invisible()
                        .when(pinned, |utility| utility.visible())
                        .group_hover(group, |style| style.visible())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                let range = this.selected_range(file).and(this.comments.utility_range).unwrap_or(Selection {
                                    start: number,
                                    side,
                                    end: number,
                                    end_side: side,
                                });
                                this.select_range(file, range);
                                this.comments.dragging = true;
                                this.comments.utility = true;
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )
                        .child(Icon::Plus.render(px(14.), hsla(p.primary_foreground))),
                )
            })
            .into_any_element()
    }

    fn selected_range(&self, file: usize) -> Option<Selection> {
        let (key, range) = self.comments.selected.as_ref()?;
        (self.view.as_ref()?.key(file) == Some(key)).then_some(*range)
    }

    fn select_range(&mut self, file: usize, range: Selection) {
        let Some(key) = self.view.as_ref().and_then(|view| view.key(file)) else { return };
        self.comments.selected = Some((key.to_owned(), range));
        self.comments.utility_range =
            self.view.as_ref().and_then(|view| review::gutter_selection(view.parsed.files().get(file)?, range, view.layout));
        self.comments.highlight.clear();
        if let Some(diff) = self.view.as_ref().and_then(|view| view.parsed.files().get(file))
            && let Some((start, end)) = review::selection_indexes(diff, range)
        {
            for line in diff.hunks.iter().flat_map(|hunk| &hunk.lines).skip(start.min(end)).take(start.abs_diff(end) + 1) {
                if let Some(n) = line.old_line {
                    self.comments.highlight.insert((Side::Deletions, n));
                }
                if let Some(n) = line.new_line {
                    self.comments.highlight.insert((Side::Additions, n));
                }
            }
        }
    }

    pub(super) fn drag_comment_selection(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button == Some(MouseButton::Left) {
            self.track_comment_pointer(event.position, cx);
        }
    }

    pub(super) fn track_comment_pointer(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.comments.dragging {
            return;
        }
        let viewport = self.list.viewport_bounds();
        if !viewport.contains(&position) || position.y < viewport.top() + px(32.) {
            return;
        }
        let Some(view) = &self.view else { return };
        for index in self.list.logical_scroll_top().item_ix..view.row_count() {
            let Some(bounds) = self.list.bounds_for_item(index) else { break };
            if bounds.top() > viewport.bottom() {
                break;
            }
            if !bounds.contains(&position) {
                continue;
            }
            let Some(row) = view.row(index) else { return };
            let file = row.file();
            let Some(diff) = view.parsed.files().get(file) else { return };
            let Some((number, side)) = review::row_selection_target(diff, row, position.x >= bounds.center().x) else { return };
            if let Some(mut range) = self.selected_range(file)
                && (range.end != number || range.end_side != side)
            {
                range.end = number;
                range.end_side = side;
                self.select_range(file, range);
                cx.notify();
            }
            return;
        }
    }

    pub(super) fn finish_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.comments.dragging {
            return;
        }
        self.comments.dragging = false;
        if !self.comments.utility {
            return;
        }
        let Some((key, selection)) = &self.comments.selected else { return };
        let Some(file) = self.view.as_ref().and_then(|view| (0..view.parsed.files().len()).find(|&file| view.key(file) == Some(key)))
        else {
            return;
        };
        self.open_comment(file, *selection, window, cx);
    }

    pub(super) fn open_comment(&mut self, file: usize, selection: Selection, window: &mut Window, cx: &mut Context<Self>) {
        let (section, title) = match &self.scope {
            DiffScope::Branch { .. } => ("branch".into(), "Changes".into()),
            DiffScope::Unstaged => ("unstaged".into(), "Uncommitted".into()),
            DiffScope::Turn { summary_index, .. } => {
                let summary = &self.summaries[*summary_index];
                (format!("turn:{}", summary.run_id), format!("Turn {}", summary.turn_count))
            }
            DiffScope::ToolCall(target) => (format!("tool-call:{}:{}", target.thread_id, target.item_id), "Tool call".into()),
            DiffScope::NoTurns => return,
        };
        let Some(diff) = self.view.as_ref().and_then(|view| view.parsed.files().get(file)) else { return };
        let Some(comment) = review::build_diff_comment(uuid_v4(), section, title, diff, selection, "") else { return };
        let Some(file_key) = self.view.as_ref().and_then(|view| view.key(file)).map(str::to_owned) else { return };
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder("Add a comment…"));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change | InputEvent::Focus => {
                this.sync_comment_completion(cx);
                cx.notify();
            }
            InputEvent::Blur => {
                if this.comments.completion.dismiss() {
                    this.comments.completion_loading = false;
                    this.comments._completion_task = None;
                    this.invalidate_comment_row();
                    cx.notify();
                }
            }
            InputEvent::PressEnter { .. } => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.comments.editor = Some(Editor { file_key, comment, input, _subscription: subscription });
        self.comments.completion = PrCompletionState::default();
        self.comments.completion_loading = false;
        self.comments.completion_error = None;
        self.comments.completion_query_key = None;
        self.comments.last_caret_snapshot = None;
        self.comments._completion_task = None;
        self.invalidate_comment_row();
        cx.notify();
    }

    fn invalidate_comment_row(&mut self) {
        let Some(editor) = &self.comments.editor else { return };
        let Some(selection) = editor.comment.selection else { return };
        let Some(view) = &self.view else { return };
        let row = (0..view.row_count()).find(|&index| match view.row(index) {
            Some(DiffRow::Line { file, hunk, line }) if view.key(file) == Some(editor.file_key.as_str()) => {
                self.line_for_anchor(file, hunk, Some(line), selection)
            }
            Some(DiffRow::Split { file, hunk, left, right }) if view.key(file) == Some(editor.file_key.as_str()) => {
                self.line_for_anchor(file, hunk, if selection.end_side == Side::Deletions { left } else { right }, selection)
            }
            _ => false,
        });
        if let Some(row) = row {
            self.list.remeasure_items(row..row + 1);
        }
    }

    fn cancel_comment(&mut self, cx: &mut Context<Self>) {
        self.invalidate_comment_row();
        self.comments.editor = None;
        self.comments.selected = None;
        self.reset_comment_completion();
        cx.notify();
    }

    fn reset_comment_completion(&mut self) {
        self.comments.completion = PrCompletionState::default();
        self.comments.completion_loading = false;
        self.comments.completion_error = None;
        self.comments.completion_query_key = None;
        self.comments._completion_task = None;
    }

    pub(super) fn submit_comment(&mut self, cx: &mut Context<Self>) {
        if self.comments_config.dictation_state.blocks_submission() {
            return;
        }
        let Some(editor) = &self.comments.editor else { return };
        let text = editor.input.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let Some(view) = &self.view else { return };
        let Some((_, diff)) = view.parsed.files().iter().enumerate().find(|(file, _)| view.key(*file) == Some(editor.file_key.as_str()))
        else {
            return;
        };
        let Some(selection) = editor.comment.selection else { return };
        let Some(comment) = review::build_diff_comment(
            editor.comment.id.clone(),
            editor.comment.section_id.clone(),
            editor.comment.section_title.clone(),
            diff,
            selection,
            &text,
        ) else {
            return;
        };
        if let Some(handler) = self.comments_config.external_comment_handler.clone() {
            handler(comment, cx);
            self.cancel_comment(cx);
            return;
        }
        let Some(thread) = self.thread_id.clone() else { return };
        self.state.update(cx, |state, cx| state.add_review_comment(&thread, comment, cx));
        self.cancel_comment(cx);
    }

    /// Configures PR `@user` / `#123` / `!123` completion scope for inline diff comment editors (`PR-*`).
    #[allow(dead_code)]
    pub fn set_completion_scope(&mut self, scope: Option<CompletionScope>, cx: &mut Context<Self>) {
        if self.comments_config.completion_scope == scope {
            return;
        }
        self.comments_config.completion_scope = scope;
        self.comments_config.completion_cache.clear();
        self.comments_config.completion_last_options.clear();
        self.reset_comment_completion();
        self.sync_comment_completion(cx);
        cx.notify();
    }

    /// Seeds or updates mention/reference candidates for inline diff comment editors (`PR-*`).
    #[allow(dead_code)]
    pub fn set_completion_options(&mut self, options: CompletionOptions, cx: &mut Context<Self>) {
        self.comments_config.completion_options = options;
        self.invalidate_comment_row();
        cx.notify();
    }

    /// Routes submitted inline diff comments to a host handler (e.g. a detached PR Diff tab).
    #[allow(dead_code)]
    pub fn set_external_comment_handler(
        &mut self,
        handler: Option<ExternalCommentHandler>,
        cx: &mut Context<Self>,
    ) {
        self.comments_config.external_comment_handler = handler;
        cx.notify();
    }

    /// Updates speech capture state (`Preparing` / `Active` freezes the editor and blocks send; `#376`).
    #[allow(dead_code)]
    pub fn set_comment_dictation_state(
        &mut self,
        state: CommentDictationState,
        cx: &mut Context<Self>,
    ) {
        if self.comments_config.dictation_state == state {
            return;
        }
        self.comments_config.dictation_state = state;
        cx.notify();
    }

    /// Registers the Phase C5 (`#376`) dictation controls renderer in the comment editor footer.
    #[allow(dead_code)]
    pub fn set_comment_dictation_controls(
        &mut self,
        builder: Option<CommentDictationControlsBuilder>,
        cx: &mut Context<Self>,
    ) {
        self.comments_config.dictation_controls = builder;
        self.invalidate_comment_row();
        cx.notify();
    }

    /// Registers an Escape-time hook to cancel active comment dictation before dismissing UI (`#376`).
    #[allow(dead_code)]
    pub fn set_comment_dictation_cancel_handler(
        &mut self,
        handler: Option<CommentDictationCancelHandler>,
        cx: &mut Context<Self>,
    ) {
        self.comments_config.dictation_cancel_handler = handler;
        cx.notify();
    }

    /// Reads the current inline diff comment draft (`readDraft` in `useTextFieldSpeech.tsx`; `#376`).
    #[allow(dead_code)]
    pub fn read_comment_draft(&self, cx: &App) -> Option<String> {
        Some(self.comments.editor.as_ref()?.input.read(cx).value().to_string())
    }

    /// Inserts recognized dictation transcript into the active diff comment editor (`#376`).
    #[allow(dead_code)]
    pub fn insert_comment_text(
        &mut self,
        transcript: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(editor) = &self.comments.editor else { return false };
        let current = editor.input.read(cx).value().to_string();
        let range = editor.input.read(cx).selected_range();
        let (next, caret) =
            review::insert_dictation_transcript(&current, range.start, range.end, transcript);
        self.commit_comment_draft(next, Some(caret), window, cx)
    }

    /// Commits a full draft replacement into the active diff comment editor (`commitDraft` hook; `#376`).
    #[allow(dead_code)]
    pub fn commit_comment_draft(
        &mut self,
        next: String,
        caret: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(editor) = &self.comments.editor else { return false };
        let end = caret.unwrap_or(next.len()).min(next.len());
        editor.input.update(cx, |input, cx| {
            let current_len = input.value().len();
            input.set_selected_range(0..current_len, cx);
            input.replace(next, window, cx);
            input.set_selected_range(end..end, cx);
        });
        self.sync_comment_completion(cx);
        self.invalidate_comment_row();
        cx.notify();
        true
    }

    fn sync_comment_completion_if_caret_changed(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.comments.editor else { return };
        let snapshot = {
            let input = editor.input.read(cx);
            (input.value().len(), input.cursor())
        };
        if self.comments.last_caret_snapshot != Some(snapshot) {
            self.sync_comment_completion(cx);
            cx.notify();
        }
    }

    fn sync_comment_completion(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.comments_config.completion_scope.clone() else {
            self.reset_comment_completion();
            return;
        };
        let Some(editor) = &self.comments.editor else {
            return;
        };
        let (value, caret) = {
            let input = editor.input.read(cx);
            (input.value().to_string(), input.cursor())
        };
        self.comments.last_caret_snapshot = Some((value.len(), caret));
        self.comments.completion.sync_input(&value, caret, &scope.support);

        let Some(trigger) = self.comments.completion.trigger.clone() else {
            self.comments.completion_loading = false;
            self.comments.completion_error = None;
            self.comments.completion_query_key = None;
            self.comments._completion_task = None;
            self.invalidate_comment_row();
            return;
        };

        let sigil = trigger_sigil(&trigger);
        let scope_key = scope.scope_key.clone();
        let key = completion_query_cache_key(&scope.scope_key, &trigger);
        if self.comments.completion_query_key.as_ref() == Some(&key) {
            self.invalidate_comment_row();
            return;
        }
        self.comments.completion_query_key = Some(key.clone());
        self.comments.completion_error = None;

        let cached = self.comments_config.completion_cache.contains_key(&key);
        let delay_ms = match debounced_pull_request_completion_search(cached, &trigger) {
            CompletionSearchDecision::Cached => {
                self.comments.completion_loading = false;
                self.comments._completion_task = None;
                self.invalidate_comment_row();
                return;
            }
            CompletionSearchDecision::Immediate => 0,
            CompletionSearchDecision::Debounce { delay_ms } => delay_ms,
        };

        let Some(client) = self.state.read(cx).client_handle() else {
            self.comments.completion_loading = false;
            self.comments._completion_task = None;
            self.invalidate_comment_row();
            return;
        };
        let runtime = self.state.read(cx).runtime().clone();
        let params = filter_options_params(&scope.project_id, &trigger);
        self.comments.completion_loading = true;
        self.invalidate_comment_row();

        self.comments._completion_task = Some(cx.spawn(async move |this, cx| {
            if delay_ms > 0 {
                cx.background_executor().timer(Duration::from_millis(delay_ms)).await;
            }
            let result = runtime
                .spawn(async move { client.request(method::FILTER_OPTIONS, params).await })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.comments.completion_query_key.as_ref() != Some(&key) {
                    return;
                }
                this.comments.completion_loading = false;
                match result {
                    Ok(Ok(value)) => {
                        let parsed = CompletionOptions::from_filter_options_value(&value);
                        this.comments_config
                            .completion_last_options
                            .insert((scope_key, sigil), parsed.clone());
                        this.comments_config.completion_cache.insert(key, parsed);
                        this.comments.completion_error = None;
                    }
                    Ok(Err(err)) => {
                        this.comments.completion_error = Some(format!("Completion failed: {err:?}"));
                    }
                    Err(err) => {
                        this.comments.completion_error = Some(err.to_string());
                    }
                }
                this.invalidate_comment_row();
                cx.notify();
            });
        }));
    }

    fn active_completion_options(&self) -> Option<&CompletionOptions> {
        let scope = self.comments_config.completion_scope.as_ref()?;
        let trigger = self.comments.completion.trigger.as_ref()?;
        let key = completion_query_cache_key(&scope.scope_key, trigger);
        let sigil = trigger_sigil(trigger);
        self.comments_config
            .completion_cache
            .get(&key)
            .or_else(|| {
                self.comments_config
                    .completion_last_options
                    .get(&(scope.scope_key.clone(), sigil))
            })
            .or_else(|| {
                (!self.comments_config.completion_options.authors.is_empty()
                    || !self.comments_config.completion_options.references.is_empty())
                .then_some(&self.comments_config.completion_options)
            })
    }

    pub(super) fn active_completion_items(&self) -> Vec<CompletionItem> {
        let Some(trigger) = self.comments.completion.trigger.as_ref() else {
            return Vec::new();
        };
        let Some(options) = self.active_completion_options() else {
            return Vec::new();
        };
        pull_request_completions(trigger, options)
    }

    fn nudge_completion(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let items = self.active_completion_items();
        if items.is_empty() {
            return false;
        }
        self.comments.completion.selected =
            nudge_completion(self.comments.completion.selected, delta, items.len());
        cx.notify();
        true
    }

    pub(super) fn accept_completion(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(trigger) = self.comments.completion.trigger.clone() else {
            return false;
        };
        let items = self.active_completion_items();
        if items.is_empty() {
            return false;
        }
        let ix = index.unwrap_or(self.comments.completion.selected).min(items.len() - 1);
        let insert = items[ix].insert_text();
        let Some(editor) = &self.comments.editor else { return false };
        let current = editor.input.read(cx).value().to_string();
        let (next, caret) = insert_pull_request_completion(&current, &trigger, &insert);
        editor.input.update(cx, |input, cx| {
            let current_len = input.value().len();
            input.set_selected_range(0..current_len, cx);
            input.replace(next, window, cx);
            input.set_selected_range(caret..caret, cx);
        });
        self.reset_comment_completion();
        self.sync_comment_completion(cx);
        self.invalidate_comment_row();
        cx.notify();
        true
    }

    pub(super) fn with_comment_editor(&self, row: Option<DiffRow>, code: AnyElement, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let code = self.with_saved_comments(row, code, cx);
        let Some(editor) = &self.comments.editor else { return code };
        let Some(selection) = editor.comment.selection else { return code };
        let Some(view) = &self.view else { return code };
        let anchor = match row {
            Some(DiffRow::Line { file, hunk, line }) if view.key(file) == Some(editor.file_key.as_str()) => {
                self.line_for_anchor(file, hunk, Some(line), selection)
            }
            Some(DiffRow::Split { file, hunk, left, right }) if view.key(file) == Some(editor.file_key.as_str()) => {
                self.line_for_anchor(file, hunk, if selection.end_side == Side::Deletions { left } else { right }, selection)
            }
            _ => false,
        };
        if !anchor {
            return code;
        }
        let p = palette(cx);
        let empty = editor.input.read(cx).value().trim().is_empty();
        let dictation_state = self.comments_config.dictation_state;
        let frozen = dictation_state.is_frozen();
        let trigger = self.comments.completion.trigger.as_ref();
        let completion_items = self.active_completion_items();
        let selected_ix = self.comments.completion.selected.min(completion_items.len().saturating_sub(1));
        let dictation_controls = self
            .comments_config
            .dictation_controls
            .as_ref()
            .map(|builder| builder(dictation_state, window, cx));

        v_flex()
            .w_full()
            .child(code)
            .child(
                v_flex()
                    .id("diff-comment-editor")
                    .w_full()
                    .px(px(12.))
                    .py(px(8.))
                    .gap(px(6.))
                    .bg(hsla(p.background))
                    .text_color(hsla(p.foreground))
                    .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                        if this
                            .comments_config
                            .dictation_cancel_handler
                            .as_ref()
                            .is_some_and(|cancel| cancel(cx))
                        {
                            cx.stop_propagation();
                            return;
                        }
                        if this.comments.completion.dismiss() {
                            this.comments.completion_loading = false;
                            this.comments._completion_task = None;
                            this.invalidate_comment_row();
                            cx.notify();
                            cx.stop_propagation();
                            return;
                        }
                        this.cancel_comment(cx);
                        cx.stop_propagation();
                    }))
                    .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                        if this.nudge_completion(-1, cx) {
                            cx.stop_propagation();
                        } else {
                            cx.propagate();
                        }
                    }))
                    .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                        if this.nudge_completion(1, cx) {
                            cx.stop_propagation();
                        } else {
                            cx.propagate();
                        }
                    }))
                    .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                        if this.accept_completion(None, window, cx) {
                            cx.stop_propagation();
                        } else {
                            cx.propagate();
                        }
                    }))
                    .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                        if action.secondary {
                            this.submit_comment(cx);
                            cx.stop_propagation();
                        } else if !action.shift && this.accept_completion(None, window, cx) {
                            cx.stop_propagation();
                        } else {
                            cx.propagate();
                        }
                    }))
                    .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        let Some(editor) = &this.comments.editor else { return };
                        let key = &event.keystroke;
                        if review::is_submit_shortcut_with_dictation(
                            &key.key,
                            key.modifiers.platform,
                            key.modifiers.control,
                            &editor.input.read(cx).value(),
                            this.comments_config.dictation_state,
                        ) {
                            this.submit_comment(cx);
                            cx.stop_propagation();
                            return;
                        }
                        let entity = cx.entity().downgrade();
                        cx.defer(move |cx| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(cx, |this, cx| {
                                    this.sync_comment_completion_if_caret_changed(cx);
                                });
                            }
                        });
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| {
                            let entity = cx.entity().downgrade();
                            cx.defer(move |cx| {
                                if let Some(entity) = entity.upgrade() {
                                    entity.update(cx, |this, cx| {
                                        this.sync_comment_completion_if_caret_changed(cx);
                                    });
                                }
                            });
                        }),
                    )
                    .children(trigger.map(|trigger| {
                        render_completion_popover(
                            trigger,
                            &completion_items,
                            selected_ix,
                            self.comments.completion_loading,
                            self.comments.completion_error.as_deref(),
                            &p,
                            cx,
                        )
                    }))
                    .child(
                        Textarea::new(&editor.input)
                            .aria_label(format!("Comment on lines {}", editor.comment.range_label))
                            .disabled(frozen),
                    )
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(10.))
                                    .text_color(hsla(p.muted_foreground).opacity(0.7))
                                    .child(if frozen {
                                        "Listening for comment dictation…"
                                    } else {
                                        "⌘/Ctrl Enter to send"
                                    }),
                            )
                            .children(dictation_controls)
                            .child(
                                Button::new("diff-comment-cancel", ButtonVariant::Ghost, ButtonSize::Xs)
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_comment(cx))),
                            )
                            .child(
                                Button::new("diff-comment-submit", ButtonVariant::Default, ButtonSize::Xs)
                                    .label("Comment")
                                    .disabled(empty || frozen)
                                    .on_click(cx.listener(|this, _, _, cx| this.submit_comment(cx))),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn with_saved_comments(&self, row: Option<DiffRow>, code: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let Some(thread) = &self.thread_id else { return code };
        let Some(view) = &self.view else { return code };
        let comments: Vec<_> = self
            .state
            .read(cx)
            .review_comments
            .get(thread)
            .into_iter()
            .flatten()
            .filter(|comment| {
                if view.scope_key != format!("{thread}:{}", comment.section_id) {
                    return false;
                }
                let Some(file) = view.parsed.files().get(row.map_or(usize::MAX, |row| row.file())) else { return false };
                if file.path() != comment.file_path {
                    return false;
                }
                let Some(selection) = comment.restore_selection(file) else { return false };
                match row {
                    Some(DiffRow::Line { file, hunk, line }) => self.line_for_anchor(file, hunk, Some(line), selection),
                    Some(DiffRow::Split { file, hunk, left, right }) => {
                        self.line_for_anchor(file, hunk, if selection.end_side == Side::Deletions { left } else { right }, selection)
                    }
                    _ => false,
                }
            })
            .cloned()
            .collect();
        if comments.is_empty() {
            return code;
        }
        let p = palette(cx);
        v_flex()
            .w_full()
            .child(code)
            .children(comments.into_iter().map(|comment| {
                let id = comment.context_id();
                let thread = thread.clone();
                h_flex()
                    .w_full()
                    .items_start()
                    .gap(px(10.))
                    .px(px(12.))
                    .py(px(10.))
                    .border_l_2()
                    .border_color(hsla(p.primary).opacity(0.55))
                    .bg(hsla(p.primary).opacity(0.045))
                    .child(Icon::MessageCircle.render(px(14.), hsla(p.primary).opacity(0.7)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(hsla(p.foreground))
                            .child(gpui::SharedString::from(comment.text)),
                    )
                    .child(
                        Button::new(format!("delete-diff-comment-{id}"), ButtonVariant::Ghost, ButtonSize::IconXs)
                            .icon(Icon::Trash2)
                            .tooltip("Delete comment")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.update(cx, |state, cx| state.remove_review_comment(&thread, id.clone(), cx))
                            })),
                    )
            }))
            .into_any_element()
    }

    fn line_for_anchor(&self, file: usize, hunk: usize, line: Option<usize>, selection: Selection) -> bool {
        let Some(line) = line.and_then(|line| self.view.as_ref()?.parsed.files().get(file)?.hunks.get(hunk)?.lines.get(line)) else {
            return false;
        };
        match selection.end_side {
            Side::Additions => line.new_line == Some(selection.end),
            Side::Deletions => line.old_line == Some(selection.end),
        }
    }
}

fn trigger_sigil(trigger: &t3_core::diff::PullRequestComposerTrigger) -> char {
    match trigger.kind {
        t3_core::diff::TriggerKind::Mention => '@',
        t3_core::diff::TriggerKind::Reference(sigil) => sigil.as_char(),
    }
}

fn completion_item_icon(item: &CompletionItem, p: &crate::theme::Palette) -> (Icon, gpui::Hsla) {
    match item {
        CompletionItem::Mention(author) => (
            if author.is_bot { Icon::Bot } else { Icon::UserRound },
            hsla(p.muted_foreground),
        ),
        CompletionItem::Reference { candidate, .. } => match (candidate.kind, candidate.state) {
            (ReferenceKind::Issue, ReferenceState::Open) => (Icon::CircleDot, hsla(p.success_foreground)),
            (ReferenceKind::Issue, ReferenceState::Closed | ReferenceState::Merged) => {
                (Icon::CircleCheck, hsla(p.muted_foreground))
            }
            (ReferenceKind::ChangeRequest, ReferenceState::Open) => {
                (Icon::GitPullRequest, hsla(p.success_foreground))
            }
            (ReferenceKind::ChangeRequest, ReferenceState::Merged) => {
                (Icon::GitMerge, hsla(p.primary))
            }
            (ReferenceKind::ChangeRequest, ReferenceState::Closed) => {
                (Icon::GitPullRequestClosed, hsla(p.destructive_foreground))
            }
        },
    }
}

fn render_completion_popover(
    trigger: &t3_core::diff::PullRequestComposerTrigger,
    items: &[CompletionItem],
    selected: usize,
    loading: bool,
    error: Option<&str>,
    p: &crate::theme::Palette,
    cx: &mut Context<DiffPanel>,
) -> AnyElement {
    let heading = match trigger.kind {
        t3_core::diff::TriggerKind::Mention => "Mention user",
        t3_core::diff::TriggerKind::Reference(_) => "Reference PR or issue",
    };

    v_flex()
        .id("diff-comment-completion-popover")
        .w_full()
        .max_w(px(420.))
        .p(px(4.))
        .gap(px(2.))
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.border))
        .bg(hsla(p.popover))
        .text_color(hsla(p.popover_foreground))
        .shadow_sm()
        .child(
            div()
                .px(px(8.))
                .py(px(2.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(heading),
        )
        .when(items.is_empty() && loading, |this| {
            this.child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .child("Searching…"),
            )
        })
        .when_some(error.filter(|_| items.is_empty()), |this, err| {
            this.child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .text_sm()
                    .text_color(hsla(p.error_foreground))
                    .child(gpui::SharedString::from(err.to_owned())),
            )
        })
        .when(items.is_empty() && !loading && error.is_none(), |this| {
            this.child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .child(match trigger.kind {
                        t3_core::diff::TriggerKind::Mention => "No matching people",
                        t3_core::diff::TriggerKind::Reference(_) => "No matching PRs or issues",
                    }),
            )
        })
        .children(items.iter().enumerate().map(|(ix, item)| {
            let active = ix == selected;
            let (icon, icon_color) = completion_item_icon(item, p);
            let (primary_label, secondary_label) = match item {
                CompletionItem::Mention(author) => (
                    author.login.clone(),
                    author
                        .name
                        .clone()
                        .filter(|name| name != &author.login)
                        .unwrap_or_default(),
                ),
                CompletionItem::Reference { candidate, .. } => {
                    (item.insert_text(), candidate.title.clone())
                }
            };
            let secondary_color = if active {
                hsla(p.accent_foreground).opacity(0.75)
            } else {
                hsla(p.muted_foreground)
            };
            h_flex()
                .id(("diff-comment-completion-item", ix))
                .w_full()
                .gap(px(8.))
                .px(px(8.))
                .py(px(5.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(active, |row| {
                    row.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground))
                })
                .hover(|row| row.bg(hsla(p.accent).opacity(0.6)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.accept_completion(Some(ix), window, cx);
                    cx.stop_propagation();
                }))
                .child(icon.render(px(14.), icon_color))
                .child(
                    div()
                        .flex_none()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(gpui::SharedString::from(primary_label)),
                )
                .when(!secondary_label.is_empty(), |row| {
                    row.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(secondary_color)
                            .child(gpui::SharedString::from(secondary_label)),
                    )
                })
        }))
        .into_any_element()
}
