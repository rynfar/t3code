//! Agent OAuth sign-in approval flow logic, wire types, and URL parsing.
//! Ports `apps/web/src/components/auth/ConnectAgentSurface.logic.ts`,
//! `apps/web/src/components/auth/ConnectAgentSurface.tsx`,
//! `apps/web/src/components/auth/ApproveAgentSurface.tsx`,
//! `apps/web/src/routes/approve-agent.tsx`,
//! and `packages/client-runtime/src/state/connectMcpApproval.ts`.

use serde::{Deserialize, Serialize};

use crate::server_config::RuntimeMode;

use super::{
    McpClientAccess, is_runtime_mode_above, managed_runtime_mode_ceiling_message,
    parse_mcp_approval_code,
};

pub const ARM_DELAY_MS: u64 = 700;
pub const UNSAFE_REDIRECT: &str = "The environment returned an address this page will not open.";
pub const UNREACHABLE: &str = "Could not reach this environment. Try again.";
pub const CONNECT_UNAVAILABLE: &str =
    "Could not reach that environment through T3 Connect with this account.";

/// `AuthMcpAuthorizationRequest` (`packages/contracts/src/auth.ts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpAuthorizationRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_challenge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_challenge_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
}

impl McpAuthorizationRequest {
    pub fn from_query_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut req = Self::default();
        for (key, value) in pairs {
            match key {
                "response_type" => req.response_type = Some(value.to_owned()),
                "client_id" => req.client_id = Some(value.to_owned()),
                "redirect_uri" => req.redirect_uri = Some(value.to_owned()),
                "code_challenge" => req.code_challenge = Some(value.to_owned()),
                "code_challenge_method" => req.code_challenge_method = Some(value.to_owned()),
                "state" => req.state = Some(value.to_owned()),
                "resource" => req.resource = Some(value.to_owned()),
                _ => {}
            }
        }
        req
    }
}

/// Managed policy runtime-mode ceiling on `AuthMcpApprovalDetails`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeModeCeiling {
    pub organization: String,
    pub max_runtime_mode: RuntimeMode,
}

/// `AuthMcpApprovalDetails` (`packages/contracts/src/auth.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpApprovalDetails {
    pub client_name: String,
    pub redirect_host: String,
    pub environment_host: String,
    #[serde(default)]
    pub csrf_token: Option<String>,
    #[serde(default)]
    pub one_click_access: Option<Vec<McpClientAccess>>,
    #[serde(default)]
    pub runtime_mode_ceiling: Option<RuntimeModeCeiling>,
}

/// `AuthMcpApprovalDecision` (`packages/contracts/src/auth.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum McpApprovalDecision {
    #[serde(rename = "deny")]
    Deny {},
    #[serde(rename = "pairing-code")]
    PairingCode {
        access: McpClientAccess,
        code: String,
    },
    #[serde(rename = "browser-session", rename_all = "camelCase")]
    BrowserSession {
        access: McpClientAccess,
        csrf_token: String,
    },
}

/// `AuthMcpApprovalDecisionRequest` (`packages/contracts/src/auth.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpApprovalDecisionRequest {
    pub authorization: McpAuthorizationRequest,
    pub decision: McpApprovalDecision,
}

/// `ConnectMcpApprovalAnswer` / `McpApprovalAnswer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpApprovalAnswer {
    Details(McpApprovalDetails),
    Redirect(String),
    Refused(String),
}

impl McpApprovalAnswer {
    /// Decodes a 2xx JSON response from `POST /oauth/mcp/approval` or `POST /oauth/mcp/decision`.
    pub fn from_response_json(value: &serde_json::Value) -> Result<Self, String> {
        if let Some(redirect) = value.get("redirectTo").and_then(serde_json::Value::as_str) {
            return Ok(Self::Redirect(redirect.to_owned()));
        }
        serde_json::from_value::<McpApprovalDetails>(value.clone())
            .map(Self::Details)
            .map_err(|_| UNREACHABLE.to_owned())
    }
}

