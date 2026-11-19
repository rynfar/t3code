//! Pairing links (`https://host:port/pair#token=…`, hosted `https://app.t3.codes/pair?host=…#token=…`,
//! `t3code://pair?u=…`), link-request machine resolution (`packages/client-runtime/src/linkRequest.ts`),
//! one-use peer pairing & MCP approval codes (`packages/client-runtime/src/state/peerPairingCode.ts`),
//! `LinkMachineSelect` presentation helpers (`apps/web/src/components/settings/LinkMachineSelect.tsx`),
//! access environment selection (`apps/web/src/components/settings/ConnectionsSettings.logic.ts`),
//! Tailscale Serve reachability (`pingdotgg/t3code#35543`), and legacy-permission detection
//! (`packages/contracts/src/auth.ts`).

use std::collections::HashSet;

use serde_json::{Value, json};
use url::Url;

use crate::auth::AuthSessionState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingLink {
    /// `scheme://host[:port]`, no trailing slash.
    pub http_base: String,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    #[error("Enter a pairing link like https://host:3773/pair#token=…")]
    Invalid,
    #[error("That link has no pairing token.")]
    MissingToken,
}

/// Accepts a full pairing URL, a hosted pairing URL (`https://app.t3.codes/pair?host=…#token=…`),
/// a `t3code://pair?host=…` or `t3code://pair?u=…` app link, or `host[:port]` plus a separate token.
pub fn parse_pairing_link(input: &str, separate_token: Option<&str>) -> Result<PairingLink, PairingError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(PairingError::Invalid);
    }
    let with_scheme = if trimmed.contains("://") { trimmed.to_owned() } else { format!("http://{trimmed}") };
    let url = Url::parse(&with_scheme).map_err(|_| PairingError::Invalid)?;

    // Hosted pairing link (`?host=<target>#token=…`) or `t3code://pair?host=…` / `?u=…`.
    if let Some(target_raw) = query_param(&url, "host").or_else(|| {
        (url.scheme() == "t3code").then(|| query_param(&url, "u")).flatten()
    }) {
        let target_with_scheme = if target_raw.contains("://") {
            target_raw
        } else {
            format!("http://{target_raw}")
        };
        let target_url = Url::parse(&target_with_scheme).map_err(|_| PairingError::Invalid)?;
        if !matches!(target_url.scheme(), "http" | "https") || target_url.host_str().is_none() {
            return Err(PairingError::Invalid);
        }
        let token = token_from_url(&url)
            .or_else(|| token_from_url(&target_url))
            .or_else(|| separate_token.map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned))
            .ok_or(PairingError::MissingToken)?;
        return Ok(PairingLink { http_base: origin(&target_url), token });
    }

    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(PairingError::Invalid);
    }
    let token = token_from_url(&url)
        .or_else(|| separate_token.map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned))
        .ok_or(PairingError::MissingToken)?;
    Ok(PairingLink { http_base: origin(&url), token })
}

fn query_param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn token_from_url(url: &Url) -> Option<String> {
    let from_pairs = |pairs: url::form_urlencoded::Parse<'_>| {
        pairs.into_iter().find(|(key, _)| key == "token").map(|(_, value)| value.into_owned())
    };
    url.fragment()
        .and_then(|fragment| from_pairs(url::form_urlencoded::parse(fragment.as_bytes())))
        .or_else(|| from_pairs(url.query_pairs()))
        .filter(|token| !token.is_empty())
}

fn origin(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    let host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.to_owned() };
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// Normalized `scheme://host[:port]` origin of an `http(s)` URL, if parseable.
pub fn origin_of(raw: &str) -> Option<String> {
    let url = Url::parse(raw.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| origin(&url))
}

/// `isLocalLoopbackHost`: localhost, `*.localhost`, `127.*`, `::1`, or `0.0.0.0`.
pub fn is_loopback_host(host: &str) -> bool {
    let normalized = host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    normalized == "localhost"
        || normalized.ends_with(".localhost")
        || normalized == "::1"
        || normalized == "0.0.0.0"
        || normalized.starts_with("127.")
}

/// A loopback URL names whichever machine opens it: the linking server, not the target machine.
pub fn is_loopback_url(raw: &str) -> bool {
    Url::parse(raw.trim())
        .ok()
        .and_then(|url| url.host_str().map(is_loopback_host))
        .unwrap_or(false)
}

