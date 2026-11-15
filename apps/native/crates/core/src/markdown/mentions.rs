//! Parsing and rewriting for `@model` mentions (`model:<instanceId>/<slug>`) emitted by the
//! composer (`composerModelMentions.ts`, `pingdotgg/t3code#31207`).

/// One matched `model:<instanceId>/<model>` occurrence in prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMentionOccurrence<'a> {
    pub instance_id: &'a str,
    pub model: &'a str,
    pub source: &'a str,
    pub start: usize,
    pub end: usize,
}

pub const MODEL_MENTION_URL_SCHEME: &str = "t3-model://";

/// Finds `model:<instanceId>/<model>` references that `formatComposerModelMention` writes.
pub fn collect_composer_model_mentions(text: &str) -> Vec<ModelMentionOccurrence<'_>> {
    if !text.contains("model:") {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = text[i..].find("model:") {
        let start = i + rel;
        let prev_ok = start == 0
            || text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace() || matches!(c, ',' | '(' | '[' | '"' | '\''));
        if !prev_ok {
            i = start + "model:".len();
            continue;
        }
        let after_prefix = start + "model:".len();
        let mut inst_end = after_prefix;
        if bytes.get(inst_end).is_some_and(u8::is_ascii_alphabetic) {
            inst_end += 1;
            while inst_end < bytes.len() && (bytes[inst_end].is_ascii_alphanumeric() || matches!(bytes[inst_end], b'_' | b'-')) {
                inst_end += 1;
            }
        }
        if inst_end == after_prefix || bytes.get(inst_end) != Some(&b'/') {
            i = after_prefix;
            continue;
        }
        let slug_start = inst_end + 1;
        let mut slug_end = slug_start;
        for (off, ch) in text[slug_start..].char_indices() {
            if ch.is_whitespace() || matches!(ch, ',' | '"' | '\'') {
                break;
            }
            slug_end = slug_start + off + ch.len_utf8();
        }
        let trimmed = trim_model_slug(&text[slug_start..slug_end]);
        if trimmed.is_empty() {
            i = slug_start;
            continue;
        }
        let end = slug_start + trimmed.len();
        out.push(ModelMentionOccurrence {
            instance_id: &text[after_prefix..inst_end],
            model: trimmed,
            source: &text[start..end],
            start,
            end,
        });
        i = end;
    }
    out
}

fn trim_model_slug(slug: &str) -> &str {
    let mut end = slug.len();
    let mut excess_parens: isize = 0;
    let mut excess_brackets: isize = 0;
    let mut computed_parens = false;
    let mut computed_brackets = false;
    while end > 0 {
        let last = slug[..end].chars().next_back().unwrap();
        if matches!(last, '.' | ';' | ':' | '!' | '?') {
            end -= last.len_utf8();
            continue;
        }
        if last == ')' {
            if !computed_parens {
                let open = slug.bytes().filter(|&b| b == b'(').count() as isize;
                let close = slug.bytes().filter(|&b| b == b')').count() as isize;
                excess_parens = close - open;
                computed_parens = true;
            }
            if excess_parens <= 0 {
                break;
            }
            excess_parens -= 1;
            end -= 1;
            continue;
        }
        if last == ']' {
            if !computed_brackets {
                let open = slug.bytes().filter(|&b| b == b'[').count() as isize;
                let close = slug.bytes().filter(|&b| b == b']').count() as isize;
                excess_brackets = close - open;
                computed_brackets = true;
            }
            if excess_brackets <= 0 {
                break;
            }
            excess_brackets -= 1;
            end -= 1;
            continue;
        }
        break;
    }
    &slug[..end]
}

