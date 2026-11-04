//! Settings → Providers, the pure parts. The instance list and its rows follow
//! `web/src/components/settings/ProviderSettingsPanel.tsx`; status, version and update copy follow
//! `providerStatus.ts` and `ProviderInstanceCard.tsx`; the models list follows
//! `ProviderModelsSection.tsx` and `web/src/modelOrdering.ts`; the add dialog's ids follow
//! `AddProviderInstanceDialog.tsx` and `AddProviderInstanceDialog.logic.ts`. The driver catalog
//! mirrors `client-runtime/src/providerClients.ts`. Instance edits save as a
//! `providerInstanceMutation` on `server.updateSettings` (`usePersistEnvironmentProviderInstanceMutation`).

pub mod auth;
pub mod background;
pub mod chatgpt;
pub mod form;
pub mod wizard;

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use crate::providers;
use crate::server_config::{
    CompatibilityAdvisory, ModelCapabilities, ProviderInstanceConfig, ProviderModel, ServerProvider, ServerSettings, VersionAdvisory,
};

pub const SERVER_REFRESH_PROVIDERS: &str = "server.refreshProviders";
pub const SERVER_UPDATE_PROVIDER: &str = "server.updateProvider";
pub const SERVER_UNINSTALL_ACP_REGISTRY_BINARY: &str = "server.uninstallAcpRegistryManagedBinary";

/// One entry of `providerClients`: what the settings know about a driver without a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Driver {
    pub kind: &'static str,
    pub label: &'static str,
    pub badge_label: Option<&'static str>,
    /// `false` only for the ACP Registry, whose instances are always user-made.
    pub has_default_instance: bool,
    /// `false` lists the driver under Add provider's "Show more".
    pub featured: bool,
    /// `CUSTOM_MODEL_PLACEHOLDER_BY_KIND`.
    pub custom_model_placeholder: &'static str,
}

const fn driver(kind: &'static str, label: &'static str, featured: bool, placeholder: &'static str) -> Driver {
    Driver { kind, label, badge_label: None, has_default_instance: true, featured, custom_model_placeholder: placeholder }
}

pub const ACP_REGISTRY: &str = "acpRegistry";

/// `providerClients.definitions`, in the web's order.
pub const DRIVERS: [Driver; 11] = [
    driver("codex", "Codex", true, "gpt-6.7-codex-ultra-preview"),
    driver("claudeAgent", "Claude", true, "claude-sonnet-5"),
    driver("cursor", "Cursor", true, "claude-sonnet-4-6"),
    driver("grok", "Grok", true, "model-slug"),
    driver("fx", "fx", false, "model-slug"),
    driver("opencode", "OpenCode", true, "openai/gpt-5"),
    driver("antigravity", "Antigravity", true, "model-slug"),
    Driver { badge_label: Some("Early Access"), ..driver("kiro", "Kiro", false, "model-slug") },
    driver("muse", "Muse Code", true, "model-slug"),
    driver("pi", "Pi", true, "anthropic/claude-sonnet-5"),
    Driver { kind: ACP_REGISTRY, label: "ACP Registry", badge_label: None, has_default_instance: false, featured: true, custom_model_placeholder: "model-slug" },
];

/// Drivers in the Add provider wizard, in catalog order.
pub fn offered_drivers(featured: bool, policy: Option<&Value>) -> impl Iterator<Item = &'static Driver> {
    DRIVERS.iter().filter(move |driver| driver.kind != ACP_REGISTRY && driver.featured == featured && crate::managed_policy::driver_allowed(policy, driver.kind))
}

pub fn find_driver(kind: &str) -> Option<&'static Driver> {
    DRIVERS.iter().find(|driver| driver.kind == kind)
}

/// One instance in the list: a configured instance, or a driver's default slot.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceRow {
    pub instance_id: String,
    pub driver: String,
    /// `None` for a default slot the user never configured.
    pub explicit: Option<ProviderInstanceConfig>,
    pub is_default: bool,
    /// A default slot with saved settings, which the editor offers to reset.
    pub is_dirty: bool,
}

impl InstanceRow {
    /// The instance's settings, or an empty config for an unconfigured default slot.
    pub fn instance(&self) -> ProviderInstanceConfig {
        self.explicit.clone().unwrap_or_else(|| ProviderInstanceConfig { driver: self.driver.clone(), ..Default::default() })
    }

    pub fn enabled(&self) -> bool {
        providers::resolve_instance_enabled(&self.instance())
    }
}

