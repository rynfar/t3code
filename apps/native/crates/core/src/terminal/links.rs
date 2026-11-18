//! Links in terminal output: `extractTerminalLinks`, `isTerminalUrl`,
//! `resolveTerminalLinkTarget` (OSC 8 `file://` targets, `pingdotgg/t3code#36025`) and
//! `collectWrappedTerminalLinkLine` from `apps/web/src/terminal-links.ts`, and
//! `terminalLinkAtPositionWithRange` from `apps/web/src/terminal/ghostty/surface.ts`.
//!
//! The web patterns use lookaround and backreferences, which `regex` lacks. Lookbehinds become
//! a check of the preceding character and a retry one character on, lookaheads a check of what
//! follows, and the quoted-path backreference one pattern per quote; each keeps the web's
//! leftmost-first results. Offsets are UTF-8 byte offsets into the line.

use std::sync::LazyLock;

use regex::Regex;

use crate::files::position::{
    format_file_path_position, parse_file_url_href, resolve_path_link_target, safe_decode_uri_component, split_file_path_position,
    strip_slash_prefixed_windows_drive,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Url,
    Path,
}

/// `TerminalLinkMatch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMatch {
    pub kind: LinkKind,
    pub text: String,
    pub start: usize,
    pub end: usize,
}

const C: &str = r"\p{L}\p{M}\p{N}._\-";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid terminal link pattern")
}

