//! Scoped sidebar composition over the existing local partition, inbox and ordering rules.
//! Group/folder behavior ports `apps/web/src/components/Sidebar.logic.ts`.

use std::collections::{HashMap, HashSet};

use crate::server_config::ServerConfigState;
use crate::shell::{Lineage, ShellState, ThreadShell};
use super::grouping::{EnvironmentOwner, ProjectRef, ProjectPreferences, ScopedProject, ScopedProjectGroup};
use super::inbox::InboxReturnTracker;
use super::older_settled::{self, OlderSettled, Cursor};
use super::thread::{self, SidebarThread, SubagentRequest};
use super::{Partition, PartitionInput, Section, Shelf, ShelfState, SidebarSettings, sort};
use super::drag::Marker;
use super::time::Ms;
pub use super::tag_filter::ThreadKey;

#[derive(Debug, Clone, PartialEq)]
pub struct ScopedThread {
    pub owner: EnvironmentOwner,
    pub thread: SidebarThread,
    pub created_at: String,
    pub lineage: Lineage,
    pub delegated_from: Option<serde_json::Value>,
    pub tags: Vec<String>,
}

impl ScopedThread {
    pub fn from_shell(owner: &EnvironmentOwner, thread: &ThreadShell, pending: Option<SubagentRequest>) -> Self {
        Self { owner: owner.clone(), thread: thread::present_thread(thread, pending), created_at: thread.created_at.clone(), lineage: thread.lineage.clone(), delegated_from: thread.delegated_from.clone(), tags: thread.tags.clone() }
    }
    pub fn key(&self) -> ThreadKey { ThreadKey::new(&self.owner.saved_id, &self.thread.id) }
    pub fn project_ref(&self) -> ProjectRef { ProjectRef { environment_id: self.owner.saved_id.clone(), project_id: self.thread.project_id.clone() } }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentSummary {
    pub owner: EnvironmentOwner,
    pub projects: Vec<ScopedProject>,
    pub threads: Vec<ScopedThread>,
    pub synchronized: bool,
    pub snapshot_sequence: Option<u64>,
    pub older_settled: Option<crate::shell::OlderSettledCut>,
    pub removed_at: HashMap<String, u64>,
    pub older_count_key: Option<String>,
}

impl EnvironmentSummary {
    pub fn from_shell(mut owner: EnvironmentOwner, shell: &ShellState, config: &ServerConfigState) -> Self {
        owner.shell_generation = shell.generation;
        let config = config.config.as_ref();
        let label = config.map_or_else(|| owner.saved_id.clone(), |config| config.environment.label.clone());
        let scratch = config.and_then(|config| config.scratch_workspace_root.as_deref());
        let (projects, threads) = shell.snapshot.as_ref().map(|snapshot| {
            let pending = thread::pending_subagent_requests(&snapshot.threads);
            (snapshot.projects.iter().map(|project| ScopedProject { owner: owner.clone(), project: project.clone(), is_scratch: crate::settings::projects::is_scratch(&project.workspace_root, scratch), environment_label: label.clone() }).collect(),
             snapshot.threads.iter().chain(&snapshot.archived_threads).map(|thread| ScopedThread::from_shell(&owner, thread, pending.get(&thread.id).copied())).collect())
        }).unwrap_or_default();
        let older_settled = shell.snapshot.as_ref().and_then(|snapshot| snapshot.older_settled.clone());
        let older_count_key = shell.snapshot.as_ref().zip(older_settled.as_ref().and_then(OlderSettled::from_cut)).map(|(snapshot, older)| older_settled::count_key(&snapshot.threads, older.before_ms, &shell.removed_at));
        Self { owner, projects, threads, synchronized: shell.synchronized, snapshot_sequence: shell.sequence(), older_settled, removed_at: shell.removed_at.clone(), older_count_key }
    }
}

pub struct ScopedOlderSettled<'a> {
    pub owner: &'a EnvironmentOwner,
    pub older: &'a OlderSettled,
    pub frontier: Option<&'a Cursor>,
}

pub struct ScopedExtraThreads<'a> {
    pub owner: &'a EnvironmentOwner,
    pub threads: &'a [ScopedThread],
}