/// Rewrites unlinked `model:<instanceId>/<model>` tokens outside code fences, inline code, and
/// existing markdown links into `[<label>](t3-model://<instanceId>/<model>)` links when `resolve_label`
/// returns a display name.
pub fn link_model_mentions<F>(source: &str, mut resolve_label: F) -> String
where
    F: FnMut(&str, &str) -> Option<String>,
{
    if !source.contains("model:") {
        return source.to_owned();
    }

    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0usize;
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
            if leading <= 3
                && let Some(&fc @ (b'`' | b'~')) = trimmed.as_bytes().first()
            {
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

        if bytes[i] == b'`' {
            let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if let Some(close) = find_closing_ticks(source, i + run, run) {
                out.push_str(&source[i..close + run]);
                i = close + run;
                continue;
            }
            out.push_str(&source[i..i + run]);
            i += run;
            continue;
        }

        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let ch = source[i + 1..].chars().next().unwrap();
            let span = 1 + ch.len_utf8();
            out.push_str(&source[i..i + span]);
            i += span;
            continue;
        }

        if bytes[i] == b'[' {
            bracket_depth += 1;
            out.push('[');
            i += 1;
            continue;
        }
        if bytes[i] == b']' {
            bracket_depth = bracket_depth.saturating_sub(1);
            out.push(']');
            i += 1;
            if bytes.get(i) == Some(&b'(')
                && let Some(dest_end) = find_link_dest_end(source, i + 1)
            {
                out.push_str(&source[i..=dest_end]);
                i = dest_end + 1;
            }
            continue;
        }

        if bracket_depth == 0
            && source[i..].starts_with("model:")
            && (i == 0
                || source[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_whitespace() || matches!(c, ',' | '(' | '[' | '"' | '\'')))
            && let Some(occ) = collect_composer_model_mentions(&source[i..]).into_iter().find(|o| o.start == 0)
            && let Some(label) = resolve_label(occ.instance_id, occ.model)
        {
            let escaped_label = label.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]");
            out.push('[');
            out.push_str(&escaped_label);
            out.push_str("](");
            out.push_str(MODEL_MENTION_URL_SCHEME);
            out.push_str(&encode_url_part(occ.instance_id));
            out.push('/');
            out.push_str(&encode_url_part(occ.model));
            out.push(')');
            i += occ.end;
            continue;
        }

        let ch = source[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

pub fn parse_model_mention_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix(MODEL_MENTION_URL_SCHEME)?;
    let (instance_id, model) = rest.split_once('/')?;
    let instance_id = super::links::percent_decode(instance_id);
    let model = super::links::percent_decode(model);
    if instance_id.is_empty() || model.is_empty() {
        return None;
    }
    Some((instance_id, model))
}

fn encode_url_part(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn find_closing_ticks(source: &str, start: usize, run: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let count = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if count == run {
                return Some(i);
            }
            i += count;
        } else {
            i += 1;
        }
    }
    None
}

fn find_link_dest_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 1usize;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            b'\n' => return None,
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_and_links_model_mentions_with_balanced_brackets() {
        let src = "Compare model:codex/GPT-6.1-Sol[1m], (model:openCode/anthropic/claude-opus-4-6), and model:foo/bar(baz). Not `model:codex/ignored` or foo@model:codex/bad.";
        let found = collect_composer_model_mentions(src);
        assert_eq!(found.len(), 3);
        assert_eq!((found[0].instance_id, found[0].model), ("codex", "GPT-6.1-Sol[1m]"));
        assert_eq!((found[1].instance_id, found[1].model), ("openCode", "anthropic/claude-opus-4-6"));
        assert_eq!((found[2].instance_id, found[2].model), ("foo", "bar(baz)"));

        let linked = link_model_mentions(src, |inst, slug| match (inst, slug) {
            ("codex", "GPT-6.1-Sol[1m]") => Some("GPT-6.1 Sol".to_owned()),
            _ => None,
        });
        assert!(linked.contains("[GPT-6.1 Sol](t3-model://codex/GPT-6.1-Sol%5B1m%5D),"));
        assert!(linked.contains("model:openCode/anthropic/claude-opus-4-6"));
        assert!(linked.contains("`model:codex/ignored`"));
        assert_eq!(
            parse_model_mention_url("t3-model://codex/GPT-6.1-Sol%5B1m%5D"),
            Some(("codex".into(), "GPT-6.1-Sol[1m]".into()))
        );
    }
}
