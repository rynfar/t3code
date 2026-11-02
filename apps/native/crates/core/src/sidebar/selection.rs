//! Sidebar multi-select. Ports `apps/web/src/threadSelectionStore.ts` and the bulk menu builders
//! (`buildBulkTitleRegenerationContextMenuItem`, `buildBulkUnpinContextMenuItem`) and the bulk
//! context menu in `apps/web/src/components/Sidebar.tsx` (`handleMultiSelectContextMenu`).

use std::collections::HashSet;

use crate::sidebar::settled::SnoozePreset;
use crate::thread_actions::MenuItem;

/// Selected thread ids in insertion order, as the web's `Set`, plus the range anchor.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ThreadSelection {
    selected: Vec<String>,
    anchor: Option<String>,
}

impl ThreadSelection {
    pub fn selected(&self) -> &[String] {
        &self.selected
    }

    pub fn anchor(&self) -> Option<&str> {
        self.anchor.as_deref()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.selected.iter().any(|key| key == id)
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub fn len(&self) -> usize {
        self.selected.len()
    }

    /// `toggleThread`: a newly selected thread becomes the anchor.
    pub fn toggle(&mut self, id: &str) {
        if let Some(position) = self.selected.iter().position(|key| key == id) {
            self.selected.remove(position);
        } else {
            self.selected.push(id.to_owned());
            self.anchor = Some(id.to_owned());
        }
    }

    /// `rangeSelectTo`: adds the anchor-to-target run of `ordered` and keeps the anchor. Without a
    /// usable anchor it adds the target alone and anchors there.
    pub fn range_select_to(&mut self, id: &str, ordered: &[&str]) {
        let range = self.anchor.as_deref().and_then(|anchor| {
            let from = ordered.iter().position(|key| *key == anchor)?;
            let to = ordered.iter().position(|key| *key == id)?;
            Some(from.min(to)..=from.max(to))
        });
        match range {
            Some(range) => {
                for key in &ordered[range] {
                    self.add(key);
                }
            }
            None => {
                self.add(id);
                self.anchor = Some(id.to_owned());
            }
        }
    }

    fn add(&mut self, id: &str) {
        if !self.contains(id) {
            self.selected.push(id.to_owned());
        }
    }

    /// `clearSelection`. Returns whether anything changed.
    pub fn clear(&mut self) -> bool {
        if self.selected.is_empty() && self.anchor.is_none() {
            return false;
        }
        self.selected.clear();
        self.anchor = None;
        true
    }

    /// `setAnchor`: a plain click anchors the next shift-click.
    pub fn set_anchor(&mut self, id: &str) {
        self.anchor = Some(id.to_owned());
    }

    /// `removeFromSelection`. Returns whether anything changed.
    pub fn remove(&mut self, ids: &[String]) -> bool {
        let remove: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let before = self.selected.len();
        self.selected.retain(|key| !remove.contains(key.as_str()));
        if self.selected.len() == before {
            return false;
        }
        if self.anchor.as_deref().is_some_and(|anchor| remove.contains(anchor)) {
            self.anchor = None;
        }
        true
    }
}

/// `getThreadKeysToDeselectAfterDelete`: drop deleted threads and ones that no longer exist.
pub fn keys_to_deselect_after_delete(selected: &[String], deleted: &HashSet<String>, has_thread: impl Fn(&str) -> bool) -> Vec<String> {
    selected.iter().filter(|key| deleted.contains(*key) || !has_thread(key)).cloned().collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkAction {
    Unpin,
    Settle,
    Snooze,
    SnoozePreset(String),
    SnoozeCustom,
    RegenerateTitles,
    MarkUnread,
    Delete,
}

/// What the bulk menu needs to know about the live selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkSelection {
    pub count: usize,
    pub pinned: usize,
    /// Selected threads that are not settled yet.
    pub settling: usize,
    /// Every selected thread can be snoozed now.
    pub can_snooze: bool,
    /// Selected threads whose title is not already regenerating.
    pub regeneratable: usize,
    /// Older servers lack settle, snooze, pin and title regeneration.
    pub config_loaded: bool,
    /// `canOperateThreads`: the session may change threads.
    pub can_operate: bool,
}

/// `buildBulkTitleRegenerationContextMenuItem`.
pub fn bulk_title_regeneration_item(supported: usize, actionable: usize) -> Option<MenuItem<BulkAction>> {
    if supported == 0 {
        return None;
    }
    if actionable == 0 {
        let mut item = MenuItem::new(BulkAction::RegenerateTitles, format!("Regenerating… ({supported})"), None);
        item.disabled = true;
        return Some(item);
    }
    Some(MenuItem::new(BulkAction::RegenerateTitles, format!("Regenerate titles ({actionable})"), None))
}

/// `buildBulkUnpinContextMenuItem`.
pub fn bulk_unpin_item(pinned: usize) -> Option<MenuItem<BulkAction>> {
    (pinned > 0).then(|| MenuItem::new(BulkAction::Unpin, format!("Unpin ({pinned})"), None))
}

/// The multi-select context menu, in the web's order.
pub fn build_bulk_menu(selection: &BulkSelection, presets: &[SnoozePreset]) -> Vec<MenuItem<BulkAction>> {
    let count = selection.count;
    let blocked = !selection.can_operate;
    let mut items = Vec::new();
    if let Some(mut unpin) = bulk_unpin_item(if selection.config_loaded { selection.pinned } else { 0 }) {
        unpin.disabled = blocked;
        items.push(unpin);
    }
    let mut settle = MenuItem::new(BulkAction::Settle, format!("Settle ({count})"), None);
    settle.disabled = selection.settling == 0 || blocked;
    items.push(settle);
    if selection.config_loaded && selection.can_snooze {
        let mut snooze = MenuItem::new(BulkAction::Snooze, format!("Snooze ({count})"), None);
        snooze.disabled = blocked;
        snooze.children = presets
            .iter()
            .map(|preset| {
                let mut item = MenuItem::new(BulkAction::SnoozePreset(preset.id.clone()), format!("{} ({})", preset.label, preset.when_label), None);
                item.disabled = blocked;
                item
            })
            .collect();
        let mut custom = MenuItem::new(BulkAction::SnoozeCustom, "Custom…", None);
        custom.separator_before = !presets.is_empty();
        custom.disabled = blocked;
        snooze.children.push(custom);
        items.push(snooze);
    }
    let supported = if selection.config_loaded { count } else { 0 };
    if let Some(mut regenerate) = bulk_title_regeneration_item(supported, selection.regeneratable) {
        regenerate.disabled = regenerate.disabled || blocked;
        items.push(regenerate);
    }
    items.push(MenuItem::new(BulkAction::MarkUnread, format!("Mark unread ({count})"), None));
    let mut delete = MenuItem::new(BulkAction::Delete, format!("Delete ({count})"), None);
    delete.destructive = true;
    delete.disabled = blocked;
    items.push(delete);
    items
}

/// The bulk delete confirmation: title and description.
pub fn bulk_delete_confirmation(count: usize) -> (String, &'static str) {
    (format!("Delete {count} thread{}?", if count == 1 { "" } else { "s" }), "This permanently clears conversation history for these threads.")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDERED: [&str; 5] = ["thread-a", "thread-b", "thread-c", "thread-d", "thread-e"];

    fn selected(selection: &ThreadSelection) -> Vec<&str> {
        selection.selected().iter().map(String::as_str).collect()
    }

    #[test]
    fn bulk_deletion_clears_missing_rows_and_completed_deletions_but_keeps_a_failed_thread() {
        let mut selection = ThreadSelection::default();
        for id in ["thread-a", "thread-b", "thread-c"] {
            selection.toggle(id);
        }
        let snapshot = selection.selected().to_vec();
        let existing: HashSet<&str> = ["thread-a", "thread-b"].into();
        let deleted: HashSet<String> = ["thread-a".to_owned()].into();
        selection.remove(&keys_to_deselect_after_delete(&snapshot, &deleted, |key| existing.contains(key)));
        assert_eq!(selected(&selection), ["thread-b"]);
        assert_eq!(selection.anchor(), None);
    }

    #[test]
    fn bulk_deletion_keeps_unprocessed_threads_and_selections_added_while_pending() {
        let mut selection = ThreadSelection::default();
        for id in ["thread-a", "thread-b", "thread-c"] {
            selection.toggle(id);
        }
        let snapshot = selection.selected().to_vec();
        selection.toggle("thread-d");
        let deleted: HashSet<String> = ["thread-a".to_owned()].into();
        selection.remove(&keys_to_deselect_after_delete(&snapshot, &deleted, |_| true));
        assert_eq!(selected(&selection), ["thread-b", "thread-c", "thread-d"]);
        assert_eq!(selection.anchor(), Some("thread-d"));
    }

    #[test]
    fn a_plain_click_anchor_enables_range_select() {
        let mut selection = ThreadSelection::default();
        selection.set_anchor("thread-b");
        selection.range_select_to("thread-d", &ORDERED);
        assert_eq!(selected(&selection), ["thread-b", "thread-c", "thread-d"]);
    }

    #[test]
    fn the_anchor_stays_put_across_range_selects() {
        let mut selection = ThreadSelection::default();
        selection.toggle("thread-b");
        selection.range_select_to("thread-d", &ORDERED);
        selection.range_select_to("thread-e", &ORDERED);
        assert_eq!(selection.anchor(), Some("thread-b"));
        assert_eq!(selected(&selection), ["thread-b", "thread-c", "thread-d", "thread-e"]);
    }

    #[test]
    fn range_select_falls_back_to_adding_the_target_when_the_anchor_is_not_listed() {
        let mut selection = ThreadSelection::default();
        selection.toggle("thread-a");
        selection.range_select_to("thread-c", &["thread-b", "thread-c", "thread-d"]);
        assert!(selection.contains("thread-c"));
        assert_eq!(selection.anchor(), Some("thread-c"));
    }

    #[test]
    fn range_select_keeps_earlier_picks_outside_the_range() {
        let mut selection = ThreadSelection::default();
        selection.toggle("thread-a");
        selection.toggle("thread-b");
        selection.range_select_to("thread-d", &ORDERED);
        assert_eq!(selected(&selection), ["thread-a", "thread-b", "thread-c", "thread-d"]);
    }

    #[test]
    fn toggling_off_keeps_the_anchor_and_clear_reports_changes() {
        let mut selection = ThreadSelection::default();
        selection.toggle("thread-a");
        selection.toggle("thread-a");
        assert!(selection.is_empty());
        assert_eq!(selection.anchor(), Some("thread-a"));
        assert!(selection.clear());
        assert!(!selection.clear());
    }

    fn labels(items: &[MenuItem<BulkAction>]) -> Vec<(String, bool)> {
        items.iter().map(|item| (item.label.clone(), item.disabled)).collect()
    }

    #[test]
    fn bulk_title_regeneration_reflects_threads_already_regenerating() {
        assert!(bulk_title_regeneration_item(0, 0).is_none());
        let busy = bulk_title_regeneration_item(3, 0).unwrap();
        assert_eq!((busy.label.as_str(), busy.disabled), ("Regenerating… (3)", true));
        let ready = bulk_title_regeneration_item(3, 2).unwrap();
        assert_eq!((ready.label.as_str(), ready.disabled), ("Regenerate titles (2)", false));
        assert!(bulk_unpin_item(0).is_none());
        assert_eq!(bulk_unpin_item(2).unwrap().label, "Unpin (2)");
    }

    #[test]
    fn the_bulk_menu_lists_actions_with_counts_in_the_web_order() {
        let presets = vec![SnoozePreset { id: "later".into(), label: "Later today".into(), when_label: "6:00 PM".into(), snoozed_until: 1 }];
        let selection = BulkSelection { count: 3, pinned: 1, settling: 2, can_snooze: true, regeneratable: 3, config_loaded: true, can_operate: true };
        let menu = build_bulk_menu(&selection, &presets);
        assert_eq!(
            labels(&menu),
            [
                ("Unpin (1)".to_owned(), false),
                ("Settle (3)".to_owned(), false),
                ("Snooze (3)".to_owned(), false),
                ("Regenerate titles (3)".to_owned(), false),
                ("Mark unread (3)".to_owned(), false),
                ("Delete (3)".to_owned(), false),
            ]
        );
        assert_eq!(labels(&menu[2].children), [("Later today (6:00 PM)".to_owned(), false), ("Custom…".to_owned(), false)]);
        assert!(menu[2].children[1].separator_before);
        assert!(menu[5].destructive);

        let settled = BulkSelection { pinned: 0, settling: 0, can_snooze: false, ..selection };
        let menu = build_bulk_menu(&settled, &presets);
        assert_eq!(menu[0].label, "Settle (3)");
        assert!(menu[0].disabled);
        assert!(menu.iter().all(|item| item.action != BulkAction::Snooze));
    }
}
