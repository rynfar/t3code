//! Display and resolution helpers for secret requests, link requests, and context handoffs.
//! Ports `packages/client-runtime/src/{secretRequest,linkRequest,handoff}.ts`.

use serde_json::Value;

use crate::auth::{ACCESS_WRITE_SCOPE, AuthSessionState};
use crate::providers::ProviderEntry;

/// Display model for a `secret_request` timeline item (`secretRequestDisplay`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRequestDisplay {
    pub tone: SecretTone,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretTone {
    Pending,
    PendingElsewhere,
    Saved,
    Declined,
    TimedOut,
}

pub fn secret_request_privacy_note(delivery: Option<&str>) -> &'static str {
    if delivery == Some("file") {
        "Kept out of chat, in a private file this agent's commands can read for 24 hours"
    } else {
        "Stored securely, never shown to the agent"
    }
}

pub fn secret_request_display(secret_status: &str, label: &str, can_answer_here: bool) -> SecretRequestDisplay {
    let trimmed = if label.trim().is_empty() { "Secret" } else { label.trim() };
    match secret_status {
        "saved" => SecretRequestDisplay {
            tone: SecretTone::Saved,
            label: format!("Saved {trimmed}"),
            detail: "Stored securely. You can replace it in Settings -> Connections.".to_owned(),
        },
        "declined" => SecretRequestDisplay {
            tone: SecretTone::Declined,
            label: format!("Declined {trimmed}"),
            detail: "Not shared with the agent.".to_owned(),
        },
        "timed_out" => SecretRequestDisplay {
            tone: SecretTone::TimedOut,
            label: format!("Timed out waiting for {trimmed}"),
            detail: "No secret was saved before the request expired.".to_owned(),
        },
        _ if !can_answer_here => SecretRequestDisplay {
            tone: SecretTone::PendingElsewhere,
            label: format!("Waiting for {trimmed}"),
            detail: "Answer this secret request on the host environment.".to_owned(),
        },
        _ => SecretRequestDisplay {
            tone: SecretTone::Pending,
            label: trimmed.to_owned(),
            detail: String::new(),
        },
    }
}

/// Display model for a `link_request` timeline item (`linkRequestDisplay`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRequestDisplay {
    pub tone: LinkTone,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTone {
    Pending,
    PendingElsewhere,
    Linked,
    Declined,
    Failed,
    TimedOut,
}

pub const RPC_REQUEST_FALLBACK_ERROR: &str = "Could not answer the request. Try again.";

/// `secretRequestFailureMessage`: only surface explicit tagged domain errors so transport/encoding
/// errors never echo entered secret text.
pub fn sanitize_secret_request_error(tag: Option<&str>, message: &str) -> String {
    let trimmed = message.trim();
    if !trimmed.is_empty() && matches!(tag, Some("SecretRequestError" | "EnvironmentAuthorizationError")) {
        trimmed.to_owned()
    } else {
        RPC_REQUEST_FALLBACK_ERROR.to_owned()
    }
}

/// `linkRequestFailureMessage`: only surface explicit tagged domain errors so raw errors never
/// leak entered pairing codes.
pub fn sanitize_link_request_error(tag: Option<&str>, message: &str) -> String {
    let trimmed = message.trim();
    if !trimmed.is_empty()
        && matches!(tag, Some("PeerLinkRequestError" | "EnvironmentAuthorizationError" | "PeerPairingCodeError"))
    {
        trimmed.to_owned()
    } else {
        RPC_REQUEST_FALLBACK_ERROR.to_owned()
    }
}

pub const LINK_ACCESS_LEVELS: [&str; 5] =
    ["read-only", "approval-required", "auto-accept-edits", "auto", "full-access"];

pub fn link_request_display(
    link_status: &str,
    label: Option<&str>,
    url: Option<&str>,
    linked_label: Option<&str>,
    linked_access: Option<&str>,
    failure: Option<&str>,
    can_answer_here: bool,
) -> LinkRequestDisplay {
    let target = linked_label.or(label).or(url).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("machine");
    match link_status {
        "linked" => {
            let access_note = linked_access.map(link_access_label).map(|a| format!(" ({a})")).unwrap_or_default();
            LinkRequestDisplay {
                tone: LinkTone::Linked,
                label: format!("Linked {target}{access_note}"),
                detail: "Agents on this machine can now reach it.".to_owned(),
            }
        }
        "declined" => LinkRequestDisplay {
            tone: LinkTone::Declined,
            label: format!("Declined linking {target}"),
            detail: "No link was created.".to_owned(),
        },
        "failed" => LinkRequestDisplay {
            tone: LinkTone::Failed,
            label: format!("Failed to link {target}"),
            detail: failure.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("Linking did not succeed.").to_owned(),
        },
        "timed_out" => LinkRequestDisplay {
            tone: LinkTone::TimedOut,
            label: format!("Timed out linking {target}"),
            detail: "No link was created before the request expired.".to_owned(),
        },
        _ if !can_answer_here => LinkRequestDisplay {
            tone: LinkTone::PendingElsewhere,
            label: format!("Waiting to link {target}"),
            detail: "Answer this link request on a connected machine.".to_owned(),
        },
        _ => LinkRequestDisplay {
            tone: LinkTone::Pending,
            label: format!("Link {target}"),
            detail: String::new(),
        },
    }
}

