//! Rich Markdown helpers for `apps/native` (`TR-MARKDOWN`): fenced code metadata & shell run
//! detection, GFM table serialization, GitHub Alerts, `<details>`/`<picture>` preprocessing,
//! LaTeX math formatting, Mermaid SVG rendering, and file/thread/citation link classification.

pub mod alerts_details;
pub mod clipboard;
pub mod code;
pub mod links;
pub mod math;
pub mod mentions;
pub mod mermaid;
pub mod table;

/// Options controlling display preprocessing of Markdown text.
#[derive(Debug, Clone, Default)]
pub struct DisplayMarkdownOptions<'a> {
    pub is_dark: bool,
    pub cwd: Option<&'a str>,
    pub math_enabled: bool,
}

/// Applies `<picture>` theme selection, `<details>` disclosure extraction, optional LaTeX math
/// formatting, and inline-code file-path linking to `source` for rendering in `TextView`.
pub fn prepare_display_markdown(source: &str, options: DisplayMarkdownOptions<'_>) -> String {
    prepare_display_markdown_with_raw_html(source, options, true)
}

/// Same as [`prepare_display_markdown`], letting user/work messages disable raw HTML expansion
/// (`parseRawHtml={false}`) while keeping inline file linking and optional math.
pub fn prepare_display_markdown_with_raw_html(source: &str, options: DisplayMarkdownOptions<'_>, parse_raw_html: bool) -> String {
    prepare_display_markdown_for_surface(source, options, parse_raw_html, false)
}

/// Full display preparation for a message surface, optionally converting assistant `★ Insight`
/// blocks into `[!INSIGHT]` callouts before HTML/file/math preprocessing.
pub fn prepare_display_markdown_for_surface(
    source: &str,
    options: DisplayMarkdownOptions<'_>,
    parse_raw_html: bool,
    render_insights: bool,
) -> String {
    let with_insights = if render_insights {
        alerts_details::preprocess_assistant_insights(source)
    } else {
        source.to_owned()
    };
    let with_html_and_files = alerts_details::preprocess_markdown_for_display_with_html(&with_insights, options.is_dark, options.cwd, parse_raw_html);
    if options.math_enabled {
        math::preprocess_math_for_display(&with_html_and_files)
    } else {
        with_html_and_files
    }
}

/// Returns true when the Markdown text contains syntax handled by custom extensions or display
/// preprocessing (code blocks, tables, blockquotes/alerts, insights, math, details, HTML/markdown media, or file paths).
pub fn has_rich_markdown_syntax(source: &str) -> bool {
    source.contains("```")
        || source.contains("~~~")
        || source.contains('>')
        || source.contains('|')
        || source.contains("![")
        || source.contains("★ Insight")
        || contains_ascii_case_insensitive_tag(source)
        || math::might_contain_math(source)
        || source.contains('`')
}

fn contains_ascii_case_insensitive_tag(source: &str) -> bool {
    if !source.contains('<') {
        return false;
    }
    let lower = source.to_ascii_lowercase();
    ["<details", "<picture", "<img", "<video", "<table", "<h1", "<h2", "<h3", "<h4", "<h5", "<h6"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// Formats copied text cleanly (normalizing CRLF and trimming excess trailing blank lines).
pub fn normalize_clipboard_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end_matches('\n').to_owned()
}

/// Drops the fence's single closing newline on code block copy (`pingdotgg/t3code#37284`).
pub fn strip_code_fence_trailing_newline(code: &str) -> String {
    clipboard::strip_code_fence_trailing_newline(code)
}

/// Text reading direction (`packages/shared/src/textDirection.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDirection {
    Ltr,
    Rtl,
}

fn is_rtl_letter(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x0590..=0x08FF
            | 0xFB1D..=0xFDFF
            | 0xFE70..=0xFEFF
            | 0x10800..=0x10FFF
            | 0x1E800..=0x1EFFF
    )
}

fn word_direction(word: &str) -> Option<TextDirection> {
    let letter = word.chars().find(|c| c.is_alphabetic())?;
    Some(if is_rtl_letter(letter) { TextDirection::Rtl } else { TextDirection::Ltr })
}

