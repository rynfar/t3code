//! Source control at environment scope: `SourceControlSettings.tsx`, the `source-control` rows of
//! `ProjectDefaultsSettings.tsx`, and `SourceControlWritingSettings.tsx`. Discovery decodes
//! `SourceControlDiscoveryResult` (`contracts/src/sourceControl.ts`), whose `Option` fields arrive
//! as `{"_tag":"Some","value":…}` or `{"_tag":"None"}`.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use super::{ServerSettingChange, lenient_or};
use crate::device::hosts::SecretValue;
use crate::model::ModelSelection;
use crate::pull_requests::wire::MergeMethod;
use crate::server_config::ServerSettings;

/// `server.discoverSourceControl`, payload `{}`.
pub const DISCOVER: &str = "server.discoverSourceControl";
/// `sourceControl.signInStart`, payload `{ provider, host }`.
pub const SIGN_IN_START: &str = "sourceControl.signInStart";
/// `sourceControl.signInWait`, payload `{ provider, host }`.
pub const SIGN_IN_WAIT: &str = "sourceControl.signInWait";
/// `sourceControl.signInCancel`, payload `{ provider, host }`.
pub const SIGN_IN_CANCEL: &str = "sourceControl.signInCancel";

/// The merge method select: `last` saves null.
pub const MERGE_METHODS: [(&str, &str); 5] = [
    ("last", "Last selected"),
    ("merge", "Merge"),
    ("squash", "Squash and merge"),
    ("rebase", "Rebase and merge"),
    ("fast-forward", "Fast-forward merge"),
];

pub fn merge_method_value(method: Option<MergeMethod>) -> &'static str {
    method.map_or("last", MergeMethod::as_str)
}

pub fn parse_merge_method(value: &str) -> Option<Option<MergeMethod>> {
    match value {
        "last" => Some(None),
        "merge" => Some(Some(MergeMethod::Merge)),
        "squash" => Some(Some(MergeMethod::Squash)),
        "rebase" => Some(Some(MergeMethod::Rebase)),
        "fast-forward" => Some(Some(MergeMethod::FastForward)),
        _ => None,
    }
}

/// `SourceControlWritingStyleMode` with its labels and descriptions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WritingMode {
    #[default]
    RepoConventions,
    ConventionalCommits,
    Custom,
}

impl WritingMode {
    pub const ALL: [WritingMode; 3] = [WritingMode::RepoConventions, WritingMode::ConventionalCommits, WritingMode::Custom];

    pub fn as_str(self) -> &'static str {
        match self {
            WritingMode::RepoConventions => "repo_conventions",
            WritingMode::ConventionalCommits => "conventional_commits",
            WritingMode::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Option<WritingMode> {
        WritingMode::ALL.into_iter().find(|mode| mode.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            WritingMode::RepoConventions => "Repository conventions",
            WritingMode::ConventionalCommits => "Conventional Commits",
            WritingMode::Custom => "Custom instructions",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            WritingMode::RepoConventions => "Commit messages and pull request titles and descriptions match each project's history.",
            WritingMode::ConventionalCommits => "Conventional Commit prefixes, with short pull request titles and descriptions.",
            WritingMode::Custom => "Use your instructions for commit messages and pull request titles and descriptions in every project.",
        }
    }
}

/// `SourceControlWritingStyleSetting`. A legacy plain string decodes as custom instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritingStyle {
    pub mode: WritingMode,
    pub custom_instructions: String,
    pub follow_change_request_templates: bool,
}

impl Default for WritingStyle {
    fn default() -> Self {
        WritingStyle { mode: WritingMode::default(), custom_instructions: String::new(), follow_change_request_templates: true }
    }
}

impl WritingStyle {
    /// The writing style row's reset shows while mode or instructions differ from the defaults.
    pub fn is_default_style(&self) -> bool {
        self.mode == WritingMode::default() && self.custom_instructions.is_empty()
    }
}

