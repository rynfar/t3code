use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{App, Global};
use t3_client::auth::Credentials;
use t3_client::store::{load_ui_state, save_ui_state};
use t3_client::supervisor::ConnectionStatus;
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::voice_input::{
    AudioGrant, SavedAudioChoice, VoiceConsentState, VoiceTranscriptionDestination,
};

use crate::app_state::AppState;

const AUDIO_GRANT_STORE_KEY: &str = "voice-audio-grant";
const CLEANUP_GRANT_STORE_KEY: &str = "voice-cleanup-grant";

#[derive(Debug, Clone)]
pub struct VoiceEnvironmentCandidate {
    pub environment_id: String,
    pub saved_id: String,
    pub label: String,
    pub credentials: Credentials,
    pub local_transcription: bool,
    pub cloud_transcription: bool,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedVoiceDestination {
    pub grant_id: String,
    pub destination: VoiceTranscriptionDestination,
    pub credentials: Credentials,
    pub environment_label: String,
    pub cleanup_grant_id: Option<String>,
    pub cleanup_granted: bool,
}

#[derive(Debug, Clone)]
pub struct VoiceDestinationResolution {
    pub offer_first_run_setup: bool,
    pub active: Option<ResolvedVoiceDestination>,
}

struct StoredVoiceConsent {
    state: VoiceConsentState,
    persist_to_disk: bool,
    generation: Arc<AtomicU64>,
}

impl Global for StoredVoiceConsent {}

impl StoredVoiceConsent {
    fn load(generation: Option<Arc<AtomicU64>>) -> Self {
        let mut state = VoiceConsentState::new();
        if let Some(value) = load_ui_state(AUDIO_GRANT_STORE_KEY)
            && let Ok(choice) = serde_json::from_value::<SavedAudioChoice>(value)
        {
            match choice {
                SavedAudioChoice::Unset => {}
                SavedAudioChoice::Off => state.choose_audio_destination(None, None),
                SavedAudioChoice::Granted(grant) => {
                    state.choose_audio_destination(Some(grant.id), Some(grant.destination));
                }
            }
        }
        if let Some(value) = load_ui_state(CLEANUP_GRANT_STORE_KEY)
            && let Ok(Some(grant_id)) = serde_json::from_value::<Option<String>>(value)
            && state.bind_audio_grant(&grant_id).is_some()
        {
            state.choose_transcript_cleanup(Some(grant_id));
        }
        Self {
            state,
            persist_to_disk: true,
            generation: generation.unwrap_or_else(|| Arc::new(AtomicU64::new(1))),
        }
    }

