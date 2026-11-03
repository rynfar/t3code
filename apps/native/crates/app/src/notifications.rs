//! Presents thread notifications as `ThreadNotificationCoordinator.tsx` does: an in-app toast for
//! another thread while the window is focused, an OS notification while it is not. Clicking one
//! opens its thread; focusing the window clears the rest. `t3_core::notifications` decides what
//! notifies. `threadNotifications.ts` sounds and the macOS desktop bridge badge use AppKit.

use std::collections::HashMap;

use gpui::{App, Context, SystemNotification};
use t3_client::supervisor::ConnectionStatus;
use t3_core::notifications::{NotificationKind, PendingNotifications, ThreadNotification, ThreadNotifications, notification_tag};
use t3_core::sidebar::ENVIRONMENT_ID;
use t3_core::sidebar::status::ThreadStatus;

use crate::app_state::{AppState, Phase};
use crate::toast::{Toast, ToastKind};

pub(crate) use platform::prepare_sounds;

#[derive(Default)]
pub struct NotificationCenter {
    watcher: ThreadNotifications,
    /// Delivered OS notifications that focus retires: tag to thread.
    pending: PendingNotifications,
    /// Where each delivered notification leads, kept past retirement: a click can arrive after
    /// the focus it caused cleared `pending`. Tags are per thread, so this is bounded by threads.
    destinations: HashMap<String, String>,
    badge_count: Option<usize>,
}

/// Whether any of the app's windows has focus.
fn app_focused(cx: &mut App) -> bool {
    cx.windows().into_iter().any(|window| window.update(cx, |_, window, _| window.is_window_active()).unwrap_or(false))
}

impl AppState {
    pub(crate) fn sync_notification_state(&mut self, cx: &mut App) {
        let connected = matches!(self.phase, Phase::Online(ConnectionStatus::Connected));
        let focused = app_focused(cx);
        for tag in self.notifications.pending.sync(self.client_settings.notification_mode, connected, focused) {
            cx.dismiss_system_notification(&tag);
        }
        self.update_notification_badge();
    }

    fn update_notification_badge(&mut self) {
        let count = self.notifications.pending.badge_count();
        if self.notifications.badge_count != Some(count) {
            platform::set_badge(count);
            self.notifications.badge_count = Some(count);
        }
    }

    /// Turns the live shell's status transitions into notifications. Runs after shell changes.
    pub(crate) fn observe_notifications(&mut self, cx: &mut Context<Self>) {
        let mode = self.client_settings.notification_mode;
        self.sync_notification_state(cx);
        if !mode.has_sound() && !mode.has_desktop_notifications() && !self.client_settings.in_app_notifications_enabled {
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
        platform::play_sound(notification.kind, self.client_settings.notification_mode);
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
        self.notifications.pending.track(tag, notification.thread_id);
        self.update_notification_badge();
    }

    /// An OS notification was clicked: bring the app forward on its thread.
    pub fn open_notification(&mut self, tag: &str, cx: &mut Context<Self>) {
        let Some(thread_id) = self.notifications.destinations.get(tag).cloned() else { return };
        cx.activate(true);
        self.select_thread(&thread_id, cx);
    }

    /// Focus or a mode change retires delivered notifications.
    pub fn clear_pending_notifications(&mut self, cx: &mut App) {
        for tag in self.notifications.pending.clear() {
            cx.dismiss_system_notification(&tag);
        }
        self.update_notification_badge();
    }

    pub(crate) fn retire_thread_notifications(&mut self, thread_id: &str, cx: &mut App) {
        for tag in self.notifications.pending.visit(thread_id) {
            cx.dismiss_system_notification(&tag);
        }
        self.update_notification_badge();
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::OnceCell;

    use objc2::{AnyThread, MainThreadMarker, rc::Retained};
    use objc2_app_kit::{NSApplication, NSSound};
    use objc2_foundation::{NSData, NSString};
    use t3_core::notifications::{NotificationKind, NotificationMode, should_play_notification_sound};

    struct Sounds {
        completion: Option<Retained<NSSound>>,
        input: Option<Retained<NSSound>>,
    }

    impl Sounds {
        fn load() -> Self {
            let load = |bytes: &[u8]| NSSound::initWithData(NSSound::alloc(), &NSData::with_bytes(bytes));
            Self {
                completion: load(include_bytes!("../assets/sounds/notification-completion.mp3")),
                input: load(include_bytes!("../assets/sounds/notification-input.mp3")),
            }
        }
    }

    thread_local! {
        // AppKit access stays on GPUI's main thread; environment resets keep these sounds loaded.
        static SOUNDS: OnceCell<Sounds> = const { OnceCell::new() };
    }

    pub(crate) fn prepare_sounds(mode: NotificationMode) {
        if !mode.has_sound() || MainThreadMarker::new().is_none() {
            return;
        }
        SOUNDS.with(|sounds| {
            sounds.get_or_init(Sounds::load);
        });
    }

    pub fn play_sound(kind: NotificationKind, mode: NotificationMode) {
        if !mode.has_sound() || MainThreadMarker::new().is_none() {
            return;
        }
        SOUNDS.with(|sounds| {
            let Some(sounds) = sounds.get() else { return };
            let sound = match kind {
                NotificationKind::Completion => &sounds.completion,
                NotificationKind::Input => &sounds.input,
            };
            if let Some(sound) = sound
                && should_play_notification_sound(mode, sound.isPlaying())
            {
                sound.play();
            }
        });
    }

    pub fn set_badge(count: usize) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
        NSApplication::sharedApplication(mtm).dockTile().setBadgeLabel(label.as_deref());
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use t3_core::notifications::{NotificationKind, NotificationMode};

    pub(crate) fn prepare_sounds(_: NotificationMode) {}
    pub fn play_sound(_: NotificationKind, _: NotificationMode) {}
    pub fn set_badge(_: usize) {}
}
