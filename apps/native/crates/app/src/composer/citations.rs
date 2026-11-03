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
use t3_core::assistant_citations::{self as rules, Citation, CommentAnchor, CommentSettlement};

pub(super) struct CommentEditor {
    anchor: CommentAnchor,
    input: Entity<TextareaState>,
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
        let selected = self.input.read(cx).selected_range();
        let (edit, anchor) = rules::insert_citation(&prompt, selected, citation);
        self.apply_citation_edit(edit, window, cx);
        self.open_citation_comment(anchor, window, cx);
    }

    fn apply_citation_edit(&mut self, edit: rules::PromptEdit, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_selected_range(edit.range, cx);
            input.replace(edit.text, window, cx);
        });
        self.draft_changed(cx);
    }

    fn open_citation_comment(&mut self, anchor: CommentAnchor, window: &mut Window, cx: &mut Context<Self>) {
        if !self.close_citation_comment("outside-press", window, cx) || anchor.link.replace(&self.prompt(cx), String::new()).is_none() {
            return;
        }
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder("Add an optional comment...").submit_on_enter(true));
        input.update(cx, |input, cx| {
            input.set_value(anchor.link.citation.comment.as_deref().unwrap_or_default(), window, cx);
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
        self.citation_editor = Some(CommentEditor { anchor, input, _subscription: subscription });
        cx.notify();
    }

    pub(super) fn close_citation_comment(&mut self, reason: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editor) = &self.citation_editor else { return true };
        let draft = editor.input.read(cx).value().to_string();
        match editor.anchor.dismiss(&self.prompt(cx), &draft, reason) {
            CommentSettlement::KeepOpen => return false,
            CommentSettlement::Close(edit) => {
                self.citation_editor = None;
                if let Some(edit) = edit { self.apply_citation_edit(edit, window, cx); }
            }
        }
        if reason == "save" || reason == "escape-key" { self.focus(window, cx); }
        cx.notify();
        true
    }

    pub(super) fn render_citation_chips(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.answering.is_some() || self.editing_queued().is_some() {
            return None;
        }
        let prompt = self.prompt(cx);
        let citations = rules::collect(&prompt);
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
                    let link = rules::CitationLink::from_match(&prompt, range, citation.clone());
                    let go = citation.clone();
                    let edit = CommentAnchor { link: link.clone(), inserted_spaces: None };
                    let remove = link.clone();
                    let open = self.citation_editor.as_ref().is_some_and(|editor| editor.anchor.link == link);
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
                                        .on_click(cx.listener(move |this, _, window, cx| this.open_citation_comment(edit.clone(), window, cx)))
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
                                            if let Some(edit) = remove.replace(&prompt, String::new()) {
                                                this.apply_citation_edit(edit, window, cx);
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