/// What a link request card calls an address: its origin when parseable, else trimmed text.
pub fn link_destination(address: &str) -> String {
    origin_of(address).unwrap_or_else(|| address.trim().to_owned())
}

/// Deduplicates and drops loopback URLs when assembling known reachable addresses for a machine
/// (`linkableMachines` in `packages/client-runtime/src/linkRequest.ts`, `pingdotgg/t3code#34139`).
pub fn collect_known_machine_urls<'a>(candidates: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    for raw in candidates {
        let trimmed = raw.trim();
        if trimmed.is_empty() || is_loopback_url(trimmed) {
            continue;
        }
        let key = origin_of(trimmed).unwrap_or_else(|| trimmed.to_owned());
        if seen.insert(key) {
            urls.push(trimmed.trim_end_matches('/').to_owned());
        }
    }
    urls
}

/// The machine choice constant for linking by an address typed in (`ANOTHER_ADDRESS`).
pub const ANOTHER_ADDRESS: &str = "another-address";
/// `MINT_TIMEOUT_MS` in `packages/client-runtime/src/state/peerPairingCode.ts`.
pub const MINT_PAIRING_CODE_TIMEOUT_MS: u64 = 8_000;
/// `PEER_PAIRING_CODE_LABEL` in `packages/client-runtime/src/state/peerPairingCode.ts`.
pub const PEER_PAIRING_CODE_LABEL: &str = "T3 Code link";
/// `purpose` binding automatic pairing codes to peer links (`pingdotgg/t3code#37825`).
pub const PEER_LINK_PURPOSE: &str = "peer-link";
/// `MCP_APPROVAL_CODE_LABEL` in `packages/client-runtime/src/state/peerPairingCode.ts`.
pub const MCP_APPROVAL_CODE_LABEL: &str = "App sign-in";

/// Why minting a one-use pairing code on a peer environment failed (`PeerPairingCodeError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerPairingCodeErrorReason {
    NotAllowed,
    Unavailable,
}

/// `PeerPairingCodeError` (`packages/client-runtime/src/state/peerPairingCode.ts`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", self.user_message())]
pub struct PeerPairingCodeError {
    pub reason: PeerPairingCodeErrorReason,
    pub detail: Option<String>,
}

impl PeerPairingCodeError {
    pub fn not_allowed(detail: impl Into<Option<String>>) -> Self {
        Self { reason: PeerPairingCodeErrorReason::NotAllowed, detail: detail.into() }
    }

    pub fn unavailable(detail: impl Into<Option<String>>) -> Self {
        Self { reason: PeerPairingCodeErrorReason::Unavailable, detail: detail.into() }
    }

    pub fn user_message(&self) -> &'static str {
        match self.reason {
            PeerPairingCodeErrorReason::NotAllowed => {
                "Your session there cannot create pairing codes. Paste a code from there instead."
            }
            PeerPairingCodeErrorReason::Unavailable => {
                "Could not get a pairing code from there. Paste a code from there instead."
            }
        }
    }
}

/// Decoded `AuthPairingCredentialResult` from `POST /api/auth/pairing-token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCredentialResult {
    pub id: Option<String>,
    pub credential: String,
    pub expires_at: Option<String>,
    pub label: Option<String>,
    pub purpose: Option<String>,
    pub access: Option<String>,
}

/// Builds the `POST /api/auth/pairing-token` body for `createPeerPairingCodeCommand`
/// (`pingdotgg/t3code#37825`: binds automatic codes to `purpose: "peer-link"` + `access`).
pub fn peer_pairing_code_request_body(access: &str) -> Value {
    json!({
        "label": PEER_PAIRING_CODE_LABEL,
        "access": access,
        "purpose": PEER_LINK_PURPOSE,
    })
}

/// Validates that a minted pairing credential is bound to `purpose: "peer-link"` at `expected_access`
/// (`pingdotgg/t3code#37825`).
pub fn validate_peer_link_credential(
    minted: &PairingCredentialResult,
    expected_access: &str,
) -> Result<String, PeerPairingCodeError> {
    if minted.purpose.as_deref() == Some(PEER_LINK_PURPOSE)
        && minted.access.as_deref() == Some(expected_access)
    {
        Ok(minted.credential.clone())
    } else {
        Err(PeerPairingCodeError::unavailable(Some(
            "The environment did not issue a peer-link code.".to_owned(),
        )))
    }
}