static URL: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)https?://[^\s"'`<>]+"#));
static IS_URL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^https?://"));
static POWERSHELL_PROMPT_PATH: LazyLock<Regex> = LazyLock::new(|| re(r"^PS ((?:[A-Za-z]:[\\/]|\\\\|/)[^>\r\n]*)>"));
/// `FILE_PATH_PATTERN`: group `word` is the bare-path alternative its lookbehind guards. A
/// parenthesized segment must be followed by a separator, so it only ends a segment that
/// another follows; group `ts` is the TypeScript `(line,col)` its lookahead guards.
static FILE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    let t = format!(r"(?:[{C}$+@]|\[{{1,2}}[{C}]+\]{{1,2}})");
    re(&format!(
        r#"(?:~[\\/]|\.{{1,2}}[\\/]|/|[A-Za-z]:[\\/]|\\\\)[^\s"'`<>]+|(?P<word>[{C}]+(?:[\\/]{t}*\([{C}]+\)|[\\/]{t}+)*[\\/]{t}+)(?:(?::[0-9]+){{1,2}}|(?P<ts>\([0-9]+,[0-9]+\)))?"#
    ))
});
static TS_DIAGNOSTIC: LazyLock<Regex> = LazyLock::new(|| re(r"^:\s+(?:error|warning) TS[0-9]+:"));
static TS_POSITION: LazyLock<Regex> = LazyLock::new(|| re(r"\(([0-9]+),([0-9]+)\)$"));
/// `QUOTED_FILE_PATH_PATTERN`, once per quote for its `\1`.
static QUOTED_FILE_PATH: LazyLock<[Regex; 3]> = LazyLock::new(|| {
    ['"', '\'', '`'].map(|q| {
        re(&format!(
            r#"{q}(?P<path>(?:~[\\/]|\.{{1,2}}[\\/]|/|[A-Za-z]:[\\/]|\\\\|[{C} ]+[\\/])[^\r\n]*?|[{C} ]+\.[\p{{L}}\p{{M}}\p{{N}}_-]+(?::[0-9]+){{0,2}}){q}(?P<position>(?::[0-9]+){{0,2}})"#
        ))
    })
});
static TRAILING_PUNCTUATION: LazyLock<Regex> = LazyLock::new(|| re(r"[.,;!?]+$"));
static TRAILING_PATH_PUNCTUATION: LazyLock<Regex> = LazyLock::new(|| re(r"[.,;:!?]+$"));
static REQUEST_ROUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?:^|[\s"'`])(?:GET|HEAD|POST|PUT|PATCH|DELETE|OPTIONS|CONNECT|TRACE|proxying)\s+["'`]?(?:/|[A-Za-z0-9_-]+/)[^\s?]*\?\S*"#)
});
static SCHEME_URL: LazyLock<Regex> = LazyLock::new(|| re(r#"[+-]?[A-Za-z][A-Za-z0-9+.-]+://[^\s"'`<>]+"#));
static SCP_REMOTE: LazyLock<Regex> = LazyLock::new(|| re(r#"[A-Za-z0-9_.-]+@[A-Za-z0-9_.-]+:[^\s"'`<>]+"#));

/// The bare-path lookbehind's class, `[\p{L}\p{M}\p{N}._-]`.
fn is_word(c: char) -> bool {
    static WORD: LazyLock<Regex> = LazyLock::new(|| re(&format!("^[{C}]$")));
    let mut buffer = [0; 4];
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') || (!c.is_ascii() && WORD.is_match(c.encode_utf8(&mut buffer)))
}

fn previous_char(line: &str, at: usize) -> Option<char> {
    line[..at].chars().next_back()
}

fn next_boundary(line: &str, at: usize) -> usize {
    line[at..].chars().next().map_or(line.len(), |c| at + c.len_utf8())
}

/// Every match of `pattern` whose start `allowed` accepts, as `matchAll` finds them with a
/// lookbehind: a refused start is retried one character on.
fn guarded_matches(line: &str, pattern: &Regex, allowed: impl Fn(&regex::Captures, usize) -> bool) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while at <= line.len() {
        let Some(captures) = pattern.captures_at(line, at) else { break };
        let whole = captures.get(0).expect("group 0");
        if !allowed(&captures, whole.start()) {
            at = next_boundary(line, whole.start());
            continue;
        }
        found.push((whole.start(), whole.end()));
        at = if whole.end() > whole.start() { whole.end() } else { next_boundary(line, whole.end()) };
    }
    found
}

/// `trimClosingDelimiters`.
fn trim_closing_delimiters(value: &str, kind: LinkKind) -> &str {
    let punctuation = if kind == LinkKind::Path { &TRAILING_PATH_PUNCTUATION } else { &TRAILING_PUNCTUATION };
    let mut output = match punctuation.find(value) {
        Some(found) => &value[..found.start()],
        None => value,
    };
    for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
        while output.ends_with(close) {
            let opens = output.matches(open).count();
            let closes = output.matches(close).count();
            if opens >= closes {
                break;
            }
            output = &output[..output.len() - close.len_utf8()];
        }
    }
    output
}

fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// A raw match: where it starts, its text, and whether a position follows a closing quote.
struct Raw {
    start: usize,
    text: String,
    quoted: bool,
    position_after_quote: bool,
}

/// `collectMatches`.
fn collect(line: &str, kind: LinkKind, raws: Vec<Raw>, existing: &[LinkMatch]) -> Vec<LinkMatch> {
    let mut matches: Vec<LinkMatch> = Vec::new();
    for raw in raws {
        if raw.text.is_empty() {
            continue;
        }
        let trimmed = if raw.quoted { raw.text.as_str() } else { trim_closing_delimiters(&raw.text, kind) };
        if trimmed.is_empty() || (kind == LinkKind::Path && is_url(trimmed)) {
            continue;
        }
        let rest = line.get(raw.start + trimmed.len()..).unwrap_or("");
        let text = if kind == LinkKind::Path && TS_DIAGNOSTIC.is_match(rest) {
            TS_POSITION.replace(trimmed, ":$1:$2").into_owned()
        } else {
            trimmed.to_owned()
        };
        let span = (raw.start, raw.start + trimmed.len() + usize::from(raw.position_after_quote));
        let collides = existing.iter().chain(&matches).any(|other| overlaps(span, (other.start, other.end)));
        if !collides {
            matches.push(LinkMatch { kind, text, start: span.0, end: span.1 });
        }
    }
    matches
}

fn url_raws(line: &str) -> Vec<Raw> {
    URL.find_iter(line).map(|found| Raw { start: found.start(), text: found.as_str().to_owned(), quoted: false, position_after_quote: false }).collect()
}

/// `QUOTED_FILE_PATH_PATTERN` through `matchAll`: the leftmost match of any quote, then on.
fn quoted_raws(line: &str) -> Vec<Raw> {
    let mut raws = Vec::new();
    let mut at = 0;
    while at < line.len() {
        let next = QUOTED_FILE_PATH
            .iter()
            .filter_map(|pattern| pattern.captures_at(line, at))
            .min_by_key(|captures| captures.get(0).map_or(usize::MAX, |m| m.start()));
        let Some(captures) = next else { break };
        let (path, whole) = (captures.name("path").expect("path group"), captures.get(0).expect("group 0"));
        let position = captures.name("position").map_or("", |m| m.as_str());
        // The position outside the quotes joins the path without the closing quote.
        let text = format!("{}{position}", path.as_str());
        raws.push(Raw { start: path.start(), text, quoted: true, position_after_quote: !position.is_empty() });
        at = whole.end().max(next_boundary(line, whole.start()));
    }
    raws
}

fn path_raws(line: &str) -> Vec<Raw> {
    let mut raws = Vec::new();
    let mut at = 0;
    while at <= line.len() {
        let Some(captures) = FILE_PATH.captures_at(line, at) else { break };
        let whole = captures.get(0).expect("group 0");
        let guarded = captures.name("word").is_some();
        if guarded && previous_char(line, whole.start()).is_some_and(is_word) {
            at = next_boundary(line, whole.start());
            continue;
        }
        let mut end = whole.end();
        if let Some(ts) = captures.name("ts")
            && !TS_DIAGNOSTIC.is_match(&line[ts.end()..])
        {
            end = ts.start();
        }
        raws.push(Raw { start: whole.start(), text: line[whole.start()..end].to_owned(), quoted: false, position_after_quote: false });
        at = end.max(next_boundary(line, whole.start()));
    }
    raws
}

/// `extractTerminalLinks`: URLs, quoted paths and bare paths in one line, by start. Paths inside
/// request routes, other schemes' URLs, and the tail of an SCP-style Git remote are not links.
pub fn extract_links(line: &str) -> Vec<LinkMatch> {
    let prompt: Vec<LinkMatch> = POWERSHELL_PROMPT_PATH
        .captures(line)
        .and_then(|captures| captures.get(1))
        .map(|path| LinkMatch { kind: LinkKind::Path, text: path.as_str().to_owned(), start: 3, end: 3 + path.as_str().len() })
        .into_iter()
        .collect();
    let mut non_path: Vec<(usize, usize)> = REQUEST_ROUTE.find_iter(line).map(|found| (found.start(), found.end())).collect();
    non_path.extend(guarded_matches(line, &SCHEME_URL, |_, start| {
        !previous_char(line, start).is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    }));
    let remote_tails: Vec<(usize, usize)> = guarded_matches(line, &SCP_REMOTE, |_, start| {
        !previous_char(line, start).is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '@' | '-'))
    })
    .into_iter()
    .map(|(start, end)| (start + line[start..end].find(':').map_or(0, |colon| colon + 1), end))
    .collect();

    let urls = collect(line, LinkKind::Url, url_raws(line), &prompt);
    let mut seen: Vec<LinkMatch> = prompt.iter().chain(&urls).cloned().collect();
    let quoted = collect(line, LinkKind::Path, quoted_raws(line), &seen);
    seen.extend(quoted.iter().cloned());
    let paths = collect(line, LinkKind::Path, path_raws(line), &seen);

    let mut links: Vec<LinkMatch> = prompt
        .into_iter()
        .chain(urls)
        .chain(quoted)
        .chain(paths)
        .filter(|link| {
            link.kind == LinkKind::Url
                || (!non_path.iter().any(|range| overlaps((link.start, link.end), *range))
                    && !remote_tails.iter().any(|tail| link.start >= tail.0 && link.end <= tail.1))
        })
        .collect();
    links.sort_by_key(|link| link.start);
    links
}

