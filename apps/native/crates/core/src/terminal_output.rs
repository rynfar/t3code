//! Command output normalization: `packages/shared/src/terminalOutput.ts`, the fold the server and
//! every client apply to command output, and its SGR span styling. Server and client folds must
//! agree, so this is a line-for-line port: only the last `LIVE_LINES` lines are
//! cursor-addressable, SGR resets at line ends make tail cuts safe, and every length counts UTF-16
//! code units as JavaScript strings do, so a cut lands where the server's does.

use std::collections::HashMap;

/// Default command tail, in UTF-16 code units (`TERMINAL_OUTPUT_TAIL_CHARS`).
pub const TERMINAL_OUTPUT_TAIL_CHARS: usize = 64 * 1024;
/// `terminalOutputSpans`' default cap on styled spans.
pub const MAX_SPANS: usize = 2_000;

const LIVE_LINES: usize = 64;
const MAX_PENDING_ESCAPE: usize = 64;
// Strip oldest styles to bound work on color-per-character output.
const MAX_LINE_RUNS: usize = 2_048;

const ESC: u16 = 0x1b;
const NEWLINE: u16 = b'\n' as u16;
const STRING_SEQUENCE_INTRODUCERS: [u16; 5] = [b']' as u16, b'P' as u16, b'X' as u16, b'_' as u16, b'^' as u16];

/// `TerminalOutputState`: the normalized `text` plus what the next chunk folds into.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerminalOutputState {
    /// Fixed lines followed by the live window; contains only SGR escapes.
    pub text: String,
    pub truncated: bool,
    /// An unfinished escape sequence awaiting the next chunk.
    pending: Vec<u16>,
    screen: Screen,
}

#[derive(Debug, Clone, PartialEq)]
struct Screen {
    /// Scrolled-out lines, each ending in a newline (the TypeScript keeps it as head + tail
    /// only to spare V8 copies; only the concatenation matters).
    settled: Vec<u16>,
    /// Never empty.
    lines: Vec<Line>,
    row: usize,
    col: usize,
    pen: String,
    saved: Option<(usize, usize)>,
}

impl Default for Screen {
    fn default() -> Self {
        Screen { settled: Vec::new(), lines: vec![Line::default()], row: 0, col: 0, pen: String::new(), saved: None }
    }
}

/// Canonical SGR parameters; an empty pen is the default style.
#[derive(Debug, Clone, PartialEq)]
struct Run {
    pen: String,
    text: Vec<u16>,
}

/// A line's runs with their total length cached, as `lineLengths` caches it.
#[derive(Debug, Clone, Default, PartialEq)]
struct Line {
    runs: Vec<Run>,
    len: usize,
}

impl Line {
    fn new(runs: Vec<Run>) -> Line {
        let len = runs.iter().map(|run| run.text.len()).sum();
        Line { runs, len }
    }
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xdc00..=0xdfff).contains(&unit)
}

fn sgr(pen: &str) -> Vec<u16> {
    if pen.is_empty() { units("\u{1b}[0m") } else { units(&format!("\u{1b}[0;{pen}m")) }
}

fn slice_line(line: &Line, start: usize, end: usize) -> Vec<Run> {
    let mut out = Vec::new();
    let mut offset = 0;
    for run in &line.runs {
        let run_end = offset + run.text.len();
        if run_end > start && offset < end {
            let from = start.saturating_sub(offset);
            let to = run.text.len().min(end - offset);
            // An empty slice only arises when `start > end`, where every caller drops it.
            if from < to {
                out.push(Run { pen: run.pen.clone(), text: run.text[from..to].to_vec() });
            }
        }
        offset = run_end;
        if offset >= end {
            break;
        }
    }
    out
}

fn push_run(runs: &mut Vec<Run>, run: Run) {
    if run.text.is_empty() {
        return;
    }
    match runs.last_mut() {
        Some(last) if last.pen == run.pen => last.text.extend_from_slice(&run.text),
        _ => runs.push(run),
    }
}

