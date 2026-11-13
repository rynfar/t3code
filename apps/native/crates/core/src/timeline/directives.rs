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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexArtifactTemplate {
    pub artifact_kind: String,
    pub display_name: String,
    pub skill_directory: String,
    pub skill_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexAssistantSegment {
    Markdown(String),
    ArtifactTemplate(CodexArtifactTemplate),
}

pub fn codex_artifact_template_presentation_label(artifact_kind: &str) -> Option<&'static str> {
    match artifact_kind {
        "document" => Some("Document template"),
        "presentation" => Some("Presentation template"),
        "spreadsheet" => Some("Spreadsheet template"),
        "site" => Some("Site template"),
        "google-docs" => Some("Google Doc template"),
        "google-slides" => Some("Google Slides template"),
        "google-sheets" => Some("Google Sheet template"),
        "image" => Some("Image template"),
        "email" => Some("Email template"),
        "slack" => Some("Slack template"),
        _ => None,
    }
}

/// Prompt text inserted when clicking "Use template" (`codexArtifactTemplateUsePrompt`).
pub fn codex_artifact_template_use_prompt(template: &CodexArtifactTemplate) -> String {
    let skill = format!("${}", template.skill_name);
    match template.artifact_kind.as_str() {
        "document" => format!("Create a document using this {skill} about\u{2026}"),
        "presentation" => format!("Create a presentation using the {skill} template about\u{2026}"),
        "spreadsheet" => format!("Create a spreadsheet using this {skill} about\u{2026}"),
        "site" => format!("Create a Site using this {skill} about\u{2026}"),
        "google-docs" => format!("Create a Google Doc using this {skill} about\u{2026}"),
        "google-slides" => format!("Create a Google Slides presentation using this {skill} about\u{2026}"),
        "google-sheets" => format!("Create a Google Sheet using this {skill} about\u{2026}"),
        "image" => format!("Create an image using this {skill} of\u{2026}"),
        "email" => format!("Draft an email using this {skill} about\u{2026}"),
        "slack" => format!("Draft a Slack message using this {skill} about\u{2026}"),
        _ => format!("Create an artifact using this {skill}"),
    }
}

/// Appends the template use prompt to `draft`, avoiding duplicate trailing inserts (`appendCodexArtifactTemplateUsePrompt`).
pub fn codex_artifact_template_prompt_to_append(draft: &str, template: &CodexArtifactTemplate) -> Option<String> {
    let prompt = codex_artifact_template_use_prompt(template);
    let trimmed = draft.trim_end();
    if let Some(start) = trimmed.len().checked_sub(prompt.len())
        && trimmed[start..] == prompt
        && (start == 0 || trimmed[..start].chars().next_back().is_some_and(char::is_whitespace))
    {
        return None;
    }
    Some(prompt)
}

fn try_parse_artifact(input: &str) -> Option<(CodexArtifactTemplate, usize)> {
    let (_label, attrs, consumed) = parse_directive(input, ARTIFACT_PREFIX)?;
    let artifact_kind = attrs.get("artifact_kind")?.trim().to_owned();
    codex_artifact_template_presentation_label(&artifact_kind)?;
    let display_name = attrs.get("display_name")?.trim().to_owned();
    let skill_directory = attrs.get("skill_directory")?.clone();
    let skill_name = attrs.get("skill_name")?.clone();
    if display_name.is_empty()
        || !(skill_directory.starts_with('/') && !skill_directory.starts_with("//"))
        || !skill_name.starts_with("artifact-template-")
    {
        return None;
    }
    Some((CodexArtifactTemplate { artifact_kind, display_name, skill_directory, skill_name }, consumed))
}

fn try_render_artifact(input: &str) -> Option<(String, usize)> {
    let (template, consumed) = try_parse_artifact(input)?;
    let kind_label = codex_artifact_template_presentation_label(&template.artifact_kind)?;
    Some((format!("{} ({kind_label})", template.display_name), consumed))
}

