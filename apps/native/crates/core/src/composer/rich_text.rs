//! Rich-text Markdown parsing, editable source-marker visibility (`#277`), fenced code block
//! editing (`composer-code-block.ts`), and inline formatting toggles for the GPUI `EditorState`
//! composer (`CO-RICH`, `#387`). All offsets are UTF-8 byte offsets into the buffer.

use std::collections::HashSet;
use std::ops::Range;

/// Inline formatting toggle (`Mod+B`, `Mod+I`, `Mod+E`, `Mod+Shift+X`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InlineFormat {
    Bold,
    Italic,
    Strike,
    Code,
}

impl InlineFormat {
    pub const fn delimiter(self) -> &'static str {
        match self {
            Self::Bold => "**",
            Self::Italic => "*",
            Self::Strike => "~~",
            Self::Code => "`",
        }
    }
}

/// A text replacement and resulting selection after an editing command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichTextEdit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
    pub selection_after: Range<usize>,
}

/// A parsed fenced code block (` ```lang ` or ` ~~~lang `).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FencedCodeBlock {
    pub full_range: Range<usize>,
    pub open_fence: Range<usize>,
    pub fence_str: String,
    pub language: Option<String>,
    pub code_range: Range<usize>,
    pub close_fence: Option<Range<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineTokenKind {
    ContextLink,
    CitationLink,
    ModelMention,
    FileMention,
    SkillMention,
}

