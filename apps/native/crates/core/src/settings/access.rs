//! Sharing & access: the authorized clients list and pairing links of
//! `web/src/components/settings/AuthorizedClientsSettings.tsx`, the `subscribeAuthAccess` reducer
//! (`client-runtime/src/state/auth.ts`), `AUTH_SCOPE_OPTIONS` (`shared/src/authScopeOptions.ts`),
//! and `setPairingTokenOnUrl` (`pairingUrls.ts`). Wire shapes follow `contracts/src/auth.ts`.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::sidebar::time::parse_iso;

pub const SUBSCRIBE: &str = "subscribeAuthAccess";
pub const ACCESS_READ: &str = "access:read";
pub const ACCESS_WRITE: &str = "access:write";

/// `AUTH_SCOPE_OPTIONS`: scope, title, description, in the dialog's order.
pub const SCOPE_OPTIONS: [(&str, &str, &str); 16] = [
    ("orchestration:read", "View environment", "Read threads, status, checkpoints, and configuration."),
    ("orchestration:operate", "Operate tasks", "Start, update, and stop tasks."),
    ("settings:write", "Change environment settings", "Edit environment preferences and keybindings."),
    ("providers:manage", "Manage providers", "Configure, install, sign in to, and update providers and usage sources."),
    ("environment:maintain", "Maintain environment", "Update the server and control environment processes."),
    ("preview:operate", "Control previews", "Open browser previews and host browser automation."),
    ("diagnostics:read", "View diagnostics and usage", "Read process diagnostics, resource history, and usage totals."),
    ("terminal:read", "View terminals", "Read existing terminal output and status."),
    ("terminal:operate", "Use terminals", "Create terminals and send input to running shells."),
    ("source-control:write", "Change source control", "Commit, push, manage branches and repositories, and change pull requests."),
    ("filesystem:read", "Read files", "Browse host files, search workspaces, and inspect local changes."),
    ("filesystem:write", "Write files", "Edit workspace files and save plans to disk."),
    (ACCESS_READ, "View access", "Inspect pairing links and authorized clients."),
    (ACCESS_WRITE, "Manage access", "Issue and revoke credentials for other clients."),
    ("relay:read", "View relay", "Inspect managed relay connectivity."),
    ("relay:write", "Manage relay", "Change managed tunnel connectivity."),
];

/// The "Read only" preset.
pub const READ_ONLY_SCOPES: [&str; 4] = ["orchestration:read", "filesystem:read", "diagnostics:read", "terminal:read"];

/// `AuthStandardClientScopes`, the "Standard" preset and the dialog's default.
pub fn standard_scopes() -> Vec<String> {
    SCOPE_OPTIONS.iter().map(|(scope, _, _)| (*scope).to_owned()).collect()
}

/// A preset limited to what this session may delegate; `None` grants means unknown, so all.
pub fn preset(scopes: &[&str], grants: Option<&dyn Fn(&str) -> bool>) -> Vec<String> {
    scopes.iter().filter(|scope| grants.is_none_or(|grants| grants(scope))).map(|scope| (*scope).to_owned()).collect()
}

/// A checkbox change: using terminals needs viewing them, so the two move together.
pub fn toggle_scope(selected: &mut Vec<String>, scope: &str, on: bool) {
    let mut set = |scope: &str, on: bool| {
        let present = selected.iter().any(|candidate| candidate == scope);
        if on && !present {
            selected.push(scope.to_owned());
        } else if !on {
            selected.retain(|candidate| candidate != scope);
        }
    };
    set(scope, on);
    if on && scope == "terminal:operate" {
        set("terminal:read", true);
    }
    if !on && scope == "terminal:read" {
        set("terminal:operate", false);
    }
    selected.sort_by_key(|scope| SCOPE_OPTIONS.iter().position(|(candidate, _, _)| candidate == scope).unwrap_or(usize::MAX));
}

/// What the create dialog warns about under the checkboxes.
pub fn scopes_notice(selected: &[String]) -> Option<(&'static str, bool)> {
    if selected.is_empty() {
        return Some(("Select at least one permission.", true));
    }
    selected.iter().any(|scope| scope == ACCESS_WRITE).then_some(("This client can create or revoke access for other devices.", false))
}

