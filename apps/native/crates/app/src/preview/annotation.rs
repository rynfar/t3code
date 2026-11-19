//! Floating annotation composer for server-streamed preview tabs, with voice dictation wired
//! into `crate::voice::VoiceSpeechController` (`pingdotgg/t3code#30624`, `PreviewAnnotationSpeech.tsx`)
//! using the exact same microphone consent, transcription, live preview, editor freezing, and
//! Escape-cancels-dictation-first behavior as the main composer and diff comments.

use std::io::Cursor;
use std::rc::Rc;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, Render, SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_component::{h_flex, v_flex};
use t3_core::preview::annotation::{AnnotationTarget, PreviewAnnotationDraft};
use t3_core::preview::stream::Viewport;
use t3_core::voice_input::{VoiceDraftSnapshot, VoiceInputPhase, VoiceSelection, utf8_to_utf16_offset, utf16_to_utf8_offset};

use crate::app_state::AppState;
use crate::icons::Icon;
use crate::right_panel::{Button as IconButton, Size as IconSize, Variant as IconVariant};
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};
use crate::voice::{
    VoiceControllerEvent, VoiceSpeechConfig, VoiceSpeechController, register_dictation_target, render_text_field_speech_controls,
    render_voice_preview, unregister_dictation_target,
};

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationEditorEvent {
    Submit(PreviewAnnotationDraft),
    Cancel,
}

/// Crops the selected region (or full frame for point/full-page annotations) from the latest
/// streamed JPEG frame into PNG bytes suitable for composer image attachment.
pub fn crop_frame_to_png(jpeg_bytes: &[u8], viewport: Option<Viewport>, target: &AnnotationTarget) -> Option<Vec<u8>> {
    let image = image::load_from_memory_with_format(jpeg_bytes, image::ImageFormat::Jpeg).ok()?;
    let output = match (target, viewport) {
        (AnnotationTarget::Region(region), Some(viewport)) => {
            if let Some((x, y, width, height)) = region.frame_crop_rect((image.width(), image.height()), viewport) {
                image.crop_imm(x, y, width, height)
            } else {
                image
            }
        }
        _ => image,
    };
    let mut png = Vec::new();
    output.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(png)
}

pub struct AnnotationEditor {
    page_url: String,
    page_title: String,
    target: AnnotationTarget,
    focus: FocusHandle,
    comment: Entity<TextareaState>,
    voice: Entity<VoiceSpeechController>,
    dictation_target_id: u64,
    disposed: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AnnotationEditorEvent> for AnnotationEditor {}

impl Focusable for AnnotationEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AnnotationEditor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: Entity<AppState>,
        thread_id: String,
        tab_id: String,
        page_url: String,
        page_title: String,
        target: AnnotationTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session_id = crate::app_state::uuid_v4();
        let owner_key = format!("preview-annotation:{thread_id}:{tab_id}:{session_id}");
        let comment = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder("Describe what to change…"));

        let owner_for_draft = owner_key.clone();
        let input_for_draft = comment.downgrade();
        let input_for_focus = comment.downgrade();

        let project_id = state
            .read(cx)
            .thread_shell(&thread_id)
            .map(|thread| thread.project_id.clone())
            .or_else(|| thread_id.strip_prefix("draft:").map(str::to_owned));
        let env_id = state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .map(|cfg| cfg.environment.environment_id.clone())
            .filter(|id| !id.is_empty());
        let mut hints = Vec::new();
        if !page_title.trim().is_empty() {
            hints.push(page_title.clone());
        }
        if let Some(host) = t3_core::preview::host_label(&page_url) {
            hints.push(host);
        }

        let voice = cx.new(|cx| {
            VoiceSpeechController::new(
                VoiceSpeechConfig {
                    app: state,
                    owner_key,
                    offer_setup: false,
                    draft_reader: Rc::new(move |cx| {
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
                    hints_reader: Rc::new(move |_| hints.clone()),
                    project_id_reader: Rc::new(move |_| project_id.clone()),
                    environment_id_reader: Rc::new(move |_| env_id.clone()),
                    on_open_voice_settings: Rc::new(|_, cx| {
                        crate::settings::open_voice(cx);
                    }),
                },
                cx,
            )
        });

        let editor_weak = cx.entity().downgrade();
        let dictation_target_id = register_dictation_target(
            &voice,
            true,
            Rc::new(move |window, cx| {
                input_for_focus
                    .upgrade()
                    .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
            }),
            Rc::new(move |cx| editor_weak.upgrade().is_none_or(|editor| editor.read(cx).disposed)),
            cx,
        );

        let input_sub = cx.subscribe_in(&comment, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
            if let InputEvent::PressEnter { secondary, .. } = event
                && *secondary
            {
                this.submit(cx);
            }
        });

        let voice_sub = cx.subscribe_in(&voice, window, |this, voice_entity, event: &VoiceControllerEvent, window, cx| match event {
            VoiceControllerEvent::StateChanged => {
                let frozen = voice_entity.read(cx).freezes_editor();
                this.comment.update(cx, |input, cx| input.set_readonly(frozen, cx));
                cx.notify();
            }
            VoiceControllerEvent::CommitDraft { text, selection } => {
                let caret = utf16_to_utf8_offset(text, selection.start);
                let next_text = text.clone();
                this.comment.update(cx, |input, cx| {
                    input.set_value(next_text, window, cx);
                    input.set_selected_range(caret..caret, cx);
                    input.focus(window, cx);
                });
                cx.notify();
            }
            VoiceControllerEvent::SubmitAfterDictation { .. } => {
                this.submit(cx);
            }
        });

        comment.update(cx, |input, cx| input.focus(window, cx));

        cx.on_release(|this: &mut AnnotationEditor, cx| {
            this.dispose(cx);
        })
        .detach();

        Self {
            page_url,
            page_title,
            target,
            focus: cx.focus_handle(),
            comment,
            voice,
            dictation_target_id,
            disposed: false,
            _subscriptions: vec![input_sub, voice_sub],
        }
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub fn voice(&self) -> &Entity<VoiceSpeechController> {
        &self.voice
    }

    pub fn dispose(&mut self, cx: &mut App) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        unregister_dictation_target(self.dictation_target_id, cx);
        self.voice.update(cx, |voice, cx| voice.cancel(cx));
    }

    /// Cancels active dictation first (`pingdotgg/t3code#32682`), otherwise cancels the annotation.
    pub fn handle_escape(&mut self, cx: &mut Context<Self>) {
        let phase = self.voice.read(cx).state().phase;
        if !matches!(phase, VoiceInputPhase::Idle | VoiceInputPhase::Error) {
            self.voice.update(cx, |voice, cx| voice.cancel(cx));
            cx.notify();
            return;
        }
        self.dispose(cx);
        cx.emit(AnnotationEditorEvent::Cancel);
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.voice.read(cx).blocks_submission() {
            if !self.voice.read(cx).preparing() {
                self.voice.update(cx, |voice, cx| {
                    voice.submit_after_dictation(cx);
                });
            }
            return;
        }
        let comment = self.comment.read(cx).value().trim().to_owned();
        if comment.is_empty() {
            return;
        }
        let draft = PreviewAnnotationDraft {
            page_url: self.page_url.clone(),
            page_title: self.page_title.clone(),
            target: self.target.clone(),
            comment,
        };
        self.dispose(cx);
        cx.emit(AnnotationEditorEvent::Submit(draft));
    }
}

