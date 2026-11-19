//! File link targets: `parseFileUrlHref`, `splitFilePathPosition`, `formatFilePathPosition` and
//! `resolvePathLinkTarget` from `packages/shared/src/fileLinks.ts`, with the path predicates of
//! `packages/shared/src/path.ts` they rely on.

use std::sync::LazyLock;

use regex::Regex;

static POSITION_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":(\d+)(?::(\d+)|-(\d+))?$").expect("valid regex"));
static POSITION_HASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^#L(\d+)(?:C(\d+)|-L?(\d+))?$").expect("valid regex"));

/// `FilePathPosition`: a path and the 1-based place an editor opens it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePathPosition {
    pub path: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
    /// The last line of a range such as `:42-64`. Editors open at `line`.
    pub end_line: Option<u64>,
}

/// `splitFilePathPosition`: a trailing `:line[:col]` or `:line-end`, else a `#L..` hash.
pub fn split_file_path_position(path: &str, hash: &str) -> FilePathPosition {
    let plain = || FilePathPosition { path: path.to_owned(), line: None, column: None, end_line: None };
    let (captures, from_suffix) = match POSITION_SUFFIX.captures(path) {
        Some(captures) => (captures, true),
        None => match POSITION_HASH.captures(hash) {
            Some(captures) => (captures, false),
            None => return plain(),
        },
    };
    let number = |index: usize| captures.get(index).and_then(|m| m.as_str().parse::<u64>().ok());
    let Some(line) = number(1) else { return plain() };
    let stripped = if from_suffix { &path[..captures.get(0).map_or(path.len(), |m| m.start())] } else { path };
    FilePathPosition {
        path: stripped.to_owned(),
        line: (line > 0).then_some(line),
        column: number(2).filter(|column| *column > 0),
        end_line: number(3).filter(|end| line > 0 && *end > line),
    }
}

/// `formatFilePathPosition`: `path:line[:col]`, the form editors parse.
pub fn format_file_path_position(position: &FilePathPosition) -> String {
    match (position.line, position.column) {
        (Some(line), Some(column)) => format!("{}:{line}:{column}", position.path),
        (Some(line), None) => format!("{}:{line}", position.path),
        _ => position.path.clone(),
    }
}

pub fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes.len() == 2 || matches!(bytes[2], b'/' | b'\\'))
}

pub fn is_unc_path(value: &str) -> bool {
    value.starts_with("\\\\")
}

/// `isNetworkPath`: any two leading separators name a UNC or device path, which Windows signs in
/// to with the user's credentials, so agent-authored paths must never reach one.
pub fn is_network_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && matches!(bytes[0], b'/' | b'\\') && matches!(bytes[1], b'/' | b'\\')
}

pub fn is_absolute_path(value: &str) -> bool {
    value.starts_with('/') || is_unc_path(value) || is_windows_drive_path(value)
}

/// `stripSlashPrefixedWindowsDrive`: `/C:/x` (a file URL's path) is `C:/x`.
pub fn strip_slash_prefixed_windows_drive(path: &str) -> &str {
    match path.strip_prefix('/') {
        Some(rest) if rest.len() >= 3 && is_windows_drive_path(rest) => rest,
        _ => path,
    }
}

fn is_windows_path_style(value: &str) -> bool {
    is_unc_path(value) || is_windows_drive_path(value) || value.as_bytes().windows(3).any(|w| w[0].is_ascii_alphabetic() && w[1] == b':' && w[2] == b'\\')
}

/// `joinPath`.
pub fn join_path(base: &str, next: &str, windows: bool) -> String {
    let clean = base.trim_end_matches(['/', '\\']);
    if windows { format!("{clean}\\{}", next.replace('/', "\\")) } else { format!("{clean}/{}", next.trim_start_matches('/')) }
}

/// `inferHomeFromCwd`: the home directory a `~/` link means, read off the working directory.
fn infer_home(cwd: &str) -> Option<String> {
    for prefix in ["/Users/", "/home/"] {
        if let Some(rest) = cwd.strip_prefix(prefix) {
            let user = rest.split('/').next().filter(|user| !user.is_empty())?;
            return Some(format!("{prefix}{user}"));
        }
    }
    let bytes = cwd.as_bytes();
    if bytes.len() > 9 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\') && cwd[3..].to_ascii_lowercase().starts_with("users") && matches!(bytes[8], b'/' | b'\\') {
        let user = cwd[9..].split(['/', '\\']).next().filter(|user| !user.is_empty())?;
        return Some(cwd[..9 + user.len()].to_owned());
    }
    None
}

/// `resolvePathLinkTarget`: a link's path made absolute against `cwd`, keeping its position.
pub fn resolve_path_link_target(raw: &str, cwd: &str) -> String {
    let mut position = split_file_path_position(raw, "");
    let path = position.path.clone();
    if path.starts_with("~/") || (is_windows_path_style(cwd) && path.starts_with("~\\")) {
        if let Some(home) = infer_home(cwd) {
            position.path = join_path(&home, &path[2..], is_windows_path_style(&home));
        }
    } else if !is_absolute_path(&path) {
        position.path = join_path(cwd, &path, is_windows_path_style(cwd));
    }
    format_file_path_position(&position)
}

/// `parseFileUrlHref`: the still percent-encoded path and hash of a `file:` URL; a host other
/// than `localhost` names a UNC share.
pub fn parse_file_url_href(href: &str) -> Option<(String, String)> {
    if !href.trim().to_ascii_lowercase().starts_with("file:") {
        return None;
    }
    let parsed = url::Url::parse(href.trim()).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    let host = parsed.host_str().filter(|host| !host.eq_ignore_ascii_case("localhost")).unwrap_or("");
    let path = if host.is_empty() { parsed.path().to_owned() } else { format!("\\\\{host}{}", parsed.path().replace('/', "\\")) };
    if path.is_empty() {
        return None;
    }
    let hash = parsed.fragment().map(|fragment| format!("#{fragment}")).unwrap_or_default();
    Some((strip_slash_prefixed_windows_drive(&path).to_owned(), hash))
}

/// `safeDecodeURIComponent`: percent-decoded, or unchanged when that is not valid UTF-8.
pub fn safe_decode_uri_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = value.get(index + 1..index + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok());
            match hex {
                Some(byte) => {
                    out.push(byte);
                    index += 3;
                    continue;
                }
                None => return value.to_owned(),
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_owned())
}