fn rewrite_inline_for_display(line: &str) -> String {
    if !line.contains(CITATION_PREFIX) && !line.contains(FOLLOW_UP_PREFIX) && !line.contains(ARTIFACT_PREFIX) {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = find_next_directive(rest) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        if let Some((replacement, consumed)) = try_render_artifact(after)
            .or_else(|| try_render_citation(after))
            .or_else(|| {
                let (label, consumed) = try_render_follow_up(after)?;
                Some((format!("**{}**", escape_md_label(&label)), consumed))
            })
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

/// Rewrite `:codex-file-citation` and `:codex-followup` directives into display markdown and split
/// standalone `::artifact-template` lines into structured segments (`ChatMarkdown.tsx`).
pub fn split_codex_assistant_segments(markdown: &str) -> Vec<CodexAssistantSegment> {
    if !markdown.contains(CITATION_PREFIX) && !markdown.contains(FOLLOW_UP_PREFIX) && !markdown.contains(ARTIFACT_PREFIX) {
        return vec![CodexAssistantSegment::Markdown(markdown.to_owned())];
    }
    let mut segments = Vec::new();
    let mut current_md = String::new();
    let mut fence: Option<(u8, usize)> = None;

    for raw_line in markdown.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let has_nl = raw_line.ends_with('\n');
        let trimmed = line.trim_start();
        if let Some((ch, min_len)) = fence {
            let count = trimmed.bytes().take_while(|b| *b == ch).count();
            if count >= min_len && trimmed[count..].trim().is_empty() {
                fence = None;
            }
            current_md.push_str(raw_line);
            continue;
        }
        let fence_byte = trimmed.as_bytes().first().copied().filter(|b| *b == b'`' || *b == b'~');
        if let Some(ch) = fence_byte {
            let count = trimmed.bytes().take_while(|b| *b == ch).count();
            if count >= 3 && (ch == b'~' || !trimmed[count..].contains('`')) {
                fence = Some((ch, count));
                current_md.push_str(raw_line);
                continue;
            }
        }
        let trimmed_all = line.trim();
        if let Some((template, consumed)) = try_parse_artifact(trimmed_all)
            && consumed == trimmed_all.len()
        {
            if !current_md.trim().is_empty() {
                segments.push(CodexAssistantSegment::Markdown(std::mem::take(&mut current_md).trim_end().to_owned()));
            } else {
                current_md.clear();
            }
            segments.push(CodexAssistantSegment::ArtifactTemplate(template));
            continue;
        }
        current_md.push_str(&rewrite_inline_for_display(line));
        if has_nl {
            current_md.push('\n');
        }
    }
    if !current_md.trim().is_empty() || segments.is_empty() {
        segments.push(CodexAssistantSegment::Markdown(current_md));
    }
    segments
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

    #[test]
    fn splits_assistant_segments_and_ignores_code_fences() {
        let md = "Intro :codex-file-citation[app.rs]{path=\"src/app.rs\" line_range_start=\"42\"}\n\n\
            ::artifact-template{artifact_kind=\"presentation\" display_name=\"Deck\" skill_directory=\"/skills/deck\" skill_name=\"artifact-template-deck\"}\n\n\
            ```md\n:codex-followup[Raw]{prompt=\"keep\"}\n```\n\n\
            Next :codex-followup[Ship it]{prompt=\"go\"}";
        let segs = super::split_codex_assistant_segments(md);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0], super::CodexAssistantSegment::Markdown("Intro [app.rs](<src/app.rs#L42>)".into()));
        assert!(matches!(&segs[1], super::CodexAssistantSegment::ArtifactTemplate(t) if t.display_name == "Deck" && t.artifact_kind == "presentation"));
        let super::CodexAssistantSegment::Markdown(tail) = &segs[2] else { panic!("expected markdown tail") };
        assert!(tail.contains(":codex-followup[Raw]{prompt=\"keep\"}"));
        assert!(tail.contains("Next **Ship it**"));
    }

    #[test]
    fn template_use_prompt_avoids_duplicate_trailing_append() {
        let template = super::CodexArtifactTemplate {
            artifact_kind: "document".into(),
            display_name: "RFC".into(),
            skill_directory: "/skills/doc".into(),
            skill_name: "artifact-template-rfc".into(),
        };
        let prompt = super::codex_artifact_template_use_prompt(&template);
        assert_eq!(prompt, "Create a document using this $artifact-template-rfc about\u{2026}");
        assert_eq!(super::codex_artifact_template_prompt_to_append("Write about otters", &template).as_deref(), Some(prompt.as_str()));
        assert_eq!(super::codex_artifact_template_prompt_to_append(&format!("{prompt}\n"), &template), None);
        assert_eq!(
            super::codex_artifact_template_prompt_to_append(&format!("{prompt}\nExtra notes"), &template).as_deref(),
            Some(prompt.as_str())
        );
    }
}
