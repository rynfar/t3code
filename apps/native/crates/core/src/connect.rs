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
fn jwt_claims(jwt: &str) -> Option<serde_json::Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}
fn identity(jwt: &str) -> Option<String> {
    let claims = jwt_claims(jwt)?;
    ["email", "preferred_username", "sub"]
        .into_iter()
        .find_map(|k| claims[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned))
}
pub fn jwt_exp_epoch_ms(jwt: &str) -> Option<u64> {
    let exp = jwt_claims(jwt)?.get("exp")?.as_u64()?;
    Some(exp.saturating_mul(1000))
}
pub fn jwt_sub(jwt: &str) -> Option<String> {
    jwt_claims(jwt)?
        .get("sub")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
pub fn permanent_refresh_failure(status: u16) -> bool {
    (400..500).contains(&status) && status != 408 && status != 429
}

/// Authorized Clerk Native OAuth callback URL scheme and redirect URL (`clerk.t3.codes`).
pub const CLERK_NATIVE_CALLBACK_SCHEME: &str = "com.t3tools.t3code";
pub const CLERK_NATIVE_CALLBACK_URL: &str = "com.t3tools.t3code://callback";

/// Persisted Clerk Native Frontend API (`_is_native=1`) session stored in `cloud-clerk.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClerkSession {
    pub client_token: String,
    pub session_id: String,
    #[serde(default)]
    pub identity: Option<String>,
}

/// Supported Clerk Native social OAuth providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocialProvider {
    Google,
    GitHub,
    Apple,
    Microsoft,
}

impl SocialProvider {
    pub const ALL: [Self; 4] = [Self::Google, Self::GitHub, Self::Apple, Self::Microsoft];

