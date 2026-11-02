//! Pairing and socket authorization, as `client-runtime/src/authorization/remote.ts` does it:
//! pairing token → `POST /oauth/token` (token exchange) → bearer access token →
//! `POST /api/auth/websocket-ticket/rpc` → `wsTicket` on the socket URL.

use serde::{Deserialize, Serialize};
use t3_core::auth::AuthSessionState;
pub use t3_core::compatibility::ORCHESTRATION_PROTOCOL_VERSION;
use t3_core::compatibility::protocol_mismatch;
use t3_core::pairing::{PairingLink, websocket_base};
use t3_core::server_config::EnvironmentDescriptor;

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const BOOTSTRAP_TOKEN_TYPE: &str = "urn:t3:params:oauth:token-type:environment-bootstrap";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
/// `WELL_KNOWN_ENVIRONMENT_PATH`: the unauthenticated descriptor every server answers.
const DESCRIPTOR_PATH: &str = "/.well-known/t3/environment";

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
    let host = http_base.to_owned();
    let response = http()
        .get(format!("{http_base}{DESCRIPTOR_PATH}"))
        .timeout(std::time::Duration::from_secs(5))
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

pub fn socket_url(credentials: &Credentials, ticket: &str) -> String {
    let mut url = url::Url::parse(&websocket_base(&credentials.http_base)).expect("valid ws base");
    url.query_pairs_mut()
        .append_pair("wsTicket", ticket)
        .append_pair("orchestrationProtocol", &ORCHESTRATION_PROTOCOL_VERSION.to_string())
        .append_pair("clientSurface", "desktop")
        .append_pair("clientAppVersion", env!("CARGO_PKG_VERSION"))
        .append_pair("clientDeviceType", "desktop")
        .append_pair("clientOs", client_os())
        .append_pair("connectionMethod", "direct");
    url.to_string()
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
