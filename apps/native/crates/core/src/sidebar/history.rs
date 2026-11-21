//! Navigation history, as the web router's back and forward (`navigation.back`,
//! `navigation.forward`, `HistoryNavigationButton`): each opened thread or new-thread draft is an
//! entry, opening one after going back drops the forward entries, and entries whose thread or
//! project is gone are skipped.

use std::collections::HashSet;

use crate::composer::drafts::DraftRef;

const HISTORY_LIMIT: usize = 100;

/// A page Back and Forward return to: a thread, or a draft in a project (the draft route, which
/// the home page also lands on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavEntry {
    Thread(String),
    Draft(DraftRef),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavHistory {
    entries: Vec<NavEntry>,
    index: usize,
    replacing_draft: bool,
    launched_from_draft: HashSet<String>,
}

impl NavHistory {
    /// Records an opened page. Reopening the current entry (a back or forward step) is a no-op.
    pub fn visit(&mut self, entry: NavEntry) {
        if std::mem::take(&mut self.replacing_draft)
            && let NavEntry::Thread(ref id) = entry
        {
            self.launched_from_draft.insert(id.clone());
        }
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

    pub fn forget_draft(&mut self, draft: &DraftRef) {
        let current = self.index;
        let mut position = 0;
        self.entries.retain(|entry| {
            let keep = !matches!(entry, NavEntry::Draft(entry_draft) if entry_draft == draft);
            if !keep && position <= current {
                self.index = self.index.saturating_sub(1);
            }
            position += 1;
            keep
        });
    }

    pub fn promote_draft(&mut self, draft: &DraftRef, thread_id: &str) {
        for entry in &mut self.entries {
            if matches!(entry, NavEntry::Draft(entry_draft) if entry_draft == draft) {
                *entry = NavEntry::Thread(thread_id.to_owned());
            }
        }
    }

    pub fn retarget_draft(&mut self, draft: &DraftRef) {
        for entry in &mut self.entries {
            if let NavEntry::Draft(previous) = entry
                && previous.id == draft.id
            {
                *previous = draft.clone();
            }
        }
    }

    /// A draft that launched its thread gives way to it, as the web replaces the draft route.
    pub fn replace_draft(&mut self) {
        if matches!(self.entries.get(self.index), Some(NavEntry::Draft(_))) {
            self.replacing_draft = true;
            self.entries.truncate(self.index);
            self.index = self.index.saturating_sub(1);
        }
    }

    pub fn launching_threads(&self) -> &HashSet<String> {
        &self.launched_from_draft
    }

    pub fn mark_launching(&mut self, id: &str) {
        self.launched_from_draft.insert(id.to_owned());
    }

    pub fn clear_launching(&mut self, id: &str) {
        self.launched_from_draft.remove(id);
    }

    pub fn retain_launching(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.launched_from_draft.retain(|id| keep(id));
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
    fn promoting_a_background_draft_replaces_only_its_history_entry() {
        let first = DraftRef::legacy("project");
        let second = DraftRef::new(crate::composer::drafts::DraftId::new("second".into()), "", "project");
        let mut history = NavHistory::default();
        history.visit(NavEntry::Draft(first.clone()));
        history.visit(NavEntry::Draft(second.clone()));
        history.promote_draft(&first, "launched");
        assert_eq!(history.go(true, |_| true), Some(&thread("launched")));
        assert_eq!(history.go(false, |_| true), Some(&NavEntry::Draft(second)));
    }

    #[test]
    fn going_home_is_an_entry_back_returns_from() {
        let all = |_: &NavEntry| true;
        let mut history = NavHistory::default();
        history.visit(thread("a"));
        history.visit(NavEntry::Draft(DraftRef::legacy("p")));
        assert_eq!(history.go(true, all), Some(&thread("a")));
        assert_eq!(history.go(false, all), Some(&NavEntry::Draft(DraftRef::legacy("p"))));

        // A launched draft is replaced by its thread.
        history.replace_draft();
        history.visit(thread("b"));
        assert!(history.launching_threads().contains("b"));
        history.retain_launching(|id| id != "b");
        assert!(history.launching_threads().is_empty());
        assert_eq!(history.go(true, all), Some(&thread("a")));
        assert_eq!(history.go(false, all), Some(&thread("b")));
        assert!(!history.can_go(false, all));
    }
}