/// An inline composer chip/token occurrence (not parsed as Markdown emphasis).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineTokenSpan {
    pub kind: InlineTokenKind,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichMarkSpan {
    pub mark: InlineFormat,
    pub full_range: Range<usize>,
    pub open_marker: Range<usize>,
    pub close_marker: Range<usize>,
    pub content_range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockPrefixSpan {
    pub heading_level: Option<u8>,
    pub full_range: Range<usize>,
    pub marker_range: Range<usize>,
    pub content_range: Range<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerRichDoc {
    pub code_blocks: Vec<FencedCodeBlock>,
    pub marks: Vec<RichMarkSpan>,
    pub prefixes: Vec<BlockPrefixSpan>,
    pub tokens: Vec<InlineTokenSpan>,
}

/// Returns `true` when the caret or selection touches/intersects `full_range`, revealing its
/// editable Markdown source markers (`#277`).
pub fn is_marker_revealed(full_range: &Range<usize>, selection: &Range<usize>) -> bool {
    let (sel_start, sel_end) = if selection.start <= selection.end {
        (selection.start, selection.end)
    } else {
        (selection.end, selection.start)
    };
    sel_start <= full_range.end && sel_end >= full_range.start
}

fn parse_opening_fence_line(line: &str) -> Option<(usize, String, Option<String>)> {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let fence_char = rest.bytes().next().filter(|b| matches!(b, b'`' | b'~'))?;
    let fence_len = rest.bytes().take_while(|b| *b == fence_char).count();
    if fence_len < 3 {
        return None;
    }
    let after = &rest[fence_len..];
    if fence_char == b'`' && after.contains('`') {
        return None;
    }
    let info = after.trim();
    let language = info.split_whitespace().next().filter(|s| !s.is_empty()).map(str::to_owned);
    Some((indent, rest[..fence_len].to_owned(), language))
}

fn is_closing_fence_line(line: &str, fence: &str) -> bool {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let fence_char = fence.as_bytes()[0];
    let count = rest.bytes().take_while(|b| *b == fence_char).count();
    count >= fence.len() && rest[count..].trim().is_empty()
}

/// Parses fenced code blocks in `text`.
pub fn parse_fenced_code_blocks(text: &str) -> Vec<FencedCodeBlock> {
    let mut blocks = Vec::new();
    let mut offset = 0usize;
    let mut open: Option<(usize, usize, String, Option<String>, usize)> = None;

    for raw_line in text.split('\n') {
        let line_start = offset;
        let line_end = line_start + raw_line.len();
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        offset = (line_end + 1).min(text.len());

        if let Some((block_start, open_end, ref fence_str, ref language, code_start)) = open {
            if is_closing_fence_line(line, fence_str) {
                let code_end = line_start.saturating_sub(1).max(code_start);
                blocks.push(FencedCodeBlock {
                    full_range: block_start..line_end,
                    open_fence: block_start..open_end,
                    fence_str: fence_str.clone(),
                    language: language.clone(),
                    code_range: code_start..code_end,
                    close_fence: Some(line_start..line_end),
                });
                open = None;
            }
        } else if let Some((_, fence_str, language)) = parse_opening_fence_line(line) {
            let code_start = (line_end + 1).min(text.len());
            open = Some((line_start, line_end, fence_str, language, code_start));
        }
    }

    if let Some((block_start, open_end, fence_str, language, code_start)) = open {
        blocks.push(FencedCodeBlock {
            full_range: block_start..text.len(),
            open_fence: block_start..open_end,
            fence_str,
            language,
            code_range: code_start..text.len(),
            close_fence: None,
        });
    }
    blocks
}

/// Collects inline composer chip/mention ranges so emphasis does not split URLs, paths, or IDs.
pub fn collect_inline_tokens(text: &str, code_blocks: &[FencedCodeBlock]) -> Vec<InlineTokenSpan> {
    let mut spans = Vec::new();
    let in_code_block = |start: usize, end: usize| code_blocks.iter().any(|b| start < b.full_range.end && end > b.full_range.start);

    for reference in super::menu::context_references(text) {
        if !in_code_block(reference.start, reference.end) {
            spans.push(InlineTokenSpan { kind: InlineTokenKind::ContextLink, range: reference.start..reference.end });
        }
    }
    for (range, _) in crate::assistant_citations::collect(text) {
        if !in_code_block(range.start, range.end) {
            spans.push(InlineTokenSpan { kind: InlineTokenKind::CitationLink, range });
        }
    }
    for mention in crate::markdown::mentions::collect_composer_model_mentions(text) {
        if !in_code_block(mention.start, mention.end) {
            spans.push(InlineTokenSpan { kind: InlineTokenKind::ModelMention, range: mention.start..mention.end });
        }
    }

    // Also detect inline `@path/to/file` and `$skill` mentions outside already-collected links/code.
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = text[i..].chars().next().unwrap_or('\0');
        let clen = ch.len_utf8();
        if (ch == '@' || ch == '$')
            && (i == 0 || text[..i].chars().next_back().is_some_and(|p| p.is_whitespace() || matches!(p, '(' | '[' | '{' | ',')))
        {
            let rest = &text[i + clen..];
            let token_len: usize = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, ')' | ']' | '}' | ',' | ';' | '"' | '\'' | '`'))
                .map(char::len_utf8)
                .sum();
            if token_len > 0 {
                let range = i..(i + clen + token_len);
                let overlaps = spans.iter().any(|s| range.start < s.range.end && range.end > s.range.start);
                if !overlaps && !in_code_block(range.start, range.end) {
                    let body = &text[i + clen..range.end];
                    let is_amount = ch == '$' && super::menu::is_currency_amount_query(body);
                    if !is_amount {
                        spans.push(InlineTokenSpan {
                            kind: if ch == '@' { InlineTokenKind::FileMention } else { InlineTokenKind::SkillMention },
                            range,
                        });
                    }
                }
                i += clen + token_len;
                continue;
            }
        }
        i += clen;
    }

    spans.sort_by_key(|s| (s.range.start, s.range.end));
    spans
}

fn is_token_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '/' | '.' | ':' | '@' | '%' | '+' | '=' | '&' | '?' | '#' | '-' | '~')
}

fn is_emphasis_alternatives(token: &str) -> bool {
    let parts: Vec<&str> = token.split('/').collect();
    parts.len() >= 2 && parts.iter().all(|p| p.len() >= 3 && p.starts_with('_') && p.ends_with('_') && !p[1..p.len() - 1].is_empty())
}

