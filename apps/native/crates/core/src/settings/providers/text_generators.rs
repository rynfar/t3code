//! Text generators (`pingdotgg/t3code#33147`): lightweight model APIs (OpenAI, OpenRouter,
//! Anthropic, or any OpenAI-compatible server) for background text tasks — thread titles &
//! summaries, commit/PR writing, and speech post-processing. Ports `TextGeneratorConfig`,
//! `ListTextGeneratorModelsInput` (`contracts/src/settings.ts`), `apps/web/src/textGenerators.ts`,
//! and `apps/web/src/components/settings/TextGeneratorsSettings.tsx`.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use crate::device::hosts::SecretValue;
use crate::model::ModelSelection;
use crate::server_config::{ServerProvider, ServerSettings};

pub const LIST_TEXT_GENERATOR_MODELS: &str = "server.listTextGeneratorModels";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextGeneratorKind {
    #[default]
    OpenAiCompatible,
    Anthropic,
}

impl TextGeneratorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TextGeneratorKind::OpenAiCompatible => "openaiCompatible",
            TextGeneratorKind::Anthropic => "anthropic",
        }
    }

    pub fn parse(value: &str) -> Option<TextGeneratorKind> {
        match value {
            "openaiCompatible" => Some(TextGeneratorKind::OpenAiCompatible),
            "anthropic" => Some(TextGeneratorKind::Anthropic),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TextGeneratorKind::OpenAiCompatible => "OpenAI-compatible",
            TextGeneratorKind::Anthropic => "Anthropic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextGeneratorPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: TextGeneratorKind,
    pub base_url: &'static str,
}

pub const TEXT_GENERATOR_PRESETS: [TextGeneratorPreset; 4] = [
    TextGeneratorPreset {
        id: "openai",
        label: "OpenAI",
        kind: TextGeneratorKind::OpenAiCompatible,
        base_url: "https://api.openai.com/v1",
    },
    TextGeneratorPreset {
        id: "openrouter",
        label: "OpenRouter",
        kind: TextGeneratorKind::OpenAiCompatible,
        base_url: "https://openrouter.ai/api/v1",
    },
    TextGeneratorPreset {
        id: "anthropic",
        label: "Anthropic",
        kind: TextGeneratorKind::Anthropic,
        base_url: "https://api.anthropic.com/v1",
    },
    TextGeneratorPreset {
        id: "custom",
        label: "Custom (OpenAI-compatible)",
        kind: TextGeneratorKind::OpenAiCompatible,
        base_url: "",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextGeneratorConfig {
    pub kind: TextGeneratorKind,
    pub label: Option<String>,
    pub base_url: String,
    pub api_key: SecretValue,
    pub enabled: bool,
}

impl TextGeneratorConfig {
    pub fn decode(value: &Value) -> Option<TextGeneratorConfig> {
        let obj = value.as_object()?;
        let kind = TextGeneratorKind::parse(obj.get("kind")?.as_str()?)?;
        let base_url = obj.get("baseUrl")?.as_str()?.trim();
        if !is_valid_base_url(base_url) {
            return None;
        }
        let label = obj
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let api_key = obj.get("apiKey").and_then(SecretValue::decode).unwrap_or_default();
        let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        Some(TextGeneratorConfig {
            kind,
            label,
            base_url: base_url.to_owned(),
            api_key,
            enabled,
        })
    }

    pub fn validate(&self) -> Result<TextGeneratorConfig, String> {
        let base_url = self.base_url.trim().trim_end_matches('/');
        if !is_valid_base_url(base_url) {
            return Err("Enter an http:// or https:// base URL.".to_owned());
        }
        if let SecretValue::Reference { provider, reference } = &self.api_key
            && SecretValue::reference(provider, reference).is_none()
        {
            return Err("Enter a valid secret reference.".to_owned());
        }
        let label = self.label.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        Ok(TextGeneratorConfig {
            kind: self.kind,
            label,
            base_url: base_url.to_owned(),
            api_key: self.api_key.clone(),
            enabled: self.enabled,
        })
    }

    pub fn encode(&self) -> Value {
        let mut out = json!({
            "kind": self.kind.as_str(),
            "baseUrl": self.base_url,
            "apiKey": self.api_key.encode(),
            "enabled": self.enabled,
        });
        if let Some(label) = self.label.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            out["label"] = Value::String(label.to_owned());
        }
        out
    }

    /// `textGeneratorDisplayName` (`apps/web/src/textGenerators.ts`).
    pub fn display_name(&self) -> String {
        if let Some(label) = self.label.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            return label.to_owned();
        }
        if let Some(preset) = TEXT_GENERATOR_PRESETS
            .iter()
            .find(|preset| !preset.base_url.is_empty() && preset.base_url == self.base_url && preset.kind == self.kind)
        {
            return preset.label.to_owned();
        }
        Url::parse(&self.base_url)
            .ok()
            .and_then(|url| url.host_str().map(|host| match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_owned(),
            }))
            .unwrap_or_else(|| self.base_url.clone())
    }

    /// `textGeneratorBrand`.
    pub fn brand(&self) -> &'static str {
        if self.kind == TextGeneratorKind::Anthropic {
            return "anthropic";
        }
        if let Ok(url) = Url::parse(&self.base_url) {
            let host = url.host_str().unwrap_or_default();
            if host == "openrouter.ai" || host.ends_with(".openrouter.ai") {
                return "openrouter";
            }
            if host == "api.openai.com" {
                return "openai";
            }
            if url.port() == Some(11434) || host.contains("ollama") {
                return "ollama";
            }
        }
        "openaiCompatible"
    }
}

