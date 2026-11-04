use serde_json::{Value, json};

use super::*;
use crate::shell::{ShellSnapshot, ShellStreamItem};

fn thread(id: &str, status: &str, extra: Value) -> ThreadShell {
    let mut value = json!({
        "id": id, "projectId": "p1", "title": format!("Title {id}"), "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "latestRunId": "run-1", "activeRunId": null, "status": status, "pendingRuntimeRequest": null,
        "latestVisibleMessage": null, "latestUserMessageAt": null, "hasActionableProposedPlan": false,
        "itemCount": 1, "visibleItemCount": 1, "createdAt": "2026-11-01T00:00:00.000Z",
        "updatedAt": "2026-11-01T00:00:00.000Z", "archivedAt": null, "settledOverride": null,
        "settledAt": null, "deletedAt": null
    });
    for (key, field) in extra.as_object().into_iter().flatten() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).unwrap()
}

/// The live shell the watcher reads: each step is a new snapshot, as a reconnect would send.
#[derive(Default)]
struct Watched {
    shell: ShellState,
    watcher: ThreadNotifications,
}

impl Watched {
    fn observe(&mut self, threads: &[ThreadShell]) -> Vec<ThreadNotification> {
        let snapshot = ShellSnapshot { threads: threads.to_vec(), ..ShellSnapshot::default() };
        self.shell.apply(ShellStreamItem::Snapshot { snapshot: Box::new(snapshot), resolved_repository_identity_roots: None });
        self.watcher.observe(&self.shell)
    }

    fn update(&mut self, sequence: u64, thread: ThreadShell) -> Vec<ThreadNotification> {
        self.shell.apply(ShellStreamItem::ThreadUpdated { sequence, location: "active".into(), thread: Box::new(thread) });
        self.watcher.observe(&self.shell)
    }
}

fn kinds(notifications: &[ThreadNotification]) -> Vec<(&str, &str)> {
    notifications.iter().map(|n| (n.thread_id.as_str(), n.title)).collect()
}

#[test]
fn sound_modes_recheck_the_current_mode_and_coalesce_playback() {
    for mode in [NotificationMode::Off, NotificationMode::Notifications, NotificationMode::Sound, NotificationMode::NotificationsAndSound] {
        assert_eq!(should_play_notification_sound(mode, false), mode.has_sound());
        assert!(!should_play_notification_sound(mode, true));
    }
}

fn track(pending: &mut PendingNotifications, thread_id: &str) {
    let attempt = pending.begin(ThreadKey::new("env", thread_id));
    assert!(pending.confirm(&attempt, DeliveryOutcome::Accepted));
}

#[test]
fn badge_counts_pending_threads_and_retires_on_visit_focus_or_mode_change() {
    let mut pending = PendingNotifications::default();
    let mode = NotificationMode::NotificationsAndSound;
    assert!(pending.sync(mode, false).is_empty());
    track(&mut pending, "a");
    track(&mut pending, "a");
    track(&mut pending, "b");
    assert_eq!(pending.badge_count(), 2, "replacing a thread's notification does not increment");
    assert!(pending.sync(mode, false).is_empty());
    assert!(pending.visit_target(&ThreadKey::new("env", "unrelated")).is_empty());
    assert_eq!(pending.visit_target(&ThreadKey::new("env", "a")), [notification_tag("env", "a")]);
    assert_eq!(pending.badge_count(), 1);
    assert_eq!(pending.sync(mode, true), [notification_tag("env", "b")]);
    assert_eq!(pending.badge_count(), 0);

    for next_mode in [NotificationMode::Off, NotificationMode::Sound, NotificationMode::Notifications] {
        pending.sync(mode, false);
        track(&mut pending, "a");
        assert_eq!(pending.sync(next_mode, false), [notification_tag("env", "a")]);
        assert_eq!(pending.badge_count(), 0);
        assert!(pending.sync(next_mode, false).is_empty());
    }
    pending.sync(mode, false);
    track(&mut pending, "a");
    assert_eq!(pending.clear(), [notification_tag("env", "a")]);
    assert_eq!(pending.badge_count(), 0);
}

