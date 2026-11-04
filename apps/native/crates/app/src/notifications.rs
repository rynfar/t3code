//! Presents thread notifications as `ThreadNotificationCoordinator.tsx` does: an in-app toast for
//! another thread while the window is focused, an OS notification while it is not. Clicking one
//! opens its thread; focusing the window clears the rest. `t3_core::notifications` decides what
//! notifies. `threadNotifications.ts` sounds and the macOS desktop bridge badge use AppKit.

use gpui::{App, Context, SystemNotification};
use t3_client::supervisor::ConnectionStatus;
use t3_core::notifications::{DeliveryOutcome, NotificationKind, PendingNotifications, ThreadNotification, ThreadNotifications, notification_visible, presentation};
use t3_core::sidebar::tag_filter::ThreadKey;
use t3_core::sidebar::ENVIRONMENT_ID;
use t3_core::sidebar::status::ThreadStatus;

use crate::app_state::{AppState, Phase};
use crate::toast::{Toast, ToastKind};

mod delivery;

pub(crate) use platform::prepare_sounds;

#[derive(Default)]
pub struct NotificationCenter {
    watcher: ThreadNotifications,
    /// Delivered OS notifications that focus retires: tag to thread.
    pending: PendingNotifications,
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
        for notification in self.notifications.watcher.observe_visible(&self.shell, &self.sidebar.hidden_thread_ids) {
            self.present_notification(notification, cx);
        }
    }

    fn present_notification(&mut self, notification: ThreadNotification, cx: &mut Context<Self>) {
        let plan = presentation(
            self.client_settings.notification_mode,
            self.client_settings.in_app_notifications_enabled,
            app_focused(cx),
            self.selected_thread.as_deref() == Some(notification.thread_id.as_str()),
            self.sidebar.hidden_thread_ids.contains(&notification.thread_id),
        );
        if plan.sound {
            platform::play_sound(notification.kind, self.client_settings.notification_mode);
        }
        if plan.toast {
            let kind = match (notification.kind, notification.status) {
                (NotificationKind::Completion, _) => ToastKind::Success,
                (_, ThreadStatus::Failed) => ToastKind::Error,
                _ => ToastKind::Warning,
            };
            let toast = Toast::new(kind, notification.title).description(notification.body).open_thread(notification.thread_id);
            self.show_toast(toast, cx);
            return;
        }
        if !plan.desktop {
            return;
        }
        let attempt = self.notifications.pending.begin(self.notification_target(&notification.thread_id));
        let system_notification = SystemNotification {
            tag: attempt.tag.clone().into(),
            title: notification.title.into(),
            body: notification.body.into(),
            actions: Vec::new(),
        };
        let connection = self.connection_id();
        cx.spawn(async move |this, cx| {
            let authorization = delivery::authorize().await;
            let outcome = if matches!(authorization, DeliveryOutcome::Accepted | DeliveryOutcome::Unconfirmed) {
                let posted = this.update(cx, |state, cx| {
                    if state.connection_id() != connection { return None; }
                    state.sync_notification_state(cx);
                    state.retire_hidden_notifications(cx);
                    state.notifications.pending.is_current(&attempt).then(|| delivery::post(system_notification, cx))
                }).ok().flatten();
                let Some(posted) = posted else { return };
                posted.await
            } else { authorization };
            let _ = this.update(cx, |state, cx| {
                // An old environment's callback must never change the new environment's state.
                if state.connection_id() != connection {
                    if !state.notifications.pending.is_outstanding(&attempt.tag) { cx.dismiss_system_notification(&attempt.tag); }
                    return;
                }
                state.sync_notification_state(cx);
                state.retire_hidden_notifications(cx);
                state.notifications.pending.confirm(&attempt, outcome);
                if !state.notifications.pending.is_outstanding(&attempt.tag) {
                    cx.dismiss_system_notification(&attempt.tag);
                }
                tracing::debug!(tag = %attempt.tag, ?outcome, "system notification delivery completed");
                state.update_notification_badge();
            });
        }).detach();
    }

    fn notification_target(&self, thread_id: &str) -> ThreadKey {
        let environment = self.server_config.config.as_ref().map(|config| config.environment.environment_id.as_str());
        ThreadKey::new(environment.filter(|id| !id.is_empty()).unwrap_or(ENVIRONMENT_ID), thread_id)
    }

    pub(crate) fn retire_hidden_notifications(&mut self, cx: &mut Context<Self>) {
        if self.notifications.pending.is_empty() && !self.toasts.has_thread_notifications() { return; }
        let hidden = &self.sidebar.hidden_thread_ids;
        let live: std::collections::HashSet<_> = self.shell.snapshot.iter().flat_map(|snapshot| snapshot.threads.iter())
            .chain(self.omitted.mounts.extras(&self.shell)).map(|thread| thread.id.clone()).collect();
        let filtering = self.server_config.config.as_ref().is_some_and(|config| config.settings.hidden_tags().is_some());
        let visible = |id: &str| notification_visible(id, hidden, filtering.then_some(&live));
        for tag in self.notifications.pending.retire_where(|key| !visible(&key.thread_id)) {
            cx.dismiss_system_notification(&tag);
        }
        self.notifications.pending.prune_destinations(|key| live.contains(&key.thread_id));
        if self.toasts.retire_thread_notifications(visible) {
            cx.emit(crate::app_state::AppEvent::Notice);
        }
        self.update_notification_badge();
    }

    /// An OS notification was clicked: bring the app forward on its thread.
    pub fn open_notification(&mut self, tag: &str, cx: &mut Context<Self>) {
        let Some(target) = self.notifications.pending.destination(tag).cloned() else { return };
        if target.environment_id != self.notification_target(&target.thread_id).environment_id { return; }
        cx.activate(true);
        self.select_thread(&target.thread_id, cx);
    }

    /// Focus or a mode change retires delivered notifications.
    pub fn clear_pending_notifications(&mut self, cx: &mut App) {
        for tag in self.notifications.pending.clear() {
            cx.dismiss_system_notification(&tag);
        }
        self.update_notification_badge();
    }

    pub(crate) fn retire_thread_notifications(&mut self, thread_id: &str, cx: &mut App) {
        for tag in self.notifications.pending.visit_target(&self.notification_target(thread_id)) {
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
