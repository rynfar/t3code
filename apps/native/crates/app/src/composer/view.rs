//! The composer's layout (design-system §7.9, `ChatComposer.tsx`, `ComposerControlsBar.tsx`,
//! `ComposerFooterControls.tsx`, `ComposerPrimaryActions.tsx`): attached banners, the glass card
//! with the prompt, and the footer with the model picker, permissions, mode and send controls.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::input::{Escape, Textarea};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands::ComposerDispatch;
use t3_core::composer::{self as rules, runtime_mode_description, runtime_mode_label};
use t3_core::requests;
use t3_core::server_config::InteractionMode;
use t3_core::traits;

use super::controls::{Button, ButtonSize, ButtonVariant, ComposerControl, Float, Pill, RoundButton, RoundKind, floating, menu_surface};
use super::model_picker::permissions_section;
use super::{Composer, Resolved, Target, TogglePlanMode, runtime_mode_icon};
use crate::icons::Icon;
use crate::provider_icon::{BadgeSize, ProviderIcon};
use crate::theme::{hsla, palette};

/// The web's collapsed-composer prompt. Its full placeholder names @ files, $ skills and
/// / commands, which come with the composer menus (ledger CO-TRIGGERS).
pub const DEFAULT_PLACEHOLDER: &str = "Ask anything...";

/// A click that closed a popup must not reopen it through the trigger it landed on.
const REOPEN_GUARD: Duration = Duration::from_millis(300);