fn majority(
    units: impl IntoIterator<Item = Option<TextDirection>>,
    tie: Option<&dyn Fn() -> Option<TextDirection>>,
) -> Option<TextDirection> {
    let mut rtl = 0usize;
    let mut ltr = 0usize;
    let mut first = None;
    for dir in units.into_iter().flatten() {
        first.get_or_insert(dir);
        match dir {
            TextDirection::Rtl => rtl += 1,
            TextDirection::Ltr => ltr += 1,
        }
    }
    if rtl == ltr {
        tie.and_then(|f| f()).or(first)
    } else if rtl > ltr {
        Some(TextDirection::Rtl)
    } else {
        Some(TextDirection::Ltr)
    }
}

fn split_sentences(text: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            sentences.push(&text[start..i]);
            while i < bytes.len() && bytes[i] == b'\n' {
                i += 1;
            }
            start = i;
            continue;
        }
        let ch = text[i..].chars().next().unwrap();
        let next_i = i + ch.len_utf8();
        if matches!(ch, '.' | '!' | '?' | '؟') && text[next_i..].starts_with(char::is_whitespace) {
            let prefix = text[start..next_i].to_ascii_lowercase();
            let abbrev = prefix.ends_with("e.g.") || prefix.ends_with("i.e.");
            if !abbrev {
                sentences.push(&text[start..next_i]);
                let rest = &text[next_i..];
                let ws_len = rest.len() - rest.trim_start().len();
                i = next_i + ws_len;
                start = i;
                continue;
            }
        }
        i = next_i;
    }
    if start <= text.len() {
        sentences.push(&text[start..]);
    }
    sentences
}

/// `textDirection` (`packages/shared/src/textDirection.ts`, `pingdotgg/t3code#36876`, `#36922`):
/// Each sentence follows its main language by word count, and the text follows most sentences.
pub fn text_direction(text: &str) -> Option<TextDirection> {
    if !text.chars().any(is_rtl_letter) {
        return text.chars().any(char::is_alphabetic).then_some(TextDirection::Ltr);
    }
    let sentence_dirs: Vec<Option<TextDirection>> = split_sentences(text)
        .into_iter()
        .map(|sentence| majority(sentence.split_whitespace().map(word_direction), None))
        .collect();
    majority(sentence_dirs, Some(&|| majority(text.split_whitespace().map(word_direction), None)))
}

pub fn is_rtl_text(text: &str) -> bool {
    text_direction(text) == Some(TextDirection::Rtl)
}

fn is_list_or_heading_line(trimmed: &str) -> bool {
    if trimmed.starts_with('#') {
        let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].is_empty() || trimmed[hashes..].starts_with([' ', '\t']) {
            return true;
        }
    }
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits)
        && trimmed[digits..].starts_with(['.', ')'])
        && trimmed[digits + 1..].starts_with([' ', '\t'])
    {
        return true;
    }
    if ["---", "***", "___"].iter().any(|rule| trimmed.starts_with(rule) && trimmed.chars().all(|c| c == rule.chars().next().unwrap() || c == ' ')) {
        return true;
    }
    false
}

fn is_github_alert_marker(trimmed: &str) -> bool {
    let upper = trimmed.to_ascii_uppercase();
    ["[!NOTE]", "[!TIP]", "[!IMPORTANT]", "[!WARNING]", "[!CAUTION]"]
        .iter()
        .any(|marker| upper == *marker || upper.starts_with(&format!("{marker}\r")))
}