fn trim_line_start(runs: &mut Vec<Run>, count: usize) {
    let mut dropped = 0;
    let mut remaining = count;
    while remaining > 0 && dropped < runs.len() {
        let run = &mut runs[dropped];
        if run.text.len() <= remaining {
            remaining -= run.text.len();
            dropped += 1;
        } else {
            run.text.drain(..remaining);
            remaining = 0;
        }
    }
    runs.drain(..dropped);
}

fn spaces(count: usize) -> Vec<u16> {
    vec![b' ' as u16; count]
}

fn write_line(line: &Line, col: usize, text: &[u16], pen: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    for run in slice_line(line, 0, col.min(line.len)) {
        push_run(&mut runs, run);
    }
    if col > line.len {
        push_run(&mut runs, Run { pen: String::new(), text: spaces(col - line.len) });
    }
    push_run(&mut runs, Run { pen: pen.to_owned(), text: text.to_vec() });
    for run in slice_line(line, col + text.len(), line.len) {
        push_run(&mut runs, run);
    }
    runs
}

/// Erase in line: 0 cursor to end, 1 start to cursor, anything else the whole line.
fn erase_line(line: &Line, col: usize, mode: f64) -> Line {
    if mode == 0. {
        return Line::new(slice_line(line, 0, col));
    }
    if mode == 1. {
        let mut runs = Vec::new();
        push_run(&mut runs, Run { pen: String::new(), text: spaces((col + 1).min(line.len)) });
        for run in slice_line(line, col + 1, line.len) {
            push_run(&mut runs, run);
        }
        return Line::new(runs);
    }
    Line::default()
}

fn serialize_line(line: &Line, out: &mut Vec<u16>) {
    let mut current = "";
    for run in &line.runs {
        if run.pen != current {
            out.extend(sgr(&run.pen));
            current = &run.pen;
        }
        out.extend_from_slice(&run.text);
    }
    if !current.is_empty() {
        out.extend(sgr(""));
    }
}

#[derive(Debug, Clone, Default)]
struct Attributes {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    inverse: bool,
    fg: String,
    bg: String,
}

fn parse_pen(pen: &str) -> Attributes {
    let mut attributes = Attributes::default();
    if !pen.is_empty() {
        apply_sgr(&mut attributes, &pen.split(';').map(js_number).collect::<Vec<_>>());
    }
    attributes
}

fn format_pen(attributes: &Attributes) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for (on, code) in [
        (attributes.bold, "1"),
        (attributes.dim, "2"),
        (attributes.italic, "3"),
        (attributes.underline, "4"),
        (attributes.inverse, "7"),
        (attributes.strike, "9"),
    ] {
        if on {
            parts.push(code);
        }
    }
    if !attributes.fg.is_empty() {
        parts.push(&attributes.fg);
    }
    if !attributes.bg.is_empty() {
        parts.push(&attributes.bg);
    }
    parts.join(";")
}

/// Reads `5;n` or `2;r;g;b` colors; returns the code and the next index.
fn extended_color(code: f64, params: &[f64], index: usize) -> (Option<String>, usize) {
    let mode = params.get(index + 1).copied();
    if mode == Some(5.) {
        return match params.get(index + 2) {
            // `value > 255` is false for NaN, so NaN makes a colour as it does on the web.
            Some(value) if *value <= 255. || value.is_nan() => (Some(format!("{};5;{}", js_string(code), js_string(*value))), index + 3),
            _ => (None, index + 3),
        };
    }
    if mode == Some(2.) {
        let channels = (params.get(index + 2), params.get(index + 3), params.get(index + 4));
        let (Some(r), Some(g), Some(b)) = channels else { return (None, index + 5) };
        let byte = |value: f64| to_int32(value) & 255;
        return (Some(format!("{};2;{};{};{}", js_string(code), byte(*r), byte(*g), byte(*b))), index + 5);
    }
    (None, index + 2)
}

