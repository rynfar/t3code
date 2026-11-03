//! Project files: the `projects.listEntries`, `projects.searchEntries` and `projects.readFile`
//! wire shapes (`packages/contracts/src/project.ts`), the path rules of
//! `apps/web/src/components/files/filePath.ts`, `filePreviewMode.ts` and
//! `packages/shared/src/path.ts` (`workspaceRelativeFilePath`, `isHostPath`), the mention text of
//! `packages/shared/src/composerTrigger.ts` (`serializeComposerFileLink`), and the read-error copy
//! of `filePreviewReadErrorMessage`. The explorer tree lives in `tree`, line layout in `text`.

mod text;
mod tree;

use serde_json::{Value, json};

pub use text::{reveal_top_line, LineIndex, MAX_LINE_CHARS, expand_tabs};
pub use tree::{FileTree, TreeRow};

pub const LIST_ENTRIES: &str = "projects.listEntries";
pub const SEARCH_ENTRIES: &str = "projects.searchEntries";
pub const READ_FILE: &str = "projects.readFile";
pub const CREATE_ASSET_URL: &str = "assets.createUrl";

/// `PROJECT_SEARCH_ENTRIES_MAX_LIMIT`, the limit `useProjectPathSearch` asks for.
pub const SEARCH_LIMIT: u32 = 200;
/// `ProjectSearchEntriesInput.query` is at most 256 characters.
const SEARCH_QUERY_MAX_CHARS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Directory,
}

/// `ProjectEntry`: a workspace-relative path with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
    /// Git ignores it (`node_modules`, build output): shown dimmed, skipped by expand all.
    pub ignored: bool,
}

impl Entry {
    pub fn name(&self) -> &str {
        basename(&self.path)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

pub fn list_entries_request(cwd: &str, directory: &str) -> Value {
    json!({ "cwd": cwd, "directoryPath": directory })
}

pub fn search_entries_request(cwd: &str, query: &str) -> Value {
    let query: String = query.chars().take(SEARCH_QUERY_MAX_CHARS).collect();
    json!({ "cwd": cwd, "query": query.trim(), "limit": SEARCH_LIMIT })
}

pub fn read_file_request(cwd: &str, relative_path: &str) -> Value {
    json!({ "cwd": cwd, "relativePath": relative_path })
}

/// `AssetResource` `workspace-file`, or `media-file` for a path outside the workspace
/// (`workspaceAssetResource`): the server signs workspace files only below the root.
pub fn image_asset_request(thread_id: &str, workspace_root: &str, absolute_path: &str) -> Value {
    let tag = if workspace_relative_path(absolute_path, workspace_root).is_some_and(|path| path != ".") {
        "workspace-file"
    } else {
        "media-file"
    };
    json!({ "resource": { "_tag": tag, "threadId": thread_id, "path": absolute_path } })
}

/// `AssetCreateUrlResult.relativeUrl`.
pub fn decode_asset_url(value: &Value) -> Result<String, String> {
    value
        .get("relativeUrl")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with('/'))
        .map(str::to_owned)
        .ok_or_else(|| "Unable to load workspace image.".to_owned())
}

fn decode_entry(value: &Value) -> Option<Entry> {
    let path = value.get("path")?.as_str()?.trim().trim_end_matches('/');
    let kind = match value.get("kind")?.as_str()? {
        "file" => EntryKind::File,
        "directory" => EntryKind::Directory,
        _ => return None,
    };
    if path.is_empty() {
        return None;
    }
    Some(Entry { path: path.to_owned(), kind, ignored: value.get("ignored").and_then(Value::as_bool).unwrap_or(false) })
}

fn decode_entries(value: &Value) -> Result<Listing, String> {
    let entries = value.get("entries").and_then(Value::as_array).ok_or_else(|| "The server sent no entries.".to_owned())?;
    Ok(Listing {
        entries: entries.iter().filter_map(decode_entry).collect(),
        truncated: value.get("truncated").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// `ProjectListEntriesResult`, keeping only `directory`'s direct children as
/// `useDirectoryEntries` does: an older server may answer with deeper entries.
pub fn decode_listing(value: &Value, directory: &str) -> Result<Listing, String> {
    let mut listing = decode_entries(value)?;
    listing.entries.retain(|entry| parent_directory(&entry.path) == directory);
    Ok(listing)
}

/// `ProjectSearchEntriesResult`.
pub fn decode_search(value: &Value) -> Result<Listing, String> {
    decode_entries(value)
}

/// `ProjectReadFileResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContents {
    pub relative_path: String,
    pub contents: String,
    pub byte_length: u64,
    /// The server read only the first `PROJECT_READ_FILE_MAX_BYTES` (1 MB).
    pub truncated: bool,
}

/// Takes the contents out of the reply instead of copying a megabyte.
pub fn decode_read_file(mut value: Value) -> Result<FileContents, String> {
    let contents = match value.get_mut("contents").map(Value::take) {
        Some(Value::String(contents)) => contents,
        _ => return Err("The server sent a file without contents.".to_owned()),
    };
    Ok(FileContents {
        relative_path: value.get("relativePath").and_then(Value::as_str).unwrap_or_default().to_owned(),
        byte_length: value.get("byteLength").and_then(Value::as_u64).unwrap_or(contents.len() as u64),
        truncated: value.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        contents,
    })
}

/// `ProjectFileFailure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFailure {
    OutsideRoot,
    ResolvedOutsideRoot,
    NotFile,
    Binary,
    OperationFailed,
}

/// `ProjectReadFileError`, or any other failure with only a message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadFailure {
    pub failure: Option<FileFailure>,
    pub operation: Option<String>,
    pub message: String,
    pub cwd: Option<String>,
    /// `resolvedPath ?? operationPath`: the path the server tried.
    pub attempted_path: Option<String>,
}

impl ReadFailure {
    pub fn message(message: impl Into<String>) -> ReadFailure {
        ReadFailure { message: message.into(), ..ReadFailure::default() }
    }

