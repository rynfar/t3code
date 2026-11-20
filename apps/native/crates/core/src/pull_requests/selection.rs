//! Draft selection state for pull request label and reviewer pickers (`pingdotgg/t3code#37733`).
//! Ports `packages/client-runtime/src/pullRequestSelection.ts`: picks accumulate locally while a
//! picker is open and are diffed against the host's latest state and sent in contract-sized
//! batches when the picker closes.

/// `PULL_REQUEST_CHANGE_BATCH_LIMIT`: the most labels or reviewers one write may change.
pub const PULL_REQUEST_CHANGE_BATCH_LIMIT: usize = 25;

/// Picks that differ from the host, keyed by candidate. Preserves insertion order so batches are
/// deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionDraft<T> {
    entries: Vec<(String, T, bool)>,
}

impl<T> Default for SelectionDraft<T> {
    fn default() -> Self {
        Self { entries: Vec::new() }
    }
}

impl<T> SelectionDraft<T> {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// `isSelected`: returns the draft selection if toggled, otherwise the host state.
    pub fn is_selected(&self, key: &str, host_selected: bool) -> bool {
        self.entries
            .iter()
            .find(|(k, _, _)| k == key)
            .map_or(host_selected, |(_, _, selected)| *selected)
    }

    /// `toggleSelection`: flips the candidate's selection; drops the draft entry if it returns to
    /// `host_selected`.
    pub fn toggle(&mut self, key: String, item: T, host_selected: bool) {
        let next_selected = !self.is_selected(&key, host_selected);
        if next_selected == host_selected {
            self.entries.retain(|(k, _, _)| k != &key);
        } else if let Some((_, existing, selected)) = self.entries.iter_mut().find(|(k, _, _)| k == &key) {
            *existing = item;
            *selected = next_selected;
        } else {
            self.entries.push((key, item, next_selected));
        }
    }

    /// `selectionChanges`: diffs the accumulated draft against the host's latest state at close,
    /// so a refresh while the picker was open never resends already-matching changes.
    pub fn changes(&self, host_selected: impl Fn(&str, &T) -> bool) -> SelectionChanges<T>
    where
        T: Clone,
    {
        let mut add = Vec::new();
        let mut remove = Vec::new();
        for (key, item, selected) in &self.entries {
            if *selected == host_selected(key, item) {
                continue;
            }
            if *selected {
                add.push(item.clone());
            } else {
                remove.push(item.clone());
            }
        }
        SelectionChanges { add, remove }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionChanges<T> {
    pub add: Vec<T>,
    pub remove: Vec<T>,
}

impl<T> SelectionChanges<T> {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty()
    }
}

/// `changeBatches`: splits a change list into writes the contract accepts (max 25 per call).
pub fn change_batches<T>(items: &[T]) -> Vec<&[T]> {
    if items.is_empty() {
        return Vec::new();
    }
    items.chunks(PULL_REQUEST_CHANGE_BATCH_LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn groups_picks_into_add_and_remove_and_ignores_toggled_back() {
        let host: HashSet<&str> = ["bug", "chore"].into_iter().collect();
        let mut draft = SelectionDraft::<String>::empty();
        for key in ["docs", "bug", "feature", "chore", "chore"] {
            draft.toggle(key.into(), key.into(), host.contains(key));
        }
        assert!(!draft.is_selected("bug", true));
        assert!(draft.is_selected("docs", false));
        assert!(draft.is_selected("chore", true));
        let changes = draft.changes(|key, _| host.contains(key));
        assert_eq!(changes.add, vec!["docs", "feature"]);
        assert_eq!(changes.remove, vec!["bug"]);

        // Toggling back leaves an empty draft.
        let mut back = SelectionDraft::<String>::empty();
        back.toggle("bug".into(), "bug".into(), true);
        back.toggle("bug".into(), "bug".into(), true);
        assert!(back.is_empty());
        assert!(back.changes(|key, _| host.contains(key)).is_empty());
    }

    #[test]
    fn diffs_against_host_state_at_close_and_chunks_batches_at_25() {
        let initial_host: HashSet<&str> = ["bug"].into_iter().collect();
        let mut draft = SelectionDraft::<String>::empty();
        draft.toggle("docs".into(), "docs".into(), initial_host.contains("docs"));
        draft.toggle("bug".into(), "bug".into(), initial_host.contains("bug"));

        // Host refreshed while picker was open: docs is now applied, bug is already gone.
        let refreshed_host: HashSet<&str> = ["docs"].into_iter().collect();
        let changes = draft.changes(|key, _| refreshed_host.contains(key));
        assert!(changes.is_empty());

        let many: Vec<usize> = (0..26).collect();
        let batches = change_batches(&many);
        assert_eq!(batches.iter().map(|b| b.len()).collect::<Vec<_>>(), vec![25, 1]);
        assert!(change_batches::<usize>(&[]).is_empty());
    }
}
