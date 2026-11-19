//! Hidden hunk context expansion (`apps/web/src/lib/diffFileContents.ts` and Pierre's
//! `expandHunk`): fetches old/new file contents via `review.getDiffFileContents`, checks that
//! the live file still reproduces the loaded patch (`reproducesPatch`), and splices unchanged
//! context lines into adjacent hunks (merging hunks once the gap closes).

use std::sync::Arc;

use serde_json::{Value, json};

use crate::composer::review::Side;

use super::{ChangeKind, DiffLine, FileDiff, FileHunks, Hunk, LineKind, PreviewSourceKind, parse};

/// Pierre's default step size for hunk separator expansion.
pub const DEFAULT_EXPAND_LINES: u32 = 20;

/// Error message matching `apps/web/src/lib/diffFileContents.ts:130`.
pub const STALE_DIFF_FILE_ERROR: &str =
    "This file changed since the diff was loaded. Refresh the diff to expand it.";

/// `review.getDiffFileContents` RPC method name (`WS_METHODS.reviewGetDiffFileContents`).
pub const DIFF_FILE_CONTENTS_METHOD: &str = "review.getDiffFileContents";

/// `ReviewDiffFileContentsResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFileContents {
    pub old_contents: String,
    pub new_contents: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpandDirection {
    /// Expand upward from the hunk below the separator (towards smaller line numbers).
    Up,
    /// Expand downward from the hunk above the separator (towards larger line numbers).
    Down,
    /// Expand every hidden line in this gap.
    All,
}

/// `ReviewDiffFileContentsInput` (`createGitDiffFileContentsLoader`).
pub fn diff_file_contents_request(
    cwd: &str,
    source_kind: PreviewSourceKind,
    change_type: ChangeKind,
    base_ref: Option<&str>,
    head_ref: Option<&str>,
    old_path: &str,
    new_path: &str,
) -> (&'static str, Value) {
    (
        DIFF_FILE_CONTENTS_METHOD,
        json!({
            "cwd": cwd,
            "sourceKind": source_kind.wire(),
            "changeType": change_type.as_str(),
            "baseRef": base_ref,
            "headRef": head_ref,
            "oldPath": old_path,
            "newPath": new_path,
        }),
    )
}

pub fn decode_diff_file_contents(value: &Value) -> Result<DiffFileContents, String> {
    let old_contents = value
        .get("oldContents")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing oldContents in diff file contents response.".to_owned())?;
    let new_contents = value
        .get("newContents")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing newContents in diff file contents response.".to_owned())?;
    Ok(DiffFileContents {
        old_contents: old_contents.to_owned(),
        new_contents: new_contents.to_owned(),
    })
}

/// Mirrors `splitLines` in `diffFileContents.ts` (keeps `\n` attached so line counts match).
fn split_lines_with_endings(contents: &str) -> Vec<&str> {
    if contents.is_empty() {
        return Vec::new();
    }
    contents.split_inclusive('\n').collect()
}

/// Strips trailing `\n` (keeping `\r` so CRLF matches `parse_patch`).
fn strip_lf(line: &str) -> &str {
    line.strip_suffix('\n').unwrap_or(line)
}

