//! Thread notifications: `createThreadNotificationGate` from
//! `apps/web/src/components/ThreadNotificationCoordinator.logic.ts`, the per-thread observation
//! and pending badge set of `ThreadNotificationCoordinator.tsx`, and the sound gate and tag of
//! `apps/web/src/threadNotifications.ts`. The app presents what this decides.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use serde::Deserialize;

use crate::shell::{ShellState, ThreadShell};
use crate::sidebar::tag_filter::ThreadKey;
use crate::sidebar::status::{ThreadStatus, resolve_status};
use crate::sidebar::thread::{SubagentRequest, pending_subagent_requests, present_thread};
use crate::sidebar::time::Ms;

/// `ClientSettings.notificationMode`; off by default, as on the web.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationMode {
    #[default]
    Off,
    Notifications,
    Sound,
    NotificationsAndSound,
}

impl NotificationMode {
    /// `NOTIFICATION_MODE_LABELS`.
    pub fn label(self) -> &'static str {
        match self {
            NotificationMode::Off => "Off",
            NotificationMode::Notifications => "Notifications only",
            NotificationMode::Sound => "Sound only",
            NotificationMode::NotificationsAndSound => "Notifications with sound",
        }
    }

    pub fn has_sound(self) -> bool {
        matches!(self, NotificationMode::Sound | NotificationMode::NotificationsAndSound)
    }

    pub fn has_desktop_notifications(self) -> bool {
        matches!(self, NotificationMode::Notifications | NotificationMode::NotificationsAndSound)
    }
}

/// Rechecked after the sound loads; each sound has its own playback slot.
pub fn should_play_notification_sound(mode: NotificationMode, is_playing: bool) -> bool {
    mode.has_sound() && !is_playing
}

/// Permission and request acceptance, not a guarantee that the OS displayed a banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Accepted,
    Denied,
    Unavailable,
    Failed,
    /// A platform posting API without a delivery result must not claim a delivered badge.
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryAttempt {
    pub tag: String,
    sequence: u64,
}

/// The coordinator's delivered OS notifications and requests awaiting delivery confirmation.
#[derive(Debug, Default)]
pub struct PendingNotifications {
    pending: HashMap<String, ThreadKey>,
    attempts: HashMap<String, (u64, ThreadKey)>,
    // Focus can arrive before the click callback; keep destinations while the thread is loaded.
    destinations: HashMap<String, ThreadKey>,
    sequence: u64,
    applied_mode: Option<NotificationMode>,
}

impl PendingNotifications {
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.attempts.is_empty() && self.destinations.is_empty()
    }

    pub fn badge_count(&self) -> usize {
        self.pending.len()
    }

    pub fn begin(&mut self, target: ThreadKey) -> DeliveryAttempt {
        self.sequence += 1;
        let tag = notification_tag(&target.environment_id, &target.thread_id);
        self.destinations.insert(tag.clone(), target.clone());
        self.attempts.insert(tag.clone(), (self.sequence, target));
        DeliveryAttempt { tag, sequence: self.sequence }
    }

    pub fn is_current(&self, attempt: &DeliveryAttempt) -> bool {
        self.attempts.get(&attempt.tag).is_some_and(|(sequence, _)| *sequence == attempt.sequence)
    }

    pub fn confirm(&mut self, attempt: &DeliveryAttempt, outcome: DeliveryOutcome) -> bool {
        if !self.is_current(attempt) {
            return false;
        }
        if outcome == DeliveryOutcome::Unconfirmed { return false; }
        let (_, target) = self.attempts.remove(&attempt.tag).unwrap();
        let delivered = outcome == DeliveryOutcome::Accepted;
        if delivered {
            self.pending.insert(attempt.tag.clone(), target);
        }
        delivered
    }

    pub fn is_outstanding(&self, tag: &str) -> bool {
        self.pending.contains_key(tag) || self.attempts.contains_key(tag)
    }

    pub fn destination(&self, tag: &str) -> Option<&ThreadKey> {
        self.destinations.get(tag)
    }

    pub fn prune_destinations(&mut self, loaded: impl Fn(&ThreadKey) -> bool) {
        self.destinations.retain(|tag, target| loaded(target) || self.pending.contains_key(tag) || self.attempts.contains_key(tag));
    }

    /// Mode changes and focus retire pending; transport reconnects leave this set untouched.
    pub fn sync(&mut self, mode: NotificationMode, focused: bool) -> Vec<String> {
        let changed = self.applied_mode != Some(mode);
        self.applied_mode = Some(mode);
        if changed || focused || !mode.has_desktop_notifications() {
            self.clear()
        } else {
            Vec::new()
        }
    }

    pub fn visit_target(&mut self, target: &ThreadKey) -> Vec<String> {
        self.retire_pending(|key| key == target)
    }

    fn retire_pending(&mut self, retire: impl Fn(&ThreadKey) -> bool) -> Vec<String> {
        let mut retired = HashSet::new();
        self.pending.retain(|tag, target| if retire(target) { retired.insert(tag.clone()); false } else { true });
        self.attempts.retain(|tag, (_, target)| if retire(target) { retired.insert(tag.clone()); false } else { true });
        retired.into_iter().collect()
    }

    /// Hiding a thread closes existing notifications and invalidates delivery still in flight.
    pub fn retire_where(&mut self, retire: impl Fn(&ThreadKey) -> bool) -> Vec<String> {
        let mut retired: HashSet<_> = self.retire_pending(&retire).into_iter().collect();
        self.destinations.retain(|tag, target| if retire(target) { retired.insert(tag.clone()); false } else { true });
        retired.into_iter().collect()
    }

    pub fn clear(&mut self) -> Vec<String> {
        self.retire_pending(|_| true)
    }

}