/// Builds the `POST /api/auth/pairing-token` body for `createMcpApprovalCodeCommand`.
pub fn mcp_approval_code_request_body(access: &str) -> Value {
    json!({
        "label": MCP_APPROVAL_CODE_LABEL,
        "access": access,
    })
}

/// Decodes a `POST /api/auth/pairing-token` JSON reply into `PairingCredentialResult`.
pub fn decode_pairing_credential_result(value: &Value) -> Option<PairingCredentialResult> {
    let credential = value
        .get("credential")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    Some(PairingCredentialResult {
        id: value.get("id").and_then(Value::as_str).map(str::to_owned),
        credential,
        expires_at: value.get("expiresAt").and_then(Value::as_str).map(str::to_owned),
        label: value.get("label").and_then(Value::as_str).map(str::to_owned),
        purpose: value.get("purpose").and_then(Value::as_str).map(str::to_owned),
        access: value.get("access").and_then(Value::as_str).map(str::to_owned),
    })
}

/// Status badge shown beside a machine in `LinkMachineSelect` (`LinkMachineBadge`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMachineBadge {
    Linked,
    T3Connect,
    NotConnected,
}

impl LinkMachineBadge {
    pub fn label(self) -> &'static str {
        match self {
            LinkMachineBadge::Linked => "Linked",
            LinkMachineBadge::T3Connect => "T3 Connect",
            LinkMachineBadge::NotConnected => "Not connected",
        }
    }
}

/// A machine the user could link a thread's environment to (`LinkableMachine`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkableMachine {
    pub environment_id: String,
    pub label: String,
    pub machine: Option<String>,
    pub urls: Vec<String>,
    pub connected: bool,
    pub connect_only: bool,
    pub linked: bool,
}

/// `LinkMachineBadge` (`apps/web/src/components/settings/LinkMachineSelect.tsx`).
pub fn link_machine_badge(machine: &LinkableMachine) -> Option<LinkMachineBadge> {
    if machine.linked {
        return Some(LinkMachineBadge::Linked);
    }
    if machine.connected {
        return None;
    }
    Some(if machine.connect_only { LinkMachineBadge::T3Connect } else { LinkMachineBadge::NotConnected })
}

/// Trigger label for `LinkMachineSelect` (`apps/web/src/components/settings/LinkMachineSelect.tsx`).
pub fn link_machine_select_label(machines: &[LinkableMachine], selected: Option<&str>) -> String {
    match selected {
        Some(ANOTHER_ADDRESS) => "Another address\u{2026}".to_owned(),
        Some(id) => machines
            .iter()
            .find(|candidate| candidate.environment_id == id)
            .map(|machine| machine.label.clone())
            .unwrap_or_else(|| "Choose a machine".to_owned()),
        None => "Choose a machine".to_owned(),
    }
}

/// What a link request resolves to (`LinkRequestTarget` in `linkRequest.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRequestTarget {
    None,
    Machine {
        environment_id: String,
        label: String,
        urls: Vec<String>,
        linked: bool,
    },
    Address,
}

/// Resolves a peer link request target using ONLY addresses this app already knows for a machine
/// (`linkRequestTarget`, `pingdotgg/t3code#34139`). Two machines sharing a private address fall
/// through to `Address` so pairing codes are never misrouted.
pub fn resolve_link_request_target(
    item_environment_id: Option<&str>,
    item_url: Option<&str>,
    machines: &[LinkableMachine],
    choice: Option<&str>,
) -> LinkRequestTarget {
    let asked = item_url.and_then(origin_of);
    let chosen: Option<&str> = match item_url {
        None => item_environment_id.or(choice),
        Some(_) => {
            let matches: Vec<&LinkableMachine> = match asked.as_deref() {
                Some(asked) => machines
                    .iter()
                    .filter(|machine| machine.urls.iter().any(|url| origin_of(url).as_deref() == Some(asked)))
                    .collect(),
                None => Vec::new(),
            };
            if matches.len() == 1 {
                Some(matches[0].environment_id.as_str())
            } else {
                Some(ANOTHER_ADDRESS)
            }
        }
    };
    match chosen {
        None => LinkRequestTarget::None,
        Some(ANOTHER_ADDRESS) => LinkRequestTarget::Address,
        Some(env_id) => match machines.iter().find(|machine| machine.environment_id == env_id) {
            Some(machine) => LinkRequestTarget::Machine {
                environment_id: machine.environment_id.clone(),
                label: machine.label.clone(),
                urls: machine.urls.clone(),
                linked: machine.linked,
            },
            None => LinkRequestTarget::Machine {
                environment_id: env_id.to_owned(),
                label: item_url.map_or_else(|| "a machine".to_owned(), link_destination),
                urls: Vec::new(),
                linked: false,
            },
        },
    }
}

