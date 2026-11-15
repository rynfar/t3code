//! Markdown link classification and resolution (`packages/shared/src/markdownLinks.ts`,
//! `packages/shared/src/threadLinks.ts`, `apps/web/src/markdown-links.ts`,
//! `apps/web/src/components/chat/MarkdownFileLink.logic.ts`).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{Value, json};
use url::Url;

use crate::assistant_citations::{self, Citation};

/// VS Code-family URI schemes that name local host files (`LOCAL_FILE_LINK_SCHEMES`).
pub const LOCAL_FILE_LINK_SCHEMES: &[&str] = &["cursor", "vscode", "vscode-insiders", "vscodium"];
pub const GET_THREAD_LINK_METADATA: &str = "orchestration.getThreadLinkMetadata";
pub const GET_THREAD_TITLES: &str = "orchestration.getThreadTitles";
pub const WORKSPACE_BASENAME_LOOKUP_LIMIT: usize = 25;
pub const AGENT_DIRECTORY_SEARCH_LIMIT: usize = 4;
pub const FILESYSTEM_METADATA_BATCH_LIMIT: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadLinkMetadata {
    pub title: String,
    pub project_id: Option<String>,
}

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
    pub end_line: Option<u64>,
    pub media_kind: FileLinkMediaKind,
}