#[test]
fn transitions_notify_and_first_sightings_do_not() {
    let mut watcher = Watched::default();
    let completed = json!({"latestRunCompletedAt": "2026-11-01T00:05:00.000Z"});
    assert!(watcher.observe(&[thread("a", "completed", completed.clone()), thread("b", "running", json!({}))]).is_empty());

    let approval = json!({"pendingRuntimeRequest": {"id": "r1", "kind": "command_approval", "createdAt": "2026-11-01T00:06:00.000Z"}});
    let next = watcher.observe(&[thread("a", "completed", completed.clone()), thread("b", "running", approval)]);
    assert_eq!(kinds(&next), [("b", "Approval needed")]);
    assert_eq!(next[0].body, "Title b");

    let finished = json!({"latestRunCompletedAt": "2026-11-01T00:09:00.000Z"});
    assert_eq!(kinds(&watcher.observe(&[thread("a", "completed", completed.clone()), thread("b", "completed", finished.clone())])), [("b", "Thread completed")]);
    assert!(watcher.observe(&[thread("a", "completed", completed), thread("b", "completed", finished)]).is_empty(), "one completion notifies once");

    // A delta replaces one row; the next observation reads it and keeps the rest.
    let asking = json!({"latestRunId": "run-2", "pendingRuntimeRequest": {"id": "r2", "kind": "command_approval", "createdAt": "2026-11-01T00:10:00.000Z"}});
    assert_eq!(kinds(&watcher.update(1, thread("a", "running", asking))), [("a", "Approval needed")]);
    let renamed = json!({"latestRunCompletedAt": "2026-11-01T00:09:00.000Z", "title": "Renamed"});
    assert!(watcher.update(2, thread("b", "completed", renamed)).is_empty());
}

#[test]
fn archived_stopped_and_nested_threads_stay_quiet() {
    let mut watcher = Watched::default();
    let child = json!({"lineage": {"parentThreadId": "p", "relationshipToParent": "subagent", "rootThreadId": "p"}});
    watcher.observe(&[thread("archived", "running", json!({})), thread("stopped", "running", json!({})), thread("child", "running", child.clone())]);
    let done = |extra: Value| {
        let mut base = json!({"latestRunCompletedAt": "2026-11-01T00:09:00.000Z"});
        for (key, field) in extra.as_object().into_iter().flatten() {
            base[key] = field.clone();
        }
        base
    };
    let next = watcher.observe(&[
        thread("archived", "completed", done(json!({"archivedAt": "2026-11-01T00:09:00.000Z"}))),
        thread("stopped", "completed", done(json!({"latestRunStopRequested": true}))),
        thread("child", "completed", done(child)),
    ]);
    assert!(next.is_empty(), "{next:?}");
    let failed = watcher.observe(&[thread("stopped", "failed", json!({"latestRunCompletedAt": "2026-11-01T00:10:00.000Z", "lastErrorClass": "usage_limit"}))]);
    assert_eq!(kinds(&failed), [("stopped", "Usage limit reached")]);
}

#[test]
fn hidden_notifications_are_consumed_without_replaying_when_shown() {
    let mut watcher = Watched::default();
    watcher.observe(&[thread("a", "running", json!({}))]);
    let hidden = std::collections::HashSet::from(["a".to_owned()]);
    let finished = thread("a", "completed", json!({"latestRunCompletedAt": "2026-11-01T00:09:00.000Z"}));
    watcher.shell.apply(ShellStreamItem::Snapshot { snapshot: Box::new(ShellSnapshot { threads: vec![finished.clone()], ..Default::default() }), resolved_repository_identity_roots: None });
    assert!(watcher.watcher.observe_visible(&watcher.shell, &hidden).is_empty());
    assert!(watcher.observe(&[finished]).is_empty());
    assert_eq!(kinds(&watcher.observe(&[thread("a", "failed", json!({}))])), [("a", "Thread failed")]);
}

#[test]
fn presentation_matches_focus_visibility_settings_and_hidden_threads() {
    for mode in [NotificationMode::Off, NotificationMode::Notifications, NotificationMode::Sound, NotificationMode::NotificationsAndSound] {
        for focused in [false, true] {
            for in_app in [false, true] {
                for selected in [false, true] {
                    let plan = presentation(mode, in_app, focused, selected, false);
                    assert_eq!(plan.sound, mode.has_sound());
                    assert_eq!(plan.toast, focused && in_app && !selected);
                    assert_eq!(plan.desktop, !focused && mode.has_desktop_notifications());
                    assert_eq!(presentation(mode, in_app, focused, selected, true), Presentation::default());
                }
            }
        }
    }
}

