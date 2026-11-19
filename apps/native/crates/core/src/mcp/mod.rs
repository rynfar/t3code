//! MCP server access ("Use in another app") and OAuth agent sign-in approval flow.
//! Ports `packages/contracts/src/auth.ts` (`AuthMcpClientAccess`, `mcpClientScopes`,
//! `mcpApprovalCode`, `parseMcpApprovalCode`), `packages/contracts/src/managedPolicy.ts`
//! (`managedRuntimeModeDenial`, `managedRuntimeModeCeilingMessage`),
//! `apps/web/src/components/mcp/{UseInAnotherAppDialog.tsx,mcpApps.ts,mcpGate.ts}`,
//! `apps/web/src/components/auth/{ApproveAgentSurface.tsx,ConnectAgentSurface.tsx,ConnectAgentSurface.logic.ts}`,
//! and `apps/web/src/components/settings/useAccessEnvironment.ts`.

pub mod approval;
pub mod apps;
pub mod gate;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::{
    ACCESS_WRITE_SCOPE, AuthSessionState, ORCHESTRATION_OPERATE_SCOPE, ORCHESTRATION_READ_SCOPE,
    SOURCE_CONTROL_WRITE_SCOPE,
};
use crate::managed_policy::{self, Capability};
use crate::server_config::RuntimeMode;

pub const SETTINGS_WRITE_SCOPE: &str = "settings:write";
pub const RELAY_READ_SCOPE: &str = "relay:read";
pub const RELAY_WRITE_SCOPE: &str = "relay:write";
pub const MCP_APPROVAL_CODE_LABEL: &str = "App sign-in";
pub const PAIRING_CODE_ALPHABET: &str = "23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
pub const PAIRING_CODE_LENGTH: usize = 12;

/// `AuthMcpClientAccess` (`packages/contracts/src/auth.ts`), in picker & rank order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpClientAccess {
    ReadOnly,
    ApprovalRequired,
    AutoAcceptEdits,
    Auto,
    FullAccess,
}

impl McpClientAccess {
    pub const ALL: [McpClientAccess; 5] = [
        McpClientAccess::ReadOnly,
        McpClientAccess::ApprovalRequired,
        McpClientAccess::AutoAcceptEdits,
        McpClientAccess::Auto,
        McpClientAccess::FullAccess,
    ];

