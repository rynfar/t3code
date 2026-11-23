//! Adding projects: the browse-path helpers from `client-runtime/src/state/projects.ts` and
//! `filesystem.ts` (POSIX and Windows drive paths), and the `projects.mutate`,
//! `projects.createNew` and `filesystem.browse` payloads (`operations/commands.ts`).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::sidebar::grouping::normalize_project_path;

pub const PROJECTS_CREATE_FROM_THREAD: &str = "projects.createFromThread";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFromThreadResult { pub project_id: String }

pub fn create_from_thread(thread_id: &str) -> Value { json!({"threadId":thread_id}) }

pub const PROJECTS_MUTATE: &str = "projects.mutate";
pub const PROJECTS_CREATE_NEW: &str = "projects.createNew";
pub const PROJECTS_LIST_IGNORED_FILES: &str = "projects.listIgnoredFiles";
pub const PROJECTS_READ_FILE: &str = "projects.readFile";
pub const PROJECTS_WRITE_FILE: &str = "projects.writeFile";
pub const FILESYSTEM_BROWSE: &str = "filesystem.browse";
pub const PROJECT_CLONE_START: &str = "projectClone.start";
pub const PROJECT_CLONE_CANCEL: &str = "projectClone.cancel";
pub const PROJECT_CLONE_RETRY: &str = "projectClone.retry";
pub const SUBSCRIBE_PROJECT_CLONES: &str = "subscribeProjectClones";

/// Matches the server's read cap (`addProjectToMachine.ts`): a larger file would arrive truncated.
pub const COPYABLE_FILE_MAX_BYTES: u64 = 1024 * 1024;
/// The server lists at most this many ignored files (`addProjectToMachine.ts`).
pub const COPIED_FILES_MAX_COUNT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIgnoredFile {
    pub path: String,
    pub byte_length: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListIgnoredFilesResult {
    #[serde(default)]
    pub files: Vec<ProjectIgnoredFile>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReadFileResult {
    pub relative_path: String,
    pub contents: String,
    #[serde(default)]
    pub byte_length: u64,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectCloneStage {
    Connecting,
    Counting,
    Receiving,
    Resolving,
    Checkout,
    #[serde(other)]
    Unknown,
}

impl ProjectCloneStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting | Self::Unknown => "Connecting",
            Self::Counting => "Counting objects",
            Self::Receiving => "Receiving objects",
            Self::Resolving => "Resolving deltas",
            Self::Checkout => "Checking out files",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectClonePhase {
    Running,
    Done,
    Failed,
    Cancelled,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCloneRepositoryInfo {
    #[serde(default)]
    pub name_with_owner: String,
}

/// `ProjectCloneSnapshot` (`packages/contracts/src/projectClone.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCloneSnapshot {
    pub project_id: String,
    pub remote_url: String,
    pub destination_path: String,
    #[serde(default)]
    pub repository: Option<ProjectCloneRepositoryInfo>,
    pub phase: ProjectClonePhase,
    pub stage: ProjectCloneStage,
    #[serde(default)]
    pub percent: Option<u8>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub stalled: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub sequence: u64,
}

/// `projectCloneDisplayName` (`packages/contracts/src/projectClone.ts`).
pub fn project_clone_display_name(snapshot: &ProjectCloneSnapshot) -> String {
    if let Some(repo) = snapshot.repository.as_ref()
        && !repo.name_with_owner.trim().is_empty()
    {
        return repo.name_with_owner.clone();
    }
    snapshot
        .destination_path
        .split(['/', '\\'])
        .rfind(|segment| !segment.is_empty())
        .unwrap_or(&snapshot.destination_path)
        .to_owned()
}

/// `projectCloneProgressSummary` (`packages/contracts/src/projectClone.ts`).
pub fn project_clone_progress_summary(snapshot: &ProjectCloneSnapshot) -> String {
    let mut parts = vec![snapshot.stage.label().to_owned()];
    if let Some(percent) = snapshot.percent {
        parts.push(format!("{percent}%"));
    }
    if snapshot.stalled {
        parts.push("Stalled".to_owned());
    } else if let Some(detail) = snapshot.detail.as_deref().filter(|s| !s.is_empty()) {
        parts.push(detail.to_owned());
    }
    parts.join(" \u{00b7} ")
}

/// Live preview of a repository clone's URL, host, protocol alternatives, and destination path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClonePreview {
    pub remote_url: String,
    pub host: Option<String>,
    pub protocol: Option<CloneProtocol>,
    pub ssh_url: Option<String>,
    pub https_url: Option<String>,
    pub directory_path: String,
    pub destination_path: String,
    pub title: String,
    pub incomplete_hint: Option<&'static str>,
}

/// Computes URL normalization, host extraction, SSH/HTTPS conversion, and flat vs ghq
/// `<host>/<owner>/<repo>` destination preview for the interactive clone modal.
pub fn compute_clone_preview(
    raw_input: &str,
    base_directory: Option<&str>,
    fallback_root: Option<&str>,
    ghq_clone_layout: bool,
    preferred_protocol: Option<CloneProtocol>,
) -> ClonePreview {
    let trimmed = raw_input.trim();
    let incomplete_hint = github_incomplete_owner_repository_message("github", trimmed);
    let normalized = if trimmed.is_empty() {
        String::new()
    } else {
        normalize_pasted_clone_url(trimmed)
    };
    let ssh_url = clone_url_for_protocol(&normalized, CloneProtocol::Ssh);
    let https_url = clone_url_for_protocol(&normalized, CloneProtocol::Https);
    let detected_protocol = clone_url_protocol(&normalized);
    let effective_protocol = preferred_protocol.or(detected_protocol);
    let remote_url = match effective_protocol {
        Some(CloneProtocol::Ssh) => ssh_url.clone().unwrap_or_else(|| normalized.clone()),
        Some(CloneProtocol::Https) => https_url.clone().unwrap_or_else(|| normalized.clone()),
        None => normalized.clone(),
    };
    let host = parse_clone_url(&remote_url).map(|parsed| parsed.host);
    let title = clone_directory_name(&remote_url);
    let root = if ghq_clone_layout {
        add_project_initial_query(base_directory)
    } else {
        base_directory
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| fallback_root.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
            .unwrap_or_else(|| add_project_initial_query(None))
    };
    let directory_path = if remote_url.is_empty() {
        String::new()
    } else {
        clone_directory_path(&remote_url, ghq_clone_layout, None)
    };
    let destination_path = if directory_path.is_empty() {
        root
    } else {
        clone_destination_path(&root, &directory_path)
    };
    ClonePreview {
        remote_url,
        host,
        protocol: clone_url_protocol(&normalized).or(effective_protocol),
        ssh_url,
        https_url,
        directory_path,
        destination_path,
        title,
        incomplete_hint,
    }
}

/// `projectSubdirectory` (`AddProjectToMachineDialog.tsx`): path of a project below its repo root.
pub fn project_subdirectory(repository_root: Option<&str>, workspace_root: &str) -> String {
    let Some(repo_root) = repository_root else {
        return String::new();
    };
    let prefix = repo_root.trim_end_matches(['/', '\\']);
    if prefix.is_empty() || !workspace_root.starts_with(prefix) {
        return String::new();
    }
    let rest = &workspace_root[prefix.len()..];
    if rest.starts_with('/') || rest.starts_with('\\') {
        rest[1..].replace('\\', "/")
    } else {
        String::new()
    }
}

/// Default ignored files pre-selected by `AddProjectToMachineDialog`: `.env*`, `.envrc`, `.dev.vars`
/// within the 1 MB copy cap, bounded to `COPIED_FILES_MAX_COUNT`.
pub fn default_selected_ignored_files(files: &[ProjectIgnoredFile]) -> Vec<ProjectIgnoredFile> {
    files
        .iter()
        .filter(|file| file.byte_length <= COPYABLE_FILE_MAX_BYTES && is_default_copied_file(&file.path))
        .take(COPIED_FILES_MAX_COUNT)
        .cloned()
        .collect()
}

/// Human-readable file size (`apps/web/src/lib/formatBytes.ts`).
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return if kb >= 10.0 {
            format!("{kb:.0} KB")
        } else {
            format!("{kb:.1} KB")
        };
    }
    let mb = kb / 1024.0;
    format!("{mb:.1} MB")
}