fn looks_like_file_extension(ext: &str) -> bool {
    let mut parts = ext.split(':');
    let Some(first) = parts.next() else { return false };
    let Some(name) = first.strip_prefix('.') else { return false };
    if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let rest: Vec<&str> = parts.collect();
    rest.len() <= 2 && rest.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Byte offsets of underscore runs belonging to a URL, path, filename, or snake_case identifier
/// (`literalUnderscoreRuns`, `#36363`).
pub fn literal_underscore_runs(text: &str) -> HashSet<usize> {
    let mut literal = HashSet::new();
    if !text.contains('_') {
        return literal;
    }
    let mut i = 0usize;
    while i < text.len() {
        let ch = text[i..].chars().next().unwrap_or('\0');
        if !is_token_char(ch) {
            i += ch.len_utf8();
            continue;
        }
        let start = i;
        while i < text.len() {
            let c = text[i..].chars().next().unwrap_or('\0');
            if !is_token_char(c) {
                break;
            }
            if c == '~' && (text[i..].starts_with("~~") || text[..i].ends_with('~')) {
                break;
            }
            i += c.len_utf8();
        }
        if i == start {
            i += text[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let token = &text[start..i];
        let mut body_end = token.len();
        while body_end > 0 && matches!(token.as_bytes()[body_end - 1], b'.' | b':' | b'?') {
            body_end -= 1;
        }
        if is_emphasis_alternatives(&token[..body_end]) {
            continue;
        }
        while body_end > 0 && token.as_bytes()[body_end - 1] == b'_' {
            body_end -= 1;
        }
        let body = &token[..body_end];
        let is_path = if body.contains('/') {
            body.starts_with('/') || body.starts_with("~/") || body.contains('.') || body.contains(':')
        } else if let Some(dot) = body.rfind('.') {
            looks_like_file_extension(&body[dot..])
        } else {
            false
        };
        if !is_path {
            continue;
        }
        let mut closes_inside = false;
        let mut pos = 0usize;
        while let Some(rel) = body[pos..].find('_') {
            let run_start = pos + rel;
            let mut run_end = run_start;
            while run_end < body.len() && body.as_bytes()[run_end] == b'_' {
                run_end += 1;
            }
            if run_start > 0 {
                literal.insert(start + run_start);
                if body[run_end..].chars().next().is_some_and(|c| !c.is_alphanumeric() && c != '_') {
                    closes_inside = true;
                }
            }
            pos = run_end;
        }
        if closes_inside && body.starts_with('_') {
            literal.insert(start);
        }
    }
    literal
}

fn is_word_char(c: Option<char>) -> bool {
    c.is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
}

struct OpenDelimiter {
    mark: InlineFormat,
    delimiter: &'static str,
    open_start: usize,
    open_end: usize,
}

/// Parses inline Markdown spans inside `piece` (where `base` is `piece`'s byte offset in `text`).
fn parse_inline_piece(piece: &str, base: usize, out: &mut Vec<RichMarkSpan>) {
    let literal_runs = literal_underscore_runs(piece);
    let mut stack: Vec<OpenDelimiter> = Vec::new();
    let mut index = 0usize;

    while index < piece.len() {
        let ch = piece[index..].chars().next().unwrap_or('\0');
        if ch == '\\' {
            let next_len = piece[index + 1..].chars().next().map_or(0, char::len_utf8);
            index += 1 + next_len;
            continue;
        }
        if ch == '`' {
            let run_len = piece[index..].bytes().take_while(|b| *b == b'`').count();
            if run_len == 1
                && let Some(rel_close) = piece[index + 1..].find('`')
            {
                let close = index + 1 + rel_close;
                if close > index + 1 && !piece[index..close].contains('\n') && !piece[close + 1..].starts_with('`') {
                    out.push(RichMarkSpan {
                        mark: InlineFormat::Code,
                        full_range: (base + index)..(base + close + 1),
                        open_marker: (base + index)..(base + index + 1),
                        close_marker: (base + close)..(base + close + 1),
                        content_range: (base + index + 1)..(base + close),
                    });
                    index = close + 1;
                    continue;
                }
            }
            index += run_len;
            continue;
        }
        if !matches!(ch, '*' | '_' | '~') {
            index += ch.len_utf8();
            continue;
        }
        let run_byte = ch as u8;
        let run_len = piece[index..].bytes().take_while(|b| *b == run_byte).count();
        let end = index + run_len;
        if ch == '_' && literal_runs.contains(&index) {
            index = end;
            continue;
        }
        let before = piece[..index].chars().next_back();
        let after = piece[end..].chars().next();
        let can_close = before.is_some_and(|c| !c.is_whitespace()) && (ch != '_' || !is_word_char(after));
        let can_open = after.is_some_and(|c| !c.is_whitespace()) && (ch != '_' || !is_word_char(before));

        while index < end {
            let top = stack.last();
            let opens_nested_bold = top.is_some_and(|t| t.mark == InlineFormat::Italic)
                && end - index == 2
                && can_open
                && matches!(ch, '*' | '_')
                && !stack.iter().any(|f| f.mark == InlineFormat::Bold);
            if !opens_nested_bold
                && can_close
                && let Some(top) = top
                && piece[index..].starts_with(top.delimiter)
                && index + top.delimiter.len() <= end
            {
                let popped = stack.pop().unwrap();
                let close_end = index + popped.delimiter.len();
                out.push(RichMarkSpan {
                    mark: popped.mark,
                    full_range: (base + popped.open_start)..(base + close_end),
                    open_marker: (base + popped.open_start)..(base + popped.open_end),
                    close_marker: (base + index)..(base + close_end),
                    content_range: (base + popped.open_end)..(base + index),
                });
                index = close_end;
            } else if can_open && (ch != '~' || end - index >= 2) {
                let len = if ch == '~' || end - index >= 2 { 2 } else { 1 };
                let (mark, delimiter) = match (ch, len) {
                    ('~', _) => (InlineFormat::Strike, "~~"),
                    ('*', 2) => (InlineFormat::Bold, "**"),
                    ('*', _) => (InlineFormat::Italic, "*"),
                    ('_', 2) => (InlineFormat::Bold, "__"),
                    _ => (InlineFormat::Italic, "_"),
                };
                if stack.iter().any(|f| f.mark == mark) {
                    index = end;
                    break;
                }
                stack.push(OpenDelimiter { mark, delimiter, open_start: index, open_end: index + len });
                index += len;
            } else {
                index = end;
                break;
            }
        }
    }
}

/// Parses headings, quote/list markers, fenced code blocks, inline tokens, and rich spans.
pub fn parse_composer_document(text: &str) -> ComposerRichDoc {
    let code_blocks = parse_fenced_code_blocks(text);
    let tokens = collect_inline_tokens(text, &code_blocks);

    let mut excluded: Vec<Range<usize>> = Vec::with_capacity(code_blocks.len() + tokens.len());
    excluded.extend(code_blocks.iter().map(|b| b.full_range.clone()));
    excluded.extend(tokens.iter().map(|t| t.range.clone()));
    excluded.sort_by_key(|r| (r.start, r.end));

    // Parse inline markdown in all prose segments outside fenced code blocks and inline tokens.
    let mut marks = Vec::new();
    let mut cursor = 0usize;
    for range in &excluded {
        if range.start > cursor {
            parse_inline_piece(&text[cursor..range.start], cursor, &mut marks);
        }
        cursor = cursor.max(range.end);
    }
    if cursor < text.len() {
        parse_inline_piece(&text[cursor..], cursor, &mut marks);
    }
    marks.sort_by_key(|m| (m.full_range.start, m.full_range.end));

    // Parse line prefixes (headings, blockquotes, list markers) outside fenced code blocks.
    let mut prefixes = Vec::new();
    let mut offset = 0usize;
    for raw_line in text.split('\n') {
        let line_start = offset;
        let line_end = line_start + raw_line.len();
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        offset = (line_end + 1).min(text.len());

        let in_code_or_token = code_blocks.iter().any(|b| line_start >= b.full_range.start && line_start < b.full_range.end)
            || tokens.iter().any(|t| line_start > t.range.start && line_start < t.range.end);
        if in_code_or_token {
            continue;
        }
        let hashes = line.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            prefixes.push(BlockPrefixSpan {
                heading_level: Some(hashes as u8),
                full_range: line_start..line_end,
                marker_range: line_start..(line_start + hashes + 1),
                content_range: (line_start + hashes + 1)..line_end,
            });
            continue;
        }
        let indent = line.bytes().take_while(|b| matches!(b, b' ' | b'\t')).count();
        let rest = &line[indent..];
        if rest.starts_with("> ") || rest == ">" {
            let marker_len = if rest.starts_with("> ") { 2 } else { 1 };
            prefixes.push(BlockPrefixSpan {
                heading_level: None,
                full_range: line_start..line_end,
                marker_range: (line_start + indent)..(line_start + indent + marker_len),
                content_range: (line_start + indent + marker_len)..line_end,
            });
            continue;
        }
        if let Some(marker_end) = list_prefix_end(rest) {
            prefixes.push(BlockPrefixSpan {
                heading_level: None,
                full_range: line_start..line_end,
                marker_range: (line_start + indent)..(line_start + indent + marker_end),
                content_range: (line_start + indent + marker_end)..line_end,
            });
        }
    }

    ComposerRichDoc { code_blocks, marks, prefixes, tokens }
}

fn list_prefix_end(rest: &str) -> Option<usize> {
    if rest.starts_with("- [ ] ") || rest.starts_with("- [x] ") || rest.starts_with("- [X] ") {
        return Some(6);
    }
    if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ") {
        return Some(2);
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && rest[digits..].starts_with(". ") || digits > 0 && rest[digits..].starts_with(") ") {
        return Some(digits + 2);
    }
    None
}

/// `composer-code-block.ts` Enter behavior inside a fenced code block (auto-indent, closing
/// fence exit, trailing two blank lines exit, or opening fence auto-close).
pub fn code_block_enter(text: &str, start: usize, end: usize) -> Option<RichTextEdit> {
    if start > text.len() || end > text.len() || start > end {
        return None;
    }
    let blocks = parse_fenced_code_blocks(text);
    let block = blocks.iter().find(|b| start >= b.full_range.start && end <= b.full_range.end)?;

    // Unclosed opening fence line with caret at end: open + close fence around blank body.
    if start == end && start == block.open_fence.end && block.close_fence.is_none() && block.code_range.is_empty() {
        let replacement = format!("\n\n{}", block.fence_str);
        return Some(RichTextEdit {
            start,
            end,
            replacement,
            selection_after: (start + 1)..(start + 1),
        });
    }

    // Pressing Enter at the end of the closing fence exits below the block.
    if start == end
        && let Some(close) = &block.close_fence
        && start == close.end
    {
        return Some(RichTextEdit { start, end, replacement: "\n".to_owned(), selection_after: (start + 1)..(start + 1) });
    }

    if start < block.code_range.start || end > block.code_range.end {
        return None;
    }

    // Exiting on two trailing blank lines at the end of a code block.
    if start == end && start == block.code_range.end {
        let code = &text[block.code_range.start..block.code_range.end];
        if let Some(trailing) = trailing_two_blank_lines(code) {
            let cut_start = start - trailing;
            if let Some(close) = &block.close_fence {
                let tail = &text[cut_start..close.end];
                let replacement = format!("\n{}\n", block.fence_str);
                let after = cut_start + replacement.len();
                let _ = tail;
                return Some(RichTextEdit { start: cut_start, end: close.end, replacement, selection_after: after..after });
            } else {
                let replacement = format!("\n{}\n", block.fence_str);
                let after = cut_start + replacement.len();
                return Some(RichTextEdit { start: cut_start, end: start, replacement, selection_after: after..after });
            }
        }
    }

    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1).max(block.code_range.start);
    let indent_len = text[line_start..start].bytes().take_while(|b| matches!(b, b' ' | b'\t')).count();
    let indent = &text[line_start..line_start + indent_len];
    let replacement = format!("\n{indent}");
    let caret = start + replacement.len();
    Some(RichTextEdit { start, end, replacement, selection_after: caret..caret })
}

fn trailing_two_blank_lines(code: &str) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut i = bytes.len();
    while i > 0 && matches!(bytes[i - 1], b' ' | b'\t') {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] != b'\n' {
        return None;
    }
    i -= 1;
    while i > 0 && matches!(bytes[i - 1], b' ' | b'\t') {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] != b'\n' {
        return None;
    }
    Some(bytes.len() - (i - 1))
}