/// The `ServerSettings` keys the Source control page edits, besides `pullRequestMergeMethod`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SourceControlSettings {
    #[serde(deserialize_with = "lenient_or")]
    pub default_auto_pull: bool,
    #[serde(deserialize_with = "lenient_or")]
    pub remove_agent_credits_on_merge: bool,
    #[serde(deserialize_with = "offered_providers")]
    pub offered_source_control_providers: BTreeMap<String, bool>,
    #[serde(deserialize_with = "writing_style")]
    pub source_control_writing_style: WritingStyle,
    #[serde(deserialize_with = "lenient_or")]
    pub source_control_writer_model_selection: Option<ModelSelection>,
    #[serde(deserialize_with = "lenient_or")]
    pub review_guides_enabled: bool,
    #[serde(deserialize_with = "lenient_or")]
    pub review_guide_model_selection: Option<ModelSelection>,
    #[serde(deserialize_with = "trimmed")]
    pub review_guide_instructions: String,
}

impl SourceControlSettings {
    /// `isSourceControlProviderOffered`: true unless explicitly disabled for `kind`.
    pub fn is_provider_offered(&self, kind: &str) -> bool {
        self.offered_source_control_providers.get(kind).copied().unwrap_or(true)
    }
}

/// The writer model in use: its own selection, else the text generation model, which
/// `ProviderPanelSettings` decodes (two flattened structs cannot share a key).
pub fn writer_model(settings: &ServerSettings) -> Option<ModelSelection> {
    settings.source_control.source_control_writer_model_selection.clone().or_else(|| {
        settings.provider_panel.text_generation_model_selection.clone().and_then(|selection| serde_json::from_value(selection).ok())
    })
}

/// One Source control page edit.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceControlChange {
    AutoPull(bool),
    RemoveAgentCredits(bool),
    MergeMethod(Option<MergeMethod>),
    OfferProvider { kind: String, offered: bool },
    /// Saves or removes one host token while sending the full `tokens` map so other hosts'
    /// redacted tokens or secret references are preserved (`pingdotgg/t3code#29554`).
    SaveHostToken { kind: String, existing: BTreeMap<String, SecretValue>, host: String, token: SecretValue },
    /// Signs out of one app host while sending the full `signIns` map so other hosts' sessions stay.
    SignOutApp { kind: String, existing: BTreeMap<String, String>, host: String },
    /// Saves the per-host enable / pinned account choices under `sourceControlHosts.<kind>.hosts`.
    SaveHostAccounts { kind: String, hosts: BTreeMap<String, HostAccountChoice> },
    /// Saves generic host fields (Bitbucket / GitCafe) under `sourceControlHosts.<kind>`.
    SaveHostFields { kind: String, fields: Map<String, Value> },
    /// The mode, with the instructions on screen when the custom field is showing.
    WritingMode { mode: WritingMode, custom_instructions: Option<String> },
    CustomInstructions(String),
    ResetWritingStyle,
    FollowTemplates(bool),
    WriterModel(Option<ModelSelection>),
    ReviewGuides(bool),
    ReviewGuideModel(Option<ModelSelection>),
    ReviewGuideInstructions(String),
}

