//! GitHub Alerts (`[!NOTE]`..`[!CAUTION]`), `<details>`/`<summary>`, `<picture>` theme
//! selection, and clickable inline-code file path linking (`markdownGithubAlerts.ts`,
//! `MarkdownDetails.tsx`, `MarkdownFileLink.logic.ts`).

use markdown::mdast;

use super::links;

/// GitHub Alert variants (`packages/shared/src/markdownGithubAlerts.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
        }
    }

    pub fn from_marker(marker: &str) -> Option<Self> {
        match marker.trim().to_ascii_uppercase().as_str() {
            "NOTE" => Some(Self::Note),
            "TIP" => Some(Self::Tip),
            "IMPORTANT" => Some(Self::Important),
            "WARNING" => Some(Self::Warning),
            "CAUTION" => Some(Self::Caution),
            _ => None,
        }
    }
}

/// Parsed GitHub Alert content extracted from a blockquote AST node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubAlert {
    pub kind: AlertKind,
    pub title: &'static str,
    pub body_markdown: String,
}

/// Parsed `<details>` disclosure block encoded via preprocessor fence or raw HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailsBlock {
    pub open: bool,
    pub summary: String,
    pub body_markdown: String,
}

/// Attempts to interpret a `Blockquote` node as a GitHub Alert (`> [!NOTE]\n> ...`).
pub fn parse_github_alert(blockquote: &mdast::Blockquote, raw_source: &str) -> Option<GithubAlert> {
    let raw_slice = blockquote
        .position
        .as_ref()
        .and_then(|pos| raw_source.get(pos.start.offset..pos.end.offset));
    if let Some(raw) = raw_slice
        && let Some(alert) = parse_github_alert_markdown(raw)
    {
        return Some(alert);
    }

    // Fallback when `position` offsets were altered (e.g. inside preprocessed blocks): inspect AST text.
    let first_para = match blockquote.children.first()? {
        mdast::Node::Paragraph(p) => p,
        _ => return None,
    };
    let first_text = match first_para.children.first()? {
        mdast::Node::Text(t) => t.value.as_str(),
        _ => return None,
    };
    let (kind, rest_first_line) = parse_alert_marker_line(first_text)?;
    let mut body = String::new();
    if !rest_first_line.is_empty() {
        body.push_str(rest_first_line);
        body.push('\n');
    }
    for node in blockquote.children.iter().skip(1) {
        let piece = collect_plain_markdownish(node);
        if !piece.is_empty() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&piece);
        }
    }
    if rest_first_line.is_empty() && first_para.children.len() > 1 {
        let mut head = String::new();
        for inline in first_para.children.iter().skip(1) {
            head.push_str(&collect_plain_markdownish(inline));
        }
        let head_trimmed = head.trim();
        if !head_trimmed.is_empty() {
            body = if body.is_empty() { head_trimmed.to_owned() } else { format!("{head_trimmed}\n\n{body}") };
        }
    }
    Some(GithubAlert { kind, title: kind.title(), body_markdown: body.trim().to_owned() })
}

/// Parses a raw blockquote markdown string starting with `> [!NOTE|TIP|IMPORTANT|WARNING|CAUTION]`.
pub fn parse_github_alert_markdown(raw_blockquote: &str) -> Option<GithubAlert> {
    let stripped_lines: Vec<String> = raw_blockquote
        .lines()
        .map(strip_blockquote_prefix)
        .collect();
    let first = stripped_lines.first()?.trim_start();
    let (kind, first_rest) = parse_alert_marker_line(first)?;

    let mut body_lines = Vec::new();
    if !first_rest.is_empty() {
        body_lines.push(first_rest.to_owned());
    }
    body_lines.extend(stripped_lines.into_iter().skip(1));
    let body = body_lines.join("\n").trim().to_owned();
    Some(GithubAlert { kind, title: kind.title(), body_markdown: body })
}

fn strip_blockquote_prefix(line: &str) -> String {
    let trimmed = line.trim_start();
    if let Some(after_gt) = trimmed.strip_prefix('>') {
        after_gt.strip_prefix(' ').unwrap_or(after_gt).to_owned()
    } else {
        line.to_owned()
    }
}

