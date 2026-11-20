//! The address bar and recently used history. Ports `normalizePreviewUrl` and
//! `resolveAddressBarInput` from `packages/shared/src/preview.ts`, per-project history and
//! Incognito exclusion from `apps/web/src/browserHistoryStore.ts` (`pingdotgg/t3code#34859`),
//! and labels from `PreviewRecentUrlCard.tsx`.

use serde::{Deserialize, Serialize};

use super::viewport::{INCOGNITO_BROWSER_PROFILE_ID, is_incognito_profile};
use super::{NavStatus, SessionSnapshot};

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

/// DuckDuckGo, the web's default engine and fallback for an invalid custom template.
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

/// `isHistoryProfile` (`PreviewView.tsx`, `pingdotgg/t3code#34859`): Incognito visits stay out
/// of the project's persisted history.
pub fn is_history_profile(profile_id: Option<&str>) -> bool {
    !is_incognito_profile(profile_id)
}

/// `normalizeHistoryUrl`: normalizes to http(s) and strips embedded credentials.
pub fn normalize_history_url(raw: &str) -> Option<String> {
    let normalized = normalize_preview_url(raw).ok()?;
    let mut url = url::Url::parse(&normalized).ok()?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let href = url.to_string();
    (href.len() <= HISTORY_MAX_URL_LENGTH).then_some(href)
}

fn normalize_hostname(host: &str) -> String {
    host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase()
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "0:0:0:0:0:0:0:1" | "0.0.0.0" | "::")
        || host.ends_with(".localhost")
}

fn visit_lookup_key(normalized: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(normalized) else { return normalized.to_owned() };
    if let Some(host) = parsed.host_str().map(normalize_hostname)
        && is_loopback_host(&host)
    {
        let _ = parsed.set_host(Some("local"));
    }
    parsed.to_string()
}