impl SourceControlChange {
    fn patch(&self) -> Value {
        match self {
            SourceControlChange::AutoPull(on) => json!({ "defaultAutoPull": on }),
            SourceControlChange::RemoveAgentCredits(on) => json!({ "removeAgentCreditsOnMerge": on }),
            SourceControlChange::MergeMethod(method) => json!({ "pullRequestMergeMethod": method.map(MergeMethod::as_str) }),
            SourceControlChange::OfferProvider { kind, offered } => json!({ "offeredSourceControlProviders": { kind: offered } }),
            SourceControlChange::SaveHostToken { kind, existing, host, token } => {
                let mut tokens = Map::new();
                for (saved_host, value) in existing {
                    tokens.insert(saved_host.clone(), value.encode());
                }
                tokens.insert(host.trim().to_ascii_lowercase(), token.encode());
                json!({ "sourceControlHosts": { kind: { "tokens": Value::Object(tokens) } } })
            }
            SourceControlChange::SignOutApp { kind, existing, host } => {
                let mut sign_ins = Map::new();
                for (saved_host, value) in existing {
                    sign_ins.insert(saved_host.clone(), Value::String(value.clone()));
                }
                sign_ins.insert(host.trim().to_ascii_lowercase(), Value::String(String::new()));
                json!({ "sourceControlHosts": { kind: { "signIns": Value::Object(sign_ins) } } })
            }
            SourceControlChange::SaveHostAccounts { kind, hosts } => {
                let mut encoded = Map::new();
                for (host, choice) in hosts {
                    let mut entry = Map::new();
                    entry.insert("enabled".to_owned(), Value::Bool(choice.enabled));
                    if let Some(account) = &choice.account {
                        entry.insert("account".to_owned(), Value::String(account.clone()));
                    }
                    encoded.insert(host.clone(), Value::Object(entry));
                }
                json!({ "sourceControlHosts": { kind: { "hosts": Value::Object(encoded) } } })
            }
            SourceControlChange::SaveHostFields { kind, fields } => {
                json!({ "sourceControlHosts": { kind: Value::Object(fields.clone()) } })
            }
            SourceControlChange::WritingMode { mode, custom_instructions } => {
                let mut style = json!({ "mode": mode.as_str() });
                if let Some(text) = custom_instructions {
                    style["customInstructions"] = json!(text.trim());
                }
                json!({ "sourceControlWritingStyle": style })
            }
            SourceControlChange::CustomInstructions(text) => json!({ "sourceControlWritingStyle": { "customInstructions": text.trim() } }),
            SourceControlChange::ResetWritingStyle => {
                json!({ "sourceControlWritingStyle": { "mode": WritingMode::default().as_str(), "customInstructions": "" } })
            }
            SourceControlChange::FollowTemplates(on) => json!({ "sourceControlWritingStyle": { "followChangeRequestTemplates": on } }),
            SourceControlChange::WriterModel(selection) => json!({ "sourceControlWriterModelSelection": selection }),
            SourceControlChange::ReviewGuides(on) => json!({ "reviewGuidesEnabled": on }),
            SourceControlChange::ReviewGuideModel(selection) => json!({ "reviewGuideModelSelection": selection }),
            SourceControlChange::ReviewGuideInstructions(text) => json!({ "reviewGuideInstructions": text.trim() }),
        }
    }

    pub fn into_server(self) -> ServerSettingChange {
        ServerSettingChange::Prepared(json!({ "patch": self.patch() }))
    }
}

fn offered_providers<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, bool>, D::Error> {
    let Value::Object(map) = Value::deserialize(d)? else {
        return Ok(BTreeMap::new());
    };
    Ok(map.into_iter().filter_map(|(k, v)| v.as_bool().map(|b| (k, b))).collect())
}

fn trimmed<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Value::deserialize(d)?.as_str().map(|value| value.trim().to_owned()).unwrap_or_default())
}

fn writing_style<'de, D: Deserializer<'de>>(d: D) -> Result<WritingStyle, D::Error> {
    let value = Value::deserialize(d)?;
    let defaults = WritingStyle::default();
    Ok(match value {
        Value::String(text) if text.trim().is_empty() => defaults,
        Value::String(text) => WritingStyle { mode: WritingMode::Custom, custom_instructions: text.trim().to_owned(), ..defaults },
        Value::Object(map) => WritingStyle {
            mode: map.get("mode").and_then(Value::as_str).and_then(WritingMode::parse).unwrap_or_default(),
            custom_instructions: map.get("customInstructions").and_then(Value::as_str).map(|text| text.trim().to_owned()).unwrap_or_default(),
            follow_change_request_templates: map.get("followChangeRequestTemplates").and_then(Value::as_bool).unwrap_or(true),
        },
        _ => defaults,
    })
}

/// `SourceControlProviderAuth.status`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AuthStatus {
    Authenticated,
    Unauthenticated,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveredAccount {
    pub host: String,
    pub account: String,
    pub authenticated: bool,
    pub active: bool,
    pub detail: Option<String>,
    pub environment_variable: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderAuth {
    pub status: AuthStatus,
    pub account: Option<String>,
    pub detail: Option<String>,
    pub accounts: Vec<DiscoveredAccount>,
    /// Some discovered account is signed in, though every host may be turned off.
    pub any_account_authenticated: bool,
}

/// A `VcsDiscoveryItem` (with `implemented`) or a `SourceControlProviderDiscoveryItem` (with `auth`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryItem {
    pub kind: String,
    pub label: String,
    pub executable: Option<String>,
    pub available: bool,
    pub version: Option<String>,
    pub install_hint: String,
    pub implemented: bool,
    pub app_sign_in: bool,
    pub auth: Option<ProviderAuth>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    pub version_control_systems: Vec<DiscoveryItem>,
    pub source_control_providers: Vec<DiscoveryItem>,
}