/// `isServerNetworkAccessible` (`ConnectionsSettings.logic.ts`, `pingdotgg/t3code#35543`):
/// a loopback server published through Tailscale Serve advertises a direct endpoint and is
/// network-accessible rather than local-only.
pub fn is_server_network_accessible(auth_policy: Option<&str>, direct_endpoints_count: usize) -> bool {
    match auth_policy {
        Some("remote-reachable") => true,
        Some("loopback-browser") => direct_endpoints_count > 0,
        _ => direct_endpoints_count > 0,
    }
}

/// One candidate environment for Settings → Sharing & access (`resolveAccessEnvironmentId`,
/// `pingdotgg/t3code#26464`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessEnvironmentCandidate {
    pub id: String,
    pub connected: bool,
}

/// An explicit choice while it is listed, else the primary, else a connected environment,
/// else the first listed environment (`resolveAccessEnvironmentId`).
pub fn resolve_access_environment_id<'a>(
    primary_id: Option<&'a str>,
    environments: &'a [AccessEnvironmentCandidate],
    selected: Option<&'a str>,
) -> Option<&'a str> {
    let is_listed = |wanted: Option<&str>| {
        wanted.is_some_and(|wanted| environments.iter().any(|env| env.id == wanted))
    };
    if is_listed(selected) {
        return selected;
    }
    if is_listed(primary_id) {
        return primary_id;
    }
    environments
        .iter()
        .find(|env| env.connected)
        .or_else(|| environments.first())
        .map(|env| env.id.as_str())
}

/// Original pre-split scopes (`legacyScopes` in `packages/contracts/src/auth.ts`).
const LEGACY_SCOPES: &[&str] = &[
    "orchestration:read",
    "orchestration:operate",
    "terminal:operate",
    "access:read",
    "access:write",
    "relay:read",
    "relay:write",
];

/// Parent scopes that were later split (`Object.values(legacyParents)` in `contracts/src/auth.ts`).
const LEGACY_PARENT_SCOPES: &[&str] = &[
    "orchestration:read",
    "orchestration:operate",
    "terminal:operate",
];

/// `sessionHasLegacyPermissions` (`packages/contracts/src/auth.ts` / `PermissionUpdateNotice.ts`):
/// true when the authenticated session carries explicit `permissions` consisting solely of legacy
/// scopes including at least one split parent scope.
pub fn session_has_legacy_permissions(session: &AuthSessionState) -> bool {
    let Some(permissions) = session.permissions.as_deref() else {
        return false;
    };
    session.authenticated
        && !permissions.is_empty()
        && permissions.iter().all(|scope| LEGACY_SCOPES.contains(&scope.as_str()))
        && LEGACY_PARENT_SCOPES
            .iter()
            .any(|parent| permissions.iter().any(|scope| scope == parent))
}