impl ResolvedFileLink {
    /// Formats a file position label such as `L42–64` or `L42:C5` (`fileLinks.ts`).
    pub fn position_label(&self) -> Option<String> {
        let line = self.line?;
        Some(match (self.end_line, self.column) {
            (Some(end_line), _) => format!("L{line}–{end_line}"),
            (None, Some(col)) => format!("L{line}:C{col}"),
            (None, None) => format!("L{line}"),
        })
    }
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
    ModelMention { instance_id: String, model: String },
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
    if let Some((instance_id, model)) = crate::markdown::mentions::parse_model_mention_url(trimmed) {
        return MarkdownLinkKind::ModelMention { instance_id, model };
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

    let (raw_path, line, column, end_line) = if trimmed.to_ascii_lowercase().starts_with("file://") {
        parse_file_url(trimmed)?
    } else if let Some(local) = parse_local_editor_file_url(trimmed) {
        local
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
        end_line,
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

type ParsedFileLocation = (String, Option<u64>, Option<u64>, Option<u64>);

fn parse_file_url(raw: &str) -> Option<ParsedFileLocation> {
    let url = Url::parse(raw).ok()?;
    if !url.scheme().eq_ignore_ascii_case("file") {
        return None;
    }
    let (line, col, end_line) = url.fragment().map(parse_hash_line_col).unwrap_or((None, None, None));
    let raw_path = percent_decode(url.path());
    let path = if raw_path.starts_with('/') && raw_path.len() >= 4 && is_windows_drive_path(&raw_path[1..]) {
        raw_path[1..].to_owned()
    } else {
        raw_path
    };
    let (path, line2, col2, end2) = split_line_column(&path);
    let final_line = line.or(line2);
    let final_end = end_line.or(end2).filter(|&end| final_line.is_some_and(|start| end > start));
    Some((path, final_line, col.or(col2), final_end))
}

/// Parses `cursor://file/...`, `vscode://file/...`, `vscode-insiders://file/...`, or `vscodium://file/...`
/// (`isLocalEditorFileUrl` in `packages/contracts/src/editor.ts`).
pub fn parse_local_editor_file_url(raw: &str) -> Option<ParsedFileLocation> {
    let url = Url::parse(raw).ok()?;
    let scheme = url.scheme().to_ascii_lowercase();
    if !LOCAL_FILE_LINK_SCHEMES.contains(&scheme.as_str()) {
        return None;
    }
    if url.host_str() != Some("file") || !url.username().is_empty() || url.password().is_some() || url.port().is_some() || url.path().len() <= 1 {
        return None;
    }
    let (line, col, end_line) = url.fragment().map(parse_hash_line_col).unwrap_or((None, None, None));
    let raw_path = percent_decode(url.path());
    let path = if raw_path.starts_with('/') && raw_path.len() >= 4 && is_windows_drive_path(&raw_path[1..]) {
        raw_path[1..].to_owned()
    } else {
        raw_path
    };
    let (path, line2, col2, end2) = split_line_column(&path);
    let final_line = line.or(line2);
    let final_end = end_line.or(end2).filter(|&end| final_line.is_some_and(|start| end > start));
    Some((path, final_line, col.or(col2), final_end))
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

fn split_line_column(value: &str) -> (String, Option<u64>, Option<u64>, Option<u64>) {
    if let Some((path, fragment)) = value.split_once('#') {
        let (line, col, end_line) = parse_hash_line_col(fragment);
        if line.is_some() {
            return (path.to_owned(), line, col, end_line);
        }
        return (path.to_owned(), None, None, None);
    }

    // Trailing `:line`, `:line:col`, or `:line-end` without breaking Windows `C:\...` prefixes.
    let drive_prefix_end = if is_windows_drive_path(value) { 2 } else { 0 };
    if let Some((before_last, last)) = value.rsplit_once(':')
        && before_last.len() > drive_prefix_end
    {
        if let Some((start_s, end_s)) = last.split_once('-')
            && let (Ok(line_num), Ok(end_num)) = (start_s.parse::<u64>(), end_s.parse::<u64>())
            && line_num > 0
        {
            let end_line = (end_num > line_num).then_some(end_num);
            return (before_last.to_owned(), Some(line_num), None, end_line);
        }
        if let Ok(n1) = last.parse::<u64>() {
            if let Some((path, mid)) = before_last.rsplit_once(':')
                && path.len() > drive_prefix_end
                && let Ok(line_num) = mid.parse::<u64>()
            {
                return (path.to_owned(), Some(line_num), Some(n1), None);
            }
            return (before_last.to_owned(), Some(n1), None, None);
        }
    }

    (value.to_owned(), None, None, None)
}

fn parse_hash_line_col(fragment: &str) -> (Option<u64>, Option<u64>, Option<u64>) {
    let cleaned = fragment.trim();
    if let Some((head, tail)) = cleaned.split_once('-') {
        let start_s = head.strip_prefix('L').or_else(|| head.strip_prefix('l')).unwrap_or(head);
        let end_s = tail.strip_prefix('L').or_else(|| tail.strip_prefix('l')).unwrap_or(tail);
        if let (Ok(line), Ok(end_num)) = (start_s.parse::<u64>(), end_s.parse::<u64>())
            && line > 0
        {
            return (Some(line), None, (end_num > line).then_some(end_num));
        }
        let body = start_s;
        if let Some((l, c)) = body.split_once(['C', 'c', ':']) {
            return (l.parse::<u64>().ok(), c.parse::<u64>().ok(), None);
        }
        return (body.parse::<u64>().ok(), None, None);
    }
    let body = cleaned.strip_prefix('L').or_else(|| cleaned.strip_prefix('l')).unwrap_or(cleaned);
    if let Some((l, c)) = body.split_once(['C', 'c', ':']) {
        return (l.parse::<u64>().ok(), c.parse::<u64>().ok(), None);
    }
    (body.parse::<u64>().ok(), None, None)
}

fn normalize_separators(path: &str) -> String {
    if is_windows_drive_path(path) {
        path.replace('/', "\\")
    } else {
        path.replace('\\', "/")
    }
}

pub fn resolve_full_and_relative(path: &str, cwd: Option<&str>) -> (String, Option<String>) {
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

/// Entry kind returned by `projects.searchEntries` (`WorkspaceEntryCandidate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceEntryKind {
    File,
    Directory,
}

/// Workspace entry candidate used for chat file-link fallback lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEntryCandidate {
    pub path: String,
    pub kind: WorkspaceEntryKind,
}

/// Returns `true` when `relative_path` is a bare file name without separators (`#33943`).
pub fn needs_workspace_basename_lookup(relative_path: &str) -> bool {
    let trimmed = relative_path.trim();
    trimmed != "."
        && trimmed != ".."
        && !trimmed.is_empty()
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
}

/// Picks a unique file match by basename from `entries` (exact case first, then folded).
pub fn pick_workspace_basename_match(
    basename: &str,
    entries: &[WorkspaceEntryCandidate],
) -> Option<String> {
    let target = basename.trim();
    if target.is_empty() {
        return None;
    }
    let files = entries
        .iter()
        .filter(|entry| entry.kind == WorkspaceEntryKind::File);
    if let Some(exact) = files.clone().find(|entry| basename_of(&entry.path) == target) {
        return Some(exact.path.clone());
    }
    let folded = target.to_lowercase();
    let mut folded_matches =
        files.filter(|entry| basename_of(&entry.path).to_lowercase() == folded);
    let first = folded_matches.next()?;
    folded_matches.next().is_none().then(|| first.path.clone())
}

fn strip_repeated_workspace_prefix(relative_path: &str, cwd: &str) -> Option<String> {
    if relative_path.starts_with('/') || is_windows_drive_path(relative_path) {
        return None;
    }
    let path_segments: Vec<&str> = relative_path.split(['/', '\\']).collect();
    let cwd_segments: Vec<&str> = cwd.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let win_cwd = is_windows_drive_path(cwd);
    let eq = |a: &str, b: &str| {
        if win_cwd {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    let max_count = cwd_segments
        .len()
        .min(path_segments.len().saturating_sub(1));
    for count in (1..=max_count).rev() {
        let prefix = &cwd_segments[cwd_segments.len() - count..];
        if prefix
            .iter()
            .zip(path_segments.iter())
            .all(|(a, b)| eq(a, b))
        {
            let rest = path_segments[count..].join("/");
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    None
}

/// Project search query for a chat file link, or `None` when the link already names a concrete path.
pub fn workspace_link_lookup_query(relative_path: &str, cwd: &str) -> Option<String> {
    let trimmed = relative_path.trim();
    let dot_or_tilde = trimmed.starts_with("./")
        || trimmed.starts_with(".\\")
        || trimmed.starts_with("../")
        || trimmed.starts_with("..\\")
        || trimmed.starts_with("~/")
        || trimmed.starts_with("~\\");
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || is_windows_drive_path(trimmed)
        || dot_or_tilde
        || trimmed.ends_with('/')
        || trimmed.ends_with('\\')
    {
        return None;
    }
    if needs_workspace_basename_lookup(trimmed) {
        return Some(trimmed.to_owned());
    }
    Some(strip_repeated_workspace_prefix(trimmed, cwd).unwrap_or_else(|| trimmed.to_owned()))
}

fn pick_stripped_workspace_match(
    stripped_path: &str,
    cwd: &str,
    entries: &[WorkspaceEntryCandidate],
) -> Option<String> {
    let win = is_windows_drive_path(cwd);
    let key = |path: &str| {
        let norm = path.replace('\\', "/");
        if win { norm.to_lowercase() } else { norm }
    };
    let target = key(stripped_path);
    entries
        .iter()
        .filter(|e| e.kind == WorkspaceEntryKind::File)
        .find(|e| key(&e.path) == target)
        .map(|e| e.path.clone())
}

/// Picks the single matching workspace-relative file from `entries` (`#33943`).
pub fn pick_workspace_link_match(
    relative_path: &str,
    cwd: &str,
    entries: &[WorkspaceEntryCandidate],
) -> Option<String> {
    let trimmed = relative_path.trim();
    if needs_workspace_basename_lookup(trimmed) {
        return pick_workspace_basename_match(trimmed, entries);
    }
    if let Some(stripped) = strip_repeated_workspace_prefix(trimmed, cwd)
        && let Some(found) = pick_stripped_workspace_match(&stripped, cwd, entries)
    {
        return Some(found);
    }
    let suffix = trimmed.replace('\\', "/");
    let suffix_slash = format!("/{suffix}");
    let mut matches = entries
        .iter()
        .filter(|e| e.kind == WorkspaceEntryKind::File)
        .filter(|e| {
            let norm = e.path.replace('\\', "/");
            norm == suffix || norm.ends_with(&suffix_slash)
        });
    let first = matches.next()?;
    matches.next().is_none().then(|| first.path.clone())
}

/// Resolves a unique absolute path across candidate project roots (`#35036`).
pub fn pick_project_link_match<'a, I>(relative_path: &str, searches: I) -> Option<String>
where
    I: IntoIterator<Item = (&'a str, &'a [WorkspaceEntryCandidate])>,
{
    let mut matches = searches.into_iter().filter_map(|(root, entries)| {
        let matched = pick_workspace_link_match(relative_path, root, entries)?;
        Some(resolve_full_and_relative(&matched, Some(root)).0)
    });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn is_shell_name(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    matches!(base, "sh" | "bash" | "zsh" | "dash")
}

fn is_cd_option(word: &str) -> bool {
    if word == "--" {
        return true;
    }
    let Some(flags) = word.strip_prefix('-') else {
        return false;
    };
    flags
        .chars()
        .all(|c| matches!(c, 'L' | 'P' | 'e' | '@'))
}

fn is_shell_c_flag(word: &str) -> bool {
    let Some(flags) = word.strip_prefix('-') else {
        return false;
    };
    !flags.is_empty() && flags.ends_with('c') && flags.chars().all(|c| c.is_ascii_lowercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellTokKind {
    Quoted,
    Comment,
    Separator,
    Space,
    Bare,
}

fn next_shell_token(bytes: &[u8], pos: usize) -> Option<(usize, ShellTokKind)> {
    let &b = bytes.get(pos)?;
    if matches!(b, b';' | b'&' | b'|' | b'(' | b')' | b'\n') {
        return Some((pos + 1, ShellTokKind::Separator));
    }
    if matches!(b, b' ' | b'\t') {
        let mut end = pos + 1;
        while matches!(bytes.get(end), Some(b' ' | b'\t')) {
            end += 1;
        }
        return Some((end, ShellTokKind::Space));
    }
    if b == b'"' || b == b'\'' {
        let mut end = pos + 1;
        while end < bytes.len() && bytes[end] != b {
            end += 1;
        }
        if end < bytes.len() {
            end += 1;
        }
        return Some((end, ShellTokKind::Quoted));
    }
    let prev_is_delim = pos == 0
        || matches!(
            bytes[pos - 1],
            b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')'
        );
    if b == b'#' && prev_is_delim {
        let mut end = pos + 1;
        while end < bytes.len() && bytes[end] != b'\n' {
            end += 1;
        }
        return Some((end, ShellTokKind::Comment));
    }
    let mut end = pos + 1;
    while end < bytes.len()
        && !matches!(
            bytes[end],
            b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')' | b'"' | b'\''
        )
    {
        end += 1;
    }
    Some((end, ShellTokKind::Bare))
}

/// Extracts `cd` operands from a shell command, recursing into `sh -c "<script>"` (`#36364`).
pub fn cd_operands(script: &str) -> Vec<String> {
    let mut operands = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word_open = false;

    let flush_command = |words: &mut Vec<String>, word_open: &mut bool, operands: &mut Vec<String>| {
        if let Some((name, rest)) = words.split_first() {
            if name == "cd" {
                if let Some(operand) = rest.iter().find(|w| !is_cd_option(w)) {
                    operands.push(operand.clone());
                }
            } else if is_shell_name(name)
                && let Some(c_idx) = rest.iter().position(|w| is_shell_c_flag(w))
                && let Some(inner) = rest.get(c_idx + 1)
                && ((inner.starts_with('"') && inner.ends_with('"') && inner.len() >= 2)
                    || (inner.starts_with('\'') && inner.ends_with('\'') && inner.len() >= 2))
            {
                operands.extend(cd_operands(&inner[1..inner.len() - 1]));
            }
        }
        words.clear();
        *word_open = false;
    };

    let bytes = script.as_bytes();
    let mut pos = 0;
    while let Some((end, kind)) = next_shell_token(bytes, pos) {
        let tok = &script[pos..end];
        pos = end;
        if kind == ShellTokKind::Bare && tok.contains("<<") {
            break;
        }
        match kind {
            ShellTokKind::Separator => flush_command(&mut words, &mut word_open, &mut operands),
            ShellTokKind::Space => word_open = false,
            ShellTokKind::Comment if !word_open => word_open = false,
            ShellTokKind::Quoted | ShellTokKind::Comment | ShellTokKind::Bare => {
                if word_open {
                    if let Some(last) = words.last_mut() {
                        last.push_str(tok);
                    }
                } else {
                    words.push(tok.to_owned());
                    word_open = true;
                }
            }
        }
    }
    flush_command(&mut words, &mut word_open, &mut operands);
    operands
}

fn literal_directory(operand: &str) -> Option<String> {
    let path = if (operand.starts_with('"') && operand.ends_with('"') && operand.len() >= 2)
        || (operand.starts_with('\'') && operand.ends_with('\'') && operand.len() >= 2)
    {
        &operand[1..operand.len() - 1]
    } else {
        operand
    };
    let is_abs = path.starts_with('/') || is_windows_drive_path(path);
    if !is_abs
        || path
            .chars()
            .any(|c| matches!(c, '$' | '`' | '*' | '?' | '[' | ']' | '{' | '}' | '"' | '\''))
    {
        return None;
    }
    if path.contains('\\') && !is_windows_drive_path(path) {
        return None;
    }
    Some(path.to_owned())
}

/// Ordered deduplicated list of literal directories from prior `command_execution` inputs, latest first.
pub fn agent_working_directories_from_commands<'a, I>(commands_before_reply_latest_first: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for command in commands_before_reply_latest_first {
        for operand in cd_operands(command).into_iter().rev() {
            if let Some(dir) = literal_directory(&operand)
                && seen.insert(dir.clone())
            {
                out.push(dir);
                if out.len() >= FILESYSTEM_METADATA_BATCH_LIMIT {
                    return out;
                }
            }
        }
    }
    out
}

/// True when `path` targets a macOS `.app` application bundle (`pingdotgg/t3code@a11ad7424d`).
pub fn is_macos_app_bundle_path(path: &str) -> bool {
    let trimmed = path.trim_end_matches(['/', '\\']);
    trimmed.to_ascii_lowercase().ends_with(".app")
}

/// Builds a `shell.openInEditor` payload with optional `projectRoot` for JetBrains IDEs (`#33558`).
pub fn open_in_editor_request_with_root(
    cwd: &str,
    editor: &str,
    project_root: Option<&str>,
) -> Value {
    match project_root.map(str::trim).filter(|r| !r.is_empty()) {
        Some(root) => json!({ "cwd": cwd, "editor": editor, "projectRoot": root }),
        None => json!({ "cwd": cwd, "editor": editor }),
    }
}

/// Fallback label when a thread link's target lives on an offline/unlinked environment (`#34657`).
pub fn unknown_thread_link_label(environment_reachable: bool) -> &'static str {
    if environment_reachable {
        "Thread"
    } else {
        "Thread on another machine"
    }
}

/// Decodes `{ entries: [{ threadId, title, projectId? }] }` (`getThreadLinkMetadata`) or
/// `{ titles: Record<ThreadId, string> }` (`getThreadTitles`) into a normalized map (`#32830`).
pub fn decode_thread_link_metadata_map(
    result: &Value,
    from_metadata_rpc: bool,
) -> HashMap<String, ThreadLinkMetadata> {
    let mut out = HashMap::new();
    if from_metadata_rpc {
        if let Some(entries) = result.get("entries").and_then(Value::as_array) {
            for entry in entries {
                let Some(thread_id) = entry.get("threadId").and_then(Value::as_str) else {
                    continue;
                };
                let Some(title) = entry.get("title").and_then(Value::as_str) else {
                    continue;
                };
                let project_id = entry
                    .get("projectId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                out.insert(
                    thread_id.to_owned(),
                    ThreadLinkMetadata {
                        title: title.to_owned(),
                        project_id,
                    },
                );
            }
        }
        return out;
    }
    if let Some(titles) = result.get("titles").and_then(Value::as_object) {
        for (thread_id, title_val) in titles {
            if let Some(title) = title_val.as_str() {
                out.insert(
                    thread_id.clone(),
                    ThreadLinkMetadata {
                        title: title.to_owned(),
                        project_id: None,
                    },
                );
            }
        }
    }
    out
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
        assert_eq!(f1.end_line, Some(50));
        assert_eq!(f1.position_label().as_deref(), Some("L42–50"));

        let f2 = resolve_file_link("C:\\work\\app\\main.ts:19:4", None).unwrap();
        assert_eq!(f2.full_path, "C:\\work\\app\\main.ts");
        assert_eq!(f2.line, Some(19));
        assert_eq!(f2.column, Some(4));
        assert_eq!(f2.position_label().as_deref(), Some("L19:C4"));

        let f3 = resolve_file_link("src/app.rs:42-64", Some("/repo")).unwrap();
        assert_eq!(f3.full_path, "/repo/src/app.rs");
        assert_eq!(f3.line, Some(42));
        assert_eq!(f3.end_line, Some(64));

        assert!(inline_code_file_path_candidate("apps/native/crates/core/src/lib.rs:10", None).is_some());
        assert!(inline_code_file_path_candidate("https://example.com/a.rs", None).is_none());
        assert!(inline_code_file_path_candidate("v1.2.3", None).is_none());
        assert!(inline_code_file_path_candidate("foo.barBaz", None).is_none());
    }

    #[test]
    fn classifies_local_editor_schemes_and_app_bundles() {
        assert_eq!(
            parse_local_editor_file_url("cursor://file/Volumes/repo/src/lib.rs:42-64"),
            Some(("/Volumes/repo/src/lib.rs".to_owned(), Some(42), None, Some(64)))
        );
        assert_eq!(
            parse_local_editor_file_url("vscode://file/c:/work/app.ts:10"),
            Some(("c:/work/app.ts".to_owned(), Some(10), None, None))
        );
        match classify_link("vscode://file/Volumes/repo/src/main.rs:8:2", Some("/Volumes/repo")) {
            MarkdownLinkKind::File(file) => {
                assert_eq!(file.full_path, "/Volumes/repo/src/main.rs");
                assert_eq!(file.line, Some(8));
                assert_eq!(file.column, Some(2));
            }
            other => panic!("expected file link, got {other:?}"),
        }
        assert!(is_macos_app_bundle_path("/Applications/Calculator.app"));
        assert!(is_macos_app_bundle_path("/Applications/Safari.app/"));
        assert!(!is_macos_app_bundle_path("/Applications/Calculator.rs"));
    }

    #[test]
    fn basename_project_and_agent_directory_fallbacks_match_typescript() {
        let entries = [
            WorkspaceEntryCandidate {
                path: "crates/app/src/markdown.rs".into(),
                kind: WorkspaceEntryKind::File,
            },
            WorkspaceEntryCandidate {
                path: "crates/core/src/lib.rs".into(),
                kind: WorkspaceEntryKind::File,
            },
        ];
        assert_eq!(
            workspace_link_lookup_query("markdown.rs", "/Volumes/repo"),
            Some("markdown.rs".into())
        );
        assert_eq!(
            workspace_link_lookup_query("repo/crates/app/src/markdown.rs", "/Volumes/repo"),
            Some("crates/app/src/markdown.rs".into())
        );
        assert_eq!(
            pick_workspace_link_match("markdown.rs", "/Volumes/repo", &entries).as_deref(),
            Some("crates/app/src/markdown.rs")
        );
        assert_eq!(
            pick_project_link_match("lib.rs", [("/Volumes/repo", &entries[..])]).as_deref(),
            Some("/Volumes/repo/crates/core/src/lib.rs")
        );

        let dirs = agent_working_directories_from_commands([
            "sh -c \"cd /Volumes/other-repo && cargo test\"",
            "cd -P /Volumes/first && echo hi # cd /ignored\ncd $HOME",
        ]);
        assert_eq!(dirs, vec!["/Volumes/other-repo", "/Volumes/first"]);
    }

    #[test]
    fn thread_link_metadata_decoding_and_unreachable_label() {
        let rpc = json!({
            "entries": [
                { "threadId": "t1", "title": "Fix parser", "projectId": "p1" },
                { "threadId": "t2", "title": "No project" }
            ]
        });
        let map = decode_thread_link_metadata_map(&rpc, true);
        assert_eq!(
            map.get("t1"),
            Some(&ThreadLinkMetadata {
                title: "Fix parser".into(),
                project_id: Some("p1".into()),
            })
        );
        assert_eq!(map.get("t2").and_then(|m| m.project_id.as_deref()), None);

        let fallback = json!({ "titles": { "t1": "Legacy title" } });
        let fallback_map = decode_thread_link_metadata_map(&fallback, false);
        assert_eq!(
            fallback_map.get("t1"),
            Some(&ThreadLinkMetadata {
                title: "Legacy title".into(),
                project_id: None,
            })
        );
        assert_eq!(unknown_thread_link_label(false), "Thread on another machine");
        assert_eq!(unknown_thread_link_label(true), "Thread");
        assert_eq!(
            open_in_editor_request_with_root("/repo/a.rs:5", "intellij", Some("/repo")),
            json!({ "cwd": "/repo/a.rs:5", "editor": "intellij", "projectRoot": "/repo" })
        );
    }
}
