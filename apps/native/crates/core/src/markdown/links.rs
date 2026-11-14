//! Markdown link classification and resolution (`packages/shared/src/markdownLinks.ts`,
//! `packages/shared/src/threadLinks.ts`, `apps/web/src/markdown-links.ts`,
//! `apps/web/src/components/chat/MarkdownFileLink.logic.ts`).

use std::path::Path;

use url::Url;

use crate::assistant_citations::{self, Citation};

/// Parsed target of a `t3-thread://` or hosted `app.t3.codes` thread link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadLinkTarget {
    pub version: u8,
    pub environment_id: Option<String>,
    pub thread_id: String,
    pub allow_cross_env_fallback: bool,
}

/// Media classification for file links so images/videos/PDFs can open preview flows cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileLinkMediaKind {
    CodeOrText,
    Image,
    Video,
    Pdf,
}

/// Resolved file link metadata for opening in the right panel, external editor, or file manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFileLink {
    pub raw_href: String,
    pub full_path: String,
    pub relative_path: Option<String>,
    pub basename: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
    pub media_kind: FileLinkMediaKind,
}

/// Safe external HTTP(S) or mailto link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalWebLink {
    pub url: String,
    pub host: Option<String>,
}

/// High-level semantic kind of a link or inline-code candidate found in Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownLinkKind {
    Citation(Citation),
    Thread(ThreadLinkTarget),
    File(ResolvedFileLink),
    External(ExternalWebLink),
    Anchor(String),
    Unsupported,
}

/// Classifies any Markdown `href` using optional workspace `cwd` for file link resolution.
pub fn classify_link(href: &str, cwd: Option<&str>) -> MarkdownLinkKind {
    let trimmed = href.trim();
    if trimmed.is_empty() {
        return MarkdownLinkKind::Unsupported;
    }
    if let Some(fragment) = trimmed.strip_prefix('#') {
        return MarkdownLinkKind::Anchor(fragment.to_owned());
    }
    if let Some(citation) = assistant_citations::parse(trimmed) {
        return MarkdownLinkKind::Citation(citation);
    }
    if let Some(thread) = parse_thread_link(trimmed) {
        return MarkdownLinkKind::Thread(thread);
    }
    if let Some(file) = resolve_file_link(trimmed, cwd) {
        return MarkdownLinkKind::File(file);
    }
    if crate::media::markdown_external_link(trimmed) {
        let host = Url::parse(trimmed)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.strip_prefix("www.").unwrap_or(h).to_owned()));
        return MarkdownLinkKind::External(ExternalWebLink { url: trimmed.to_owned(), host });
    }
    MarkdownLinkKind::Unsupported
}

/// Parses `t3-thread://v1/...`, `t3-thread://v2/...`, and `https://app.t3.codes/...` thread URLs.
pub fn parse_thread_link(raw: &str) -> Option<ThreadLinkTarget> {
    let url = Url::parse(raw).or_else(|_| {
        let decoded = percent_decode(raw);
        Url::parse(&decoded)
    }).ok()?;

    if url.scheme().eq_ignore_ascii_case("t3-thread") {
        let version_seg = url.host_str()?.to_ascii_lowercase();
        let segments: Vec<String> = url
            .path_segments()?
            .map(percent_decode)
            .filter(|s| !s.is_empty())
            .collect();
        return match (version_seg.as_str(), segments.as_slice()) {
            ("v1", [thread_id]) => Some(ThreadLinkTarget {
                version: 1,
                environment_id: None,
                thread_id: thread_id.clone(),
                allow_cross_env_fallback: true,
            }),
            ("v1", [env_id, thread_id]) => Some(ThreadLinkTarget {
                version: 1,
                environment_id: Some(env_id.clone()),
                thread_id: thread_id.clone(),
                allow_cross_env_fallback: true,
            }),
            ("v2", [env_id, thread_id]) => Some(ThreadLinkTarget {
                version: 2,
                environment_id: Some(env_id.clone()),
                thread_id: thread_id.clone(),
                allow_cross_env_fallback: false,
            }),
            ("v2", [thread_id]) => Some(ThreadLinkTarget {
                version: 2,
                environment_id: None,
                thread_id: thread_id.clone(),
                allow_cross_env_fallback: false,
            }),
            _ => None,
        };
    }

    if matches!(url.scheme().to_ascii_lowercase().as_str(), "http" | "https")
        && url.host_str().is_some_and(|h| h.eq_ignore_ascii_case("app.t3.codes"))
    {
        let segments: Vec<String> = url
            .path_segments()?
            .map(percent_decode)
            .filter(|s| !s.is_empty())
            .collect();
        match segments.as_slice() {
            [first, env_id, thread_id] if first == "threads" => {
                return Some(ThreadLinkTarget {
                    version: 2,
                    environment_id: Some(env_id.clone()),
                    thread_id: thread_id.clone(),
                    allow_cross_env_fallback: true,
                });
            }
            [first, thread_id] if first == "threads" || first == "t" => {
                return Some(ThreadLinkTarget {
                    version: 1,
                    environment_id: None,
                    thread_id: thread_id.clone(),
                    allow_cross_env_fallback: true,
                });
            }
            [thread_id] => {
                return Some(ThreadLinkTarget {
                    version: 1,
                    environment_id: None,
                    thread_id: thread_id.clone(),
                    allow_cross_env_fallback: true,
                });
            }
            _ => {}
        }
    }
    None
}

