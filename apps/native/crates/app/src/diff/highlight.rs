use std::collections::VecDeque;
use std::ops::Range;

use gpui::{App, HighlightStyle, SharedString, combine_highlights};
use gpui_component::highlighter::SyntaxHighlighter;
use gpui_component::{ActiveTheme, Rope};
use t3_core::diff::{DiffLine, FileDiff, FileHunks, LineKind, cut_line_note_start};
use t3_core::files::syntax_language;

use crate::files::code::syntax_runs;
use crate::selectable_text::Highlights;
use crate::theme::{hsla, palette};

const CHUNK_LINES: usize = 32;
const MAX_CHUNKS: usize = 32;
const CHUNK_BYTES: usize = 32 * 1024;
const WORD_MAX_BYTES: usize = 512;
const PAIR_SEARCH_LIMIT: usize = 64;

#[derive(Default)]
pub(crate) struct DiffHighlightCache {
    chunks: VecDeque<Chunk>,
    theme_revision: Option<usize>,
    frame_chunks: Option<usize>,
}

struct Chunk {
    hunks: FileHunks,
    hunk: usize,
    start: usize,
    old: bool,
    language: Option<&'static str>,
    drawn: bool,
    highlighter: Option<SyntaxHighlighter>,
    ranges: Vec<Range<usize>>,
    painted: Vec<Option<(SharedString, Highlights)>>,
}

impl DiffHighlightCache {
    /// Guide stop cards lay out all their lines, including the ones outside the viewport.
    pub(crate) fn begin_bounded_frame(&mut self) {
        self.frame_chunks = Some(0);
        for chunk in &mut self.chunks {
            chunk.drawn = false;
        }
    }

    pub(crate) fn line(
        &mut self,
        file: &FileDiff,
        hunk: usize,
        index: usize,
        old: bool,
        cx: &App,
    ) -> Option<(SharedString, Highlights)> {
        let revision = crate::theme::revision(cx);
        if self.theme_revision != Some(revision) {
            self.theme_revision = Some(revision);
            self.chunks.clear();
        }
        let lines = &file.hunks.get(hunk)?.lines;
        let line = lines.get(index)?;
        let start = index / CHUNK_LINES * CHUNK_LINES;
        let language = syntax_language(if old {
            file.previous_path()
        } else {
            file.path()
        });
        let hit = self.chunks.iter().position(|chunk| {
            chunk.hunk == hunk
                && chunk.start == start
                && chunk.old == old
                && chunk.language == language
                && chunk.hunks.shares_with(&file.hunks)
        });
        if let Some(n) = &mut self.frame_chunks
            && !hit.is_some_and(|hit| self.chunks[hit].drawn)
        {
            if *n == MAX_CHUNKS {
                return None;
            }
            *n += 1;
        }
        if hit != Some(0) {
            let chunk = hit
                .and_then(|hit| self.chunks.remove(hit))
                .unwrap_or_else(|| Chunk::new(&file.hunks, hunk, start, old, language));
            self.chunks.push_front(chunk);
            self.chunks.truncate(MAX_CHUNKS);
        }
        let chunk = self.chunks.front_mut().expect("current chunk");
        chunk.drawn = true;
        let offset = index - start;
        Some(
            chunk.painted[offset]
                .get_or_insert_with(|| {
                    let text = display_text(line);
                    let runs = syntax_runs(
                        chunk.highlighter.as_ref(),
                        text,
                        chunk.ranges[offset].clone(),
                        &*cx.theme().highlight_theme,
                    );
                    let p = palette(cx);
                    let word_runs = paired_line(lines, index).into_iter().flat_map(|paired| {
                        let fill = match line.kind {
                            LineKind::Addition => p.diff_addition,
                            _ => p.diff_deletion,
                        };
                        word_changes(text, display_text(paired))
                            .into_iter()
                            .map(move |range| {
                                (
                                    range,
                                    HighlightStyle {
                                        background_color: Some(hsla(fill).opacity(0.29)),
                                        ..Default::default()
                                    },
                                )
                            })
                    });
                    let cut_note = cut_line_note_start(text).map(|offset| {
                        (
                            offset..text.len(),
                            HighlightStyle {
                                color: Some(hsla(p.warning_foreground)),
                                background_color: Some(hsla(p.warning).opacity(0.14)),
                                ..Default::default()
                            },
                        )
                    });
                    let spans =
                        combine_highlights(runs, word_runs.chain(cut_note)).collect::<Vec<_>>();
                    (text.to_owned().into(), spans.into())
                })
                .clone(),
        )
    }
}

impl Chunk {
    fn new(
        hunks: &FileHunks,
        hunk: usize,
        start: usize,
        old: bool,
        language: Option<&'static str>,
    ) -> Self {
        let lines = &hunks[hunk].lines;
        let lines = &lines[start..lines.len().min(start + CHUNK_LINES)];
        let mut source = String::new();
        let ranges = lines
            .iter()
            .map(|line| {
                let begin = source.len();
                let text = display_text(line);
                let on_side = if old {
                    line.kind != LineKind::Addition
                } else {
                    line.kind != LineKind::Deletion
                };
                if language.is_some() && on_side && begin + text.len() < CHUNK_BYTES {
                    source.push_str(text);
                    let end = source.len();
                    source.push('\n');
                    begin..end
                } else {
                    begin..begin
                }
            })
            .collect();
        let highlighter = language.filter(|_| !source.is_empty()).map(|language| {
            let mut highlighter = SyntaxHighlighter::new(language);
            highlighter.update(None, &Rope::from_str(&source), None);
            highlighter
        });
        Self {
            hunks: hunks.clone(),
            hunk,
            start,
            old,
            language,
            drawn: false,
            highlighter,
            ranges,
            painted: vec![None; lines.len()],
        }
    }
}

