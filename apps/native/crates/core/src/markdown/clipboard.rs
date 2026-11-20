//! Markdown clipboard serialization helpers (`apps/web/src/markdown-clipboard.ts`,
//! `apps/web/src/components/chat/MarkdownCodeBlock.tsx`):
//! - Drop the fence's single trailing newline on code block copy (`pingdotgg/t3code#37284`)
//! - Pad inline code spans with meaningful leading+trailing spaces (`pingdotgg/t3code#36712`)
//! - Preserve `mailto:` email links and escape destinations cleanly (`pingdotgg/t3code#36830`)
//! - Escape literal inline Markdown prose while preserving Windows drive paths (`pingdotgg/t3code#36867`)
//! - Keep ordered list item numbers and task checkboxes when copying list slices (`pingdotgg/t3code#36915`, `pingdotgg/t3code#39150`)
//! - Strip leading-whitespace display padding on clipboard copy (`pingdotgg/t3code#38079`)

/// Drops the fence's single closing newline (`code.endsWith("\n") ? code.slice(0, -1) : code`,
/// `pingdotgg/t3code#37284`), normalizing CRLF to LF without stripping intentional extra blank lines.
pub fn strip_code_fence_trailing_newline(code: &str) -> String {
    let normalized = code.replace("\r\n", "\n");
    normalized.strip_suffix('\n').unwrap_or(&normalized).to_owned()
}

/// Strips non-breaking-space line-start padding inserted for user-message display (`pingdotgg/t3code#38079`)
/// so copied text pastes cleanly without turning lines into indented code blocks.
pub fn strip_preserved_leading_whitespace(text: &str) -> String {
    if !text.contains('\u{00A0}') {
        return text.to_owned();
    }
    text.split('\n')
        .map(|line| line.trim_start_matches('\u{00A0}'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wraps inline code in a backtick fence longer than any internal run, padding with one space when
/// the content starts/ends with a backtick or has meaningful edge spaces (`pingdotgg/t3code#36712`).
pub fn wrap_inline_code(code: &str) -> String {
    let mut longest_run = 0usize;
    let mut current_run = 0usize;
    for ch in code.chars() {
        if ch == '`' {
            current_run += 1;
            longest_run = longest_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    let fence = "`".repeat((longest_run + usize::from(longest_run > 0)).max(1));
    // CommonMark strips one edge space from content that has both and isn't all spaces.
    let space_edges = code.starts_with(' ') && code.ends_with(' ') && code.chars().any(|c| c != ' ');
    let pad = if space_edges || code.starts_with('`') || code.ends_with('`') { " " } else { "" };
    format!("{fence}{pad}{code}{pad}{fence}")
}

fn escape_table_pipes(text: &str, in_table_cell: bool) -> String {
    if in_table_cell {
        text.replace('|', "\\|")
    } else {
        text.to_owned()
    }
}

fn is_ascii_md_punctuation(ch: char) -> bool {
    matches!(ch, '!'..='/' | ':'..='@' | '['..='`' | '{'..='~')
}

fn unescape_punctuation(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(&next) = chars.peek()
            && is_ascii_md_punctuation(next)
        {
            out.push(next);
            chars.next();
            continue;
        }
        out.push(ch);
    }
    out
}

/// Returns whether `address` matches GFM's bare email autolink shape (`serializeAnchor`, `pingdotgg/t3code#36830`).
fn is_gfm_bare_email(address: &str) -> bool {
    let Some((local, domain)) = address.split_once('@') else { return false };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return false;
    }
    if !local.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-')) {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|label| label.is_empty() || !label.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))) {
        return false;
    }
    address.chars().last().is_some_and(|c| c.is_ascii_alphabetic())
}

/// `serializeAnchor` (`markdown-clipboard.ts`, `pingdotgg/t3code#36830` & `pingdotgg/t3code#36867`):
/// serializes an `http(s)://` or `mailto:` anchor back into Markdown.
pub fn serialize_markdown_anchor(label: &str, href: &str, in_table_cell: bool) -> String {
    let lower = href.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")) {
        return label.to_owned();
    }
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let shown = unescape_punctuation(trimmed);
    if shown == href {
        return escape_table_pipes(href, in_table_cell);
    }
    if let Some(target) = href.strip_prefix("mailto:")
        && target == shown
        && is_gfm_bare_email(&shown)
    {
        return shown;
    }
    let mut escaped_dest = String::with_capacity(href.len());
    for ch in href.chars() {
        if matches!(ch, '\\' | '(' | ')') {
            escaped_dest.push('\\');
        }
        escaped_dest.push(ch);
    }
    let destination = escape_table_pipes(&escaped_dest, in_table_cell);
    format!("[{trimmed}]({destination})")
}

/// Finds the byte index after the drive prefix (`C:`) of an unfinished Windows drive path inside
/// a single whitespace-delimited word (`UNFINISHED_DRIVE_PATH`).
fn windows_drive_start_in_word(word: &str) -> Option<usize> {
    let bytes = word.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
        if boundary && bytes[i].is_ascii_alphabetic() && bytes[i + 1] == b':' {
            let after = i + 2;
            if after == bytes.len() || bytes[after] == b'\\' || bytes[after] == b'/' {
                return Some(after);
            }
        }
    }
    None
}

