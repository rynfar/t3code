//! Pairing and socket authorization, as `client-runtime/src/authorization/{remote,service}.ts`
//! and `client-runtime/src/connection/onboarding.ts` do it:
//! - Older hosts (without `capabilities.accessTokenRenewal`): pairing token → `POST /oauth/token`
//!   → Bearer access token → `POST /api/auth/websocket-ticket/rpc` → `wsTicket`.
//! - Hosts advertising `capabilities.accessTokenRenewal`: pairing token + `DPoP` proof from the
//!   device's P-256 key → `DPoP` access token stored in `crate::dpop` → automatic renewal via
//!   `subject_token_type=urn:ietf:params:oauth:token-type:access_token` (`pingdotgg/t3code#37980`,
//!   `pingdotgg/t3code#37990`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use t3_core::auth::AuthSessionState;
pub use t3_core::compatibility::{
    CLIENT_CAPABILITIES_QUERY_PARAM, ORCHESTRATION_PROTOCOL_QUERY_PARAM, ORCHESTRATION_PROTOCOL_VERSION,
    SETTINGS_SECRET_REFERENCES_CAPABILITY,
};
use t3_core::compatibility::{append_orchestration_protocol, protocol_mismatch};
use t3_core::pairing::{
    MINT_PAIRING_CODE_TIMEOUT_MS, PairingCredentialResult, PairingLink, PeerPairingCodeError,
    decode_pairing_credential_result, mcp_approval_code_request_body, peer_pairing_code_request_body,
    validate_peer_link_credential, websocket_base,
};
use t3_core::server_config::EnvironmentDescriptor;

use crate::dpop::{
    self, DPOP_ACCESS_TOKEN_REFRESH_SKEW_MS, DpopConnectionCredential, now_epoch_ms,
};

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const BOOTSTRAP_TOKEN_TYPE: &str = "urn:t3:params:oauth:token-type:environment-bootstrap";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
/// `WELL_KNOWN_ENVIRONMENT_PATH`: the unauthenticated descriptor every server answers.
const DESCRIPTOR_PATH: &str = "/.well-known/t3/environment";
/// `SOCKET_FRAME_PARTS_QUERY_PARAM` (`packages/shared/src/socketFrameParts.ts`, `pingdotgg/t3code#23758`).
pub const SOCKET_FRAME_PARTS_QUERY_PARAM: &str = "frameParts";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub http_base: String,
    pub access_token: String,
}

/// Capabilities read from `GET /.well-known/t3/environment` (`ExecutionEnvironmentCapabilities`)
/// without altering `t3_core::server_config::EnvironmentDescriptor`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DescriptorCapabilities {
    /// `accessTokenRenewal` (`pingdotgg/t3code#37980`, `pingdotgg/t3code#37990`).
    pub access_token_renewal: bool,
    /// `peerLinkPairingCodes` (`pingdotgg/t3code#37825`).
    pub peer_link_pairing_codes: bool,
}

fn descriptor_caps_cache() -> &'static Mutex<HashMap<String, DescriptorCapabilities>> {
    static CACHE: OnceLock<Mutex<HashMap<String, DescriptorCapabilities>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn normalize_base(http_base: &str) -> String {
    http_base.trim().trim_end_matches('/').to_owned()
}

pub fn descriptor_capabilities(http_base: &str) -> Option<DescriptorCapabilities> {
    descriptor_caps_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&normalize_base(http_base))
        .copied()
}

pub fn record_descriptor_capabilities(http_base: &str, caps: DescriptorCapabilities) {
    descriptor_caps_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(normalize_base(http_base), caps);
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("{0} rejected the pairing token. Create a new pairing link and try again.")]
    Rejected(String),
    #[error("This device is no longer authorized. Pair again.")]
    Unauthorized,
    /// Host refused this device's credential, carrying optional trace ID and clock-skew flag
    /// (`pingdotgg/t3code#38041`, `pingdotgg/t3code#33199`, `pingdotgg/t3code#37990`).
    #[error("{}", format_auth_refused(.message, .trace_id.as_deref()))]
    AuthRefused {
        message: String,
        trace_id: Option<String>,
        clock_skew: bool,
    },
    /// HTTP 403 `EnvironmentConnectApprovalPendingError` (`pingdotgg/t3code#36419`):
    /// waiting for someone on the host to approve this device; credentials remain valid and retry.
    #[error("{message}")]
    ConnectApprovalPending { message: String, trace_id: Option<String> },
    #[error("Could not reach {host}: {source}")]
    Unreachable { host: String, source: reqwest::Error },
    #[error("Unexpected response from {host} ({status}): {body}")]
    Unexpected { host: String, status: u16, body: String },
    /// The server speaks another orchestration protocol; the message says which side to update.
    #[error("{0}")]
    Incompatible(String),
}

fn format_auth_refused(message: &str, trace_id: Option<&str>) -> String {
    match trace_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(trace) => format!("{message} (Trace ID: {trace})"),
        None => message.to_owned(),
    }
}

impl AuthError {
    /// True when the host itself refused this device's credential (not a clock-skew rejection)
    /// (`isHostCredentialRefusal` in `packages/client-runtime/src/connection/errors.ts`, `pingdotgg/t3code#33199`).
    pub fn is_host_credential_refusal(&self) -> bool {
        matches!(
            self,
            AuthError::Unauthorized
                | AuthError::Rejected(_)
                | AuthError::AuthRefused { clock_skew: false, .. }
        )
    }

    pub fn trace_id(&self) -> Option<&str> {
        match self {
            AuthError::AuthRefused { trace_id, .. } | AuthError::ConnectApprovalPending { trace_id, .. } => {
                trace_id.as_deref()
            }
            _ => None,
        }
    }
}

