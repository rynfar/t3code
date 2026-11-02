//! Bounded thread windows and paging earlier history. Ports
//! `client-runtime/src/state/threadHistoryMerge.ts` and the `loadEarlier` loop of
//! `client-runtime/src/state/threads.ts` as a pure state machine; `t3-client` fetches pages
//! (`threadHistoryHttp.ts`) and `AppState` drives the loop for its one open thread, which is all
//! `threadHistoryController.ts` routes. Collapsed work (`deferWork`, `view=work`) and
//! `throughEntryId` targeting are not requested, so their branches are not ported.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use crate::thread::{ApplyOptions, ProjectedTurnItem, ThreadProjection, ThreadState, Visibility, is_item_visible};
use crate::turn_item::{TurnItem, TurnItemKind};
use crate::wire;

/// `ThreadHistoryMeta`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadHistoryMeta {
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// Older pages were merged in. Stays true after the last page.
    pub expanded: bool,
    pub latest_local_turn_ordinal: Option<u64>,
}

impl ThreadHistoryMeta {
    /// An incomplete bounded window: events for rows outside it are dropped, not appended.
    pub fn is_partial(&self) -> bool {
        self.has_more_history || self.history_cursor.is_some()
    }

    pub fn apply_options(&self) -> ApplyOptions {
        if self.is_partial() {
            ApplyOptions { partial_timeline: true, latest_local_turn_ordinal: self.latest_local_turn_ordinal }
        } else {
            ApplyOptions::default()
        }
    }

    /// `shouldShowLoadEarlierControl`.
    pub fn should_show_load_earlier_control(&self) -> bool {
        self.has_more_history || self.error.is_some()
    }

    fn is_active_request(&self, cursor: &str) -> bool {
        self.history_cursor.as_deref() == Some(cursor)
    }
}

