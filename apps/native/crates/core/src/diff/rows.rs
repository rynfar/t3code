//! The flat row list a virtualized diff view paints: Pierre's stacked (`unified`) and `split`
//! layouts with `hunkSeparators: "line-info"`. One row per visual line, so a view only lays out
//! what is on screen.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{FileDiff, Hunk, LineKind};

/// The `diffLayout` client setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    #[default]
    Stacked,
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    FileHeader { file: usize },
    /// The unchanged lines between hunks, labelled by `separator_label`.
    Separator { file: usize, hunk: usize, hidden: u32 },
    /// Stacked layout: one line of a hunk.
    Line { file: usize, hunk: usize, line: usize },
    /// Split layout: a deletion beside its addition, or a context line on both sides.
    Split { file: usize, hunk: usize, left: Option<usize>, right: Option<usize> },
    /// A binary file, whose patch carries no lines.
    Notice { file: usize },
}

impl DiffRow {
    pub fn file(&self) -> usize {
        match *self {
            DiffRow::FileHeader { file }
            | DiffRow::Separator { file, .. }
            | DiffRow::Line { file, .. }
            | DiffRow::Split { file, .. }
            | DiffRow::Notice { file } => file,
        }
    }
}

/// Pierre's `DiffHunksRenderer` copy.
pub fn separator_label(hidden: u32) -> String {
    format!("{hidden} unmodified line{}", if hidden == 1 { "" } else { "s" })
}

pub fn build_rows(files: &[FileDiff], collapsed: &HashSet<String>, layout: Layout) -> Vec<DiffRow> {
    let capacity = files.len() + files.iter().map(FileDiff::line_count).sum::<usize>();
    let mut rows = Vec::with_capacity(capacity);
    for (file_index, file) in files.iter().enumerate() {
        rows.push(DiffRow::FileHeader { file: file_index });
        if collapsed.contains(&file.key()) {
            continue;
        }
        if file.binary {
            rows.push(DiffRow::Notice { file: file_index });
            continue;
        }
        let mut previous_end: Option<u32> = None;
        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
            let hidden = hidden_before(hunk, previous_end);
            if hidden > 0 {
                rows.push(DiffRow::Separator { file: file_index, hunk: hunk_index, hidden });
            }
            previous_end = Some(hunk_end(hunk));
            match layout {
                Layout::Stacked => rows.extend(
                    (0..hunk.lines.len()).map(|line| DiffRow::Line { file: file_index, hunk: hunk_index, line }),
                ),
                Layout::Split => push_split(&mut rows, file_index, hunk_index, hunk),
            }
        }
    }
    rows
}

/// The new-side line after the hunk, or the old side for a deletion-only file.
fn hunk_end(hunk: &Hunk) -> u32 {
    if hunk.new_count == 0 && hunk.new_start == 0 {
        hunk.old_start + hunk.old_count
    } else {
        hunk.new_start + hunk.new_count
    }
}

fn hidden_before(hunk: &Hunk, previous_end: Option<u32>) -> u32 {
    let start = if hunk.new_count == 0 && hunk.new_start == 0 { hunk.old_start } else { hunk.new_start };
    match previous_end {
        Some(end) => start.saturating_sub(end),
        None => start.saturating_sub(1),
    }
}

/// Context lines sit on both sides; a run of deletions pairs, row by row, with the additions
/// that follow it.
fn push_split(rows: &mut Vec<DiffRow>, file: usize, hunk_index: usize, hunk: &Hunk) {
    let lines = &hunk.lines;
    let mut index = 0;
    while index < lines.len() {
        if lines[index].kind == LineKind::Context {
            rows.push(DiffRow::Split { file, hunk: hunk_index, left: Some(index), right: Some(index) });
            index += 1;
            continue;
        }
        let deletions_start = index;
        while index < lines.len() && lines[index].kind == LineKind::Deletion {
            index += 1;
        }
        let additions_start = index;
        while index < lines.len() && lines[index].kind == LineKind::Addition {
            index += 1;
        }
        let deletions = additions_start - deletions_start;
        let additions = index - additions_start;
        for offset in 0..deletions.max(additions) {
            rows.push(DiffRow::Split {
                file,
                hunk: hunk_index,
                left: (offset < deletions).then_some(deletions_start + offset),
                right: (offset < additions).then_some(additions_start + offset),
            });
        }
    }
}
