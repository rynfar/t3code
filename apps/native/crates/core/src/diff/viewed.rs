//! Revision-tracked `pullRequests.setFilesViewed` state (`pingdotgg/t3code#35114`), porting
//! `apps/web/src/components/pullRequest/pullRequestFilesViewed.logic.ts` and `filesViewedStore.ts`.
//!
//! Each toggle records the file's diff revision (`newObjectId`, omitted on single-commit diffs)
//! so a file ticked Viewed before loading a newer push still shows as `Changed` (`Stale`).

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

pub const SET_FILES_VIEWED_METHOD: &str = "pullRequests.setFilesViewed";
pub const FILES_VIEWED_FLUSH_DELAY_MS: u64 = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileViewedState {
    Unviewed,
    Viewed,
    /// Viewed at an older file revision than the PR's current head ("Changed" pill).
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileViewedPress {
    pub viewed: bool,
    pub revision: Option<String>,
}

/// Optimistic + debounced batch tracker for `pullRequests.setFilesViewed`.
#[derive(Debug, Clone, Default)]
pub struct FilesViewedTracker {
    server_states: HashMap<String, FileViewedState>,
    overlay: HashMap<String, bool>,
    queued: BTreeMap<String, FileViewedPress>,
    sent_by: HashMap<String, u64>,
    request_seq: u64,
}

impl FilesViewedTracker {
    pub fn set_server_states(&mut self, states: HashMap<String, FileViewedState>) {
        self.server_states = states;
    }

    pub fn state_for(&self, path: &str) -> FileViewedState {
        if let Some(&viewed) = self.overlay.get(path) {
            return if viewed { FileViewedState::Viewed } else { FileViewedState::Unviewed };
        }
        self.server_states.get(path).copied().unwrap_or(FileViewedState::Unviewed)
    }

    pub fn is_viewed(&self, path: &str) -> bool {
        self.state_for(path) == FileViewedState::Viewed
    }

    pub fn is_stale(&self, path: &str) -> bool {
        self.state_for(path) == FileViewedState::Stale
    }

    /// Records a press on `path` with the file revision shown in the diff (`None` for commit diffs).
    pub fn set_viewed(&mut self, path: &str, viewed: bool, revision: Option<String>) {
        let rev = revision.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty());
        self.queued.insert(path.to_owned(), FileViewedPress { viewed, revision: rev });
        self.overlay.insert(path.to_owned(), viewed);
    }

    pub fn has_queued(&self) -> bool {
        !self.queued.is_empty()
    }

    /// Drains queued presses into wire entries `[{ path, viewed, revision? }]` (`toFileViewedBatch`).
    pub fn take_batch(&mut self) -> Option<(u64, Vec<Value>)> {
        if self.queued.is_empty() {
            return None;
        }
        self.request_seq += 1;
        let seq = self.request_seq;
        let presses = std::mem::take(&mut self.queued);
        let mut batch = Vec::with_capacity(presses.len());
        for (path, press) in presses {
            self.sent_by.insert(path.clone(), seq);
            let entry = match press.revision {
                Some(revision) => json!({ "path": path, "viewed": press.viewed, "revision": revision }),
                None => json!({ "path": path, "viewed": press.viewed }),
            };
            batch.push(entry);
        }
        Some((seq, batch))
    }

    /// Settles an in-flight batch: commits optimistic entries to `server_states` on success, or
    /// reverts them on failure — leaving newer presses untouched (`revertFileViewedOverlay`).
    pub fn finish_batch(&mut self, seq: u64, ok: bool) {
        let settled: Vec<String> = self
            .sent_by
            .iter()
            .filter(|&(_, &sent_seq)| sent_seq == seq)
            .map(|(path, _)| path.clone())
            .collect();
        for path in settled {
            self.sent_by.remove(&path);
            if self.queued.contains_key(&path) {
                continue;
            }
            if let Some(viewed) = self.overlay.remove(&path)
                && ok
            {
                self.server_states.insert(
                    path,
                    if viewed { FileViewedState::Viewed } else { FileViewedState::Unviewed },
                );
            }
        }
    }
}