impl Render for AnnotationEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let voice = self.voice.read(cx);
        let blocks = voice.blocks_submission();
        let preparing = voice.preparing();
        let can_submit = if blocks { !preparing } else { !self.comment.read(cx).value().trim().is_empty() };
        let summary = SharedString::from(self.target.summary());

        v_flex()
            .id("preview-annotation-editor")
            .track_focus(&self.focus)
            .w_full()
            .max_w(px(384.))
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(10.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.popover))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "enter"
                    && !event.is_held
                    && (event.keystroke.modifiers.platform || event.keystroke.modifiers.control)
                    && this.voice.read(cx).blocks_submission()
                    && !this.voice.read(cx).preparing()
                {
                    this.voice.update(cx, |voice, cx| voice.submit_after_dictation(cx));
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                cx.stop_propagation();
                this.handle_escape(cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.handle_escape(cx);
                }
            }))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap(px(6.))
                            .child(Icon::MousePointerClick.sized(px(14.)).text_color(hsla(p.primary)))
                            .child(div().truncate().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(summary)),
                    )
                    .child(
                        IconButton::new("preview-annotation-close")
                            .variant(IconVariant::GhostMuted)
                            .size(IconSize::IconMicro)
                            .icon(Icon::X)
                            .tooltip("Cancel annotation (Esc)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dispose(cx);
                                cx.emit(AnnotationEditorEvent::Cancel);
                            })),
                    ),
            )
            .child(render_voice_preview("preview-annotation", &self.voice, cx))
            .child(
                div()
                    .w_full()
                    .min_h(px(68.))
                    .max_h(px(140.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hsla(p.input))
                    .bg(hsla(p.background))
                    .px(px(8.))
                    .py(px(6.))
                    .text_sm()
                    .child(Textarea::new(&self.comment).appearance(false).bordered(false)),
            )
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(render_text_field_speech_controls("preview-annotation", &self.voice, false, cx))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                Button::new("preview-annotation-cancel", ButtonVariant::Outline, ButtonSize::Sm)
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.dispose(cx);
                                        cx.emit(AnnotationEditorEvent::Cancel);
                                    })),
                            )
                            .child(
                                Button::new("preview-annotation-submit", ButtonVariant::Default, ButtonSize::Sm)
                                    .label("Add to composer")
                                    .disabled(!can_submit)
                                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                            ),
                    ),
            )
    }
}
