//! Per-thread drawer layout: which terminals a thread shows, how they are grouped into splits,
//! which is active, whether the drawer is open, and its height. Ports
//! `apps/web/src/terminalUiStateStore.ts` and the reconcile rules of
//! `components/PersistentThreadTerminals.tsx`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{DEFAULT_DRAWER_HEIGHT, DEFAULT_TERMINAL_ID, TerminalSummary};

pub const MAX_TERMINALS_PER_GROUP: usize = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    /// Side by side.
    #[default]
    Horizontal,
    /// Stacked.
    Vertical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalGroup {
    pub id: String,
    pub terminal_ids: Vec<String>,
    #[serde(default)]
    pub split_direction: SplitDirection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadTerminalLayout {
    pub terminal_open: bool,
    pub terminal_height: f32,
    pub terminal_ids: Vec<String>,
    pub active_terminal_id: String,
    pub terminal_groups: Vec<TerminalGroup>,
    pub active_terminal_group_id: String,
}

impl Default for ThreadTerminalLayout {
    fn default() -> Self {
        ThreadTerminalLayout {
            terminal_open: false,
            terminal_height: DEFAULT_DRAWER_HEIGHT,
            terminal_ids: Vec::new(),
            active_terminal_id: String::new(),
            terminal_groups: Vec::new(),
            active_terminal_group_id: String::new(),
        }
    }
}

fn normalize_ids(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty() && seen.insert(id.to_owned()))
        .map(str::to_owned)
        .collect()
}

fn fallback_group_id(terminal_id: &str) -> String {
    format!("group-{terminal_id}")
}

fn assign_unique_group_id(base: String, used: &mut HashSet<String>) -> String {
    let mut candidate = base.clone();
    let mut index = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}-{index}");
        index += 1;
    }
    used.insert(candidate.clone());
    candidate
}

fn normalize_groups(groups: &[TerminalGroup], ids: &[String]) -> Vec<TerminalGroup> {
    if ids.is_empty() {
        return Vec::new();
    }
    let valid: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut assigned: HashSet<String> = HashSet::new();
    let mut used = HashSet::new();
    let mut next = Vec::new();
    for group in groups {
        let members: Vec<String> = normalize_ids(&group.terminal_ids)
            .into_iter()
            .filter(|id| valid.contains(id.as_str()) && !assigned.contains(id))
            .collect();
        if members.is_empty() {
            continue;
        }
        assigned.extend(members.iter().cloned());
        let base = match group.id.trim() {
            "" => fallback_group_id(&members[0]),
            id => id.to_owned(),
        };
        next.push(TerminalGroup {
            id: assign_unique_group_id(base, &mut used),
            terminal_ids: members,
            split_direction: group.split_direction,
        });
    }
    for id in ids {
        if !assigned.contains(id) {
            next.push(TerminalGroup {
                id: assign_unique_group_id(fallback_group_id(id), &mut used),
                terminal_ids: vec![id.clone()],
                split_direction: SplitDirection::Horizontal,
            });
        }
    }
    next
}

enum Placement {
    New,
    Split(SplitDirection),
}

impl ThreadTerminalLayout {
    pub fn normalized(&self) -> Self {
        let terminal_ids = normalize_ids(&self.terminal_ids);
        let active_terminal_id = if terminal_ids.contains(&self.active_terminal_id) {
            self.active_terminal_id.clone()
        } else {
            terminal_ids.first().cloned().unwrap_or_default()
        };
        let terminal_groups = normalize_groups(&self.terminal_groups, &terminal_ids);
        let active_terminal_group_id = terminal_groups
            .iter()
            .find(|group| group.id == self.active_terminal_group_id)
            .or_else(|| terminal_groups.iter().find(|group| group.terminal_ids.contains(&active_terminal_id)))
            .or(terminal_groups.first())
            .map(|group| group.id.clone())
            .unwrap_or_default();
        let terminal_height = if self.terminal_height.is_finite() && self.terminal_height > 0. {
            self.terminal_height
        } else {
            DEFAULT_DRAWER_HEIGHT
        };
        ThreadTerminalLayout {
            terminal_open: self.terminal_open,
            terminal_height,
            terminal_ids,
            active_terminal_id,
            terminal_groups,
            active_terminal_group_id,
        }
    }

