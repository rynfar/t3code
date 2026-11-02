//! The older-settled shelf for shells subscribed with `omitOlderSettled`
//! (`client-runtime/src/state/olderSettledThreads.ts`), for one environment.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::time::{Ms, format_iso, parse_iso};
use crate::shell::{OlderSettledCut, ThreadShell};

/// Rows per page: one "Show more" step of the Settled shelf.
pub const OLDER_SETTLED_PAGE_SIZE: usize = 25;
pub const GET_OLDER_SETTLED_THREADS: &str = "orchestration.getOlderSettledThreads";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OlderSettled {
    pub before: String,
    pub before_ms: Ms,
    pub counts: HashMap<String, u64>,
}

impl OlderSettled {
    pub fn from_cut(cut: &OlderSettledCut) -> Option<OlderSettled> {
        Some(OlderSettled {
            before: cut.before.clone(),
            before_ms: parse_iso(&cut.before)?,
            counts: cut.counts.iter().map(|count| (count.project_id.clone(), count.count)).collect(),
        })
    }
}

/// `OrchestrationV2OlderSettledCursor`: settle time desc, then id asc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub settled_at: Ms,
    pub thread_id: Option<String>,
}

impl Cursor {
    pub fn to_value(&self) -> Value {
        let mut value = json!({ "settledAt": format_iso(self.settled_at) });
        if let Some(id) = &self.thread_id {
            value["threadId"] = json!(id);
        }
        value
    }
}

/// `isOlderSettledThreadShell`.
pub fn is_older_settled(thread: &ThreadShell, before_ms: Ms) -> bool {
    thread.lineage.is_listed()
        && thread.settled_override.as_deref() == Some("settled")
        && thread.settled_at.as_deref().and_then(parse_iso).is_some_and(|at| at < before_ms)
}

/// Whether a row sits at or above a cursor in the server's order.
pub fn at_or_above_cursor(id: &str, settled_at: Option<Ms>, cursor: &Cursor) -> bool {
    let Some(settled_at) = settled_at else { return true };
    !(settled_at < cursor.settled_at || (cursor.thread_id.as_deref().is_some_and(|cursor_id| settled_at == cursor.settled_at && id > cursor_id)))
}

/// `isListedSettledThread`: the shelf lists rows at or above its frontier.
pub fn is_listed_settled(id: &str, settled_at: Option<Ms>, frontier: Option<&Cursor>) -> bool {
    frontier.is_none_or(|cursor| at_or_above_cursor(id, settled_at, cursor))
}

/// `unloadedOlderSettledCount` for one environment: server counts minus the older rows held.
pub fn unloaded_count(older: &OlderSettled, project_ids: Option<&HashSet<String>>, threads: &[&ThreadShell]) -> usize {
    let in_scope = |project_id: &str| project_ids.is_none_or(|ids| ids.contains(project_id));
    let counted: u64 = older.counts.iter().filter(|(project, _)| in_scope(project)).map(|(_, count)| count).sum();
    let held = threads.iter().filter(|thread| in_scope(&thread.project_id) && is_older_settled(thread, older.before_ms)).count() as u64;
    counted.saturating_sub(held) as usize
}

/// The `getOlderSettledThreads` payload for one page below `before`.
pub fn page_request(before: &Cursor, project_ids: Option<&[String]>) -> Value {
    let mut payload = json!({ "before": before.to_value(), "limit": OLDER_SETTLED_PAGE_SIZE });
    if let Some(ids) = project_ids {
        let mut ids = ids.to_vec();
        ids.sort();
        payload["projectIds"] = json!(ids);
    }
    payload
}

/// The next cursor after a page, or `None` when the chain ended (a short page).
pub fn next_cursor(page: &[ThreadShell]) -> Option<Cursor> {
    if page.len() < OLDER_SETTLED_PAGE_SIZE {
        return None;
    }
    let last = page.last()?;
    Some(Cursor { settled_at: parse_iso(last.settled_at.as_deref()?)?, thread_id: Some(last.id.clone()) })
}
