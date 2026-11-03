//! Ref snapshot revisions from `packages/client-runtime/src/state/vcs.ts`,
//! `vcsRefInvalidation.ts`, and `git.ts`. One cache belongs to one environment session owner.

use std::collections::BTreeMap;

use serde_json::Value;

use super::method;

pub fn invalidates_refs(method: &str) -> bool {
    matches!(
        method,
        method::VCS_PULL
            | method::VCS_REFRESH_STATUS
            | method::VCS_CREATE_REF
            | method::VCS_SWITCH_REF
            | method::VCS_INIT
            | "vcs.createWorktree"
            | "vcs.removeWorktree"
            | "git.preparePullRequestThread"
    )
}

#[derive(Debug, Default)]
pub struct RefCache {
    revision: u64,
    server_revisions: BTreeMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct RefRequest {
    pub input: Value,
    revision: u64,
}

impl RefRequest {
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

impl RefCache {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn settle(&mut self, method: &str) -> bool {
        if !invalidates_refs(method) {
            return false;
        }
        self.revision += 1;
        true
    }

    pub fn begin(&self, mut input: Value) -> RefRequest {
        let cwd = input["cwd"].as_str().unwrap_or_default();
        if self.revision > self.server_revisions.get(cwd).copied().unwrap_or(0) {
            input["refresh"] = Value::Bool(true);
        }
        RefRequest { input, revision: self.revision }
    }

    pub fn commit(&mut self, request: &RefRequest) -> bool {
        if request.revision != self.revision {
            return false;
        }
        if request.input["refresh"] == true {
            let cwd = request.input["cwd"].as_str().unwrap_or_default();
            // Eviction only causes another bypass; it never makes a stale snapshot readable.
            if !self.server_revisions.contains_key(cwd) && self.server_revisions.len() >= 256 {
                self.server_revisions.pop_first();
            }
            self.server_revisions.insert(cwd.to_owned(), request.revision);
        }
        true
    }
}

/// The picker listing and request identity, independent of UI task cancellation.
#[derive(Debug, Clone, Default)]
pub struct RefListing {
    pub query: String,
    pub refs: Vec<super::VcsRef>,
    pub revision: u64,
    pub next_cursor: Option<u64>,
    pub total: u64,
    pub loading: bool,
    pub loaded: bool,
    pub error: Option<String>,
    request: u64,
}

impl RefListing {
    pub fn reset(&mut self) {
        *self = Self {
            request: self.request + 1,
            revision: self.revision + 1,
            ..Self::default()
        };
    }

    pub fn begin(&mut self, query: &str, next_page: bool) -> Option<(u64, Option<u64>)> {
        let cursor = if next_page {
            if self.query != query || self.loading {
                return None;
            }
            Some(self.next_cursor?)
        } else {
            self.reset();
            self.query = query.to_owned();
            None
        };
        self.request += 1;
        self.loading = true;
        Some((self.request, cursor))
    }

    pub fn finish(&mut self, request: u64, result: Result<super::ListRefsResult, String>) -> bool {
        if request != self.request || !self.loading {
            return false;
        }
        self.loading = false;
        self.loaded = true;
        match result {
            Ok(page) => {
                for item in page.refs {
                    match self.refs.iter_mut().find(|existing| existing.name == item.name) {
                        Some(existing) => *existing = item,
                        None => self.refs.push(item),
                    }
                }
                self.revision += 1;
                self.next_cursor = page.next_cursor;
                self.total = self.total.max(page.total_count);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
}

#[cfg(test)]
mod tests;
