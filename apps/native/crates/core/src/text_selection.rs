//! Selecting plain text the way a browser selects the web's: `CommandOutputPanel.tsx` (and every
//! transcript row) leaves its text to native DOM selection. A press places a caret or takes the
//! word or line under the pointer; a drag or shift-click grows the selection from that unit by the
//! same granularity. Offsets are UTF-8 byte offsets into the drawn text, and every edge falls on a
//! grapheme boundary. The view turns pointer positions into a [`Hit`], and paints and copies
//! [`Selection::range`].

use std::ops::Range;

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// What a press takes and a drag then grows by: characters, words (double-click) or lines
/// (triple-click).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Char,
    Word,
    Line,
}

impl Granularity {
    /// From a press's click count; a fourth click and beyond stays on lines.
    pub fn from_clicks(clicks: usize) -> Self {
        match clicks {
            0 | 1 => Granularity::Char,
            2 => Granularity::Word,
            _ => Granularity::Line,
        }
    }
}

/// Where the pointer is in the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// The character boundary nearest the pointer.
    pub caret: usize,
    /// The character under the pointer, or its line's end when the pointer is past the last one.
    pub under: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The unit the gesture started on; it stays selected whichever way the pointer moves.
    anchor: Range<usize>,
    /// The unit under the pointer now.
    head: Range<usize>,
    granularity: Granularity,
}

impl Selection {
    /// A press: a caret, or the word or line under the pointer.
    pub fn press(text: &str, hit: Hit, granularity: Granularity) -> Self {
        let unit = unit(text, hit, granularity);
        Selection { anchor: unit.clone(), head: unit, granularity }
    }

    /// Select all.
    pub fn all(text: &str) -> Self {
        Selection { anchor: 0..0, head: text.len()..text.len(), granularity: Granularity::Char }
    }

    /// A drag or shift-click: from the anchor through the unit under the pointer. Returns whether
    /// the selected range changed.
    pub fn extend(&mut self, text: &str, hit: Hit) -> bool {
        let before = self.range();
        self.head = unit(text, hit, self.granularity);
        self.range() != before
    }

    pub fn range(&self) -> Range<usize> {
        self.anchor.start.min(self.head.start)..self.anchor.end.max(self.head.end)
    }

    pub fn is_empty(&self) -> bool {
        self.range().is_empty()
    }

    /// The selected text, as the clipboard gets it.
    pub fn text<'a>(&self, text: &'a str) -> &'a str {
        text.get(self.range()).unwrap_or_default()
    }

    /// The same selection after the text changed from `old` to `new`, where `dropped` bytes left
    /// the front in between (a bounded tail; zero for text that only grows or is redrawn). It moves
    /// with its text, and is `None` once that text was dropped or redrawn.
    pub fn remap(&self, old: &str, new: &str, dropped: usize) -> Option<Self> {
        let range = self.range();
        let start = range.start.checked_sub(dropped)?;
        if new.get(start..start + range.len())? != old.get(range)? {
            return None;
        }
        let moved = |range: &Range<usize>| range.start - dropped..range.end - dropped;
        Some(Selection { anchor: moved(&self.anchor), head: moved(&self.head), granularity: self.granularity })
    }
}

fn unit(text: &str, hit: Hit, granularity: Granularity) -> Range<usize> {
    match granularity {
        Granularity::Char => {
            let caret = grapheme_start(text, hit.caret);
            caret..caret
        }
        Granularity::Word => word_range_at(text, hit.under),
        Granularity::Line => line_range_at(text, hit.under),
    }
}