impl Discovery {
    pub fn is_empty(&self) -> bool {
        self.version_control_systems.is_empty() && self.source_control_providers.is_empty()
    }
}

/// The status dot beside an item's mark (`itemStatusDot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    Success,
    Warning,
    Muted,
}

/// An Effect `Option` as JSON; a bare value or null from an older encoder also reads.
fn option_string(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let inner = match value.get("_tag").and_then(Value::as_str) {
        Some("Some") => value.get("value")?,
        Some(_) => return None,
        None => value,
    };
    inner.as_str().map(str::to_owned).filter(|text| !text.is_empty())
}

fn decode_discovered_account(value: &Value) -> Option<DiscoveredAccount> {
    let host = value.get("host")?.as_str()?.trim();
    let account = value.get("account")?.as_str()?.trim();
    if host.is_empty() || account.is_empty() {
        return None;
    }
    Some(DiscoveredAccount {
        host: host.to_owned(),
        account: account.to_owned(),
        authenticated: value.get("authenticated").and_then(Value::as_bool).unwrap_or(false),
        active: value.get("active").and_then(Value::as_bool).unwrap_or(false),
        detail: option_string(value.get("detail")),
        environment_variable: option_string(value.get("environmentVariable")),
    })
}

fn decode_auth(value: &Value) -> ProviderAuth {
    let accounts: Vec<DiscoveredAccount> = value
        .get("accounts")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(decode_discovered_account).collect())
        .unwrap_or_default();
    let any_account_authenticated = accounts.iter().any(|account| account.authenticated);
    ProviderAuth {
        status: match value.get("status").and_then(Value::as_str) {
            Some("authenticated") => AuthStatus::Authenticated,
            Some("unauthenticated") => AuthStatus::Unauthenticated,
            _ => AuthStatus::Unknown,
        },
        account: option_string(value.get("account")),
        detail: option_string(value.get("detail")),
        accounts,
        any_account_authenticated,
    }
}

fn decode_item(value: &Value, provider: bool) -> Option<DiscoveryItem> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(DiscoveryItem {
        kind: text("kind")?,
        label: text("label")?,
        executable: text("executable").filter(|exe| !exe.is_empty()),
        available: value.get("status").and_then(Value::as_str) == Some("available"),
        version: option_string(value.get("version")),
        install_hint: text("installHint").unwrap_or_default(),
        implemented: value.get("implemented").and_then(Value::as_bool).unwrap_or(provider),
        app_sign_in: value.get("appSignIn").and_then(Value::as_bool).unwrap_or(false),
        auth: provider.then(|| value.get("auth").map(decode_auth).unwrap_or_default()),
    })
}

/// Items that fail to decode are skipped alone.
pub fn decode_discovery(value: &Value) -> Discovery {
    let list = |key: &str, provider: bool| {
        value.get(key).and_then(Value::as_array).map(|items| items.iter().filter_map(|item| decode_item(item, provider)).collect()).unwrap_or_default()
    };
    Discovery { version_control_systems: list("versionControlSystems", false), source_control_providers: list("sourceControlProviders", true) }
}

impl DiscoveryItem {
    /// `isVcsNotReady`: a version control system T3 Code cannot drive yet.
    pub fn coming_soon(&self) -> bool {
        self.auth.is_none() && !self.implemented
    }

    /// The disabled switch: available and, for a host, signed in.
    pub fn enabled(&self) -> bool {
        match &self.auth {
            Some(auth) => self.available && auth.status == AuthStatus::Authenticated,
            None => self.available && self.implemented,
        }
    }

