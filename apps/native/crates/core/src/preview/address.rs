//! The address bar and the recently used list. Ports `normalizePreviewUrl` and
//! `resolveAddressBarInput` from `packages/shared/src/preview.ts`, and the per-project history
//! of `apps/web/src/browserHistoryStore.ts` (`upsertHistoryEntry`, `evictExcessProjects`,
//! `normalizeHistoryUrl`) with the labels of `PreviewRecentUrlCard.tsx`.

use serde::{Deserialize, Serialize};

/// `PreviewUrlNormalizationError`; every reason reads the same to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlError {
    Empty,
    Parse,
    UnsupportedProtocol(String),
}

impl UrlError {
    /// The toast the web shows for a normalization error.
    pub const MESSAGE: &'static str = "Enter an http or https URL.";
}

/// DuckDuckGo, the web's default engine. Native has no search engine setting yet.
pub const DEFAULT_SEARCH_TEMPLATE: &str = "https://duckduckgo.com/?q=%s";

const NON_WEB_SCHEMES: &[&str] =
    &["about:", "blob:", "chrome:", "data:", "file:", "ftp:", "javascript:", "mailto:", "sms:", "ssh:", "tel:", "view-source:", "ws:", "wss:"];

/// `^[a-z][a-z\d+.-]*:` — the scheme with its colon, lowercased.
fn protocol(raw: &str) -> Option<String> {
    let mut chars = raw.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    for (index, char) in chars {
        if char == ':' {
            return Some(format!("{}:", raw[..index].to_ascii_lowercase()));
        }
        if !(char.is_ascii_alphanumeric() || matches!(char, '+' | '.' | '-')) {
            return None;
        }
    }
    None
}

/// `URL_SCHEME_WITH_AUTHORITY_PATTERN`.
fn has_scheme_with_authority(raw: &str) -> bool {
    protocol(raw).is_some_and(|protocol| raw[protocol.len()..].starts_with("//"))
}

/// `LOOPBACK_PREFIX_PATTERN`: loopback hosts default to http.
fn is_loopback_prefix(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    ["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "[::]"].iter().any(|host| {
        lower.strip_prefix(host).is_some_and(|rest| rest.is_empty() || rest.starts_with(':') || rest.starts_with('/'))
    })
}

/// `BARE_HOST_PORT_PATTERN`: `host:1234` with nothing but a path, query or hash after it.
fn is_bare_host_port(raw: &str) -> bool {
    let Some(colon) = raw.find(':') else { return false };
    let host = &raw[..colon];
    if host.is_empty() || host.contains(['/', '?', '#', '@']) {
        return false;
    }
    let rest = &raw[colon + 1..];
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && rest[digits..].chars().next().is_none_or(|next| matches!(next, '/' | '?' | '#'))
}

/// `ADDRESS_HOST_PATTERN`: localhost, a dotted IPv4 address, or a bracketed IPv6 one.
fn is_address_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Some(inner) = host.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
        return !inner.is_empty() && inner.chars().all(|char| char.is_ascii_hexdigit() || char == ':' || char == '.');
    }
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4 && parts.iter().all(|part| (1..=3).contains(&part.len()) && part.chars().all(|char| char.is_ascii_digit()))
}

/// `^[^.]+(?:\.[^.]+)+\.?$`: a name with at least one inner dot.
fn is_dotted_name(host: &str) -> bool {
    let trimmed = host.strip_suffix('.').unwrap_or(host);
    let parts: Vec<&str> = trimmed.split('.').collect();
    parts.len() >= 2 && parts.iter().all(|part| !part.is_empty())
}

/// `normalizePreviewUrl`: an http(s) URL, adding the scheme (http for loopback) when missing.
pub fn normalize_preview_url(raw: &str) -> Result<String, UrlError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(UrlError::Empty);
    }
    let candidate = if has_scheme_with_authority(trimmed) {
        trimmed.to_owned()
    } else {
        format!("{}://{trimmed}", if is_loopback_prefix(trimmed) { "http" } else { "https" })
    };
    let parsed = url::Url::parse(&candidate).map_err(|_| UrlError::Parse)?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        other => Err(UrlError::UnsupportedProtocol(format!("{other}:"))),
    }
}

