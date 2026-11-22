//! What the thread itself is doing, around the transcript. The banner lane over its top
//! (`ChatBannerLane.tsx`: `ProviderStatusBanner.tsx`, then `ThreadErrorBanner.tsx`) and the lines
//! after the last row (`ThreadNow.tsx`, `ThreadStatusLine.tsx`, `ThreadActivityBanners.tsx`): a
//! Stop with its elapsed time, the goal, then Snoozed, Settled or Woke from snooze.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, App, BoxShadow, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, FontWeight, Hsla, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, Render, SharedString, Styled, Task, Window, anchored, deferred, div, point, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::supervisor::ConnectionStatus;
use t3_core::command_output::{CommandOutputStatus, LOADING, NO_OUTPUT, Panel, TRUNCATED};
use t3_core::commands::{self, DispatchMode, method};
use t3_core::timeline::{
    BackgroundItem, BackgroundWork, CHATGPT_USAGE_URL, CHATGPT_WORKSPACE_ADMIN_REQUEST, ChildInputs, FORCE_STOP_THREAD, GoalLine, ProviderBanner,
    RunStop, StatusLine, ThreadError, background_work, child_inputs, error_banner_message, error_can_expand, force_stop_notice, force_stop_request,
    goal_line, limit_recovery_owns_snooze, presented_latest_run, provider_banner, run_stop, status_line_for, thread_error, timeline_thread_error,
};

use super::ThreadView;
use crate::app_state::{AppState, Phase, uuid_v4};
use crate::composer::controls::{Button, ButtonSize, ButtonVariant};
use crate::glass;
use crate::icons::{Icon, ProviderLogo, provider_logo};
use crate::theme::{hsla, metrics, palette, rem};
use crate::toast::{Toast, ToastKind};
use crate::transcript::{InlineTone, TimerKind, WorkingTimer, ghost_icon_button, inline_button};

/// The lines after the last row, in the web's order.
#[derive(Debug, Clone, Default, PartialEq)]
struct FooterLines {
    unmatched_approvals: Vec<t3_core::requests::PendingApproval>,
    stop: Option<RunStop>,
    /// Subagents and side chats waiting on the user.
    children: Option<ChildInputs>,
    /// Background commands and subagents left running after the turn.
    background: Option<BackgroundWork>,
    /// True when the active run or connection prevents background Stop.
    run_is_live: bool,
    can_operate: bool,
    /// Loading or resubscribing detail ("Loading messages..." / "Syncing messages...").
    syncing: Option<&'static str>,
    goal: Option<GoalLine>,
    status: Option<StatusLine>,
}

impl FooterLines {
    fn is_shown(&self) -> bool {
        !self.unmatched_approvals.is_empty()
            || self.stop.is_some()
            || self.children.is_some()
            || self.background.is_some()
            || self.syncing.is_some()
            || self.goal.is_some()
            || self.status.is_some()
    }
}

/// `useDelayedStatus`: a quick resync does not flash a line at the end of the timeline.
const SYNC_LINE_DELAY: Duration = Duration::from_millis(500);
/// `BACKGROUND_WORK_STOP_RETRY_AFTER_MS`: Stop on background work can be retried after this.
const BACKGROUND_STOP_RETRY: Duration = Duration::from_secs(10);

/// The footer list item: its lines, the timer the Stop line counts with, and the actions in flight.
#[derive(Default)]
pub struct ThreadFooter {
    lines: FooterLines,
    stop_timer: Option<Entity<WorkingTimer>>,
    background_timer: Option<Entity<WorkingTimer>>,
    status_pending: bool,
    goal_pending: bool,
    force_stopping: bool,
    /// The background tasks a Stop was sent for, until they change or the retry window passes.
    background_stop: Option<(Vec<String>, std::time::Instant)>,
    background_details: Option<Entity<BackgroundWorkDialog>>,
    /// When the thread was first seen unsynchronized, for the sync line's delay.
    unsynced_since: Option<std::time::Instant>,
    /// Expanded notice row id when the user clicked a truncated footer description.
    expanded_notice: Option<&'static str>,
    /// Re-reads the relative labels ("Settled 5m ago", "Snoozed, 2h left") once a minute.
    minute: Option<Task<()>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct BannerLines {
    provider: Option<ProviderBanner>,
    error: Option<ThreadError>,
}

/// The banner lane and what was dismissed. Dismissed errors and provider statuses last the
/// session across thread switches (`ProviderStatusBanner.tsx` `dismissedProviderBannerKeysRef`).
#[derive(Default)]
pub struct Banners {
    lines: BannerLines,
    dismissed_errors: HashSet<String>,
    error_expanded: bool,
    refresh: t3_core::providers::StatusRefresh,
    can_refresh: bool,
    grants: Option<(u64, Task<()>)>,
    target: Option<crate::composer::ProviderTarget>,
    environment: String,
    composer: Option<gpui::Subscription>,
    app_subscription: Option<gpui::Subscription>,
}

impl ThreadView {
    pub(crate) fn dismiss_provider(&mut self, key: &str, cx: &mut Context<Self>) {
        let environment = self.banners.environment.clone();
        self.state.update(cx, |state, cx| state.dismiss_provider_banner(&environment, key, cx));
        self.sync_banners(cx);
    }

    pub(super) fn sync_status(&mut self, cx: &mut Context<Self>) {
        self.watch_banner_grants(cx);
        self.sync_banners(cx);
        self.sync_footer(cx);
    }

    pub(super) fn watch_banner_grants(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let (connection, client) = (state.connection_id(), state.client_handle());
        if self.banners.grants.as_ref().map(|(id, _)| *id) == connection {
            return;
        }
        self.banners.grants = None;
        self.banners.refresh.reset();
        self.banners.can_refresh = false;
        cx.notify();
        let (Some(connection), Some(client)) = (connection, client) else { return };
        let mut grants = client.session_state();
        self.banners.can_refresh = t3_core::providers::can_refresh(grants.borrow_and_update().as_ref());
        self.banners.grants = Some((connection, cx.spawn(async move |this, cx| {
            while grants.changed().await.is_ok() {
                let allowed = t3_core::providers::can_refresh(grants.borrow_and_update().as_ref());
                if this.update(cx, |view, cx| {
                    if view.banners.can_refresh != allowed {
                        view.banners.can_refresh = allowed;
                        cx.notify();
                    }
                }).is_err() { return; }
            }
        })));
    }

    fn open_provider_setup(
        &mut self,
        target: &crate::composer::ProviderTarget,
        cx: &mut Context<Self>,
    ) {
        self.sync_banners(cx);
        if self.banners.target.as_ref() == Some(target)
            && self
                .banners
                .lines
                .provider
                .as_ref()
                .is_some_and(|banner| banner.can_setup)
        {
            crate::settings::open_provider_setup(target.clone(), cx);
        }
    }

    fn refresh_banner_provider(&mut self, cx: &mut Context<Self>) {
        self.sync_banners(cx);
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let Some(ticket) = self.banners.refresh.start(client.session_state().borrow().as_ref()) else { return };
        let request = state.rpc(t3_core::settings::providers::SERVER_REFRESH_PROVIDERS, ticket.payload());
        cx.notify();
        cx.spawn(async move |this, cx| {
            // The stream owns provider status; a unary reply may predate a newer push.
            let result = request.await.map(|_| ());
            let _ = this.update(cx, |view, cx| {
                if !view.banners.refresh.finish(&ticket) { return; }
                if let Err(error) = result {
                    tracing::warn!(%error, "Refresh provider status failed");
                }
                cx.notify();
            });
        }).detach();
    }

