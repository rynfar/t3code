//! Accessibility delivery for `web/src/lib/announce.ts` and the two thread announcers.
use crate::app_state::{AppEvent, AppState, Phase};
use gpui::{App, Context, Subscription, Task};
use std::time::{Duration, Instant};
use t3_core::announcements::{
    self, Activity, Announcer, Background, Priority, Status, ThreadMemory,
};

pub struct Announcements {
    queue: Announcer,
    clock: Instant,
    timer: Option<Task<()>>,
    background: Background,
    thread: Option<(String, ThreadMemory)>,
    activity: Activity,
    undo_notice: Option<t3_core::thread_actions::UndoNotice>,
    _events: Subscription,
}
impl Announcements {
    pub fn new(cx: &mut Context<AppState>) -> Self {
        let events = cx.subscribe(&cx.entity(), |state, _, event: &AppEvent, cx| {
            if matches!(
                event,
                AppEvent::Thread | AppEvent::Selection | AppEvent::Phase | AppEvent::Shell
            ) {
                state.observe_thread_announcement(cx);
            }
        });
        Self {
            queue: Announcer::default(),
            clock: Instant::now(),
            timer: None,
            background: Background::default(),
            thread: None,
            activity: Activity::default(),
            undo_notice: None,
            _events: events,
        }
    }
}
impl AppState {
    pub(crate) fn announce_undo_notice(&mut self, cx: &mut Context<Self>) {
        let notice = self.sidebar.undo.notice();
        if notice == self.announcements.undo_notice { return; }
        self.announcements.undo_notice = notice.clone();
        if let Some(notice) = notice {
            let label = crate::keymap::label("thread.undo", cx).map_or_else(|| "Undo".to_owned(), |shortcut| format!("{shortcut} to undo"));
            self.announce(format!("{}, {label}", notice.announcement_summary()), Some("thread-undo-notice"), cx);
        } else { self.cancel_announcement("thread-undo-notice"); }
    }
    pub fn announce(
        &mut self,
        message: impl Into<String>,
        source: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        self.announce_priority(message.into(), source, Priority::Polite, cx);
    }
    pub(crate) fn announce_priority(
        &mut self,
        message: String,
        source: Option<&'static str>,
        priority: Priority,
        cx: &mut Context<Self>,
    ) {
        let a = &mut self.announcements;
        a.queue.announce(
            message,
            source,
            priority,
            a.clock.elapsed().as_millis() as u64,
        );
        self.schedule_announcement(cx);
    }
    pub fn cancel_announcement(&mut self, source: &str) {
        self.announcements.queue.cancel(source);
    }
    fn schedule_announcement(&mut self, cx: &mut Context<Self>) {
        let Some(due) = self.announcements.queue.due() else {
            self.announcements.timer = None;
            return;
        };
        let wait = due.saturating_sub(self.announcements.clock.elapsed().as_millis() as u64);
        let focus = focused(cx);
        self.announcements.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(wait))
                .await;
            let _ = this.update(cx, |state, cx| {
                let now = state.announcements.clock.elapsed().as_millis() as u64;
                // GPUI exposes focus per window, but no app-wide focus-change subscription.
                if focused(cx) != focus {
                    state.announcements.queue.focus_changed(now);
                    state.schedule_announcement(cx);
                } else if let Some(message) = state.announcements.queue.flush(now) {
                    post(&message);
                } else if state.announcements.queue.due().is_some() {
                    state.schedule_announcement(cx);
                }
            });
        }));
    }
    pub(crate) fn observe_background_announcements(&mut self, cx: &mut Context<Self>) {
        if !self.shell.synchronized
            || !matches!(
                self.phase,
                Phase::Online(t3_client::supervisor::ConnectionStatus::Connected)
            )
        {
            return;
        }
        let now = crate::sidebar::controller::now_ms();
        if let Some(message) =
            self.announcements
                .background
                .observe(&self.shell, self.selected_thread.as_deref(), now)
        {
            self.announce(message, None, cx);
        }
    }
    fn observe_thread_announcement(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            self.phase,
            Phase::Online(t3_client::supervisor::ConnectionStatus::Connected)
        ) {
            self.announcements.background = Background::default();
            self.announcements.thread = None;
            self.announcements.queue = Announcer::default();
            self.announcements.timer = None;
            return;
        }
        if self
            .announcements
            .thread
            .as_ref()
            .is_some_and(|(id, _)| Some(id) != self.selected_thread.as_ref())
        {
            self.announcements.thread = None;
            self.cancel_announcement("thread-activity");
        }
        let Some(id) = self.selected_thread.as_deref() else {
            self.announcements.thread = None;
            self.cancel_announcement("thread-activity");
            return;
        };
        let Some(p) = self
            .thread
            .projection
            .as_ref()
            .filter(|p| p.thread.id == id)
        else {
            return;
        };
        let activity_run = p
            .runs
            .iter()
            .filter(|r| {
                ["preparing", "starting", "running", "waiting"].contains(&r.status.as_str())
            })
            .max_by_key(|r| r.ordinal)
            .or_else(|| p.runs.iter().max_by_key(|r| r.ordinal));
        let latest = p.runs.iter().max_by_key(|r| r.ordinal);
        let shell_run = self.shell.thread(id).and_then(|s| s.latest_run_id.as_deref().map(|id| (id, if s.status == "idle" { "completed" } else { s.status.as_str() })));
        if announcements::detail_behind(latest.map(|r| (r.id.as_str(), r.status.as_str())), shell_run) { return; }
        let run = if activity_run.is_some_and(|r| r.status == "cancelled" && r.started_at.is_none())
        {
            t3_core::announcements::latest_executed_run(&p.runs)
        } else {
            activity_run
        };
        let pending = |r: &&t3_core::thread::Entity| r.value["status"] == "pending";
        let approval = p.runtime_requests.iter().filter(pending).find(|r| !matches!(r.value["kind"].as_str(), Some("user_input" | "auth_refresh" | "dynamic_tool_call")));
        let input = p.runtime_requests.iter().filter(pending).find(|r| r.value["kind"] == "user_input");
        let child = self.sidebar.model.thread(id).and_then(|t| t.pending_subagent_request);
        use t3_core::sidebar::thread::SubagentRequest;
        let (status, request) = if approval.is_some() { (Status::Approval, approval) }
            else if child == Some(SubagentRequest::Approval) { (Status::Approval, None) }
            else if input.is_some() { (Status::Input, input) }
            else if child == Some(SubagentRequest::UserInput) { (Status::Input, None) }
            else {
                let runless = p.thread.lineage.relationship_to_parent.as_deref() == Some("subagent") && p.nodes.iter().rev().find(|n| n.value["kind"] == "root_turn" && n.value["runId"].is_null()).is_some_and(|n| matches!(n.value["status"].as_str(), Some("pending" | "running" | "waiting")));
                let working = runless || activity_run.is_some_and(|r| ["preparing", "starting", "running", "waiting"].contains(&r.status.as_str()));
                (if working { Status::Working } else { Status::Idle }, None)
            };
        let a = &mut self.announcements;
        let switched = a.thread.as_ref().is_none_or(|(key, _)| key != id);
        let request_id = request.map(|r| r.id.as_str());
        let run_id = run.map(|r| r.id.as_str());
        let run_status = run.map_or("", |r| r.status.as_str());
        if !switched
            && a.activity.status == status
            && a.activity.request_id.as_deref() == request_id
            && a.activity.run_id.as_deref() == run_id
            && a.activity.run_status == run_status
        {
            return;
        }
        let next = Activity {
            status,
            request_id: request_id.map(str::to_owned),
            run_id: run_id.map(str::to_owned),
            run_status: run_status.into(),
        };
        a.activity = next.clone();
        let previous = a
            .thread
            .take()
            .filter(|(key, _)| key == id)
            .map(|(_, memory)| memory);
        let (memory, text) = announcements::next_thread(previous, next);
        a.thread = Some((id.to_owned(), memory));
        if switched {
            a.queue.cancel("thread-activity");
        }
        if let Some(text) = text {
            self.announce(text, Some("thread-activity"), cx);
        }
    }
}
fn focused(cx: &mut App) -> Vec<Option<gpui::FocusHandle>> {
    cx.windows()
        .into_iter()
        .filter_map(|w| w.update(cx, |_, window, cx| window.focused(cx)).ok())
        .collect()
}
#[cfg(target_os = "macos")]
fn post(message: &announcements::Announcement) {
    use objc2::{MainThreadMarker, runtime::AnyObject};
    use objc2_app_kit::{
        NSAccessibilityAnnouncementKey, NSAccessibilityAnnouncementRequestedNotification,
        NSAccessibilityPostNotificationWithUserInfo, NSAccessibilityPriorityKey,
        NSAccessibilityPriorityLevel, NSApplication,
    };
    use objc2_foundation::{NSDictionary, NSNumber, NSString};
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let text = NSString::from_str(&message.message);
    let priority = NSNumber::new_isize(if message.priority == Priority::Assertive {
        NSAccessibilityPriorityLevel::High.0
    } else {
        NSAccessibilityPriorityLevel::Medium.0
    });
    // GPUI has no public AX element. AppKit accepts the application as the announcement host.
    unsafe {
        let info = NSDictionary::from_slices(
            &[NSAccessibilityAnnouncementKey, NSAccessibilityPriorityKey],
            &[&*text as &AnyObject, &*priority as &AnyObject],
        );
        let app = NSApplication::sharedApplication(mtm);
        NSAccessibilityPostNotificationWithUserInfo(
            &app,
            NSAccessibilityAnnouncementRequestedNotification,
            Some(&info),
        );
    }
    tracing::info!(text = %message.message, priority = ?message.priority, host = "NSApplication", "accessibility announcement posted");
}
#[cfg(not(target_os = "macos"))]
fn post(message: &announcements::Announcement) {
    tracing::info!(text = %message.message, "accessibility announcement (platform unsupported)");
}
