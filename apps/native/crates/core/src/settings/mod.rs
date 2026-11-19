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
pub const DEFAULT_AUTO_SETTLE_DAYS: u32 = 7;

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
    pub ghq_clone_layout: bool,
    #[serde(deserialize_with = "lenient_or")]
    pub thread_title_instructions: String,
    #[serde(deserialize_with = "lenient_or")]
    pub response_streaming_mode: ResponseStreamingMode,
    #[serde(deserialize_with = "lenient_or")]
    pub linked_environments_enabled: bool,
    #[serde(deserialize_with = "lenient_or")]
    pub simlock_device_hosts_enabled: bool,
    #[serde(deserialize_with = "lenient_or")]
    pub fast_worktree_checkout: bool,
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
            ghq_clone_layout: false,
            thread_title_instructions: String::new(),
            response_streaming_mode: ResponseStreamingMode::default(),
            linked_environments_enabled: false,
            simlock_device_hosts_enabled: false,
            fast_worktree_checkout: false,
        }
    }
}

/// Reads the saved `textGenerationModelSelection` from the provider panel's raw value without
/// duplicating the flattened serde field on [`ServerSettings`].
pub fn text_generation_model_selection(settings: &ServerSettings) -> Option<ModelSelection> {
    settings.provider_panel.text_generation_model_selection.clone().and_then(|value| serde_json::from_value(value).ok())
}

/// One `ServerSettingsPatch` entry.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerSettingChange {
    DefaultModelSelection(Option<ModelSelection>),
    FallbackModelSelection(Option<ModelSelection>),
    DefaultRuntimeMode(RuntimeMode),
    AgentBrowserAccess(bool),
    DefaultThreadEnvMode(Option<ThreadEnvMode>),
    AutoSettleAfterDays(Option<u32>),
    AutoSettleOnMerge(bool),
    AutoSettleKeepPullRequests(bool),
    AutoResumeLimitedThreads(bool),
    SnoozeLimitedThreads(bool),
    SnoozePresets(Vec<SnoozePreset>),
    AddProjectBaseDirectory(String),
    GhqCloneLayout(bool),
    TextGenerationModelSelection(Option<ModelSelection>),
    ThreadTitleInstructions(String),
    ResponseStreamingMode(ResponseStreamingMode),
    GuestSharingEnabled(bool),
    LinkedEnvironmentsEnabled(bool),
    PromptSuggestionsEnabled(bool),
    SimlockDeviceHostsEnabled(bool),
    FastWorktreeCheckout(bool),
    /// A whole `server.updateSettings` payload a panel built itself: provider instance mutations
    /// and model preferences (`settings::providers`).
    Prepared(Value),
    /// Built from the settings as they stand when the queue sends it (`providers::edit_payload`).
    ProviderInstance { instance_id: String, edit: providers::InstanceEdit },
    /// Project override entries, built when the queue sends them (`projects::override_patch`).
    ProjectOverride { members: Vec<project_actions::Member>, edit: projects::OverrideEdit },
    /// A background activity edit that pins every other resolved value, built when the queue
    /// sends it so an edit still in flight is never undone (`maintenance::BackgroundEdit`).
    BackgroundActivity(maintenance::BackgroundEdit),
}

