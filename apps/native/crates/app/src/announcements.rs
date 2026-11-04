//! Accessibility delivery for `web/src/lib/announce.ts` and the two thread announcers.
use crate::app_state::{AppEvent, AppState, Phase};
use gpui::{Context, Entity, FocusHandle, Subscription, Task, Window};
use std::time::{Duration, Instant};
use t3_core::announcements::{self, Activity, Announcer, Background, Priority, ThreadMemory};

pub struct Announcements {
    queue: Announcer<FocusHandle>,
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
        if notice == self.announcements.undo_notice {
            return;
        }
        self.announcements.undo_notice = notice.clone();
        if let Some(notice) = notice {
            let label = crate::keymap::label("thread.undo", cx).map_or_else(
                || "Undo".to_owned(),
                |shortcut| format!("{shortcut} to undo"),
            );
            self.announce(
                format!("{}, {label}", notice.announcement_summary()),
                Some("thread-undo-notice"),
                cx,
            );
        } else {
            self.cancel_announcement("thread-undo-notice");
        }
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
    fn announcement_focus_changed(&mut self, focus: Option<FocusHandle>, cx: &mut Context<Self>) {
        let now = self.announcements.clock.elapsed().as_millis() as u64;
        if self.announcements.queue.focus_changed(focus, now) {
            self.schedule_announcement(cx);
        }
    }
    fn schedule_announcement(&mut self, cx: &mut Context<Self>) {
        let Some(due) = self.announcements.queue.due() else {
            self.announcements.timer = None;
            return;
        };
        let wait = due.saturating_sub(self.announcements.clock.elapsed().as_millis() as u64);
        self.announcements.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(wait))
                .await;
            let _ = this.update(cx, |state, cx| {
                let now = state.announcements.clock.elapsed().as_millis() as u64;
                if let Some(message) = state.announcements.queue.flush(now) {
                    post(&message);
                } else if state.announcements.queue.due().is_some() {
                    state.schedule_announcement(cx);
                }
            });
        }));
    }
    pub(crate) fn reset_environment_announcements(&mut self) {
        self.announcements.background = Background::default();
        self.announcements.thread = None;
        self.announcements.queue = Announcer::default();
        self.announcements.timer = None;
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
            self.reset_environment_announcements();
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
        let child = self
            .sidebar
            .model
            .thread(id)
            .and_then(|t| t.pending_subagent_request);
        let Some(announcements::ActivityRef {
            status,
            request_id,
            run_id,
            run_status,
        }) = announcements::open_activity(p, self.shell.thread(id), child)
        else {
            return;
        };
        let a = &mut self.announcements;
        let switched = a.thread.as_ref().is_none_or(|(key, _)| key != id);
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
/// GPUI reports changes through each focus handle's blur event; rearm on its successor.
pub(crate) struct FocusObserver {
    pub root: FocusHandle,
    state: Entity<AppState>,
    watched: Option<(FocusHandle, Subscription)>,
    _entry: Subscription,
    _exit: Subscription,
}
impl FocusObserver {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = cx.focus_handle();
        let entry = cx.on_focus_in(&root, window, |this, window, cx| this.observe(window, cx));
        let exit = cx.on_focus_out(&root, window, |this, _, _, cx| {
            this.watched = None;
            this.state
                .update(cx, |state, cx| state.announcement_focus_changed(None, cx));
        });
        let mut observer = Self {
            root,
            state,
            watched: None,
            _entry: entry,
            _exit: exit,
        };
        observer.observe(window, cx);
        observer
    }
    fn observe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = window
            .is_window_active()
            .then(|| window.focused(cx))
            .flatten();
        self.state.update(cx, |state, cx| {
            state.announcement_focus_changed(focus.clone(), cx)
        });
        if self.watched.as_ref().map(|(handle, _)| handle) == focus.as_ref() {
            return;
        }
        self.watched = focus.map(|handle| {
            let subscription =
                cx.on_blur(&handle, window, |this, window, cx| this.observe(window, cx));
            (handle, subscription)
        });
    }
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