/// Resolves a Markdown link target into a workspace/host file path, line, column, and media kind.
pub fn resolve_file_link(href: &str, cwd: Option<&str>) -> Option<ResolvedFileLink> {
    let trimmed = href.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let (raw_path, line, column) = if trimmed.to_ascii_lowercase().starts_with("file://") {
        parse_file_url(trimmed)?
    } else if has_non_file_url_scheme(trimmed) {
        return None;
    } else {
        let stripped = strip_query(trimmed);
        split_line_column(stripped)
    };

    let normalized_path = normalize_separators(&percent_decode(&raw_path));
    if normalized_path.is_empty() || normalized_path.contains('\0') {
        return None;
    }

    let (full_path, relative_path) = resolve_full_and_relative(&normalized_path, cwd);
    let basename = basename_of(&full_path);
    if basename.is_empty() {
        return None;
    }

    let media_kind = classify_media_extension(&full_path);
    Some(ResolvedFileLink {
        raw_href: trimmed.to_owned(),
        full_path,
        relative_path,
        basename,
        line,
        column,
        media_kind,
    })
}

/// Heuristic for treating inline code spans like `` `src/main.rs:42` `` as clickable file links
/// when no explicit hyperlink wraps them (`inlineCodeFilePathCandidate`).
pub fn inline_code_file_path_candidate(code: &str, cwd: Option<&str>) -> Option<ResolvedFileLink> {
    let trimmed = code.trim();
    if trimmed.is_empty()
        || trimmed.len() > 300
        || trimmed.contains('\n')
        || trimmed.contains(' ')
        || trimmed.starts_with('-')
        || has_non_file_url_scheme(trimmed)
    {
        return None;
    }

    let is_file_url = trimmed.to_ascii_lowercase().starts_with("file://");
    let has_windows_drive = is_windows_drive_path(trimmed);
    let has_slash = trimmed.contains('/') || trimmed.contains('\\');
    let stripped = split_line_column(strip_query(trimmed)).0;
    let has_line_suffix = stripped.len() < trimmed.len();

    if !is_file_url && !has_windows_drive && !has_slash && !has_line_suffix {
        let ext = stripped.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        if !ext.as_deref().is_some_and(is_known_bare_filename_extension) {
            return None;
        }
    }

    if trimmed.starts_with('/') && trimmed.split('/').filter(|s| !s.is_empty()).count() < 2 && !has_line_suffix {
        return None;
    }

    let resolved = resolve_file_link(trimmed, cwd)?;
    if !is_file_url && !has_windows_drive && resolved.basename.ends_with(".0") {
        return None;
    }
    Some(resolved)
}

fn is_known_bare_filename_extension(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go" | "java" | "kt"
            | "swift" | "c" | "cc" | "cpp" | "h" | "hpp" | "rb" | "php" | "cs" | "scala"
            | "md" | "mdx" | "txt" | "json" | "jsonc" | "toml" | "yaml" | "yml" | "xml"
            | "html" | "css" | "scss" | "sql" | "sh" | "bash" | "zsh" | "lock" | "env"
            | "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "pdf" | "mp4" | "mov" | "webm"
    )
}

fn classify_media_extension(path: &str) -> FileLinkMediaKind {
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif") => {
            FileLinkMediaKind::Image
        }
        Some("mp4" | "mov" | "webm" | "m4v" | "mkv") => FileLinkMediaKind::Video,
        Some("pdf") => FileLinkMediaKind::Pdf,
        _ => FileLinkMediaKind::CodeOrText,
    }
}

fn parse_file_url(raw: &str) -> Option<(String, Option<u64>, Option<u64>)> {
    let url = Url::parse(raw).ok()?;
    if !url.scheme().eq_ignore_ascii_case("file") {
        return None;
    }
    let (line, col) = url.fragment().map(parse_hash_line_col).unwrap_or((None, None));
    let raw_path = percent_decode(url.path());
    let path = if raw_path.starts_with('/') && raw_path.len() >= 4 && is_windows_drive_path(&raw_path[1..]) {
        raw_path[1..].to_owned()
    } else {
        raw_path
    };
    let (path, line2, col2) = split_line_column(&path);
    Some((path, line.or(line2), col.or(col2)))
}

