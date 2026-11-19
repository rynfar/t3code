//! Review guide tour navigation, stop file lists, lettered notes, and diff excerpt slicing.
//! Ports `packages/client-runtime/src/reviewGuideTour.ts`.

use std::collections::{HashMap, HashSet};

use crate::diff::{DiffLine, FileDiff, LineKind};

use super::wire::{ReviewGuideExcerpt, ReviewGuideNote, ReviewGuideStop};

/// `stepTourPosition`.
pub fn step_tour_position(current: usize, delta: isize, stop_count: usize) -> usize {
    if stop_count == 0 {
        return 0;
    }
    let next = current as isize + delta;
    next.clamp(0, (stop_count - 1) as isize) as usize
}

/// `stopFilePaths`: unique paths referenced by a stop's excerpts followed by its files.
pub fn stop_file_paths(stop: &ReviewGuideStop) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for path in stop.excerpts.iter().map(|e| &e.path).chain(stop.files.iter().map(|f| &f.path)) {
        if seen.insert(path.as_str()) {
            paths.push(path.clone());
        }
    }
    paths
}

/// `stopSubline`.
pub fn stop_subline(stop: &ReviewGuideStop) -> String {
    match stop_file_paths(stop).len() {
        0 => "Context, no code".into(),
        1 => "1 file".into(),
        count => format!("{count} files"),
    }
}