    /// From the encoded error of a failed RPC; anything but a `ProjectReadFileError` keeps only
    /// its message.
    pub fn from_error(error: &Value, message: &str) -> ReadFailure {
        let field = |name: &str| error.get(name).and_then(Value::as_str).map(str::to_owned);
        if error.get("_tag").and_then(Value::as_str) != Some("ProjectReadFileError") {
            return ReadFailure::message(message);
        }
        let failure = match error.get("failure").and_then(Value::as_str) {
            Some("workspace_path_outside_root") => Some(FileFailure::OutsideRoot),
            Some("resolved_path_outside_root") => Some(FileFailure::ResolvedOutsideRoot),
            Some("path_not_file") => Some(FileFailure::NotFile),
            Some("binary_file") => Some(FileFailure::Binary),
            Some("operation_failed") => Some(FileFailure::OperationFailed),
            _ => None,
        };
        ReadFailure {
            failure,
            operation: field("operation"),
            message: field("message").unwrap_or_else(|| message.to_owned()),
            cwd: field("cwd"),
            attempted_path: field("resolvedPath").or_else(|| field("operationPath")),
        }
    }

    /// `filePreviewReadErrorMessage`.
    pub fn describe(&self) -> String {
        match self.failure {
            Some(FileFailure::NotFile) => "The path is a directory or special file, not a regular file.".to_owned(),
            Some(FileFailure::Binary) => "The file is binary and cannot be displayed as text.".to_owned(),
            Some(FileFailure::OutsideRoot) => "The requested path is outside the workspace.".to_owned(),
            Some(FileFailure::ResolvedOutsideRoot) => "The path resolves to a location outside the workspace.".to_owned(),
            Some(FileFailure::OperationFailed) if self.operation.as_deref() == Some("realpath-workspace-root") => {
                "The workspace folder could not be accessed.".to_owned()
            }
            Some(FileFailure::OperationFailed) => {
                "The file could not be accessed or read. It may be missing or inaccessible.".to_owned()
            }
            None => self.message.clone(),
        }
    }
}

pub fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let trimmed = if trimmed.is_empty() { path } else { trimmed };
    trimmed.rfind(['/', '\\']).map_or(trimmed, |index| &trimmed[index + 1..])
}