/// Access level options shown in `LinkAccessPicker` (`accessConfig` in `ConnectAgentSurface.tsx`).
pub fn link_access_label(access: &str) -> &'static str {
    use crate::composer::runtime_mode_label;
    use crate::server_config::RuntimeMode;
    match access {
        "read-only" => "Read only",
        "auto-accept-edits" => runtime_mode_label(RuntimeMode::AutoAcceptEdits),
        "auto" => runtime_mode_label(RuntimeMode::Auto),
        "full-access" => runtime_mode_label(RuntimeMode::FullAccess),
        _ => runtime_mode_label(RuntimeMode::ApprovalRequired),
    }
}

pub fn link_access_description(access: &str) -> &'static str {
    use crate::composer::runtime_mode_description;
    use crate::server_config::RuntimeMode;
    match access {
        "read-only" => "Read projects and threads. Cannot start, message or change anything.",
        "auto-accept-edits" => runtime_mode_description(RuntimeMode::AutoAcceptEdits),
        "auto" => runtime_mode_description(RuntimeMode::Auto),
        "full-access" => runtime_mode_description(RuntimeMode::FullAccess),
        _ => runtime_mode_description(RuntimeMode::ApprovalRequired),
    }
}

/// Scopes encoded into a pairing code for `access` (`mcpClientScopes`).
pub fn mcp_client_scopes(access: &str) -> Vec<String> {
    match access {
        "read-only" => vec!["orchestration:read".to_owned()],
        "full-access" => vec![
            "orchestration:read".to_owned(),
            "orchestration:operate".to_owned(),
            "settings:write".to_owned(),
            "source-control:write".to_owned(),
        ],
        _ => vec!["orchestration:read".to_owned(), "orchestration:operate".to_owned()],
    }
}

pub fn link_request_default_access(requested_access: Option<&str>) -> &'static str {
    match requested_access {
        Some("read-only") => "read-only",
        Some("auto-accept-edits") => "auto-accept-edits",
        Some("auto") => "auto",
        Some("full-access") => "full-access",
        _ => "approval-required",
    }
}

/// Input environment record for `linkable_machines`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineCandidate {
    pub id: String,
    pub label: String,
    pub urls: Vec<String>,
    pub reachable: bool,
}

/// Existing peer link summary from `peerLinks.list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingPeerLink {
    pub environment_id: String,
    pub label: String,
}

/// A saved/reachable machine candidate for a `link_request` (`linkableMachines`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkableMachine {
    pub id: String,
    pub label: String,
    pub urls: Vec<String>,
    pub existing_link_environment_id: Option<String>,
    pub reachable: bool,
}

pub fn linkable_machines(
    environments: &[MachineCandidate],
    linked: &[ExistingPeerLink],
    host_environment_id: Option<&str>,
) -> Vec<LinkableMachine> {
    environments
        .iter()
        .filter(|env| host_environment_id != Some(env.id.as_str()))
        .map(|env| {
            let existing = linked
                .iter()
                .find(|l| l.environment_id == env.id || l.label.eq_ignore_ascii_case(&env.label));
            LinkableMachine {
                id: env.id.clone(),
                label: env.label.clone(),
                urls: env.urls.clone(),
                existing_link_environment_id: existing.map(|l| l.environment_id.clone()),
                reachable: env.reachable,
            }
        })
        .collect()
}

pub fn rank_machines_by_hint(
    mut machines: Vec<LinkableMachine>,
    hint: Option<&str>,
    label: Option<&str>,
    environment_id: Option<&str>,
) -> Vec<LinkableMachine> {
    let needles: Vec<String> = [hint, label, environment_id]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    machines.sort_by_key(|machine| {
        let id_lc = machine.id.to_ascii_lowercase();
        let lbl_lc = machine.label.to_ascii_lowercase();
        let exact = needles.iter().any(|n| &id_lc == n || &lbl_lc == n);
        let partial = needles.iter().any(|n| id_lc.contains(n) || lbl_lc.contains(n));
        let score = if exact { 0u8 } else if partial { 1 } else { 2 };
        (score, !machine.reachable, lbl_lc)
    });
    machines
}