    pub fn is_default(&self) -> bool {
        self.normalized() == ThreadTerminalLayout::default()
    }

    fn upsert(&self, terminal_id: &str, placement: Placement) -> Self {
        let normalized = self.normalized();
        let terminal_id = terminal_id.trim();
        if terminal_id.is_empty() {
            return normalized;
        }
        let placement = if normalized.terminal_ids.is_empty() { Placement::New } else { placement };
        let is_new = !normalized.terminal_ids.iter().any(|id| id == terminal_id);
        let mut terminal_ids = normalized.terminal_ids.clone();
        if is_new {
            terminal_ids.push(terminal_id.to_owned());
        }
        let mut groups = normalized.terminal_groups.clone();
        if let Some(index) = groups.iter().position(|group| group.terminal_ids.iter().any(|id| id == terminal_id)) {
            groups[index].terminal_ids.retain(|id| id != terminal_id);
            if groups[index].terminal_ids.is_empty() {
                groups.remove(index);
            }
        }
        let direction = match placement {
            Placement::New => {
                let mut used: HashSet<String> = groups.iter().map(|group| group.id.clone()).collect();
                let group_id = assign_unique_group_id(fallback_group_id(terminal_id), &mut used);
                groups.push(TerminalGroup {
                    id: group_id.clone(),
                    terminal_ids: vec![terminal_id.to_owned()],
                    split_direction: SplitDirection::Horizontal,
                });
                return ThreadTerminalLayout {
                    terminal_open: true,
                    terminal_ids,
                    active_terminal_id: terminal_id.to_owned(),
                    terminal_groups: groups,
                    active_terminal_group_id: group_id,
                    ..normalized
                }
                .normalized();
            }
            Placement::Split(direction) => direction,
        };
        let active = &normalized.active_terminal_id;
        let mut index = groups
            .iter()
            .position(|group| group.id == normalized.active_terminal_group_id)
            .or_else(|| groups.iter().position(|group| group.terminal_ids.contains(active)));
        if index.is_none() {
            let mut used: HashSet<String> = groups.iter().map(|group| group.id.clone()).collect();
            groups.push(TerminalGroup {
                id: assign_unique_group_id(fallback_group_id(active), &mut used),
                terminal_ids: vec![active.clone()],
                split_direction: SplitDirection::Horizontal,
            });
            index = Some(groups.len() - 1);
        }
        let Some(index) = index else { return normalized };
        let destination = &mut groups[index];
        let contains = destination.terminal_ids.iter().any(|id| id == terminal_id);
        if is_new && !contains && destination.terminal_ids.len() >= MAX_TERMINALS_PER_GROUP {
            return normalized;
        }
        if !contains {
            match destination.terminal_ids.iter().position(|id| id == active) {
                Some(anchor) => destination.terminal_ids.insert(anchor + 1, terminal_id.to_owned()),
                None => destination.terminal_ids.push(terminal_id.to_owned()),
            }
        }
        destination.split_direction = direction;
        let active_terminal_group_id = destination.id.clone();
        ThreadTerminalLayout {
            terminal_open: true,
            terminal_ids,
            active_terminal_id: terminal_id.to_owned(),
            terminal_groups: groups,
            active_terminal_group_id,
            ..normalized
        }
        .normalized()
    }

    /// Opening a thread with no terminals adds `term-1` (`setThreadTerminalOpen`).
    pub fn with_open(&self, open: bool) -> Self {
        let normalized = self.normalized();
        if open && normalized.terminal_ids.is_empty() {
            return normalized.upsert(DEFAULT_TERMINAL_ID, Placement::New);
        }
        ThreadTerminalLayout { terminal_open: open, ..normalized }
    }