/// Shift-click range selection across visible file keys (`PullRequestCodeTab.tsx:825`).
pub fn viewed_range_indices(file_keys: &[String], clicked_key: &str, anchor_key: Option<&str>) -> Vec<usize> {
    let Some(clicked) = file_keys.iter().position(|key| key == clicked_key) else {
        return Vec::new();
    };
    let Some(anchor) = anchor_key.and_then(|anchor| file_keys.iter().position(|key| key == anchor)) else {
        return vec![clicked];
    };
    let (start, end) = if anchor <= clicked { (anchor, clicked) } else { (clicked, anchor) };
    (start..=end).collect()
}

/// Builds a `pullRequests.setFilesViewed` request payload.
pub fn set_files_viewed_payload(
    project_id: &str,
    number: u64,
    host: Option<&str>,
    repository: Option<&str>,
    files: Vec<Value>,
) -> (&'static str, Value) {
    let mut payload = json!({
        "projectId": project_id,
        "number": number,
        "files": files,
    });
    if let Some(host) = host {
        payload["host"] = json!(host);
    }
    if let Some(repository) = repository {
        payload["repository"] = json!(repository);
    }
    (SET_FILES_VIEWED_METHOD, payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_tracked_batch_omits_absent_revision_and_reverts_on_error() {
        let mut tracker = FilesViewedTracker::default();
        tracker.set_server_states(HashMap::from([("b.ts".to_owned(), FileViewedState::Stale)]));
        assert!(tracker.is_stale("b.ts"));

        tracker.set_viewed("a.ts", true, Some("shown-a".into()));
        tracker.set_viewed("b.ts", true, None);
        assert!(tracker.is_viewed("a.ts"));
        assert!(tracker.is_viewed("b.ts"));

        let (seq, batch) = tracker.take_batch().expect("batch");
        assert_eq!(
            batch,
            vec![
                json!({ "path": "a.ts", "viewed": true, "revision": "shown-a" }),
                json!({ "path": "b.ts", "viewed": true }),
            ]
        );
        // Simulate a failure: overlay reverts back to server state (including Stale).
        tracker.finish_batch(seq, false);
        assert_eq!(tracker.state_for("a.ts"), FileViewedState::Unviewed);
        assert_eq!(tracker.state_for("b.ts"), FileViewedState::Stale);
    }

    #[test]
    fn newer_press_survives_earlier_batch_settlement() {
        let mut tracker = FilesViewedTracker::default();
        tracker.set_viewed("a.ts", true, Some("rev1".into()));
        let (seq1, _) = tracker.take_batch().expect("batch 1");
        tracker.set_viewed("a.ts", false, Some("rev2".into()));
        tracker.finish_batch(seq1, true);
        assert_eq!(tracker.state_for("a.ts"), FileViewedState::Unviewed);
        let (seq2, batch2) = tracker.take_batch().expect("batch 2");
        assert_eq!(batch2, vec![json!({ "path": "a.ts", "viewed": false, "revision": "rev2" })]);
        tracker.finish_batch(seq2, true);
        assert_eq!(tracker.state_for("a.ts"), FileViewedState::Unviewed);
    }

    #[test]
    fn viewed_range_indices_handles_forward_and_reverse_shift_clicks() {
        let keys: Vec<String> = ["a", "b", "c", "d"].into_iter().map(str::to_owned).collect();
        assert_eq!(viewed_range_indices(&keys, "c", None), vec![2]);
        assert_eq!(viewed_range_indices(&keys, "d", Some("b")), vec![1, 2, 3]);
        assert_eq!(viewed_range_indices(&keys, "a", Some("c")), vec![0, 1, 2]);
    }
}
