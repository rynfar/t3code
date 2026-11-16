//! Display and resolution helpers for secret requests, link requests, and context handoffs.
//! Ports `packages/client-runtime/src/{secretRequest,linkRequest,handoff}.ts`.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::auth::{ACCESS_WRITE_SCOPE, AuthSessionState};
use crate::providers::ProviderEntry;
use crate::settings::providers::form::{valid_one_password_reference, valid_reference};
use crate::settings::secrets::{Classified, SecretProviders, classify};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRequestSource {
    pub id: String,
    pub label: String,
    pub one_password: bool,
    pub reference_placeholder: &'static str,
    pub invalid_reference_message: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRequestLink {
    pub href: String,
    pub host: String,
}

/// Empty unless `secret_request_references` is enabled on the server; unsupported providers are omitted.
pub fn secret_request_sources(secret_request_references: bool, providers: &SecretProviders) -> Vec<SecretRequestSource> {
    if !secret_request_references {
        return Vec::new();
    }
    providers
        .0
        .iter()
        .filter_map(|provider| match classify(provider) {
            Classified::OnePassword { .. } => Some(SecretRequestSource {
                id: provider.id.clone(),
                label: provider.label.clone(),
                one_password: true,
                reference_placeholder: "op://vault/item/field",
                invalid_reference_message: "Use a 1Password reference like op://vault/item/field.",
            }),
            Classified::Unsupported => None,
        })
        .collect()
}

/// Validates and normalizes `text` against `source`'s reference syntax.
pub fn secret_request_reference<'a>(source: &'a SecretRequestSource, text: &str) -> Option<(&'a str, String)> {
    let trimmed = text.trim();
    if !valid_reference(trimmed) {
        return None;
    }
    if source.one_password && !valid_one_password_reference(trimmed) {
        return None;
    }
    Some((source.id.as_str(), trimmed.to_owned()))
}

/// `secretRequestPageUrl`: allows only `http` / `https` URLs with a non-empty host.
pub fn secret_request_page_url(value: &str) -> Option<(String, String)> {
    let parsed = url::Url::parse(value.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str().filter(|h| !h.is_empty())?;
    let authority_host = match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    Some((parsed.to_string(), authority_host))
}

fn trim_punctuation(link: &str) -> &str {
    let mut end = link.len();
    while end > 0 {
        let ch = link[..end].chars().next_back().unwrap();
        if !matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')') {
            break;
        }
        let kept = &link[..end];
        if ch == ')' && kept.bytes().filter(|&b| b == b'(').count() > kept.bytes().filter(|&b| b == b')').count().saturating_sub(1) {
            break;
        }
        end -= ch.len_utf8();
    }
    &link[..end]
}

fn trailing_emphasis_run(link: &str) -> Option<&str> {
    let last = link.bytes().next_back()?;
    if last != b'*' && last != b'_' {
        return None;
    }
    let count = link.bytes().rev().take_while(|&b| b == last).take(3).count();
    Some(&link[link.len() - count..])
}

fn trim_emphasis<'a>(link: &'a str, before: &str) -> &'a str {
    let Some(run) = trailing_emphasis_run(link) else {
        return link;
    };
    let mut opener_end = None;
    for (idx, _) in before.match_indices(run) {
        let prev_ok = idx == 0 || before[..idx].chars().next_back().is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '*' | '_'));
        let after_idx = idx + run.len();
        let next_ok = after_idx == before.len() || before[after_idx..].chars().next().is_some_and(|c| !c.is_whitespace());
        if prev_ok && next_ok {
            opener_end = Some(after_idx);
        }
    }
    let Some(after_opener) = opener_end.map(|end| &before[end..]) else {
        return link;
    };
    let closed = after_opener.match_indices(run).any(|(idx, _)| {
        let prev_ok = idx > 0 && after_opener[..idx].chars().next_back().is_some_and(|c| !c.is_whitespace());
        let after_idx = idx + run.len();
        let next_ok = after_idx == after_opener.len()
            || after_opener[after_idx..].chars().next().is_some_and(|c| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')'));
        prev_ok && next_ok
    });
    if closed { link } else { trim_punctuation(&link[..link.len() - run.len()]) }
}

