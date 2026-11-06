//! The sidebar model: sections, shelves and rows, derived once per shell change.
//! Ports `partitionSidebarThreads` and the list builder in `apps/web/src/components/Sidebar.tsx`
//! with the helpers in `apps/web/src/components/Sidebar.logic.ts`.

pub mod drag;
pub mod drafts;
pub mod favicon;
pub mod grouping;
pub mod history;
pub mod inbox;
pub mod older_settled;
pub mod selection;
pub mod settled;
pub mod sort;
pub mod status;
pub mod thread;
pub mod tag_filter;
pub mod time;
pub mod visits;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use drag::{DropItem, Marker};
use grouping::{GroupingSettings, build_project_groups};
use inbox::{InboxObservation, InboxReturnTracker};
use settled::effective_snoozed;
use thread::{SidebarThread, pending_subagent_requests, present_thread};
use time::Ms;

use crate::shell::{ShellSnapshot, ThreadShell};

pub const SETTLED_TAIL_INITIAL_COUNT: usize = 10;
pub const SETTLED_TAIL_PAGE_COUNT: usize = 25;

/// The native client talks to one environment.
pub const ENVIRONMENT_ID: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Pinned,
    Active,
    Working,
    Snoozed,
    Settled,
}

impl Section {
    /// Pinned, active and working rows are cards; shelves below them are slim.
    pub fn is_card(self) -> bool {
        matches!(self, Section::Pinned | Section::Active | Section::Working)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shelf {
    Working,
    Snoozed,
    Settled,
}

impl Shelf {
    pub fn label(self) -> &'static str {
        match self {
            Shelf::Working => "Working",
            Shelf::Snoozed => "Snoozed",
            Shelf::Settled => "Settled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarRow {
    Thread { index: usize, section: Section },
    Shelf { shelf: Shelf, count: usize, expanded: bool },
    ShowMore { count: usize },
    /// Drop boundaries and empty-section hints, present only while a thread is dragged.
    Marker(Marker),
}

/// Identity and height class of a row, for incremental list updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKey {
    Thread { id: String, card: bool, meta: bool },
    Shelf(Shelf),
    ShowMore,
    Marker(Marker),
}

/// `resolveSidebarThreadSection`.
pub fn resolve_section(snoozed: bool, settled: bool, pinned: bool) -> Section {
    if snoozed {
        Section::Snoozed
    } else if settled {
        Section::Settled
    } else if pinned {
        Section::Pinned
    } else {
        Section::Active
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarSettings {
    pub working_shelf_enabled: bool,
    pub grouping: GroupingSettings,
}

impl Default for SidebarSettings {
    fn default() -> Self {
        SidebarSettings { working_shelf_enabled: true, grouping: GroupingSettings::default() }
    }
}

/// Shelf toggles, collapsed by default as on the web, and the Settled tail length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfState {
    pub working_expanded: bool,
    pub snoozed_expanded: bool,
    pub settled_expanded: bool,
    pub settled_visible: usize,
}

impl Default for ShelfState {
    fn default() -> Self {
        ShelfState { working_expanded: false, snoozed_expanded: false, settled_expanded: false, settled_visible: SETTLED_TAIL_INITIAL_COUNT }
    }
}

impl ShelfState {
    pub fn toggle(&mut self, shelf: Shelf) {
        let expanded = match shelf {
            Shelf::Working => &mut self.working_expanded,
            Shelf::Snoozed => &mut self.snoozed_expanded,
            Shelf::Settled => &mut self.settled_expanded,
        };
        *expanded = !*expanded;
    }

    pub fn show_more_settled(&mut self) {
        self.settled_visible += SETTLED_TAIL_PAGE_COUNT;
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Partition {
    pub pinned: Vec<usize>,
    pub active: Vec<usize>,
    pub working: Vec<usize>,
    pub snoozed: Vec<usize>,
    pub settled: Vec<usize>,
}

pub struct PartitionInput<'a> {
    pub threads: &'a [SidebarThread],
    pub working_shelf_enabled: bool,
    /// Older servers can't settle or snooze; the web gates both on a loaded server config.
    pub config_loaded: bool,
    pub now: Ms,
    pub inbox: &'a InboxObservation,
}

/// `partitionSidebarThreads`, without drag projections. Threads must already be filtered.
pub fn partition(input: &PartitionInput) -> Partition {
    let mut buckets: [Vec<&SidebarThread>; 5] = Default::default();
    for thread in input.threads {
        let section = resolve_section(
            input.config_loaded && effective_snoozed(thread, input.now),
            input.config_loaded && thread.is_settled(),
            thread.pinned,
        );
        let section = match section {
            Section::Active if input.working_shelf_enabled && input.inbox.is_working(thread) => Section::Working,
            other => other,
        };
        buckets[section as usize].push(thread);
    }
    let [mut pinned, mut active, mut working, mut snoozed, mut settled] = buckets;
    sort::sort_pinned(&mut pinned);
    if input.working_shelf_enabled {
        sort::sort_inbox_by_return(&mut active, |thread| input.inbox.returned_at(thread));
    } else {
        sort::sort_active_by_order_key(&mut active);
    }
    sort::sort_working_by_send(&mut working);
    snoozed.sort_by_key(|thread| thread.snoozed_until.unwrap_or(0));
    sort::sort_settled(&mut settled);
    let index: HashMap<&str, usize> = input.threads.iter().enumerate().map(|(i, thread)| (thread.id.as_str(), i)).collect();
    let ids = |list: Vec<&SidebarThread>| list.into_iter().filter_map(|thread| index.get(thread.id.as_str()).copied()).collect();
    Partition { pinned: ids(pinned), active: ids(active), working: ids(working), snoozed: ids(snoozed), settled: ids(settled) }
}

/// The Settled shelf below the shell's cut, when the shell omits older settled threads.
#[derive(Debug, Clone, Copy)]
pub struct OlderSettledShelf<'a> {
    pub older: &'a older_settled::OlderSettled,
    /// Rows below it wait for the page that holds them (`isListedSettledThread`).
    pub frontier: Option<&'a older_settled::Cursor>,
}

pub struct ModelInput<'a> {
    pub snapshot: &'a ShellSnapshot,
    /// Rows on-demand reads hold that the shell omits (shelf pages, families); shell rows win.
    pub extra_threads: &'a [&'a ThreadShell],
    pub older_settled: Option<OlderSettledShelf<'a>>,
    pub settings: &'a SidebarSettings,
    pub shelves: &'a ShelfState,
    pub selected_thread_id: Option<&'a str>,
    /// Scoped project ids, or `None` for every project.
    pub project_scope: Option<&'a HashSet<String>>,
    pub hidden_thread_ids: Option<&'a HashSet<String>>,
    pub now: Ms,
    /// The section a thread is being dragged out of, which shows the drop markers.
    pub dragging: Option<Section>,
}

#[derive(Debug, Default, Clone)]
pub struct SidebarModel {
    pub threads: Vec<SidebarThread>,
    pub index_by_id: HashMap<String, usize>,
    /// Project id to the display name of its group.
    pub project_names: HashMap<String, String>,
    pub partition: Partition,
    pub rows: Vec<SidebarRow>,
    /// Thread indices in row order: thread navigation and `mod+1..9` read this.
    pub ordered: Vec<usize>,
    pub settled_count: usize,
    pub hidden_settled_count: usize,
    /// Settled rows the shelf may list; it reads older pages until this fills what it shows.
    pub listed_settled_count: usize,
    /// The next instant the partition can change without a shell update.
    pub next_change_at: Option<Ms>,
}

impl SidebarModel {
    pub fn build(input: &ModelInput, tracker: &mut InboxReturnTracker) -> SidebarModel {
        let snapshot = input.snapshot;
        let subagent_requests = pending_subagent_requests(&snapshot.threads);
        let rows: Vec<&ThreadShell> = snapshot
            .threads
            .iter()
            .chain(input.extra_threads.iter().copied())
            .filter(|thread| thread.archived_at.is_none() && thread.deleted_at.is_none() && thread.lineage.is_listed())
            .filter(|thread| input.hidden_thread_ids.is_none_or(|hidden| !hidden.contains(&thread.id)))
            .filter(|thread| input.project_scope.is_none_or(|scope| scope.contains(&thread.project_id)))
            .collect();
        let unloaded_settled = input
            .older_settled
            .map_or(0, |shelf| older_settled::unloaded_count(shelf.older, input.project_scope, &rows));
        let threads: Vec<SidebarThread> =
            rows.iter().map(|thread| present_thread(thread, subagent_requests.get(&thread.id).copied())).collect();
        let observation = if input.settings.working_shelf_enabled {
            tracker.observe(&threads, input.now)
        } else {
            tracker.clear();
            InboxObservation::default()
        };
        let partition = partition(&PartitionInput {
            threads: &threads,
            working_shelf_enabled: input.settings.working_shelf_enabled,
            config_loaded: true,
            now: input.now,
            inbox: &observation,
        });
        let groups = build_project_groups(ENVIRONMENT_ID, &snapshot.projects, &input.settings.grouping);
        let project_names = groups.iter().flat_map(|group| group.member_project_ids.iter().map(|id| (id.clone(), group.label.clone()))).collect();
        let index_by_id = threads.iter().enumerate().map(|(index, thread)| (thread.id.clone(), index)).collect();
        let next_change_at = threads
            .iter()
            .filter_map(|thread| thread.snoozed_until.filter(|wake| *wake > input.now))
            .chain(tracker.next_deadline())
            .min();
        let mut model = SidebarModel { threads, index_by_id, project_names, partition, next_change_at, ..Default::default() };
        let frontier = input.older_settled.and_then(|shelf| shelf.frontier);
        model.build_rows(input.shelves, input.selected_thread_id, frontier, unloaded_settled, input.dragging);
        model
    }

