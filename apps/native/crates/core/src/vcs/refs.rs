//! Ref snapshot revisions from `packages/client-runtime/src/state/vcs.ts`,
//! `vcsRefInvalidation.ts`, `git.ts`, `vcsAction.ts`, `sourceControl.ts`, and web
//! `state/queries.ts`. One cache belongs to one environment session owner.

use std::collections::{BTreeMap, HashMap};

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
            | method::GIT_RUN_STACKED_ACTION
            | "sourceControl.publishRepository"
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
    pub loading_next_page: bool,
    pub loaded: bool,
    pub error: Option<String>,
    cursors: Vec<Option<u64>>,
    request: u64,
}

#[derive(Debug, Clone)]
pub struct RefLoad {
    pub cursors: Vec<Option<u64>>,
    id: u64,
    append: bool,
}

impl RefListing {
    pub fn reset(&mut self) {
        *self = Self {
            request: self.request + 1,
            revision: self.revision + 1,
            ..Self::default()
        };
    }

    pub fn begin(&mut self, query: &str, next_page: bool) -> Option<RefLoad> {
        let cursors = if next_page {
            if self.query != query || self.loading {
                return None;
            }
            let cursor = Some(self.next_cursor?);
            if !self.cursors.contains(&cursor) {
                self.cursors.push(cursor);
            }
            vec![cursor]
        } else {
            if self.query != query {
                self.reset();
            }
            self.query = query.to_owned();
            if self.cursors.is_empty() {
                self.cursors.push(None);
            }
            self.cursors.clone()
        };
        self.request += 1;
        self.loading = true;
        self.loading_next_page = next_page;
        Some(RefLoad {
            id: self.request,
            cursors,
            append: next_page,
        })
    }

    pub fn finish(&mut self, request: &RefLoad, result: Result<Vec<super::ListRefsResult>, String>) -> bool {
        if request.id != self.request || !self.loading {
            return false;
        }
        self.loading = false;
        self.loading_next_page = false;
        self.loaded = true;
        match result {
            Ok(pages) => {
                if !request.append {
                    self.refs.clear();
                    self.cursors.clear();
                    self.total = 0;
                    self.next_cursor = None;
                }
                let mut positions = self.refs.iter().enumerate().map(|(index, item)| (item.name.clone(), index)).collect::<HashMap<_, _>>();
                for (page, cursor) in pages.into_iter().zip(&request.cursors) {
                    for item in page.refs {
                        if let Some(&index) = positions.get(&item.name) {
                            self.refs[index] = item;
                        } else {
                            positions.insert(item.name.clone(), self.refs.len());
                            self.refs.push(item);
                        }
                    }
                    if !self.cursors.contains(cursor) {
                        self.cursors.push(*cursor);
                    }
                    self.next_cursor = page.next_cursor;
                    self.total = self.total.max(page.total_count);
                }
                self.revision += 1;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
}

/// Combobox item values, retained by identity while mounted pages refresh (`BranchPicker.tsx`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefChoice {
    Ref(String),
    Create(String),
}

impl RefChoice {
    pub fn at(refs: &[super::VcsRef], indices: &[usize], create: Option<&str>, index: usize) -> Option<Self> {
        indices
            .get(index)
            .map(|&index| Self::Ref(refs[index].name.clone()))
            .or_else(|| create.filter(|_| index == indices.len()).map(|name| Self::Create(name.to_owned())))
    }

    pub fn index_in(&self, refs: &[super::VcsRef], indices: &[usize], create: Option<&str>) -> Option<usize> {
        match self {
            Self::Ref(name) => indices.iter().position(|&index| refs[index].name == *name),
            Self::Create(name) => (create == Some(name.as_str())).then_some(indices.len()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RefHighlight {
    pub choice: Option<RefChoice>,
    automatic: bool,
}

impl Default for RefHighlight {
    fn default() -> Self {
        Self { choice: None, automatic: true }
    }
}

impl RefHighlight {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn select(&mut self, choice: RefChoice) {
        self.choice = Some(choice);
        self.automatic = false;
    }

    pub fn update(&mut self, listing: &RefListing, indices: &[usize], create: Option<&str>, query: &str) {
        if self.automatic {
            self.choice = RefChoice::at(&listing.refs, indices, create, 0);
            let search_pending = listing.query != super::sanitize_new_ref_name(query) || !listing.loaded || listing.loading;
            self.automatic = search_pending || !matches!(self.choice.as_ref(), Some(RefChoice::Ref(_)));
        }
    }
}

#[cfg(test)]
mod tests;