/// `isTerminalUrl`.
pub fn is_url(value: &str) -> bool {
    IS_URL.is_match(value)
}

/// `resolveTerminalLinkTarget`: an explicit OSC 8 target may be a `file://` URI, which names its
/// file directly; anything else is a path relative to `cwd`.
pub fn resolve_link_target(text: &str, cwd: &str) -> String {
    let file_url = text.get(..7).is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://")).then(|| parse_file_url_href(text)).flatten();
    match file_url {
        Some((path, hash)) => {
            let decoded = safe_decode_uri_component(&path);
            format_file_path_position(&split_file_path_position(strip_slash_prefixed_windows_drive(&decoded), &hash))
        }
        None => resolve_path_link_target(text, cwd),
    }
}

/// One visible row as link detection reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowText {
    /// Every cell's text, trailing blanks included.
    pub text: String,
    /// The byte offset each column's text starts at; a wide character's spacer column starts
    /// where the next column does.
    pub offsets: Vec<usize>,
    /// The column is the second half of a wide character.
    pub spacer: Vec<bool>,
    /// The row continues the one above (`isWrapContinuation`).
    pub continuation: bool,
    /// The row continues on the one below (`wrapsToNext`).
    pub wraps: bool,
}

impl RowText {
    fn trimmed(&self) -> &str {
        self.text.trim_end_matches(' ')
    }