    pub fn dot(&self) -> Dot {
        if self.coming_soon() {
            return Dot::Muted;
        }
        match &self.auth {
            Some(_) if self.enabled() => Dot::Success,
            Some(_) => Dot::Muted,
            None if !self.available => Dot::Warning,
            None => Dot::Success,
        }
    }

    /// `authPresentation`'s badge: only the unauthenticated state shows one.
    pub fn auth_badge(&self) -> Option<&'static str> {
        self.auth.as_ref().filter(|auth| auth.status == AuthStatus::Unauthenticated).map(|_| "Not authenticated")
    }

    /// `itemSummary` as text. An authenticated account is left to the view, which hides it.
    pub fn summary(&self) -> String {
        if self.coming_soon() {
            return format!("Support for {} is coming soon.", self.label);
        }
        if !self.available {
            return format!("Not available on this server: {}", self.install_hint);
        }
        let Some(auth) = &self.auth else { return "Available".to_owned() };
        match auth.status {
            AuthStatus::Authenticated => match &auth.detail {
                Some(detail) => format!("Authenticated \u{00b7} {detail}"),
                None => "Authenticated".to_owned(),
            },
            AuthStatus::Unauthenticated if self.executable.is_none() => format!("Available. {}", self.install_hint),
            AuthStatus::Unauthenticated if auth.any_account_authenticated => {
                auth.detail.clone().unwrap_or_else(|| format!("Every {} host is turned off.", self.label))
            }
            AuthStatus::Unauthenticated => format!(
                "{} is not authenticated on this server. Sign in or configure credentials using the {} tool on the server host to enable change request features.",
                self.label,
                self.executable.as_deref().unwrap_or_default()
            ),
            AuthStatus::Unknown => format!("Could not verify {}. {}", self.label, auth.detail.as_deref().unwrap_or(&self.install_hint)),
        }
    }
}

/// `SourceControlSignInPrompt`: returned by `sourceControl.signInStart`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInPrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at: i64,
}

pub fn decode_sign_in_prompt(value: &Value) -> Result<SignInPrompt, String> {
    let user_code = value.get("userCode").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or("Missing sign-in userCode")?;
    let verification_uri = value
        .get("verificationUri")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("Missing sign-in verificationUri")?;
    let expires_at = value.get("expiresAt").and_then(Value::as_i64).unwrap_or(0);
    Ok(SignInPrompt {
        user_code: user_code.to_owned(),
        verification_uri: verification_uri.to_owned(),
        expires_at,
    })
}

/// `SourceControlAppSignIn` from `packages/source-control-github/src/shared/definition.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSignInDef {
    pub app_name: &'static str,
    pub host: &'static str,
    pub install_url: &'static str,
    pub revoke_url: &'static str,
}

/// `SourceControlHostCredentials`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCredentialsDef {
    pub cli_name: &'static str,
    pub cli_label: &'static str,
    pub login_command: &'static str,
    pub token_env_var: &'static str,
    pub token_access: &'static str,
    pub new_token_path: &'static str,
    pub app_sign_in: Option<AppSignInDef>,
}

impl HostCredentialsDef {
    pub fn new_token_url(&self, host: &str) -> String {
        format!("https://{}{}", host.trim(), self.new_token_path)
    }
}

/// One generic setting field on a host without CLI credentials (Bitbucket, GitCafe).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostFieldDef {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub placeholder: &'static str,
    pub secret: bool,
}

/// Built-in host settings definitions matching `packages/source-control-*/src/shared/definition.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostDefinition {
    pub kind: &'static str,
    pub label: &'static str,
    pub public_host: Option<&'static str>,
    pub credentials: Option<HostCredentialsDef>,
    pub fields: &'static [HostFieldDef],
}

const GITHUB_DEF: HostDefinition = HostDefinition {
    kind: "github",
    label: "GitHub",
    public_host: Some("github.com"),
    credentials: Some(HostCredentialsDef {
        cli_name: "gh",
        cli_label: "GitHub CLI",
        login_command: "gh auth login",
        token_env_var: "GH_TOKEN",
        token_access: "read and write access to pull requests and contents",
        new_token_path: "/settings/personal-access-tokens/new",
        app_sign_in: Some(AppSignInDef {
            app_name: "T3 Code for GitHub",
            host: "github.com",
            install_url: "https://github.com/apps/t3-code-for-github/installations/new",
            revoke_url: "https://github.com/settings/apps/authorizations",
        }),
    }),
    fields: &[],
};