/// `escapeProse` (`markdown-clipboard.ts`, `pingdotgg/t3code#36867`): escapes rendered prose so it
/// parses back to the same text when pasted into the composer, leaving Windows drive paths and
/// intra-word underscores unescaped.
pub fn escape_markdown_prose(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut idx = 0usize;
    while idx < chars.len() {
        if chars[idx].1.is_whitespace() {
            out.push(chars[idx].1);
            idx += 1;
            continue;
        }
        let word_start_char = idx;
        while idx < chars.len() && !chars[idx].1.is_whitespace() {
            idx += 1;
        }
        let byte_start = chars[word_start_char].0;
        let byte_end = if idx < chars.len() { chars[idx].0 } else { text.len() };
        let word = &text[byte_start..byte_end];
        let drive_start = windows_drive_start_in_word(word).unwrap_or(usize::MAX);

        let wchars: Vec<(usize, char)> = word.char_indices().collect();
        let mut wi = 0usize;
        while wi < wchars.len() {
            let (rel_byte, ch) = wchars[wi];
            if rel_byte >= drive_start {
                out.push_str(&word[rel_byte..]);
                break;
            }
            let global_pos = word_start_char + wi;
            let before = global_pos.checked_sub(1).map(|p| chars[p].1);
            if ch == '\\' {
                let after = chars.get(global_pos + 1).map(|(_, c)| *c);
                let needs_escape = match after {
                    None => true,
                    Some('\n') => true,
                    Some(c) if is_ascii_md_punctuation(c) => true,
                    _ => false,
                };
                if needs_escape {
                    out.push('\\');
                }
                out.push(ch);
                wi += 1;
                continue;
            }
            if matches!(ch, '`' | '[' | ']') {
                out.push('\\');
                out.push(ch);
                wi += 1;
                continue;
            }
            if matches!(ch, '*' | '_' | '~') {
                let run_start = wi;
                while wi < wchars.len() && wchars[wi].1 == ch {
                    wi += 1;
                }
                let run_len = wi - run_start;
                let after = chars.get(word_start_char + wi).map(|(_, c)| *c);
                if ch == '~' && run_len < 2 {
                    out.push('~');
                    continue;
                }
                let between_space = before.is_some_and(char::is_whitespace) && after.is_some_and(char::is_whitespace);
                let between_word = ch == '_'
                    && before.is_some_and(char::is_alphanumeric)
                    && after.is_some_and(char::is_alphanumeric);
                if between_space || between_word {
                    for _ in 0..run_len {
                        out.push(ch);
                    }
                } else {
                    for _ in 0..run_len {
                        out.push('\\');
                        out.push(ch);
                    }
                }
                continue;
            }
            out.push(ch);
            wi += 1;
        }
    }
    out
}

/// One item in a copied list slice (`pingdotgg/t3code#36915`, `pingdotgg/t3code#39150`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListCopyItem<'a> {
    /// Explicit item number override (e.g. HTML `<li value="N">`), or `None` to increment from previous.
    pub value: Option<usize>,
    /// Task checkbox state (`Some(true)` -> `[x] `, `Some(false)` -> `[ ] `, `None` -> plain item).
    pub checked: Option<bool>,
    pub text: &'a str,
}

