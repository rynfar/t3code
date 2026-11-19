//! Ports shared/connectAuth.ts, relayAuth.ts and server/cloud/CliTokenManager.ts.
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}
pub fn authorize_url(hosted: &str, state: &str, challenge: &str, port: u16) -> Result<String, url::ParseError> {
    let mut url = url::Url::parse(hosted)?.join("/connect")?;
    let fragment = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("state", state)
        .append_pair("challenge", challenge)
        .append_pair("port", &port.to_string())
        .finish();
    url.set_fragment(Some(&fragment));
    Ok(url.into())
}
pub fn clerk_origin(key: &str) -> Option<String> {
    let encoded = key.strip_prefix("pk_live_").or_else(|| key.strip_prefix("pk_test_"))?;
    let decoded = String::from_utf8(STANDARD.decode(encoded).ok()?).ok()?;
    let host = decoded.strip_suffix('$').unwrap_or(&decoded);
    if host.is_empty() || host.contains(['/', '?', '#', '@', ':']) {
        return None;
    }
    let url = url::Url::parse(&format!("https://{host}")).ok()?;
    Some(url.origin().ascii_serialization())
}
#[derive(Debug, PartialEq, Eq)]
pub enum Callback {
    Invalid,
    Denied,
    Code(String),
}
pub fn callback(target: &str, expected_state: &str) -> Callback {
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Callback::Invalid;
    };
    let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
    if url.path() != "/callback" || params.get("state").map(|s| s.as_ref()) != Some(expected_state) {
        return Callback::Invalid;
    }
    if params.contains_key("error") {
        return Callback::Denied;
    }
    match params.get("code").filter(|s| !s.is_empty()) {
        Some(code) => Callback::Code(code.to_string()),
        None => Callback::Invalid,
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_epoch_ms: u64,
    pub identity: Option<String>,
}
impl Token {
    pub fn needs_refresh(&self, now: u64) -> bool {
        self.expires_at_epoch_ms <= now.saturating_add(300_000)
    }
}
#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub expires_in: u64,
    pub token_type: String,
}
impl TokenResponse {
    pub fn into_token(self, previous: Option<&Token>, now: u64) -> Result<Token, &'static str> {
        if self.access_token.is_empty() || !self.token_type.eq_ignore_ascii_case("bearer") || self.expires_in == 0 {
            return Err("Invalid T3 Connect token response.");
        }
        let identity = self
            .id_token
            .as_deref()
            .and_then(identity)
            .or_else(|| previous.and_then(|p| p.identity.clone()));
        Ok(Token {
            access_token: self.access_token,
            refresh_token: self.refresh_token.or_else(|| previous.map(|p| p.refresh_token.clone())).unwrap_or_default(),
            expires_at_epoch_ms: now.saturating_add(self.expires_in.saturating_mul(1000)),
            identity,
        })
    }
}
fn identity(jwt: &str) -> Option<String> {
    let claims: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(jwt.split('.').nth(1)?).ok()?).ok()?;
    ["email", "preferred_username", "sub"]
        .into_iter()
        .find_map(|k| claims[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned))
}
pub fn permanent_refresh_failure(status: u16) -> bool {
    (400..500).contains(&status) && status != 408 && status != 429
}

const BASE64URL_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
/// `PAIRING_CODE_ALPHABET` (`packages/contracts/src/auth.ts`).
const PAIRING_CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

/// `connectDeviceVerificationCode` (`packages/contracts/src/auth.ts`, `pingdotgg/t3code#36419`):
/// short `XXXX-XXXX` code derived from a T3 Connect device's proof key thumbprint.
pub fn connect_device_verification_code(proof_key_thumbprint: &str) -> String {
    let mut code = String::with_capacity(9);
    for (index, byte) in proof_key_thumbprint.bytes().take(8).enumerate() {
        if index == 4 {
            code.push('-');
        }
        let pos = BASE64URL_ALPHABET.iter().position(|candidate| *candidate == byte).unwrap_or(0);
        let mapped = PAIRING_CODE_ALPHABET[pos % PAIRING_CODE_ALPHABET.len()] as char;
        code.push(mapped);
    }
    if code.len() < 4 {
        code.push('-');
    }
    code
}

/// Client metadata reported by a T3 Connect device (`AuthClientMetadata`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConnectDeviceClient {
    pub label: Option<String>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub device_type: Option<String>,
    pub os: Option<String>,
    pub browser: Option<String>,
}

/// Pending T3 Connect device request embedded on a pairing link (`AuthConnectDeviceRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectDeviceRequest {
    pub verification_code: String,
    #[serde(default)]
    pub client: Option<ConnectDeviceClient>,
}

