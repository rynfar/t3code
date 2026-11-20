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
pub mod shared;
pub mod snapshot;
pub mod snooze;
pub mod source_control;
pub mod voice;
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

/// Reads the saved `speechPostProcessingModelSelection` from the provider panel's raw value.
pub fn speech_post_processing_model_selection(settings: &ServerSettings) -> Option<ModelSelection> {
    settings
        .provider_panel
        .speech_post_processing_model_selection
        .clone()
        .and_then(|value| serde_json::from_value(value).ok())
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
    SpeechPostProcessingModelSelection(Option<ModelSelection>),
    SpeechProjectCustomWords(Vec<crate::voice_input::SpeechCustomWordEntry>),
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
            ServerSettingChange::SpeechPostProcessingModelSelection(_) => "speechPostProcessingModelSelection",
            ServerSettingChange::SpeechProjectCustomWords(_) => "speechProjectCustomWords",
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
            | ServerSettingChange::TextGenerationModelSelection(selection)
            | ServerSettingChange::SpeechPostProcessingModelSelection(selection) => {
                json!(selection)
            }
            ServerSettingChange::SpeechProjectCustomWords(words) => json!(words),
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

/// One selectable model row for a text-generation picker (`Threads` / `Voice` cleanup).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextGenerationOption {
    pub instance_id: String,
    pub group_label: String,
    pub model_slug: String,
    pub model_label: String,
    pub is_text_generator: bool,
}

/// Resolves all text-generation model options:
/// - Excludes providers with `supportsTextGeneration === false` and models with
///   `supportsTextGeneration === false`.
/// - Drops provider instances shadowed by a `textGenerators` entry with the same id.
/// - Appends enabled `textGenerators` using their fetched models (plus any currently selected
///   model slugs so an already-selected model stays visible while the list loads).
pub fn text_generation_options(
    config: &crate::server_config::ServerConfig,
    project_id: Option<&str>,
    generator_models: &std::collections::HashMap<String, Vec<providers::text_generators::TextGeneratorModel>>,
    selected_models: &[Option<&ModelSelection>],
) -> Vec<TextGenerationOption> {
    let generators = providers::text_generators::decode_text_generators(&config.settings.text_generators);
    let mut out = Vec::new();
    for entry in crate::providers::entries(config, project_id) {
        if !entry.enabled || !entry.provider.is_available() || !entry.provider.supports_text_generation() {
            continue;
        }
        if generators.contains_key(entry.instance_id()) {
            continue;
        }
        for model in crate::providers::model_options(&config.settings, entry.provider) {
            if !model.supports_text_generation() {
                continue;
            }
            out.push(TextGenerationOption {
                instance_id: entry.instance_id().to_owned(),
                group_label: entry.display_name.clone(),
                model_slug: model.slug.clone(),
                model_label: crate::providers::model_name(model).to_owned(),
                is_text_generator: false,
            });
        }
    }

    for (id, gen_cfg) in &generators {
        if !gen_cfg.enabled {
            continue;
        }
        let group_label = gen_cfg.display_name();
        let discovered = generator_models.get(id).map(Vec::as_slice).unwrap_or(&[]);
        let mut seen = std::collections::HashSet::new();
        for model in discovered {
            if seen.insert(model.id.clone()) {
                out.push(TextGenerationOption {
                    instance_id: id.clone(),
                    group_label: group_label.clone(),
                    model_slug: model.id.clone(),
                    model_label: if model.name.trim().is_empty() { model.id.clone() } else { model.name.clone() },
                    is_text_generator: true,
                });
            }
        }
        for sel in selected_models.iter().flatten() {
            if sel.instance_id == *id && !sel.model.is_empty() && seen.insert(sel.model.clone()) {
                out.push(TextGenerationOption {
                    instance_id: id.clone(),
                    group_label: group_label.clone(),
                    model_slug: sel.model.clone(),
                    model_label: sel.model.clone(),
                    is_text_generator: true,
                });
            }
        }
    }
    out
}

/// Resolves whether `selection` is currently enabled for text generation (either an enabled,
/// text-gen-capable provider instance not shadowed by a text generator, or an enabled text
/// generator).
pub fn is_text_generation_selection_valid(
    config: &crate::server_config::ServerConfig,
    project_id: Option<&str>,
    selection: &ModelSelection,
) -> bool {
    let generators = providers::text_generators::decode_text_generators(&config.settings.text_generators);
    if let Some(gen_cfg) = generators.get(&selection.instance_id) {
        return gen_cfg.enabled;
    }
    config
        .provider(&selection.instance_id)
        .filter(|provider| {
            provider.is_available()
                && provider.supports_text_generation()
                && crate::providers::instance_enabled(&config.settings, project_id, &provider.instance_id)
        })
        .is_some_and(|provider| {
            provider
                .models
                .iter()
                .find(|model| model.slug == selection.model)
                .is_none_or(|model| model.supports_text_generation())
        })
}

/// Resolves the best fallback text-generation selection when none is explicitly saved or the
/// saved one is disabled/unsupported.
pub fn resolve_default_text_generation_selection(
    config: &crate::server_config::ServerConfig,
    project_id: Option<&str>,
    generator_models: &std::collections::HashMap<String, Vec<providers::text_generators::TextGeneratorModel>>,
) -> Option<ModelSelection> {
    if let Some(saved) = text_generation_model_selection(&config.settings)
        && is_text_generation_selection_valid(config, project_id, &saved)
    {
        return Some(saved);
    }
    if let Some(default) = config.settings.default_model_selection.as_ref()
        && is_text_generation_selection_valid(config, project_id, default)
    {
        return Some(default.clone());
    }
    let options = text_generation_options(config, project_id, generator_models, &[]);
    options.first().map(|opt| ModelSelection {
        instance_id: opt.instance_id.clone(),
        model: opt.model_slug.clone(),
        options: None,
    })
}

/// Display label for a text-generation selection across both providers and text generators.
pub fn text_generation_selection_label(
    config: &crate::server_config::ServerConfig,
    selection: &ModelSelection,
    generator_models: &std::collections::HashMap<String, Vec<providers::text_generators::TextGeneratorModel>>,
) -> String {
    let generators = providers::text_generators::decode_text_generators(&config.settings.text_generators);
    if generators.contains_key(&selection.instance_id) {
        if let Some(model) = generator_models
            .get(&selection.instance_id)
            .and_then(|models| models.iter().find(|m| m.id == selection.model))
            && !model.name.trim().is_empty()
        {
            return model.name.clone();
        }
        return selection.model.clone();
    }
    config
        .provider(&selection.instance_id)
        .and_then(|provider| provider.models.iter().find(|model| model.slug == selection.model))
        .map(|model| crate::providers::trigger_model_name(model).to_owned())
        .unwrap_or_else(|| selection.model.clone())
}

