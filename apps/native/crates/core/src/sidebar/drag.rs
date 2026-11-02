//! Dragging threads between sidebar sections. Ports `resolveSidebarDropTarget`,
//! `resolveSidebarDropVerb`, `planSidebarThreadDrop` and the list markers from
//! `apps/web/src/components/Sidebar.logic.ts`, and the item list `sidebarListItemsFresh` builds in
//! `apps/web/src/components/Sidebar.tsx`. The dnd-kit sorting animation (`Sidebar.drag.ts`) is
//! replaced by an insertion slot: the native view reports which gap the pointer is over.

use std::collections::{HashMap, HashSet};

use super::Section;
use super::sort::plan_pinned_reorder;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Marker {
    PinnedHeader,
    PinnedDivider,
    ActivePlaceholder,
    WorkingHeader,
    SnoozedHeader,
    SettledHeader,
    SettledPlaceholder,
}

impl Marker {
    fn is_shelf_header(self) -> bool {
        matches!(self, Marker::WorkingHeader | Marker::SnoozedHeader | Marker::SettledHeader)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropItem {
    Thread { id: String, section: Section },
    Marker(Marker),
}

impl DropItem {
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            DropItem::Thread { id, .. } => Some(id),
            DropItem::Marker(_) => None,
        }
    }
}

pub fn position_of(items: &[DropItem], id: &str) -> Option<usize> {
    items.iter().position(|item| item.thread_id() == Some(id))
}