/// One pending T3 Connect device approval entry from `subscribeAuthAccess`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingConnectDeviceRequest {
    pub pairing_link_id: String,
    pub expires_at: String,
    pub verification_code: String,
    pub client: Option<ConnectDeviceClient>,
}

/// A T3 Connect device key trusted by this environment (`AuthConnectDevice`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectDevice {
    pub proof_key_thumbprint: String,
    pub verification_code: String,
    #[serde(default)]
    pub client: Option<ConnectDeviceClient>,
    pub approved_at: String,
}

/// Tracks pending T3 Connect device approval requests and trusted Connect devices from
/// `subscribeAuthAccess` stream items (`pingdotgg/t3code#36419`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectDevicesState {
    pub pending_requests: Vec<PendingConnectDeviceRequest>,
    pub devices: Vec<ConnectDevice>,
    pub supported: bool,
}

impl ConnectDevicesState {
    pub fn apply(&mut self, item: &serde_json::Value) {
        let payload = item.get("payload").unwrap_or(&serde_json::Value::Null);
        match item.get("type").and_then(serde_json::Value::as_str) {
            Some("snapshot") => {
                self.pending_requests = payload
                    .get("pairingLinks")
                    .and_then(serde_json::Value::as_array)
                    .map(|links| links.iter().filter_map(parse_pending_request).collect())
                    .unwrap_or_default();
                if let Some(devices) = payload.get("connectDevices").and_then(serde_json::Value::as_array) {
                    self.supported = true;
                    self.devices = devices
                        .iter()
                        .filter_map(|value| serde_json::from_value::<ConnectDevice>(value.clone()).ok())
                        .collect();
                } else {
                    self.devices.clear();
                }
            }
            Some("pairingLinkUpserted") => {
                if let Some(id) = payload.get("id").and_then(serde_json::Value::as_str) {
                    self.pending_requests.retain(|req| req.pairing_link_id != id);
                    if let Some(request) = parse_pending_request(payload) {
                        self.supported = true;
                        self.pending_requests.push(request);
                    }
                }
            }
            Some("pairingLinkRemoved") => {
                if let Some(id) = payload.get("id").and_then(serde_json::Value::as_str) {
                    self.pending_requests.retain(|req| req.pairing_link_id != id);
                }
            }
            _ => {}
        }
    }

    /// Pending Connect device requests that have not yet expired.
    pub fn visible_pending_requests(&self, now_ms: i64) -> Vec<&PendingConnectDeviceRequest> {
        self.pending_requests
            .iter()
            .filter(|req| crate::sidebar::time::parse_iso(&req.expires_at).is_some_and(|at| at > now_ms))
            .collect()
    }
}

fn parse_pending_request(link: &serde_json::Value) -> Option<PendingConnectDeviceRequest> {
    let id = link.get("id")?.as_str()?.to_owned();
    let expires_at = link.get("expiresAt")?.as_str()?.to_owned();
    let request: ConnectDeviceRequest = serde_json::from_value(link.get("connectDeviceRequest")?.clone()).ok()?;
    Some(PendingConnectDeviceRequest {
        pairing_link_id: id,
        expires_at,
        verification_code: request.verification_code,
        client: request.client,
    })
}

/// `connectDeviceName` (`ConnectionsSettings.logic.ts`): short name for a Connect device.
pub fn connect_device_name(client: Option<&ConnectDeviceClient>) -> String {
    let Some(client) = client else {
        return "this device".to_owned();
    };
    [client.label.as_deref(), client.os.as_deref(), client.browser.as_deref()]
        .into_iter()
        .flatten()
        .find(|value| !value.trim().is_empty())
        .map_or_else(|| "this device".to_owned(), str::to_owned)
}

/// `describeConnectDeviceClient` (`ConnectionsSettings.logic.ts`): deduplicated client summary.
pub fn describe_connect_device_client(client: Option<&ConnectDeviceClient>) -> String {
    let Some(client) = client else {
        return "Unknown device".to_owned();
    };
    let mut parts: Vec<&str> = Vec::new();
    for candidate in [
        client.label.as_deref(),
        client.os.as_deref(),
        client.browser.as_deref(),
        client.ip_address.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !candidate.is_empty() && !parts.contains(&candidate) {
            parts.push(candidate);
        }
    }
    if parts.is_empty() { "Unknown device".to_owned() } else { parts.join(" \u{00b7} ") }
}