fn same_line_ignoring_whitespace(patch_line: &str, file_line: &str) -> bool {
    let mut a = patch_line.chars().filter(|c| !c.is_whitespace());
    let mut b = file_line.chars().filter(|c| !c.is_whitespace());
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if x == y => continue,
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// 1-based line number of the first line inside a hunk, or the first line *after* the insertion
/// point when `count == 0` (in unified diffs `@@ -5,0 +6,3 @@` means "after old line 5", so old
/// lines `1..=5` precede the hunk and line `6` is the first line after it).
fn hunk_side_start(start: u32, count: u32) -> u32 {
    if count == 0 {
        start.saturating_add(1)
    } else {
        start
    }
}

fn hunk_side_end(start: u32, count: u32) -> usize {
    if count == 0 {
        start as usize
    } else {
        (start as usize).saturating_sub(1) + count as usize
    }
}

/// `reproducesPatch` (`apps/web/src/lib/diffFileContents.ts`): verifies every hunk's old and new
/// lines match `contents` (whitespace-insensitively) and that trailing line counts agree.
pub fn reproduces_patch(file: &FileDiff, contents: &DiffFileContents) -> bool {
    if file.change == ChangeKind::RenamePure {
        return true;
    }
    let Some(last_hunk) = file.hunks.last() else {
        return true;
    };
    let old_lines = split_lines_with_endings(&contents.old_contents);
    let new_lines = split_lines_with_endings(&contents.new_contents);

    for hunk in file.hunks.iter() {
        let old_start_ix = hunk_side_end(hunk.old_start, hunk.old_count).saturating_sub(hunk.old_count as usize);
        let mut old_offset = 0usize;
        let new_start_ix = hunk_side_end(hunk.new_start, hunk.new_count).saturating_sub(hunk.new_count as usize);
        let mut new_offset = 0usize;
        for line in &hunk.lines {
            if matches!(line.kind, LineKind::Context | LineKind::Deletion) {
                let Some(file_line) = old_lines.get(old_start_ix + old_offset) else {
                    return false;
                };
                if !same_line_ignoring_whitespace(&line.source_text, file_line) {
                    return false;
                }
                old_offset += 1;
            }
            if matches!(line.kind, LineKind::Context | LineKind::Addition) {
                let Some(file_line) = new_lines.get(new_start_ix + new_offset) else {
                    return false;
                };
                if !same_line_ignoring_whitespace(&line.source_text, file_line) {
                    return false;
                }
                new_offset += 1;
            }
        }
        if old_offset != hunk.old_count as usize || new_offset != hunk.new_count as usize {
            return false;
        }
    }

    let old_tail = old_lines.len().checked_sub(hunk_side_end(last_hunk.old_start, last_hunk.old_count));
    let new_tail = new_lines.len().checked_sub(hunk_side_end(last_hunk.new_start, last_hunk.new_count));
    matches!((old_tail, new_tail), (Some(a), Some(b)) if a == b)
}

/// Number of unchanged trailing lines after the last hunk, once full file contents are known.
pub fn trailing_hidden_lines(file: &FileDiff, contents: &DiffFileContents) -> u32 {
    let Some(last) = file.hunks.last() else {
        return 0;
    };
    let total_new = split_lines_with_endings(&contents.new_contents).len();
    let total_old = split_lines_with_endings(&contents.old_contents).len();
    if last.new_count == 0 && last.new_start == 0 {
        total_old.saturating_sub(hunk_side_end(last.old_start, last.old_count)) as u32
    } else {
        total_new.saturating_sub(hunk_side_end(last.new_start, last.new_count)) as u32
    }
}

fn make_context_line(old_line: u32, new_line: u32, raw_with_ending: &str) -> DiffLine {
    let text_no_lf = strip_lf(raw_with_ending);
    let source_text: Arc<str> = Arc::from(text_no_lf);
    let text = if text_no_lf.contains('\t') {
        parse::expand_tabs(text_no_lf)
    } else {
        source_text.clone()
    };
    DiffLine {
        kind: LineKind::Context,
        old_line: Some(old_line),
        new_line: Some(new_line),
        text,
        source_text,
        no_newline_at_eof: !raw_with_ending.ends_with('\n'),
    }
}

/// Expands hidden context lines at separator `hunk_index`:
/// - `0`: lines above hunk 0
/// - `1..hunks.len()`: lines between `hunks[hunk_index - 1]` and `hunks[hunk_index]`
/// - `hunks.len()`: trailing lines below the last hunk
pub fn expand_hunk_context(
    file: &FileDiff,
    contents: &DiffFileContents,
    hunk_index: usize,
    direction: ExpandDirection,
    count: u32,
) -> Result<FileDiff, String> {
    if !reproduces_patch(file, contents) {
        return Err(STALE_DIFF_FILE_ERROR.to_owned());
    }
    if file.hunks.is_empty() || hunk_index > file.hunks.len() {
        return Ok(file.clone());
    }
    let old_lines = split_lines_with_endings(&contents.old_contents);
    let new_lines = split_lines_with_endings(&contents.new_contents);
    let mut hunks: Vec<Hunk> = file.hunks.iter().cloned().collect();

    if hunk_index == 0 {
        let first = &mut hunks[0];
        let first_old_start = hunk_side_start(first.old_start, first.old_count);
        let first_new_start = hunk_side_start(first.new_start, first.new_count);
        let hidden = first_new_start.saturating_sub(1).min(first_old_start.saturating_sub(1));
        if hidden == 0 {
            return Ok(file.clone());
        }
        let take = if direction == ExpandDirection::All { hidden } else { count.min(hidden) };
        let start_old = first_old_start - take;
        let start_new = first_new_start - take;
        let mut prefix = Vec::with_capacity(take as usize + first.lines.len());
        for offset in 0..take {
            let old_no = start_old + offset;
            let new_no = start_new + offset;
            let raw = new_lines
                .get((new_no - 1) as usize)
                .or_else(|| old_lines.get((old_no - 1) as usize))
                .copied()
                .unwrap_or("");
            prefix.push(make_context_line(old_no, new_no, raw));
        }
        prefix.append(&mut first.lines);
        first.lines = prefix;
        first.old_start = start_old;
        first.old_count += take;
        first.new_start = start_new;
        first.new_count += take;
    } else if hunk_index == hunks.len() {
        let last = hunks.last_mut().expect("non-empty hunks");
        let after_old = hunk_side_end(last.old_start, last.old_count);
        let after_new = hunk_side_end(last.new_start, last.new_count);
        let hidden = old_lines.len().saturating_sub(after_old).min(new_lines.len().saturating_sub(after_new)) as u32;
        if hidden == 0 {
            return Ok(file.clone());
        }
        let take = if direction == ExpandDirection::All { hidden } else { count.min(hidden) };
        if last.old_count == 0 {
            last.old_start = after_old as u32 + 1;
        }
        if last.new_count == 0 {
            last.new_start = after_new as u32 + 1;
        }
        for offset in 0..take {
            let old_no = after_old as u32 + 1 + offset;
            let new_no = after_new as u32 + 1 + offset;
            let raw = new_lines.get((new_no - 1) as usize).copied().unwrap_or("");
            last.lines.push(make_context_line(old_no, new_no, raw));
        }
        last.old_count += take;
        last.new_count += take;
    } else {
        let prev_old_end = hunk_side_end(hunks[hunk_index - 1].old_start, hunks[hunk_index - 1].old_count) as u32;
        let prev_new_end = hunk_side_end(hunks[hunk_index - 1].new_start, hunks[hunk_index - 1].new_count) as u32;
        let next_old_start = hunk_side_start(hunks[hunk_index].old_start, hunks[hunk_index].old_count);
        let next_new_start = hunk_side_start(hunks[hunk_index].new_start, hunks[hunk_index].new_count);
        let gap_old = next_old_start.saturating_sub(prev_old_end + 1);
        let gap_new = next_new_start.saturating_sub(prev_new_end + 1);
        let hidden = gap_old.min(gap_new);
        if hidden == 0 {
            return Ok(file.clone());
        }
        let take = if direction == ExpandDirection::All { hidden } else { count.min(hidden) };
        if take >= hidden || direction == ExpandDirection::All {
            let mut bridge = Vec::with_capacity(hidden as usize);
            for offset in 0..hidden {
                let old_no = prev_old_end + 1 + offset;
                let new_no = prev_new_end + 1 + offset;
                let raw = new_lines.get((new_no - 1) as usize).copied().unwrap_or("");
                bridge.push(make_context_line(old_no, new_no, raw));
            }
            let next_hunk = hunks.remove(hunk_index);
            let prev_hunk = &mut hunks[hunk_index - 1];
            if prev_hunk.old_count == 0 {
                prev_hunk.old_start = prev_old_end + 1;
            }
            if prev_hunk.new_count == 0 {
                prev_hunk.new_start = prev_new_end + 1;
            }
            prev_hunk.lines.extend(bridge);
            prev_hunk.lines.extend(next_hunk.lines);
            prev_hunk.old_count += hidden + next_hunk.old_count;
            prev_hunk.new_count += hidden + next_hunk.new_count;
        } else {
            match direction {
                ExpandDirection::Down => {
                    let prev_hunk = &mut hunks[hunk_index - 1];
                    if prev_hunk.old_count == 0 {
                        prev_hunk.old_start = prev_old_end + 1;
                    }
                    if prev_hunk.new_count == 0 {
                        prev_hunk.new_start = prev_new_end + 1;
                    }
                    for offset in 0..take {
                        let old_no = prev_old_end + 1 + offset;
                        let new_no = prev_new_end + 1 + offset;
                        let raw = new_lines.get((new_no - 1) as usize).copied().unwrap_or("");
                        prev_hunk.lines.push(make_context_line(old_no, new_no, raw));
                    }
                    prev_hunk.old_count += take;
                    prev_hunk.new_count += take;
                }
                ExpandDirection::Up | ExpandDirection::All => {
                    let next_hunk = &mut hunks[hunk_index];
                    let start_old = next_old_start - take;
                    let start_new = next_new_start - take;
                    let mut prefix = Vec::with_capacity(take as usize + next_hunk.lines.len());
                    for offset in 0..take {
                        let old_no = start_old + offset;
                        let new_no = start_new + offset;
                        let raw = new_lines.get((new_no - 1) as usize).copied().unwrap_or("");
                        prefix.push(make_context_line(old_no, new_no, raw));
                    }
                    prefix.append(&mut next_hunk.lines);
                    next_hunk.lines = prefix;
                    next_hunk.old_start = start_old;
                    next_hunk.old_count += take;
                    next_hunk.new_start = start_new;
                    next_hunk.new_count += take;
                }
            }
        }
    }

    let mut next_file = file.clone();
    next_file.hunks = FileHunks::with_object_ids(
        hunks,
        file.old_object_id().map(str::to_owned),
        file.new_object_id().map(str::to_owned),
    );
    Ok(next_file)
}

/// Expands `file` around `target_line` (when outside the visible hunks), porting
/// `expandPartialDiffToLine` (`apps/web/src/components/diffs/useCodeViewFileReveal.ts`).
pub fn expand_file_to_line(
    file: &FileDiff,
    contents: &DiffFileContents,
    target_line: u32,
    side: Option<Side>,
) -> Result<FileDiff, String> {
    const REVEAL_CONTEXT: u32 = 3;
    let use_old = side == Some(Side::Deletions);
    if file.hunks.is_empty() {
        return Ok(file.clone());
    }
    let range_of = |hunk: &Hunk| -> (u32, u32) {
        if use_old {
            let start = hunk_side_start(hunk.old_start, hunk.old_count);
            (start, start.saturating_add(hunk.old_count))
        } else {
            let start = hunk_side_start(hunk.new_start, hunk.new_count);
            (start, start.saturating_add(hunk.new_count))
        }
    };
    for (idx, hunk) in file.hunks.iter().enumerate() {
        let (start, end) = range_of(hunk);
        if target_line >= start && target_line < end {
            return Ok(file.clone());
        }
        if target_line < start {
            if idx == 0 {
                let needed = start.saturating_sub(target_line) + REVEAL_CONTEXT;
                return expand_hunk_context(file, contents, 0, ExpandDirection::Up, needed);
            }
            let (_, prev_end) = range_of(&file.hunks[idx - 1]);
            let from_prev = target_line.saturating_sub(prev_end).saturating_add(1);
            let from_next = start.saturating_sub(target_line);
            return if from_prev <= from_next {
                expand_hunk_context(file, contents, idx, ExpandDirection::Down, from_prev + REVEAL_CONTEXT)
            } else {
                expand_hunk_context(file, contents, idx, ExpandDirection::Up, from_next + REVEAL_CONTEXT)
            };
        }
    }
    let (_, last_end) = range_of(file.hunks.last().expect("non-empty hunks"));
    let needed = target_line.saturating_sub(last_end).saturating_add(1) + REVEAL_CONTEXT;
    expand_hunk_context(file, contents, file.hunks.len(), ExpandDirection::Down, needed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{ParsedPatch, parse_patch};

    fn sample_patch_and_contents() -> (FileDiff, DiffFileContents) {
        let old_lines: Vec<String> = (1..=30).map(|i| format!("line {i}\n")).collect();
        let mut new_lines = old_lines.clone();
        new_lines[4] = "line 5 edited\n".to_owned();
        new_lines[24] = "line 25 edited\n".to_owned();
        let patch = "\
diff --git a/demo.txt b/demo.txt
index 1111111..2222222 100644
--- a/demo.txt
+++ b/demo.txt
@@ -4,3 +4,3 @@
 line 4
-line 5
+line 5 edited
 line 6
@@ -24,3 +24,3 @@
 line 24
-line 25
+line 25 edited
 line 26
";
        let ParsedPatch::Files(mut files) = parse_patch(patch) else {
            panic!("expected parsed files");
        };
        let contents = DiffFileContents {
            old_contents: old_lines.concat(),
            new_contents: new_lines.concat(),
        };
        (files.remove(0), contents)
    }

    #[test]
    fn reproduces_patch_validates_matching_and_rejects_stale_contents() {
        let (file, contents) = sample_patch_and_contents();
        assert!(reproduces_patch(&file, &contents));
        assert_eq!(trailing_hidden_lines(&file, &contents), 4);

        let mut stale = contents.clone();
        stale.new_contents.push_str("extra trailing line\n");
        assert!(!reproduces_patch(&file, &stale));
        let err = expand_hunk_context(&file, &stale, 1, ExpandDirection::Up, 5).unwrap_err();
        assert_eq!(err, STALE_DIFF_FILE_ERROR);
    }

    #[test]
    fn expands_above_between_trailing_and_merges_when_gap_closes() {
        let (file, contents) = sample_patch_and_contents();
        let above = expand_hunk_context(&file, &contents, 0, ExpandDirection::Up, 2).unwrap();
        assert_eq!(above.hunks[0].new_start, 2);
        assert_eq!(above.hunks[0].lines[0].text.as_ref(), "line 2");

        let partial = expand_hunk_context(&file, &contents, 1, ExpandDirection::Down, 5).unwrap();
        assert_eq!(partial.hunks.len(), 2);
        assert_eq!(partial.hunks[0].new_count, 8);
        assert_eq!(partial.hunks[0].lines.last().unwrap().text.as_ref(), "line 11");

        let merged = expand_hunk_context(&partial, &contents, 1, ExpandDirection::All, DEFAULT_EXPAND_LINES).unwrap();
        assert_eq!(merged.hunks.len(), 1);
        assert_eq!(merged.hunks[0].new_start, 4);
        assert_eq!(merged.hunks[0].new_count, 23);

        let with_tail = expand_hunk_context(&merged, &contents, 1, ExpandDirection::Down, 10).unwrap();
        assert_eq!(trailing_hidden_lines(&with_tail, &contents), 0);
        assert_eq!(with_tail.hunks[0].lines.last().unwrap().text.as_ref(), "line 30");
    }

    #[test]
    fn expand_file_to_line_brings_hidden_target_into_hunks() {
        let (file, contents) = sample_patch_and_contents();
        let expanded = expand_file_to_line(&file, &contents, 14, Some(Side::Additions)).unwrap();
        assert!(expanded.hunks.iter().flat_map(|h| &h.lines).any(|l| l.new_line == Some(14)));
    }

    #[test]
    fn zero_count_pure_addition_and_deletion_hunks_expand_and_merge_without_off_by_one() {
        // Pure addition hunk @@ -5,0 +6,3 @@ followed by a second pure addition hunk @@ -10,0 +14,2 @@.
        let old_lines: Vec<String> = (1..=14).map(|i| format!("old {i}\n")).collect();
        let mut new_lines: Vec<String> = Vec::new();
        for i in 1..=5 {
            new_lines.push(format!("old {i}\n"));
        }
        new_lines.push("added a\n".into());
        new_lines.push("added b\n".into());
        new_lines.push("added c\n".into());
        for i in 6..=10 {
            new_lines.push(format!("old {i}\n"));
        }
        new_lines.push("added d\n".into());
        new_lines.push("added e\n".into());
        for i in 11..=14 {
            new_lines.push(format!("old {i}\n"));
        }
        let add_patch = "\
diff --git a/add.txt b/add.txt
index 1111111..2222222 100644
--- a/add.txt
+++ b/add.txt
@@ -5,0 +6,3 @@
+added a
+added b
+added c
@@ -10,0 +14,2 @@
+added d
+added e
";
        let ParsedPatch::Files(mut files) = parse_patch(add_patch) else { panic!("files") };
        let add_file = files.remove(0);
        let add_contents = DiffFileContents { old_contents: old_lines.concat(), new_contents: new_lines.concat() };
        assert!(reproduces_patch(&add_file, &add_contents));

        // Incremental expand above hunk 0: line 5 must pair (old 5, new 5) and still reproduce_patch.
        let step1 = expand_hunk_context(&add_file, &add_contents, 0, ExpandDirection::Up, 1).unwrap();
        assert_eq!((step1.hunks[0].old_start, step1.hunks[0].old_count), (5, 1));
        assert_eq!((step1.hunks[0].new_start, step1.hunks[0].new_count), (5, 4));
        assert_eq!((step1.hunks[0].lines[0].old_line, step1.hunks[0].lines[0].new_line), (Some(5), Some(5)));
        assert!(reproduces_patch(&step1, &add_contents));
        let step2 = expand_hunk_context(&step1, &add_contents, 0, ExpandDirection::All, 20).unwrap();
        assert_eq!((step2.hunks[0].old_start, step2.hunks[0].old_count), (1, 5));
        assert_eq!((step2.hunks[0].new_start, step2.hunks[0].new_count), (1, 8));
        assert!(reproduces_patch(&step2, &add_contents));

        // Incremental Up from second zero-count hunk, then merge across the gap, then trailing.
        let mid_up = expand_hunk_context(&step2, &add_contents, 1, ExpandDirection::Up, 1).unwrap();
        assert_eq!((mid_up.hunks[1].old_start, mid_up.hunks[1].old_count), (10, 1));
        assert_eq!((mid_up.hunks[1].new_start, mid_up.hunks[1].new_count), (13, 3));
        assert!(reproduces_patch(&mid_up, &add_contents));
        let merged = expand_hunk_context(&mid_up, &add_contents, 1, ExpandDirection::All, 20).unwrap();
        assert_eq!(merged.hunks.len(), 1);
        assert_eq!((merged.hunks[0].old_start, merged.hunks[0].old_count), (1, 10));
        assert_eq!((merged.hunks[0].new_start, merged.hunks[0].new_count), (1, 15));
        assert!(reproduces_patch(&merged, &add_contents));

        // Trailing down from an unexpanded pure-addition hunk also advances old_start from 10 to 11.
        let tail_direct = expand_hunk_context(&add_file, &add_contents, 2, ExpandDirection::Down, 2).unwrap();
        assert_eq!((tail_direct.hunks[1].old_start, tail_direct.hunks[1].old_count), (11, 2));
        assert!(reproduces_patch(&tail_direct, &add_contents));

        // Pure deletion hunk @@ -6,3 +5,0 @@ (old lines 6..=8 removed after new line 5).
        let del_old: Vec<String> = (1..=12).map(|i| format!("line {i}\n")).collect();
        let mut del_new: Vec<String> = (1..=5).map(|i| format!("line {i}\n")).collect();
        del_new.extend((9..=12).map(|i| format!("line {i}\n")));
        let del_patch = "\
diff --git a/del.txt b/del.txt
index 1111111..2222222 100644
--- a/del.txt
+++ b/del.txt
@@ -6,3 +5,0 @@
-line 6
-line 7
-line 8
";
        let ParsedPatch::Files(mut files) = parse_patch(del_patch) else { panic!("files") };
        let del_file = files.remove(0);
        let del_contents = DiffFileContents { old_contents: del_old.concat(), new_contents: del_new.concat() };
        assert!(reproduces_patch(&del_file, &del_contents));

        let del_up = expand_hunk_context(&del_file, &del_contents, 0, ExpandDirection::Up, 1).unwrap();
        assert_eq!((del_up.hunks[0].old_start, del_up.hunks[0].old_count), (5, 4));
        assert_eq!((del_up.hunks[0].new_start, del_up.hunks[0].new_count), (5, 1));
        assert!(reproduces_patch(&del_up, &del_contents));

        let del_down = expand_hunk_context(&del_file, &del_contents, 1, ExpandDirection::Down, 2).unwrap();
        assert_eq!((del_down.hunks[0].old_start, del_down.hunks[0].old_count), (6, 5));
        assert_eq!((del_down.hunks[0].new_start, del_down.hunks[0].new_count), (6, 2));
        assert!(reproduces_patch(&del_down, &del_contents));

        // expand_file_to_line on old line 5 (above @@ -5,0 +6,3 @@) reaches line 5 without off-by-one.
        let revealed = expand_file_to_line(&add_file, &add_contents, 5, Some(Side::Deletions)).unwrap();
        assert!(revealed.hunks[0].lines.iter().any(|l| l.old_line == Some(5) && l.new_line == Some(5)));
        assert!(reproduces_patch(&revealed, &add_contents));
    }
}
