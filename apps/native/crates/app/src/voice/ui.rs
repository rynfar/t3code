use gpui::{
    AnyElement, App, BoxShadow, ElementId, Entity, FontWeight, MouseButton, ParentElement,
    SharedString, Styled, StyledText, TextRun, Window, deferred, div, point, prelude::*, px, svg,
};
use gpui_component::ActiveTheme;
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};
use t3_core::voice_input::{
    MICROPHONE_PERMISSION_ERROR, WAVEFORM_BAR_COUNT, WAVEFORM_REST_SCALE, VoiceErrorAction,
    VoiceInputPhase, format_elapsed_mm_ss, is_retryable_transcription_error,
    resolve_speech_presentation, speech_language_label, voice_error_label,
};

use super::controller::{VoiceSetupStep, VoiceSpeechController};
use super::shortcuts::{
    active_floating_indicator_controller, active_setup_controller, dictation_shortcut_label,
};
use crate::components::motion::{Spinner, StatusDot};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, Select, SelectOption};
use crate::theme::{ThemeSlot, ThemeSlotExt, hsla, palette};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceUiSize {
    Regular,
    Compact,
}

fn format_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let f = bytes as f64;
    if f >= GB {
        format!("{:.1} GB", f / GB)
    } else {
        format!("{:.0} MB", (f / MB).max(1.0))
    }
}

pub fn render_speech_button(
    id_prefix: &str,
    controller: &Entity<VoiceSpeechController>,
    size: VoiceUiSize,
    disabled: bool,
    cx: &App,
) -> AnyElement {
    let voice = controller.read(cx);
    if !voice.available(cx) {
        return div().into_any_element();
    }
    if !matches!(
        voice.state().phase,
        VoiceInputPhase::Idle | VoiceInputPhase::Error
    ) {
        return div().into_any_element();
    }

    let p = palette(cx);
    let shortcut = dictation_shortcut_label(cx);
    let tooltip_text: SharedString = format!("Voice input ({shortcut})").into();
    let entity = controller.clone();

    let (btn_size, icon_px, radius_px) = match size {
        VoiceUiSize::Regular => (px(28.), px(16.), px(999.)),
        VoiceUiSize::Compact => (px(24.), px(14.), px(6.)),
    };

    div()
        .id(SharedString::from(format!("{id_prefix}-speech-btn")))
        .size(btn_size)
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius_px)
        .text_color(hsla(p.muted_foreground))
        .when(disabled, |el| el.opacity(0.4))
        .when(!disabled, |el| {
            el.cursor_pointer()
                .hover(|s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, window, cx| {
                    entity.update(cx, |voice, cx| voice.start(window, cx));
                })
        })
        .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
        .child(
            svg()
                .path(Icon::Mic.path())
                .size(icon_px)
                .flex_none()
                .text_color(hsla(p.muted_foreground)),
        )
        .into_any_element()
}

pub fn render_speech_recording_pill(
    id_prefix: &str,
    controller: &Entity<VoiceSpeechController>,
    size: VoiceUiSize,
    disabled: bool,
    cx: &App,
) -> AnyElement {
    render_speech_recording_pill_inner(id_prefix, controller, size, disabled, false, cx)
}