impl Composer {
    /// `ChatComposer` placeholders, most specific first.
    fn placeholder(&self) -> &'static str {
        if let Some(approval) = self.snapshot.pending.approvals.first() {
            return if approval.supports_feedback {
                "Send feedback to decline this request"
            } else {
                "Send a message to decline and redirect the agent"
            };
        }
        if let Some((_, question)) = self.active_question() {
            return if question.allows_custom_answer() {
                "Type your own answer, or leave this blank to use the selected option"
            } else {
                "Choose an option above"
            };
        }
        DEFAULT_PLACEHOLDER
    }

    /// Placeholder and disabled state follow what the agent waits on; written only on change.
    pub(super) fn sync_input_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self.placeholder();
        let disabled = self.active_question().is_some_and(|(request_id, question)| {
            !question.allows_custom_answer() || self.responding.contains(&request_id)
        });
        if self.input_state == Some((placeholder, disabled)) {
            return;
        }
        self.input_state = Some((placeholder, disabled));
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            input.set_disabled(disabled, cx);
        });
    }

    pub(super) fn guard_reopen(dismissed: &mut Option<Instant>) -> bool {
        dismissed.take().is_some_and(|at| at.elapsed() < REOPEN_GUARD)
    }

    fn render_model_trigger(&self, resolved: &Resolved, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let suffix = traits::effort_suffix(&resolved.driver, &resolved.descriptors);
        let surface = hsla(if cx.theme().is_dark() { p.surface_raised } else { p.card });
        let mark = resolved.mark.clone().map(|mark| ProviderIcon::new(mark).badge(BadgeSize::Compact, surface));
        let label = if resolved.model_label.is_empty() { "Choose model".to_owned() } else { resolved.model_label.clone() };
        let suffix_color = if self.picker_open { hsla(p.foreground) } else { hsla(p.muted_foreground) };
        let suffix = h_flex()
            .gap(px(6.))
            .text_color(suffix_color)
            .children(suffix.word.map(|word| div().max_w(px(96.)).truncate().child(word)))
            .when(suffix.fast || suffix.ultrafast, |this| this.child(Icon::ZapFill.render(px(12.), suffix_color)));
        let tooltip = [Some(label.clone()), Some("⌘⇧M".to_owned())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
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
                    .disabled(self.sending)
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
        div()
            .relative()
            .child(
                ComposerControl::new("permissions")
                    .icon(runtime_mode_icon(mode))
                    .label(runtime_mode_label(mode))
                    .emphasized(true)
                    .pressed(self.permissions_open)
                    .tooltip(format!("Permissions: {}. {}", runtime_mode_label(mode), runtime_mode_description(mode)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if Self::guard_reopen(&mut this.menu_dismissed) {
                            return;
                        }
                        this.permissions_open = !this.permissions_open;
                        cx.notify();
                    })),
            )
            .when(self.permissions_open, |this| {
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

    fn render_plan_toggle(&self, resolved: &Resolved, cx: &mut Context<Self>) -> AnyElement {
        let plan = resolved.interaction_mode == InteractionMode::Plan;
        let tooltip = if plan { "Plan mode. Click to return to normal build mode" } else { "Default mode. Click to enter plan mode" };
        ComposerControl::new("interaction-mode")
            .icon(if plan { Icon::PencilRuler } else { Icon::Bot })
            .label(if plan { "Plan" } else { "Build" })
            .emphasized(plan)
            .tooltip(tooltip)
            .on_click(cx.listener(|this, _, window, cx| this.toggle_plan_mode(window, cx)))
            .into_any_element()
    }

    fn render_primary(&self, resolved: &Resolved, cx: &mut Context<Self>) -> AnyElement {
        let running = self.snapshot.running;
        let stop = || {
            RoundButton::new("stop", RoundKind::Stop, "Interrupt")
                .on_click(cx.listener(|this, _, _, cx| {
                    let fixed = this.fixed.clone();
                    this.state.update(cx, |state, cx| match fixed {
                        Some(id) => state.stop_thread(&id, cx),
                        None => state.stop_selected_thread(cx),
                    })
                }))
        };
        if let (Some(input), Some(progress)) = (self.active_input(), self.progress()) {
            let responding = self.responding.contains(&input.request_id) || !input.can_respond();
            let enabled = if progress.is_last_question { progress.is_complete } else { progress.can_advance };
            return h_flex()
                .gap(px(8.))
                .when(running, |this| this.child(stop()))
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
                        .disabled(responding || !enabled)
                        .on_click(cx.listener(|this, _, window, cx| this.advance(window, cx))),
                )
                .into_any_element();
        }
        let has_text = self.has_text;
        if running && !has_text {
            return stop().into_any_element();
        }
        let settings = self.state.read(cx).client_settings;
        let follow_up = if self.snapshot.pending.approvals.is_empty() { settings.follow_up_behavior } else { rules::FollowUpBehavior::Steer };
        let can_steer = !self.snapshot.compacting;
        let dispatch = rules::dispatch_mode(running, false, follow_up, can_steer);
        let alternate = rules::dispatch_mode(running, true, follow_up, can_steer);
        let (kind, label) = match (running, dispatch) {
            (true, ComposerDispatch::Queue) => (RoundKind::Queue, "Queue message"),
            (true, _) => (RoundKind::Steer, "Steer message"),
            _ => (RoundKind::Send, "Submit message"),
        };
        let disabled = self.sending || !has_text || resolved.model.is_none();
        let tooltip: SharedString = if resolved.model.is_none() {
            "No provider available".into()
        } else if self.sending {
            "Submitting message".into()
        } else if running && can_steer {
            let action = |mode| if mode == ComposerDispatch::Queue { "queue" } else { "steer" };
            format!("Click to {}, ⌘-click to {}", action(dispatch), action(alternate)).into()
        } else {
            label.into()
        };
        RoundButton::new("send", kind, tooltip)
            .disabled(disabled)
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                let modifier = event.modifiers().platform || event.modifiers().control;
                let draft = matches!(this.snapshot.target, Some(Target::Draft(_)));
                let key = rules::EnterKey { modifier, ..rules::EnterKey::default() };
                let intent = rules::submit_intent(key, this.snapshot.running, draft, rules::SendShortcut::Enter, "");
                if let Some(intent) = intent {
                    this.submit(intent, window, cx);
                }
            }))
            .into_any_element()
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_input_state(window, cx);
        let resolved = self.resolve(cx);
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let error = self.error.clone().or_else(|| self.state.read(cx).last_error.clone());
        let approval = self.snapshot.pending.approvals.first().cloned();
        let approval_count = self.snapshot.pending.approvals.len();
        let question = self.active_input().cloned();
        let surface = hsla(if dark { p.surface_raised } else { p.card });
        let outline = if dark { gpui::white().opacity(0.05) } else { gpui::black().opacity(0.08) };
        let show_permissions = resolved.runtime_mode != resolved.default_runtime_mode;
        let gutter = if self.fixed.is_some() { 12. } else { 48. };
        div().w_full().flex().justify_center().px(px(gutter)).pb(px(if self.fixed.is_some() { 12. } else { 16. })).pt(px(8.)).child(
            v_flex()
                .key_context("Composer")
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, _| {
                    if event.keystroke.key == "enter" {
                        this.enter_repeat = event.is_held;
                    }
                }))
                .on_action(cx.listener(|this, _: &TogglePlanMode, window, cx| this.toggle_plan_mode(window, cx)))
                .on_action(cx.listener(|this, _: &Escape, _, cx| {
                    if this.permissions_open || this.approval_menu_open {
                        this.permissions_open = false;
                        this.approval_menu_open = false;
                        cx.notify();
                    } else {
                        cx.propagate();
                    }
                }))
                .w_full()
                .when_some(crate::theme::chat_max_width(cx), |this, width| this.max_w(width))
                .children(error.map(|error| {
                    div().pb(px(6.)).text_xs().text_color(hsla(p.error_foreground)).child(SharedString::from(error))
                }))
                .when(self.fixed.is_none(), |this| this.child(self.limit_recovery.clone()))
                .children(question.as_ref().map(|input| self.render_question(input, cx)))
                .when(question.is_none(), |this| {
                    this.children(approval.as_ref().map(|approval| self.render_approval(approval, approval_count, cx)))
                })
                .child(
                    v_flex()
                        .rounded(px(16.))
                        .border_1()
                        .border_color(outline)
                        .bg(surface)
                        .when(!dark, |this| {
                            this.shadow(vec![gpui::BoxShadow {
                                color: gpui::black().opacity(0.05),
                                offset: gpui::point(px(0.), px(1.)),
                                blur_radius: px(2.),
                                spread_radius: px(0.),
                                inset: false,
                            }])
                        })
                        .child(
                            div()
                                .px(px(16.))
                                .pt(px(16.))
                                .pb(px(8.))
                                .child(Textarea::new(&self.input).appearance(false).line_height(px(22.75))),
                        )
                        .child(
                            h_flex()
                                .px(px(8.))
                                .pb(px(8.))
                                .gap(px(2.))
                                .child(self.render_model_trigger(&resolved, cx))
                                .when(show_permissions, |this| this.child(self.render_permissions_chip(&resolved, cx)))
                                .when(resolved.plan_enabled, |this| this.child(self.render_plan_toggle(&resolved, cx)))
                                .child(div().flex_1().min_w(px(8.)))
                                .child(self.render_primary(&resolved, cx)),
                        ),
                ),
        )
    }
}
