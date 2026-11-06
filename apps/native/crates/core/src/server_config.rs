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
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub subscription_sharing: Option<bool>,
    #[serde(default)]
    pub can_logout: Option<bool>,
    /// `AcpRegistryUrlAuthAction`: a URL the agent asked the user to open to finish signing in.
    #[serde(default, deserialize_with = "lenient_option")]
    pub action: Option<UrlAuthAction>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlAuthAction {
    pub elicitation_id: String,
    pub url: String,
    #[serde(default)]
    pub message: String,
}

/// `ServerProvider.setup`: whether the server can sign this provider in or install it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSetup {
    #[serde(default)]
    pub can_authenticate: bool,
    #[serde(default)]
    pub can_install: bool,
    #[serde(default)]
    pub documentation_url: Option<String>,
}

/// `ServerProviderVersionAdvisory`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionAdvisory {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub update_command: Option<String>,
    #[serde(default)]
    pub can_update: bool,
    #[serde(default)]
    pub can_install_version: Option<bool>,
    #[serde(default)]
    pub message: Option<String>,
}

/// `ServerProviderCompatibilityAdvisory`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatibilityAdvisory {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub latest_version_status: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub recommended_version: Option<String>,
    #[serde(default)]
    pub recommended_range: Option<String>,
}

/// `ServerProviderUpdateState`: a `server.updateProvider` run as the server last reported it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub message: Option<String>,
}

/// `checkedAt`, which every status push restamps on every provider. Equality ignores it so an
/// otherwise unchanged push stays quiet (`providersIgnoringCheckedAtAtom`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct CheckedAt(pub Option<String>);

impl PartialEq for CheckedAt {
    fn eq(&self, _: &Self) -> bool {
        true
    }
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
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub version_advisory: Option<VersionAdvisory>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub compatibility_advisory: Option<CompatibilityAdvisory>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub update_state: Option<UpdateState>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub setup: Option<ProviderSetup>,
    #[serde(default)]
    pub runtime_paths: Option<Value>,
    #[serde(default)]
    pub checked_at: CheckedAt,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageWindowKind {
    Session,
    Weekly,
    Monthly,
    Other,
}

/// `ServerProviderUsageWindow`: how much of one quota is spent and when it resets. A window of
/// an unknown kind fails to decode and its array drops it, as `ForwardCompatibleArray` does.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    pub kind: UsageWindowKind,
    pub label: String,
    #[serde(default)]
    pub used_percent: f64,
    #[serde(default)]
    pub resets_at: Option<String>,
    #[serde(default)]
    pub window_duration_mins: Option<u64>,
}

/// `ServerProviderResetCredits`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetCredits {
    #[serde(default)]
    pub available_count: u64,
    #[serde(default)]
    pub next_expires_at: Option<String>,
    #[serde(default)]
    pub next_credit_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ExternalUsage {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LimitsUnavailable {
    /// `unsupported` or `probeFailed`.
    pub reason: String,
    #[serde(default)]
    pub message: Option<String>,
}

/// `ServerProviderUsageLimits`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimits {
    #[serde(default)]
    pub checked_at: String,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub windows: Vec<UsageWindow>,
    #[serde(default)]
    pub credential_fingerprint: Option<String>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub reset_credits: Option<ResetCredits>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub external_usage: Option<ExternalUsage>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub unavailable: Option<LimitsUnavailable>,
}

/// `UsageLimitSourceAccount`: one account a limit source (CLI Proxy) reports.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitSourceAccount {
    pub id: String,
    pub driver: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub usage_limits: UsageLimits,
}

/// `UsageLimitSourceSnapshot`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitSource {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub checked_at: String,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub accounts: Vec<UsageLimitSourceAccount>,
    #[serde(default)]
    pub error: Option<String>,
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
    pub server_self_update: Option<String>,
    #[serde(default)]
    pub desktop_app_update: Option<bool>,
    /// File attachments and the largest upload the server takes; absent when it takes none.
    #[serde(default, deserialize_with = "lenient_option")]
    pub file_attachments: Option<FileAttachmentsCapability>,
    #[serde(default)]
    pub question_attachments: Option<bool>,
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
    #[serde(default, deserialize_with = "lenient_option")]
    pub thread_project_from_scratch: Option<bool>,
    #[serde(default)]
    pub project_organization: Option<bool>,
    #[serde(default)]
    pub worktrees_directory: Option<bool>,
    #[serde(default)]
    pub storage_cleanup: Option<bool>,
    #[serde(default)]
    pub storage_cleanup_run: Option<bool>,
    #[serde(default)]
    pub settled_worktree_cleanup: Option<bool>,
    #[serde(default)]
    pub database_storage: Option<bool>,
    #[serde(default)]
    pub otlp_endpoint_check: Option<bool>,
    #[serde(default)]
    pub one_time_schedules: Option<bool>,
    /// The server hosts headless Chromium tabs streamed over `/api/preview-stream`.
    #[serde(default)]
    pub server_browser: Option<bool>,
    /// The server answers `orchestration.subscribeCommandOutput`; older servers never send it.
    #[serde(default)]
    pub command_output_streaming: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachmentsCapability {
    pub max_upload_bytes: u64,
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
    #[serde(default, deserialize_with = "lenient_option")]
    pub platform: Option<EnvironmentPlatform>,
}

