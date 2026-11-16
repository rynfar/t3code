//! The composer's layout (design-system §7.9, `ChatComposer.tsx`, `ComposerControlsBar.tsx`,
//! `ComposerFooterControls.tsx`, `ComposerPrimaryActions.tsx`): attached banners, the glass card
//! with the prompt, and the footer with the model picker, permissions, mode and send controls.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, ExternalPaths, Focusable, IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::input::{Copy, Editor, Enter, Escape, IndentInline, MoveDown, MoveUp, Paste};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands::ComposerDispatch;
use t3_core::composer::{self as rules, runtime_mode_description, runtime_mode_label};
use t3_core::requests;
use t3_core::traits;

use super::controls::{Button, ButtonSize, ButtonVariant, ComposerControl, Float, Pill, RoundButton, RoundKind, floating, menu_surface};
use super::model_picker::permissions_section;
use super::{Composer, MenuKey, OpenPermissions, OutdentList, PasteAsText, Resolved, StashPrompt, StopPhase, Target, ToggleModelPicker, runtime_mode_icon};
use crate::icons::Icon;
use crate::provider_icon::{BadgeSize, ProviderIcon};
use crate::theme::{hsla, palette};

/// `DEFAULT_COMPOSER_PLACEHOLDER` (`web/src/composerPlaceholder.ts`).
pub const DEFAULT_PLACEHOLDER: &str = "Ask anything. @ files, $ skills, / commands";

/// A click that closed a popup must not reopen it through the trigger it landed on.
const REOPEN_GUARD: Duration = Duration::from_millis(300);

impl Composer {
    /// `ChatComposer` placeholders, most specific first.
    fn placeholder(&self, cx: &App) -> String {
        if let Some(approval) = self.snapshot.pending.approvals.first() {
            return if approval.supports_feedback {
                "Send feedback to decline this request".to_owned()
            } else {
                "Send a message to decline and redirect the agent".to_owned()
            };
        }
        if let Some((_, question)) = self.active_question() {
            return if question.allows_custom_answer() {
                "Type your own answer, or leave this blank to use the selected option".to_owned()
            } else {
                "Choose an option above".to_owned()
            };
        }
        if let Some(suggestion) = self.offered_prompt_suggestion(cx) {
            return format!("{suggestion}  (Tab)");
        }
        DEFAULT_PLACEHOLDER.to_owned()
    }