    /// The level the Access step starts at: Auto, else the next one down that can be granted.
    pub const DEFAULT_ORDER: [McpClientAccess; 4] = [
        McpClientAccess::Auto,
        McpClientAccess::AutoAcceptEdits,
        McpClientAccess::ApprovalRequired,
        McpClientAccess::ReadOnly,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            McpClientAccess::ReadOnly => "read-only",
            McpClientAccess::ApprovalRequired => "approval-required",
            McpClientAccess::AutoAcceptEdits => "auto-accept-edits",
            McpClientAccess::Auto => "auto",
            McpClientAccess::FullAccess => "full-access",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == value)
    }

    pub fn rank(self) -> usize {
        match self {
            McpClientAccess::ReadOnly => 0,
            McpClientAccess::ApprovalRequired => 1,
            McpClientAccess::AutoAcceptEdits => 2,
            McpClientAccess::Auto => 3,
            McpClientAccess::FullAccess => 4,
        }
    }

    pub fn as_runtime_mode(self) -> Option<RuntimeMode> {
        match self {
            McpClientAccess::ReadOnly => None,
            McpClientAccess::ApprovalRequired => Some(RuntimeMode::ApprovalRequired),
            McpClientAccess::AutoAcceptEdits => Some(RuntimeMode::AutoAcceptEdits),
            McpClientAccess::Auto => Some(RuntimeMode::Auto),
            McpClientAccess::FullAccess => Some(RuntimeMode::FullAccess),
        }
    }

    pub fn from_runtime_mode(mode: RuntimeMode) -> Self {
        match mode {
            RuntimeMode::ApprovalRequired => McpClientAccess::ApprovalRequired,
            RuntimeMode::AutoAcceptEdits => McpClientAccess::AutoAcceptEdits,
            RuntimeMode::Auto => McpClientAccess::Auto,
            RuntimeMode::FullAccess => McpClientAccess::FullAccess,
        }
    }

    /// Label shown in `accessConfig` (`ConnectAgentSurface.tsx`).
    pub fn label(self) -> &'static str {
        match self {
            McpClientAccess::ReadOnly => "Read only",
            McpClientAccess::ApprovalRequired => {
                crate::composer::runtime_mode_label(RuntimeMode::ApprovalRequired)
            }
            McpClientAccess::AutoAcceptEdits => {
                crate::composer::runtime_mode_label(RuntimeMode::AutoAcceptEdits)
            }
            McpClientAccess::Auto => crate::composer::runtime_mode_label(RuntimeMode::Auto),
            McpClientAccess::FullAccess => {
                crate::composer::runtime_mode_label(RuntimeMode::FullAccess)
            }
        }
    }

    /// Default description shown on the agent sign-in consent screen (`accessConfig`).
    pub fn description(self) -> &'static str {
        match self {
            McpClientAccess::ReadOnly => {
                "Read projects and threads. Cannot start, message or change anything."
            }
            McpClientAccess::ApprovalRequired => {
                crate::composer::runtime_mode_description(RuntimeMode::ApprovalRequired)
            }
            McpClientAccess::AutoAcceptEdits => {
                crate::composer::runtime_mode_description(RuntimeMode::AutoAcceptEdits)
            }
            McpClientAccess::Auto => crate::composer::runtime_mode_description(RuntimeMode::Auto),
            McpClientAccess::FullAccess => {
                crate::composer::runtime_mode_description(RuntimeMode::FullAccess)
            }
        }
    }

    /// Description override used in the "Use in another app" wizard step 1 (`UseInAnotherAppDialog.tsx`).
    pub fn wizard_description(self) -> Option<&'static str> {
        match self {
            McpClientAccess::FullAccess => Some("Runs commands and edits files without asking"),
            _ => None,
        }
    }

    pub fn approval_code_prefix(self) -> &'static str {
        match self {
            McpClientAccess::ReadOnly => "READ",
            McpClientAccess::ApprovalRequired => "SUPERVISED",
            McpClientAccess::AutoAcceptEdits => "EDITS",
            McpClientAccess::Auto => "AUTO",
            McpClientAccess::FullAccess => "FULL",
        }
    }
}

/// `mcpClientScopes` (`packages/contracts/src/auth.ts`).
pub fn mcp_client_scopes(access: McpClientAccess) -> &'static [&'static str] {
    match access {
        McpClientAccess::ReadOnly => &[ORCHESTRATION_READ_SCOPE],
        McpClientAccess::FullAccess => &[
            ORCHESTRATION_READ_SCOPE,
            ORCHESTRATION_OPERATE_SCOPE,
            SETTINGS_WRITE_SCOPE,
            SOURCE_CONTROL_WRITE_SCOPE,
        ],
        McpClientAccess::ApprovalRequired
        | McpClientAccess::AutoAcceptEdits
        | McpClientAccess::Auto => &[ORCHESTRATION_READ_SCOPE, ORCHESTRATION_OPERATE_SCOPE],
    }
}