/// `fileBreadcrumbParent` for any path: `""` for a top-level entry.
pub fn parent_directory(path: &str) -> &str {
    path.rfind('/').map_or("", |index| &path[..index])
}

/// Every directory above `path`, outermost first, starting with the root `""`
/// (`loadFileAncestors`).
pub fn ancestor_directories(path: &str) -> Vec<String> {
    let mut ancestors = vec![String::new()];
    let mut end = 0;
    while let Some(offset) = path[end..].find('/') {
        end += offset;
        ancestors.push(path[..end].to_owned());
        end += 1;
    }
    ancestors
}

fn is_windows_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'/' || bytes[2] == b'\\')
}

/// `isHostPath`: absolute on any platform, or under `~/`.
pub fn is_host_path(path: &str) -> bool {
    path.starts_with('/') || path.starts_with("\\\\") || path.starts_with("~/") || is_windows_drive_path(path)
}

/// `workspaceRelativeFilePath`: `path` relative to `root` with `/` separators, `"."` for the root
/// itself, `None` outside it. Windows roots compare case-insensitively.
pub fn workspace_relative_path(path: &str, root: &str) -> Option<String> {
    if root.is_empty() {
        return None;
    }
    let normalized_path = path.replace('\\', "/");
    let normalized_root = root.replace('\\', "/");
    let normalized_root = normalized_root.trim_end_matches('/');
    let case_insensitive = is_windows_drive_path(root) || root.starts_with("\\\\");
    let fold = |value: &str| if case_insensitive { value.to_lowercase() } else { value.to_owned() };
    let (path_cmp, root_cmp) = (fold(&normalized_path), fold(normalized_root));
    if path_cmp.trim_end_matches('/') == root_cmp {
        return Some(".".to_owned());
    }
    if !path_cmp.starts_with(&format!("{root_cmp}/")) {
        return None;
    }
    let relative = &normalized_path[normalized_root.len() + 1..];
    let mut depth = 0i32;
    for segment in relative.split('/') {
        match segment {
            ".." => depth -= 1,
            "" | "." => {}
            _ => depth += 1,
        }
        if depth < 0 {
            return None;
        }
    }
    Some(relative.trim_end_matches('/').to_owned())
}

/// The path a file tab keys on (`openFile` in `rightPanelStore.ts`): trailing separators go, a
/// bare drive root stays, and an absolute path inside the workspace becomes workspace-relative,
/// as callers resolve links before opening them. `None` means the workspace root itself.
pub fn file_surface_path(requested: &str, workspace_root: Option<&str>) -> Option<String> {
    let requested = requested.trim();
    if requested.is_empty() || requested == "." {
        return None;
    }
    if let Some(relative) = workspace_root.and_then(|root| workspace_relative_path(requested, root)) {
        return (relative != ".").then_some(relative);
    }
    let drive_root = requested.len() >= 3
        && requested.as_bytes()[0].is_ascii_alphabetic()
        && requested[1..].starts_with(':')
        && requested[2..].chars().all(|c| c == '/');
    if drive_root {
        return Some(requested.to_owned());
    }
    let trimmed = requested.trim_end_matches('/');
    Some(if trimmed.is_empty() { requested.to_owned() } else { trimmed.to_owned() })
}

/// `resolvePathLinkTarget` for a workspace path: the host path the asset route resolves.
pub fn absolute_path(relative_path: &str, workspace_root: &str) -> String {
    if is_host_path(relative_path) {
        return relative_path.to_owned();
    }
    let separator = if is_windows_drive_path(workspace_root) || workspace_root.starts_with("\\\\") { '\\' } else { '/' };
    let root = workspace_root.trim_end_matches(['/', '\\']);
    let relative = relative_path.trim_start_matches("./");
    if separator == '\\' { format!("{root}\\{}", relative.replace('/', "\\")) } else { format!("{root}/{relative}") }
}