    fn sync_banners(&mut self, cx: &mut Context<Self>) {
        if self.banners.app_subscription.is_none() {
            let state = self.state.clone();
            self.banners.app_subscription = Some(cx.subscribe(&state, |this, _, event, cx| {
                if matches!(event, crate::app_state::AppEvent::BannerDismissed) {
                    this.sync_banners(cx);
                }
            }));
        }
        let state = self.state.read(cx);
        let was_pending = self.banners.refresh.pending();
        let environment = crate::composer::ProviderTarget::environment(state);
        if environment != self.banners.environment {
            self.banners.environment = environment.clone();
            self.banners.refresh.reset();
        }
        let mut next = BannerLines::default();
        if let (Some(projection), Some(thread_id)) = (self.thread_state(state).projection.as_ref(), self.thread_id.as_deref()) {
            let local = self.local_error.clone().map(|message| ThreadError { message, class: None, code: None, reset_at: None, runtime_failed: false });
            next.error = local
                .or_else(|| timeline_thread_error(thread_error(projection), state.thread_shell(thread_id)))
                .filter(|error| !self.banners.dismissed_errors.contains(&error.banner_key(thread_id)));
        }
        if let Some(target) = &self.banners.target
            && target.thread.is_some()
            && target.connection == state.connection_id()
            && target.environment == environment
            && target.thread == self.thread_id
        {
            next.provider = target
                .instance
                .as_ref()
                .and_then(|id| state.server_config.config.as_ref()?.provider(id))
                .and_then(provider_banner)
                .filter(|banner| !state.is_provider_banner_dismissed(&environment, &banner.key));
        }
        self.banners.refresh.set_target(state.connection_id(), self.thread_id.as_deref(), next.provider.as_ref().map(|banner| banner.instance_id.as_str()));
        if next == self.banners.lines {
            if was_pending != self.banners.refresh.pending() { cx.notify(); }
            return;
        }
        if next.error.as_ref().map(|error| &error.message) != self.banners.lines.error.as_ref().map(|error| &error.message) {
            self.banners.error_expanded = false;
        }
        self.banners.lines = next;
        cx.notify();
    }