    /// Placeholder and disabled state follow what the agent waits on; written only on change.
    pub(super) fn sync_input_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self.placeholder(cx);
        let disabled = self.is_rewinding(cx)
            || self.voice.read(cx).freezes_editor()
            || self.active_input().is_some_and(|input| {
                !self.can_answer(cx) || !input.can_respond() || self.question_responding(&input.request_id, cx)
            })
            || self.active_question().is_some_and(|(_, question)| !question.allows_custom_answer());
        if self.input_state.as_ref() == Some(&(placeholder.clone(), disabled)) {
            return;
        }
        self.input_state = Some((placeholder.clone(), disabled));
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            input.set_disabled(disabled, cx);
        });
    }

    pub(super) fn guard_reopen(dismissed: &mut Option<Instant>) -> bool {
        dismissed.take().is_some_and(|at| at.elapsed() < REOPEN_GUARD)
    }

    pub(super) fn render_model_trigger(&self, resolved: &Resolved, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let multi = !resolved.multi_models.is_empty();
        let suffix = traits::effort_suffix(&resolved.driver, &resolved.descriptors);
        let surface = hsla(if cx.theme().is_dark() { p.surface_raised } else { p.card });
        let mark = resolved.mark.clone().map(|mark| ProviderIcon::new(mark).badge(BadgeSize::Compact, surface));
        let label = if let Some(multi_label) = &resolved.multi_model_label {
            multi_label.clone()
        } else if resolved.model_label.is_empty() {
            "Choose model".to_owned()
        } else {
            resolved.model_label.clone()
        };
        let suffix_color = if self.picker_open { hsla(p.foreground) } else { hsla(p.muted_foreground) };
        let suffix = h_flex()
            .gap(px(6.))
            .text_color(suffix_color)
            .when(!multi, |this| {
                this.children(suffix.word.map(|word| div().max_w(px(96.)).truncate().child(word)))
                    .when(suffix.fast || suffix.ultrafast, |this| this.child(Icon::ZapFill.render(px(12.), suffix_color)))
                    .when(suffix.ultracode, |this| this.child(Icon::Workflow.render(px(12.), suffix_color)))
                    .children(suffix.context_window.map(|window| div().max_w(px(96.)).truncate().text_xs().child(window)))
            });
        let tooltip_label = resolved.multi_model_tooltip.as_deref().unwrap_or(&label);
        let tooltip = [Some(tooltip_label.to_owned()), Some("⌘⇧M".to_owned())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        div()
            .relative()
            .min_w(px(52.))
            .child(
                ComposerControl::new("model-picker")
                    .when_some(mark, |control, mark| control.leading(mark))
                    .label(label)
                    .max_label_width(224.)
                    .suffix(suffix)
                    .pressed(self.picker_open)
                    .disabled(self.sending || self.is_rewinding(cx))
                    .tooltip(tooltip)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if Self::guard_reopen(&mut this.picker_dismissed) {
                            return;
                        }
                        this.toggle_picker(window, cx);
                    })),
            )
            .when(self.picker_open, |this| {
                this.child(floating(
                    Float::AboveStart,
                    div()
                        .mb(px(4.))
                        .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                            this.picker_open = false;
                            this.picker_dismissed = Some(Instant::now());
                            this.focus(window, cx);
                            cx.notify();
                        }))
                        .child(self.picker.clone()),
                ))
            })
            .into_any_element()
    }

    fn render_permissions_chip(&self, resolved: &Resolved, cx: &mut Context<Self>) -> AnyElement {
        let mode = resolved.runtime_mode;
        let modes = resolved.runtime_modes.clone();
        let rewinding = self.is_rewinding(cx);
        div()
            .relative()
            .child(
                ComposerControl::new("permissions")
                    .icon(runtime_mode_icon(mode))
                    .label(runtime_mode_label(mode))
                    .emphasized(true)
                    .pressed(self.permissions_open)
                    .disabled(rewinding)
                    .tooltip(format!("Permissions: {}. {}", runtime_mode_label(mode), runtime_mode_description(mode)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.is_rewinding(cx) || Self::guard_reopen(&mut this.menu_dismissed) {
                            return;
                        }
                        this.permissions_open = !this.permissions_open;
                        cx.notify();
                    })),
            )
            .when(self.permissions_open && !rewinding, |this| {
                this.child(floating(
                    Float::AboveStart,
                    menu_surface(cx)
                        .occlude()
                        .mb(px(4.))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.permissions_open = false;
                            this.menu_dismissed = Some(Instant::now());
                            cx.notify();
                        }))
                        .child(permissions_section(mode, &modes, cx, |mode, this: &mut Self, cx: &mut Context<Self>| {
                            this.set_runtime_mode(mode, cx)
                        })),
                ))
            })
            .into_any_element()
    }

    fn render_primary(&self, resolved: &Resolved, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let running = self.snapshot.running;
        let can_interrupt = self.snapshot.can_interrupt;
        let can_operate = self.snapshot.can_operate_thread;
        let rewinding = self.is_rewinding(cx);
        let stop_pending = self.stop_phase == StopPhase::Pending;
        let stop_retained = matches!(self.stop_phase, StopPhase::Pending | StopPhase::TimedOut);
        let stop_button = || {
            div()
                .flex_none()
                .debug_selector(|| "stop".to_owned())
                .child(
                    RoundButton::new("stop", RoundKind::Stop, if stop_pending { "Stopping…" } else { "Interrupt" })
                        .disabled(rewinding || !can_operate || stop_pending)
                        .on_click(cx.listener(|this, _, _, cx| this.request_stop(cx))),
                )
        };
        let voice_blocks = self.voice.read(cx).blocks_submission();
        let voice_preparing = self.voice.read(cx).preparing();
        let speech_pill = crate::voice::render_speech_recording_pill(
            "composer",
            &self.voice,
            crate::voice::VoiceUiSize::Regular,
            self.sending || rewinding,
            cx,
        );
        let speech_button = crate::voice::render_speech_button(
            "composer",
            &self.voice,
            crate::voice::VoiceUiSize::Regular,
            self.sending || rewinding,
            cx,
        );
        let with_voice = |primary: AnyElement| -> AnyElement {
            h_flex()
                .items_center()
                .gap(px(8.))
                .child(speech_pill)
                .child(speech_button)
                .child(primary)
                .into_any_element()
        };
        if let (Some(input), Some(progress)) = (self.active_input(), self.progress(cx)) {
            let responding = rewinding || !self.can_answer(cx) || self.question_responding(&input.request_id, cx) || !input.can_respond();
            let enabled = if progress.is_last_question { progress.is_complete } else { progress.can_advance };
            return with_voice(
                h_flex()
                    .gap(px(8.))
                    .when(progress.question_index > 0, |this| {
                        this.child(
                            Button::new("question-previous", ButtonVariant::Outline, ButtonSize::Sm)
                                .label("Previous")
                                .disabled(responding)
                                .on_click(cx.listener(|this, _, window, cx| this.previous(window, cx))),
                        )
                    })
                    .child(
                        Pill::new("question-submit", requests::primary_action_label(&progress, responding))
                            .disabled(!can_operate || responding || !enabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.stop_phase != StopPhase::Landed {
                                    this.advance(window, cx);
                                }
                            })),
                    )
                    .when(can_interrupt || stop_retained, |this| this.child(stop_button()))
                    .into_any_element(),
            );
        }
        let has_text = self.has_text || !self.staged().is_empty() || self.current_reply_to().is_some();
        let editing_queued = self.editing_queued().is_some();
        if (can_interrupt || stop_retained) && !has_text && !editing_queued && !voice_blocks {
            return with_voice(stop_button().into_any_element());
        }
        let show_resume = self.snapshot.can_resume && !has_text && !editing_queued && !voice_blocks;
        if show_resume {
            let resume_btn = RoundButton::new("send", RoundKind::Resume, "Resume thread")
                .disabled(self.sending || !can_operate || rewinding)
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.stop_phase != StopPhase::Landed {
                        this.resume(window, cx);
                    }
                }));
            let elem = if stop_retained {
                h_flex().gap(px(8.)).child(resume_btn).child(stop_button()).into_any_element()
            } else {
                resume_btn.into_any_element()
            };
            return with_voice(elem);
        }
        let fresh_armed = self.is_fresh_session_armed(cx);
        let compact_tokens = (!editing_queued && !fresh_armed)
            .then(|| self.compact_before_send_tokens(resolved, cx))
            .flatten()
            .map(|tokens| t3_core::usage::context_window::format_context_tokens(Some(tokens)));
        let keep_full_history = self.is_keep_full_history(cx);
        let compacts_before_send = compact_tokens.is_some() && !keep_full_history;
        let attachment_block = if self.snapshot.pending.approvals.is_empty() {
            t3_core::composer::attachments::send_block_reason(&self.staged_attachments())
        } else {
            t3_core::composer::attachments::approval_redirect_block_reason(&self.staged_attachments())
        };
        let settings = self.state.read(cx).client_settings;
        let follow_up = if self.snapshot.pending.approvals.is_empty() { settings.follow_up_behavior } else { rules::FollowUpBehavior::Steer };
        let can_steer = !self.snapshot.compaction_blocks_steer && !fresh_armed;
        let dispatch = rules::dispatch_mode(running, false, follow_up, can_steer);
        let alternate = rules::dispatch_mode(running, true, follow_up, can_steer);
        let (kind, label) = match (editing_queued, compacts_before_send, running, dispatch) {
            (true, _, _, _) => (RoundKind::Send, "Update queued message"),
            (false, true, _, _) => (RoundKind::Send, "Compact and send"),
            (false, false, true, ComposerDispatch::Queue) => (RoundKind::Queue, "Queue message"),
            (false, false, true, _) => (RoundKind::Steer, "Steer message"),
            _ => (RoundKind::Send, "Submit message"),
        };
        let blocked = self.snapshot.setup_blocks_send || rewinding || !can_operate;
        let disabled = self.sending || voice_preparing || (!has_text && !voice_blocks) || (resolved.model.is_none() && resolved.multi_models.is_empty()) || attachment_block.is_some() || blocked;
        let tooltip: SharedString = if resolved.model.is_none() && resolved.multi_models.is_empty() {
            "No provider available".into()
        } else if rewinding {
            "Reverting checkpoint...".into()
        } else if self.snapshot.setup_blocks_send {
            t3_core::worktree_setup::SEND_BLOCKED.into()
        } else if let Some(reason) = attachment_block {
            reason.into()
        } else if self.sending {
            if editing_queued { "Updating queued message".into() } else { "Submitting message".into() }
        } else if let Some(tokens) = compact_tokens.as_deref().filter(|_| compacts_before_send) {
            format!("Summarize {tokens} tokens of history, then send").into()
        } else if running && can_steer && !editing_queued {
            let action = |mode| if mode == ComposerDispatch::Queue { "queue" } else { "steer" };
            let alt_key = crate::keymap::label_in(
                "composer.sendAlternate",
                &t3_core::keybindings::ShortcutMatchContext::default().with("composerFocus", true).with("turnRunning", true),
                cx,
            );
            let via = alt_key.map_or_else(|| "⌘-click".to_owned(), |shortcut| format!("{shortcut} or ⌘-click"));
            format!("Click to {}, {via} to {}", action(dispatch), action(alternate)).into()
        } else {
            label.into()
        };
        let submit_btn = div().flex_none().debug_selector(|| "send".to_owned()).child(
            RoundButton::new("send", kind, tooltip)
                .disabled(disabled)
                .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                    if this.stop_phase == StopPhase::Landed {
                        return;
                    }
                    let modifier = event.modifiers().platform || event.modifiers().control;
                    let draft = matches!(this.snapshot.target, Some(Target::Draft(_)));
                    let key = rules::EnterKey { modifier, ..rules::EnterKey::default() };
                    let intent = rules::submit_intent(key, this.snapshot.running, draft, rules::SendShortcut::Enter, "");
                    if let Some(intent) = intent {
                        if this.voice.read(cx).blocks_submission() {
                            if !this.voice.read(cx).preparing() {
                                this.voice_pending_intent = Some(intent);
                                this.voice.update(cx, |voice, cx| {
                                    voice.submit_after_dictation(cx);
                                });
                            }
                        } else {
                            this.submit(intent, window, cx);
                        }
                    }
                })),
        );
        let compact_toggle = compact_tokens.map(|tokens| {
            let toggle_tooltip = if keep_full_history {
                format!("Next send keeps all {tokens} tokens. Click to compact first")
            } else {
                format!("Next send compacts {tokens} tokens first. Click to keep full history")
            };
            div().flex_none().debug_selector(|| "compact-before-send".to_owned()).child(
                ComposerControl::new("compact-before-send")
                    .icon(Icon::Minimize2)
                    .label(if keep_full_history { "Keep" } else { "Compact" })
                    .suffix(div().text_color(hsla(p.muted_foreground)).child(SharedString::from(tokens)))
                    .active(!keep_full_history)
                    .disabled(!can_operate)
                    .tooltip(toggle_tooltip)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_keep_full_history(cx))),
            )
        });
        if compact_toggle.is_none() && !stop_retained {
            return with_voice(submit_btn.into_any_element());
        }
        h_flex()
            .gap(px(8.))
            .children(compact_toggle)
            .child(with_voice(submit_btn.into_any_element()))
            .when(stop_retained, |this| this.child(stop_button()))
            .into_any_element()
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.question_markdown.begin_frame();
        self.sync_input_state(window, cx);
        self.sync_rich_decorations(cx);
        let resolved = self.resolve(cx);
        let rewinding = self.is_rewinding(cx);
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let error = self.error.clone().or_else(|| self.snapshot.last_error.clone());
        let question = self.active_input().cloned();
        let surface = hsla(p.composer_surface);
        let outline = hsla(p.composer_outline);
        let show_permissions = resolved.runtime_mode != resolved.default_runtime_mode;
        let show_attach = self.shows_attach_button(cx);
        let (drop_fill, drop_ring) = (hsla(p.accent).opacity(0.45), hsla(p.primary).opacity(0.7));
        let gutter = if self.fixed.is_some() { 12. } else { 48. };
        div().w_full().flex().justify_center().px(px(gutter)).pb(px(if self.fixed.is_some() { 12. } else { 16. })).pt(px(8.)).child(
            v_flex()
                .key_context("Composer")
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "enter" {
                        this.enter_repeat = event.is_held;
                        if !event.is_held
                            && !event.keystroke.modifiers.shift
                            && this.voice.read(cx).blocks_submission()
                            && !this.voice.read(cx).preparing()
                        {
                            let modifier = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
                            let draft = matches!(this.snapshot.target, Some(Target::Draft(_)));
                            let settings = this.state.read(cx).client_settings;
                            let prompt = this.prompt(cx);
                            if let Some(intent) = rules::submit_intent(
                                rules::EnterKey { modifier, repeat: false, composing: false },
                                this.snapshot.running,
                                draft,
                                settings.send_shortcut,
                                &prompt,
                            ) {
                                this.voice_pending_intent = Some(intent);
                                this.voice.update(cx, |voice, cx| {
                                    voice.submit_after_dictation(cx);
                                });
                                cx.stop_propagation();
                            }
                        }
                    }
                }))
                .on_action(cx.listener(|this, _: &OutdentList, window, cx| this.outdent_list(window, cx)))
                // Each composer takes its own shortcuts, so a side chat's never reach the open thread's.
                .on_action(cx.listener(|this, _: &ToggleModelPicker, window, cx| this.toggle_picker(window, cx)))
                .on_action(cx.listener(|this, _: &super::OpenWorkspace, window, cx| this.open_workspace(window, cx)))
                .on_action(cx.listener(|this, _: &super::OpenHost, window, cx| this.open_host(window, cx)))
                .on_action(cx.listener(|this, _: &super::CycleHost, window, cx| this.cycle_host(window, cx)))
                .on_action(cx.listener(|this, _: &super::UsePreviousWorktree, window, cx| this.use_previous_worktree(window, cx)))
                .on_action(cx.listener(|this, _: &super::SendAlternate, window, cx| this.send_alternate(window, cx)))
                .on_action(cx.listener(|this, _: &super::SteerQueuedMessage, window, cx| {
                    if !this.steer_queued_message(window, cx) {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|this, _: &super::EditQueuedMessage, window, cx| {
                    if !this.edit_queued_message(window, cx) {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|this, _: &OpenPermissions, window, cx| this.open_permissions(window, cx)))
                // The open menu takes these keys before the prompt does (`composerCommandKey`).
                .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                    if this.workspace_key(MenuKey::Up, window, cx) || this.stash_key(MenuKey::Up, window, cx) || this.menu_key(MenuKey::Up, window, cx) || this.history_key(true, window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                    if this.workspace_key(MenuKey::Down, window, cx) || this.stash_key(MenuKey::Down, window, cx)
                        || this.menu_key(MenuKey::Down, window, cx)
                        || this.history_key(false, window, cx)
                    {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                    if this.workspace_key(MenuKey::Accept, window, cx) {cx.stop_propagation(); return;}
                    if !action.secondary
                        && !action.shift
                        && (this.stash_key(MenuKey::Accept, window, cx) || this.menu_key(MenuKey::Accept, window, cx))
                    {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                    if this.menu_key(MenuKey::Accept, window, cx)
                        || this.accept_prompt_suggestion(window, cx)
                        || this.indent_list_or_code(window, cx)
                    {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                    if this.workspace_key(MenuKey::Dismiss, window, cx)
                        || this.stash_key(MenuKey::Dismiss, window, cx)
                        || this.menu_key(MenuKey::Dismiss, window, cx)
                        || this.dismiss_prompt_suggestion(cx)
                    {
                        cx.stop_propagation();
                    }
                }))
                .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    let mod_key = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
                    if mod_key
                        && !event.keystroke.modifiers.alt
                        && !event.keystroke.modifiers.shift
                        && event.keystroke.key == "backspace"
                        && this.stash_key(MenuKey::Delete, window, cx)
                    {
                        cx.stop_propagation();
                    }
                }))
                // Formatting shortcuts run in bubble phase so app keybindings (like Mod+B sidebar
                // toggle) always take precedence first (#29445, #34493).
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    let mod_key = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
                    if !mod_key || event.keystroke.modifiers.alt || !this.input.read(cx).focus_handle(cx).is_focused(window) {
                        return;
                    }
                    let format = match (event.keystroke.key.as_str(), event.keystroke.modifiers.shift) {
                        ("b", false) => Some(t3_core::composer::rich_text::InlineFormat::Bold),
                        ("i", false) => Some(t3_core::composer::rich_text::InlineFormat::Italic),
                        ("e", false) => Some(t3_core::composer::rich_text::InlineFormat::Code),
                        ("s", true) => Some(t3_core::composer::rich_text::InlineFormat::Strike),
                        _ => None,
                    };
                    if let Some(format) = format {
                        this.toggle_inline_format(format, window, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_action(cx.listener(|this, _: &StashPrompt, window, cx| this.stash_prompt(window, cx)))
                .capture_action(cx.listener(|this, _: &Copy, _, cx| {
                    if this.copy_context_selection(cx) { cx.stop_propagation(); }
                }))
                .capture_action(cx.listener(|this, _: &Paste, window, cx| {
                    if this.input.read(cx).focus_handle(cx).is_focused(window) && this.paste(window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .on_action(cx.listener(|this, _: &PasteAsText, window, cx| this.paste_as_text(window, cx)))
                .on_action(cx.listener(|this, _: &Escape, _, cx| {
                    if this.voice.read(cx).state().is_busy() {
                        this.voice.update(cx, |voice, cx| voice.cancel(cx));
                        cx.stop_propagation();
                        return;
                    }
                    if this.close_context_details(cx) {
                        return;
                    }
                    if this.workspace.open || this.permissions_open {
                        this.workspace.open = false;
                        this.permissions_open = false;
                        cx.notify();
                    } else {
                        cx.propagate();
                    }
                }))
                .relative()
                .w_full()
                .when_some(crate::theme::chat_max_width(cx), |this, width| this.max_w(width))
                .children(self.render_queue(cx))
                .children(error.map(|error| {
                    div().id("composer-stream-error").debug_selector(|| "composer-stream-error".to_owned()).pb(px(6.)).text_xs().text_color(hsla(p.error_foreground)).child(SharedString::from(error))
                }))
                .when(!matches!(self.snapshot.target, Some(Target::Draft(_))) && self.fixed.is_none(), |this| this.child(self.limit_recovery.clone()))
                .when(!matches!(self.snapshot.target, Some(Target::Draft(_))), |this| this.children(self.render_branch_mismatch_banner(cx)))
                .when(!matches!(self.snapshot.target, Some(Target::Draft(_))), |this| this.children(self.render_usage_limits_banner(false, cx)))
                .children(self.render_fresh_session_banner(cx))
                .when_some(question, |this, input| this.child(self.render_question(&input, cx)))
                .when(self.active_input().is_none(), |this| {
                    this.children(self.render_reply_banner(cx))
                })
                .children(self.render_stash_badge(cx))
                .children(self.render_stash_menu(cx))
                .when(self.voice.read(cx).preview().is_some(), |this| {
                    this.child(
                        div()
                            .pb(px(8.))
                            .child(crate::voice::render_voice_preview("composer", &self.voice, cx)),
                    )
                })
                .child(
                    div().relative().children(self.render_menu(cx)).child(
                    v_flex()
                        .id("composer-card")
                        .when(rewinding, |this| this.opacity(0.65))
                        .rounded(px(16.))
                        .border_1()
                        .border_color(outline.opacity(0.))
                        .relative()
                        .child(crate::glass::backdrop_with_highlight(16., surface, outline, dark.then(||hsla(p.composer_highlight)), cx))
                        .when(!dark, |this| {
                            this.shadow(vec![gpui::BoxShadow {
                                color: gpui::black().opacity(0.05),
                                offset: gpui::point(px(0.), px(1.)),
                                blur_radius: px(2.),
                                spread_radius: px(0.),
                                inset: false,
                            }])
                        })
                        // Files dragged in from Finder (`isDragOverComposer`).
                        .when(!rewinding, |this| {
                            this.drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_fill).border_color(drop_ring))
                                .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.on_drop_paths(paths, window, cx)))
                        })
                        .child(
                            div()
                                .px(px(16.))
                                .pt(px(16.))
                                .pb(px(8.))
                                .children(self.render_attachments(cx))
                                .children(self.render_context_chips(cx))
                                .children(self.render_citation_chips(cx))
                                .child(Editor::new(&self.input).appearance(false)
                                    .h(px(self.composer_editor_height(cx)))
                                    .font_family(crate::theme::font_family(t3_core::client_settings::typography::FontRole::Prompt, cx))
                                    .text_size(px(crate::theme::font_size(t3_core::client_settings::typography::FontRole::Prompt, cx)))
                                    .line_height(px(crate::theme::font_size(t3_core::client_settings::typography::FontRole::Prompt, cx) * 1.625))),
                        )
                        .when(self.answering.is_none(), |this| {
                            this.when_some(self.prompt_length_error.clone(), |this, message| {
                                this.child(
                                    div()
                                        .id("prompt-length-validation")
                                        .debug_selector(|| "prompt-length-validation".to_owned())
                                        .px(px(16.))
                                        .pb(px(8.))
                                        .text_xs()
                                        .text_color(hsla(p.destructive))
                                        .child(message),
                                )
                            })
                        })
                        .child(
                            h_flex()
                                .px(px(8.))
                                .pb(px(8.))
                                .gap(px(2.))
                                .when(show_attach, |this| {
                                    this.child(
                                        div().mr(px(8.)).child(
                                            Button::new("attach-files", ButtonVariant::Ghost, ButtonSize::IconSm)
                                                .icon(Icon::Paperclip)
                                                .disabled(rewinding)
                                                .tooltip("Attach files")
                                                .on_click(cx.listener(|this, _, window, cx| this.pick_files(window, cx))),
                                        ),
                                    )
                                })
                                .child(self.render_model_trigger(&resolved, cx))
                                .when(show_permissions, |this| this.child(self.render_permissions_chip(&resolved, cx)))
                                .child(div().flex_1().min_w(px(8.)))
                                .child(self.render_primary(&resolved, cx)),
                        ),
                    ),
                )
                .children(self.render_workspace(cx).map(|control| h_flex().pt(px(8.)).px(px(8.)).child(control)))
                .when(matches!(self.snapshot.target, Some(Target::Draft(_))), |this| {
                    this.children(self.render_usage_limits_banner(true, cx).map(render_draft_usage_limits_overlay))
                })
                .child(crate::voice::render_voice_setup_modal("composer", &self.voice, cx)),
        )
    }
}

pub(super) fn render_draft_usage_limits_overlay(banner: AnyElement) -> AnyElement {
    div()
        .id("draft-usage-limits-overlay")
        .debug_selector(|| "draft-usage-limits-overlay".to_owned())
        .absolute()
        .left_0()
        .right_0()
        .top_full()
        .pt(px(16.))
        .child(banner)
        .into_any_element()
}