fn render_speech_recording_pill_inner(
    id_prefix: &str,
    controller: &Entity<VoiceSpeechController>,
    size: VoiceUiSize,
    disabled: bool,
    use_session_state: bool,
    cx: &App,
) -> AnyElement {
    let voice = controller.read(cx);
    let state = if use_session_state {
        voice.session_state()
    } else {
        voice.state()
    };
    if state.phase == VoiceInputPhase::Idle {
        return div().into_any_element();
    }

    if state.phase == VoiceInputPhase::Error {
        return render_error_pill(id_prefix, controller, size, disabled, use_session_state, cx);
    }

    let p = palette(cx);
    let shortcut = dictation_shortcut_label(cx);
    let (status_label, _) = resolve_speech_presentation(state, voice.progress());
    let compact = size == VoiceUiSize::Compact;
    let entity_stop = controller.clone();
    let entity_cancel = controller.clone();
    let entity_skip = controller.clone();

    let pill_h = if compact { px(26.) } else { px(30.) };
    let border_color = if state.phase == VoiceInputPhase::Recording {
        hsla(p.primary).opacity(0.45)
    } else {
        hsla(p.border)
    };
    let bg_color = if state.phase == VoiceInputPhase::Recording {
        hsla(p.primary).opacity(0.08)
    } else {
        hsla(p.muted).opacity(0.6)
    };

    let mut pill = h_flex()
        .id(SharedString::from(format!("{id_prefix}-speech-pill")))
        .h(pill_h)
        .px(px(8.))
        .gap(px(6.))
        .items_center()
        .rounded(px(999.))
        .border_1()
        .border_color(border_color)
        .bg(bg_color)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

    match state.phase {
        VoiceInputPhase::Preparing => {
            pill = pill
                .child(Spinner::new("speech-preparing", px(13.), hsla(p.muted_foreground)))
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(status_label.unwrap_or_else(|| "Starting…".to_owned())),
                )
                .child(render_pill_icon_action(
                    format!("{id_prefix}-speech-cancel"),
                    Icon::X,
                    "Cancel dictation (Esc)",
                    hsla(p.muted_foreground),
                    disabled,
                    move |_, _, cx| {
                        entity_cancel.update(cx, |voice, cx| voice.cancel(cx));
                    },
                    cx,
                ));
        }

        VoiceInputPhase::Recording => {
            let levels = voice.recording_levels();
            let elapsed = format_elapsed_mm_ss(voice.elapsed_seconds());
            let finish_tip = format!("Finish dictation ({shortcut})");
            pill = pill
                .child(StatusDot::new("speech-recording-dot", px(6.), hsla(p.destructive), false))
                .child(render_sample_bars(levels, compact, cx))
                .when(!compact, |el| {
                    el.child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child(elapsed),
                    )
                })
                .child(render_pill_icon_action(
                    format!("{id_prefix}-speech-finish"),
                    Icon::Check,
                    finish_tip,
                    hsla(p.primary),
                    disabled,
                    move |_, _, cx| {
                        entity_stop.update(cx, |voice, cx| voice.stop(cx));
                    },
                    cx,
                ))
                .child(render_pill_icon_action(
                    format!("{id_prefix}-speech-cancel"),
                    Icon::X,
                    "Cancel dictation (Esc)",
                    hsla(p.muted_foreground),
                    disabled,
                    move |_, _, cx| {
                        entity_cancel.update(cx, |voice, cx| voice.cancel(cx));
                    },
                    cx,
                ));
        }

        VoiceInputPhase::Transcribing => {
            pill = pill
                .child(Spinner::new("speech-transcribing", px(13.), hsla(p.muted_foreground)))
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(status_label.unwrap_or_else(|| "Transcribing…".to_owned())),
                )
                .child(render_pill_icon_action(
                    format!("{id_prefix}-speech-cancel"),
                    Icon::X,
                    "Cancel transcription (Esc)",
                    hsla(p.muted_foreground),
                    disabled,
                    move |_, _, cx| {
                        entity_cancel.update(cx, |voice, cx| voice.cancel(cx));
                    },
                    cx,
                ));
        }

        VoiceInputPhase::PostProcessing => {
            pill = pill
                .child(
                    svg()
                        .path(Icon::Sparkles.path())
                        .size(px(13.))
                        .flex_none()
                        .text_color(hsla(p.primary)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(status_label.unwrap_or_else(|| "Cleaning up…".to_owned())),
                )
                .child(
                    Button::new(
                        SharedString::from(format!("{id_prefix}-speech-skip")),
                        ButtonVariant::Ghost,
                        ButtonSize::Xs,
                    )
                    .label("Skip")
                    .disabled(disabled)
                    .on_click(move |_, _, cx| {
                        entity_skip.update(cx, |voice, cx| voice.skip_post_processing(cx));
                    }),
                )
                .child(render_pill_icon_action(
                    format!("{id_prefix}-speech-cancel"),
                    Icon::X,
                    "Cancel (Esc)",
                    hsla(p.muted_foreground),
                    disabled,
                    move |_, _, cx| {
                        entity_cancel.update(cx, |voice, cx| voice.cancel(cx));
                    },
                    cx,
                ));
        }

        VoiceInputPhase::Idle | VoiceInputPhase::Error => {}
    }

    pill.into_any_element()
}