/// Serializes a list slice (ordered or unordered) preserving the starting item number and any
/// per-item `value` / task checkbox markers (`pingdotgg/t3code#36915`, `pingdotgg/t3code#39150`).
pub fn serialize_list_slice(ordered: bool, start_number: usize, items: &[ListCopyItem<'_>]) -> String {
    let mut next = start_number.max(1);
    let mut lines = Vec::with_capacity(items.len());
    for item in items {
        if let Some(explicit) = item.value {
            next = explicit;
        }
        let bullet = if ordered { format!("{next}. ") } else { "- ".to_owned() };
        next += 1;
        let task = match item.checked {
            Some(true) => "[x] ",
            Some(false) => "[ ] ",
            None => "",
        };
        let marker = format!("{bullet}{task}");
        let indent = " ".repeat(marker.len());
        let mut item_lines = item.text.lines();
        if let Some(first) = item_lines.next() {
            let mut rendered = format!("{marker}{first}");
            for cont in item_lines {
                rendered.push('\n');
                if !cont.is_empty() {
                    rendered.push_str(&indent);
                    rendered.push_str(cont);
                }
            }
            lines.push(rendered);
        } else {
            lines.push(marker.trim_end().to_owned());
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_fence_copy_drops_single_trailing_newline_only() {
        assert_eq!(strip_code_fence_trailing_newline("sudo pacman -S patchelf\n"), "sudo pacman -S patchelf");
        assert_eq!(strip_code_fence_trailing_newline("echo hi\r\n"), "echo hi");
        assert_eq!(strip_code_fence_trailing_newline("echo hi\n\n"), "echo hi\n");
        assert_eq!(strip_code_fence_trailing_newline("echo hi"), "echo hi");
    }

    #[test]
    fn inline_code_keeps_meaningful_edge_spaces_and_backticks() {
        assert_eq!(wrap_inline_code(" foo "), "`  foo  `");
        assert_eq!(wrap_inline_code("   "), "`   `");
        assert_eq!(wrap_inline_code("`x`"), "`` `x` ``");
        assert_eq!(wrap_inline_code("plain"), "`plain`");
    }

    #[test]
    fn email_links_keep_destination_and_escape_special_chars() {
        assert_eq!(serialize_markdown_anchor("user@example.com", "mailto:user@example.com", false), "user@example.com");
        assert_eq!(serialize_markdown_anchor("Email us", "mailto:support@example.com", false), "[Email us](mailto:support@example.com)");
        assert_eq!(serialize_markdown_anchor("a@b.1", "mailto:a@b.1", false), "[a@b.1](mailto:a@b.1)");
        assert_eq!(
            serialize_markdown_anchor("docs", "https://example.com/a(b)|c", true),
            r"[docs](https://example.com/a\(b\)\|c)"
        );
    }

    #[test]
    fn escape_prose_escapes_literal_markdown_without_breaking_identifiers_or_windows_paths() {
        assert_eq!(escape_markdown_prose("foo_bar"), "foo_bar");
        assert_eq!(escape_markdown_prose("*not italic*"), r"\*not italic\*");
        assert_eq!(escape_markdown_prose("a * b"), "a * b");
        assert_eq!(escape_markdown_prose("~~strike~~"), r"\~\~strike\~\~");
        assert_eq!(escape_markdown_prose("~tilde~"), "~tilde~");
        assert_eq!(escape_markdown_prose(r"C:\Users\me_dir\[a]"), r"C:\Users\me_dir\[a]");
    }

    #[test]
    fn list_slice_keeps_start_number_values_and_task_checkboxes() {
        let items = [
            ListCopyItem { value: None, checked: None, text: "Third" },
            ListCopyItem { value: Some(7), checked: Some(true), text: "Seventh\nmore" },
        ];
        assert_eq!(serialize_list_slice(true, 3, &items), "3. Third\n7. [x] Seventh\n       more");
        assert_eq!(
            serialize_list_slice(false, 1, &[ListCopyItem { value: None, checked: Some(false), text: "Todo" }]),
            "- [ ] Todo"
        );
    }

    #[test]
    fn strips_preserved_leading_whitespace_on_clipboard_copy() {
        assert_eq!(strip_preserved_leading_whitespace("\u{00A0}\u{00A0}hello\n\u{00A0}\u{00A0}\u{00A0}\u{00A0}world"), "hello\nworld");
    }
}