#[test]
fn delivery_requires_confirmation_and_retired_callbacks_cannot_restore_badges() {
    let mut pending = PendingNotifications::default();
    let mode = NotificationMode::Notifications;
    pending.sync(mode, false);
    let target = crate::sidebar::tag_filter::ThreadKey::new("one", "thread");
    for outcome in [DeliveryOutcome::Denied, DeliveryOutcome::Unavailable, DeliveryOutcome::Failed, DeliveryOutcome::Unconfirmed] {
        let attempt = pending.begin(target.clone());
        assert_eq!(pending.badge_count(), 0);
        assert!(!pending.confirm(&attempt, outcome));
        assert_eq!(pending.badge_count(), 0, "{outcome:?} never badges");
        assert_eq!(pending.is_outstanding(&attempt.tag), outcome == DeliveryOutcome::Unconfirmed);
    }
    let old = pending.begin(target.clone());
    let new = pending.begin(target.clone());
    assert!(!pending.is_current(&old));
    assert!(pending.is_current(&new));
    assert!(!pending.confirm(&old, DeliveryOutcome::Accepted), "replacement ignores stale callback");
    assert!(pending.confirm(&new, DeliveryOutcome::Accepted));
    assert_eq!(pending.destination(&new.tag), Some(&target));
    assert_eq!(pending.badge_count(), 1);
    let other = crate::sidebar::tag_filter::ThreadKey::new("two", "thread");
    let second = pending.begin(other.clone());
    assert!(pending.confirm(&second, DeliveryOutcome::Accepted));
    assert_eq!(pending.badge_count(), 2, "same thread id in two environments counts twice");
    assert_eq!(pending.visit_target(&target).as_slice(), std::slice::from_ref(&new.tag));
    assert_eq!(pending.badge_count(), 1);
    assert_eq!(pending.destination(&new.tag), Some(&target), "focus may precede the click callback");
    for (next_mode, focused) in [(mode, true), (NotificationMode::Off, false)] {
        pending.sync(mode, false);
        let attempt = pending.begin(target.clone());
        pending.sync(next_mode, focused);
        assert!(!pending.is_current(&attempt), "retired before authorization, so do not post");
        assert!(!pending.confirm(&attempt, DeliveryOutcome::Accepted));
        assert!(!pending.is_outstanding(&attempt.tag));
        assert_eq!(pending.badge_count(), 0);
    }
}

#[test]
fn hiding_retires_os_and_inflight_notifications_including_missing_rows() {
    let mut pending = PendingNotifications::default();
    pending.sync(NotificationMode::Notifications, false);
    let target = crate::sidebar::tag_filter::ThreadKey::new("one", "a");
    let attempt = pending.begin(target.clone());
    pending.confirm(&attempt, DeliveryOutcome::Accepted);
    let missing = pending.begin(crate::sidebar::tag_filter::ThreadKey::new("one", "missing"));
    let visible = pending.begin(crate::sidebar::tag_filter::ThreadKey::new("one", "visible"));
    pending.confirm(&visible, DeliveryOutcome::Accepted);
    let hidden = std::collections::HashSet::from(["a".to_owned()]);
    let live = std::collections::HashSet::from(["a".to_owned(), "visible".to_owned()]);
    assert!(notification_visible("missing", &hidden, None), "missing rows stay pending without filtering");
    let mut retired = pending.retire_where(|key| !notification_visible(&key.thread_id, &hidden, Some(&live)));
    retired.sort();
    let mut expected = vec![attempt.tag.clone(), missing.tag.clone()];
    expected.sort();
    assert_eq!(retired, expected);
    assert_eq!(pending.badge_count(), 1);
    assert!(!pending.is_current(&missing));
    assert!(!pending.confirm(&missing, DeliveryOutcome::Accepted));
    assert_eq!(pending.destination(&attempt.tag), None);
    pending.prune_destinations(|_| false);
    assert_eq!(pending.destination(&visible.tag).unwrap().thread_id, "visible", "unread click remains routable");
    pending.clear();
    pending.prune_destinations(|_| false);
    assert_eq!(pending.destination(&visible.tag), None);
}

#[test]
fn completion_gate_matches_web_once_only_and_skip_cases() {
    let working = Observation::default();
    let completed = |at| Observation { settled: true, completed_at: Some(at), ..Default::default() };
    let attention = Observation { attention: Some("run-1:limited".into()), ..Default::default() };
    let mut gate = NotificationGate::default();
    let key = "thread";
    assert_eq!(gate.observe(&key, None, &completed(1), false, false), None);
    assert_eq!(gate.observe(&key, Some(&working), &completed(2), false, true), None);
    assert_eq!(gate.observe(&key, Some(&working), &completed(2), false, false), None);
    assert_eq!(gate.observe(&key, Some(&working), &completed(3), true, false), None);
    assert_eq!(gate.observe(&key, Some(&working), &completed(3), false, false), None);
    assert_eq!(gate.observe(&key, Some(&working), &completed(4), false, false), Some(NotificationKind::Completion));
    assert_eq!(gate.observe(&key, Some(&working), &attention, false, false), Some(NotificationKind::Input));
    assert_eq!(gate.observe(&key, Some(&attention), &attention, false, false), None);
    assert_eq!(gate.observe(&key, Some(&attention), &completed(4), false, false), None);
    assert_eq!(gate.observe(&key, Some(&working), &completed(5), false, false), Some(NotificationKind::Completion));
    assert_eq!(gate.observe(&key, Some(&working), &completed(4), false, false), None);
    gate.retain(|_| false);
    assert_eq!(gate.observe(&key, Some(&working), &completed(1), false, false), Some(NotificationKind::Completion));
    gate.clear();
    assert_eq!(gate.observe(&key, Some(&working), &completed(1), false, false), Some(NotificationKind::Completion));
}