fn apply_sgr(attributes: &mut Attributes, params: &[f64]) {
    let params = if params.is_empty() { &[0.][..] } else { params };
    let mut index = 0;
    while index < params.len() {
        let code = params[index];
        let in_range = |low: f64, high: f64| code >= low && code <= high;
        if code == 0. || code.is_nan() {
            *attributes = Attributes::default();
        } else if code == 1. {
            attributes.bold = true;
        } else if code == 2. {
            attributes.dim = true;
        } else if code == 3. {
            attributes.italic = true;
        } else if code == 4. {
            attributes.underline = true;
        } else if code == 7. {
            attributes.inverse = true;
        } else if code == 9. {
            attributes.strike = true;
        } else if code == 22. {
            attributes.bold = false;
            attributes.dim = false;
        } else if code == 23. {
            attributes.italic = false;
        } else if code == 24. {
            attributes.underline = false;
        } else if code == 27. {
            attributes.inverse = false;
        } else if code == 29. {
            attributes.strike = false;
        } else if in_range(30., 37.) || in_range(90., 97.) {
            attributes.fg = js_string(code);
        } else if in_range(40., 47.) || in_range(100., 107.) {
            attributes.bg = js_string(code);
        } else if code == 39. {
            attributes.fg.clear();
        } else if code == 49. {
            attributes.bg.clear();
        } else if code == 38. || code == 48. {
            let (color, next) = extended_color(code, params, index);
            if let Some(color) = color {
                if code == 38. { attributes.fg = color } else { attributes.bg = color }
            }
            index = next;
            continue;
        }
        index += 1;
    }
}

/// JavaScript `Number(text)`. CSI parameters never hold letters (those end the sequence), so
/// only signs, digits, a decimal point and whitespace can make a number.
fn js_number(text: &str) -> f64 {
    let is_space = |c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}';
    let trimmed = text.trim_matches(is_space);
    if trimmed.is_empty() {
        return 0.;
    }
    let unsigned = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if whole.len() + fraction.len() == 0 || !digits(whole) || !digits(fraction) {
        return f64::NAN;
    }
    trimmed.parse().unwrap_or(f64::NAN)
}

/// JavaScript `String(number)` for the values a pen can hold.
fn js_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0. { "Infinity".into() } else { "-Infinity".into() };
    }
    if value == 0. {
        return "0".into();
    }
    if value.abs() >= 1e21 || value.abs() < 1e-6 {
        let formatted = format!("{value:e}");
        return match formatted.split_once('e') {
            Some((mantissa, exponent)) if !exponent.starts_with('-') => format!("{mantissa}e+{exponent}"),
            _ => formatted,
        };
    }
    format!("{value}")
}

/// JavaScript `ToInt32`, which `&` applies.
fn to_int32(value: f64) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    let wrapped = value.trunc().rem_euclid(4_294_967_296.);
    (if wrapped >= 2_147_483_648. { wrapped - 4_294_967_296. } else { wrapped }) as i64
}

/// Colon color sub-parameters carry an optional color-space id absent from the semicolon form.
fn csi_params(params: &str) -> Vec<f64> {
    let mut numbers = Vec::new();
    for part in params.split(';') {
        if !part.contains(':') {
            numbers.push(js_number(part));
            continue;
        }
        let mut sub: Vec<f64> = part.split(':').map(js_number).collect();
        if (sub[0] == 38. || sub[0] == 48.) && sub.get(1) == Some(&2.) && sub.len() >= 6 {
            sub.remove(2);
        }
        numbers.extend(sub);
    }
    numbers
}