/// `closeHiddenPending`: with filtering active, a missing row cannot be judged and closes too.
pub fn notification_visible(thread_id: &str, hidden: &HashSet<String>, live_when_filtering: Option<&HashSet<String>>) -> bool {
    !hidden.contains(thread_id) && live_when_filtering.is_none_or(|live| live.contains(thread_id))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Presentation {
    pub sound: bool,
    pub toast: bool,
    pub desktop: bool,
}

pub fn presentation(mode: NotificationMode, in_app: bool, focused_and_visible: bool, selected: bool, hidden: bool) -> Presentation {
    if hidden { return Presentation::default(); }
    Presentation {
        sound: mode.has_sound(),
        toast: in_app && focused_and_visible && !selected,
        desktop: mode.has_desktop_notifications() && !focused_and_visible,
    }
}

/// `threadNotificationTag`: FNV-1a over the UTF-16 units of `environment:thread`, so tags stay
/// short enough for every platform's notification center.
pub fn notification_tag(environment_id: &str, thread_id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for unit in format!("{environment_id}:{thread_id}").encode_utf16() {
        hash ^= u64::from(unit);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    Input,
    Completion,
}

/// `ThreadNotificationObservation`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Observation {
    /// Run and status while the thread needs attention.
    pub attention: Option<String>,
    /// Idle, with its latest run completed.
    pub settled: bool,
    pub completed_at: Option<Ms>,
}

/// `createThreadNotificationGate`: attention changes notify input; a newer completion notifies
/// completion once. First sightings, archived threads and stopped runs never notify completion.
#[derive(Debug, Clone)]
pub struct NotificationGate<K> {
    handled: HashMap<K, Ms>,
}

impl<K> Default for NotificationGate<K> {
    fn default() -> Self {
        NotificationGate { handled: HashMap::new() }
    }
}

impl<K: Eq + Hash + Clone> NotificationGate<K> {
    fn skip(&mut self, key: &K, at: Option<Ms>) {
        if let Some(at) = at
            && self.handled.get(key).is_none_or(|handled| at > *handled)
        {
            self.handled.insert(key.clone(), at);
        }
    }

    pub fn observe(&mut self, key: &K, prior: Option<&Observation>, next: &Observation, archived: bool, quiet: bool) -> Option<NotificationKind> {
        if prior.is_none() || archived || quiet {
            self.skip(key, next.completed_at);
        }
        let prior = prior.filter(|_| !archived)?;
        if next.attention.is_some() && next.attention != prior.attention {
            return Some(NotificationKind::Input);
        }
        let at = next.completed_at.filter(|_| next.settled)?;
        if self.handled.get(key).is_some_and(|handled| at <= *handled) {
            return None;
        }
        self.handled.insert(key.clone(), at);
        Some(NotificationKind::Completion)
    }

    /// Forgets threads that left the snapshot.
    pub fn retain(&mut self, keep: impl Fn(&K) -> bool) {
        self.handled.retain(|key, _| keep(key));
    }

    pub fn clear(&mut self) {
        self.handled.clear();
    }
}

/// How the app should present one notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadNotification {
    pub thread_id: String,
    pub kind: NotificationKind,
    pub status: ThreadStatus,
    pub title: &'static str,
    /// The thread's title, the notification body.
    pub body: String,
}

/// The headline `present` picks.
pub fn notification_title(kind: NotificationKind, status: ThreadStatus) -> &'static str {
    match (kind, status) {
        (NotificationKind::Completion, _) => "Thread completed",
        (_, ThreadStatus::Approval) => "Approval needed",
        (_, ThreadStatus::Limited) => "Usage limit reached",
        (_, ThreadStatus::Failed) => "Thread failed",
        (_, ThreadStatus::Request) => "Guest request",
        _ => "Input needed",
    }
}

#[derive(Debug, Clone)]
struct Observed {
    /// The shell row's stamp when observed; the web compares row identity instead.
    revision: Option<u64>,
    request: Option<SubagentRequest>,
    observation: Observation,
    status: ThreadStatus,
}