    pub fn with_height(&self, height: f32) -> Self {
        let normalized = self.normalized();
        if !height.is_finite() || height <= 0. {
            return normalized;
        }
        ThreadTerminalLayout { terminal_height: height, ..normalized }
    }

    pub fn with_split(&self, terminal_id: &str, direction: SplitDirection) -> Self {
        self.upsert(terminal_id, Placement::Split(direction))
    }

    pub fn with_new(&self, terminal_id: &str) -> Self {
        self.upsert(terminal_id, Placement::New)
    }

    pub fn with_active(&self, terminal_id: &str) -> Self {
        let normalized = self.normalized();
        if !normalized.terminal_ids.iter().any(|id| id == terminal_id) {
            return normalized;
        }
        let active_terminal_group_id = normalized
            .terminal_groups
            .iter()
            .find(|group| group.terminal_ids.iter().any(|id| id == terminal_id))
            .map(|group| group.id.clone())
            .unwrap_or_else(|| normalized.active_terminal_group_id.clone());
        ThreadTerminalLayout { active_terminal_id: terminal_id.to_owned(), active_terminal_group_id, ..normalized }
    }

    /// `ensureTerminal`: adds the terminal if missing; `active: None` behaves like `Some(true)`.
    pub fn with_ensured(&self, terminal_id: &str, open: bool, active: Option<bool>) -> Self {
        let mut next = self.clone();
        if !self.terminal_ids.iter().any(|id| id == terminal_id) {
            next = next.with_new(terminal_id);
        }
        if active == Some(false) {
            next.active_terminal_id = self.active_terminal_id.clone();
            next.active_terminal_group_id = self.active_terminal_group_id.clone();
        }
        if active.unwrap_or(true) {
            next = next.with_active(terminal_id);
        }
        if open {
            next = next.with_open(true);
        }
        next.normalized()
    }

    /// Closing the last terminal resets the thread, which also closes the drawer.
    pub fn with_closed(&self, terminal_id: &str) -> Self {
        let normalized = self.normalized();
        let Some(closed_index) = normalized.terminal_ids.iter().position(|id| id == terminal_id) else {
            return normalized;
        };
        let remaining: Vec<String> = normalized.terminal_ids.iter().filter(|id| *id != terminal_id).cloned().collect();
        if remaining.is_empty() {
            return ThreadTerminalLayout::default();
        }
        let group_ids: &[String] = normalized
            .terminal_groups
            .iter()
            .find(|group| group.terminal_ids.iter().any(|id| id == terminal_id))
            .map(|group| group.terminal_ids.as_slice())
            .unwrap_or(&[]);
        let next_active = if normalized.active_terminal_id == terminal_id {
            let in_group = group_ids.iter().position(|id| id == terminal_id);
            in_group
                .and_then(|index| group_ids.get(index + 1).or_else(|| index.checked_sub(1).and_then(|i| group_ids.get(i))))
                .or_else(|| remaining.get(closed_index.min(remaining.len() - 1)))
                .cloned()
                .unwrap_or_default()
        } else {
            normalized.active_terminal_id.clone()
        };
        let groups: Vec<TerminalGroup> = normalized
            .terminal_groups
            .iter()
            .filter_map(|group| {
                let ids: Vec<String> = group.terminal_ids.iter().filter(|id| *id != terminal_id).cloned().collect();
                (!ids.is_empty()).then(|| TerminalGroup { terminal_ids: ids, ..group.clone() })
            })
            .collect();
        let active_terminal_group_id = groups
            .iter()
            .find(|group| group.terminal_ids.contains(&next_active))
            .or(groups.first())
            .map(|group| group.id.clone())
            .unwrap_or_else(|| fallback_group_id(&next_active));
        ThreadTerminalLayout {
            terminal_open: normalized.terminal_open,
            terminal_height: normalized.terminal_height,
            terminal_ids: remaining,
            active_terminal_id: next_active,
            terminal_groups: groups,
            active_terminal_group_id,
        }
        .normalized()
    }

