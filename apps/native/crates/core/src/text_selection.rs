//! Selecting plain text the way a browser selects the web's: `CommandOutputPanel.tsx` (and every
//! transcript row) leaves its text to native DOM selection. A press places a caret or takes the
//! word or line under the pointer; a drag or shift-click grows the selection from that unit by the
//! same granularity. Offsets are UTF-8 byte offsets into the drawn text. The view turns pointer
//! positions into a [`Hit`], and paints and copies [`Selection::range`].

use std::ops::Range;

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

    /// The same selection after the text changed from `old` to `new`: kept while everything up to
    /// its end is unchanged (output appended or redrawn below it), moved when a bounded tail
    /// dropped its oldest lines, and `None` once the selected text itself was redrawn or dropped.
    pub fn remap(&self, old: &str, new: &str) -> Option<Self> {
        let range = self.range();
        if range.end <= common_prefix(old, new) {
            return Some(self.clone());
        }
        // A tail that drops its oldest lines starts at a line of the old text, and at least as
        // far in as the old text outgrew the new one.
        let lowest = old.len().saturating_sub(new.len());
        let shift = (lowest..=range.start)
            .filter(|&at| at == lowest || old.as_bytes()[at - 1] == b'\n')
            .find(|&at| old.is_char_boundary(at) && new.as_bytes().starts_with(&old.as_bytes()[at..range.end]))?;
        let moved = |range: &Range<usize>| range.start - shift..range.end - shift;
        Some(Selection { anchor: moved(&self.anchor), head: moved(&self.head), granularity: self.granularity })
    }
}

fn unit(text: &str, hit: Hit, granularity: Granularity) -> Range<usize> {
    match granularity {
        Granularity::Char => {
            let caret = text.floor_char_boundary(hit.caret);
            caret..caret
        }
        Granularity::Word => word_range_at(text, hit.under),
        Granularity::Line => line_range_at(text, hit.under),
    }
}

/// The longest shared prefix, ending on a character boundary.
fn common_prefix(a: &str, b: &str) -> usize {
    let same = a.bytes().zip(b.bytes()).take_while(|(a, b)| a == b).count();
    a.floor_char_boundary(same)
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

/// What a double-click on the character at `index` selects: its word, its run of spaces, or the
/// punctuation mark alone. Past a line's end it is the line's last character; an empty line
/// selects nothing.
pub fn word_range_at(text: &str, index: usize) -> Range<usize> {
    let mut index = text.floor_char_boundary(index);
    if matches!(text[index..].chars().next(), None | Some('\n')) {
        match text[..index].chars().next_back() {
            Some(previous) if previous != '\n' => index -= previous.len_utf8(),
            _ => return index..index,
        }
    }
    let Some(character) = text[index..].chars().next() else { return index..index };
    let kind = kind(character);
    if kind == Kind::Other {
        return index..index + character.len_utf8();
    }
    let same = |other: &char| self::kind(*other) == kind;
    let start = index - text[..index].chars().rev().take_while(same).map(char::len_utf8).sum::<usize>();
    let end = index + text[index..].chars().take_while(same).map(char::len_utf8).sum::<usize>();
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
    fn appended_or_redrawn_output_below_keeps_the_selection() {
        let old = "compiling a\ncompiling b\n10%";
        let mut selection = Selection::press(old, hit(at(old, "a")), Granularity::Word);
        selection.extend(old, hit(at(old, "b\n")));
        let appended = format!("{old}\ncompiling c");
        assert_eq!(selection.remap(old, &appended), Some(selection.clone()));
        // A progress line redrawn below the selection leaves it alone.
        let redrawn = "compiling a\ncompiling b\n20%";
        assert_eq!(selection.remap(old, redrawn).map(|kept| kept.text(redrawn).to_owned()), Some("a\ncompiling b".into()));
        // Redrawing the selected text drops it.
        let rewritten = "compiling a\nCOMPILED b\n10%";
        assert_eq!(selection.remap(old, rewritten), None);
    }

    /// Past its cap the tail drops its oldest lines; a selection below them moves with its text.
    #[test]
    fn a_tail_that_drops_its_oldest_lines_moves_the_selection() {
        let line = |n: usize| format!("line {n:02}\n");
        let mut state = TerminalOutputState::default();
        for n in 0..6 {
            state.append(&line(n), 64);
        }
        let old = terminal_output_display(&state.text, &DARK_PALETTE).text;
        let mut selection = Selection::press(&old, hit(at(&old, "line 04")), Granularity::Line);
        selection.extend(&old, hit(at(&old, "line 05")));
        assert_eq!(selection.text(&old), "line 04\nline 05");
        for n in 6..9 {
            state.append(&line(n), 64);
        }
        assert!(state.truncated);
        let new = terminal_output_display(&state.text, &DARK_PALETTE).text;
        assert!(!new.starts_with(&old[..old.find('\n').unwrap()]), "the oldest lines are gone: {new:?}");
        let moved = selection.remap(&old, &new).expect("the selected lines are still there");
        assert_eq!(moved.text(&new), "line 04\nline 05");
        // Selected lines that scrolled out of the tail take the selection with them.
        let first = Selection::press(&old, hit(0), Granularity::Line);
        assert_eq!(first.remap(&old, &new), None);
    }
}
