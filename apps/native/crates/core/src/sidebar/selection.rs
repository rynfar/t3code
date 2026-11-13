//! Sidebar multi-select. Ports `apps/web/src/threadSelectionStore.ts` and the bulk menu builders
//! (`buildBulkTitleRegenerationContextMenuItem`, `buildBulkUnpinContextMenuItem`) and the bulk
//! context menu in `apps/web/src/components/Sidebar.tsx` (`handleMultiSelectContextMenu`).

use std::collections::HashSet;
use std::hash::Hash;

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

    /// `navigateToThread`: opening a thread from any entry point drops the selection and anchors
    /// there. Returns whether the selection changed.
    pub fn open(&mut self, id: &str) -> bool {
        let changed = !self.selected.is_empty() || self.anchor.as_deref() != Some(id);
        self.selected.clear();
        self.anchor = Some(id.to_owned());
        changed
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
/// `selected` is the selection captured when the deletion started, so later picks survive.
pub fn keys_to_deselect_after_delete<K: Eq + Hash + Clone>(selected: &[K], deleted: &HashSet<K>, has_thread: impl Fn(&K) -> bool) -> Vec<K> {
    selected.iter().filter(|key| deleted.contains(*key) || !has_thread(key)).cloned().collect()
}

/// What Escape does in the sidebar: the search field's `handleThreadSearchKeyDown`, then the chat
/// route's `onKeyDown` in `chatRouteCommands.ts`, which modal pages and the palette switch off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EscapeContext {
    pub composing: bool,
    pub search_focused: bool,
    pub searching: bool,
    pub overlay_open: bool,
    pub has_selection: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarEscape {
    /// Leave the key, and the range anchor, to other handlers.
    Ignore,
    ClearSearch,
    /// `clearSelection`: the selection and its anchor, consuming the key.
    ClearSelection,
}

pub fn sidebar_escape(context: EscapeContext) -> SidebarEscape {
    if context.composing {
        SidebarEscape::Ignore
    } else if context.search_focused && context.searching {
        SidebarEscape::ClearSearch
    } else if context.has_selection && !context.overlay_open {
        SidebarEscape::ClearSelection
    } else {
        SidebarEscape::Ignore
    }
}

impl BulkAction {
    pub fn selection_clear(&self) -> SelectionClear {
        match self {
            Self::RegenerateTitles => SelectionClear::AllSucceeded,
            Self::Delete => SelectionClear::Results,
            _ => SelectionClear::Started,
        }
    }
}

/// When a bulk action lets go of the selection (`handleMultiSelectContextMenu`). A refused or
/// cancelled action keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionClear {
    /// Once the writes are under way.
    Started,
    /// Only when every write succeeded; the first failure stops and keeps the selection.
    AllSucceeded,
    /// Per result: `keys_to_deselect_after_delete`.
    Results,
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

#[derive(Debug, Clone, Copy)]
pub struct BulkTarget {
    pub pinned: bool,
    pub settled: bool,
    pub regenerating: bool,
    pub config_loaded: bool,
    pub can_snooze: bool,
    pub can_operate: bool,
}

impl BulkAction {
    pub fn eligible(&self, target: &BulkTarget) -> bool {
        match self {
            Self::Unpin => target.config_loaded && target.pinned,
            Self::Settle => !target.settled,
            Self::RegenerateTitles => target.config_loaded && !target.regenerating,
            _ => true,
        }
    }

    pub fn ready(&self, target: &BulkTarget) -> bool {
        target.can_operate && (matches!(self, Self::Delete) || target.config_loaded)
            && (!matches!(self, Self::Snooze | Self::SnoozePreset(_) | Self::SnoozeCustom) || target.can_snooze)
    }
}

/// What the bulk menu needs to know about the live selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkSelection {
    pub count: usize,
    pub pinned: usize,
    pub settling: usize,
    pub can_snooze: bool,
    pub supported: usize,
    pub regeneratable: usize,
    pub unpin_ready: bool,
    pub settle_ready: bool,
    pub snooze_ready: bool,
    pub regenerate_ready: bool,
    pub unread_ready: bool,
    pub delete_ready: bool,
}