/// `ws(s)://host[:port]/ws` for an `http(s)` base.
pub fn websocket_base(http_base: &str) -> String {
    let rest = http_base.trim_end_matches('/');
    let ws = if let Some(rest) = rest.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = rest.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        rest.to_owned()
    };
    format!("{ws}/ws")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fragment_query_hosted_app_link_and_bare_host_forms() {
        let link = parse_pairing_link("https://box.example.ts.net:5230/pair#token=abc", None).unwrap();
        assert_eq!(link, PairingLink { http_base: "https://box.example.ts.net:5230".into(), token: "abc".into() });
        let link = parse_pairing_link("http://127.0.0.1:3773/pair?token=xyz", None).unwrap();
        assert_eq!(link.token, "xyz");
        let link = parse_pairing_link("localhost:3773", Some(" tok ")).unwrap();
        assert_eq!(link, PairingLink { http_base: "http://localhost:3773".into(), token: "tok".into() });
        // Hosted pairing link (`apps/web/src/hostedPairing.ts`).
        let hosted = parse_pairing_link(
            "https://app.t3.codes/pair?host=https%3A%2F%2Fstudio.ts.net%3A3773&label=Studio#token=HOSTED1",
            None,
        )
        .unwrap();
        assert_eq!(hosted, PairingLink { http_base: "https://studio.ts.net:3773".into(), token: "HOSTED1".into() });
        // Native app deep link from `t3 pair`.
        let app_link = parse_pairing_link("t3code://pair?u=http%3A%2F%2F192.168.1.4%3A3773&token=APP99", None).unwrap();
        assert_eq!(app_link, PairingLink { http_base: "http://192.168.1.4:3773".into(), token: "APP99".into() });
        assert_eq!(parse_pairing_link("localhost:3773", None), Err(PairingError::MissingToken));
        assert_eq!(parse_pairing_link("", None), Err(PairingError::Invalid));
    }

    #[test]
    fn websocket_base_swaps_scheme() {
        assert_eq!(websocket_base("https://h:1"), "wss://h:1/ws");
        assert_eq!(websocket_base("http://h"), "ws://h/ws");
    }

    #[test]
    fn link_requests_reuse_only_known_non_loopback_machine_addresses() {
        let urls = collect_known_machine_urls([
            "http://127.0.0.1:3773",
            "http://localhost:3773",
            "https://studio.ts.net:3773/",
            "https://studio.ts.net:3773/pair",
            "http://10.0.0.5:3773",
        ]);
        assert_eq!(urls, vec!["https://studio.ts.net:3773", "http://10.0.0.5:3773"]);

        let machines = vec![
            LinkableMachine {
                environment_id: "env-a".into(),
                label: "Studio".into(),
                machine: Some("mac-studio".into()),
                urls: vec!["https://studio.ts.net:3773".into(), "http://10.0.0.5:3773".into()],
                connected: true,
                connect_only: false,
                linked: false,
            },
            LinkableMachine {
                environment_id: "env-b".into(),
                label: "Lab".into(),
                machine: Some("linux".into()),
                urls: vec!["http://10.0.0.5:3773".into()],
                connected: false,
                connect_only: false,
                linked: false,
            },
        ];
        // Unique match reuses the machine's known URLs.
        assert_eq!(
            resolve_link_request_target(None, Some("https://studio.ts.net:3773/ws"), &machines, None),
            LinkRequestTarget::Machine {
                environment_id: "env-a".into(),
                label: "Studio".into(),
                urls: vec!["https://studio.ts.net:3773".into(), "http://10.0.0.5:3773".into()],
                linked: false,
            }
        );
        // Shared private IP across two machines falls back to Address so codes aren't misrouted.
        assert_eq!(
            resolve_link_request_target(None, Some("http://10.0.0.5:3773"), &machines, None),
            LinkRequestTarget::Address
        );
        // Unknown URL falls back to Address.
        assert_eq!(
            resolve_link_request_target(None, Some("https://unknown.example:3773"), &machines, None),
            LinkRequestTarget::Address
        );
    }

    #[test]
    fn tailscale_serve_direct_endpoint_makes_loopback_server_network_accessible() {
        assert!(!is_server_network_accessible(Some("loopback-browser"), 0));
        assert!(is_server_network_accessible(Some("loopback-browser"), 1));
        assert!(is_server_network_accessible(Some("remote-reachable"), 0));
    }

    #[test]
    fn access_environment_resolution_falls_back_to_saved_when_local_is_off() {
        let envs = vec![
            AccessEnvironmentCandidate { id: "remote-1".into(), connected: false },
            AccessEnvironmentCandidate { id: "remote-2".into(), connected: true },
        ];
        // Primary (local) is off / unlisted -> picks connected remote.
        assert_eq!(resolve_access_environment_id(Some("local"), &envs, None), Some("remote-2"));
        // Explicit selection wins while listed.
        assert_eq!(resolve_access_environment_id(Some("local"), &envs, Some("remote-1")), Some("remote-1"));
        // Stale explicit selection falls back to connected.
        assert_eq!(resolve_access_environment_id(Some("local"), &envs, Some("removed")), Some("remote-2"));
    }

    #[test]
    fn detects_legacy_pre_split_permissions() {
        let legacy = AuthSessionState {
            authenticated: true,
            permissions: Some(vec!["orchestration:read".into(), "orchestration:operate".into()]),
            ..Default::default()
        };
        assert!(session_has_legacy_permissions(&legacy));
        let modern = AuthSessionState {
            authenticated: true,
            permissions: Some(vec!["orchestration:read".into(), "filesystem:read".into()]),
            ..Default::default()
        };
        assert!(!session_has_legacy_permissions(&modern));
        let unauth = AuthSessionState { authenticated: false, ..legacy };
        assert!(!session_has_legacy_permissions(&unauth));
    }

    #[test]
    fn peer_pairing_code_payloads_errors_and_link_machine_select_helpers_match_web() {
        let peer_body = peer_pairing_code_request_body("read-only");
        assert_eq!(peer_body, json!({
            "label": PEER_PAIRING_CODE_LABEL,
            "access": "read-only",
            "purpose": PEER_LINK_PURPOSE,
        }));

        let full_body = peer_pairing_code_request_body("full-access");
        assert_eq!(full_body, json!({
            "label": PEER_PAIRING_CODE_LABEL,
            "access": "full-access",
            "purpose": PEER_LINK_PURPOSE,
        }));

        let approval_body = mcp_approval_code_request_body("approval-required");
        assert_eq!(approval_body, json!({ "label": MCP_APPROVAL_CODE_LABEL, "access": "approval-required" }));

        let decoded = decode_pairing_credential_result(&json!({
            "id": "link-1",
            "credential": "AUTO-abc123",
            "expiresAt": "2026-11-19T10:00:00.000Z",
            "label": "App sign-in",
            "purpose": "peer-link",
            "access": "approval-required"
        }))
        .expect("decoded credential");
        assert_eq!(decoded.credential, "AUTO-abc123");
        assert_eq!(decoded.id.as_deref(), Some("link-1"));
        assert_eq!(decoded.purpose.as_deref(), Some("peer-link"));
        assert_eq!(decoded.access.as_deref(), Some("approval-required"));
        assert_eq!(validate_peer_link_credential(&decoded, "approval-required").as_deref(), Ok("AUTO-abc123"));
        assert_eq!(
            validate_peer_link_credential(&decoded, "full-access").unwrap_err().reason,
            PeerPairingCodeErrorReason::Unavailable
        );
        let without_purpose = decode_pairing_credential_result(&json!({
            "credential": "AUTO-old",
            "access": "approval-required"
        }))
        .expect("decoded without purpose");
        assert_eq!(
            validate_peer_link_credential(&without_purpose, "approval-required").unwrap_err().reason,
            PeerPairingCodeErrorReason::Unavailable
        );
        assert_eq!(decode_pairing_credential_result(&json!({ "credential": "   " })), None);

        assert_eq!(
            PeerPairingCodeError::not_allowed(None).to_string(),
            "Your session there cannot create pairing codes. Paste a code from there instead."
        );
        assert_eq!(
            PeerPairingCodeError::unavailable(None).to_string(),
            "Could not get a pairing code from there. Paste a code from there instead."
        );

        let linked = LinkableMachine {
            environment_id: "e1".into(),
            label: "Studio".into(),
            machine: Some("mac-studio".into()),
            urls: vec!["https://studio.ts.net".into()],
            connected: false,
            connect_only: true,
            linked: true,
        };
        let connected = LinkableMachine { environment_id: "e2".into(), label: "Mini".into(), linked: false, connected: true, connect_only: false, ..linked.clone() };
        let connect_only = LinkableMachine { environment_id: "e3".into(), label: "Cloud".into(), linked: false, connected: false, connect_only: true, ..linked.clone() };
        let offline = LinkableMachine { environment_id: "e4".into(), label: "Offline".into(), linked: false, connected: false, connect_only: false, ..linked.clone() };
        assert_eq!(link_machine_badge(&linked), Some(LinkMachineBadge::Linked));
        assert_eq!(link_machine_badge(&connected), None);
        assert_eq!(link_machine_badge(&connect_only), Some(LinkMachineBadge::T3Connect));
        assert_eq!(link_machine_badge(&offline), Some(LinkMachineBadge::NotConnected));

        let all = [linked, connected, connect_only, offline];
        assert_eq!(link_machine_select_label(&all, None), "Choose a machine");
        assert_eq!(link_machine_select_label(&all, Some(ANOTHER_ADDRESS)), "Another address\u{2026}");
        assert_eq!(link_machine_select_label(&all, Some("e2")), "Mini");
        assert_eq!(link_machine_select_label(&all, Some("missing")), "Choose a machine");
    }
}