pub struct ScopedSidebarInput<'a> {
    pub environments: &'a [&'a EnvironmentSummary],
    pub groups: &'a [ScopedProjectGroup],
    pub preferences: &'a ProjectPreferences,
    pub settings: &'a SidebarSettings,
    pub shelves: &'a ShelfState,
    pub selected: Option<&'a ThreadKey>,
    pub hidden: &'a HashSet<ThreadKey>,
    pub older: &'a [ScopedOlderSettled<'a>],
    pub extra_threads: &'a [ScopedExtraThreads<'a>],
    pub now: Ms,
    pub folder_mode: bool,
    pub dragging: Option<Section>,
}

#[derive(Default)]
pub struct ScopedSidebarState {
    trackers: HashMap<EnvironmentOwner, InboxReturnTracker>,
}

#[derive(Debug, Clone)]
pub struct ScopedSidebarThread {
    pub key: ThreadKey,
    pub project: ProjectRef,
    pub owner: EnvironmentOwner,
    pub thread: SidebarThread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderDot { Attention, Failed, Unread, Working }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedSidebarRow {
    Thread { index: usize, section: Section },
    Shelf { shelf: Shelf, count: usize, expanded: bool },
    ShowMore { count: usize },
    Marker(Marker),
    Folder { key: String, expanded: bool, hidden_count: usize, dot: Option<FolderDot>, archived: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedRowKey {
    Thread { key: ThreadKey, card: bool, meta: bool },
    Shelf(Shelf), ShowMore, Marker(Marker), Folder(String),
}

#[derive(Debug, Default, Clone)]
pub struct ScopedSidebarModel {
    pub threads: Vec<ScopedSidebarThread>,
    pub index_by_key: HashMap<ThreadKey, usize>,
    pub project_names: HashMap<ProjectRef, String>,
    pub folder_groups: HashMap<String, ScopedProjectGroup>,
    pub partition: Partition,
    pub rows: Vec<ScopedSidebarRow>,
    pub ordered: Vec<usize>,
    pub settled_count: usize,
    pub hidden_settled_count: usize,
    pub listed_settled_count: usize,
    pub next_change_at: Option<Ms>,
}

fn folder_dot(threads: &[&SidebarThread]) -> Option<FolderDot> {
    use super::status::{ThreadStatus, resolve_status, has_unseen_completion};
    let statuses = threads.iter().map(|thread| resolve_status(thread)).collect::<Vec<_>>();
    if statuses.iter().any(|status| matches!(status, ThreadStatus::Input | ThreadStatus::Approval | ThreadStatus::Request | ThreadStatus::Limited)) { Some(FolderDot::Attention) }
    else if statuses.contains(&ThreadStatus::Failed) { Some(FolderDot::Failed) }
    else if threads.iter().any(|thread| resolve_status(thread) == ThreadStatus::Ready && has_unseen_completion(thread)) { Some(FolderDot::Unread) }
    else if statuses.iter().any(|status| matches!(status, ThreadStatus::Working | ThreadStatus::Preparing)) { Some(FolderDot::Working) }
    else { None }
}

impl ScopedSidebarModel {
    pub fn build(input: &ScopedSidebarInput<'_>, state: &mut ScopedSidebarState) -> Self {
        state.trackers.retain(|owner, _| input.environments.iter().any(|environment| environment.owner == *owner));
        let ready = input.environments.iter().all(|environment| environment.synchronized);
        let scope = input.preferences.scoped_members(input.groups, ready);
        let working_shelf_enabled = input.settings.working_shelf_enabled && !input.folder_mode;
        let mut model = Self::default();
        let mut returned = HashMap::new();
        let mut unloaded = 0;
        let mut frontier = HashMap::new();
        let eligible = |thread: &&ScopedThread| !thread.thread.archived && !thread.thread.deleted && thread.thread.listed
            && !input.hidden.contains(&thread.key()) && scope.as_ref().is_none_or(|scope| scope.contains(&thread.project_ref()));
        let candidates = input.environments.iter().map(|environment| {
            let mut seen = HashSet::new();
            let extra = input.extra_threads.iter().filter(|extra| *extra.owner == environment.owner).flat_map(|extra| extra.threads);
            environment.threads.iter().chain(extra).filter(|thread| thread.owner == environment.owner)
                .filter(|thread| seen.insert(thread.thread.id.clone())).filter(eligible).collect::<Vec<_>>()
        }).collect::<Vec<_>>();
        let mut aliases = HashMap::new();
        for (environment, threads) in input.environments.iter().zip(&candidates) {
            for thread in threads {
                let identity = (environment.owner.identity(), thread.thread.id.clone());
                let rank = (input.selected == Some(&thread.key()), environment.synchronized, thread.thread.updated_at);
                let replace = aliases.get(&identity).is_none_or(|(prior_rank, prior_key): &((bool, bool, Option<Ms>), ThreadKey)| rank > *prior_rank || (rank == *prior_rank && environment.owner.saved_id < prior_key.environment_id));
                if replace { aliases.insert(identity, (rank, thread.key())); }
            }
        }
        let mut loaded_projects = HashSet::new();
        for thread in candidates.iter().flatten() {
            if aliases.get(&(thread.owner.identity(), thread.thread.id.clone())).is_some_and(|(_, key)| *key == thread.key()) {
                loaded_projects.insert((thread.owner.identity(), thread.thread.project_id.clone(), thread.owner.saved_id.clone()));
            }
        }
        let mut count_owners = HashMap::new();
        for older in input.older.iter().filter(|older| input.environments.iter().any(|environment| environment.owner == *older.owner)) {
            for project in older.older.counts.keys() {
                let reference = ProjectRef { environment_id: older.owner.saved_id.clone(), project_id: project.clone() };
                if scope.as_ref().is_some_and(|scope| !scope.contains(&reference)) { continue; }
                let loaded = loaded_projects.contains(&(older.owner.identity(), project.clone(), older.owner.saved_id.clone()));
                let synchronized = input.environments.iter().any(|environment| environment.owner == *older.owner && environment.synchronized);
                let rank = (loaded, synchronized);
                let key = (older.owner.identity(), project.clone());
                let replace = count_owners.get(&key).is_none_or(|(prior_rank, saved): &((bool, bool), String)| rank > *prior_rank || (rank == *prior_rank && older.owner.saved_id < *saved));
                if replace { count_owners.insert(key, (rank, older.owner.saved_id.clone())); }
            }
        }
        let mut cuts = HashMap::new();
        for older in input.older {
            for project in older.older.counts.keys() {
                let identity = (older.owner.identity(), project.clone());
                if count_owners.get(&identity).is_some_and(|(_, saved)| *saved == older.owner.saved_id) {
                    cuts.insert(identity, (&older.owner.saved_id, older.older.before_ms));
                }
            }
        }
        let mut held_counts = HashMap::<String, usize>::new();
        for thread in candidates.iter().flatten() {
            if !aliases.get(&(thread.owner.identity(), thread.thread.id.clone())).is_some_and(|(_, key)| *key == thread.key()) { continue; }
            if let Some((saved, before)) = cuts.get(&(thread.owner.identity(), thread.thread.project_id.clone()))
                && thread.thread.is_settled() && thread.thread.settled_at.is_some_and(|at| at < *before) {
                *held_counts.entry((*saved).clone()).or_default() += 1;
            }
        }
        for (environment, candidates) in input.environments.iter().zip(candidates) {
            let scoped = candidates.into_iter().filter(|thread| aliases.get(&(environment.owner.identity(), thread.thread.id.clone())).is_some_and(|(_, key)| *key == thread.key())).collect::<Vec<_>>();
            let local = scoped.iter().map(|thread| thread.thread.clone()).collect::<Vec<_>>();
            let tracker = state.trackers.entry(environment.owner.clone()).or_default();
            let observation = if working_shelf_enabled { tracker.observe(&local, input.now) } else { tracker.clear(); Default::default() };
            let partition = super::partition(&PartitionInput { threads: &local, working_shelf_enabled, config_loaded: true, now: input.now, inbox: &observation });
            let offset = model.threads.len();
            for thread in scoped {
                let key = thread.key();
                returned.insert(key.clone(), observation.returned_at(&thread.thread));
                model.threads.push(ScopedSidebarThread { key, project: thread.project_ref(), owner: environment.owner.clone(), thread: thread.thread.clone() });
            }
            model.partition.pinned.extend(partition.pinned.iter().map(|index| offset + index));
            model.partition.active.extend(partition.active.iter().map(|index| offset + index));
            model.partition.working.extend(partition.working.iter().map(|index| offset + index));
            model.partition.snoozed.extend(partition.snoozed.iter().map(|index| offset + index));
            model.partition.settled.extend(partition.settled.iter().map(|index| offset + index));
            model.next_change_at = model.next_change_at.into_iter().chain(tracker.next_deadline()).min();
            if let Some(older) = input.older.iter().find(|older| *older.owner == environment.owner) {
                let counted = older.older.counts.iter().filter(|(project, _)| count_owners.get(&(environment.owner.identity(), (*project).clone())).is_some_and(|(_, saved)| *saved == environment.owner.saved_id)).map(|(_, count)| *count).sum::<u64>();
                let held = held_counts.get(&environment.owner.saved_id).copied().unwrap_or(0);
                unloaded += (counted as usize).saturating_sub(held);
                frontier.insert(environment.owner.saved_id.clone(), older.frontier);
            }
        }
        let threads = &model.threads;
        let sort_indexes = |indexes: &mut Vec<usize>, section: Section| {
            let mut rows = indexes.iter().map(|index| &threads[*index].thread).collect::<Vec<_>>();
            let by_pointer = indexes.iter().map(|index| (&threads[*index].thread as *const SidebarThread, *index)).collect::<HashMap<_, _>>();
            match section {
                Section::Pinned => sort::sort_pinned(&mut rows),
                Section::Active if input.folder_mode => sort::sort_working_by_send(&mut rows),
                Section::Active if working_shelf_enabled => sort::sort_inbox_by_return(&mut rows, |thread| returned[&threads[by_pointer[&(thread as *const SidebarThread)]].key]),
                Section::Active => sort::sort_active_by_order_key(&mut rows),
                Section::Working => sort::sort_working_by_send(&mut rows),
                Section::Snoozed => rows.sort_by_key(|thread| thread.snoozed_until.unwrap_or(0)),
                Section::Settled => sort::sort_settled(&mut rows),
            }
            *indexes = rows.iter().map(|thread| by_pointer[&(*thread as *const SidebarThread)]).collect();
        };
        sort_indexes(&mut model.partition.pinned, Section::Pinned);
        sort_indexes(&mut model.partition.active, Section::Active);
        sort_indexes(&mut model.partition.working, Section::Working);
        sort_indexes(&mut model.partition.snoozed, Section::Snoozed);
        sort_indexes(&mut model.partition.settled, Section::Settled);
        model.index_by_key = model.threads.iter().enumerate().map(|(index, thread)| (thread.key.clone(), index)).collect();
        model.project_names = input.groups.iter().flat_map(|group| group.member_refs.iter().map(|reference| (reference.clone(), group.label.clone()))).collect();
        model.next_change_at = model.next_change_at.into_iter().chain(model.threads.iter().filter_map(|thread| thread.thread.snoozed_until.filter(|wake| *wake > input.now))).min();
        let selected = input.selected.and_then(|key| model.index_by_key.get(key)).copied();
        let push = |model: &mut Self, indexes: &[usize], section| {
            for &index in indexes { model.rows.push(ScopedSidebarRow::Thread { index, section }); model.ordered.push(index); }
        };
        let marker = |model: &mut Self, marker| { if input.dragging.is_some() { model.rows.push(ScopedSidebarRow::Marker(marker)); } };
        let pinned = model.partition.pinned.clone();
        let active = model.partition.active.clone();
        marker(&mut model, Marker::PinnedHeader);
        push(&mut model, &pinned, Section::Pinned);
        marker(&mut model, Marker::PinnedDivider);
        if input.folder_mode {
            let resolved = input.preferences.resolved_scope(input.groups, ready);
            let scoped = !resolved.is_empty();
            for archived in [false, true] {
                if archived && scoped { continue; }
                for group in input.groups.iter().filter(|group| group.archived == archived && (!scoped || resolved.contains(&group.key))) {
                    let rows = active.iter().copied().filter(|index| group.member_refs.contains(&model.threads[*index].project)).collect::<Vec<_>>();
                    if rows.is_empty() && !(scoped || (group.pinned && !archived)) { continue; }
                    let expanded = input.preferences.expanded(group);
                    let hidden = rows.iter().copied().filter(|index| !expanded && Some(*index) != selected).collect::<Vec<_>>();
                    let dot = folder_dot(&hidden.iter().map(|index| &model.threads[*index].thread).collect::<Vec<_>>());
                    model.folder_groups.insert(group.key.clone(), group.clone());
                    model.rows.push(ScopedSidebarRow::Folder { key: group.key.clone(), expanded, hidden_count: hidden.len(), dot, archived });
                    let visible = rows.into_iter().filter(|index| expanded || Some(*index) == selected).collect::<Vec<_>>();
                    push(&mut model, &visible, Section::Active);
                }
            }
        } else {
            if active.is_empty() { marker(&mut model, Marker::ActivePlaceholder); }
            push(&mut model, &active, Section::Active);
        }
        for (shelf, section, indexes, expanded) in [
            (Shelf::Working, Section::Working, model.partition.working.clone(), input.shelves.working_expanded),
            (Shelf::Snoozed, Section::Snoozed, model.partition.snoozed.clone(), input.shelves.snoozed_expanded),
        ] {
            if indexes.is_empty() { continue; }
            model.rows.push(ScopedSidebarRow::Shelf { shelf, count: indexes.len(), expanded });
            push(&mut model, &indexes.into_iter().filter(|index| expanded || Some(*index) == selected).collect::<Vec<_>>(), section);
        }
        model.settled_count = model.partition.settled.len() + unloaded;
        if !model.threads.is_empty() || unloaded > 0 {
            let expanded = input.shelves.settled_expanded;
            model.rows.push(ScopedSidebarRow::Shelf { shelf: Shelf::Settled, count: model.settled_count, expanded });
            let listed = model.partition.settled.iter().copied().filter(|index| {
                let thread = &model.threads[*index];
                older_settled::is_listed_settled(&thread.thread.id, thread.thread.settled_at, frontier.get(&thread.owner.saved_id).copied().flatten())
            }).collect::<Vec<_>>();
            model.listed_settled_count = listed.len();
            let mut visible = listed.into_iter().take(input.shelves.settled_visible).collect::<Vec<_>>();
            if let Some(selected) = selected && model.partition.settled.contains(&selected) && !visible.contains(&selected) { visible.push(selected); }
            model.hidden_settled_count = model.settled_count.saturating_sub(visible.len());
            visible.retain(|index| expanded || Some(*index) == selected);
            if visible.is_empty() { marker(&mut model, Marker::SettledPlaceholder); }
            push(&mut model, &visible, Section::Settled);
            if expanded && model.hidden_settled_count > 0 { model.rows.push(ScopedSidebarRow::ShowMore { count: model.hidden_settled_count.min(super::SETTLED_TAIL_PAGE_COUNT) }); }
        }
        model
    }

    pub fn thread(&self, key: &ThreadKey) -> Option<&ScopedSidebarThread> { self.index_by_key.get(key).map(|index| &self.threads[*index]) }
    pub fn section_of(&self, key: &ThreadKey) -> Option<Section> {
        let index = self.index_by_key.get(key)?;
        [(&self.partition.pinned, Section::Pinned), (&self.partition.active, Section::Active), (&self.partition.working, Section::Working), (&self.partition.snoozed, Section::Snoozed), (&self.partition.settled, Section::Settled)].into_iter().find_map(|(list, section)| list.contains(index).then_some(section))
    }
    pub fn row_key(&self, row: &ScopedSidebarRow) -> ScopedRowKey {
        match row {
            ScopedSidebarRow::Thread { index, section } => ScopedRowKey::Thread { key: self.threads[*index].key.clone(), card: section.is_card(), meta: self.threads[*index].thread.branch.is_some() },
            ScopedSidebarRow::Shelf { shelf, .. } => ScopedRowKey::Shelf(*shelf),
            ScopedSidebarRow::ShowMore { .. } => ScopedRowKey::ShowMore,
            ScopedSidebarRow::Marker(marker) => ScopedRowKey::Marker(*marker),
            ScopedSidebarRow::Folder { key, .. } => ScopedRowKey::Folder(key.clone()),
        }
    }
    pub fn row_keys(&self) -> Vec<ScopedRowKey> { self.rows.iter().map(|row| self.row_key(row)).collect() }
    pub fn adjacent_thread(&self, current: Option<&ThreadKey>, next: bool) -> Option<ThreadKey> {
        let keys = self.ordered.iter().map(|index| &self.threads[*index].key).collect::<Vec<_>>();
        let Some(current) = current else { return if next { keys.first() } else { keys.last() }.map(|key| (*key).clone()) };
        let index = keys.iter().position(|key| *key == current)?;
        if next { keys.get(index + 1) } else { index.checked_sub(1).and_then(|index| keys.get(index)) }.map(|key| (*key).clone())
    }
    pub fn jump_target(&self, index: usize) -> Option<ThreadKey> { self.ordered.get(index).map(|index| self.threads[*index].key.clone()) }
}