    /// Recomputes the footer lines; splices the footer item in or out when it appears or goes.
    fn sync_footer(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let working = self.thread_id.as_deref().is_some_and(|id| state.is_thread_running(id));
        let shell = self.thread_id.as_deref().and_then(|id| state.thread_shell(id));
        let background = !working && shell.is_some_and(|shell| !shell.pending_background_tasks.is_empty());
        let now = chrono::Utc::now().timestamp_millis();
        let config_loaded = state.server_config.config.is_some();
        let parent_awaiting = shell.is_some_and(|shell| shell.pending_runtime_request.is_some()) || !self.activity.pending_approval_ids.is_empty();
        let children = match (self.thread_id.as_deref(), state.shell.snapshot.as_ref()) {
            (Some(id), Some(snapshot)) => child_inputs(&snapshot.threads, id, parent_awaiting),
            _ => None,
        };
        let thread = self.thread_state(state);
        let sync_label = match (self.thread_id.is_some() && shell.is_some(), thread.projection.is_some(), thread.synchronized) {
            (true, false, false) => Some("Loading messages..."),
            (_, true, false) => Some("Syncing messages..."),
            _ => None,
        };
        let syncing = match (sync_label, self.footer.unsynced_since) {
            (None, _) => {
                self.footer.unsynced_since = None;
                None
            }
            (Some(label), Some(since)) if since.elapsed() >= SYNC_LINE_DELAY => Some(label),
            (Some(_), Some(_)) => None,
            (Some(_), None) => {
                self.footer.unsynced_since = Some(std::time::Instant::now());
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SYNC_LINE_DELAY).await;
                    let _ = this.update(cx, |view, cx| view.sync_footer(cx));
                })
                .detach();
                None
            }
        };
        let thread = self.thread_state(state);
        let turn_items = thread.projection.as_ref().map(|projection| projection.turn_items.as_slice());
        let shell = self.thread_id.as_deref().and_then(|id| state.thread_shell(id));
        let run_is_live = (working && shell.is_some_and(|shell| shell.status != "waiting"))
            || matches!(state.phase, Phase::Online(ConnectionStatus::Connecting));
        let can_operate = crate::context_menu::ThreadMenuOwner::can_operate(state);
        let limit_recovery_holds_snooze = thread
            .projection
            .as_ref()
            .zip(shell)
            .zip(thread.projection.as_ref().and_then(thread_error).filter(|error| error.limit_blocked()))
            .is_some_and(|((projection, shell), error)| {
                let run_id = presented_latest_run(projection).map(|run| run.id.as_str());
                limit_recovery_owns_snooze(run_id, error.reset_at.as_deref(), shell.limit_recovery.as_ref(), shell.snoozed_until.as_deref())
            });
        let next = FooterLines {
            unmatched_approvals: self.unmatched_pending_approvals(),
            stop: thread.projection.as_ref().and_then(|projection| run_stop(projection, background)),
            children,
            background: shell.filter(|_| !working).and_then(|shell| match turn_items {
                Some(items) => BackgroundWork::with_items(&shell.pending_background_tasks, items),
                None => background_work(&shell.pending_background_tasks),
            }),
            run_is_live,
            can_operate,
            syncing,
            goal: shell.and_then(|shell| shell.goal.as_ref()).and_then(|goal| goal_line(goal, working)),
            status: self
                .thread_id
                .as_deref()
                .and_then(|id| state.sidebar.model.thread(id))
                .filter(|_| config_loaded)
                .and_then(|thread| status_line_for(thread, limit_recovery_holds_snooze, now)),
        };
        if let Some(dialog) = self.footer.background_details.clone() {
            let items = next.background.as_ref().map(|work| work.items.clone()).unwrap_or_default();
            if items.is_empty() {
                self.footer.background_details = None;
            } else {
                dialog.update(cx, |dialog, cx| dialog.sync_items(items, cx));
            }
        }
        let task_ids: Vec<String> = next.background.iter().flat_map(|work| work.items.iter().map(|item| item.task_id.clone())).collect();
        if self.footer.background_stop.as_ref().is_some_and(|(ids, at)| *ids != task_ids || at.elapsed() >= BACKGROUND_STOP_RETRY) {
            self.footer.background_stop = None;
        }
        if next == self.footer.lines {
            return;
        }
        let env_id = self.state.read(cx).environment().map(str::to_owned);
        self.footer.stop_timer = match &next.stop {
            Some(stop) if self.footer.stop_timer.as_ref().is_some_and(|timer| timer.read(cx).matches(&stop.requested_at, env_id.as_deref(), TimerKind::Elapsed)) => {
                self.footer.stop_timer.take()
            }
            Some(stop) => {
                let started = stop.requested_at.clone();
                Some(cx.new(|cx| WorkingTimer::new(started, env_id.clone(), TimerKind::Elapsed, cx)))
            }
            None => None,
        };
        self.footer.background_timer = match next.background.as_ref().and_then(BackgroundWork::only_started_at) {
            Some(started) if self.footer.background_timer.as_ref().is_some_and(|timer| timer.read(cx).matches(started, env_id.as_deref(), TimerKind::Elapsed)) => {
                self.footer.background_timer.take()
            }
            Some(started) => {
                let started = started.to_owned();
                Some(cx.new(|cx| WorkingTimer::new(started, env_id, TimerKind::Elapsed, cx)))
            }
            None => None,
        };
        if next.status != self.footer.lines.status {
            self.footer.status_pending = false;
        }
        self.footer.minute = match (&next.status, self.footer.minute.take()) {
            (Some(StatusLine::Snoozed { .. } | StatusLine::Settled { .. }), Some(task)) => Some(task),
            (Some(StatusLine::Snoozed { .. } | StatusLine::Settled { .. }), None) => Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(60)).await;
                    if this.update(cx, |view, cx| view.sync_footer(cx)).is_err() {
                        return;
                    }
                }
            })),
            _ => None,
        };
        self.footer.lines = next;
        self.sync_tail_item(true);
        cx.notify();
    }

    pub(super) fn footer_shown(&self) -> bool {
        self.footer.lines.is_shown()
    }

    /// Forgets the footer and the thread's local error when another thread opens; the list was reset.
    pub(super) fn reset_status(&mut self) {
        self.banners.refresh.reset();
        self.footer = ThreadFooter::default();
        self.local_error = None;
        self.banners.error_expanded = false;
    }

    /// `sendStandaloneCommand`: `/goal resume` and `/goal clear` go out as a turn.
    fn send_goal_command(&mut self, text: &'static str, failure: &'static str, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id.clone() else { return };
        if self.footer.goal_pending {
            return;
        }
        self.footer.goal_pending = true;
        self.local_error = None;
        let request = self.state.read(cx).rpc(method::DISPATCH_COMMAND, commands::send_message(&uuid_v4(), &thread_id, text, &DispatchMode::StartImmediately));
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |view, cx| {
                view.footer.goal_pending = false;
                if let Err(error) = result {
                    let message = error.to_string();
                    view.show_local_error(if message.trim().is_empty() { failure.to_owned() } else { message }, cx);
                }
                view.remeasure_footer();
                cx.notify();
            });
        })
        .detach();
        self.remeasure_footer();
        cx.notify();
    }

    pub(super) fn remeasure_footer(&self) {
        if self.tail_in_list {
            let end = self.header_count() + self.rows.len();
            self.list.remeasure_items(end..end + 1);
        }
    }

    fn run_status_action(&mut self, status: StatusLine, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id.clone() else { return };
        if self.footer.status_pending {
            return;
        }
        if !matches!(status, StatusLine::Woke { .. }) {
            self.footer.status_pending = true;
            self.remeasure_footer();
            cx.notify();
        }
        let view = cx.entity().downgrade();
        self.state.update(cx, |state, cx| {
            let done = move |_: &mut crate::app_state::AppState, _: bool, cx: &mut Context<crate::app_state::AppState>| {
                let _ = view.update(cx, |view, cx| {
                    view.footer.status_pending = false;
                    view.remeasure_footer();
                    cx.notify();
                });
            };
            match status {
                StatusLine::Snoozed { .. } => {
                    state.sidebar.undo.invalidate(t3_core::thread_actions::UndoKind::Snooze, &thread_id);
                    state.dispatch_then(t3_core::thread_actions::unsnooze(&uuid_v4(), &thread_id), Some("Failed to wake thread"), cx, done);
                }
                StatusLine::Settled { .. } => {
                    state.sidebar.undo.invalidate(t3_core::thread_actions::UndoKind::Settle, &thread_id);
                    state.dispatch_then(t3_core::thread_actions::unsettle(&uuid_v4(), &thread_id), Some("Failed to un-settle thread"), cx, done);
                }
                StatusLine::Woke { woke_at } => state.acknowledge_woke(&thread_id, woke_at, cx),
            }
        });
    }

    fn render_notice_detail(&self, id: &'static str, description: String, detail: gpui::Hsla, cx: &mut Context<Self>) -> AnyElement {
        let expanded = self.footer.expanded_notice == Some(id);
        let tip = SharedString::from(description.clone());
        div()
            .id(id)
            .min_w_0()
            .ml(px(6.))
            .text_color(detail)
            .cursor_pointer()
            .when(!expanded, |this| this.truncate())
            .when(expanded, |this| this.whitespace_normal())
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |view, _, _, cx| {
                view.footer.expanded_notice = if view.footer.expanded_notice == Some(id) { None } else { Some(id) };
                cx.notify();
            }))
            .child(description)
            .into_any_element()
    }

    pub(super) fn render_footer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let muted = hsla(p.muted_foreground);
        let detail = if cx.theme().is_dark() { muted.opacity(0.6) } else { muted.opacity(0.8) };
        let line = || h_flex().w_full().min_w_0().gap(px(6.)).px(px(4.)).pb(px(8.)).text_xs().line_height(px(16.)).text_color(muted);
        let actions = || h_flex().flex_none().ml(px(6.)).gap(px(10.));
        let mut lines: Vec<AnyElement> = Vec::new();
        let unmatched = self.footer.lines.unmatched_approvals.clone();
        for approval in &unmatched {
            lines.push(self.with_env(|env, _| crate::transcript::approval_card(approval, None, false, env, cx)));
        }
        let stop_line = self.footer.lines.stop.as_ref().map(|stop| {
            let icon = if stop.unresponsive { Icon::TriangleAlert.render(px(14.), hsla(p.warning_foreground)) } else { Icon::CircleStop.render(px(14.), muted) };
            let can_force = stop.unresponsive && self.state.read(cx).server_config.config.as_ref().is_some_and(|config| config.environment.capabilities.thread_force_stop == Some(true));
            let run_id = stop.run_id.clone();
            let force = can_force.then(|| {
                let pending = self.footer.force_stopping;
                inline_button(
                    "force-stop",
                    if pending { "Force stopping..." } else { "Force stop" },
                    InlineTone::Default,
                    pending,
                    cx.listener(move |view, _, _, cx| view.force_stop(&run_id, cx)),
                    cx,
                )
            });
            let stop_expanded = self.footer.expanded_notice == Some("notice-stop-detail");
            let detail_el = stop.description().map(|desc| self.render_notice_detail("notice-stop-detail", desc.to_owned(), detail, cx));
            line()
                .child(div().flex_none().child(icon))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .when(!stop_expanded, |this| this.overflow_hidden().whitespace_nowrap())
                        .child(div().flex_none().child(stop.title()))
                        .children(detail_el),
                )
                .child(actions().children(self.footer.stop_timer.clone()).children(force))
                .into_any_element()
        });
        let unresponsive = self.footer.lines.stop.as_ref().is_some_and(|stop| stop.unresponsive);
        let (attention, work) = if unresponsive { (stop_line, None) } else { (None, stop_line) };
        // `childInputNowItem`: the only line here that needs you gets the info color.
        let attention = attention.or_else(|| {
            let children = self.footer.lines.children.as_ref()?;
            let info = hsla(p.info_foreground);
            let icon = if children.approval_icon() { Icon::ShieldQuestion } else { Icon::MessageCircleQuestion };
            let names: Vec<AnyElement> = children
                .children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let id = child.thread_id.clone();
                    let label = if index + 1 < children.children.len() { format!("{},", child.title) } else { child.title.clone() };
                    inline_button(SharedString::from(format!("child-input:{id}")), label, InlineTone::Muted, false, cx.listener(move |view, _, _, cx| view.open_thread(&id, cx)), cx)
                })
                .collect();
            let first = children.children.first()?.thread_id.clone();
            Some(
                line()
                    .child(div().flex_none().child(icon.render(px(14.), info)))
                    .child(h_flex().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().gap(px(6.)).child(div().flex_none().child(children.title())).children(names))
                    .child(actions().child(inline_button("child-input-open", children.action(), InlineTone::Default, false, cx.listener(move |view, _, _, cx| view.open_thread(&first, cx)), cx)))
                    .into_any_element(),
            )
        });
        let work = work.or_else(|| {
            let background = self.footer.lines.background.as_ref()?;
            let mono = cx.theme().mono_font_family.clone();
            let icon = if background.all_commands() { Icon::Terminal } else { Icon::CircleDashed };
            let stopping = self.footer.background_stop.is_some();
            let stop_disabled = stopping || self.footer.lines.run_is_live || !self.footer.lines.can_operate;
            let details = inline_button(
                "background-details",
                "Details",
                InlineTone::Muted,
                false,
                cx.listener(|view, _, window, cx| view.open_background_details(window, cx)),
                cx,
            );
            let open = background.only_child_thread().map(|thread_id| {
                let id = thread_id.to_owned();
                inline_button("background-open", "Open", InlineTone::Muted, false, cx.listener(move |view, _, _, cx| view.open_thread(&id, cx)), cx)
            });
            let stop = inline_button(
                "background-stop",
                if stopping { "Stopping..." } else { "Stop" },
                InlineTone::Default,
                stop_disabled,
                cx.listener(|view, _, _, cx| view.stop_background_work(cx)),
                cx,
            );
            let title_el = if background.only_command()
                && let [only] = background.items.as_slice()
            {
                let prefix = if background.waiting { "Waiting" } else { "Running" };
                h_flex()
                    .flex_none()
                    .gap(px(4.))
                    .child(format!("{prefix} ·"))
                    .child(div().font_family(mono.clone()).child(only.label.clone()))
                    .into_any_element()
            } else {
                div().flex_none().child(background.title.clone()).into_any_element()
            };
            let description_el = if background.items.len() > 1 && background.items.iter().all(|item| item.activity.is_none()) {
                let mut parts: Vec<AnyElement> = Vec::new();
                for (index, item) in background.items.iter().enumerate() {
                    if index > 0 {
                        parts.push(div().child(", ").into_any_element());
                    }
                    let label = div().when(item.command, |this| this.font_family(mono.clone())).child(item.label.clone());
                    parts.push(label.into_any_element());
                }
                Some(h_flex().min_w_0().overflow_hidden().ml(px(6.)).text_color(detail).children(parts).into_any_element())
            } else {
                background.description().map(|description| self.render_notice_detail("notice-background-detail", description, detail, cx))
            };
            let bg_expanded = self.footer.expanded_notice == Some("notice-background-detail");
            let body = h_flex().flex_1().min_w_0().when(!bg_expanded, |this| this.overflow_hidden().whitespace_nowrap()).child(title_el).children(description_el);
            Some(
                line()
                    .child(div().flex_none().child(icon.render(px(14.), muted)))
                    .child(body)
                    .child(actions().children(self.footer.background_timer.clone()).child(details).children(open).child(stop))
                    .into_any_element(),
            )
        });
        lines.extend(attention);
        lines.extend(work);
        if let Some(sync_label) = self.footer.lines.syncing {
            lines.push(line().child(div().flex_none().child(Icon::RefreshCw.render(px(14.), muted))).child(div().min_w_0().truncate().child(sync_label)).into_any_element());
        }
        if let Some(goal) = &self.footer.lines.goal {
            let pending = self.footer.goal_pending;
            let mut buttons: Vec<AnyElement> = Vec::new();
            if goal.can_resume && !self.thread_id.as_deref().is_some_and(|id| self.state.read(cx).is_thread_running(id)) {
                buttons.push(inline_button(
                    "goal-resume",
                    "Resume",
                    InlineTone::Default,
                    pending,
                    cx.listener(|view, _, _, cx| view.send_goal_command("/goal resume", "Failed to resume goal.", cx)),
                    cx,
                ));
                buttons.push(div().child("·").into_any_element());
            }
            buttons.push(inline_button(
                "goal-clear",
                "Clear",
                InlineTone::Muted,
                pending,
                cx.listener(|view, _, _, cx| view.send_goal_command("/goal clear", "Failed to clear goal.", cx)),
                cx,
            ));
            let goal_expanded = self.footer.expanded_notice == Some("notice-goal-detail");
            let detail_el = self.render_notice_detail("notice-goal-detail", goal.objective.clone(), detail, cx);
            lines.push(
                line()
                    .child(div().flex_none().child(Icon::Target.render(px(14.), muted)))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .when(!goal_expanded, |this| this.overflow_hidden().whitespace_nowrap())
                            .child(div().flex_none().child(goal.title.clone()))
                            .child(detail_el),
                    )
                    .child(actions().children(buttons))
                    .into_any_element(),
            );
        }
        if let Some(status) = self.footer.lines.status.clone() {
            let icon = match status {
                StatusLine::Settled { .. } => Icon::CircleCheck,
                StatusLine::Snoozed { .. } | StatusLine::Woke { .. } => Icon::AlarmClock,
            };
            let (action, running) = status.action();
            let pending = self.footer.status_pending;
            let label = status.label().to_owned();
            let button = inline_button(
                "thread-status-action",
                if pending { running } else { action },
                InlineTone::Default,
                pending,
                cx.listener(move |view, _, _, cx| view.run_status_action(status.clone(), cx)),
                cx,
            );
            lines.push(
                line()
                    .child(div().flex_none().child(icon.render(px(14.), muted)))
                    .child(div().min_w_0().truncate().child(label))
                    .child(div().flex_none().child("·"))
                    .child(button)
                    .into_any_element(),
            );
        }
        v_flex().w_full().children(lines).children(self.footer.background_details.clone()).into_any_element()
    }

    fn open_background_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(thread_id), Some(background)) = (self.thread_id.clone(), self.footer.lines.background.as_ref()) else {
            return;
        };
        let items = background.items.clone();
        let dialog = cx.new(|cx| BackgroundWorkDialog::new(self.state.clone(), thread_id, items, window, cx));
        cx.subscribe(&dialog, |this, _, _: &BackgroundWorkDismissed, cx| {
            this.footer.background_details = None;
            cx.notify();
        })
        .detach();
        self.footer.background_details = Some(dialog);
        cx.notify();
    }

    /// `useForceStopThread`: kills the agent that ignored Stop on `run_id`, then says what is left.
    fn force_stop(&mut self, run_id: &str, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id.clone() else { return };
        if self.footer.force_stopping {
            return;
        }
        self.footer.force_stopping = true;
        cx.notify();
        let request = self.state.read(cx).rpc(FORCE_STOP_THREAD, force_stop_request(&thread_id, run_id));
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |view, cx| {
                view.footer.force_stopping = false;
                let toast = match &result {
                    Err(error) => Some(Toast::new(ToastKind::Error, "Could not force stop the agent").description(error.to_string())),
                    Ok(value) => force_stop_notice(value).map(|(title, description)| {
                        let toast = Toast::new(ToastKind::Info, title);
                        match description {
                            Some(description) => toast.description(description),
                            None => toast,
                        }
                    }),
                };
                if let Some(toast) = toast {
                    view.state.update(cx, |state, cx| state.show_toast(toast, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `handleStopBackgroundWork`: the thread's Stop, which ends its background work; the line
    /// reads "Stopping..." while those tasks are still listed, for up to ten seconds.
    fn stop_background_work(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id.clone() else { return };
        if self.footer.background_stop.is_some() {
            return;
        }
        let ids = self.footer.lines.background.iter().flat_map(|work| work.items.iter().map(|item| item.task_id.clone())).collect();
        self.footer.background_stop = Some((ids, std::time::Instant::now()));
        self.state.update(cx, |state, cx| state.stop_thread(&thread_id, cx));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BACKGROUND_STOP_RETRY).await;
            let _ = this.update(cx, |view, cx| {
                view.footer.background_stop = None;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// A failure from an action taken in this view, shown in the error banner.
    pub(super) fn show_local_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.local_error = Some(message);
        self.sync_banners(cx);
    }

    fn dismiss_error(&mut self, key: String, cx: &mut Context<Self>) {
        self.local_error = None;
        self.banners.dismissed_errors.insert(key);
        self.sync_banners(cx);
    }

    fn render_error_actions(&self, error: &ThreadError, key: String, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut actions = Vec::new();
        if error.workspace_limit() {
            let copy = Button::new("copy-admin-request", ButtonVariant::Default, ButtonSize::Sm).label("Copy request for admin").on_click(cx.listener(
                |view, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(CHATGPT_WORKSPACE_ADMIN_REQUEST.to_owned()));
                    let toast = Toast::new(ToastKind::Success, "Request copied").description("Send it to your workspace admin.");
                    view.state.update(cx, |state, cx| state.show_toast(toast, cx));
                },
            ));
            actions.push(copy.into_any_element());
        } else if error.chatgpt_limit() {
            let manage = Button::new("manage-usage", ButtonVariant::Default, ButtonSize::Sm)
                .icon(Icon::ExternalLink)
                .label("Manage usage")
                .on_click(|_, _, cx| cx.open_url(CHATGPT_USAGE_URL));
            actions.push(manage.into_any_element());
        }
        actions.push(ghost_icon_button("dismiss-error", Icon::X, cx.listener(move |view, _, _, cx| view.dismiss_error(key.clone(), cx)), cx));
        actions
    }

    /// The banner lane: centered over the transcript's top, each banner `pt-3` below the last.
    pub(super) fn render_banners(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let lines = &self.banners.lines;
        if lines.provider.is_none() && lines.error.is_none() {
            return None;
        }
        let mut banners: Vec<AnyElement> = Vec::new();
        if let Some(banner) = &lines.provider {
            let key = banner.key.clone();
            let message = SharedString::from(banner.message.clone());
            let body = div()
                .id("provider-status-message")
                .line_clamp(3)
                .child(message.clone())
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(message.clone()).build(window, cx))
                .into_any_element();
            let dismiss = ghost_icon_button(
                "dismiss-provider-status",
                Icon::X,
                cx.listener(move |view, _, _, cx| {
                    view.dismiss_provider(&key, cx);
                }),
                cx,
            );
            let refreshing = self.banners.refresh.pending();
            #[cfg(feature = "snapshot")]
            let refreshing = refreshing || cx.try_global::<crate::snapshot::ProviderRefreshPending>().is_some();
            let refresh = Button::new("refresh-provider-status", ButtonVariant::Outline, ButtonSize::Xs)
                .icon(Icon::RefreshCw)
                .label(if refreshing { "Refreshing..." } else { "Refresh" })
                .tooltip(banner.refresh_tooltip.clone())
                .disabled(!self.banners.can_refresh || refreshing)
                .on_click(cx.listener(|view, _, _, cx| view.refresh_banner_provider(cx)));
            let target = self.banners.target.clone().filter(|_| banner.can_setup);
            let setup = target.map(|target| inline_button("provider-setup", "Open provider setup", InlineTone::Default, false, cx.listener(move |view, _, _, cx| view.open_provider_setup(&target, cx)), cx));
            let body = v_flex().gap(rem(8.)).child(body).child(h_flex().flex_wrap().justify_end().gap(rem(12.)).children(setup).child(refresh)).into_any_element();
            banners.push(alert(banner.warning, AlertIcon::Lucide(Icon::Info), Some(banner.title.clone()), vec![body], vec![dismiss], f32::INFINITY, cx));
        }
        if let (Some(error), Some(thread_id)) = (&lines.error, self.thread_id.as_deref()) {
            let key = error.banner_key(thread_id);
            let body: Vec<AnyElement> = if error.chatgpt_limit() {
                vec![
                    v_flex()
                        .gap(px(4.))
                        .child(div().font_weight(FontWeight::MEDIUM).child("ChatGPT usage limit reached"))
                        .when(error.workspace_limit(), |this| {
                            this.child("You've reached your workspace's limit for using your ChatGPT plan in other apps. Copy a request to ask your workspace admin for increased usage limits.")
                        })
                        .into_any_element(),
                ]
            } else {
                let collapsed = error_banner_message(&error.message);
                let expandable = error_can_expand(&error.message, &collapsed);
                let expanded = self.banners.error_expanded;
                let shown = if expanded { error.message.clone() } else { collapsed };
                let mut text = v_flex().min_w_0().child(div().whitespace_normal().when(expandable && !expanded, |this| this.line_clamp(3)).child(shown));
                if expandable {
                    text = text.child(div().mt(px(4.)).child(inline_button(
                        "toggle-full-error",
                        if expanded { "Show less" } else { "Show full error" },
                        InlineTone::Muted,
                        false,
                        cx.listener(|view, _, _, cx| {
                            view.banners.error_expanded = !view.banners.error_expanded;
                            cx.notify();
                        }),
                        cx,
                    )));
                }
                vec![text.into_any_element()]
            };
            let actions = self.render_error_actions(error, key, cx);
            let icon = match (error.chatgpt_limit(), provider_logo("codex")) {
                (true, Some(ProviderLogo::Mono(path))) => AlertIcon::Mark(path),
                _ => AlertIcon::Lucide(Icon::CircleAlert),
            };
            banners.push(alert(error.is_usage_limit(), icon, None, body, actions, 768., cx));
        }
        let cover = crate::webview::occluder("thread-banners-cover", crate::webview::Layer::TranscriptChrome);
        Some(v_flex().absolute().top_0().left_0().right_0().items_center().px(px(16.)).child(cover).children(banners).into_any_element())
    }
}

enum AlertIcon {
    /// Drawn in the variant's color.
    Lucide(Icon),
    /// A brand mark in `foreground` (the OpenAI mark on ChatGPT limits).
    Mark(&'static str),
}

/// `Alert` with `surface="glass"` and `controlAlignment="first-line"` in the error or warning
/// variant: glass over `background`, tinted 4% toward the variant, 32% variant border.
fn alert(warning: bool, icon: AlertIcon, title: Option<String>, body: Vec<AnyElement>, actions: Vec<AnyElement>, max_width: f32, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let (tint, accent, text): (Hsla, Hsla, Hsla) =
        if warning { (hsla(p.warning), hsla(p.warning), hsla(p.warning_foreground)) } else { (hsla(p.destructive), hsla(p.error), hsla(p.error_foreground)) };
    let has_actions = !actions.is_empty();
    div()
        .pt(px(12.))
        .when(max_width.is_finite(), |this| this.max_w(px(max_width)))
        .child(
            div()
                .relative()
                .rounded(px(12.))
                .occlude()
                .child(glass::backdrop(12., hsla(p.background).opacity(metrics::GLASS_OPACITY), accent.opacity(0.32), cx))
                .child(div().absolute().inset_0().rounded(px(12.)).bg(tint.opacity(0.04)))
                .child(
                    h_flex()
                        .relative()
                        .items_start()
                        .gap(px(8.))
                        .px(px(14.))
                        .py(px(12.))
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .text_color(text)
                        .when(has_actions, |this| this.min_h(px(24.)))
                        .child(div().flex_none().h(px(20.)).w(px(16.)).flex().items_center().child(match icon {
                            AlertIcon::Lucide(icon) => icon.render(px(16.), accent),
                            AlertIcon::Mark(path) => gpui::svg().path(path).size(px(16.)).text_color(hsla(p.foreground)),
                        }))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(2.))
                                .children(title.map(|title| div().font_weight(FontWeight::MEDIUM).child(title)))
                                .child(v_flex().gap(px(10.)).text_color(text.opacity(0.8)).children(body)),
                        )
                        .when(has_actions, |this| this.child(h_flex().flex_none().h(px(20.)).items_center().gap(px(4.)).children(actions))),
                ),
        )
        .into_any_element()
}

pub(super) struct BackgroundWorkDismissed;

struct BackgroundOutputSlot {
    thread_id: String,
    item_id: String,
    connection: Option<u64>,
    status: CommandOutputStatus,
    _stream: t3_client::command_output::CommandOutputStream,
    _pump: Task<()>,
}

/// Modal details dialog for background commands and subagents (`BackgroundWorkDetails.tsx`).
pub(super) struct BackgroundWorkDialog {
    state: Entity<AppState>,
    thread_id: String,
    items: Vec<BackgroundItem>,
    outputs: HashMap<String, BackgroundOutputSlot>,
    focus: FocusHandle,
}

impl EventEmitter<BackgroundWorkDismissed> for BackgroundWorkDialog {}

impl BackgroundWorkDialog {
    pub fn new(state: Entity<AppState>, thread_id: String, items: Vec<BackgroundItem>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut dialog = BackgroundWorkDialog { state, thread_id, items, outputs: HashMap::new(), focus };
        dialog.sync_outputs(cx);
        dialog
    }

    pub fn sync_items(&mut self, items: Vec<BackgroundItem>, cx: &mut Context<Self>) {
        self.items = items;
        self.sync_outputs(cx);
        cx.notify();
    }

    fn sync_outputs(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let streams_output = state
            .server_config
            .config
            .as_ref()
            .is_some_and(|config| config.environment.capabilities.command_output_streaming == Some(true));
        if !streams_output {
            self.outputs.clear();
            return;
        }
        let (Some(client), connection) = (state.client_handle(), state.connection_id()) else {
            self.outputs.clear();
            return;
        };
        let wanted: HashMap<String, String> = self
            .items
            .iter()
            .filter(|item| item.item_running)
            .filter_map(|item| {
                let item_id = item.item_id.clone()?;
                let thread_id = item.source_thread_id.clone().unwrap_or_else(|| self.thread_id.clone());
                Some((item_id, thread_id))
            })
            .collect();
        self.outputs.retain(|id, slot| {
            wanted.get(id).is_some_and(|thread_id| slot.thread_id == *thread_id && slot.connection == connection)
        });
        let runtime = state.runtime().clone();
        for (item_id, thread_id) in wanted {
            if self.outputs.contains_key(&item_id) {
                continue;
            }
            let stream = t3_client::command_output::subscribe(&runtime, client.clone(), &thread_id, &item_id);
            let mut rx = stream.status.clone();
            let key = item_id.clone();
            let pump = cx.spawn(async move |this, cx| {
                let initial = rx.borrow_and_update().clone();
                if this
                    .update(cx, |dialog, cx| {
                        if let Some(slot) = dialog.outputs.get_mut(&key) {
                            slot.status = initial;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    return;
                }
                while rx.changed().await.is_ok() {
                    let next = rx.borrow_and_update().clone();
                    if this
                        .update(cx, |dialog, cx| {
                            if let Some(slot) = dialog.outputs.get_mut(&key) {
                                slot.status = next;
                                cx.notify();
                            }
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            });
            self.outputs.insert(
                item_id.clone(),
                BackgroundOutputSlot { thread_id, item_id, connection, status: CommandOutputStatus::default(), _stream: stream, _pump: pump },
            );
        }
    }

    fn render_output(slot: &BackgroundOutputSlot, cx: &App) -> Option<AnyElement> {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let text = match slot.status.panel() {
            Panel::Output(output) => {
                if output.truncated {
                    format!("{TRUNCATED}\n{}", output.text)
                } else {
                    output.text.to_string()
                }
            }
            Panel::Empty => NO_OUTPUT.to_owned(),
            Panel::Failed(error) => error.to_owned(),
            Panel::Loading => LOADING.to_owned(),
            Panel::Waiting => return None,
        };
        Some(
            div()
                .id(SharedString::from(format!("background-output-{}", slot.item_id)))
                .max_h(px(192.))
                .overflow_y_scroll()
                .rounded(px(6.))
                .border_1()
                .border_color(hsla(p.border))
                .bg(hsla(p.muted).opacity(0.2))
                .p(px(12.))
                .font_family(mono)
                .text_xs()
                .text_color(hsla(p.foreground))
                .child(text)
                .into_any_element(),
        )
    }
}

impl Render for BackgroundWorkDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let muted = hsla(p.muted_foreground);
        let mono = cx.theme().mono_font_family.clone();
        let sections: Vec<AnyElement> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let status_label = if item.awaited { "Waiting" } else { "Running" };
                let command = item.command_text.as_ref().map(|cmd| {
                    div()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.muted).opacity(0.2))
                        .p(px(12.))
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(hsla(p.foreground))
                        .child(cmd.clone())
                        .into_any_element()
                });
                let output = item
                    .item_id
                    .as_deref()
                    .filter(|_| item.item_running)
                    .and_then(|id| self.outputs.get(id))
                    .and_then(|slot| Self::render_output(slot, cx));
                let open = (item.kind == "subagent")
                    .then(|| item.child_thread_id.clone())
                    .flatten()
                    .map(|child_id| {
                        inline_button(
                            SharedString::from(format!("background-details-open-{index}")),
                            "Open thread",
                            InlineTone::Default,
                            false,
                            cx.listener(move |dialog, _, _, cx| {
                                cx.emit(BackgroundWorkDismissed);
                                dialog.state.update(cx, |state, cx| state.select_thread(&child_id, cx));
                            }),
                            cx,
                        )
                    });
                v_flex()
                    .min_w_0()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .items_baseline()
                            .justify_between()
                            .gap(px(12.))
                            .child(div().min_w_0().font_weight(FontWeight::MEDIUM).text_sm().text_color(hsla(p.foreground)).child(item.details_title.clone()))
                            .child(div().flex_none().text_xs().text_color(muted).child(status_label)),
                    )
                    .children(command)
                    .children(output)
                    .children(open)
                    .into_any_element()
            })
            .collect();
        let viewport = window.viewport_size();
        let dark = cx.theme().is_dark();
        deferred(
            anchored().position(point(px(0.), px(0.))).child(
                div()
                    .id("background-work-backdrop")
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(16.))
                    .bg(hsla(p.dialog_backdrop))
                    .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.emit(BackgroundWorkDismissed)))
                    .child(
                        v_flex()
                            .id("background-work-dialog")
                            .debug_selector(|| "background-work-dialog".to_owned())
                            .track_focus(&self.focus)
                            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape" {
                                    cx.stop_propagation();
                                    cx.emit(BackgroundWorkDismissed);
                                }
                            }))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .w_full()
                            .max_w(px(672.))
                            .max_h(viewport.height - px(32.))
                            .relative()
                            .rounded(px(16.))
                            .child(glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                            .shadow(vec![BoxShadow {
                                color: gpui::black().opacity(if dark { 0.9 } else { 0.65 }),
                                offset: point(px(0.), px(24.)),
                                blur_radius: px(64.),
                                spread_radius: px(-24.),
                                inset: false,
                            }])
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .p(px(24.))
                                    .pb(px(12.))
                                    .child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child("Background work"))
                                    .child(ghost_icon_button("background-work-close", Icon::X, cx.listener(|_, _, _, cx| cx.emit(BackgroundWorkDismissed)), cx)),
                            )
                            .child(v_flex().id("background-work-panel").flex_1().min_h_0().overflow_y_scroll().px(px(24.)).pt(px(4.)).pb(px(24.)).gap(px(20.)).children(sections)),
                    ),
            ),
        )
        .with_priority(1)
        .into_any_element()
    }
}