/// The list's rows: each visible driver's default slot (when it runs unconfigured, is enabled,
/// was customized, or is the requested instance), then that driver's custom instances; instances
/// of drivers this build does not know come last. Cursor shows only where the server has it.
pub fn instance_rows(settings: &ServerSettings, live: &[ServerProvider], target: Option<&str>) -> Vec<InstanceRow> {
    let visible: Vec<&Driver> =
        DRIVERS.iter().filter(|driver| driver.kind != "cursor" || live.iter().any(|provider| provider.instance_id == "cursor")).collect();
    let instances = &settings.provider_instances;
    let mut rows = Vec::new();
    for driver in &visible {
        let explicit = instances.get(driver.kind).cloned();
        let is_dirty = explicit.is_some();
        let fallback = ProviderInstanceConfig { driver: driver.kind.to_owned(), ..Default::default() };
        let enabled = providers::resolve_instance_enabled(explicit.as_ref().unwrap_or(&fallback));
        let shows_default = (driver.has_default_instance || is_dirty)
            && (matches!(driver.kind, "codex" | "claudeAgent") || is_dirty || enabled || target == Some(driver.kind));
        if shows_default {
            rows.push(InstanceRow { instance_id: driver.kind.to_owned(), driver: driver.kind.to_owned(), explicit, is_default: true, is_dirty });
        }
        for (id, instance) in instances.iter().filter(|(id, instance)| instance.driver == driver.kind && *id != driver.kind) {
            rows.push(InstanceRow {
                instance_id: id.to_owned(),
                driver: instance.driver.clone(),
                explicit: Some(instance.clone()),
                is_default: false,
                is_dirty: false,
            });
        }
    }
    let mut unknown: Vec<&str> = Vec::new();
    for (_, instance) in instances.iter() {
        if !visible.iter().any(|driver| driver.kind == instance.driver) && !unknown.contains(&instance.driver.as_str()) {
            unknown.push(&instance.driver);
        }
    }
    for driver in unknown {
        for (id, instance) in instances.iter().filter(|(_, instance)| instance.driver == driver) {
            rows.push(InstanceRow {
                instance_id: id.to_owned(),
                driver: instance.driver.clone(),
                explicit: Some(instance.clone()),
                is_default: visible.iter().any(|driver| driver.kind == id),
                is_dirty: false,
            });
        }
    }
    rows
}

/// `PROVIDER_STATUS_STYLES` keys: which dot a status shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKey {
    Disabled,
    Error,
    Neutral,
    Ready,
    Warning,
}

/// Antigravity's local health check verifies installation, not Google sign-in.
fn antigravity_sign_in_unchecked(provider: &ServerProvider) -> bool {
    provider.driver == "antigravity"
        && provider.installed
        && provider.status == "warning"
        && provider.auth.status == "unknown"
        && provider.auth.kind.as_deref().is_none_or(|kind| matches!(kind, "oauth-personal" | "oauth-business"))
}

/// `getProviderStatusKey`, with the instance's own enablement standing in for the server's.
pub fn status_key(provider: Option<&ServerProvider>, enabled: bool) -> StatusKey {
    let Some(provider) = provider else { return StatusKey::Warning };
    if !enabled {
        return StatusKey::Disabled;
    }
    if antigravity_sign_in_unchecked(provider) {
        return StatusKey::Neutral;
    }
    match provider.status.as_str() {
        "ready" => StatusKey::Ready,
        "error" => StatusKey::Error,
        "disabled" => StatusKey::Disabled,
        _ => StatusKey::Warning,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub headline: String,
    pub detail: Option<String>,
}

fn summary_of(headline: impl Into<String>, detail: Option<&str>) -> Summary {
    Summary { headline: headline.into(), detail: detail.map(str::to_owned) }
}

/// `getProviderSummary`.
pub fn summary(provider: Option<&ServerProvider>, enabled: bool) -> Summary {
    let Some(provider) = provider else {
        return summary_of("Checking provider status", Some("Waiting for the server to report installation and authentication details."));
    };
    let message = provider.message.as_deref();
    let auth_label = provider.auth.label.as_deref().or(provider.auth.kind.as_deref());
    if !enabled || provider.status == "disabled" {
        return summary_of("Disabled", Some(message.unwrap_or("This provider is installed but disabled for new sessions in T3 Code.")));
    }
    if !provider.installed {
        return summary_of("Not found", Some(message.unwrap_or("CLI not detected on PATH.")));
    }
    if provider.auth.status == "unauthenticated" {
        let headline = auth_label.map_or_else(|| "Not authenticated".to_owned(), |label| format!("Not authenticated \u{00b7} {label}"));
        return summary_of(headline, message);
    }
    if antigravity_sign_in_unchecked(provider) {
        return summary_of("Installed \u{00b7} Sign-in not checked", message);
    }
    if provider.status == "warning" {
        return summary_of("Needs attention", Some(message.unwrap_or("The provider is installed, but the server could not fully verify it.")));
    }
    if provider.status == "error" {
        return summary_of("Unavailable", Some(message.unwrap_or("The provider failed its startup checks.")));
    }
    if provider.auth.status == "authenticated" {
        let headline = auth_label.map_or_else(|| "Authenticated".to_owned(), |label| format!("Authenticated \u{00b7} {label}"));
        return summary_of(headline, message);
    }
    summary_of("Available", message)
}

/// `getProviderVersionLabel`: `v1.2.3`, or Antigravity's dated build as `2026-01-31 rc`.
pub fn version_label(version: Option<&str>) -> Option<String> {
    let version = version.filter(|version| !version.is_empty())?;
    if let Some(rest) = version.strip_prefix("agy_acp_server_") {
        let mut parts = rest.split('_');
        let date = parts.next().filter(|date| date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()));
        let build = parts.next().filter(|build| !build.is_empty() && build.bytes().all(|byte| byte.is_ascii_digit()));
        let candidate = parts.next();
        let candidate_ok = candidate.is_none_or(|word| !word.is_empty() && word.chars().all(|ch| ch.is_alphanumeric() || ch == '_'));
        if let (Some(date), Some(_), true, None) = (date, build, candidate_ok, parts.next()) {
            let formatted = format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..]);
            return Some(candidate.map_or(formatted.clone(), |word| format!("{formatted} {word}")));
        }
    }
    Some(if version.starts_with(|ch: char| ch.is_ascii_digit()) { format!("v{version}") } else { version.to_owned() })
}

