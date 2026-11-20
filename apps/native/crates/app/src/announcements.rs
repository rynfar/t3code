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
    detail: Option<(String, announcements::ScreenReaderAnnouncements, announcements::TurnDetailMemory)>,
    step_quiet_until: u64,
    step_waiting: Option<String>,
    step_timer: Option<Task<()>>,
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
            if matches!(event, AppEvent::Selection | AppEvent::Shell) {
                state.observe_background_announcements(cx);
            }
        });
        Self {
            queue: Announcer::default(),
            clock: Instant::now(),
            timer: None,
            background: Background::default(),
            thread: None,
            detail: None,
            step_quiet_until: 0,
            step_waiting: None,
            step_timer: None,
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
                    post(&message, state.runtime());
                } else if state.announcements.queue.due().is_some() {
                    state.schedule_announcement(cx);
                }
            });
        }));
    }
    pub(crate) fn reset_environment_announcements(&mut self) {
        self.announcements.background = Background::default();
        self.announcements.thread = None;
        self.announcements.detail = None;
        self.announcements.step_quiet_until = 0;
        self.announcements.step_waiting = None;
        self.announcements.step_timer = None;
        self.announcements.queue = Announcer::default();
        self.announcements.timer = None;
    }

    pub(crate) fn observe_background_announcements(&mut self, cx: &mut Context<Self>) {
        let now = crate::sidebar::controller::now_ms();
        let active_online = self.shell.synchronized
            && matches!(
                self.phase,
                Phase::Online(t3_client::supervisor::ConnectionStatus::Connected)
            );
        if active_online
            && let Some(message) =
                self.announcements
                    .background
                    .observe(&self.shell, self.selected_thread.as_deref(), now)
        {
            self.announce(message, None, cx);
        }
        let active_env = self.environments.active.as_deref();
        let secondary: Vec<_> = self
            .fleet
            .environments
            .iter()
            .filter(|entry| {
                entry.status == t3_client::supervisor::ConnectionStatus::Connected
                    && entry.summary.synchronized
                    && Some(entry.summary.owner.saved_id.as_str()) != active_env
            })
            .map(|entry| entry.summary.as_ref())
            .collect();
        let open = active_env.zip(self.selected_thread.as_deref());
        if let Some(message) =
            self.announcements
                .background
                .observe_secondary_fleet(&secondary, open, now)
        {
            self.announce(message, None, cx);
        }
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn flush_announcements_for_test(&mut self) -> Option<announcements::Announcement> {
        let due = self.announcements.queue.due()?;
        self.announcements.timer = None;
        self.announcements.queue.flush(due)
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
            self.announcements.detail = None;
            self.announcements.step_waiting = None;
            self.announcements.step_timer = None;
            self.announcements.step_quiet_until = 0;
            self.cancel_announcement("thread-activity");
            self.cancel_announcement("thread-detail");
        }
        let Some(id) = self.selected_thread.as_deref().map(str::to_owned) else {
            self.announcements.thread = None;
            self.announcements.detail = None;
            self.announcements.step_waiting = None;
            self.announcements.step_timer = None;
            self.cancel_announcement("thread-activity");
            self.cancel_announcement("thread-detail");
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
        let level = announcements::screen_reader_announcements_setting(
            crate::settings::client_settings(cx).entries(),
        );
        let child = self
            .sidebar
            .model
            .thread(&id)
            .and_then(|t| t.pending_subagent_request);
        let waiting_label = self.announcements.step_waiting.take();
        self.announcements.step_timer = None;
        let Some(announcements::ActivityRef {
            status,
            request_id,
            run_id,
            run_status,
        }) = announcements::open_activity(p, self.shell.thread(&id), child)
        else {
            if waiting_label.is_some()
                && let Some((_, _, memory)) = self.announcements.detail.as_mut()
            {
                memory.activity = None;
            }
            return;
        };
        let next = Activity {
            status,
            request_id: request_id.map(str::to_owned),
            run_id: run_id.map(str::to_owned),
            run_status: run_status.into(),
        };
        let known_tail = self
            .announcements
            .detail
            .as_ref()
            .filter(|(key, prev_level, _)| key == &id && *prev_level == level)
            .and_then(|(_, _, memory)| memory.tail_id.as_deref());
        let turn_detail = if level == announcements::ScreenReaderAnnouncements::Status {
            announcements::TurnDetail::default()
        } else {
            announcements::turn_detail_from_projection(p, known_tail)
        };
        let switched = self.announcements.thread.as_ref().is_none_or(|(key, _)| key != &id);
        let status_same = !switched && self.announcements.activity.status == next.status
            && self.announcements.activity.request_id == next.request_id
            && self.announcements.activity.run_id == next.run_id
            && self.announcements.activity.run_status == next.run_status;
        if status_same && level == announcements::ScreenReaderAnnouncements::Status {
            return;
        }
        let a = &mut self.announcements;
        a.activity = next.clone();
        let previous_status = a
            .thread
            .take()
            .filter(|(key, _)| key == &id)
            .map(|(_, memory)| memory);
        let (memory, status_text) = announcements::next_thread(previous_status, next);
        a.thread = Some((id.clone(), memory));

        let previous_detail = a
            .detail
            .take()
            .filter(|(key, prev_level, _)| key == &id && *prev_level == level)
            .map(|(_, _, memory)| memory);
        let detail_out = announcements::next_turn_detail_announcement(
            previous_detail.as_ref(),
            &turn_detail,
            level,
            status,
        );
        let current_activity = detail_out.memory.activity.clone();
        a.detail = Some((id, level, detail_out.memory));

        if switched {
            a.step_quiet_until = 0;
            a.queue.cancel("thread-activity");
            a.queue.cancel("thread-detail");
            return;
        }
        let now = a.clock.elapsed().as_millis() as u64;
        if !detail_out.messages.is_empty() {
            let mut parts = detail_out.messages;
            if let Some(status_text) = status_text {
                parts.push(status_text.to_owned());
            }
            let combined = announcements::as_one_announcement(&parts);
            let hold = announcements::hold_after(&combined);
            a.step_quiet_until = now.saturating_add(hold);
            self.announce(combined, Some("thread-detail"), cx);
        } else if let Some(text) = status_text {
            a.step_quiet_until = a.step_quiet_until.max(now.saturating_add(announcements::ACTIVITY_GAP_MS));
            self.announce(text, Some("thread-activity"), cx);
        }

        let step_label = if level != announcements::ScreenReaderAnnouncements::Everything || current_activity.is_none() {
            None
        } else {
            detail_out.activity.or(waiting_label)
        };
        let Some(step_label) = step_label else {
            return;
        };
        let now = self.announcements.clock.elapsed().as_millis() as u64;
        let wait = self.announcements.step_quiet_until.saturating_sub(now);
        if wait == 0 {
            self.announcements.step_quiet_until = now.saturating_add(announcements::ACTIVITY_GAP_MS);
            self.announce(step_label, Some("thread-detail"), cx);
        } else {
            self.announcements.step_waiting = Some(step_label.clone());
            self.announcements.step_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(wait)).await;
                let _ = this.update(cx, |state, cx| {
                    let Some(label) = state.announcements.step_waiting.take() else { return };
                    state.announcements.step_timer = None;
                    let now = state.announcements.clock.elapsed().as_millis() as u64;
                    state.announcements.step_quiet_until = now.saturating_add(announcements::ACTIVITY_GAP_MS);
                    state.announce(label, Some("thread-detail"), cx);
                });
            }));
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
#[cfg(any(target_os = "linux", test))]
fn build_spd_say_args(message: &announcements::Announcement) -> Option<Vec<String>> {
    if message.message.trim().is_empty() {
        return None;
    }
    let priority = match message.priority {
        Priority::Assertive => "important",
        Priority::Polite => "message",
    };
    Some(vec![
        "-p".to_owned(),
        priority.to_owned(),
        "--".to_owned(),
        message.message.clone(),
    ])
}

