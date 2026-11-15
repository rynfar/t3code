//! Voice consent state, preferred speech language normalization, and microphone priority order.
//!
//! Mirrors `apps/web/src/speech/voiceConsent.ts`,
//! `apps/web/src/speech/preferredSpeechLanguages.ts`, and
//! `apps/web/src/speech/microphoneOrder.ts`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::contracts::{CloudSpeechProvider, validate_cloud_speech_model_id};

pub const VOICE_AUDIO_GRANT_STORAGE_KEY: &str = "t3code:voice-audio-grant:v1";
pub const VOICE_CLEANUP_GRANT_STORAGE_KEY: &str = "t3code:voice-cleanup-grant:v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceTranscriptionDestination {
    Environment {
        environment_id: String,
    },
    Cloud {
        environment_id: String,
        provider: CloudSpeechProvider,
        model: Option<String>,
    },
}

impl VoiceTranscriptionDestination {
    pub fn environment(environment_id: impl Into<String>) -> Self {
        Self::Environment { environment_id: environment_id.into() }
    }

    pub fn cloud(environment_id: impl Into<String>, provider: CloudSpeechProvider) -> Self {
        Self::Cloud { environment_id: environment_id.into(), provider, model: None }
    }

    pub fn openrouter(environment_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self::Cloud {
            environment_id: environment_id.into(),
            provider: CloudSpeechProvider::Openrouter,
            model: Some(model.into()),
        }
    }

    pub fn environment_id(&self) -> &str {
        match self {
            Self::Environment { environment_id } | Self::Cloud { environment_id, .. } => {
                environment_id
            }
        }
    }
}