impl ServerSettingChange {
    pub fn key(&self) -> &'static str {
        match self {
            ServerSettingChange::DefaultModelSelection(_) => "defaultModelSelection",
            ServerSettingChange::FallbackModelSelection(_) => "fallbackModelSelection",
            ServerSettingChange::AgentBrowserAccess(_) => "enableAgentBrowserAccess",
            ServerSettingChange::DefaultRuntimeMode(_) => "defaultRuntimeMode",
            ServerSettingChange::DefaultThreadEnvMode(_) => "defaultThreadEnvMode",
            ServerSettingChange::AutoSettleAfterDays(_) => "sidebarAutoSettleAfterDays",
            ServerSettingChange::AutoSettleOnMerge(_) => "sidebarAutoSettleOnMerge",
            ServerSettingChange::AutoSettleKeepPullRequests(_) => "sidebarAutoSettleKeepPullRequests",
            ServerSettingChange::AutoResumeLimitedThreads(_) => "autoResumeLimitedThreads",
            ServerSettingChange::SnoozeLimitedThreads(_) => "snoozeLimitedThreads",
            ServerSettingChange::SnoozePresets(_) => "snoozePresets",
            ServerSettingChange::AddProjectBaseDirectory(_) => "addProjectBaseDirectory",
            ServerSettingChange::GhqCloneLayout(_) => "ghqCloneLayout",
            ServerSettingChange::TextGenerationModelSelection(_) => "textGenerationModelSelection",
            ServerSettingChange::ThreadTitleInstructions(_) => "threadTitleInstructions",
            ServerSettingChange::ResponseStreamingMode(_) => "responseStreamingMode",
            ServerSettingChange::GuestSharingEnabled(_) => "guestSharingEnabled",
            ServerSettingChange::LinkedEnvironmentsEnabled(_) => "linkedEnvironmentsEnabled",
            ServerSettingChange::PromptSuggestionsEnabled(_) => "promptSuggestionsEnabled",
            ServerSettingChange::SimlockDeviceHostsEnabled(_) => "simlockDeviceHostsEnabled",
            ServerSettingChange::FastWorktreeCheckout(_) => "fastWorktreeCheckout",
            ServerSettingChange::Prepared(_)
            | ServerSettingChange::ProviderInstance { .. }
            | ServerSettingChange::ProjectOverride { .. }
            | ServerSettingChange::BackgroundActivity(_) => "",
        }
    }

    fn value(&self) -> Value {
        match self {
            ServerSettingChange::DefaultModelSelection(selection)
            | ServerSettingChange::FallbackModelSelection(selection)
            | ServerSettingChange::TextGenerationModelSelection(selection) => {
                json!(selection)
            }
            ServerSettingChange::AgentBrowserAccess(on) => json!(on),
            ServerSettingChange::DefaultRuntimeMode(mode) => json!(mode),
            ServerSettingChange::DefaultThreadEnvMode(mode) => json!(mode),
            ServerSettingChange::AutoSettleAfterDays(days) => {
                json!(days.map(|days| days.clamp(MIN_AUTO_SETTLE_DAYS, MAX_AUTO_SETTLE_DAYS)))
            }
            ServerSettingChange::AutoSettleOnMerge(on)
            | ServerSettingChange::AutoSettleKeepPullRequests(on)
            | ServerSettingChange::AutoResumeLimitedThreads(on)
            | ServerSettingChange::SnoozeLimitedThreads(on)
            | ServerSettingChange::GhqCloneLayout(on)
            | ServerSettingChange::GuestSharingEnabled(on)
            | ServerSettingChange::LinkedEnvironmentsEnabled(on)
            | ServerSettingChange::PromptSuggestionsEnabled(on)
            | ServerSettingChange::SimlockDeviceHostsEnabled(on)
            | ServerSettingChange::FastWorktreeCheckout(on) => json!(on),
            ServerSettingChange::SnoozePresets(presets) => json!(presets),
            ServerSettingChange::AddProjectBaseDirectory(path)
            | ServerSettingChange::ThreadTitleInstructions(path) => json!(path.trim()),
            ServerSettingChange::ResponseStreamingMode(mode) => json!(mode),
            ServerSettingChange::Prepared(payload) => payload.get("patch").cloned().unwrap_or_default(),
            ServerSettingChange::ProviderInstance { .. } | ServerSettingChange::ProjectOverride { .. } | ServerSettingChange::BackgroundActivity(_) => {
                Value::Null
            }
        }
    }

    /// The payload to send now, given the settings every earlier save left; `None` when there is
    /// nothing to send.
    pub fn payload_for(&self, settings: Option<&ServerSettings>) -> Option<Value> {
        match self {
            ServerSettingChange::ProviderInstance { instance_id, edit } => providers::edit_payload(settings?, instance_id, edit),
            ServerSettingChange::ProjectOverride { members, edit } => Some(projects::override_patch(settings?, members, edit)),
            ServerSettingChange::BackgroundActivity(edit) => Some(edit.payload(&settings?.provider_panel)),
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

/// Deferred auto-settle days editor (`AutoSettleDaysField` in `SettingsPanels.logic.ts`,
/// `pingdotgg/t3code#36303`): saving a shorter age settles threads at once, so typing stays
/// local until Enter/Blur/close; `pending` tracks the in-flight save and `saved` is the fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoSettleDaysField {
    pub saved: u32,
    pub draft: String,
    pub dirty: bool,
    pub pending: Option<u32>,
    pub revision: u64,
}

impl AutoSettleDaysField {
    pub fn new(saved: u32) -> Self {
        Self { saved, draft: saved.to_string(), dirty: false, pending: None, revision: 0 }
    }

    /// Reconciles with the latest saved server value. Returns `true` if `draft` changed.
    pub fn sync(&mut self, saved: u32) -> bool {
        let next_pending = if self.pending == Some(saved) { None } else { self.pending };
        if self.saved == saved && next_pending == self.pending {
            return false;
        }
        self.saved = saved;
        self.pending = next_pending;
        if !self.dirty {
            self.draft = self.pending.unwrap_or(saved).to_string();
            return true;
        }
        false
    }

    pub fn edit(&mut self, draft: String) {
        self.draft = draft;
        self.dirty = true;
    }

    pub fn cancel(&mut self) -> String {
        self.dirty = false;
        self.draft = self.pending.unwrap_or(self.saved).to_string();
        self.draft.clone()
    }

    /// Validates and commits the draft. Returns `(normalized_draft, Some((days, revision)))` when
    /// a server write should be sent, or `(restored_draft, None)` when nothing should be sent.
    pub fn commit(&mut self) -> (String, Option<(u32, u64)>) {
        let current = self.pending.unwrap_or(self.saved);
        let trimmed = self.draft.trim();
        let parsed = trimmed
            .parse::<u32>()
            .ok()
            .filter(|days| (MIN_AUTO_SETTLE_DAYS..=MAX_AUTO_SETTLE_DAYS).contains(days));
        let Some(days) = parsed else {
            self.dirty = false;
            self.draft = current.to_string();
            return (self.draft.clone(), None);
        };
        let dirty = self.dirty;
        self.dirty = false;
        self.draft = days.to_string();
        if !dirty || self.pending == Some(days) {
            return (self.draft.clone(), None);
        }
        self.pending = Some(days);
        self.revision += 1;
        (self.draft.clone(), Some((days, self.revision)))
    }

    /// Reverts a failed in-flight save if its revision is still current. Returns the text to show
    /// if the input is not dirty and should be updated.
    pub fn fail(&mut self, revision: u64) -> Option<String> {
        if self.revision != revision {
            return None;
        }
        self.pending = None;
        if !self.dirty {
            self.draft = self.saved.to_string();
            return Some(self.draft.clone());
        }
        None
    }
}

/// `ThirdPartyLicenseEntry` (`packages/shared/src/thirdPartyLicenses.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThirdPartyLicenseEntry {
    pub bundles: Vec<String>,
    pub kind: String,
    pub license: String,
    pub name: String,
    pub notice_text: String,
    pub source_url: Option<String>,
    pub version: Option<String>,
}