impl ThreadView {
    pub(crate) fn bind_composer(&mut self, composer: &Entity<crate::composer::Composer>, cx: &mut Context<Self>) {
        self.banners.target = Some(composer.read(cx).provider_target.clone());
        self.banners.composer = Some(cx.subscribe(composer, |view, _, target: &crate::composer::ProviderTarget, cx| {
            view.banners.target = Some(target.clone());
            view.sync_banners(cx);
        }));
        self.sync(cx);
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod selected_provider_tests {
    use super::*;
    use crate::app_state::AppEvent;
    use crate::composer::Composer;
    use gpui::{AppContext, Render, size};
    use serde_json::json;

    struct Pair { transcript: Entity<ThreadView>, composer: Entity<Composer> }
    impl Render for Pair {
        fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement { div() }
    }

    #[gpui::test]
    fn unsent_provider_choices_follow_the_matching_composer(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let state = cx.update(|cx| {
            cx.set_global(serde_json::from_value::<crate::chatgpt_setup::Fixture>(json!({
                "provider": {"instanceId":"codex","driver":"codex","enabled":true,"installed":true,"status":"ready"},
                "auth":{"phase":"idle"},"install":{"phase":"idle"}
            })).unwrap());
            let state = crate::boot(runtime.handle().clone(), cx);
            state.update(cx, |state, _| {
                state.selected_thread = Some("main".into());
                state.server_config.config = Some(serde_json::from_value(json!({
                    "environment":{"environmentId":"one"},
                    "providers":[
                        {"instanceId":"codex","driver":"codex","enabled":true,"installed":true,"status":"warning","message":"A","setup":{"canAuthenticate":true},"models":[{"slug":"m","name":"A"}]},
                        {"instanceId":"claudeAgent","driver":"claudeAgent","enabled":true,"installed":true,"status":"warning","message":"B","setup":{"canInstall":true},"models":[{"slug":"m","name":"B"}]}
                    ]
                })).unwrap());
                let thread = json!({"id":"main","projectId":"p","title":"Main","providerInstanceId":"codex",
                    "modelSelection":{"instanceId":"codex","model":"m"},"runtimeMode":"full-access","interactionMode":"default",
                    "lineage":{"rootThreadId":"main"},"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z"});
                state.thread.projection = Some(serde_json::from_value(json!({
                    "thread":thread,"runs":[],"attempts":[],"nodes":[],"subagents":[],"providerSessions":[],"providerThreads":[],"providerTurns":[],
                    "runtimeRequests":[],"messages":[],"plans":[],"turnItems":[],"checkpointScopes":[],"checkpoints":[],"contextHandoffs":[],"contextTransfers":[],"visibleTurnItems":[],"updatedAt":""
                })).unwrap());
                let shell = |id| { let mut value=thread.clone(); value["id"]=json!(id); value["tags"]=json!([]); value["status"]=json!("idle"); value["hasActionableProposedPlan"]=json!(false); value["itemCount"]=json!(0); value["visibleItemCount"]=json!(0); serde_json::from_value(value).unwrap() };
                state.shell.snapshot = Some(t3_core::shell::ShellSnapshot { threads:vec![shell("main"),shell("side")], ..Default::default() });
            });
            state
        });
        let main = cx.open_window(size(px(800.),px(600.)), |window, cx| {
            let transcript = cx.new(|cx| ThreadView::new(state.clone(), window, cx));
            let composer = cx.new(|cx| Composer::new(state.clone(), window, cx));
            transcript.update(cx, |view, cx| { view.sync(cx); view.bind_composer(&composer, cx); });
            Pair {transcript, composer}
        });
        let side = cx.open_window(size(px(800.),px(600.)), |window, cx| {
            let transcript = cx.new(|cx| ThreadView::embedded(state.clone(), "side".into(), window, cx));
            let composer = cx.new(|cx| Composer::embedded(state.clone(), "side".into(), window, cx));
            transcript.update(cx, |view, cx| view.bind_composer(&composer, cx));
            Pair {transcript, composer}
        });
        let (view, composer) = main.read_with(cx, |pair, _| (pair.transcript.clone(),pair.composer.clone())).unwrap();
        let (side_view, side_composer) = side.read_with(cx, |pair, _| (pair.transcript.clone(),pair.composer.clone())).unwrap();
        let shown = |view: &Entity<ThreadView>, cx: &mut gpui::TestAppContext| view.read_with(cx, |view, _| view.banners.lines.provider.as_ref().map(|banner| banner.instance_id.clone()));
        assert_eq!(shown(&view,cx).as_deref(),Some("codex"));
        cx.update_window(main.into(), |_,window,cx| composer.update(cx, |composer,cx| composer.pick_banner_test_model("claudeAgent",window,cx))).unwrap();
        assert_eq!(shown(&view,cx).as_deref(),Some("claudeAgent"),"unsent B must replace persisted A's banner");
        assert_eq!(shown(&side_view,cx).as_deref(),Some("codex"),"main choices must not reach side chat");
        cx.update_window(side.into(), |_,window,cx| side_composer.update(cx, |composer,cx| composer.pick_banner_test_model("claudeAgent",window,cx))).unwrap();
        cx.update_window(main.into(), |_,window,cx| composer.update(cx, |composer,cx| composer.pick_banner_test_model("codex",window,cx))).unwrap();
        assert_eq!(shown(&view,cx).as_deref(),Some("codex"));
        assert_eq!(shown(&side_view,cx).as_deref(),Some("claudeAgent"));
        cx.update_window(side.into(), |_, window, cx| {
            side_composer.update(cx, |composer, cx| {
                composer.pick_banner_test_model("claudeAgent", window, cx)
            })
        })
        .unwrap();
        cx.update_window(main.into(), |_, window, cx| {
            composer.update(cx, |composer, cx| {
                composer.pick_banner_test_model("codex", window, cx)
            })
        })
        .unwrap();
        assert_eq!(shown(&view, cx).as_deref(), Some("codex"));
        assert_eq!(shown(&side_view, cx).as_deref(), Some("claudeAgent"));
        cx.update_window(side.into(), |_, window, cx| {
            side_view.update(cx, |view, cx| {
                view.open_provider_setup(&side_composer.read(cx).provider_target.clone(), cx)
            });
            assert_eq!(
                crate::settings::setup_selection_for_test(window, cx).as_deref(),
                Some("claudeAgent"),
                "setup must open the side composer's exact non-first instance"
            );
        })
        .unwrap();
        cx.update_window(main.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.open_provider_setup(&composer.read(cx).provider_target.clone(), cx)
            });
            assert_eq!(
                crate::settings::setup_selection_for_test(window, cx).as_deref(),
                Some("codex"),
                "already-open Settings must follow the main composer"
            );
        })
        .unwrap();
        cx.update_window(main.into(), |_, window, cx| {
            crate::settings::test_setup_target_lifetime(
                composer.read(cx).provider_target.clone(),
                window,
                cx,
            );
            let mut stale = composer.read(cx).provider_target.clone();
            stale.thread = Some("side".into());
            view.update(cx, |view, cx| view.open_provider_setup(&stale, cx));
            assert_eq!(
                crate::settings::setup_selection_for_test(window, cx),
                None,
                "a side source cannot activate the main transcript's action"
            );
            stale = composer.read(cx).provider_target.clone();
            stale.instance = Some("claudeAgent".into());
            view.update(cx, |view, cx| view.open_provider_setup(&stale, cx));
            assert_eq!(
                crate::settings::setup_selection_for_test(window, cx),
                None,
                "a replaced composer choice cannot activate an old banner action"
            );
        })
        .unwrap();
        let queued = composer.read_with(cx, |composer, _| composer.provider_target.clone());
        cx.update(|cx| crate::settings::open_provider_setup(queued.clone(), cx));
        state.update(cx, |state, cx| {
            state
                .server_config
                .config
                .as_mut()
                .unwrap()
                .environment
                .environment_id = "before-consumption".into();
            cx.emit(AppEvent::ServerConfig);
        });
        state.update(cx, |state, cx| {
            state
                .server_config
                .config
                .as_mut()
                .unwrap()
                .environment
                .environment_id = queued.environment;
            cx.emit(AppEvent::ServerConfig);
        });
        cx.update_window(main.into(), |_,window,cx| assert_eq!(crate::settings::setup_selection_for_test(window,cx),None,"an unconsumed target is retired on the first invalidating event, even if the environment returns")).unwrap();
        state.update(cx, |state,cx| {state.server_config.config.as_mut().unwrap().providers[1].message=Some("B updated".into());cx.emit(AppEvent::ServerConfig);});
        side_view.read_with(cx, |view,_| assert_eq!(view.banners.lines.provider.as_ref().unwrap().message,"B updated"));
        side_view.update(cx, |view,cx| {
            let key = view.banners.lines.provider.as_ref().unwrap().key.clone();
            view.dismiss_provider(&key, cx);
        });
        assert_eq!(shown(&side_view,cx),None);
        cx.update_window(main.into(), |_,window,cx| composer.update(cx, |composer,cx| composer.pick_banner_test_model("claudeAgent",window,cx))).unwrap();
        assert_eq!(shown(&view,cx),None,"session-wide dismissal shares across ThreadViews");
        cx.update_window(main.into(), |_,window,cx| composer.update(cx, |composer,cx| composer.pick_banner_test_model("codex",window,cx))).unwrap();
        cx.update_window(side.into(), |_,window,cx| side_composer.update(cx, |composer,cx| composer.pick_banner_test_model("codex",window,cx))).unwrap();
        assert_eq!(shown(&side_view,cx).as_deref(),Some("codex"));
        cx.update_window(side.into(), |_,window,cx| side_composer.update(cx, |composer,cx| composer.pick_banner_test_model("claudeAgent",window,cx))).unwrap();
        assert_eq!(shown(&side_view,cx),None,"returning to the same dismissed key stays dismissed");
        // Dismissal persists across thread switches and brief absence of any warning banner.
        state.update(cx, |state,cx| {state.server_config.config.as_mut().unwrap().providers[1].status="ready".into();cx.emit(AppEvent::ServerConfig);});
        assert_eq!(shown(&side_view,cx),None);
        state.update(cx, |state,cx| {state.server_config.config.as_mut().unwrap().providers[1].status="warning".into();cx.emit(AppEvent::ServerConfig);});
        assert_eq!(shown(&side_view,cx),None,"dismissed provider banner stays hidden even after returning from ready");
        state.update(cx, |state,cx| {state.server_config.config.as_mut().unwrap().providers[1].message=Some("B new key".into());cx.emit(AppEvent::ServerConfig);});
        assert_eq!(shown(&side_view,cx).as_deref(),Some("claudeAgent"));
        let notifications=std::rc::Rc::new(std::cell::Cell::new(0));
        let observed=notifications.clone();
        let _subscription=cx.update(|cx| cx.observe(&view,move|_,_| observed.set(observed.get()+1)));
        cx.update_window(main.into(), |_,window,cx| composer.update(cx, |composer,cx| composer.type_banner_test_prompt(window,cx))).unwrap();
        assert_eq!(notifications.get(),0,"typing does not redraw the transcript");
        composer.update(cx, |composer,cx| composer.restore_banner_test_model("claudeAgent",cx));
        assert_eq!(shown(&view,cx).as_deref(),Some("claudeAgent"),"draft choices are published on restore");
        let old_refresh=view.update(cx, |view,_| {
            view.banners.refresh.set_target(Some(1),Some("main"),Some("claudeAgent"));
            let grants=serde_json::from_value(json!({"authenticated":true,"permissions":["orchestration:read"]})).unwrap();
            view.banners.refresh.start(Some(&grants)).unwrap()
        });
        let before=notifications.get();
        state.update(cx, |state,cx| {state.server_config.config.as_mut().unwrap().environment.environment_id="pending-transition".into();cx.emit(AppEvent::ServerConfig);});
        assert!(notifications.get()>before,"retiring Refreshing must redraw even when the banner text is unchanged");
        view.update(cx, |view,_| {assert!(!view.banners.refresh.pending());assert!(!view.banners.refresh.finish(&old_refresh));});
        state.update(cx, |state,cx| {state.server_config.config.as_mut().unwrap().providers[1].availability=Some("unavailable".into());cx.emit(AppEvent::ServerConfig);});
        assert_eq!(shown(&view,cx).as_deref(),Some("codex"),"unusable local B follows the composer's effective fallback");
        state.update(cx, |state,cx| {let config=state.server_config.config.as_mut().unwrap();config.providers[1].availability=None;config.environment.environment_id="two".into();cx.emit(AppEvent::ServerConfig);});
        assert_eq!(shown(&view,cx).as_deref(),Some("claudeAgent"));
        state.update(cx, |state,cx| {state.selected_thread=Some("side".into());state.thread=Default::default();cx.emit(AppEvent::Selection);});
        assert_eq!(shown(&view,cx).as_deref(),Some("codex"),"the newly selected target must not inherit main's B choice");
        state.update(cx, |state,cx| {state.selected_thread=Some("main".into());cx.emit(AppEvent::Selection);});
        assert_eq!(shown(&view,cx).as_deref(),Some("claudeAgent"));
        cx.update_window(main.into(), |_,window,cx| {
            let seeded=cx.new(|cx| ThreadView::new(state.clone(),window,cx));
            seeded.update(cx, |view,cx| view.bind_composer(&composer,cx));
            assert_eq!(seeded.read(cx).banners.lines.provider.as_ref().unwrap().instance_id,"claudeAgent","binding seeds an already resolved composer");
        }).unwrap();
        state.update(cx, |state,cx| {state.selected_thread=None;cx.emit(AppEvent::Selection);});
        assert_eq!(shown(&view,cx),None,"no transcript target must not show the default provider's banner");
        assert_eq!(shown(&side_view,cx).as_deref(),Some("claudeAgent"),"side chat keeps its own target when the main selection closes");
        state.update(cx, |state,cx| {state.server_config.config.as_mut().unwrap().providers.clear();cx.emit(AppEvent::ServerConfig);});
        assert_eq!(shown(&view,cx),None);
        assert_eq!(shown(&side_view,cx),None);
    }

    struct CaptureStage {
        limit_banner: Entity<crate::thread_view::limit_recovery::LimitRecoveryBanner>,
        thread_view: Entity<ThreadView>,
    }

    impl Render for CaptureStage {
        fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
            let p = palette(cx);
            let footer = self.thread_view.update(cx, |view, cx| view.render_footer(cx));
            let banners = self.thread_view.update(cx, |view, cx| view.render_banners(cx));
            v_flex()
                .size_full()
                .bg(hsla(p.background))
                .p(px(24.))
                .gap(px(12.))
                .justify_between()
                .child(v_flex().w_full().gap(px(8.)).children(banners).child(self.limit_banner.clone()))
                .child(div().w_full().child(footer))
        }
    }

    #[test]
    fn background_work_dialog_snooze_ownership_and_snapshot_capture() {
        let mut cx = crate::snapshot::test_context();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let state = cx.update(|cx| {
            let state = crate::boot(runtime.handle().clone(), cx);
            state.update(cx, |state, cx| {
                state.stop_pump();
                state.selected_thread = Some("main".into());
                state.server_config.config = Some(serde_json::from_value(json!({
                    "environment": { "environmentId": "env-1", "label": "Local" },
                    "providers": []
                })).unwrap());
                let thread = json!({
                    "id": "main", "projectId": "p", "title": "Main", "providerInstanceId": "pi",
                    "modelSelection": { "instanceId": "pi", "model": "claude-sonnet" },
                    "runtimeMode": "full-access", "interactionMode": "default",
                    "lineage": { "rootThreadId": "main" },
                    "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                    "snoozedAt": "2026-01-01T00:00:10Z",
                    "snoozedUntil": "2099-01-01T06:00:00Z",
                    "activeProviderThreadId": "pt-pi"
                });
                let mut shell_thread = thread.clone();
                shell_thread["tags"] = json!([]);
                shell_thread["status"] = json!("error");
                shell_thread["hasActionableProposedPlan"] = json!(false);
                shell_thread["itemCount"] = json!(1);
                shell_thread["visibleItemCount"] = json!(1);
                shell_thread["latestRunId"] = json!("r1");
                shell_thread["latestRunCompletedAt"] = json!("2026-01-01T00:00:01Z");
                shell_thread["limitRecovery"] = json!({
                    "runId": "r1",
                    "resetAt": "2099-01-01T05:00:00Z",
                    "autoResume": true,
                    "snooze": true
                });
                shell_thread["pendingBackgroundTasks"] = json!([{
                    "taskId": "bg-1",
                    "kind": "command",
                    "description": "cargo test -p t3-core",
                    "command": "cargo test -p t3-core --lib",
                    "itemId": "cmd-1",
                    "nativeItemId": "native-cmd-1",
                    "sourceThreadId": "main"
                }]);
                state.shell.snapshot = Some(t3_core::shell::ShellSnapshot {
                    threads: vec![serde_json::from_value(shell_thread).unwrap()],
                    ..Default::default()
                });
                state.thread.projection = Some(serde_json::from_value(json!({
                    "thread": thread,
                    "runs": [{
                        "id": "r1", "ordinal": 1, "providerInstanceId": "pi",
                        "modelSelection": { "instanceId": "pi", "model": "claude-sonnet" },
                        "userMessageId": "u1", "rootNodeId": "root", "status": "failed",
                        "requestedAt": "2026-01-01T00:00:00Z", "startedAt": "2026-01-01T00:00:00Z",
                        "completedAt": "2026-01-01T00:00:01Z"
                    }],
                    "attempts": [], "nodes": [], "subagents": [],
                    "providerSessions": [{
                        "id": "ps-pi", "providerInstanceId": "pi", "status": "running",
                        "extensionStatuses": [{ "key": "watch", "text": "Watching workspace" }]
                    }],
                    "providerThreads": [{ "id": "pt-pi", "providerInstanceId": "pi", "providerSessionId": "ps-pi" }],
                    "providerTurns": [], "runtimeRequests": [], "messages": [], "plans": [],
                    "turnItems": [
                        {
                            "id": "e1", "threadId": "main", "runId": "r1", "nodeId": "root",
                            "parentItemId": null, "ordinal": 1, "status": "failed",
                            "title": null, "startedAt": "2026-01-01T00:00:00Z", "completedAt": null,
                            "updatedAt": "2026-01-01T00:00:00Z", "type": "error",
                            "failure": {
                                "class": "usage_limit",
                                "message": "Usage limit reached",
                                "resetAt": "2099-01-01T05:00:00Z",
                                "retryable": null
                            }
                        },
                        {
                            "id": "cmd-1", "threadId": "main", "runId": "r1", "nodeId": "root",
                            "parentItemId": null, "ordinal": 2, "status": "running",
                            "title": "Running cargo test", "startedAt": "2026-01-01T00:00:00Z",
                            "completedAt": null, "updatedAt": "2026-01-01T00:00:00Z",
                            "type": "command_execution", "input": "cargo test -p t3-core --lib",
                            "nativeItemRef": { "provider": "pi", "itemId": "native-cmd-1" }
                        }
                    ],
                    "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [],
                    "contextTransfers": [], "visibleTurnItems": [], "updatedAt": ""
                })).unwrap());
                cx.emit(AppEvent::Shell);
                cx.emit(AppEvent::Thread);
            });
            state
        });
        let mut stage_view = None;
        let window = cx.open_window(size(px(820.), px(640.)), |window, cx| {
            let thread_view = cx.new(|cx| ThreadView::new(state.clone(), window, cx));
            thread_view.update(cx, |view, cx| view.sync(cx));
            let limit_banner = cx.new(|cx| crate::thread_view::limit_recovery::LimitRecoveryBanner::new(state.clone(), cx));
            let stage = cx.new(|_| CaptureStage { limit_banner, thread_view });
            stage_view = Some(stage.clone());
            stage
        }).unwrap();
        let stage = stage_view.unwrap();
        let draw = |cx: &mut gpui::HeadlessAppContext, name: Option<&str>| {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear();
            }).unwrap();
            if let (Some(name), Some(out)) = (name, std::env::var_os("T3_CAPTURE_381")) {
                let out = std::path::PathBuf::from(out);
                std::fs::create_dir_all(&out).unwrap();
                cx.capture_screenshot(window.into()).unwrap().save(out.join(name)).unwrap();
            }
        };
        draw(&mut cx, Some("banners-limit-pi.png"));

        // LimitRecovery owns snooze -> duplicate StatusLine::Snoozed is suppressed while background line shows.
        stage.read_with(&cx, |stage, cx| {
            let view = stage.thread_view.read(cx);
            assert_eq!(view.footer.lines.status, None, "limit recovery must suppress duplicate StatusLine::Snoozed");
            let bg = view.footer.lines.background.as_ref().expect("background work must render");
            assert_eq!(bg.items[0].item_id.as_deref(), Some("cmd-1"));
            assert!(!view.footer.lines.run_is_live);
        });

        // Open BackgroundWorkDialog and verify it opens cleanly and captures screenshot.
        cx.update_window(window.into(), |_, window, cx| {
            stage.update(cx, |stage, cx| {
                stage.thread_view.update(cx, |view, cx| view.open_background_details(window, cx));
            });
        }).unwrap();
        draw(&mut cx, Some("background-work-dialog.png"));
        stage.read_with(&cx, |stage, cx| {
            assert!(stage.thread_view.read(cx).footer.background_details.is_some());
        });

        // Setting status to Running sets run_is_live=true; clearing snooze ownership checks live state cleanly.
        state.update(&mut cx, |state, cx| {
            state.shell.snapshot.as_mut().unwrap().threads[0].status = "running".into();
            cx.emit(AppEvent::Shell);
        });
        draw(&mut cx, None);
        stage.read_with(&cx, |stage, cx| {
            assert!(stage.thread_view.read(cx).footer.lines.run_is_live);
        });
    }
}
