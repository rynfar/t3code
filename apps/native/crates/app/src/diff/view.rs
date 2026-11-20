//! What one scope paints: the parsed patch, each file's rows kept separately so folding a file
//! rebuilds only that file (`useDiffPanelSessionStore` folds), the file tree, and gutter widths.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use t3_core::diff::{DiffRow, DiffSession, FileDiff, Layout, ParsedPatch, TreeRow, file_rows_with_trailing, file_tree_rows_shown, parse_patch};

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
    /// Files "Hide test files" leaves out: no rows, no tree entry, still indexed (`isFileShown`).
    hidden: Vec<bool>,
    /// Unchanged lines below the last hunk, known once full file contents hydrate for expansion.
    trailing_hidden: Vec<u32>,
    rows: Vec<Arc<Vec<DiffRow>>>,
    /// `starts[file]` is the file's first row; the last entry is the row count.
    starts: Vec<usize>,
}

impl View {
    pub fn new(parsed: Arc<Parsed>, scope_key: String, layout: Layout, session: &DiffSession, files_collapsed_default: bool, tree_collapsed: &HashSet<String>, review: bool) -> View {
        let files = parsed.files();
        let keys = file_keys(files, review);
        let folded = session.resolve_folds(&scope_key, &keys, files_collapsed_default);
        let trailing_hidden = vec![0; files.len()];
        let rows = rows_for(files, &keys, &folded, &trailing_hidden, layout, None);
        let mut view = View {
            tree: file_tree_rows_shown(files, tree_collapsed, |_| true),
            digits: files.iter().map(gutter_digits).collect(),
            hidden: vec![false; files.len()],
            trailing_hidden,
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
            let cached: std::collections::HashMap<&str, usize> =
                self.keys.iter().enumerate().map(|(i, key)| (key.as_str(), i)).collect();
            let trailing_hidden: Vec<u32> = files
                .iter()
                .enumerate()
                .map(|(i, file)| {
                    cached
                        .get(keys[i].as_str())
                        .filter(|&&old_ix| same_rows(&old[old_ix], file))
                        .map_or(0, |&old_ix| self.trailing_hidden[old_ix])
                })
                .collect();
            self.rows = rows_for(files, &keys, &folded, &trailing_hidden, self.layout, Some(self));
            self.tree = file_tree_rows_shown(files, tree_collapsed, |_| true);
            self.digits = files.iter().map(gutter_digits).collect();
            self.keys = keys;
            self.hidden = vec![false; files.len()];
            self.trailing_hidden = trailing_hidden;
            self.folded = folded;
            self.parsed = parsed;
            self.recount(0);
            return None;
        }
        let mut changes = Vec::new();
        for (i, (before, file)) in old.iter().zip(files).enumerate() {
            if self.hidden[i] {
                if !same_rows(before, file) {
                    self.trailing_hidden[i] = 0;
                }
                continue;
            }
            if !same_rows(before, file) {
                self.trailing_hidden[i] = 0;
                let rows = Arc::new(file_rows_with_trailing(0, file, self.folded[i], self.layout, 0));
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

    pub fn file_rows_slice(&self, file: usize) -> Option<&[DiffRow]> {
        self.rows.get(file).map(|rows| rows.as_slice())
    }

    pub fn is_folded(&self, file: usize) -> bool {
        self.folded.get(file).copied().unwrap_or(false)
    }

    /// `areAllDiffFilesCollapsed(shownFileKeys, …)`.
    pub fn all_folded(&self) -> bool {
        let mut shown = self.folded.iter().zip(&self.hidden).filter(|(_, hidden)| !**hidden).peekable();
        shown.peek().is_some() && shown.all(|(folded, _)| *folded)
    }

    pub fn is_hidden(&self, file: usize) -> bool {
        self.hidden.get(file).copied().unwrap_or(false)
    }

    /// Applies "Hide test files": hidden files lose their rows and tree entries. Returns whether
    /// anything changed, so the caller resets the list.
    pub fn set_hidden(&mut self, hidden: Vec<bool>, tree_collapsed: &HashSet<String>) -> bool {
        if hidden.len() != self.hidden.len() || hidden == self.hidden {
            return false;
        }
        let files = self.parsed.files();
        for (i, file) in files.iter().enumerate() {
            if hidden[i] != self.hidden[i] {
                self.rows[i] = if hidden[i] {
                    Arc::new(Vec::new())
                } else {
                    Arc::new(file_rows_with_trailing(0, file, self.folded[i], self.layout, self.trailing_hidden[i]))
                };
            }
        }
        self.hidden = hidden;
        self.rebuild_tree(tree_collapsed);
        self.recount(0);
        true
    }

    /// Folds or unfolds one file: returns the row range it held and how many rows replace it.
    pub fn set_folded(&mut self, file: usize, folded: bool) -> Option<(Range<usize>, usize)> {
        if self.is_folded(file) == folded {
            return None;
        }
        let diff = self.parsed.files().get(file)?;
        let trailing = self.trailing_hidden.get(file).copied().unwrap_or(0);
        let next = if self.is_hidden(file) {
            Arc::new(Vec::new())
        } else {
            Arc::new(file_rows_with_trailing(0, diff, folded, self.layout, trailing))
        };
        let old = self.starts[file]..self.starts[file + 1];
        let count = next.len();
        self.rows[file] = next;
        self.folded[file] = folded;
        self.recount(file + 1);
        Some((old, count))
    }

    /// `toggleAllDiffFiles(shownFileKeys, …)`: hidden files keep their folds.
    pub fn set_all_folded(&mut self, folded: bool) {
        let files = self.parsed.files();
        for (i, file) in files.iter().enumerate().filter(|(i, _)| !self.hidden[*i]) {
            let trailing = self.trailing_hidden.get(i).copied().unwrap_or(0);
            self.rows[i] = Arc::new(file_rows_with_trailing(0, file, folded, self.layout, trailing));
            self.folded[i] = folded;
        }
        self.recount(0);
    }

    /// Applies an explicit per-file folded vector (used by `PullRequestDiffFolds`), rebuilding
    /// rows only for files whose fold state changed.
    pub fn set_folds(&mut self, next_folded: Vec<bool>) -> bool {
        if next_folded.len() != self.folded.len() || next_folded == self.folded {
            return false;
        }
        let files = self.parsed.files();
        for (i, file) in files.iter().enumerate() {
            if next_folded[i] != self.folded[i] {
                self.folded[i] = next_folded[i];
                if !self.hidden[i] {
                    let trailing = self.trailing_hidden.get(i).copied().unwrap_or(0);
                    self.rows[i] = Arc::new(file_rows_with_trailing(0, file, next_folded[i], self.layout, trailing));
                }
            }
        }
        self.recount(0);
        true
    }

    pub fn folded_keys(&self) -> HashSet<String> {
        self.keys.iter().zip(&self.folded).filter(|(_, folded)| **folded).map(|(key, _)| key.clone()).collect()
    }

    /// Replaces one file's parsed hunks after hidden context expansion, returning the row range
    /// to splice in the virtualized list.
    pub fn replace_expanded_file(&mut self, file: usize, expanded: FileDiff, trailing_hidden: u32) -> Option<(Range<usize>, usize)> {
        let ParsedPatch::Files(mut files) = self.parsed.patch.clone() else {
            return None;
        };
        if file >= files.len() {
            return None;
        }
        files[file] = expanded;
        let next_parsed = Parsed::from_patch(
            ParsedPatch::Files(files),
            self.parsed.truncated,
            self.parsed.totals,
            self.parsed.base_ref.clone(),
            self.parsed.head_ref.clone(),
        );
        self.trailing_hidden[file] = trailing_hidden;
        let next_rows = if self.is_hidden(file) {
            Arc::new(Vec::new())
        } else {
            Arc::new(file_rows_with_trailing(0, &next_parsed.files()[file], self.folded[file], self.layout, trailing_hidden))
        };
        let old = self.starts[file]..self.starts[file + 1];
        let count = next_rows.len();
        self.digits[file] = gutter_digits(&next_parsed.files()[file]);
        self.rows[file] = next_rows;
        self.parsed = next_parsed;
        self.recount(file + 1);
        Some((old, count))
    }

    pub fn rebuild_tree(&mut self, tree_collapsed: &HashSet<String>) {
        let hidden = &self.hidden;
        self.tree = file_tree_rows_shown(self.parsed.files(), tree_collapsed, |file| !hidden[file]);
    }
}

pub fn seed_height_estimates(list: &gpui::ListState, width: gpui::Pixels, previous_width: &mut Option<gpui::Pixels>) -> bool {
    if width <= gpui::px(0.) || *previous_width == Some(width) { return false; }
    *previous_width = Some(width);
    // GPUI discards all height hints on width changes, including the first layout.
    list.clone().with_uniform_item_height(gpui::px(super::lines::LINE_HEIGHT));
    true
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

fn rows_for(
    files: &[FileDiff],
    keys: &[String],
    folded: &[bool],
    trailing_hidden: &[u32],
    layout: Layout,
    previous: Option<&View>,
) -> Vec<Arc<Vec<DiffRow>>> {
    let cached: std::collections::HashMap<_, _> = previous.into_iter().flat_map(|view| view.keys.iter().enumerate().map(move |(i, key)| (key.as_str(), (view, i)))).collect();
    files.iter().enumerate().map(|(i, file)| {
        if let Some((view, old)) = cached.get(keys[i].as_str())
            && view.layout == layout && view.folded[*old] == folded[i] && !view.hidden[*old]
            && view.trailing_hidden[*old] == trailing_hidden[i]
            && same_rows(&view.parsed.files()[*old], file) {
            return view.rows[*old].clone();
        }
        Arc::new(file_rows_with_trailing(0, file, folded[i], layout, trailing_hidden[i]))
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
        let stat = ReviewFile { path: "new.rs".into(), previous_path: Some("old.rs".into()), additions: 1, deletions: 0, change_type: None };
        let placeholder = stat.placeholder();
        let parsed = |files| Parsed::from_patch(ParsedPatch::Files(files), false, None, None, None);
        let mut session = DiffSession::default();
        let initial = View::new(parsed(vec![placeholder.clone()]), "scope".into(), Layout::Stacked, &session, true, &HashSet::new(), true);
        let key = initial.key(0).unwrap().to_owned();
        session.set_file_fold("scope".into(), key.clone(), false, || initial.folded_keys());
        let loaded = FileDiff { change: ChangeKind::RenamePure, ..placeholder };
        let inserted = ReviewFile { path: "a.rs".into(), previous_path: None, additions: 1, deletions: 0, change_type: Some(ChangeKind::New) }.placeholder();
        assert_eq!(inserted.change, ChangeKind::New);
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
        let unmeasured = gpui::ListState::new(200, gpui::ListAlignment::Top, gpui::px(0.));
        unmeasured.scroll_to(gpui::ListOffset { item_ix: 140, offset_in_item: gpui::px(0.) });
        let mut width = None;
        seed_height_estimates(&unmeasured, gpui::px(800.), &mut width);
        assert_eq!(unmeasured.offset_for_item(140), gpui::px(2800.), "width invalidation must not leave later headers at zero distance");
        assert_eq!(unmeasured.logical_scroll_top().item_ix, 140);
        assert!(!seed_height_estimates(&unmeasured, gpui::px(800.), &mut width), "unchanged width does not remeasure the list again");
    }

    #[test]
    fn trailing_hidden_separator_survives_fold_collapse_all_hide_and_reorder_update() {
        let ParsedPatch::Files(mut files) = parse_patch(
            "--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n keep\n",
        ) else {
            panic!("files");
        };
        let file = files.remove(0);
        let parsed = Parsed::from_patch(ParsedPatch::Files(vec![file.clone()]), false, None, None, None);
        let session = DiffSession::default();
        let mut view = View::new(parsed, "scope".into(), Layout::Stacked, &session, false, &HashSet::new(), true);
        view.replace_expanded_file(0, file.clone(), 7).expect("expanded");
        let has_trailing = |view: &View, file_ix: usize| {
            view.file_rows_slice(file_ix)
                .is_some_and(|rows| rows.last() == Some(&DiffRow::Separator { file: 0, hunk: 1, hidden: 7 }))
        };
        assert!(has_trailing(&view, 0));

        // Fold and unfold single file (e.g. marking/unmarking Viewed).
        view.set_folded(0, true);
        view.set_folded(0, false);
        assert!(has_trailing(&view, 0), "set_folded must keep trailing_hidden");

        // Collapse all / expand all.
        view.set_all_folded(true);
        view.set_all_folded(false);
        assert!(has_trailing(&view, 0), "set_all_folded must keep trailing_hidden");

        // Hide test files / show again.
        assert!(view.set_hidden(vec![true], &HashSet::new()));
        assert!(view.set_hidden(vec![false], &HashSet::new()));
        assert!(has_trailing(&view, 0), "set_hidden must keep trailing_hidden");

        // Reorder / insertion update with same file hunks preserves trailing_hidden by key.
        let ParsedPatch::Files(mut extra) = parse_patch(
            "--- a/src/other.rs\n+++ b/src/other.rs\n@@ -1 +1 @@\n-a\n+b\n",
        ) else {
            panic!("files");
        };
        let reordered = Parsed::from_patch(
            ParsedPatch::Files(vec![extra.remove(0), view.parsed.files()[0].clone()]),
            false,
            None,
            None,
            None,
        );
        view.update(reordered, &session, &HashSet::new(), true);
        assert!(has_trailing(&view, 1), "update preserving file hunks must keep trailing_hidden");
    }
}