/// `OrchestrationV2ThreadHistoryPage`, decoded.
#[derive(Debug, Clone)]
pub struct HistoryPage {
    pub snapshot_sequence: u64,
    pub items: Vec<ProjectedTurnItem>,
    pub next_cursor: Option<String>,
    pub has_more_history: bool,
    /// The page holds a user-authored `user_message`; the loader pages on until one arrives.
    pub has_user_turn: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPage {
    snapshot_sequence: u64,
    items: Vec<ProjectedTurnItem>,
    next_cursor: Option<String>,
    has_more_history: bool,
}

/// Decodes a page response, expanding its ID table (records sit at the top level).
pub fn decode_history_page(mut value: Value) -> Result<HistoryPage, serde_json::Error> {
    if let Some(ids) = wire::take_id_table(&mut value)
        && let Value::Object(container) = &mut value
    {
        wire::expand_record_ids(container, &ids);
    }
    let has_user_turn = value.get("items").and_then(Value::as_array).is_some_and(|rows| {
        rows.iter().filter_map(|row| row.get("item")).any(|item| {
            item.get("type").and_then(Value::as_str) == Some("user_message")
                && item.get("createdBy").and_then(Value::as_str) == Some("user")
        })
    });
    let raw: RawPage = serde_json::from_value(value)?;
    Ok(HistoryPage {
        snapshot_sequence: raw.snapshot_sequence,
        items: raw.items.into_iter().filter(|row| row.item.is_known()).collect(),
        next_cursor: raw.next_cursor,
        has_more_history: raw.has_more_history,
        has_user_turn,
    })
}

fn row_key(row: &ProjectedTurnItem) -> (&str, &str) {
    (row.source_thread_id.as_str(), row.source_item_id.as_str())
}

/// `mergeOlderHistoryIntoProjection` without a work anchor: page rows go before the window,
/// rows the window already holds keep their current version, and a page row whose item the
/// projection holds but hides stays out (unless it is a visible interrupt request).
/// Indexes the page, not the transcript, so the transcript is scanned by borrowed key once
/// and never cloned. Returns true when the projection changed.
pub fn merge_older_history(projection: &mut ThreadProjection, older: Vec<ProjectedTurnItem>) -> bool {
    if older.is_empty() {
        return false;
    }
    let local = |row: &ProjectedTurnItem| row.visibility == Visibility::Local || row.source_thread_id == projection.thread.id;
    let mut first: HashMap<(&str, &str), usize> = HashMap::new();
    for (index, row) in older.iter().enumerate() {
        first.entry(row_key(row)).or_insert(index);
    }
    // Current rows that repeat a page key: the last one is its current version (`Map` semantics).
    let mut repeated: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, row) in projection.visible_turn_items.iter().enumerate() {
        if let Some(&page_index) = first.get(&row_key(row)) {
            repeated.entry(page_index).or_default().push(index);
        }
    }
    let page_item_ids: HashSet<&str> = older.iter().filter(|row| local(row)).map(|row| row.source_item_id.as_str()).collect();
    let mut known: HashMap<&str, &Arc<TurnItem>> = HashMap::new();
    for item in projection.turn_items.iter().filter(|item| page_item_ids.contains(item.id.as_str())) {
        known.insert(item.id.as_str(), item);
    }
    let mut page: Vec<ProjectedTurnItem> = Vec::new();
    let mut dropped: HashSet<usize> = HashSet::new();
    let mut appended: Vec<Arc<TurnItem>> = Vec::new();
    let mut appended_ids: HashSet<&str> = HashSet::new();
    for (index, page_row) in older.iter().enumerate() {
        if first.get(&row_key(page_row)) != Some(&index) {
            continue;
        }
        let current = repeated.get(&index);
        let mut row = match current.and_then(|rows| rows.last()) {
            Some(&existing) => projection.visible_turn_items[existing].clone(),
            None => page_row.clone(),
        };
        if current.is_none()
            && local(&row)
            && let Some(item) = known.get(row.source_item_id.as_str())
        {
            if !matches!(item.kind, TurnItemKind::RunInterruptRequest { .. }) || !is_item_visible(projection, item) {
                continue;
            }
            row.item = Arc::clone(item);
        }
        if local(&row) && !known.contains_key(page_row.source_item_id.as_str()) && appended_ids.insert(page_row.source_item_id.as_str()) {
            appended.push(row.item.clone());
        }
        dropped.extend(current.into_iter().flatten());
        page.push(row);
    }
    if page.is_empty() {
        return false;
    }
    let unchanged = dropped.len() == page.len() && (0..page.len()).all(|index| dropped.contains(&index))
        && projection.visible_turn_items.iter().zip(&page).all(|(a, b)| row_key(a) == row_key(b));
    if unchanged {
        return false;
    }
    let mut rest = std::mem::take(&mut projection.visible_turn_items);
    if !dropped.is_empty() {
        let mut index = 0;
        rest.retain(|_| {
            index += 1;
            !dropped.contains(&(index - 1))
        });
    }
    rest.splice(0..0, page);
    for (position, row) in rest.iter_mut().enumerate() {
        row.position = position;
    }
    projection.visible_turn_items = rest;
    projection.turn_items.extend(appended);
    true
}

/// What the page loader does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryStep {
    /// The page merged; fetch the next one from this cursor (inside one long agent turn).
    Continue(String),
    Done,
    /// A newer snapshot or load replaced this request; nothing changed.
    Stale,
    Failed(String),
}

/// A fetched page ready to merge off the UI thread: `ThreadState::begin_history_merge` checks
/// the projection out, `run` merges, and `ThreadState::finish_history_merge` swaps it back.
#[derive(Debug)]
pub struct HistoryMerge {
    projection: ThreadProjection,
    items: Vec<ProjectedTurnItem>,
    generation: u64,
    next: HistoryStep,
}

impl HistoryMerge {
    pub fn run(self) -> MergedHistory {
        let HistoryMerge { mut projection, items, generation, next } = self;
        merge_older_history(&mut projection, items);
        MergedHistory { projection, generation, next }
    }
}

#[derive(Debug)]
pub struct MergedHistory {
    projection: ThreadProjection,
    generation: u64,
    next: HistoryStep,
}

#[derive(Debug)]
pub enum PageOutcome {
    Merge(Box<HistoryMerge>),
    Step(HistoryStep),
}

