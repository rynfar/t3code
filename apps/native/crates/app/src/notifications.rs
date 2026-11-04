//! Presents thread notifications as `ThreadNotificationCoordinator.tsx` does: an in-app toast for
//! another thread while the window is focused, an OS notification while it is not. Clicking one
//! opens its thread; focusing the window clears the rest. `t3_core::notifications` decides what
//! notifies. `threadNotifications.ts` sounds and the macOS desktop bridge badge use AppKit.

use gpui::{App, Context, SystemNotification};
use t3_core::notifications::{DeliveryOutcome, NotificationKind, PendingNotifications, ThreadNotification, NotificationVisibility, NotificationRoute, notification_route, presentation};
use t3_core::sidebar::tag_filter::ThreadKey;
use t3_core::sidebar::status::ThreadStatus;

use crate::app_state::AppState;
use crate::toast::{Toast, ToastKind};

mod delivery;

pub(crate) use platform::prepare_sounds;

#[derive(Default)]
pub struct NotificationCenter {
    pub(crate) visibility: NotificationVisibility,
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
        let focused = app_focused(cx);
        for tag in self.notifications.pending.sync(self.client_settings.notification_mode, true, focused) {
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

    pub(crate) fn observe_notifications(&mut self, cx: &mut Context<Self>) {
        self.connections.notifications_enabled(self.client_settings.notification_mode != t3_core::notifications::NotificationMode::Off || self.client_settings.in_app_notifications_enabled);
        self.sync_notification_state(cx);
    }

    pub(crate) fn present_notification(&mut self, environment: &str, notification: ThreadNotification, cx: &mut Context<Self>) {
        let target = ThreadKey::new(environment, &notification.thread_id);
        let plan = presentation(
            self.client_settings.notification_mode,
            self.client_settings.in_app_notifications_enabled,
            app_focused(cx),
            self.environments.active.as_deref() == Some(environment) && self.selected_thread.as_deref() == Some(notification.thread_id.as_str()),
            !self.notification_visible(&target),
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
            let toast = Toast::new(kind, notification.title).description(notification.body).open_environment_thread(target.clone());
            self.show_toast(toast, cx);
            return;
        }
        if !plan.desktop {
            return;
        }
        let attempt = self.notifications.pending.begin(target);
        let system_notification = SystemNotification {
            tag: attempt.tag.clone().into(),
            title: notification.title.into(),
            body: notification.body.into(),
            actions: Vec::new(),
        };
        cx.spawn(async move |this, cx| {
            let authorization = delivery::authorize().await;
            let outcome = if matches!(authorization, DeliveryOutcome::Accepted | DeliveryOutcome::Unconfirmed) {
                let posted = this.update(cx, |state, cx| {
                    state.sync_notification_state(cx);
                    state.retire_hidden_notifications(cx);
                    state.notifications.pending.is_current(&attempt).then(|| delivery::post(system_notification, cx))
                }).ok().flatten();
                let Some(posted) = posted else { return };
                posted.await
            } else { authorization };
            let _ = this.update(cx, |state, cx| {
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
        ThreadKey::new(self.environments.active.as_deref().unwrap_or("local"), thread_id)
    }

    fn notification_visible(&self, target: &ThreadKey) -> bool {
        if self.notifications.visibility.hidden.contains(target) { return false; }
        if self.environments.active.as_deref() == Some(&target.environment_id) {
            if self.sidebar.hidden_thread_ids.contains(&target.thread_id) { return false; }
            if self.thread_shell(&target.thread_id).is_some() { return true; }
        }
        self.notifications.visibility.visible(target)
    }

    pub(crate) fn retire_hidden_notifications(&mut self, cx: &mut Context<Self>) {
        if self.notifications.pending.is_empty() && !self.toasts.has_thread_notifications() { return; }
        let mut pending = std::mem::take(&mut self.notifications.pending);
        for tag in pending.retire_where(|key| !self.notification_visible(key)) { cx.dismiss_system_notification(&tag); }
        pending.prune_destinations(|key| self.notifications.visibility.live.contains(key) || (self.environments.active.as_deref() == Some(&key.environment_id) && self.thread_shell(&key.thread_id).is_some()));
        self.notifications.pending = pending;
        let mut toasts = std::mem::take(&mut self.toasts);
        if toasts.retire_environment_notifications(|key| self.notification_visible(key)) { cx.emit(crate::app_state::AppEvent::Notice); }
        self.toasts = toasts;
        self.update_notification_badge();
    }

    pub(crate) fn retire_environment_notifications(&mut self, environment: &str, cx: &mut Context<Self>) {
        for tag in self.notifications.pending.retire_where(|key| key.environment_id == environment) { cx.dismiss_system_notification(&tag); }
        if self.toasts.retire_environment_notifications(|key| key.environment_id != environment) { cx.emit(crate::app_state::AppEvent::Notice); }
        self.update_notification_badge();
    }

    pub fn open_notification(&mut self, tag: &str, cx: &mut Context<Self>) {
        let Some(target) = self.notifications.pending.destination(tag).cloned() else { return };
        cx.activate(true);
        self.open_notification_target(target, cx);
    }

    pub(crate) fn open_notification_target(&mut self, target: ThreadKey, cx: &mut Context<Self>) {
        if self.environments.get(&target.environment_id).is_none_or(|e| e.disabled || e.access_token.is_none()) { return; }
        if !self.notification_visible(&target) { return; }
        let environment = target.environment_id.clone();
        let route = notification_route(&target, self.environments.active.as_deref(), false, false);
        self.notification_route = Some(target);
        if route == NotificationRoute::SwitchEnvironment {
            self.connect_environment(&environment, cx);
        } else { self.finish_notification_route(cx); }
    }

    pub(crate) fn finish_notification_route(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.notification_route.as_ref() else { return };
        let route = notification_route(target, self.environments.active.as_deref(), self.client_handle().is_some() && self.shell.synchronized, self.thread_shell(&target.thread_id).is_some());
        if matches!(route, NotificationRoute::SwitchEnvironment | NotificationRoute::WaitForShell) { return; }
        let target = self.notification_route.take().unwrap();
        self.draft_project = None;
        if !self.notification_visible(&target) { self.land_home(cx); return; }
        match route {
            NotificationRoute::OpenThread => self.select_thread(&target.thread_id, cx),
            NotificationRoute::ReadFamily => self.read_family(&target.thread_id, true, cx),
            _ => {}
        }
        self.land_home(cx);
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
