//! The older-settled shelf for shells subscribed with `omitOlderSettled`
//! (`client-runtime/src/state/olderSettledThreads.ts`), for one environment.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::time::{Ms, format_iso, parse_iso};
use crate::families::FamilyRead;
use crate::shell::{OlderSettledCut, ShellState, ThreadShell};

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

/// `olderSettledCountKey`: changes whenever the older count can (a removal, an older row that
/// arrives or changes, an old row that unsettles). Threads created after the cut never enter it.
pub fn count_key(threads: &[ThreadShell], before_ms: Ms, removed_at: &HashMap<String, u64>) -> String {
    let last_removal = removed_at.values().copied().max().unwrap_or(0);
    let mut rows: Vec<String> = threads
        .iter()
        .filter(|thread| thread.lineage.is_listed())
        .filter_map(|thread| {
            let older = is_older_settled(thread, before_ms);
            let old = parse_iso(&thread.created_at).is_some_and(|created| created < before_ms);
            (older || old).then(|| format!("{}:{}", thread.id, u8::from(older)))
        })
        .collect();
    rows.sort();
    format!("{last_removal}|{}", rows.join(","))
}

/// The count read `olderSettledThreads.ts` makes when the key moves: the counts alone.
pub fn count_request(before: &str) -> Value {
    json!({ "before": { "settledAt": before }, "limit": 0 })
}

pub const SEARCH_THREADS: &str = "orchestration.searchThreads";
/// `THREAD_SEARCH_QUERY_MAX_LENGTH`: a pasted wall of text searches by its start.
const SEARCH_QUERY_MAX_CHARS: usize = 200;

/// The server needs a word of three or more letters to match messages.
pub fn searches_messages(query: &str) -> bool {
    query.split(|c: char| !c.is_alphanumeric()).any(|word| word.chars().count() >= 3)
}

/// The sidebar's `searchThreads` payload (`threadSearch.ts`, `useThreadSearch`), or `None` when the
/// server has nothing to add. Below the shell's cut (`before`) the result also carries the rows of
/// the best older settled matches, whose titles match from two letters.
pub fn search_request(query: &str, before: Option<&str>, project_ids: Option<&[String]>) -> Option<Value> {
    let query: String = query.trim().chars().take(SEARCH_QUERY_MAX_CHARS).collect();
    let query = query.trim();
    let messages = searches_messages(query);
    if query.chars().count() < 2 || (!messages && before.is_none()) {
        return None;
    }
    let mut payload = json!({ "query": query });
    if let Some(before) = before {
        payload["olderSettledBefore"] = json!(before);
        if let Some(ids) = project_ids {
            payload["projectIds"] = json!(ids);
        }
        if !messages {
            payload["titlesOnly"] = json!(true);
        }
    }
    Some(payload)
}

/// The cut and its current counts: the snapshot's while nothing it counted has changed, then
/// the latest count read for the current key (`olderSettledAtom`).
#[derive(Debug, Clone, Default)]
pub struct OlderSettledCounts {
    generation: u64,
    from_snapshot: Option<OlderSettled>,
    snapshot_key: Option<String>,
    key: String,
    read: Option<(String, HashMap<String, u64>)>,
}

impl OlderSettledCounts {
    /// Follows the shell. Returns the key to read counts for, when the snapshot's no longer hold
    /// and no read for this key has landed.
    pub fn sync(&mut self, shell: &ShellState) -> Option<String> {
        let Some(snapshot) = &shell.snapshot else {
            *self = OlderSettledCounts::default();
            return None;
        };
        let Some(older) = snapshot.older_settled.as_ref().and_then(OlderSettled::from_cut) else {
            *self = OlderSettledCounts::default();
            return None;
        };
        let cut = snapshot.older_settled.as_ref().map_or(0, |cut| cut.as_of_sequence);
        self.key = count_key(&snapshot.threads, older.before_ms, &shell.removed_at);
        if self.generation != shell.generation || self.from_snapshot.as_ref().map(|o| &o.before) != Some(&older.before) {
            self.generation = shell.generation;
            self.snapshot_key = (snapshot.snapshot_sequence == cut).then(|| self.key.clone());
            self.read = None;
        }
        self.from_snapshot = Some(older);
        let stale = self.snapshot_key.as_deref() != Some(self.key.as_str());
        (stale && self.read.as_ref().is_none_or(|(key, _)| *key != self.key)).then(|| self.key.clone())
    }