// ---------------------------------------------------------------------------
// Settings scope axis & scoped model availability (`SettingsScopeSentence.tsx`,
// `settingsScopeAxis.ts`, `useScopedModelAvailability.ts`)
// ---------------------------------------------------------------------------

pub const ALL_ENVIRONMENTS_VALUE: &str = "all";
pub const ALL_PROJECTS_VALUE: &str = "all";
pub const ALL_CHECKOUTS_VALUE: &str = "all";

/// `SETTINGS_DEVICE_ONLY_PATHS` (`SettingsScopeSentence.tsx`).
pub fn is_device_only_section(section: search::Section) -> bool {
    matches!(
        section,
        search::Section::General
            | search::Section::Appearance
            | search::Section::Chat
            | search::Section::Diffs
            | search::Section::SnapShot
            | search::Section::Connections
            | search::Section::Access
            | search::Section::About
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsScopeSearch {
    pub project: Option<String>,
    pub machine: Option<String>,
    pub checkout: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedScopeKind {
    All,
    Environment,
    Project,
    Checkout,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeEnvironment {
    pub environment_id: String,
    pub label: String,
    pub display_url: String,
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeCheckout {
    pub environment_id: String,
    pub physical_project_key: String,
    pub workspace_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeProjectGroup {
    pub project_key: String,
    pub display_name: String,
    pub workspace_root: String,
    pub archived: bool,
    pub members: Vec<ScopeCheckout>,
}

pub fn settings_scope_environment_address(env: &ScopeEnvironment) -> String {
    if env.display_url.is_empty() {
        return env.environment_id.clone();
    }
    url::Url::parse(&env.display_url)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?;
            Some(match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_owned(),
            })
        })
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| env.display_url.clone())
}

pub fn settings_scope_environment_labels(
    env: &ScopeEnvironment,
    all: &[ScopeEnvironment],
) -> (String, String) {
    let duplicates: Vec<&ScopeEnvironment> = all
        .iter()
        .filter(|other| other.environment_id != env.environment_id && other.label == env.label)
        .collect();
    if duplicates.is_empty() {
        return (env.label.clone(), String::new());
    }
    let address = settings_scope_environment_address(env);
    let secondary = if duplicates.iter().any(|other| settings_scope_environment_address(other) == address) {
        if !env.display_url.is_empty() { env.display_url.clone() } else { env.environment_id.clone() }
    } else {
        address
    };
    (env.label.clone(), secondary)
}

pub fn settings_scope_environment_label(env: &ScopeEnvironment, all: &[ScopeEnvironment]) -> String {
    let (primary, secondary) = settings_scope_environment_labels(env, all);
    if secondary.is_empty() { primary } else { format!("{primary} \u{00b7} {secondary}") }
}

pub fn select_environment_axis(search: &SettingsScopeSearch, value: &str) -> SettingsScopeSearch {
    SettingsScopeSearch {
        project: search.project.clone(),
        machine: (value != ALL_ENVIRONMENTS_VALUE).then(|| value.to_owned()),
        checkout: None,
    }
}

pub fn select_project_axis(search: &SettingsScopeSearch, value: &str) -> SettingsScopeSearch {
    SettingsScopeSearch {
        project: (value != ALL_PROJECTS_VALUE).then(|| value.to_owned()),
        machine: search.machine.clone(),
        checkout: None,
    }
}

pub fn select_checkout_axis(search: &SettingsScopeSearch, checkout: Option<&ScopeCheckout>) -> SettingsScopeSearch {
    match checkout {
        Some(member) => SettingsScopeSearch {
            project: search.project.clone(),
            machine: Some(member.environment_id.clone()),
            checkout: Some(member.physical_project_key.clone()),
        },
        None => SettingsScopeSearch {
            project: search.project.clone(),
            machine: search.machine.clone(),
            checkout: None,
        },
    }
}

pub fn project_scope_menu_groups<'a>(
    groups: &'a [ScopeProjectGroup],
    machine: Option<&str>,
) -> Vec<&'a ScopeProjectGroup> {
    match machine {
        None => groups.iter().collect(),
        Some(env_id) => groups
            .iter()
            .filter(|group| group.members.iter().any(|member| member.environment_id == env_id))
            .collect(),
    }
}

pub fn settings_scope_checkout_choices<'a>(
    group: &'a ScopeProjectGroup,
    search: &SettingsScopeSearch,
) -> Vec<&'a ScopeCheckout> {
    let selected_env = search.checkout.as_deref().and_then(|key| {
        group
            .members
            .iter()
            .find(|m| m.physical_project_key == key)
            .map(|m| m.environment_id.as_str())
    });
    let env_id = search.machine.as_deref().or(selected_env);
    let members: Vec<&ScopeCheckout> = match env_id {
        None => group.members.iter().collect(),
        Some(id) => group.members.iter().filter(|m| m.environment_id == id).collect(),
    };
    if members.len() > 1
        && members.iter().all(|m| m.environment_id == members[0].environment_id)
    {
        members
    } else {
        Vec::new()
    }
}

fn path_basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
}

pub fn settings_scope_checkout_label(checkout: &ScopeCheckout, all: &[&ScopeCheckout]) -> String {
    let name = path_basename(&checkout.workspace_root);
    let count = all.iter().filter(|other| path_basename(&other.workspace_root) == name).count();
    if count > 1 { checkout.workspace_root.clone() } else { name.to_owned() }
}

/// `PROJECT_SCOPED_SERVER_SETTING_KEYS` (`packages/contracts/src/settings.ts:1171-1199`).
pub const PROJECT_SCOPED_SERVER_SETTING_KEYS: &[&str] = &[
    "worktreeCleanup",
    "defaultModelSelection",
    "defaultRuntimeMode",
    "defaultThreadEnvMode",
    "newWorktreesStartFromOrigin",
    "worktreeSubmodules",
    "defaultAutoPull",
    "defaultProjectScripts",
    "enableAgentBrowserAccess",
    "enableAgentDeviceAccess",
    "textGenerationModelSelection",
    "threadTitleInstructions",
    "sourceControlWriterModelSelection",
    "reviewGuideModelSelection",
    "reviewGuideInstructions",
    "sourceControlWritingStyle",
    "removeAgentCreditsOnMerge",
    "branchNamingMode",
    "branchNamePrefix",
    "branchNameInstructions",
    "pullRequestMergeMethod",
    "sidebarAutoSettleOnMerge",
    "sidebarAutoSettleAfterDays",
    "sidebarAutoSettleKeepPullRequests",
    "continueThreadsAfterServerUpdate",
    "responseStreamingMode",
    "speechProjectCustomWords",
];

pub fn is_project_scoped_setting_key(key: &str) -> bool {
    PROJECT_SCOPED_SERVER_SETTING_KEYS.contains(&key)
}

