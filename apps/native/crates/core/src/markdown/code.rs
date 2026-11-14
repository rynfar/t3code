//! Fenced code metadata, language normalization, closing detection, and runnable shell checks
//! (`apps/web/src/components/chat/MarkdownCodeBlock.logic.ts`).

/// Normalizes a fenced code block language label to match syntax highlighting grammars.
pub fn normalize_language(raw: &str) -> Option<String> {
    let trimmed = raw.trim().strip_prefix("language-").unwrap_or(raw.trim()).to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = match trimmed.as_str() {
        "gitignore" | ".gitignore" | "gitattributes" | ".gitattributes" | ".editorconfig" | "editorconfig" => "ini",
        "docker" => "dockerfile",
        "yml" => "yaml",
        "py" => "python",
        "rs" => "rust",
        "ts" => "typescript",
        "js" => "javascript",
        _ => return Some(trimmed),
    };
    Some(normalized.to_owned())
}

/// Extracts an explicit title or filename from a markdown code fence's metadata string.
/// Supports `title="..."`, `file="..."`, `filename="..."`, `path="..."`, or bare path/filename tokens.
pub fn extract_fence_title(meta: Option<&str>) -> Option<String> {
    let meta = meta?.trim();
    if meta.is_empty() {
        return None;
    }

    for key in ["title=", "file=", "filename=", "path="] {
        if let Some(value) = extract_kv_value(meta, key)
            && !value.is_empty()
        {
            return Some(value);
        }
    }

    for token in split_meta_tokens(meta) {
        if is_ignored_meta_token(token) {
            continue;
        }
        if looks_like_filename_or_path(token) {
            return Some(token.to_owned());
        }
    }
    None
}

fn extract_kv_value(meta: &str, key: &str) -> Option<String> {
    let mut search_from = 0;
    while let Some(pos) = meta[search_from..].find(key) {
        let abs = search_from + pos;
        let boundary_ok = abs == 0 || meta[..abs].chars().next_back().is_some_and(char::is_whitespace);
        if !boundary_ok {
            search_from = abs + key.len();
            continue;
        }
        let rest = &meta[abs + key.len()..];
        if let Some(quote @ ('"' | '\'')) = rest.chars().next() {
            let inner = &rest[quote.len_utf8()..];
            if let Some(end) = inner.find(quote) {
                return Some(inner[..end].trim().to_owned());
            }
            return None;
        }
        let value = rest.split_whitespace().next().unwrap_or("");
        if !value.is_empty() {
            return Some(value.to_owned());
        }
        return None;
    }
    None
}

fn split_meta_tokens(meta: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut in_quote: Option<char> = None;
    let mut brace_depth = 0usize;

    for (idx, ch) in meta.char_indices() {
        if let Some(q) = in_quote {
            if ch == q {
                in_quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                in_quote = Some(ch);
                if start.is_none() {
                    start = Some(idx);
                }
            }
            '{' => {
                brace_depth += 1;
                if start.is_none() {
                    start = Some(idx);
                }
            }
            '}' => {
                brace_depth = brace_depth.saturating_sub(1);
            }
            c if c.is_whitespace() && brace_depth == 0 => {
                if let Some(begin) = start.take() {
                    tokens.push(&meta[begin..idx]);
                }
            }
            _ => {
                if start.is_none() {
                    start = Some(idx);
                }
            }
        }
    }
    if let Some(begin) = start {
        tokens.push(&meta[begin..]);
    }
    tokens
}

fn is_ignored_meta_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    token.starts_with('{')
        || token.contains('=')
        || matches!(lower.as_str(), "diff" | "showlinenumbers" | "linenos" | "copy" | "nowrap" | "wrap")
}

fn looks_like_filename_or_path(token: &str) -> bool {
    if token.contains('/') || token.contains('\\') {
        return token.chars().any(char::is_alphanumeric);
    }
    if let Some((stem, ext)) = token.rsplit_once('.') {
        !stem.is_empty() && !ext.is_empty() && ext.len() <= 16 && ext.chars().all(|c| c.is_ascii_alphanumeric())
    } else {
        false
    }
}

/// Returns true when `lang` is a runnable interactive shell language.
pub fn is_shell_language(lang: Option<&str>) -> bool {
    let Some(lang) = lang.and_then(normalize_language) else { return false };
    matches!(lang.as_str(), "sh" | "bash" | "zsh" | "shell" | "fish" | "ksh" | "ash" | "dash")
}

/// Returns true when `lang` denotes a Mermaid diagram fence.
pub fn is_mermaid_language(lang: Option<&str>) -> bool {
    lang.and_then(normalize_language).as_deref() == Some("mermaid")
}

/// Returns true when `lang` denotes a math / LaTeX block fence.
pub fn is_math_language(lang: Option<&str>) -> bool {
    matches!(lang.and_then(normalize_language).as_deref(), Some("math" | "latex" | "tex"))
}

