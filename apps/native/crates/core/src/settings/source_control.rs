//! Source control at environment scope: `SourceControlSettings.tsx`, the `source-control` rows of
//! `ProjectDefaultsSettings.tsx`, and `SourceControlWritingSettings.tsx`. Discovery decodes
//! `SourceControlDiscoveryResult` (`contracts/src/sourceControl.ts`), whose `Option` fields arrive
//! as `{"_tag":"Some","value":…}` or `{"_tag":"None"}`.

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::{ServerSettingChange, lenient_or};
use crate::model::ModelSelection;
use crate::pull_requests::wire::MergeMethod;
use crate::server_config::ServerSettings;

/// `server.discoverSourceControl`, payload `{}`.
pub const DISCOVER: &str = "server.discoverSourceControl";

/// The merge method select: `last` saves null.
pub const MERGE_METHODS: [(&str, &str); 4] = [("last", "Last selected"), ("merge", "Merge"), ("squash", "Squash and merge"), ("rebase", "Rebase and merge")];

pub fn merge_method_value(method: Option<MergeMethod>) -> &'static str {
    method.map_or("last", MergeMethod::as_str)
}

pub fn parse_merge_method(value: &str) -> Option<Option<MergeMethod>> {
    match value {
        "last" => Some(None),
        "merge" => Some(Some(MergeMethod::Merge)),
        "squash" => Some(Some(MergeMethod::Squash)),
        "rebase" => Some(Some(MergeMethod::Rebase)),
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
            WritingMode::RepoConventions => "In each project, matches recent change descriptions and change request titles.",
            WritingMode::ConventionalCommits => "Use Conventional Commit prefixes and keep change request text concise.",
            WritingMode::Custom => "Use your instructions for change descriptions and change requests in every project.",
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
pub struct ProviderAuth {
    pub status: AuthStatus,
    pub account: Option<String>,
    pub detail: Option<String>,
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

fn decode_auth(value: &Value) -> ProviderAuth {
    ProviderAuth {
        status: match value.get("status").and_then(Value::as_str) {
            Some("authenticated") => AuthStatus::Authenticated,
            Some("unauthenticated") => AuthStatus::Unauthenticated,
            _ => AuthStatus::Unknown,
        },
        account: option_string(value.get("account")),
        detail: option_string(value.get("detail")),
        any_account_authenticated: value
            .get("accounts")
            .and_then(Value::as_array)
            .is_some_and(|accounts| accounts.iter().any(|account| account.get("authenticated").and_then(Value::as_bool) == Some(true))),
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

#[cfg(test)]
mod tests;