/// The start of the grapheme `index` falls in: a caret never splits a base from its marks.
fn grapheme_start(text: &str, index: usize) -> usize {
    let index = text.floor_char_boundary(index);
    let mut cursor = GraphemeCursor::new(index, text.len(), true);
    match cursor.is_boundary(text, 0) {
        Ok(false) => cursor.prev_boundary(text, 0).ok().flatten().unwrap_or(0),
        _ => index,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    Space,
    Newline,
    Other,
}

fn kind(character: char) -> Kind {
    if character == '\n' {
        Kind::Newline
    } else if character.is_alphanumeric() || character == '_' {
        Kind::Word
    } else if character.is_whitespace() {
        Kind::Space
    } else {
        Kind::Other
    }
}

/// A grapheme's kind is its base character's: combining marks and emoji modifiers ride along.
fn grapheme_kind(grapheme: &str) -> Kind {
    grapheme.chars().next().map_or(Kind::Other, kind)
}

/// What a double-click on the grapheme at `index` selects: its word, its run of spaces, or the
/// punctuation mark or symbol alone. Past a line's end it is the line's last grapheme; an empty
/// line selects nothing.
pub fn word_range_at(text: &str, index: usize) -> Range<usize> {
    let mut index = grapheme_start(text, index);
    if matches!(text[index..].chars().next(), None | Some('\n')) {
        match text[..index].graphemes(true).next_back() {
            Some(previous) if previous != "\n" => index -= previous.len(),
            _ => return index..index,
        }
    }
    let Some(grapheme) = text[index..].graphemes(true).next() else { return index..index };
    let kind = grapheme_kind(grapheme);
    if kind == Kind::Other {
        return index..index + grapheme.len();
    }
    let same = |other: &&str| grapheme_kind(other) == kind;
    let start = index - text[..index].graphemes(true).rev().take_while(same).map(str::len).sum::<usize>();
    let end = index + text[index..].graphemes(true).take_while(same).map(str::len).sum::<usize>();
    start..end
}

/// What a triple-click on the character at `index` selects: its line, without the newline.
pub fn line_range_at(text: &str, index: usize) -> Range<usize> {
    let index = text.floor_char_boundary(index);
    let start = text[..index].rfind('\n').map_or(0, |at| at + 1);
    let end = text[index..].find('\n').map_or(text.len(), |at| index + at);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_output::{DARK_PALETTE, TerminalOutputState, terminal_output_display};

    fn hit(at: usize) -> Hit {
        Hit { caret: at, under: at }
    }

    fn at(text: &str, needle: &str) -> usize {
        text.find(needle).unwrap()
    }

    #[test]
    fn a_double_click_takes_the_word_the_spaces_or_one_mark() {
        let text = "let foo_bar  = café;\n\nnext";
        let word = |index| &text[word_range_at(text, index)];
        assert_eq!(word(at(text, "foo") + 4), "foo_bar");
        assert_eq!(word(at(text, "  =") + 1), "  ");
        assert_eq!(word(at(text, "=")), "=");
        assert_eq!(word(at(text, "é")), "café");
        // Past a line's end: its last character, here the `;`.
        assert_eq!(word(at(text, "\n")), ";");
        // An empty line has nothing to take.
        assert_eq!(word_range_at(text, at(text, "\n\n") + 1), at(text, "\n\n") + 1..at(text, "\n\n") + 1);
        assert_eq!(word(text.len()), "next");
    }

    #[test]
    fn a_triple_click_takes_the_line_without_its_newline() {
        let text = "first\nsecond\n\nlast";
        let line = |index| &text[line_range_at(text, index)];
        assert_eq!(line(0), "first");
        assert_eq!(line(at(text, "cond")), "second");
        assert_eq!(line(at(text, "\n\n")), "second", "past the end of `second`");
        assert_eq!(line(at(text, "\n\n") + 1), "");
        assert_eq!(line(text.len()), "last");
    }

    #[test]
    fn a_drag_runs_from_the_caret_either_way() {
        let text = "alpha beta\ngamma";
        let mut selection = Selection::press(text, hit(2), Granularity::Char);
        assert!(selection.is_empty());
        assert!(selection.extend(text, hit(at(text, "mma"))));
        assert_eq!(selection.text(text), "pha beta\nga");
        assert!(!selection.extend(text, hit(at(text, "mma"))), "the same pointer changes nothing");
        // Back past the anchor: the range flips around it.
        selection.extend(text, hit(0));
        assert_eq!(selection.text(text), "al");
    }

    #[test]
    fn a_drag_after_a_double_click_grows_by_whole_words_and_keeps_the_first() {
        let text = "one two three\nfour five";
        let mut selection = Selection::press(text, hit(at(text, "wo")), Granularity::Word);
        assert_eq!(selection.text(text), "two");
        selection.extend(text, Hit { caret: at(text, "ve"), under: at(text, "ve") });
        assert_eq!(selection.text(text), "two three\nfour five");
        selection.extend(text, hit(1));
        assert_eq!(selection.text(text), "one two");
    }

    #[test]
    fn a_drag_after_a_triple_click_grows_by_lines() {
        let text = "one\ntwo\nthree";
        let mut selection = Selection::press(text, hit(at(text, "w")), Granularity::Line);
        assert_eq!(selection.text(text), "two");
        selection.extend(text, hit(at(text, "ee")));
        assert_eq!(selection.text(text), "two\nthree");
    }

    #[test]
    fn a_shift_click_extends_from_the_anchor() {
        let text = "0123456789";
        let mut selection = Selection::press(text, hit(3), Granularity::Char);
        selection.extend(text, hit(6));
        // A later shift-click keeps the press it started from.
        selection.extend(text, hit(8));
        assert_eq!(selection.range(), 3..8);
        selection.extend(text, hit(1));
        assert_eq!(selection.range(), 1..3);
    }

    #[test]
    fn select_all_and_a_caret_off_a_char_boundary() {
        let text = "naïve";
        assert_eq!(Selection::all(text).text(text), "naïve");
        // Inside `ï`: the caret falls back to the boundary before it.
        let selection = Selection::press(text, hit(3), Granularity::Char);
        assert_eq!(selection.range(), 2..2);
    }

    /// Copying a selection that crosses styled spans gets the text without its escapes.
    #[test]
    fn copy_takes_plain_text_across_styled_spans() {
        let display = terminal_output_display("\u{1b}[31merror\u{1b}[0m: \u{1b}[1mbuild\u{1b}[0m failed\n", &DARK_PALETTE);
        assert_eq!(display.text, "error: build failed");
        assert_eq!(display.styles.iter().map(|(range, _)| range.clone()).collect::<Vec<_>>(), vec![0..5, 7..12]);
        let mut selection = Selection::press(&display.text, hit(2), Granularity::Char);
        selection.extend(&display.text, hit(9));
        assert_eq!(selection.text(&display.text), "ror: bu");
        selection.extend(&display.text, hit(display.text.len()));
        assert_eq!(selection.text(&display.text), "ror: build failed", "the dropped final newline is not copied");
    }

    #[test]
    fn a_double_click_keeps_marks_and_modifiers_with_their_base() {
        // `e` plus a combining acute, and a thumbs-up with a skin tone: one grapheme each.
        let text = "cafe\u{301} au lait 👍🏽!";
        let word = |index| &text[word_range_at(text, index)];
        assert_eq!(word(at(text, "f")), "cafe\u{301}");
        assert_eq!(word(at(text, "\u{301}")), "cafe\u{301}", "on the mark itself");
        assert_eq!(word(at(text, "👍")), "👍🏽");
        assert_eq!(word(at(text, "🏽")), "👍🏽", "on the modifier itself");
        // A caret that lands between a base and its mark falls back before the base.
        let caret = Selection::press(text, hit(at(text, "\u{301}")), Granularity::Char);
        assert_eq!(caret.range(), at(text, "e\u{301}")..at(text, "e\u{301}"));
    }

    #[test]
    fn appended_or_redrawn_output_below_keeps_the_selection() {
        let old = "compiling a\ncompiling b\n10%";
        let mut selection = Selection::press(old, hit(at(old, "a")), Granularity::Word);
        selection.extend(old, hit(at(old, "b\n")));
        let appended = format!("{old}\ncompiling c");
        assert_eq!(selection.remap(old, &appended, 0), Some(selection.clone()));
        // A progress line redrawn below the selection leaves it alone.
        let redrawn = "compiling a\ncompiling b\n20%";
        assert_eq!(selection.remap(old, redrawn, 0).map(|kept| kept.text(redrawn).to_owned()), Some("a\ncompiling b".into()));
        // Redrawing the selected text drops it.
        let rewritten = "compiling a\nCOMPILED b\n10%";
        assert_eq!(selection.remap(old, rewritten, 0), None);
    }

    /// Folds `chunks` in order, returning each drawn text with its origin.
    fn fold(chunks: &[&str], max_chars: usize) -> Vec<(String, usize)> {
        let mut state = TerminalOutputState::default();
        chunks
            .iter()
            .map(|chunk| {
                state.append(chunk, max_chars);
                (terminal_output_display(&state.text, &DARK_PALETTE).text, state.origin)
            })
            .collect()
    }

    fn follow(selection: &Selection, (old, old_origin): &(String, usize), (new, new_origin): &(String, usize)) -> Option<Selection> {
        selection.remap(old, new, new_origin - old_origin)
    }

    /// Past its cap the tail drops its oldest lines; a selection below them moves with its text.
    #[test]
    fn a_tail_that_drops_its_oldest_lines_moves_the_selection() {
        let lines: Vec<String> = (0..9).map(|n| format!("line {n:02}\n")).collect();
        let chunks: Vec<&str> = lines.iter().map(String::as_str).collect();
        let texts = fold(&chunks, 64);
        let (old, new) = (&texts[5], &texts[8]);
        assert!(!new.0.starts_with("line 00"), "the oldest lines are gone: {:?}", new.0);
        let mut selection = Selection::press(&old.0, hit(at(&old.0, "line 04")), Granularity::Line);
        selection.extend(&old.0, hit(at(&old.0, "line 05")));
        let moved = follow(&selection, old, new).expect("the selected lines are still there");
        assert_eq!(moved.text(&new.0), "line 04\nline 05");
        // Selected lines that scrolled out of the tail take the selection with them.
        let first = Selection::press(&old.0, hit(0), Granularity::Line);
        assert_eq!(follow(&first, old, new), None);
    }

    /// Identical lines can't tell a dropped front from a redrawn end; the fold's origin can.
    #[test]
    fn a_selection_follows_its_line_among_identical_ones() {
        let texts = fold(&["tick\ntick\ntick", "\ndone"], 14);
        assert_eq!(texts[0].0, "tick\ntick\ntick");
        assert_eq!(texts[1].0, "tick\ntick\ndone");
        let second = Selection::press(&texts[0].0, hit(5), Granularity::Line);
        assert_eq!(second.range(), 5..9);
        assert_eq!(follow(&second, &texts[0], &texts[1]).map(|moved| moved.range()), Some(0..4));
    }

    /// Output with no newlines loses its front mid-line.
    #[test]
    fn a_selection_follows_a_line_cut_mid_way() {
        let texts = fold(&["abcdefghij", "klmnopqrst"], 16);
        assert_eq!(texts[1].0, "efghijklmnopqrst");
        let selection = Selection::press(&texts[0].0, hit(at(&texts[0].0, "ghij")), Granularity::Char);
        let mut selection = selection;
        selection.extend(&texts[0].0, hit(10));
        let moved = follow(&selection, &texts[0], &texts[1]).expect("`ghij` is still drawn");
        assert_eq!(moved.text(&texts[1].0), "ghij");
    }

    /// Remapping compares the selected bytes once, whatever repeats around them.
    #[test]
    fn a_redraw_under_a_selection_among_many_repeated_lines_drops_it() {
        let old = format!("{}X", "a\n".repeat(32 * 1024));
        let new = format!("{}Y", "a\n".repeat(32 * 1024));
        let selection = Selection::press(&old, hit(old.len() - 1), Granularity::Word);
        assert_eq!(selection.text(&old), "X");
        assert_eq!(selection.remap(&old, &new, 0), None);
        assert_eq!(selection.remap(&old, &new, 2), None);
    }
}