/// `encodeURIComponent`.
fn encode_component(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// `resolveAddressBarInput`: a URL when the text reads as one, otherwise a search.
pub fn resolve_address_bar_input(raw: &str, search_template: &str) -> Result<String, UrlError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || has_scheme_with_authority(trimmed) {
        return normalize_preview_url(trimmed);
    }
    if !trimmed.chars().any(char::is_whitespace) {
        if let Some(protocol) = protocol(trimmed)
            && (NON_WEB_SCHEMES.contains(&protocol.as_str()) || !is_bare_host_port(trimmed))
        {
            if protocol != "http:" && protocol != "https:" {
                return Err(UrlError::UnsupportedProtocol(protocol));
            }
            return normalize_preview_url(trimmed);
        }
        let authority = trimmed.split(['/', '?', '#']).next().unwrap_or("");
        let without_credentials = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        let host = strip_port(without_credentials);
        let has_port = host.len() < without_credentials.len();
        if (has_port || is_address_host(host) || is_dotted_name(host))
            && let Ok(url) = normalize_preview_url(trimmed)
        {
            return Ok(url);
        }
    }
    Ok(search_template.replace("%s", &encode_component(trimmed)))
}

/// `.replace(/:\d+$/, "")`.
fn strip_port(authority: &str) -> &str {
    match authority.rfind(':') {
        Some(colon) if colon + 1 < authority.len() && authority[colon + 1..].chars().all(|char| char.is_ascii_digit()) => &authority[..colon],
        _ => authority,
    }
}

/// `BrowserHistoryEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub url: String,
    pub last_visited_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl HistoryEntry {
    /// `PreviewRecentUrlCard`'s label: host, path (none for `/`), query and hash.
    pub fn label(&self) -> String {
        let Ok(url) = url::Url::parse(&self.url) else { return self.url.clone() };
        let host = match (url.host_str(), url.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            _ => String::new(),
        };
        let path = if url.path() == "/" { "" } else { url.path() };
        let query = url.query().map(|query| format!("?{query}")).unwrap_or_default();
        let hash = url.fragment().map(|hash| format!("#{hash}")).unwrap_or_default();
        format!("{host}{path}{query}{hash}")
    }
}

pub const HISTORY_MAX_ENTRIES_PER_PROJECT: usize = 50;
const HISTORY_MAX_PROJECTS: usize = 20;
const HISTORY_MAX_URL_LENGTH: usize = 2048;
const HISTORY_MAX_TITLE_LENGTH: usize = 512;
/// The empty state shows this many (`PreviewEmptyState`).
pub const RECENT_SHOWN: usize = 8;

/// `normalizeHistoryUrl`.
fn history_url(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw.trim()).ok().filter(|url| matches!(url.scheme(), "http" | "https"))?;
    let href = url.to_string();
    (href.len() <= HISTORY_MAX_URL_LENGTH).then_some(href)
}

/// Recently used addresses per project, newest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BrowserHistory {
    #[serde(default)]
    by_project: std::collections::BTreeMap<String, Vec<HistoryEntry>>,
}

impl BrowserHistory {
    pub fn recent(&self, project: &str) -> &[HistoryEntry] {
        self.by_project.get(project).map_or(&[], Vec::as_slice)
    }

    /// `recordVisit`: moves the address to the front, keeping a title it already had.
    pub fn record(&mut self, project: &str, raw: &str, now_ms: i64) -> bool {
        let Some(url) = history_url(raw) else { return false };
        let entries = self.by_project.entry(project.to_owned()).or_default();
        let title = entries.iter().find(|entry| entry.url == url).and_then(|entry| entry.title.clone());
        entries.retain(|entry| entry.url != url);
        entries.insert(0, HistoryEntry { url, last_visited_at: now_ms, title });
        entries.truncate(HISTORY_MAX_ENTRIES_PER_PROJECT);
        self.evict();
        true
    }

    /// A loaded page names the entry it was opened from.
    pub fn set_title(&mut self, project: &str, raw: &str, title: &str) -> bool {
        let (Some(url), Some(entries)) = (history_url(raw), self.by_project.get_mut(project)) else { return false };
        let title = title.trim();
        let Some(entry) = entries.iter_mut().find(|entry| entry.url == url) else { return false };
        let next = (!title.is_empty()).then(|| title.chars().take(HISTORY_MAX_TITLE_LENGTH).collect::<String>());
        if entry.title == next || next.is_none() {
            return false;
        }
        entry.title = next;
        true
    }

