//! `hideWhitespaceChanges` from `apps/web/src/lib/diffRendering.ts`: the diff panel applies the
//! whitespace toggle to a tool call's recorded patch itself, since no server diff is involved.
//! Each hunk is diffed again with all whitespace removed, so lines that differ only in whitespace
//! read as context; lines keep their original text.

use super::{DiffLine, FileDiff, Hunk, LineKind, ParsedPatch};

/// Past this many edits in one hunk the hunk is shown as recorded, bounding time and memory.
const MAX_EDITS: usize = 1_000;

pub fn hide_whitespace_changes(patch: &ParsedPatch) -> ParsedPatch {
    match patch {
        ParsedPatch::Files(files) => ParsedPatch::Files(files.iter().map(hide_in_file).collect()),
        other => other.clone(),
    }
}

fn hide_in_file(file: &FileDiff) -> FileDiff {
    let hunks: Vec<Hunk> = file.hunks.iter().map(hide_in_hunk).collect();
    let count = |kind: LineKind| hunks.iter().flat_map(|hunk| &hunk.lines).filter(|line| line.kind == kind).count() as u32;
    FileDiff { additions: count(LineKind::Addition), deletions: count(LineKind::Deletion), hunks, ..file.clone() }
}

fn hide_in_hunk(hunk: &Hunk) -> Hunk {
    let old: Vec<&DiffLine> = hunk.lines.iter().filter(|line| line.kind != LineKind::Addition).collect();
    let new: Vec<&DiffLine> = hunk.lines.iter().filter(|line| line.kind != LineKind::Deletion).collect();
    let strip = |lines: &[&DiffLine]| -> Vec<String> { lines.iter().map(|line| line.text.chars().filter(|c| !c.is_whitespace()).collect()).collect() };
    let Some(edits) = edit_script(&strip(&old), &strip(&new), MAX_EDITS) else { return hunk.clone() };
    let mut lines = Vec::with_capacity(hunk.lines.len());
    let (mut deleted, mut added): (Vec<DiffLine>, Vec<DiffLine>) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    for edit in edits {
        match edit {
            Edit::Equal => {
                lines.append(&mut deleted);
                lines.append(&mut added);
                lines.push(DiffLine { kind: LineKind::Context, old_line: old[i].old_line, ..new[j].clone() });
                i += 1;
                j += 1;
            }
            Edit::Delete => {
                deleted.push(DiffLine { kind: LineKind::Deletion, new_line: None, ..old[i].clone() });
                i += 1;
            }
            Edit::Insert => {
                added.push(DiffLine { kind: LineKind::Addition, old_line: None, ..new[j].clone() });
                j += 1;
            }
        }
    }
    lines.append(&mut deleted);
    lines.append(&mut added);
    Hunk { lines, ..hunk.clone() }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    Equal,
    Delete,
    Insert,
}

/// Myers' shortest edit script from `a` to `b`, or `None` when it needs more than `limit` edits.
fn edit_script<T: PartialEq>(a: &[T], b: &[T], limit: usize) -> Option<Vec<Edit>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let limit = limit.min(a.len() + b.len()) as isize;
    let offset = limit + 1;
    let index = |k: isize| (k + offset) as usize;
    let mut v = vec![0isize; 2 * limit as usize + 3];
    // V as it stood before each round, to walk back from the end.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=limit {
        trace.push(v.clone());
        for k in (-d..=d).step_by(2) {
            let mut x = if k == -d || (k != d && v[index(k - 1)] < v[index(k + 1)]) { v[index(k + 1)] } else { v[index(k - 1)] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[index(k)] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m, index));
            }
        }
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize, index: impl Fn(isize) -> usize) -> Vec<Edit> {
    let mut edits = Vec::new();
    let (mut x, mut y) = (n, m);
    for (d, v) in trace.iter().enumerate().rev() {
        let d = d as isize;
        let k = x - y;
        let previous_k = if k == -d || (k != d && v[index(k - 1)] < v[index(k + 1)]) { k + 1 } else { k - 1 };
        let previous_x = v[index(previous_k)];
        let previous_y = previous_x - previous_k;
        while x > previous_x && y > previous_y {
            edits.push(Edit::Equal);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            edits.push(if x == previous_x { Edit::Insert } else { Edit::Delete });
        }
        x = previous_x;
        y = previous_y;
    }
    edits.reverse();
    edits
}