/// `getProviderVersionAdvisoryPresentation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisoryView {
    pub title: String,
    pub detail: String,
    pub update_command: Option<String>,
    /// Shown in the warning color.
    pub strong: bool,
    /// The version an "Install …" button puts in place of "Update now".
    pub target_version: Option<String>,
}

pub fn advisory(advisory: Option<&VersionAdvisory>, compatibility: Option<&CompatibilityAdvisory>, show_compatibility: bool) -> Option<AdvisoryView> {
    let latest_incompatible =
        compatibility.and_then(|compat| compat.latest_version_status.as_deref()).is_some_and(|status| matches!(status, "broken" | "unsupported"));
    if let Some(compat) = compatibility.filter(|compat| show_compatibility && matches!(compat.status.as_str(), "graceful" | "unsupported" | "broken")) {
        let target = compat.recommended_version.clone();
        let recommendation = version_label(target.as_deref()).or_else(|| compat.recommended_range.clone());
        let already_latest = advisory.is_some_and(|advisory| advisory.status == "current");
        let title = match compat.status.as_str() {
            "graceful" => "Limited support",
            "unsupported" => "Unsupported version",
            _ => "Known broken version",
        };
        let detail = compat.message.clone().unwrap_or_else(|| match recommendation {
            Some(recommendation) => format!("Use {recommendation} for full support."),
            None => "Update for full support.".to_owned(),
        });
        let update_command = if target.is_some() || latest_incompatible || already_latest {
            None
        } else {
            advisory.and_then(|advisory| advisory.update_command.clone())
        };
        return Some(AdvisoryView { title: title.to_owned(), detail, update_command, strong: compat.status != "graceful", target_version: target });
    }
    let advisory = advisory.filter(|advisory| !matches!(advisory.status.as_str(), "current" | "unknown") && !latest_incompatible)?;
    let detail = advisory.message.clone().unwrap_or_else(|| match version_label(advisory.latest_version.as_deref()) {
        Some(label) => format!("Update available: install {label}."),
        None => "Update available: install the latest provider version.".to_owned(),
    });
    Some(AdvisoryView {
        title: "Update available".to_owned(),
        detail,
        update_command: advisory.update_command.clone(),
        strong: false,
        target_version: None,
    })
}

/// `isProviderSettingsUpdateCandidate`: a one-click update the server can run.
pub fn can_run_update(provider: &ServerProvider) -> bool {
    let latest = provider.compatibility_advisory.as_ref().and_then(|compat| compat.latest_version_status.as_deref());
    provider.enabled
        && !matches!(latest, Some("broken" | "unsupported"))
        && provider.version_advisory.as_ref().is_some_and(|advisory| {
            advisory.status == "behind_latest" && advisory.can_update && advisory.update_command.is_some()
        })
}

/// `isProviderUpdateActive`.
pub fn update_active(provider: &ServerProvider) -> bool {
    provider.update_state.as_ref().is_some_and(|state| matches!(state.status.as_str(), "queued" | "running"))
}

/// Everything a list row and the editor header show about one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceView {
    pub display_name: String,
    pub enabled: bool,
    pub status: StatusKey,
    pub headline: String,
    /// The " · …" after the headline: the summary detail, or a short compatibility verdict.
    pub inline_detail: Option<String>,
    /// Warnings and errors earn a dot and their detail in the list.
    pub needs_attention: bool,
    /// The full compatibility detail, as the status tooltip.
    pub diagnostic: Option<String>,
    pub authenticated_email: Option<String>,
    pub auth_label: Option<String>,
    pub version: Option<String>,
    pub advisory: Option<AdvisoryView>,
    pub compatibility_warning: bool,
    /// "Starting update", or the server's progress, while an update runs.
    pub update_progress: Option<String>,
    /// Why the last update failed or changed nothing.
    pub update_problem: Option<String>,
    /// The advisory offers "Install <version>" (true) or "Update now" (false), when it acts at all.
    pub version_action: Option<VersionAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionAction {
    UpdateNow,
    Install(String),
}

impl VersionAction {
    pub fn label(&self) -> String {
        match self {
            VersionAction::UpdateNow => "Update now".to_owned(),
            VersionAction::Install(version) => format!("Install {}", version_label(Some(version)).unwrap_or_else(|| version.clone())),
        }
    }

    /// The `server.updateProvider` target, when the action installs a specific version.
    pub fn target_version(&self) -> Option<&str> {
        match self {
            VersionAction::UpdateNow => None,
            VersionAction::Install(version) => Some(version),
        }
    }
}

