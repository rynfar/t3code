//! The usage-limit recovery banner over the composer (`UsageLimitRecoveryBanner.tsx`): when the
//! limit resets, with Resume at reset and Snooze until reset. Its own view, so it redraws alone
//! and can wake at the reset time, when Snooze until reset stops being offered.

use std::time::Duration;

use gpui::{ClipboardItem, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Styled, Subscription, Task, Window, div, px};
use gpui_component::h_flex;
use t3_core::commands::method;
use t3_core::timeline::{CHATGPT_USAGE_URL, CHATGPT_WORKSPACE_ADMIN_REQUEST, LimitRecovery, limit_recovery, thread_error};

use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::composer::banner_surface;
use crate::icons::{Icon, ProviderLogo, provider_logo};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use crate::transcript::{Clock, ghost_button, ghost_text_button};

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
        let has_actions = recovery.can_schedule || recovery.chatgpt_limit();
        let actions = has_actions.then(|| {
            let mut row = h_flex().flex_none().ml_auto().gap(px(4.));
            if recovery.can_schedule {
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
                row = row
                    .child(ghost_button("limit-resume", resume, pending, cx.listener(|this, _, _, cx| this.toggle(Action::Resume, cx)), cx))
                    .child(ghost_button(
                        "limit-snooze",
                        snooze,
                        pending || (!recovery.snoozed && recovery.reset_passed),
                        cx.listener(|this, _, _, cx| this.toggle(Action::Snooze, cx)),
                        cx,
                    ));
            }
            if recovery.workspace_limit() {
                row = row.child(ghost_button(
                    "limit-copy-admin",
                    "Copy request for admin",
                    false,
                    cx.listener(|this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(CHATGPT_WORKSPACE_ADMIN_REQUEST.to_owned()));
                        let toast = Toast::new(ToastKind::Success, "Request copied").description("Send it to your workspace admin.");
                        this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                    }),
                    cx,
                ));
            } else if recovery.chatgpt_limit() {
                row = row.child(ghost_text_button("limit-manage-usage", "Manage usage", Some(Icon::ExternalLink), |_, _, cx| cx.open_url(CHATGPT_USAGE_URL), cx));
            }
            row
        });
        let lead_icon = match (recovery.chatgpt_limit(), provider_logo("codex")) {
            (true, Some(ProviderLogo::Mono(path))) => gpui::svg().path(path).size(px(12.)).text_color(hsla(p.foreground)).into_any_element(),
            _ => Icon::Gauge.render(px(12.), hsla(p.muted_foreground)).into_any_element(),
        };
        let content = h_flex()
            .w_full()
            .min_h(px(24.))
            .gap(px(4.))
            .child(div().flex_none().w(px(24.)).flex().justify_center().child(lead_icon))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .whitespace_nowrap()
                    .child(div().flex_none().font_weight(gpui::FontWeight::MEDIUM).line_height(px(24.)).child(recovery.title()))
                    .child(div().min_w_0().truncate().text_color(hsla(p.muted_foreground)).child(recovery.description(clock.now_ms, &clock.zone, crate::settings::store::client(cx).timestamp_format(), &crate::timestamp::SystemTimestampLocale))),
            )
            .children(actions);
        banner_surface(true, content, cx)
            .id("limit-recovery-banner")
            .debug_selector(|| "limit-recovery-banner".to_owned())
            .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::{AppContext, ParentElement, Render, Styled, div};
    use serde_json::json;

    struct Host(Entity<LimitRecoveryBanner>);
    impl Render for Host {
        fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(720.)).child(self.0.clone())
        }
    }

    #[gpui::test]
    fn chatgpt_and_workspace_limits_select_the_matching_recovery_actions(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let set_error = |state: &mut AppState, code: Option<&str>, cx: &mut Context<AppState>| {
            state.selected_thread = Some("t1".into());
            let thread = json!({
                "id": "t1", "projectId": "p1", "title": "T1",
                "providerInstanceId": "codex", "modelSelection": { "instanceId": "codex", "model": "gpt-5" },
                "runtimeMode": "full-access", "interactionMode": "default",
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                "lineage": { "rootThreadId": "t1" }
            });
            let mut shell_thread = thread.clone();
            shell_thread["status"] = json!("error");
            shell_thread["hasActionableProposedPlan"] = json!(false);
            shell_thread["itemCount"] = json!(0);
            shell_thread["visibleItemCount"] = json!(0);
            shell_thread["latestRunId"] = json!("r1");
            shell_thread["latestRunCompletedAt"] = json!("2026-01-01T00:00:00Z");
            state.shell.snapshot = Some(
                serde_json::from_value(json!({ "snapshotSequence": 1, "projects": [], "threads": [shell_thread] })).unwrap(),
            );
            state.thread.projection = Some(
                serde_json::from_value(json!({
                    "thread": thread,
                    "runs": [{
                        "id": "r1", "ordinal": 1, "providerInstanceId": "codex", "modelSelection": { "instanceId": "codex", "model": "gpt-5" },
                        "userMessageId": "u1", "rootNodeId": "root", "status": "failed",
                        "requestedAt": "2026-01-01T00:00:00Z", "startedAt": "2026-01-01T00:00:00Z", "completedAt": "2026-01-01T00:00:01Z"
                    }],
                    "attempts": [], "nodes": [], "subagents": [], "providerSessions": [], "providerThreads": [], "providerTurns": [],
                    "runtimeRequests": [], "messages": [], "plans": [],
                    "turnItems": [{
                        "id": "e1", "threadId": "t1", "runId": "r1", "nodeId": "root", "parentItemId": null, "ordinal": 1, "status": "failed",
                        "title": null, "startedAt": "2026-01-01T00:00:00Z", "completedAt": null, "updatedAt": "2026-01-01T00:00:00Z",
                        "type": "error",
                        "failure": { "class": "usage_limit", "message": "Limited", "code": code, "resetAt": "2099-01-01T05:00:00Z", "retryable": null }
                    }],
                    "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [], "updatedAt": ""
                }))
                .unwrap(),
            );
            cx.emit(AppEvent::Thread);
        };

        state.update(cx, |state, cx| set_error(state, Some("subscription_sharing_usage_limit_exceeded"), cx));
        let (host, vcx) = cx.add_window_view(|_, cx| Host(cx.new(|cx| LimitRecoveryBanner::new(state.clone(), cx))));
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("limit-recovery-banner").is_some());
        host.read_with(vcx, |host, cx| {
            let rec = host.0.read(cx).recovery.as_ref().unwrap();
            assert_eq!(rec.title(), "ChatGPT usage limit reached");
            assert!(rec.chatgpt_limit() && !rec.workspace_limit());
        });

        vcx.update(|_, cx| state.update(cx, |state, cx| set_error(state, Some("subscription_sharing_workplace_individual_usage_limit_exceeded"), cx)));
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        host.read_with(vcx, |host, cx| {
            let rec = host.0.read(cx).recovery.as_ref().unwrap();
            assert!(rec.workspace_limit());
        });
    }
}