/// `ThirdPartyLicenseManifest` (`packages/shared/src/thirdPartyLicenses.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThirdPartyLicenseManifest {
    pub entries: Vec<ThirdPartyLicenseEntry>,
}

fn is_http_url(value: &str) -> bool {
    url::Url::parse(value)
        .ok()
        .is_some_and(|parsed| matches!(parsed.scheme(), "http" | "https"))
}

pub fn third_party_license_entry_key(entry: &ThirdPartyLicenseEntry) -> String {
    format!("{}\u{1f}{}\u{1f}{}", entry.kind, entry.name, entry.version.as_deref().unwrap_or(""))
}

/// `decodeThirdPartyLicenseManifest` (`packages/shared/src/thirdPartyLicenses.ts`).
pub fn decode_third_party_license_manifest(value: &Value) -> Result<ThirdPartyLicenseManifest, String> {
    let Some(obj) = value.as_object() else {
        return Err("The open-source license manifest has an unsupported format.".to_owned());
    };
    if obj.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err("The open-source license manifest has an unsupported format.".to_owned());
    }
    let Some(raw_entries) = obj.get("entries").and_then(Value::as_array) else {
        return Err("The open-source license manifest has an unsupported format.".to_owned());
    };
    let mut entries = Vec::with_capacity(raw_entries.len());
    let mut seen = std::collections::HashSet::with_capacity(raw_entries.len());
    for (index, item) in raw_entries.iter().enumerate() {
        let Some(entry) = item.as_object() else {
            return Err(format!("License entry {} is not an object.", index + 1));
        };
        let bundles = entry
            .get("bundles")
            .and_then(Value::as_array)
            .and_then(|arr| arr.iter().map(|v| v.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>())
            .filter(|arr| !arr.is_empty());
        let kind = entry.get("kind").and_then(Value::as_str).filter(|k| matches!(*k, "custom" | "package"));
        let license = entry.get("license").and_then(Value::as_str);
        let name = entry.get("name").and_then(Value::as_str);
        let notice_text = entry.get("noticeText").and_then(Value::as_str);
        let source_url = match entry.get("sourceUrl") {
            Some(Value::Null) => Some(None),
            Some(Value::String(s)) if is_http_url(s) => Some(Some(s.clone())),
            _ => None,
        };
        let version = match entry.get("version") {
            Some(Value::Null) => Some(None),
            Some(Value::String(s)) => Some(Some(s.clone())),
            _ => None,
        };
        let (Some(bundles), Some(kind), Some(license), Some(name), Some(notice_text), Some(source_url), Some(version)) =
            (bundles, kind, license, name, notice_text, source_url, version)
        else {
            return Err(format!("License entry {} has an invalid shape.", index + 1));
        };
        let decoded = ThirdPartyLicenseEntry {
            bundles,
            kind: kind.to_owned(),
            license: license.to_owned(),
            name: name.to_owned(),
            notice_text: notice_text.to_owned(),
            source_url,
            version,
        };
        let key = third_party_license_entry_key(&decoded);
        if !seen.insert(key.clone()) {
            return Err(format!("The open-source license manifest contains a duplicate entry: {key}"));
        }
        entries.push(decoded);
    }
    Ok(ThirdPartyLicenseManifest { entries })
}