#[test]
fn background_environments_keep_independent_gates_pending_and_click_destinations() {
    let mut watched = EnvironmentNotifications::default();
    let mut pending = PendingNotifications::default();
    pending.sync(NotificationMode::Notifications, false);
    let mut shell = ShellState::default();
    let update = |shell: &mut ShellState, status, extra| {
        shell.apply(ShellStreamItem::Snapshot { snapshot: Box::new(ShellSnapshot {
            threads: vec![thread("same-thread-id", status, extra)], ..Default::default()
        }), resolved_repository_identity_roots: None });
    };
    update(&mut shell, "running", json!({}));
    for env in ["active", "background"] {
        assert!(watched.observe(env, &shell, &HashSet::new()).is_empty());
    }
    update(&mut shell, "completed", json!({"latestRunCompletedAt":"2026-11-01T00:09:00.000Z"}));
    for env in ["background", "active"] {
        assert_eq!(kinds(&watched.observe(env, &shell, &HashSet::new())), [("same-thread-id", "Thread completed")]);
        let attempt = pending.begin(ThreadKey::new(env, "same-thread-id"));
        assert!(pending.confirm(&attempt, DeliveryOutcome::Accepted));
    }
    assert_eq!(pending.badge_count(), 2);
    // Switching views and a reconnect replay don't create another notification.
    assert!(watched.observe("background", &shell, &HashSet::new()).is_empty());
    assert_eq!(pending.badge_count(), 2);
    let tag = notification_tag("background", "same-thread-id");
    assert_eq!(pending.destination(&tag), Some(&ThreadKey::new("background", "same-thread-id")));
    let in_flight = pending.begin(ThreadKey::new("active", "later"));
    watched.remove("active");
    pending.retire_where(|key| key.environment_id == "active");
    assert!(!pending.confirm(&in_flight, DeliveryOutcome::Accepted));
    assert_eq!(pending.badge_count(), 1);
    assert!(pending.destination(&notification_tag("active", "same-thread-id")).is_none());
    assert_eq!(pending.destination(&tag), Some(&ThreadKey::new("background", "same-thread-id")));
    assert!(watched.observe("background", &shell, &HashSet::new()).is_empty());
    // Re-enabling a removed environment establishes a fresh baseline.
    assert!(watched.observe("active", &shell, &HashSet::new()).is_empty());
}

#[test]
fn notification_click_switches_environment_before_opening_the_thread_or_family() {
    let target = ThreadKey::new("background", "thread");
    assert_eq!(notification_route(&target, Some("active"), true, true), NotificationRoute::SwitchEnvironment);
    assert_eq!(notification_route(&target, Some("background"), false, true), NotificationRoute::WaitForShell);
    assert_eq!(notification_route(&target, Some("background"), true, true), NotificationRoute::OpenThread);
    assert_eq!(notification_route(&target, Some("background"), true, false), NotificationRoute::ReadFamily);
}

#[test]
fn transient_reconnect_keeps_pending_badges_and_click_destinations() {
    let mut pending = PendingNotifications::default();
    let mode = NotificationMode::Notifications;
    pending.sync(mode, false);
    for environment in ["a", "b"] {
        let attempt = pending.begin(ThreadKey::new(environment, "same"));
        assert!(pending.confirm(&attempt, DeliveryOutcome::Accepted));
    }
    let in_flight = pending.begin(ThreadKey::new("a", "next"));
    assert!(pending.sync(mode, false).is_empty(), "a transport reconnect is not environment removal");
    assert_eq!(pending.badge_count(), 2);
    assert_eq!(pending.destination(&notification_tag("a", "same")), Some(&ThreadKey::new("a", "same")));
    assert!(pending.confirm(&in_flight, DeliveryOutcome::Accepted));
    assert_eq!(pending.badge_count(), 3);
    pending.retire_where(|target| target.environment_id == "a");
    assert_eq!(pending.badge_count(), 1);
    assert!(pending.destination(&notification_tag("a", "same")).is_none());
    assert_eq!(pending.destination(&notification_tag("b", "same")), Some(&ThreadKey::new("b", "same")));
}