/// What the transcript's history control shows, as `TimelineHistoryControl` in
/// `MessagesTimeline.tsx` does; `None` when there is no earlier history and no error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryControl {
    /// Offer the load button. Clicking it, or retrying after an error, is
    /// `AppState::load_older_history`.
    pub has_more: bool,
    /// The button is disabled and says it is loading, until the rows are in.
    pub loading: bool,
    /// The status line under the button.
    pub error: Option<String>,
}

impl ThreadState {
    /// A page still merging keeps the control up and loading until its rows are in.
    pub fn history_control(&self) -> Option<HistoryControl> {
        let merging = self.is_merging_history();
        (self.history.should_show_load_earlier_control() || merging).then(|| HistoryControl {
            has_more: self.history.has_more_history || merging,
            loading: self.history.loading || merging,
            error: self.history.error.clone(),
        })
    }

    /// Starts loading earlier history. Returns the cursor to fetch, or `None` when there is
    /// nothing to load or a load is already running.
    pub fn begin_history_load(&mut self) -> Option<String> {
        self.projection.as_ref()?;
        let cursor = self.history.history_cursor.clone()?;
        if !self.history.has_more_history || self.history.loading {
            return None;
        }
        self.history.loading = true;
        self.history.error = None;
        Some(cursor)
    }

    /// Accepts one fetched page requested with `request_cursor`. A page to merge checks the
    /// projection out; the history meta changes now, as the web's single update does.
    pub fn begin_history_merge(&mut self, request_cursor: &str, page: HistoryPage) -> PageOutcome {
        let keep_paging = page.has_more_history && !page.has_user_turn;
        if keep_paging && page.next_cursor.as_deref().is_none_or(|next| next == request_cursor) {
            return PageOutcome::Step(self.fail_history_load(request_cursor, "History did not advance.".into()));
        }
        if !self.history.is_active_request(request_cursor) {
            return PageOutcome::Step(HistoryStep::Stale);
        }
        let Some(projection) = self.projection.take() else {
            self.history = ThreadHistoryMeta::default();
            return PageOutcome::Step(HistoryStep::Stale);
        };
        let next = match &page.next_cursor {
            Some(next) if keep_paging => HistoryStep::Continue(next.clone()),
            _ => HistoryStep::Done,
        };
        self.history = ThreadHistoryMeta {
            history_cursor: page.next_cursor,
            has_more_history: page.has_more_history,
            loading: keep_paging,
            error: None,
            expanded: true,
            latest_local_turn_ordinal: self.history.latest_local_turn_ordinal,
        };
        self.merging = Some(Vec::new());
        PageOutcome::Merge(Box::new(HistoryMerge { projection, items: page.items, generation: self.generation, next }))
    }

    /// Swaps a merged projection back in and applies the events that arrived meanwhile.
    /// A snapshot or deletion since `begin_history_merge` wins: the merge is dropped.
    pub fn finish_history_merge(&mut self, merged: MergedHistory) -> HistoryStep {
        if merged.generation != self.generation {
            return HistoryStep::Stale;
        }
        let Some(pending) = self.merging.take() else { return HistoryStep::Stale };
        self.projection = Some(merged.projection);
        self.replay_pending(pending);
        merged.next
    }

    /// Merges a page on the calling thread: `begin_history_merge`, `run`, `finish_history_merge`.
    pub fn apply_history_page(&mut self, request_cursor: &str, page: HistoryPage) -> HistoryStep {
        match self.begin_history_merge(request_cursor, page) {
            PageOutcome::Merge(merge) => self.finish_history_merge(merge.run()),
            PageOutcome::Step(step) => step,
        }
    }

    /// Records a failed fetch for the request that is still current.
    pub fn fail_history_load(&mut self, request_cursor: &str, message: String) -> HistoryStep {
        if !self.history.is_active_request(request_cursor) {
            return HistoryStep::Stale;
        }
        self.history.loading = false;
        self.history.error = Some(message.clone());
        HistoryStep::Failed(message)
    }
}

#[cfg(test)]
mod tests;
