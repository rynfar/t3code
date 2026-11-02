//! Diff panel model. Ports the patch handling the web delegates to `@pierre/diffs`
//! (`apps/web/src/lib/diffRendering.ts`: `getRenderablePatch`, `getDiffLineStat`,
//! `resolveFileDiffPath`, `buildFileDiffIdentityKey`), the row layout Pierre's stacked and split
//! views paint, the file tree (`components/diffs/diffFileTree.logic.ts`), and the scope logic of
//! `apps/web/src/diffPanelStore.ts`, `client-runtime/src/state/threadCheckpoints.ts` and
//! `apps/web/src/state/queries.ts` (`useCheckpointDiff`).

mod parse;
mod rows;
mod scope;
mod tree;

use std::sync::Arc;

pub use parse::parse_patch;
pub use rows::{DiffRow, Layout, build_rows, separator_label};
pub use scope::{
    CheckpointFile, method, DiffPreviewSource, DiffScope, DiffSelection, FileTotals, PreviewSourceKind,
    TurnDiffSummary, checkpoint_diff_request, decode_diff_preview, decode_turn_diff,
    diff_preview_request, turn_diff_summaries,
};
pub use tree::{TreeRow, TreeRowKind, file_tree_rows};

/// Pierre's `FileDiffMetadata.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Modified,
    New,
    Deleted,
    RenamePure,
    RenameChanged,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Modified => "change",
            ChangeKind::New => "new",
            ChangeKind::Deleted => "deleted",
            ChangeKind::RenamePure => "rename-pure",
            ChangeKind::RenameChanged => "rename-changed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    /// Without the `+`/`-`/` ` marker, tabs expanded to Pierre's tab size.
    pub text: Arc<str>,
    pub no_newline_at_eof: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Text after the closing `@@`, usually the enclosing function.
    pub context: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileDiff {
    /// `None` for `/dev/null`.
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub change: ChangeKind,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
    pub additions: u32,
    pub deletions: u32,
}

impl FileDiff {
    /// `resolveFileDiffPath`: the new name, else the old one.
    pub fn path(&self) -> &str {
        self.new_path.as_deref().or(self.old_path.as_deref()).unwrap_or_default()
    }

    pub fn previous_path(&self) -> &str {
        self.old_path.as_deref().or(self.new_path.as_deref()).unwrap_or_default()
    }

    /// `buildFileDiffIdentityKey`: stable across refetches, distinct for delete/add pairs.
    pub fn key(&self) -> String {
        format!("{}\u{0}{}\u{0}{}", self.previous_path(), self.path(), self.change.as_str())
    }

    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|hunk| hunk.lines.len()).sum()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedPatch {
    /// Blank patch: no net changes.
    Empty,
    Files(Vec<FileDiff>),
    /// Text that is not a unified diff, shown as-is with the reason.
    Raw { text: String, reason: &'static str },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineStat {
    pub additions: u64,
    pub deletions: u64,
}

/// `getDiffLineStat`.
pub fn line_stat(files: &[FileDiff]) -> LineStat {
    files.iter().fold(LineStat::default(), |total, file| LineStat {
        additions: total.additions + u64::from(file.additions),
        deletions: total.deletions + u64::from(file.deletions),
    })
}

/// `formatCompactDiffCount` from `DiffStatLabel.tsx`.
pub fn compact_count(value: u64) -> String {
    let scaled = |value: f64, unit: &str| {
        if value < 10. {
            let text = format!("{value:.1}");
            format!("{}{unit}", text.strip_suffix(".0").unwrap_or(&text))
        } else {
            format!("{}{unit}", value.round())
        }
    };
    match value {
        0..1000 => value.to_string(),
        1000..999_500 => scaled(value as f64 / 1e3, "k"),
        999_500..999_500_000 => scaled(value as f64 / 1e6, "m"),
        _ => scaled(value as f64 / 1e9, "b"),
    }
}

#[cfg(test)]
mod tests;