pub(crate) fn display_text(line: &DiffLine) -> &str {
    line.text.strip_suffix('\r').unwrap_or(&line.text)
}

/// Pairs lines by ordinal within a replacement block, like the split diff rows.
fn paired_line(lines: &[DiffLine], index: usize) -> Option<&DiffLine> {
    let kind = lines.get(index)?.kind;
    if kind == LineKind::Context {
        return None;
    }
    let group_start = |index, kind| {
        let mut start: usize = index;
        while start > 0 && index - start < PAIR_SEARCH_LIMIT && lines[start - 1].kind == kind {
            start -= 1;
        }
        (index - start < PAIR_SEARCH_LIMIT).then_some(start)
    };
    let start = group_start(index, kind)?;
    let offset = index - start;
    match kind {
        LineKind::Addition if start > 0 && lines[start - 1].kind == LineKind::Deletion => {
            let deletion = group_start(start - 1, LineKind::Deletion)? + offset;
            (deletion < start).then(|| &lines[deletion])
        }
        LineKind::Deletion => {
            let first_addition = (index + 1..lines.len().min(index + PAIR_SEARCH_LIMIT))
                .find(|&i| lines[i].kind != LineKind::Deletion)?;
            let additions = lines.get(first_addition..=first_addition + offset)?;
            additions
                .iter()
                .all(|line| line.kind == LineKind::Addition)
                .then(|| &additions[offset])
        }
        _ => None,
    }
}

fn tokens(text: &str) -> Vec<Range<usize>> {
    let mut tokens: Vec<Range<usize>> = Vec::new();
    let mut previous = None;
    for (start, character) in text.char_indices() {
        let class = if character.is_alphanumeric() || character == '_' {
            Some(false)
        } else if character.is_whitespace() {
            Some(true)
        } else {
            None
        };
        let end = start + character.len_utf8();
        if class.is_some() && previous == class {
            tokens.last_mut().expect("previous token").end = end;
        } else {
            tokens.push(start..end);
        }
        previous = class;
    }
    tokens
}

fn word_changes(source: &str, paired: &str) -> Vec<Range<usize>> {
    if source == paired || source.len().max(paired.len()) > WORD_MAX_BYTES {
        return Vec::new();
    }
    let left = tokens(source);
    let right = tokens(paired);
    if left.len().max(right.len()) > 96 {
        return Vec::new();
    }
    let matches = |i: usize, j: usize| source[left[i].clone()] == paired[right[j].clone()];
    let width = right.len() + 1;
    let mut lcs = vec![0_u8; (left.len() + 1) * width];
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            lcs[i * width + j] = if matches(i, j) {
                1 + lcs[(i + 1) * width + j + 1]
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut changes: Vec<Range<usize>> = Vec::new();
    while i < left.len() {
        if j < right.len() && matches(i, j) {
            i += 1;
            j += 1;
        } else if j < right.len() && lcs[i * width + j + 1] > lcs[(i + 1) * width + j] {
            j += 1;
        } else {
            let range = &left[i];
            if let Some(previous) = changes
                .last_mut()
                .filter(|previous| previous.end == range.start)
            {
                previous.end = range.end;
            } else {
                changes.push(range.clone());
            }
            i += 1;
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use t3_core::diff::{ParsedPatch, parse_patch};

    #[test]
    fn word_changes_preserve_shared_tokens_and_utf8_boundaries() {
        let left = "let café = old(42);";
        let right = "let thé = new(42);";
        fn changed<'a>(source: &'a str, other: &str) -> Vec<&'a str> {
            word_changes(source, other)
                .into_iter()
                .map(|r| &source[r])
                .collect()
        }
        assert_eq!(changed(left, right), ["café", "old"]);
        assert_eq!(changed(right, left), ["thé", "new"]);
        assert!(word_changes("a b", "a new b").is_empty());
    }

    #[gpui::test]
    fn syntax_colors_and_word_backgrounds_share_cached_non_overlapping_spans(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let ParsedPatch::Files(files) = parse_patch(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-let a = false;\n+let a = true;\n"
            ) else { panic!("file patch") };
            let mut cache = DiffHighlightCache::default();
            let (text, runs) = cache.line(&files[0], 0, 1, false, cx).unwrap();
            let (_, cached) = cache.line(&files[0], 0, 1, false, cx).unwrap();
            assert!(std::sync::Arc::ptr_eq(&runs, &cached));
            assert_eq!(text.as_ref(), "let a = true;");
            assert!(runs.iter().any(|(range, style)| &text[range.clone()] == "let" && style.color.is_some()));
            assert!(runs.iter().any(|(range, style)| &text[range.clone()] == "true" && style.background_color.is_some()));
            assert!(runs.windows(2).all(|runs| runs[0].0.end <= runs[1].0.start));
        });
    }
}