fn parse_alert_marker_line(text: &str) -> Option<(AlertKind, &str)> {
    let first_line = text.lines().next()?.trim_start();
    let rest = first_line.strip_prefix("[!")?;
    let end = rest.find(']')?;
    let kind = AlertKind::from_marker(&rest[..end])?;
    let after = &text[first_line.len() - rest.len() + end + 1..];
    let after = after.trim_start_matches([' ', '\t', '\n', '\r']);
    Some((kind, after))
}

fn collect_plain_markdownish(node: &mdast::Node) -> String {
    match node {
        mdast::Node::Text(t) => t.value.clone(),
        mdast::Node::InlineCode(c) => format!("`{}`", c.value),
        mdast::Node::Strong(s) => format!("**{}**", s.children.iter().map(collect_plain_markdownish).collect::<String>()),
        mdast::Node::Emphasis(e) => format!("*{}*", e.children.iter().map(collect_plain_markdownish).collect::<String>()),
        mdast::Node::Link(l) => {
            let label: String = l.children.iter().map(collect_plain_markdownish).collect();
            format!("[{label}]({})", l.url)
        }
        mdast::Node::Paragraph(p) => p.children.iter().map(collect_plain_markdownish).collect(),
        mdast::Node::Break(_) => "\n".to_owned(),
        _ => node.children().map(|c| c.iter().map(collect_plain_markdownish).collect::<Vec<_>>().join(" ")).unwrap_or_default(),
    }
}

/// Decodes a preprocessed `t3-details` custom fence emitted by `preprocess_html_containers`.
pub fn parse_details_fence(lang: Option<&str>, meta: Option<&str>, code: &str) -> Option<DetailsBlock> {
    if lang != Some("t3-details") {
        return None;
    }
    let meta = meta.unwrap_or("");
    let open = meta.contains("open=true");
    let summary = meta
        .split_once("summary=")
        .and_then(|(_, rest)| {
            let raw = rest.trim();
            serde_json::from_str::<String>(raw).ok()
        })
        .unwrap_or_else(|| "Details".to_owned());
    Some(DetailsBlock {
        open,
        summary,
        body_markdown: code.trim().to_owned(),
    })
}

/// Rewrites `<picture>...</picture>` for `is_dark`, rewrites `<details>/<summary>` containers into
/// `t3-details` custom fences (preserving nested fences), and links standalone inline-code file paths.
pub fn preprocess_markdown_for_display(source: &str, is_dark: bool, cwd: Option<&str>) -> String {
    let html_rewritten = preprocess_html_containers(source, is_dark);
    link_inline_code_file_paths(&html_rewritten, cwd)
}

fn preprocess_html_containers(source: &str, is_dark: bool) -> String {
    if !source.contains("<details") && !source.contains("<picture") {
        return source.to_owned();
    }

    let mut out = String::with_capacity(source.len());
    let mut pos = 0;
    let mut fence: Option<(char, usize)> = None;

    while pos < source.len() {
        let rest = &source[pos..];
        if let Some((fc, flen)) = fence {
            let line_end = rest.find('\n').map(|i| pos + i + 1).unwrap_or(source.len());
            let line = &source[pos..line_end];
            out.push_str(line);
            let trimmed = line.trim_start();
            let close_len = trimmed.bytes().take_while(|&b| b as char == fc).count();
            if close_len >= flen && trimmed[close_len..].trim().is_empty() {
                fence = None;
            }
            pos = line_end;
            continue;
        }

        let line_start = pos == 0 || source.as_bytes()[pos - 1] == b'\n';
        if line_start {
            let trimmed = rest.trim_start_matches(' ');
            let leading = rest.len() - trimmed.len();
            if leading <= 3
                && let Some(fc @ ('`' | '~')) = trimmed.chars().next()
            {
                let count = trimmed.bytes().take_while(|&b| b as char == fc).count();
                if count >= 3 {
                    fence = Some((fc, count));
                    let line_end = rest.find('\n').map(|i| pos + i + 1).unwrap_or(source.len());
                    out.push_str(&source[pos..line_end]);
                    pos = line_end;
                    continue;
                }
            }
        }

        if let Some(next_tag) = find_next_html_container(rest) {
            out.push_str(&rest[..next_tag.offset]);
            let tag_abs = pos + next_tag.offset;
            match next_tag.kind {
                HtmlContainer::Picture => {
                    if let Some((replacement, consumed)) = rewrite_picture_block(&source[tag_abs..], is_dark) {
                        out.push_str(&replacement);
                        pos = tag_abs + consumed;
                        continue;
                    }
                }
                HtmlContainer::Details => {
                    if let Some((replacement, consumed)) = rewrite_details_block(&source[tag_abs..]) {
                        out.push_str(&replacement);
                        pos = tag_abs + consumed;
                        continue;
                    }
                }
            }
            out.push('<');
            pos = tag_abs + 1;
        } else {
            out.push_str(rest);
            break;
        }
    }

    out
}