fn render_error_pill(
    id_prefix: &str,
    controller: &Entity<VoiceSpeechController>,
    size: VoiceUiSize,
    disabled: bool,
    use_session_state: bool,
    cx: &App,
) -> AnyElement {
    let voice = controller.read(cx);
    let state = if use_session_state {
        voice.session_state()
    } else {
        voice.state()
    };
    let p = palette(cx);
    let compact = size == VoiceUiSize::Compact;
    let err_msg = state.error.as_deref();
    let short_label = voice_error_label(err_msg);
    let full_detail: SharedString = err_msg
        .unwrap_or("Voice input failed")
        .to_owned()
        .into();

    let is_retry = state.error_action == Some(VoiceErrorAction::Retry)
        || is_retryable_transcription_error(err_msg);
    let is_settings = state.error_action == Some(VoiceErrorAction::Settings);
    let is_mic_blocked = err_msg == Some(MICROPHONE_PERMISSION_ERROR)
        || err_msg == Some("Microphone access is required for voice input.");

    let entity_retry = controller.clone();
    let entity_settings = controller.clone();
    let entity_dismiss = controller.clone();

    let mut pill = h_flex()
        .id(SharedString::from(format!("{id_prefix}-speech-error-pill")))
        .h(if compact { px(26.) } else { px(30.) })
        .px(px(8.))
        .gap(px(5.))
        .items_center()
        .rounded(px(999.))
        .border_1()
        .border_color(hsla(p.destructive).opacity(0.4))
        .bg(hsla(p.error_surface))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .tooltip(move |window, cx| Tooltip::new(full_detail.clone()).build(window, cx))
        .child(
            svg()
                .path(Icon::CircleAlert.path())
                .size(px(13.))
                .flex_none()
                .text_color(hsla(p.destructive)),
        )
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.error_foreground))
                .child(short_label),
        );

    if is_settings {
        pill = pill.child(
            Button::new(
                SharedString::from(format!("{id_prefix}-speech-error-settings")),
                ButtonVariant::Ghost,
                if compact { ButtonSize::IconXs } else { ButtonSize::Xs },
            )
            .icon(Icon::Settings)
            .when(!compact, |btn| btn.label("Settings"))
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                entity_settings.update(cx, |voice, cx| {
                    if is_mic_blocked && voice.open_system_microphone_settings(cx) {
                        return;
                    }
                    voice.open_voice_settings(window, cx);
                });
            }),
        );
    } else if is_retry {
        pill = pill.child(render_pill_icon_action(
            format!("{id_prefix}-speech-error-retry"),
            Icon::RotateCcw,
            "Try again",
            hsla(p.error_foreground),
            disabled,
            move |_, window, cx| {
                entity_retry.update(cx, |voice, cx| {
                    if use_session_state {
                        voice.retry_active_target(window, cx);
                    } else {
                        voice.start(window, cx);
                    }
                });
            },
            cx,
        ));
    }

    pill = pill.child(render_pill_icon_action(
        format!("{id_prefix}-speech-error-dismiss"),
        Icon::X,
        "Dismiss",
        hsla(p.muted_foreground),
        false,
        move |_, _, cx| {
            entity_dismiss.update(cx, |voice, cx| voice.cancel(cx));
        },
        cx,
    ));

    pill.into_any_element()
}

