//! What the thread itself is doing, around the transcript. The banner lane over its top
//! (`ChatBannerLane.tsx`: `ProviderStatusBanner.tsx`, then `ThreadErrorBanner.tsx`) and the lines
//! after the last row (`ThreadNow.tsx`, `ThreadStatusLine.tsx`, `ThreadActivityBanners.tsx`): a
//! Stop with its elapsed time, the goal, then Snoozed, Settled or Woke from snooze.

use std::collections::HashSet;
use std::time::Duration;

use gpui::{AnyElement, ClipboardItem, Context, Entity, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Styled, Task, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands::{self, DispatchMode, method};
use t3_core::timeline::{
    BackgroundWork, CHATGPT_USAGE_URL, CHATGPT_WORKSPACE_ADMIN_REQUEST, ChildInputs, FORCE_STOP_THREAD, GoalLine, ProviderBanner, RunStop, StatusLine, ThreadError,
    background_work, child_inputs, error_banner_message, error_can_expand, force_stop_notice, force_stop_request, goal_line, provider_banner, run_stop, status_line,
    thread_error, timeline_thread_error,
};

use super::ThreadView;
use crate::app_state::uuid_v4;
use crate::composer::controls::{Button, ButtonSize, ButtonVariant};
use crate::glass;
use crate::icons::{Icon, ProviderLogo, provider_logo};
use crate::theme::{hsla, metrics, palette};
use crate::toast::{Toast, ToastKind};
use crate::transcript::{InlineTone, TimerKind, WorkingTimer, ghost_icon_button, ghost_text_button, inline_button};

/// The lines after the last row, in the web's order.
#[derive(Debug, Clone, Default, PartialEq)]
struct FooterLines {
    stop: Option<RunStop>,
    /// Subagents and side chats waiting on the user.
    children: Option<ChildInputs>,
    /// Background commands and subagents left running after the turn.
    background: Option<BackgroundWork>,
    /// The thread is resubscribing ("Syncing messages...").
    syncing: bool,
    goal: Option<GoalLine>,
    status: Option<StatusLine>,
}

impl FooterLines {
    fn is_shown(&self) -> bool {
        self.stop.is_some() || self.children.is_some() || self.background.is_some() || self.syncing || self.goal.is_some() || self.status.is_some()
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
    status_pending: bool,
    goal_pending: bool,
    force_stopping: bool,
    /// The background tasks a Stop was sent for, until they change or the retry window passes.
    background_stop: Option<(Vec<String>, std::time::Instant)>,
    /// When the thread was first seen unsynchronized, for the sync line's delay.
    unsynced_since: Option<std::time::Instant>,
    /// Re-reads the relative labels ("Settled 5m ago", "Snoozed, 2h left") once a minute.
    minute: Option<Task<()>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct BannerLines {
    provider: Option<ProviderBanner>,
    error: Option<ThreadError>,
}

/// The banner lane and what was dismissed. Dismissed errors last the session, as on the web.
#[derive(Default)]
pub struct Banners {
    lines: BannerLines,
    dismissed_errors: HashSet<String>,
    dismissed_provider: Option<String>,
    error_expanded: bool,
}

impl ThreadView {
    pub(super) fn sync_status(&mut self, cx: &mut Context<Self>) {
        self.sync_banners(cx);
        self.sync_footer(cx);
    }

    fn sync_banners(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let mut next = BannerLines::default();
        if let (Some(projection), Some(thread_id)) = (self.thread_state(state).projection.as_ref(), self.thread_id.as_deref()) {
            let thread = &projection.thread;
            let local = self.local_error.clone().map(|message| ThreadError { message, class: None, code: None, reset_at: None, runtime_failed: false });
            next.error = local
                .or_else(|| timeline_thread_error(thread_error(projection), state.thread_shell(thread_id)))
                .filter(|error| !self.banners.dismissed_errors.contains(&error.banner_key(thread_id)));
            let instance = if thread.provider_instance_id.is_empty() { &thread.model_selection.instance_id } else { &thread.provider_instance_id };
            let provider = state.server_config.config.as_ref().and_then(|config| config.providers.iter().find(|provider| &provider.instance_id == instance));
            next.provider = provider.and_then(provider_banner);
        }
        match &next.provider {
            None => self.banners.dismissed_provider = None,
            Some(banner) if self.banners.dismissed_provider.as_deref() == Some(banner.key.as_str()) => next.provider = None,
            Some(_) => {}
        }
        if next == self.banners.lines {
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
        let unsynced = thread.projection.is_some() && !thread.synchronized;
        let syncing = match (unsynced, self.footer.unsynced_since) {
            (false, _) => {
                self.footer.unsynced_since = None;
                false
            }
            (true, Some(since)) => since.elapsed() >= SYNC_LINE_DELAY,
            (true, None) => {
                self.footer.unsynced_since = Some(std::time::Instant::now());
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SYNC_LINE_DELAY).await;
                    let _ = this.update(cx, |view, cx| view.sync_footer(cx));
                })
                .detach();
                false
            }
        };
        let state = self.state.read(cx);
        let shell = self.thread_id.as_deref().and_then(|id| state.thread_shell(id));
        let next = FooterLines {
            stop: self.thread_state(state).projection.as_ref().and_then(|projection| run_stop(projection, background)),
            children,
            background: shell.filter(|_| !working).and_then(|shell| background_work(&shell.pending_background_tasks)),
            syncing,
            goal: shell.and_then(|shell| shell.goal.as_ref()).and_then(|goal| goal_line(goal, working)),
            status: self.thread_id.as_deref().and_then(|id| state.sidebar.model.thread(id)).filter(|_| config_loaded).and_then(|thread| status_line(thread, now)),
        };
        let task_ids: Vec<String> = next.background.iter().flat_map(|work| work.items.iter().map(|item| item.task_id.clone())).collect();
        if self.footer.background_stop.as_ref().is_some_and(|(ids, at)| *ids != task_ids || at.elapsed() >= BACKGROUND_STOP_RETRY) {
            self.footer.background_stop = None;
        }
        if next == self.footer.lines {
            return;
        }
        let was_shown = self.footer.lines.is_shown();
        self.footer.stop_timer = match &next.stop {
            Some(stop) if self.footer.stop_timer.as_ref().is_some_and(|timer| timer.read(cx).matches(&stop.requested_at, TimerKind::Elapsed)) => {
                self.footer.stop_timer.take()
            }
            Some(stop) => {
                let started = stop.requested_at.clone();
                Some(cx.new(|cx| WorkingTimer::new(started, TimerKind::Elapsed, cx)))
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
        let end = self.header_count() + self.rows.len();
        match (was_shown, self.footer.lines.is_shown()) {
            (false, true) => self.list.splice(end..end, 1),
            (true, false) => self.list.splice(end..end + 1, 0),
            (true, true) => self.list.remeasure_items(end..end + 1),
            (false, false) => {}
        }
        cx.notify();
    }

    pub(super) fn footer_shown(&self) -> bool {
        self.footer.lines.is_shown()
    }

    /// Forgets the footer and the thread's local error when another thread opens; the list was reset.
    pub(super) fn reset_status(&mut self) {
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
                view.list.remeasure_items(view.header_count() + view.rows.len()..view.header_count() + view.rows.len() + 1);
                cx.notify();
            });
        })
        .detach();
        self.remeasure_footer();
        cx.notify();
    }

    fn remeasure_footer(&self) {
        if self.footer.lines.is_shown() {
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

    pub(super) fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let muted = hsla(p.muted_foreground);
        let detail = if cx.theme().is_dark() { muted.opacity(0.6) } else { muted.opacity(0.8) };
        let line = || h_flex().w_full().min_w_0().gap(px(6.)).px(px(4.)).pb(px(8.)).text_xs().text_color(muted);
        let text = |title: String, description: Option<String>| {
            h_flex()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(div().flex_none().child(title))
                .children(description.map(|description| div().min_w_0().truncate().ml(px(6.)).text_color(detail).child(description)))
        };
        let actions = || h_flex().flex_none().ml(px(6.)).gap(px(10.));
        let mut lines: Vec<AnyElement> = Vec::new();
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
            line()
                .child(div().flex_none().child(icon))
                .child(text(stop.title().to_owned(), stop.description().map(str::to_owned)))
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
            let icon = if background.all_commands() { Icon::Terminal } else { Icon::CircleDashed };
            let stopping = self.footer.background_stop.is_some();
            let open = background.only_child_thread().map(|thread_id| {
                let id = thread_id.to_owned();
                inline_button("background-open", "Open", InlineTone::Muted, false, cx.listener(move |view, _, _, cx| view.open_thread(&id, cx)), cx)
            });
            let stop = inline_button(
                "background-stop",
                if stopping { "Stopping..." } else { "Stop" },
                InlineTone::Default,
                stopping,
                cx.listener(|view, _, _, cx| view.stop_background_work(cx)),
                cx,
            );
            Some(
                line()
                    .child(div().flex_none().child(icon.render(px(14.), muted)))
                    .child(text(background.title.clone(), background.description()))
                    .child(actions().children(open).child(stop))
                    .into_any_element(),
            )
        });
        lines.extend(attention);
        lines.extend(work);
        if self.footer.lines.syncing {
            lines.push(line().child(div().flex_none().child(Icon::RefreshCw.render(px(14.), muted))).child(div().min_w_0().truncate().child("Syncing messages...")).into_any_element());
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
            lines.push(
                line()
                    .child(div().flex_none().child(Icon::Target.render(px(14.), muted)))
                    .child(text(goal.title.clone(), Some(goal.objective.clone())))
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
        v_flex().w_full().children(lines).into_any_element()
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
            actions.push(ghost_text_button("manage-usage", "Manage usage", Some(Icon::ExternalLink), |_, _, cx| cx.open_url(CHATGPT_USAGE_URL), cx));
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
                    view.banners.dismissed_provider = Some(key.clone());
                    view.sync_banners(cx);
                }),
                cx,
            );
            banners.push(alert(banner.warning, AlertIcon::Lucide(Icon::Info), Some(banner.title.clone()), vec![body], vec![dismiss], f32::INFINITY, cx));
        }
        if let (Some(error), Some(thread_id)) = (&lines.error, self.thread_id.as_deref()) {
            let key = error.banner_key(thread_id);
            let body: Vec<AnyElement> = if error.chatgpt_limit() {
                let text = if error.workspace_limit() {
                    "You've reached your workspace's limit for using your ChatGPT plan in other apps. Copy a request to ask your workspace admin for increased usage limits."
                } else {
                    "Review your usage settings in ChatGPT."
                };
                vec![v_flex().gap(px(4.)).child(div().font_weight(FontWeight::MEDIUM).child("ChatGPT usage limit reached")).child(text).into_any_element()]
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
