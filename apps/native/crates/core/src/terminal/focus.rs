//! Pending focus queue and per-thread focus restoration for drawer and right-panel terminals.
//! Ports `apps/web/src/lib/terminalFocus.ts`: only one request waits at a time so an older open
//! never steals focus from a newer one, requests wait until the target viewport is visible and
//! the client holds the operate grant, and thread navigation remembers whether focus belonged to
//! the composer or a specific terminal.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFocus {
    pub thread_id: String,
    pub terminal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastThreadFocus {
    Composer,
    Terminal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadEnterFocusDecision {
    /// A navigation handler explicitly asked to focus a terminal in this thread; keep its pending request.
    KeepPending,
    /// Focus this terminal (and queue it as pending until its pane is ready).
    FocusTerminal(String),
    /// Focus the thread's composer.
    FocusComposer,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalFocusQueue {
    pending: Option<PendingFocus>,
    last_focus: HashMap<String, LastThreadFocus>,
    entered_thread: Option<String>,
    navigation_request_thread: Option<String>,
}

impl TerminalFocusQueue {
    pub fn pending(&self) -> Option<&PendingFocus> {
        self.pending.as_ref()
    }

    pub fn is_pending(&self, thread_id: &str, terminal_id: &str) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|req| req.thread_id == thread_id && req.terminal_id == terminal_id)
    }

    /// Queues a focus request for `(thread_id, terminal_id)`. Replaces any older waiting request.
    pub fn request(&mut self, thread_id: &str, terminal_id: &str) {
        self.pending = Some(PendingFocus {
            thread_id: thread_id.to_owned(),
            terminal_id: terminal_id.to_owned(),
        });
    }

    /// `focusTerminalOnNavigation`: marks a cross-thread navigation request so `enter_thread`
    /// keeps the pending terminal focus instead of falling back to the composer or drawer.
    pub fn request_on_navigation(&mut self, thread_id: &str, terminal_id: &str) {
        self.navigation_request_thread =
            (self.entered_thread.as_deref() != Some(thread_id)).then(|| thread_id.to_owned());
        self.request(thread_id, terminal_id);
    }

    /// `cancelTerminalFocus`: drops a waiting request when its thread's drawer/panel closes.
    pub fn cancel(&mut self, thread_id: &str) {
        if self.pending.as_ref().is_some_and(|req| req.thread_id == thread_id) {
            self.pending = None;
        }
    }

    /// `consumePendingTerminalFocus` + `focusTerminalViewport` readiness gate: consumes the
    /// waiting request only when the target pane matches, is visible, and has operate access.
    pub fn consume_if_ready(
        &mut self,
        thread_id: &str,
        terminal_id: &str,
        visible: bool,
        can_operate: bool,
    ) -> bool {
        if !visible || !can_operate || !self.is_pending(thread_id, terminal_id) {
            return false;
        }
        self.pending = None;
        self.last_focus.insert(thread_id.to_owned(), LastThreadFocus::Terminal(terminal_id.to_owned()));
        true
    }

    pub fn record_terminal_focus(&mut self, thread_id: &str, terminal_id: &str) {
        self.last_focus.insert(thread_id.to_owned(), LastThreadFocus::Terminal(terminal_id.to_owned()));
        if self.is_pending(thread_id, terminal_id) {
            self.pending = None;
        }
    }

    pub fn record_composer_focus(&mut self, thread_id: &str) {
        self.last_focus.insert(thread_id.to_owned(), LastThreadFocus::Composer);
        self.cancel(thread_id);
    }

    pub fn last_focus(&self, thread_id: &str) -> Option<&LastThreadFocus> {
        self.last_focus.get(thread_id)
    }

    /// `restoreFocusOnThreadEnter`: decides whether entering `thread_id` should preserve an
    /// explicit navigation request, restore a visible remembered terminal, focus the active open
    /// drawer terminal, or return focus to the composer.
    pub fn enter_thread(
        &mut self,
        thread_id: &str,
        drawer_open: bool,
        active_drawer_group_ids: &[String],
        active_drawer_terminal_id: Option<&str>,
        panel_terminal_ids: Option<&[String]>,
    ) -> ThreadEnterFocusDecision {
        let requested_by_handler = self.navigation_request_thread.as_deref() == Some(thread_id)
            || (self.entered_thread.as_deref() == Some(thread_id)
                && self.pending.as_ref().is_some_and(|req| req.thread_id == thread_id));
        if !requested_by_handler {
            self.pending = None;
        }
        self.navigation_request_thread = None;
        self.entered_thread = Some(thread_id.to_owned());
        if requested_by_handler {
            return ThreadEnterFocusDecision::KeepPending;
        }
        match self.last_focus.get(thread_id).cloned() {
            Some(LastThreadFocus::Composer) => ThreadEnterFocusDecision::FocusComposer,
            Some(LastThreadFocus::Terminal(terminal_id))
                if (drawer_open && active_drawer_group_ids.iter().any(|id| id == &terminal_id))
                    || panel_terminal_ids.is_some_and(|ids| ids.iter().any(|id| id == &terminal_id)) =>
            {
                self.request(thread_id, &terminal_id);
                ThreadEnterFocusDecision::FocusTerminal(terminal_id)
            }
            _ => {
                if let Some(active) = active_drawer_terminal_id.filter(|_| drawer_open) {
                    self.request(thread_id, active);
                    ThreadEnterFocusDecision::FocusTerminal(active.to_owned())
                } else {
                    ThreadEnterFocusDecision::FocusComposer
                }
            }
        }
    }

    pub fn leave_thread(&mut self, thread_id: &str) {
        if self.entered_thread.as_deref() == Some(thread_id) {
            self.entered_thread = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_focus_request_wins_and_waits_for_visibility_and_grant() {
        let mut queue = TerminalFocusQueue::default();
        queue.request("t1", "term-1");
        queue.request("t1", "term-2");
        assert!(!queue.is_pending("t1", "term-1"));
        assert!(queue.is_pending("t1", "term-2"));
        // Hidden or read-only panes do not spend the request.
        assert!(!queue.consume_if_ready("t1", "term-2", false, true));
        assert!(!queue.consume_if_ready("t1", "term-2", true, false));
        assert!(queue.is_pending("t1", "term-2"));
        // Once visible and operable, the request is consumed once.
        assert!(queue.consume_if_ready("t1", "term-2", true, true));
        assert_eq!(queue.pending(), None);
        assert!(!queue.consume_if_ready("t1", "term-2", true, true));
        assert_eq!(queue.last_focus("t1"), Some(&LastThreadFocus::Terminal("term-2".into())));
    }

    #[test]
    fn cancel_drops_only_the_matching_threads_pending_request() {
        let mut queue = TerminalFocusQueue::default();
        queue.request("t1", "term-1");
        queue.cancel("t2");
        assert!(queue.is_pending("t1", "term-1"));
        queue.cancel("t1");
        assert_eq!(queue.pending(), None);
    }

    #[test]
    fn thread_enter_honors_navigation_request_and_restores_last_focus() {
        let mut queue = TerminalFocusQueue::default();
        queue.enter_thread("t1", false, &[], None, None);
        // Cross-thread navigation request survives entering t2.
        queue.request_on_navigation("t2", "term-9");
        assert_eq!(
            queue.enter_thread("t2", false, &[], None, Some(&["term-9".into()])),
            ThreadEnterFocusDecision::KeepPending
        );
        assert!(queue.is_pending("t2", "term-9"));
        assert!(queue.consume_if_ready("t2", "term-9", true, true));

        // Returning to t2 with term-9 still visible in the right panel restores it.
        assert_eq!(
            queue.enter_thread("t1", false, &[], None, None),
            ThreadEnterFocusDecision::FocusComposer
        );
        assert_eq!(
            queue.enter_thread("t2", false, &[], None, Some(&["term-9".into()])),
            ThreadEnterFocusDecision::FocusTerminal("term-9".into())
        );

        // If composer was last focused, open drawer does not steal focus back.
        queue.record_composer_focus("t2");
        assert_eq!(
            queue.enter_thread("t2", true, &["term-1".into()], Some("term-1"), None),
            ThreadEnterFocusDecision::FocusComposer
        );

        // Fresh thread with open drawer focuses active drawer terminal.
        assert_eq!(
            queue.enter_thread("t3", true, &["term-1".into()], Some("term-1"), None),
            ThreadEnterFocusDecision::FocusTerminal("term-1".into())
        );
    }
}
