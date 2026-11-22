//! Account-list, status, connect models and errors from client-runtime/relay/{managedRelay,errorPresentation}.ts.
use serde::Deserialize;
use serde_json::Value;

pub const CONNECTION_UNAVAILABLE: &str = "Connecting through T3 Connect is not supported with native sign-in yet. Use a pairing link or SSH.";
pub const LEGACY_OAUTH_NOTICE: &str = "Sign in with T3 Connect to enable direct relay connections.";

pub const DEFAULT_MANAGED_TUNNEL_LIMIT: u32 = 3;
pub const T3CHAT_PRO_TUNNEL_LIMIT: u32 = 10;
pub const T3CHAT_PREMIER_TUNNEL_LIMIT: u32 = 100;
pub const T3CHAT_SUBSCRIPTION_URL: &str = "https://t3.chat/settings/subscription";
pub const T3_CONNECT_TROUBLESHOOTING_URL: &str = "https://github.com/pingdotgg/t3code/blob/main/docs/user/t3-connect-troubleshooting.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum T3ChatPlan {
    Free,
    Pro,
    Premier,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct T3ChatLink {
    #[serde(default)]
    pub email: Option<String>,
    pub plan: T3ChatPlan,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct T3ChatLinkResponse {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub link: Option<T3ChatLink>,
    #[serde(default)]
    pub max_tunnels_override: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedTunnelUpgrade {
    Link,
    Pro,
    Premier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedTunnelCap {
    pub used: usize,
    pub limit: u32,
    pub upgrade: Option<ManagedTunnelUpgrade>,
}

pub fn managed_tunnel_cap(environments: &[Environment], t3_chat: Option<&T3ChatLinkResponse>) -> ManagedTunnelCap {
    let used = environments
        .iter()
        .filter(|env| env.endpoint.provider_kind == "cloudflare_tunnel")
        .count();
    let (limit, upgrade) = match t3_chat {
        Some(cfg) if cfg.max_tunnels_override.is_some() => (cfg.max_tunnels_override.unwrap_or(DEFAULT_MANAGED_TUNNEL_LIMIT), None),
        Some(cfg) if !cfg.available => (DEFAULT_MANAGED_TUNNEL_LIMIT, None),
        Some(cfg) => match cfg.link.as_ref().map(|link| link.plan) {
            Some(T3ChatPlan::Premier) => (T3CHAT_PREMIER_TUNNEL_LIMIT, None),
            Some(T3ChatPlan::Pro) => (T3CHAT_PRO_TUNNEL_LIMIT, Some(ManagedTunnelUpgrade::Premier)),
            Some(T3ChatPlan::Free | T3ChatPlan::Other) => (DEFAULT_MANAGED_TUNNEL_LIMIT, Some(ManagedTunnelUpgrade::Pro)),
            None => (DEFAULT_MANAGED_TUNNEL_LIMIT, Some(ManagedTunnelUpgrade::Link)),
        },
        None => (DEFAULT_MANAGED_TUNNEL_LIMIT, None),
    };
    ManagedTunnelCap { used, limit, upgrade }
}

pub fn tunnel_upgrade_copy(upgrade: ManagedTunnelUpgrade) -> String {
    match upgrade {
        ManagedTunnelUpgrade::Link => format!(
            "Link T3 Chat Pro for up to {T3CHAT_PRO_TUNNEL_LIMIT} tunnels, or Premier for up to {T3CHAT_PREMIER_TUNNEL_LIMIT}."
        ),
        ManagedTunnelUpgrade::Pro => format!(
            "Upgrade to T3 Chat Pro for up to {T3CHAT_PRO_TUNNEL_LIMIT} tunnels, or Premier for up to {T3CHAT_PREMIER_TUNNEL_LIMIT}."
        ),
        ManagedTunnelUpgrade::Premier => {
            format!("Upgrade to T3 Chat Premier for up to {T3CHAT_PREMIER_TUNNEL_LIMIT} tunnels.")
        }
    }
}

pub fn extract_tunnel_limit_error(status: u16, error: &Value) -> Option<u32> {
    if !(400..500).contains(&status) {
        return None;
    }
    if error["_tag"].as_str() != Some("RelayManagedTunnelLimitReachedError") {
        return None;
    }
    Some(
        error["maxTunnels"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MANAGED_TUNNEL_LIMIT),
    )
}

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
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentStatusResponse {
    pub environment_id: String,
    pub endpoint: Endpoint,
    pub status: EnvironmentStatusValue,
    #[serde(default)]
    pub descriptor: Option<crate::server_config::EnvironmentDescriptor>,
    #[serde(default)]
    pub checked_at: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub offline_reason: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
}

/// `RelayEnvironmentLinkResponse` from `packages/contracts/src/relay.ts`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentLinkResponse {
    pub ok: bool,
    pub environment_id: String,
    pub endpoint: Endpoint,
    pub relay_issuer: String,
    pub cloud_user_id: String,
    pub environment_credential: String,
    pub cloud_mint_public_key: String,
    pub endpoint_runtime: Value,
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
    Incompatible(String),
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
        "RelayManagedTunnelLimitReachedError" => {
            let max = error["maxTunnels"]
                .as_u64()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default();
            format!("You have reached the T3 Connect tunnel limit{max}.")
        }
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

    #[test]
    fn managed_tunnel_cap_and_limit_errors_follow_t3chat_plan_rules() {
        let envs = vec![
            Environment {
                environment_id: "e1".into(),
                label: "One".into(),
                endpoint: Endpoint {
                    http_base_url: "https://e1.t3.codes".into(),
                    ws_base_url: "wss://e1.t3.codes/ws".into(),
                    provider_kind: "cloudflare_tunnel".into(),
                },
            },
            Environment {
                environment_id: "e2".into(),
                label: "Two".into(),
                endpoint: Endpoint {
                    http_base_url: "https://e2.ts.net".into(),
                    ws_base_url: "wss://e2.ts.net/ws".into(),
                    provider_kind: "manual".into(),
                },
            },
        ];
        let cap_unlinked = managed_tunnel_cap(
            &envs,
            Some(&T3ChatLinkResponse { available: true, link: None, max_tunnels_override: None }),
        );
        assert_eq!(
            cap_unlinked,
            ManagedTunnelCap { used: 1, limit: 3, upgrade: Some(ManagedTunnelUpgrade::Link) }
        );
        let cap_pro = managed_tunnel_cap(
            &envs,
            Some(&T3ChatLinkResponse {
                available: true,
                link: Some(T3ChatLink { email: None, plan: T3ChatPlan::Pro }),
                max_tunnels_override: None,
            }),
        );
        assert_eq!(
            cap_pro,
            ManagedTunnelCap { used: 1, limit: 10, upgrade: Some(ManagedTunnelUpgrade::Premier) }
        );
        let cap_override = managed_tunnel_cap(
            &envs,
            Some(&T3ChatLinkResponse {
                available: true,
                link: Some(T3ChatLink { email: None, plan: T3ChatPlan::Free }),
                max_tunnels_override: Some(25),
            }),
        );
        assert_eq!(cap_override, ManagedTunnelCap { used: 1, limit: 25, upgrade: None });
        assert_eq!(
            extract_tunnel_limit_error(403, &json!({"_tag": "RelayManagedTunnelLimitReachedError", "maxTunnels": 3})),
            Some(3)
        );
        assert_eq!(extract_tunnel_limit_error(403, &json!({"_tag": "Other"})), None);
        assert!(tunnel_upgrade_copy(ManagedTunnelUpgrade::Pro).contains("10"));
    }
}