pub fn extract_project_remote_url(repository_identity: Option<&Value>) -> Option<String> {
    let id = repository_identity?;
    id.pointer("/origin/remoteUrl")
        .or_else(|| id.pointer("/locator/remoteUrl"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub fn extract_repository_root_path(repository_identity: Option<&Value>) -> Option<String> {
    repository_identity?
        .get("rootPath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub fn extract_repository_display_name(repository_identity: Option<&Value>) -> Option<String> {
    repository_identity?
        .get("displayName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub fn list_ignored_files_request(cwd: &str, query: Option<&str>) -> Value {
    let mut payload = json!({ "cwd": cwd });
    if let Some(q) = query.map(str::trim).filter(|s| !s.is_empty()) {
        payload["query"] = json!(q);
    }
    payload
}

pub fn read_project_file_request(cwd: &str, relative_path: &str) -> Value {
    json!({ "cwd": cwd, "relativePath": relative_path })
}

pub fn write_project_file_request(cwd: &str, relative_path: &str, contents: &str) -> Value {
    json!({ "cwd": cwd, "relativePath": relative_path, "contents": contents })
}

pub fn cancel_project_clone_request(project_id: &str) -> Value {
    json!({ "projectId": project_id })
}

pub fn retry_project_clone_request(project_id: &str) -> Value {
    json!({ "projectId": project_id })
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseEntry {
    pub name: String,
    pub full_path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseResult {
    #[serde(default)]
    pub parent_path: String,
    #[serde(default)]
    pub entries: Vec<BrowseEntry>,
    #[serde(default)]
    pub path_missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateNewResult {
    pub project_id: String,
    pub workspace_root: String,
    #[serde(default)]
    pub commit_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCloneStartResult {
    pub project_id: String,
    pub cwd: String,
    pub remote_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneProtocol {
    Ssh,
    Https,
}

impl CloneProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            CloneProtocol::Ssh => "ssh",
            CloneProtocol::Https => "https",
        }
    }
}

struct ParsedCloneUrl {
    protocol: CloneProtocol,
    host: String,
    path: String,
    custom_port: bool,
    scp: bool,
}

fn parse_clone_url(url: &str) -> Option<ParsedCloneUrl> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        if path.is_empty() {
            return None;
        }
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
        let (host, custom_port) = split_host_port(host_port)?;
        return Some(ParsedCloneUrl {
            protocol: CloneProtocol::Ssh,
            host: host.to_owned(),
            path: path.to_owned(),
            custom_port,
            scp: false,
        });
    }
    if let Some(rest) = trimmed.strip_prefix("https://").or_else(|| trimmed.strip_prefix("http://")) {
        let (authority, path) = rest.split_once('/')?;
        if path.is_empty() {
            return None;
        }
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
        let (host, custom_port) = split_host_port(host_port)?;
        return Some(ParsedCloneUrl {
            protocol: CloneProtocol::Https,
            host: host.to_owned(),
            path: path.to_owned(),
            custom_port,
            scp: false,
        });
    }
    // SCP-like: `[user@]host:path` (not starting with `/` after `:`, not Windows drive)
    if is_windows_drive(trimmed) {
        return None;
    }
    let (left, path) = trimmed.split_once(':')?;
    if path.is_empty() || path.starts_with('/') || left.contains('/') {
        return None;
    }
    let host = left.rsplit_once('@').map_or(left, |(_, h)| h);
    if host.is_empty() || (!host.starts_with('[') && (host.contains('[') || host.contains(']'))) {
        return None;
    }
    if host.starts_with('[') && !host.ends_with(']') {
        return None;
    }
    Some(ParsedCloneUrl {
        protocol: CloneProtocol::Ssh,
        host: host.to_owned(),
        path: path.to_owned(),
        custom_port: false,
        scp: true,
    })
}

fn split_host_port(host_port: &str) -> Option<(&str, bool)> {
    if host_port.is_empty() {
        return None;
    }
    if host_port.starts_with('[') {
        let close = host_port.find(']')?;
        let host = &host_port[..=close];
        let tail = &host_port[close + 1..];
        if tail.is_empty() {
            return Some((host, false));
        }
        let port = tail.strip_prefix(':')?;
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            return Some((host, true));
        }
        return None;
    }
    if let Some((host, port)) = host_port.split_once(':') {
        if host.is_empty() || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        return Some((host, true));
    }
    Some((host_port, false))
}

pub fn clone_url_protocol(url: &str) -> Option<CloneProtocol> {
    parse_clone_url(url).map(|parsed| parsed.protocol)
}

/// Ports `cloneUrlForProtocol` from `packages/shared/src/cloneUrl.ts`.
pub fn clone_url_for_protocol(url: &str, protocol: CloneProtocol) -> Option<String> {
    let parsed = parse_clone_url(url)?;
    if parsed.protocol == protocol {
        return Some(url.trim().to_owned());
    }
    if parsed.custom_port {
        return None;
    }
    if parsed.protocol == CloneProtocol::Ssh && parsed.host.eq_ignore_ascii_case("ssh.dev.azure.com") {
        let clean = parsed.path.split(['?', '#']).next().unwrap_or("");
        let parts: Vec<&str> = clean.split('/').collect();
        if parts.len() == 4 && parts[0] == "v3" && parts.iter().all(|p| !p.is_empty()) {
            return Some(format!("https://dev.azure.com/{}/{}/_git/{}", parts[1], parts[2], parts[3]));
        }
        return None;
    }
    if parsed.protocol == CloneProtocol::Https && parsed.host.eq_ignore_ascii_case("dev.azure.com") {
        let clean = parsed.path.split(['?', '#']).next().unwrap_or("");
        let parts: Vec<&str> = clean.split('/').collect();
        if parts.len() == 4 && parts[2] == "_git" && parts.iter().all(|p| !p.is_empty()) {
            return Some(format!("git@ssh.dev.azure.com:v3/{}/{}/{}", parts[0], parts[1], parts[3]));
        }
        return None;
    }
    match protocol {
        CloneProtocol::Ssh => {
            let decoded = percent_decode(&parsed.path)?;
            Some(format!("git@{}:{decoded}", parsed.host))
        }
        CloneProtocol::Https => {
            let path = if parsed.scp {
                parsed.path.split('/').map(percent_encode_segment).collect::<Vec<_>>().join("/")
            } else {
                parsed.path
            };
            Some(format!("https://{}/{path}", parsed.host))
        }
    }
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn percent_encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn is_unsafe_segment(segment: &str) -> bool {
    if segment.is_empty() || segment == "." || segment == ".." {
        return true;
    }
    if segment.ends_with(' ') || segment.ends_with('.') {
        return true;
    }
    if segment.chars().any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return true;
    }
    is_windows_device_name(segment)
}

fn is_windows_device_name(segment: &str) -> bool {
    let base = segment.split('.').next().unwrap_or(segment).trim_end_matches(' ');
    let lower = base.to_ascii_lowercase();
    if matches!(lower.as_str(), "con" | "prn" | "aux" | "nul" | "conin$" | "conout$") {
        return true;
    }
    if let Some(tail) = lower.strip_prefix("com").or_else(|| lower.strip_prefix("lpt")) {
        let mut chars = tail.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return c.is_ascii_digit() || matches!(c, '¹' | '²' | '³');
        }
    }
    false
}

/// Ports `cloneUrlRepositoryPath` from `packages/shared/src/cloneUrl.ts`:
/// the `<host>/<owner>/<repo>` folders ghq clones a URL into, or `None` for local paths and
/// unsafe segments.
pub fn clone_url_repository_path(url: &str) -> Option<String> {
    if is_windows_drive(url.trim()) {
        return None;
    }
    let parsed = parse_clone_url(url)?;
    let path_without_query = parsed.path.split(['?', '#']).next().unwrap_or("");
    let mut segments: Vec<String> = Vec::new();
    segments.push(parsed.host);
    for segment in path_without_query.split('/').filter(|s| !s.is_empty()) {
        segments.push(segment.to_owned());
    }
    if let Some(last) = segments.last_mut()
        && let Some(stripped) = last.strip_suffix(".git")
    {
        *last = stripped.to_owned();
    }
    if segments.len() < 2 || segments.iter().any(|s| is_unsafe_segment(s)) {
        return None;
    }
    Some(segments.join("/"))
}

/// `getAddProjectInitialQuery` (`pingdotgg/t3code#37691`, `pingdotgg/t3code#38510`):
/// a bare relative setting such as `Desktop/dev` opens the picker under `~/`.
/// Folder browsers navigate `~/` but not `~\` home paths.
pub fn add_project_initial_query(base_directory: Option<&str>) -> String {
    let trimmed = base_directory.map(str::trim).unwrap_or("");
    if trimmed.is_empty() {
        return "~/".to_owned();
    }
    let is_explicit_relative = trimmed == "."
        || trimmed == ".."
        || trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with(".\\")
        || trimmed.starts_with("..\\");
    let is_anchored = trimmed == "~"
        || is_unix_absolute(trimmed)
        || is_windows_drive(trimmed)
        || trimmed.starts_with("\\\\")
        || trimmed.starts_with("~/")
        || is_explicit_relative;
    let anchored = if is_anchored {
        trimmed.to_owned()
    } else {
        format!("~/{trimmed}")
    };
    if has_trailing_separator(&anchored) {
        anchored
    } else {
        format!("{anchored}{}", preferred_separator(&anchored))
    }
}

/// `getCloneDirectoryName` (`packages/client-runtime/src/operations/projects.ts`).
pub fn clone_directory_name(repository_or_remote_url: &str) -> String {
    let without_query = repository_or_remote_url.split(['?', '#']).next().unwrap_or("").trim();
    let scheme_index = without_query.find("://");
    let has_host = scheme_index.is_some()
        || without_query.split_once(':').is_some_and(|(left, _)| {
            !left.contains('/') && !left.contains('\\') && left.contains('@')
        });
    let path_part = match scheme_index {
        Some(idx) => &without_query[idx + 3..],
        None => without_query,
    };
    let segments: Vec<&str> = path_part
        .split(['/', '\\', ':'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if has_host && segments.len() < 2 {
        return String::new();
    }
    let last = segments.last().copied().unwrap_or("");
    if has_host && segments.len() == 2 && !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) {
        return String::new();
    }
    last.strip_suffix(".git").unwrap_or(last).to_owned()
}

/// `getCloneDirectoryPath`: `<repo>`, or `<host>/<owner>/<repo>` like ghq.
pub fn clone_directory_path(remote_url: &str, ghq_clone_layout: bool, repository: Option<&str>) -> String {
    if ghq_clone_layout
        && let Some(ghq) = clone_url_repository_path(remote_url)
    {
        return ghq;
    }
    clone_directory_name(repository.unwrap_or(remote_url))
}

/// `getCloneDirectoryLeafName`.
pub fn clone_directory_leaf_name(remote_url: &str, ghq_clone_layout: bool, repository: Option<&str>) -> String {
    clone_directory_path(remote_url, ghq_clone_layout, repository)
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_owned()
}

/// `getCloneDestinationPath`.
pub fn clone_destination_path(directory_path: &str, directory_name: &str) -> String {
    let name = directory_name.trim();
    if name.is_empty() {
        return directory_path.to_owned();
    }
    let base = if has_trailing_separator(directory_path) {
        directory_path.to_owned()
    } else {
        format!("{directory_path}{}", preferred_separator(directory_path))
    };
    if base.ends_with('\\') {
        format!("{base}{}", name.replace('/', "\\"))
    } else {
        format!("{base}{name}")
    }
}

/// `getCloneDestinationBrowsePath`.
pub fn clone_destination_browse_path(
    browse_directory_path_str: &str,
    selected_directory_name: &str,
    clone_dir_name: &str,
    case_sensitive: bool,
) -> String {
    let selected_path = append_browse_segment(browse_directory_path_str, selected_directory_name);
    let matches = if case_sensitive {
        selected_directory_name == clone_dir_name
    } else {
        selected_directory_name.eq_ignore_ascii_case(clone_dir_name)
    };
    if matches {
        selected_path
    } else {
        clone_destination_path(&selected_path, clone_dir_name)
    }
}

/// `getCloneDestinationForPickedFolder`.
pub fn clone_destination_for_picked_folder(picked_path: &str, clone_dir_name: &str, case_sensitive: bool) -> String {
    match browse_parent_path(picked_path) {
        None => clone_destination_path(picked_path, clone_dir_name),
        Some(parent) => clone_destination_browse_path(&parent, &infer_project_title(picked_path), clone_dir_name, case_sensitive),
    }
}

fn is_repository_shorthand(input: &str) -> bool {
    let Some((owner, repo)) = input.split_once('/') else {
        return false;
    };
    if repo.contains('/') || owner.is_empty() || owner.len() > 39 {
        return false;
    }
    let owner_ok = owner.as_bytes()[0].is_ascii_alphanumeric()
        && owner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let repo_base = repo.strip_suffix(".git").unwrap_or(repo);
    let repo_ok = !repo_base.is_empty()
        && repo_base.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    owner_ok && repo_ok
}

/// `normalizePastedCloneUrl`: accepts `owner/repo` shorthand or full SSH/HTTPS clone URLs.
pub fn normalize_pasted_clone_url(input: &str) -> String {
    let trimmed = input.trim();
    if is_repository_shorthand(trimmed) {
        let path = if trimmed.ends_with(".git") {
            trimmed.to_owned()
        } else {
            format!("{trimmed}.git")
        };
        return format!("https://github.com/{path}");
    }
    trimmed.to_owned()
}

/// Whether a typed Add Project query looks like a git clone URL or `owner/repo` shorthand.
pub fn is_clone_query(input: &str) -> bool {
    let trimmed = input.trim();
    !is_browse_query(trimmed, true)
        && (is_repository_shorthand(trimmed) || parse_clone_url(trimmed).is_some())
}

/// `githubIncompleteOwnerRepositoryMessage`.
pub fn github_incomplete_owner_repository_message(source: &str, input: &str) -> Option<&'static str> {
    if source != "github" {
        return None;
    }
    let trimmed = input.trim();
    let owner = trimmed.trim_end_matches('/');
    if owner.len() == trimmed.len() || owner.is_empty() || owner.len() > 39 || owner.contains('/') {
        return None;
    }
    let valid = owner.as_bytes()[0].is_ascii_alphanumeric()
        && owner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    valid.then_some("Enter the full owner/repository name.")
}

/// `isDefaultCopiedFile` (`addProjectToMachine.ts`).
pub fn is_default_copied_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name == ".env" || name.starts_with(".env.") || name == ".envrc" || name == ".dev.vars"
}

/// `isExplicitRelativeProjectPath`.
pub fn is_explicit_relative_project_path(raw: &str) -> bool {
    raw == "." || raw == ".." || raw.starts_with("./") || raw.starts_with("../") || raw.starts_with(".\\") || raw.starts_with("..\\")
}

/// `isUnsupportedWindowsProjectPath`.
pub fn is_unsupported_windows_project_path(raw: &str, platform: &str) -> bool {
    !matches!(platform, "win32" | "windows") && (is_windows_drive(raw) || raw.starts_with("\\\\"))
}

struct AbsolutePathParts {
    root: String,
    separator: char,
    segments: Vec<String>,
}

fn split_absolute_path(value: &str) -> Option<AbsolutePathParts> {
    if is_windows_drive(value) {
        let root = format!("{}\\", &value[..2]);
        let rest = value[2..].trim_start_matches(['/', '\\']);
        let segments = rest.split(['/', '\\']).filter(|s| !s.is_empty()).map(str::to_owned).collect();
        return Some(AbsolutePathParts { root, separator: '\\', segments });
    }
    if value.starts_with("\\\\") {
        let parts: Vec<&str> = value.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        let (Some(server), Some(share)) = (parts.first(), parts.get(1)) else {
            return None;
        };
        return Some(AbsolutePathParts {
            root: format!("\\\\{server}\\{share}\\"),
            separator: '\\',
            segments: parts[2..].iter().map(|s| (*s).to_owned()).collect(),
        });
    }
    if let Some(rest) = value.strip_prefix('/') {
        let segments = rest.split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect();
        return Some(AbsolutePathParts { root: "/".to_owned(), separator: '/', segments });
    }
    None
}

/// `resolveAddProjectPath` / `resolveProjectPathForDispatch` (`packages/client-runtime/src/state/projects.ts`).
pub fn resolve_add_project_path(raw_path: &str, current_project_cwd: Option<&str>, platform: &str) -> Result<String, &'static str> {
    let raw = raw_path.trim();
    if raw.is_empty() {
        return Err("Enter a project path.");
    }
    if is_unsupported_windows_project_path(raw, platform) {
        return Err("Windows-style paths are only supported on Windows environments.");
    }
    if is_explicit_relative_project_path(raw) && current_project_cwd.is_none() {
        return Err("Relative paths require an active project in this environment.");
    }
    if is_explicit_relative_project_path(raw)
        && let Some(cwd) = current_project_cwd
    {
        let normalized_cwd = add_target(cwd);
        if let Some(base) = split_absolute_path(&normalized_cwd) {
            let mut next_segments = base.segments;
            for segment in raw.split(['/', '\\']) {
                if segment.is_empty() || segment == "." {
                    continue;
                }
                if segment == ".." {
                    next_segments.pop();
                    continue;
                }
                next_segments.push(segment.to_owned());
            }
            let sep = base.separator.to_string();
            let joined = next_segments.join(&sep);
            let combined = if joined.is_empty() {
                base.root
            } else {
                format!("{}{joined}", base.root)
            };
            return Ok(add_target(&combined));
        }
    }
    let resolved = add_target(raw);
    if resolved.is_empty() {
        Err("Enter a project path.")
    } else {
        Ok(resolved)
    }
}

/// `projectClone.start` payload (`ProjectCloneStartInput`).
pub fn start_project_clone(
    project_id: &str,
    title: &str,
    created_at: &str,
    remote_url: &str,
    destination_path: &str,
) -> Value {
    json!({
        "projectId": project_id,
        "title": title,
        "createdAt": created_at,
        "remoteUrl": remote_url,
        "destinationPath": destination_path,
    })
}

fn is_bare_windows_drive(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_windows_drive(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes.len() == 2 || matches!(bytes[2], b'/' | b'\\'))
}

fn is_unix_absolute(value: &str) -> bool {
    value.starts_with('/')
}

/// `isFilesystemBrowseQuery`: a path-shaped query switches the palette to browsing.
pub fn is_browse_query(value: &str, windows: bool) -> bool {
    ["./", "../", ".\\", "..\\", "/", "~/"].iter().any(|prefix| value.starts_with(prefix)) || (windows && (is_windows_drive(value) || value.starts_with("\\\\")))
}

pub fn has_trailing_separator(value: &str) -> bool {
    if is_unix_absolute(value) { value.ends_with('/') } else { value.ends_with(['/', '\\']) }
}

fn last_separator(value: &str) -> Option<usize> {
    if is_unix_absolute(value) { value.rfind('/') } else { value.rfind(['/', '\\']) }
}

fn preferred_separator(value: &str) -> char {
    if is_windows_drive(value) || value.starts_with("\\\\") {
        '\\'
    } else if is_unix_absolute(value) || !value.contains('\\') {
        '/'
    } else {
        '\\'
    }
}

/// `getBrowseDirectoryPath`: the folder part of a typed path, with its trailing separator.
pub fn browse_directory_path(value: &str) -> &str {
    if has_trailing_separator(value) {
        return value;
    }
    match last_separator(value) {
        Some(index) => &value[..=index],
        None => value,
    }
}

/// `getBrowseLeafPathSegment`: what is typed after the last separator.
pub fn browse_leaf(value: &str) -> &str {
    last_separator(value).map_or(value, |index| &value[index + 1..])
}

/// `appendBrowsePathSegment`: enter a folder.
pub fn append_browse_segment(current: &str, segment: &str) -> String {
    if is_bare_windows_drive(current) {
        return format!("{current}\\{segment}\\");
    }
    format!("{}{segment}{}", browse_directory_path(current), preferred_separator(current))
}

/// `getBrowseParentPath` for POSIX paths: `None` at the root.
pub fn browse_parent_path(current: &str) -> Option<String> {
    let trimmed = current.trim().trim_end_matches('/');
    if !current.trim().starts_with('/') {
        let index = trimmed.rfind(['/', '\\'])?;
        return Some(trimmed[..=index].to_owned());
    }
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    match segments.len() {
        0 => None,
        1 => Some("/".to_owned()),
        n => Some(format!("/{}/", segments[..n - 1].join("/"))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowsePath {
    pub directory: String,
    pub filter: String,
}

/// `getFilesystemBrowsePath`: the folder to list and the leaf to filter its entries by.
pub fn browse_path(query: &str, windows: bool) -> Option<BrowsePath> {
    is_browse_query(query, windows).then(|| {
        if windows && is_bare_windows_drive(query) {
            return BrowsePath { directory: format!("{query}\\"), filter: String::new() };
        }
        BrowsePath {
            directory: browse_directory_path(query).to_owned(),
            filter: if has_trailing_separator(query) { String::new() } else { browse_leaf(query).to_owned() },
        }
    })
}

fn score_browse_subsequence(value: &str, query: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    let v: Vec<char> = value.chars().collect();
    let q: Vec<char> = query.chars().collect();
    let mut qi = 0usize;
    let mut first_match: Option<usize> = None;
    let mut prev_match: Option<usize> = None;
    let mut gap_penalty = 0usize;
    for (vi, &ch) in v.iter().enumerate() {
        if ch != q[qi] {
            continue;
        }
        if first_match.is_none() {
            first_match = Some(vi);
        }
        if let Some(prev) = prev_match {
            gap_penalty += vi - prev - 1;
        }
        prev_match = Some(vi);
        qi += 1;
        if qi == q.len() {
            let first = first_match.unwrap_or(0);
            let span_penalty = (vi - first + 1).saturating_sub(q.len());
            let length_penalty = v.len().saturating_sub(q.len()).min(64);
            return Some(first * 2 + gap_penalty * 3 + span_penalty + length_penalty);
        }
    }
    None
}

fn score_browse_query_match(value: &str, query: &str) -> Option<usize> {
    if value.is_empty() || query.is_empty() {
        return None;
    }
    let v_len = value.chars().count();
    let q_len = query.chars().count();
    let penalty = v_len.saturating_sub(q_len).min(64);
    if value == query {
        return Some(0);
    }
    if value.starts_with(query) {
        return Some(1_000 + penalty);
    }
    let mut best_boundary: Option<usize> = None;
    for marker in [' ', '-', '_', '.'] {
        let needle = format!("{marker}{query}");
        if let Some(byte_idx) = value.find(&needle) {
            let char_idx = value[..byte_idx].chars().count() + 1;
            best_boundary = Some(best_boundary.map_or(char_idx, |best| best.min(char_idx)));
        }
    }
    if let Some(boundary_idx) = best_boundary {
        return Some(2_000 + boundary_idx * 2 + penalty);
    }
    if let Some(byte_idx) = value.find(query) {
        let char_idx = value[..byte_idx].chars().count();
        return Some(3_000 + char_idx * 2 + penalty);
    }
    score_browse_subsequence(value, query).map(|fuzzy| 4_000 + fuzzy)
}

/// `filterFilesystemBrowseEntries` (`pingdotgg/t3code#26376`): matches exact, prefix, word
/// boundary, substring, and fuzzy subsequence matches; hidden folders appear only when the leaf
/// starts with a dot.
pub fn filter_browse_entries<'a>(entries: &'a [BrowseEntry], filter: &str) -> Vec<&'a BrowseEntry> {
    use unicode_normalization::UnicodeNormalization;
    let lower_query = filter.to_lowercase();
    let lower_nfc_query = filter.nfc().collect::<String>().to_lowercase();
    let show_hidden = filter.starts_with('.');
    let candidates: Vec<&'a BrowseEntry> = entries
        .iter()
        .filter(|entry| show_hidden || !entry.name.starts_with('.'))
        .collect();
    if lower_query.is_empty() {
        return candidates;
    }
    let mut scored: Vec<(&'a BrowseEntry, usize)> = candidates
        .into_iter()
        .filter_map(|entry| {
            let raw_lower = entry.name.to_lowercase();
            let nfc_lower = entry.name.nfc().collect::<String>().to_lowercase();
            let raw_score = score_browse_query_match(&raw_lower, &lower_query);
            let nfc_score = score_browse_query_match(&nfc_lower, &lower_nfc_query);
            let score = match (raw_score, nfc_score) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }?;
            Some((entry, score))
        })
        .collect();
    scored.sort_by_key(|(_, score)| *score);
    scored.into_iter().map(|(entry, _)| entry).collect()
}

/// `getFilesystemBrowseTabCompletion` (`pingdotgg/t3code#37925`): considers only prefix-matching
/// entries so Tab still completes a folder name when the visible list includes fuzzy matches.
pub fn browse_tab_completion(visible: &[&BrowseEntry], filter: &str) -> Option<(String, bool)> {
    use unicode_normalization::UnicodeNormalization;
    let lower_query = filter.to_lowercase();
    let lower_nfc_query = filter.nfc().collect::<String>().to_lowercase();
    let prefix_entries: Vec<&BrowseEntry> = visible
        .iter()
        .copied()
        .filter(|entry| {
            entry.name.to_lowercase().starts_with(&lower_query)
                || entry.name.nfc().collect::<String>().to_lowercase().starts_with(&lower_nfc_query)
        })
        .collect();
    let (first, rest) = prefix_entries.split_first()?;
    if rest.is_empty() {
        return Some((first.name.clone(), true));
    }
    let mut prefix: Vec<char> = first.name.chars().collect();
    for entry in rest {
        let chars: Vec<char> = entry.name.chars().collect();
        let common = prefix.iter().zip(&chars).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase())).count();
        prefix.truncate(common);
    }
    let name: String = prefix.into_iter().collect();
    let lower = name.to_lowercase();
    (lower.chars().count() > lower_query.chars().count()
        && prefix_entries.iter().all(|entry| entry.name.to_lowercase().starts_with(&lower)))
    .then_some((name, false))
}