/// `mcpApprovalCode` (`packages/contracts/src/auth.ts`).
pub fn mcp_approval_code(access: McpClientAccess, token: &str) -> String {
    format!("{}-{token}", access.approval_code_prefix())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMcpApprovalCode {
    pub access: McpClientAccess,
    pub code: String,
}

/// `parseMcpApprovalCode` (`packages/contracts/src/auth.ts`):
/// The level a code approves and the code as issued, or `None` for any other code.
pub fn parse_mcp_approval_code(code: &str) -> Option<ParsedMcpApprovalCode> {
    let trimmed = code.trim();
    let (raw_prefix, raw_token) = trimmed.split_once('-')?;
    if raw_token.len() != PAIRING_CODE_LENGTH {
        return None;
    }
    let upper_token = raw_token.to_ascii_uppercase();
    if !upper_token
        .bytes()
        .all(|b| PAIRING_CODE_ALPHABET.as_bytes().contains(&b))
    {
        return None;
    }
    let upper_prefix = raw_prefix.to_ascii_uppercase();
    let access = McpClientAccess::ALL
        .into_iter()
        .find(|level| level.approval_code_prefix() == upper_prefix)?;
    Some(ParsedMcpApprovalCode {
        access,
        code: mcp_approval_code(access, &upper_token),
    })
}

/// `isRuntimeModeAbove` (`packages/contracts/src/providerPolicy.ts`).
pub fn is_runtime_mode_above(mode: RuntimeMode, ceiling: Option<RuntimeMode>) -> bool {
    let rank = |m: RuntimeMode| match m {
        RuntimeMode::ApprovalRequired => 0u8,
        RuntimeMode::AutoAcceptEdits => 1,
        RuntimeMode::Auto => 2,
        RuntimeMode::FullAccess => 3,
    };
    ceiling.is_some_and(|c| rank(mode) > rank(c))
}

/// `managedRuntimeModeCeilingMessage` (`packages/contracts/src/managedPolicy.ts`).
pub fn managed_runtime_mode_ceiling_message(organization: &str, ceiling: RuntimeMode) -> String {
    format!(
        "{organization} limits threads to {}.",
        crate::composer::runtime_mode_label(ceiling)
    )
}

/// `managedRuntimeModeDenial` (`packages/contracts/src/managedPolicy.ts`).
pub fn managed_runtime_mode_denial(policy: Option<&Value>, mode: RuntimeMode) -> Option<String> {
    let p = policy?;
    let ceiling = p
        .get("limits")
        .and_then(|l| l.get("maxRuntimeMode"))
        .and_then(Value::as_str)
        .and_then(RuntimeMode::parse)?;
    is_runtime_mode_above(mode, Some(ceiling))
        .then(|| managed_runtime_mode_ceiling_message(managed_policy::organization(p), ceiling))
}

/// `useGrantableLevels` (`UseInAnotherAppDialog.tsx`):
/// Map from each ungrantable `McpClientAccess` level to the reason it cannot be granted.
pub fn ungrantable_levels(
    environment_label: &str,
    session: Option<&AuthSessionState>,
    policy: Option<&Value>,
) -> HashMap<McpClientAccess, String> {
    let remote_denial = managed_policy::capability_denial(policy, Capability::RemoteClients);
    let can_mint = session.is_some_and(|s| s.grants(ACCESS_WRITE_SCOPE));
    let mut map = HashMap::new();
    for level in McpClientAccess::ALL {
        let reason = if let Some(denial) = remote_denial.clone() {
            Some(denial)
        } else if !can_mint {
            Some(format!(
                "This device needs the Manage access permission on {environment_label}."
            ))
        } else if session.is_none_or(|s| {
            !mcp_client_scopes(level)
                .iter()
                .all(|scope| s.grants(scope))
        }) {
            Some(format!(
                "This device cannot grant this on {environment_label}."
            ))
        } else if let Some(mode) = level.as_runtime_mode() {
            managed_runtime_mode_denial(policy, mode)
        } else {
            None
        };
        if let Some(reason) = reason {
            map.insert(level, reason);
        }
    }
    map
}

/// Default access level in the Access step (`DEFAULT_LEVELS.find(...)`).
pub fn default_wizard_access(
    disabled: &HashMap<McpClientAccess, String>,
) -> Option<McpClientAccess> {
    McpClientAccess::DEFAULT_ORDER
        .into_iter()
        .find(|level| !disabled.contains_key(level))
}

/// `POST /api/auth/pairing-token` payload for `createMcpApprovalCodeCommand`.
pub fn approval_code_request_body(access: McpClientAccess) -> Value {
    serde_json::json!({
        "label": MCP_APPROVAL_CODE_LABEL,
        "access": access.as_str(),
    })
}

/// Decoded `AuthPairingCredentialResult` returned from `POST /api/auth/pairing-token`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MintedApprovalCode {
    pub id: String,
    pub credential: String,
    #[serde(default)]
    pub label: Option<String>,
    pub expires_at: String,
    #[serde(default)]
    pub access: Option<McpClientAccess>,
}