    fn build_rows(&mut self, shelves: &ShelfState, selected: Option<&str>, frontier: Option<&older_settled::Cursor>, unloaded_settled: usize, dragging: Option<Section>) {
        let selected = selected.and_then(|id| self.index_by_id.get(id).copied());
        let route_only = |list: &[usize]| -> Vec<usize> { list.iter().copied().filter(|index| Some(*index) == selected).collect() };
        let p = &self.partition;
        let mut rows = Vec::new();
        let mut ordered = Vec::new();
        // Older settled threads still to load count, so the Settled shelf outlives its loaded rows.
        let total = p.pinned.len() + p.active.len() + p.working.len() + p.snoozed.len() + p.settled.len() + unloaded_settled;
        if total == 0 {
            self.rows = rows;
            self.ordered = ordered;
            return;
        }
        let mut push_threads = |rows: &mut Vec<SidebarRow>, list: &[usize], section: Section| {
            for &index in list {
                rows.push(SidebarRow::Thread { index, section });
                ordered.push(index);
            }
        };
        let marker = |rows: &mut Vec<SidebarRow>, marker: Marker| {
            if dragging.is_some() {
                rows.push(SidebarRow::Marker(marker));
            }
        };
        marker(&mut rows, Marker::PinnedHeader);
        push_threads(&mut rows, &p.pinned, Section::Pinned);
        marker(&mut rows, Marker::PinnedDivider);
        if p.active.is_empty() {
            marker(&mut rows, Marker::ActivePlaceholder);
        }
        push_threads(&mut rows, &p.active, Section::Active);
        if !p.working.is_empty() {
            rows.push(SidebarRow::Shelf { shelf: Shelf::Working, count: p.working.len(), expanded: shelves.working_expanded });
            let visible = if shelves.working_expanded { p.working.clone() } else { route_only(&p.working) };
            push_threads(&mut rows, &visible, Section::Working);
        }
        if !p.snoozed.is_empty() {
            rows.push(SidebarRow::Shelf { shelf: Shelf::Snoozed, count: p.snoozed.len(), expanded: shelves.snoozed_expanded });
            let visible = if shelves.snoozed_expanded { p.snoozed.clone() } else { route_only(&p.snoozed) };
            push_threads(&mut rows, &visible, Section::Snoozed);
        }
        // Older settled threads count before they load, and list only as far as pages have loaded.
        let settled_count = p.settled.len() + unloaded_settled;
        rows.push(SidebarRow::Shelf { shelf: Shelf::Settled, count: settled_count, expanded: shelves.settled_expanded });
        let threads = &self.threads;
        let listed: Vec<usize> = p
            .settled
            .iter()
            .copied()
            .filter(|&index| older_settled::is_listed_settled(&threads[index].id, threads[index].settled_at, frontier))
            .collect();
        let mut visible_settled: Vec<usize> = listed.iter().copied().take(shelves.settled_visible).collect();
        if let Some(selected) = selected
            && !visible_settled.contains(&selected)
            && p.settled.contains(&selected)
        {
            visible_settled.push(selected);
        }
        let hidden_settled = settled_count.saturating_sub(visible_settled.len());
        let rendered = if shelves.settled_expanded { visible_settled } else { route_only(&visible_settled) };
        if rendered.is_empty() {
            marker(&mut rows, Marker::SettledPlaceholder);
        }
        push_threads(&mut rows, &rendered, Section::Settled);
        if shelves.settled_expanded && hidden_settled > 0 {
            rows.push(SidebarRow::ShowMore { count: hidden_settled.min(SETTLED_TAIL_PAGE_COUNT) });
        }
        self.rows = rows;
        self.ordered = ordered;
        self.settled_count = settled_count;
        self.hidden_settled_count = hidden_settled;
        self.listed_settled_count = listed.len();
    }

