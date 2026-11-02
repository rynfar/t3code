//! Thread navigation history, as the web router's back and forward (`navigation.back`,
//! `navigation.forward`, `HistoryNavigationButton`): each opened thread is an entry, opening one
//! after going back drops the forward entries, and threads that are gone are skipped.

const HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavHistory {
    entries: Vec<String>,
    index: usize,
}

impl NavHistory {
    /// Records an opened thread. Reopening the current entry (a back or forward step) is a no-op.
    pub fn visit(&mut self, thread_id: &str) {
        if self.entries.get(self.index).is_some_and(|current| current == thread_id) {
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.index + 1);
        }
        self.entries.push(thread_id.to_owned());
        if self.entries.len() > HISTORY_LIMIT {
            self.entries.remove(0);
        }
        self.index = self.entries.len() - 1;
    }

    fn step(&self, back: bool, exists: &impl Fn(&str) -> bool) -> Option<usize> {
        let mut index = self.index;
        loop {
            index = if back { index.checked_sub(1)? } else { Some(index + 1).filter(|next| *next < self.entries.len())? };
            if exists(&self.entries[index]) {
                return Some(index);
            }
        }
    }

    pub fn can_go(&self, back: bool, exists: impl Fn(&str) -> bool) -> bool {
        self.step(back, &exists).is_some()
    }

    /// Moves back or forward to the nearest thread that still exists.
    pub fn go(&mut self, back: bool, exists: impl Fn(&str) -> bool) -> Option<&str> {
        self.index = self.step(back, &exists)?;
        Some(&self.entries[self.index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_and_forward_walk_visits_and_a_new_visit_drops_forward() {
        let mut history = NavHistory::default();
        for id in ["a", "b", "c"] {
            history.visit(id);
        }
        let all = |_: &str| true;
        assert!(!history.can_go(false, all));
        assert_eq!(history.go(true, all), Some("b"));
        history.visit("b");
        assert_eq!(history.go(true, all), Some("a"));
        assert!(!history.can_go(true, all));
        assert_eq!(history.go(false, all), Some("b"));
        history.visit("d");
        assert!(!history.can_go(false, all));
        assert_eq!(history.go(true, all), Some("b"));
    }

    #[test]
    fn deleted_threads_are_skipped() {
        let mut history = NavHistory::default();
        for id in ["a", "gone", "c"] {
            history.visit(id);
        }
        assert_eq!(history.go(true, |id| id != "gone"), Some("a"));
        assert_eq!(history.go(false, |id| id != "gone"), Some("c"));
        assert!(!history.can_go(true, |id| id == "c"));
    }
}