#[derive(Clone, Copy)]
enum HtmlContainer {
    Picture,
    Details,
}

struct NextHtmlTag {
    offset: usize,
    kind: HtmlContainer,
}

fn find_next_html_container(input: &str) -> Option<NextHtmlTag> {
    let lower = input.to_ascii_lowercase();
    let pic = lower.find("<picture").filter(|&i| matches_tag_boundary(&lower[i + 8..]));
    let det = lower.find("<details").filter(|&i| matches_tag_boundary(&lower[i + 8..]));
    match (pic, det) {
        (Some(p), Some(d)) => Some(if p < d {
            NextHtmlTag { offset: p, kind: HtmlContainer::Picture }
        } else {
            NextHtmlTag { offset: d, kind: HtmlContainer::Details }
        }),
        (Some(p), None) => Some(NextHtmlTag { offset: p, kind: HtmlContainer::Picture }),
        (None, Some(d)) => Some(NextHtmlTag { offset: d, kind: HtmlContainer::Details }),
        (None, None) => None,
    }
}

fn matches_tag_boundary(after_name: &str) -> bool {
    after_name.starts_with('>') || after_name.starts_with(char::is_whitespace)
}

fn rewrite_picture_block(input: &str, is_dark: bool) -> Option<(String, usize)> {
    let lower = input.to_ascii_lowercase();
    let close = lower.find("</picture>")?;
    let consumed = close + "</picture>".len();
    let block = &input[..consumed];

    let target_scheme = if is_dark { "dark" } else { "light" };
    let mut chosen_src: Option<String> = None;
    let mut fallback_src: Option<String> = None;
    let mut alt = String::new();

    for tag in iter_html_tags(block) {
        let tag_lower = tag.to_ascii_lowercase();
        if tag_lower.starts_with("<source") {
            let media = extract_html_attr(tag, "media").unwrap_or_default().to_ascii_lowercase();
            let srcset = extract_html_attr(tag, "srcset").or_else(|| extract_html_attr(tag, "src"));
            if let Some(src) = srcset.and_then(|s| s.split(',').next().map(|part| part.split_whitespace().next().unwrap_or("").to_owned()))
                && !src.is_empty()
                && media.contains("prefers-color-scheme")
                && media.contains(target_scheme)
            {
                chosen_src = Some(src);
            }
        } else if tag_lower.starts_with("<img") {
            if let Some(src) = extract_html_attr(tag, "src") {
                fallback_src = Some(src);
            }
            if let Some(a) = extract_html_attr(tag, "alt") {
                alt = a;
            }
        }
    }

    let final_src = chosen_src.or(fallback_src)?;
    let escaped_alt = alt.replace(']', "\\]");
    Some((format!("![{escaped_alt}]({final_src})"), consumed))
}