    pub fn thread(&self, id: &str) -> Option<&SidebarThread> {
        self.index_by_id.get(id).map(|index| &self.threads[*index])
    }

    pub fn row_key(&self, row: &SidebarRow) -> RowKey {
        match row {
            SidebarRow::Thread { index, section } => {
                let thread = &self.threads[*index];
                RowKey::Thread { id: thread.id.clone(), card: section.is_card(), meta: thread.branch.is_some() }
            }
            SidebarRow::Shelf { shelf, .. } => RowKey::Shelf(*shelf),
            SidebarRow::ShowMore { .. } => RowKey::ShowMore,
            SidebarRow::Marker(marker) => RowKey::Marker(*marker),
        }
    }

    /// The drag list (`sidebarListItems`): rows as drop items, without Show more.
    pub fn drop_items(&self) -> Vec<DropItem> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Thread { index, section } => Some(DropItem::Thread { id: self.threads[*index].id.clone(), section: *section }),
                SidebarRow::Shelf { shelf, .. } => Some(DropItem::Marker(match shelf {
                    Shelf::Working => Marker::WorkingHeader,
                    Shelf::Snoozed => Marker::SnoozedHeader,
                    Shelf::Settled => Marker::SettledHeader,
                })),
                SidebarRow::Marker(marker) => Some(DropItem::Marker(*marker)),
                SidebarRow::ShowMore { .. } => None,
            })
            .collect()
    }

    /// The insertion slot in `drop_items` for the pointer over `row`, in its lower half when
    /// `after`.
    pub fn drop_slot(&self, row: usize, after: bool) -> Option<usize> {
        let before = self.rows.get(..row)?.iter().filter(|row| !matches!(row, SidebarRow::ShowMore { .. })).count();
        let is_item = !matches!(self.rows.get(row)?, SidebarRow::ShowMore { .. });
        Some(before + usize::from(after && is_item))
    }

    pub fn row_keys(&self) -> Vec<RowKey> {
        self.rows.iter().map(|row| self.row_key(row)).collect()
    }

    pub fn section_of(&self, thread_id: &str) -> Option<Section> {
        let index = *self.index_by_id.get(thread_id)?;
        let p = &self.partition;
        [(&p.pinned, Section::Pinned), (&p.active, Section::Active), (&p.working, Section::Working), (&p.snoozed, Section::Snoozed), (&p.settled, Section::Settled)]
            .into_iter()
            .find_map(|(list, section)| list.contains(&index).then_some(section))
    }

    /// `resolveAdjacentThreadId` over the visible order.
    pub fn adjacent_thread(&self, current: Option<&str>, next: bool) -> Option<&str> {
        let ids: Vec<&str> = self.ordered.iter().map(|index| self.threads[*index].id.as_str()).collect();
        let Some(current) = current else {
            return if next { ids.first().copied() } else { ids.last().copied() };
        };
        let position = ids.iter().position(|id| *id == current)?;
        if next { ids.get(position + 1).copied() } else { position.checked_sub(1).and_then(|p| ids.get(p).copied()) }
    }

    pub fn jump_target(&self, index: usize) -> Option<&str> {
        self.ordered.get(index).map(|thread| self.threads[*thread].id.as_str())
    }
}