/// Resolves effective `ServerSettings` for `project_id` by overlaying any explicit entries in
/// `projectSettingsOverrides[project_id]` onto the environment's `ServerSettings`
/// (`resolveProjectSettings` in `packages/shared/src/projectSettings.ts`).
pub fn resolve_project_settings(base: &ServerSettings, project_id: &str) -> ServerSettings {
    let mut resolved = base.clone();
    let Some(overrides) = base.project_settings_overrides.get(project_id) else {
        return resolved;
    };
    let raw = &overrides.raw;
    if let Some(val) = raw.get("defaultModelSelection") {
        resolved.default_model_selection =
            if val.is_null() { None } else { serde_json::from_value(val.clone()).ok() };
    }
    if let Some(val) = raw.get("defaultRuntimeMode")
        && let Ok(mode) = serde_json::from_value(val.clone())
    {
        resolved.default_runtime_mode = mode;
    }
    if let Some(val) = raw.get("defaultThreadEnvMode") {
        resolved.default_thread_env_mode =
            if val.is_null() { None } else { serde_json::from_value(val.clone()).ok() };
    }
    if let Some(val) = raw.get("newWorktreesStartFromOrigin")
        && let Some(flag) = val.as_bool()
    {
        resolved.new_worktrees_start_from_origin = Some(flag);
    }
    if let Some(val) = raw.get("worktreeSubmodules") {
        resolved.worktrees.worktree_submodules =
            val.as_str().and_then(worktrees::Submodules::parse);
    }
    if let Some(val) = raw.get("worktreeCleanup") {
        resolved.worktrees.worktree_cleanup = if val.is_null() {
            None
        } else {
            serde_json::from_value::<worktrees::WorktreeSettings>(json!({ "worktreeCleanup": val }))
                .ok()
                .and_then(|parsed| parsed.worktree_cleanup)
        };
    }
    if let Some(val) = raw.get("branchNamingMode")
        && let Some(mode) = val.as_str().and_then(worktrees::BranchNaming::parse)
    {
        resolved.worktrees.branch_naming_mode = mode;
    }
    if let Some(val) = raw.get("branchNamePrefix")
        && let Some(text) = val.as_str()
    {
        resolved.worktrees.branch_name_prefix = text.trim().to_owned();
    }
    if let Some(val) = raw.get("branchNameInstructions")
        && let Some(text) = val.as_str()
    {
        resolved.worktrees.branch_name_instructions = text.trim().to_owned();
    }
    if let Some(val) = raw.get("defaultAutoPull")
        && let Some(flag) = val.as_bool()
    {
        resolved.source_control.default_auto_pull = flag;
    }
    if let Some(val) = raw.get("removeAgentCreditsOnMerge")
        && let Some(flag) = val.as_bool()
    {
        resolved.source_control.remove_agent_credits_on_merge = flag;
    }
    if let Some(val) = raw.get("sourceControlWritingStyle")
        && let Ok(parsed) = serde_json::from_value::<source_control::SourceControlSettings>(
            json!({ "sourceControlWritingStyle": val }),
        )
    {
        resolved.source_control.source_control_writing_style = parsed.source_control_writing_style;
    }
    if let Some(val) = raw.get("sourceControlWriterModelSelection") {
        resolved.source_control.source_control_writer_model_selection =
            if val.is_null() { None } else { serde_json::from_value(val.clone()).ok() };
    }
    if let Some(val) = raw.get("reviewGuideModelSelection") {
        resolved.source_control.review_guide_model_selection =
            if val.is_null() { None } else { serde_json::from_value(val.clone()).ok() };
    }
    if let Some(val) = raw.get("reviewGuideInstructions")
        && let Some(text) = val.as_str()
    {
        resolved.source_control.review_guide_instructions = text.trim().to_owned();
    }
    if let Some(val) = raw.get("pullRequestMergeMethod") {
        resolved.pull_request_merge_method =
            if val.is_null() { None } else { serde_json::from_value(val.clone()).ok() };
    }
    if let Some(val) = raw.get("defaultProjectScripts")
        && let Ok(scripts) = serde_json::from_value(val.clone())
    {
        resolved.default_project_scripts = scripts;
    }
    if let Some(val) = raw.get("enableAgentBrowserAccess")
        && let Some(flag) = val.as_bool()
    {
        resolved.enable_agent_browser_access = Some(flag);
    }
    if let Some(val) = raw.get("textGenerationModelSelection") {
        resolved.provider_panel.text_generation_model_selection =
            if val.is_null() { None } else { Some(val.clone()) };
    }
    if let Some(val) = raw.get("threadTitleInstructions")
        && let Some(text) = val.as_str()
    {
        resolved.threads.thread_title_instructions = text.trim().to_owned();
    }
    if let Some(val) = raw.get("sidebarAutoSettleOnMerge")
        && let Some(flag) = val.as_bool()
    {
        resolved.threads.sidebar_auto_settle_on_merge = flag;
    }
    if let Some(val) = raw.get("sidebarAutoSettleAfterDays") {
        resolved.threads.sidebar_auto_settle_after_days = if val.is_null() {
            None
        } else {
            val.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|days| (1..=90).contains(days))
                .or(resolved.threads.sidebar_auto_settle_after_days)
        };
    }
    if let Some(val) = raw.get("sidebarAutoSettleKeepPullRequests")
        && let Some(flag) = val.as_bool()
    {
        resolved.threads.sidebar_auto_settle_keep_pull_requests = flag;
    }
    if let Some(val) = raw.get("continueThreadsAfterServerUpdate")
        && let Some(flag) = val.as_bool()
    {
        resolved.maintenance.continue_threads_after_server_update = flag;
    }
    if let Some(val) = raw.get("responseStreamingMode")
        && let Ok(mode) = serde_json::from_value(val.clone())
    {
        resolved.threads.response_streaming_mode = mode;
    }
    if let Some(val) = raw.get("speechProjectCustomWords")
        && let Ok(words) = serde_json::from_value(val.clone())
    {
        resolved.speech_project_custom_words = words;
    }
    resolved
}

/// Whether `project_id` has an explicit project-scoped override stored for any key in `keys`.
pub fn project_override_present(settings: &ServerSettings, project_id: &str, keys: &[&str]) -> bool {
    settings
        .project_settings_overrides
        .get(project_id)
        .is_some_and(|entry| keys.iter().any(|key| entry.raw.contains_key(*key)))
}

/// Effective `enableAgentDeviceAccess` for a project scope (override when present, else host service state).
pub fn effective_agent_device_access(
    settings: &ServerSettings,
    project_id: Option<&str>,
    service_enabled: bool,
) -> bool {
    project_id
        .and_then(|id| settings.project_settings_overrides.get(id))
        .and_then(|entry| entry.raw.get("enableAgentDeviceAccess"))
        .and_then(Value::as_bool)
        .unwrap_or(service_enabled)
}