/// `ExecutionEnvironmentPlatform`: which shell the server's commands are written for.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct EnvironmentPlatform {
    #[serde(default)]
    pub machine: String,
    /// `darwin`, `linux`, `windows`, or `unknown`.
    #[serde(default)]
    pub os: String,
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

/// `ProviderInstanceConfig`: config stays raw; environment references use the contracts' wire
/// codec. Redacted secrets remain present so unrelated edits preserve them on the server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceConfig {
    #[serde(default)]
    pub driver: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with="crate::settings::providers::form::decode_environment", serialize_with="crate::settings::providers::form::encode_environment")]
    pub environment: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// Present-null means no project default; absent inherits the environment's.
    #[serde(default, deserialize_with = "present_nullable")]
    pub pull_request_merge_method: Option<Option<crate::pull_requests::wire::MergeMethod>>,
    /// The entry as the server sent it. A write replaces the whole entry, so it starts from this
    /// to keep the keys this client does not read (`keepUnknownOverrideKeys`).
    #[serde(skip)]
    pub raw: serde_json::Map<String, Value>,
}

/// `ServerSettings`: the subset the composer reads. Absent keys take the contract's defaults.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    #[serde(default, deserialize_with = "lenient_option")]
    pub guest_sharing_enabled: Option<bool>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub environment_icon: Option<String>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub enable_agent_browser_access: Option<bool>,
    /// Missing on older hosts; the Maintenance UI default does not prove restart continuation.
    #[serde(skip)]
    pub update_continuation_advertised: bool,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub hidden_thread_tags: Vec<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub hide_tagged_threads: bool,
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
    #[serde(default, deserialize_with = "overrides_map")]
    pub project_settings_overrides: HashMap<String, ProjectSettingsOverrides>,
    #[serde(default, deserialize_with = "lenient")]
    pub project_settings_folded: bool,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub default_project_scripts: Vec<ProjectScript>,
    /// The legacy per-project scripts; null means the defaults.
    #[serde(default, deserialize_with = "lenient_map")]
    pub project_script_overrides: HashMap<String, Option<Vec<ProjectScript>>>,
    #[serde(default)]
    pub provider_instances: ProviderInstances,
    #[serde(default, deserialize_with = "lenient_option")]
    pub pull_request_merge_method: Option<crate::pull_requests::wire::MergeMethod>,
    /// The Usage page asks for summaries through this filter.
    #[serde(default, deserialize_with = "lenient_option")]
    pub usage_model_filter: Option<crate::usage::UsageModelFilter>,
    #[serde(flatten)]
    pub threads: crate::settings::ThreadSettings,
    #[serde(flatten)]
    pub provider_panel: crate::settings::providers::background::ProviderPanelSettings,
    #[serde(flatten)]
    pub worktrees: crate::settings::worktrees::WorktreeSettings,
    #[serde(flatten)]
    pub source_control: crate::settings::source_control::SourceControlSettings,
    #[serde(flatten)]
    pub maintenance: crate::settings::maintenance::MaintenanceSettings,
    #[serde(default, deserialize_with = "lenient")]
    pub secret_providers: crate::settings::secrets::SecretProviders,
    #[serde(default, deserialize_with = "lenient")]
    pub observability: crate::settings::diagnostics::ObservabilitySettings,
}

fn update_settings<'de, D: Deserializer<'de>>(d: D) -> Result<ServerSettings, D::Error> {
    let value = Value::deserialize(d)?;
    let advertised = value.get("continueThreadsAfterServerUpdate").and_then(Value::as_bool) == Some(true);
    let mut settings: ServerSettings = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
    settings.update_continuation_advertised = advertised;
    Ok(settings)
}