/// `ProviderInstanceCard`'s derived state. `dispatching` is true while this client's own
/// `server.updateProvider` call is in flight.
pub fn instance_view(row: &InstanceRow, live: Option<&ServerProvider>, dispatching: bool, read_only: bool) -> InstanceView {
    let instance = row.instance();
    let enabled = row.enabled();
    let compatibility = live.and_then(|provider| provider.compatibility_advisory.as_ref()).filter(|_| enabled);
    let status = status_key(live, enabled);
    let summary = if enabled { summary(live, true) } else { summary_of("Disabled", None) };
    let authenticated = enabled && live.is_some_and(|provider| provider.auth.status == "authenticated");
    let auth_label = live.filter(|_| authenticated).and_then(|provider| provider.auth.label.clone().or_else(|| provider.auth.kind.clone()));
    let advisory = advisory(live.and_then(|provider| provider.version_advisory.as_ref()), live.and_then(|provider| provider.compatibility_advisory.as_ref()), enabled);
    let updating = dispatching || live.is_some_and(update_active);
    let update_state = live.and_then(|provider| provider.update_state.as_ref());
    let update_progress = updating.then(|| {
        update_state
            .filter(|state| matches!(state.status.as_str(), "queued" | "running"))
            .and_then(|state| state.message.clone())
            .unwrap_or_else(|| "Starting update".to_owned())
    });
    let update_problem = (!updating)
        .then(|| update_state.filter(|state| matches!(state.status.as_str(), "failed" | "unchanged")).and_then(|state| state.message.clone()))
        .flatten();
    let compatibility_warning = compatibility.is_some_and(|compat| !matches!(compat.status.as_str(), "supported" | "unknown"));
    let needs_attention = matches!(status, StatusKey::Warning | StatusKey::Error);
    let inline_detail = if compatibility_warning {
        Some(match compatibility.map(|compat| compat.status.as_str()) {
            Some("broken") => "Incompatible",
            Some("unsupported") => "Unsupported",
            _ => "Limited support",
        }
        .to_owned())
    } else {
        summary.detail.clone()
    };
    let version_action = match (&advisory, live) {
        (Some(advisory), Some(provider)) if !read_only => match &advisory.target_version {
            Some(target) => {
                let installable = compatibility.is_some_and(|compat| compat.message.is_some())
                    && provider.version_advisory.as_ref().and_then(|advisory| advisory.can_install_version) == Some(true);
                installable.then(|| VersionAction::Install(target.clone()))
            }
            None => can_run_update(provider).then_some(VersionAction::UpdateNow),
        },
        _ => None,
    };
    InstanceView {
        display_name: instance_display_name(&instance),
        enabled,
        status,
        headline: summary.headline,
        inline_detail,
        needs_attention,
        diagnostic: summary.detail.filter(|_| compatibility_warning && needs_attention),
        authenticated_email: live
            .filter(|_| authenticated)
            .and_then(|provider| provider.auth.email.as_deref().map(str::trim).filter(|email| !email.is_empty()).map(str::to_owned)),
        auth_label,
        version: version_label(live.and_then(|provider| provider.version.as_deref())),
        advisory,
        compatibility_warning,
        update_progress,
        update_problem,
        version_action,
    }
}

/// The card's title: the configured name, else the driver's label, else the driver id.
pub fn instance_display_name(instance: &ProviderInstanceConfig) -> String {
    instance
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| find_driver(&instance.driver).map(|driver| driver.label.to_owned()))
        .unwrap_or_else(|| instance.driver.clone())
}

/// `normalizeProviderAccentColor`: a `#rrggbb`, else nothing.
pub fn normalize_accent(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let hex = trimmed.strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| trimmed.to_owned())
}

/// The accent as `0xrrggbbff`, for the swatch.
pub fn accent_rgba(value: &str) -> Option<u32> {
    let hex = normalize_accent(value)?;
    u32::from_str_radix(&hex[1..], 16).ok().map(|rgb| (rgb << 8) | 0xff)
}

pub fn with_enabled(instance: &ProviderInstanceConfig, enabled: bool) -> ProviderInstanceConfig {
    ProviderInstanceConfig { enabled: Some(enabled), ..instance.clone() }
}

/// `updateDisplayName`: a blank name removes the key.
pub fn with_display_name(instance: &ProviderInstanceConfig, name: &str) -> ProviderInstanceConfig {
    let trimmed = name.trim();
    ProviderInstanceConfig { display_name: (!trimmed.is_empty()).then(|| trimmed.to_owned()), ..instance.clone() }
}

/// `updateAccentColor`: anything but a `#rrggbb` removes the key.
pub fn with_accent(instance: &ProviderInstanceConfig, accent: &str) -> ProviderInstanceConfig {
    ProviderInstanceConfig { accent_color: normalize_accent(accent), ..instance.clone() }
}

fn mutation_payload(mutation: Value, patch: Map<String, Value>) -> Value {
    json!({ "patch": patch, "providerInstanceMutation": mutation })
}

fn instance_json(instance: &ProviderInstanceConfig) -> Value {
    serde_json::to_value(instance).unwrap_or(Value::Null)
}

/// The text generation selection a disabled instance can no longer serve
/// (`DEFAULT_UNIFIED_SETTINGS.textGenerationModelSelection`).
fn default_text_generation_selection() -> Value {
    json!({ "instanceId": "codex", "model": "gpt-6-luna", "options": [{ "id": "reasoningEffort", "value": "low" }] })
}

/// `persistInstance` from `updateInstance`: upserts the instance. Switching off the instance
/// that generates titles and commit messages also puts that model back to its default.
pub fn upsert_payload(settings: &ServerSettings, instance_id: &str, previous: &ProviderInstanceConfig, next: &ProviderInstanceConfig) -> Value {
    let mut patch = Map::new();
    let disabling = next.enabled == Some(false) && providers::resolve_instance_enabled(previous);
    if disabling && settings.provider_panel.text_generation_instance() == instance_id {
        patch.insert("textGenerationModelSelection".into(), default_text_generation_selection());
    }
    mutation_payload(json!({ "operation": "upsert", "instanceId": instance_id, "instance": instance_json(next) }), patch)
}

/// One change to an instance. The save queue turns it into an upsert only when it is sent, so
/// edits queued before an earlier save answers build on that save rather than undo it.
#[derive(Debug, Clone, PartialEq)]
pub enum InstanceEdit {
    ConfigField { key: String, value: Value },
    Environment(Vec<Value>),
    Enabled(bool),
    DisplayName(String),
    Accent(String),
    AddCustomModel(String),
    RemoveCustomModel(String),
}