/// Effective enabled state of a provider instance under an optional project scope.
pub fn effective_provider_enabled(
    settings: &ServerSettings,
    project_id: Option<&str>,
    instance_id: &str,
    base_enabled: bool,
) -> bool {
    project_id
        .and_then(|id| project_provider_instance_override(settings, id, instance_id))
        .unwrap_or(base_enabled)
}

/// Explicit project-scoped override value for `providerInstances[instanceId].enabled`, if any.
pub fn project_provider_instance_override(
    settings: &ServerSettings,
    project_id: &str,
    instance_id: &str,
) -> Option<bool> {
    settings
        .project_settings_overrides
        .get(project_id)
        .and_then(|entry| entry.provider_instances.get(instance_id))
        .and_then(|inst| inst.enabled)
}

/// Converts a server setting change into a project-override edit when every key in its patch is
/// project-scoped (`planScopedSettingsPatch` in `scopedSettings.ts`). Returns `None` for
/// environment-only settings that cannot be overridden per project.
pub fn server_change_to_override_edit(change: &ServerSettingChange) -> Option<projects::OverrideEdit> {
    match change {
        ServerSettingChange::ProviderInstance { .. }
        | ServerSettingChange::ProjectOverride { .. }
        | ServerSettingChange::BackgroundActivity(_) => None,
        ServerSettingChange::Prepared(payload) => {
            let patch = payload.get("patch")?.as_object()?;
            if patch.is_empty() || !patch.keys().all(|k| is_project_scoped_setting_key(k)) {
                return None;
            }
            if patch.len() == 1 {
                let (key, value) = patch.iter().next()?;
                Some(projects::OverrideEdit::Set { key: key.clone(), value: value.clone() })
            } else {
                Some(projects::OverrideEdit::SetMap(patch.clone()))
            }
        }
        other => {
            let key = other.key();
            if key.is_empty() || !is_project_scoped_setting_key(key) {
                return None;
            }
            Some(projects::OverrideEdit::Set { key: key.to_owned(), value: other.value() })
        }
    }
}

/// Checks whether `(instance_id, model)` is available across every target environment in scope
/// (`useScopedModelDisabledReason` in `useScopedModelAvailability.ts`).
pub fn scoped_model_disabled_reason(
    shown_settings: &ServerSettings,
    shown_driver: Option<&str>,
    targets: &[(&str, &crate::server_config::ServerConfig, Option<&str>)],
    instance_id: &str,
    model: &str,
    text_generation: bool,
) -> Option<String> {
    let shown_generators = providers::text_generators::decode_text_generators(&shown_settings.text_generators);
    let shown_gen = shown_generators.get(instance_id);
    for (env_label, config, project_id) in targets {
        let unavailable = format!(
            "This model is unavailable on {env_label}. Select that environment to choose its model separately."
        );
        let candidate_generators =
            providers::text_generators::decode_text_generators(&config.settings.text_generators);
        let candidate_gen = candidate_generators.get(instance_id);
        if shown_gen.is_some() || candidate_gen.is_some() {
            let matches = match (shown_gen, candidate_gen) {
                (Some(s), Some(c)) => c.enabled && c.kind == s.kind && c.base_url == s.base_url,
                _ => false,
            };
            if !matches {
                return Some(unavailable);
            }
            continue;
        }
        let entry = crate::providers::entries(config, *project_id)
            .into_iter()
            .find(|entry| entry.instance_id() == instance_id);
        let Some(entry) = entry else {
            return Some(unavailable);
        };
        if !entry.enabled || !entry.provider.is_available() {
            return Some(unavailable);
        }
        if let Some(driver) = shown_driver
            && entry.driver() != driver
        {
            return Some(unavailable);
        }
        if text_generation && !entry.provider.supports_text_generation() {
            return Some(unavailable);
        }
        let has_model = crate::providers::model_options(&config.settings, entry.provider)
            .into_iter()
            .any(|candidate| {
                candidate.slug == model && (!text_generation || candidate.supports_text_generation())
            });
        if !has_model {
            return Some(unavailable);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// ST-ENVS: Linked environments, Load balancing, Pull request sharing
// (`LinkedEnvironmentsSettings.tsx`, `LoadBalancingSettings.tsx`,
// `PullRequestRoutingSettings.tsx`, `peerLinks.ts`, `pullRequestRoutingPermissions.ts`)
// ---------------------------------------------------------------------------

pub const PEER_LINKS_LIST_METHOD: &str = "peerLinks.list";
pub const PEER_LINKS_LINK_METHOD: &str = "peerLinks.link";
pub const PEER_LINKS_UNLINK_METHOD: &str = "peerLinks.unlink";
pub const PEER_LINK_EXPIRY_WARNING_DAYS: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerLinkStatus {
    Reachable,
    Unreachable,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerLinkSummary {
    pub environment_id: String,
    pub label: String,
    #[serde(default)]
    pub urls: Vec<String>,
    pub access: String,
    #[serde(default)]
    pub linked_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub last_reached_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    pub status: PeerLinkStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerLinkHealth {
    Reachable { expires_in_days: Option<i64> },
    Unreachable { detail: Option<String> },
    Failing { detail: String },
    Expired,
}

/// `peerLinkHealth` (`packages/client-runtime/src/state/peerLinks.ts`).
pub fn peer_link_health(link: &PeerLinkSummary, now_ms: i64) -> PeerLinkHealth {
    let expires_ms = crate::sidebar::time::parse_iso(&link.expires_at);
    if link.status == PeerLinkStatus::Expired || expires_ms.is_some_and(|exp| exp <= now_ms) {
        return PeerLinkHealth::Expired;
    }
    if link.status == PeerLinkStatus::Unreachable {
        return PeerLinkHealth::Unreachable { detail: link.last_error.clone() };
    }
    if let Some(detail) = link.last_error.clone() {
        return PeerLinkHealth::Failing { detail };
    }
    let expires_in_days = expires_ms.and_then(|exp| {
        let diff_ms = exp.saturating_sub(now_ms);
        let day_ms: i64 = 86_400_000;
        let days = (diff_ms + day_ms - 1) / day_ms;
        (days <= PEER_LINK_EXPIRY_WARNING_DAYS).then_some(days.max(1))
    });
    PeerLinkHealth::Reachable { expires_in_days }
}

pub fn peer_link_status_text(health: &PeerLinkHealth) -> String {
    match health {
        PeerLinkHealth::Expired => "Expired. Link it again with a new pairing code.".to_owned(),
        PeerLinkHealth::Unreachable { detail } => {
            detail.clone().unwrap_or_else(|| "Not answering right now.".to_owned())
        }
        PeerLinkHealth::Failing { detail } => detail.clone(),
        PeerLinkHealth::Reachable { expires_in_days: Some(days) } => {
            let unit = if *days == 1 { "day" } else { "days" };
            format!("Expires in {days} {unit}. Link it again to renew.")
        }
        PeerLinkHealth::Reachable { expires_in_days: None } => "Reachable".to_owned(),
    }
}

/// `mcpClientScopes` (`packages/contracts/src/auth.ts`).
pub fn mcp_client_scopes(access: &str) -> Vec<&'static str> {
    match access {
        "read-only" => vec![crate::auth::ORCHESTRATION_READ_SCOPE],
        "full-access" => vec![
            crate::auth::ORCHESTRATION_READ_SCOPE,
            crate::auth::ORCHESTRATION_OPERATE_SCOPE,
            "settings:write",
            "source-control:write",
        ],
        _ => vec![crate::auth::ORCHESTRATION_READ_SCOPE, crate::auth::ORCHESTRATION_OPERATE_SCOPE],
    }
}

pub fn peer_link_create_payload(
    url: &str,
    alternate_urls: &[String],
    pairing_code: &str,
    access: &str,
    expected_environment_id: Option<&str>,
    expected_label: Option<&str>,
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("url".to_owned(), json!(url.trim()));
    if !alternate_urls.is_empty() {
        obj.insert("alternateUrls".to_owned(), json!(alternate_urls));
    }
    obj.insert("pairingCode".to_owned(), json!(pairing_code.trim()));
    obj.insert("access".to_owned(), json!(access));
    if let Some(id) = expected_environment_id {
        obj.insert("expectedEnvironmentId".to_owned(), json!(id));
    }
    if let Some(label) = expected_label.filter(|l| !l.trim().is_empty()) {
        obj.insert("expectedLabel".to_owned(), json!(label.trim()));
    }
    Value::Object(obj)
}

pub fn peer_link_unlink_payload(environment_id: &str) -> Value {
    json!({ "environmentId": environment_id })
}

/// `LoadPreference` (`LoadBalancingSettings.tsx`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LoadPreference {
    Prefer = 100,
    #[default]
    Normal = 50,
    LessOften = 25,
    ManualOnly = 0,
}

impl LoadPreference {
    pub const ALL: &'static [Self] = &[Self::Prefer, Self::Normal, Self::LessOften, Self::ManualOnly];

    pub const fn weight(self) -> u32 {
        self as u32
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prefer => "100",
            Self::Normal => "50",
            Self::LessOften => "25",
            Self::ManualOnly => "0",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "100" => Some(Self::Prefer),
            "50" => Some(Self::Normal),
            "25" => Some(Self::LessOften),
            "0" => Some(Self::ManualOnly),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Prefer => "Prefer",
            Self::Normal => "Normal",
            Self::LessOften => "Less often",
            Self::ManualOnly => "Manual only",
        }
    }
}

pub fn load_preference_for_weight(weight: Option<u32>) -> LoadPreference {
    match weight {
        None | Some(50) => LoadPreference::Normal,
        Some(0) => LoadPreference::ManualOnly,
        Some(w) if w < 50 => LoadPreference::LessOften,
        Some(_) => LoadPreference::Prefer,
    }
}

pub fn summarize_load_preferences(
    environments: &[(&str, &str)],
    weights: &std::collections::BTreeMap<String, u32>,
) -> Option<String> {
    let parts: Vec<String> = environments
        .iter()
        .filter_map(|(id, label)| {
            let pref = load_preference_for_weight(weights.get(*id).copied());
            (pref != LoadPreference::Normal).then(|| format!("{label} {}", pref.label().to_lowercase()))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" \u{00b7} "))
}

pub fn read_load_balancing_enabled(doc: &crate::client_settings::ClientSettingsDoc) -> bool {
    doc.entries().get("loadBalancingEnabled").and_then(Value::as_bool).unwrap_or(false)
}

pub fn read_load_balancing_weights(
    doc: &crate::client_settings::ClientSettingsDoc,
) -> std::collections::BTreeMap<String, u32> {
    doc.entries()
        .get("loadBalancingWeights")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_u64().and_then(|n| u32::try_from(n).ok()).map(|w| (k.clone(), w)))
                .collect()
        })
        .unwrap_or_default()
}

