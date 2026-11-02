//! The environment's config from `subscribeServerConfig`: providers with their models and option
//! descriptors, server settings, and keybindings. Mirrors `ServerConfigStreamEvent` in
//! `contracts/src/server.ts` and the reducer in `client-runtime/src/state/serverConfigProjection.ts`.
//! Only the fields the native client reads are typed. Keybindings stay raw until the keymap ports
//! them (ledger KB-USER). Arrays the contract marks forward-compatible drop entries that fail to
//! decode instead of failing the event.

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::model::ModelSelection;
use crate::project_scripts::ProjectScript;

/// `RuntimeMode` in `contracts/src/providerPolicy.ts`, in the web's picker order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeMode {
    ApprovalRequired,
    AutoAcceptEdits,
    Auto,
    FullAccess,
}

impl RuntimeMode {
    pub const ALL: [RuntimeMode; 4] =
        [RuntimeMode::ApprovalRequired, RuntimeMode::AutoAcceptEdits, RuntimeMode::Auto, RuntimeMode::FullAccess];

    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeMode::ApprovalRequired => "approval-required",
            RuntimeMode::AutoAcceptEdits => "auto-accept-edits",
            RuntimeMode::Auto => "auto",
            RuntimeMode::FullAccess => "full-access",
        }
    }

    pub fn parse(value: &str) -> Option<RuntimeMode> {
        RuntimeMode::ALL.into_iter().find(|mode| mode.as_str() == value)
    }
}

impl Default for RuntimeMode {
    /// `DEFAULT_RUNTIME_MODE`.
    fn default() -> Self {
        RuntimeMode::FullAccess
    }
}

/// `ThreadEnvMode`: where a new thread works, the project checkout or a new worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadEnvMode {
    Local,
    Worktree,
}

/// `ProviderInteractionMode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InteractionMode {
    #[default]
    Default,
    Plan,
}

