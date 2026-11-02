//! What one scope paints: the parsed patch, each file's rows kept separately so folding a file
//! rebuilds only that file (`useDiffPanelSessionStore` folds), the file tree, and gutter widths.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use t3_core::diff::{DiffRow, FileDiff, Layout, ParsedPatch, TreeRow, file_rows, file_tree_rows, parse_patch};

/// One fetched patch, parsed off the UI thread.
pub struct Parsed {
    pub patch: ParsedPatch,
    pub truncated: bool,
    /// Totals over every changed file when the patch holds only some of them.
    pub totals: Option<(u64, u64)>,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    /// The longest line in bytes, for horizontal scrolling without wrap.
    pub max_line_len: usize,
}

impl Parsed {
    pub fn new(patch: &str, truncated: bool, totals: Option<(u64, u64)>, base_ref: Option<String>, head_ref: Option<String>) -> Arc<Parsed> {
        Parsed::from_patch(parse_patch(patch), truncated, totals, base_ref, head_ref)
    }

    pub fn from_patch(patch: ParsedPatch, truncated: bool, totals: Option<(u64, u64)>, base_ref: Option<String>, head_ref: Option<String>) -> Arc<Parsed> {
        let max_line_len = match &patch {
            ParsedPatch::Files(files) => {
                files.iter().flat_map(|file| &file.hunks).flat_map(|hunk| &hunk.lines).map(|line| line.text.len()).max().unwrap_or(0)
            }
            _ => 0,
        };
        Arc::new(Parsed { patch, truncated, totals, base_ref, head_ref, max_line_len })
    }

    pub fn files(&self) -> &[FileDiff] {
        match &self.patch {
            ParsedPatch::Files(files) => files,
            _ => &[],
        }
    }
}

pub struct View {
    pub parsed: Arc<Parsed>,
    pub scope_key: String,
    pub layout: Layout,
    pub tree: Vec<TreeRow>,
    /// Line-number digits per file: each file sizes its own gutter, as Pierre does.
    pub digits: Vec<usize>,
    keys: Vec<String>,
    folded: Vec<bool>,
    folded_count: usize,
    rows: Vec<Vec<DiffRow>>,
    /// `starts[file]` is the file's first row; the last entry is the row count.
    starts: Vec<usize>,
}

impl View {
    /// `folds` are the scope's remembered folds; `None` folds every file (`diffFilesCollapsed`).
    pub fn new(parsed: Arc<Parsed>, scope_key: String, layout: Layout, folds: Option<&HashSet<String>>, tree_collapsed: &HashSet<String>) -> View {
        let files = parsed.files();
        let keys: Vec<String> = files.iter().map(FileDiff::key).collect();
        let folded: Vec<bool> = keys.iter().map(|key| folds.is_none_or(|folds| folds.contains(key))).collect();
        let rows: Vec<Vec<DiffRow>> =
            files.iter().enumerate().map(|(index, file)| file_rows(index, file, folded[index], layout)).collect();
        let mut view = View {
            tree: file_tree_rows(files, tree_collapsed),
            digits: files.iter().map(gutter_digits).collect(),
            folded_count: folded.iter().filter(|folded| **folded).count(),
            keys,
            folded,
            rows,
            starts: Vec::new(),
            parsed,
            scope_key,
            layout,
        };
        view.recount(0);
        view
    }

    fn recount(&mut self, from: usize) {
        self.starts.truncate(from);
        let mut start = if from == 0 { 0 } else { self.starts[from - 1] + self.rows[from - 1].len() };
        for rows in &self.rows[from..] {
            self.starts.push(start);
            start += rows.len();
        }
        self.starts.push(start);
    }

    pub fn row_count(&self) -> usize {
        self.starts.last().copied().unwrap_or(0)
    }

    pub fn row(&self, index: usize) -> Option<DiffRow> {
        if index >= self.row_count() {
            return None;
        }
        let file = self.starts.partition_point(|start| *start <= index).checked_sub(1)?;
        self.rows.get(file)?.get(index - self.starts[file]).copied()
    }

    pub fn header_row(&self, file: usize) -> Option<usize> {
        (file < self.rows.len()).then(|| self.starts[file])
    }

    pub fn key(&self, file: usize) -> Option<&str> {
        self.keys.get(file).map(String::as_str)
    }

    pub fn is_folded(&self, file: usize) -> bool {
        self.folded.get(file).copied().unwrap_or(false)
    }

    pub fn all_folded(&self) -> bool {
        !self.folded.is_empty() && self.folded_count == self.folded.len()
    }

    /// Folds or unfolds one file: returns the row range it held and how many rows replace it.
    pub fn set_folded(&mut self, file: usize, folded: bool) -> Option<(Range<usize>, usize)> {
        if self.is_folded(file) == folded {
            return None;
        }
        let diff = self.parsed.files().get(file)?;
        let next = file_rows(file, diff, folded, self.layout);
        let old = self.starts[file]..self.starts[file + 1];
        let count = next.len();
        self.rows[file] = next;
        self.folded[file] = folded;
        if folded {
            self.folded_count += 1;
        } else {
            self.folded_count -= 1;
        }
        self.recount(file + 1);
        Some((old, count))
    }

    pub fn set_all_folded(&mut self, folded: bool) {
        let files = self.parsed.files();
        self.rows = files.iter().enumerate().map(|(index, file)| file_rows(index, file, folded, self.layout)).collect();
        self.folded = vec![folded; files.len()];
        self.folded_count = if folded { files.len() } else { 0 };
        self.recount(0);
    }

    pub fn folded_keys(&self) -> HashSet<String> {
        self.keys.iter().zip(&self.folded).filter(|(_, folded)| **folded).map(|(key, _)| key.clone()).collect()
    }

    pub fn rebuild_tree(&mut self, tree_collapsed: &HashSet<String>) {
        self.tree = file_tree_rows(self.parsed.files(), tree_collapsed);
    }
}

/// Digits in the largest line number a file shows.
fn gutter_digits(file: &FileDiff) -> usize {
    let largest = file
        .hunks
        .iter()
        .map(|hunk| (hunk.old_start + hunk.old_count).max(hunk.new_start + hunk.new_count))
        .max()
        .unwrap_or(1);
    largest.max(1).ilog10() as usize + 1
}