const BITBUCKET_FIELDS: [HostFieldDef; 3] = [
    HostFieldDef {
        key: "accessToken",
        label: "Access token",
        description: "Repository, project, or workspace access token used as a Bearer token. Preferred over email + API token when both are set.",
        placeholder: "ATCTT3xFfGN0...",
        secret: true,
    },
    HostFieldDef {
        key: "email",
        label: "Atlassian email",
        description: "Account email for Basic authentication with an Atlassian API token.",
        placeholder: "you@example.com",
        secret: false,
    },
    HostFieldDef {
        key: "apiToken",
        label: "API token",
        description: "Scoped Atlassian API token with Bitbucket pull request scopes, paired with your Atlassian email.",
        placeholder: "ATATT3xFfGF0...",
        secret: true,
    },
];

const BITBUCKET_DEF: HostDefinition = HostDefinition {
    kind: "bitbucket",
    label: "Bitbucket",
    public_host: Some("bitbucket.org"),
    credentials: None,
    fields: &BITBUCKET_FIELDS,
};

const GITCAFE_FIELDS: [HostFieldDef; 1] = [HostFieldDef {
    key: "token",
    label: "API token",
    description: "Personal access token for GitCafe pull request operations.",
    placeholder: "gc_...",
    secret: true,
}];

const GITCAFE_DEF: HostDefinition = HostDefinition {
    kind: "gitcafe",
    label: "GitCafe",
    public_host: Some("gitcafe.net"),
    credentials: None,
    fields: &GITCAFE_FIELDS,
};

pub fn host_definition(kind: &str) -> Option<&'static HostDefinition> {
    match kind {
        "github" => Some(&GITHUB_DEF),
        "bitbucket" => Some(&BITBUCKET_DEF),
        "gitcafe" => Some(&GITCAFE_DEF),
        _ => None,
    }
}

/// One host entry under `sourceControlHosts.<kind>.hosts.<host>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAccountChoice {
    pub enabled: bool,
    pub account: Option<String>,
}

/// Decoded credential settings for a CLI-backed host (`readHostCredentialSettings`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostCredentialSettings {
    pub tokens: BTreeMap<String, SecretValue>,
    pub sign_ins: BTreeMap<String, String>,
    pub hosts: BTreeMap<String, HostAccountChoice>,
}

pub fn has_secret_value(value: Option<&SecretValue>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}

pub fn read_host_credential_settings(source_control_hosts: &Value, kind: &str) -> HostCredentialSettings {
    let Some(entry) = source_control_hosts.get(kind).and_then(Value::as_object) else {
        return HostCredentialSettings::default();
    };
    let mut out = HostCredentialSettings::default();
    if let Some(tokens) = entry.get("tokens").and_then(Value::as_object) {
        for (host, val) in tokens {
            let normalized = host.trim().to_ascii_lowercase();
            if !normalized.is_empty()
                && let Some(secret) = SecretValue::decode(val)
            {
                out.tokens.insert(normalized, secret);
            }
        }
    }
    if let Some(sign_ins) = entry.get("signIns").and_then(Value::as_object) {
        for (host, val) in sign_ins {
            let normalized = host.trim().to_ascii_lowercase();
            if !normalized.is_empty()
                && let Some(token) = val.as_str()
            {
                out.sign_ins.insert(normalized, token.to_owned());
            }
        }
    }
    if let Some(hosts) = entry.get("hosts").and_then(Value::as_object) {
        for (host, val) in hosts {
            let Some(obj) = val.as_object() else { continue };
            let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            let account = obj.get("account").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
            out.hosts.insert(host.clone(), HostAccountChoice { enabled, account });
        }
    }
    out
}

/// Decoded generic fields under `sourceControlHosts.<kind>` (Bitbucket, GitCafe).
pub fn read_host_fields(source_control_hosts: &Value, kind: &str) -> BTreeMap<String, SecretValue> {
    let Some(entry) = source_control_hosts.get(kind).and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    entry
        .iter()
        .filter_map(|(key, value)| SecretValue::decode(value).map(|secret| (key.clone(), secret)))
        .collect()
}