impl InstanceEdit {
    pub fn apply(&self, instance: &ProviderInstanceConfig) -> ProviderInstanceConfig {
        match self {
            InstanceEdit::ConfigField {key,value} => form::with_field(instance,key,value.clone()),
            InstanceEdit::Environment(rows) => form::with_environment(instance,rows.clone()),
            InstanceEdit::Enabled(enabled) => with_enabled(instance, *enabled),
            InstanceEdit::DisplayName(name) => with_display_name(instance, name),
            InstanceEdit::Accent(accent) => with_accent(instance, accent),
            InstanceEdit::AddCustomModel(slug) => with_custom_model_added(instance, slug),
            InstanceEdit::RemoveCustomModel(slug) => with_custom_model_removed(instance, slug),
        }
    }
}

/// The upsert `edit` makes to `instance_id` as `settings` stand now; `None` when it changes
/// nothing. An unconfigured default slot starts from its driver's defaults.
pub fn edit_payload(settings: &ServerSettings, instance_id: &str, edit: &InstanceEdit) -> Option<Value> {
    let previous =
        settings.provider_instances.get(instance_id).cloned().unwrap_or_else(|| ProviderInstanceConfig { driver: instance_id.to_owned(), ..Default::default() });
    let next = edit.apply(&previous);
    (next != previous).then(|| upsert_payload(settings, instance_id, &previous, &next))
}

/// Deletes a custom instance, or resets a default slot to its unconfigured defaults.
pub fn remove_payload(instance_id: &str) -> Value {
    mutation_payload(json!({ "operation": "remove", "instanceId": instance_id }), Map::new())
}

pub fn create_payload(instance_id: &str, instance: &ProviderInstanceConfig) -> Value {
    mutation_payload(json!({ "operation": "create", "instanceId": instance_id, "instance": instance_json(instance) }), Map::new())
}

/// The registry agent a deleted ACP instance installed, which the server may now uninstall.
pub fn managed_acp_agent(instance: &ProviderInstanceConfig) -> Option<String> {
    if instance.driver != ACP_REGISTRY || config_string(instance.config.as_ref(), "source").as_deref() == Some("local") {
        return None;
    }
    config_string(instance.config.as_ref(), "agentId")
}

pub fn uninstall_acp_payload(agent_id: &str) -> Value {
    json!({ "agentId": agent_id })
}

/// `readConfigString`.
pub fn config_string(config: Option<&Value>, key: &str) -> Option<String> {
    config?.get(key)?.as_str().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned)
}

/// `readCodexSetupMode`: Codex runs either the user's own CLI or a T3-managed runtime.
pub fn codex_managed(instance: &ProviderInstanceConfig) -> bool {
    instance.driver == "codex" && instance.config.as_ref().and_then(|config| config.get("setupMode")).and_then(Value::as_str) == Some("managed")
}

/// The explicit refresh the header's button runs: rediscover models and re-read secrets.
pub fn refresh_payload() -> Value {
    crate::providers::RefreshProviders::Settings.payload()
}

/// `runProviderUpdate`.
pub fn update_payload(driver: &str, instance_id: &str, target_version: Option<&str>) -> Value {
    let mut payload = json!({ "provider": driver, "instanceId": instance_id });
    if let Some(version) = target_version {
        payload["targetVersion"] = json!(version);
    }
    payload
}

/// The providers a `server.refreshProviders` or `server.updateProvider` reply carries.
pub fn decode_providers_reply(reply: Value) -> Option<Vec<ServerProvider>> {
    reply.get("providers")?.as_array()?;
    serde_json::from_value::<crate::server_config::ProvidersPayload>(reply).ok().map(|payload| payload.providers)
}

/// The newest `checkedAt` across providers (`ProviderLastChecked`).
pub fn last_checked(live: &[ServerProvider]) -> Option<&str> {
    live.iter().filter_map(|provider| provider.checked_at.0.as_deref()).max()
}

/// The refresh button's label: "Checked just now", "Checked 5m ago", or "Checked unavailable".
pub fn last_checked_label(live: &[ServerProvider], now_ms: i64) -> Option<String> {
    let checked = last_checked(live)?;
    if crate::sidebar::time::parse_iso(checked).is_none() {
        return Some("Checked unavailable".to_owned());
    }
    Some(format!("Checked {}", super::archive::relative_label(checked, now_ms)))
}

/// `msUntilRelativeTimeChange`: when the relative label next reads differently.
pub fn ms_until_label_changes(live: &[ServerProvider], now_ms: i64) -> Option<i64> {
    let at = crate::sidebar::time::parse_iso(last_checked(live)?)?;
    let elapsed = (now_ms - at).max(0);
    let unit = match elapsed {
        e if e < 3_600_000 => 60_000,
        e if e < 86_400_000 => 3_600_000,
        _ => 86_400_000,
    };
    Some((at + (elapsed / unit + 1) * unit - now_ms).min(86_400_000))
}

/// A custom model as `readCustomModelEntries` reads `config.customModels`.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomModel {
    pub slug: String,
    pub name: String,
    pub capabilities: Option<ModelCapabilities>,
}

fn custom_model_slug(entry: &Value) -> Option<String> {
    let slug = match entry {
        Value::String(slug) => slug.as_str(),
        Value::Object(record) => record.get("slug")?.as_str()?,
        _ => return None,
    };
    Some(slug.trim().to_owned()).filter(|slug| !slug.is_empty())
}