/// `noteMarker`: A..Z, then 27, 28, ...
pub fn note_marker(index: usize) -> String {
    if index < 26 {
        ((b'A' + index as u8) as char).to_string()
    } else {
        (index + 1).to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetteredNote {
    pub marker: String,
    pub note: ReviewGuideNote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcerptLineMarker {
    pub marker: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetteredStopNotes {
    pub lettered: Vec<LetteredNote>,
    /// Per excerpt index, line number -> markers on that line.
    pub by_excerpt: Vec<Vec<(u32, Vec<ExcerptLineMarker>)>>,
}

/// `letterStopNotes`.
pub fn letter_stop_notes(stop: &ReviewGuideStop) -> LetteredStopNotes {
    let lettered: Vec<LetteredNote> = stop
        .notes
        .iter()
        .enumerate()
        .map(|(index, note)| LetteredNote { marker: note_marker(index), note: note.clone() })
        .collect();
    let by_excerpt = stop
        .excerpts
        .iter()
        .map(|excerpt| {
            let mut order: Vec<u32> = Vec::new();
            let mut by_line: HashMap<u32, Vec<ExcerptLineMarker>> = HashMap::new();
            for entry in &lettered {
                if entry.note.path != excerpt.path || entry.note.line < excerpt.start || entry.note.line > excerpt.end {
                    continue;
                }
                if !by_line.contains_key(&entry.note.line) {
                    order.push(entry.note.line);
                }
                by_line.entry(entry.note.line).or_default().push(ExcerptLineMarker {
                    marker: entry.marker.clone(),
                    title: entry.note.title.clone(),
                });
            }
            order.into_iter().filter_map(|line| Some((line, by_line.remove(&line)?))).collect()
        })
        .collect();
    LetteredStopNotes { lettered, by_excerpt }
}

/// One extracted line from a `FileDiff` for a guide excerpt, preserving hunk index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcerptDiffRow {
    pub kind: LineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
    pub hunk: usize,
}

/// Extracts the matching diff rows for `excerpt` from `file`, including paired deletions before
/// an addition on `side == "new"` and paired additions after a deletion on `side == "old"` within
/// the same hunk (`excerptRows` in `reviewGuideTour.ts`).
pub fn excerpt_rows(file: &FileDiff, excerpt: &ReviewGuideExcerpt) -> Vec<ExcerptDiffRow> {
    let mut rows: Vec<ExcerptDiffRow> = Vec::new();
    for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
        for line in &hunk.lines {
            rows.push(diff_line_row(line, hunk_ix));
        }
    }
    let line_of = |row: &ExcerptDiffRow| if excerpt.side == "old" { row.old_line } else { row.new_line };
    let in_range = |row: &ExcerptDiffRow| line_of(row).is_some_and(|line| line >= excerpt.start && line <= excerpt.end);
    let Some(mut first) = rows.iter().position(in_range) else {
        return Vec::new();
    };
    let mut last = rows.iter().rposition(in_range).unwrap_or(first);
    let same_hunk = |a: usize, b: usize| rows[a].hunk == rows[b].hunk;
    if excerpt.side != "old" && rows[first].kind == LineKind::Addition {
        while first > 0 && rows[first - 1].kind == LineKind::Deletion && same_hunk(first - 1, first) {
            first -= 1;
        }
    }
    if excerpt.side == "old" && rows[last].kind == LineKind::Deletion {
        while last + 1 < rows.len() && rows[last + 1].kind == LineKind::Addition && same_hunk(last + 1, last) {
            last += 1;
        }
    }
    rows[first..=last].to_vec()
}

fn diff_line_row(line: &DiffLine, hunk: usize) -> ExcerptDiffRow {
    ExcerptDiffRow {
        kind: line.kind,
        old_line: line.old_line,
        new_line: line.new_line,
        text: line.text.to_string(),
        hunk,
    }
}

/// Tracks the reader's position and seen stops across a review guide tour (`guideRunStore.ts`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TourProgress {
    pub position: usize,
    pub seen: HashSet<usize>,
}

impl TourProgress {
    pub fn go_to(&mut self, next: usize, stop_count: usize) {
        if stop_count == 0 {
            self.position = 0;
            return;
        }
        let clamped = next.min(stop_count - 1);
        if clamped > self.position {
            self.seen.insert(self.position);
        }
        self.position = clamped;
    }

    pub fn step(&mut self, delta: isize, stop_count: usize) {
        let next = step_tour_position(self.position, delta, stop_count);
        self.go_to(next, stop_count);
    }

    pub fn finish_current(&mut self) {
        self.seen.insert(self.position);
    }
}

#[cfg(test)]
mod tests {
    use crate::diff::{ParsedPatch, parse_patch};
    use crate::pull_requests::wire::ReviewGuideFile;

    use super::*;

    fn sample_stop() -> ReviewGuideStop {
        ReviewGuideStop {
            title: "State update".into(),
            summary: "Updates the reducer.".into(),
            attention: None,
            excerpts: vec![ReviewGuideExcerpt { path: "src/a.rs".into(), side: "new".into(), start: 21, end: 21, caption: "New branch".into() }],
            files: vec![
                ReviewGuideFile { path: "src/a.rs".into(), note: None },
                ReviewGuideFile { path: "src/b.rs".into(), note: Some("Tests".into()) },
            ],
            notes: vec![
                ReviewGuideNote { path: "src/a.rs".into(), line: 21, title: "Watch ordering".into(), body: "Keep stable".into() },
                ReviewGuideNote { path: "src/b.rs".into(), line: 5, title: "Coverage".into(), body: "Edge case".into() },
            ],
            questions: vec!["Should this be cached?".into()],
        }
    }

    #[test]
    fn tour_stepping_subline_and_lettered_notes_match_web() {
        assert_eq!(step_tour_position(0, -1, 3), 0);
        assert_eq!(step_tour_position(0, 1, 3), 1);
        assert_eq!(step_tour_position(2, 1, 3), 2);
        assert_eq!(note_marker(0), "A");
        assert_eq!(note_marker(25), "Z");
        assert_eq!(note_marker(26), "27");

        let stop = sample_stop();
        assert_eq!(stop_file_paths(&stop), vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(stop_subline(&stop), "2 files");
        let notes = letter_stop_notes(&stop);
        assert_eq!(notes.lettered[0].marker, "A");
        assert_eq!(notes.lettered[1].marker, "B");
        assert_eq!(notes.by_excerpt[0].len(), 1);
        assert_eq!(notes.by_excerpt[0][0].0, 21);
        assert_eq!(notes.by_excerpt[0][0].1[0].marker, "A");

        let mut progress = TourProgress::default();
        progress.step(1, 3);
        assert_eq!(progress.position, 1);
        assert!(progress.seen.contains(&0));
        progress.step(-1, 3);
        assert_eq!(progress.position, 0);
    }

    #[test]
    fn excerpt_rows_include_paired_deletions_within_same_hunk() {
        let patch = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,3 +20,3 @@\n ctx\n-old\n+new\n tail\n";
        let ParsedPatch::Files(files) = parse_patch(patch) else { panic!("files") };
        let rows = excerpt_rows(&files[0], &sample_stop().excerpts[0]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, LineKind::Deletion);
        assert_eq!(rows[1].kind, LineKind::Addition);
    }
}