/// Formats remaining lifetime `M:SS` for `"Expires M:SS · one use"`.
pub fn format_code_expiry(remaining_secs: u64) -> String {
    format!(
        "Expires {}:{:02} \u{00b7} one use",
        remaining_secs / 60,
        remaining_secs % 60
    )
}

/// One peer link entry returned by `peerLinks.list` (`PeerLinkSummary`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerLinkEntry {
    pub environment_id: String,
    pub label: String,
    pub access: McpClientAccess,
    #[serde(default)]
    pub status: String,
}

/// `LinkedLine` (`UseInAnotherAppDialog.tsx`):
/// Formats `"Linked: {name} · {level/Expired}, ..."`, clamping each link's access to `access`
/// when `access` is known (`!hostedApproval`).
pub fn format_linked_line(
    links: &[PeerLinkEntry],
    access: Option<McpClientAccess>,
    resolve_label: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if links.is_empty() {
        return None;
    }
    let parts: Vec<String> = links
        .iter()
        .map(|link| {
            let name = resolve_label(&link.environment_id).unwrap_or_else(|| link.label.clone());
            let level = match access {
                Some(app_level) if app_level.rank() < link.access.rank() => app_level,
                _ => link.access,
            };
            let suffix = if link.status == "expired" {
                "Expired"
            } else {
                level.label()
            };
            format!("{name} \u{00b7} {suffix}")
        })
        .collect();
    Some(format!("Linked: {}", parts.join(", ")))
}

/// Candidate environment for `resolve_access_environment_id` (`useAccessEnvironment.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessEnvironmentCandidate {
    pub id: String,
    pub environment_id: Option<String>,
    pub connected: bool,
}