impl ServerSettings {
    /// `hiddenThreadTagsAtom` in `client-runtime/src/state/server.ts`.
    pub fn hidden_tags(&self) -> Option<Vec<String>> {
        let tags: Vec<String> = self.hidden_thread_tags.iter().map(|tag| tag.trim()).filter(|tag| !tag.is_empty()).map(str::to_owned).collect();
        (self.hide_tagged_threads && !tags.is_empty()).then_some(tags)
    }
}

/// `providerInstances` in the server's order, which is the order the provider settings list
/// custom instances in. Entries that fail to decode are dropped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderInstances(Vec<(String, ProviderInstanceConfig)>);

impl ProviderInstances {
    pub fn get(&self, instance_id: &str) -> Option<&ProviderInstanceConfig> {
        self.0.iter().find(|(id, _)| id == instance_id).map(|(_, instance)| instance)
    }

    pub fn contains_key(&self, instance_id: &str) -> bool {
        self.get(instance_id).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ProviderInstanceConfig)> {
        self.0.iter().map(|(id, instance)| (id.as_str(), instance))
    }
}

impl std::ops::Index<&str> for ProviderInstances {
    type Output = ProviderInstanceConfig;

    fn index(&self, instance_id: &str) -> &ProviderInstanceConfig {
        self.get(instance_id).unwrap_or_else(|| panic!("no provider instance {instance_id:?}"))
    }
}

impl FromIterator<(String, ProviderInstanceConfig)> for ProviderInstances {
    fn from_iter<I: IntoIterator<Item = (String, ProviderInstanceConfig)>>(entries: I) -> Self {
        ProviderInstances(entries.into_iter().collect())
    }
}