fn rewrite_details_block(input: &str) -> Option<(String, usize)> {
    let open_tag_end = input.find('>')?;
    let open_tag = &input[..=open_tag_end];
    let is_open = open_tag.to_ascii_lowercase().split_whitespace().any(|tok| {
        let tok = tok.trim_end_matches('>');
        tok == "open" || tok.starts_with("open=")
    });

    // Balance nested `<details ...>` / `</details>` tags.
    let lower = input.to_ascii_lowercase();
    let mut depth = 1usize;
    let mut scan = open_tag_end + 1;
    let close_start = loop {
        let next_open = lower[scan..].find("<details").filter(|&i| matches_tag_boundary(&lower[scan + i + 8..]));
        let next_close = lower[scan..].find("</details>");
        match (next_open, next_close) {
            (_, None) => return None,
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                scan += o + 8;
            }
            (_, Some(c)) => {
                depth -= 1;
                let abs = scan + c;
                if depth == 0 {
                    break abs;
                }
                scan = abs + "</details>".len();
            }
        }
    };

    let consumed = close_start + "</details>".len();
    let inner = &input[open_tag_end + 1..close_start];
    let inner_lower = inner.to_ascii_lowercase();
    let (summary, body) = if let Some(sum_start) = inner_lower.find("<summary")
        && let Some(sum_tag_end_rel) = inner[sum_start..].find('>')
        && let Some(sum_close_rel) = inner_lower[sum_start + sum_tag_end_rel + 1..].find("</summary>")
    {
        let content_start = sum_start + sum_tag_end_rel + 1;
        let content_end = content_start + sum_close_rel;
        let sum_text = strip_simple_html_tags(&inner[content_start..content_end]).trim().to_owned();
        let after_sum = &inner[content_end + "</summary>".len()..];
        let before_sum = &inner[..sum_start];
        let combined = format!("{before_sum}\n{after_sum}").trim().to_owned();
        (if sum_text.is_empty() { "Details".to_owned() } else { sum_text }, combined)
    } else {
        ("Details".to_owned(), inner.trim().to_owned())
    };

    let fence_ticks = "`".repeat(max_backtick_run(&body).max(3) + 1);
    let summary_json = serde_json::to_string(&summary).unwrap_or_else(|_| "\"Details\"".to_owned());
    let encoded = format!("\n{fence_ticks}t3-details open={is_open} summary={summary_json}\n{body}\n{fence_ticks}\n");
    Some((encoded, consumed))
}

fn max_backtick_run(text: &str) -> usize {
    let mut max_run = 0;
    let mut cur = 0;
    for b in text.bytes() {
        if b == b'`' {
            cur += 1;
            max_run = max_run.max(cur);
        } else {
            cur = 0;
        }
    }
    max_run
}

fn iter_html_tags(input: &str) -> Vec<&str> {
    let mut tags = Vec::new();
    let mut i = 0;
    while let Some(start) = input[i..].find('<') {
        let abs = i + start;
        let Some(end) = input[abs..].find('>') else { break };
        tags.push(&input[abs..=abs + end]);
        i = abs + end + 1;
    }
    tags
}

fn extract_html_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{attr}=");
    let mut pos = 0;
    while let Some(idx) = lower[pos..].find(&needle) {
        let abs = pos + idx;
        if abs > 0 && !lower.as_bytes()[abs - 1].is_ascii_whitespace() {
            pos = abs + needle.len();
            continue;
        }
        let rest = &tag[abs + needle.len()..];
        let first = rest.chars().next()?;
        if first == '"' || first == '\'' {
            let inner = &rest[1..];
            let end = inner.find(first)?;
            return Some(inner[..end].to_owned());
        }
        return Some(rest.split(|c: char| c.is_whitespace() || c == '>').next()?.to_owned());
    }
    None
}