fn custom_model_entries(instance: &ProviderInstanceConfig) -> Vec<Value> {
    instance.config.as_ref().and_then(|config| config.get("customModels")).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// `readCustomModelEntries`. Antigravity has no custom models.
pub fn custom_models(instance: &ProviderInstanceConfig) -> Vec<CustomModel> {
    if instance.driver == "antigravity" {
        return Vec::new();
    }
    let mut seen = Vec::new();
    let mut models = Vec::new();
    for entry in custom_model_entries(instance) {
        let Some(slug) = custom_model_slug(&entry) else { continue };
        if seen.contains(&slug) {
            continue;
        }
        seen.push(slug.clone());
        let name = entry.get("name").and_then(Value::as_str).map(str::trim).filter(|name| !name.is_empty()).map_or_else(|| slug.clone(), str::to_owned);
        let capabilities = entry.get("capabilities").filter(|value| !value.is_null()).and_then(|value| serde_json::from_value(value.clone()).ok());
        models.push(CustomModel { slug, name, capabilities });
    }
    models
}

/// `MAX_CUSTOM_MODEL_LENGTH`.
pub const MAX_CUSTOM_MODEL_LENGTH: usize = 256;

/// `handleAdd`'s checks: the trimmed slug to add, or why it cannot be.
pub fn validate_custom_model(input: &str, live_models: &[ProviderModel], custom: &[CustomModel]) -> Result<String, String> {
    let slug = input.trim();
    if slug.is_empty() {
        return Err("Enter a model slug.".into());
    }
    if live_models.iter().any(|model| !model.is_custom && model.slug == slug) {
        return Err("That model is already built in.".into());
    }
    if slug.chars().count() > MAX_CUSTOM_MODEL_LENGTH {
        return Err(format!("Model slugs must be {MAX_CUSTOM_MODEL_LENGTH} characters or less."));
    }
    if custom.iter().any(|model| model.slug == slug) {
        return Err("That custom model is already saved.".into());
    }
    Ok(slug.to_owned())
}

fn with_custom_entries(instance: &ProviderInstanceConfig, entries: Vec<Value>) -> ProviderInstanceConfig {
    let mut config = match &instance.config {
        Some(Value::Object(config)) => config.clone(),
        _ => Map::new(),
    };
    config.insert("customModels".into(), Value::Array(entries));
    ProviderInstanceConfig { config: Some(Value::Object(config)), ..instance.clone() }
}

/// Appends a custom model, keeping the saved entries (and their options) as they are.
pub fn with_custom_model_added(instance: &ProviderInstanceConfig, slug: &str) -> ProviderInstanceConfig {
    let mut entries = custom_model_entries(instance);
    entries.push(json!(slug));
    with_custom_entries(instance, entries)
}

/// Removes a custom model. An emptied list stays an explicit `[]`, which differs from the
/// driver's defaults (`nextConfigBlobWithValue`).
pub fn with_custom_model_removed(instance: &ProviderInstanceConfig, slug: &str) -> ProviderInstanceConfig {
    let entries = custom_model_entries(instance).into_iter().filter(|entry| custom_model_slug(entry).as_deref() != Some(slug)).collect();
    with_custom_entries(instance, entries)
}

/// A row of the models list.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayModel {
    pub slug: String,
    pub name: String,
    pub is_custom: bool,
    pub capabilities: Option<ModelCapabilities>,
}

/// `deriveProviderModelsForDisplay`: the server's built-in models, then saved custom models it
/// does not already list.
pub fn models_for_display(live: &[ProviderModel], custom: &[CustomModel]) -> Vec<DisplayModel> {
    let built_in: Vec<&ProviderModel> = live.iter().filter(|model| !model.is_custom).collect();
    let mut models: Vec<DisplayModel> = built_in
        .iter()
        .map(|model| DisplayModel { slug: model.slug.clone(), name: model.name.clone(), is_custom: false, capabilities: model.capabilities.clone() })
        .collect();
    for entry in custom.iter().filter(|entry| !built_in.iter().any(|model| model.slug == entry.slug)) {
        let reported = live.iter().find(|model| model.is_custom && model.slug == entry.slug).and_then(|model| model.capabilities.clone());
        models.push(DisplayModel {
            slug: entry.slug.clone(),
            name: entry.name.clone(),
            is_custom: true,
            capabilities: entry.capabilities.clone().or(reported),
        });
    }
    models
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelGroup {
    Favorite,
    Visible,
    Hidden,
}

/// `groupModelsForDisplay`: favorites, then the rest, then built-ins hidden from the picker, each
/// in the user's order and then the server's.
pub fn group_models(models: Vec<DisplayModel>, favorites: &[String], hidden: &[String], order: &[String]) -> Vec<(DisplayModel, ModelGroup)> {
    let favorites: HashSet<&str> = favorites.iter().map(String::as_str).collect();
    let hidden: HashSet<&str> = hidden.iter().map(String::as_str).collect();
    let mut rank: HashMap<&str, usize> = HashMap::with_capacity(order.len());
    for (index, slug) in order.iter().enumerate() {
        rank.entry(slug.as_str()).or_insert(index);
    }
    let mut ranked: Vec<((u8, usize, usize), DisplayModel, ModelGroup)> = models
        .into_iter()
        .enumerate()
        .map(|(index, model)| {
            let (key, group) = if favorites.contains(model.slug.as_str()) {
                (0, ModelGroup::Favorite)
            } else if !model.is_custom && hidden.contains(model.slug.as_str()) {
                (2, ModelGroup::Hidden)
            } else {
                (1, ModelGroup::Visible)
            };
            ((key, rank.get(model.slug.as_str()).copied().unwrap_or(usize::MAX), index), model, group)
        })
        .collect();
    ranked.sort_by_key(|(key, _, _)| *key);
    ranked.into_iter().map(|(_, model, group)| (model, group)).collect()
}

/// The models list offers a filter past this many models.
pub const MODEL_FILTER_THRESHOLD: usize = 8;

/// One drawn row of the models list (`ProviderModelsSection.tsx`).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    pub model: DisplayModel,
    pub group: ModelGroup,
    /// Hidden from the picker. A hidden favorite stays in Favorites, so this is not its group.
    pub hidden: bool,
    pub heading: Option<&'static str>,
    pub can_move_up: bool,
    pub can_move_down: bool,
}

