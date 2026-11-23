//! GitHub `:shortcode:` emoji expansion for pull request markdown
//! (`packages/client-runtime/src/pullRequestMarkdown.ts`).

use std::collections::HashMap;
use std::sync::OnceLock;

use markdown::mdast::Node;

static ALIASES: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn aliases() -> &'static HashMap<&'static str, &'static str> {
    ALIASES.get_or_init(|| {
        include_str!("github_emoji_aliases.txt")
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .collect()
    })
}

fn is_escaped_colon(bytes: &[u8], colon: usize) -> bool {
    let mut backslashes = 0usize;
    let mut i = colon;
    while i > 0 && bytes[i - 1] == b'\\' {
        backslashes += 1;
        i -= 1;
    }
    backslashes % 2 == 1
}

/// Expands GitHub `:alias:` shortcodes in plain text. A closing colon ends one alias only, so
/// `:tada:tada:` expands only `:tada:` (`expandGithubEmojiAliases`).
pub fn expand_github_emoji_aliases(text: &str) -> String {
    if !text.contains(':') {
        return text.to_owned();
    }
    let map = aliases();
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut out = String::new();
    let mut last = 0usize;
    let mut i = 0usize;
    while i < len {
        if bytes[i] != b':' || is_escaped_colon(bytes, i) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < len && matches!(bytes[j], b'a'..=b'z' | b'0'..=b'9' | b'_' | b'+' | b'-') {
            j += 1;
        }
        if j > i + 1 && j < len && bytes[j] == b':' {
            if let Some(emoji) = map.get(&text[i + 1..j]) {
                out.push_str(&text[last..i]);
                out.push_str(emoji);
                i = j + 1;
                last = i;
                continue;
            }
            // Match JS `/:([a-z\d_+-]+)(?=:)/gu`: keep scanning from the closing `:`.
            i = j;
        } else {
            i = j.max(i + 1);
        }
    }
    if last == 0 {
        text.to_owned()
    } else {
        out.push_str(&text[last..]);
        out
    }
}

fn collect_prose_text_spans(node: &Node, out: &mut Vec<(usize, usize)>) {
    match node {
        Node::Code(_) | Node::InlineCode(_) | Node::Html(_) | Node::Yaml(_) | Node::Toml(_) => {}
        Node::Link(link) => {
            let is_autolink = link.children.len() == 1
                && matches!(&link.children[0], Node::Text(text) if text.value == link.url);
            if !is_autolink {
                for child in &link.children {
                    collect_prose_text_spans(child, out);
                }
            }
        }
        Node::Text(_) => {
            if let Some(pos) = node.position()
                && pos.start.offset < pos.end.offset
            {
                out.push((pos.start.offset, pos.end.offset));
            }
        }
        _ => {
            if let Some(children) = node.children() {
                for child in children {
                    collect_prose_text_spans(child, out);
                }
            }
        }
    }
}

/// Expands GitHub `:shortcode:` emoji in markdown prose while keeping fenced code blocks, inline
/// code spans, link destinations, autolink URLs, and HTML tags literal (`#39315`).
pub fn replace_github_emojis(markdown_text: &str) -> String {
    if !markdown_text.contains(':') {
        return markdown_text.to_owned();
    }
    let Ok(root) = markdown::to_mdast(markdown_text, &markdown::ParseOptions::gfm()) else {
        return markdown_text.to_owned();
    };
    let mut spans = Vec::new();
    collect_prose_text_spans(&root, &mut spans);
    spans.sort_unstable();
    let mut out = String::with_capacity(markdown_text.len());
    let mut cursor = 0usize;
    for (start, end) in spans {
        if start < cursor || end > markdown_text.len() || start >= end {
            continue;
        }
        if !markdown_text.is_char_boundary(start) || !markdown_text.is_char_boundary(end) {
            continue;
        }
        out.push_str(&markdown_text[cursor..start]);
        out.push_str(&expand_github_emoji_aliases(&markdown_text[start..end]));
        cursor = end;
    }
    if cursor == 0 {
        markdown_text.to_owned()
    } else {
        out.push_str(&markdown_text[cursor..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_known_shortcodes_in_prose_and_preserves_unknown() {
        assert_eq!(
            expand_github_emoji_aliases("Ship it :tada: :+1: :nonexistent_xyz:"),
            "Ship it 🎉 👍 :nonexistent_xyz:"
        );
        assert_eq!(expand_github_emoji_aliases(":tada:tada:"), "🎉tada:");
        assert_eq!(expand_github_emoji_aliases(":unknown:rocket:"), ":unknown🚀");
    }

    #[test]
    fn skips_code_links_urls_and_html_when_replacing_markdown() {
        let input = concat!(
            "Launch :rocket: [label :tada:](https://example.com/:shipit:/path) `:tada:`\n\n",
            "```rust\nlet s = \":rocket:\";\n```\n\n",
            "https://example.com/:tada:/raw <a href=\":rocket:\">:smile:</a>",
        );
        let actual = replace_github_emojis(input);
        assert!(actual.contains("Launch 🚀 [label 🎉](https://example.com/:shipit:/path) `:tada:`"));
        assert!(actual.contains("let s = \":rocket:\";"));
        assert!(actual.contains("https://example.com/:tada:/raw"));
        assert!(actual.contains("<a href=\":rocket:\">"));
    }
}