/// Returns whether a fenced code block starting at `fence_start_offset` in `source` has an explicit
/// closing fence line (so partial streaming fences stay raw until complete).
pub fn is_code_fence_closed(source: &str, fence_start_offset: usize) -> bool {
    let Some(slice) = source.get(fence_start_offset..) else { return false };
    let mut lines = slice.lines();
    let Some(first) = lines.next() else { return false };
    let trimmed = first.trim_start();
    let Some(fence_char @ ('`' | '~')) = trimmed.chars().next() else { return false };
    let open_len = trimmed.bytes().take_while(|&b| b as char == fence_char).count();
    if open_len < 3 {
        return false;
    }

    for line in lines {
        let indent = line.bytes().take_while(|&b| b == b' ').count();
        if indent > 3 {
            continue;
        }
        let body = &line[indent..];
        let close_len = body.bytes().take_while(|&b| b as char == fence_char).count();
        if close_len >= open_len && body[close_len..].trim().is_empty() {
            return true;
        }
    }
    false
}

/// Whether the shell code fence should offer a "Run in terminal" action.
pub fn can_run_shell_command(lang: Option<&str>, code: &str, closed: bool) -> bool {
    closed && is_shell_language(lang) && !code.trim().is_empty()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FenceMeta {
    pub title: Option<String>,
    pub closed: bool,
    pub display_lang: Option<String>,
}

/// Pre-indexed metadata for fenced code blocks in one rendered markdown document.
#[derive(Debug, Clone, Default)]
pub struct CodeFenceMetaMap {
    by_lang_code: std::collections::HashMap<(Option<String>, String), FenceMeta>,
    by_code: std::collections::HashMap<String, FenceMeta>,
}

impl CodeFenceMetaMap {
    pub fn from_markdown(source: &str) -> Self {
        let mut map = Self::default();
        let Ok(root) = markdown::to_mdast(source, &markdown::ParseOptions::gfm()) else {
            return map;
        };
        fn walk(node: &markdown::mdast::Node, source: &str, out: &mut CodeFenceMetaMap) {
            if let markdown::mdast::Node::Code(code) = node {
                let raw_lang = code.lang.clone();
                let display_lang = raw_lang.as_deref().and_then(normalize_language).or_else(|| raw_lang.clone());
                let title = extract_fence_title(code.meta.as_deref());
                let closed = code.position.as_ref().is_some_and(|p| is_code_fence_closed(source, p.start.offset));
                let entry = FenceMeta { title, closed, display_lang };
                out.by_lang_code.entry((raw_lang, code.value.clone())).or_insert_with(|| entry.clone());
                out.by_code.entry(code.value.clone()).or_insert(entry);
                return;
            }
            if let Some(children) = node.children() {
                for child in children {
                    walk(child, source, out);
                }
            }
        }
        walk(&root, source, &mut map);
        map
    }

    pub fn lookup(&self, lang: Option<&str>, code: &str) -> Option<&FenceMeta> {
        let lang_key = lang.filter(|s| !s.is_empty()).map(str::to_owned);
        self.by_lang_code.get(&(lang_key, code.to_owned())).or_else(|| self.by_code.get(code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_gitignore_and_docker_aliases() {
        assert_eq!(normalize_language(".gitignore").as_deref(), Some("ini"));
        assert_eq!(normalize_language("language-gitignore").as_deref(), Some("ini"));
        assert_eq!(normalize_language("docker").as_deref(), Some("dockerfile"));
        assert_eq!(normalize_language("Rust").as_deref(), Some("rust"));
        assert_eq!(normalize_language("   "), None);
    }

    #[test]
    fn extracts_fence_titles_from_kv_and_filenames() {
        assert_eq!(extract_fence_title(Some(r#"title="src/main.rs" {1-4}"#)).as_deref(), Some("src/main.rs"));
        assert_eq!(extract_fence_title(Some("file='app/config.ts'")).as_deref(), Some("app/config.ts"));
        assert_eq!(extract_fence_title(Some("showLineNumbers apps/web/package.json")).as_deref(), Some("apps/web/package.json"));
        assert_eq!(extract_fence_title(Some("{1,3-5} diff")), None);
    }

    #[test]
    fn detects_closed_vs_streaming_code_fences() {
        let closed = "```bash title=\"run.sh\"\necho hi\n```\n";
        assert!(is_code_fence_closed(closed, 0));
        assert!(can_run_shell_command(Some("bash"), "echo hi", true));

        let unclosed = "```bash\necho hi\n";
        assert!(!is_code_fence_closed(unclosed, 0));
        assert!(!can_run_shell_command(Some("bash"), "echo hi", false));
    }

    #[test]
    fn indexes_code_fence_titles_aliases_and_closed_states() {
        let doc = "```ts title=\"src/index.ts\"\nconst x = 1;\n```\n\n```sh\necho live\n";
        let map = CodeFenceMetaMap::from_markdown(doc);
        let ts = map.lookup(Some("ts"), "const x = 1;").expect("ts fence indexed");
        assert_eq!(ts.title.as_deref(), Some("src/index.ts"));
        assert_eq!(ts.display_lang.as_deref(), Some("typescript"));
        assert!(ts.closed);

        let sh = map.lookup(Some("sh"), "echo live").expect("streaming sh fence indexed");
        assert_eq!(sh.display_lang.as_deref(), Some("sh"));
        assert!(!sh.closed);
    }
}