impl Serialize for VoiceTranscriptionDestination {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct EnvWire<'a> {
            kind: &'static str,
            environment_id: &'a str,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct CloudWire<'a> {
            kind: &'static str,
            environment_id: &'a str,
            provider: CloudSpeechProvider,
            #[serde(skip_serializing_if = "Option::is_none")]
            model: Option<&'a str>,
        }
        match self {
            Self::Environment { environment_id } => {
                EnvWire { kind: "environment", environment_id }.serialize(serializer)
            }
            Self::Cloud { environment_id, provider, model } => CloudWire {
                kind: "cloud",
                environment_id,
                provider: *provider,
                model: model.as_deref(),
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for VoiceTranscriptionDestination {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            kind: String,
            environment_id: String,
            provider: Option<CloudSpeechProvider>,
            model: Option<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        match wire.kind.as_str() {
            "environment" => Ok(Self::Environment { environment_id: wire.environment_id }),
            "cloud" => {
                let provider = wire
                    .provider
                    .ok_or_else(|| serde::de::Error::missing_field("provider"))?;
                if provider == CloudSpeechProvider::Openrouter {
                    let model = wire
                        .model
                        .filter(|m| validate_cloud_speech_model_id(m))
                        .ok_or_else(|| serde::de::Error::custom("OpenRouter destination requires a valid CloudSpeechModelId"))?;
                    Ok(Self::Cloud {
                        environment_id: wire.environment_id,
                        provider,
                        model: Some(model),
                    })
                } else {
                    Ok(Self::Cloud {
                        environment_id: wire.environment_id,
                        provider,
                        model: None,
                    })
                }
            }
            other => Err(serde::de::Error::custom(format!("Unknown voice destination kind: {other}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioGrant {
    pub id: String,
    pub destination: VoiceTranscriptionDestination,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SavedAudioChoice {
    #[default]
    Unset,
    Off,
    Granted(AudioGrant),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioChoice {
    Saved(SavedAudioChoice),
    Pending { pending: AudioGrant, previous: SavedAudioChoice },
}

impl Default for AudioChoice {
    fn default() -> Self {
        Self::Saved(SavedAudioChoice::Unset)
    }
}

impl AudioChoice {
    pub fn pending_grant(&self) -> Option<&AudioGrant> {
        match self {
            Self::Pending { pending, .. } => Some(pending),
            Self::Saved(_) => None,
        }
    }
}

/// Manages explicit audio and transcript-cleanup consent grants (`apps/web/src/speech/voiceConsent.ts`).
#[derive(Debug, Clone, Default)]
pub struct VoiceConsentState {
    audio_choice: AudioChoice,
    save_markers: HashMap<String, bool>,
    cleanup_grant: Option<String>,
}

impl VoiceConsentState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_audio_choice(&self) -> SavedAudioChoice {
        match &self.audio_choice {
            AudioChoice::Saved(saved) => saved.clone(),
            AudioChoice::Pending { pending, previous } => match self.save_markers.get(&pending.id) {
                None => previous.clone(),
                Some(true) => SavedAudioChoice::Granted(pending.clone()),
                Some(false) => SavedAudioChoice::Off,
            },
        }
    }

    pub fn active_grant(&self) -> Option<AudioGrant> {
        match self.active_audio_choice() {
            SavedAudioChoice::Granted(grant) => Some(grant),
            SavedAudioChoice::Unset | SavedAudioChoice::Off => None,
        }
    }

    pub fn offer_first_run_setup(&self) -> bool {
        matches!(self.active_audio_choice(), SavedAudioChoice::Unset)
    }

    pub fn choose_audio_destination(
        &mut self,
        grant_id: Option<String>,
        destination: Option<VoiceTranscriptionDestination>,
    ) {
        let next = match (grant_id, destination) {
            (Some(id), Some(destination)) => SavedAudioChoice::Granted(AudioGrant { id, destination }),
            _ => SavedAudioChoice::Off,
        };
        self.replace_choice(AudioChoice::Saved(next));
    }

    pub fn begin_pending_cloud_choice(
        &mut self,
        pending_id: impl Into<String>,
        destination: VoiceTranscriptionDestination,
    ) -> String {
        let pending_id = pending_id.into();
        let previous = self.active_audio_choice();
        let pending = AudioGrant { id: pending_id.clone(), destination };
        self.replace_choice(AudioChoice::Pending { pending, previous });
        pending_id
    }

    pub fn acknowledge_pending_cloud_choice(&mut self, pending_id: &str) {
        if self.audio_choice.pending_grant().is_some_and(|p| p.id == pending_id) {
            self.save_markers.insert(pending_id.to_owned(), true);
        }
    }

    pub fn fail_pending_cloud_choice(&mut self, pending_id: &str) {
        if self.audio_choice.pending_grant().is_some_and(|p| p.id == pending_id) {
            self.save_markers.insert(pending_id.to_owned(), false);
        }
    }

    pub fn bind_audio_grant(&self, id: &str) -> Option<AudioGrant> {
        self.active_grant().filter(|grant| grant.id == id)
    }

    pub fn is_bound_grant_valid(&self, bound: &AudioGrant) -> bool {
        self.active_grant().as_ref() == Some(bound)
    }

    pub fn choose_transcript_cleanup(&mut self, grant_id: Option<String>) {
        self.cleanup_grant = grant_id;
    }

    pub fn cleanup_grant(&self) -> Option<&str> {
        self.cleanup_grant.as_deref()
    }

    fn replace_choice(&mut self, next: AudioChoice) {
        let old_pending = self.audio_choice.pending_grant().map(|g| g.id.clone());
        let new_pending = next.pending_grant().map(|g| g.id.clone());
        self.audio_choice = next;
        if let Some(old_id) = old_pending
            && new_pending.as_deref() != Some(old_id.as_str())
        {
            self.save_markers.remove(&old_id);
        }
    }
}

/// `preferredSpeechLanguages` from `apps/web/src/speech/preferredSpeechLanguages.ts`.
pub fn preferred_speech_languages(
    system_languages: &[&str],
    system_locale: Option<&str>,
    fallback_languages: &[&str],
) -> Vec<String> {
    let mut system: Vec<&str> = system_languages.to_vec();
    if system.is_empty()
        && let Some(locale) = system_locale
        && !locale.is_empty()
    {
        system.push(locale);
    }
    system
        .into_iter()
        .chain(fallback_languages.iter().copied())
        .map(|tag| tag.to_lowercase().split(['-', '_']).next().unwrap_or("").to_owned())
        .collect()
}

/// `VoiceMicrophoneEntry` from `packages/contracts/src/settings.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum VoiceMicrophoneEntry {
    Default,
    #[serde(rename_all = "camelCase")]
    Device { device_id: String, label: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub device_id: String,
    pub label: String,
}

pub fn is_system_default_device_id(device_id: &str) -> bool {
    matches!(device_id, "" | "default" | "communications")
}

pub fn microphone_entry_key(entry: &VoiceMicrophoneEntry) -> String {
    match entry {
        VoiceMicrophoneEntry::Default => "default".to_owned(),
        VoiceMicrophoneEntry::Device { device_id, .. } => format!("device:{device_id}"),
    }
}

/// `microphoneOrder` from `apps/web/src/speech/microphoneOrder.ts`.
pub fn microphone_order(
    voice_microphones: &[VoiceMicrophoneEntry],
    legacy_voice_microphone: &str,
) -> Vec<VoiceMicrophoneEntry> {
    if voice_microphones.is_empty() {
        return if !legacy_voice_microphone.is_empty() {
            vec![
                VoiceMicrophoneEntry::Device {
                    device_id: legacy_voice_microphone.to_owned(),
                    label: String::new(),
                },
                VoiceMicrophoneEntry::Default,
            ]
        } else {
            vec![VoiceMicrophoneEntry::Default]
        };
    }

    let mut keys = HashSet::new();
    let mut order = Vec::with_capacity(voice_microphones.len() + 1);
    for entry in voice_microphones {
        let key = microphone_entry_key(entry);
        if keys.insert(key) {
            order.push(entry.clone());
        }
    }
    if !keys.contains("default") {
        order.push(VoiceMicrophoneEntry::Default);
    }
    order
}

/// `rememberMicrophones` from `apps/web/src/speech/microphoneOrder.ts`.
pub fn remember_microphones(
    order: &[VoiceMicrophoneEntry],
    devices: &[AudioDeviceInfo],
) -> Vec<VoiceMicrophoneEntry> {
    let mut next = order.to_vec();
    let mut insert_at = next
        .iter()
        .position(|entry| matches!(entry, VoiceMicrophoneEntry::Default))
        .map_or(0, |index| index + 1);

    for device in devices {
        if device.label.is_empty() || is_system_default_device_id(&device.device_id) {
            continue;
        }
        if let Some(index) = next.iter().position(|entry| {
            matches!(entry, VoiceMicrophoneEntry::Device { device_id, .. } if device_id == &device.device_id)
        }) {
            if let VoiceMicrophoneEntry::Device { label, .. } = &next[index]
                && label == &device.label
            {
                continue;
            }
            next[index] = VoiceMicrophoneEntry::Device {
                device_id: device.device_id.clone(),
                label: device.label.clone(),
            };
            continue;
        }
        next.insert(
            insert_at,
            VoiceMicrophoneEntry::Device {
                device_id: device.device_id.clone(),
                label: device.label.clone(),
            },
        );
        insert_at += 1;
    }
    next
}

/// `preferredMicrophone` from `apps/web/src/speech/microphoneOrder.ts`.
pub fn preferred_microphone(
    order: &[VoiceMicrophoneEntry],
    devices: &[AudioDeviceInfo],
) -> VoiceMicrophoneEntry {
    if devices.iter().all(|device| device.device_id.is_empty()) {
        return order.first().cloned().unwrap_or(VoiceMicrophoneEntry::Default);
    }
    order
        .iter()
        .find(|entry| match entry {
            VoiceMicrophoneEntry::Default => true,
            VoiceMicrophoneEntry::Device { device_id, .. } => {
                devices.iter().any(|device| &device.device_id == device_id)
            }
        })
        .cloned()
        .unwrap_or(VoiceMicrophoneEntry::Default)
}

/// `preferredMicrophoneId` from `apps/web/src/speech/microphoneOrder.ts`.
pub fn preferred_microphone_id(
    order: &[VoiceMicrophoneEntry],
    devices: &[AudioDeviceInfo],
) -> String {
    match preferred_microphone(order, devices) {
        VoiceMicrophoneEntry::Default => String::new(),
        VoiceMicrophoneEntry::Device { device_id, .. } => device_id,
    }
}

/// `moveMicrophone` from `apps/web/src/speech/microphoneOrder.ts`.
pub fn move_microphone(
    order: &[VoiceMicrophoneEntry],
    key: &str,
    steps: isize,
) -> Vec<VoiceMicrophoneEntry> {
    let Some(from) = order.iter().position(|entry| microphone_entry_key(entry) == key) else {
        return order.to_vec();
    };
    if order.is_empty() {
        return order.to_vec();
    }
    let to = (from as isize + steps).clamp(0, (order.len() - 1) as isize) as usize;
    if from == to {
        return order.to_vec();
    }
    let mut next = order.to_vec();
    let entry = next.remove(from);
    next.insert(to, entry);
    next
}

/// `preferMicrophone` from `apps/web/src/speech/microphoneOrder.ts`.
pub fn prefer_microphone(
    order: &[VoiceMicrophoneEntry],
    entry: VoiceMicrophoneEntry,
) -> Vec<VoiceMicrophoneEntry> {
    let key = microphone_entry_key(&entry);
    let first = order
        .iter()
        .find(|candidate| microphone_entry_key(candidate) == key)
        .cloned()
        .unwrap_or(entry);
    let mut next = Vec::with_capacity(order.len() + 1);
    next.push(first);
    for candidate in order {
        if microphone_entry_key(candidate) != key {
            next.push(candidate.clone());
        }
    }
    next
}

/// `forgetMicrophone` from `apps/web/src/speech/microphoneOrder.ts`.
pub fn forget_microphone(order: &[VoiceMicrophoneEntry], key: &str) -> Vec<VoiceMicrophoneEntry> {
    if key == "default" {
        return order.to_vec();
    }
    order.iter().filter(|entry| microphone_entry_key(entry) != key).cloned().collect()
}