    pub fn remove(&mut self, project: &str, url: &str) -> bool {
        let Some(entries) = self.by_project.get_mut(project) else { return false };
        let before = entries.len();
        entries.retain(|entry| entry.url != url);
        let removed = entries.len() != before;
        if entries.is_empty() {
            self.by_project.remove(project);
        }
        removed
    }

    /// `evictExcessProjects`: the projects visited most recently stay.
    fn evict(&mut self) {
        if self.by_project.len() <= HISTORY_MAX_PROJECTS {
            return;
        }
        let mut latest: Vec<(i64, String)> =
            self.by_project.iter().map(|(project, entries)| (entries.first().map_or(0, |entry| entry.last_visited_at), project.clone())).collect();
        latest.sort_by(|a, b| b.cmp(a));
        for (_, project) in latest.into_iter().skip(HISTORY_MAX_PROJECTS) {
            self.by_project.remove(&project);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_like_the_web_address_bar() {
        assert_eq!(normalize_preview_url("localhost:3000").unwrap(), "http://localhost:3000/");
        assert_eq!(normalize_preview_url("127.0.0.1:14658/x").unwrap(), "http://127.0.0.1:14658/x");
        assert_eq!(normalize_preview_url("example.com").unwrap(), "https://example.com/");
        assert_eq!(normalize_preview_url(" https://example.com/a?b#c ").unwrap(), "https://example.com/a?b#c");
        assert_eq!(normalize_preview_url(""), Err(UrlError::Empty));
        assert_eq!(normalize_preview_url("ftp://example.com"), Err(UrlError::UnsupportedProtocol("ftp:".into())));
    }

    #[test]
    fn address_bar_input_searches_unless_it_reads_as_a_url() {
        let resolve = |raw| resolve_address_bar_input(raw, DEFAULT_SEARCH_TEMPLATE);
        assert_eq!(resolve("example.com").unwrap(), "https://example.com/");
        assert_eq!(resolve("localhost:5173/app").unwrap(), "http://localhost:5173/app");
        assert_eq!(resolve("myhost:8080").unwrap(), "https://myhost:8080/");
        assert_eq!(resolve("192.168.1.2").unwrap(), "https://192.168.1.2/");
        assert_eq!(resolve("rust lifetimes").unwrap(), "https://duckduckgo.com/?q=rust%20lifetimes");
        assert_eq!(resolve("gpui").unwrap(), "https://duckduckgo.com/?q=gpui");
        assert_eq!(resolve("c++").unwrap(), "https://duckduckgo.com/?q=c%2B%2B");
        assert_eq!(resolve("mailto:me@example.com"), Err(UrlError::UnsupportedProtocol("mailto:".into())));
        assert_eq!(resolve("javascript:alert(1)"), Err(UrlError::UnsupportedProtocol("javascript:".into())));
    }

    #[test]
    fn history_keeps_the_newest_visit_first_per_project() {
        let mut history = BrowserHistory::default();
        assert!(history.record("p", "http://localhost:3000", 1));
        assert!(history.record("p", "https://example.com/docs?q=1", 2));
        assert!(history.set_title("p", "http://localhost:3000/", "Dev"));
        assert!(history.record("p", "http://localhost:3000/", 3));
        let urls: Vec<_> = history.recent("p").iter().map(|entry| (entry.url.as_str(), entry.title.as_deref())).collect();
        assert_eq!(urls, [("http://localhost:3000/", Some("Dev")), ("https://example.com/docs?q=1", None)]);
        assert_eq!(history.recent("p")[1].label(), "example.com/docs?q=1");
        assert_eq!(history.recent("p")[0].label(), "localhost:3000");
        assert!(!history.record("p", "file:///etc/hosts", 4));
        assert!(history.remove("p", "http://localhost:3000/"));
        assert_eq!(history.recent("p").len(), 1);
        for project in 0..25 {
            history.record(&project.to_string(), "https://example.com/", 10 + project);
        }
        assert!(history.recent("p").is_empty());
        assert_eq!(history.recent("24").len(), 1);
    }
}