    /// A count read for `key` landed.
    pub fn apply_read(&mut self, key: &str, counts: HashMap<String, u64>) {
        if key == self.key {
            self.read = Some((key.to_owned(), counts));
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// The cut with the newest counts, or `None` when the shell keeps every settled thread.
    pub fn current(&self) -> Option<OlderSettled> {
        let mut older = self.from_snapshot.clone()?;
        if self.snapshot_key.as_deref() != Some(self.key.as_str())
            && let Some((_, counts)) = &self.read
        {
            older.counts = counts.clone();
        }
        Some(older)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageStatus {
    #[default]
    Ready,
    Pending,
    Failed,
}

/// The pages read below the cut (`pagesAtom`): everything at or above `frontier` is local. A
/// `None` frontier means nothing more can load. Reads belong to one shell generation.
#[derive(Debug, Clone, Default)]
pub struct SettledPages {
    generation: u64,
    before: Option<String>,
    pub reads: Vec<FamilyRead>,
    pub frontier: Option<Cursor>,
    pub status: PageStatus,
    /// Pages the shelf wants; kept across generations, which read them again.
    pub requested: usize,
}

impl SettledPages {
    /// Follows the shell: a new generation or cut starts the reads over from the cut.
    pub fn sync(&mut self, shell: &ShellState, older: Option<&OlderSettled>) {
        let before = older.map(|older| older.before.clone());
        if self.generation == shell.generation && self.before == before {
            return;
        }
        self.generation = shell.generation;
        self.before = before;
        self.reads.clear();
        self.status = PageStatus::Ready;
        self.frontier = older.map(|older| Cursor { settled_at: older.before_ms, thread_id: None });
    }

    /// The next page to read, marking it pending: while ready, below the frontier, and short of
    /// the pages requested.
    pub fn next_request(&mut self) -> Option<(u64, Cursor)> {
        let frontier = self.frontier.clone()?;
        if self.status != PageStatus::Ready || self.reads.len() >= self.requested {
            return None;
        }
        self.status = PageStatus::Pending;
        Some((self.generation, frontier))
    }

    /// A page read from `before` for shell `generation`; stale answers are dropped. A short
    /// page ends the chain.
    pub fn apply_page(&mut self, generation: u64, before: &Cursor, page: Result<FamilyRead, String>) -> bool {
        if generation != self.generation || self.frontier.as_ref() != Some(before) || self.status != PageStatus::Pending {
            return false;
        }
        match page {
            Ok(page) => {
                self.frontier = next_cursor(&page.threads);
                self.reads.push(page);
                self.status = PageStatus::Ready;
            }
            Err(_) => self.status = PageStatus::Failed,
        }
        true
    }

    /// `retryPages`: reads a failed page again; the pages after it follow.
    pub fn retry(&mut self) {
        if self.status == PageStatus::Failed {
            self.status = PageStatus::Ready;
        }
    }

    /// `settledShelfFrontier` for one environment: where the shelf stops listing, or `None` to
    /// list every row (nothing more can load, or the read failed and holds no one back).
    pub fn listing_frontier(&self, older: Option<&OlderSettled>) -> Option<&Cursor> {
        if older.is_none() || self.status == PageStatus::Failed {
            return None;
        }
        self.frontier.as_ref()
    }

    /// `wantedOlderSettledPages` for one environment: one more page while the shelf lists fewer
    /// rows than it shows and its pages have loaded.
    pub fn grow(&mut self, older: Option<&OlderSettled>, listed: usize, visible: usize) -> bool {
        if listed >= visible || older.is_none() || self.status != PageStatus::Ready || self.frontier.is_none() {
            return false;
        }
        if self.requested > self.reads.len() {
            return false;
        }
        self.requested = self.reads.len() + 1;
        true
    }
}

#[cfg(test)]
mod tests;
