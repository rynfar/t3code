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

/// Rewrites `<picture>`, `<img>`, `<video>`, media-bearing `<a>`/`<h1..h6>`/`<table>`, and `<details>`
/// containers while preserving fenced code blocks, inline code spans, and linking inline file paths.
pub fn preprocess_markdown_for_display(source: &str, is_dark: bool, cwd: Option<&str>) -> String {
    preprocess_markdown_for_display_with_html(source, is_dark, cwd, true)
}

pub fn preprocess_markdown_for_display_with_html(
    source: &str,
    is_dark: bool,
    cwd: Option<&str>,
    parse_raw_html: bool,
) -> String {
    let html_rewritten = if parse_raw_html {
        preprocess_html_containers(source, is_dark)
    } else {
        source.to_owned()
    };
    link_inline_code_file_paths(&html_rewritten, cwd)
}

fn preprocess_html_containers(source: &str, is_dark: bool) -> String {
    if !source.contains('<') {
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

        if let Some(next_tag) = find_next_html_container_outside_code(rest, line_start) {
            out.push_str(&rest[..next_tag.offset]);
            let tag_abs = pos + next_tag.offset;
            let rewritten = match next_tag.kind {
                HtmlContainer::Picture => rewrite_picture_block(&source[tag_abs..], is_dark),
                HtmlContainer::Details => rewrite_details_block(&source[tag_abs..], is_dark),
                HtmlContainer::Img => rewrite_img_tag(&source[tag_abs..]),
                HtmlContainer::Video => rewrite_video_block(&source[tag_abs..]),
                HtmlContainer::Anchor => rewrite_media_anchor(&source[tag_abs..], is_dark),
                HtmlContainer::Heading(level) => rewrite_media_heading(&source[tag_abs..], level, is_dark),
                HtmlContainer::Table => rewrite_media_table(&source[tag_abs..], is_dark),
            };
            if let Some((replacement, consumed)) = rewritten {
                out.push_str(&replacement);
                pos = tag_abs + consumed;
                continue;
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
    Img,
    Video,
    Anchor,
    Heading(u8),
    Table,
}

struct NextHtmlTag {
    offset: usize,
    kind: HtmlContainer,
}

fn find_next_html_container_outside_code(input: &str, at_line_start: bool) -> Option<NextHtmlTag> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut fence: Option<(u8, usize)> = None;
    while i < bytes.len() {
        let line_start = if i == 0 { at_line_start } else { bytes[i - 1] == b'\n' };
        if let Some((fc, flen)) = fence {
            let line_end = input[i..].find('\n').map(|n| i + n + 1).unwrap_or(bytes.len());
            let trimmed = input[i..line_end].trim_start_matches(' ');
            if input[i..line_end].len() - trimmed.len() <= 3 {
                let close_len = trimmed.bytes().take_while(|&b| b == fc).count();
                if close_len >= flen && trimmed[close_len..].trim().is_empty() {
                    fence = None;
                }
            }
            i = line_end;
            continue;
        }
        if line_start {
            let trimmed = input[i..].trim_start_matches(' ');
            if input[i..].len() - trimmed.len() <= 3
                && let Some(fc @ (b'`' | b'~')) = trimmed.as_bytes().first().copied()
            {
                let count = trimmed.bytes().take_while(|&b| b == fc).count();
                if count >= 3 {
                    let line_end = input[i..].find('\n').map(|n| i + n + 1).unwrap_or(bytes.len());
                    let info = trimmed[count..].split('\n').next().unwrap_or("");
                    if fc != b'`' || !info.contains('`') {
                        fence = Some((fc, count));
                        i = line_end;
                        continue;
                    }
                }
            }
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == b'`' {
            let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if let Some(close) = find_closing_backtick_run(&input[i + run..], run) {
                i += run + close + run;
                continue;
            }
            i += run;
            continue;
        }
        if bytes[i] == b'<'
            && let Some(kind) = classify_open_tag(&input[i..])
        {
            return Some(NextHtmlTag { offset: i, kind });
        }
        i += input[i..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn find_closing_backtick_run(rest: &str, run: usize) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut j = 0;
    while j < bytes.len() {
        if bytes[j] == b'\n' && bytes.get(j + 1) == Some(&b'\n') {
            return None;
        }
        if bytes[j] == b'`' {
            let len = bytes[j..].iter().take_while(|&&b| b == b'`').count();
            if len == run {
                return Some(j);
            }
            j += len;
            continue;
        }
        j += 1;
    }
    None
}

fn classify_open_tag(slice: &str) -> Option<HtmlContainer> {
    let after = slice.strip_prefix('<')?;
    let name_len = after.bytes().take_while(|b| b.is_ascii_alphanumeric()).count();
    if name_len == 0 {
        return None;
    }
    let name = &after[..name_len];
    let rest = &after[name_len..];
    if !matches_tag_boundary(rest) {
        return None;
    }
    if name.eq_ignore_ascii_case("picture") {
        Some(HtmlContainer::Picture)
    } else if name.eq_ignore_ascii_case("details") {
        Some(HtmlContainer::Details)
    } else if name.eq_ignore_ascii_case("img") {
        Some(HtmlContainer::Img)
    } else if name.eq_ignore_ascii_case("video") {
        Some(HtmlContainer::Video)
    } else if name.eq_ignore_ascii_case("a") {
        Some(HtmlContainer::Anchor)
    } else if name.eq_ignore_ascii_case("table") {
        Some(HtmlContainer::Table)
    } else if name.len() == 2 && matches!(name.as_bytes()[0], b'h' | b'H') && matches!(name.as_bytes()[1], b'1'..=b'6') {
        Some(HtmlContainer::Heading(name.as_bytes()[1] - b'0'))
    } else {
        None
    }
}

fn matches_tag_boundary(after_name: &str) -> bool {
    after_name.starts_with('>') || after_name.starts_with("/>") || after_name.starts_with(char::is_whitespace)
}

fn find_tag_end(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            None => {}
        }
        i += 1;
    }
    None
}

fn contains_media_hint(input: &str) -> bool {
    if input.contains("![") {
        return true;
    }
    if !input.contains('<') {
        return false;
    }
    let lower = input.to_ascii_lowercase();
    ["<img", "<picture", "<video"].iter().any(|needle| {
        let mut start = 0;
        while let Some(pos) = lower[start..].find(needle) {
            let abs = start + pos;
            if matches_tag_boundary(&lower[abs + needle.len()..]) {
                return true;
            }
            start = abs + needle.len();
        }
        false
    })
}

fn decode_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn parse_html_dimension(value: Option<String>) -> Option<u32> {
    let raw = value?;
    let trimmed = raw.trim();
    let digits = trimmed.strip_suffix("px").or_else(|| trimmed.strip_suffix("PX")).unwrap_or(trimmed).trim();
    digits.parse::<u32>().ok().filter(|&n| n > 0 && n <= 16_384)
}

fn format_markdown_destination(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().any(char::is_whitespace) || trimmed.contains(['(', ')', '<', '>']) {
        format!("<{}>", trimmed.replace('<', "%3C").replace('>', "%3E"))
    } else {
        trimmed.to_owned()
    }
}

fn format_markdown_media(alt: &str, src: &str, title: Option<&str>, hints: crate::media::MediaHints) -> Option<String> {
    let src = src.trim();
    if src.is_empty() {
        return None;
    }
    let escaped_alt = alt.replace(['\r', '\n'], " ").replace(']', "\\]");
    let encoded_title = crate::media::encode_media_title(hints, title);
    let dest = format_markdown_destination(src);
    let title_part = encoded_title
        .map(|t| format!(" \"{}\"", t.replace('\\', "\\\\").replace('"', "\\\"")))
        .unwrap_or_default();
    Some(format!("![{escaped_alt}]({dest}{title_part})"))
}

fn rewrite_img_tag(input: &str) -> Option<(String, usize)> {
    let end = find_tag_end(input)?;
    let tag = &input[..=end];
    let src = extract_html_attr(tag, "src")?;
    let alt = extract_html_attr(tag, "alt").unwrap_or_default();
    let title = extract_html_attr(tag, "title");
    let hints = crate::media::MediaHints {
        width: parse_html_dimension(extract_html_attr(tag, "width")),
        height: parse_html_dimension(extract_html_attr(tag, "height")),
        explicit_video: false,
    };
    let md = format_markdown_media(&alt, &src, title.as_deref(), hints)?;
    Some((md, end + 1))
}

fn rewrite_video_block(input: &str) -> Option<(String, usize)> {
    let open_end = find_tag_end(input)?;
    let open_tag = &input[..=open_end];
    let self_closing = open_tag.trim_end().ends_with("/>");
    let (block, consumed) = if self_closing {
        (open_tag, open_end + 1)
    } else {
        let lower = input.to_ascii_lowercase();
        if let Some(close) = lower[open_end + 1..].find("</video>") {
            let end = open_end + 1 + close + "</video>".len();
            (&input[..end], end)
        } else {
            (open_tag, open_end + 1)
        }
    };

    let mut src = extract_html_attr(open_tag, "src");
    if src.is_none() {
        for tag in iter_html_tags(block) {
            if tag.len() >= 7 && tag[..7].eq_ignore_ascii_case("<source") && matches_tag_boundary(&tag[7..])
                && let Some(candidate) = extract_html_attr(tag, "src")
                && !candidate.trim().is_empty()
            {
                src = Some(candidate);
                break;
            }
        }
    }
    let src = src?;
    let title = extract_html_attr(open_tag, "title");
    let alt = extract_html_attr(open_tag, "aria-label")
        .or_else(|| title.clone())
        .or_else(|| src.rsplit('/').next().map(|s| s.split(['?', '#']).next().unwrap_or(s).to_owned()))
        .unwrap_or_else(|| "video".to_owned());
    let hints = crate::media::MediaHints {
        width: parse_html_dimension(extract_html_attr(open_tag, "width")),
        height: parse_html_dimension(extract_html_attr(open_tag, "height")),
        explicit_video: true,
    };
    let md = format_markdown_media(&alt, &src, title.as_deref(), hints)?;
    Some((md, consumed))
}

fn rewrite_media_anchor(input: &str, is_dark: bool) -> Option<(String, usize)> {
    let open_end = find_tag_end(input)?;
    let open_tag = &input[..=open_end];
    let href = extract_html_attr(open_tag, "href")?;
    if href.trim().is_empty() {
        return None;
    }
    let lower = input.to_ascii_lowercase();
    let close_rel = lower[open_end + 1..].find("</a>")?;
    let inner = &input[open_end + 1..open_end + 1 + close_rel];
    if !contains_media_hint(inner) || inner.to_ascii_lowercase().contains("<a") {
        return None;
    }
    let rewritten = preprocess_html_containers(inner, is_dark).replace(['\r', '\n'], " ");
    let label = rewritten.trim();
    if label.is_empty() {
        return None;
    }
    let dest = format_markdown_destination(&href);
    Some((format!("[{label}]({dest})"), open_end + 1 + close_rel + "</a>".len()))
}

fn rewrite_media_heading(input: &str, level: u8, is_dark: bool) -> Option<(String, usize)> {
    let open_end = find_tag_end(input)?;
    let lower = input.to_ascii_lowercase();
    let close_tag = format!("</h{level}>");
    let close_rel = lower[open_end + 1..].find(&close_tag)?;
    let inner = &input[open_end + 1..open_end + 1 + close_rel];
    if !contains_media_hint(inner) {
        return None;
    }
    let rewritten = preprocess_html_containers(inner, is_dark);
    let stripped = strip_simple_html_tags(&rewritten).replace(['\r', '\n'], " ");
    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    let hashes = "#".repeat(usize::from(level));
    Some((format!("\n{hashes} {collapsed}\n"), open_end + 1 + close_rel + close_tag.len()))
}

fn find_matching_closing_tag(input: &str, tag_name: &str, start_after_open: usize) -> Option<usize> {
    let lower = input.to_ascii_lowercase();
    let open_needle = format!("<{tag_name}");
    let close_needle = format!("</{tag_name}>");
    let close_pos = lower[start_after_open..].find(&close_needle)?;
    if lower[start_after_open..start_after_open + close_pos].contains(&open_needle) {
        // Skip nested tables so inner tags are never mispaired.
        return None;
    }
    Some(start_after_open + close_pos)
}

fn rewrite_media_table(input: &str, is_dark: bool) -> Option<(String, usize)> {
    let open_end = find_tag_end(input)?;
    let close_start = find_matching_closing_tag(input, "table", open_end + 1)?;
    let body = &input[open_end + 1..close_start];
    if !contains_media_hint(body) {
        return None;
    }
    let consumed = close_start + "</table>".len();
    let body_lower = body.to_ascii_lowercase();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut aligns: Vec<Option<&'static str>> = Vec::new();
    let mut cursor = 0usize;

    while let Some(tr_rel) = body_lower[cursor..].find("<tr") {
        let tr_start = cursor + tr_rel;
        if !matches_tag_boundary(&body_lower[tr_start + 3..]) {
            cursor = tr_start + 3;
            continue;
        }
        let Some(tr_open_end) = find_tag_end(&body[tr_start..]).map(|e| tr_start + e) else { break };
        let tr_end = body_lower[tr_open_end + 1..]
            .find("</tr>")
            .map(|c| tr_open_end + 1 + c)
            .unwrap_or(body.len());
        let tr_body = &body[tr_open_end + 1..tr_end];
        let tr_lower = tr_body.to_ascii_lowercase();

        let mut cells = Vec::new();
        let mut col = 0usize;
        let mut cell_cur = 0usize;
        while let Some((cell_off, is_th)) = find_next_table_cell(&tr_lower[cell_cur..]) {
            let cell_abs = cell_cur + cell_off;
            let Some(cell_open_end) = find_tag_end(&tr_body[cell_abs..]).map(|e| cell_abs + e) else { break };
            let tag = &tr_body[cell_abs..=cell_open_end];
            let close_tag = if is_th { "</th>" } else { "</td>" };
            let cell_close = tr_lower[cell_open_end + 1..]
                .find(close_tag)
                .map(|c| cell_open_end + 1 + c)
                .unwrap_or(tr_body.len());
            let raw_cell = &tr_body[cell_open_end + 1..cell_close];
            let colspan = extract_html_attr(tag, "colspan")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, 32);
            let align = extract_html_attr(tag, "align").and_then(|a| match a.trim().to_ascii_lowercase().as_str() {
                "left" => Some(":---"),
                "center" => Some(":---:"),
                "right" => Some("---:"),
                _ => None,
            });
            if aligns.len() <= col {
                aligns.resize(col + 1, None);
            }
            if aligns[col].is_none() {
                aligns[col] = align;
            }
            let normalized = normalize_html_table_cell(raw_cell, is_dark);
            cells.push(normalized);
            for _ in 1..colspan {
                cells.push(String::new());
            }
            col += colspan;
            cell_cur = (cell_close + close_tag.len()).min(tr_body.len());
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
        cursor = (tr_end + "</tr>".len()).min(body.len());
    }

    let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.is_empty() || col_count == 0 {
        return None;
    }
    let mut md = String::from("\n");
    let write_row = |out: &mut String, row: &[String]| {
        out.push('|');
        for idx in 0..col_count {
            out.push(' ');
            out.push_str(row.get(idx).map(String::as_str).unwrap_or(""));
            out.push_str(" |");
        }
        out.push('\n');
    };
    write_row(&mut md, &rows[0]);
    md.push('|');
    for idx in 0..col_count {
        md.push(' ');
        md.push_str(aligns.get(idx).copied().flatten().unwrap_or("---"));
        md.push_str(" |");
    }
    md.push('\n');
    for row in rows.iter().skip(1) {
        write_row(&mut md, row);
    }
    Some((md, consumed))
}

fn find_next_table_cell(lower: &str) -> Option<(usize, bool)> {
    let mut i = 0;
    while let Some(pos) = lower[i..].find('<') {
        let abs = i + pos;
        let rest = &lower[abs..];
        if rest.starts_with("<th") && matches_tag_boundary(&rest[3..]) {
            return Some((abs, true));
        }
        if rest.starts_with("<td") && matches_tag_boundary(&rest[3..]) {
            return Some((abs, false));
        }
        i = abs + 1;
    }
    None
}

fn normalize_html_table_cell(raw_cell: &str, is_dark: bool) -> String {
    let with_media = preprocess_html_containers(raw_cell, is_dark);
    let mut out = String::with_capacity(with_media.len());
    let mut i = 0;
    while i < with_media.len() {
        let rest = &with_media[i..];
        if rest.starts_with("](<")
            && let Some(close) = rest[3..].find('>')
        {
            let span = 3 + close + 1;
            out.push_str(&rest[..span]);
            i += span;
            continue;
        }
        if rest.starts_with('<')
            && let Some(len) = html_tag_len(rest)
        {
            let tag = &rest[..len];
            let lower = tag.to_ascii_lowercase();
            if lower.starts_with("<br") {
                out.push(' ');
            } else if lower.starts_with("<code") || lower.starts_with("</code") {
                out.push('`');
            }
            i += len;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        match ch {
            '|' => out.push_str("\\|"),
            '\r' | '\n' => out.push(' '),
            _ => out.push(ch),
        }
        i += ch.len_utf8();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
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
    let mut title: Option<String> = None;
    let mut hints = crate::media::MediaHints::default();

    for tag in iter_html_tags(block) {
        if tag.len() >= 7 && tag[..7].eq_ignore_ascii_case("<source") && matches_tag_boundary(&tag[7..]) {
            let media = extract_html_attr(tag, "media").unwrap_or_default().to_ascii_lowercase();
            let srcset = extract_html_attr(tag, "srcset").or_else(|| extract_html_attr(tag, "src"));
            if let Some(src) = srcset.and_then(|s| s.split(',').next().map(|part| part.split_whitespace().next().unwrap_or("").to_owned()))
                && !src.is_empty()
                && media.contains("prefers-color-scheme")
                && media.contains(target_scheme)
            {
                chosen_src = Some(src);
            }
        } else if tag.len() >= 4 && tag[..4].eq_ignore_ascii_case("<img") && matches_tag_boundary(&tag[4..]) {
            if let Some(src) = extract_html_attr(tag, "src") {
                fallback_src = Some(src);
            }
            if let Some(a) = extract_html_attr(tag, "alt") {
                alt = a;
            }
            if title.is_none() {
                title = extract_html_attr(tag, "title");
            }
            hints.width = hints.width.or_else(|| parse_html_dimension(extract_html_attr(tag, "width")));
            hints.height = hints.height.or_else(|| parse_html_dimension(extract_html_attr(tag, "height")));
        }
    }

    let final_src = chosen_src.or(fallback_src)?;
    let md = format_markdown_media(&alt, &final_src, title.as_deref(), hints)?;
    Some((md, consumed))
}

fn rewrite_details_block(input: &str, is_dark: bool) -> Option<(String, usize)> {
    let open_tag_end = find_tag_end(input)?;
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
    let body = preprocess_html_containers(&body, is_dark);

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
        let Some(end) = find_tag_end(&input[abs..]) else { break };
        tags.push(&input[abs..=abs + end]);
        i = abs + end + 1;
    }
    tags
}

fn extract_html_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(idx) = lower[pos..].find(attr) {
        let abs = pos + idx;
        if abs > 0 && !lower.as_bytes()[abs - 1].is_ascii_whitespace() {
            pos = abs + attr.len();
            continue;
        }
        let after_name = tag[abs + attr.len()..].trim_start();
        let Some(after_eq) = after_name.strip_prefix('=') else {
            pos = abs + attr.len();
            continue;
        };
        let rest = after_eq.trim_start();
        let first = rest.chars().next()?;
        if first == '"' || first == '\'' {
            let inner = &rest[1..];
            let end = inner.find(first)?;
            return Some(decode_html_entities(&inner[..end]));
        }
        let raw = rest.split(|c: char| c.is_whitespace() || c == '>').next()?;
        return Some(decode_html_entities(raw.trim_end_matches('/')));
    }
    None
}

fn html_tag_len(slice: &str) -> Option<usize> {
    if let Some(after_comment) = slice.strip_prefix("<!--") {
        return after_comment.find("-->").map(|end| 4 + end + 3);
    }
    let after = slice.strip_prefix('<')?;
    let (closing, name_slice) = match after.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, after),
    };
    if !matches!(name_slice.as_bytes().first(), Some(b) if b.is_ascii_alphabetic()) {
        return None;
    }
    let name_len = name_slice.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'-').count();
    let boundary = &name_slice[name_len..];
    let valid_boundary = if closing {
        boundary.starts_with('>') || boundary.starts_with(char::is_whitespace)
    } else {
        matches_tag_boundary(boundary)
    };
    if !valid_boundary {
        return None;
    }
    find_tag_end(slice).map(|end| end + 1)
}