/// `filterThirdPartyLicenseEntries` (`packages/shared/src/thirdPartyLicenses.ts`).
pub fn filter_third_party_license_entries<'a>(
    entries: &'a [ThirdPartyLicenseEntry],
    query: &str,
) -> Vec<&'a ThirdPartyLicenseEntry> {
    let terms: Vec<String> = query.split_whitespace().map(|term| term.to_lowercase()).collect();
    if terms.is_empty() {
        return entries.iter().collect();
    }
    entries
        .iter()
        .filter(|entry| {
            let mut searchable = String::new();
            searchable.push_str(&entry.name);
            if let Some(version) = &entry.version {
                searchable.push(' ');
                searchable.push_str(version);
            }
            searchable.push(' ');
            searchable.push_str(&entry.license);
            for bundle in &entry.bundles {
                searchable.push(' ');
                searchable.push_str(bundle);
            }
            let lower = searchable.to_lowercase();
            terms.iter().all(|term| lower.contains(term))
        })
        .collect()
}

/// `formatLicenseBundles` (`packages/shared/src/thirdPartyLicenses.ts`).
pub fn format_license_bundles(bundles: &[String]) -> String {
    bundles
        .iter()
        .map(|bundle| match bundle.as_str() {
            "android" => "Android",
            "assets" => "Assets",
            "desktop" => "Desktop",
            "device-tools" => "Device tools",
            "ios" => "iOS",
            "mobile" => "Mobile",
            "server" => "Server",
            "web" => "Web",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod panel_tests {
    use super::*;

    #[test]
    fn auto_settle_days_field_defers_save_and_handles_cancel_and_failure() {
        let mut field = AutoSettleDaysField::new(7);
        field.edit("3".into());
        assert!(field.dirty);
        assert_eq!(field.pending, None);
        field.edit("30".into());
        let (draft, submit) = field.commit();
        assert_eq!(draft, "30");
        assert_eq!(submit, Some((30, 1)));
        assert_eq!(field.pending, Some(30));

        // Re-committing unchanged pending does not resubmit.
        assert_eq!(field.commit(), ("30".into(), None));

        // Stale echo of old saved value keeps pending.
        assert!(!field.sync(7));
        assert_eq!(field.draft, "30");

        // Failure on matching revision reverts draft to saved.
        assert_eq!(field.fail(1), Some("7".into()));
        assert_eq!(field.draft, "7");
        assert_eq!(field.pending, None);

        // Out-of-range reverts to saved without submitting.
        field.edit("0".into());
        assert_eq!(field.commit(), ("7".into(), None));
        field.edit("91".into());
        assert_eq!(field.commit(), ("7".into(), None));

        // Escape cancels dirty draft back to saved/pending.
        field.edit("14".into());
        assert_eq!(field.cancel(), "7");
        assert!(!field.dirty);
    }

    #[test]
    fn thread_and_labs_server_settings_decode_and_patch() {
        let settings: crate::server_config::ServerSettings = serde_json::from_value(json!({
            "ghqCloneLayout": true,
            "threadTitleInstructions": "Use conventional style",
            "textGenerationModelSelection": { "instanceId": "codex", "model": "gpt-5.4-mini" },
            "linkedEnvironmentsEnabled": true,
            "simlockDeviceHostsEnabled": true,
            "fastWorktreeCheckout": true,
        }))
        .unwrap();
        assert!(settings.threads.ghq_clone_layout);
        assert_eq!(settings.threads.thread_title_instructions, "Use conventional style");
        assert_eq!(
            text_generation_model_selection(&settings).as_ref().map(|s| s.model.as_str()),
            Some("gpt-5.4-mini")
        );
        assert!(settings.threads.linked_environments_enabled);
        assert!(settings.threads.simlock_device_hosts_enabled);
        assert!(settings.threads.fast_worktree_checkout);

        assert_eq!(
            ServerSettingChange::GhqCloneLayout(true).payload(),
            json!({ "patch": { "ghqCloneLayout": true } })
        );
        assert_eq!(
            ServerSettingChange::ThreadTitleInstructions("  concise  ".into()).payload(),
            json!({ "patch": { "threadTitleInstructions": "concise" } })
        );
        assert_eq!(
            ServerSettingChange::FastWorktreeCheckout(true).payload(),
            json!({ "patch": { "fastWorktreeCheckout": true } })
        );
        assert_eq!(
            ServerSettingChange::GuestSharingEnabled(true).payload(),
            json!({ "patch": { "guestSharingEnabled": true } })
        );
        assert_eq!(
            ServerSettingChange::LinkedEnvironmentsEnabled(true).payload(),
            json!({ "patch": { "linkedEnvironmentsEnabled": true } })
        );
        assert_eq!(
            ServerSettingChange::PromptSuggestionsEnabled(true).payload(),
            json!({ "patch": { "promptSuggestionsEnabled": true } })
        );
        assert_eq!(
            ServerSettingChange::SimlockDeviceHostsEnabled(true).payload(),
            json!({ "patch": { "simlockDeviceHostsEnabled": true } })
        );
    }

    #[test]
    fn third_party_license_manifest_decodes_filters_and_rejects_invalid() {
        let raw = json!({
            "schemaVersion": 1,
            "entries": [
                {
                    "bundles": ["web", "device-tools"],
                    "kind": "package",
                    "license": "MIT",
                    "name": "lucide-react",
                    "noticeText": "MIT License",
                    "sourceUrl": "https://github.com/lucide-icons/lucide",
                    "version": "1.0.0"
                },
                {
                    "bundles": ["assets"],
                    "kind": "custom",
                    "license": "Apache-2.0",
                    "name": "Inter",
                    "noticeText": "Apache License",
                    "sourceUrl": null,
                    "version": null
                }
            ]
        });
        let manifest = decode_third_party_license_manifest(&raw).unwrap();
        assert_eq!(manifest.entries.len(), 2);
        assert_eq!(format_license_bundles(&manifest.entries[0].bundles), "Web, Device tools");
        assert_eq!(filter_third_party_license_entries(&manifest.entries, "mit lucide").len(), 1);
        assert_eq!(filter_third_party_license_entries(&manifest.entries, "assets").len(), 1);
        assert_eq!(filter_third_party_license_entries(&manifest.entries, "nonexistent").len(), 0);

        let bad_url = json!({
            "schemaVersion": 1,
            "entries": [{
                "bundles": ["web"],
                "kind": "package",
                "license": "MIT",
                "name": "bad",
                "noticeText": "x",
                "sourceUrl": "javascript:alert(1)",
                "version": null
            }]
        });
        assert!(decode_third_party_license_manifest(&bad_url).is_err());
    }
}