/// `sectionAtSidebarSlot`: the section a row lands in when placed at `index`.
fn section_at_slot(items: &[DropItem], index: usize) -> Section {
    let mut section = Section::Pinned;
    for item in items.iter().take(index) {
        if let DropItem::Marker(marker) = item {
            section = match marker {
                Marker::PinnedDivider => Section::Active,
                Marker::WorkingHeader => Section::Working,
                Marker::SnoozedHeader => Section::Snoozed,
                Marker::SettledHeader => Section::Settled,
                _ => section,
            };
        }
    }
    section
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropTarget {
    /// Pinned, Active or Settled.
    pub section: Section,
    pub pinned_order: Vec<String>,
    pub active_order: Vec<String>,
}

/// `resolveSidebarDropTarget`, where dnd-kit moves the dragged item to the index of the item it
/// is over.
pub fn resolve_drop_target(items: &[DropItem], active: usize, over: usize) -> Option<DropTarget> {
    resolve_drop_target_at_slot(items, active, if active < over { over + 1 } else { over })
}

/// The same target for an insertion gap: `slot` is the index in `items` the row would sit
/// before (`items.len()` for the end).
pub fn resolve_drop_target_at_slot(items: &[DropItem], active: usize, slot: usize) -> Option<DropTarget> {
    let dragged = items.get(active).filter(|item| matches!(item, DropItem::Thread { .. }))?;
    if slot > items.len() {
        return None;
    }
    let insert = if active < slot { slot - 1 } else { slot };
    let mut moved: Vec<&DropItem> = items.iter().enumerate().filter(|(index, _)| *index != active).map(|(_, item)| item).collect();
    moved.insert(insert, dragged);
    let moved: Vec<DropItem> = moved.into_iter().cloned().collect();
    let section = section_at_slot(&moved, insert);
    if matches!(section, Section::Working | Section::Snoozed) {
        return None;
    }
    let (mut pinned_order, mut active_order) = (Vec::new(), Vec::new());
    let mut current = Section::Pinned;
    for item in &moved {
        match item {
            DropItem::Marker(Marker::PinnedDivider) => current = Section::Active,
            DropItem::Marker(marker) if marker.is_shelf_header() => break,
            DropItem::Marker(_) => {}
            DropItem::Thread { id, .. } if current == Section::Pinned => pinned_order.push(id.clone()),
            DropItem::Thread { id, .. } => active_order.push(id.clone()),
        }
    }
    Some(DropTarget { section, pinned_order, active_order })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropVerb {
    Pin,
    Unpin,
    Settle,
    Unsettle,
    Wake,
}

impl DropVerb {
    /// The drop badge: Lucide icon and label (`dropVerbBadge` in `SidebarThreadRow.tsx`).
    pub fn badge(self) -> (&'static str, &'static str) {
        match self {
            DropVerb::Pin => ("pin", "Pin"),
            DropVerb::Unpin => ("pin-off", "Unpin"),
            DropVerb::Settle => ("circle-check", "Settle"),
            DropVerb::Unsettle => ("undo-2", "Un-settle"),
            DropVerb::Wake => ("alarm-clock-off", "Wake"),
        }
    }
}

/// `resolveSidebarDropVerb`.
pub fn resolve_drop_verb(from: Section, to: Option<Section>) -> Option<DropVerb> {
    let to = to?;
    if to == from || matches!(to, Section::Working | Section::Snoozed) {
        return None;
    }
    Some(match (to, from) {
        (Section::Pinned, _) => DropVerb::Pin,
        (Section::Settled, _) => DropVerb::Settle,
        (_, Section::Pinned) => DropVerb::Unpin,
        (_, Section::Settled) => DropVerb::Unsettle,
        _ => DropVerb::Wake,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub id: String,
    pub order_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropPlan {
    None,
    ReorderPinned { order: Vec<String>, assignments: Vec<Assignment> },
    Pin { order: Vec<String>, order_key: Option<String>, extra_assignments: Vec<Assignment> },
    MoveActive { order: Option<Vec<String>>, assignments: Vec<Assignment>, unpin: bool, unsettle: bool, unsnooze: bool },
    Settle,
}

pub struct DropPlanInput<'a> {
    pub active_key: &'a str,
    pub active_section: Section,
    pub active_pinned: bool,
    pub active_settled: bool,
    pub supports_settlement: bool,
    pub target: &'a DropTarget,
    pub pinned_order: &'a [String],
    pub pinned_keys_by_id: &'a HashMap<String, Option<String>>,
    pub reorderable_keys: Option<&'a HashSet<String>>,
    pub active_order: &'a [String],
    pub active_keys_by_id: &'a HashMap<String, Option<String>>,
    pub active_reorderable_keys: Option<&'a HashSet<String>>,
    pub active_time_ordered: bool,
}

/// `planSidebarThreadDrop`.
pub fn plan_thread_drop(input: &DropPlanInput) -> DropPlan {
    let target = input.target;
    if input.active_section == Section::Working {
        return DropPlan::None;
    }
    if !input.supports_settlement && (target.section == Section::Settled || input.active_settled) {
        return DropPlan::None;
    }
    let arrange = |order: &[String], keys_by_id: &HashMap<String, Option<String>>, writable: Option<&HashSet<String>>| -> Option<Vec<Assignment>> {
        let Some(writable) = writable else {
            return Some(plan_pinned_reorder(order, keys_by_id, input.active_key));
        };
        if !writable.contains(input.active_key) {
            return None;
        }
        let has_key = |key: &str| keys_by_id.get(key).is_some_and(Option::is_some);
        let candidates: Vec<String> = order.iter().filter(|key| writable.contains(*key) || has_key(key)).cloned().collect();
        let assignments = plan_pinned_reorder(&candidates, keys_by_id, input.active_key);
        if assignments.iter().any(|assignment| !writable.contains(&assignment.id)) {
            return None;
        }
        // Keyless Active rows sort before keyed rows; keyless Pinned rows sort after them.
        // Materializing any row across a frozen keyless boundary cannot persist that position.
        let assigned: HashSet<&str> = assignments.iter().map(|assignment| assignment.id.as_str()).collect();
        let (mut frozen_before, mut assigned_before) = (false, false);
        for id in order {
            let frozen = !writable.contains(id) && !has_key(id);
            let is_assigned = assigned.contains(id.as_str());
            if (target.section == Section::Active && frozen && assigned_before) || (target.section == Section::Pinned && is_assigned && frozen_before) {
                return None;
            }
            frozen_before |= frozen;
            assigned_before |= is_assigned;
        }
        Some(assignments)
    };
    match target.section {
        Section::Active => {
            let lifecycle = |order, assignments| DropPlan::MoveActive {
                order,
                assignments,
                unpin: input.active_pinned,
                unsettle: input.active_settled,
                unsnooze: input.active_section == Section::Snoozed,
            };
            if input.active_time_ordered {
                return if input.active_section == Section::Active { DropPlan::None } else { lifecycle(None, Vec::new()) };
            }
            let order = &target.active_order;
            if input.active_section == Section::Active && order.as_slice() == input.active_order {
                return DropPlan::None;
            }
            match arrange(order, input.active_keys_by_id, input.active_reorderable_keys) {
                Some(assignments) => lifecycle(Some(order.clone()), assignments),
                None => DropPlan::None,
            }
        }
        Section::Settled => {
            if input.active_section == Section::Settled {
                DropPlan::None
            } else {
                DropPlan::Settle
            }
        }
        Section::Pinned => {
            let order = &target.pinned_order;
            if input.active_section == Section::Pinned && order.as_slice() == input.pinned_order {
                return DropPlan::None;
            }
            let Some(assignments) = arrange(order, input.pinned_keys_by_id, input.reorderable_keys) else { return DropPlan::None };
            if input.active_section == Section::Pinned {
                return if assignments.is_empty() { DropPlan::None } else { DropPlan::ReorderPinned { order: order.clone(), assignments } };
            }
            let order_key = assignments.iter().find(|assignment| assignment.id == input.active_key).map(|assignment| assignment.order_key.clone());
            let extra_assignments = if input.active_pinned { assignments } else { assignments.into_iter().filter(|assignment| assignment.id != input.active_key).collect() };
            DropPlan::Pin { order: order.clone(), order_key, extra_assignments }
        }
        Section::Working | Section::Snoozed => DropPlan::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::sort::{generate_spread_pin_order_keys, pin_order_key_between};

    fn marker(marker: Marker) -> DropItem {
        DropItem::Marker(marker)
    }

    fn row(id: &str, section: Section) -> DropItem {
        DropItem::Thread { id: id.into(), section }
    }

    fn items() -> Vec<DropItem> {
        vec![
            marker(Marker::PinnedHeader),
            row("p1", Section::Pinned),
            marker(Marker::PinnedDivider),
            row("a1", Section::Active),
            row("a2", Section::Active),
            marker(Marker::WorkingHeader),
            row("w1", Section::Working),
            marker(Marker::SettledHeader),
            row("s1", Section::Settled),
        ]
    }

    fn target(items: &[DropItem], active: &str, over: &str) -> Option<DropTarget> {
        resolve_drop_target(items, position_of(items, active)?, position_of(items, over)?)
    }

    fn strings(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| (*key).to_owned()).collect()
    }

    fn keys(entries: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
        entries.iter().map(|(id, key)| ((*id).to_owned(), key.map(str::to_owned))).collect()
    }

    #[test]
    fn never_drops_into_the_working_shelf_and_keeps_it_out_of_the_inbox_order() {
        let items = items();
        assert_eq!(target(&items, "a1", "w1"), None);
        assert_eq!(target(&items, "p1", "a2"), Some(DropTarget { section: Section::Active, pinned_order: vec![], active_order: strings(&["a1", "a2", "p1"]) }));
        assert_eq!(resolve_drop_verb(Section::Active, Some(Section::Working)), None);
    }

    #[test]
    fn a_working_thread_never_moves_when_its_drag_ends_in_the_sidebar() {
        let items = items();
        let pinned_keys = keys(&[("p1", Some("m"))]);
        let active_keys = keys(&[("a1", Some("f")), ("a2", Some("t"))]);
        let (pinned_order, active_order) = (strings(&["p1"]), strings(&["a1", "a2"]));
        for (section, over) in [(Section::Pinned, "p1"), (Section::Active, "a2"), (Section::Settled, "s1")] {
            let target = target(&items, "w1", over).unwrap();
            assert_eq!(target.section, section);
            let plan = plan_thread_drop(&DropPlanInput {
                active_key: "w1",
                active_section: Section::Working,
                active_pinned: false,
                active_settled: false,
                supports_settlement: true,
                target: &target,
                pinned_order: &pinned_order,
                pinned_keys_by_id: &pinned_keys,
                reorderable_keys: None,
                active_order: &active_order,
                active_keys_by_id: &active_keys,
                active_reorderable_keys: None,
                active_time_ordered: true,
            });
            assert_eq!(plan, DropPlan::None);
        }
    }

    #[test]
    fn slots_resolve_the_gap_the_pointer_is_over() {
        let items = items();
        let a1 = position_of(&items, "a1").unwrap();
        // Above the Pinned label and just below it both pin.
        assert_eq!(resolve_drop_target_at_slot(&items, a1, 0).unwrap().section, Section::Pinned);
        assert_eq!(resolve_drop_target_at_slot(&items, a1, 1).unwrap().pinned_order, strings(&["a1", "p1"]));
        // Either side of the Active divider.
        assert_eq!(resolve_drop_target_at_slot(&items, a1, 2).unwrap().section, Section::Pinned);
        assert_eq!(resolve_drop_target_at_slot(&items, a1, 3).unwrap().section, Section::Active);
        // Below the Settled header settles; inside Working is refused.
        assert_eq!(resolve_drop_target_at_slot(&items, a1, 8).unwrap().section, Section::Settled);
        assert_eq!(resolve_drop_target_at_slot(&items, a1, 6), None);
        assert_eq!(resolve_drop_target_at_slot(&items, a1, 99), None);
    }

    #[test]
    fn drop_verbs_name_the_lifecycle_change() {
        let cases = [
            (Section::Active, Some(Section::Pinned), Some(DropVerb::Pin)),
            (Section::Pinned, Some(Section::Active), Some(DropVerb::Unpin)),
            (Section::Active, Some(Section::Settled), Some(DropVerb::Settle)),
            (Section::Settled, Some(Section::Active), Some(DropVerb::Unsettle)),
            (Section::Snoozed, Some(Section::Active), Some(DropVerb::Wake)),
            (Section::Active, Some(Section::Active), None),
            (Section::Active, Some(Section::Snoozed), None),
            (Section::Active, None, None),
        ];
        for (from, to, verb) in cases {
            assert_eq!(resolve_drop_verb(from, to), verb, "{from:?} -> {to:?}");
        }
    }

    #[test]
    fn arranges_the_rows_it_can_write_when_another_rows_server_cannot_store_an_order() {
        let none_keys = keys(&[("offline", None), ("a1", None), ("a2", None)]);
        let writable: HashSet<String> = ["a1", "a2"].iter().map(|key| (*key).to_owned()).collect();
        let target = DropTarget { section: Section::Active, pinned_order: vec![], active_order: strings(&["offline", "a2", "a1"]) };
        let empty = HashMap::new();
        let active_order = strings(&["offline", "a1", "a2"]);
        let input = DropPlanInput {
            active_key: "a2",
            active_section: Section::Active,
            active_pinned: false,
            active_settled: false,
            supports_settlement: true,
            target: &target,
            pinned_order: &[],
            pinned_keys_by_id: &empty,
            reorderable_keys: None,
            active_order: &active_order,
            active_keys_by_id: &none_keys,
            active_reorderable_keys: Some(&writable),
            active_time_ordered: false,
        };
        let DropPlan::MoveActive { assignments, .. } = plan_thread_drop(&input) else { panic!("expected move-active") };
        assert_eq!(assignments.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["a2", "a1"]);
        assert!(assignments[0].order_key < assignments[1].order_key);

        let keyed = keys(&[("offline", Some("m")), ("a2", Some("t"))]);
        let only_a2: HashSet<String> = ["a2".to_owned()].into();
        let above_target = DropTarget { section: Section::Active, pinned_order: vec![], active_order: strings(&["a2", "offline"]) };
        let above_order = strings(&["offline", "a2"]);
        let above = plan_thread_drop(&DropPlanInput { target: &above_target, active_order: &above_order, active_keys_by_id: &keyed, active_reorderable_keys: Some(&only_a2), ..input });
        assert!(matches!(above, DropPlan::MoveActive { ref assignments, .. } if assignments[0].order_key.as_str() < "m"));
    }

    /// The offline-configuration cases: `online` is writable, `offline` is cached but not
    /// connected, `neighbor` is writable. Returns the plan and whether the resulting sort matches.
    fn plan_offline(section: Section, rows: &[(&str, Option<&str>)], moved: &str, order: &[&str], from: Option<Section>, connected: bool) -> (DropPlan, Vec<Assignment>, bool) {
        let writable: HashSet<String> = rows.iter().filter(|(id, _)| connected && *id != "offline").map(|(id, _)| (*id).to_owned()).collect();
        let created = |id: &str| match id {
            "offline" => 12,
            "online" => 11,
            _ => 10,
        };
        let sort = |rows: &mut Vec<(String, Option<String>)>| {
            let (mut keyed, mut keyless): (Vec<_>, Vec<_>) = rows.drain(..).partition(|(_, key)| key.is_some());
            keyed.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
            keyless.sort_by(|a, b| created(&b.0).cmp(&created(&a.0)).then(a.0.cmp(&b.0)));
            *rows = if section == Section::Active { keyless.into_iter().chain(keyed).collect() } else { keyed.into_iter().chain(keyless).collect() };
        };
        let mut original: Vec<(String, Option<String>)> = rows.iter().map(|(id, key)| ((*id).to_owned(), key.map(str::to_owned))).collect();
        sort(&mut original);
        let original_order: Vec<String> = original.iter().map(|(id, _)| id.clone()).collect();
        let target_order = strings(order);
        let keys_by_id = keys(rows);
        let target = DropTarget { section, pinned_order: target_order.clone(), active_order: target_order.clone() };
        let plan = plan_thread_drop(&DropPlanInput {
            active_key: moved,
            active_section: from.unwrap_or(section),
            active_pinned: from.unwrap_or(section) == Section::Pinned,
            active_settled: false,
            supports_settlement: true,
            target: &target,
            pinned_order: &original_order,
            pinned_keys_by_id: &keys_by_id,
            reorderable_keys: Some(&writable),
            active_order: &original_order,
            active_keys_by_id: &keys_by_id,
            active_reorderable_keys: Some(&writable),
            active_time_ordered: false,
        });
        let assignments = match &plan {
            DropPlan::MoveActive { assignments, .. } | DropPlan::ReorderPinned { assignments, .. } => assignments.clone(),
            DropPlan::Pin { order_key, extra_assignments, .. } => extra_assignments.iter().cloned().chain(order_key.clone().map(|order_key| Assignment { id: moved.into(), order_key })).collect(),
            _ => vec![],
        };
        let mut actual: Vec<(String, Option<String>)> = rows
            .iter()
            .map(|(id, key)| ((*id).to_owned(), assignments.iter().find(|a| a.id == *id).map(|a| a.order_key.clone()).or(key.map(str::to_owned))))
            .collect();
        sort(&mut actual);
        let matches = actual.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>() == target_order;
        (plan, assignments, matches)
    }

    fn reorder_kind(section: Section) -> fn(&DropPlan) -> bool {
        if section == Section::Active { |plan| matches!(plan, DropPlan::MoveActive { .. }) } else { |plan| matches!(plan, DropPlan::ReorderPinned { .. }) }
    }

    #[test]
    fn replans_a_disconnected_drag_source() {
        for section in [Section::Active, Section::Pinned] {
            let rows = [("offline", Some("m")), ("online", Some("t"))];
            let (plan, _, _) = plan_offline(section, &rows, "online", &["online", "offline"], None, true);
            assert!(reorder_kind(section)(&plan), "{section:?}");
            let (plan, assignments, _) = plan_offline(section, &rows, "online", &["online", "offline"], None, false);
            assert_eq!(plan, DropPlan::None);
            assert!(assignments.is_empty());
        }
    }

    #[test]
    fn preserves_a_keyed_offline_bound() {
        for section in [Section::Active, Section::Pinned] {
            let (plan, assignments, matches) = plan_offline(section, &[("offline", Some("m")), ("online", Some("t"))], "online", &["online", "offline"], None, true);
            assert!(reorder_kind(section)(&plan), "{section:?}");
            assert_eq!(assignments.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["online"]);
            assert!(matches, "{section:?}");
        }
    }

    #[test]
    fn rejects_rebalancing_a_keyed_offline_bound() {
        for section in [Section::Active, Section::Pinned] {
            let (plan, assignments, _) = plan_offline(section, &[("offline", Some("m")), ("online", None), ("neighbor", None)], "online", &["offline", "neighbor", "online"], None, true);
            assert_eq!(plan, DropPlan::None, "{section:?}");
            assert!(assignments.is_empty());
        }
    }

    #[test]
    fn allows_realizable_moves_beside_a_frozen_keyless_row() {
        for section in [Section::Active, Section::Pinned] {
            let order: &[&str] = if section == Section::Active { &["offline", "neighbor", "online"] } else { &["online", "neighbor", "offline"] };
            let (plan, assignments, matches) = plan_offline(section, &[("offline", None), ("online", None), ("neighbor", None)], "online", order, None, true);
            assert!(reorder_kind(section)(&plan), "{section:?}");
            assert!(!assignments.is_empty());
            assert!(assignments.iter().all(|a| a.id != "offline"));
            assert!(matches, "{section:?}");
        }
    }

    #[test]
    fn checks_fallback_assignments_to_other_rows_at_the_boundary() {
        for section in [Section::Active, Section::Pinned] {
            let order: &[&str] = if section == Section::Active { &["neighbor", "offline", "online"] } else { &["online", "offline", "neighbor"] };
            let (plan, _, _) = plan_offline(section, &[("offline", None), ("online", None), ("neighbor", None)], "online", order, None, true);
            assert_eq!(plan, DropPlan::None, "{section:?}");
        }
    }

    #[test]
    fn rejects_impossible_pin_and_unpin_placement_across_the_boundary() {
        for section in [Section::Active, Section::Pinned] {
            let order: &[&str] = if section == Section::Active { &["online", "offline"] } else { &["offline", "online"] };
            let from = if section == Section::Active { Section::Pinned } else { Section::Active };
            let (plan, _, _) = plan_offline(section, &[("offline", None), ("online", None)], "online", order, Some(from), true);
            assert_eq!(plan, DropPlan::None, "{section:?}");
        }
    }

    #[test]
    fn only_changes_lifecycle_when_the_inbox_is_time_ordered() {
        let pinned_keys = keys(&[("p1", Some("m"))]);
        let active_keys = keys(&[("a1", Some("f")), ("a2", Some("t"))]);
        let (pinned_order, active_order) = (strings(&["p1"]), strings(&["a1", "a2"]));
        let reorder = DropTarget { section: Section::Active, pinned_order: strings(&["p1"]), active_order: strings(&["a2", "a1"]) };
        let base = DropPlanInput {
            active_key: "a1",
            active_section: Section::Active,
            active_pinned: false,
            active_settled: false,
            supports_settlement: true,
            target: &reorder,
            pinned_order: &pinned_order,
            pinned_keys_by_id: &pinned_keys,
            reorderable_keys: None,
            active_order: &active_order,
            active_keys_by_id: &active_keys,
            active_reorderable_keys: None,
            active_time_ordered: true,
        };
        assert_eq!(plan_thread_drop(&base), DropPlan::None);
        let unpin = DropTarget { section: Section::Active, pinned_order: vec![], active_order: strings(&["a1", "p1", "a2"]) };
        assert_eq!(
            plan_thread_drop(&DropPlanInput { active_key: "p1", active_section: Section::Pinned, active_pinned: true, target: &unpin, ..base }),
            DropPlan::MoveActive { order: None, assignments: vec![], unpin: true, unsettle: false, unsnooze: false }
        );
    }

    #[test]
    fn pinning_from_active_assigns_a_key_between_neighbors_and_settle_needs_a_config() {
        let pinned_keys = keys(&[("p1", Some("m")), ("a1", None)]);
        let empty = HashMap::new();
        let pin_target = DropTarget { section: Section::Pinned, pinned_order: strings(&["a1", "p1"]), active_order: vec![] };
        let pinned_order = strings(&["p1"]);
        let input = DropPlanInput {
            active_key: "a1",
            active_section: Section::Active,
            active_pinned: false,
            active_settled: false,
            supports_settlement: true,
            target: &pin_target,
            pinned_order: &pinned_order,
            pinned_keys_by_id: &pinned_keys,
            reorderable_keys: None,
            active_order: &[],
            active_keys_by_id: &empty,
            active_reorderable_keys: None,
            active_time_ordered: true,
        };
        let DropPlan::Pin { order_key: Some(key), extra_assignments, .. } = plan_thread_drop(&input) else { panic!("expected pin") };
        assert!(key.as_str() < "m");
        assert!(extra_assignments.is_empty());
        let settle = DropTarget { section: Section::Settled, pinned_order: vec![], active_order: vec![] };
        assert_eq!(plan_thread_drop(&DropPlanInput { target: &settle, ..input }), DropPlan::Settle);
        assert_eq!(plan_thread_drop(&DropPlanInput { target: &settle, supports_settlement: false, ..input }), DropPlan::None);
    }

    #[test]
    fn plan_pinned_reorder_keeps_hidden_slots_available_between_visible_neighbors() {
        let midpoint = pin_order_key_between(Some("f"), Some("t")).unwrap();
        let keys_by_id = keys(&[("a", Some("f")), ("b", Some("t")), ("moved", Some("z")), ("snoozed", Some(&midpoint))]);
        let assignments = plan_pinned_reorder(&strings(&["a", "moved", "b"]), &keys_by_id, "moved");
        assert_eq!(assignments.len(), 1);
        let key = &assignments[0].order_key;
        assert!(key.as_str() > "f" && key.as_str() < "t");
        assert_ne!(key, &midpoint);
        assert_eq!(assignments[0].id, "moved");
    }

    #[test]
    fn plan_pinned_reorder_materializes_keyless_rows_without_overwriting_hidden_slots() {
        let reserved = generate_spread_pin_order_keys(6);
        let mut keys_by_id = keys(&[("a", None), ("b", None), ("c", None)]);
        for (index, key) in reserved.iter().enumerate() {
            keys_by_id.insert(format!("hidden-{index}"), Some(key.clone()));
        }
        let assignments = plan_pinned_reorder(&strings(&["c", "a", "b"]), &keys_by_id, "c");
        assert_eq!(assignments.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["c", "a", "b"]);
        let keys: Vec<&String> = assignments.iter().map(|a| &a.order_key).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert_eq!(keys.iter().collect::<HashSet<_>>().len(), 3);
        assert!(keys.iter().all(|key| !reserved.contains(key)));
    }

    #[test]
    fn spread_keys_stay_unique_sorted_and_insertable() {
        for count in [0, 1, 650, 675, 676, 1_001, 2_000] {
            let keys = generate_spread_pin_order_keys(count);
            assert_eq!(keys.len(), count);
            assert_eq!(keys.iter().collect::<HashSet<_>>().len(), count);
            let mut sorted = keys.clone();
            sorted.sort();
            assert_eq!(sorted, keys);
            for index in 0..keys.len() {
                let before = index.checked_sub(1).map(|i| keys[i].as_str());
                let after = keys[index].as_str();
                assert!(after.bytes().all(|b| b.is_ascii_lowercase()) && !after.ends_with('a'));
                let between = pin_order_key_between(before, Some(after)).unwrap();
                assert!(between.as_str() < after);
                if let Some(before) = before {
                    assert!(between.as_str() > before);
                }
            }
        }
    }
}