/// `POST /api/auth/pairing-token`: a trimmed label is sent only when not empty.
pub fn pairing_token_body(label: &str, scopes: &[String]) -> Value {
    let mut body = json!({ "scopes": scopes });
    if !label.trim().is_empty() {
        body["label"] = json!(label.trim());
    }
    body
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingLink {
    pub id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub permissions: Option<Vec<String>>,
    #[serde(default)]
    pub label: Option<String>,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClientInfo {
    pub label: Option<String>,
    pub ip_address: Option<String>,
    pub device_type: Option<String>,
    pub os: Option<String>,
    pub browser: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSession {
    pub session_id: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub permissions: Option<Vec<String>>,
    #[serde(default)]
    pub client: ClientInfo,
    #[serde(default)]
    pub issued_at: String,
    #[serde(default)]
    pub last_connected_at: Option<String>,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub current: bool,
}

/// `permissions ?? scopes`.
pub fn shown_scopes<'a>(permissions: &'a Option<Vec<String>>, scopes: &'a [String]) -> &'a [String] {
    permissions.as_deref().unwrap_or(scopes)
}

pub fn scope_count(scopes: &[String]) -> String {
    format!("{} {}", scopes.len(), if scopes.len() == 1 { "scope" } else { "scopes" })
}

impl PairingLink {
    pub fn title(&self) -> &str {
        self.label.as_deref().filter(|label| !label.is_empty()).unwrap_or("Pairing link")
    }
}

impl ClientSession {
    /// `client.label`, else `os · browser`, else the subject.
    pub fn title(&self) -> String {
        if let Some(label) = self.client.label.as_deref().filter(|label| !label.is_empty()) {
            return label.to_owned();
        }
        let parts: Vec<&str> = [self.client.os.as_deref(), self.client.browser.as_deref()].into_iter().flatten().filter(|part| !part.is_empty()).collect();
        if parts.is_empty() { self.subject.clone() } else { parts.join(" \u{00b7} ") }
    }

    /// Device type (capitalized, unless unknown), OS, browser, and IP.
    pub fn subline(&self) -> String {
        let device = self.client.device_type.as_deref().filter(|kind| !kind.is_empty() && *kind != "unknown").map(|kind| {
            let mut chars = kind.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        });
        [device.as_deref(), self.client.os.as_deref(), self.client.browser.as_deref(), self.client.ip_address.as_deref()]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" \u{00b7} ")
    }

    pub fn online(&self) -> bool {
        self.current || self.connected
    }
}

/// The `subscribeAuthAccess` state: a snapshot replaces it, an upsert replaces or appends, a
/// removal filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccessState {
    pub loaded: bool,
    pub pairing_links: Vec<PairingLink>,
    pub clients: Vec<ClientSession>,
}

impl AccessState {
    /// Applies one stream item; unknown types and undecodable payloads are skipped.
    pub fn apply(&mut self, item: &Value) {
        let payload = item.get("payload").cloned().unwrap_or(Value::Null);
        match item.get("type").and_then(Value::as_str) {
            Some("snapshot") => {
                let list = |key: &str| payload.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
                self.pairing_links = list("pairingLinks").into_iter().filter_map(|link| serde_json::from_value(link).ok()).collect();
                self.clients = list("clientSessions").into_iter().filter_map(|client| serde_json::from_value(client).ok()).collect();
                self.loaded = true;
            }
            Some("pairingLinkUpserted") => {
                if let Ok(link) = serde_json::from_value::<PairingLink>(payload) {
                    self.pairing_links.retain(|candidate| candidate.id != link.id);
                    self.pairing_links.push(link);
                }
            }
            Some("pairingLinkRemoved") => {
                let id = payload.get("id").and_then(Value::as_str).unwrap_or_default();
                self.pairing_links.retain(|link| link.id != id);
            }
            Some("clientUpserted") => {
                if let Ok(client) = serde_json::from_value::<ClientSession>(payload) {
                    self.clients.retain(|candidate| candidate.session_id != client.session_id);
                    self.clients.push(client);
                }
            }
            Some("clientRemoved") => {
                let id = payload.get("sessionId").and_then(Value::as_str).unwrap_or_default();
                self.clients.retain(|client| client.session_id != id);
            }
            _ => {}
        }
    }

    /// Unexpired links, newest first.
    pub fn visible_links(&self, now_ms: i64) -> Vec<&PairingLink> {
        let mut links: Vec<&PairingLink> = self.pairing_links.iter().filter(|link| parse_iso(&link.expires_at).is_some_and(|at| at > now_ms)).collect();
        links.sort_by_key(|link| std::cmp::Reverse(parse_iso(&link.created_at).unwrap_or(0)));
        links
    }

    /// This device first, then connected clients, then newest issued.
    pub fn sorted_clients(&self) -> Vec<&ClientSession> {
        let mut clients: Vec<&ClientSession> = self.clients.iter().collect();
        clients.sort_by_key(|client| (!client.current, !client.connected, std::cmp::Reverse(parse_iso(&client.issued_at).unwrap_or(0))));
        clients
    }

    /// The fold's summary: `"{n} client(s)"`, then `" · {m} pairing link(s)"` while any exist.
    pub fn summary(&self, now_ms: i64) -> String {
        let clients = self.clients.len();
        let links = self.visible_links(now_ms).len();
        let mut summary = format!("{clients} {}", if clients == 1 { "client" } else { "clients" });
        if links > 0 {
            summary.push_str(&format!(" \u{00b7} {links} pairing {}", if links == 1 { "link" } else { "links" }));
        }
        summary
    }

    /// "Revoke others" stays on until the list shows nobody else.
    pub fn can_revoke_others(&self) -> bool {
        !self.loaded || self.clients.iter().any(|client| !client.current)
    }
}

/// `formatExpiresInLabel`.
pub fn expires_label(expires_at: &str, now_ms: i64) -> String {
    let Some(at) = parse_iso(expires_at) else { return String::new() };
    let diff = at - now_ms;
    if diff <= 0 {
        return "Expired".to_owned();
    }
    let total = diff / 1000;
    if total < 5 {
        return "Expires in a moment".to_owned();
    }
    if total < 60 {
        return format!("Expires in {total}s");
    }
    let (days, hours, minutes, seconds) = (total / 86_400, total % 86_400 / 3600, total % 3600 / 60, total % 60);
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 {
        parts.push(format!("{seconds}s"));
    }
    format!("Expires in {}", parts.join(" "))
}

/// The revoke-others toast title.
pub fn revoked_others_label(count: u64) -> String {
    if count == 1 { "Revoked 1 other client".to_owned() } else { format!("Revoked {count} clients") }
}

/// `<server>/pair#token=<credential>`, or `None` for a loopback server another device cannot reach.
pub fn pairing_url(http_base: &str, credential: &str) -> Option<String> {
    let base = http_base.trim_end_matches('/');
    let authority = base.split("://").nth(1).unwrap_or(base).split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return None;
    }
    Some(format!("{base}/pair#token={credential}"))
}

#[cfg(test)]
mod tests;
