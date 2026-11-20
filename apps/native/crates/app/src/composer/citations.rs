//! `AssistantCitationChip.tsx`, `AssistantCitationCommentEditor.tsx`. Serialized links remain
//! in the prompt, so drafts, undo, queued turns and turn sends all carry the web's payload.

use super::{
    Composer,
    context::{Ink, chip_shell},
    controls::{Button, ButtonSize, ButtonVariant, Float, floating, menu_surface},
};
use crate::{
    icons::Icon,
    theme::{hsla, palette},
};
use gpui::{AnyElement, Context, Entity, Focusable, IntoElement, KeyDownEvent, ParentElement, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_component::{h_flex, v_flex};
use t3_core::assistant_citations::{self as rules, Citation, CommentAnchor, CommentSettlement};
use t3_core::voice_input::{
    VoiceDraftSnapshot, VoiceSelection, utf8_to_utf16_offset, utf16_to_utf8_offset,
};

pub(super) struct CommentEditor {
    anchor: CommentAnchor,
    input: Entity<TextareaState>,
    pub(super) voice: Entity<crate::voice::VoiceSpeechController>,
    dictation_target_id: u64,
    pending_secondary_submit: bool,
    _subscription: Subscription,
    _voice_subscription: Subscription,
}

impl Composer {
    pub(super) fn add_citation(&mut self, citation: Citation, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) || self.answering.is_some() || !citation.valid() {
            return;
        }
        if !self.close_citation_comment("outside-press", window, cx) {
            return;
        }
        let prompt = self.prompt(cx);
        let (selected, reversed) = {
            let input = self.input.read(cx);
            let selected = input.selected_range();
            let reversed = input.cursor() < selected.end;
            (selected, reversed)
        };
        let (edit, anchor) = rules::insert_citation(&prompt, selected, reversed, citation);
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
            let text = anchor.link.citation.comment.as_deref().unwrap_or_default();
            let end = text.len();
            input.set_value(text, window, cx);
            input.set_selected_range(end..end, cx);
            input.focus(window, cx);
        });
        let owner_key = format!("citation-comment:{}", crate::app_state::uuid_v4());
        let input_for_draft = input.downgrade();
        let owner_for_draft = owner_key.clone();
        let composer_weak_hints = cx.entity().downgrade();
        let composer_weak_proj = cx.entity().downgrade();
        let composer_weak_env = cx.entity().downgrade();
        let composer_weak_disabled = cx.entity().downgrade();
        let input_for_focus = input.downgrade();
        let voice = cx.new(|cx| {
            crate::voice::VoiceSpeechController::new(
                crate::voice::VoiceSpeechConfig {
                    app: self.state.clone(),
                    owner_key,
                    offer_setup: false,
                    draft_reader: std::rc::Rc::new(move |cx| {
                        let input = input_for_draft.upgrade()?;
                        let state = input.read(cx);
                        let text = state.value().to_string();
                        let range = state.selected_range();
                        Some(VoiceDraftSnapshot {
                            owner_key: owner_for_draft.clone(),
                            selection: VoiceSelection {
                                start: utf8_to_utf16_offset(&text, range.start),
                                end: utf8_to_utf16_offset(&text, range.end),
                            },
                            text,
                        })
                    }),
                    hints_reader: std::rc::Rc::new(move |cx| {
                        composer_weak_hints
                            .upgrade()
                            .map(|c| c.read(cx).voice_vocabulary_hints(cx))
                            .unwrap_or_default()
                    }),
                    project_id_reader: std::rc::Rc::new(move |cx| {
                        composer_weak_proj
                            .upgrade()
                            .and_then(|c| c.read(cx).snapshot.project_id.clone())
                    }),
                    environment_id_reader: std::rc::Rc::new(move |cx| {
                        composer_weak_env
                            .upgrade()
                            .map(|c| c.read(cx).environment_id(cx))
                            .filter(|id| !id.is_empty())
                    }),
                    on_open_voice_settings: std::rc::Rc::new(|_, cx| {
                        crate::settings::open_voice(cx);
                    }),
                },
                cx,
            )
        });
        let dictation_target_id = crate::voice::register_dictation_target(
            &voice,
            true,
            std::rc::Rc::new(move |window, cx| {
                input_for_focus
                    .upgrade()
                    .is_some_and(|i| i.read(cx).focus_handle(cx).is_focused(window))
            }),
            std::rc::Rc::new(move |cx| {
                composer_weak_disabled
                    .upgrade()
                    .is_some_and(|c| c.read(cx).is_rewinding(cx))
            }),
            cx,
        );
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { secondary, shift: false } => {
                if let Some(editor) = this.citation_editor.as_mut()
                    && editor.voice.read(cx).blocks_submission()
                {
                    if !editor.voice.read(cx).preparing() {
                        editor.pending_secondary_submit = *secondary;
                        editor.voice.update(cx, |voice, cx| {
                            voice.submit_after_dictation(cx);
                        });
                    }
                    return;
                }
                if this.close_citation_comment("save", window, cx) && *secondary {
                    this.submit(t3_core::composer::SubmitIntent::Foreground, window, cx);
                }
            }
            InputEvent::Change => cx.notify(),
            InputEvent::Blur => { this.close_citation_comment("focus-out", window, cx); },
            _ => {}
        });
        let voice_subscription = cx.subscribe_in(&voice, window, |this, _, event: &crate::voice::VoiceControllerEvent, window, cx| {
            match event {
                crate::voice::VoiceControllerEvent::StateChanged => {
                    if let Some(editor) = &this.citation_editor {
                        let frozen = editor.voice.read(cx).freezes_editor();
                        editor.input.update(cx, |input, cx| input.set_readonly(frozen, cx));
                    }
                    cx.notify();
                }
                crate::voice::VoiceControllerEvent::CommitDraft { text, selection } => {
                    if let Some(editor) = &this.citation_editor {
                        let caret = utf16_to_utf8_offset(text, selection.start).min(text.len());
                        editor.input.update(cx, |input, cx| {
                            let current_len = input.value().len();
                            input.set_selected_range(0..current_len, cx);
                            input.replace(text.clone(), window, cx);
                            input.set_selected_range(caret..caret, cx);
                            input.focus(window, cx);
                        });
                    }
                    cx.notify();
                }
                crate::voice::VoiceControllerEvent::SubmitAfterDictation { text } => {
                    if rules::utf16_len(text) > rules::MAX_COMMENT {
                        if let Some(editor) = this.citation_editor.as_mut() {
                            editor.pending_secondary_submit = false;
                        }
                        cx.notify();
                        return;
                    }
                    let secondary = this
                        .citation_editor
                        .as_ref()
                        .is_some_and(|editor| editor.pending_secondary_submit);
                    if this.close_citation_comment("save", window, cx) && secondary {
                        this.submit(t3_core::composer::SubmitIntent::Foreground, window, cx);
                    }
                }
            }
        });
        self.citation_editor = Some(CommentEditor {
            anchor,
            input,
            voice,
            dictation_target_id,
            pending_secondary_submit: false,
            _subscription: subscription,
            _voice_subscription: voice_subscription,
        });
        cx.notify();
    }

    fn clear_citation_editor(&mut self, cx: &mut gpui::App) {
        if let Some(editor) = self.citation_editor.take() {
            crate::voice::unregister_dictation_target(editor.dictation_target_id, cx);
            editor.voice.update(cx, |voice, cx| voice.cancel(cx));
        }
    }

    pub(super) fn close_citation_comment(&mut self, reason: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editor) = &self.citation_editor else { return true };
        if matches!(reason, "focus-out" | "outside-press")
            && (editor.voice.read(cx).blocks_submission() || editor.voice.read(cx).setup().open)
        {
            return false;
        }
        let draft = editor.input.read(cx).value().to_string();
        match editor.anchor.dismiss(&self.prompt(cx), &draft, reason) {
            CommentSettlement::KeepOpen => return false,
            CommentSettlement::Close(edit) => {
                self.clear_citation_editor(cx);
                if let Some(edit) = edit { self.apply_citation_edit(edit, window, cx); }
            }
        }
        if reason == "save" || reason == "escape-key" { self.focus(window, cx); }
        cx.notify();
        true
    }

    pub(super) fn sync_citation_comment(&mut self, prompt: &str, cx: &mut Context<Self>) {
        let Some(editor) = self.citation_editor.as_mut() else { return };
        let previous = editor.anchor.link.start;
        if !editor.anchor.relocate(prompt) {
            self.clear_citation_editor(cx);
            cx.notify();
        } else if editor.anchor.link.start != previous {
            cx.notify();
        }
    }

    pub(super) fn render_citation_chips(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.answering.is_some() {
            return None;
        }
        let prompt = self.prompt(cx);
        let citations = rules::collect(&prompt);
        if citations.is_empty() {
            return None;
        }
        let p = palette(cx);
        let color = hsla(p.context_chip_citation);
        let rewinding = self.is_rewinding(cx);
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
                    let open = !rewinding && self.citation_editor.as_ref().is_some_and(|editor| editor.anchor.link == link);
                    div()
                        .relative()
                        .child(
                            h_flex()
                                .gap(px(2.))
                                .child(chip_shell(&format!("citation-{i}"), Icon::Quote, citation.label(), Ink::Accent(p.context_chip_citation), !rewinding, cx).when(!rewinding, |this| {
                                    this.on_click(cx.listener(move |this, _, _, cx| {
                                        crate::transcript::markdown::links::navigate_to_citation_source(go.clone(), &this.state, cx);
                                    }))
                                }))
                                .child(
                                    div()
                                        .id(("citation-comment", i))
                                        .rounded(px(4.))
                                        .when(rewinding, |s| s.opacity(0.5))
                                        .when(!rewinding, |s| {
                                            s.tab_index(0)
                                                .cursor_pointer()
                                                .hover(|s| s.bg(color.opacity(0.17)))
                                                .on_click(cx.listener(move |this, _, window, cx| this.open_citation_comment(edit.clone(), window, cx)))
                                        })
                                        .child(Icon::Pencil.render(px(12.), color)),
                                )
                                .child(
                                    div()
                                        .id(("remove-citation", i))
                                        .rounded(px(4.))
                                        .when(rewinding, |s| s.opacity(0.5))
                                        .when(!rewinding, |s| {
                                            s.tab_index(0)
                                                .cursor_pointer()
                                                .hover(|s| s.bg(color.opacity(0.17)))
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    if this.is_rewinding(cx) {
                                                        return;
                                                    }
                                                    this.clear_citation_editor(cx);
                                                    let prompt = this.prompt(cx);
                                                    if let Some(edit) = remove.replace(&prompt, String::new()) {
                                                        this.apply_citation_edit(edit, window, cx);
                                                        this.focus(window, cx);
                                                        cx.notify();
                                                    }
                                                }))
                                        })
                                        .child(Icon::X.render(px(12.), color)),
                                ),
                        )
                        .when(open, |this| this.child(floating(Float::AboveEnd, self.citation_comment_popup(cx))))
                }))
                .into_any_element(),
        )
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn open_citation_voice_for_snapshot(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<crate::voice::VoiceSpeechController>> {
        let citation = Citation {
            version: 1,
            environment_id: "env-snapshot".to_owned(),
            thread_id: "thread-1".to_owned(),
            message_id: "msg-1".to_owned(),
            text: "audio resampler".to_owned(),
            start: 9,
            end: 24,
            prefix: String::new(),
            suffix: String::new(),
            comment: None,
        };
        let prompt = self.prompt(cx);
        let len = prompt.len();
        let (edit, anchor) = rules::insert_citation(&prompt, len..len, false, citation);
        self.apply_citation_edit(edit, window, cx);
        self.open_citation_comment(anchor, window, cx);
        self.citation_editor.as_ref().map(|editor| editor.voice.clone())
    }

    fn citation_comment_popup(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = &self.citation_editor else {
            return div().into_any_element();
        };
        let too_long = rules::utf16_len(&editor.input.read(cx).value()) > rules::MAX_COMMENT;
        let voice_frozen = editor.voice.read(cx).freezes_editor();
        let voice_blocks = editor.voice.read(cx).blocks_submission();
        let voice_preparing = editor.voice.read(cx).preparing();
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
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "enter"
                    && !event.is_held
                    && !event.keystroke.modifiers.shift
                    && let Some(editor) = this.citation_editor.as_mut()
                    && editor.voice.read(cx).blocks_submission()
                    && !editor.voice.read(cx).preparing()
                {
                    editor.pending_secondary_submit =
                        event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
                    editor.voice.update(cx, |voice, cx| {
                        voice.submit_after_dictation(cx);
                    });
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if let Some(editor) = &this.citation_editor
                    && editor.voice.read(cx).state().is_busy()
                {
                    editor.voice.update(cx, |voice, cx| voice.cancel(cx));
                    cx.stop_propagation();
                    return;
                }
                this.close_citation_comment("escape-key", window, cx);
                cx.stop_propagation();
            }))
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(crate::voice::render_voice_preview(
                        "citation-comment",
                        &editor.voice,
                        cx,
                    ))
                    .child(
                        Textarea::new(&editor.input)
                            .aria_label(crate::i18n::t(cx, "Comment on selected text"))
                            .appearance(false)
                            .bordered(false)
                            .readonly(voice_frozen),
                    )
                    .when(too_long, |this| this.child(div().text_size(px(12.)).text_color(hsla(palette(cx).destructive)).child("Comments can contain up to 8,000 characters.")))
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.))
                            .child(crate::voice::render_text_field_speech_controls(
                                "citation-comment",
                                &editor.voice,
                                false,
                                cx,
                            ))
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(Button::new("cancel-citation-comment", ButtonVariant::Outline, ButtonSize::Xs).label(crate::i18n::t(cx, "Cancel")).on_click(cx.listener(|this, _, window, cx| {
                                        this.close_citation_comment("escape-key", window, cx);
                                    })))
                                    .child(
                                        Button::new("save-citation-comment", ButtonVariant::Default, ButtonSize::Xs)
                                            .disabled(voice_preparing || (too_long && !voice_blocks))
                                            .label(crate::i18n::t(cx, if too_long { "Shorten comment" } else { "Save" }))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                if let Some(editor) = this.citation_editor.as_mut()
                                                    && editor.voice.read(cx).blocks_submission()
                                                {
                                                    if !editor.voice.read(cx).preparing() {
                                                        editor.pending_secondary_submit = false;
                                                        editor.voice.update(cx, |voice, cx| {
                                                            voice.submit_after_dictation(cx);
                                                        });
                                                    }
                                                    return;
                                                }
                                                this.close_citation_comment("save", window, cx);
                                            })),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }
}
