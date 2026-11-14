use std::collections::HashMap;

/// Session-only per-scope scroll restoration state (`useCodeViewScrollRestore.ts`).
#[derive(Clone, Debug, Default)]
pub struct ScrollRestore {
    current_scope: Option<String>,
    saved: HashMap<String, f32>,
    pending: Option<PendingRestore>,
}

#[derive(Clone, Debug, PartialEq)]
struct PendingRestore {
    scope_key: String,
    target: f32,
    last_applied: Option<f32>,
}

/// Outcome of evaluating a pending scroll restoration against the current rendered list metrics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollRestoreAction {
    None,
    /// Scroll vertically to `offset_px` (clamped to `[0, max_scroll_top]`).
    /// When `done` is false, lazy patches are still loading and `target > max_scroll_top`.
    ScrollTo { offset_px: f32, done: bool },
    /// Waiting for more lazy patches before `target` is reachable (current list cannot advance yet).
    WaitMore,
}

impl ScrollRestore {
    pub fn current_scope(&self) -> Option<&str> {
        self.current_scope.as_deref()
    }

    pub fn saved_offset(&self, scope_key: &str) -> Option<f32> {
        self.saved.get(scope_key).copied()
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Transitions to `scope_key`, staging any saved offset unless an explicit reveal owns focus.
    /// Returns `true` when `scope_key` changed from the active scope.
    pub fn sync_scope(&mut self, scope_key: &str, has_pending_reveal: bool) -> bool {
        if self.current_scope.as_deref() == Some(scope_key) {
            if has_pending_reveal {
                self.discard(scope_key);
            }
            return false;
        }
        self.current_scope = Some(scope_key.to_owned());
        if has_pending_reveal {
            self.pending = None;
            self.saved.remove(scope_key);
        } else {
            self.pending = self
                .saved
                .get(scope_key)
                .copied()
                .filter(|top| *top > 0.0)
                .map(|target| PendingRestore {
                    scope_key: scope_key.to_owned(),
                    target,
                    last_applied: None,
                });
        }
        true
    }

    /// Records the user's current scroll position for `scope_key` unless a restore is still pending.
    pub fn on_scroll(&mut self, scope_key: &str, scroll_top_px: f32) {
        if self.pending.as_ref().is_some_and(|pending| pending.scope_key == scope_key) {
            return;
        }
        let clamped = scroll_top_px.max(0.0);
        if clamped > 0.0 {
            self.saved.insert(scope_key.to_owned(), clamped);
        } else {
            self.saved.remove(scope_key);
        }
    }

    /// Discards pending and saved scroll state for `scope_key` (e.g. explicit file reveal or fold-all).
    pub fn discard(&mut self, scope_key: &str) {
        if self.pending.as_ref().is_some_and(|pending| pending.scope_key == scope_key) {
            self.pending = None;
        }
        self.saved.remove(scope_key);
    }

    /// Cancels an active pending restore when the user explicitly interacts with the list.
    pub fn cancel_pending(&mut self) -> bool {
        self.pending.take().is_some()
    }

    /// Applies the pending scroll restoration for `scope_key` once content can satisfy `target`
    /// or `settled` indicates all lazy patches for the scope have finished loading.
    pub fn apply(
        &mut self,
        scope_key: &str,
        max_scroll_top_px: f32,
        settled: bool,
    ) -> ScrollRestoreAction {
        let Some(pending) = self.pending.as_mut() else {
            return ScrollRestoreAction::None;
        };
        if pending.scope_key != scope_key {
            return ScrollRestoreAction::None;
        }

        let max_top = max_scroll_top_px.max(0.0);
        let reached_target = max_top >= pending.target;
        let done = settled || reached_target;
        let next = pending.target.min(max_top).max(0.0);
        let unchanged = pending
            .last_applied
            .is_some_and(|prev| (prev - next).abs() < 0.5);

        if done {
            self.pending = None;
            if next <= 0.0 {
                return ScrollRestoreAction::None;
            }
            return ScrollRestoreAction::ScrollTo { offset_px: next, done: true };
        }

        if max_top <= 0.0 || unchanged || next <= 0.0 {
            return ScrollRestoreAction::WaitMore;
        }
        pending.last_applied = Some(next);
        ScrollRestoreAction::ScrollTo { offset_px: next, done: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_restores_scroll_across_scope_switches() {
        let mut restore = ScrollRestore::default();
        assert!(restore.sync_scope("thread:a", false));
        restore.on_scroll("thread:a", 420.0);
        assert_eq!(restore.saved_offset("thread:a"), Some(420.0));

        // Switching to thread:b has no saved scroll yet.
        assert!(restore.sync_scope("thread:b", false));
        assert!(!restore.has_pending());
        restore.on_scroll("thread:b", 120.0);

        // Switching back to thread:a stages restoration of 420px.
        assert!(restore.sync_scope("thread:a", false));
        assert!(restore.has_pending());
        // Scroll events emitted during restore do not clobber 420px.
        restore.on_scroll("thread:a", 0.0);
        assert_eq!(restore.saved_offset("thread:a"), Some(420.0));

        assert_eq!(
            restore.apply("thread:a", 900.0, true),
            ScrollRestoreAction::ScrollTo { offset_px: 420.0, done: true }
        );
        assert!(!restore.has_pending());
    }

    #[test]
    fn waits_for_lazy_patches_and_clamps_once_settled() {
        let mut restore = ScrollRestore::default();
        restore.sync_scope("thread:a", false);
        restore.on_scroll("thread:a", 600.0);
        restore.sync_scope("thread:b", false);
        restore.sync_scope("thread:a", false);

        // Empty list while patches are still loading -> asks caller to fetch more without consuming pending.
        assert_eq!(restore.apply("thread:a", 0.0, false), ScrollRestoreAction::WaitMore);
        assert!(restore.has_pending());

        // Partial load shorter than 600px -> scrolls partway, keeps pending.
        assert_eq!(
            restore.apply("thread:a", 300.0, false),
            ScrollRestoreAction::ScrollTo { offset_px: 300.0, done: false }
        );
        assert_eq!(restore.apply("thread:a", 300.0, false), ScrollRestoreAction::WaitMore);
        assert!(restore.has_pending());

        // Settled at 450px -> clamps to 450px and finishes.
        assert_eq!(
            restore.apply("thread:a", 450.0, true),
            ScrollRestoreAction::ScrollTo { offset_px: 450.0, done: true }
        );
        assert!(!restore.has_pending());
    }

    #[test]
    fn pending_reveal_and_discard_clear_restore() {
        let mut restore = ScrollRestore::default();
        restore.sync_scope("thread:a", false);
        restore.on_scroll("thread:a", 280.0);
        restore.sync_scope("thread:b", false);

        // Explicit reveal on entering thread:a wins over saved scroll.
        restore.sync_scope("thread:a", true);
        assert!(!restore.has_pending());
        assert_eq!(restore.saved_offset("thread:a"), None);

        // Explicit discard clears both saved and pending.
        restore.on_scroll("thread:a", 350.0);
        restore.sync_scope("thread:b", false);
        restore.sync_scope("thread:a", false);
        restore.discard("thread:a");
        assert!(!restore.has_pending());
        assert_eq!(restore.saved_offset("thread:a"), None);
    }

    #[test]
    fn scrolling_to_top_clears_saved_entry() {
        let mut restore = ScrollRestore::default();
        restore.sync_scope("thread:a", false);
        restore.on_scroll("thread:a", 200.0);
        assert_eq!(restore.saved_offset("thread:a"), Some(200.0));
        restore.on_scroll("thread:a", 0.0);
        assert_eq!(restore.saved_offset("thread:a"), None);
    }
}