/// Preserves user-typed leading spaces/tabs on ordinary paragraph lines (including past `> ` quote
/// prefixes) by converting them to non-breaking space columns (`pingdotgg/t3code#38079`), while
/// leaving fenced code blocks, headings, lists, and GitHub alert markers untouched.
pub fn preserve_user_leading_whitespace(source: &str) -> String {
    if !source.contains([' ', '\t']) {
        return source.to_owned();
    }
    let mut out = String::with_capacity(source.len());
    let mut open_fence: Option<(u8, usize)> = None;
    let lines: Vec<&str> = source.split('\n').collect();
    for (idx, raw_line) in lines.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let (line, cr) = match raw_line.strip_suffix('\r') {
            Some(stripped) => (stripped, "\r"),
            None => (*raw_line, ""),
        };
        let trimmed_start = line.trim_start_matches([' ', '\t']);
        let leading_spaces = line.len() - trimmed_start.len();
        if let Some((fence_byte, fence_len)) = open_fence {
            out.push_str(line);
            out.push_str(cr);
            if leading_spaces <= 3 {
                let run = trimmed_start.bytes().take_while(|&b| b == fence_byte).count();
                if run >= fence_len && trimmed_start[run..].trim().is_empty() {
                    open_fence = None;
                }
            }
            continue;
        }
        if leading_spaces <= 3 {
            let first = trimmed_start.as_bytes().first().copied();
            if matches!(first, Some(b'`' | b'~')) {
                let fence_byte = first.unwrap();
                let run = trimmed_start.bytes().take_while(|&b| b == fence_byte).count();
                if run >= 3 && (fence_byte == b'~' || !trimmed_start[run..].contains('`')) {
                    open_fence = Some((fence_byte, run));
                    out.push_str(line);
                    out.push_str(cr);
                    continue;
                }
            }
        }
        if trimmed_start.is_empty() {
            out.push_str(line);
            out.push_str(cr);
            continue;
        }

        // Peel CommonMark blockquote prefixes (`0..=3` spaces + `>` + optional single space).
        let mut rest = line;
        let mut quote_prefix = String::new();
        let mut in_quote = false;
        loop {
            let spaces = rest.bytes().take_while(|&b| b == b' ').count();
            if spaces <= 3 && rest[spaces..].starts_with('>') {
                in_quote = true;
                let mut take = spaces + 1;
                if rest[take..].starts_with(' ') {
                    take += 1;
                }
                quote_prefix.push_str(&rest[..take]);
                rest = &rest[take..];
            } else {
                break;
            }
        }

        let content_trimmed = rest.trim_start_matches([' ', '\t']);
        let ws_len = rest.len() - content_trimmed.len();
        if ws_len == 0
            || content_trimmed.is_empty()
            || (ws_len <= 3 && is_list_or_heading_line(content_trimmed))
            || (in_quote && is_github_alert_marker(content_trimmed))
        {
            out.push_str(line);
            out.push_str(cr);
            continue;
        }

        // Expand leading spaces & tabs (4-column tab stops) to NBSP so CommonMark keeps them.
        let mut col = 0usize;
        for ch in rest[..ws_len].chars() {
            let step = if ch == '\t' { ((col / 4) + 1) * 4 - col } else { 1 };
            col += step;
        }
        out.push_str(&quote_prefix);
        for _ in 0..col {
            out.push('\u{00A0}');
        }
        out.push_str(content_trimmed);
        out.push_str(cr);
    }
    out
}

/// Prepares display Markdown for a user-authored message, disabling raw HTML and preserving
/// leading spaces/tabs typed at the start of paragraph lines (`pingdotgg/t3code#38079`).
pub fn prepare_display_markdown_for_user(source: &str, options: DisplayMarkdownOptions<'_>) -> String {
    let preserved = preserve_user_leading_whitespace(source);
    prepare_display_markdown_for_surface(&preserved, options, false, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_direction_handles_arabic_hebrew_and_mixed_english_openers() {
        assert_eq!(text_direction("تفاصيل مهمة حول التطبيق"), Some(TextDirection::Rtl));
        assert_eq!(text_direction("פרטים נוספים כאן"), Some(TextDirection::Rtl));
        // Opens with an English term, majority Arabic words -> RTL.
        assert_eq!(text_direction("API توثيق مفصل للنظام"), Some(TextDirection::Rtl));
        assert_eq!(text_direction("Plain English summary"), Some(TextDirection::Ltr));
        assert_eq!(text_direction("12345"), None);
    }

    #[test]
    fn preserve_user_leading_whitespace_keeps_indented_lines_without_breaking_fences_or_lists() {
        let input = "  indented line\n    four spaces\n- list item\n```\n  keep fence raw\n```\n>   quoted indent";
        let out = preserve_user_leading_whitespace(input);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "\u{00A0}\u{00A0}indented line");
        assert_eq!(lines[1], "\u{00A0}\u{00A0}\u{00A0}\u{00A0}four spaces");
        assert_eq!(lines[2], "- list item");
        assert_eq!(lines[4], "  keep fence raw");
        assert_eq!(lines[6], "> \u{00A0}\u{00A0}quoted indent");
    }
}
