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
            hunks: Vec::new().into(),
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
    pub source: Option<Arc<DiffPreviewSource>>,
    pub incomplete: bool,
    pub repository: bool,
    pub revision: u64,
    pub states: HashMap<String, FilePatchState>,
    requested: Arc<HashSet<String>>,
    rendered: Arc<ParsedPatch>,
    render_version: u64,
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
            requested: Arc::new(HashSet::new()),
            rendered: Arc::new(ParsedPatch::Empty),
            render_version: 0,
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
        let source = decode_diff_preview(value, self.scope.kind).map(Arc::new);
        self.repository = super::preview_is_repository(value);
        self.incomplete = value["incomplete"].as_bool().unwrap_or(false);
        let same_refs = self
            .source
            .as_ref()
            .zip(source.as_ref())
            .is_some_and(|(a, b)| a.kind == b.kind && a.base_ref == b.base_ref && a.head_ref == b.head_ref);
        if !same_refs {
            Arc::make_mut(&mut self.requested).clear();
        }
        let changed = self.source != source;
        if changed {
            self.revision += 1;
            self.states.clear();
        }
        self.source = source;
        let identities: HashSet<_> = self.files().iter().map(ReviewFile::identity).collect();
        Arc::make_mut(&mut self.requested).retain(|id| identities.contains(id));
        let automatic = self.files().iter().take(4).map(ReviewFile::identity).collect::<Vec<_>>();
        Arc::make_mut(&mut self.requested).extend(automatic);
        if changed {
            self.rendered = Arc::new(if self.lazy() {
                ParsedPatch::Files(self.files().iter().map(ReviewFile::placeholder).collect())
            } else {
                self.source.as_ref().map_or(ParsedPatch::Empty, |s| parse_patch(&s.diff))
            });
            self.render_version += 1;
        }
    }

    pub fn request_file(&mut self, path: &str) -> bool {
        self.files()
            .iter()
            .find(|f| f.path == path)
            .map(ReviewFile::identity)
            .is_some_and(|id| Arc::make_mut(&mut self.requested).insert(id))
    }

    pub fn load_next(&mut self) -> bool {
        let start = self.files().iter().position(|file| self.state(file).is_none_or(|s| s.pending));
        let Some(start) = start else { return false };
        if self.requested.contains(&self.files()[start].identity()) {
            return false;
        }
        let next = self.files()[start..].iter().take(4).map(ReviewFile::identity).collect::<Vec<_>>();
        Arc::make_mut(&mut self.requested).extend(next);
        true
    }

    pub fn retry(&mut self, path: &str) -> bool {
        if let Some(file) = self.files().iter().find(|f| f.path == path).cloned() {
            let id = file.identity();
            if self.states.get(&id).is_some_and(|s| s.error.is_some() && !s.pending) {
                let had_patch = self.states.remove(&id).is_some_and(|s| s.files.is_some());
                if had_patch { self.replace_file(&file, &[file.placeholder()]); }
            }
            Arc::make_mut(&mut self.requested).insert(id);
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
                state.files = Some(files.clone());
                state.truncated = truncated;
                state.error = None;
                self.replace_file(&ticket.file, &files);
            }
            Err(error) => state.error = Some(error),
        }
        true
    }

    fn replace_file(&mut self, file: &ReviewFile, replacement: &[FileDiff]) {
        let ParsedPatch::Files(files) = Arc::make_mut(&mut self.rendered) else { return };
        let matches = |f: &&FileDiff| f.path() == file.path && f.previous_path() == file.previous_path.as_deref().unwrap_or(&file.path);
        let Some(start) = files.iter().position(|f| matches(&f)) else { return };
        let end = start + files[start..].iter().take_while(matches).count();
        files.splice(start..end, replacement.iter().cloned());
        self.render_version += 1;
    }

    pub fn render_version(&self) -> u64 { self.render_version }

    pub fn patch(&self) -> ParsedPatch { (*self.rendered).clone() }
}

pub fn review_boundary_near(header: usize, top: usize, measured_offset: Option<f32>, estimated_offset: f32, viewport_height: f32) -> bool {
    viewport_height > 0. && (header <= top || measured_offset.unwrap_or(estimated_offset) <= viewport_height + 600.)
}