/// A redeemed pairing and, when the server answered it, its descriptor (label, id, version).
#[derive(Debug, Clone)]
pub struct Paired {
    pub credentials: Credentials,
    pub descriptor: Option<EnvironmentDescriptor>,
}

#[derive(Deserialize)]
struct AccessTokenResult {
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct TicketResult {
    ticket: String,
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest client builds")
}

/// `GET /.well-known/t3/environment`: who the server is and which protocol it speaks.
pub async fn fetch_descriptor(http_base: &str) -> Result<EnvironmentDescriptor, AuthError> {
    fetch_descriptor_timeout(http_base, std::time::Duration::from_secs(5)).await
}

/// `GET /.well-known/t3/environment` with an explicit timeout, returning both the descriptor and
/// extra capability flags (`accessTokenRenewal`, `peerLinkPairingCodes`).
pub async fn fetch_descriptor_with_capabilities(
    http_base: &str,
    timeout: std::time::Duration,
) -> Result<(EnvironmentDescriptor, DescriptorCapabilities), AuthError> {
    let host = http_base.to_owned();
    let response = http()
        .get(format!("{}{DESCRIPTOR_PATH}", http_base.trim_end_matches('/')))
        .timeout(timeout)
        .send()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Unexpected { host, status: status.as_u16(), body });
    }
    let raw: serde_json::Value = response
        .json()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    let caps = DescriptorCapabilities {
        access_token_renewal: raw
            .pointer("/capabilities/accessTokenRenewal")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        peer_link_pairing_codes: raw
            .pointer("/capabilities/peerLinkPairingCodes")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    };
    record_descriptor_capabilities(http_base, caps);
    let descriptor: EnvironmentDescriptor = serde_json::from_value(raw).unwrap_or_default();
    Ok((descriptor, caps))
}

/// `GET /.well-known/t3/environment` with an explicit timeout (used by multi-route probing).
pub async fn fetch_descriptor_timeout(
    http_base: &str,
    timeout: std::time::Duration,
) -> Result<EnvironmentDescriptor, AuthError> {
    fetch_descriptor_with_capabilities(http_base, timeout)
        .await
        .map(|(descriptor, _)| descriptor)
}

/// Pairs with a link, refusing a server on another protocol before spending its token. A server
/// advertising `capabilities.accessTokenRenewal` pairs with this device's DPoP key so its access
/// token renews automatically; older hosts keep Bearer pairing (`pingdotgg/t3code#37990`).
pub async fn pair_with_link(link: &PairingLink) -> Result<Paired, AuthError> {
    let fetched = fetch_descriptor_with_capabilities(&link.http_base, std::time::Duration::from_secs(5))
        .await
        .ok();
    let (descriptor, caps) = match fetched {
        Some((descriptor, caps)) => (Some(descriptor), caps),
        None => (None, DescriptorCapabilities::default()),
    };
    if let Some(descriptor) = &descriptor
        && let Some(mismatch) = protocol_mismatch(descriptor)
    {
        return Err(AuthError::Incompatible(mismatch.message(&descriptor.label)));
    }
    // Create the DPoP proof before spending the one-time code, so a device that cannot sign
    // falls back cleanly to Bearer (`onboarding.ts`).
    let token_url = format!("{}/oauth/token", link.http_base.trim_end_matches('/'));
    let dpop_proof = if caps.access_token_renewal {
        dpop::device_key()
            .ok()
            .and_then(|key| key.create_proof("POST", &token_url, None).ok())
    } else {
        None
    };
    let env_id = descriptor
        .as_ref()
        .map(|d| d.environment_id.as_str())
        .filter(|id| !id.is_empty());
    let credentials = if let Some(proof) = dpop_proof {
        redeem_pairing_with_dpop(link, &proof, env_id).await?
    } else {
        dpop::remove_direct_credential(&link.http_base);
        redeem_pairing(link).await?
    };
    Ok(Paired { credentials, descriptor })
}

async fn redeem_pairing_with_dpop(
    link: &PairingLink,
    dpop_proof: &str,
    environment_id: Option<&str>,
) -> Result<Credentials, AuthError> {
    let host = link.http_base.clone();
    let token_url = format!("{}/oauth/token", link.http_base.trim_end_matches('/'));
    let response = http()
        .post(&token_url)
        .header("DPoP", dpop_proof)
        .form(&[
            ("grant_type", GRANT_TYPE),
            ("subject_token", link.token.as_str()),
            ("subject_token_type", BOOTSTRAP_TOKEN_TYPE),
            ("requested_token_type", ACCESS_TOKEN_TYPE),
            ("client_label", "T3 Code Native"),
            ("client_device_type", "desktop"),
            ("client_os", client_os()),
        ])
        .send()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    let status = response.status();
    if status.as_u16() == 400 || status.as_u16() == 401 {
        return Err(AuthError::Rejected(host));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Unexpected { host, status: status.as_u16(), body });
    }
    let token: AccessTokenResult = response
        .json()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    if token.token_type.as_deref().is_some_and(|t| t.eq_ignore_ascii_case("dpop")) {
        let expires_in_secs = token.expires_in.unwrap_or(3600).max(0);
        dpop::put_direct_credential(
            &link.http_base,
            DpopConnectionCredential::new(
                token.access_token.clone(),
                now_epoch_ms().saturating_add(expires_in_secs.saturating_mul(1_000)),
                environment_id.map(str::to_owned),
            ),
        );
    } else {
        dpop::remove_direct_credential(&link.http_base);
    }
    Ok(Credentials {
        http_base: link.http_base.clone(),
        access_token: token.access_token,
    })
}