pub fn is_valid_base_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value.trim()) else { return false };
    matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(|h| !h.is_empty())
}

/// Decodes `ServerSettings.text_generators` skipping malformed entries.
pub fn decode_text_generators(value: &Value) -> BTreeMap<String, TextGeneratorConfig> {
    let Some(obj) = value.as_object() else { return BTreeMap::new() };
    obj.iter()
        .filter_map(|(id, raw)| TextGeneratorConfig::decode(raw).map(|cfg| (id.clone(), cfg)))
        .collect()
}

/// `nextTextGeneratorId`: generates a unique id that collides with neither provider instances,
/// built-in driver kinds, nor existing text generators.
pub fn next_text_generator_id(base: &str, settings: &ServerSettings, providers: &[ServerProvider]) -> String {
    let mut taken: HashSet<String> = HashSet::new();
    for (id, _) in settings.provider_instances.iter() {
        taken.insert(id.to_owned());
    }
    for id in decode_text_generators(&settings.text_generators).keys() {
        taken.insert(id.clone());
    }
    for provider in providers {
        taken.insert(provider.instance_id.clone());
        taken.insert(provider.driver.clone());
    }
    for built_in in [
        "codex",
        "claudeAgent",
        "cursor",
        "grok",
        "opencode",
        "antigravity",
        "kiro",
        "pi",
        "museCode",
        "fx",
        "acp",
    ] {
        taken.insert(built_in.to_owned());
    }
    let clean_base = sanitize_id_base(base);
    if !taken.contains(&clean_base) {
        return clean_base;
    }
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{clean_base}-{suffix}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn sanitize_id_base(base: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in base.to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(ch);
        } else {
            dash = true;
        }
    }
    if out.is_empty() { "text-generator".to_owned() } else { out }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TextGeneratorModel {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListModelsResult {
    #[serde(default)]
    pub models: Vec<TextGeneratorModel>,
    #[serde(default)]
    pub tested_model: Option<String>,
}

impl ListModelsResult {
    pub fn decode(value: Value) -> Result<ListModelsResult, String> {
        serde_json::from_value(value).map_err(|err| format!("Invalid text generator models: {err}"))
    }
}

pub fn list_models_payload(id: &str, refresh: bool) -> Value {
    if refresh {
        json!({ "id": id, "refresh": true })
    } else {
        json!({ "id": id })
    }
}

pub fn check_config_payload(id: &str, config: &TextGeneratorConfig) -> Value {
    json!({ "id": id, "config": config.encode(), "refresh": true })
}

pub fn test_model_payload(id: &str, model: Option<&str>) -> Value {
    match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(model) => json!({ "id": id, "test": true, "model": model }),
        None => json!({ "id": id, "test": true }),
    }
}

pub fn upsert_text_generator_patch(id: &str, config: &TextGeneratorConfig) -> Value {
    json!({ "patch": { "textGenerators": { id: config.encode() } } })
}

pub fn remove_text_generator_patch(id: &str) -> Value {
    json!({ "patch": { "textGenerators": { id: Value::Null } } })
}