/// Length of a complete escape sequence starting at `index` (an ESC), `None` while unfinished.
fn escape_length(input: &[u16], index: usize) -> Option<usize> {
    let next = *input.get(index + 1)?;
    if next == b'[' as u16 {
        return (index + 2..input.len()).find(|&cursor| (0x40..=0x7e).contains(&input[cursor])).map(|cursor| cursor - index + 1);
    }
    if STRING_SEQUENCE_INTRODUCERS.contains(&next) {
        // BEL/ST ends string sequences; also stop at newlines so stray binary introducers cannot
        // hide later output.
        for cursor in index + 2..input.len() {
            match input[cursor] {
                0x07 => return Some(cursor - index + 1),
                NEWLINE => return Some(cursor - index),
                ESC => {
                    let after = *input.get(cursor + 1)?;
                    if after == b'\\' as u16 {
                        return Some(cursor - index + 2);
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    // Charset designations take three units; other sequences take two.
    if b"()*+".iter().any(|&b| next == u16::from(b)) {
        return (index + 2 < input.len()).then_some(3);
    }
    Some(2)
}

struct Terminal {
    settled: Vec<u16>,
    lines: Vec<Line>,
    row: usize,
    col: usize,
    pen: String,
    saved: Option<(usize, usize)>,
    /// Output was discarded on purpose (scrollback cleared or window over budget).
    dropped: bool,
    max_chars: usize,
}

impl Terminal {
    fn write(&mut self, text: &[u16]) {
        let line = &mut self.lines[self.row];
        if self.col == line.len {
            push_run(&mut line.runs, Run { pen: self.pen.clone(), text: text.to_vec() });
            line.len += text.len();
        } else {
            *line = Line::new(write_line(line, self.col, text, &self.pen));
        }
        self.col += text.len();
        if line.runs.len() > MAX_LINE_RUNS {
            // Flatten old runs so later trimming stays cheap.
            let keep = line.runs.len() - MAX_LINE_RUNS / 8;
            let plain: Vec<u16> = line.runs[..keep].iter().flat_map(|run| run.text.iter().copied()).collect();
            line.runs.splice(..keep, [Run { pen: String::new(), text: plain }]);
        }
        // Trim each write to max_chars columns so results do not depend on chunk boundaries.
        if line.len > self.max_chars {
            let mut removed = line.len - self.max_chars;
            trim_line_start(&mut line.runs, removed);
            // Never start on a lone low surrogate.
            if line.runs.first().and_then(|run| run.text.first()).is_some_and(|&unit| is_low_surrogate(unit)) {
                trim_line_start(&mut line.runs, 1);
                removed += 1;
            }
            line.len -= removed;
            self.dropped = true;
            self.col = self.col.saturating_sub(removed);
            if let Some((row, col)) = self.saved
                && row == self.row
            {
                self.saved = Some((row, col.saturating_sub(removed)));
            }
        }
    }

    fn newline(&mut self) {
        self.row += 1;
        self.col = 0;
        if self.row == self.lines.len() {
            self.lines.push(Line::default());
        }
        while self.lines.len() > LIVE_LINES {
            self.scroll_out();
        }
    }

    fn scroll_out(&mut self) -> usize {
        let line = self.lines.remove(0);
        self.settle(&line);
        self.row = self.row.saturating_sub(1);
        if let Some((row, col)) = self.saved {
            self.saved = Some((row.saturating_sub(1), col));
        }
        line.len
    }

    fn settle(&mut self, line: &Line) {
        serialize_line(line, &mut self.settled);
        self.settled.push(NEWLINE);
        if self.settled.len() > 2 * self.max_chars {
            self.settled = tail(&self.settled, self.max_chars).0;
            self.dropped = true;
        }
    }

    fn move_to(&mut self, row: f64, col: f64) {
        let last_row = (self.lines.len() - 1) as f64;
        self.row = row.min(last_row).max(0.) as usize;
        // A huge column (ESC[999999999C) would otherwise pad a line past any string limit.
        self.col = col.min(TERMINAL_OUTPUT_TAIL_CHARS as f64).max(0.) as usize;
    }

    fn csi(&mut self, params: &str, final_byte: u16) {
        if params.starts_with(['?', '>', '=']) {
            return; // Mode switches (cursor visibility, bracketed paste).
        }
        let numbers = if params.is_empty() { Vec::new() } else { csi_params(params) };
        if final_byte == b'm' as u16 {
            let mut attributes = parse_pen(&self.pen);
            apply_sgr(&mut attributes, &numbers);
            self.pen = format_pen(&attributes);
            return;
        }
        let or_one = |value: Option<&f64>| value.copied().filter(|n| *n != 0. && !n.is_nan()).unwrap_or(1.).max(1.);
        let count = or_one(numbers.first());
        let (row, col) = (self.row as f64, self.col as f64);
        let mode = numbers.first().copied().unwrap_or(0.);
        match final_byte as u8 {
            b'A' => self.move_to(row - count, col),
            b'B' => self.move_to(row + count, col),
            b'C' => self.move_to(row, col + count),
            b'D' => self.move_to(row, col - count),
            b'E' => self.move_to(row + count, 0.),
            b'F' => self.move_to(row - count, 0.),
            b'G' => self.move_to(row, count - 1.),
            // Rows count from the top of the window: there is no fixed screen height.
            b'H' | b'f' => self.move_to(count - 1., or_one(numbers.get(1)) - 1.),
            b'K' => self.lines[self.row] = erase_line(&self.lines[self.row], self.col, mode),
            b'J' => {
                if mode == 0. {
                    self.lines[self.row] = erase_line(&self.lines[self.row], self.col, 0.);
                    self.lines.truncate(self.row + 1);
                } else if mode == 1. {
                    for line in &mut self.lines[..self.row] {
                        *line = Line::default();
                    }
                    self.lines[self.row] = erase_line(&self.lines[self.row], self.col, 1.);
                } else if mode == 3. {
                    if !self.settled.is_empty() {
                        self.dropped = true;
                    }
                    self.settled.clear();
                } else {
                    self.lines = vec![Line::default()];
                    self.row = 0;
                    self.col = 0;
                }
            }
            b's' => self.saved = Some((self.row, self.col)),
            b'u' => {
                if let Some((row, col)) = self.saved {
                    self.move_to(row as f64, col as f64);
                }
            }
            _ => {}
        }
    }

    fn escape(&mut self, sequence: &[u16]) {
        if sequence[1] == b'[' as u16 {
            let params = String::from_utf16_lossy(&sequence[2..sequence.len() - 1]);
            self.csi(&params, sequence[sequence.len() - 1]);
        } else if sequence == [ESC, b'7' as u16] {
            self.saved = Some((self.row, self.col));
        } else if sequence == [ESC, b'8' as u16] {
            if let Some((row, col)) = self.saved {
                self.move_to(row as f64, col as f64);
            }
        } else if sequence == [ESC, b'M' as u16] {
            self.move_to(self.row as f64 - 1., self.col as f64);
        }
    }

    /// Applies `input`; returns the unfinished escape left at its end.
    fn feed(&mut self, input: &[u16]) -> Vec<u16> {
        let mut printable = 0;
        let mut index = 0;
        while index < input.len() {
            let code = input[index];
            if code == 0x09 || (code >= 0x20 && code != 0x7f && !(0x80..=0x9f).contains(&code)) {
                index += 1;
                continue;
            }
            if index > printable {
                self.write(&input[printable..index]);
            }
            printable = index + 1;
            match code {
                NEWLINE => self.newline(),
                0x0d => self.col = 0,
                0x08 => self.col = self.col.saturating_sub(1),
                ESC => match escape_length(input, index) {
                    None => {
                        let fragment = &input[index..];
                        if fragment.len() <= MAX_PENDING_ESCAPE {
                            return fragment.to_vec();
                        }
                        // Retain the introducer and possible ST prefix so long string payloads
                        // stay hidden until termination.
                        if STRING_SEQUENCE_INTRODUCERS.contains(&fragment[1]) {
                            let mut kept = fragment[..2].to_vec();
                            if fragment.last() == Some(&ESC) {
                                kept.push(ESC);
                            }
                            return kept;
                        }
                        return Vec::new();
                    }
                    Some(length) => {
                        self.escape(&input[index..index + length]);
                        index += length - 1;
                        printable = index + 1;
                    }
                },
                _ => {}
            }
            index += 1;
        }
        if input.len() > printable {
            self.write(&input[printable..]);
        }
        Vec::new()
    }
}

/// The active style at `index` of `text` and where a cut there may safely start.
fn style_at(text: &[u16], index: usize) -> (usize, Vec<u16>) {
    let line_start = text[..index].iter().rposition(|&unit| unit == NEWLINE).map_or(0, |at| at + 1);
    let mut prefix: &[u16] = &[];
    let mut cursor = line_start;
    while cursor < index {
        // Only an escape starting before `index` matters, so the search stops there.
        let window = &text[cursor..(index + 1).min(text.len())];
        let Some(found) = window.windows(2).position(|pair| pair == [ESC, b'[' as u16]).map(|at| cursor + at) else { break };
        let Some(end) = text[found..].iter().position(|&unit| unit == b'm' as u16).map(|at| found + at) else { break };
        if end >= index {
            return (end + 1, text[found..=end].to_vec());
        }
        prefix = &text[found..=end];
        cursor = end + 1;
    }
    (index, if prefix == sgr("") { Vec::new() } else { prefix.to_vec() })
}

/// `terminalOutputTail`: keeps the last `max_chars`, preferring to start on a fresh line.
fn tail(text: &[u16], max_chars: usize) -> (Vec<u16>, bool) {
    if text.len() <= max_chars {
        return (text.to_vec(), false);
    }
    let start = text.len() - max_chars;
    // Drop the partial first line unless that would discard most of the tail.
    if let Some(newline) = text[start..].iter().position(|&unit| unit == NEWLINE)
        && 4 * newline < max_chars
    {
        return (text[start + newline + 1..].to_vec(), true);
    }
    // Cutting inside a line: never split an escape, and keep the colour in force.
    let (mut start, mut out) = style_at(text, start);
    // Never start on a lone low surrogate.
    if text.get(start).is_some_and(|&unit| is_low_surrogate(unit)) {
        start += 1;
    }
    out.extend_from_slice(&text[start..]);
    (out, true)
}

impl TerminalOutputState {
    /// `appendTerminalOutput`, in place.
    pub fn append(&mut self, chunk: &str, max_chars: usize) {
        let mut input = std::mem::take(&mut self.pending);
        input.extend(chunk.encode_utf16());
        if input.is_empty() {
            return;
        }
        let screen = std::mem::take(&mut self.screen);
        let mut terminal = Terminal {
            settled: screen.settled,
            lines: screen.lines,
            row: screen.row,
            col: screen.col,
            pen: screen.pen,
            saved: screen.saved,
            dropped: false,
            max_chars,
        };
        self.pending = terminal.feed(&input);
        let mut truncated = self.truncated;
        let mut window_chars: usize = terminal.lines.iter().map(|line| line.len).sum();
        while window_chars > max_chars && terminal.row > 0 {
            window_chars -= terminal.scroll_out();
        }
        // Lines below a cursor that moved up can't scroll out; they keep only their ends.
        if window_chars > max_chars {
            let mut budget = max_chars as i64 - terminal.lines[..=terminal.row].iter().map(|line| line.len as i64).sum::<i64>();
            for row in terminal.row + 1..terminal.lines.len() {
                let length = terminal.lines[row].len;
                let keep = budget.clamp(0, length as i64) as usize;
                if keep < length {
                    truncated = true;
                    terminal.lines[row] = Line::new(slice_line(&terminal.lines[row], length - keep, length));
                    if let Some((saved_row, col)) = terminal.saved
                        && saved_row == row
                    {
                        terminal.saved = Some((saved_row, col.saturating_sub(length - keep)));
                    }
                }
                budget -= keep as i64;
            }
        }
        if terminal.dropped {
            truncated = true;
        }
        let (settled, settled_truncated) = tail(&terminal.settled, max_chars);
        let mut combined = settled.clone();
        for (index, line) in terminal.lines.iter().enumerate() {
            if index > 0 {
                combined.push(NEWLINE);
            }
            serialize_line(line, &mut combined);
        }
        let (text, tail_truncated) = tail(&combined, max_chars);
        self.text = String::from_utf16_lossy(&text);
        self.truncated = truncated || settled_truncated || tail_truncated;
        self.screen = Screen { settled, lines: terminal.lines, row: terminal.row, col: terminal.col, pen: terminal.pen, saved: terminal.saved };
    }

    /// The unfinished escape carried into the next chunk.
    pub fn pending(&self) -> String {
        String::from_utf16_lossy(&self.pending)
    }
}

/// `normalizeTerminalOutput`.
pub fn normalize_terminal_output(text: &str, max_chars: usize) -> (String, bool) {
    let mut state = TerminalOutputState::default();
    state.append(text, max_chars);
    (state.text, state.truncated)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

const fn hex(value: u32) -> Rgb {
    Rgb { r: (value >> 16) as u8, g: (value >> 8) as u8, b: value as u8 }
}

/// `TERMINAL_PALETTES`: the 16 ANSI colors command output is painted with.
pub const DARK_PALETTE: [Rgb; 16] = [
    hex(0x3b4048),
    hex(0xf47067),
    hex(0x57ab5a),
    hex(0xc69026),
    hex(0x539bf5),
    hex(0xb083f0),
    hex(0x39c5cf),
    hex(0xadbac7),
    hex(0x768390),
    hex(0xff938a),
    hex(0x6bc46d),
    hex(0xdaaa3f),
    hex(0x6cb6ff),
    hex(0xdcbdfb),
    hex(0x56d4dd),
    hex(0xcdd9e5),
];

pub const LIGHT_PALETTE: [Rgb; 16] = [
    hex(0x24292f),
    hex(0xcf222e),
    hex(0x116329),
    hex(0x7d4e00),
    hex(0x0550ae),
    hex(0x8250df),
    hex(0x1b7c83),
    hex(0x6e7781),
    hex(0x57606a),
    hex(0xa40e26),
    hex(0x1a7f37),
    hex(0x633c01),
    hex(0x0969da),
    hex(0x6639ba),
    hex(0x3192aa),
    hex(0x8c959f),
];

const BADGE_DARK_TEXT: Rgb = hex(0x1f2328);
const BADGE_LIGHT_TEXT: Rgb = hex(0xffffff);

/// `TerminalSpanStyle`, with colours resolved against the palette.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpanStyle {
    pub color: Option<Rgb>,
    pub background: Option<Rgb>,
    /// `fontWeight: 600`.
    pub bold: bool,
    /// `opacity: 0.65`.
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalSpan {
    pub text: String,
    pub style: Option<SpanStyle>,
}

fn palette_entry(index: f64, palette: &[Rgb; 16]) -> Option<Rgb> {
    (index.fract() == 0. && (0. ..16.).contains(&index)).then(|| palette[index as usize])
}

/// A CSS `rgb(…)` the web builds: `Some(None)` when `parseCssColor` would reject it.
fn css_rgb(channels: [f64; 3]) -> Option<Option<Rgb>> {
    let byte = |value: f64| (value.fract() == 0. && (0. ..=255.).contains(&value)).then_some(value as u8);
    Some(match channels.map(byte) {
        [Some(r), Some(g), Some(b)] => Some(Rgb { r, g, b }),
        _ => None,
    })
}

/// `xtermColor`. The outer `None` is an undefined colour, the inner one an unparseable string.
fn xterm_color(index: f64, palette: &[Rgb; 16]) -> Option<Option<Rgb>> {
    if index < 16. {
        return palette_entry(index, palette).map(Some);
    }
    if index >= 232. {
        let level = 8. + (index - 232.) * 10.;
        return css_rgb([level; 3]);
    }
    let cube = index - 16.;
    let channel = |value: f64| if value == 0. { 0. } else { 55. + value * 40. };
    css_rgb([channel((cube / 36.).floor()), channel((cube / 6.).floor() % 6.), channel(cube % 6.)])
}

fn resolve_color(code: &str, base: f64, palette: &[Rgb; 16]) -> Option<Option<Rgb>> {
    let parts: Vec<f64> = code.split(';').map(js_number).collect();
    let part = |index: usize| parts.get(index).copied().unwrap_or(f64::NAN);
    let head = part(0);
    if head == base + 8. {
        if part(1) == 5. {
            return xterm_color(part(2), palette);
        }
        return css_rgb([part(2), part(3), part(4)]);
    }
    if head >= base && head <= base + 7. {
        return palette_entry(head - base, palette).map(Some);
    }
    if head >= base + 60. && head <= base + 67. {
        return palette_entry(head - base - 52., palette).map(Some);
    }
    None
}

fn luminance(rgb: Rgb) -> f64 {
    let channel = |value: u8| {
        let scaled = f64::from(value) / 255.;
        if scaled <= 0.03928 { scaled / 12.92 } else { ((scaled + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b)
}

fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (first, second) = (luminance(a), luminance(b));
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

/// `spanStyle`.
fn span_style(pen: &str, palette: &[Rgb; 16]) -> Option<SpanStyle> {
    if pen.is_empty() {
        return None;
    }
    let attributes = parse_pen(pen);
    let mut color = if attributes.fg.is_empty() { None } else { resolve_color(&attributes.fg, 30., palette) };
    let mut background = if attributes.bg.is_empty() { None } else { resolve_color(&attributes.bg, 40., palette) };
    if attributes.inverse {
        (color, background) = (background, color.or(Some(Some(palette[7]))));
    }
    // Badges like "PASS" on green assume a terminal's own palette; keep their text readable.
    if let Some(Some(back)) = background
        && color.flatten().is_none_or(|fore| contrast_ratio(fore, back) < 3.)
    {
        color = Some(Some(if luminance(back) > 0.35 { BADGE_DARK_TEXT } else { BADGE_LIGHT_TEXT }));
    }
    Some(SpanStyle {
        color: color.flatten(),
        background: background.flatten(),
        bold: attributes.bold,
        dim: attributes.dim,
        italic: attributes.italic,
        underline: attributes.underline,
        strike: attributes.strike,
    })
}

/// `terminalOutputSpans`: caps styled spans at `max_spans`, then emits one plain remainder to
/// bound the elements drawn.
pub fn terminal_output_spans(text: &str, palette: &[Rgb; 16], max_spans: usize) -> Vec<TerminalSpan> {
    struct Spans<'a> {
        text: &'a str,
        palette: &'a [Rgb; 16],
        spans: Vec<TerminalSpan>,
        styles: HashMap<&'a str, Option<SpanStyle>>,
    }
    impl<'a> Spans<'a> {
        fn push(&mut self, pen: &'a str, cursor: usize, end: usize) {
            if end <= cursor {
                return;
            }
            let palette = self.palette;
            let style = self.styles.entry(pen).or_insert_with(|| span_style(pen, palette)).clone();
            self.spans.push(TerminalSpan { text: self.text[cursor..end].to_owned(), style });
        }
    }
    let mut out = Spans { text, palette, spans: Vec::new(), styles: HashMap::new() };
    let mut pen = "";
    let mut cursor = 0;
    while cursor < text.len() {
        let Some(escape) = text[cursor..].find("\u{1b}[").map(|at| cursor + at) else { break };
        if out.spans.len() >= max_spans {
            break;
        }
        out.push(pen, cursor, escape);
        let Some(end) = text[escape..].find('m').map(|at| escape + at) else { break };
        let params = &text[escape + 2..end];
        pen = if params == "0" { "" } else { params.strip_prefix("0;").unwrap_or(params) };
        cursor = end + 1;
    }
    if cursor < text.len() {
        if out.spans.len() >= max_spans {
            out.spans.push(TerminalSpan { text: terminal_output_plain_text(&text[cursor..]), style: None });
        } else {
            out.push(pen, cursor, text.len());
        }
    }
    out.spans
}

/// `terminalOutputPlainText`: the text without its SGR escapes.
pub fn terminal_output_plain_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\u{1b}[") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let params = after.bytes().take_while(|b| b.is_ascii_digit() || *b == b';').count();
        if after.as_bytes().get(params) == Some(&b'm') {
            rest = &after[params + 1..];
        } else {
            out.push('\u{1b}');
            rest = &rest[at + 1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests;
