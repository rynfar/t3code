//! Which items the thread details card shows (`components/chat/threadDetailsItems.ts`). Only
//! choices that differ from the default are stored, so a later default change reaches everyone
//! who never touched that item.

use std::collections::HashMap;

/// Ids are persisted, so never rename one.
pub const ITEMS: &[(&str, &str, bool)] = &[
    ("environment", "Environment and worktree", true),
    ("openIn", "Open in editor", true),
    ("scripts", "Project scripts", true),
    ("branch", "Branch and pull request", true),
    ("gitActions", "Git actions", true),
    ("changes", "Changes", true),
    ("lineage", "Lineage", true),
];

fn default_visible(id: &str) -> bool {
    ITEMS.iter().find(|(item, _, _)| *item == id).is_some_and(|(_, _, visible)| *visible)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Visibility {
    overrides: HashMap<String, bool>,
}

impl Visibility {
    pub fn from_overrides(overrides: HashMap<String, bool>) -> Self {
        Visibility { overrides }
    }

    pub fn overrides(&self) -> &HashMap<String, bool> {
        &self.overrides
    }

    /// `resolveThreadDetailsVisibility`.
    pub fn shows(&self, id: &str) -> bool {
        self.overrides.get(id).copied().unwrap_or_else(|| default_visible(id))
    }

    /// `setThreadDetailsItem`.
    pub fn set(&mut self, id: &str, visible: bool) {
        if visible == default_visible(id) {
            self.overrides.remove(id);
        } else {
            self.overrides.insert(id.to_owned(), visible);
        }
    }

    /// `isDefaultThreadDetailsVisibility`.
    pub fn is_default(&self) -> bool {
        ITEMS.iter().all(|(id, _, visible)| self.shows(id) == *visible)
    }

    pub fn reset(&mut self) {
        self.overrides.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_only_choices_that_differ_from_the_default() {
        let mut visibility = Visibility::default();
        assert!(visibility.shows("branch") && visibility.is_default());
        visibility.set("branch", false);
        assert!(!visibility.shows("branch"));
        assert_eq!(visibility.overrides().get("branch"), Some(&false));
        visibility.set("branch", true);
        assert!(visibility.overrides().is_empty(), "back to the default stores nothing");
        assert!(!visibility.shows("unknown"));
        for (id, _, _) in ITEMS {
            visibility.set(id, false);
        }
        assert!(ITEMS.iter().all(|(id, _, _)| !visibility.shows(id)));
        visibility.reset();
        assert!(visibility.is_default());
    }
}