/// `inferProjectTitleFromPath`: the last path segment.
pub fn infer_project_title(path: &str) -> String {
    let trimmed = path.trim();
    trimmed.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).find(|segment| !segment.is_empty()).unwrap_or(trimmed).to_owned()
}

/// `findProjectByPath`.
pub fn find_project_by_path<'a, T>(projects: &'a [T], path: &str, root: impl Fn(&T) -> &str) -> Option<&'a T> {
    let candidate = normalize_project_path(path);
    if candidate.is_empty() {
        return None;
    }
    projects.iter().find(|project| normalize_project_path(root(project)) == candidate)
}

/// The path a browse query adds: the typed folder without its trailing separator.
pub fn add_target(query: &str) -> String {
    let trimmed = query.trim();
    if trimmed.len() > 1 { trimmed.trim_end_matches(['/', '\\']).to_owned() } else { trimmed.to_owned() }
}

pub fn browse_request(partial_path: &str, cwd: Option<&str>) -> Value {
    let mut payload = json!({ "partialPath": partial_path });
    if let Some(cwd) = cwd {
        payload["cwd"] = json!(cwd);
    }
    payload
}

pub fn create_project(command_id: &str, project_id: &str, title: &str, workspace_root: &str) -> Value {
    json!({
        "type": "project.create",
        "commandId": command_id,
        "projectId": project_id,
        "title": title,
        "workspaceRoot": workspace_root,
        "createWorkspaceRootIfMissing": true,
        "defaultModelSelection": Value::Null,
    })
}

