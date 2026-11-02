//! Navigation history, as the web router's back and forward (`navigation.back`,
//! `navigation.forward`, `HistoryNavigationButton`): each opened thread or new-thread draft is an
//! entry, opening one after going back drops the forward entries, and entries whose thread or
//! project is gone are skipped.

const HISTORY_LIMIT: usize = 100;

/// A page Back and Forward return to: a thread, or a draft in a project (the draft route, which
/// the home page also lands on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavEntry {
    Thread(String),
    Draft(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavHistory {
    entries: Vec<NavEntry>,
    index: usize,
}

impl NavHistory {
    /// Records an opened page. Reopening the current entry (a back or forward step) is a no-op.
    pub fn visit(&mut self, entry: NavEntry) {
        if self.entries.get(self.index) == Some(&entry) {
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.index + 1);
        }
        self.entries.push(entry);
        if self.entries.len() > HISTORY_LIMIT {
            self.entries.remove(0);
        }
        self.index = self.entries.len() - 1;
    }

    /// A draft that launched its thread gives way to it, as the web replaces the draft route.
    pub fn replace_draft(&mut self) {
        if matches!(self.entries.get(self.index), Some(NavEntry::Draft(_))) {
            self.entries.truncate(self.index);
            self.index = self.index.saturating_sub(1);
        }
    }

    fn step(&self, back: bool, exists: &impl Fn(&NavEntry) -> bool) -> Option<usize> {
        let mut index = self.index;
        loop {
            index = if back { index.checked_sub(1)? } else { Some(index + 1).filter(|next| *next < self.entries.len())? };
            if exists(&self.entries[index]) {
                return Some(index);
            }
        }
    }

    pub fn can_go(&self, back: bool, exists: impl Fn(&NavEntry) -> bool) -> bool {
        self.step(back, &exists).is_some()
    }

    /// Moves back or forward to the nearest entry that still exists.
    pub fn go(&mut self, back: bool, exists: impl Fn(&NavEntry) -> bool) -> Option<&NavEntry> {
        self.index = self.step(back, &exists)?;
        Some(&self.entries[self.index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str) -> NavEntry {
        NavEntry::Thread(id.to_owned())
    }

    #[test]
    fn back_and_forward_walk_visits_and_a_new_visit_drops_forward() {
        let mut history = NavHistory::default();
        for id in ["a", "b", "c"] {
            history.visit(thread(id));
        }
        let all = |_: &NavEntry| true;
        assert!(!history.can_go(false, all));
        assert_eq!(history.go(true, all), Some(&thread("b")));
        history.visit(thread("b"));
        assert_eq!(history.go(true, all), Some(&thread("a")));
        assert!(!history.can_go(true, all));
        assert_eq!(history.go(false, all), Some(&thread("b")));
        history.visit(thread("d"));
        assert!(!history.can_go(false, all));
        assert_eq!(history.go(true, all), Some(&thread("b")));
    }

    #[test]
    fn deleted_threads_are_skipped() {
        let mut history = NavHistory::default();
        for id in ["a", "gone", "c"] {
            history.visit(thread(id));
        }
        let exists = |entry: &NavEntry| *entry != thread("gone");
        assert_eq!(history.go(true, exists), Some(&thread("a")));
        assert_eq!(history.go(false, exists), Some(&thread("c")));
        assert!(!history.can_go(true, |entry| *entry == thread("c")));
    }

    #[test]
    fn going_home_is_an_entry_back_returns_from() {
        let all = |_: &NavEntry| true;
        let mut history = NavHistory::default();
        history.visit(thread("a"));
        history.visit(NavEntry::Draft("p".into()));
        assert_eq!(history.go(true, all), Some(&thread("a")));
        assert_eq!(history.go(false, all), Some(&NavEntry::Draft("p".into())));

        // A launched draft is replaced by its thread.
        history.replace_draft();
        history.visit(thread("b"));
        assert_eq!(history.go(true, all), Some(&thread("a")));
        assert_eq!(history.go(false, all), Some(&thread("b")));
        assert!(!history.can_go(false, all));
    }
}