    fn save(&self) {
        if !self.persist_to_disk {
            return;
        }
        save_ui_state(AUDIO_GRANT_STORE_KEY, &self.state.active_audio_choice());
        let cleanup = self
            .state
            .active_grant()
            .and_then(|g| (self.state.cleanup_grant() == Some(g.id.as_str())).then_some(g.id));
        save_ui_state(CLEANUP_GRANT_STORE_KEY, &cleanup);
    }
}

fn ensure_consent(cx: &mut App) {
    if cx.try_global::<StoredVoiceConsent>().is_none() {
        cx.set_global(StoredVoiceConsent::load(None));
    }
}

pub fn voice_consent_generation(cx: &mut App) -> (Arc<AtomicU64>, u64) {
    ensure_consent(cx);
    let stored = cx.global::<StoredVoiceConsent>();
    (
        stored.generation.clone(),
        stored.generation.load(Ordering::SeqCst),
    )
}

/// Installs an isolated in-memory consent state for GPUI tests and `--snapshot` fixtures.
#[cfg(any(test, feature = "snapshot"))]
pub fn install_memory_voice_consent(
    choice: SavedAudioChoice,
    cleanup_granted: bool,
    cx: &mut App,
) {
    let mut state = VoiceConsentState::new();
    if let SavedAudioChoice::Granted(grant) = &choice {
        state.choose_audio_destination(Some(grant.id.clone()), Some(grant.destination.clone()));
        if cleanup_granted {
            state.choose_transcript_cleanup(Some(grant.id.clone()));
        }
    } else if matches!(choice, SavedAudioChoice::Off) {
        state.choose_audio_destination(None, None);
    }
    let generation = cx
        .try_global::<StoredVoiceConsent>()
        .map(|existing| {
            existing.generation.fetch_add(1, Ordering::SeqCst);
            existing.generation.clone()
        })
        .unwrap_or_else(|| Arc::new(AtomicU64::new(1)));
    cx.set_global(StoredVoiceConsent {
        state,
        persist_to_disk: false,
        generation,
    });
}

pub fn voice_audio_choice(cx: &App) -> SavedAudioChoice {
    cx.try_global::<StoredVoiceConsent>()
        .map(|stored| stored.state.active_audio_choice())
        .unwrap_or_else(|| StoredVoiceConsent::load(None).state.active_audio_choice())
}

pub fn voice_cleanup_granted(cx: &App) -> bool {
    let check = |stored: &StoredVoiceConsent| {
        stored
            .state
            .active_grant()
            .is_some_and(|g| stored.state.cleanup_grant() == Some(g.id.as_str()))
    };
    if let Some(stored) = cx.try_global::<StoredVoiceConsent>() {
        check(stored)
    } else {
        check(&StoredVoiceConsent::load(None))
    }
}

pub fn choose_voice_destination(
    destination: Option<VoiceTranscriptionDestination>,
    enable_cleanup: bool,
    cx: &mut App,
) -> Option<AudioGrant> {
    ensure_consent(cx);
    let grant_id = destination.as_ref().map(|_| uuid::Uuid::new_v4().to_string());
    let stored = cx.global_mut::<StoredVoiceConsent>();
    stored.state.choose_audio_destination(grant_id.clone(), destination);
    if let Some(id) = grant_id.filter(|_| enable_cleanup) {
        stored.state.choose_transcript_cleanup(Some(id));
    } else {
        stored.state.choose_transcript_cleanup(None);
    }
    stored.generation.fetch_add(1, Ordering::SeqCst);
    stored.save();
    stored.state.active_grant()
}

pub fn set_voice_cleanup_enabled(enabled: bool, cx: &mut App) {
    ensure_consent(cx);
    let stored = cx.global_mut::<StoredVoiceConsent>();
    let active_id = stored.state.active_grant().map(|g| g.id);
    stored
        .state
        .choose_transcript_cleanup(enabled.then_some(active_id).flatten());
    stored.generation.fetch_add(1, Ordering::SeqCst);
    stored.save();
}

pub fn begin_pending_cloud_choice(
    destination: VoiceTranscriptionDestination,
    cx: &mut App,
) -> String {
    ensure_consent(cx);
    let pending_id = uuid::Uuid::new_v4().to_string();
    let stored = cx.global_mut::<StoredVoiceConsent>();
    let id = stored.state.begin_pending_cloud_choice(pending_id, destination);
    stored.generation.fetch_add(1, Ordering::SeqCst);
    id
}

pub fn acknowledge_pending_cloud_choice(pending_id: &str, cx: &mut App) {
    ensure_consent(cx);
    let stored = cx.global_mut::<StoredVoiceConsent>();
    let had_cleanup = stored
        .state
        .active_grant()
        .is_some_and(|g| stored.state.cleanup_grant() == Some(g.id.as_str()));
    stored.state.acknowledge_pending_cloud_choice(pending_id);
    if had_cleanup {
        stored
            .state
            .choose_transcript_cleanup(Some(pending_id.to_owned()));
    }
    stored.generation.fetch_add(1, Ordering::SeqCst);
    stored.save();
}

pub fn fail_pending_cloud_choice(pending_id: &str, cx: &mut App) {
    ensure_consent(cx);
    let stored = cx.global_mut::<StoredVoiceConsent>();
    stored.state.fail_pending_cloud_choice(pending_id);
    stored.generation.fetch_add(1, Ordering::SeqCst);
}

pub fn list_voice_candidates(state: &AppState) -> Vec<VoiceEnvironmentCandidate> {
    let mut candidates = Vec::new();
    let active_env_id = state
        .server_config
        .config
        .as_ref()
        .map(|c| c.environment.environment_id.as_str());

    for view in &state.fleet.environments {
        if view.status != ConnectionStatus::Connected {
            continue;
        }
        let Some(cfg) = view.config.config.as_ref() else {
            continue;
        };
        let local = cfg.environment.capabilities.voice_transcription == Some(true);
        let cloud = cfg.environment.capabilities.cloud_voice_transcription == Some(true);
        if !local && !cloud {
            continue;
        }
        let saved_id = &view.summary.owner.saved_id;
        let Some(credentials) = state
            .environments
            .get(saved_id)
            .and_then(|saved| saved.credentials())
            .or_else(|| {
                state
                    .client_handle()
                    .filter(|_| active_env_id == Some(cfg.environment.environment_id.as_str()))
                    .map(|client| client.credentials().clone())
            })
        else {
            continue;
        };
        let label = if !cfg.environment.label.trim().is_empty() {
            cfg.environment.label.clone()
        } else {
            saved_id.clone()
        };
        let env_id = cfg.environment.environment_id.clone();
        if candidates
            .iter()
            .any(|existing: &VoiceEnvironmentCandidate| existing.environment_id == env_id)
        {
            continue;
        }
        candidates.push(VoiceEnvironmentCandidate {
            is_active: active_env_id == Some(env_id.as_str()),
            environment_id: env_id,
            saved_id: saved_id.clone(),
            label,
            credentials,
            local_transcription: local,
            cloud_transcription: cloud,
        });
    }

    // Single-client fallback when no fleet summary has populated yet (e.g. fixture environments).
    if candidates.is_empty()
        && let (Some(client), Some(cfg)) = (state.client_handle(), state.server_config.config.as_ref())
    {
        let local = cfg.environment.capabilities.voice_transcription == Some(true);
        let cloud = cfg.environment.capabilities.cloud_voice_transcription == Some(true);
        if local || cloud {
            candidates.push(VoiceEnvironmentCandidate {
                environment_id: cfg.environment.environment_id.clone(),
                saved_id: state
                    .environments
                    .active()
                    .map(|e| e.id.clone())
                    .unwrap_or_else(|| cfg.environment.environment_id.clone()),
                label: cfg.environment.label.clone(),
                credentials: client.credentials().clone(),
                local_transcription: local,
                cloud_transcription: cloud,
                is_active: true,
            });
        }
    }

    // Sort active environment first, preserving fleet order among others.
    candidates.sort_by_key(|c| !c.is_active);
    candidates
}

pub fn resolve_voice_destination(state: &AppState, cx: &App) -> VoiceDestinationResolution {
    let candidates = list_voice_candidates(state);
    let stored = cx.try_global::<StoredVoiceConsent>();
    let fallback;
    let stored = match stored {
        Some(stored) => stored,
        None => {
            fallback = StoredVoiceConsent::load(None);
            &fallback
        }
    };
    let choice = stored.state.active_audio_choice();
    let offer_first_run_setup =
        matches!(choice, SavedAudioChoice::Unset) && !candidates.is_empty();

    let active = match &choice {
        SavedAudioChoice::Granted(grant) => {
            let env_id = grant.destination.environment_id();
            candidates
                .iter()
                .find(|candidate| candidate.environment_id == env_id || candidate.saved_id == env_id)
                .and_then(|candidate| {
                    let capable = match &grant.destination {
                        VoiceTranscriptionDestination::Environment { .. } => {
                            candidate.local_transcription
                        }
                        VoiceTranscriptionDestination::Cloud { .. } => {
                            candidate.cloud_transcription
                        }
                    };
                    let cleanup_grant_id = stored
                        .state
                        .cleanup_grant()
                        .filter(|id| *id == grant.id)
                        .map(str::to_owned);
                    capable.then(|| ResolvedVoiceDestination {
                        grant_id: grant.id.clone(),
                        destination: grant.destination.clone(),
                        credentials: candidate.credentials.clone(),
                        environment_label: candidate.label.clone(),
                        cleanup_granted: cleanup_grant_id.is_some(),
                        cleanup_grant_id,
                    })
                })
        }
        SavedAudioChoice::Unset | SavedAudioChoice::Off => None,
    };

    VoiceDestinationResolution {
        offer_first_run_setup,
        active,
    }
}

/// Resolves credentials for optional transcript post-processing on the draft's own environment
/// (never the audio destination unless they are the same environment).
pub fn resolve_cleanup_credentials(
    state: &AppState,
    draft_env_id: Option<&str>,
) -> Option<Credentials> {
    let target_env_id = draft_env_id
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            state
                .server_config
                .config
                .as_ref()
                .map(|c| c.environment.environment_id.clone())
        });
    let target_id = target_env_id.as_deref();

    for view in &state.fleet.environments {
        if view.status != ConnectionStatus::Connected {
            continue;
        }
        let Some(cfg) = view.config.config.as_ref() else {
            continue;
        };
        let env_id = cfg.environment.environment_id.as_str();
        let saved_id = view.summary.owner.saved_id.as_str();
        let matches = target_id.is_none_or(|id| id == env_id || id == saved_id);
        if !matches {
            continue;
        }
        let supports_voice = cfg.environment.capabilities.voice_transcription == Some(true)
            || cfg.environment.capabilities.cloud_voice_transcription == Some(true);
        let can_operate = view
            .grants
            .as_ref()
            .is_none_or(|session| session.grants(ORCHESTRATION_OPERATE_SCOPE));
        if !supports_voice || !can_operate {
            return None;
        }
        return state
            .environments
            .get(saved_id)
            .and_then(|saved| saved.credentials())
            .or_else(|| {
                state
                    .client_handle()
                    .map(|client| client.credentials().clone())
            });
    }

    if let (Some(client), Some(cfg)) = (state.client_handle(), state.server_config.config.as_ref()) {
        let env_id = cfg.environment.environment_id.as_str();
        let matches = target_id.is_none_or(|id| id == env_id);
        if !matches {
            return None;
        }
        let supports_voice = cfg.environment.capabilities.voice_transcription == Some(true)
            || cfg.environment.capabilities.cloud_voice_transcription == Some(true);
        let can_operate = client
            .session_state()
            .borrow()
            .as_ref()
            .is_none_or(|session| session.grants(ORCHESTRATION_OPERATE_SCOPE));
        if supports_voice && can_operate {
            return Some(client.credentials().clone());
        }
    }

    None
}
