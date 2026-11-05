//! The open thread's visit watermark (`client-runtime/src/state/threadVisits.ts`): showing a
//! thread records one open, and later shell updates advance `lastVisitedAt`, throttled while a
//! run streams, so the sidebar's Done and Woke markers clear for what the user has seen.

use super::time::Ms;

/// Every visit is a command plus a broadcast to all shell subscribers.
pub const VISIT_DISPATCH_THROTTLE_MS: Ms = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisitedShell {
    pub updated_at: Ms,
    pub last_visited_at: Option<Ms>,
    pub completed_at: Option<Ms>,
    pub woke_at: Option<Ms>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitDecision {
    Skip,
    Now { visited_at: Ms, opened: bool },
    At { due: Ms, visited_at: Ms },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisitTracker {
    thread_id: String,
    open_pending: bool,
    last_dispatched: Option<Ms>,
    last_dispatch_at: Ms,
}

impl VisitTracker {
    pub fn new(thread_id: &str) -> VisitTracker {
        VisitTracker { thread_id: thread_id.to_owned(), open_pending: true, last_dispatched: None, last_dispatch_at: Ms::MIN / 2 }
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn on_shell(&mut self, shell: VisitedShell, now: Ms) -> VisitDecision {
        let wake_is_latest = shell.woke_at.is_some_and(|woke| woke > shell.updated_at);
        let visited_at = if wake_is_latest { shell.woke_at.unwrap_or(shell.updated_at) } else { shell.updated_at };
        if self.open_pending {
            self.open_pending = false;
            return self.dispatched(visited_at, now, true);
        }
        if shell.last_visited_at.is_some_and(|visited| visited >= visited_at) {
            if self.last_dispatched.is_none_or(|sent| sent < visited_at) {
                self.last_dispatched = Some(visited_at);
            }
            return VisitDecision::Skip;
        }
        if self.last_dispatched.is_some_and(|sent| sent >= visited_at) {
            return VisitDecision::Skip;
        }
        let unseen_completion = shell.completed_at.is_some_and(|completed| shell.last_visited_at.is_none_or(|visited| completed > visited));
        let elapsed = now - self.last_dispatch_at;
        if unseen_completion || wake_is_latest || elapsed >= VISIT_DISPATCH_THROTTLE_MS {
            return self.dispatched(visited_at, now, false);
        }
        VisitDecision::At { due: self.last_dispatch_at + VISIT_DISPATCH_THROTTLE_MS, visited_at }
    }

    /// Call when a scheduled `At` decision fires.
    pub fn fire(&mut self, visited_at: Ms, now: Ms) -> VisitDecision {
        if self.last_dispatched.is_some_and(|sent| sent >= visited_at) {
            return VisitDecision::Skip;
        }
        self.dispatched(visited_at, now, false)
    }

    fn dispatched(&mut self, visited_at: Ms, now: Ms, opened: bool) -> VisitDecision {
        self.last_dispatched = Some(visited_at);
        self.last_dispatch_at = now;
        VisitDecision::Now { visited_at, opened }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(updated_at: Ms, last_visited_at: Option<Ms>) -> VisitedShell {
        VisitedShell { updated_at, last_visited_at, completed_at: None, woke_at: None }
    }

    #[test]
    fn the_first_shell_records_an_open_then_updates_throttle() {
        let mut tracker = VisitTracker::new("t");
        assert_eq!(tracker.on_shell(shell(100, Some(50)), 1_000), VisitDecision::Now { visited_at: 100, opened: true });
        assert_eq!(tracker.on_shell(shell(100, Some(100)), 1_100), VisitDecision::Skip);
        assert_eq!(tracker.on_shell(shell(200, Some(100)), 1_200), VisitDecision::At { due: 11_000, visited_at: 200 });
        assert_eq!(tracker.fire(200, 11_000), VisitDecision::Now { visited_at: 200, opened: false });
        assert_eq!(tracker.on_shell(shell(300, Some(200)), 30_000), VisitDecision::Now { visited_at: 300, opened: false });

        for seen in [shell(20, Some(20)), shell(20, Some(30)), VisitedShell { woke_at: Some(20), ..shell(16, Some(20)) }] {
            let mut tracker = VisitTracker::new("t");
            assert_eq!(tracker.on_shell(shell(10, Some(0)), 1_000), VisitDecision::Now { visited_at: 10, opened: true });
            assert_eq!(tracker.on_shell(shell(15, Some(10)), 1_100), VisitDecision::At { due: 11_000, visited_at: 15 });
            assert_eq!(tracker.on_shell(seen, 2_000), VisitDecision::Skip);
            let unread = VisitedShell { completed_at: Some(seen.updated_at - 1), ..shell(seen.updated_at, Some(seen.updated_at - 2)) };
            let mut older = tracker.clone();
            let decisions = [tracker.on_shell(unread, 2_001), older.on_shell(shell(5, Some(5)), 2_002), older.on_shell(unread, 2_003)];
            assert_eq!(decisions, [VisitDecision::Skip; 3], "mark unread stays sticky, including after an older snapshot: {seen:?}");
            assert_eq!(tracker.on_shell(shell(21, Some(20)), 2_004), VisitDecision::At { due: 11_000, visited_at: 21 });
            assert_eq!(tracker.fire(15, 11_000), VisitDecision::Skip);
            assert_eq!(tracker.fire(20, 11_000), VisitDecision::Skip);
            assert_eq!(tracker.on_shell(shell(21, Some(20)), 11_000), VisitDecision::Now { visited_at: 21, opened: false });
            assert_eq!(tracker.fire(21, 11_000), VisitDecision::Skip);
            assert_eq!(tracker.on_shell(shell(22, Some(21)), 11_001), VisitDecision::At { due: 21_000, visited_at: 22 });
            assert_eq!(tracker.fire(22, 21_000), VisitDecision::Now { visited_at: 22, opened: false });
        }
    }

    #[test]
    fn an_unseen_completion_or_wake_dispatches_at_once() {
        let mut tracker = VisitTracker::new("t");
        tracker.on_shell(shell(100, Some(100)), 1_000);
        let completed = VisitedShell { completed_at: Some(150), ..shell(160, Some(100)) };
        assert_eq!(tracker.on_shell(completed, 1_001), VisitDecision::Now { visited_at: 160, opened: false });
        let woke = VisitedShell { woke_at: Some(500), ..shell(160, Some(160)) };
        assert_eq!(tracker.on_shell(woke, 1_002), VisitDecision::Now { visited_at: 500, opened: false });
    }
}