pub async fn redeem_pairing(link: &PairingLink) -> Result<Credentials, AuthError> {
    let host = link.http_base.clone();
    let response = http()
        .post(format!("{}/oauth/token", link.http_base.trim_end_matches('/')))
        .form(&[
            ("grant_type", GRANT_TYPE),
            ("subject_token", link.token.as_str()),
            ("subject_token_type", BOOTSTRAP_TOKEN_TYPE),
            ("requested_token_type", ACCESS_TOKEN_TYPE),
            ("client_label", "T3 Code Native"),
            ("client_device_type", "desktop"),
            ("client_os", client_os()),
        ])
        .send()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    let status = response.status();
    if status.as_u16() == 400 || status.as_u16() == 401 {
        return Err(AuthError::Rejected(host));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Unexpected { host, status: status.as_u16(), body });
    }
    let token: AccessTokenResult = response
        .json()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    Ok(Credentials { http_base: link.http_base.clone(), access_token: token.access_token })
}

/// Renews a direct DPoP credential for `credentials` via `POST /oauth/token` with
/// `subject_token_type = urn:ietf:params:oauth:token-type:access_token` (`directDpopToken` in
/// `packages/client-runtime/src/authorization/service.ts`, `pingdotgg/t3code#37980`, `pingdotgg/t3code#37990`).
/// Requests for the same saved host share one lock without waiting on other hosts, even across
/// alternate routes or relocated ports.
pub async fn renew_direct_dpop_token(
    credentials: &Credentials,
    rejected_access_token: Option<&str>,
) -> Result<DpopConnectionCredential, AuthError> {
    let Some((store_key, _)) = dpop::find_direct_credential(&credentials.http_base, &credentials.access_token) else {
        return Err(AuthError::Unauthorized);
    };
    let lock = dpop::direct_renewal_lock(&store_key);
    let _guard = lock.lock().await;

    let Some((store_key, current)) =
        dpop::find_direct_credential(&credentials.http_base, &credentials.access_token)
    else {
        return Err(AuthError::Unauthorized);
    };
    let now = now_epoch_ms();
    if rejected_access_token != Some(current.access_token.as_str())
        && current.expires_at_epoch_ms.saturating_sub(DPOP_ACCESS_TOKEN_REFRESH_SKEW_MS) > now
    {
        return Ok(current);
    }
    let key = dpop::device_key().map_err(|message| AuthError::AuthRefused {
        message,
        trace_id: None,
        clock_skew: false,
    })?;
    let token_url = format!("{}/oauth/token", credentials.http_base.trim_end_matches('/'));
    let proof = key
        .create_proof("POST", &token_url, None)
        .map_err(|message| AuthError::AuthRefused {
            message,
            trace_id: None,
            clock_skew: false,
        })?;
    let host = credentials.http_base.clone();
    let response = http()
        .post(&token_url)
        .header("DPoP", proof)
        .form(&[
            ("grant_type", GRANT_TYPE),
            ("subject_token", current.access_token.as_str()),
            ("subject_token_type", ACCESS_TOKEN_TYPE),
            ("requested_token_type", ACCESS_TOKEN_TYPE),
        ])
        .send()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        let body = response.text().await.unwrap_or_default();
        return Err(parse_auth_refusal_error(&body, status.as_u16(), true));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Unexpected { host, status: status.as_u16(), body });
    }
    let token: AccessTokenResult = response
        .json()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    let expires_in_secs = token.expires_in.unwrap_or(3600).max(0);
    let renewed = current.renewed(
        token.access_token,
        now_epoch_ms().saturating_add(expires_in_secs.saturating_mul(1_000)),
    );
    dpop::update_direct_credential(&store_key, renewed.clone());
    Ok(renewed)
}

fn parse_auth_refusal_error(body: &str, status: u16, allow_rich_refusal: bool) -> AuthError {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if status == 403 && t3_core::connect::is_connect_approval_pending_error(&value) {
            let message = value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .filter(|m| !m.trim().is_empty())
                .unwrap_or("This device is waiting for approval on the host.")
                .to_owned();
            let trace_id = value.get("traceId").and_then(serde_json::Value::as_str).map(str::to_owned);
            return AuthError::ConnectApprovalPending { message, trace_id };
        }
        let trace_id = value
            .get("traceId")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let clock_skew = value
            .get("dpopFailureReason")
            .and_then(serde_json::Value::as_str)
            == Some("time_window");
        if allow_rich_refusal && (trace_id.is_some() || clock_skew) {
            let message = if clock_skew {
                "Your device clock is too far off from the server. Check your system time and retry.".to_owned()
            } else {
                "This device is no longer authorized. Pair again.".to_owned()
            };
            return AuthError::AuthRefused { message, trace_id, clock_skew };
        }
    }
    AuthError::Unauthorized
}

/// Resolves the HTTP `Authorization` header (and optional `DPoP` proof header) for `(method, url)`.
/// Returns `(authorization_header, dpop_proof_header, dpop_token_used)`.
async fn resolve_http_auth_headers(
    credentials: &Credentials,
    method: &str,
    url: &str,
    rejected_access_token: Option<&str>,
) -> Result<(String, Option<String>, Option<String>), AuthError> {
    if dpop::find_direct_credential(&credentials.http_base, &credentials.access_token).is_some() {
        let cred = renew_direct_dpop_token(credentials, rejected_access_token).await?;
        let key = dpop::device_key().map_err(|message| AuthError::AuthRefused {
            message,
            trace_id: None,
            clock_skew: false,
        })?;
        let proof = key
            .create_proof(method, url, Some(&cred.access_token))
            .map_err(|message| AuthError::AuthRefused {
                message,
                trace_id: None,
                clock_skew: false,
            })?;
        return Ok((
            format!("DPoP {}", cred.access_token),
            Some(proof),
            Some(cred.access_token),
        ));
    }
    Ok((format!("Bearer {}", credentials.access_token), None, None))
}

