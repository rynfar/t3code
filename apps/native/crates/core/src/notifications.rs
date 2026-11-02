//! Thread notifications: `createThreadNotificationGate` from
//! `apps/web/src/components/ThreadNotificationCoordinator.logic.ts`, the per-thread observation
//! and titles of `ThreadNotificationCoordinator.tsx`, and the mode helpers and tag of
//! `apps/web/src/threadNotifications.ts`. The app presents what this decides.

use std::collections::HashMap;
use std::hash::Hash;

use serde::Deserialize;

use crate::shell::{ShellState, ThreadShell};
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
