//! Pairing and socket authorization, as `client-runtime/src/authorization/remote.ts` does it:
//! pairing token → `POST /oauth/token` (token exchange) → bearer access token →
//! `POST /api/auth/websocket-ticket/rpc` → `wsTicket` on the socket URL.

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
    websocket_base,
};
use t3_core::server_config::EnvironmentDescriptor;

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

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("{0} rejected the pairing token. Create a new pairing link and try again.")]
    Rejected(String),
    #[error("This device is no longer authorized. Pair again.")]
    Unauthorized,
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

/// A redeemed pairing and, when the server answered it, its descriptor (label, id, version).
#[derive(Debug, Clone)]
pub struct Paired {
    pub credentials: Credentials,
    pub descriptor: Option<EnvironmentDescriptor>,
}

#[derive(Deserialize)]
struct AccessTokenResult {
    access_token: String,
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

/// `GET /.well-known/t3/environment` with an explicit timeout (used by multi-route probing).
pub async fn fetch_descriptor_timeout(
    http_base: &str,
    timeout: std::time::Duration,
) -> Result<EnvironmentDescriptor, AuthError> {
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
    response.json().await.map_err(|source| AuthError::Unreachable { host, source })
}

/// Pairs with a link, refusing a server on another protocol before spending its token. A server
/// whose descriptor can't be read (an older one, or a proxy) still pairs, as it always has.
pub async fn pair_with_link(link: &PairingLink) -> Result<Paired, AuthError> {
    let descriptor = fetch_descriptor(&link.http_base).await.ok();
    if let Some(descriptor) = &descriptor
        && let Some(mismatch) = protocol_mismatch(descriptor)
    {
        return Err(AuthError::Incompatible(mismatch.message(&descriptor.label)));
    }
    let credentials = redeem_pairing(link).await?;
    Ok(Paired { credentials, descriptor })
}

pub async fn redeem_pairing(link: &PairingLink) -> Result<Credentials, AuthError> {
    let host = link.http_base.clone();
    let response = http()
        .post(format!("{}/oauth/token", link.http_base))
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

/// Mints a single-use RPC ticket. Servers that predate ticket audiences answer 404 on the
/// `/rpc` route; their bare-path tickets are valid for either use.
pub async fn issue_rpc_ticket(credentials: &Credentials) -> Result<String, AuthError> {
    match post_ticket(credentials, "/api/auth/websocket-ticket/rpc").await {
        Err(AuthError::Unexpected { status: 404, .. }) => {
            post_ticket(credentials, "/api/auth/websocket-ticket").await
        }
        other => other,
    }
}

/// Mints a single-use `media` ticket for a socket that cannot carry a bearer header, such as the
/// preview stream (`resolveDeviceHubAccess`). Older servers answer only the bare path.
pub async fn issue_media_ticket(credentials: &Credentials) -> Result<String, AuthError> {
    match post_ticket(credentials, "/api/auth/websocket-ticket/media").await {
        Err(AuthError::Unexpected { status: 404, .. }) => post_ticket(credentials, "/api/auth/websocket-ticket").await,
        other => other,
    }
}

async fn post_ticket(credentials: &Credentials, path: &str) -> Result<String, AuthError> {
    let host = credentials.http_base.clone();
    let response = http()
        .post(format!("{}{path}", credentials.http_base))
        .bearer_auth(&credentials.access_token)
        .send()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        if status.as_u16() == 403 {
            let body = response.text().await.unwrap_or_default();
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body)
                && t3_core::connect::is_connect_approval_pending_error(&value)
            {
                let message = value
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or("This device is waiting for approval on the host.")
                    .to_owned();
                let trace_id = value.get("traceId").and_then(serde_json::Value::as_str).map(str::to_owned);
                return Err(AuthError::ConnectApprovalPending { message, trace_id });
            }
        }
        return Err(AuthError::Unauthorized);
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Unexpected { host, status: status.as_u16(), body });
    }
    let ticket: TicketResult = response
        .json()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    Ok(ticket.ticket)
}