    /// `reconcileThreadTerminalSessionIds`: adopt the server's list, keeping groups that survive.
    pub fn with_reconciled(&self, next_ids: &[String]) -> Self {
        let normalized = self.normalized();
        if normalized.terminal_ids == next_ids {
            return normalized;
        }
        let next_active = if next_ids.contains(&normalized.active_terminal_id) {
            normalized.active_terminal_id.clone()
        } else {
            next_ids.first().cloned().unwrap_or_default()
        };
        let groups = normalize_groups(&normalized.terminal_groups, next_ids);
        let active_terminal_group_id = groups
            .iter()
            .find(|group| group.terminal_ids.contains(&next_active))
            .or(groups.first())
            .map(|group| group.id.clone())
            .unwrap_or_default();
        ThreadTerminalLayout {
            terminal_ids: next_ids.to_vec(),
            active_terminal_id: next_active,
            terminal_groups: groups,
            active_terminal_group_id,
            ..normalized
        }
        .normalized()
    }

    pub fn active_group(&self) -> Option<&TerminalGroup> {
        self.terminal_groups
            .iter()
            .find(|group| group.id == self.active_terminal_group_id)
            .or_else(|| self.terminal_groups.iter().find(|group| group.terminal_ids.contains(&self.active_terminal_id)))
            .or(self.terminal_groups.first())
    }

    /// The panes the drawer shows: the active group's terminals.
    pub fn visible_terminal_ids(&self) -> Vec<String> {
        match self.active_group() {
            Some(group) => group.terminal_ids.clone(),
            None if !self.active_terminal_id.is_empty() => vec![self.active_terminal_id.clone()],
            None => Vec::new(),
        }
    }

    pub fn split_direction(&self) -> SplitDirection {
        self.active_group().map(|group| group.split_direction).unwrap_or_default()
    }

    pub fn has_reached_split_limit(&self) -> bool {
        self.visible_terminal_ids().len() >= MAX_TERMINALS_PER_GROUP
    }

    /// Group headers appear once there is more than one group or any split.
    pub fn shows_group_headers(&self) -> bool {
        self.terminal_groups.len() > 1 || self.terminal_groups.iter().any(|group| group.terminal_ids.len() > 1)
    }
}

/// Every thread's layout plus the terminals a close is still removing, so a server list that
/// lags the close does not bring them back. Only the layouts are persisted.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerminalLayouts {
    by_thread: HashMap<String, ThreadTerminalLayout>,
    suppressed: HashMap<String, Vec<String>>,
}

