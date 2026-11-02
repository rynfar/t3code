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
/// Returns true when the projection changed.
pub fn merge_older_history(projection: &mut ThreadProjection, older: Vec<ProjectedTurnItem>) -> bool {
    if older.is_empty() {
        return false;
    }
    let thread_id = projection.thread.id.clone();
    let local = |row: &ProjectedTurnItem| row.visibility == Visibility::Local || row.source_thread_id == thread_id;
    let current: HashMap<(&str, &str), &ProjectedTurnItem> =
        projection.visible_turn_items.iter().map(|row| (row_key(row), row)).collect();
    let known: HashMap<&str, &Arc<TurnItem>> =
        projection.turn_items.iter().map(|item| (item.id.as_str(), item)).collect();
    let mut page_keys: HashSet<(String, String)> = HashSet::new();
    let mut page: Vec<ProjectedTurnItem> = Vec::new();
    let mut appended: Vec<Arc<TurnItem>> = Vec::new();
    let mut appended_ids: HashSet<String> = HashSet::new();
    for page_row in &older {
        let key = (page_row.source_thread_id.clone(), page_row.source_item_id.clone());
        if page_keys.contains(&key) {
            continue;
        }
        let mut row = match current.get(&row_key(page_row)) {
            Some(existing) => (*existing).clone(),
            None => page_row.clone(),
        };
        if !current.contains_key(&row_key(page_row))
            && local(&row)
            && let Some(item) = known.get(row.source_item_id.as_str())
        {
            if !matches!(item.kind, TurnItemKind::RunInterruptRequest { .. }) || !is_item_visible(projection, item) {
                continue;
            }
            row.item = Arc::clone(item);
        }
        if local(&row) && !known.contains_key(row.source_item_id.as_str()) && appended_ids.insert(row.source_item_id.clone()) {
            appended.push(row.item.clone());
        }
        page_keys.insert(key);
        page.push(row);
    }
    if page.is_empty() {
        return false;
    }
    let unchanged = page.iter().all(|row| current.contains_key(&row_key(row)))
        && projection.visible_turn_items.iter().take(page.len()).zip(&page).all(|(a, b)| row_key(a) == row_key(b));
    if unchanged {
        return false;
    }
    let rest = std::mem::take(&mut projection.visible_turn_items)
        .into_iter()
        .filter(|row| !page_keys.contains(&(row.source_thread_id.clone(), row.source_item_id.clone())));
    let mut visible: Vec<ProjectedTurnItem> = page.into_iter().chain(rest).collect();
    for (position, row) in visible.iter_mut().enumerate() {
        row.position = position;
    }
    projection.visible_turn_items = visible;
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

impl ThreadState {
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

    /// Merges one fetched page requested with `request_cursor`.
    pub fn apply_history_page(&mut self, request_cursor: &str, page: HistoryPage) -> HistoryStep {
        let keep_paging = page.has_more_history && !page.has_user_turn;
        if keep_paging && page.next_cursor.as_deref().is_none_or(|next| next == request_cursor) {
            return self.fail_history_load(request_cursor, "History did not advance.".into());
        }
        if !self.history.is_active_request(request_cursor) {
            return HistoryStep::Stale;
        }
        let Some(projection) = self.projection.as_mut() else {
            self.history = ThreadHistoryMeta::default();
            return HistoryStep::Stale;
        };
        merge_older_history(projection, page.items);
        let next_cursor = page.next_cursor.clone();
        self.history = ThreadHistoryMeta {
            history_cursor: page.next_cursor,
            has_more_history: page.has_more_history,
            loading: keep_paging,
            error: None,
            expanded: true,
            latest_local_turn_ordinal: self.history.latest_local_turn_ordinal,
        };
        match next_cursor {
            Some(next) if keep_paging => HistoryStep::Continue(next),
            _ => HistoryStep::Done,
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
