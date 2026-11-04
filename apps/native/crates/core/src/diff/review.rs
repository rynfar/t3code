//! Selected comparison and lazy file reads from `client-runtime/src/state/review.ts` and
//! `web/src/components/diffs/useReviewFilePatches.ts`. Call parsing methods off the UI thread.

use super::{
    ChangeKind, DiffPreviewSource, FileDiff, ParsedPatch, PreviewSourceKind, decode_diff_preview, diff_preview_request, parse_patch,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewScope {
    pub environment: String,
    pub connection: u64,
    pub cwd: String,
    pub kind: PreviewSourceKind,
    pub base_ref: Option<String>,
    pub ignore_whitespace: bool,
}

impl ReviewScope {
    pub fn request(&self, tracked_only: bool) -> (&'static str, Value) {
        let (method, mut payload) = diff_preview_request(&self.cwd, self.base_ref.as_deref(), self.ignore_whitespace);
        payload["sourceKind"] = json!(self.kind.wire());
        payload["metadataOnly"] = json!(true);
        if tracked_only {
            payload["trackedOnly"] = json!(true);
        }
        (method, payload)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReviewFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub additions: u64,
    pub deletions: u64,
}

impl ReviewFile {
    pub fn identity(&self) -> String {
        json!([self.previous_path.as_deref().unwrap_or(&self.path), self.path]).to_string()
    }

    pub fn placeholder(&self) -> FileDiff {
        FileDiff {
            old_path: Some(self.previous_path.clone().unwrap_or_else(|| self.path.clone())),
            new_path: Some(self.path.clone()),
            change: if self.previous_path.is_some() {
                ChangeKind::RenameChanged
            } else {
                ChangeKind::Modified
            },
            binary: false,
            hunks: Vec::new(),
            additions: self.additions as u32,
            deletions: self.deletions as u32,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FilePatchState {
    pub files: Option<Arc<Vec<FileDiff>>>,
    pub pending: bool,
    pub error: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchTicket {
    pub scope: ReviewScope,
    pub revision: u64,
    pub file: ReviewFile,
    pub cwd: String,
    pub base_ref: Option<String>,
}

impl PatchTicket {
    pub fn request(&self) -> (&'static str, Value) {
        let (method, mut input) = diff_preview_request(&self.cwd, self.base_ref.as_deref(), self.scope.ignore_whitespace);
        input["file"] = json!({"path": self.file.path, "previousPath": self.file.previous_path, "sourceKind": self.scope.kind.wire()});
        (method, input)
    }
}

#[derive(Debug, Clone)]
pub struct ReviewModel {
    pub scope: ReviewScope,
    pub source: Option<DiffPreviewSource>,
    pub incomplete: bool,
    pub repository: bool,
    pub revision: u64,
    pub states: HashMap<String, FilePatchState>,
    requested: HashSet<String>,
    legacy: ParsedPatch,
}

impl ReviewModel {
    pub fn new(scope: ReviewScope) -> Self {
        Self {
            scope,
            source: None,
            incomplete: false,
            repository: true,
            revision: 0,
            states: HashMap::new(),
            requested: HashSet::new(),
            legacy: ParsedPatch::Empty,
        }
    }

    pub fn has_complete_preview(&self) -> bool {
        self.source.is_some() && !self.incomplete
    }
    pub fn lazy(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|s| (s.patches_deferred || s.truncated) && !s.files.is_empty())
    }
    pub fn files(&self) -> &[ReviewFile] {
        self.source.as_ref().map_or(&[], |s| &s.files)
    }
    pub fn state(&self, file: &ReviewFile) -> Option<&FilePatchState> {
        self.states.get(&file.identity())
    }

    pub fn accept_preview(&mut self, value: &Value) {
        let source = decode_diff_preview(value, self.scope.kind);
        self.repository = super::preview_is_repository(value);
        self.incomplete = value["incomplete"].as_bool().unwrap_or(false);
        let same_refs = self
            .source
            .as_ref()
            .zip(source.as_ref())
            .is_some_and(|(a, b)| a.kind == b.kind && a.base_ref == b.base_ref && a.head_ref == b.head_ref);
        if !same_refs {
            self.requested.clear();
        }
        if self.source != source {
            self.revision += 1;
            self.states.clear();
        }
        self.source = source;
        let identities: HashSet<_> = self.files().iter().map(ReviewFile::identity).collect();
        self.requested.retain(|id| identities.contains(id));
        self.requested
            .extend(self.files().iter().take(4).map(ReviewFile::identity).collect::<Vec<_>>());
        self.legacy = if self.lazy() {
            ParsedPatch::Empty
        } else {
            self.source.as_ref().map_or(ParsedPatch::Empty, |s| parse_patch(&s.diff))
        };
    }

    pub fn request_file(&mut self, path: &str) -> bool {
        self.files()
            .iter()
            .find(|f| f.path == path)
            .map(ReviewFile::identity)
            .is_some_and(|id| self.requested.insert(id))
    }

    pub fn load_next(&mut self) -> bool {
        let start = self.files().iter().position(|file| self.state(file).is_none_or(|s| s.pending));
        let Some(start) = start else { return false };
        if self.requested.contains(&self.files()[start].identity()) {
            return false;
        }
        self.requested
            .extend(self.files()[start..].iter().take(4).map(ReviewFile::identity).collect::<Vec<_>>());
        true
    }

    pub fn retry(&mut self, path: &str) -> bool {
        if let Some(file) = self.files().iter().find(|f| f.path == path) {
            let id = file.identity();
            if self.states.get(&id).is_some_and(|s| s.error.is_some() && !s.pending) {
                self.states.remove(&id);
            }
            self.requested.insert(id);
            return true;
        }
        false
    }

    pub fn take_requests(&mut self) -> Vec<PatchTicket> {
        if !self.lazy() {
            return Vec::new();
        }
        let available = 4usize.saturating_sub(self.states.values().filter(|s| s.pending).count());
        let files: Vec<_> = self
            .files()
            .iter()
            .filter(|f| self.requested.contains(&f.identity()) && !self.states.contains_key(&f.identity()))
            .take(available)
            .cloned()
            .collect();
        files
            .into_iter()
            .map(|file| {
                self.states.insert(
                    file.identity(),
                    FilePatchState {
                        pending: true,
                        ..Default::default()
                    },
                );
                PatchTicket {
                    scope: self.scope.clone(),
                    revision: self.revision,
                    file,
                    cwd: self
                        .source
                        .as_ref()
                        .and_then(|s| s.cwd.clone())
                        .unwrap_or_else(|| self.scope.cwd.clone()),
                    base_ref: self
                        .source
                        .as_ref()
                        .and_then(|s| s.base_ref.clone())
                        .or_else(|| self.scope.base_ref.clone()),
                }
            })
            .collect()
    }

    pub fn accept_patch(&mut self, ticket: &PatchTicket, result: Result<DiffPreviewSource, String>) -> bool {
        if ticket.scope != self.scope
            || ticket.revision != self.revision
            || !self.files().iter().any(|f| f.identity() == ticket.file.identity())
        {
            return false;
        }
        let result = result.and_then(|source| {
            let ParsedPatch::Files(mut files) = parse_patch(&source.diff) else {
                return Err("Diff no longer available. Refresh the comparison.".into());
            };
            if files.len() == 1 && source.files.len() == 1 {
                let stat = &source.files[0];
                let file = &mut files[0];
                file.additions = stat.additions as u32;
                file.deletions = stat.deletions as u32;
                if file.change != ChangeKind::Deleted {
                    file.new_path = Some(stat.path.clone());
                }
                if file.change != ChangeKind::New {
                    file.old_path = Some(stat.previous_path.clone().unwrap_or_else(|| stat.path.clone()));
                }
            }
            files.retain(|file| file.path() == ticket.file.path);
            if files.is_empty() {
                return Err("Diff no longer available. Refresh the comparison.".into());
            }
            Ok((Arc::new(files), source.truncated))
        });
        let state = self.states.entry(ticket.file.identity()).or_default();
        state.pending = false;
        match result {
            Ok((files, truncated)) => {
                state.files = Some(files);
                state.truncated = truncated;
                state.error = None;
            }
            Err(error) => state.error = Some(error),
        }
        true
    }

    pub fn patch(&self) -> ParsedPatch {
        if !self.lazy() {
            return self.legacy.clone();
        }
        ParsedPatch::Files(
            self.files()
                .iter()
                .flat_map(|f| {
                    self.state(f)
                        .and_then(|s| s.files.as_deref())
                        .cloned()
                        .unwrap_or_else(|| vec![f.placeholder()])
                })
                .collect(),
        )
    }
}