/// `HostAccountGroup` (`SourceControlAccountSettings.logic.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAccountGroup {
    pub host: String,
    pub active_account: Option<String>,
    pub selectable: Vec<String>,
    pub broken: Vec<DiscoveredAccount>,
    pub environment_variable: Option<String>,
}

/// `groupHostAccounts`: preserves host discovery order, separates env-var accounts, selectable
/// authenticated accounts, and broken accounts.
pub fn group_host_accounts(accounts: &[DiscoveredAccount]) -> Vec<HostAccountGroup> {
    let mut hosts = Vec::new();
    for entry in accounts {
        if !hosts.iter().any(|existing: &String| existing == &entry.host) {
            hosts.push(entry.host.clone());
        }
    }
    hosts
        .into_iter()
        .map(|host| {
            let entries: Vec<&DiscoveredAccount> = accounts.iter().filter(|entry| entry.host == host).collect();
            let stored: Vec<&DiscoveredAccount> = entries.iter().copied().filter(|entry| entry.environment_variable.is_none()).collect();
            let selectable_entries: Vec<&DiscoveredAccount> = stored.iter().copied().filter(|entry| entry.authenticated).collect();
            let active_account = selectable_entries
                .iter()
                .find(|entry| entry.active)
                .or_else(|| selectable_entries.first())
                .map(|entry| entry.account.clone());
            let selectable = selectable_entries.iter().map(|entry| entry.account.clone()).collect();
            let broken = stored.into_iter().filter(|entry| !entry.authenticated).cloned().collect();
            let environment_variable = entries.into_iter().find_map(|entry| entry.environment_variable.clone());
            HostAccountGroup { host, active_account, selectable, broken, environment_variable }
        })
        .collect()
}

/// `nextHostAccountChoices`: updates one host's enable / pinned account choice, omitting the host
/// entry when it returns to default (`enabled: true`, `account: None`).
pub fn next_host_account_choices(
    current: &BTreeMap<String, HostAccountChoice>,
    host: &str,
    enabled: Option<bool>,
    account: Option<Option<String>>,
) -> BTreeMap<String, HostAccountChoice> {
    let previous = current.get(host);
    let next_enabled = enabled.unwrap_or_else(|| previous.is_none_or(|choice| choice.enabled));
    let next_account = match account {
        Some(next) => next.filter(|s| !s.trim().is_empty()),
        None => previous.and_then(|choice| choice.account.clone()),
    };
    let mut next = current.clone();
    next.remove(host);
    if !next_enabled || next_account.is_some() {
        next.insert(host.to_owned(), HostAccountChoice { enabled: next_enabled, account: next_account });
    }
    next
}

/// `issueTrackerSummary` (`packages/client-runtime/src/state/issueTrackers.ts`,
/// `pingdotgg/t3code#19274`): describes one standalone issue tracker's connection and linked
/// project count.
pub fn issue_tracker_summary(
    tracker: &crate::issues::wire::IssueTrackerConnection,
    bindings: &std::collections::HashMap<String, crate::issues::wire::IssueTrackerProjectBinding>,
) -> String {
    if !tracker.connected {
        return format!(
            "Not connected. Add a {} to list its issues.",
            tracker.credential.label.to_lowercase()
        );
    }
    let Some(account) = tracker.account.as_deref().filter(|s| !s.is_empty()) else {
        return tracker
            .detail
            .clone()
            .unwrap_or_else(|| "The saved credential was rejected.".to_owned());
    };
    let linked = bindings
        .values()
        .filter(|binding| binding.tracker == tracker.tracker)
        .count();
    if linked == 0 {
        format!(
            "Connected as {account} \u{00b7} no projects linked yet. Pick a {} for a project to list its issues.",
            tracker.scope_noun
        )
    } else if linked == 1 {
        format!("Connected as {account} \u{00b7} 1 project linked")
    } else {
        format!("Connected as {account} \u{00b7} {linked} projects linked")
    }
}

#[cfg(test)]
mod tests;
