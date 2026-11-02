//! The usage-limit recovery banner over the composer (`UsageLimitRecoveryBanner.tsx`): when the
//! limit resets, with Resume at reset and Snooze until reset. Its own view, so it redraws alone
//! and can wake at the reset time, when Snooze until reset stops being offered.

use std::time::Duration;

use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Task, Window, div, px};
use gpui_component::h_flex;
use t3_core::commands::method;
use t3_core::timeline::{LimitRecovery, limit_recovery, thread_error};

use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::composer::banner_surface;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use crate::transcript::{Clock, ghost_button};

pub struct LimitRecoveryBanner {
    state: Entity<AppState>,
    thread_id: Option<String>,
    recovery: Option<LimitRecovery>,
    pending: bool,
    _deadline: Option<Task<()>>,
    _subscription: Subscription,
}

#[derive(Clone, Copy)]
enum Action {
    Resume,
    Snooze,
}

impl LimitRecoveryBanner {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Thread | AppEvent::Selection | AppEvent::Shell) {
                this.sync(cx);
            }
        });
        let mut banner = LimitRecoveryBanner { state, thread_id: None, recovery: None, pending: false, _deadline: None, _subscription: subscription };
        banner.sync(cx);
        banner
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let now = chrono::Utc::now().timestamp_millis();
        let thread_id = state.selected_thread.clone();
        let next = state
            .thread
            .projection
            .as_ref()
            .filter(|projection| thread_id.as_deref() == Some(projection.thread.id.as_str()))
            .and_then(|projection| limit_recovery(thread_error(projection).as_ref(), state.thread_shell(&projection.thread.id)?, now));
        if thread_id == self.thread_id && next == self.recovery {
            return;
        }
        // The web keys the actions by run and reset, so a new limit starts with nothing in flight.
        if thread_id != self.thread_id || next.as_ref().map(|r| (&r.run_id, &r.reset_at)) != self.recovery.as_ref().map(|r| (&r.run_id, &r.reset_at)) {
            self.pending = false;
        }
        self._deadline = next.as_ref().and_then(LimitRecovery::deadline).map(|at| {
            let wait = Duration::from_millis((at - now).max(0) as u64 + 50);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| this.sync(cx));
            })
        });
        self.thread_id = thread_id;
        self.recovery = next;
        cx.notify();
    }

    fn toggle(&mut self, action: Action, cx: &mut Context<Self>) {
        let (Some(recovery), Some(thread_id)) = (self.recovery.clone(), self.thread_id.clone()) else { return };
        if self.pending {
            return;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let command = match action {
            Action::Resume => recovery.toggle_resume(&uuid_v4(), &thread_id),
            Action::Snooze => match recovery.toggle_snooze(&uuid_v4(), &thread_id, now) {
                Ok(command) => Some(command),
                Err(_) => {
                    let toast = Toast::new(ToastKind::Error, "Could not snooze thread").description("The reset time has passed. Retry the thread manually.");
                    self.state.update(cx, |state, cx| state.show_toast(toast, cx));
                    self.sync(cx);
                    return;
                }
            },
        };
        let Some(command) = command else { return };
        self.pending = true;
        cx.notify();
        let request = self.state.read(cx).rpc(method::DISPATCH_COMMAND, command);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                if let Err(error) = result {
                    let title = match action {
                        Action::Resume => "Could not change auto-resume",
                        Action::Snooze => "Could not snooze thread",
                    };
                    let message = error.to_string();
                    let description = if message.trim().is_empty() { "An error occurred.".to_owned() } else { message };
                    this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, title).description(description), cx));
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for LimitRecoveryBanner {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(recovery) = self.recovery.clone() else { return div().into_any_element() };
        let p = palette(cx);
        let clock = Clock::now();
        let pending = self.pending;
        let actions = recovery.can_schedule.then(|| {
            let resume = match (pending, recovery.scheduled) {
                (true, _) => "Saving...",
                (false, true) => "Cancel auto-resume",
                (false, false) => "Resume at reset",
            };
            let snooze = match (pending, recovery.snoozed) {
                (true, _) => "Saving...",
                (false, true) => "Wake now",
                (false, false) => "Snooze until reset",
            };
            h_flex()
                .flex_none()
                .ml_auto()
                .gap(px(4.))
                .child(ghost_button("limit-resume", resume, pending, cx.listener(|this, _, _, cx| this.toggle(Action::Resume, cx)), cx))
                .child(ghost_button(
                    "limit-snooze",
                    snooze,
                    pending || (!recovery.snoozed && recovery.reset_passed),
                    cx.listener(|this, _, _, cx| this.toggle(Action::Snooze, cx)),
                    cx,
                ))
        });
        let content = h_flex()
            .w_full()
            .min_h(px(24.))
            .gap(px(4.))
            .child(div().flex_none().w(px(24.)).flex().justify_center().child(Icon::Gauge.render(px(12.), hsla(p.muted_foreground))))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .whitespace_nowrap()
                    .child(div().flex_none().font_weight(gpui::FontWeight::MEDIUM).line_height(px(24.)).child(LimitRecovery::TITLE))
                    .child(div().min_w_0().truncate().text_color(hsla(p.muted_foreground)).child(recovery.description(clock.now_ms, clock.offset_minutes))),
            )
            .children(actions);
        banner_surface(true, content, cx).into_any_element()
    }
}