fn strip_simple_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        if rest.starts_with("](<")
            && let Some(close) = rest[3..].find('>')
        {
            let span = 3 + close + 1;
            out.push_str(&rest[..span]);
            i += span;
            continue;
        }
        if rest.starts_with('<')
            && let Some(len) = html_tag_len(rest)
        {
            i += len;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
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

    #[test]
    fn rewrites_html_media_headings_tables_and_videos_preserving_code_and_dimensions() {
        let md = concat!(
            "<h1><img src=\"logo.png\" alt=\"Logo\" width=\"24\" height=\"24\"> Title</h1>\n\n",
            "<a href=\"https://example.com\"><img src=\"thumb.png\" alt=\"Thumb\" width=\"200\"></a>\n\n",
            "<video controls width=\"480\" height=\"270\"><source src=\"https://example.com/demo\" type=\"video/mp4\"></video>\n\n",
            "<table><tr><th>Preview</th><th>Status</th></tr><tr><td align=\"center\"><img src=\"cell.png\" alt=\"Cell\"></td><td>OK | Done</td></tr></table>\n\n",
            "Inline `<img src=\"ignored.png\">` stays literal.\n",
        );
        let out = preprocess_markdown_for_display(md, true, None);
        assert!(out.contains("# ![Logo](logo.png "), "heading: {out}");
        assert!(out.contains("[![Thumb](thumb.png "), "anchor: {out}");
        assert!(out.contains("](https://example.com)"), "anchor dest: {out}");
        assert!(out.contains("![demo](https://example.com/demo "), "video: {out}");
        assert!(out.contains("| Preview | Status |"), "table header: {out}");
        assert!(out.contains("| ![Cell](cell.png) | OK \\| Done |"), "table row: {out}");
        assert!(out.contains("`<img src=\"ignored.png\">`"), "inline code: {out}");

        let doc = crate::media::MarkdownMediaDocument::new(out.into()).prepare();
        let gallery = doc.gallery_images();
        assert_eq!(gallery.len(), 3, "logo, video, table cell (linked thumb excluded): {gallery:?}");
        assert_eq!(gallery[0].2, crate::media::MediaHints { width: Some(24), height: Some(24), explicit_video: false });
        assert_eq!(gallery[1].2, crate::media::MediaHints { width: Some(480), height: Some(270), explicit_video: true });

        let raw_disabled = preprocess_markdown_for_display_with_html(md, true, None, false);
        assert!(raw_disabled.contains("<img src=\"logo.png\""));
        assert!(raw_disabled.contains("<video controls"));
    }

    #[test]
    fn preserves_bracketed_media_urls_comparisons_and_multiline_code_fences() {
        let md = concat!(
            "<h1><img src=\"my folder/logo (1).png\" alt=\"Logo\"> <span>Heading</span></h1>\n\n",
            "<table><tr><th>Preview</th><th>Expr</th></tr>",
            "<tr><td><a href=\"https://example.com/a<b (1)\"><img src=\"https://example.com/my shot (2).png\" alt=\"Shot\"></a></td>",
            "<td>a < b > c and <https://example.com></td></tr></table>\n\n",
            "Inline `code` then fence:\n",
            "```html\n<img src=\"first.png\">\n\n<video src=\"second.mp4\"></video>\n```\n",
        );
        let out = preprocess_markdown_for_display(md, true, None);
        assert!(out.contains("# ![Logo](<my folder/logo (1).png>) Heading"), "heading dest preserved: {out}");
        assert!(
            out.contains("[![Shot](<https://example.com/my shot (2).png>)](<https://example.com/a%3Cb (1)>)"),
            "anchor and table media dest preserved: {out}"
        );
        assert!(out.contains("a < b > c and <https://example.com>"), "literal comparisons preserved: {out}");
        assert!(out.contains("<img src=\"first.png\">\n\n<video src=\"second.mp4\"></video>"), "blank line fence untouched: {out}");
    }
}