#[cfg(target_os = "macos")]
fn post(message: &announcements::Announcement, _runtime: &tokio::runtime::Handle) {
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
#[cfg(target_os = "linux")]
fn post(message: &announcements::Announcement, runtime: &tokio::runtime::Handle) {
    #[cfg(test)]
    {
        let _ = (message, runtime);
    }
    #[cfg(not(test))]
    {
        let Some(args) = build_spd_say_args(message) else {
            return;
        };
        let text = message.message.clone();
        let priority = message.priority;
        runtime.spawn(async move {
            match tokio::process::Command::new("spd-say")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
            {
                Ok(status) if status.success() => {
                    tracing::info!(text = %text, ?priority, host = "spd-say", "accessibility announcement posted");
                }
                Ok(status) => {
                    tracing::debug!(text = %text, ?priority, code = ?status.code(), "spd-say exited non-zero");
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    tracing::debug!(text = %text, ?priority, "spd-say unavailable on PATH");
                }
                Err(error) => {
                    tracing::debug!(text = %text, ?priority, %error, "spd-say failed to spawn");
                }
            }
        });
    }
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn post(message: &announcements::Announcement, _runtime: &tokio::runtime::Handle) {
    tracing::info!(text = %message.message, priority = ?message.priority, "accessibility announcement (platform fallback)");
}

#[cfg(test)]
mod tests {
    use super::*;
    use t3_core::announcements::{Announcement, Priority};

    #[test]
    fn spd_say_args_map_priority_and_guard_leading_dashes() {
        assert_eq!(
            build_spd_say_args(&Announcement {
                message: "Done: Build".into(),
                priority: Priority::Polite,
            }),
            Some(vec![
                "-p".into(),
                "message".into(),
                "--".into(),
                "Done: Build".into(),
            ])
        );
        assert_eq!(
            build_spd_say_args(&Announcement {
                message: "Needs approval".into(),
                priority: Priority::Assertive,
            }),
            Some(vec![
                "-p".into(),
                "important".into(),
                "--".into(),
                "Needs approval".into(),
            ])
        );
        assert_eq!(
            build_spd_say_args(&Announcement {
                message: "-r 100".into(),
                priority: Priority::Polite,
            }),
            Some(vec![
                "-p".into(),
                "message".into(),
                "--".into(),
                "-r 100".into(),
            ])
        );
        assert_eq!(
            build_spd_say_args(&Announcement {
                message: "   ".into(),
                priority: Priority::Polite,
            }),
            None
        );
    }
}
