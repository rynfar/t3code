//! Codex inline markdown directive rendering for clipboard copy.
//! Ports `renderCodexDirectivesForCopy` from `packages/shared/src/codexMarkdownDirectives.ts`.

use std::collections::HashMap;

const CITATION_PREFIX: &str = ":codex-file-citation";
const FOLLOW_UP_PREFIX: &str = ":codex-followup";
const ARTIFACT_PREFIX: &str = "::artifact-template";

/// Rewrite Codex inline/leaf directives into clean markdown for copying (`renderCodexDirectivesForCopy`).
pub fn render_codex_directives_for_copy(markdown: &str) -> String {
    if !markdown.contains(CITATION_PREFIX) && !markdown.contains(FOLLOW_UP_PREFIX) && !markdown.contains(ARTIFACT_PREFIX) {
        return markdown.to_owned();
    }
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(pos) = find_next_directive(rest) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        if let Some((replacement, consumed)) = try_render_artifact(after)
            .or_else(|| try_render_citation(after))
            .or_else(|| try_render_follow_up(after))
        {
            out.push_str(&replacement);
            rest = &after[consumed..];
        } else {
            out.push(':');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    out
}

fn find_next_directive(s: &str) -> Option<usize> {
    s.find(':')
}

fn directive_boundary(next: Option<u8>) -> bool {
    match next {
        None => true,
        Some(b) => b.is_ascii_whitespace() || b == b'[' || b == b'{' || (b.is_ascii_punctuation() && b != b'-' && b != b'_'),
    }
}

fn parse_bracketed(s: &str) -> Option<(&str, usize)> {
    if !s.starts_with('[') {
        return None;
    }
    let mut depth = 0usize;
    for (idx, ch) in s.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[1..idx], idx + 1));
                }
            }
            '\n' => return None,
            _ => {}
        }
    }
    None
}

fn parse_attrs(s: &str) -> Option<(HashMap<String, String>, usize)> {
    if !s.starts_with('{') {
        return None;
    }
    let end = s.find('}')?;
    let inner = &s[1..end];
    let mut attrs = HashMap::new();
    let mut bytes = inner.trim();
    while !bytes.is_empty() {
        let eq = bytes.find('=')?;
        let key = bytes[..eq].trim().to_owned();
        bytes = bytes[eq + 1..].trim_start();
        let value = if let Some(after_q) = bytes.strip_prefix('"') {
            let mut val = String::new();
            let mut escaped = false;
            let mut consumed = 0;
            for (idx, ch) in after_q.char_indices() {
                if escaped {
                    val.push(ch);
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    consumed = idx + 1;
                    break;
                } else {
                    val.push(ch);
                }
            }
            if consumed == 0 {
                return None;
            }
            bytes = after_q[consumed..].trim_start();
            val
        } else {
            let stop = bytes.find(char::is_whitespace).unwrap_or(bytes.len());
            let val = bytes[..stop].to_owned();
            bytes = bytes[stop..].trim_start();
            val
        };
        attrs.insert(key, value);
    }
    Some((attrs, end + 1))
}

fn parse_directive<'a>(input: &'a str, name: &str) -> Option<(Option<&'a str>, HashMap<String, String>, usize)> {
    let after_name = input.strip_prefix(name)?;
    if !directive_boundary(after_name.as_bytes().first().copied()) {
        return None;
    }
    let mut idx = name.len();
    let label = if let Some((lbl, len)) = parse_bracketed(&input[idx..]) {
        idx += len;
        Some(lbl)
    } else {
        None
    };
    let attrs = if let Some((map, len)) = parse_attrs(&input[idx..]) {
        idx += len;
        map
    } else {
        HashMap::new()
    };
    Some((label, attrs, idx))
}

fn try_render_citation(input: &str) -> Option<(String, usize)> {
    let (_label, attrs, consumed) = parse_directive(input, CITATION_PREFIX)?;
    let path = attrs.get("path")?.trim();
    if path.is_empty() {
        return None;
    }
    let line_start = attrs.get("line_range_start").and_then(|v| v.trim().parse::<u64>().ok()).filter(|&n| n > 0);
    let dest_path = path.replace('%', "%25").replace('#', "%23").replace('?', "%3F");
    let href = match line_start {
        Some(line) => format!("{dest_path}#L{line}"),
        None => dest_path,
    };
    let normalized = path.replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    let label = trimmed.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(if trimmed.is_empty() { "File" } else { trimmed });
    let escaped_label = escape_md_label(label);
    let escaped_dest = href.replace('\\', "%5C").replace('<', "%3C").replace('>', "%3E").replace('\r', "%0D").replace('\n', "%0A");
    Some((format!("[{escaped_label}](<{escaped_dest}>)"), consumed))
}

fn try_render_follow_up(input: &str) -> Option<(String, usize)> {
    let (label, attrs, consumed) = parse_directive(input, FOLLOW_UP_PREFIX)?;
    let label = label?;
    if label.trim().is_empty() || attrs.get("prompt").map(String::as_str).unwrap_or("").trim().is_empty() {
        return None;
    }
    Some((label.to_owned(), consumed))
}

fn try_render_artifact(input: &str) -> Option<(String, usize)> {
    let (_label, attrs, consumed) = parse_directive(input, ARTIFACT_PREFIX)?;
    let kind_label = match attrs.get("artifact_kind").map(String::as_str)? {
        "document" => "Document template",
        "presentation" => "Presentation template",
        "spreadsheet" => "Spreadsheet template",
        "site" => "Site template",
        "google-docs" => "Google Doc template",
        "google-slides" => "Google Slides template",
        "google-sheets" => "Google Sheet template",
        "image" => "Image template",
        "email" => "Email template",
        "slack" => "Slack template",
        _ => return None,
    };
    let display = attrs.get("display_name")?.trim();
    let skill_dir = attrs.get("skill_directory")?;
    let skill_name = attrs.get("skill_name")?;
    if display.is_empty() || !(skill_dir.starts_with('/') && !skill_dir.starts_with("//")) || !skill_name.starts_with("artifact-template-") {
        return None;
    }
    Some((format!("{display} ({kind_label})"), consumed))
}

fn escape_md_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '[' | ']' | '*' | '_' | '`' | '<' | '&') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::render_codex_directives_for_copy;

    #[test]
    fn renders_file_citation_and_follow_up_for_copy() {
        let md = "See :codex-file-citation[src/app.rs]{path=\"src/app.rs\" line_range_start=\"42\"} or :codex-followup[Run tests]{prompt=\"cargo test\"}.";
        assert_eq!(render_codex_directives_for_copy(md), "See [app.rs](<src/app.rs#L42>) or Run tests.");
    }

    #[test]
    fn renders_artifact_template_for_copy() {
        let md = "::artifact-template{artifact_kind=\"document\" display_name=\"RFC\" skill_directory=\"/skills/doc\" skill_name=\"artifact-template-rfc\"}";
        assert_eq!(render_codex_directives_for_copy(md), "RFC (Document template)");
    }
}
