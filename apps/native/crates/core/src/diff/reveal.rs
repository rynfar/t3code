use std::sync::Arc;

use super::{DiffLine, DiffRow, FileDiff, Hunk, LineKind};
use crate::composer::review::Side;

/// Target location requested by a file-tree click, turn summary link, or PR review comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRevealTarget {
    pub path: String,
    pub line: Option<u32>,
    pub end_line: Option<u32>,
    pub side: Option<Side>,
    pub highlight: bool,
}

impl FileRevealTarget {
    pub fn file(path: impl Into<String>) -> Self {
        Self { path: path.into(), line: None, end_line: None, side: None, highlight: false }
    }

    pub fn line(path: impl Into<String>, line: u32) -> Self {
        Self { path: path.into(), line: Some(line), end_line: None, side: None, highlight: true }
    }

    pub fn range(
        path: impl Into<String>,
        line: u32,
        end_line: Option<u32>,
        side: Option<Side>,
        highlight: bool,
    ) -> Self {
        Self { path: path.into(), line: Some(line), end_line, side, highlight }
    }

    pub fn normalized_range(&self) -> Option<(u32, u32)> {
        let start = self.line?;
        let end = self.end_line.unwrap_or(start).max(start);
        Some((start, end))
    }
}

/// Action emitted by `FileReveal` when a target is requested or patch data arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevealAction {
    None,
    /// File is not yet loaded in the partial review patch; request its patch first.
    RequestPatch { path: String },
    /// Scroll to the file's header row (and unfold it if collapsed).
    ScrollToFile { file: usize, path: String },
    /// Scroll to a resolved line/separator row and optionally apply a transient highlight.
    ScrollToRow {
        file: usize,
        path: String,
        row: usize,
        exact_line: bool,
        highlight: Option<(u32, u32, Option<Side>)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingReveal {
    key: String,
    target: FileRevealTarget,
}

/// State machine mirroring `createCodeViewFileReveal`: prioritizes explicit reveals over scroll
/// restoration, waits for lazy file patches, and cancels immediately on user interaction.
#[derive(Clone, Debug, Default)]
pub struct FileReveal {
    pending: Option<PendingReveal>,
    handled_key: Option<String>,
    highlight: Option<(String, u32, u32, Option<Side>)>,
}

impl FileReveal {
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn pending_path(&self) -> Option<&str> {
        self.pending.as_ref().map(|p| p.target.path.as_str())
    }

    pub fn highlight_for(&self, path: &str) -> Option<(u32, u32, Option<Side>)> {
        let (p, start, end, side) = self.highlight.as_ref()?;
        (p == path).then_some((*start, *end, *side))
    }

    /// Cancels an active pending reveal because the user scrolled, dragged, or typed.
    pub fn cancel_on_user_interaction(&mut self) -> bool {
        self.pending.take().is_some()
    }

    pub fn clear_highlight(&mut self) {
        self.highlight = None;
    }

    pub fn reset(&mut self) {
        self.pending = None;
        self.handled_key = None;
        self.highlight = None;
    }

    /// Starts a reveal request identified by `scope_key` and `request_id`.
    /// Duplicate `(scope_key, target, request_id)` requests are ignored once handled.
    pub fn request(
        &mut self,
        scope_key: &str,
        request_id: u64,
        target: FileRevealTarget,
    ) -> bool {
        let key = reveal_dedup_key(scope_key, request_id, &target);
        if self.handled_key.as_ref() == Some(&key) {
            return false;
        }
        if self.pending.as_ref().is_some_and(|p| p.key == key) {
            return false;
        }
        self.highlight = None;
        self.pending = Some(PendingReveal { key, target });
        true
    }

    /// Resolves the pending reveal against the currently parsed `files` and laid-out `rows`.
    ///
    /// Pass `patch_pending = true` when the requested file exists in the change list or more
    /// patches are still loading lazily so the reveal waits instead of dropping early.
    pub fn resolve(
        &mut self,
        files: &[FileDiff],
        rows: &[DiffRow],
        patch_pending: bool,
    ) -> RevealAction {
        self.resolve_with(files, patch_pending, |file_ix| (file_ix, 0, rows))
    }

    /// Variant of [`Self::resolve`] for partitioned per-file row tables (`DiffView`), where each
    /// file stores its own slice starting at `row_base` with local file index `local_file_ix`.
    pub fn resolve_with<'a>(
        &mut self,
        files: &[FileDiff],
        patch_pending: bool,
        mut lookup: impl FnMut(usize) -> (usize, usize, &'a [DiffRow]),
    ) -> RevealAction {
        let Some(pending) = self.pending.as_ref() else {
            return RevealAction::None;
        };
        let target = &pending.target;

        let Some(file_ix) = files.iter().position(|file| file_matches_path(file, &target.path)) else {
            if patch_pending {
                return RevealAction::RequestPatch { path: target.path.clone() };
            }
            self.pending = None;
            return RevealAction::None;
        };

        let file = &files[file_ix];
        let path = file.path().to_owned();

        let Some((start, end)) = target.normalized_range() else {
            let key = pending.key.clone();
            self.pending = None;
            self.handled_key = Some(key);
            return RevealAction::ScrollToFile { file: file_ix, path };
        };

        let (local_file_ix, row_base, rows) = lookup(file_ix);
        if let Some(local_row) = find_selection_row(rows, local_file_ix, file, start, target.side) {
            let highlight = target.highlight.then_some((start, end, target.side));
            if let Some((hs, he, hside)) = highlight {
                self.highlight = Some((path.clone(), hs, he, hside));
            }
            let key = pending.key.clone();
            self.pending = None;
            self.handled_key = Some(key);
            return RevealAction::ScrollToRow {
                file: file_ix,
                path,
                row: row_base + local_row,
                exact_line: true,
                highlight,
            };
        }

        // If the file is folded or its rows haven't expanded yet, callers should unfold first.
        // If rows already exist for this file (e.g. a gap in a partial diff), fall back to the
        // nearest separator or file header without losing the target range.
        if let Some(local_row) = find_nearest_row_for_line(rows, local_file_ix, file, start, target.side) {
            let highlight = target.highlight.then_some((start, end, target.side));
            if let Some((hs, he, hside)) = highlight {
                self.highlight = Some((path.clone(), hs, he, hside));
            }
            let key = pending.key.clone();
            self.pending = None;
            self.handled_key = Some(key);
            return RevealAction::ScrollToRow {
                file: file_ix,
                path,
                row: row_base + local_row,
                exact_line: false,
                highlight,
            };
        }

        if patch_pending {
            return RevealAction::RequestPatch { path };
        }

        let key = pending.key.clone();
        self.pending = None;
        self.handled_key = Some(key);
        RevealAction::ScrollToFile { file: file_ix, path }
    }
}

fn reveal_dedup_key(scope_key: &str, request_id: u64, target: &FileRevealTarget) -> String {
    let side = match target.side {
        Some(Side::Deletions) => "del",
        Some(Side::Additions) => "add",
        None => "any",
    };
    format!(
        "{scope_key}:{request_id}:{}:{:?}:{:?}:{side}",
        target.path, target.line, target.end_line
    )
}

pub fn file_matches_path(file: &FileDiff, path: &str) -> bool {
    file.path() == path
        || file.new_path.as_deref() == Some(path)
        || file.old_path.as_deref() == Some(path)
}

/// Returns the effective 1-based line number on a `DiffLine` for the given `side`
/// (defaulting to the addition/new side when `side` is `None`).
pub fn line_number_for_side(line: &DiffLine, side: Option<Side>) -> Option<u32> {
    match side {
        Some(Side::Deletions) => line.old_line,
        Some(Side::Additions) | None => line.new_line,
    }
}

/// Returns the addition (`new_line`) number at `hunk[line_ix]` if present (`additionLine`).
pub fn addition_line(file: &FileDiff, hunk_ix: usize, line_ix: usize) -> Option<u32> {
    file.hunks.get(hunk_ix)?.lines.get(line_ix)?.new_line
}

/// Finds the first row in `rows` belonging to `file_ix` that matches `target_line` on `side`.
pub fn find_selection_row(
    rows: &[DiffRow],
    file_ix: usize,
    file: &FileDiff,
    target_line: u32,
    side: Option<Side>,
) -> Option<usize> {
    let hunks = &file.hunks;
    rows.iter().position(|row| match *row {
        DiffRow::Line { file: rf, hunk, line } if rf == file_ix => hunks
            .get(hunk)
            .and_then(|h| h.lines.get(line))
            .and_then(|l| line_number_for_side(l, side))
            == Some(target_line),
        DiffRow::Split { file: rf, hunk, left, right } if rf == file_ix => {
            let hunk = hunks.get(hunk);
            let left_match = left
                .and_then(|li| hunk.and_then(|h| h.lines.get(li)))
                .and_then(|l| l.old_line)
                == Some(target_line);
            let right_match = right
                .and_then(|ri| hunk.and_then(|h| h.lines.get(ri)))
                .and_then(|l| l.new_line)
                == Some(target_line);
            match side {
                Some(Side::Deletions) => left_match,
                Some(Side::Additions) => right_match,
                None => right_match || left_match,
            }
        }
        _ => false,
    })
}

/// Finds the nearest rendered row for `target_line` when a line falls inside a collapsed
/// unchanged gap between hunks (prefers the separator directly before the gap, else file header).
pub fn find_nearest_row_for_line(
    rows: &[DiffRow],
    file_ix: usize,
    file: &FileDiff,
    target_line: u32,
    side: Option<Side>,
) -> Option<usize> {
    if let Some(exact) = find_selection_row(rows, file_ix, file, target_line, side) {
        return Some(exact);
    }

    let hunks = &file.hunks;
    let target_hunk = hunks.iter().position(|h| hunk_start_for_side(h, side) > target_line);

    if let Some(hunk_ix) = target_hunk
        && let Some(sep_row) = rows
            .iter()
            .position(|row| matches!(*row, DiffRow::Separator { file: rf, hunk, .. } if rf == file_ix && hunk == hunk_ix))
    {
        return Some(sep_row);
    }

    // Fall back to the last visible line row of this file if `target_line` sits after the last hunk,
    // or the file header when no content rows exist (e.g. folded file).
    let mut header_row = None;
    let mut last_line_row = None;
    for (ix, row) in rows.iter().enumerate() {
        if row.file() != file_ix {
            continue;
        }
        match row {
            DiffRow::FileHeader { .. } => header_row = Some(ix),
            DiffRow::Line { .. } | DiffRow::Split { .. } | DiffRow::Separator { .. } => {
                last_line_row = Some(ix);
            }
            _ => {}
        }
    }
    if target_hunk.is_none() && last_line_row.is_some() {
        return last_line_row;
    }
    header_row
}

fn hunk_start_for_side(hunk: &Hunk, side: Option<Side>) -> u32 {
    match side {
        Some(Side::Deletions) => hunk.old_start,
        Some(Side::Additions) | None => hunk.new_start,
    }
}

/// Expands a partial diff's hunks in memory using `file_lines` (1-indexed file lines stored in
/// a 0-indexed slice) so `target_start..=target_end` on `Side::Additions` is covered by a hunk.
///
/// Mirrors `expandPartialDiffToLine` from `useCodeViewFileReveal.ts`. Returns `None` if `file`
/// already renders `target_start..=target_end`, or if `file_lines` is empty / out of range.
pub fn expand_partial_diff_to_line(
    file: &FileDiff,
    file_lines: &[&str],
    target_start: u32,
    target_end: Option<u32>,
    context_lines: u32,
) -> Option<FileDiff> {
    if file.binary || file_lines.is_empty() || target_start == 0 {
        return None;
    }
    let total_lines = u32::try_from(file_lines.len()).unwrap_or(u32::MAX);
    let end = target_end.unwrap_or(target_start).max(target_start).min(total_lines);
    let start = target_start.min(total_lines);

    if range_covered_on_new_side(&file.hunks, start, end) {
        return None;
    }

    let expand_start = start.saturating_sub(context_lines).max(1);
    let expand_end = end.saturating_add(context_lines).min(total_lines);

    let mut cursor = expand_start;
    let mut old_delta = 0i64;
    let mut pieces: Vec<Hunk> = Vec::with_capacity(file.hunks.len() * 2 + 1);

    for hunk in &file.hunks {
        let before_end = if hunk.new_count == 0 {
            hunk.new_start
        } else {
            hunk.new_start.saturating_sub(1)
        };
        if cursor <= expand_end && cursor <= before_end {
            let gap_end = before_end.min(expand_end);
            pieces.push(build_context_hunk(file_lines, cursor, gap_end, old_delta));
            cursor = gap_end.saturating_add(1);
        }
        pieces.push(hunk.clone());
        old_delta += i64::from(hunk.new_count) - i64::from(hunk.old_count);
        let hunk_after = if hunk.new_count == 0 {
            hunk.new_start.saturating_add(1)
        } else {
            hunk.new_start.saturating_add(hunk.new_count)
        };
        cursor = cursor.max(hunk_after);
    }

    if cursor <= expand_end {
        pieces.push(build_context_hunk(file_lines, cursor, expand_end, old_delta));
    }

    let merged = merge_adjacent_hunks(pieces);
    let mut expanded = file.clone();
    expanded.hunks = super::FileHunks::from(merged);
    Some(expanded)
}

fn range_covered_on_new_side(hunks: &[Hunk], start: u32, end: u32) -> bool {
    (start..=end).all(|target| {
        hunks.iter().any(|hunk| hunk.lines.iter().any(|line| line.new_line == Some(target)))
    })
}

fn build_context_hunk(file_lines: &[&str], new_start: u32, new_end: u32, old_delta: i64) -> Hunk {
    let old_start = u32::try_from((i64::from(new_start) - old_delta).max(1)).unwrap_or(1);
    let mut lines = Vec::with_capacity((new_end.saturating_sub(new_start) + 1) as usize);
    for (offset, new_num) in (new_start..=new_end).enumerate() {
        let raw = file_lines[(new_num - 1) as usize];
        let old_num = old_start.saturating_add(offset as u32);
        lines.push(DiffLine {
            kind: LineKind::Context,
            old_line: Some(old_num),
            new_line: Some(new_num),
            text: Arc::from(raw),
            source_text: Arc::from(raw),
            no_newline_at_eof: false,
        });
    }
    let count = lines.len() as u32;
    Hunk {
        old_start,
        old_count: count,
        new_start,
        new_count: count,
        context: String::new(),
        lines,
    }
}

fn hunks_touch(prev: &Hunk, next: &Hunk) -> bool {
    match (prev.new_count, next.new_count) {
        (0, 0) => prev.new_start == next.new_start,
        (0, _) => next.new_start <= prev.new_start.saturating_add(1),
        (n, 0) => prev.new_start.saturating_add(n).saturating_sub(1) >= next.new_start,
        (n, _) => next.new_start <= prev.new_start.saturating_add(n),
    }
}

fn merge_adjacent_hunks(hunks: Vec<Hunk>) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        if let Some(prev) = out.last_mut().filter(|prev| hunks_touch(prev, &hunk)) {
            prev.lines.extend(hunk.lines);
            recompute_hunk_bounds(prev);
        } else {
            out.push(hunk);
        }
    }
    out
}

