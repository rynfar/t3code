//! `AssistantCitationChip.tsx`, `AssistantCitationCommentEditor.tsx`. Serialized links remain
//! in the prompt, so drafts, undo, queued turns and turn sends all carry the web's payload.

use super::{
    Composer,
    context::{Ink, chip_shell},
    controls::{Button, ButtonSize, ButtonVariant, Float, floating, menu_surface},
};
use crate::{
    app_state::AppEvent,
    icons::Icon,
    theme::{hsla, palette},
};
use gpui::{AnyElement, Context, Entity, IntoElement, ParentElement, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_component::{h_flex, v_flex};
use t3_core::assistant_citations::{self as rules, Citation, CommentDismissal};

pub(super) struct CommentEditor {
    source: String,
    start: usize,
    citation: Citation,
    input: Entity<TextareaState>,
    remove_on_cancel: Option<bool>,
    _subscription: Subscription,
}

impl Composer {
    pub(super) fn add_citation(&mut self, citation: Citation, window: &mut Window, cx: &mut Context<Self>) {
        if self.answering.is_some() || self.editing_queued().is_some() || !citation.valid() {
            return;
        }
        if !self.close_citation_comment("outside-press", window, cx) {
            return;
        }
        let prompt = self.prompt(cx);
        let cursor = self.input.read(cx).selected_range();
        let leading = if cursor.start > 0 && !prompt[..cursor.start].ends_with(char::is_whitespace) { " " } else { "" };
        let source = citation.serialize();
        self.input.update(cx, |input, cx| input.replace(format!("{leading}{source} "), window, cx));
        self.draft_changed(cx);
        self.open_citation_comment(citation, cursor.start + leading.len(), window, cx);
        if let Some(editor) = self.citation_editor.as_mut() {
            editor.remove_on_cancel = Some(!leading.is_empty());
        }
    }

    fn open_citation_comment(&mut self, citation: Citation, start: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.close_citation_comment("outside-press", window, cx) {
            return;
        }
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder("Add an optional comment...").submit_on_enter(true));
        input.update(cx, |input, cx| {
            input.set_value(citation.comment.as_deref().unwrap_or_default(), window, cx);
            input.focus(window, cx);
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { secondary, shift: false } => {
                if this.close_citation_comment("save", window, cx) && *secondary {
                    this.submit(t3_core::composer::SubmitIntent::Foreground, window, cx);
                }
            }
            InputEvent::Change => cx.notify(),
            InputEvent::Blur => { this.close_citation_comment("focus-out", window, cx); },
            _ => {}
        });
        self.citation_editor = Some(CommentEditor { source: citation.serialize(), start, citation, input, remove_on_cancel: None, _subscription: subscription });
        cx.notify();
    }

    pub(super) fn close_citation_comment(&mut self, reason: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editor) = &self.citation_editor else {
            return true;
        };
        if reason == "escape-key"
            && let Some(before) = editor.remove_on_cancel
        {
            let prompt = self.prompt(cx);
            if prompt.get(editor.start..editor.start + editor.source.len()) == Some(editor.source.as_str()) {
                let start = editor.start;
                let end = start + editor.source.len();
                let from = if before && prompt[..start].ends_with(' ') { start - 1 } else { start };
                let to = if prompt[end..].starts_with(' ') { end + 1 } else { end };
                self.input.update(cx, |input, cx| {
                    input.set_selected_range(from..to, cx);
                    input.replace("", window, cx);
                });
                self.draft_changed(cx);
            }
            self.citation_editor = None;
            self.focus(window, cx);
            cx.notify();
            return true;
        }
        let draft = editor.input.read(cx).value().to_string();
        match rules::dismiss_comment(reason, Some(&draft), editor.citation.comment.as_deref()) {
            CommentDismissal::KeepOpen => return false,
            CommentDismissal::Commit(comment) => {
                let source = editor.source.clone();
                let replacement = editor.citation.with_comment(&comment).serialize();
                let prompt = self.prompt(cx);
                let start = editor.start;
                if prompt.get(start..start + source.len()) != Some(source.as_str()) { return false; }
                self.input.update(cx, |input, cx| {
                    input.set_selected_range(start..start + source.len(), cx);
                    input.replace(replacement, window, cx);
                });
                self.draft_changed(cx);
            }
            CommentDismissal::Close => {}
        }
        self.citation_editor = None;
        if reason == "save" || reason == "escape-key" { self.focus(window, cx); }
        cx.notify();
        true
    }

    pub(super) fn render_citation_chips(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.answering.is_some() || self.editing_queued().is_some() {
            return None;
        }
        let citations = rules::collect(&self.prompt(cx));
        if citations.is_empty() {
            return None;
        }
        let p = palette(cx);
        let color = hsla(p.context_chip_citation);
        Some(
            h_flex()
                .id("citation-chips")
                .flex_wrap()
                .gap(px(12.))
                .mb(px(8.))
                .children(citations.into_iter().enumerate().map(|(i, (range, citation))| {
                    let start = range.start;
                    let source = citation.serialize();
                    let go = citation.clone();
                    let edit = citation.clone();
                    let remove = source.clone();
                    let open = self.citation_editor.as_ref().is_some_and(|editor| editor.source == source && editor.start == start);
                    div()
                        .relative()
                        .child(
                            h_flex()
                                .gap(px(2.))
                                .child(chip_shell(&format!("citation-{i}"), Icon::Quote, citation.label(), Ink::Accent(p.context_chip_citation), true, cx).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let environment = this.environment_id(cx);
                                        if go.environment_id != environment {
                                            return;
                                        }
                                        this.state.update(cx, |state, cx| {
                                            state.select_thread(&go.thread_id, cx);
                                            state.citation_source = Some(go.clone());
                                            cx.emit(AppEvent::CitationSource);
                                        });
                                    },
                                )))
                                .child(
                                    div()
                                        .id(("citation-comment", i))
                                        .tab_index(0)
                                        .cursor_pointer()
                                        .rounded(px(4.))
                                        .hover(|s| s.bg(color.opacity(0.17)))
                                        .on_click(cx.listener(move |this, _, window, cx| this.open_citation_comment(edit.clone(), start, window, cx)))
                                        .child(Icon::Pencil.render(px(12.), color)),
                                )
                                .child(
                                    div()
                                        .id(("remove-citation", i))
                                        .tab_index(0)
                                        .cursor_pointer()
                                        .rounded(px(4.))
                                        .hover(|s| s.bg(color.opacity(0.17)))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.citation_editor = None;
                                            let prompt = this.prompt(cx);
                                            if prompt.get(start..start + remove.len()) == Some(remove.as_str()) {
                                                this.input.update(cx, |input, cx| {
                                                    input.set_selected_range(start..start + remove.len(), cx);
                                                    input.replace("", window, cx);
                                                });
                                                this.draft_changed(cx);
                                                this.focus(window, cx);
                                                cx.notify();
                                            }
                                        }))
                                        .child(Icon::X.render(px(12.), color)),
                                ),
                        )
                        .when(open, |this| this.child(floating(Float::AboveEnd, self.citation_comment_popup(cx))))
                }))
                .into_any_element(),
        )
    }

    fn citation_comment_popup(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = &self.citation_editor else {
            return div().into_any_element();
        };
        let too_long = rules::utf16_len(&editor.input.read(cx).value()) > rules::MAX_COMMENT;
        menu_surface(cx)
            .id("citation-comment-editor")
            .w(px(360.))
            .max_w_full()
            .p(px(12.))
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.close_citation_comment("outside-press", window, cx);
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.close_citation_comment("escape-key", window, cx);
                cx.stop_propagation();
            }))
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(Textarea::new(&editor.input).aria_label("Comment on selected text").appearance(false).bordered(false))
                    .when(too_long, |this| this.child(div().text_size(px(12.)).text_color(hsla(palette(cx).destructive)).child("Comments can contain up to 8,000 characters.")))
                    .child(
                        h_flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(Button::new("cancel-citation-comment", ButtonVariant::Outline, ButtonSize::Xs).label("Cancel").on_click(cx.listener(|this, _, window, cx| {
                                this.close_citation_comment("escape-key", window, cx);
                            })))
                            .child(
                                Button::new("save-citation-comment", ButtonVariant::Default, ButtonSize::Xs)
                                    .disabled(too_long)
                                    .label(if too_long { "Shorten comment" } else { "Save" })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_citation_comment("save", window, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
