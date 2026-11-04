//! What one scope paints: the parsed patch, each file's rows kept separately so folding a file
//! rebuilds only that file (`useDiffPanelSessionStore` folds), the file tree, and gutter widths.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use t3_core::diff::{DiffRow, DiffSession, FileDiff, Layout, ParsedPatch, TreeRow, file_rows, file_tree_rows, parse_patch};

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
                files.iter().map(|file| file.hunks.max_line_len()).max().unwrap_or(0)
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
    pub files_collapsed_default: bool,
    pub tree: Vec<TreeRow>,
    /// Line-number digits per file: each file sizes its own gutter, as Pierre does.
    pub digits: Vec<usize>,
    keys: Vec<String>,
    folded: Vec<bool>,
    folded_count: usize,
    rows: Vec<Arc<Vec<DiffRow>>>,
    /// `starts[file]` is the file's first row; the last entry is the row count.
    starts: Vec<usize>,
}

impl View {
    pub fn new(parsed: Arc<Parsed>, scope_key: String, layout: Layout, session: &DiffSession, files_collapsed_default: bool, tree_collapsed: &HashSet<String>, review: bool) -> View {
        let files = parsed.files();
        let keys = file_keys(files, review);
        let folded = session.resolve_folds(&scope_key, &keys, files_collapsed_default);
        let rows = rows_for(files, &keys, &folded, layout, None);
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
            files_collapsed_default,
        };
        view.recount(0);
        view
    }

    pub fn update(&mut self, parsed: Arc<Parsed>, session: &DiffSession, tree_collapsed: &HashSet<String>, review: bool) -> Option<Vec<(Range<usize>, usize)>> {
        if Arc::ptr_eq(&self.parsed, &parsed) { return Some(Vec::new()); }
        let old = self.parsed.files();
        let files = parsed.files();
        let same_order = files.len() == old.len() && files.iter().zip(old).all(|(a,b)| a.path() == b.path() && a.previous_path() == b.previous_path() && (review || a.change == b.change));
        if !same_order {
            let keys = file_keys(files, review);
            let folded = session.resolve_folds(&self.scope_key, &keys, self.files_collapsed_default);
            self.rows = rows_for(files, &keys, &folded, self.layout, Some(self));
            self.tree = file_tree_rows(files, tree_collapsed);
            self.digits = files.iter().map(gutter_digits).collect();
            self.keys = keys;
            self.folded_count = folded.iter().filter(|folded| **folded).count();
            self.folded = folded;
            self.parsed = parsed;
            self.recount(0);
            return None;
        }
        let mut changes = Vec::new();
        for (i, (before, file)) in old.iter().zip(files).enumerate() {
            if !same_rows(before, file) {
                let rows = Arc::new(file_rows(0, file, self.folded[i], self.layout));
                changes.push((self.starts[i]..self.starts[i + 1], rows.len()));
                self.rows[i] = rows;
                self.digits[i] = gutter_digits(file);
            } else if before.change != file.change || before.additions != file.additions || before.deletions != file.deletions {
                changes.push((self.starts[i]..self.starts[i] + 1, 1));
            }
        }
        for row in &mut self.tree {
            if let t3_core::diff::TreeRowKind::File { file, change } = &mut row.kind { *change = files[*file].change; }
        }
        self.parsed = parsed;
        if !changes.is_empty() { self.recount(0); }
        Some(changes)
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
        self.rows.get(file)?.get(index - self.starts[file]).copied().map(|row| row.at_file(file))
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
        let next = Arc::new(file_rows(0, diff, folded, self.layout));
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
        self.rows = files.iter().map(|file| Arc::new(file_rows(0, file, folded, self.layout))).collect();
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

pub fn apply_row_changes(list: &gpui::ListState, changes: Vec<(Range<usize>, usize)>) {
    for (range, count) in changes.into_iter().rev() {
        if range.len() == count { list.remeasure_items(range); } else { list.splice(range, count); }
    }
}

fn file_keys(files: &[FileDiff], review: bool) -> Vec<String> {
    let mut occurrences = std::collections::HashMap::new();
    files.iter().map(|file| {
        if !review { return file.key(); }
        let path_key = serde_json::json!([file.previous_path(), file.path()]).to_string();
        let count = occurrences.entry(path_key.clone()).or_insert(0);
        let key = format!("{path_key}:{count}");
        *count += 1;
        key
    }).collect()
}

fn rows_for(files: &[FileDiff], keys: &[String], folded: &[bool], layout: Layout, previous: Option<&View>) -> Vec<Arc<Vec<DiffRow>>> {
    let cached: std::collections::HashMap<_, _> = previous.into_iter().flat_map(|view| view.keys.iter().enumerate().map(move |(i, key)| (key.as_str(), (view, i)))).collect();
    files.iter().enumerate().map(|(i, file)| {
        if let Some((view, old)) = cached.get(keys[i].as_str())
            && view.layout == layout && view.folded[*old] == folded[i]
            && same_rows(&view.parsed.files()[*old], file) {
            return view.rows[*old].clone();
        }
        Arc::new(file_rows(0, file, folded[i], layout))
    }).collect()
}

fn same_rows(before: &FileDiff, after: &FileDiff) -> bool {
    before.binary == after.binary && before.hunks.shares_with(&after.hunks)
}

fn gutter_digits(file: &FileDiff) -> usize { file.hunks.gutter_digits() }

#[cfg(test)]
mod tests {
    use super::*;
    use t3_core::diff::{ChangeKind, ReviewFile};

    #[test]
    fn lazy_file_keys_keep_folds_when_metadata_changes_type_and_inserts_a_path() {
        let stat = ReviewFile { path: "new.rs".into(), previous_path: Some("old.rs".into()), additions: 1, deletions: 0 };
        let placeholder = stat.placeholder();
        let parsed = |files| Parsed::from_patch(ParsedPatch::Files(files), false, None, None, None);
        let mut session = DiffSession::default();
        let initial = View::new(parsed(vec![placeholder.clone()]), "scope".into(), Layout::Stacked, &session, true, &HashSet::new(), true);
        let key = initial.key(0).unwrap().to_owned();
        session.set_file_fold("scope".into(), key.clone(), false, || initial.folded_keys());
        let loaded = FileDiff { change: ChangeKind::RenamePure, ..placeholder };
        let inserted = ReviewFile { path: "a.rs".into(), previous_path: None, additions: 1, deletions: 0 }.placeholder();
        let next = View::new(parsed(vec![inserted, loaded.clone()]), "scope".into(), Layout::Stacked, &session, true, &HashSet::new(), true);
        assert_eq!(next.key(1), Some(key.as_str()));
        assert!(!next.is_folded(1));
        let pair = View::new(parsed(vec![loaded.clone(), loaded]), "scope".into(), Layout::Stacked, &session, true, &HashSet::new(), true);
        assert_ne!(pair.key(0), pair.key(1), "same-path delete/add entries remain distinct");
    }
    #[test]
    fn lazy_updates_reuse_status_data_and_unaffected_file_rows() {
        let file = |index, text| {
            let ParsedPatch::Files(mut files) = parse_patch(&format!("--- a/f{index}.rs\n+++ b/f{index}.rs\n@@ -1 +1 @@\n-before\n+{text}\n")) else { panic!("files") };
            files.remove(0)
        };
        let files: Vec<_> = (0..12).map(|i| file(i, "after")).collect();
        let parsed = |files| Parsed::from_patch(ParsedPatch::Files(files), false, None, None, None);
        let session = DiffSession::default();
        let mut view = View::new(parsed(files.clone()), "scope".into(), Layout::Stacked, &session, false, &HashSet::new(), true);
        let pointers = |view: &View| view.rows.iter().map(|rows| rows.as_ptr()).collect::<Vec<_>>();
        let original = pointers(&view);
        view.update(view.parsed.clone(), &session, &HashSet::new(), true);
        assert_eq!(pointers(&view), original, "status-only updates must reuse every file's rows");
        let mut changed = files.clone();
        changed[5] = file(5, "changed");
        let changes = view.update(parsed(changed.clone()), &session, &HashSet::new(), true).expect("same file order");
        assert_eq!(changes.len(), 1);
        for (i, pointer) in pointers(&view).iter().enumerate() {
            assert_eq!(*pointer == original[i], i != 5, "only the settled file regenerates rows");
        }
        let previous = pointers(&view);
        changed.insert(0, file(99, "inserted"));
        view.update(parsed(changed), &session, &HashSet::new(), true);
        assert_eq!(&pointers(&view)[1..], previous.as_slice(), "metadata insertion reuses unchanged rows");
        for i in 0..13 { assert_eq!(view.row(view.header_row(i).unwrap()).unwrap().file(), i); }
        let list = gpui::ListState::new(200, gpui::ListAlignment::Top, gpui::px(0.));
        list.reset_with_uniform_height(200, gpui::px(32.));
        list.scroll_to(gpui::ListOffset { item_ix: 140, offset_in_item: gpui::px(0.) });
        let offset = list.offset_for_item(140);
        apply_row_changes(&list, vec![(0..1, 1), (1..2, 1)]);
        assert_eq!(list.offset_for_item(140), offset, "settling headers above the viewport keeps their height estimates");
        assert_eq!(list.logical_scroll_top().item_ix, 140);

    }

}
