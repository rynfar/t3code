//! Markdown clipboard helpers (`apps/web/src/components/chat/MarkdownCodeBlock.tsx`,
//! `apps/web/src/components/chat/ChatMarkdown.tsx`):
//! - Drop the fence's single trailing newline on code block copy (`pingdotgg/t3code#37284`)
//! - Restore NBSP indentation injected by `preserve_user_leading_whitespace` back to ASCII
//!   spaces/tabs on user-message Copy and Cite while preserving literal user-typed NBSPs
//!   (`pingdotgg/t3code#38079`)

/// Drops the fence's single closing newline (`code.endsWith("\n") ? code.slice(0, -1) : code`,
/// `pingdotgg/t3code#37284`), normalizing CRLF to LF without stripping intentional extra blank lines.
pub fn strip_code_fence_trailing_newline(code: &str) -> String {
    let normalized = code.replace("\r\n", "\n");
    normalized.strip_suffix('\n').unwrap_or(&normalized).to_owned()
}

fn strip_blockquote_prefix(mut line: &str) -> &str {
    loop {
        let spaces = line.bytes().take_while(|&b| b == b' ').count();
        if spaces <= 3 && line[spaces..].starts_with('>') {
            let mut take = spaces + 1;
            if line[take..].starts_with(' ') {
                take += 1;
            }
            line = &line[take..];
        } else {
            return line;
        }
    }
}

fn expanded_ascii_columns(ascii_ws: &str) -> usize {
    let mut col = 0usize;
    for ch in ascii_ws.chars() {
        let step = if ch == '\t' { ((col / 4) + 1) * 4 - col } else { 1 };
        col += step;
    }
    col
}

/// Restores NBSP line-start padding inserted by `preserve_user_leading_whitespace` (`pingdotgg/t3code#38079`)
/// back to the user's original ASCII spaces/tabs while keeping any literal user-typed `\u{00A0}` intact.
pub fn restore_preserved_leading_whitespace(selected: &str, raw_source: &str) -> String {
    if !selected.contains('\u{00A0}') {
        return selected.to_owned();
    }

    struct RawLine<'a> {
        raw_ascii_ws: &'a str,
        injected_cols: usize,
        literal_nbsp: usize,
        rest: &'a str,
    }

    let raw_lines: Vec<RawLine<'_>> = raw_source
        .split('\n')
        .map(|raw| {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            let unquoted = strip_blockquote_prefix(line);
            let after_ascii = unquoted.trim_start_matches([' ', '\t']);
            let raw_ascii_ws = &unquoted[..unquoted.len() - after_ascii.len()];
            let rest = after_ascii.trim_start_matches('\u{00A0}');
            let literal_nbsp = after_ascii[..after_ascii.len() - rest.len()].chars().count();
            RawLine {
                raw_ascii_ws,
                injected_cols: expanded_ascii_columns(raw_ascii_ws),
                literal_nbsp,
                rest,
            }
        })
        .collect();

    selected
        .split('\n')
        .map(|raw_line| {
            let (line, cr) = match raw_line.strip_suffix('\r') {
                Some(stripped) => (stripped, "\r"),
                None => (raw_line, ""),
            };
            let rest = line.trim_start_matches('\u{00A0}');
            let nbsp_count = line[..line.len() - rest.len()].chars().count();
            if nbsp_count == 0 {
                return raw_line.to_owned();
            }

            let find_match = |exact_rest: bool| {
                raw_lines.iter().find(|candidate| {
                    let rest_matches = if exact_rest {
                        candidate.rest == rest
                    } else {
                        !rest.is_empty() && candidate.rest.starts_with(rest)
                    };
                    rest_matches && candidate.injected_cols + candidate.literal_nbsp >= nbsp_count
                })
            };

            if let Some(candidate) = find_match(true).or_else(|| find_match(false)) {
                let total = candidate.injected_cols + candidate.literal_nbsp;
                if nbsp_count == total {
                    let mut out = String::with_capacity(
                        candidate.raw_ascii_ws.len() + candidate.literal_nbsp * '\u{00A0}'.len_utf8() + rest.len() + cr.len(),
                    );
                    out.push_str(candidate.raw_ascii_ws);
                    for _ in 0..candidate.literal_nbsp {
                        out.push('\u{00A0}');
                    }
                    out.push_str(rest);
                    out.push_str(cr);
                    return out;
                }
                let literal_keep = nbsp_count.min(candidate.literal_nbsp);
                let ascii_spaces = nbsp_count - literal_keep;
                let mut out = String::with_capacity(ascii_spaces + literal_keep * '\u{00A0}'.len_utf8() + rest.len() + cr.len());
                for _ in 0..ascii_spaces {
                    out.push(' ');
                }
                for _ in 0..literal_keep {
                    out.push('\u{00A0}');
                }
                out.push_str(rest);
                out.push_str(cr);
                return out;
            }

            let mut out = String::with_capacity(nbsp_count + rest.len() + cr.len());
            for _ in 0..nbsp_count {
                out.push(' ');
            }
            out.push_str(rest);
            out.push_str(cr);
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_code_fence_trailing_newline_drops_only_closing_newline() {
        assert_eq!(strip_code_fence_trailing_newline("sudo pacman -S patchelf\n"), "sudo pacman -S patchelf");
        assert_eq!(strip_code_fence_trailing_newline("echo hi\r\n"), "echo hi");
        assert_eq!(strip_code_fence_trailing_newline("echo hi\n\n"), "echo hi\n");
        assert_eq!(strip_code_fence_trailing_newline("echo hi"), "echo hi");
    }

    #[test]
    fn restore_preserved_leading_whitespace_restores_spaces_tabs_and_preserves_literal_nbsp() {
        let raw = "  alpha\n\tbeta\n\u{00A0}\u{00A0}literal\n  \u{00A0}mixed\n>   quoted\n  **bold**";
        let rendered = "\u{00A0}\u{00A0}alpha\n\u{00A0}\u{00A0}\u{00A0}\u{00A0}beta\n\u{00A0}\u{00A0}literal\n\u{00A0}\u{00A0}\u{00A0}mixed\n\u{00A0}\u{00A0}quoted\n\u{00A0}\u{00A0}bold";
        assert_eq!(
            restore_preserved_leading_whitespace(rendered, raw),
            "  alpha\n\tbeta\n\u{00A0}\u{00A0}literal\n  \u{00A0}mixed\n  quoted\n  bold"
        );
        // Partial selection starting inside leading indentation keeps literal NBSPs and restores ASCII spaces.
        assert_eq!(restore_preserved_leading_whitespace("\u{00A0}\u{00A0}mixed", raw), " \u{00A0}mixed");
        assert_eq!(restore_preserved_leading_whitespace("\u{00A0}alp", raw), " alp");
    }
}