impl BulkSelection {
    pub fn from_targets(targets: &[BulkTarget]) -> Self {
        let count = |action: &BulkAction| targets.iter().filter(|target| action.eligible(target)).count();
        let ready = |action: &BulkAction| targets.iter().filter(|target| action.eligible(target)).all(|target| action.ready(target));
        Self {
            count: targets.len(), pinned: count(&BulkAction::Unpin), settling: count(&BulkAction::Settle),
            can_snooze: targets.iter().all(|target| target.config_loaded && target.can_snooze),
            supported: targets.iter().filter(|target| target.config_loaded).count(),
            regeneratable: count(&BulkAction::RegenerateTitles),
            unpin_ready: ready(&BulkAction::Unpin), settle_ready: ready(&BulkAction::Settle), snooze_ready: ready(&BulkAction::Snooze),
            regenerate_ready: ready(&BulkAction::RegenerateTitles), unread_ready: ready(&BulkAction::MarkUnread), delete_ready: ready(&BulkAction::Delete),
        }
    }
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
    let mut items = Vec::new();
    if let Some(mut unpin) = bulk_unpin_item(selection.pinned) {
        unpin.disabled = !selection.unpin_ready;
        items.push(unpin);
    }
    let mut settle = MenuItem::new(BulkAction::Settle, format!("Settle ({count})"), None);
    settle.disabled = selection.settling == 0 || !selection.settle_ready;
    items.push(settle);
    if selection.can_snooze {
        let mut snooze = MenuItem::new(BulkAction::Snooze, format!("Snooze ({count})"), None);
        snooze.disabled = !selection.snooze_ready;
        snooze.children = presets
            .iter()
            .map(|preset| {
                let mut item = MenuItem::new(BulkAction::SnoozePreset(preset.id.clone()), format!("{} ({})", preset.label, preset.when_label), None);
                item.disabled = !selection.snooze_ready;
                item
            })
            .collect();
        let mut custom = MenuItem::new(BulkAction::SnoozeCustom, "Custom…", None);
        custom.separator_before = !presets.is_empty();
        custom.disabled = !selection.snooze_ready;
        snooze.children.push(custom);
        items.push(snooze);
    }
    if let Some(mut regenerate) = bulk_title_regeneration_item(selection.supported, selection.regeneratable) {
        regenerate.disabled = regenerate.disabled || !selection.regenerate_ready;
        items.push(regenerate);
    }
    let mut unread = MenuItem::new(BulkAction::MarkUnread, format!("Mark unread ({count})"), None);
    unread.disabled = !selection.unread_ready;
    items.push(unread);
    let mut delete = MenuItem::new(BulkAction::Delete, format!("Delete ({count})"), None);
    delete.destructive = true;
    delete.disabled = !selection.delete_ready;
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
        selection.remove(&keys_to_deselect_after_delete(&snapshot, &deleted, |key: &String| existing.contains(key.as_str())));
        assert_eq!(selected(&selection), ["thread-b"]);
        assert_eq!(selection.anchor(), None);
    }

    #[test]
    fn bulk_deletion_deselects_by_environment_scoped_key() {
        let selected = [("env-a", "thread"), ("env-b", "thread"), ("env-a", "gone"), ("env-a", "failed")];
        let deleted: HashSet<_> = [("env-a", "thread")].into();
        let existing: HashSet<_> = [("env-a", "thread"), ("env-b", "thread"), ("env-a", "failed")].into();
        assert_eq!(keys_to_deselect_after_delete(&selected, &deleted, |key| existing.contains(key)), [("env-a", "thread"), ("env-a", "gone")]);
    }

    #[test]
    fn opening_a_thread_drops_the_selection_and_anchors_there() {
        let mut selection = ThreadSelection::default();
        selection.toggle("thread-a");
        selection.toggle("thread-b");
        assert!(selection.open("thread-d"));
        assert!(selection.is_empty());
        assert_eq!(selection.anchor(), Some("thread-d"));
        assert!(!selection.open("thread-d"));
        selection.range_select_to("thread-b", &ORDERED);
        assert_eq!(selected(&selection), ["thread-b", "thread-c", "thread-d"]);
    }

    #[test]
    fn escape_clears_search_first_then_a_selection_and_otherwise_passes() {
        let selecting = EscapeContext { has_selection: true, ..EscapeContext::default() };
        assert_eq!(sidebar_escape(selecting), SidebarEscape::ClearSelection);
        assert_eq!(sidebar_escape(EscapeContext { search_focused: true, searching: true, ..selecting }), SidebarEscape::ClearSearch);
        assert_eq!(sidebar_escape(EscapeContext { search_focused: true, ..selecting }), SidebarEscape::ClearSelection);
        for context in [
            EscapeContext { composing: true, search_focused: true, searching: true, ..selecting },
            EscapeContext { overlay_open: true, ..selecting },
            EscapeContext { search_focused: true, ..EscapeContext::default() },
            EscapeContext::default(),
        ] {
            assert_eq!(sidebar_escape(context), SidebarEscape::Ignore);
        }
    }

    #[test]
    fn bulk_actions_release_the_selection_at_the_web_moment() {
        for action in [BulkAction::Unpin, BulkAction::Settle, BulkAction::Snooze, BulkAction::SnoozePreset("later".into()), BulkAction::SnoozeCustom, BulkAction::MarkUnread] {
            assert_eq!(action.selection_clear(), SelectionClear::Started);
        }
        assert_eq!(BulkAction::RegenerateTitles.selection_clear(), SelectionClear::AllSucceeded);
        assert_eq!(BulkAction::Delete.selection_clear(), SelectionClear::Results);
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
        let target = BulkTarget { pinned:false, settled:false, regenerating:false, config_loaded:true, can_snooze:true, can_operate:true };
        let selection = BulkSelection::from_targets(&[BulkTarget { pinned:true, settled:true, ..target }, target, target]);
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
        let denied_non_target = BulkTarget { settled:true, config_loaded:false, can_operate:false, ..target };
        let subset = BulkSelection::from_targets(&[denied_non_target, BulkTarget { pinned:true, ..target }]);
        let menu = build_bulk_menu(&subset, &presets);
        for action in [BulkAction::Unpin, BulkAction::Settle, BulkAction::RegenerateTitles] {
            assert!(!menu.iter().find(|item| item.action == action).unwrap().disabled);
        }
        assert!(menu.iter().find(|item| item.action == BulkAction::MarkUnread).unwrap().disabled);
        assert!(!BulkAction::MarkUnread.ready(&denied_non_target));
        let busy = BulkSelection::from_targets(&[BulkTarget { regenerating:true, settled:true, ..target }]);
        let menu = build_bulk_menu(&busy, &presets);
        for action in [BulkAction::Settle, BulkAction::RegenerateTitles] {
            assert!(menu.iter().find(|item| item.action == action).unwrap().disabled);
        }

        let no_config_operate = BulkTarget { pinned: true, config_loaded: false, can_snooze: false, ..target };
        let no_config_only = BulkSelection::from_targets(&[no_config_operate]);
        let menu = build_bulk_menu(&no_config_only, &presets);
        assert!(menu.iter().all(|item| item.action != BulkAction::Unpin && item.action != BulkAction::Snooze && item.action != BulkAction::RegenerateTitles));
        assert!(!menu.iter().find(|item| item.action == BulkAction::Delete).unwrap().disabled);
        assert!(menu.iter().find(|item| item.action == BulkAction::Settle).unwrap().disabled);
        assert!(menu.iter().find(|item| item.action == BulkAction::MarkUnread).unwrap().disabled);

        let mixed = BulkSelection::from_targets(&[
            no_config_operate,
            BulkTarget { pinned: true, regenerating: true, can_operate: false, ..target },
            BulkTarget { pinned: true, ..target },
        ]);
        let menu = build_bulk_menu(&mixed, &presets);
        assert_eq!(menu.iter().find(|item| item.action == BulkAction::Unpin).map(|i| (i.label.as_str(), i.disabled)), Some(("Unpin (2)", true)));
        assert_eq!(menu.iter().find(|item| item.action == BulkAction::RegenerateTitles).map(|i| (i.label.as_str(), i.disabled)), Some(("Regenerate titles (1)", false)));
        assert!(menu.iter().find(|item| item.action == BulkAction::Delete).unwrap().disabled);
        assert!(menu.iter().all(|item| item.action != BulkAction::Snooze));

        let read_only_snooze = BulkSelection::from_targets(&[target, BulkTarget { can_operate: false, ..target }]);
        let menu = build_bulk_menu(&read_only_snooze, &presets);
        let snooze = menu.iter().find(|item| item.action == BulkAction::Snooze).unwrap();
        assert!(snooze.disabled && snooze.children.iter().all(|child| child.disabled));
    }
}