/// Kind of incoming MCP agent approval URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalUrlKind {
    /// `/approve-agent?...&environment=<id>` (hosted T3 Connect approval flow).
    Hosted {
        environment_id: Option<String>,
        raw_url: String,
        search_without_environment: String,
    },
    /// `/connect-agent?...` (direct environment approval flow).
    Direct {
        origin: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedApprovalUrl {
    pub kind: ApprovalUrlKind,
    pub authorization: McpAuthorizationRequest,
}

/// Parses an `/approve-agent` or `/connect-agent` URL (`https://`, `http://`, or `t3code-native://`).
pub fn parse_approval_url(raw: &str) -> Option<ParsedApprovalUrl> {
    let parsed = url::Url::parse(raw.trim()).ok()?;
    let scheme = parsed.scheme();
    let is_custom = scheme == "t3code-native" || scheme == "t3code";
    let route = if is_custom {
        let host = parsed.host_str().unwrap_or("");
        let path = parsed.path().trim_matches('/');
        if !host.is_empty() && path.is_empty() {
            format!("/{host}")
        } else if !host.is_empty() {
            format!("/{host}/{path}")
        } else {
            format!("/{path}")
        }
    } else if matches!(scheme, "http" | "https") {
        parsed.path().trim_end_matches('/').to_owned()
    } else {
        return None;
    };

    let pairs: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
    let authorization = McpAuthorizationRequest::from_query_pairs(
        pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    );

    if route == "/approve-agent" {
        let environment_id = pairs
            .iter()
            .find(|(k, _)| k == "environment")
            .map(|(_, v)| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &pairs {
            if k != "environment" {
                ser.append_pair(k, v);
            }
        }
        let search_without_environment = ser.finish();
        return Some(ParsedApprovalUrl {
            kind: ApprovalUrlKind::Hosted {
                environment_id,
                raw_url: raw.trim().to_owned(),
                search_without_environment,
            },
            authorization,
        });
    }

    if route == "/connect-agent" {
        let origin = (!is_custom).then(|| parsed.origin().ascii_serialization());
        return Some(ParsedApprovalUrl {
            kind: ApprovalUrlKind::Direct { origin },
            authorization,
        });
    }

    None
}

/// Builds `<environmentUrl>/connect-agent?<search>` for `"Use a sign-in code instead"`.
pub fn connect_agent_fallback_url(
    environment_url: &str,
    search_without_environment: &str,
) -> Option<String> {
    let base = url::Url::parse(environment_url).ok()?;
    let mut target = base.join("/connect-agent").ok()?;
    if !search_without_environment.is_empty() {
        target.set_query(Some(search_without_environment));
    }
    Some(target.into())
}

/// `isLoopbackHost` (`packages/shared/src/preview.ts`).
pub fn is_loopback_host(host: &str) -> bool {
    let lower = host.trim().to_ascii_lowercase();
    let bare = lower
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(&lower);
    bare == "localhost"
        || bare == "::1"
        || bare == "0:0:0:0:0:0:0:1"
        || bare.ends_with(".localhost")
        || bare.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
        || bare.parse::<std::net::Ipv6Addr>().is_ok_and(|ip| ip.is_loopback())
}

/// `redirectsToThisComputer` (`ConnectAgentSurface.tsx`).
pub fn redirects_to_this_computer(redirect_host: &str) -> bool {
    url::Url::parse(&format!("http://{redirect_host}"))
        .ok()
        .and_then(|u| u.host_str().map(is_loopback_host))
        .unwrap_or(false)
}

/// `resolveMcpAccess` (`ConnectAgentSurface.logic.ts`):
/// The picker's access, lowered to a managed ceiling: the user's choice, else Auto for a typed
/// code, else Read only, since approving without a code says nothing about what the user wants.
pub fn resolve_mcp_access(
    requested: Option<McpClientAccess>,
    ceiling: Option<&RuntimeModeCeiling>,
    has_code: bool,
) -> McpClientAccess {
    let access = requested.unwrap_or(if has_code {
        McpClientAccess::Auto
    } else {
        McpClientAccess::ReadOnly
    });
    if let (Some(mode), Some(ceiling)) = (access.as_runtime_mode(), ceiling)
        && is_runtime_mode_above(mode, Some(ceiling.max_runtime_mode))
    {
        return McpClientAccess::from_runtime_mode(ceiling.max_runtime_mode);
    }
    access
}

/// `resolveApprovalDecision` (`ConnectAgentSurface.logic.ts`):
/// What Approve sends. A typed code is always checked as a code, even when the session could
/// approve, and a level-bound code sends its own level. `None`: nothing to send.
pub fn resolve_approval_decision(
    code: &str,
    access: McpClientAccess,
    details: &McpApprovalDetails,
) -> Option<McpApprovalDecision> {
    let trimmed = code.trim();
    if !trimmed.is_empty() {
        let effective_access = parse_mcp_approval_code(trimmed)
            .map(|parsed| parsed.access)
            .unwrap_or(access);
        return Some(McpApprovalDecision::PairingCode {
            access: effective_access,
            code: trimmed.to_owned(),
        });
    }
    let csrf_token = details.csrf_token.as_ref()?;
    let allowed = details
        .one_click_access
        .as_deref()
        .is_some_and(|list| list.contains(&access));
    allowed.then(|| McpApprovalDecision::BrowserSession {
        access,
        csrf_token: csrf_token.clone(),
    })
}

/// `ceilingReason` (`ConnectAgentSurface.tsx`):
/// Why a managed policy keeps `access` from being granted, or `None` when it may be.
pub fn ceiling_reason(details: &McpApprovalDetails, access: McpClientAccess) -> Option<String> {
    let ceiling = details.runtime_mode_ceiling.as_ref()?;
    let mode = access.as_runtime_mode()?;
    is_runtime_mode_above(mode, Some(ceiling.max_runtime_mode))
        .then(|| managed_runtime_mode_ceiling_message(&ceiling.organization, ceiling.max_runtime_mode))
}

/// `agentRedirectUrl` (`ConnectAgentSurface.logic.ts`):
/// The address to send the browser/agent to after a decision, or `None`. Only `https`, or `http`
/// on loopback, can carry a sign-in to an agent; anything else, such as `javascript:`, is refused.
pub fn agent_redirect_url(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    let ok = parsed.scheme() == "https"
        || (parsed.scheme() == "http" && parsed.host_str().is_some_and(is_loopback_host));
    ok.then(|| parsed.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_mcp_access_within_policy_ceiling() {
        for max_mode in RuntimeMode::ALL {
            let ceiling = RuntimeModeCeiling {
                organization: "Example".to_owned(),
                max_runtime_mode: max_mode,
            };
            let expected = if max_mode == RuntimeMode::FullAccess {
                McpClientAccess::Auto
            } else {
                McpClientAccess::from_runtime_mode(max_mode)
            };
            assert_eq!(
                resolve_mcp_access(Some(McpClientAccess::Auto), Some(&ceiling), false),
                expected
            );
            assert_eq!(
                resolve_mcp_access(Some(McpClientAccess::ReadOnly), Some(&ceiling), false),
                McpClientAccess::ReadOnly
            );
            assert_eq!(
                resolve_mcp_access(
                    Some(McpClientAccess::ApprovalRequired),
                    Some(&ceiling),
                    false
                ),
                McpClientAccess::ApprovalRequired
            );
        }
    }

    #[test]
    fn checks_typed_code_even_when_signed_in_and_approves_without_one_at_read_only() {
        let signed_out = McpApprovalDetails {
            client_name: "Poke".to_owned(),
            redirect_host: "poke.com".to_owned(),
            environment_host: "prod-a1.t3coderelay.com".to_owned(),
            csrf_token: None,
            one_click_access: None,
            runtime_mode_ceiling: None,
        };
        let details = McpApprovalDetails {
            csrf_token: Some("csrf".to_owned()),
            one_click_access: Some(McpClientAccess::ALL.to_vec()),
            ..signed_out.clone()
        };
        let access = resolve_mcp_access(None, None, false);

        assert_eq!(
            resolve_approval_decision(
                " 7K2P9QXR4MZT ",
                resolve_mcp_access(None, None, true),
                &details
            ),
            Some(McpApprovalDecision::PairingCode {
                access: McpClientAccess::Auto,
                code: "7K2P9QXR4MZT".to_owned(),
            })
        );
        assert_eq!(
            resolve_approval_decision("auto-q8rz3nwd5ktc", access, &details),
            Some(McpApprovalDecision::PairingCode {
                access: McpClientAccess::Auto,
                code: "auto-q8rz3nwd5ktc".to_owned(),
            })
        );
        assert_eq!(
            resolve_approval_decision("", access, &details),
            Some(McpApprovalDecision::BrowserSession {
                access: McpClientAccess::ReadOnly,
                csrf_token: "csrf".to_owned(),
            })
        );
        assert_eq!(resolve_approval_decision("", access, &signed_out), None);
    }

    #[test]
    fn follows_only_https_or_loopback_http_redirects_to_an_agent() {
        assert_eq!(
            agent_redirect_url("https://www.cursor.com/cb?code=x").as_deref(),
            Some("https://www.cursor.com/cb?code=x")
        );
        assert_eq!(
            agent_redirect_url("http://localhost:51234/callback?code=x").as_deref(),
            Some("http://localhost:51234/callback?code=x")
        );
        assert_eq!(
            agent_redirect_url("http://127.0.0.1:8080/cb").as_deref(),
            Some("http://127.0.0.1:8080/cb")
        );
        for url in [
            "javascript:alert(document.domain)",
            "data:text/html,hi",
            "http://evil.example/cb",
            "not a url",
        ] {
            assert_eq!(agent_redirect_url(url), None, "{url}");
        }
    }

    #[test]
    fn parses_approve_agent_and_connect_agent_urls() {
        let hosted = parse_approval_url(
            "https://app.t3.codes/approve-agent?environment=env-1&client_id=cid&redirect_uri=https%3A%2F%2Fpoke.com%2Fcb&response_type=code&state=s1",
        )
        .expect("hosted approval url");
        assert_eq!(
            hosted.kind,
            ApprovalUrlKind::Hosted {
                environment_id: Some("env-1".to_owned()),
                raw_url: "https://app.t3.codes/approve-agent?environment=env-1&client_id=cid&redirect_uri=https%3A%2F%2Fpoke.com%2Fcb&response_type=code&state=s1".to_owned(),
                search_without_environment: "client_id=cid&redirect_uri=https%3A%2F%2Fpoke.com%2Fcb&response_type=code&state=s1".to_owned(),
            }
        );
        assert_eq!(hosted.authorization.client_id.as_deref(), Some("cid"));
        assert_eq!(hosted.authorization.state.as_deref(), Some("s1"));

        let direct = parse_approval_url(
            "https://studio.t3coderelay.com/connect-agent?client_id=cid&response_type=code",
        )
        .expect("direct approval url");
        assert_eq!(
            direct.kind,
            ApprovalUrlKind::Direct {
                origin: Some("https://studio.t3coderelay.com".to_owned())
            }
        );
        assert_eq!(direct.authorization.client_id.as_deref(), Some("cid"));

        let native_scheme = parse_approval_url(
            "t3code-native://approve-agent?environment=env-2&client_id=claude",
        )
        .expect("native scheme");
        assert!(matches!(
            native_scheme.kind,
            ApprovalUrlKind::Hosted {
                environment_id: Some(ref id),
                ..
            } if id == "env-2"
        ));

        assert!(parse_approval_url("https://app.t3.codes/new?prompt=hi").is_none());
    }

    #[test]
    fn decision_json_matches_contract_tags() {
        let deny = serde_json::to_value(McpApprovalDecision::Deny {}).unwrap();
        assert_eq!(deny, serde_json::json!({ "_tag": "deny" }));
        let code = serde_json::to_value(McpApprovalDecision::PairingCode {
            access: McpClientAccess::AutoAcceptEdits,
            code: "EDITS-23456789ABCD".to_owned(),
        })
        .unwrap();
        assert_eq!(
            code,
            serde_json::json!({
                "_tag": "pairing-code",
                "access": "auto-accept-edits",
                "code": "EDITS-23456789ABCD"
            })
        );
        let session = serde_json::to_value(McpApprovalDecision::BrowserSession {
            access: McpClientAccess::ReadOnly,
            csrf_token: "tok".to_owned(),
        })
        .unwrap();
        assert_eq!(
            session,
            serde_json::json!({
                "_tag": "browser-session",
                "access": "read-only",
                "csrfToken": "tok"
            })
        );
    }

    #[test]
    fn redirects_to_this_computer_checks_loopback() {
        assert!(redirects_to_this_computer("localhost:51234"));
        assert!(redirects_to_this_computer("127.0.0.1:8080"));
        assert!(redirects_to_this_computer("[::1]:9000"));
        assert!(!redirects_to_this_computer("poke.com"));
    }
}