/// The three background text tasks a text generator can power (`TextGeneratorWizard` Step 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextTask {
    General,
    SourceControl,
    Speech,
}

impl TextTask {
    pub const ALL: [TextTask; 3] = [TextTask::General, TextTask::SourceControl, TextTask::Speech];

    pub fn setting_key(self) -> &'static str {
        match self {
            TextTask::General => "textGenerationModelSelection",
            TextTask::SourceControl => "sourceControlWriterModelSelection",
            TextTask::Speech => "speechPostProcessingModelSelection",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TextTask::General => "Text generation",
            TextTask::SourceControl => "Commits & pull requests",
            TextTask::Speech => "Voice transcription cleanup",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            TextTask::General => "Thread titles, summaries, and other short background text.",
            TextTask::SourceControl => "Commit messages, branch names, and pull request titles & descriptions.",
            TextTask::Speech => "Fixes punctuation and project terms after speech-to-text.",
        }
    }

    pub fn current_selection(self, settings: &ServerSettings) -> Option<ModelSelection> {
        match self {
            TextTask::General => settings
                .provider_panel
                .text_generation_model_selection
                .clone()
                .and_then(|v| serde_json::from_value(v).ok()),
            TextTask::SourceControl => settings.source_control.source_control_writer_model_selection.clone(),
            TextTask::Speech => settings
                .provider_panel
                .speech_post_processing_model_selection
                .clone()
                .and_then(|v| serde_json::from_value(v).ok()),
        }
    }

    pub fn patch(self, selection: Option<&ModelSelection>) -> Value {
        json!({ "patch": { self.setting_key(): selection } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_generators_decode_validate_brand_and_allocate_unique_ids() {
        let settings: ServerSettings = serde_json::from_value(json!({
            "providerInstances": { "openai": { "driver": "codex" } },
            "textGenerators": {
                "openai-2": {
                    "kind": "openaiCompatible",
                    "baseUrl": "https://api.openai.com/v1",
                    "apiKey": "__REDACTED__",
                    "enabled": true
                },
                "local": {
                    "kind": "openaiCompatible",
                    "label": "Local Ollama",
                    "baseUrl": "http://127.0.0.1:11434/v1",
                    "apiKey": "",
                    "enabled": false
                },
                "broken": { "kind": "unknown", "baseUrl": "not-a-url" }
            }
        }))
        .unwrap();

        let decoded = decode_text_generators(&settings.text_generators);
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded["openai-2"].display_name(), "OpenAI");
        assert_eq!(decoded["openai-2"].brand(), "openai");
        assert_eq!(decoded["local"].display_name(), "Local Ollama");
        assert_eq!(decoded["local"].brand(), "ollama");
        assert!(!decoded["local"].enabled);

        let next = next_text_generator_id("openai", &settings, &[]);
        assert_eq!(next, "openai-3");
        assert_eq!(next_text_generator_id("codex", &settings, &[]), "codex-2");

        let invalid_url = TextGeneratorConfig {
            kind: TextGeneratorKind::OpenAiCompatible,
            label: None,
            base_url: "ftp://bad.host".into(),
            api_key: SecretValue::default(),
            enabled: true,
        };
        assert!(invalid_url.validate().is_err());

        let valid = TextGeneratorConfig {
            kind: TextGeneratorKind::Anthropic,
            label: Some("  Claude Direct  ".into()),
            base_url: "https://api.anthropic.com/v1/".into(),
            api_key: SecretValue::reference("op", "op://Work/Anthropic/key").unwrap(),
            enabled: true,
        }
        .validate()
        .unwrap();
        assert_eq!(valid.base_url, "https://api.anthropic.com/v1");
        assert_eq!(valid.brand(), "anthropic");
        assert_eq!(
            upsert_text_generator_patch("anthropic", &valid)["patch"]["textGenerators"]["anthropic"]["apiKey"],
            json!({ "provider": "op", "reference": "op://Work/Anthropic/key" })
        );
        assert_eq!(remove_text_generator_patch("anthropic")["patch"]["textGenerators"]["anthropic"], Value::Null);
        assert_eq!(
            test_model_payload("anthropic", Some("claude-haiku-4-5")),
            json!({ "id": "anthropic", "test": true, "model": "claude-haiku-4-5" })
        );
    }
}