impl<'de> Deserialize<'de> for ProviderInstances {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let Value::Object(entries) = Value::deserialize(d)? else { return Ok(ProviderInstances::default()) };
        Ok(entries.into_iter().filter_map(|(id, value)| serde_json::from_value(value).ok().map(|instance| (id, instance))).collect())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default)]
    pub environment_themes: Vec<Value>,
    #[serde(skip)]
    pub environment_theme_definitions: Vec<crate::themes::ThemeDefinition>,
    #[serde(default)]
    pub environment: EnvironmentDescriptor,
    #[serde(default)]
    pub cwd: String,
    /// `ResolvedKeybindingsConfig`, kept raw for the keymap (ledger KB-USER).
    #[serde(default)]
    pub keybindings: Value,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub providers: Vec<ServerProvider>,
    #[serde(default, deserialize_with = "update_settings")]
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
    /// Sent only to clients that opt in with `usageLimitSources`; empty means none.
    #[serde(default, deserialize_with = "lenient_vec")]
    pub usage_limit_sources: Vec<UsageLimitSource>,
    /// Where `server.upsertKeybinding` writes; Settings opens it in an editor.
    #[serde(default)]
    pub keybindings_config_path: Option<String>,
    /// `ManagedPolicySummary`, kept raw; scheduled tasks read its runtime mode ceiling.
    #[serde(default)]
    pub managed_policy: Option<Value>,
    /// The workspace "Start without a project" opens (`projects.ensureScratch`); absent on
    /// servers without scratch projects.
    #[serde(default)]
    pub scratch_workspace_root: Option<String>,
    /// What the server runs with: its logs folder and OTLP exports (Settings → Diagnostics).
    #[serde(default, deserialize_with = "lenient_option")]
    pub observability: Option<crate::settings::diagnostics::ServerObservability>,
    /// Settings patches may carry `secretProviders`.
    #[serde(default)]
    pub secret_provider_settings: Option<bool>,
    #[serde(default)]
    pub secret_references: bool,
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
    #[serde(deserialize_with = "update_settings")]
    pub settings: ServerSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageLimitSourcesPayload {
    #[serde(default, deserialize_with = "lenient_vec")]
    pub sources: Vec<UsageLimitSource>,
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
    #[serde(rename = "usageLimitSourcesUpdated")]
    UsageLimitSourcesUpdated { payload: UsageLimitSourcesPayload },
    #[serde(rename = "environmentThemesUpdated")]
    EnvironmentThemesUpdated { payload: EnvironmentThemesPayload },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnvironmentThemesPayload {
    #[serde(skip)]
    pub definitions: Vec<crate::themes::ThemeDefinition>,
    #[serde(default)]
    pub themes: Vec<Value>,
}

impl ServerConfigStreamEvent {
    /// Run by the network worker before crossing into the UI's config reducer.
    pub fn prepare(mut self)->Self {
        match &mut self {
            Self::EnvironmentThemesUpdated{payload}=>payload.definitions=crate::themes::environment(&payload.themes),
            Self::Snapshot{config}=>config.environment_theme_definitions=crate::themes::environment(&config.environment_themes),
            _=>{}
        }
        self
    }
}

/// Client-side config. Events before the first snapshot are dropped, as `Option.map` drops them.
#[derive(Debug, Clone, Default)]
pub struct ServerConfigState {
    pub config: Option<ServerConfig>,
    /// Counts settings the stream delivered. The protocol carries no revision, so an
    /// `updateSettings` reply applies only while nothing has streamed since it was sent.
    settings_revision: u64,
    /// The same for keymaps, so a keybinding edit's reply never overwrites a newer stream.
    keybindings_revision: u64,
}

impl ServerConfigState {
    /// Returns true when the config changed. Reconnects resend unchanged data, so equal values
    /// are reported as unchanged and views stay quiet.
    pub fn apply(&mut self, event: ServerConfigStreamEvent) -> bool {
        match event {
            ServerConfigStreamEvent::Snapshot { mut config } => {
                self.settings_revision += 1;
                self.keybindings_revision += 1;
                // `serverConfigProjection.ts`: sources follow their own event once known.
                if let Some(current) = self.config.as_ref().filter(|current| !current.usage_limit_sources.is_empty()) {
                    config.usage_limit_sources = current.usage_limit_sources.clone();
                }
                if let Some(current) = self.config.as_ref() { config.environment_themes = current.environment_themes.clone();config.environment_theme_definitions=current.environment_theme_definitions.clone(); }
                if self.config.as_ref() == Some(&*config) {
                    return false;
                }
                self.config = Some(*config);
                true
            }
            ServerConfigStreamEvent::KeybindingsUpdated { payload } => {
                self.keybindings_revision += 1;
                self.update(|config| replace(&mut config.keybindings, payload.keybindings))
            }
            ServerConfigStreamEvent::ProviderStatuses { payload } => self.apply_providers(payload.providers),
            ServerConfigStreamEvent::SettingsUpdated { payload } => {
                self.settings_revision += 1;
                self.update(|config| replace(&mut config.settings, payload.settings))
            }
            ServerConfigStreamEvent::UsageLimitSourcesUpdated { payload } => {
                self.update(|config| replace(&mut config.usage_limit_sources, payload.sources))
            }
            ServerConfigStreamEvent::EnvironmentThemesUpdated { payload } => self.update(|config| {
                let raw=replace(&mut config.environment_themes,payload.themes);
                let prepared=replace(&mut config.environment_theme_definitions,payload.definitions);
                raw||prepared
            }),
            ServerConfigStreamEvent::Unknown => false,
        }
    }

    pub fn settings_revision(&self) -> u64 {
        self.settings_revision
    }

    /// Takes a provider list from the stream or a `server.refreshProviders` reply. The fresh
    /// `checkedAt` stamps are kept even when nothing else changed, which reports no change.
    pub fn apply_providers(&mut self, providers: Vec<ServerProvider>) -> bool {
        self.update(|config| {
            let changed = config.providers != providers;
            config.providers = providers;
            changed
        })
    }

    /// Applies what a `server.updateSettings` sent at `sent_at` answered with, unless settings
    /// streamed since: those are at least as new. Returns true when the config changed.
    pub fn apply_settings_reply(&mut self, settings: ServerSettings, sent_at: u64) -> bool {
        sent_at == self.settings_revision && self.update(|config| replace(&mut config.settings, settings))
    }

    pub fn keybindings_revision(&self) -> u64 {
        self.keybindings_revision
    }

    /// Applies the `keybindings` a `server.upsertKeybinding` / `server.removeKeybinding` sent at
    /// `sent_at` answered with, so an edit shows before `keybindingsUpdated` streams the same
    /// set, unless a keymap streamed since. Returns true when the config changed.
    pub fn apply_keybindings_reply(&mut self, reply: &Value, sent_at: u64) -> bool {
        if sent_at != self.keybindings_revision {
            return false;
        }
        match reply.get("keybindings") {
            Some(keybindings @ Value::Array(_)) => self.update(|config| replace(&mut config.keybindings, keybindings.clone())),
            _ => false,
        }
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

fn overrides_map<'de, D: Deserializer<'de>>(d: D) -> Result<HashMap<String, ProjectSettingsOverrides>, D::Error> {
    let Value::Object(entries) = Value::deserialize(d)? else { return Ok(HashMap::new()) };
    Ok(entries
        .into_iter()
        .filter_map(|(key, value)| {
            let Value::Object(raw) = value else { return None };
            let mut entry: ProjectSettingsOverrides = serde_json::from_value(Value::Object(raw.clone())).ok()?;
            entry.raw = raw;
            Some((key, entry))
        })
        .collect())
}

#[cfg(test)]
mod tests;
