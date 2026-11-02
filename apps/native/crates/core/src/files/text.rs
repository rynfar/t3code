//! Line layout for the source preview: where each line starts (built once off the UI thread so
//! the view never scans the file), `clampFileLine` from `FilePreviewPanel.tsx`, and the display
//! form of one line with Pierre's `--diffs-tab-size` of 2 and a cap on very long lines.

use std::ops::Range;

/// Lines longer than this are cut for display: shaping a minified megabyte on one line would
/// stall every frame that draws it.
pub const MAX_LINE_CHARS: usize = 4000;
const TAB_SIZE: usize = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineIndex {
    /// Byte range of each line, without its terminator.
    lines: Vec<Range<usize>>,
    /// Lines a reveal can target, counting a final empty line after a trailing newline.
    addressable: usize,
    /// The widest line in characters (capped), for the horizontal scroll extent.
    pub max_columns: usize,
}

impl LineIndex {
    /// Splits on `\n`, `\r\n` and a lone `\r`, as `clampFileLine` counts.
    pub fn new(text: &str) -> LineIndex {
        let bytes = text.as_bytes();
        let mut lines = Vec::new();
        let mut start = 0;
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'\n' => {
                    lines.push(start..index);
                    start = index + 1;
                }
                b'\r' => {
                    lines.push(start..index);
                    if bytes.get(index + 1) == Some(&b'\n') {
                        index += 1;
                    }
                    start = index + 1;
                }
                _ => {}
            }
            index += 1;
        }
        lines.push(start..bytes.len());
        let addressable = lines.len();
        // A trailing newline ends the last line; it does not draw an empty one.
        if lines.len() > 1 && lines.last().is_some_and(Range::is_empty) {
            lines.pop();
        }
        let max_columns = lines
            .iter()
            .map(|range| {
                let line = &text[range.clone()];
                if line.len() > MAX_LINE_CHARS { MAX_LINE_CHARS + 1 } else { display_columns(line) }
            })
            .max()
            .unwrap_or(0);
        LineIndex { lines, addressable, max_columns }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn range(&self, line: usize) -> Option<Range<usize>> {
        self.lines.get(line).cloned()
    }

    /// Digits in the largest line number: the gutter's width.
    pub fn digits(&self) -> usize {
        self.lines.len().max(1).to_string().len()
    }

    /// `clampFileLine`, 0-based and limited to drawn lines.
    pub fn reveal_index(&self, requested_line: u64) -> usize {
        let line = (requested_line.max(1) as usize).min(self.addressable);
        (line - 1).min(self.lines.len().saturating_sub(1))
    }
}

fn display_columns(line: &str) -> usize {
    line.chars().fold(0, |column, ch| if ch == '\t' { column + TAB_SIZE - column % TAB_SIZE } else { column + 1 })
}

/// One line as drawn: tabs expanded to the next stop, cut at `MAX_LINE_CHARS` with an ellipsis.
/// `runs` are byte ranges into `line` and come back as ranges into the result.
pub fn expand_tabs<T: Clone>(line: &str, runs: &[(Range<usize>, T)]) -> (String, Vec<(Range<usize>, T)>) {
    let mut text = String::with_capacity(line.len().min(MAX_LINE_CHARS * 4));
    // offsets[i] is where byte i of `line` lands in `text`, for every char boundary we keep.
    let mut offsets: Vec<(usize, usize)> = Vec::new();
    let mut column = 0;
    let mut cut = line.len();
    for (count, (index, ch)) in line.char_indices().enumerate() {
        if count == MAX_LINE_CHARS {
            cut = index;
            break;
        }
        offsets.push((index, text.len()));
        if ch == '\t' {
            let width = TAB_SIZE - column % TAB_SIZE;
            text.extend(std::iter::repeat_n(' ', width));
            column += width;
        } else {
            text.push(ch);
            column += 1;
        }
    }
    offsets.push((cut, text.len()));
    let map = |byte: usize| -> usize {
        let byte = byte.min(cut);
        match offsets.binary_search_by_key(&byte, |(source, _)| *source) {
            Ok(found) => offsets[found].1,
            Err(next) => offsets.get(next).map_or(text.len(), |(_, target)| *target),
        }
    };
    let mapped = runs
        .iter()
        .filter_map(|(range, style)| {
            let mapped = map(range.start)..map(range.end);
            (!mapped.is_empty()).then(|| (mapped, style.clone()))
        })
        .collect();
    if cut < line.len() {
        text.push('…');
    }
    (text, mapped)
}