fn title_lookup_key(normalized: &str) -> String {
    let key = visit_lookup_key(normalized);
    let Ok(mut parsed) = url::Url::parse(&key) else { return key };
    let path = parsed.path().to_owned();
    if path != "/" && path.ends_with('/') {
        parsed.set_path(path.trim_end_matches('/'));
    }
    parsed.to_string()
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

    /// `recordVisit`: moves the address to the front, keeping a title it already had and
    /// deduplicating loopback hosts.
    pub fn record(&mut self, project: &str, raw: &str, now_ms: i64) -> bool {
        let Some(url) = normalize_history_url(raw) else { return false };
        let key = visit_lookup_key(&url);
        let entries = self.by_project.entry(project.to_owned()).or_default();
        let existing = entries.iter().find(|entry| visit_lookup_key(&entry.url) == key).cloned();
        let title = existing.as_ref().and_then(|entry| entry.title.clone());
        let stored_url = existing
            .as_ref()
            .filter(|entry| {
                let existing_local = url::Url::parse(&entry.url).ok().and_then(|u| u.host_str().map(normalize_hostname)).is_some_and(|h| is_loopback_host(&h));
                let next_local = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(normalize_hostname)).is_some_and(|h| is_loopback_host(&h));
                existing_local || !next_local
            })
            .map_or(url, |entry| entry.url.clone());
        entries.retain(|entry| visit_lookup_key(&entry.url) != key);
        entries.insert(0, HistoryEntry { url: stored_url, last_visited_at: now_ms, title });
        entries.truncate(HISTORY_MAX_ENTRIES_PER_PROJECT);
        self.evict();
        true
    }

    /// `recordVisitForThread` gated by `is_history_profile`: Incognito visits are ignored.
    pub fn record_for_profile(&mut self, project: &str, profile_id: Option<&str>, raw: &str, now_ms: i64) -> bool {
        if !is_history_profile(profile_id) {
            return false;
        }
        self.record(project, raw, now_ms)
    }

    /// A loaded page names the entry it was opened from (`setTitleForUrl`).
    pub fn set_title(&mut self, project: &str, raw: &str, title: &str) -> bool {
        let (Some(url), Some(entries)) = (normalize_history_url(raw), self.by_project.get_mut(project)) else {
            return false;
        };
        let title = title.trim();
        if title.is_empty() {
            return false;
        }
        let key = title_lookup_key(&url);
        let Some(entry) = entries.iter_mut().find(|entry| title_lookup_key(&entry.url) == key) else {
            return false;
        };
        let next: String = title.chars().take(HISTORY_MAX_TITLE_LENGTH).collect();
        if entry.title.as_deref() == Some(next.as_str()) {
            return false;
        }
        entry.title = Some(next);
        true
    }

    /// `recordPreviewTitles` (`browserHistoryStore.ts`, `pingdotgg/t3code#34859`): applies settled
    /// page titles oldest-first so the newest settle wins, skipping Incognito tabs completely.
    pub fn record_settled_titles(&mut self, project: &str, sessions: &[SessionSnapshot]) -> bool {
        let mut ordered: Vec<&SessionSnapshot> = sessions.iter().collect();
        ordered.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        let mut changed = false;
        for session in ordered {
            if session.profile_id.as_deref() == Some(INCOGNITO_BROWSER_PROFILE_ID) {
                continue;
            }
            if let NavStatus::Success { url, title } = &session.nav_status {
                changed |= self.set_title(project, url, title);
            }
        }
        changed
    }

    pub fn remove(&mut self, project: &str, url: &str) -> bool {
        let Some(entries) = self.by_project.get_mut(project) else { return false };
        let normalized = normalize_history_url(url);
        let before = entries.len();
        entries.retain(|entry| entry.url != url && normalized.as_deref() != Some(entry.url.as_str()));
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

/// `resolveExternalServerUrl` (`browserTargetResolver.ts`, `pingdotgg/t3code#37948`):
/// resolves a preview URL for the system browser, which has no loopback tunnel and needs an
/// address it can reach itself. Rewrites loopback (`localhost`, `127.0.0.1`, `::1`, `0.0.0.0`,
/// `[::]`) hosts to the remote environment's host when `environment_http_base` is non-loopback.
pub fn resolve_external_server_url(raw_url: &str, environment_http_base: Option<&str>) -> String {
    let Ok(normalized) = normalize_preview_url(raw_url) else {
        return raw_url.to_owned();
    };
    let Ok(mut parsed) = url::Url::parse(&normalized) else {
        return normalized;
    };
    let Some(host) = parsed.host_str().map(normalize_hostname) else {
        return normalized;
    };
    if !is_loopback_host(&host) {
        return normalized;
    }
    let Some(base) = environment_http_base.and_then(|value| url::Url::parse(value).ok()) else {
        return normalized;
    };
    let Some(env_host) = base.host_str() else {
        return normalized;
    };
    let env_norm = normalize_hostname(env_host);
    if is_loopback_host(&env_norm) {
        return normalized;
    }
    let _ = parsed.set_host(Some(env_host));
    parsed.to_string()
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

    #[test]
    fn incognito_visits_and_titles_stay_out_of_project_history() {
        let mut history = BrowserHistory::default();
        assert!(!is_history_profile(Some(INCOGNITO_BROWSER_PROFILE_ID)));
        assert!(is_history_profile(Some("default")));
        assert!(is_history_profile(None));

        // Incognito visits are ignored.
        assert!(!history.record_for_profile("p", Some(INCOGNITO_BROWSER_PROFILE_ID), "https://secret.test/", 1));
        assert!(history.recent("p").is_empty());

        // Normal visits strip credentials and accept normal settled titles, never Incognito titles.
        assert!(history.record_for_profile("p", Some("default"), "https://user:pass@a.test/path", 2));
        assert_eq!(history.recent("p")[0].url, "https://a.test/path");

        let incognito_session = SessionSnapshot::decode(serde_json::json!({
            "threadId": "t1",
            "tabId": "tab-inc",
            "navStatus": { "_tag": "Success", "url": "https://a.test/path", "title": "Private Title" },
            "canGoBack": false,
            "canGoForward": false,
            "profileId": "incognito",
            "updatedAt": "2026-11-12T00:00:02Z"
        }))
        .unwrap();
        assert!(!history.record_settled_titles("p", &[incognito_session]));
        assert_eq!(history.recent("p")[0].title, None);

        let normal_session = SessionSnapshot::decode(serde_json::json!({
            "threadId": "t1",
            "tabId": "tab-pub",
            "navStatus": { "_tag": "Success", "url": "https://a.test/path/", "title": "Public Title" },
            "canGoBack": false,
            "canGoForward": false,
            "profileId": "default",
            "updatedAt": "2026-11-12T00:00:03Z"
        }))
        .unwrap();
        assert!(history.record_settled_titles("p", &[normal_session]));
        assert_eq!(history.recent("p")[0].title.as_deref(), Some("Public Title"));
    }

    #[test]
    fn resolve_external_server_url_rewrites_loopback_for_remote_environments() {
        assert_eq!(
            resolve_external_server_url("http://localhost:5173/app?a=1", Some("https://devbox.ts.net:3773")),
            "http://devbox.ts.net:5173/app?a=1"
        );
        assert_eq!(
            resolve_external_server_url("http://127.0.0.1:3000/", Some("http://10.0.0.9:3773")),
            "http://10.0.0.9:3000/"
        );
        // Local environment keeps loopback intact.
        assert_eq!(
            resolve_external_server_url("http://localhost:5173/app", Some("http://127.0.0.1:3773")),
            "http://localhost:5173/app"
        );
        // Public URLs are never rewritten.
        assert_eq!(
            resolve_external_server_url("https://example.com/docs", Some("https://devbox.ts.net:3773")),
            "https://example.com/docs"
        );
    }
}
