//! Account-list, status, connect models and errors from client-runtime/relay/{managedRelay,errorPresentation}.ts.
use serde::Deserialize;
use serde_json::Value;

pub const CONNECTION_UNAVAILABLE: &str = "Connecting through T3 Connect is not supported with native sign-in yet. Use a pairing link or SSH.";
pub const LEGACY_OAUTH_NOTICE: &str = "Sign in with T3 Connect to enable direct relay connections.";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub http_base_url: String,
    pub ws_base_url: String,
    pub provider_kind: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub environment_id: String,
    pub label: String,
    pub endpoint: Endpoint,
}
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub listed: bool,
    pub refreshing: bool,
    pub environments: Vec<Environment>,
}

/// `RelayEnvironmentStatusValue` (`packages/contracts/src/relay.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentStatusValue {
    Online,
    Offline,
}

/// `RelayEnvironmentStatusResponse` (`packages/contracts/src/relay.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentStatusResponse {
    pub environment_id: String,
    pub endpoint: Endpoint,
    pub status: EnvironmentStatusValue,
    #[serde(default)]
    pub checked_at: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub offline_reason: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
}

/// `RelayEnvironmentConnectResponse` (`packages/contracts/src/relay.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentConnectResponse {
    pub environment_id: String,
    pub endpoint: Endpoint,
    pub credential: String,
    pub expires_at: String,
}

impl EnvironmentConnectResponse {
    /// Parses RFC 3339 `expiresAt` into epoch milliseconds.
    pub fn expires_at_epoch_ms(&self) -> Option<i64> {
        crate::sidebar::time::parse_iso(&self.expires_at)
    }
}

/// Live reachability check state for one discovered T3 Connect environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentHealth {
    Checking,
    Online,
    Offline {
        offline_reason: Option<String>,
        error: Option<String>,
        trace_id: Option<String>,
    },
    Error(String),
}

/// `relayOfflineReasonMessage` (`packages/client-runtime/src/relay/errorPresentation.ts`).
pub fn offline_reason_message(offline_reason: Option<&str>) -> Option<&'static str> {
    match offline_reason {
        Some("tunnel_released") => Some(
            "Offline for a while, so its T3 Connect tunnel was removed. Start T3 Code on that computer and update it to the latest version to reconnect.",
        ),
        _ => None,
    }
}

/// Whether the relay rejected an expired/revoked bearer or DPoP access token (`401 invalid_bearer`).
pub fn is_invalid_bearer_error(status: u16, error: &Value) -> bool {
    status == 401
        && error["_tag"].as_str() == Some("RelayAuthInvalidError")
        && error["reason"].as_str() == Some("invalid_bearer")
}

pub fn error_message(status: u16, error: &Value) -> String {
    let message = match error["_tag"].as_str().unwrap_or("") {
        "RelayAuthInvalidError" => match error["reason"].as_str() {
            Some("missing_bearer" | "invalid_bearer") => "Relay rejected the cloud session token.".into(),
            Some("invalid_dpop") => match error["dpopFailureReason"].as_str() {
                Some("time_window") => {
                    "Relay rejected the DPoP proof. Check that automatic date and time is enabled on both devices, then try again."
                        .into()
                }
                _ => "Relay rejected the DPoP proof.".into(),
            },
            _ => "Relay rejected the authenticated request.".into(),
        },
        "RelayEnvironmentConnectNotAuthorizedError" => match error["reason"].as_str() {
            Some("environment_link_not_found") => {
                "Relay has no active link for this environment. If it was deregistered from T3 Connect, link it again on that machine."
                    .into()
            }
            Some("managed_endpoint_allocation_not_ready") => {
                "The environment's T3 Connect tunnel is still being set up.".into()
            }
            Some("endpoint_provider_not_managed") => {
                "This environment is linked only to publish agent activity, so it cannot accept connections. On that computer, open Settings > Sharing & access and turn T3 Connect on."
                    .into()
            }
            Some(reason) if !reason.is_empty() => format!("Relay rejected the environment connection request ({reason})."),
            _ => "Relay rejected the environment connection request.".into(),
        },
        "RelayEnvironmentEndpointUnavailableError" => format!(
            "Relay could not reach the environment endpoint ({}).",
            error["reason"].as_str().unwrap_or("unknown")
        ),
        "RelayEnvironmentEndpointTimedOutError" => "Relay timed out while contacting the environment endpoint.".into(),
        "RelayInternalError" => format!(
            "Relay encountered an internal error ({}).",
            error["reason"].as_str().unwrap_or("unknown")
        ),
        _ if status == 504 => {
            "Relay timed out before it finished the request (HTTP 504). Try again in a few minutes. If it keeps failing, copy the trace ID when you report it."
                .into()
        }
        _ => format!("Relay returned an unexpected HTTP {status}."),
    };
    match error["traceId"].as_str().filter(|s| !s.is_empty()) {
        Some(trace) => format!("{message} Trace ID: {trace}"),
        None => message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listing_errors_preserve_diagnostics_and_accept_unknown_errors() {
        assert_eq!(
            error_message(401, &json!({"_tag":"RelayAuthInvalidError","reason":"invalid_bearer","traceId":"trace"})),
            "Relay rejected the cloud session token. Trace ID: trace"
        );
        assert!(is_invalid_bearer_error(
            401,
            &json!({"_tag":"RelayAuthInvalidError","reason":"invalid_bearer"})
        ));
        assert!(!is_invalid_bearer_error(
            401,
            &json!({"_tag":"RelayAuthInvalidError","reason":"invalid_dpop"})
        ));
        assert!(
            error_message(
                401,
                &json!({"_tag":"RelayAuthInvalidError","reason":"invalid_dpop","dpopFailureReason":"time_window"})
            )
            .contains("automatic date and time")
        );
        assert!(
            error_message(
                403,
                &json!({"_tag":"RelayEnvironmentConnectNotAuthorizedError","reason":"environment_link_not_found"})
            )
            .contains("no active link")
        );
        assert_eq!(
            error_message(500, &json!({"_tag":"RelayInternalError","reason":"upstream_unavailable"})),
            "Relay encountered an internal error (upstream_unavailable)."
        );
        assert!(error_message(504, &Value::Null).contains("HTTP 504"));
        assert_eq!(error_message(503, &json!({"_tag":"FutureError"})), "Relay returned an unexpected HTTP 503.");
        assert!(offline_reason_message(Some("tunnel_released")).unwrap().contains("tunnel was removed"));
        assert_eq!(offline_reason_message(None), None);

        let connect: EnvironmentConnectResponse = serde_json::from_value(json!({
            "environmentId": "env-1",
            "endpoint": {
                "httpBaseUrl": "https://env-1.t3.codes",
                "wsBaseUrl": "wss://env-1.t3.codes/ws",
                "providerKind": "cloudflare_tunnel"
            },
            "credential": "cred-123",
            "expiresAt": "2026-11-20T18:00:00.000Z"
        }))
        .unwrap();
        assert_eq!(connect.expires_at_epoch_ms(), Some(1_795_197_600_000));
    }
}