/// `indentCodeBlock` for Tab / Shift+Tab inside a fenced code block.
pub fn code_block_indent(text: &str, start: usize, end: usize, outdent: bool) -> Option<RichTextEdit> {
    if start > text.len() || end > text.len() || start > end {
        return None;
    }
    let blocks = parse_fenced_code_blocks(text);
    let block = blocks.iter().find(|b| start >= b.code_range.start && end <= b.code_range.end)?;

    if start == end && !outdent {
        return Some(RichTextEdit { start, end, replacement: "  ".to_owned(), selection_after: (start + 2)..(start + 2) });
    }

    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1).max(block.code_range.start);
    let anchor = if end > start { end - 1 } else { end };
    let line_end = text[anchor..].find('\n').map_or(text.len(), |i| anchor + i).min(block.code_range.end);
    let slice = &text[line_start..line_end];

    let mut next_lines = Vec::new();
    let mut first_delta: isize = 0;
    for (idx, line) in slice.split('\n').enumerate() {
        let next = if !outdent {
            if line.is_empty() { line.to_owned() } else { format!("  {line}") }
        } else if let Some(s) = line.strip_prefix("  ") {
            s.to_owned()
        } else if let Some(s) = line.strip_prefix('\t').or_else(|| line.strip_prefix(' ')) {
            s.to_owned()
        } else {
            line.to_owned()
        };
        if idx == 0 {
            first_delta = next.len() as isize - line.len() as isize;
        }
        next_lines.push(next);
    }
    let replacement = next_lines.join("\n");
    let total_delta = replacement.len() as isize - slice.len() as isize;
    let next_start = (start as isize + first_delta).max(line_start as isize) as usize;
    let next_end = (end as isize + total_delta).max(next_start as isize) as usize;
    Some(RichTextEdit { start: line_start, end: line_end, replacement, selection_after: next_start..next_end })
}