fn has_non_file_url_scheme(value: &str) -> bool {
    if is_windows_drive_path(value) {
        return false;
    }
    let Some((scheme, rest)) = value.split_once(':') else { return false };
    if !rest.starts_with("//") && !scheme.eq_ignore_ascii_case("mailto") && !scheme.eq_ignore_ascii_case("tel") {
        return false;
    }
    !scheme.is_empty()
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !scheme.eq_ignore_ascii_case("file")
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn strip_query(value: &str) -> &str {
    value.split('?').next().unwrap_or(value)
}

fn split_line_column(value: &str) -> (String, Option<u64>, Option<u64>) {
    if let Some((path, fragment)) = value.split_once('#') {
        let (line, col) = parse_hash_line_col(fragment);
        if line.is_some() {
            return (path.to_owned(), line, col);
        }
        return (path.to_owned(), None, None);
    }

    // Trailing `:line` or `:line:col` without breaking Windows `C:\...` prefixes.
    let drive_prefix_end = if is_windows_drive_path(value) { 2 } else { 0 };
    if let Some((before_last, last)) = value.rsplit_once(':')
        && before_last.len() > drive_prefix_end
        && let Ok(n1) = last.parse::<u64>()
    {
        if let Some((path, mid)) = before_last.rsplit_once(':')
            && path.len() > drive_prefix_end
            && let Ok(line_num) = mid.parse::<u64>()
        {
            return (path.to_owned(), Some(line_num), Some(n1));
        }
        return (before_last.to_owned(), Some(n1), None);
    }

    (value.to_owned(), None, None)
}

fn parse_hash_line_col(fragment: &str) -> (Option<u64>, Option<u64>) {
    let cleaned = fragment.trim();
    let head = cleaned.split('-').next().unwrap_or(cleaned);
    let body = head.strip_prefix('L').or_else(|| head.strip_prefix('l')).unwrap_or(head);
    if let Some((l, c)) = body.split_once(['C', 'c', ':']) {
        return (l.parse::<u64>().ok(), c.parse::<u64>().ok());
    }
    (body.parse::<u64>().ok(), None)
}

fn normalize_separators(path: &str) -> String {
    if is_windows_drive_path(path) {
        path.replace('/', "\\")
    } else {
        path.replace('\\', "/")
    }
}

fn resolve_full_and_relative(path: &str, cwd: Option<&str>) -> (String, Option<String>) {
    let cleaned = path.strip_prefix("./").unwrap_or(path);
    let is_abs = path.starts_with('/') || is_windows_drive_path(path);
    if is_abs {
        let relative = cwd.and_then(|root| strip_workspace_root(path, root));
        (path.to_owned(), relative)
    } else if let Some(root) = cwd.filter(|r| !r.is_empty()) {
        let sep = if is_windows_drive_path(root) { '\\' } else { '/' };
        let joined = format!("{}{sep}{cleaned}", root.trim_end_matches(['/', '\\']));
        (joined, Some(cleaned.to_owned()))
    } else {
        (cleaned.to_owned(), Some(cleaned.to_owned()))
    }
}

fn strip_workspace_root(abs: &str, root: &str) -> Option<String> {
    let norm_abs = abs.replace('\\', "/");
    let norm_root = root.replace('\\', "/");
    let norm_root = norm_root.trim_end_matches('/');
    let rest = norm_abs.strip_prefix(norm_root)?;
    let rest = rest.strip_prefix('/')?;
    (!rest.is_empty()).then(|| rest.to_owned())
}

fn basename_of(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_owned()
}

pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_owned())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v1_v2_and_hosted_thread_links_with_percent_fallback() {
        assert_eq!(
            parse_thread_link("t3-thread://v1/env-a/thread-1"),
            Some(ThreadLinkTarget {
                version: 1,
                environment_id: Some("env-a".into()),
                thread_id: "thread-1".into(),
                allow_cross_env_fallback: true,
            })
        );
        assert_eq!(
            parse_thread_link("t3-thread://v2/env-a/thread-2"),
            Some(ThreadLinkTarget {
                version: 2,
                environment_id: Some("env-a".into()),
                thread_id: "thread-2".into(),
                allow_cross_env_fallback: false,
            })
        );
        assert_eq!(
            parse_thread_link("https://app.t3.codes/threads/env-1/thr%2D9"),
            Some(ThreadLinkTarget {
                version: 2,
                environment_id: Some("env-1".into()),
                thread_id: "thr-9".into(),
                allow_cross_env_fallback: true,
            })
        );
    }

    #[test]
    fn resolves_file_urls_windows_paths_line_columns_and_inline_candidates() {
        let f1 = resolve_file_link("file:///Volumes/repo/src/lib.rs#L42-L50", Some("/Volumes/repo")).unwrap();
        assert_eq!(f1.full_path, "/Volumes/repo/src/lib.rs");
        assert_eq!(f1.relative_path.as_deref(), Some("src/lib.rs"));
        assert_eq!(f1.line, Some(42));

        let f2 = resolve_file_link("C:\\work\\app\\main.ts:19:4", None).unwrap();
        assert_eq!(f2.full_path, "C:\\work\\app\\main.ts");
        assert_eq!(f2.line, Some(19));
        assert_eq!(f2.column, Some(4));

        assert!(inline_code_file_path_candidate("apps/native/crates/core/src/lib.rs:10", None).is_some());
        assert!(inline_code_file_path_candidate("https://example.com/a.rs", None).is_none());
        assert!(inline_code_file_path_candidate("v1.2.3", None).is_none());
        assert!(inline_code_file_path_candidate("foo.barBaz", None).is_none());
    }
}