impl InteractionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            InteractionMode::Default => "default",
            InteractionMode::Plan => "plan",
        }
    }

    pub fn parse(value: &str) -> Option<InteractionMode> {
        match value {
            "default" => Some(InteractionMode::Default),
            "plan" => Some(InteractionMode::Plan),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionChoice {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectDescriptor {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub options: Vec<OptionChoice>,
    #[serde(default)]
    pub current_value: Option<String>,
    #[serde(default)]
    pub prompt_injected_values: Vec<String>,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BooleanDescriptor {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub current_value: Option<bool>,
}

/// `ProviderOptionDescriptor`. Unknown `type`s are dropped by the containing array.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OptionDescriptor {
    Select(SelectDescriptor),
    Boolean(BooleanDescriptor),
}

impl OptionDescriptor {
    pub fn id(&self) -> &str {
        match self {
            OptionDescriptor::Select(select) => &select.id,
            OptionDescriptor::Boolean(boolean) => &boolean.id,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            OptionDescriptor::Select(select) => &select.label,
            OptionDescriptor::Boolean(boolean) => &boolean.label,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    #[serde(default, deserialize_with = "lenient_vec")]
    pub option_descriptors: Vec<OptionDescriptor>,
}

/// `ServerProviderModel`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub short_name: Option<String>,
    #[serde(default)]
    pub sub_provider: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub badge: Option<String>,
    #[serde(default)]
    pub is_custom: bool,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub is_legacy: bool,
    #[serde(default)]
    pub capabilities: Option<ModelCapabilities>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuth {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// `ServerProvider`: one provider instance as the server reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProvider {
    pub instance_id: String,
    pub driver: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub accent_color: Option<String>,
    #[serde(default)]
    pub badge_label: Option<String>,
    #[serde(default)]
    pub show_interaction_mode_toggle: Option<bool>,
    #[serde(default, deserialize_with = "lenient_option_vec")]
    pub supported_runtime_modes: Option<Vec<RuntimeMode>>,
    pub enabled: bool,
    pub installed: bool,
    pub status: String,
    #[serde(default)]
    pub auth: ProviderAuth,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub models: Vec<ProviderModel>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub usage_limits: Option<UsageLimits>,
    /// `false` when the provider cannot rewind its conversation ("Edit from here").
    #[serde(default)]
    pub supports_conversation_rollback: Option<bool>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub slash_commands: Vec<ProviderSlashCommand>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub skills: Vec<ProviderSkill>,
    /// Per-cwd commands and skills. Left out for clients that fetch them on demand.
    #[serde(default, deserialize_with = "lenient_vec")]
    pub workspace_snapshots: Vec<WorkspaceSnapshot>,
}

/// `ServerProviderSlashCommand`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSlashCommand {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub input: Option<SlashCommandInput>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SlashCommandInput {
    pub hint: String,
}

/// `ServerProviderSkill`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSkill {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub scope: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub short_description: Option<String>,
    /// Claude's `user-invocable: false`: only agents invoke it, so the menus hide it.
    #[serde(default)]
    pub user_invocable: Option<bool>,
}

/// `ServerProviderWorkspaceSnapshot`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshot {
    pub cwd: String,
    pub checked_at: String,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub slash_commands: Vec<ProviderSlashCommand>,
    #[serde(default)]
    pub slash_commands_pending: Option<bool>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub skills: Vec<ProviderSkill>,
}

/// `ServerProviderUsageWindow`: how much of one quota is spent and when it resets.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    #[serde(default)]
    pub used_percent: f64,
    #[serde(default)]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct UsageLimits {
    #[serde(default, deserialize_with = "lenient_vec")]
    pub windows: Vec<UsageWindow>,
}

impl ServerProvider {
    pub fn is_available(&self) -> bool {
        self.availability.as_deref() != Some("unavailable")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCapabilities {
    #[serde(default)]
    pub thread_auto_settlement_scope: Option<bool>,
    #[serde(default)]
    pub thread_stop: Option<bool>,
    #[serde(default)]
    pub thread_force_stop: Option<bool>,
    #[serde(default)]
    pub side_chats: Option<bool>,
    #[serde(default)]
    pub thread_auto_settle_opt_out: Option<bool>,
    #[serde(default)]
    pub project_organization: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDescriptor {
    #[serde(default)]
    pub environment_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub server_version: String,
    #[serde(default)]
    pub orchestration_protocol_version: Option<u32>,
    #[serde(default)]
    pub capabilities: EnvironmentCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelFavorite {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPreferences {
    #[serde(default)]
    pub hidden_models: Vec<String>,
    #[serde(default)]
    pub model_order: Vec<String>,
}

/// `ProviderInstanceConfig`: only what decides whether an instance is enabled.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceConfig {
    #[serde(default)]
    pub driver: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub config: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInstanceOverride {
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `ProjectSettingsOverrides`: the project-scoped keys new threads read.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettingsOverrides {
    /// Present-null resets to "no default", so the outer option tells absent from null.
    #[serde(default, deserialize_with = "present_nullable")]
    pub default_model_selection: Option<Option<ModelSelection>>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub default_runtime_mode: Option<RuntimeMode>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub default_thread_env_mode: Option<ThreadEnvMode>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub new_worktrees_start_from_origin: Option<bool>,
    #[serde(default, deserialize_with = "lenient_map")]
    pub provider_instances: HashMap<String, ProjectInstanceOverride>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub default_project_scripts: Option<Vec<ProjectScript>>,
}

/// `ServerSettings`: the subset the composer reads. Absent keys take the contract's defaults.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    /// Null means unmigrated; the web then falls back to its client-side favorites.
    #[serde(default, deserialize_with = "lenient_option_vec")]
    pub favorites: Option<Vec<ModelFavorite>>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub provider_model_preferences: Option<HashMap<String, ModelPreferences>>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub default_model_selection: Option<ModelSelection>,
    #[serde(default, deserialize_with = "lenient")]
    pub default_runtime_mode: RuntimeMode,
    /// Absent means inherit: the project's `t3.json`, then local.
    #[serde(default, deserialize_with = "lenient_option")]
    pub default_thread_env_mode: Option<ThreadEnvMode>,
    /// Absent means true, the contract's default.
    #[serde(default, deserialize_with = "lenient_option")]
    pub new_worktrees_start_from_origin: Option<bool>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub fallback_model_selection: Option<ModelSelection>,
    #[serde(default, deserialize_with = "lenient_map")]
    pub project_settings_overrides: HashMap<String, ProjectSettingsOverrides>,
    #[serde(default, deserialize_with = "lenient")]
    pub project_settings_folded: bool,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub default_project_scripts: Vec<ProjectScript>,
    /// The legacy per-project scripts; null means the defaults.
    #[serde(default, deserialize_with = "lenient_map")]
    pub project_script_overrides: HashMap<String, Option<Vec<ProjectScript>>>,
    #[serde(default, deserialize_with = "lenient_map")]
    pub provider_instances: HashMap<String, ProviderInstanceConfig>,
    #[serde(flatten)]
    pub threads: crate::settings::ThreadSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default)]
    pub environment: EnvironmentDescriptor,
    #[serde(default)]
    pub cwd: String,
    /// `ResolvedKeybindingsConfig`, kept raw for the keymap (ledger KB-USER).
    #[serde(default)]
    pub keybindings: Value,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub providers: Vec<ServerProvider>,
    #[serde(default)]
    pub settings: ServerSettings,
    #[serde(default)]
    pub new_projects_root: Option<String>,
    /// The shell can omit idle subagent children; `getThreadFamily` serves them.
    #[serde(default)]
    pub shell_thread_families: bool,
    /// The shell can omit older settled threads; `getOlderSettledThreads` serves them.
    #[serde(default)]
    pub shell_older_settled: bool,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub available_editors: Vec<String>,
}

impl ServerConfig {
    pub fn provider(&self, instance_id: &str) -> Option<&ServerProvider> {
        self.providers.iter().find(|provider| provider.instance_id == instance_id)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingsPayload {
    #[serde(default)]
    pub keybindings: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProvidersPayload {
    #[serde(default, deserialize_with = "lenient_vec")]
    pub providers: Vec<ServerProvider>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SettingsPayload {
    pub settings: ServerSettings,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ServerConfigStreamEvent {
    #[serde(rename = "snapshot")]
    Snapshot { config: Box<ServerConfig> },
    #[serde(rename = "keybindingsUpdated")]
    KeybindingsUpdated { payload: KeybindingsPayload },
    #[serde(rename = "providerStatuses")]
    ProviderStatuses { payload: ProvidersPayload },
    #[serde(rename = "settingsUpdated")]
    SettingsUpdated { payload: Box<SettingsPayload> },
    #[serde(other)]
    Unknown,
}

/// Client-side config. Events before the first snapshot are dropped, as `Option.map` drops them.
#[derive(Debug, Clone, Default)]
pub struct ServerConfigState {
    pub config: Option<ServerConfig>,
    /// Counts settings the stream delivered. The protocol carries no revision, so an
    /// `updateSettings` reply applies only while nothing has streamed since it was sent.
    settings_revision: u64,
}

impl ServerConfigState {
    /// Returns true when the config changed. Reconnects resend unchanged data, so equal values
    /// are reported as unchanged and views stay quiet.
    pub fn apply(&mut self, event: ServerConfigStreamEvent) -> bool {
        match event {
            ServerConfigStreamEvent::Snapshot { config } => {
                self.settings_revision += 1;
                if self.config.as_ref() == Some(&*config) {
                    return false;
                }
                self.config = Some(*config);
                true
            }
            ServerConfigStreamEvent::KeybindingsUpdated { payload } => {
                self.update(|config| replace(&mut config.keybindings, payload.keybindings))
            }
            ServerConfigStreamEvent::ProviderStatuses { payload } => {
                self.update(|config| replace(&mut config.providers, payload.providers))
            }
            ServerConfigStreamEvent::SettingsUpdated { payload } => {
                self.settings_revision += 1;
                self.update(|config| replace(&mut config.settings, payload.settings))
            }
            ServerConfigStreamEvent::Unknown => false,
        }
    }

    pub fn settings_revision(&self) -> u64 {
        self.settings_revision
    }

    /// Applies what a `server.updateSettings` sent at `sent_at` answered with, unless settings
    /// streamed since: those are at least as new. Returns true when the config changed.
    pub fn apply_settings_reply(&mut self, settings: ServerSettings, sent_at: u64) -> bool {
        sent_at == self.settings_revision && self.update(|config| replace(&mut config.settings, settings))
    }

    fn update(&mut self, change: impl FnOnce(&mut ServerConfig) -> bool) -> bool {
        self.config.as_mut().is_some_and(change)
    }
}

fn replace<T: PartialEq>(slot: &mut T, next: T) -> bool {
    if *slot == next {
        return false;
    }
    *slot = next;
    true
}

fn decode_each<T: DeserializeOwned>(value: Value) -> Option<Vec<T>> {
    match value {
        Value::Array(values) => Some(values.into_iter().filter_map(|value| serde_json::from_value(value).ok()).collect()),
        _ => None,
    }
}

pub(crate) fn lenient_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(decode_each(Value::deserialize(d)?).unwrap_or_default())
}

fn lenient_option_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Option<Vec<T>>, D::Error> {
    Ok(decode_each(Value::deserialize(d)?))
}

fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn lenient_option<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

fn present_nullable<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Option<Option<T>>, D::Error> {
    let value = Value::deserialize(d)?;
    if value.is_null() {
        return Ok(Some(None));
    }
    Ok(serde_json::from_value(value).ok().map(Some))
}

fn lenient_map<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<HashMap<String, T>, D::Error> {
    let Value::Object(entries) = Value::deserialize(d)? else { return Ok(HashMap::new()) };
    Ok(entries
        .into_iter()
        .filter_map(|(key, value)| serde_json::from_value(value).ok().map(|value| (key, value)))
        .collect())
}

#[cfg(test)]
mod tests;