    /// `terminalColumnAtOffset`: the column whose text holds `offset`.
    fn column_at(&self, offset: usize) -> usize {
        let columns = self.offsets.len();
        (0..columns)
            .find(|&column| offset < self.offsets.get(column + 1).copied().unwrap_or(self.text.len()))
            .unwrap_or(columns.saturating_sub(1))
    }
}

/// A link under the pointer and the cells it covers, inclusive, in visible rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHit {
    pub text: String,
    pub start: (usize, usize),
    pub end: (usize, usize),
}

struct Segment {
    row: usize,
    start: usize,
    end: usize,
}

/// `collectWrappedTerminalLinkLine`: the logical line through `row`, joined across soft wraps.
fn wrapped_line(row: usize, get: &impl Fn(usize) -> Option<RowText>) -> Option<(String, Vec<Segment>, Vec<RowText>)> {
    let mut first = row;
    let mut start_row = get(row)?;
    while first > 0 && start_row.continuation {
        start_row = get(first - 1)?;
        first -= 1;
    }
    let (mut text, mut segments, mut rows) = (String::new(), Vec::new(), Vec::new());
    let mut current = Some(start_row);
    let mut index = first;
    while let Some(line) = current {
        let next = get(index + 1).filter(|next| next.continuation);
        let part = if next.is_some() { line.text.as_str() } else { line.trimmed() };
        segments.push(Segment { row: index, start: text.len(), end: text.len() + part.len() });
        text.push_str(part);
        rows.push(line);
        current = next;
        index += 1;
    }
    Some((text, segments, rows))
}

/// `terminalLinkAtPositionWithRange`: the detected link at a visible cell. A logical line cut
/// off by the top of the viewport, or a link running past its bottom, is no link: its text
/// would be truncated.
pub fn link_at(row: usize, column: usize, get: impl Fn(usize) -> Option<RowText>) -> Option<LinkHit> {
    let (text, segments, rows) = wrapped_line(row, &get)?;
    let first = segments.first()?;
    if rows.first().is_some_and(|line| line.continuation) {
        return None;
    }
    let row_at = |segment: &Segment| &rows[segment.row - first.row];
    let segment = segments.iter().find(|segment| segment.row == row)?;
    let line = row_at(segment);
    let continues_below = rows.last().is_some_and(|line| line.wraps);
    let hit_column = if line.spacer.get(column).copied().unwrap_or(false) { column.saturating_sub(1) } else { column };
    let offset = segment.start + line.offsets.get(hit_column).copied().unwrap_or(line.text.len());
    let link = extract_links(&text).into_iter().find(|link| offset >= link.start && offset < link.end)?;
    if link.end == text.len() && continues_below {
        return None;
    }
    let containing = |at: usize| segments.iter().find(|segment| at >= segment.start && at < segment.end);
    let (start_segment, end_segment) = (containing(link.start)?, containing(link.end - 1)?);
    let (start_row, end_row) = (row_at(start_segment), row_at(end_segment));
    let end_column = end_row.column_at(link.end - 1 - end_segment.start);
    let end_column = if end_row.spacer.get(end_column + 1).copied().unwrap_or(false) { end_column + 1 } else { end_column };
    Some(LinkHit {
        text: link.text,
        start: (start_segment.row, start_row.column_at(link.start - start_segment.start)),
        end: (end_segment.row, end_column),
    })
}
