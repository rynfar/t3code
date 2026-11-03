//! Gutter selection and inline drafts from `AnnotatableCodeView.tsx`, Pierre's
//! `InteractionManager`, `DiffCommentAnnotation.tsx`, and `commentSubmitShortcut.ts`.

use std::collections::HashSet;

use gpui::{AnyElement, Context, Div, Entity, IntoElement, MouseButton, ParentElement, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    h_flex,
    input::{Enter, Escape, InputEvent, Textarea, TextareaState},
    v_flex,
};
use t3_core::{
    composer::review::{self, ReviewComment, Selection, Side},
    diff::{DiffLine, DiffRow, DiffScope, LineKind},
};

use super::DiffPanel;
use crate::{
    app_state::uuid_v4,
    composer::controls::{Button, ButtonSize, ButtonVariant},
    icons::Icon,
    theme::{hsla, palette},
};

#[derive(Default)]
pub(super) struct Comments {
    pub scope: String,
    selected: Option<(String, Selection)>,
    dragging: bool,
    utility: bool,
    highlight: HashSet<(Side, u32)>,
    pub editor: Option<Editor>,
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
        let Some(line) = line.filter(|_| self.external.is_none() && self.comments.editor.is_none()) else {
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
                            this.view.as_ref()?.parsed.files().get(file).map(|diff| review::extend_selection(diff, range, number, side))
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
            .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                if !this.comments.dragging || event.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                if let Some(mut range) = this.selected_range(file)
                    && (range.end != number || range.end_side != side)
                {
                    range.end = number;
                    range.end_side = side;
                    this.select_range(file, range);
                    cx.notify();
                }
            }))
            .child(
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
                    .group_hover(group, |style| style.visible())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            if this.selected_range(file).is_none() {
                                this.select_range(file, Selection { start: number, side, end: number, end_side: side });
                            }
                            this.comments.dragging = true;
                            this.comments.utility = true;
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(Icon::Plus.render(px(14.), hsla(p.primary_foreground))),
            )
            .into_any_element()
    }

    fn selected_range(&self, file: usize) -> Option<Selection> {
        let (key, range) = self.comments.selected.as_ref()?;
        (self.view.as_ref()?.key(file) == Some(key)).then_some(*range)
    }

    fn select_range(&mut self, file: usize, range: Selection) {
        let Some(key) = self.view.as_ref().and_then(|view| view.key(file)) else { return };
        self.comments.selected = Some((key.to_owned(), range));
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

    fn open_comment(&mut self, file: usize, selection: Selection, window: &mut Window, cx: &mut Context<Self>) {
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
        let file_key = diff.key();
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder("Add a comment…"));
        let subscription = cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.comments.editor = Some(Editor { file_key, comment, input, _subscription: subscription });
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
            self.list.splice(row..row + 1, 1);
        }
    }

    fn cancel_comment(&mut self, cx: &mut Context<Self>) {
        self.invalidate_comment_row();
        self.comments.editor = None;
        self.comments.selected = None;
        cx.notify();
    }

    fn submit_comment(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.comments.editor else { return };
        let text = editor.input.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let Some(thread) = self.thread_id.clone() else { return };
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
        self.state.update(cx, |state, cx| state.add_review_comment(&thread, comment, cx));
        self.cancel_comment(cx);
    }

    pub(super) fn with_comment_editor(&self, row: Option<DiffRow>, code: AnyElement, cx: &mut Context<Self>) -> AnyElement {
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
                        this.cancel_comment(cx);
                        cx.stop_propagation();
                    }))
                    .capture_action(cx.listener(|this, action: &Enter, _, cx| {
                        if action.secondary {
                            this.submit_comment(cx);
                            cx.stop_propagation();
                        } else {
                            cx.propagate();
                        }
                    }))
                    .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        let Some(editor) = &this.comments.editor else { return };
                        let key = &event.keystroke;
                        if review::is_submit_shortcut(
                            &key.key,
                            key.modifiers.platform,
                            key.modifiers.control,
                            &editor.input.read(cx).value(),
                            false,
                        ) {
                            this.submit_comment(cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(Textarea::new(&editor.input).aria_label(format!("Comment on lines {}", editor.comment.range_label)))
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(10.))
                                    .text_color(hsla(p.muted_foreground).opacity(0.7))
                                    .child("⌘/Ctrl Enter to send"),
                            )
                            .child(
                                Button::new("diff-comment-cancel", ButtonVariant::Ghost, ButtonSize::Xs)
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_comment(cx))),
                            )
                            .child(
                                Button::new("diff-comment-submit", ButtonVariant::Default, ButtonSize::Xs)
                                    .label("Comment")
                                    .disabled(empty)
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