pub fn load_balancing_enabled_patch(enabled: bool) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    map.insert("loadBalancingEnabled".to_owned(), Value::Bool(enabled));
    map
}

pub fn load_balancing_weight_patch(
    current: &std::collections::BTreeMap<String, u32>,
    environment_id: &str,
    preference: LoadPreference,
) -> serde_json::Map<String, Value> {
    let mut next = current.clone();
    next.insert(environment_id.to_owned(), preference.weight());
    let mut map = serde_json::Map::new();
    map.insert("loadBalancingWeights".to_owned(), json!(next));
    map
}

/// `PullRequestRoutingPermission` (`pullRequestRoutingPermissions.ts`, `PullRequestRoutingSettings.tsx`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestRoutingPermission {
    #[default]
    Off,
    Read,
    ReadWrite,
}

impl PullRequestRoutingPermission {
    pub const ALL: &'static [Self] = &[Self::Off, Self::Read, Self::ReadWrite];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Read => "read",
            Self::ReadWrite => "read-write",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "read" => Some(Self::Read),
            "read-write" => Some(Self::ReadWrite),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Read => "Read PRs",
            Self::ReadWrite => "Read and act",
        }
    }

    pub const fn summary_label(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Read => Some("read PRs"),
            Self::ReadWrite => Some("read and act"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredPullRequestRoutingPermission {
    pub environment_id: String,
    pub connection_key: String,
    pub permission: PullRequestRoutingPermission,
}

/// Derives a trust-bound connection key from saved endpoint details so changing a server's
/// URL or SSH target automatically revokes trust (`pullRequestRoutingConnectionKey`).
pub fn pull_request_routing_connection_key(
    environment_id: &str,
    http_base: &str,
    ssh_target: Option<(&str, &str, Option<&str>, Option<u16>)>,
) -> Option<String> {
    if let Some((alias, hostname, username, port)) = ssh_target {
        return Some(json!(["SshConnectionTarget", environment_id, alias, hostname, username, port]).to_string());
    }
    let parsed = url::Url::parse(http_base.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    let normalized = parsed.as_str().trim_end_matches('/');
    Some(json!(["BearerConnectionTarget", environment_id, normalized]).to_string())
}

pub fn pull_request_routing_permission_for(
    connection_key: Option<&str>,
    permissions: &[StoredPullRequestRoutingPermission],
) -> PullRequestRoutingPermission {
    let Some(key) = connection_key else {
        return PullRequestRoutingPermission::Off;
    };
    permissions
        .iter()
        .find(|entry| entry.connection_key == key)
        .map(|entry| entry.permission)
        .unwrap_or(PullRequestRoutingPermission::Off)
}

pub fn set_pull_request_routing_permission(
    permissions: &[StoredPullRequestRoutingPermission],
    environment_id: &str,
    connection_key: &str,
    permission: PullRequestRoutingPermission,
) -> Vec<StoredPullRequestRoutingPermission> {
    let mut next: Vec<StoredPullRequestRoutingPermission> = permissions
        .iter()
        .filter(|entry| entry.environment_id != environment_id)
        .cloned()
        .collect();
    if permission != PullRequestRoutingPermission::Off {
        next.push(StoredPullRequestRoutingPermission {
            environment_id: environment_id.to_owned(),
            connection_key: connection_key.to_owned(),
            permission,
        });
    }
    next
}

pub fn summarize_pull_request_routing(
    entries: &[(&str, PullRequestRoutingPermission)],
) -> Option<String> {
    let mut groups = Vec::new();
    for permission in [PullRequestRoutingPermission::ReadWrite, PullRequestRoutingPermission::Read] {
        let labels: Vec<&str> = entries
            .iter()
            .filter_map(|(label, perm)| (*perm == permission).then_some(*label))
            .collect();
        if !labels.is_empty()
            && let Some(suffix) = permission.summary_label()
        {
            groups.push(format!("{} {suffix}", labels.join(", ")));
        }
    }
    (!groups.is_empty()).then(|| groups.join(" \u{00b7} "))
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

    #[test]
    fn text_generation_options_filter_supports_text_generation_and_include_generators() {
        let config: crate::server_config::ServerConfig = serde_json::from_value(json!({
            "providers": [
                {
                    "instanceId": "codex",
                    "driver": "codex",
                    "displayName": "Codex",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "models": [
                        { "slug": "gpt-5", "name": "GPT-5" },
                        { "slug": "gpt-no-text", "name": "No Text", "supportsTextGeneration": false }
                    ]
                },
                {
                    "instanceId": "fx",
                    "driver": "fx",
                    "displayName": "fx",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "supportsTextGeneration": false,
                    "models": [{ "slug": "fx-1", "name": "fx 1" }]
                }
            ],
            "settings": {
                "providerInstances": {
                    "codex": { "driver": "codex", "enabled": true },
                    "fx": { "driver": "fx", "enabled": true }
                },
                "textGenerators": {
                    "openrouter": {
                        "kind": "openaiCompatible",
                        "baseUrl": "https://openrouter.ai/api/v1",
                        "apiKey": "__REDACTED__",
                        "enabled": true
                    },
                    "disabled-gen": {
                        "kind": "anthropic",
                        "baseUrl": "https://api.anthropic.com/v1",
                        "apiKey": "",
                        "enabled": false
                    }
                }
            }
        }))
        .unwrap();

        let mut gen_models = std::collections::HashMap::new();
        gen_models.insert(
            "openrouter".to_owned(),
            vec![providers::text_generators::TextGeneratorModel {
                id: "anthropic/claude-haiku".into(),
                name: "Claude Haiku".into(),
            }],
        );
        let sel = ModelSelection {
            instance_id: "openrouter".into(),
            model: "anthropic/claude-haiku".into(),
            options: None,
        };
        let options = text_generation_options(&config, None, &gen_models, &[Some(&sel)]);
        let slugs: Vec<(&str, &str)> = options
            .iter()
            .map(|o| (o.instance_id.as_str(), o.model_slug.as_str()))
            .collect();
        assert_eq!(slugs, vec![("codex", "gpt-5"), ("openrouter", "anthropic/claude-haiku")]);
        assert_eq!(text_generation_selection_label(&config, &sel, &gen_models), "Claude Haiku");
        assert!(!is_text_generation_selection_valid(
            &config,
            None,
            &ModelSelection { instance_id: "fx".into(), model: "fx-1".into(), options: None }
        ));
        assert!(is_text_generation_selection_valid(&config, None, &sel));
    }

    #[test]
    fn peer_link_health_load_balancing_and_pr_routing_match_web() {
        let now = crate::sidebar::time::parse_iso("2026-11-19T00:00:00Z").unwrap();
        let healthy = PeerLinkSummary {
            environment_id: "vps".into(),
            label: "VPS".into(),
            urls: vec!["https://vps.example.ts.net".into()],
            access: "approval-required".into(),
            linked_at: "2026-11-01T00:00:00Z".into(),
            expires_at: "2026-12-01T00:00:00Z".into(),
            last_reached_at: None,
            last_error: None,
            status: PeerLinkStatus::Reachable,
        };
        assert_eq!(peer_link_health(&healthy, now), PeerLinkHealth::Reachable { expires_in_days: None });

        let expiring = PeerLinkSummary { expires_at: "2026-11-22T00:00:00Z".into(), ..healthy.clone() };
        assert_eq!(peer_link_health(&expiring, now), PeerLinkHealth::Reachable { expires_in_days: Some(3) });
        assert_eq!(
            peer_link_status_text(&peer_link_health(&expiring, now)),
            "Expires in 3 days. Link it again to renew."
        );

        let expired = PeerLinkSummary { expires_at: "2026-11-18T00:00:00Z".into(), ..healthy.clone() };
        assert_eq!(peer_link_health(&expired, now), PeerLinkHealth::Expired);

        let failing = PeerLinkSummary { last_error: Some("Revoked".into()), ..healthy.clone() };
        assert_eq!(peer_link_health(&failing, now), PeerLinkHealth::Failing { detail: "Revoked".into() });

        let mut weights = std::collections::BTreeMap::new();
        weights.insert("mac".into(), 100);
        weights.insert("vps".into(), 25);
        assert_eq!(
            summarize_load_preferences(&[("mac", "Mac"), ("vps", "VPS")], &weights).as_deref(),
            Some("Mac prefer \u{00b7} VPS less often")
        );

        let key = pull_request_routing_connection_key("vps", "https://vps.example.ts.net/", None).unwrap();
        let perms = set_pull_request_routing_permission(&[], "vps", &key, PullRequestRoutingPermission::ReadWrite);
        assert_eq!(pull_request_routing_permission_for(Some(&key), &perms), PullRequestRoutingPermission::ReadWrite);
        // Changing endpoint revokes trust automatically.
        let changed_key = pull_request_routing_connection_key("vps", "https://other.example.ts.net", None).unwrap();
        assert_eq!(pull_request_routing_permission_for(Some(&changed_key), &perms), PullRequestRoutingPermission::Off);
        assert_eq!(
            summarize_pull_request_routing(&[
                ("Mac", PullRequestRoutingPermission::Read),
                ("VPS", PullRequestRoutingPermission::ReadWrite),
            ])
            .as_deref(),
            Some("VPS read and act \u{00b7} Mac read PRs")
        );
    }

    #[test]
    fn settings_scope_axis_disambiguates_duplicate_labels_and_checkouts() {
        let envs = vec![
            ScopeEnvironment {
                environment_id: "e1".into(),
                label: "Mac".into(),
                display_url: "https://mac-1.ts.net".into(),
                connected: true,
            },
            ScopeEnvironment {
                environment_id: "e2".into(),
                label: "Mac".into(),
                display_url: "https://mac-2.ts.net".into(),
                connected: false,
            },
        ];
        assert_eq!(settings_scope_environment_label(&envs[0], &envs), "Mac \u{00b7} mac-1.ts.net");
        let group = ScopeProjectGroup {
            project_key: "repo".into(),
            display_name: "t3code".into(),
            workspace_root: "/a/t3code".into(),
            archived: false,
            members: vec![
                ScopeCheckout {
                    environment_id: "e1".into(),
                    physical_project_key: "e1::/a/t3code".into(),
                    workspace_root: "/a/t3code".into(),
                },
                ScopeCheckout {
                    environment_id: "e1".into(),
                    physical_project_key: "e1::/b/t3code".into(),
                    workspace_root: "/b/t3code".into(),
                },
            ],
        };
        let choices = settings_scope_checkout_choices(&group, &SettingsScopeSearch::default());
        assert_eq!(choices.len(), 2);
        assert_eq!(settings_scope_checkout_label(choices[0], &choices), "/a/t3code");
    }

    #[test]
    fn server_change_to_override_edit_routes_project_scoped_keys_and_refuses_env_only_keys() {
        assert_eq!(
            server_change_to_override_edit(&ServerSettingChange::AgentBrowserAccess(false)),
            Some(projects::OverrideEdit::Set {
                key: "enableAgentBrowserAccess".into(),
                value: json!(false),
            })
        );
        assert_eq!(
            server_change_to_override_edit(&ServerSettingChange::AutoSettleAfterDays(None)),
            Some(projects::OverrideEdit::Set {
                key: "sidebarAutoSettleAfterDays".into(),
                value: Value::Null,
            })
        );
        // Environment-only keys cannot be overridden per project.
        assert_eq!(
            server_change_to_override_edit(&ServerSettingChange::AddProjectBaseDirectory("~/dev".into())),
            None
        );
        assert_eq!(
            server_change_to_override_edit(&ServerSettingChange::GhqCloneLayout(true)),
            None
        );
        assert_eq!(
            server_change_to_override_edit(&ServerSettingChange::FastWorktreeCheckout(true)),
            None
        );
        // Multi-key prepared patch with all project-scoped keys succeeds.
        let prepared = ServerSettingChange::Prepared(json!({
            "patch": {
                "branchNamingMode": "prefix",
                "branchNamePrefix": "feat/"
            }
        }));
        assert!(matches!(
            server_change_to_override_edit(&prepared),
            Some(projects::OverrideEdit::SetMap(ref map)) if map.len() == 2
        ));
        // Prepared patch containing an env-only key is refused.
        let env_prepared = ServerSettingChange::Prepared(json!({
            "patch": { "addProjectBaseDirectory": "~/x" }
        }));
        assert_eq!(server_change_to_override_edit(&env_prepared), None);
    }

    #[test]
    fn scoped_model_disabled_reason_reports_unavailable_target_environment() {
        let mac: crate::server_config::ServerConfig = serde_json::from_value(json!({
            "environment": { "environmentId": "mac", "label": "Mac" },
            "providers": [{
                "instanceId": "codex",
                "driver": "codex",
                "displayName": "Codex",
                "enabled": true,
                "installed": true,
                "status": "ready",
                "models": [{ "slug": "gpt-5", "name": "GPT-5" }]
            }],
            "settings": { "providerInstances": { "codex": { "driver": "codex", "enabled": true } } }
        }))
        .unwrap();
        let vps: crate::server_config::ServerConfig = serde_json::from_value(json!({
            "environment": { "environmentId": "vps", "label": "Build VPS" },
            "providers": [{
                "instanceId": "codex",
                "driver": "codex",
                "displayName": "Codex",
                "enabled": true,
                "installed": true,
                "status": "ready",
                "models": [{ "slug": "gpt-4.1", "name": "GPT-4.1" }]
            }],
            "settings": { "providerInstances": { "codex": { "driver": "codex", "enabled": true } } }
        }))
        .unwrap();

        let targets = [("Mac", &mac, None), ("Build VPS", &vps, None)];
        assert_eq!(
            scoped_model_disabled_reason(&mac.settings, Some("codex"), &targets, "codex", "gpt-5", false),
            Some("This model is unavailable on Build VPS. Select that environment to choose its model separately.".into())
        );
        assert_eq!(
            scoped_model_disabled_reason(&mac.settings, Some("codex"), &[("Mac", &mac, None)], "codex", "gpt-5", false),
            None
        );
    }

    #[test]
    fn resolve_project_settings_overlays_project_overrides_and_tracks_presence() {
        let base: crate::server_config::ServerSettings = serde_json::from_value(json!({
            "defaultModelSelection": { "instanceId": "codex", "model": "gpt-5" },
            "defaultThreadEnvMode": "local",
            "defaultAutoPull": false,
            "sidebarAutoSettleAfterDays": 14,
            "continueThreadsAfterServerUpdate": true,
            "responseStreamingMode": "paragraph",
            "enableAgentBrowserAccess": true,
            "projectSettingsOverrides": {
                "proj-1": {
                    "defaultModelSelection": null,
                    "defaultThreadEnvMode": "worktree",
                    "defaultAutoPull": true,
                    "sidebarAutoSettleAfterDays": null,
                    "continueThreadsAfterServerUpdate": false,
                    "responseStreamingMode": "turn",
                    "enableAgentBrowserAccess": false,
                    "enableAgentDeviceAccess": false,
                    "branchNamingMode": "custom",
                    "branchNameInstructions": "prefix with feat/",
                    "providerInstances": { "codex": { "enabled": false } }
                }
            }
        }))
        .unwrap();

        let resolved = resolve_project_settings(&base, "proj-1");
        assert_eq!(resolved.default_model_selection, None);
        assert_eq!(resolved.default_thread_env_mode, Some(ThreadEnvMode::Worktree));
        assert!(resolved.source_control.default_auto_pull);
        assert_eq!(resolved.threads.sidebar_auto_settle_after_days, None);
        assert!(!resolved.maintenance.continue_threads_after_server_update);
        assert_eq!(resolved.threads.response_streaming_mode, ResponseStreamingMode::Turn);
        assert_eq!(resolved.enable_agent_browser_access, Some(false));
        assert_eq!(resolved.worktrees.branch_naming_mode, worktrees::BranchNaming::Custom);
        assert_eq!(resolved.worktrees.branch_name_instructions, "prefix with feat/");
        assert!(project_override_present(&base, "proj-1", &["defaultAutoPull"]));
        assert!(!project_override_present(&base, "proj-1", &["removeAgentCreditsOnMerge"]));
        assert!(!effective_agent_device_access(&base, Some("proj-1"), true));
        assert!(effective_agent_device_access(&base, Some("proj-other"), true));
        assert!(!effective_provider_enabled(&base, Some("proj-1"), "codex", true));
        assert!(effective_provider_enabled(&base, Some("proj-other"), "codex", true));

        assert_eq!(project_provider_instance_override(&base, "proj-1", "codex"), Some(false));
        assert_eq!(project_provider_instance_override(&base, "proj-other", "codex"), None);

        // Clearing the last provider instance override while sibling keys remain must send
        // `"providerInstances": {}` so the server replaces (instead of keeping) the stored map.
        let patch = projects::override_patch(
            &base,
            &[project_actions::Member { id: "proj-1".into(), scripts: Vec::new() }],
            &projects::OverrideEdit::ProviderInstanceEnabled {
                instance_id: "codex".into(),
                enabled: None,
            },
        );
        assert_eq!(
            patch["patch"]["projectSettingsOverrides"]["proj-1"]["providerInstances"],
            json!({}),
        );
        assert_eq!(patch["patch"]["projectSettingsOverrides"]["proj-1"]["defaultAutoPull"], json!(true));

        // Clearing the last provider instance override when it is the ONLY key emits `null`.
        let solo: crate::server_config::ServerSettings = serde_json::from_value(json!({
            "projectSettingsOverrides": {
                "proj-solo": { "providerInstances": { "codex": { "enabled": false } } }
            }
        }))
        .unwrap();
        let solo_patch = projects::override_patch(
            &solo,
            &[project_actions::Member { id: "proj-solo".into(), scripts: Vec::new() }],
            &projects::OverrideEdit::ProviderInstanceEnabled {
                instance_id: "codex".into(),
                enabled: None,
            },
        );
        assert_eq!(solo_patch["patch"]["projectSettingsOverrides"]["proj-solo"], Value::Null);
    }

    #[test]
    fn project_override_merges_source_control_writing_style_and_worktree_cleanup() {
        let base: crate::server_config::ServerSettings = serde_json::from_value(json!({
            "sourceControlWritingStyle": {
                "mode": "conventional_commits",
                "customInstructions": "env rule",
                "followChangeRequestTemplates": true
            },
            "storageCleanup": {
                "worktreeOnDelete": true,
                "worktreeOnMerge": false,
                "worktreeAfterDays": 14,
                "worktreeKeepWhen": "tracked-changes"
            },
            "projectSettingsOverrides": {
                "proj-1": {
                    "sourceControlWritingStyle": {
                        "mode": "custom",
                        "customInstructions": "keep me",
                        "followChangeRequestTemplates": true
                    }
                }
            }
        }))
        .unwrap();
        let members = [project_actions::Member { id: "proj-1".into(), scripts: Vec::new() }];

        // Toggling followChangeRequestTemplates off preserves custom mode & instructions.
        let toggle_templates = server_change_to_override_edit(
            &source_control::SourceControlChange::FollowTemplates(false).into_server(),
        )
        .expect("follow templates is project-scoped");
        let patch = projects::override_patch(&base, &members, &toggle_templates);
        assert_eq!(
            patch["patch"]["projectSettingsOverrides"]["proj-1"]["sourceControlWritingStyle"],
            json!({
                "mode": "custom",
                "customInstructions": "keep me",
                "followChangeRequestTemplates": false,
            }),
        );

        // Switching mode / resetting writing style preserves followChangeRequestTemplates=false.
        let with_templates_off: crate::server_config::ServerSettings = serde_json::from_value(json!({
            "sourceControlWritingStyle": {
                "mode": "conventional_commits",
                "customInstructions": "env rule",
                "followChangeRequestTemplates": true
            },
            "projectSettingsOverrides": {
                "proj-1": {
                    "sourceControlWritingStyle": {
                        "mode": "custom",
                        "customInstructions": "keep me",
                        "followChangeRequestTemplates": false
                    }
                }
            }
        }))
        .unwrap();
        let reset_style = server_change_to_override_edit(
            &source_control::SourceControlChange::WritingMode {
                mode: source_control::WritingMode::ConventionalCommits,
                custom_instructions: Some("env rule".into()),
            }
            .into_server(),
        )
        .unwrap();
        let patch = projects::override_patch(&with_templates_off, &members, &reset_style);
        assert_eq!(
            patch["patch"]["projectSettingsOverrides"]["proj-1"]["sourceControlWritingStyle"],
            json!({
                "mode": "conventional_commits",
                "customInstructions": "env rule",
                "followChangeRequestTemplates": false,
            }),
        );

        // Worktree cleanup: Off, Custom rule merge (preserving sibling rules from effective
        // storageCleanup), and Inherit clear.
        let off_edit = server_change_to_override_edit(&worktrees::WorktreeChange::ProjectCleanupOff.into_server())
            .expect("worktreeCleanup is project-scoped");
        let off_patch = projects::override_patch(&base, &members, &off_edit);
        assert_eq!(
            off_patch["patch"]["projectSettingsOverrides"]["proj-1"]["worktreeCleanup"],
            json!({ "mode": "off" }),
        );

        let rule_edit = server_change_to_override_edit(
            &worktrees::WorktreeChange::ProjectCleanupRule(worktrees::CleanupRule::OnMerge(true)).into_server(),
        )
        .expect("project worktree cleanup rule is project-scoped");
        let rule_patch = projects::override_patch(&base, &members, &rule_edit);
        assert_eq!(
            rule_patch["patch"]["projectSettingsOverrides"]["proj-1"]["worktreeCleanup"],
            json!({
                "mode": "custom",
                "rules": {
                    "worktreeKeepWhen": "tracked-changes",
                    "worktreeAfterDays": 14,
                    "worktreeSettledAfterDays": null,
                    "worktreeOnMerge": true,
                    "worktreeOnDelete": true,
                    "worktreeUnchanged": false,
                }
            }),
        );

        // Environment capabilities decode projectWorktreeCleanup & projectProviderInstanceOverrides.
        let caps: crate::server_config::EnvironmentCapabilities = serde_json::from_value(json!({
            "projectWorktreeCleanup": true,
            "projectProviderInstanceOverrides": true,
        }))
        .unwrap();
        assert_eq!(caps.project_worktree_cleanup, Some(true));
        assert_eq!(caps.project_provider_instance_overrides, Some(true));
    }
}