impl TerminalLayouts {
    pub fn from_json(text: &str) -> Self {
        let by_thread: HashMap<String, ThreadTerminalLayout> = serde_json::from_str(text).unwrap_or_default();
        let by_thread = by_thread
            .into_iter()
            .filter(|(thread_id, _)| !thread_id.is_empty())
            .map(|(thread_id, layout)| (thread_id, layout.normalized()))
            .filter(|(_, layout)| !layout.is_default())
            .collect();
        TerminalLayouts { by_thread, suppressed: HashMap::new() }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.by_thread).unwrap_or_else(|_| "{}".to_owned())
    }

    pub fn get(&self, thread_id: &str) -> ThreadTerminalLayout {
        self.by_thread.get(thread_id).cloned().unwrap_or_default()
    }

    /// Applies `update` to one thread. Returns whether anything changed.
    fn update(
        &mut self,
        thread_id: &str,
        update: impl FnOnce(&ThreadTerminalLayout, &[String]) -> ThreadTerminalLayout,
        suppression: Option<(&str, bool)>,
    ) -> bool {
        if thread_id.is_empty() {
            return false;
        }
        let current = self.get(thread_id);
        let suppressed = self.suppressed.get(thread_id).cloned().unwrap_or_default();
        let next = update(&current, &suppressed);
        let mut changed = false;
        if next != current {
            changed = true;
            if next.is_default() {
                self.by_thread.remove(thread_id);
            } else {
                self.by_thread.insert(thread_id.to_owned(), next);
            }
        }
        if let Some((terminal_id, suppress)) = suppression {
            let terminal_id = terminal_id.trim();
            let ids = self.suppressed.entry(thread_id.to_owned()).or_default();
            let present = ids.iter().any(|id| id == terminal_id);
            if !terminal_id.is_empty() && present != suppress {
                changed = true;
                if suppress {
                    ids.push(terminal_id.to_owned());
                } else {
                    ids.retain(|id| id != terminal_id);
                }
            }
            if ids.is_empty() {
                self.suppressed.remove(thread_id);
            }
        }
        changed
    }

    pub fn set_open(&mut self, thread_id: &str, open: bool) -> bool {
        let opens_default = open && self.get(thread_id).terminal_ids.is_empty();
        self.update(thread_id, |layout, _| layout.with_open(open), opens_default.then_some((DEFAULT_TERMINAL_ID, false)))
    }

    pub fn set_height(&mut self, thread_id: &str, height: f32) -> bool {
        self.update(thread_id, |layout, _| layout.with_height(height), None)
    }

    pub fn split(&mut self, thread_id: &str, terminal_id: &str, direction: SplitDirection) -> bool {
        self.update(thread_id, |layout, _| layout.with_split(terminal_id, direction), Some((terminal_id, false)))
    }

    pub fn new_terminal(&mut self, thread_id: &str, terminal_id: &str) -> bool {
        self.update(thread_id, |layout, _| layout.with_new(terminal_id), Some((terminal_id, false)))
    }

    pub fn ensure(&mut self, thread_id: &str, terminal_id: &str, open: bool, active: Option<bool>) -> bool {
        self.update(thread_id, |layout, _| layout.with_ensured(terminal_id, open, active), Some((terminal_id, false)))
    }

    pub fn set_active(&mut self, thread_id: &str, terminal_id: &str) -> bool {
        self.update(thread_id, |layout, _| layout.with_active(terminal_id), None)
    }

    pub fn close(&mut self, thread_id: &str, terminal_id: &str) -> bool {
        self.update(thread_id, |layout, _| layout.with_closed(terminal_id), Some((terminal_id, true)))
    }

    pub fn reconcile(&mut self, thread_id: &str, server_ids: &[String]) -> bool {
        self.update(
            thread_id,
            |layout, suppressed| {
                let ids: Vec<String> = server_ids.iter().filter(|id| !suppressed.contains(id)).cloned().collect();
                layout.with_reconciled(&ids)
            },
            None,
        )
    }
}

/// Opening an empty drawer reuses a terminal the server already has for the thread, preferring
/// one with a running program, before starting a new shell.
pub fn pick_existing_terminal(server_terminals: &[&TerminalSummary]) -> Option<String> {
    server_terminals
        .iter()
        .find(|terminal| terminal.has_running_subprocess)
        .or(server_terminals.first())
        .map(|terminal| terminal.terminal_id.clone())
}

/// Whether the drawer should adopt the server's terminal list. A server list that is a strict
/// subset of the drawer's lags a `terminal.open`, and adopting it would drop new splits.
pub fn should_reconcile(server_ids: &[String], client_ids: &[String]) -> bool {
    let mut sorted_server = server_ids.to_vec();
    let mut sorted_client = client_ids.to_vec();
    sorted_server.sort();
    sorted_client.sort();
    if sorted_server == sorted_client {
        return false;
    }
    let strict_subset = server_ids.len() < client_ids.len()
        && !client_ids.is_empty()
        && server_ids.iter().all(|id| client_ids.contains(id));
    !strict_subset
}
