//! The SSH password prompt (`apps/web/src/components/desktop/SshPasswordPromptDialog.tsx`): asks
//! for the password or verification code an SSH host wants, with its countdown, and hands the
//! answer to the `ssh` waiting for it (`t3_client::ssh`). Prompts queue in
//! `AppState::ssh_prompts` and show one at a time, over the connect screen or the workspace.

use std::time::{Duration, Instant};

use gpui::{
    App, BoxShadow, Context, Entity, FontWeight, IntoElement, MouseButton, ParentElement, Render, Styled, StyledText, Subscription, Task, TextRun,
    Window, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, v_flex};
use t3_core::ssh::{describe_prompt_target, format_remaining_seconds};

use crate::app_state::{AppEvent, AppState};
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

const EXPIRED: &str = "This SSH verification prompt expired. Try connecting again.";

pub struct SshPromptDialog {
    state: Entity<AppState>,
    password: Entity<InputState>,
    /// The queued prompt the field and its error belong to.
    showing: Option<u64>,
    error: Option<&'static str>,
    /// Repaints the countdown once a second while a prompt is open.
    _tick: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SshPromptDialog {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let subscriptions = vec![
            cx.subscribe_in(&password, window, |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { .. } => this.submit(cx),
                InputEvent::Change if this.error.is_some() => {
                    this.error = None;
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if matches!(event, AppEvent::SshPrompt) {
                    this.follow_queue(window, cx);
                }
            }),
        ];
        SshPromptDialog { state, password, showing: None, error: None, _tick: None, _subscriptions: subscriptions }
    }

    /// A new prompt starts with an empty, focused field and its own countdown.
    fn follow_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let front = self.state.read(cx).ssh_prompts.front().map(|(sequence, _)| *sequence);
        if front == self.showing {
            return;
        }
        self.showing = front;
        self.error = None;
        self.password.update(cx, |input, cx| input.set_value("", window, cx));
        self._tick = front.map(|_| {
            self.password.update(cx, |input, cx| input.focus(window, cx));
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    let ticking = this.update(cx, |this, cx| {
                        cx.notify();
                        !this.expired(cx)
                    });
                    if !matches!(ticking, Ok(true)) {
                        return;
                    }
                }
            })
        });
        cx.notify();
    }

    /// Past its deadline, or nothing waits for the answer anymore.
    fn expired(&self, cx: &App) -> bool {
        self.state.read(cx).ssh_prompts.front().is_none_or(|(_, request)| !request.is_pending())
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(sequence) = self.showing else { return };
        if self.expired(cx) {
            self.error = Some(EXPIRED);
            return cx.notify();
        }
        let answer = self.password.read(cx).unmask_value().to_string();
        if let Some(request) = self.state.update(cx, |state, cx| state.take_ssh_prompt(sequence, cx)) {
            request.respond(Some(answer));
        }
    }

    /// Cancel answers "no"; an expired prompt is only dismissed.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(sequence) = self.showing else { return };
        if let Some(request) = self.state.update(cx, |state, cx| state.take_ssh_prompt(sequence, cx)) {
            request.respond(None);
        }
    }
}

/// The description, with the target set in the mono font as the web's `<code>`.
fn description(target: &str, cx: &App) -> StyledText {
    let (sans, mono) = (cx.theme().font_family.clone(), cx.theme().mono_font_family.clone());
    let color = hsla(palette(cx).muted_foreground);
    let segments = [
        ("Enter the password or verification code required by ", false),
        (target, true),
        (". T3 Code passes your response to the local SSH process and keeps it in memory to reuse for this host. T3 Code does not write it to disk.", false),
    ];
    let runs = segments
        .iter()
        .map(|(text, code)| TextRun {
            len: text.len(),
            font: gpui::font(if *code { mono.clone() } else { sans.clone() }),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        })
        .collect();
    StyledText::new(segments.iter().map(|(text, _)| *text).collect::<String>()).with_runs(runs)
}

impl Render for SshPromptDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let Some((_, request)) = state.ssh_prompts.front().filter(|(sequence, _)| Some(*sequence) == self.showing) else {
            return div().into_any_element();
        };
        let target = describe_prompt_target(&request.destination, request.username.as_deref());
        let prompt = request.prompt.clone();
        let expired = !request.is_pending();
        let remaining = request.expires_at.saturating_duration_since(Instant::now());
        let remaining = format_remaining_seconds(remaining.as_millis().div_ceil(1000) as u64);
        let error = if expired { Some(EXPIRED) } else { self.error };
        let p = palette(cx);
        deferred(
            div()
                .id("ssh-prompt-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.cancel(cx)))
                .child(
                    div()
                        .id("ssh-prompt")
                        .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                            this.cancel(cx);
                            cx.stop_propagation();
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w_full()
                        .max_w(px(448.))
                        .relative()
                        .rounded(px(16.))
                        .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                        .shadow(vec![BoxShadow { color: gpui_hsla(0., 0., 0., 0.65), offset: point(px(0.), px(24.)), blur_radius: px(64.), spread_radius: px(-24.), inset: false }])
                        .child(
                            v_flex()
                                .p(px(24.))
                                .gap(px(8.))
                                .child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child("SSH verification required"))
                                .child(div().text_sm().child(description(&target, cx))),
                        )
                        .child(
                            v_flex()
                                .px(px(24.))
                                .pb(px(24.))
                                .gap(px(12.))
                                .child(
                                    v_flex()
                                        .gap(px(8.))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap(px(12.))
                                                .child(div().min_w_0().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(prompt))
                                                .child(if expired {
                                                    div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.destructive)).child("Expired")
                                                } else {
                                                    div().flex_none().text_xs().text_color(hsla(p.muted_foreground)).child(remaining)
                                                }),
                                        )
                                        .child(Input::new(&self.password).disabled(expired)),
                                )
                                .child(match error {
                                    Some(error) => div().text_sm().text_color(hsla(p.destructive)).child(error),
                                    None => div()
                                        .text_sm()
                                        .text_color(hsla(p.muted_foreground))
                                        .child("SSH keys may replace a password, but your server can still require a verification code."),
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .px(px(24.))
                                .py(px(16.))
                                .border_t_1()
                                .border_color(hsla(p.border))
                                .rounded_b(px(15.))
                                .bg(hsla(p.muted).opacity(0.72))
                                .child(
                                    Button::new("ssh-prompt-cancel", ButtonVariant::Outline, ButtonSize::Default)
                                        .label(if expired { "Dismiss" } else { "Cancel" })
                                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                                )
                                .child(
                                    Button::new("ssh-prompt-continue", ButtonVariant::Default, ButtonSize::Default)
                                        .label("Continue")
                                        .disabled(expired)
                                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                                ),
                        ),
                ),
        )
        .with_priority(60)
        .into_any_element()
    }
}