/// `formatFilePathForClipboard`: Windows hosts get backslashes.
pub fn clipboard_path(path: &str, host_path: &str) -> String {
    if is_windows_drive_path(host_path) || host_path.starts_with("\\\\") { path.replace('/', "\\") } else { path.to_owned() }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |offset: usize| bytes.get(index + offset).and_then(|byte| (*byte as char).to_digit(16));
        match (bytes[index], hex(1), hex(2)) {
            (b'%', Some(high), Some(low)) => {
                decoded.push((high * 16 + low) as u8);
                index += 3;
            }
            (byte, ..) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// `splitFilePathPosition`: a `:line` or `:line:column` suffix, else a `#L12` or `#L12C3` hash.
fn split_position(path: &str, hash: &str) -> (String, Option<u64>) {
    let digits_suffix = |text: &str| -> Option<(usize, u64)> {
        let start = text.rfind(':')?;
        let digits = &text[start + 1..];
        (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())).then(|| (start, digits.parse().unwrap_or(0)))
    };
    if let Some((end, last)) = digits_suffix(path) {
        let (end, line) = match digits_suffix(&path[..end]) {
            Some((start, line)) => (start, line),
            None => (end, last),
        };
        return (path[..end].to_owned(), (line > 0).then_some(line));
    }
    let line = hash
        .strip_prefix("#L")
        .or_else(|| hash.strip_prefix("#l"))
        .map(|rest| rest.split(['C', 'c']).next().unwrap_or_default())
        .and_then(|digits| digits.parse::<u64>().ok())
        .filter(|line| *line > 0);
    (path.to_owned(), line)
}

/// What a markdown link to a file opens: its path and 1-based line, from a relative or absolute
/// path, a `file://` URL (`parseFileUrlHref`), with a `:line` suffix or `#L` hash. Web links and
/// other schemes are not files.
pub fn file_link_target(href: &str) -> Option<(String, Option<u64>)> {
    let href = href.trim();
    let (body, hash) = href.split_once('#').map_or((href, ""), |(body, hash)| (body, hash));
    let hash = if hash.is_empty() { String::new() } else { format!("#{hash}") };
    if let Some(rest) = body.strip_prefix("file://") {
        let (host, path) = rest.split_once('/').map_or((rest, ""), |(host, path)| (host, path));
        let path = if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
            let path = format!("/{path}");
            // `/C:/x` names a drive path.
            if path.len() > 3 && path.as_bytes()[2] == b':' && path.as_bytes()[1].is_ascii_alphabetic() { path[1..].to_owned() } else { path }
        } else {
            format!("\\\\{host}\\{}", path.replace('/', "\\"))
        };
        let (path, line) = split_position(&percent_decode(&path), &hash);
        return (!path.is_empty()).then_some((path, line));
    }
    let scheme = body.find(':').filter(|index| *index > 1 && body[..*index].bytes().all(|byte| byte.is_ascii_alphanumeric() || b"+.-".contains(&byte)));
    if scheme.is_some_and(|index| !body[index + 1..].bytes().all(|byte| byte.is_ascii_digit() || byte == b':')) || body.is_empty() {
        return None;
    }
    let (path, line) = split_position(&percent_decode(body), &hash);
    (!path.is_empty()).then_some((path, line))
}

/// One breadcrumb (`FileBreadcrumb`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    /// `""` is the workspace root.
    pub path: String,
    pub is_last: bool,
}

/// `fileBreadcrumbs`: the project, then each segment. Host paths have no project crumb.
pub fn breadcrumbs(project_name: &str, relative_path: &str) -> Vec<Crumb> {
    let host = is_host_path(relative_path);
    let windows = is_windows_drive_path(relative_path) || relative_path.starts_with("\\\\");
    let separator = if windows { "\\" } else { "/" };
    let root = if relative_path.starts_with("\\\\") {
        "\\\\"
    } else if relative_path.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = relative_path.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    let mut crumbs = Vec::with_capacity(parts.len() + 1);
    if !host {
        crumbs.push(Crumb { label: project_name.to_owned(), path: String::new(), is_last: parts.is_empty() });
    }
    for (index, part) in parts.iter().enumerate() {
        crumbs.push(Crumb {
            label: (*part).to_owned(),
            path: format!("{root}{}", parts[..=index].join(separator)),
            is_last: index + 1 == parts.len(),
        });
    }
    crumbs
}

