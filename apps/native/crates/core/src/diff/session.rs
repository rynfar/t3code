//! Defaults and explicit session overrides from `web/src/components/DiffPanel.tsx` and
//! `web/src/diffPanelStore.ts`.

use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct DiffSession {
    whitespace: HashMap<String, bool>,
    folds: HashMap<String, HashSet<String>>,
}

impl DiffSession {
    pub fn ignore_whitespace(&self, thread: &str, default: bool) -> bool {
        self.whitespace.get(thread).copied().unwrap_or(default)
    }

    pub fn toggle_whitespace(&mut self, thread: &str, default: bool) -> bool {
        let next = !self.ignore_whitespace(thread, default);
        self.whitespace.insert(thread.to_owned(), next);
        next
    }

    pub fn resolve_folds(&self, scope: &str, keys: &[String], default: bool) -> Vec<bool> {
        let stored = self.stored_folds(scope);
        keys.iter().map(|key| stored.map_or(default, |folds| folds.contains(key))).collect()
    }

    pub fn stored_folds(&self, scope: &str) -> Option<&HashSet<String>> {
        self.folds.get(scope)
    }

    pub fn set_file_fold(&mut self, scope: String, key: String, folded: bool, initial: impl FnOnce() -> HashSet<String>) {
        let folds = self.folds.entry(scope).or_insert_with(initial);
        if folded {
            folds.insert(key);
        } else {
            folds.remove(&key);
        }
    }

    pub fn remember_folds(&mut self, scope: String, folds: HashSet<String>) {
        self.folds.insert(scope, folds);
    }
}
