//! Markdown list editing in the prompt. Ports `listContinuationForEnter`, `listIndentForTab`,
//! and `listOutdentForTab` from `web/src/composer-list-continuation.ts`.
//! Offsets are UTF-8 byte offsets, as the editor reports them.

use std::ops::Range;

/// A text replacement and where the caret lands after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEdit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
    pub cursor_after: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Ordered { number: String, delimiter: char },
    Task,
    Bullet { ch: char },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Marker {
    kind: Kind,
    indent: String,
    marker_end: usize,
    has_space: bool,
    content_column: usize,
}

fn content_column(indent: &str, marker_width: usize, padding: &str) -> usize {
    let marker_end = indent_width(indent) + marker_width;
    let space = padding_width(padding, marker_end);
    marker_end + if (1..=4).contains(&space) { space } else { 1 }
}

fn padding_width(padding: &str, from_column: usize) -> usize {
    let mut column = from_column;
    for byte in padding.bytes() {
        match byte {
            b' ' => column += 1,
            b'\t' => column += 4 - column % 4,
            _ => break,
        }
    }
    column - from_column
}

fn next_marker_text(marker: &Marker) -> String {
    match &marker.kind {
        Kind::Ordered { number, delimiter } => format!("{}{}{delimiter} ", marker.indent, next_number(number)),
        Kind::Task => format!("{}- [ ] ", marker.indent),
        Kind::Bullet { ch } => format!("{}{ch} ", marker.indent),
    }
}

fn ws_prefix(s: &str) -> (usize, &str) {
    let len = s.bytes().take_while(|b| matches!(b, b' ' | b'\t')).count();
    (len, &s[..len])
}

fn parse_marker(line: &str) -> Option<Marker> {
    let indent_len = line.bytes().take_while(|byte| matches!(byte, b' ' | b'\t')).count();
    let (indent, rest) = line.split_at(indent_len);
    let spaced_or_end = |after: &str| after.is_empty() || after.starts_with([' ', '\t']) || after.trim().is_empty();
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        let after_number = &rest[digits..];
        if let Some(delimiter) = after_number.chars().next().filter(|c| matches!(c, '.' | ')'))
            && spaced_or_end(&after_number[1..])
        {
            let (space_len, space) = ws_prefix(&after_number[1..]);
            return Some(Marker {
                kind: Kind::Ordered { number: rest[..digits].to_owned(), delimiter },
                indent: indent.to_owned(),
                marker_end: indent_len + digits + 1 + space_len,
                has_space: !space.is_empty() || after_number[1..].trim().is_empty() && !after_number[1..].is_empty(),
                content_column: content_column(indent, digits + 1, space),
            });
        }
    }
    if rest.starts_with('-')
        && rest[1..].starts_with([' ', '\t'])
        && rest.get(2..5).is_some_and(|checkbox| matches!(checkbox, "[ ]" | "[x]" | "[X]"))
        && spaced_or_end(&rest[5..])
    {
        let (space_len, space) = ws_prefix(&rest[5..]);
        return Some(Marker {
            kind: Kind::Task,
            indent: indent.to_owned(),
            marker_end: indent_len + 5 + space_len,
            has_space: !space.is_empty() || !rest[5..].is_empty(),
            content_column: content_column(indent, 1, &rest[1..2]),
        });
    }
    if let Some(ch) = rest.chars().next().filter(|c| matches!(c, '-' | '*' | '+'))
        && spaced_or_end(&rest[1..])
    {
        let (space_len, space) = ws_prefix(&rest[1..]);
        return Some(Marker {
            kind: Kind::Bullet { ch },
            indent: indent.to_owned(),
            marker_end: indent_len + 1 + space_len,
            has_space: !space.is_empty() || !rest[1..].is_empty(),
            content_column: content_column(indent, 1, space),
        });
    }
    None
}

fn inline_token_ranges(value: &str) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = crate::composer::menu::context_references(value)
        .into_iter()
        .map(|r| r.start..r.end)
        .chain(crate::assistant_citations::collect(value).into_iter().map(|(r, _)| r))
        .chain(crate::markdown::mentions::collect_composer_model_mentions(value).into_iter().map(|m| m.start..m.end))
        .collect();
    ranges.sort_by_key(|r| r.start);
    ranges
}

fn is_inside_inline_token(tokens: &[Range<usize>], cursor: usize) -> bool {
    tokens.iter().any(|range| cursor > range.start && cursor < range.end)
}

/// `indentWidth`: tabs advance to the next four-column stop.
fn indent_width(text: &str) -> usize {
    let mut column = 0;
    for byte in text.bytes() {
        match byte {
            b' ' => column += 1,
            b'\t' => column += 4 - column % 4,
            _ => break,
        }
    }
    column
}