/// Toggles `**`, `*`, `` ` ``, or `~~` around `start..end` (`Mod+B`, `Mod+I`, `Mod+E`, `Mod+Shift+X`).
pub fn toggle_inline_format(text: &str, start: usize, end: usize, format: InlineFormat) -> RichTextEdit {
    let (start, end) = if start <= end { (start.min(text.len()), end.min(text.len())) } else { (end.min(text.len()), start.min(text.len())) };
    let delim = format.delimiter();
    let dlen = delim.len();

    // Check if selection is already wrapped immediately by `delim` outside `start..end`.
    if start >= dlen && text[..start].ends_with(delim) && text[end..].starts_with(delim) {
        let inner = text[start..end].to_owned();
        let next_start = start - dlen;
        let next_end = next_start + inner.len();
        return RichTextEdit { start: next_start, end: end + dlen, replacement: inner, selection_after: next_start..next_end };
    }

    // Check if selection itself starts and ends with `delim`.
    if end >= start + 2 * dlen && text[start..end].starts_with(delim) && text[start..end].ends_with(delim) {
        let inner = text[start + dlen..end - dlen].to_owned();
        let next_end = start + inner.len();
        return RichTextEdit { start, end, replacement: inner, selection_after: start..next_end };
    }

    // Check if caret/selection lies inside an existing parsed mark of this kind.
    let doc = parse_composer_document(text);
    if let Some(span) = doc
        .marks
        .iter()
        .find(|m| m.mark == format && start >= m.full_range.start && end <= m.full_range.end)
    {
        let inner = text[span.content_range.clone()].to_owned();
        let open_len = span.open_marker.len();
        let next_start = start.saturating_sub(span.full_range.start + open_len).min(inner.len()) + span.full_range.start;
        let next_end = end.saturating_sub(span.full_range.start + open_len).min(inner.len()) + span.full_range.start;
        return RichTextEdit {
            start: span.full_range.start,
            end: span.full_range.end,
            replacement: inner,
            selection_after: next_start..next_end,
        };
    }

    let selected = &text[start..end];
    let replacement = format!("{delim}{selected}{delim}");
    let next_start = start + dlen;
    let next_end = next_start + selected.len();
    RichTextEdit { start, end, replacement, selection_after: next_start..next_end }
}

#[cfg(test)]
mod tests;
