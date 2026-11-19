//! T3 Connect tunnel readiness and gate resolution for the "Use in another app" wizard.
//! Ports `apps/web/src/components/mcp/mcpGate.ts`, `apps/web/src/cloud/t3ConnectState.ts`,
//! and the cloud link-state / reachability wire shapes in `packages/contracts/src/environmentHttp.ts`.

use serde::{Deserialize, Serialize};

/// `EnvironmentConnectReachability` (`packages/contracts/src/environmentHttp.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ConnectReachability {
    #[serde(rename_all = "camelCase")]
    Reachable {
        checked_at: String,
        #[serde(default)]
        ready_connections: Option<i64>,
        #[serde(default)]
        protocol: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Unreachable {
        checked_at: String,
        #[serde(default)]
        cause: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        hint: Option<String>,
        #[serde(default)]
        fix: String,
    },
}

impl ConnectReachability {
    pub fn checked_at(&self) -> &str {
        match self {
            ConnectReachability::Reachable { checked_at, .. }
            | ConnectReachability::Unreachable { checked_at, .. } => checked_at,
        }
    }

    pub fn unreachable_info(&self) -> Option<UnreachableInfo> {
        match self {
            ConnectReachability::Unreachable {
                checked_at,
                cause,
                reason,
                hint,
                fix,
            } => Some(UnreachableInfo {
                checked_at: checked_at.clone(),
                cause: cause.clone(),
                reason: reason.clone(),
                hint: hint.clone(),
                fix: fix.clone(),
            }),
            ConnectReachability::Reachable { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachableInfo {
    pub checked_at: String,
    pub cause: String,
    pub reason: String,
    pub hint: Option<String>,
    pub fix: String,
}

/// `EnvironmentCloudLinkStateResult` (`packages/contracts/src/environmentHttp.ts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CloudLinkState {
    pub linked: bool,
    pub cloud_user_id: Option<String>,
    pub relay_url: Option<String>,
    pub relay_issuer: Option<String>,
    pub managed_tunnel_active: Option<bool>,
    pub publish_agent_activity: bool,
    pub hold_webhooks_while_offline: Option<bool>,
    pub link_revoked: Option<bool>,
    pub reachability: Option<ConnectReachability>,
    pub tunnel_hostname: Option<String>,
    pub linkable_here: Option<bool>,
}

/// MCP-related capabilities parsed from raw `ServerConfig` or `/.well-known/t3/environment` JSON.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpCapabilities {
    pub mcp_approval_codes: bool,
    pub mcp_hosted_approval: bool,
    pub peer_links: bool,
}

impl McpCapabilities {
    /// Extracts MCP capabilities from either a `ServerConfig` payload (`environment.capabilities`)
    /// or an `ExecutionEnvironmentDescriptor` payload (`capabilities`).
    pub fn from_json(value: &serde_json::Value) -> Self {
        let caps = value
            .get("environment")
            .and_then(|env| env.get("capabilities"))
            .or_else(|| value.get("capabilities"));
        let flag = |key: &str| {
            caps.and_then(|obj| obj.get(key))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        Self {
            mcp_approval_codes: flag("mcpApprovalCodes"),
            mcp_hosted_approval: flag("mcpHostedApproval"),
            peer_links: flag("peerLinks"),
        }
    }
}

/// `RelayManagedEndpoint` (`packages/contracts/src/relay.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEndpoint {
    pub http_base_url: String,
    pub ws_base_url: String,
    pub provider_kind: String,
}

impl From<&crate::relay::Endpoint> for RelayEndpoint {
    fn from(endpoint: &crate::relay::Endpoint) -> Self {
        Self {
            http_base_url: endpoint.http_base_url.clone(),
            ws_base_url: endpoint.ws_base_url.clone(),
            provider_kind: endpoint.provider_kind.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayAvailability {
    Checking,
    Online,
    Offline,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayGateState {
    Loading,
    Failed,
    Listed {
        endpoint: Option<RelayEndpoint>,
        availability: Option<RelayAvailability>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReachabilityInput {
    Checking,
    Known(ConnectReachability),
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectedRelay {
    Offline,
    Failed,
}

/// What stands between the user and the wizard's steps. Only `Open` lets them go on: cloud apps
/// need a public address, so the environment must answer on its T3 Connect tunnel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpGate {
    Checking,
    SignIn,
    Disconnected { relay: Option<DisconnectedRelay> },
    Update,
    NoRelayRead,
    Error { message: String },
    TurnOn,
    Offline { reachability: Option<UnreachableInfo> },
    Open { mcp_url: String },
}

/// `isManagedTunnelActive` (`apps/web/src/cloud/t3ConnectState.ts`).
pub fn is_managed_tunnel_active(
    link_state: &CloudLinkState,
    relay_provider_kind: Option<&str>,
) -> bool {
    let local_managed_tunnel = link_state.managed_tunnel_active.unwrap_or(link_state.linked);
    link_state.link_revoked != Some(true)
        && local_managed_tunnel
        && matches!(relay_provider_kind, None | Some("cloudflare_tunnel"))
}

/// Plain DNS hostname check (`TUNNEL_HOSTNAME = /^[a-z0-9-]+(\.[a-z0-9-]+)+$/i`).
/// The URL ends up in commands people paste into a shell, so only plain DNS names pass.
pub fn is_plain_tunnel_hostname(host: &str) -> bool {
    let mut labels = host.split('.');
    let mut count = 0usize;
    for label in &mut labels {
        if label.is_empty()
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return false;
        }
        count += 1;
    }
    count >= 2
}

/// `t3ConnectMcpUrl` (`apps/web/src/cloud/t3ConnectState.ts`).
pub fn t3_connect_mcp_url(
    this_machine: bool,
    tunnel_hostname: Option<&str>,
    relay_endpoint: Option<&RelayEndpoint>,
) -> Option<String> {
    let parsed = if this_machine {
        let host = tunnel_hostname?;
        if host.is_empty() {
            return None;
        }
        url::Url::parse(&format!("https://{host}")).ok()?
    } else {
        let endpoint = relay_endpoint?;
        if endpoint.provider_kind != "cloudflare_tunnel" {
            return None;
        }
        url::Url::parse(&endpoint.http_base_url).ok()?
    };
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?;
    if !is_plain_tunnel_hostname(host) {
        return None;
    }
    Some(format!("{}/mcp", parsed.origin().ascii_serialization()))
}

/// Where T3 Connect can be turned on for an environment: from here, only by policy, or on its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum T3ConnectTurnOn {
    Here,
    Policy { reason: String },
    OnHost,
}

/// `resolveT3ConnectTurnOn` (`apps/web/src/cloud/t3ConnectState.ts`).
pub fn resolve_t3_connect_turn_on(
    desktop_app: bool,
    linkable_here: bool,
    cloud_configured: bool,
    can_read_relay: bool,
    can_write_relay: bool,
    connect_denial: Option<String>,
) -> T3ConnectTurnOn {
    if let Some(reason) = connect_denial {
        return T3ConnectTurnOn::Policy { reason };
    }
    if desktop_app && linkable_here && cloud_configured && can_read_relay && can_write_relay {
        T3ConnectTurnOn::Here
    } else {
        T3ConnectTurnOn::OnHost
    }
}

pub struct McpGateInput<'a> {
    /// `None` until T3 Connect sign-in has loaded.
    pub signed_in: Option<bool>,
    pub connected: bool,
    /// The environment's `mcpApprovalCodes` capability; `None` until its config loads.
    pub approval_codes: Option<bool>,
    pub can_read_relay: bool,
    pub link_state: Option<&'a CloudLinkState>,
    pub link_state_error: Option<&'a str>,
    /// A backend this device runs; its link state names its own tunnel.
    pub this_machine: bool,
    /// The account's machines on T3 Connect, where a saved machine's address and health come from.
    pub relay: &'a RelayGateState,
    /// A check run for this gate: its answer, `Checking` until it has one, or `Failed`.
    pub reachability: &'a ReachabilityInput,
}

/// `resolveMcpGate` (`apps/web/src/components/mcp/mcpGate.ts`).
pub fn resolve_mcp_gate(input: &McpGateInput<'_>) -> McpGate {
    let Some(signed_in) = input.signed_in else {
        return McpGate::Checking;
    };
    if !signed_in {
        return McpGate::SignIn;
    }
    if !input.connected {
        if input.this_machine {
            return McpGate::Disconnected { relay: None };
        }
        // T3 Connect checks only its own tunnels; a publish-only link's check always fails.
        let availability = match input.relay {
            RelayGateState::Listed {
                endpoint: Some(endpoint),
                availability,
            } if endpoint.provider_kind == "cloudflare_tunnel" => *availability,
            _ => None,
        };
        if matches!(input.relay, RelayGateState::Loading)
            || availability == Some(RelayAvailability::Checking)
        {
            return McpGate::Checking;
        }
        if matches!(input.relay, RelayGateState::Failed)
            || availability == Some(RelayAvailability::Error)
        {
            return McpGate::Disconnected {
                relay: Some(DisconnectedRelay::Failed),
            };
        }
        return McpGate::Disconnected {
            relay: (availability == Some(RelayAvailability::Offline))
                .then_some(DisconnectedRelay::Offline),
        };
    }
    let Some(approval_codes) = input.approval_codes else {
        return McpGate::Checking;
    };
    if !approval_codes {
        return McpGate::Update;
    }
    if !input.can_read_relay {
        return McpGate::NoRelayRead;
    }
    if let Some(message) = input.link_state_error {
        return McpGate::Error {
            message: message.to_owned(),
        };
    }
    let Some(link_state) = input.link_state else {
        return McpGate::Checking;
    };
    // This machine reports its own tunnel and the probe proves it; the list speaks for saved ones.
    let endpoint = if !input.this_machine {
        match input.relay {
            RelayGateState::Listed { endpoint, .. } => endpoint.as_ref(),
            _ => None,
        }
    } else {
        None
    };
    if !is_managed_tunnel_active(link_state, endpoint.map(|e| e.provider_kind.as_str())) {
        return McpGate::TurnOn;
    }
    if !input.this_machine {
        match input.relay {
            RelayGateState::Loading => return McpGate::Checking,
            RelayGateState::Failed => {
                return McpGate::Error {
                    message: "Could not reach T3 Connect.".to_owned(),
                };
            }
            RelayGateState::Listed { .. } => {
                if endpoint.is_none() {
                    return McpGate::TurnOn;
                }
            }
        }
    }
    if matches!(input.reachability, ReachabilityInput::Checking) {
        return McpGate::Checking;
    }
    let mcp_url = t3_connect_mcp_url(
        input.this_machine,
        link_state.tunnel_hostname.as_deref(),
        endpoint,
    );
    if let (ReachabilityInput::Known(ConnectReachability::Reachable { .. }), Some(mcp_url)) =
        (input.reachability, mcp_url)
    {
        return McpGate::Open { mcp_url };
    }
    let reachability = match input.reachability {
        ReachabilityInput::Known(r) => r.unreachable_info(),
        _ => None,
    };
    McpGate::Offline { reachability }
}

/// Outcome of `after_failed_check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCheckOutcome {
    pub result: Option<ConnectReachability>,
    pub since: Option<String>,
}

/// `afterFailedCheck` (`apps/web/src/components/mcp/mcpGate.ts`):
/// What a failed check request leaves: `prior` is the link state from before it, `latest` a read
/// after it (`None` when unread). The server may still have checked, so a result newer than `prior`
/// counts. Otherwise only a later server check does; with nothing read at all, only a retry does.
pub fn after_failed_check(
    prior: Option<&CloudLinkState>,
    latest: Option<&CloudLinkState>,
) -> FailedCheckOutcome {
    let fresh = latest.and_then(|s| s.reachability.as_ref());
    let known = prior.and_then(|s| s.reachability.as_ref());
    if let (Some(fresh), Some(_)) = (fresh, prior)
        && known.is_none_or(|known| fresh.checked_at() > known.checked_at())
    {
        return FailedCheckOutcome {
            result: Some(fresh.clone()),
            since: Some(fresh.checked_at().to_owned()),
        };
    }
    let baseline = latest.or(prior);
    FailedCheckOutcome {
        result: None,
        since: baseline.map(|b| {
            b.reachability
                .as_ref()
                .map(|r| r.checked_at().to_owned())
                .unwrap_or_default()
        }),
    }
}

/// Picks the effective reachability input from the dialog's check state and a later link-state report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReachabilityCheckState {
    Idle,
    Running,
    Done {
        result: Option<ConnectReachability>,
        since: Option<String>,
    },
}

impl ReachabilityCheckState {
    pub fn effective(&self, reported: Option<&ConnectReachability>) -> ReachabilityInput {
        match self {
            ReachabilityCheckState::Idle | ReachabilityCheckState::Running => {
                ReachabilityInput::Checking
            }
            ReachabilityCheckState::Done { result, since } => {
                if let (Some(rep), Some(since)) = (reported, since.as_deref())
                    && rep.checked_at() > since
                {
                    return ReachabilityInput::Known(rep.clone());
                }
                match result {
                    Some(known) => ReachabilityInput::Known(known.clone()),
                    None => ReachabilityInput::Failed,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reachable(checked_at: &str) -> ConnectReachability {
        ConnectReachability::Reachable {
            checked_at: checked_at.to_owned(),
            ready_connections: Some(4),
            protocol: Some("quic".to_owned()),
        }
    }

    fn tunnel() -> CloudLinkState {
        CloudLinkState {
            linked: true,
            cloud_user_id: Some("user_1".to_owned()),
            relay_url: Some("https://relay.example.test".to_owned()),
            relay_issuer: Some("https://relay.example.test".to_owned()),
            managed_tunnel_active: Some(true),
            publish_agent_activity: true,
            hold_webhooks_while_offline: None,
            link_revoked: None,
            reachability: None,
            tunnel_hostname: Some("prod-a1.t3coderelay.com".to_owned()),
            linkable_here: None,
        }
    }

    fn saved_tunnel() -> RelayEndpoint {
        RelayEndpoint {
            http_base_url: "https://prod-b2.t3coderelay.com".to_owned(),
            ws_base_url: "wss://prod-b2.t3coderelay.com".to_owned(),
            provider_kind: "cloudflare_tunnel".to_owned(),
        }
    }

    #[test]
    fn resolve_mcp_gate_opens_and_handles_saved_disconnected_states() {
        let link = tunnel();
        let listed_none = RelayGateState::Listed {
            endpoint: None,
            availability: None,
        };
        let reach = ReachabilityInput::Known(reachable("2026-11-08T00:00:00.000Z"));

        let open = resolve_mcp_gate(&McpGateInput {
            signed_in: Some(true),
            connected: true,
            approval_codes: Some(true),
            can_read_relay: true,
            link_state: Some(&link),
            link_state_error: None,
            this_machine: true,
            relay: &listed_none,
            reachability: &reach,
        });
        assert_eq!(
            open,
            McpGate::Open {
                mcp_url: "https://prod-a1.t3coderelay.com/mcp".to_owned(),
            }
        );

        let listed_saved = RelayGateState::Listed {
            endpoint: Some(saved_tunnel()),
            availability: None,
        };
        let saved = resolve_mcp_gate(&McpGateInput {
            signed_in: Some(true),
            connected: true,
            approval_codes: Some(true),
            can_read_relay: true,
            link_state: Some(&link),
            link_state_error: None,
            this_machine: false,
            relay: &listed_saved,
            reachability: &reach,
        });
        assert_eq!(
            saved,
            McpGate::Open {
                mcp_url: "https://prod-b2.t3coderelay.com/mcp".to_owned(),
            }
        );

        let check_away = |relay: RelayGateState| {
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: false,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: false,
                relay: &relay,
                reachability: &reach,
            })
        };
        assert_eq!(
            check_away(RelayGateState::Listed {
                endpoint: Some(saved_tunnel()),
                availability: Some(RelayAvailability::Checking),
            }),
            McpGate::Checking
        );
        assert_eq!(
            check_away(RelayGateState::Listed {
                endpoint: Some(saved_tunnel()),
                availability: Some(RelayAvailability::Offline),
            }),
            McpGate::Disconnected {
                relay: Some(DisconnectedRelay::Offline)
            }
        );
        assert_eq!(
            check_away(RelayGateState::Listed {
                endpoint: Some(saved_tunnel()),
                availability: Some(RelayAvailability::Error),
            }),
            McpGate::Disconnected {
                relay: Some(DisconnectedRelay::Failed)
            }
        );
        assert_eq!(
            check_away(RelayGateState::Listed {
                endpoint: Some(RelayEndpoint {
                    provider_kind: "manual".to_owned(),
                    ..saved_tunnel()
                }),
                availability: Some(RelayAvailability::Error),
            }),
            McpGate::Disconnected { relay: None }
        );
        assert_eq!(
            check_away(RelayGateState::Failed),
            McpGate::Disconnected {
                relay: Some(DisconnectedRelay::Failed)
            }
        );
        assert_eq!(
            check_away(RelayGateState::Listed {
                endpoint: Some(saved_tunnel()),
                availability: Some(RelayAvailability::Online),
            }),
            McpGate::Disconnected { relay: None }
        );

        // This machine's probe outranks a stale publish-only record in that list.
        let stale_manual = RelayGateState::Listed {
            endpoint: Some(RelayEndpoint {
                provider_kind: "manual".to_owned(),
                ..saved_tunnel()
            }),
            availability: None,
        };
        assert!(matches!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: true,
                relay: &stale_manual,
                reachability: &reach,
            }),
            McpGate::Open { .. }
        ));
    }

    #[test]
    fn resolve_mcp_gate_stays_closed_for_every_blocker() {
        let link = tunnel();
        let reach = ReachabilityInput::Known(reachable("2026-11-08T00:00:00.000Z"));
        let listed_none = RelayGateState::Listed {
            endpoint: None,
            availability: None,
        };
        let listed_saved = RelayGateState::Listed {
            endpoint: Some(saved_tunnel()),
            availability: None,
        };

        // Signed out
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(false),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: true,
                relay: &listed_none,
                reachability: &reach,
            }),
            McpGate::SignIn
        );
        // No relay read
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: false,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: true,
                relay: &listed_none,
                reachability: &reach,
            }),
            McpGate::NoRelayRead
        );
        // Server without approval codes
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(false),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: true,
                relay: &listed_none,
                reachability: &reach,
            }),
            McpGate::Update
        );
        // No tunnel
        let no_tunnel = CloudLinkState {
            managed_tunnel_active: Some(false),
            ..link.clone()
        };
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&no_tunnel),
                link_state_error: None,
                this_machine: true,
                relay: &listed_none,
                reachability: &reach,
            }),
            McpGate::TurnOn
        );
        // Revoked link
        let revoked = CloudLinkState {
            link_revoked: Some(true),
            ..link.clone()
        };
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&revoked),
                link_state_error: None,
                this_machine: true,
                relay: &listed_none,
                reachability: &reach,
            }),
            McpGate::TurnOn
        );
        // Publish-only saved link
        let publish_only = RelayGateState::Listed {
            endpoint: Some(RelayEndpoint {
                http_base_url: "http://127.0.0.1:3773".to_owned(),
                ws_base_url: "ws://127.0.0.1:3773".to_owned(),
                provider_kind: "manual".to_owned(),
            }),
            availability: None,
        };
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: false,
                relay: &publish_only,
                reachability: &reach,
            }),
            McpGate::TurnOn
        );
        // Saved machine unlisted
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: false,
                relay: &listed_none,
                reachability: &reach,
            }),
            McpGate::TurnOn
        );
        // Saved machine list loading
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: false,
                relay: &RelayGateState::Loading,
                reachability: &reach,
            }),
            McpGate::Checking
        );
        // Disconnected saved machine while list refreshes
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: false,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: false,
                relay: &RelayGateState::Loading,
                reachability: &reach,
            }),
            McpGate::Checking
        );
        // No tunnel before list loads
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&no_tunnel),
                link_state_error: None,
                this_machine: false,
                relay: &RelayGateState::Loading,
                reachability: &reach,
            }),
            McpGate::TurnOn
        );
        // Check still running
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: true,
                relay: &listed_saved,
                reachability: &ReachabilityInput::Checking,
            }),
            McpGate::Checking
        );
        // Offline tunnel
        let unreach = ReachabilityInput::Known(ConnectReachability::Unreachable {
            checked_at: "2026-11-08T00:00:00.000Z".to_owned(),
            cause: "no_connector".to_owned(),
            reason: "Cloudflare has no live connection from this machine for the tunnel."
                .to_owned(),
            hint: None,
            fix: "Restart T3 Code.".to_owned(),
        });
        assert!(matches!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: true,
                relay: &listed_none,
                reachability: &unreach,
            }),
            McpGate::Offline {
                reachability: Some(_)
            }
        ));
        // Failed check
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&link),
                link_state_error: None,
                this_machine: true,
                relay: &listed_none,
                reachability: &ReachabilityInput::Failed,
            }),
            McpGate::Offline { reachability: None }
        );
        // Missing tunnel hostname
        let no_host = CloudLinkState {
            tunnel_hostname: None,
            ..link.clone()
        };
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&no_host),
                link_state_error: None,
                this_machine: true,
                relay: &listed_none,
                reachability: &reach,
            }),
            McpGate::Offline { reachability: None }
        );
        // Non-plain DNS hostname (injection attempt)
        let bad_host = CloudLinkState {
            tunnel_hostname: Some("a.com;touch$IFS/tmp/x".to_owned()),
            ..link
        };
        assert_eq!(
            resolve_mcp_gate(&McpGateInput {
                signed_in: Some(true),
                connected: true,
                approval_codes: Some(true),
                can_read_relay: true,
                link_state: Some(&bad_host),
                link_state_error: None,
                this_machine: true,
                relay: &listed_none,
                reachability: &reach,
            }),
            McpGate::Offline { reachability: None }
        );
    }

    #[test]
    fn after_failed_check_matches_web_cases() {
        let base = tunnel();
        let older = CloudLinkState {
            reachability: Some(reachable("2026-11-08T00:00:00.000Z")),
            ..base.clone()
        };
        let newer = CloudLinkState {
            reachability: Some(reachable("2026-11-08T00:01:00.000Z")),
            ..base.clone()
        };
        assert_eq!(
            after_failed_check(Some(&older), Some(&newer)),
            FailedCheckOutcome {
                result: newer.reachability.clone(),
                since: Some("2026-11-08T00:01:00.000Z".to_owned()),
            }
        );
        assert_eq!(
            after_failed_check(Some(&newer), Some(&newer)),
            FailedCheckOutcome {
                result: None,
                since: Some("2026-11-08T00:01:00.000Z".to_owned()),
            }
        );
        assert_eq!(
            after_failed_check(None, Some(&newer)),
            FailedCheckOutcome {
                result: None,
                since: Some("2026-11-08T00:01:00.000Z".to_owned()),
            }
        );
        assert_eq!(
            after_failed_check(Some(&older), None),
            FailedCheckOutcome {
                result: None,
                since: Some("2026-11-08T00:00:00.000Z".to_owned()),
            }
        );
        assert_eq!(
            after_failed_check(None, None),
            FailedCheckOutcome {
                result: None,
                since: None,
            }
        );
        assert_eq!(
            after_failed_check(Some(&base), Some(&newer)),
            FailedCheckOutcome {
                result: newer.reachability.clone(),
                since: Some("2026-11-08T00:01:00.000Z".to_owned()),
            }
        );
        assert_eq!(
            after_failed_check(Some(&base), None),
            FailedCheckOutcome {
                result: None,
                since: Some(String::new()),
            }
        );
    }

    #[test]
    fn mcp_capabilities_decode_from_config_or_descriptor() {
        let cfg = serde_json::json!({
            "environment": {
                "capabilities": {
                    "mcpApprovalCodes": true,
                    "mcpHostedApproval": true,
                    "peerLinks": true
                }
            }
        });
        assert_eq!(
            McpCapabilities::from_json(&cfg),
            McpCapabilities {
                mcp_approval_codes: true,
                mcp_hosted_approval: true,
                peer_links: true,
            }
        );
        let desc = serde_json::json!({
            "capabilities": {
                "mcpApprovalCodes": true
            }
        });
        assert_eq!(
            McpCapabilities::from_json(&desc),
            McpCapabilities {
                mcp_approval_codes: true,
                mcp_hosted_approval: false,
                peer_links: false,
            }
        );
    }
}