fn line_at(value: &str, cursor: usize) -> (usize, usize) {
    let start = value[..cursor].rfind('\n').map_or(0, |index| index + 1);
    let end = value[cursor..].find('\n').map_or(value.len(), |index| cursor + index);
    (start, end)
}

/// `parentMarker`: the nearest list line above with a shallower indent, within the same list.
fn parent_marker(value: &str, line_start: usize, marker: &Marker) -> Option<Marker> {
    let width = indent_width(&marker.indent);
    let mut start = line_start;
    while start > 0 {
        let end = start - 1;
        start = value[..end].rfind('\n').map_or(0, |index| index + 1);
        let above = parse_marker(&value[start..end])?;
        if indent_width(&above.indent) < width {
            return Some(above);
        }
    }
    None
}

/// `nextOrderedMarkerText` without the delimiter: the next number, keeping zero padding.
fn next_number(number: &str) -> String {
    match number.parse::<u64>() {
        Ok(value) => format!("{:0width$}", value + 1, width = number.len()),
        Err(_) => number.to_owned(),
    }
}

/// `renumberFollowingItems`: items after the outdented one at the parent's level count on.
fn renumber_following(value: &str, after_line_end: usize, parent_number: &str, delimiter: char, base_width: usize) -> Option<(usize, String)> {
    let mut expected = parent_number.parse::<u64>().ok()? + 2;
    let mut parts = String::new();
    let mut copied_until = after_line_end;
    let mut position = after_line_end;
    while position < value.len() {
        let line_start = position + 1;
        let line_end = value[line_start..].find('\n').map_or(value.len(), |index| line_start + index);
        position = line_end;
        let line = &value[line_start..line_end];
        let width = indent_width(line);
        if line.trim().is_empty() || width < base_width {
            break;
        }
        if width > base_width {
            continue;
        }
        let Some(Marker { kind: Kind::Ordered { number, delimiter: found }, indent, .. }) = parse_marker(line) else { break };
        if found != delimiter {
            break;
        }
        let number_start = line_start + indent.len();
        let replacement = if number.starts_with('0') { format!("{:0width$}", expected, width = number.len()) } else { expected.to_string() };
        if replacement != number {
            parts.push_str(&value[copied_until..number_start]);
            parts.push_str(&replacement);
            copied_until = number_start + number.len();
        }
        expected += 1;
    }
    (!parts.is_empty()).then_some((copied_until, parts))
}

/// `listOutdentForTab`: moves a nested list item one level out, renumbering ordered siblings.
/// `None` when the caret is not on a nested list item, so Shift+Tab can do something else.
pub fn outdent_for_tab(value: &str, start: usize, end: usize) -> Option<ListEdit> {
    if start != end || start > value.len() || !value.is_char_boundary(start) {
        return None;
    }
    let tokens = inline_token_ranges(value);
    if is_inside_inline_token(&tokens, start) {
        return None;
    }
    let (line_start, line_end) = line_at(value, start);
    let marker = parse_marker(&value[line_start..line_end])?;
    if marker.indent.is_empty() {
        return None;
    }
    let parent = parent_marker(value, line_start, &marker);
    let indent = match &parent {
        Some(parent) => parent.indent.clone(),
        // With no parent above, undo one step of Tab's two-space fallback.
        None => {
            let trimmed = marker.indent.strip_prefix('\t').or_else(|| marker.indent.strip_prefix("  ")).or_else(|| marker.indent.strip_prefix(' '));
            trimmed.unwrap_or(&marker.indent).to_owned()
        }
    };
    let continues = match (&parent, &marker.kind) {
        (Some(Marker { kind: Kind::Ordered { delimiter: parent_delimiter, .. }, .. }), Kind::Ordered { delimiter, .. }) => parent_delimiter == delimiter,
        _ => false,
    };
    let (replaced_length, number_text) = match (&marker.kind, &parent) {
        (Kind::Ordered { number, .. }, Some(Marker { kind: Kind::Ordered { number: parent_number, .. }, .. })) if continues => {
            (number.len(), next_number(parent_number))
        }
        _ => (0, String::new()),
    };
    let replaced_end = line_start + marker.indent.len() + replaced_length;
    let replacement = format!("{indent}{number_text}");
    let cursor_after =
        if start < replaced_end { line_start + indent.len() } else { start - replaced_end + line_start + replacement.len() };
    let renumbered = match (&parent, continues) {
        (Some(Marker { kind: Kind::Ordered { number, delimiter }, indent: parent_indent, .. }), true) => {
            renumber_following(value, line_end, number, *delimiter, indent_width(parent_indent))
        }
        _ => None,
    };
    Some(match renumbered {
        Some((renumbered_end, text)) => ListEdit {
            start: line_start,
            end: renumbered_end,
            replacement: format!("{replacement}{}{text}", &value[replaced_end..line_end]),
            cursor_after,
        },
        None => ListEdit { start: line_start, end: replaced_end, replacement, cursor_after },
    })
}

