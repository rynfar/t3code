//! The settings experience's pure parts. Server-backed settings the panels edit decode here
//! (flattened into `server_config::ServerSettings`) and save as `ServerSettingsPatch` entries
//! (`contracts/src/settings.ts`) through `server.updateSettings`, the way
//! `web/src/hooks/useSettings.ts` sends the server half of a unified patch. Labels and copy follow
//! `web/src/components/settings/SettingsPanels.tsx` and `ProjectDefaultsSettings.tsx`.

pub mod access;
pub mod archive;
pub mod diagnostics;
pub mod keybindings;
pub mod maintenance;
pub mod project_actions;
pub mod projects;
pub mod providers;
pub mod quit_hold;
pub mod search;
pub mod scheduled_tasks;
pub mod secrets;
pub mod snooze;
pub mod source_control;
pub mod worktrees;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use crate::model::ModelSelection;
use crate::server_config::{RuntimeMode, ServerSettings, ThreadEnvMode};
use snooze::SnoozePreset;

pub const MIN_AUTO_SETTLE_DAYS: u32 = 1;
pub const MAX_AUTO_SETTLE_DAYS: u32 = 90;
/// `DEFAULT_SIDEBAR_AUTO_SETTLE_AFTER_DAYS`, also what the switch turns back on to.
pub const DEFAULT_AUTO_SETTLE_DAYS: u32 = 3;

/// The workspace choices with `resolveEnvModeLabel`'s labels.
pub const ENV_MODES: [(ThreadEnvMode, &str); 2] = [(ThreadEnvMode::Local, "Current checkout"), (ThreadEnvMode::Worktree, "New worktree")];

/// `resolveProjectSettings` at environment scope: an unset mode starts in the checkout.
pub fn effective_env_mode(settings: &ServerSettings) -> ThreadEnvMode {
    settings.default_thread_env_mode.unwrap_or(ThreadEnvMode::Local)
}

/// `ResponseStreamingMode` with its labels and descriptions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseStreamingMode {
    Turn,
    #[default]
    Paragraph,
}

impl ResponseStreamingMode {
    pub const ALL: [ResponseStreamingMode; 2] = [ResponseStreamingMode::Turn, ResponseStreamingMode::Paragraph];

    pub fn label(self) -> &'static str {
        match self {
            ResponseStreamingMode::Turn => "Wait for the full response",
            ResponseStreamingMode::Paragraph => "Show finished paragraphs",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ResponseStreamingMode::Turn => "Text appears once the agent finishes its turn.",
            ResponseStreamingMode::Paragraph => "Each paragraph or code block appears as soon as it is complete.",
        }
    }
}

/// The `ServerSettings` keys the Threads panel edits. Absent or invalid keys take the contract's
/// defaults one at a time, so one bad value never resets the rest.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadSettings {
    /// Null turns inactivity auto-settle off.
    #[serde(deserialize_with = "auto_settle_days")]
    pub sidebar_auto_settle_after_days: Option<u32>,
    #[serde(deserialize_with = "lenient_or")]
    pub sidebar_auto_settle_on_merge: bool,
    #[serde(deserialize_with = "lenient_or")]
    pub sidebar_auto_settle_keep_pull_requests: bool,
    #[serde(deserialize_with = "lenient_or")]
    pub auto_resume_limited_threads: bool,
    #[serde(deserialize_with = "lenient_or")]
    pub snooze_limited_threads: bool,
    #[serde(deserialize_with = "snooze_presets")]
    pub snooze_presets: Vec<SnoozePreset>,
    #[serde(deserialize_with = "lenient_or")]
    pub add_project_base_directory: String,
    #[serde(deserialize_with = "lenient_or")]
    pub response_streaming_mode: ResponseStreamingMode,
}

impl Default for ThreadSettings {
    fn default() -> Self {
        ThreadSettings {
            sidebar_auto_settle_after_days: Some(DEFAULT_AUTO_SETTLE_DAYS),
            sidebar_auto_settle_on_merge: true,
            sidebar_auto_settle_keep_pull_requests: false,
            auto_resume_limited_threads: false,
            snooze_limited_threads: false,
            snooze_presets: snooze::default_presets(),
            add_project_base_directory: String::new(),
            response_streaming_mode: ResponseStreamingMode::default(),
        }
    }
}

/// One `ServerSettingsPatch` entry.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerSettingChange {
    DefaultModelSelection(Option<ModelSelection>),
    FallbackModelSelection(Option<ModelSelection>),
    DefaultRuntimeMode(RuntimeMode),
    DefaultThreadEnvMode(Option<ThreadEnvMode>),
    AutoSettleAfterDays(Option<u32>),
    AutoSettleOnMerge(bool),
    AutoSettleKeepPullRequests(bool),
    AutoResumeLimitedThreads(bool),
    SnoozeLimitedThreads(bool),
    SnoozePresets(Vec<SnoozePreset>),
    AddProjectBaseDirectory(String),
    ResponseStreamingMode(ResponseStreamingMode),
    /// A whole `server.updateSettings` payload a panel built itself: provider instance mutations
    /// and model preferences (`settings::providers`).
    Prepared(Value),
    /// Built from the settings as they stand when the queue sends it (`providers::edit_payload`).
    ProviderInstance { instance_id: String, edit: providers::InstanceEdit },
    /// Project override entries, built when the queue sends them (`projects::override_patch`).
    ProjectOverride { members: Vec<project_actions::Member>, edit: projects::OverrideEdit },
}