/// `GET /api/auth/session`: what this device's credential may do now (`fetchEnvironmentSessionState`).
pub async fn fetch_session_state(credentials: &Credentials) -> Result<AuthSessionState, AuthError> {
    let host = credentials.http_base.clone();
    let response = http()
        .get(format!("{}/api/auth/session", credentials.http_base))
        .bearer_auth(&credentials.access_token)
        .send()
        .await
        .map_err(|source| AuthError::Unreachable { host: host.clone(), source })?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Unexpected { host, status: status.as_u16(), body });
    }
    response.json().await.map_err(|source| AuthError::Unreachable { host, source })
}

/// An authenticated `GET` to an environment HTTP endpoint (such as `/api/connect/link-state`).
pub async fn get_access(credentials: &Credentials, path: &str) -> Result<serde_json::Value, String> {
    let response = http()
        .get(format!("{}{path}", credentials.http_base))
        .bearer_auth(&credentials.access_token)
        .send()
        .await
        .map_err(|error| format!("Could not reach {}: {error}", credentials.http_base))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let value = serde_json::from_str::<serde_json::Value>(&text).unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let message = value.get("message").and_then(serde_json::Value::as_str).filter(|message| !message.is_empty());
        return Err(message.map_or_else(|| format!("The server answered {}.", status.as_u16()), str::to_owned));
    }
    Ok(value)
}

/// An authenticated `POST` to an access endpoint (`/api/auth/pairing-token`,
/// `/api/auth/clients/revoke`, …). A failure carries the server's `message` when it sends one.
pub async fn post_access(credentials: &Credentials, path: &str, body: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    let request = http().post(format!("{}{path}", credentials.http_base)).bearer_auth(&credentials.access_token);
    let request = match body {
        Some(body) => request.json(&body),
        None => request,
    };
    let response = request.send().await.map_err(|error| format!("Could not reach {}: {error}", credentials.http_base))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let value = serde_json::from_str::<serde_json::Value>(&text).unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let message = value.get("message").and_then(serde_json::Value::as_str).filter(|message| !message.is_empty());
        return Err(message.map_or_else(|| format!("The server answered {}.", status.as_u16()), str::to_owned));
    }
    Ok(value)
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
    let response = http()
        .post(format!("{}/api/auth/pairing-token", credentials.http_base.trim_end_matches('/')))
        .bearer_auth(&credentials.access_token)
        .timeout(std::time::Duration::from_millis(MINT_PAIRING_CODE_TIMEOUT_MS))
        .json(&body)
        .send()
        .await
        .map_err(|error| PeerPairingCodeError::unavailable(Some(error.to_string())))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let value = serde_json::from_str::<serde_json::Value>(&text).unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        return Err(classify_peer_pairing_code_http_error(status.as_u16(), &value));
    }
    decode_pairing_credential_result(&value)
        .ok_or_else(|| PeerPairingCodeError::unavailable(Some("Missing pairing credential in server response.".to_owned())))
}

/// `createPeerPairingCodeCommand` (`packages/client-runtime/src/state/peerPairingCode.ts`):
/// mints a fresh one-use pairing code on `credentials` for peer linking at `access`.
pub async fn mint_peer_pairing_code(
    credentials: &Credentials,
    access: &str,
) -> Result<String, PeerPairingCodeError> {
    post_pairing_credential(credentials, peer_pairing_code_request_body(access))
        .await
        .map(|minted| minted.credential)
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
    url.query_pairs_mut()
        .append_pair("wsTicket", ticket)
        .append_pair("clientSurface", "desktop")
        .append_pair("clientAppVersion", env!("CARGO_PKG_VERSION"))
        .append_pair("clientDeviceType", "desktop")
        .append_pair("clientOs", client_os())
        .append_pair("connectionMethod", "direct")
        .append_pair(SOCKET_FRAME_PARTS_QUERY_PARAM, "1");
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
}