/// Error from [`send_authed_request`]: either DPoP auth/renewal failed or the HTTP transport errored.
#[derive(Debug)]
pub enum AuthedHttpError {
    Auth(AuthError),
    Http(reqwest::Error),
}

/// Sends an HTTP request built by `build`, attaching `Authorization` (`DPoP` or `Bearer`) and
/// `DPoP` proof headers. If a DPoP-authenticated request receives HTTP 401, renews the direct
/// DPoP access token once and retries with a fresh proof (`pingdotgg/t3code#37990`).
pub async fn send_authed_request<F>(
    credentials: &Credentials,
    method: &str,
    url: &str,
    mut build: F,
) -> Result<reqwest::Response, AuthedHttpError>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    let mut rejected: Option<String> = None;
    for attempt in 0..2 {
        let (auth_header, dpop_header, used_dpop_token) =
            resolve_http_auth_headers(credentials, method, url, rejected.as_deref())
                .await
                .map_err(AuthedHttpError::Auth)?;
        let mut request = build().header(reqwest::header::AUTHORIZATION, auth_header);
        if let Some(proof) = dpop_header {
            request = request.header("DPoP", proof);
        }
        let response = request.send().await.map_err(AuthedHttpError::Http)?;
        if response.status().as_u16() == 401
            && attempt == 0
            && let Some(used) = used_dpop_token
        {
            rejected = Some(used);
            continue;
        }
        return Ok(response);
    }
    unreachable!("two attempts always return on attempt 1")
}

/// Mints a single-use RPC ticket. Servers that predate ticket audiences answer 404 on the
/// `/rpc` route; their bare-path tickets are valid for either use.
pub async fn issue_rpc_ticket(credentials: &Credentials) -> Result<String, AuthError> {
    match post_ticket(credentials, "/api/auth/websocket-ticket/rpc", true).await {
        Err(AuthError::Unexpected { status: 404, .. }) => {
            post_ticket(credentials, "/api/auth/websocket-ticket", true).await
        }
        other => other,
    }
}

/// Mints a single-use `media` ticket for a socket that cannot carry a bearer header, such as the
/// preview stream (`resolveDeviceHubAccess`). Older servers answer only the bare path.
pub async fn issue_media_ticket(credentials: &Credentials) -> Result<String, AuthError> {
    match post_ticket(credentials, "/api/auth/websocket-ticket/media", false).await {
        Err(AuthError::Unexpected { status: 404, .. }) => {
            post_ticket(credentials, "/api/auth/websocket-ticket", false).await
        }
        other => other,
    }
}

async fn post_ticket(
    credentials: &Credentials,
    path: &str,
    allow_rich_refusal: bool,
) -> Result<String, AuthError> {
    let host = credentials.http_base.clone();
    let url = format!("{}{path}", credentials.http_base.trim_end_matches('/'));
    let mut rejected: Option<String> = None;
    for attempt in 0..2 {
        let (auth_header, dpop_header, dpop_token) =
            resolve_http_auth_headers(credentials, "POST", &url, rejected.as_deref()).await?;
        let mut request = http().post(&url).header("Authorization", auth_header);
        if let Some(proof) = dpop_header {
            request = request.header("DPoP", proof);
        }
        let response = request
            .send()
            .await
            .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
        let status = response.status();
        if status.as_u16() == 401 && attempt == 0 && let Some(used_token) = dpop_token {
            // A DPoP token refused before local expiry (e.g. after a clock jump) renews once (`service.ts`).
            rejected = Some(used_token);
            continue;
        }
        if status.as_u16() == 401 || status.as_u16() == 403 {
            let body = response.text().await.unwrap_or_default();
            return Err(parse_auth_refusal_error(&body, status.as_u16(), allow_rich_refusal));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(AuthError::Unexpected { host, status: status.as_u16(), body });
        }
        let ticket: TicketResult = response
            .json()
            .await
            .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
        return Ok(ticket.ticket);
    }
    Err(AuthError::Unauthorized)
}

/// `GET /api/auth/session`: what this device's credential may do now (`fetchEnvironmentSessionState`).
pub async fn fetch_session_state(credentials: &Credentials) -> Result<AuthSessionState, AuthError> {
    let host = credentials.http_base.clone();
    let url = format!("{}/api/auth/session", credentials.http_base.trim_end_matches('/'));
    let mut rejected: Option<String> = None;
    for attempt in 0..2 {
        let (auth_header, dpop_header, dpop_token) =
            resolve_http_auth_headers(credentials, "GET", &url, rejected.as_deref()).await?;
        let mut request = http().get(&url).header("Authorization", auth_header);
        if let Some(proof) = dpop_header {
            request = request.header("DPoP", proof);
        }
        let response = request
            .send()
            .await
            .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
        let status = response.status();
        if status.as_u16() == 401 && attempt == 0 && let Some(used_token) = dpop_token {
            rejected = Some(used_token);
            continue;
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(AuthError::Unexpected { host, status: status.as_u16(), body });
        }
        return response
            .json()
            .await
            .map_err(|source| AuthError::Unreachable { host, source });
    }
    Err(AuthError::Unauthorized)
}

/// An authenticated `GET` to an environment HTTP endpoint (such as `/api/connect/link-state`).
pub async fn get_access(credentials: &Credentials, path: &str) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", credentials.http_base.trim_end_matches('/'));
    let mut rejected: Option<String> = None;
    for attempt in 0..2 {
        let (auth_header, dpop_header, dpop_token) =
            resolve_http_auth_headers(credentials, "GET", &url, rejected.as_deref())
                .await
                .map_err(|error| error.to_string())?;
        let mut request = http().get(&url).header("Authorization", auth_header);
        if let Some(proof) = dpop_header {
            request = request.header("DPoP", proof);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("Could not reach {}: {error}", credentials.http_base))?;
        let status = response.status();
        if status.as_u16() == 401 && attempt == 0 && let Some(used_token) = dpop_token {
            rejected = Some(used_token);
            continue;
        }
        let text = response.text().await.unwrap_or_default();
        let value = serde_json::from_str::<serde_json::Value>(&text).unwrap_or(serde_json::Value::Null);
        if !status.is_success() {
            let message = value.get("message").and_then(serde_json::Value::as_str).filter(|message| !message.is_empty());
            return Err(message.map_or_else(|| format!("The server answered {}.", status.as_u16()), str::to_owned));
        }
        return Ok(value);
    }
    Err("This device is no longer authorized. Pair again.".to_owned())
}