/// Recognizes HTTP 403 `EnvironmentConnectApprovalPendingError` payloads (`pingdotgg/t3code#36419`).
pub fn is_connect_approval_pending_error(body: &serde_json::Value) -> bool {
    body.get("_tag").and_then(serde_json::Value::as_str) == Some("EnvironmentConnectApprovalPendingError")
        || body.get("code").and_then(serde_json::Value::as_str) == Some("connect_approval_pending")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pkce_and_fragment_match_the_cli() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(
            authorize_url("https://app.t3.codes", "s", "c", 34338).unwrap(),
            "https://app.t3.codes/connect#state=s&challenge=c&port=34338"
        );
        assert_eq!(callback("/callback?state=s&code=c", "s"), Callback::Code("c".into()));
        for path in ["/callback?state=other&code=c", "/callback?state=s", "/wrong?state=s&code=c"] {
            assert_eq!(callback(path, "s"), Callback::Invalid);
        }
        assert_eq!(callback("/callback?state=s&error=access_denied", "s"), Callback::Denied);
        assert_eq!(clerk_origin("pk_live_Y2xlcmsudDMuY29kZXMk").as_deref(), Some("https://clerk.t3.codes"));
        for key in ["pk_live_", "wrong", "pk_live_aHR0cHM6Ly9iYWQ="] {
            assert_eq!(clerk_origin(key), None);
        }
    }
    #[test]
    fn token_decoding_preserves_rotation_and_identity_without_exposing_claims_as_auth() {
        let payload = encode(br#"{"email":"trevor@example.test","sub":"user"}"#);
        let response = serde_json::json!({"access_token":"access","refresh_token":"refresh","id_token":format!("a.{payload}.b"),"expires_in":3600,"token_type":"Bearer","future":true});
        let token = serde_json::from_value::<TokenResponse>(response).unwrap().into_token(None, 1000).unwrap();
        assert_eq!(token.identity.as_deref(), Some("trevor@example.test"));
        let next = serde_json::from_str::<TokenResponse>(r#"{"access_token":"next","expires_in":3600,"token_type":"Bearer"}"#)
            .unwrap()
            .into_token(Some(&token), 2000)
            .unwrap();
        assert_eq!(
            (next.refresh_token.as_str(), next.identity.as_deref()),
            ("refresh", Some("trevor@example.test"))
        );
        assert!(!token.needs_refresh(1001));
        assert!(token.needs_refresh(3_301_000));
        for status in [400, 401, 403] {
            assert!(permanent_refresh_failure(status));
        }
        for status in [408, 429, 500] {
            assert!(!permanent_refresh_failure(status));
        }
        assert!(serde_json::from_str::<TokenResponse>(r#"{"access_token":"a"}"#).is_err());
    }

    #[test]
    fn connect_device_verification_code_and_access_stream_state() {
        let code = connect_device_verification_code("ABCDEFGHijkl");
        assert_eq!(code, "2345-6789");
        assert!(is_connect_approval_pending_error(&serde_json::json!({
            "_tag": "EnvironmentConnectApprovalPendingError",
            "code": "connect_approval_pending"
        })));
        assert!(!is_connect_approval_pending_error(&serde_json::json!({ "_tag": "EnvironmentAuthInvalidError" })));

        let mut state = ConnectDevicesState::default();
        state.apply(&serde_json::json!({
            "type": "snapshot",
            "payload": {
                "pairingLinks": [
                    {
                        "id": "req-1",
                        "createdAt": "2026-11-18T00:00:00.000Z",
                        "expiresAt": "2099-01-01T00:00:00.000Z",
                        "connectDeviceRequest": {
                            "verificationCode": "2345-6789",
                            "client": { "label": "iPhone", "os": "iOS", "browser": "iOS" }
                        }
                    }
                ],
                "connectDevices": [
                    {
                        "proofKeyThumbprint": "thumb-1",
                        "verificationCode": "ABCD-EFGH",
                        "approvedAt": "2026-11-18T00:00:00.000Z",
                        "client": { "os": "macOS", "browser": "Safari" }
                    }
                ]
            }
        }));
        assert!(state.supported);
        assert_eq!(state.visible_pending_requests(0).len(), 1);
        assert_eq!(describe_connect_device_client(state.pending_requests[0].client.as_ref()), "iPhone \u{00b7} iOS");
        assert_eq!(connect_device_name(state.devices[0].client.as_ref()), "macOS");

        state.apply(&serde_json::json!({
            "type": "pairingLinkRemoved",
            "payload": { "id": "req-1" }
        }));
        assert!(state.pending_requests.is_empty());
    }
}
