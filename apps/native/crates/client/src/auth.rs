//! Pairing and socket authorization, as `client-runtime/src/authorization/remote.ts` does it:
//! pairing token → `POST /oauth/token` (token exchange) → bearer access token →
//! `POST /api/auth/websocket-ticket/rpc` → `wsTicket` on the socket URL.

use serde::{Deserialize, Serialize};
use t3_core::auth::AuthSessionState;
use t3_core::pairing::{PairingLink, websocket_base};

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const BOOTSTRAP_TOKEN_TYPE: &str = "urn:t3:params:oauth:token-type:environment-bootstrap";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
/// `ORCHESTRATION_PROTOCOL_VERSION` in `packages/contracts/src/environment.ts`.
pub const ORCHESTRATION_PROTOCOL_VERSION: u32 = 2;

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