fn render_pill_icon_action(
    id: impl Into<ElementId>,
    icon: Icon,
    tip: impl Into<SharedString>,
    color: gpui::Hsla,
    disabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let tip: SharedString = tip.into();
    div()
        .id(id)
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(999.))
        .when(disabled, |el| el.opacity(0.4))
        .when(!disabled, |el| {
            el.cursor_pointer()
                .hover(|s| s.bg(hsla(p.accent)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(on_click)
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .child(svg().path(icon.path()).size(px(12.)).flex_none().text_color(color))
        .into_any_element()
}

pub fn render_waveform(level: f32, compact: bool, cx: &App) -> AnyElement {
    render_sample_bars(std::iter::repeat_n(level, WAVEFORM_BAR_COUNT), compact, cx)
}

fn render_sample_bars(samples: impl Iterator<Item = f32>, compact: bool, cx: &App) -> AnyElement {
    let p = palette(cx);
    let max_h = if compact { 14. } else { 16. };
    h_flex()
        .items_center()
        .gap(px(if compact { 1. } else { 1.5 }))
        .h(px(max_h))
        .children(samples.map(|sample| {
            let level = sample.clamp(0., 1.);
            let scale = WAVEFORM_REST_SCALE + level * (1. - WAVEFORM_REST_SCALE);
            div()
                .w(px(if compact { 1.5 } else { 2. }))
                .h(px((max_h * scale).max(3.)))
                .rounded(px(1.))
                .bg(hsla(p.primary).opacity(0.55 + level * 0.45))
        }))
        .into_any_element()
}

pub fn render_voice_preview(
    id_prefix: &str,
    controller: &Entity<VoiceSpeechController>,
    cx: &App,
) -> AnyElement {
    let voice = controller.read(cx);
    let Some(preview) = voice.preview() else {
        return div().into_any_element();
    };
    if preview.committed.is_empty() && preview.tentative.is_empty() {
        return div().into_any_element();
    }
    let p = palette(cx);
    let font = gpui::font(cx.theme().font_family.clone());
    let mut full = String::with_capacity(preview.committed.len() + preview.tentative.len());
    let mut runs = Vec::new();
    if !preview.committed.is_empty() {
        full.push_str(&preview.committed);
        runs.push(TextRun {
            len: preview.committed.len(),
            font: font.clone(),
            color: hsla(p.foreground),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    if !preview.tentative.is_empty() {
        full.push_str(&preview.tentative);
        runs.push(TextRun {
            len: preview.tentative.len(),
            font,
            color: hsla(p.muted_foreground),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    div()
        .id(SharedString::from(format!("{id_prefix}-speech-preview")))
        .w_full()
        .max_h(px(96.))
        .overflow_y_scroll()
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.border).opacity(0.6))
        .bg(hsla(p.muted).opacity(0.3))
        .px(px(10.))
        .py(px(6.))
        .text_sm()
        .child(StyledText::new(SharedString::from(full)).with_runs(runs))
        .into_any_element()
}

pub fn render_text_field_speech_controls(
    id_prefix: &str,
    controller: &Entity<VoiceSpeechController>,
    disabled: bool,
    cx: &App,
) -> AnyElement {
    h_flex()
        .items_center()
        .gap(px(4.))
        .child(render_speech_recording_pill(
            id_prefix,
            controller,
            VoiceUiSize::Compact,
            disabled,
            cx,
        ))
        .child(render_speech_button(
            id_prefix,
            controller,
            VoiceUiSize::Compact,
            disabled,
            cx,
        ))
        .into_any_element()
}

/// Renders both the active voice setup modal and the floating `GlobalVoiceInputIndicator`
/// (`pingdotgg/t3code#35761`, `pingdotgg/t3code#38225`) through the workspace overlay slot.
pub fn render_active_voice_setup_modal(cx: &App) -> Option<AnyElement> {
    let indicator = active_floating_indicator_controller(cx)
        .map(|controller| render_global_voice_input_indicator(&controller, cx));
    let modal = active_setup_controller(cx)
        .map(|controller| render_voice_setup_modal("workspace", &controller, cx));
    match (indicator, modal) {
        (None, None) => None,
        (Some(el), None) | (None, Some(el)) => Some(el),
        (Some(indicator), Some(modal)) => Some(
            div()
                .absolute()
                .inset_0()
                .child(indicator)
                .child(modal)
                .into_any_element(),
        ),
    }
}

/// Floating top-center pill shown while dictation continues for an off-screen thread draft
/// (`GlobalVoiceInputIndicator.tsx`, `pingdotgg/t3code#35761`, `pingdotgg/t3code#38225`).
pub fn render_global_voice_input_indicator(
    controller: &Entity<VoiceSpeechController>,
    cx: &App,
) -> AnyElement {
    let voice = controller.read(cx);
    let p = palette(cx);
    let label = voice
        .active_target_label()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Untitled thread")
        .to_owned();
    let can_open = voice.can_open_active_target();
    let open_entity = controller.clone();

    deferred(
        div()
            .id("global-voice-input-indicator-layer")
            .absolute()
            .top(px(12.))
            .left_0()
            .right_0()
            .px(px(16.))
            .flex()
            .justify_center()
            .child(
                h_flex()
                    .id("global-voice-input-indicator")
                    .max_w(px(520.))
                    .items_center()
                    .gap(px(4.))
                    .p(px(2.))
                    .rounded(px(999.))
                    .border_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.popover))
                    .occlude()
                    .shadow(vec![BoxShadow {
                        color: hsla(p.dialog_backdrop),
                        offset: point(px(0.), px(8.)),
                        blur_radius: px(24.),
                        spread_radius: px(-8.),
                        inset: false,
                    }])
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new(
                            "global-voice-indicator-thread",
                            ButtonVariant::Ghost,
                            ButtonSize::Sm,
                        )
                        .label(label)
                        .disabled(!can_open)
                        .on_click(move |_, window, cx| {
                            if let Some(open) = open_entity.read(cx).active_target_open_handler() {
                                open(window, cx);
                            }
                        }),
                    )
                    .child(render_speech_recording_pill_inner(
                        "global-voice-indicator",
                        controller,
                        VoiceUiSize::Regular,
                        false,
                        true,
                        cx,
                    )),
            ),
    )
    .into_any_element()
}

pub fn render_voice_setup_modal(
    id_prefix: &str,
    controller: &Entity<VoiceSpeechController>,
    cx: &App,
) -> AnyElement {
    let voice = controller.read(cx);
    let setup = voice.setup().clone();
    if !setup.open {
        return div().into_any_element();
    }
    let p = palette(cx);
    let env_label = voice
        .transcription_environment_label(cx)
        .unwrap_or_else(|| "this environment".to_owned());

    let microphone = matches!(setup.step, VoiceSetupStep::Confirm | VoiceSetupStep::Ready).then(|| {
        let mut devices = vec![SelectOption::new("", crate::i18n::t(cx, "System default"))];
        devices.extend(voice.setup_microphones().iter()
            .filter(|device| !t3_core::voice_input::is_system_default_device_id(&device.device_id) && !device.label.is_empty())
            .map(|device| SelectOption::new(device.device_id.clone(), device.label.clone())));
        let owner = controller.clone();
        h_flex().justify_between().gap(px(8.))
            .child(div().text_sm().child(crate::i18n::t(cx, "Microphone")))
            .child(Select::new("voice-setup-microphone", devices)
                .selected(voice.setup_microphone(cx))
                .width(208.)
                .show_check()
                .on_change(move |value, _, cx| owner.update(cx, |voice, cx| voice.select_setup_microphone(&value, cx))))
    });
    let close_entity = controller.clone();
    let download_entity = controller.clone();
    let cancel_dl_entity = controller.clone();
    let retry_entity = controller.clone();
    let start_entity = controller.clone();
    let settings_entity = controller.clone();
    let scrim_close = controller.clone();

    deferred(
        div()
            .id(SharedString::from(format!("{id_prefix}-voice-setup-backdrop")))
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.))
            .bg(hsla(p.dialog_backdrop))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                if !setup.downloading {
                    scrim_close.update(cx, |voice, cx| voice.setup_set_open(false, cx));
                }
                cx.stop_propagation();
            })
            .child(
                v_flex()
                    .id(SharedString::from(format!("{id_prefix}-voice-setup-dialog")))
                    .w_full()
                    .max_w(px(420.))
                    .relative()
                    .rounded(px(16.))
                    .theme_slot(ThemeSlot::DialogPopup, cx)
                    .child(crate::glass::backdrop(
                        16.,
                        hsla(p.dialog_surface),
                        hsla(p.dialog_border),
                        cx,
                    ))
                    .shadow(vec![BoxShadow {
                        color: hsla(p.dialog_backdrop),
                        offset: point(px(0.), px(24.)),
                        blur_radius: px(64.),
                        spread_radius: px(-24.),
                        inset: false,
                    }])
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        v_flex()
                            .relative()
                            .p(px(20.))
                            .gap(px(12.))
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(hsla(p.foreground))
                                            .child(match setup.step {
                                                VoiceSetupStep::Confirm => "Download voice model",
                                                VoiceSetupStep::Downloading => "Downloading voice model…",
                                                VoiceSetupStep::Ready => "Voice model ready",
                                                VoiceSetupStep::Error => "Voice setup error",
                                            }),
                                    )
                                    .when(!setup.downloading, |el| {
                                        el.child(render_pill_icon_action(
                                            format!("{id_prefix}-voice-setup-close"),
                                            Icon::X,
                                            "Close",
                                            hsla(p.muted_foreground),
                                            false,
                                            move |_, _, cx| {
                                                close_entity.update(cx, |voice, cx| {
                                                    voice.setup_set_open(false, cx)
                                                });
                                            },
                                            cx,
                                        ))
                                    }),
                            )
                            .children(microphone)
                            .child(match setup.step {
                                VoiceSetupStep::Confirm => {
                                    let model_desc = match (&setup.model, &setup.language) {
                                        (Some(model), Some(lang)) => format!(
                                            "{} will download {} ({}) for {} transcription.",
                                            env_label,
                                            model.name,
                                            format_bytes(model.size as u64),
                                            speech_language_label(lang)
                                        ),
                                        (Some(model), None) => format!(
                                            "{} will download {} ({}) for local speech transcription.",
                                            env_label,
                                            model.name,
                                            format_bytes(model.size as u64)
                                        ),
                                        _ => format!(
                                            "{} needs a speech transcription model before recording.",
                                            env_label
                                        ),
                                    };
                                    v_flex()
                                        .gap(px(12.))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(hsla(p.muted_foreground))
                                                .child(model_desc),
                                        )
                                        .child(
                                            h_flex()
                                                .justify_end()
                                                .gap(px(8.))
                                                .child(
                                                    Button::new(
                                                        SharedString::from(format!(
                                                            "{id_prefix}-voice-setup-settings"
                                                        )),
                                                        ButtonVariant::Outline,
                                                        ButtonSize::Sm,
                                                    )
                                                    .label("Voice settings")
                                                    .on_click(move |_, window, cx| {
                                                        settings_entity.update(cx, |voice, cx| {
                                                            voice.setup_set_open(false, cx);
                                                            voice.open_voice_settings(window, cx);
                                                        });
                                                    }),
                                                )
                                                .child(
                                                    Button::new(
                                                        SharedString::from(format!(
                                                            "{id_prefix}-voice-setup-download"
                                                        )),
                                                        ButtonVariant::Default,
                                                        ButtonSize::Sm,
                                                    )
                                                    .icon(Icon::Download)
                                                    .label("Download")
                                                    .on_click(move |_, _, cx| {
                                                        download_entity
                                                            .update(cx, |voice, cx| voice.setup_download(cx));
                                                    }),
                                                ),
                                        )
                                        .into_any_element()
                                }
                                VoiceSetupStep::Downloading => {
                                    let downloaded = setup.downloaded_bytes.unwrap_or(0);
                                    let total = setup.total_bytes.unwrap_or(1).max(1);
                                    let ratio = (downloaded as f32 / total as f32).clamp(0.0, 1.0);
                                    let pct = (ratio * 100.0).round() as u64;
                                    v_flex()
                                        .gap(px(10.))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(hsla(p.muted_foreground))
                                                .child(format!(
                                                    "{} / {} ({pct}%)",
                                                    format_bytes(downloaded),
                                                    format_bytes(total)
                                                )),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .h(px(6.))
                                                .rounded(px(999.))
                                                .bg(hsla(p.muted))
                                                .overflow_hidden()
                                                .child(
                                                    div()
                                                        .h_full()
                                                        .w(gpui::relative(ratio))
                                                        .rounded(px(999.))
                                                        .bg(hsla(p.primary)),
                                                ),
                                        )
                                        .child(
                                            h_flex().justify_end().child(
                                                Button::new(
                                                    SharedString::from(format!(
                                                        "{id_prefix}-voice-setup-cancel-dl"
                                                    )),
                                                    ButtonVariant::Outline,
                                                    ButtonSize::Sm,
                                                )
                                                .label("Cancel download")
                                                .on_click(move |_, _, cx| {
                                                    cancel_dl_entity.update(cx, |voice, cx| {
                                                        voice.setup_cancel_download(cx)
                                                    });
                                                }),
                                            ),
                                        )
                                        .into_any_element()
                                }
                                VoiceSetupStep::Ready => v_flex()
                                    .gap(px(12.))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(hsla(p.muted_foreground))
                                            .child("Your speech model is downloaded and ready."),
                                    )
                                    .child(
                                        h_flex().justify_end().child(
                                            Button::new(
                                                SharedString::from(format!(
                                                    "{id_prefix}-voice-setup-start"
                                                )),
                                                ButtonVariant::Default,
                                                ButtonSize::Sm,
                                            )
                                            .icon(Icon::Mic)
                                            .label("Start recording")
                                            .on_click(move |_, window, cx| {
                                                start_entity.update(cx, |voice, cx| {
                                                    voice.setup_start_recording(window, cx)
                                                });
                                            }),
                                        ),
                                    )
                                    .into_any_element(),
                                VoiceSetupStep::Error => v_flex()
                                    .gap(px(12.))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(hsla(p.destructive))
                                            .child(
                                                setup
                                                    .error
                                                    .clone()
                                                    .unwrap_or_else(|| "Voice setup failed.".to_owned()),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .justify_end()
                                            .gap(px(8.))
                                            .child(
                                                Button::new(
                                                    SharedString::from(format!(
                                                        "{id_prefix}-voice-setup-err-settings"
                                                    )),
                                                    ButtonVariant::Outline,
                                                    ButtonSize::Sm,
                                                )
                                                .label("Voice settings")
                                                .on_click(move |_, window, cx| {
                                                    settings_entity.update(cx, |voice, cx| {
                                                        voice.setup_set_open(false, cx);
                                                        voice.open_voice_settings(window, cx);
                                                    });
                                                }),
                                            )
                                            .when(setup.catalog_failed, |el| {
                                                el.child(
                                                    Button::new(
                                                        SharedString::from(format!(
                                                            "{id_prefix}-voice-setup-retry"
                                                        )),
                                                        ButtonVariant::Default,
                                                        ButtonSize::Sm,
                                                    )
                                                    .label("Retry")
                                                    .on_click(move |_, _, cx| {
                                                        retry_entity.update(cx, |voice, cx| {
                                                            voice.setup_retry_catalog(cx)
                                                        });
                                                    }),
                                                )
                                            }),
                                    )
                                    .into_any_element(),
                            }),
                    ),
            ),
    )
    .with_priority(2)
    .into_any_element()
}