/// The rows `filter` (trimmed, lowercase) leaves, with group headings and move buttons. A
/// filter applies only past `MODEL_FILTER_THRESHOLD` models, and moving needs the full list.
pub fn model_rows(grouped: &[(DisplayModel, ModelGroup)], hidden: &[String], filter: &str) -> Vec<ModelRow> {
    let hidden: HashSet<&str> = hidden.iter().map(String::as_str).collect();
    let filtering = grouped.len() > MODEL_FILTER_THRESHOLD && !filter.is_empty();
    let has_favorites = grouped.iter().any(|(_, group)| *group == ModelGroup::Favorite);
    let mut previous = None;
    grouped
        .iter()
        .enumerate()
        .filter(|(_, (model, _))| !filtering || model.name.to_lowercase().contains(filter) || model.slug.to_lowercase().contains(filter))
        .map(|(position, (model, group))| {
            let starts_group = previous != Some(*group);
            previous = Some(*group);
            let heading = match group {
                ModelGroup::Favorite if starts_group && has_favorites => Some("Favorites"),
                ModelGroup::Visible if starts_group && has_favorites => Some("All"),
                ModelGroup::Hidden if starts_group => Some("Hidden from picker"),
                _ => None,
            };
            ModelRow {
                model: model.clone(),
                group: *group,
                hidden: !model.is_custom && hidden.contains(model.slug.as_str()),
                heading,
                can_move_up: !filtering && position > 0 && grouped[position - 1].1 == *group,
                can_move_down: !filtering && grouped.get(position + 1).is_some_and(|(_, next)| next == group),
            }
        })
        .collect()
}

/// `describeModelCapabilities`: "Fast mode", and "No reasoning" where the others reason.
pub fn capability_labels(model: &DisplayModel, flag_missing_reasoning: bool) -> Vec<&'static str> {
    use crate::server_config::OptionDescriptor;
    let descriptors = model.capabilities.as_ref().map(|caps| caps.option_descriptors.as_slice()).unwrap_or_default();
    let mut labels = Vec::new();
    let fast = descriptors.iter().any(|descriptor| match descriptor {
        OptionDescriptor::Boolean(boolean) => boolean.id == "fastMode",
        OptionDescriptor::Select(select) => {
            select.id == "fastMode" || (select.id == "serviceTier" && select.options.iter().any(|option| option.id == "fast" || option.label == "Fast"))
        }
    });
    if fast {
        labels.push("Fast mode");
    }
    if flag_missing_reasoning && !model.is_custom && model.capabilities.is_some() && !matches!(model.slug.as_str(), "default" | "auto") && !has_reasoning(model) {
        labels.push("No reasoning");
    }
    labels
}

/// `hasReasoningControl`.
pub fn has_reasoning(model: &DisplayModel) -> bool {
    model.capabilities.as_ref().is_some_and(|caps| caps.option_descriptors.iter().any(crate::traits::is_reasoning_control))
}

/// The models summary beside the list's actions: "12 models · 2 favorites · 3 hidden".
pub fn models_summary(grouped: &[(DisplayModel, ModelGroup)], hidden: &[String]) -> String {
    let count = grouped.len();
    let favorites = grouped.iter().filter(|(_, group)| *group == ModelGroup::Favorite).count();
    let hidden = grouped.iter().filter(|(model, _)| !model.is_custom && hidden.contains(&model.slug)).count();
    let mut text = format!("{count} model{}", if count == 1 { "" } else { "s" });
    if favorites > 0 {
        text.push_str(&format!(" \u{00b7} {favorites} favorite{}", if favorites == 1 { "" } else { "s" }));
    }
    if hidden > 0 {
        text.push_str(&format!(" \u{00b7} {hidden} hidden"));
    }
    text
}

/// `setModelsHidden` for the given models.
pub fn hidden_payload(instance_id: &str, models: &[(String, bool)]) -> Value {
    let entries: Vec<Value> = models.iter().map(|(model, hidden)| json!({ "provider": instance_id, "model": model, "hidden": hidden })).collect();
    json!({ "patch": { "setModelsHidden": entries } })
}

/// "Disable all" hides every built-in model; "Enable all" (when all are hidden) shows them again.
pub fn bulk_hidden_payload(instance_id: &str, models: &[DisplayModel], hidden: &[String]) -> Option<Value> {
    let built_in: Vec<&DisplayModel> = models.iter().filter(|model| !model.is_custom).collect();
    if built_in.is_empty() {
        return None;
    }
    let hide = !built_in.iter().all(|model| hidden.contains(&model.slug));
    let changes: Vec<(String, bool)> = built_in.iter().map(|model| (model.slug.clone(), hide)).collect();
    Some(hidden_payload(instance_id, &changes))
}