/// `project.update` with `pinned`; older servers without `projectOrganization` drop it.
pub fn set_project_pinned(command_id: &str, project_id: &str, pinned: bool) -> Value {
    json!({ "type": "project.update", "commandId": command_id, "projectId": project_id, "pinned": pinned })
}

pub fn create_new_project(name: &str) -> Value {
    json!({ "name": name.trim() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> BrowseEntry {
        BrowseEntry { name: name.into(), full_path: format!("/w/{name}") }
    }

    #[test]
    fn browse_queries_split_into_folder_and_leaf() {
        assert!(is_browse_query("~/code", false));
        assert!(is_browse_query("../x", false));
        assert!(!is_browse_query("code", false));
        assert!(!is_browse_query("C:\\code", false));
        assert!(is_browse_query("C:\\code", true));
        assert_eq!(browse_path("~/code/t3", false), Some(BrowsePath { directory: "~/code/".into(), filter: "t3".into() }));
        assert_eq!(browse_path("/Users/me/", false), Some(BrowsePath { directory: "/Users/me/".into(), filter: String::new() }));
        assert_eq!(append_browse_segment("~/code/t", "t3code"), "~/code/t3code/");
        assert_eq!(browse_parent_path("/Users/me/code/"), Some("/Users/me/".into()));
        assert_eq!(browse_parent_path("/Users/"), Some("/".into()));
        assert_eq!(browse_parent_path("/"), None);
        assert_eq!(browse_leaf("C:\\code\\t3"), "t3");
        assert_eq!(browse_path("D:", true), Some(BrowsePath { directory: "D:\\".into(), filter: String::new() }));
        assert_eq!(append_browse_segment("D:", "code"), "D:\\code\\");
    }

    #[test]
    fn entries_filter_by_prefix_and_hide_dotfiles() {
        let entries = [entry("src"), entry(".git"), entry("Scripts"), entry("docs")];
        let names = |filter: &str| filter_browse_entries(&entries, filter).iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("s"), ["src", "Scripts", "docs"]);
        assert_eq!(names("sc"), ["Scripts", "src"]);
        assert_eq!(names("scr"), ["Scripts"]);
        assert_eq!(names(""), ["src", "Scripts", "docs"]);
        assert_eq!(names("."), [".git"]);
    }

    #[test]
    fn tab_completion_enters_one_match_and_extends_several() {
        let entries = [entry("packages"), entry("package-lock"), entry("docs")];
        let visible = filter_browse_entries(&entries, "p");
        assert_eq!(browse_tab_completion(&visible, "p"), Some(("package".into(), false)));
        let one = filter_browse_entries(&entries, "d");
        assert_eq!(browse_tab_completion(&one, "d"), Some(("docs".into(), true)));
        let done = filter_browse_entries(&entries, "package");
        assert_eq!(browse_tab_completion(&done, "package"), None);
    }

    #[test]
    fn titles_targets_and_commands() {
        assert_eq!(infer_project_title("/Users/me/code/t3code/"), "t3code");
        assert_eq!(infer_project_title("~/x"), "x");
        assert_eq!(add_target("~/code/t3code/"), "~/code/t3code");
        assert_eq!(add_target("/"), "/");
        let roots = ["/w/a".to_owned(), "/w/b".to_owned()];
        assert_eq!(find_project_by_path(&roots, "/w/b/", |p| p.as_str()), Some(&roots[1]));
        let cmd = create_project("c", "p", "T", "/w");
        assert_eq!(cmd["type"], "project.create");
        assert_eq!(cmd["createWorkspaceRootIfMissing"], true);
        assert_eq!(set_project_pinned("c", "p", true)["pinned"], true);
        assert_eq!(create_new_project("  Demo "), json!({"name": "Demo"}));
        assert_eq!(browse_request("~/", None), json!({"partialPath": "~/"}));
    }

    #[test]
    fn ghq_clone_layout_and_protocol_conversion() {
        assert_eq!(
            clone_url_repository_path("https://github.com/pingdotgg/t3code.git"),
            Some("github.com/pingdotgg/t3code".into())
        );
        assert_eq!(
            clone_url_repository_path("git@github.com:pingdotgg/t3code.git"),
            Some("github.com/pingdotgg/t3code".into())
        );
        assert_eq!(
            clone_url_repository_path("ssh://git@gitlab.com:2222/group/sub/repo.git"),
            Some("gitlab.com/group/sub/repo".into())
        );
        assert_eq!(clone_url_repository_path("https://github.com/pingdotgg/../escape.git"), None);
        assert_eq!(clone_url_repository_path("https://github.com/owner/con.git"), None);
        assert_eq!(clone_url_repository_path("C:\\repos\\t3code"), None);

        assert_eq!(
            clone_directory_path("https://github.com/pingdotgg/t3code.git", true, None),
            "github.com/pingdotgg/t3code"
        );
        assert_eq!(
            clone_directory_path("https://github.com/pingdotgg/t3code.git", false, None),
            "t3code"
        );
        assert_eq!(
            clone_directory_leaf_name("https://github.com/pingdotgg/t3code.git", true, None),
            "t3code"
        );
        assert_eq!(
            clone_destination_path("~/code", "github.com/pingdotgg/t3code"),
            "~/code/github.com/pingdotgg/t3code"
        );
        assert_eq!(
            clone_destination_path("C:\\code\\", "github.com/pingdotgg/t3code"),
            "C:\\code\\github.com\\pingdotgg\\t3code"
        );
        assert_eq!(
            clone_url_for_protocol("https://github.com/pingdotgg/t3code.git", CloneProtocol::Ssh),
            Some("git@github.com:pingdotgg/t3code.git".into())
        );
        assert_eq!(
            clone_url_for_protocol("git@github.com:pingdotgg/t3code.git", CloneProtocol::Https),
            Some("https://github.com/pingdotgg/t3code.git".into())
        );
        assert_eq!(
            clone_url_for_protocol("git@ssh.dev.azure.com:v3/org/proj/repo", CloneProtocol::Https),
            Some("https://dev.azure.com/org/proj/_git/repo".into())
        );
        assert_eq!(
            clone_url_for_protocol("https://dev.azure.com/org/proj/_git/repo", CloneProtocol::Ssh),
            Some("git@ssh.dev.azure.com:v3/org/proj/repo".into())
        );
        assert_eq!(clone_url_for_protocol("ssh://git@github.com:2222/a/b.git", CloneProtocol::Https), None);
    }

    #[test]
    fn clone_queries_path_validation_and_browse_missing_flag() {
        assert!(is_clone_query("pingdotgg/t3code"));
        assert!(is_clone_query("https://github.com/pingdotgg/t3code.git"));
        assert!(is_clone_query("git@github.com:pingdotgg/t3code.git"));
        assert!(!is_clone_query("~/code/t3code"));
        assert!(!is_clone_query("/Users/me/t3code"));
        assert_eq!(normalize_pasted_clone_url("pingdotgg/t3code"), "https://github.com/pingdotgg/t3code.git");
        assert_eq!(github_incomplete_owner_repository_message("github", "pingdotgg/"), Some("Enter the full owner/repository name."));
        assert_eq!(github_incomplete_owner_repository_message("github", "pingdotgg/t3code"), None);
        assert!(is_default_copied_file(".env.local"));
        assert!(is_default_copied_file("apps/web/.envrc"));
        assert!(!is_default_copied_file("README.md"));

        assert_eq!(resolve_add_project_path("", None, "darwin"), Err("Enter a project path."));
        assert_eq!(
            resolve_add_project_path("C:\\repo", None, "darwin"),
            Err("Windows-style paths are only supported on Windows environments.")
        );
        assert_eq!(
            resolve_add_project_path("./sub", None, "darwin"),
            Err("Relative paths require an active project in this environment.")
        );
        assert_eq!(resolve_add_project_path("./sub", Some("/work/app"), "darwin"), Ok("/work/app/sub".into()));
        assert_eq!(resolve_add_project_path("../sibling", Some("/work/app"), "darwin"), Ok("/work/sibling".into()));
        assert_eq!(resolve_add_project_path("./", Some("/work/app"), "darwin"), Ok("/work/app".into()));
        assert_eq!(resolve_add_project_path("..", Some("/work/app"), "darwin"), Ok("/work".into()));
        assert_eq!(resolve_add_project_path("./sub", Some("/"), "darwin"), Ok("/sub".into()));
        assert_eq!(resolve_add_project_path("..\\sibling", Some("C:\\work\\app"), "win32"), Ok("C:\\work\\sibling".into()));
        let browse: BrowseResult = serde_json::from_value(json!({
            "parentPath": "~/code/github.com/pingdotgg",
            "entries": [],
            "pathMissing": true
        }))
        .unwrap();
        assert!(browse.path_missing);
    }

    #[test]
    fn clone_preview_progress_summary_and_ignored_file_relay_helpers() {
        let ghq = compute_clone_preview("pingdotgg/t3code", Some("~/src"), Some("~/projects"), true, None);
        assert_eq!(ghq.remote_url, "https://github.com/pingdotgg/t3code.git");
        assert_eq!(ghq.host.as_deref(), Some("github.com"));
        assert_eq!(ghq.ssh_url.as_deref(), Some("git@github.com:pingdotgg/t3code.git"));
        assert_eq!(ghq.https_url.as_deref(), Some("https://github.com/pingdotgg/t3code.git"));
        assert_eq!(ghq.directory_path, "github.com/pingdotgg/t3code");
        assert_eq!(ghq.destination_path, "~/src/github.com/pingdotgg/t3code");
        assert_eq!(ghq.title, "t3code");

        let ssh_flat = compute_clone_preview(
            "https://github.com/pingdotgg/t3code.git",
            None,
            Some("~/projects"),
            false,
            Some(CloneProtocol::Ssh),
        );
        assert_eq!(ssh_flat.remote_url, "git@github.com:pingdotgg/t3code.git");
        assert_eq!(ssh_flat.destination_path, "~/projects/t3code");

        let snap: ProjectCloneSnapshot = serde_json::from_value(json!({
            "projectId": "p1",
            "remoteUrl": "https://github.com/pingdotgg/t3code.git",
            "destinationPath": "/src/github.com/pingdotgg/t3code",
            "repository": { "nameWithOwner": "pingdotgg/t3code" },
            "phase": "running",
            "stage": "receiving",
            "percent": 64,
            "detail": "12.4 MiB/s",
            "stalled": false,
            "error": null,
            "sequence": 3
        }))
        .unwrap();
        assert_eq!(project_clone_display_name(&snap), "pingdotgg/t3code");
        assert_eq!(project_clone_progress_summary(&snap), "Receiving objects · 64% · 12.4 MiB/s");

        let mut stalled = snap.clone();
        stalled.stalled = true;
        assert_eq!(project_clone_progress_summary(&stalled), "Receiving objects · 64% · Stalled");

        assert_eq!(project_subdirectory(Some("/repo"), "/repo/apps/web"), "apps/web");
        assert_eq!(project_subdirectory(Some("/repo/"), "/repo"), "");
        assert_eq!(project_subdirectory(Some("/repo"), "/other/apps/web"), "");

        let files = vec![
            ProjectIgnoredFile { path: ".env.local".into(), byte_length: 256 },
            ProjectIgnoredFile { path: "apps/web/.env".into(), byte_length: COPYABLE_FILE_MAX_BYTES + 1 },
            ProjectIgnoredFile { path: "notes.txt".into(), byte_length: 64 },
            ProjectIgnoredFile { path: ".envrc".into(), byte_length: 32 },
        ];
        let defaults = default_selected_ignored_files(&files);
        let paths: Vec<&str> = defaults.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec![".env.local", ".envrc"]);
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(
            list_ignored_files_request("/repo", Some("env")),
            json!({ "cwd": "/repo", "query": "env" })
        );
        assert_eq!(
            write_project_file_request("/dst", "apps/web/.env", "KEY=1"),
            json!({ "cwd": "/dst", "relativePath": "apps/web/.env", "contents": "KEY=1" })
        );
    }

    #[test]
    fn add_project_initial_query_anchors_bare_relative_under_home_and_skips_backslash_home() {
        assert_eq!(add_project_initial_query(None), "~/");
        assert_eq!(add_project_initial_query(Some("")), "~/");
        assert_eq!(add_project_initial_query(Some("Desktop/dev")), "~/Desktop/dev/");
        assert_eq!(add_project_initial_query(Some("~")), "~/");
        assert_eq!(add_project_initial_query(Some("~/code")), "~/code/");
        assert_eq!(add_project_initial_query(Some("./local")), "./local/");
        assert_eq!(add_project_initial_query(Some("/var/repos")), "/var/repos/");
        // Folder browsers do not treat `~\` as a home prefix (`pingdotgg/t3code#38510`).
        assert_eq!(add_project_initial_query(Some("~\\work")), "~/~\\work\\");
    }

    #[test]
    fn browse_filter_matches_substrings_and_fuzzy_and_tab_completes_prefixes_only() {
        let entries = vec![
            BrowseEntry { name: "zebra-pkg".into(), full_path: "/zebra-pkg".into() },
            BrowseEntry { name: "packages".into(), full_path: "/packages".into() },
            BrowseEntry { name: "pack-tools".into(), full_path: "/pack-tools".into() },
            BrowseEntry { name: "alpha_pack".into(), full_path: "/alpha_pack".into() },
            BrowseEntry { name: "p_a_c_k".into(), full_path: "/p_a_c_k".into() },
            BrowseEntry { name: ".pack-hidden".into(), full_path: "/.pack-hidden".into() },
        ];
        // Substring, boundary, and fuzzy matches all surface in tiered order (`pingdotgg/t3code#26376`).
        let visible = filter_browse_entries(&entries, "pack");
        let names: Vec<&str> = visible.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names[0], "packages");
        assert_eq!(names[1], "pack-tools");
        assert!(names.contains(&"alpha_pack"));
        assert!(names.contains(&"p_a_c_k"));
        assert!(!names.contains(&".pack-hidden"));
        assert!(!names.contains(&"zebra-pkg"));

        // Tab completion ignores non-prefix fuzzy matches (`pingdotgg/t3code#37925`).
        let docs_entries = [
            BrowseEntry { name: "my-docs".into(), full_path: "/my-docs".into() },
            BrowseEntry { name: "docs".into(), full_path: "/docs".into() },
        ];
        let single_prefix = filter_browse_entries(&docs_entries, "do");
        assert_eq!(single_prefix.len(), 2);
        assert_eq!(browse_tab_completion(&single_prefix, "do"), Some(("docs".into(), true)));
    }
}