fn strip_simple_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Wraps standalone backticked file paths like `` `src/main.rs:42` `` in clickable links while
/// skipping fenced code blocks and inline code spans already inside `[...](...)` link labels.
pub fn link_inline_code_file_paths(source: &str, cwd: Option<&str>) -> String {
    if !source.contains('`') {
        return source.to_owned();
    }
    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut fence: Option<(u8, usize)> = None;
    let mut bracket_depth = 0usize;

    while i < bytes.len() {
        let line_start = i == 0 || bytes[i - 1] == b'\n';
        if let Some((fc, flen)) = fence {
            let line_end = source[i..].find('\n').map(|n| i + n + 1).unwrap_or(bytes.len());
            let line = &source[i..line_end];
            out.push_str(line);
            let trimmed = line.trim_start();
            let close_len = trimmed.bytes().take_while(|&b| b == fc).count();
            if close_len >= flen && trimmed[close_len..].trim().is_empty() {
                fence = None;
            }
            i = line_end;
            continue;
        }

        if line_start {
            let trimmed = source[i..].trim_start_matches(' ');
            let leading = source[i..].len() - trimmed.len();
            if leading <= 3 {
                let first = trimmed.as_bytes().first().copied();
                if let Some(fc @ (b'`' | b'~')) = first {
                    let count = trimmed.bytes().take_while(|&b| b == fc).count();
                    if count >= 3 {
                        fence = Some((fc, count));
                        let line_end = source[i..].find('\n').map(|n| i + n + 1).unwrap_or(bytes.len());
                        out.push_str(&source[i..line_end]);
                        i = line_end;
                        continue;
                    }
                }
            }
        }

        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                let ch_len = source[i + 1..].chars().next().map(char::len_utf8).unwrap_or(1);
                out.push_str(&source[i..i + 1 + ch_len]);
                i += 1 + ch_len;
            }
            b'[' => {
                bracket_depth += 1;
                out.push('[');
                i += 1;
            }
            b']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                out.push(']');
                i += 1;
            }
            b'`' => {
                let ticks = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                let content_start = i + ticks;
                if let Some(close_rel) = find_closing_inline_ticks(&source[content_start..], ticks) {
                    let inner = &source[content_start..content_start + close_rel];
                    let total_end = content_start + close_rel + ticks;
                    let raw_span = &source[i..total_end];
                    if bracket_depth == 0
                        && !inner.contains('\n')
                        && let Some(resolved) = links::inline_code_file_path_candidate(inner, cwd)
                    {
                        let href = if resolved.raw_href.contains([' ', '(', ')']) {
                            format!("<{}>", resolved.raw_href)
                        } else {
                            resolved.raw_href
                        };
                        out.push('[');
                        out.push_str(raw_span);
                        out.push_str("](");
                        out.push_str(&href);
                        out.push(')');
                    } else {
                        out.push_str(raw_span);
                    }
                    i = total_end;
                } else {
                    out.push('`');
                    i += 1;
                }
            }
            _ => {
                let ch = source[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }

    out
}

fn find_closing_inline_ticks(rest: &str, ticks: usize) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let count = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if count == ticks {
                return Some(i);
            }
            i += count;
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_github_alert_kinds() {
        for (marker, expected) in [
            ("NOTE", AlertKind::Note),
            ("tip", AlertKind::Tip),
            ("Important", AlertKind::Important),
            ("WARNING", AlertKind::Warning),
            ("caution", AlertKind::Caution),
        ] {
            let raw = format!("> [!{marker}]\n> Useful detail with `code`.\n>\n> Second line.");
            let parsed = parse_github_alert_markdown(&raw).expect("alert");
            assert_eq!(parsed.kind, expected);
            assert_eq!(parsed.body_markdown, "Useful detail with `code`.\n\nSecond line.");
        }
        assert!(parse_github_alert_markdown("> Normal quote").is_none());
    }

    #[test]
    fn rewrites_details_and_picture_and_inline_file_links_without_touching_fences() {
        let md = "<picture>\n  <source media=\"(prefers-color-scheme: dark)\" srcset=\"dark.png\">\n  <img src=\"light.png\" alt=\"Logo\">\n</picture>\n\n<details open>\n<summary>Logs</summary>\n\n```rust\nlet p = \"src/main.rs:1\";\n```\n</details>\n\nCheck `apps/native/crates/core/src/lib.rs:42` and [`already.rs`](already.rs).";
        let out_dark = preprocess_markdown_for_display(md, true, None);
        assert!(out_dark.contains("![Logo](dark.png)"));
        assert!(out_dark.contains("t3-details open=true summary=\"Logs\""));
        assert!(out_dark.contains("[`apps/native/crates/core/src/lib.rs:42`](apps/native/crates/core/src/lib.rs:42)"));
        assert!(out_dark.contains("[`already.rs`](already.rs)"));
        assert!(!out_dark.contains("[`src/main.rs:1`]"));
    }
}