/// `resolveAccessEnvironmentId` (`ConnectionsSettings.logic.ts` / `useAccessEnvironment.ts`):
/// Resolves `requested` (matching either saved id or canonical environment id), else connected
/// primary/active, else primary/active, else first connected candidate, else first candidate.
pub fn resolve_access_environment_id<'a>(
    primary_id: Option<&str>,
    candidates: &'a [AccessEnvironmentCandidate],
    requested: Option<&str>,
) -> Option<&'a str> {
    let matches_id = |c: &AccessEnvironmentCandidate, needle: &str| {
        c.id == needle || c.environment_id.as_deref() == Some(needle)
    };
    if let Some(req) = requested
        && let Some(found) = candidates.iter().find(|c| matches_id(c, req))
    {
        return Some(found.id.as_str());
    }
    if let Some(primary) = primary_id
        && let Some(found) = candidates.iter().find(|c| matches_id(c, primary) && c.connected)
    {
        return Some(found.id.as_str());
    }
    if let Some(found) = candidates.iter().find(|c| c.connected) {
        return Some(found.id.as_str());
    }
    if let Some(primary) = primary_id
        && let Some(found) = candidates.iter().find(|c| matches_id(c, primary))
    {
        return Some(found.id.as_str());
    }
    candidates.first().map(|c| c.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_mcp_approval_code_handles_all_prefixes_and_rejects_invalid() {
        for level in McpClientAccess::ALL {
            let issued = mcp_approval_code(level, "Q8RZ3NWD5KTC");
            let parsed = parse_mcp_approval_code(&issued.to_lowercase()).expect("parses lowercase");
            assert_eq!(parsed.access, level);
            assert_eq!(parsed.code, issued);
        }
        // Plain pairing code without level prefix returns None.
        assert_eq!(parse_mcp_approval_code("7K2P9QXR4MZT"), None);
        // Ambiguous alphabet chars (0, 1, I, O) rejected.
        assert_eq!(parse_mcp_approval_code("AUTO-01IOQ8RZ3NWD"), None);
        // Wrong length rejected.
        assert_eq!(parse_mcp_approval_code("AUTO-SHORT"), None);
    }

    #[test]
    fn grantable_levels_respects_scopes_and_managed_policy() {
        let full_session = AuthSessionState {
            authenticated: true,
            auth: Default::default(),
            scopes: Some(vec![
                ACCESS_WRITE_SCOPE.to_owned(),
                ORCHESTRATION_READ_SCOPE.to_owned(),
                ORCHESTRATION_OPERATE_SCOPE.to_owned(),
                SETTINGS_WRITE_SCOPE.to_owned(),
                SOURCE_CONTROL_WRITE_SCOPE.to_owned(),
            ]),
            permissions: None,
        };
        let policy = json!({
            "organization": "Acme",
            "limits": { "maxRuntimeMode": "auto-accept-edits" }
        });
        let disabled = ungrantable_levels("Mac Studio", Some(&full_session), Some(&policy));
        assert!(!disabled.contains_key(&McpClientAccess::ReadOnly));
        assert!(!disabled.contains_key(&McpClientAccess::ApprovalRequired));
        assert!(!disabled.contains_key(&McpClientAccess::AutoAcceptEdits));
        assert_eq!(
            disabled.get(&McpClientAccess::Auto).map(String::as_str),
            Some("Acme limits threads to Auto-accept edits.")
        );
        assert_eq!(
            disabled.get(&McpClientAccess::FullAccess).map(String::as_str),
            Some("Acme limits threads to Auto-accept edits.")
        );
        assert_eq!(
            default_wizard_access(&disabled),
            Some(McpClientAccess::AutoAcceptEdits)
        );

        let no_write_session = AuthSessionState {
            authenticated: true,
            auth: Default::default(),
            scopes: Some(vec![ORCHESTRATION_READ_SCOPE.to_owned()]),
            permissions: None,
        };
        let denied = ungrantable_levels("Mac Studio", Some(&no_write_session), None);
        assert_eq!(
            denied.get(&McpClientAccess::ReadOnly).map(String::as_str),
            Some("This device needs the Manage access permission on Mac Studio.")
        );
        assert_eq!(default_wizard_access(&denied), None);

        let remote_blocked = json!({
            "organization": "Acme",
            "limits": { "clients": ["desktop"] }
        });
        let blocked = ungrantable_levels("Mac Studio", Some(&full_session), Some(&remote_blocked));
        assert_eq!(
            blocked.get(&McpClientAccess::ReadOnly).map(String::as_str),
            Some("Acme's policy does not allow connecting from other devices.")
        );
    }

    #[test]
    fn linked_line_clamps_to_app_access_and_marks_expired() {
        let links = vec![
            PeerLinkEntry {
                environment_id: "env-2".to_owned(),
                label: "Server".to_owned(),
                access: McpClientAccess::FullAccess,
                status: "reachable".to_owned(),
            },
            PeerLinkEntry {
                environment_id: "env-3".to_owned(),
                label: "Old".to_owned(),
                access: McpClientAccess::ReadOnly,
                status: "expired".to_owned(),
            },
        ];
        let clamped = format_linked_line(&links, Some(McpClientAccess::Auto), |id| {
            (id == "env-2").then(|| "Build Box".to_owned())
        });
        assert_eq!(
            clamped.as_deref(),
            Some("Linked: Build Box \u{00b7} Auto, Old \u{00b7} Expired")
        );
        let hosted = format_linked_line(&links, None, |_| None);
        assert_eq!(
            hosted.as_deref(),
            Some("Linked: Server \u{00b7} Full access, Old \u{00b7} Expired")
        );
    }

    #[test]
    fn code_expiry_and_request_body_match_web() {
        assert_eq!(format_code_expiry(125), "Expires 2:05 \u{00b7} one use");
        assert_eq!(format_code_expiry(0), "Expires 0:00 \u{00b7} one use");
        assert_eq!(
            approval_code_request_body(McpClientAccess::Auto),
            json!({ "label": "App sign-in", "access": "auto" })
        );
    }
}