/// An authenticated `POST` to an access endpoint (`/api/auth/pairing-token`,
/// `/api/auth/clients/revoke`, …). A failure carries the server's `message` when it sends one.
pub async fn post_access(
    credentials: &Credentials,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", credentials.http_base.trim_end_matches('/'));
    let mut rejected: Option<String> = None;
    for attempt in 0..2 {
        let (auth_header, dpop_header, dpop_token) =
            resolve_http_auth_headers(credentials, "POST", &url, rejected.as_deref())
                .await
                .map_err(|error| error.to_string())?;
        let mut request = http().post(&url).header("Authorization", auth_header);
        if let Some(proof) = dpop_header {
            request = request.header("DPoP", proof);
        }
        if let Some(body) = &body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("Could not reach {}: {error}", credentials.http_base))?;
        let status = response.status();
        if status.as_u16() == 401 && attempt == 0 && let Some(used_token) = dpop_token {
            rejected = Some(used_token);
            continue;
        }
        let text = response.text().await.unwrap_or_default();
        let value = serde_json::from_str::<serde_json::Value>(&text).unwrap_or(serde_json::Value::Null);
        if !status.is_success() {
            let message = value.get("message").and_then(serde_json::Value::as_str).filter(|message| !message.is_empty());
            return Err(message.map_or_else(|| format!("The server answered {}.", status.as_u16()), str::to_owned));
        }
        return Ok(value);
    }
    Err("This device is no longer authorized. Pair again.".to_owned())
}

/// Classifies an HTTP failure from `POST /api/auth/pairing-token` into `PeerPairingCodeError`
/// (`packages/client-runtime/src/state/peerPairingCode.ts`).
pub fn classify_peer_pairing_code_http_error(status: u16, body: &serde_json::Value) -> PeerPairingCodeError {
    let tag = body.get("_tag").and_then(serde_json::Value::as_str);
    let detail = body
        .get("message")
        .and_then(serde_json::Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .map(str::to_owned);
    if status == 403
        || matches!(tag, Some("EnvironmentScopeRequiredError" | "EnvironmentOperationForbiddenError"))
    {
        PeerPairingCodeError::not_allowed(detail)
    } else {
        PeerPairingCodeError::unavailable(detail.or_else(|| Some(format!("HTTP {status}"))))
    }
}

async fn post_pairing_credential(
    credentials: &Credentials,
    body: serde_json::Value,
) -> Result<PairingCredentialResult, PeerPairingCodeError> {
    let url = format!("{}/api/auth/pairing-token", credentials.http_base.trim_end_matches('/'));
    let mut rejected: Option<String> = None;
    for attempt in 0..2 {
        let (auth_header, dpop_header, dpop_token) =
            resolve_http_auth_headers(credentials, "POST", &url, rejected.as_deref())
                .await
                .map_err(|error| PeerPairingCodeError::unavailable(Some(error.to_string())))?;
        let mut request = http()
            .post(&url)
            .header("Authorization", auth_header)
            .timeout(std::time::Duration::from_millis(MINT_PAIRING_CODE_TIMEOUT_MS))
            .json(&body);
        if let Some(proof) = dpop_header {
            request = request.header("DPoP", proof);
        }
        let response = request
            .send()
            .await
            .map_err(|error| PeerPairingCodeError::unavailable(Some(error.to_string())))?;
        let status = response.status();
        if status.as_u16() == 401 && attempt == 0 && let Some(used_token) = dpop_token {
            rejected = Some(used_token);
            continue;
        }
        let text = response.text().await.unwrap_or_default();
        let value = serde_json::from_str::<serde_json::Value>(&text).unwrap_or(serde_json::Value::Null);
        if !status.is_success() {
            return Err(classify_peer_pairing_code_http_error(status.as_u16(), &value));
        }
        return decode_pairing_credential_result(&value)
            .ok_or_else(|| PeerPairingCodeError::unavailable(Some("Missing pairing credential in server response.".to_owned())));
    }
    Err(PeerPairingCodeError::unavailable(Some("Authorization failed.".to_owned())))
}

/// `createPeerPairingCodeCommand` (`packages/client-runtime/src/state/peerPairingCode.ts`,
/// `pingdotgg/t3code#37825`): mints a fresh one-use pairing code bound to `purpose: "peer-link"`
/// on `credentials` at `access`, verifying server capability and response `purpose`/`access`.
pub async fn mint_peer_pairing_code(
    credentials: &Credentials,
    access: &str,
) -> Result<String, PeerPairingCodeError> {
    if let Some(caps) = descriptor_capabilities(&credentials.http_base)
        && !caps.peer_link_pairing_codes
    {
        return Err(PeerPairingCodeError::unavailable(Some(
            "The environment does not support peer-link pairing codes.".to_owned(),
        )));
    }
    let minted = post_pairing_credential(credentials, peer_pairing_code_request_body(access)).await?;
    validate_peer_link_credential(&minted, access)
}

/// `createMcpApprovalCodeCommand` (`packages/client-runtime/src/state/peerPairingCode.ts`):
/// mints a fresh one-use approval code on `credentials` for an app sign-in at `access`.
pub async fn mint_mcp_approval_code(
    credentials: &Credentials,
    access: &str,
) -> Result<PairingCredentialResult, PeerPairingCodeError> {
    post_pairing_credential(credentials, mcp_approval_code_request_body(access)).await
}

pub fn socket_url(credentials: &Credentials, ticket: &str) -> String {
    let mut url = url::Url::parse(&websocket_base(&credentials.http_base)).expect("valid ws base");
    let dpop_proof = if dpop::find_direct_credential(&credentials.http_base, &credentials.access_token).is_some() {
        dpop::ws_url_to_http_htu(&url).and_then(|http_htu| {
            dpop::device_key()
                .ok()
                .and_then(|key| key.create_proof("GET", &http_htu, Some(ticket)).ok())
        })
    } else {
        None
    };
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("wsTicket", ticket);
        if let Some(proof) = dpop_proof.as_deref() {
            pairs.append_pair("dpop", proof);
        }
        pairs
            .append_pair("clientSurface", "desktop")
            .append_pair("clientAppVersion", env!("CARGO_PKG_VERSION"))
            .append_pair("clientDeviceType", "desktop")
            .append_pair("clientOs", client_os())
            .append_pair("connectionMethod", "direct")
            .append_pair(SOCKET_FRAME_PARTS_QUERY_PARAM, "1");
    }
    append_orchestration_protocol(url.as_str()).expect("valid ws url")
}