impl ServerSettingChange {
    pub fn key(&self) -> &'static str {
        match self {
            ServerSettingChange::DefaultModelSelection(_) => "defaultModelSelection",
            ServerSettingChange::FallbackModelSelection(_) => "fallbackModelSelection",
            ServerSettingChange::DefaultRuntimeMode(_) => "defaultRuntimeMode",
            ServerSettingChange::DefaultThreadEnvMode(_) => "defaultThreadEnvMode",
            ServerSettingChange::AutoSettleAfterDays(_) => "sidebarAutoSettleAfterDays",
            ServerSettingChange::AutoSettleOnMerge(_) => "sidebarAutoSettleOnMerge",
            ServerSettingChange::AutoSettleKeepPullRequests(_) => "sidebarAutoSettleKeepPullRequests",
            ServerSettingChange::AutoResumeLimitedThreads(_) => "autoResumeLimitedThreads",
            ServerSettingChange::SnoozeLimitedThreads(_) => "snoozeLimitedThreads",
            ServerSettingChange::SnoozePresets(_) => "snoozePresets",
            ServerSettingChange::AddProjectBaseDirectory(_) => "addProjectBaseDirectory",
            ServerSettingChange::ResponseStreamingMode(_) => "responseStreamingMode",
            ServerSettingChange::Prepared(_) | ServerSettingChange::ProviderInstance { .. } | ServerSettingChange::ProjectOverride { .. } => "",
        }
    }

    fn value(&self) -> Value {
        match self {
            ServerSettingChange::DefaultModelSelection(selection) | ServerSettingChange::FallbackModelSelection(selection) => {
                json!(selection)
            }
            ServerSettingChange::DefaultRuntimeMode(mode) => json!(mode),
            ServerSettingChange::DefaultThreadEnvMode(mode) => json!(mode),
            ServerSettingChange::AutoSettleAfterDays(days) => {
                json!(days.map(|days| days.clamp(MIN_AUTO_SETTLE_DAYS, MAX_AUTO_SETTLE_DAYS)))
            }
            ServerSettingChange::AutoSettleOnMerge(on)
            | ServerSettingChange::AutoSettleKeepPullRequests(on)
            | ServerSettingChange::AutoResumeLimitedThreads(on)
            | ServerSettingChange::SnoozeLimitedThreads(on) => json!(on),
            ServerSettingChange::SnoozePresets(presets) => json!(presets),
            ServerSettingChange::AddProjectBaseDirectory(path) => json!(path.trim()),
            ServerSettingChange::ResponseStreamingMode(mode) => json!(mode),
            ServerSettingChange::Prepared(payload) => payload.get("patch").cloned().unwrap_or_default(),
            ServerSettingChange::ProviderInstance { .. } | ServerSettingChange::ProjectOverride { .. } => Value::Null,
        }
    }

    /// The payload to send now, given the settings every earlier save left; `None` when there is
    /// nothing to send.
    pub fn payload_for(&self, settings: Option<&ServerSettings>) -> Option<Value> {
        match self {
            ServerSettingChange::ProviderInstance { instance_id, edit } => providers::edit_payload(settings?, instance_id, edit),
            ServerSettingChange::ProjectOverride { members, edit } => Some(projects::override_patch(settings?, members, edit)),
            _ => Some(self.payload()),
        }
    }

    /// The `server.updateSettings` payload of a change that does not depend on current settings.
    pub fn payload(&self) -> Value {
        if let ServerSettingChange::Prepared(payload) = self {
            return payload.clone();
        }
        let mut patch = serde_json::Map::new();
        patch.insert(self.key().to_owned(), self.value());
        json!({ "patch": patch })
    }
}

/// The settings `server.updateSettings` answers with, applied at once rather than waiting for
/// the config stream's echo. `None` when the reply is not a settings object.
pub fn decode_update_reply(reply: Value) -> Option<ServerSettings> {
    if !reply.is_object() {
        return None;
    }
    serde_json::from_value(reply).ok()
}

fn lenient_or<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(d)?).unwrap_or_default())
}

fn auto_settle_days<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let value = Value::deserialize(d)?;
    if value.is_null() {
        return Ok(None);
    }
    let days = value
        .as_f64()
        .filter(|days| (f64::from(MIN_AUTO_SETTLE_DAYS)..=f64::from(MAX_AUTO_SETTLE_DAYS)).contains(days))
        .map(|days| days.round() as u32);
    Ok(Some(days.unwrap_or(DEFAULT_AUTO_SETTLE_DAYS)))
}

fn snooze_presets<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<SnoozePreset>, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(serde_json::from_value::<Vec<SnoozePreset>>(value)
        .ok()
        .filter(|presets| presets.len() <= snooze::MAX_PRESETS && presets.iter().all(SnoozePreset::is_valid))
        .unwrap_or_else(snooze::default_presets))
}

#[cfg(test)]
mod tests;
