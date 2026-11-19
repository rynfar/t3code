pub mod consent;
pub mod controller;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod platform;
pub mod shortcuts;
pub mod ui;

#[cfg(test)]
mod tests;

#[cfg(feature = "snapshot")]
pub use consent::install_memory_voice_consent;
pub use consent::{
    acknowledge_pending_cloud_choice, begin_pending_cloud_choice, choose_voice_destination,
    fail_pending_cloud_choice, list_voice_candidates, set_voice_cleanup_enabled,
    voice_audio_choice, voice_cleanup_granted,
};
#[cfg(feature = "snapshot")]
pub use controller::{VoiceSetupState, VoiceSetupStep};
pub use controller::{
    BackgroundVoiceTarget, DraftReader, StoredDraftCommit, VoiceControllerEvent,
    VoiceSettingsHandler, VoiceSpeechConfig, VoiceSpeechController,
};
#[cfg(feature = "snapshot")]
pub use platform::{MockAudioPlatform, install_audio_platform};
pub use platform::{
    AudioCaptureCallbacks, AudioCaptureSession, AudioPlaybackSession, MicPermissionStatus,
    audio_platform,
};
pub use shortcuts::{
    Dictation, handle_dictation_action, handle_escape, handle_key_up, handle_modifiers_changed,
    handle_setup_escape, handle_window_blur, is_setup_modal_open, record_key_down,
    register_dictation_target, unregister_dictation_target,
};
pub use ui::{
    VoiceUiSize, render_active_voice_setup_modal, render_speech_button,
    render_speech_recording_pill, render_text_field_speech_controls, render_voice_preview,
    render_waveform,
};