pub fn can_manage_peer_links(session: Option<&AuthSessionState>) -> bool {
    session.is_some_and(|g| g.grants(ACCESS_WRITE_SCOPE))
}

/// One resolved endpoint on a context handoff (`resolveHandoffEndpoints`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffEndpoint {
    pub driver: Option<String>,
    pub label: String,
    pub is_custom: bool,
}

pub fn resolve_handoff_endpoints(
    from_model_selections: &[Value],
    from_provider_instance_ids: &[String],
    to_provider_instance_id: Option<&str>,
    to_model: Option<&str>,
    providers: &[ProviderEntry<'_>],
) -> (Vec<HandoffEndpoint>, HandoffEndpoint) {
    let resolve_one = |instance_id: Option<&str>, model_slug: Option<&str>| -> HandoffEndpoint {
        let entry = instance_id.and_then(|id| providers.iter().find(|p| p.instance_id() == id));
        let driver = entry.map(|p| p.driver().to_owned());
        let is_custom = entry.is_some_and(|p| p.provider.instance_id != p.provider.driver);
        let label = match (entry, model_slug) {
            (Some(p), Some(slug)) => {
                if let Some(m) = p.provider.models.iter().find(|m| m.slug == slug) {
                    crate::providers::trigger_model_name(m).to_owned()
                } else {
                    slug.to_owned()
                }
            }
            (Some(p), None) => p.display_name.clone(),
            (None, Some(slug)) => slug.to_owned(),
            (None, None) => instance_id.unwrap_or("Unknown").to_owned(),
        };
        HandoffEndpoint { driver, label, is_custom }
    };

    let mut from_endpoints = Vec::new();
    if !from_model_selections.is_empty() {
        for sel in from_model_selections {
            let inst = sel.get("instanceId").and_then(Value::as_str);
            let model = sel.get("model").and_then(Value::as_str);
            from_endpoints.push(resolve_one(inst, model));
        }
    } else {
        for inst in from_provider_instance_ids {
            from_endpoints.push(resolve_one(Some(inst.as_str()), None));
        }
    }
    let to_endpoint = resolve_one(to_provider_instance_id, to_model);
    (from_endpoints, to_endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_and_link_displays_cover_all_statuses() {
        assert_eq!(secret_request_display("saved", "OPENAI_API_KEY", true).label, "Saved OPENAI_API_KEY");
        assert_eq!(secret_request_display("pending", "TOKEN", false).tone, SecretTone::PendingElsewhere);
        assert_eq!(
            link_request_display("linked", Some("Mac"), None, Some("Build Box"), Some("full-access"), None, true).label,
            "Linked Build Box (Full access)"
        );
        assert_eq!(
            link_request_display("pending", None, Some("https://box.local"), None, None, None, true).label,
            "Link https://box.local"
        );
        assert_eq!(
            link_request_display("failed", Some("Box"), None, None, None, Some("bad code"), true).detail,
            "bad code"
        );
        assert_eq!(link_access_label("approval-required"), "Supervised");
        assert_eq!(link_access_label("auto-accept-edits"), "Auto-accept edits");
        assert_eq!(link_access_label("auto"), "Auto");
        assert_eq!(link_request_default_access(Some("auto-accept-edits")), "auto-accept-edits");
        assert_eq!(sanitize_secret_request_error(Some("SecretRequestError"), "Bad secret"), "Bad secret");
        assert_eq!(sanitize_secret_request_error(None, "raw error echoing sk-secret"), RPC_REQUEST_FALLBACK_ERROR);
        assert_eq!(sanitize_link_request_error(Some("PeerPairingCodeError"), "Invalid code"), "Invalid code");
        assert_eq!(sanitize_link_request_error(Some("UnknownError"), "raw error echoing code"), RPC_REQUEST_FALLBACK_ERROR);
    }

    #[test]
    fn ranks_machines_matching_hint_first_and_keeps_urlless_machines() {
        let candidates = vec![
            MachineCandidate { id: "host".into(), label: "Host".into(), urls: vec!["http://host".into()], reachable: true },
            MachineCandidate { id: "cloud-relay".into(), label: "Cloud Relay".into(), urls: Vec::new(), reachable: false },
            MachineCandidate { id: "b".into(), label: "GPU Box".into(), urls: vec!["http://b".into()], reachable: true },
        ];
        let machines = linkable_machines(&candidates, &[], Some("host"));
        assert_eq!(machines.len(), 2);
        assert!(machines.iter().any(|m| m.id == "cloud-relay" && m.urls.is_empty()));
        let ranked = rank_machines_by_hint(machines, Some("gpu"), None, None);
        assert_eq!(ranked[0].id, "b");
    }
}