    pub fn strategy(self) -> &'static str {
        match self {
            Self::Google => "oauth_google",
            Self::GitHub => "oauth_github",
            Self::Apple => "oauth_apple",
            Self::Microsoft => "oauth_microsoft",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::GitHub => "GitHub",
            Self::Apple => "Apple",
            Self::Microsoft => "Microsoft",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstFactorKind {
    EmailCode,
    Password,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecondFactorKind {
    Totp,
    BackupCode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailSignInChallenge {
    pub sign_in_id: String,
    pub identifier: String,
    pub supports_email_code: bool,
    pub supports_password: bool,
    pub email_address_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondFactorChallenge {
    pub sign_in_id: String,
    pub supports_totp: bool,
    pub supports_backup_code: bool,
}

/// Parses the native OAuth callback URL (`com.t3tools.t3code://callback?...`), returning
/// `Ok(rotating_token_nonce)` or an error if the callback is invalid or denied.
pub fn parse_oauth_callback(raw_url: &str) -> Result<Option<String>, String> {
    let parsed = url::Url::parse(raw_url).map_err(|_| "Invalid T3 Connect callback URL.".to_owned())?;
    if !parsed.scheme().eq_ignore_ascii_case(CLERK_NATIVE_CALLBACK_SCHEME) {
        return Err("Unexpected T3 Connect callback URL scheme.".to_owned());
    }
    let params: std::collections::HashMap<_, _> = parsed.query_pairs().collect();
    if let Some(error) = params.get("error").filter(|v| !v.trim().is_empty()) {
        if let Some(description) = params.get("error_description").filter(|v| !v.trim().is_empty()) {
            return Err(description.to_string());
        }
        return Err(format!("T3 Connect sign-in failed ({error})."));
    }
    Ok(params
        .get("rotating_token_nonce")
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty()))
}

/// Extracts the most descriptive Clerk Frontend API error message from a JSON response.
pub fn extract_clerk_error(value: &serde_json::Value) -> Option<String> {
    let first = value.get("errors")?.as_array()?.first()?;
    for key in ["long_message", "message"] {
        if let Some(text) = first.get(key).and_then(serde_json::Value::as_str).map(str::trim)
            && !text.is_empty()
        {
            return Some(text.to_owned());
        }
    }
    None
}

/// Extracts `response.first_factor_verification.external_verification_redirect_url` from a Clerk
/// `POST /v1/client/sign_ins` OAuth response.
pub fn extract_oauth_redirect_url(value: &serde_json::Value) -> Option<String> {
    let url = value
        .get("response")
        .unwrap_or(value)
        .get("first_factor_verification")?
        .get("external_verification_redirect_url")?
        .as_str()?
        .trim();
    (!url.is_empty()).then(|| url.to_owned())
}

/// Whether Clerk's `/v1/client` response indicates an OAuth account needs transfer to a new sign-up.
pub fn is_oauth_transferable(value: &serde_json::Value) -> bool {
    let client = value.get("response").or_else(|| value.get("client")).unwrap_or(value);
    client
        .get("sign_in")
        .and_then(|si| si.get("first_factor_verification"))
        .and_then(|v| v.get("status"))
        .and_then(serde_json::Value::as_str)
        == Some("transferable")
}

/// Parses an active session and user identity from a Clerk `/v1/client` or sign-in response.
pub fn extract_clerk_session(value: &serde_json::Value, client_token: &str) -> Option<ClerkSession> {
    if client_token.trim().is_empty() {
        return None;
    }
    let client = value.get("client").or_else(|| value.get("response")).unwrap_or(value);
    let sessions = client.get("sessions").and_then(serde_json::Value::as_array);
    let last_active = client
        .get("last_active_session_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty());
    let created_session_id = value
        .get("response")
        .and_then(|r| r.get("created_session_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty());
    let session_obj = sessions.and_then(|list| {
        last_active
            .or(created_session_id)
            .and_then(|wanted| {
                list.iter()
                    .find(|s| s.get("id").and_then(serde_json::Value::as_str) == Some(wanted))
            })
            .or_else(|| {
                list.iter()
                    .find(|s| s.get("status").and_then(serde_json::Value::as_str) == Some("active"))
            })
            .or_else(|| list.first())
    });
    let session_id = session_obj
        .and_then(|s| s.get("id"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .or(last_active)
        .or(created_session_id)?
        .to_owned();
    let identity = session_obj.and_then(extract_session_identity);
    Some(ClerkSession {
        client_token: client_token.to_owned(),
        session_id,
        identity,
    })
}

fn extract_session_identity(session: &serde_json::Value) -> Option<String> {
    if let Some(user) = session.get("user") {
        let primary_id = user.get("primary_email_address_id").and_then(serde_json::Value::as_str);
        if let Some(emails) = user.get("email_addresses").and_then(serde_json::Value::as_array) {
            if let Some(primary) = primary_id.and_then(|wanted| {
                emails.iter().find(|e| e.get("id").and_then(serde_json::Value::as_str) == Some(wanted))
            }) && let Some(addr) = primary
                .get("email_address")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(addr.to_owned());
            }
            if let Some(addr) = emails.iter().find_map(|e| {
                e.get("email_address")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            }) {
                return Some(addr.to_owned());
            }
        }
        if let Some(username) = user
            .get("username")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(username.to_owned());
        }
        let first = user
            .get("first_name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let last = user
            .get("last_name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let full = [first, last]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !full.is_empty() {
            return Some(full);
        }
    }
    session
        .get("public_user_data")
        .and_then(|d| d.get("identifier"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Parses `POST /v1/client/sign_ins` for an email identifier into its supported first factors.
pub fn parse_email_sign_in_challenge(
    value: &serde_json::Value,
    identifier: &str,
) -> Result<EmailSignInChallenge, String> {
    let response = value.get("response").unwrap_or(value);
    let sign_in_id = response
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Invalid Clerk sign-in response.".to_owned())?
        .to_owned();
    let factors = response
        .get("supported_first_factors")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut supports_email_code = false;
    let mut supports_password = false;
    let mut email_address_id = None;
    for factor in factors {
        match factor.get("strategy").and_then(serde_json::Value::as_str) {
            Some("email_code") => {
                supports_email_code = true;
                if email_address_id.is_none() {
                    email_address_id = factor
                        .get("email_address_id")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                }
            }
            Some("password") => {
                supports_password = true;
            }
            _ => {}
        }
    }
    if !supports_email_code && !supports_password {
        return Err("This account does not support email code or password sign-in. Try a social provider.".to_owned());
    }
    Ok(EmailSignInChallenge {
        sign_in_id,
        identifier: identifier.trim().to_owned(),
        supports_email_code,
        supports_password,
        email_address_id,
    })
}

/// Parses a `needs_second_factor` state from either a sign-in response or a `/v1/client` snapshot.
pub fn parse_second_factor_challenge(value: &serde_json::Value) -> Option<SecondFactorChallenge> {
    let response = value.get("response").unwrap_or(value);
    let sign_in = if response.get("status").and_then(serde_json::Value::as_str) == Some("needs_second_factor") {
        response
    } else {
        response
            .get("sign_in")
            .or_else(|| value.get("client").and_then(|c| c.get("sign_in")))?
    };
    if sign_in.get("status").and_then(serde_json::Value::as_str) != Some("needs_second_factor") {
        return None;
    }
    let sign_in_id = sign_in
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_owned();
    let factors = sign_in
        .get("supported_second_factors")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut supports_totp = false;
    let mut supports_backup_code = false;
    for factor in factors {
        match factor.get("strategy").and_then(serde_json::Value::as_str) {
            Some("totp") => supports_totp = true,
            Some("backup_code") => supports_backup_code = true,
            _ => {}
        }
    }
    if !supports_totp && !supports_backup_code {
        supports_totp = true;
    }
    Some(SecondFactorChallenge {
        sign_in_id,
        supports_totp,
        supports_backup_code,
    })
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

    #[test]
    fn clerk_native_callback_jwt_and_session_parsing() {
        assert_eq!(
            parse_oauth_callback("com.t3tools.t3code://callback?rotating_token_nonce=nonce-123").unwrap(),
            Some("nonce-123".into())
        );
        assert_eq!(parse_oauth_callback("com.t3tools.t3code://callback").unwrap(), None);
        assert!(parse_oauth_callback("https://evil.example/callback").is_err());
        assert_eq!(
            parse_oauth_callback("com.t3tools.t3code://callback?error=access_denied&error_description=User+cancelled")
                .unwrap_err(),
            "User cancelled"
        );

        let payload = encode(br#"{"sub":"user_42","exp":1793000123}"#);
        let jwt = format!("hdr.{payload}.sig");
        assert_eq!(jwt_exp_epoch_ms(&jwt), Some(1_793_000_123_000));
        assert_eq!(jwt_sub(&jwt).as_deref(), Some("user_42"));

        let oauth_resp = serde_json::json!({
            "response": {
                "first_factor_verification": {
                    "external_verification_redirect_url": "https://accounts.google.com/o/oauth2/auth?x=1"
                }
            }
        });
        assert_eq!(
            extract_oauth_redirect_url(&oauth_resp).as_deref(),
            Some("https://accounts.google.com/o/oauth2/auth?x=1")
        );

        let client_resp = serde_json::json!({
            "response": {
                "last_active_session_id": "sess_2",
                "sessions": [
                    { "id": "sess_1", "status": "ended" },
                    {
                        "id": "sess_2",
                        "status": "active",
                        "user": {
                            "primary_email_address_id": "em_2",
                            "email_addresses": [
                                { "id": "em_1", "email_address": "alt@example.test" },
                                { "id": "em_2", "email_address": "primary@example.test" }
                            ]
                        }
                    }
                ]
            }
        });
        let session = extract_clerk_session(&client_resp, "tok_abc").expect("session");
        assert_eq!(session.session_id, "sess_2");
        assert_eq!(session.client_token, "tok_abc");
        assert_eq!(session.identity.as_deref(), Some("primary@example.test"));

        let email_resp = serde_json::json!({
            "response": {
                "id": "sia_1",
                "supported_first_factors": [
                    { "strategy": "email_code", "email_address_id": "idn_1" },
                    { "strategy": "password" }
                ]
            }
        });
        let challenge = parse_email_sign_in_challenge(&email_resp, "dev@example.test").unwrap();
        assert_eq!(challenge.sign_in_id, "sia_1");
        assert!(challenge.supports_email_code);
        assert!(challenge.supports_password);
        assert_eq!(challenge.email_address_id.as_deref(), Some("idn_1"));

        let second_factor_resp = serde_json::json!({
            "response": {
                "id": "sia_1",
                "status": "needs_second_factor",
                "supported_second_factors": [
                    { "strategy": "totp" },
                    { "strategy": "backup_code" }
                ]
            }
        });
        let sf = parse_second_factor_challenge(&second_factor_resp).expect("2FA challenge");
        assert_eq!(sf.sign_in_id, "sia_1");
        assert!(sf.supports_totp);
        assert!(sf.supports_backup_code);

        let clerk_err = serde_json::json!({
            "errors": [{ "message": "Short", "long_message": "Invalid verification code." }]
        });
        assert_eq!(extract_clerk_error(&clerk_err).as_deref(), Some("Invalid verification code."));
    }
}
