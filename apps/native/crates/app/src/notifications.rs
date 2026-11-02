//! Presents thread notifications as `ThreadNotificationCoordinator.tsx` does: an in-app toast for
//! another thread while the window is focused, an OS notification while it is not. Clicking one
//! opens its thread; focusing the window clears the rest. `t3_core::notifications` decides what
//! notifies. GPUI has no audio or dock-badge API, so the sound modes and the badge are not ported.

use std::collections::HashMap;

use gpui::{App, Context, SystemNotification};
use t3_client::supervisor::ConnectionStatus;
use t3_core::notifications::{NotificationKind, NotificationMode, ThreadNotification, ThreadNotifications, notification_tag};
use t3_core::sidebar::ENVIRONMENT_ID;
use t3_core::sidebar::status::ThreadStatus;

use crate::app_state::{AppState, Phase};
use crate::toast::{Toast, ToastKind};

#[derive(Default)]
pub struct NotificationCenter {
    watcher: ThreadNotifications,
    /// Delivered OS notifications that focus retires: tag to thread.
    pending: HashMap<String, String>,
    /// Where each delivered notification leads, kept past retirement: a click can arrive after
    /// the focus it caused cleared `pending`. Tags are per thread, so this is bounded by threads.
    destinations: HashMap<String, String>,
    applied_mode: Option<NotificationMode>,
}

/// Whether any of the app's windows has focus.
fn app_focused(cx: &mut App) -> bool {
    cx.windows().into_iter().any(|window| window.update(cx, |_, window, _| window.is_window_active()).unwrap_or(false))
}

impl AppState {
    /// Turns the live shell's status transitions into notifications. Runs after shell changes.
    pub(crate) fn observe_notifications(&mut self, cx: &mut Context<Self>) {
        let mode = self.client_settings.notification_mode;
        if self.notifications.applied_mode != Some(mode) {
            self.notifications.applied_mode = Some(mode);
            self.clear_pending_notifications(cx);
        }
        if mode == NotificationMode::Off && !self.client_settings.in_app_notifications_enabled {
            self.notifications.watcher.clear();
            return;
        }
        // A reconnecting shell is not live; its next live state is compared with the last one.
        let live = self.shell.synchronized && matches!(self.phase, Phase::Online(ConnectionStatus::Connected));
        if !live {
            return;
        }
        for notification in self.notifications.watcher.observe(&self.shell) {
            self.present_notification(notification, cx);
        }
    }

    fn present_notification(&mut self, notification: ThreadNotification, cx: &mut Context<Self>) {
        let focused = app_focused(cx);
        if self.client_settings.in_app_notifications_enabled
            && focused
            && self.selected_thread.as_deref() != Some(notification.thread_id.as_str())
        {
            let kind = match (notification.kind, notification.status) {
                (NotificationKind::Completion, _) => ToastKind::Success,
                (_, ThreadStatus::Failed) => ToastKind::Error,
                _ => ToastKind::Warning,
            };
            let toast = Toast::new(kind, notification.title).description(notification.body).open_thread(notification.thread_id);
            self.show_toast(toast, cx);
            return;
        }
        if !self.client_settings.notification_mode.has_desktop_notifications() || focused {
            return;
        }
        let environment = self.server_config.config.as_ref().map(|config| config.environment.environment_id.as_str());
        let tag = notification_tag(environment.filter(|id| !id.is_empty()).unwrap_or(ENVIRONMENT_ID), &notification.thread_id);
        cx.show_system_notification(SystemNotification {
            tag: tag.clone().into(),
            title: notification.title.into(),
            body: notification.body.into(),
            actions: Vec::new(),
        });
        self.notifications.destinations.insert(tag.clone(), notification.thread_id.clone());
        self.notifications.pending.insert(tag, notification.thread_id);
    }

    /// An OS notification was clicked: bring the app forward on its thread.
    pub fn open_notification(&mut self, tag: &str, cx: &mut Context<Self>) {
        self.notifications.pending.remove(tag);
        let Some(thread_id) = self.notifications.destinations.get(tag).cloned() else { return };
        cx.activate(true);
        self.select_thread(&thread_id, cx);
    }

    /// Focus or a mode change retires delivered notifications.
    pub fn clear_pending_notifications(&mut self, cx: &mut Context<Self>) {
        for (tag, _) in self.notifications.pending.drain() {
            cx.dismiss_system_notification(&tag);
        }
    }
}