/// Whether every built-in model is hidden, which turns "Disable all" into "Enable all".
pub fn all_built_in_hidden(models: &[DisplayModel], hidden: &[String]) -> bool {
    let mut built_in = models.iter().filter(|model| !model.is_custom).peekable();
    built_in.peek().is_some() && built_in.all(|model| hidden.contains(&model.slug))
}

pub fn favorite_payload(instance_id: &str, model: &str, favorite: bool) -> Value {
    json!({ "patch": { "setModelFavorites": [{ "provider": instance_id, "model": model, "favorite": favorite }] } })
}

pub fn order_payload(instance_id: &str, order: &[String]) -> Value {
    json!({ "patch": { "setProviderModelOrder": { "provider": instance_id, "modelOrder": order } } })
}

/// `handleMove`: swaps a model with its neighbour inside the same group, as a full order.
pub fn moved_order(grouped: &[(DisplayModel, ModelGroup)], slug: &str, down: bool) -> Option<Vec<String>> {
    let index = grouped.iter().position(|(model, _)| model.slug == slug)?;
    let next = if down { index.checked_add(1)? } else { index.checked_sub(1)? };
    let (_, neighbour_group) = grouped.get(next)?;
    if *neighbour_group != grouped[index].1 {
        return None;
    }
    let mut order: Vec<String> = grouped.iter().map(|(model, _)| model.slug.clone()).collect();
    order.swap(index, next);
    Some(order)
}

/// The favorites of one instance, in settings order.
pub fn instance_favorites(settings: &ServerSettings, instance_id: &str) -> Vec<String> {
    providers::favorites(settings).iter().filter(|favorite| favorite.provider == instance_id).map(|favorite| favorite.model.clone()).collect()
}

/// The instance's hidden models and model order (`providerModelPreferences`).
pub fn instance_preferences(settings: &ServerSettings, instance_id: &str) -> (Vec<String>, Vec<String>) {
    settings
        .provider_model_preferences
        .as_ref()
        .and_then(|preferences| preferences.get(instance_id))
        .map(|preferences| (preferences.hidden_models.clone(), preferences.model_order.clone()))
        .unwrap_or_default()
}

/// `slugifyLabel`, capped at 48 so `<driver>_<slug>` fits the 64-character id.
fn slugify_label(label: &str) -> String {
    let mut slug = String::new();
    let mut pending_separator = false;
    for ch in label.trim().to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            if pending_separator && !slug.is_empty() {
                slug.push('_');
            }
            pending_separator = false;
            slug.push(ch);
        } else {
            pending_separator = true;
        }
    }
    slug.chars().take(48).collect::<String>().trim_end_matches('_').to_owned()
}

/// `deriveInstanceId`.
pub fn derive_instance_id(driver: &str, label: &str) -> String {
    let slug = slugify_label(label);
    if slug.is_empty() { String::new() } else { format!("{driver}_{slug}") }
}

/// Ids a new instance cannot take: every configured instance, plus Codex and Claude's default
/// slots, which run before they are configured.
pub fn reserved_instance_ids(settings: &ServerSettings) -> Vec<String> {
    let mut ids = vec!["codex".to_owned(), "claudeAgent".to_owned()];
    ids.extend(settings.provider_instances.iter().map(|(id, _)| id.to_owned()));
    ids
}

/// The id the add dialog proposes for a label: the driver's own id for its plain label, else
/// `<driver>_<label>`, with `_2`, `_3`… until it is free (`deriveAvailableInstanceId`).
pub fn proposed_instance_id(driver: &Driver, label: &str, existing: &[String]) -> String {
    let base = if label.trim().is_empty() || label == driver.label {
        if driver.kind == ACP_REGISTRY { format!("{}_custom", driver.kind) } else { driver.kind.to_owned() }
    } else {
        derive_instance_id(driver.kind, label)
    };
    if base.is_empty() || !existing.contains(&base) {
        return base;
    }
    (2..)
        .map(|suffix| {
            let suffix = format!("_{suffix}");
            let keep = 64usize.saturating_sub(suffix.len()).min(base.len());
            format!("{}{suffix}", &base[..keep])
        })
        .find(|candidate| !existing.contains(candidate))
        .unwrap_or(base)
}

/// `validateInstanceId`.
pub fn validate_instance_id(id: &str, existing: &[String]) -> Option<String> {
    if id.is_empty() {
        return Some("Instance ID is required.".into());
    }
    if id.chars().count() > 64 {
        return Some("Instance ID must be 64 characters or fewer.".into());
    }
    let mut chars = id.chars();
    let valid = chars.next().is_some_and(|first| first.is_ascii_alphabetic()) && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if !valid {
        return Some("Instance ID must start with a letter and use only letters, digits, '-', or '_'.".into());
    }
    if existing.iter().any(|existing| existing == id) {
        return Some(format!("An instance named '{id}' already exists."));
    }
    None
}

/// The instance `handleSave` creates. Codex instances made here use the CLI already installed.
pub fn new_instance(driver: &str, label: &str, accent: &str) -> ProviderInstanceConfig {
    let label = label.trim();
    ProviderInstanceConfig {
        driver: driver.to_owned(),
        display_name: (!label.is_empty()).then(|| label.to_owned()),
        accent_color: normalize_accent(accent),
        environment: None,
        enabled: Some(true),
        config: (driver == "codex").then(|| json!({ "setupMode": "existing" })),
    }
}

#[cfg(test)]
mod tests;