fn observe_row(raw: &ThreadShell, request: Option<SubagentRequest>, revision: Option<u64>) -> Observed {
    let thread = present_thread(raw, request);
    let run = thread.latest_run.as_ref();
    let mut status = resolve_status(&thread);
    if status == ThreadStatus::Ready && run.is_some_and(|run| run.status == "failed") {
        status = ThreadStatus::Failed;
    }
    let attention = matches!(
        status,
        ThreadStatus::Input | ThreadStatus::Request | ThreadStatus::Approval | ThreadStatus::Failed | ThreadStatus::Limited
    )
    .then(|| format!("{}:{status:?}", run.map_or("", |run| run.run_id.as_str())));
    let settled = status == ThreadStatus::Ready && run.is_some_and(|run| run.status == "completed");
    let completed_at = run.and_then(|run| run.completed_at).filter(|_| settled);
    Observed { revision, request, observation: Observation { attention, settled, completed_at }, status }
}

/// `observeThreads` for one environment: turns status transitions in the live shell into
/// notifications. Rows shown under a parent (subagents, side chats) never notify on their own.
#[derive(Debug, Clone, Default)]
pub struct ThreadNotifications {
    previous: HashMap<String, Observed>,
    gate: NotificationGate<String>,
}

impl ThreadNotifications {
    /// Hidden transitions still reach the gate, so revealing a thread never replays them.
    pub fn observe_visible(&mut self, shell: &ShellState, hidden: &HashSet<String>) -> Vec<ThreadNotification> {
        self.observe(shell).into_iter().filter(|notification| !hidden.contains(&notification.thread_id)).collect()
    }

    /// Rows the shell has not replaced since the last call keep their observation untouched.
    pub fn observe(&mut self, shell: &ShellState) -> Vec<ThreadNotification> {
        let Some(snapshot) = &shell.snapshot else { return Vec::new() };
        let threads = &snapshot.threads;
        let requests = pending_subagent_requests(threads);
        let mut previous = std::mem::take(&mut self.previous);
        let mut changed: Vec<(&ThreadShell, Option<Observation>)> = Vec::new();
        for raw in threads.iter().filter(|thread| thread.lineage.is_listed()) {
            let request = requests.get(&raw.id).copied();
            let revision = shell.revision(&raw.id);
            let (key, prior) = match previous.remove_entry(raw.id.as_str()) {
                Some((key, prior)) if revision.is_some() && prior.revision == revision && prior.request == request => {
                    self.previous.insert(key, prior);
                    continue;
                }
                Some((key, prior)) => (key, Some(prior.observation)),
                None => (raw.id.clone(), None),
            };
            self.previous.insert(key, observe_row(raw, request, revision));
            changed.push((raw, prior));
        }
        if !previous.is_empty() {
            let current = &self.previous;
            self.gate.retain(|key| current.contains_key(key));
        }
        let mut notifications = Vec::new();
        for (raw, prior) in changed {
            let Some(state) = self.previous.get(&raw.id) else { continue };
            let archived = raw.archived_at.is_some();
            let quiet = raw.latest_run_stop_requested == Some(true);
            if let Some(kind) = self.gate.observe(&raw.id, prior.as_ref(), &state.observation, archived, quiet) {
                notifications.push(ThreadNotification {
                    thread_id: raw.id.clone(),
                    kind,
                    status: state.status,
                    title: notification_title(kind, state.status),
                    body: raw.title.clone(),
                });
            }
        }
        notifications
    }

    /// Notifications turned off: nothing is watched, and turning them on starts fresh.
    pub fn clear(&mut self) {
        self.previous.clear();
        self.gate.clear();
    }
}

#[cfg(test)]
mod tests;

/// The coordinator's `watched` map. Selection never changes these gates.
#[derive(Debug, Default)]
pub struct EnvironmentNotifications {
    watched: HashMap<String, ThreadNotifications>,
}

impl EnvironmentNotifications {
    pub fn observe(&mut self, environment: &str, shell: &ShellState, hidden: &HashSet<String>) -> Vec<ThreadNotification> {
        self.watched.entry(environment.to_owned()).or_default().observe_visible(shell, hidden)
    }

    pub fn remove(&mut self, environment: &str) { self.watched.remove(environment); }
    pub fn clear(&mut self) { self.watched.clear(); }
}

/// Small notification-only projection sent to the UI when visibility changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationVisibility {
    pub hidden: HashSet<ThreadKey>,
    pub live: HashSet<ThreadKey>,
    pub filtering: bool,
}

impl NotificationVisibility {
    pub fn visible(&self, target: &ThreadKey) -> bool {
        !self.hidden.contains(target) && (!self.filtering || self.live.contains(target))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationRoute {
    SwitchEnvironment,
    WaitForShell,
    OpenThread,
    ReadFamily,
}

/// Web notification actions route by both ids; omitted threads load through their family.
pub fn notification_route(target: &ThreadKey, active_environment: Option<&str>, live: bool, loaded: bool) -> NotificationRoute {
    if active_environment != Some(&target.environment_id) { NotificationRoute::SwitchEnvironment }
    else if !live { NotificationRoute::WaitForShell }
    else if loaded { NotificationRoute::OpenThread }
    else { NotificationRoute::ReadFamily }
}