fn recompute_hunk_bounds(hunk: &mut Hunk) {
    let old_lines: Vec<u32> = hunk.lines.iter().filter_map(|l| l.old_line).collect();
    let new_lines: Vec<u32> = hunk.lines.iter().filter_map(|l| l.new_line).collect();
    if let Some(&first_old) = old_lines.first() {
        hunk.old_start = first_old;
    }
    hunk.old_count = old_lines.len() as u32;
    if let Some(&first_new) = new_lines.first() {
        hunk.new_start = first_new;
    }
    hunk.new_count = new_lines.len() as u32;
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::diff::{Layout, ParsedPatch, build_rows, parse_patch};

    const PATCH: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
 fn a() {}
+fn added() {}
 fn b() {}
 fn c() {}
@@ -18,3 +19,3 @@
 fn x() {}
-fn y() {}
+fn y2() {}
 fn z() {}
";

    fn parse_files() -> Vec<FileDiff> {
        match parse_patch(PATCH) {
            ParsedPatch::Files(files) => files,
            other => panic!("expected parsed files, got {other:?}"),
        }
    }

    #[test]
    fn resolves_file_and_exact_line_reveal_and_dedups_requests() {
        let files = parse_files();
        let rows = build_rows(&files, &HashSet::new(), Layout::Stacked);
        let mut reveal = FileReveal::default();

        assert!(reveal.request("scope:1", 1, FileRevealTarget::line("src/lib.rs", 2)));
        // Duplicate request with same id is ignored.
        assert!(!reveal.request("scope:1", 1, FileRevealTarget::line("src/lib.rs", 2)));

        let action = reveal.resolve(&files, &rows, false);
        match action {
            RevealAction::ScrollToRow { file, path, row, exact_line, highlight } => {
                assert_eq!(file, 0);
                assert_eq!(path, "src/lib.rs");
                assert!(exact_line);
                assert_eq!(highlight, Some((2, 2, None)));
                assert!(matches!(rows[row], DiffRow::Line { file: 0, hunk: 0, line: 1 }));
            }
            other => panic!("unexpected action: {other:?}"),
        }

        // Same scope + request id stays deduplicated once handled.
        assert!(!reveal.request("scope:1", 1, FileRevealTarget::line("src/lib.rs", 2)));
        // New request id re-runs.
        assert!(reveal.request("scope:1", 2, FileRevealTarget::file("src/lib.rs")));
    }

    #[test]
    fn waits_while_patch_pending_and_cancels_on_user_interaction() {
        let files = parse_files();
        let rows = build_rows(&files, &HashSet::new(), Layout::Stacked);
        let mut reveal = FileReveal::default();

        assert!(reveal.request("scope:1", 1, FileRevealTarget::line("src/missing.rs", 10)));
        assert_eq!(
            reveal.resolve(&files, &rows, true),
            RevealAction::RequestPatch { path: "src/missing.rs".into() }
        );
        assert!(reveal.has_pending());

        // User scrolls before the patch arrives: cancel pending reveal.
        assert!(reveal.cancel_on_user_interaction());
        assert!(!reveal.has_pending());
        assert_eq!(reveal.resolve(&files, &rows, true), RevealAction::None);
    }

    #[test]
    fn falls_back_to_separator_for_line_in_collapsed_gap() {
        let files = parse_files();
        let rows = build_rows(&files, &HashSet::new(), Layout::Stacked);
        let mut reveal = FileReveal::default();

        assert!(reveal.request("scope:1", 1, FileRevealTarget::line("src/lib.rs", 12)));
        match reveal.resolve(&files, &rows, false) {
            RevealAction::ScrollToRow { row, exact_line, .. } => {
                assert!(!exact_line);
                assert!(matches!(rows[row], DiffRow::Separator { file: 0, hunk: 1, .. }));
            }
            other => panic!("expected separator fallback, got {other:?}"),
        }
    }

    #[test]
    fn expands_partial_diff_to_uncovered_line_range() {
        let files = parse_files();
        let full_lines: Vec<String> = (1..=25).map(|i| format!("line {i}")).collect();
        let line_refs: Vec<&str> = full_lines.iter().map(String::as_str).collect();

        // Line 2 is already covered -> returns None.
        assert!(expand_partial_diff_to_line(&files[0], &line_refs, 2, None, 2).is_none());

        // Line 12 is in the gap between hunk 0 (1..=4) and hunk 1 (19..=21).
        let expanded = expand_partial_diff_to_line(&files[0], &line_refs, 12, Some(13), 1)
            .expect("should expand partial diff");
        let rows = build_rows(std::slice::from_ref(&expanded), &HashSet::new(), Layout::Stacked);
        assert!(find_selection_row(&rows, 0, &expanded, 12, Some(Side::Additions)).is_some());
        assert!(find_selection_row(&rows, 0, &expanded, 13, Some(Side::Additions)).is_some());
    }

    #[test]
    fn expands_across_hunk_with_additions_and_deletions_without_dropping_lines() {
        let patch = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -5,3 +5,4 @@
 line 5
-line 6
+line 6 new
+line 7 new
 line 8
";
        let files = match parse_patch(patch) {
            ParsedPatch::Files(files) => files,
            other => panic!("expected parsed files, got {other:?}"),
        };
        let full_lines: Vec<String> = (1..=14).map(|i| format!("new {i}")).collect();
        let line_refs: Vec<&str> = full_lines.iter().map(String::as_str).collect();

        // Range 3..=11 spans before, through, and after hunk 0 (new lines 5..=8, delta +1).
        let expanded = expand_partial_diff_to_line(&files[0], &line_refs, 3, Some(11), 0)
            .expect("should expand across existing hunk");
        assert_eq!(expanded.hunks.len(), 1);
        let hunk = &expanded.hunks[0];
        let kinds_and_lines: Vec<(LineKind, Option<u32>, Option<u32>)> =
            hunk.lines.iter().map(|l| (l.kind, l.old_line, l.new_line)).collect();
        assert_eq!(
            kinds_and_lines,
            vec![
                (LineKind::Context, Some(3), Some(3)),
                (LineKind::Context, Some(4), Some(4)),
                (LineKind::Context, Some(5), Some(5)),
                (LineKind::Deletion, Some(6), None),
                (LineKind::Addition, None, Some(6)),
                (LineKind::Addition, None, Some(7)),
                (LineKind::Context, Some(7), Some(8)),
                (LineKind::Context, Some(8), Some(9)),
                (LineKind::Context, Some(9), Some(10)),
                (LineKind::Context, Some(10), Some(11)),
            ]
        );
        assert_eq!((hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count), (3, 8, 3, 9));
    }
}