fn reason_link(reason: &str) -> Option<String> {
    static REASON_LINK: OnceLock<Regex> = OnceLock::new();
    let re = REASON_LINK.get_or_init(|| {
        Regex::new(r#"(?i)`(https?://[^`\s]+)`|<(https?://[^>\s]+)>|\]\((https?://(?:[^()\s]|\([^()\s]*\))+)\)|(https?://[^\s<>"'`\[\]]+)"#)
            .expect("valid REASON_LINK regex")
    });
    let mut links = HashSet::new();
    for caps in re.captures_iter(reason) {
        let candidate = if let Some(bare) = caps.get(4) {
            let before = &reason[..caps.get(0).map_or(0, |m| m.start())];
            let mut link = trim_punctuation(bare.as_str());
            loop {
                let next = trim_emphasis(link, before);
                if next == link {
                    break;
                }
                link = next;
            }
            link
        } else {
            caps.get(1).or_else(|| caps.get(2)).or_else(|| caps.get(3)).map_or("", |m| m.as_str())
        };
        if candidate.is_empty() {
            continue;
        }
        let normalized = secret_request_page_url(candidate).map(|(href, _)| href).unwrap_or_else(|| candidate.to_owned());
        links.insert(normalized);
        if links.len() > 1 {
            return None;
        }
    }
    links.into_iter().next()
}

/// Only http(s), preferring explicit `url` over a single unambiguous link in `reason`.
pub fn secret_request_link(url: Option<&str>, reason: &str) -> Option<SecretRequestLink> {
    let raw = url.map(str::to_owned).or_else(|| reason_link(reason))?;
    let (href, host) = secret_request_page_url(&raw)?;
    Some(SecretRequestLink { href, host })
}

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
            detail: "Saved securely and kept private.".to_owned(),
        },
        "declined" => SecretRequestDisplay {
            tone: SecretTone::Declined,
            label: format!("Declined {trimmed}"),
            detail: "Not shared with the agent.".to_owned(),
        },
        "cancelled" => SecretRequestDisplay {
            tone: SecretTone::TimedOut,
            label: "Request ended".to_owned(),
            detail: String::new(),
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
    let target = linked_label
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| url.map(str::trim).filter(|s| !s.is_empty()).map(link_destination))
        .or_else(|| label.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
        .unwrap_or_else(|| "machine".to_owned());
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
        "cancelled" => LinkRequestDisplay {
            tone: LinkTone::TimedOut,
            label: "Request ended".to_owned(),
            detail: String::new(),
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

pub const ANOTHER_ADDRESS: &str = "another-address";

/// The http or https origin of an address (`originOf`).
pub fn origin_of(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw.trim()).ok()?;
    if matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some_and(|h| !h.is_empty()) {
        Some(parsed.origin().ascii_serialization())
    } else {
        None
    }
}

/// Where a link to `address` sends its pairing code (`linkDestination`).
pub fn link_destination(address: &str) -> String {
    origin_of(address).unwrap_or_else(|| address.trim().to_owned())
}

/// What the card calls the other environment: where `url` leads, never a supplied label.
pub fn link_request_target_name(url: Option<&str>) -> String {
    url.map(link_destination).filter(|s| !s.is_empty()).unwrap_or_else(|| "a machine".to_owned())
}

/// A loopback address names whichever machine opens it, never a peer reachable over the network.
pub fn is_loopback_url(raw: &str) -> bool {
    let Ok(parsed) = url::Url::parse(raw.trim()) else {
        return false;
    };
    match parsed.host() {
        Some(url::Host::Domain(host)) => {
            let normalized = host.trim_end_matches('.').to_ascii_lowercase();
            normalized == "localhost" || normalized.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
        None => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRequestTarget {
    None,
    Machine {
        environment_id: String,
        label: String,
        urls: Vec<String>,
        linked: bool,
    },
    Address,
}

pub fn link_request_target(
    environment_id: Option<&str>,
    url: Option<&str>,
    machines: &[LinkableMachine],
    choice: Option<&str>,
) -> LinkRequestTarget {
    let chosen = if let Some(asked_url) = url.map(str::trim).filter(|u| !u.is_empty()) {
        let asked_origin = origin_of(asked_url);
        let mut matched = asked_origin.as_ref().map_or_else(Vec::new, |asked| {
            machines
                .iter()
                .filter(|machine| machine.urls.iter().any(|u| origin_of(u).as_ref() == Some(asked)))
                .collect::<Vec<_>>()
        });
        if matched.len() == 1 {
            matched.pop().map(|m| m.id.as_str()).unwrap_or(ANOTHER_ADDRESS)
        } else {
            ANOTHER_ADDRESS
        }
    } else if let Some(id) = environment_id.or(choice) {
        id
    } else {
        return LinkRequestTarget::None;
    };
    if chosen == ANOTHER_ADDRESS {
        return LinkRequestTarget::Address;
    }
    match machines.iter().find(|candidate| candidate.id == chosen) {
        Some(machine) => LinkRequestTarget::Machine {
            environment_id: machine.id.clone(),
            label: machine.label.clone(),
            urls: machine.urls.clone(),
            linked: machine.existing_link_environment_id.is_some(),
        },
        None => LinkRequestTarget::Machine {
            environment_id: chosen.to_owned(),
            label: link_request_target_name(url),
            urls: Vec::new(),
            linked: false,
        },
    }
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
            let mut urls = Vec::new();
            for url in &env.urls {
                let trimmed = url.trim();
                if !trimmed.is_empty() && !is_loopback_url(trimmed) && !urls.iter().any(|u| u == trimmed) {
                    urls.push(trimmed.to_owned());
                }
            }
            LinkableMachine {
                id: env.id.clone(),
                label: env.label.clone(),
                urls,
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

fn latest_run_model_before<'a>(
    runs: &'a [crate::thread::Run],
    instance_id: &str,
    before_ordinal: Option<u64>,
) -> Option<&'a str> {
    let mut latest: Option<&crate::thread::Run> = None;
    for run in runs {
        if run.provider_instance_id != instance_id {
            continue;
        }
        if before_ordinal.is_some_and(|ord| run.ordinal >= ord) {
            continue;
        }
        if latest.is_none_or(|cur| run.ordinal > cur.ordinal) {
            latest = Some(run);
        }
    }
    latest.map(|run| run.model_selection.model.as_str())
}

pub fn resolve_handoff_endpoints(
    item_run_id: Option<&str>,
    from_model_selections: &[Value],
    from_provider_instance_ids: &[String],
    to_provider_instance_id: Option<&str>,
    to_model: Option<&str>,
    runs: &[crate::thread::Run],
    providers: &[ProviderEntry<'_>],
) -> (Vec<HandoffEndpoint>, HandoffEndpoint) {
    let handoff_run = item_run_id.and_then(|rid| runs.iter().find(|run| run.id == rid));
    let resolved_to_model = to_model.or_else(|| {
        handoff_run
            .filter(|run| Some(run.provider_instance_id.as_str()) == to_provider_instance_id)
            .map(|run| run.model_selection.model.as_str())
    });
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
        let before_ordinal = handoff_run.map(|run| run.ordinal);
        for inst in from_provider_instance_ids {
            let inferred = latest_run_model_before(runs, inst.as_str(), before_ordinal);
            from_endpoints.push(resolve_one(Some(inst.as_str()), inferred));
        }
    }
    let to_endpoint = resolve_one(to_provider_instance_id, resolved_to_model);
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
        assert_eq!(secret_request_display("cancelled", "OPENAI_API_KEY", true).tone, SecretTone::TimedOut);
        assert_eq!(secret_request_display("cancelled", "OPENAI_API_KEY", true).label, "Request ended");
        assert_eq!(link_request_display("cancelled", Some("Mac"), None, None, None, None, true).tone, LinkTone::TimedOut);
        assert_eq!(link_request_display("cancelled", Some("Mac"), None, None, None, None, true).label, "Request ended");
        assert_eq!(sanitize_link_request_error(Some("UnknownError"), "raw error echoing code"), RPC_REQUEST_FALLBACK_ERROR);
    }

    #[test]
    fn handoff_endpoints_infer_missing_models_from_runs() {
        let runs: Vec<crate::thread::Run> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1", "ordinal": 1, "providerInstanceId": "codex-1",
                "modelSelection": { "instanceId": "codex-1", "model": "gpt-5" },
                "userMessageId": "m1", "rootNodeId": null, "status": "completed",
                "requestedAt": "2026-01-01T00:00:00Z", "startedAt": null, "completedAt": null
            },
            {
                "id": "run-2", "ordinal": 2, "providerInstanceId": "claude-1",
                "modelSelection": { "instanceId": "claude-1", "model": "claude-sonnet-4-5" },
                "userMessageId": "m2", "rootNodeId": null, "status": "completed",
                "requestedAt": "2026-01-01T00:01:00Z", "startedAt": null, "completedAt": null
            },
            {
                "id": "run-3", "ordinal": 3, "providerInstanceId": "codex-1",
                "modelSelection": { "instanceId": "codex-1", "model": "gpt-5-future" },
                "userMessageId": "m3", "rootNodeId": null, "status": "completed",
                "requestedAt": "2026-01-01T00:02:00Z", "startedAt": null, "completedAt": null
            }
        ]))
        .expect("runs");
        let (from, to) = resolve_handoff_endpoints(
            Some("run-2"),
            &[],
            &["codex-1".to_owned()],
            Some("claude-1"),
            None,
            &runs,
            &[],
        );
        assert_eq!(from.len(), 1);
        assert_eq!(from[0].label, "gpt-5");
        assert_eq!(to.label, "claude-sonnet-4-5");
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

    #[test]
    fn secret_request_saved_display_uses_sharing_and_access_wording() {
        let display = secret_request_display("saved", "OPENAI_API_KEY", true);
        assert!(!display.detail.contains("Connections"), "detail should not reference Settings -> Connections: {}", display.detail);
    }

    #[test]
    fn linkable_machines_filters_loopback_urls_and_display_ignores_untrusted_label_for_url() {
        let candidates = vec![MachineCandidate {
            id: "peer-1".into(),
            label: "Studio".into(),
            urls: vec!["http://127.0.0.1:3773".into(), "http://localhost:3773".into(), "https://studio.ts.net".into()],
            reachable: true,
        }];
        let machines = linkable_machines(&candidates, &[], Some("host"));
        assert_eq!(machines[0].urls, vec!["https://studio.ts.net".to_owned()]);

        let pending_url = link_request_display("pending", Some("Spoofed Name"), Some("https://user:pass@box.local:8443/path"), None, None, None, true);
        assert_eq!(pending_url.label, "Link https://box.local:8443");
    }

    #[test]
    fn secret_request_sources_and_reference_validation() {
        let providers: SecretProviders = serde_json::from_value(serde_json::json!({
            "op-work": { "kind": "one_password", "label": "Work 1Password", "config": { "account": "work.1password.com" } },
            "legacy": { "kind": "custom_vault", "label": "Unsupported", "config": {} },
        }))
        .expect("providers");
        assert!(secret_request_sources(false, &providers).is_empty());
        let sources = secret_request_sources(true, &providers);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, "op-work");
        assert_eq!(sources[0].label, "Work 1Password");
        assert_eq!(
            secret_request_reference(&sources[0], "  op://Private/API Key/credential  "),
            Some(("op-work", "op://Private/API Key/credential".to_owned()))
        );
        assert_eq!(secret_request_reference(&sources[0], "not-op-ref"), None);
    }

    #[test]
    fn secret_request_link_prefers_safe_url_and_parses_single_reason_link() {
        assert_eq!(
            secret_request_link(Some("https://accounts.google.com/o/oauth2/auth?x=1"), "fallback https://other.dev"),
            Some(SecretRequestLink {
                href: "https://accounts.google.com/o/oauth2/auth?x=1".to_owned(),
                host: "accounts.google.com".to_owned(),
            })
        );
        assert_eq!(secret_request_link(Some("javascript:alert(1)"), "visit https://safe.example/keys"), None);
        assert_eq!(
            secret_request_link(None, "Create a token at **https://example.com/tokens_(v2)**."),
            Some(SecretRequestLink {
                href: "https://example.com/tokens_(v2)".to_owned(),
                host: "example.com".to_owned(),
            })
        );
        assert_eq!(
            secret_request_link(None, "See [docs](https://a.dev) or `https://a.dev/`."),
            Some(SecretRequestLink { href: "https://a.dev/".to_owned(), host: "a.dev".to_owned() })
        );
        assert_eq!(secret_request_link(None, "Compare https://a.dev and https://b.dev"), None);
    }

    #[test]
    fn link_request_target_matches_unique_origin_and_never_trusts_spoofed_env() {
        let machines = vec![
            LinkableMachine {
                id: "env-a".into(),
                label: "Box A".into(),
                urls: vec!["https://box-a.ts.net".into()],
                existing_link_environment_id: None,
                reachable: true,
            },
            LinkableMachine {
                id: "env-b".into(),
                label: "Box B".into(),
                urls: vec!["http://10.0.0.5:3773".into()],
                existing_link_environment_id: None,
                reachable: true,
            },
            LinkableMachine {
                id: "env-c".into(),
                label: "Box C".into(),
                urls: vec!["http://10.0.0.5:3773".into()],
                existing_link_environment_id: None,
                reachable: true,
            },
        ];
        assert_eq!(
            link_request_target(Some("env-b"), Some("https://box-a.ts.net/pair?x=1"), &machines, None),
            LinkRequestTarget::Machine {
                environment_id: "env-a".into(),
                label: "Box A".into(),
                urls: vec!["https://box-a.ts.net".into()],
                linked: false,
            }
        );
        // Ambiguous shared private IP -> requires manual address confirmation.
        assert_eq!(link_request_target(None, Some("http://10.0.0.5:3773/pair"), &machines, None), LinkRequestTarget::Address);
        // Unknown URL ignores spoofed environment_id.
        assert_eq!(link_request_target(Some("env-a"), Some("https://evil.example"), &machines, None), LinkRequestTarget::Address);
        assert_eq!(link_request_target(None, None, &machines, None), LinkRequestTarget::None);
    }

    #[test]
    fn secret_request_wire_contracts_round_trip_url_flag_and_reference_command() {
        let item: crate::turn_item::TurnItemKind = serde_json::from_value(serde_json::json!({
            "type": "secret_request",
            "label": "OPENAI_API_KEY",
            "reason": "Need key",
            "secretStatus": "pending",
            "url": "https://platform.openai.com/api-keys"
        }))
        .expect("secret request turn item");
        match item {
            crate::turn_item::TurnItemKind::SecretRequest { url, .. } => {
                assert_eq!(url.as_deref(), Some("https://platform.openai.com/api-keys"));
            }
            other => panic!("unexpected kind: {other:?}"),
        }

        let cfg: crate::server_config::ServerConfig = serde_json::from_value(serde_json::json!({
            "secretRequestReferences": true
        }))
        .expect("server config");
        assert!(cfg.secret_request_references);

        assert_eq!(
            crate::commands::secret_request_answer_reference("t1", "i1", "op-work", "op://vault/item/field"),
            serde_json::json!({
                "threadId": "t1",
                "turnItemId": "i1",
                "answer": {
                    "type": "reference",
                    "reference": { "provider": "op-work", "reference": "op://vault/item/field" }
                }
            })
        );
    }
}