fn client_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "Linux"
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn read_http_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..n]);
            if let Some(head) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let text = String::from_utf8_lossy(&bytes[..head]);
                let length = text
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= head + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn write_json(socket: &mut tokio::net::TcpStream, status: u16, body: serde_json::Value) {
        let text = body.to_string();
        let response = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    }

    fn header_value<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
        let wanted = name.to_ascii_lowercase();
        raw.lines().skip(1).find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim().to_ascii_lowercase().eq(&wanted).then(|| v.trim())
        })
    }

    #[test]
    fn socket_url_opts_into_frame_parts_and_secret_references_capability() {
        let creds = Credentials { http_base: "https://studio.ts.net:3773".into(), access_token: "tok".into() };
        let url = socket_url(&creds, "tkt");
        assert!(url.contains("frameParts=1"), "{url}");
        assert!(url.contains("wsTicket=tkt"), "{url}");
        assert!(url.contains("orchestrationProtocol=2"), "{url}");
        assert!(url.contains("clientCapabilities=settingsSecretReferences"), "{url}");
    }

    #[test]
    fn peer_pairing_code_error_classification_matches_web() {
        let forbidden = classify_peer_pairing_code_http_error(
            403,
            &serde_json::json!({ "_tag": "EnvironmentScopeRequiredError", "message": "Missing access:write" }),
        );
        assert_eq!(forbidden.reason, t3_core::pairing::PeerPairingCodeErrorReason::NotAllowed);
        let unavailable = classify_peer_pairing_code_http_error(503, &serde_json::json!({}));
        assert_eq!(unavailable.reason, t3_core::pairing::PeerPairingCodeErrorReason::Unavailable);
    }

    #[tokio::test]
    async fn direct_dpop_pairing_renews_expired_token_and_attaches_proofs() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let expected_base = base.clone();

        let server = tokio::spawn(async move {
            let mut captured = Vec::new();
            // 1. Descriptor with accessTokenRenewal: true
            let (mut sock, _) = listener.accept().await.unwrap();
            let req = read_http_request(&mut sock).await;
            assert!(req.starts_with("GET /.well-known/t3/environment "));
            write_json(
                &mut sock,
                200,
                serde_json::json!({
                    "environmentId": "env-dpop",
                    "label": "DPoP Box",
                    "serverVersion": "0.1.9",
                    "orchestrationProtocolVersion": 2,
                    "capabilities": { "accessTokenRenewal": true, "peerLinkPairingCodes": true }
                }),
            )
            .await;

            // 2. Pairing token exchange with DPoP proof
            let (mut sock, _) = listener.accept().await.unwrap();
            let req = read_http_request(&mut sock).await;
            assert!(req.starts_with("POST /oauth/token "));
            let proof = header_value(&req, "dpop").expect("DPoP header on pairing exchange");
            let verified = dpop::verify_dpop_proof(
                proof,
                "POST",
                &format!("{expected_base}/oauth/token"),
                now_epoch_ms() / 1000,
                None,
                None,
                None,
            )
            .expect("valid pairing DPoP proof");
            let thumb = verified.thumbprint.clone();
            captured.push(("pair", req));
            write_json(
                &mut sock,
                200,
                serde_json::json!({
                    "access_token": "dpop-initial-tok",
                    "token_type": "DPoP",
                    "expires_in": 0
                }),
            )
            .await;

            // 3. Automatic renewal on expired token before ticket request
            let (mut sock, _) = listener.accept().await.unwrap();
            let req = read_http_request(&mut sock).await;
            assert!(req.starts_with("POST /oauth/token "));
            let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
            assert!(body.contains("subject_token=dpop-initial-tok"), "{body}");
            assert!(
                body.contains("subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token"),
                "{body}"
            );
            let proof = header_value(&req, "dpop").expect("DPoP header on renewal");
            dpop::verify_dpop_proof(
                proof,
                "POST",
                &format!("{expected_base}/oauth/token"),
                now_epoch_ms() / 1000,
                Some(&thumb),
                None,
                None,
            )
            .expect("valid renewal DPoP proof");
            captured.push(("renew", req));
            write_json(
                &mut sock,
                200,
                serde_json::json!({
                    "access_token": "dpop-renewed-tok",
                    "token_type": "DPoP",
                    "expires_in": 3600
                }),
            )
            .await;

            // 4. Ticket request carries Authorization: DPoP <renewed> + DPoP proof with ath
            let (mut sock, _) = listener.accept().await.unwrap();
            let req = read_http_request(&mut sock).await;
            assert!(req.starts_with("POST /api/auth/websocket-ticket/rpc "));
            assert_eq!(header_value(&req, "authorization"), Some("DPoP dpop-renewed-tok"));
            let proof = header_value(&req, "dpop").expect("DPoP header on ticket");
            dpop::verify_dpop_proof(
                proof,
                "POST",
                &format!("{expected_base}/api/auth/websocket-ticket/rpc"),
                now_epoch_ms() / 1000,
                Some(&thumb),
                Some("dpop-renewed-tok"),
                None,
            )
            .expect("valid ticket DPoP proof with ath");
            captured.push(("ticket", req));
            write_json(&mut sock, 200, serde_json::json!({ "ticket": "ws-dpop-ticket" })).await;
            captured
        });

        let link = PairingLink {
            http_base: base.clone(),
            token: "PAIR-CODE-1".into(),
        };
        let paired = pair_with_link(&link).await.expect("pair with DPoP");
        assert_eq!(paired.credentials.access_token, "dpop-initial-tok");
        assert_eq!(
            dpop::get_direct_credential(&base).map(|c| c.access_token),
            Some("dpop-initial-tok".to_owned())
        );

        let ticket = issue_rpc_ticket(&paired.credentials).await.expect("ticket with renewal");
        assert_eq!(ticket, "ws-dpop-ticket");
        assert_eq!(
            dpop::get_direct_credential(&base).map(|c| c.access_token),
            Some("dpop-renewed-tok".to_owned())
        );

        // WebSocket URL attaches a DPoP upgrade proof bound to GET, http(s)://.../ws, and the ticket.
        let ws_url = url::Url::parse(&socket_url(&paired.credentials, &ticket)).unwrap();
        let ws_dpop = ws_url
            .query_pairs()
            .find(|(k, _)| k == "dpop")
            .map(|(_, v)| v.into_owned())
            .expect("DPoP upgrade proof query param");
        let device_thumb = dpop::device_key().unwrap().thumbprint().to_owned();
        dpop::verify_dpop_proof(
            &ws_dpop,
            "GET",
            &format!("{base}/ws"),
            now_epoch_ms() / 1000,
            Some(&device_thumb),
            Some(&ticket),
            None,
        )
        .expect("valid WebSocket upgrade DPoP proof");

        // Alternate route holding the saved seed token also produces a valid DPoP upgrade proof for its own htu.
        let alt_creds = Credentials {
            http_base: "https://fallback.ts.net:3773".into(),
            access_token: paired.credentials.access_token.clone(),
        };
        let alt_ws = url::Url::parse(&socket_url(&alt_creds, &ticket)).unwrap();
        let alt_dpop = alt_ws
            .query_pairs()
            .find(|(k, _)| k == "dpop")
            .map(|(_, v)| v.into_owned())
            .expect("DPoP proof on alternate route");
        dpop::verify_dpop_proof(
            &alt_dpop,
            "GET",
            "https://fallback.ts.net:3773/ws",
            now_epoch_ms() / 1000,
            Some(&device_thumb),
            Some(&ticket),
            None,
        )
        .expect("valid alternate-route WebSocket DPoP proof");

        let captured = server.await.unwrap();
        assert_eq!(captured.len(), 3);
        dpop::remove_direct_credential(&base);
    }

    #[tokio::test]
    async fn older_host_without_renewal_capability_keeps_bearer_pairing_and_surfaces_trace_id() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            // 1. Descriptor WITHOUT accessTokenRenewal
            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = read_http_request(&mut sock).await;
            write_json(
                &mut sock,
                200,
                serde_json::json!({
                    "environmentId": "env-legacy",
                    "label": "Legacy Box",
                    "serverVersion": "0.1.5",
                    "orchestrationProtocolVersion": 2,
                    "capabilities": {}
                }),
            )
            .await;

            // 2. Bearer token exchange must NOT carry DPoP header
            let (mut sock, _) = listener.accept().await.unwrap();
            let req = read_http_request(&mut sock).await;
            assert!(header_value(&req, "dpop").is_none(), "legacy host gets Bearer pairing");
            write_json(
                &mut sock,
                200,
                serde_json::json!({ "access_token": "bearer-tok", "token_type": "Bearer" }),
            )
            .await;

            // 3. Ticket refusal returns traceId (`pingdotgg/t3code#38041`)
            let (mut sock, _) = listener.accept().await.unwrap();
            let req = read_http_request(&mut sock).await;
            assert_eq!(header_value(&req, "authorization"), Some("Bearer bearer-tok"));
            assert!(header_value(&req, "dpop").is_none());
            write_json(
                &mut sock,
                401,
                serde_json::json!({
                    "_tag": "EnvironmentAuthInvalidError",
                    "code": "auth_invalid",
                    "reason": "invalid_credential",
                    "traceId": "trace-expired-99"
                }),
            )
            .await;
        });

        let link = PairingLink {
            http_base: base.clone(),
            token: "LEGACY-CODE".into(),
        };
        let paired = pair_with_link(&link).await.expect("pair with Bearer");
        assert!(dpop::get_direct_credential(&base).is_none());

        let err = issue_rpc_ticket(&paired.credentials).await.unwrap_err();
        assert!(err.is_host_credential_refusal());
        assert_eq!(err.trace_id(), Some("trace-expired-99"));
        assert!(err.to_string().contains("Trace ID: trace-expired-99"), "{err}");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn peer_link_code_minting_sends_purpose_and_validates_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        record_descriptor_capabilities(
            &base,
            DescriptorCapabilities {
                access_token_renewal: false,
                peer_link_pairing_codes: true,
            },
        );
        let server = tokio::spawn(async move {
            // 1. Valid peer-link reply
            let (mut sock, _) = listener.accept().await.unwrap();
            let req = read_http_request(&mut sock).await;
            let body: serde_json::Value = serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(body["purpose"], "peer-link");
            assert_eq!(body["access"], "approval-required");
            write_json(
                &mut sock,
                200,
                serde_json::json!({
                    "credential": "PEER-CODE-1",
                    "purpose": "peer-link",
                    "access": "approval-required"
                }),
            )
            .await;
            // 2. Reply missing purpose is rejected (`pingdotgg/t3code#37825`)
            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = read_http_request(&mut sock).await;
            write_json(
                &mut sock,
                200,
                serde_json::json!({
                    "credential": "PLAIN-CODE",
                    "access": "approval-required"
                }),
            )
            .await;
        });

        let creds = Credentials {
            http_base: base.clone(),
            access_token: "tok".into(),
        };
        assert_eq!(
            mint_peer_pairing_code(&creds, "approval-required").await.as_deref(),
            Ok("PEER-CODE-1")
        );
        assert_eq!(
            mint_peer_pairing_code(&creds, "approval-required").await.unwrap_err().reason,
            t3_core::pairing::PeerPairingCodeErrorReason::Unavailable
        );

        // Capability absent rejects before HTTP call.
        record_descriptor_capabilities(
            &base,
            DescriptorCapabilities {
                access_token_renewal: false,
                peer_link_pairing_codes: false,
            },
        );
        assert_eq!(
            mint_peer_pairing_code(&creds, "approval-required").await.unwrap_err().reason,
            t3_core::pairing::PeerPairingCodeErrorReason::Unavailable
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn send_authed_request_uses_dpop_across_relocated_route_and_retries_on_401() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alt_base = format!("http://{}", listener.local_addr().unwrap());
        let original_base = format!("http://127.0.0.1:{}", 50000 + (uuid::Uuid::new_v4().as_u128() % 8000));
        let seed = format!("seed-dpop-http-{}", uuid::Uuid::new_v4().simple());
        let renewed = format!("renewed-dpop-http-{}", uuid::Uuid::new_v4().simple());
        let device_thumb = dpop::device_key().unwrap().thumbprint().to_owned();

        // Store DPoP credential under original_base (simulating SSH/multi-route origin mismatch).
        dpop::put_direct_credential(
            &original_base,
            DpopConnectionCredential::new(seed.clone(), now_epoch_ms() + 3_600_000, Some("env-http".into())),
        );

        let expected_alt_base = alt_base.clone();
        let expected_seed = seed.clone();
        let expected_renewed = renewed.clone();
        let server = tokio::spawn(async move {
            // 1. Initial GET on alternate route sends DPoP <seed>, receives 401.
            let (mut sock, _) = listener.accept().await.unwrap();
            let req1 = read_http_request(&mut sock).await;
            assert!(req1.starts_with("GET /api/threads/t1/history "));
            assert_eq!(header_value(&req1, "authorization"), Some(format!("DPoP {expected_seed}").as_str()));
            let p1 = header_value(&req1, "dpop").expect("DPoP header on first attempt");
            dpop::verify_dpop_proof(
                p1,
                "GET",
                &format!("{expected_alt_base}/api/threads/t1/history"),
                now_epoch_ms() / 1000,
                Some(&device_thumb),
                Some(&expected_seed),
                None,
            )
            .expect("valid first DPoP proof");
            write_json(&mut sock, 401, serde_json::json!({ "error": "expired" })).await;

            // 2. Renewal POST /oauth/token on alternate route.
            let (mut sock, _) = listener.accept().await.unwrap();
            let req2 = read_http_request(&mut sock).await;
            assert!(req2.starts_with("POST /oauth/token "));
            let p2 = header_value(&req2, "dpop").expect("DPoP header on renewal");
            dpop::verify_dpop_proof(
                p2,
                "POST",
                &format!("{expected_alt_base}/oauth/token"),
                now_epoch_ms() / 1000,
                Some(&device_thumb),
                None,
                None,
            )
            .expect("valid renewal DPoP proof");
            write_json(
                &mut sock,
                200,
                serde_json::json!({
                    "access_token": expected_renewed,
                    "token_type": "DPoP",
                    "expires_in": 3600
                }),
            )
            .await;

            // 3. Retry GET carries renewed DPoP token + fresh proof and succeeds.
            let (mut sock, _) = listener.accept().await.unwrap();
            let req3 = read_http_request(&mut sock).await;
            assert!(req3.starts_with("GET /api/threads/t1/history "));
            assert_eq!(header_value(&req3, "authorization"), Some(format!("DPoP {expected_renewed}").as_str()));
            let p3 = header_value(&req3, "dpop").expect("DPoP header on retry");
            dpop::verify_dpop_proof(
                p3,
                "GET",
                &format!("{expected_alt_base}/api/threads/t1/history"),
                now_epoch_ms() / 1000,
                Some(&device_thumb),
                Some(&expected_renewed),
                None,
            )
            .expect("valid retry DPoP proof");
            write_json(&mut sock, 200, serde_json::json!({ "ok": true })).await;
        });

        let alt_creds = Credentials {
            http_base: alt_base.clone(),
            access_token: seed.clone(),
        };
        let url = format!("{alt_base}/api/threads/t1/history");
        let client = reqwest::Client::new();
        let resp = send_authed_request(&alt_creds, "GET", &url, || client.get(&url))
            .await
            .expect("DPoP HTTP retry succeeds");
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(
            dpop::get_direct_credential(&original_base).map(|c| c.access_token),
            Some(renewed)
        );
        server.await.unwrap();
        dpop::remove_direct_credential_matching(&original_base, Some(&seed));
    }
}