fn extension(path: &str) -> Option<String> {
    let name = basename(path);
    let index = name.rfind('.')?;
    (index > 0 || name.len() > 1).then(|| name[index + 1..].to_ascii_lowercase())
}

/// `isMarkdownPreviewFile`.
pub fn is_markdown(path: &str) -> bool {
    matches!(extension(path).as_deref(), Some("md" | "mdx" | "markdown" | "mdown" | "mkd"))
}

/// The image kinds a preview decodes (a subset of `WORKSPACE_IMAGE_PREVIEW_EXTENSIONS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Raster,
    Svg,
    /// Previewable on the web through the browser's decoders (AVIF, HEIC); native has none.
    Unsupported,
}

pub fn image_kind(path: &str) -> Option<ImageKind> {
    match extension(path).as_deref()? {
        "bmp" | "gif" | "ico" | "jpeg" | "jpg" | "png" | "tif" | "tiff" | "webp" => Some(ImageKind::Raster),
        "svg" => Some(ImageKind::Svg),
        "avif" | "heic" | "heif" => Some(ImageKind::Unsupported),
        _ => None,
    }
}

/// The grammar a source preview highlights with, among the languages this build ships.
pub fn syntax_language(path: &str) -> Option<&'static str> {
    let name = basename(path).to_ascii_lowercase();
    match name.as_str() {
        ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" | ".envrc" => return Some("bash"),
        "cargo.lock" | "uv.lock" | "poetry.lock" => return Some("toml"),
        _ => {}
    }
    Some(match extension(path)?.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "json" | "jsonc" | "json5" | "jsonl" => "json",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        "py" | "pyi" => "python",
        "go" => "go",
        "sql" => "sql",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" | "zsh" => "bash",
        "diff" | "patch" => "diff",
        _ => return None,
    })
}

/// `maskMarkdownFrontmatter`: blanks a leading YAML frontmatter block (every character but line
/// breaks becomes a space) so a rendered preview skips it instead of drawing a rule and heading.
pub fn mask_frontmatter(markdown: &str) -> std::borrow::Cow<'_, str> {
    let body = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
    let is_delimiter = |line: &str, marks: &[&str]| marks.contains(&line.trim_end_matches([' ', '\t']));
    let mut lines = body.split_inclusive(['\n', '\r']);
    let Some(first) = lines.next() else { return markdown.into() };
    let first_content = first.trim_end_matches(['\n', '\r']);
    if first_content.len() == first.len() || !is_delimiter(first_content, &["---"]) {
        return markdown.into();
    }
    let mut end = markdown.len() - body.len() + first.len();
    for line in lines {
        let content = line.trim_end_matches(['\n', '\r']);
        // `\r\n` splits as `...\r` then `\n`; the bare `\n` is not a line of its own.
        if content.is_empty() && line == "\n" && markdown[..end].ends_with('\r') {
            end += 1;
            continue;
        }
        if is_delimiter(content, &["---", "..."]) {
            let block = end + content.len();
            let masked: String = markdown[..block].chars().map(|ch| if ch == '\n' || ch == '\r' { ch } else { ' ' }).collect();
            return format!("{masked}{}", &markdown[block..]).into();
        }
        end += line.len();
    }
    markdown.into()
}

/// `toLocaleString` for a count in English: `1,048,576`.
pub fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `encodeMarkdownLinkDestination`: `encodeURI`, then the characters that end a link early.
fn encode_link_destination(path: &str) -> String {
    const KEEP: &[u8] = b";,/?:@&=+$-_.!~*'()#";
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        let keep = byte.is_ascii_alphanumeric() || KEEP.contains(&byte);
        match byte {
            b'(' => encoded.push_str("%28"),
            b')' => encoded.push_str("%29"),
            b'#' => encoded.push_str("%23"),
            b'?' => encoded.push_str("%3F"),
            _ if keep => encoded.push(byte as char),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// `serializeComposerFileLink`: the mention "Add to chat" and "Copy mention" produce.
pub fn composer_file_link(path: &str) -> String {
    let label = basename(path).replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]");
    format!("[{label}]({})", encode_link_destination(path))
}

#[cfg(test)]
mod tests;
