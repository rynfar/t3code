//! Find in thread: the request, result and navigation rules of `useThreadFind.ts`,
//! `threadFind.ts` and `threadFindStore.ts`. The server counts and orders matches over the whole
//! thread (`orchestration.searchThread`), including history not loaded yet; the client keeps a
//! window of nearby matches (`navigation`) so stepping through them needs no round trip.

use serde::Deserialize;
use serde_json::{Value, json};

pub const SEARCH_THREAD: &str = "orchestration.searchThread";
pub const MAX_QUERY_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindMatch {
    pub entry_id: String,
    pub run_id: Option<String>,
    pub occurrence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindNavigation {
    pub entry_id: String,
    pub run_id: Option<String>,
    pub start_index: u64,
    pub count: u64,
}

/// `OrchestrationV2SearchThreadResult`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindResult {
    #[serde(default)]
    pub complete: Option<bool>,
    pub total_matches: u64,
    pub active_index: u64,
    pub r#match: Option<FindMatch>,
    #[serde(default)]
    pub navigation: Vec<FindNavigation>,
}

impl FindResult {
    /// The server is still counting (`complete: false`); navigation waits.
    pub fn counting(&self) -> bool {
        self.complete == Some(false)
    }
}

/// Where a search starts: a match occurrence inside a timeline entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindStart {
    pub entry_id: String,
    pub occurrence: u64,
}

pub fn search_request(thread_id: &str, query: &str, start: Option<&FindStart>, offset: i64) -> Value {
    let mut input = json!({ "threadId": thread_id, "query": query.trim(), "skills": [] });
    if let Some(start) = start {
        input["start"] = json!({ "entryId": start.entry_id, "occurrence": start.occurrence });
    }
    if offset != 0 {
        input["offset"] = json!(offset);
    }
    input
}

pub fn decode_result(value: Value) -> Result<FindResult, serde_json::Error> {
    serde_json::from_value(value)
}

fn clamp_index(index: u64, total: u64) -> u64 {
    if total == 0 { 0 } else { index.min(total - 1) }
}

/// `stepThreadFindIndex`: wraps around both ends.
pub fn step_index(index: u64, total: u64, delta: i64) -> u64 {
    if total == 0 {
        return 0;
    }
    let total = total as i64;
    (clamp_index(index, total as u64) as i64 + delta).rem_euclid(total) as u64
}

/// `formatThreadFindCount`.
pub fn format_count(index: u64, total: u64) -> String {
    if total == 0 { "0/0".into() } else { format!("{}/{}", clamp_index(index, total) + 1, total) }
}

/// The selection after stepping through the result's navigation window without asking the
/// server, or `None` when the next match lies outside it.
pub fn step_locally(result: &FindResult, active_index: u64, delta: i64) -> Option<(u64, FindMatch)> {
    if result.total_matches == 0 || result.counting() {
        return None;
    }
    let next = step_index(active_index, result.total_matches, delta);
    let entry = result.navigation.iter().find(|entry| next >= entry.start_index && next < entry.start_index + entry.count)?;
    Some((next, FindMatch { entry_id: entry.entry_id.clone(), run_id: entry.run_id.clone(), occurrence: next - entry.start_index }))
}

/// The label beside the field: the count, "Searching…" or "Search failed".
pub fn bar_label(query: &str, result: Option<&FindResult>, active_index: u64, loading: bool, failed: bool) -> String {
    if failed {
        return "Search failed".into();
    }
    if loading {
        return "Searching…".into();
    }
    if query.trim().is_empty() {
        return String::new();
    }
    match result {
        Some(result) if result.counting() => "1/…".into(),
        Some(result) => format_count(active_index, result.total_matches),
        None => String::new(),
    }
}