/// `listContinuationForEnter`: continues a non-empty list item on Enter, or lifts/exits an
/// empty list item while renumbering ordered siblings.
pub fn continuation_for_enter(value: &str, cursor: usize) -> Option<ListEdit> {
    if cursor > value.len() || !value.is_char_boundary(cursor) {
        return None;
    }
    let tokens = inline_token_ranges(value);
    if is_inside_inline_token(&tokens, cursor) {
        return None;
    }
    let (line_start, line_end) = line_at(value, cursor);
    let marker = parse_marker(&value[line_start..line_end])?;
    if !marker.has_space {
        return None;
    }
    let marker_end = line_start + marker.marker_end;
    if cursor < marker_end {
        return None;
    }
    if value[marker_end..line_end].trim().is_empty() {
        let parent = parent_marker(value, line_start, &marker);
        let replacement = parent.as_ref().map(next_marker_text).unwrap_or_default();
        let cursor_after = line_start + replacement.len();
        let end = cursor.max(marker_end);
        let renumbered = match &parent {
            Some(Marker { kind: Kind::Ordered { number, delimiter }, indent, .. }) => {
                renumber_following(value, line_end, number, *delimiter, indent_width(indent))
            }
            _ => None,
        };
        return Some(match renumbered {
            Some((renumbered_end, text)) => ListEdit {
                start: line_start,
                end: renumbered_end,
                replacement: format!("{replacement}{}{text}", &value[end..line_end]),
                cursor_after,
            },
            None => ListEdit { start: line_start, end, replacement, cursor_after },
        });
    }
    let insertion = format!("\n{}", next_marker_text(&marker));
    let cursor_after = cursor + insertion.len();
    if let Kind::Ordered { ref number, delimiter } = marker.kind
        && let Some((renumbered_end, text)) = renumber_following(value, line_end, number, delimiter, indent_width(&marker.indent))
    {
        return Some(ListEdit {
            start: cursor,
            end: renumbered_end,
            replacement: format!("{insertion}{}{text}", &value[cursor..line_end]),
            cursor_after,
        });
    }
    Some(ListEdit { start: cursor, end: cursor, replacement: insertion, cursor_after })
}

/// `listIndentForTab`: indents the list item under the caret to belong to the preceding sibling.
pub fn indent_for_tab(value: &str, start: usize, end: usize) -> Option<ListEdit> {
    if start != end || start > value.len() || !value.is_char_boundary(start) {
        return None;
    }
    let tokens = inline_token_ranges(value);
    if is_inside_inline_token(&tokens, start) {
        return None;
    }
    let (line_start, line_end) = line_at(value, start);
    let marker = parse_marker(&value[line_start..line_end])?;
    let width = indent_width(&marker.indent);
    let mut previous_child: Option<Marker> = None;
    let mut scan = line_start;
    while scan > 0 {
        let prev_end = scan - 1;
        scan = value[..prev_end].rfind('\n').map_or(0, |index| index + 1);
        let Some(above) = parse_marker(&value[scan..prev_end]) else { break };
        let above_width = indent_width(&above.indent);
        if above_width < width {
            break;
        }
        if above_width > width {
            if previous_child.as_ref().is_none_or(|c| above_width < indent_width(&c.indent)) {
                previous_child = Some(above);
            }
            continue;
        }
        let indent = previous_child
            .as_ref()
            .map(|c| c.indent.clone())
            .unwrap_or_else(|| format!("{}{}", above.indent, " ".repeat(above.content_column.saturating_sub(above_width))));
        let (replaced_length, number_text) = match &marker.kind {
            Kind::Ordered { number, delimiter } => {
                let text = match &previous_child {
                    Some(Marker { kind: Kind::Ordered { number: prev_num, delimiter: prev_delim }, .. })
                        if prev_delim == delimiter =>
                    {
                        next_number(prev_num)
                    }
                    _ => "1".to_owned(),
                };
                (number.len(), text)
            }
            _ => (0, String::new()),
        };
        let replaced_end = line_start + marker.indent.len() + replaced_length;
        let replacement = format!("{indent}{number_text}");
        let cursor_after = if start < replaced_end {
            line_start + indent.len()
        } else {
            start - replaced_end + line_start + replacement.len()
        };
        return Some(ListEdit { start: line_start, end: replaced_end, replacement, cursor_after });
    }
    Some(ListEdit { start: line_start, end: line_start, replacement: "  ".to_owned(), cursor_after: start + 2 })
}

/// The text after applying `edit`, for callers that cannot edit in place.
pub fn apply(value: &str, edit: &ListEdit) -> String {
    format!("{}{}{}", &value[..edit.start], edit.replacement, &value[edit.end..])
}

#[cfg(test)]
mod tests;