/// `getFallbackThreadIdAfterDelete`: candidates come from the environment, before sidebar filtering.
pub fn fallback_after_delete<'a>(
    threads: &'a [SidebarThread],
    deleted: &str,
    doomed: &HashSet<String>,
    order: sort::ThreadSortOrder,
    now: Ms,
) -> Option<&'a str> {
    let project = &threads
        .iter()
        .find(|thread| thread.id == deleted)?
        .project_id;
    let mut survivors: Vec<_> = threads
        .iter()
        .filter(|thread| {
            thread.project_id == *project
                && thread.id != deleted
                && !doomed.contains(&thread.id)
                && !thread.is_settled()
                && !effective_snoozed(thread, now)
        })
        .collect();
    sort::sort_threads(&mut survivors, order);
    survivors.first().map(|thread| thread.id.as_str())
}

/// The smallest splice that turns `old` into `new`: `(old range, new count)`, or `None` when equal.
pub fn splice_range<T: PartialEq>(old: &[T], new: &[T]) -> Option<(Range<usize>, usize)> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old.iter().rev().zip(new.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    Some((prefix..old.len() - suffix, new.len() - prefix - suffix))
}

/// `searchSidebarThreads`' local half: title, branch and project matches ranked as the palette
/// ranks them, without picks.
pub fn search_titles(model: &SidebarModel, query: &str, now: Ms) -> Vec<String> {
    use crate::search::{ThreadSearchOptions, ThreadSearchSubject, fold_search_query, rank_search_items, score_thread_search};
    let normalized = fold_search_query(query);
    if normalized.is_empty() {
        return Vec::new();
    }
    let subjects: Vec<(usize, ThreadSearchSubject)> = model
        .threads
        .iter()
        .enumerate()
        .map(|(index, thread)| {
            let activity_at = match thread.latest_user_authored_message_at {
                None => sort::thread_sort_timestamp(thread, sort::ThreadSortOrder::UpdatedAt),
                Some(authored) => authored.or(thread.created_at),
            };
            let subject = ThreadSearchSubject {
                id: Some(thread.id.clone()),
                title: thread.title.clone(),
                branch: thread.branch.clone(),
                project_title: model.project_names.get(&thread.project_id).cloned(),
                activity_at,
                opens: thread.opens.clone(),
                ..Default::default()
            };
            (index, subject)
        })
        .collect();
    rank_search_items(&subjects, |(_, subject), typos| score_thread_search(&normalized, subject, now, ThreadSearchOptions { typos, picks: false }))
        .into_iter()
        .map(|ranked| model.threads[ranked.item.0].id.clone())
        .collect()
}
