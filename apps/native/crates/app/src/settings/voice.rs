//! Voice settings (`ST-VOICE`): ports `VoiceSettingsPanel.tsx`, `CloudVoiceConfiguration.tsx`,
//! `MicrophoneOrderList.tsx`, `MicrophoneTest.tsx`, `SpeechLanguageSelect.tsx`,
//! `SpeechModelComparison.tsx`, and `speechModelBrowser.ts`.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    ParentElement, SharedString, Styled, Subscription, Task, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{Sizable, h_flex, v_flex};
use t3_client::auth::Credentials;
use t3_client::supervisor::ConnectionStatus;
use t3_client::voice::{CloudSpeechKeyInput, VoiceClient};
use t3_core::client_settings::ClientSettingChange;
use t3_core::device::hosts::SecretValue;
use t3_core::settings::voice::{
    LOOPBACK_SECONDS, MicrophoneTestPhase, ModelSpeedSummary, SHORTCUT_MODES,
    SpeechModelFilterOptions, SpeechModelSort, UNLOAD_TIMEOUT_OPTIONS, add_dictionary_alias,
    add_dictionary_terms, can_add_dictionary_terms, cloud_speech_language_list,
    filter_speech_models_with_recommendations, format_loopback_time, model_benchmark_display,
    order_speech_languages, parse_unload_timeout, remove_dictionary_term,
    resolve_speech_language_display, shortcut_mode_meta, unload_timeout_label,
    validate_cloud_voice_config, voice_search_target_fallback,
};
use t3_core::settings::{
    ServerSettingChange, resolve_default_text_generation_selection,
    speech_post_processing_model_selection,
};
use t3_core::voice_input::{
    AudioDeviceInfo, CloudSpeechKeys, CloudSpeechModel, CloudSpeechProvider,
    DEFAULT_SPEECH_POST_PROCESSING_PROMPT, EnvironmentSpeechModel, EnvironmentSpeechModelState,
    EnvironmentSpeechStatus, SPEECH_SAMPLE_RATE, SavedAudioChoice, SpeechAcceleration,
    SpeechBenchmarkTier, SpeechCustomWordEntry, SpeechPostProcessingPromptMode,
    SpeechPostProcessingPromptSettings, SpeechRecommendationRole, VoiceDraftSnapshot,
    VoiceInputPhase, VoiceSelection, VoiceShortcutMode, VoiceTranscriptionDestination,
    cloud_speech_disclosure, forget_microphone, microphone_entry_key, microphone_order,
    move_microphone, preferred_microphone, preferred_microphone_id, preferred_speech_languages,
    recommend_speech_models, remember_microphones, speech_language_label,
};

use super::controls::{
    Button, ButtonSize, ButtonVariant, DialogFrame, Row, Select, SelectOption, Switch,
    dialog_footer, icon, muted_text, r, section,
};
use super::threads::{model_trait_controls, parse_selection, selection_value};
use super::{Block, SettingsView, store};
use crate::components::secret_source::SecretSourceInput;
use crate::i18n::t;
use crate::icons::Icon;
use crate::provider_icon::{ProviderIcon, ProviderMark};
use crate::theme::{ThemeSlot, ThemeSlotExt, hsla, palette, theme_slot_clipped_background_layer};
use crate::voice::{
    AudioCaptureCallbacks, AudioCaptureSession, AudioPlaybackSession, MicPermissionStatus,
    VoiceControllerEvent, VoiceSpeechConfig, VoiceSpeechController, VoiceUiSize,
    acknowledge_pending_cloud_choice, audio_platform, begin_pending_cloud_choice,
    choose_voice_destination, fail_pending_cloud_choice, list_voice_candidates,
    render_speech_recording_pill, render_waveform, set_voice_cleanup_enabled, voice_audio_choice,
    voice_cleanup_granted,
};

const OFF: &str = "off";

#[derive(Debug, Clone)]
enum CloudLoad<T> {
    Loading,
    Loaded(T),
    Failed(String),
}

struct CloudVoiceDialog {
    environment_id: String,
    environment_label: String,
    credentials: Credentials,
    provider: CloudSpeechProvider,
    active: bool,
    can_manage: bool,
    current_model: Option<String>,
    selected_model: String,
    model_search: Entity<InputState>,
    secret_source: Option<String>,
    key_input: Entity<InputState>,
    keys_state: CloudLoad<CloudSpeechKeys>,
    models_state: CloudLoad<Vec<CloudSpeechModel>>,
    remove_confirm: bool,
    busy: bool,
    error: Option<String>,
    focus: FocusHandle,
    seq: u64,
    _subscriptions: Vec<Subscription>,
}

pub(crate) struct VoiceState {
    devices: Vec<AudioDeviceInfo>,
    devices_loaded: bool,

    // MicrophoneTest (`MicrophoneTest.tsx`)
    mic_test_phase: MicrophoneTestPhase,
    mic_test_error: Option<String>,
    mic_test_samples: Vec<f32>,
    mic_test_pcm: Option<Arc<Mutex<Vec<f32>>>>,
    mic_test_device_id: Option<String>,
    mic_test_capture: Option<Box<dyn AudioCaptureSession>>,
    mic_test_playback: Option<Box<dyn AudioPlaybackSession>>,
    _mic_test_task: Option<Task<()>>,
    _mic_playback_task: Option<Task<()>>,

    // SpeechModelBrowser (`VoiceSettingsPanel.tsx`)
    catalog_env_id: Option<String>,
    catalog_status: Option<EnvironmentSpeechStatus>,
    catalog_models: Vec<EnvironmentSpeechModel>,
    catalog_busy: bool,
    catalog_requested_download: Option<String>,
    catalog_epoch: u64,
    catalog_poll_active: bool,
    _catalog_poll_task: Option<Task<()>>,
    model_search: Entity<InputState>,
    model_sort: SpeechModelSort,
    model_downloaded_only: bool,
    model_selected_languages: Option<Vec<String>>,
    model_language_search: Entity<InputState>,
    model_languages_open: bool,
    model_link_path: Entity<InputState>,
    advanced_open: bool,

    // TranscriptionTest (`VoiceSettingsPanel.tsx`)
    model_test_controller: Option<Entity<VoiceSpeechController>>,
    model_test_transcript: Option<String>,
    model_test_open: bool,
    _model_test_sub: Option<Subscription>,

    // Dictionary editors (personal + project/shared)
    personal_term: Entity<InputState>,
    personal_alias_target: Option<String>,
    personal_alias_input: Entity<InputState>,
    project_term: Entity<InputState>,
    project_alias_target: Option<String>,
    project_alias_input: Entity<InputState>,

    // Cleanup instructions & correction cue
    custom_prompt: Entity<TextareaState>,
    custom_prompt_saved: Option<String>,
    correction_word: Entity<InputState>,

    // CloudVoiceConfiguration modal
    cloud_dialog: Option<CloudVoiceDialog>,
    _subscriptions: Vec<Subscription>,
}

impl VoiceState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let model_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models"));
        let model_language_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search languages"));
        let model_link_path = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Already downloaded elsewhere? Paste the model file or folder path")
        });
        let personal_term = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Add words, separated by commas")
        });
        let personal_alias_input = cx.new(|cx| InputState::new(window, cx).placeholder("Misspelling"));
        let project_term = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Add words, separated by commas")
        });
        let project_alias_input = cx.new(|cx| InputState::new(window, cx).placeholder("Misspelling"));
        let custom_prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 10)
                .placeholder("Custom cleanup instructions")
        });
        let correction_word = cx.new(|cx| InputState::new(window, cx).placeholder("sorry"));

        let subscriptions = vec![
            cx.subscribe(&model_language_search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&model_search, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&model_link_path, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&personal_term, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.add_personal_dictionary_terms(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&personal_alias_input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.commit_personal_alias(window, cx),
                InputEvent::Blur => {
                    this.voice.personal_alias_target = None;
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe_in(&project_term, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.add_project_dictionary_terms(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&project_alias_input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.commit_project_alias(window, cx),
                InputEvent::Blur => {
                    this.voice.project_alias_target = None;
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe_in(&custom_prompt, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::Change) {
                    this.commit_voice_custom_prompt(cx);
                }
            }),
            cx.subscribe_in(&correction_word, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_voice_correction_word(cx);
                }
            }),
        ];

        Self {
            devices: Vec::new(),
            devices_loaded: false,
            mic_test_phase: MicrophoneTestPhase::Idle,
            mic_test_error: None,
            mic_test_samples: Vec::new(),
            mic_test_pcm: None,
            mic_test_device_id: None,
            mic_test_capture: None,
            mic_test_playback: None,
            _mic_test_task: None,
            _mic_playback_task: None,
            catalog_env_id: None,
            catalog_status: None,
            catalog_models: Vec::new(),
            catalog_busy: false,
            catalog_requested_download: None,
            catalog_epoch: 0,
            catalog_poll_active: false,
            _catalog_poll_task: None,
            model_search,
            model_sort: SpeechModelSort::Recommended,
            model_downloaded_only: false,
            model_selected_languages: None,
            model_language_search,
            model_languages_open: false,
            model_link_path,
            advanced_open: false,
            model_test_controller: None,
            model_test_transcript: None,
            model_test_open: false,
            _model_test_sub: None,
            personal_term,
            personal_alias_target: None,
            personal_alias_input,
            project_term,
            project_alias_target: None,
            project_alias_input,
            custom_prompt,
            custom_prompt_saved: None,
            correction_word,
            cloud_dialog: None,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn leave(&mut self) {
        self.stop_mic_test_resources();
        self.mic_test_phase = MicrophoneTestPhase::Idle;
        self.mic_test_error = None;
        self.mic_test_samples.clear();
        self.catalog_poll_active = false;
        self._catalog_poll_task = None;
        self.cloud_dialog = None;
        self.personal_alias_target = None;
        self.project_alias_target = None;
        self.model_test_open = false;
    }

    pub(super) fn reveal(&mut self, target: &str) {
        if matches!(target, "speech-hardware" | "speech-streaming") {
            self.advanced_open = true;
        }
    }

    fn stop_mic_test_resources(&mut self) {
        if let Some(mut capture) = self.mic_test_capture.take() {
            let _ = capture.stop();
        }
        if let Some(mut playback) = self.mic_test_playback.take() {
            playback.stop();
        }
        self.mic_test_pcm = None;
        self._mic_test_task = None;
        self._mic_playback_task = None;
    }

    pub(super) fn inputs(&self) -> Vec<&Entity<InputState>> {
        let mut out = vec![
            &self.model_search,
            &self.model_language_search,
            &self.model_link_path,
            &self.personal_term,
            &self.personal_alias_input,
            &self.project_term,
            &self.project_alias_input,
            &self.correction_word,
        ];
        if let Some(dialog) = &self.cloud_dialog {
            out.push(&dialog.model_search);
            out.push(&dialog.key_input);
        }
        out
    }

    pub(super) fn textareas(&self) -> [&Entity<TextareaState>; 1] {
        [&self.custom_prompt]
    }
}

fn cloud_speech_icon(provider: CloudSpeechProvider, cx: &App) -> AnyElement {
    match provider {
        CloudSpeechProvider::Openai | CloudSpeechProvider::Xai => {
            let driver = if provider == CloudSpeechProvider::Openai { "codex" } else { "grok" };
            ProviderIcon::new(ProviderMark::configured(driver, provider.label(), None)).into_any_element()
        }
        CloudSpeechProvider::Openrouter => icon(Icon::Workflow, 16., hsla(palette(cx).foreground)).into_any_element(),
    }
}

fn system_preferred_languages() -> Vec<String> {
    let lang_env = std::env::var("LANG").ok();
    preferred_speech_languages(&[], lang_env.as_deref(), &["en"])
}

impl SettingsView {
    pub(super) fn leave_voice(&mut self, cx: &mut Context<Self>) {
        if let Some(ctrl) = self.voice.model_test_controller.as_ref() {
            ctrl.update(cx, |controller, cx| controller.cancel(cx));
        }
        self.voice.model_languages_open = false;
        self.voice.leave();
    }

    pub(super) fn dismiss_voice_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(dialog) = &self.voice.cloud_dialog {
            if dialog.busy {
                return true;
            }
            self.voice.cloud_dialog = None;
            self.focus.focus(window, cx);
            cx.notify();
            return true;
        }
        if self.voice.model_languages_open {
            self.voice.model_languages_open = false;
            self.focus.focus(window, cx);
            cx.notify();
            return true;
        }
        if self.voice.personal_alias_target.take().is_some()
            || self.voice.project_alias_target.take().is_some()
        {
            self.focus.focus(window, cx);
            cx.notify();
            return true;
        }
        false
    }

    pub(super) fn commit_focused_voice_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice.correction_word.focus_handle(cx).is_focused(window) {
            self.commit_voice_correction_word(cx);
        }
        if self.voice.custom_prompt.focus_handle(cx).is_focused(window) {
            self.commit_voice_custom_prompt(cx);
        }
    }

    pub(super) fn resolve_voice_target(&self, target: &str, cx: &App) -> String {
        let choice = voice_audio_choice(cx);
        let (has_local_prepared, has_cloud_provider) = match &choice {
            SavedAudioChoice::Granted(grant) => match &grant.destination {
                VoiceTranscriptionDestination::Environment { .. } => (true, false),
                VoiceTranscriptionDestination::Cloud { .. } => (false, true),
            },
            _ => (false, false),
        };
        let cleanup = voice_cleanup_granted(cx);
        let has_scope = self.scoped_representative_config(cx).is_some();
        voice_search_target_fallback(
            target,
            has_local_prepared,
            self.voice.catalog_status.as_ref(),
            has_cloud_provider,
            cleanup,
            has_scope,
        )
        .unwrap_or(target)
        .to_owned()
    }

    fn refresh_microphones(&mut self, cx: &mut Context<Self>) {
        let devices = audio_platform(cx).list_input_devices();
        let doc = store::client(cx);
        let current = microphone_order(&doc.voice_microphones(), doc.legacy_voice_microphone());
        let remembered = remember_microphones(&current, &devices);
        self.voice.devices = devices;
        self.voice.devices_loaded = true;
        if remembered != current {
            store::update_client(&self.state, ClientSettingChange::VoiceMicrophones(remembered), cx);
        }
        cx.notify();
    }

    fn start_mic_test(&mut self, device_id: String, cx: &mut Context<Self>) {
        self.voice.stop_mic_test_resources();
        self.voice.mic_test_error = None;
        self.voice.mic_test_samples.clear();
        self.voice.mic_test_device_id = Some(device_id.clone());
        self.voice.mic_test_phase = MicrophoneTestPhase::Starting;
        cx.notify();

        let pcm_buf = Arc::new(Mutex::new(Vec::<f32>::new()));
        self.voice.mic_test_pcm = Some(pcm_buf.clone());
        let latest_level = Arc::new(Mutex::new(0.0f32));
        let capture_err = Arc::new(Mutex::new(None::<String>));
        let limit_hit = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let level_cb = latest_level.clone();
        let pcm_cb = pcm_buf.clone();
        let err_cb = capture_err.clone();
        let limit_cb = limit_hit.clone();

        let callbacks = AudioCaptureCallbacks {
            on_level: Arc::new(move |level| {
                if let Ok(mut guard) = level_cb.lock() {
                    *guard = level;
                }
            }),
            on_pcm: Arc::new(move |samples| {
                if let Ok(mut guard) = pcm_cb.lock() {
                    let max_samples = (SPEECH_SAMPLE_RATE as usize) * (LOOPBACK_SECONDS as usize);
                    if guard.len() < max_samples {
                        let take = (max_samples - guard.len()).min(samples.len());
                        guard.extend_from_slice(&samples[..take]);
                    }
                }
            }),
            on_limit: Arc::new(move || {
                limit_cb.store(true, std::sync::atomic::Ordering::SeqCst);
            }),
            on_error: Arc::new(move |msg| {
                if let Ok(mut guard) = err_cb.lock() {
                    *guard = Some(msg);
                }
            }),
        };

        let platform = audio_platform(cx);
        match platform.start_capture(&device_id, false, callbacks) {
            Ok(session) => {
                self.refresh_microphones(cx);
                self.voice.mic_test_capture = Some(session);
                self.voice.mic_test_phase = MicrophoneTestPhase::Recording { elapsed_secs: 0.0, level: 0.0 };
                let started_at = Instant::now();
                let task = cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(Duration::from_millis(80)).await;
                        let elapsed = started_at.elapsed().as_secs_f32();
                        let level = latest_level.lock().map(|g| *g).unwrap_or(0.0);
                        let err = capture_err.lock().ok().and_then(|mut g| g.take());
                        let hit_limit = limit_hit.load(std::sync::atomic::Ordering::SeqCst)
                            || elapsed >= LOOPBACK_SECONDS as f32;
                        let keep_going = this
                            .update(cx, |this, cx| {
                                if !matches!(this.voice.mic_test_phase, MicrophoneTestPhase::Recording { .. }) {
                                    return false;
                                }
                                if let Some(err) = err {
                                    this.finish_mic_test_capture(Some(err), &pcm_buf, elapsed, cx);
                                    return false;
                                }
                                if hit_limit {
                                    this.finish_mic_test_capture(None, &pcm_buf, LOOPBACK_SECONDS as f32, cx);
                                    return false;
                                }
                                this.voice.mic_test_phase =
                                    MicrophoneTestPhase::Recording { elapsed_secs: elapsed, level };
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !keep_going {
                            break;
                        }
                    }
                });
                self.voice._mic_test_task = Some(task);
                cx.notify();
            }
            Err(err) => {
                let message = match err {
                    crate::voice::platform::CaptureStartError::PermissionDenied(m)
                    | crate::voice::platform::CaptureStartError::Unsupported(m)
                    | crate::voice::platform::CaptureStartError::Failed(m) => m,
                };
                self.voice.mic_test_error = Some(message);
                self.voice.mic_test_phase = MicrophoneTestPhase::Idle;
                cx.notify();
            }
        }
    }

    fn finish_mic_test_capture(
        &mut self,
        error: Option<String>,
        pcm_buf: &Arc<Mutex<Vec<f32>>>,
        elapsed_secs: f32,
        cx: &mut Context<Self>,
    ) {
        if let Some(mut capture) = self.voice.mic_test_capture.take() {
            let _ = capture.stop();
        }
        self.voice.mic_test_pcm = None;
        self.voice._mic_test_task = None;
        let samples = pcm_buf.lock().map(|g| g.clone()).unwrap_or_default();
        self.voice.mic_test_samples = samples;
        if let Some(err) = error {
            self.voice.mic_test_error = Some(err);
            self.voice.mic_test_phase = MicrophoneTestPhase::Idle;
            cx.notify();
            return;
        }
        if self.voice.mic_test_samples.is_empty() {
            self.voice.mic_test_error = Some("No audio was recorded. Try again.".to_owned());
            self.voice.mic_test_phase = MicrophoneTestPhase::Idle;
            cx.notify();
            return;
        }
        let duration_secs = (self.voice.mic_test_samples.len() as f32 / SPEECH_SAMPLE_RATE as f32)
            .max(elapsed_secs.min(LOOPBACK_SECONDS as f32));
        self.voice.mic_test_phase = MicrophoneTestPhase::Ready {
            duration_secs,
            playing: false,
            position_secs: 0.0,
            playback_failed: false,
        };
        cx.notify();
    }

    fn stop_mic_test_recording(&mut self, cx: &mut Context<Self>) {
        let elapsed = match self.voice.mic_test_phase {
            MicrophoneTestPhase::Recording { elapsed_secs, .. } => elapsed_secs,
            _ => return,
        };
        if let Some(mut capture) = self.voice.mic_test_capture.take() {
            let _ = capture.stop();
        }
        if let Some(pcm_buf) = self.voice.mic_test_pcm.take()
            && let Ok(guard) = pcm_buf.lock()
            && !guard.is_empty()
        {
            self.voice.mic_test_samples = guard.clone();
        }
        self.voice._mic_test_task = None;
        if self.voice.mic_test_samples.is_empty() {
            self.voice.mic_test_error = Some("No audio was recorded. Try again.".to_owned());
            self.voice.mic_test_phase = MicrophoneTestPhase::Idle;
        } else {
            let duration_secs = (self.voice.mic_test_samples.len() as f32 / SPEECH_SAMPLE_RATE as f32)
                .max(elapsed);
            self.voice.mic_test_phase = MicrophoneTestPhase::Ready {
                duration_secs,
                playing: false,
                position_secs: 0.0,
                playback_failed: false,
            };
        }
        cx.notify();
    }

    fn cancel_mic_test(&mut self, cx: &mut Context<Self>) {
        self.voice.stop_mic_test_resources();
        self.voice.mic_test_samples.clear();
        self.voice.mic_test_error = None;
        self.voice.mic_test_phase = MicrophoneTestPhase::Idle;
        cx.notify();
    }

    fn toggle_mic_test_playback(&mut self, cx: &mut Context<Self>) {
        let MicrophoneTestPhase::Ready { duration_secs, playing, position_secs, .. } =
            self.voice.mic_test_phase
        else {
            return;
        };
        if playing {
            if let Some(mut session) = self.voice.mic_test_playback.take() {
                session.stop();
            }
            self.voice._mic_playback_task = None;
            self.voice.mic_test_phase = MicrophoneTestPhase::Ready {
                duration_secs,
                playing: false,
                position_secs,
                playback_failed: false,
            };
            cx.notify();
            return;
        }
        let start_secs = if position_secs >= duration_secs - 0.05 { 0.0 } else { position_secs };
        let start_sample = ((start_secs * SPEECH_SAMPLE_RATE as f32) as usize)
            .min(self.voice.mic_test_samples.len());
        let pos_samples = Arc::new(std::sync::atomic::AtomicUsize::new(start_sample));
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pos_cb = pos_samples.clone();
        let end_cb = ended.clone();
        match audio_platform(cx).start_playback(
            self.voice.mic_test_samples.clone(),
            start_sample,
            Arc::new(move |sample_idx| {
                pos_cb.store(sample_idx, std::sync::atomic::Ordering::SeqCst);
            }),
            Arc::new(move || {
                end_cb.store(true, std::sync::atomic::Ordering::SeqCst);
            }),
        ) {
            Ok(session) => {
                self.voice.mic_test_playback = Some(session);
                self.voice.mic_test_phase = MicrophoneTestPhase::Ready {
                    duration_secs,
                    playing: true,
                    position_secs: start_secs,
                    playback_failed: false,
                };
                let task = cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(Duration::from_millis(60)).await;
                        let sample_idx = pos_samples.load(std::sync::atomic::Ordering::SeqCst);
                        let is_ended = ended.load(std::sync::atomic::Ordering::SeqCst);
                        let keep_going = this
                            .update(cx, |this, cx| {
                                let MicrophoneTestPhase::Ready {
                                    duration_secs,
                                    playing: true,
                                    ..
                                } = this.voice.mic_test_phase
                                else {
                                    return false;
                                };
                                let pos = (sample_idx as f32 / SPEECH_SAMPLE_RATE as f32)
                                    .min(duration_secs);
                                if is_ended {
                                    this.voice.mic_test_playback = None;
                                    this.voice.mic_test_phase = MicrophoneTestPhase::Ready {
                                        duration_secs,
                                        playing: false,
                                        position_secs: duration_secs,
                                        playback_failed: false,
                                    };
                                    cx.notify();
                                    return false;
                                }
                                this.voice.mic_test_phase = MicrophoneTestPhase::Ready {
                                    duration_secs,
                                    playing: true,
                                    position_secs: pos,
                                    playback_failed: false,
                                };
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !keep_going {
                            break;
                        }
                    }
                });
                self.voice._mic_playback_task = Some(task);
                cx.notify();
            }
            Err(_) => {
                self.voice.mic_test_phase = MicrophoneTestPhase::Ready {
                    duration_secs,
                    playing: false,
                    position_secs,
                    playback_failed: true,
                };
                cx.notify();
            }
        }
    }

    fn seek_mic_test_playback(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let MicrophoneTestPhase::Ready { duration_secs, playing, .. } = self.voice.mic_test_phase
        else {
            return;
        };
        let next_pos = (fraction.clamp(0.0, 1.0) * duration_secs).min(duration_secs);
        if playing {
            if let Some(mut session) = self.voice.mic_test_playback.take() {
                session.stop();
            }
            self.voice._mic_playback_task = None;
        }
        self.voice.mic_test_phase = MicrophoneTestPhase::Ready {
            duration_secs,
            playing: false,
            position_secs: next_pos,
            playback_failed: false,
        };
        if playing {
            self.toggle_mic_test_playback(cx);
        } else {
            cx.notify();
        }
    }

    fn sync_voice_catalog(
        &mut self,
        env_id: Option<&str>,
        credentials: Option<Credentials>,
        cx: &mut Context<Self>,
    ) {
        if self.voice.catalog_env_id.as_deref() != env_id {
            self.voice.catalog_env_id = env_id.map(str::to_owned);
            self.voice.catalog_status = None;
            self.voice.catalog_models.clear();
            self.voice.catalog_busy = false;
            self.voice.catalog_requested_download = None;
            self.voice.catalog_epoch += 1;
            self.voice.catalog_poll_active = false;
            self.voice._catalog_poll_task = None;
            if let (Some(id), Some(creds)) = (env_id, credentials) {
                self.fetch_voice_catalog(id.to_owned(), creds, true, cx);
            }
        }
    }

    fn fetch_voice_catalog(
        &mut self,
        env_id: String,
        credentials: Credentials,
        foreground: bool,
        cx: &mut Context<Self>,
    ) {
        let epoch = self.voice.catalog_epoch;
        let runtime = self.state.read(cx).runtime().clone();
        let task = runtime.spawn(async move {
            let client = VoiceClient::new(credentials).map_err(|e| e.to_string())?;
            let status = client.status().await.map_err(|e| e.to_string())?;
            let models = client.models(foreground).await.map_err(|e| e.to_string())?;
            Ok::<_, String>((status, models.models))
        });

        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Could not load transcription models.".to_owned()));
            let _ = this.update(cx, |this, cx| {
                if this.voice.catalog_epoch != epoch
                    || this.voice.catalog_env_id.as_deref() != Some(env_id.as_str())
                {
                    return;
                }
                match result {
                    Ok((status, models)) => {
                        this.voice.catalog_status = Some(status);
                        if foreground {
                            this.voice.catalog_models = models;
                        } else {
                            let prev_benchmarks: std::collections::HashMap<_, _> = this
                                .voice
                                .catalog_models
                                .iter()
                                .filter_map(|m| m.benchmark.clone().map(|b| (m.id.clone(), b)))
                                .collect();
                            this.voice.catalog_models = models
                                .into_iter()
                                .map(|mut m| {
                                    if m.benchmark.is_none()
                                        && let Some(b) = prev_benchmarks.get(&m.id)
                                    {
                                        m.benchmark = Some(b.clone());
                                    }
                                    m
                                })
                                .collect();
                        }
                        this.ensure_catalog_download_poll(cx);
                        cx.notify();
                    }
                    Err(err) if foreground => {
                        store::show_error(format!("Could not load transcription models: {err}"), cx);
                    }
                    Err(_) => {}
                }
            });
        })
        .detach();
    }

    fn ensure_catalog_download_poll(&mut self, cx: &mut Context<Self>) {
        let downloading = self.voice.catalog_requested_download.is_some()
            || self.voice.catalog_models.iter().any(|m| {
                matches!(
                    m.state,
                    EnvironmentSpeechModelState::Downloading | EnvironmentSpeechModelState::Verifying
                )
            });
        if !downloading {
            self.voice.catalog_poll_active = false;
            self.voice._catalog_poll_task = None;
            return;
        }
        if self.voice.catalog_poll_active {
            return;
        }
        let Some(env_id) = self.voice.catalog_env_id.clone() else {
            return;
        };
        let Some(creds) = list_voice_candidates(self.state.read(cx))
            .into_iter()
            .find(|c| c.environment_id == env_id)
            .map(|c| c.credentials)
        else {
            return;
        };
        self.voice.catalog_poll_active = true;
        let epoch = self.voice.catalog_epoch;
        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep = this
                    .update(cx, |this, cx| {
                        if this.voice.catalog_epoch != epoch
                            || !this.voice.catalog_poll_active
                            || this.voice.catalog_env_id.as_deref() != Some(env_id.as_str())
                        {
                            return false;
                        }
                        this.fetch_voice_catalog(env_id.clone(), creds.clone(), false, cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
        self.voice._catalog_poll_task = Some(task);
    }

    fn run_voice_model_action(
        &mut self,
        env_id: String,
        credentials: Credentials,
        error_title: &'static str,
        download_id: Option<String>,
        work: impl FnOnce(VoiceClient) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
            + Send
            + 'static,
        cx: &mut Context<Self>,
    ) {
        let epoch = self.voice.catalog_epoch;
        if let Some(id) = download_id.clone() {
            self.voice.catalog_requested_download = Some(id);
            self.ensure_catalog_download_poll(cx);
        } else {
            self.voice.catalog_busy = true;
        }
        cx.notify();

        let runtime = self.state.read(cx).runtime().clone();
        let refresh_creds = credentials.clone();
        let task = runtime.spawn(async move {
            let client = VoiceClient::new(credentials).map_err(|e| e.to_string())?;
            work(client).await
        });

        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Operation cancelled.".to_owned()));
            let _ = this.update(cx, |this, cx| {
                if this.voice.catalog_epoch != epoch {
                    return;
                }
                if download_id.is_some() {
                    this.voice.catalog_requested_download = None;
                } else {
                    this.voice.catalog_busy = false;
                }
                if let Err(err) = result {
                    store::show_error(format!("{error_title}: {err}"), cx);
                }
                this.fetch_voice_catalog(env_id, refresh_creds, true, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn ensure_model_test_controller(
        &mut self,
        env_id: &str,
        cx: &mut Context<Self>,
    ) -> Entity<VoiceSpeechController> {
        let owner_key = format!("voice-settings-model-test:{env_id}");
        if let Some(existing) = self.voice.model_test_controller.clone()
            && existing.read(cx).owner_key() == owner_key
        {
            return existing;
        }
        let app = self.state.clone();
        let weak_view = cx.entity().downgrade();
        let env_owned = env_id.to_owned();
        let controller = cx.new(|cx| {
            let mut ctrl = VoiceSpeechController::new(
                VoiceSpeechConfig {
                    app,
                    owner_key,
                    offer_setup: true,
                    draft_reader: Rc::new(move |cx| {
                        let text = weak_view
                            .upgrade()
                            .and_then(|v| v.read(cx).voice.model_test_transcript.clone())
                            .unwrap_or_default();
                        let len = text.len();
                        Some(VoiceDraftSnapshot {
                            owner_key: "voice-settings-transcription-test".to_owned(),
                            text,
                            selection: VoiceSelection { start: len, end: len },
                        })
                    }),
                    hints_reader: Rc::new(|_| Vec::new()),
                    project_id_reader: Rc::new(|_| None),
                    environment_id_reader: Rc::new(move |_| Some(env_owned.clone())),
                    on_open_voice_settings: Rc::new(|_, _| {}),
                },
                cx,
            );
            ctrl.set_cleanup_override(Some(false));
            ctrl
        });
        let sub = cx.subscribe(&controller, |this, _, event: &VoiceControllerEvent, cx| match event {
            VoiceControllerEvent::StateChanged => cx.notify(),
            VoiceControllerEvent::CommitDraft { text, .. } => {
                this.voice.model_test_transcript = Some(text.clone());
                cx.notify();
            }
            VoiceControllerEvent::SubmitAfterDictation { .. } => {}
        });
        self.voice.model_test_controller = Some(controller.clone());
        self.voice._model_test_sub = Some(sub);
        controller
    }

    #[allow(clippy::too_many_arguments)]
    fn open_cloud_dialog(
        &mut self,
        environment_id: String,
        environment_label: String,
        credentials: Credentials,
        provider: CloudSpeechProvider,
        active: bool,
        can_manage: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current_model = if active {
            match voice_audio_choice(cx) {
                SavedAudioChoice::Granted(grant) => match grant.destination {
                    VoiceTranscriptionDestination::Cloud {
                        provider: CloudSpeechProvider::Openrouter,
                        model,
                        ..
                    } => model,
                    _ => None,
                },
                _ => None,
            }
        } else {
            None
        };
        let model_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search transcription models")
        });
        let key_input = cx.new(|cx| InputState::new(window, cx).placeholder("Enter API key").masked(true));
        let subscriptions = vec![
            cx.subscribe_in(&model_search, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&key_input, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let seq = self.voice.cloud_dialog.as_ref().map_or(1, |d| d.seq + 1);
        self.voice.cloud_dialog = Some(CloudVoiceDialog {
            environment_id,
            environment_label,
            credentials: credentials.clone(),
            provider,
            active,
            can_manage,
            selected_model: current_model.clone().unwrap_or_default(),
            current_model,
            model_search,
            secret_source: None,
            key_input,
            keys_state: CloudLoad::Loading,
            models_state: if provider == CloudSpeechProvider::Openrouter {
                CloudLoad::Loading
            } else {
                CloudLoad::Loaded(Vec::new())
            },
            remove_confirm: false,
            busy: false,
            error: None,
            focus,
            seq,
            _subscriptions: subscriptions,
        });
        self.load_cloud_dialog_keys(credentials.clone(), seq, cx);
        if provider == CloudSpeechProvider::Openrouter {
            self.load_cloud_dialog_models(credentials, seq, cx);
        }
        cx.notify();
    }

    fn load_cloud_dialog_keys(&mut self, credentials: Credentials, seq: u64, cx: &mut Context<Self>) {
        if let Some(d) = self.voice.cloud_dialog.as_mut().filter(|d| d.seq == seq) {
            d.keys_state = CloudLoad::Loading;
        }
        let runtime = self.state.read(cx).runtime().clone();
        let task = runtime.spawn(async move {
            let client = VoiceClient::new(credentials).map_err(|e| e.to_string())?;
            client.cloud_keys().await.map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let res = task.await.unwrap_or_else(|_| Err("Could not check API key.".to_owned()));
            let _ = this.update(cx, |this, cx| {
                if let Some(d) = this.voice.cloud_dialog.as_mut().filter(|d| d.seq == seq) {
                    d.keys_state = match res {
                        Ok(keys) => CloudLoad::Loaded(keys),
                        Err(e) => CloudLoad::Failed(e),
                    };
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn load_cloud_dialog_models(&mut self, credentials: Credentials, seq: u64, cx: &mut Context<Self>) {
        if let Some(d) = self.voice.cloud_dialog.as_mut().filter(|d| d.seq == seq) {
            d.models_state = CloudLoad::Loading;
        }
        let runtime = self.state.read(cx).runtime().clone();
        let task = runtime.spawn(async move {
            let client = VoiceClient::new(credentials).map_err(|e| e.to_string())?;
            client.cloud_models().await.map(|m| m.models).map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let res = task.await.unwrap_or_else(|_| Err("Could not load models.".to_owned()));
            let _ = this.update(cx, |this, cx| {
                if let Some(d) = this.voice.cloud_dialog.as_mut().filter(|d| d.seq == seq) {
                    d.models_state = match res {
                        Ok(models) => CloudLoad::Loaded(models),
                        Err(e) => CloudLoad::Failed(e),
                    };
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn submit_cloud_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.voice.cloud_dialog.as_ref() else {
            return;
        };
        if dialog.busy {
            return;
        }
        let routed = dialog.provider == CloudSpeechProvider::Openrouter;
        let saved_key = match &dialog.keys_state {
            CloudLoad::Loaded(keys) => match dialog.provider {
                CloudSpeechProvider::Openai => Some(keys.openai),
                CloudSpeechProvider::Xai => Some(keys.xai),
                CloudSpeechProvider::Openrouter => keys.openrouter,
            },
            _ => None,
        };
        let key_draft = dialog.key_input.read(cx).value().trim().to_owned();
        let listed_models = match &dialog.models_state {
            CloudLoad::Loaded(models) => Some(models.as_slice()),
            _ => None,
        };
        let secret_ref_valid = dialog
            .secret_source
            .as_deref()
            .is_some_and(|src| SecretValue::reference(src, &key_draft).is_some());
        let validation = validate_cloud_voice_config(
            dialog.provider,
            dialog.active,
            saved_key,
            &key_draft,
            dialog.secret_source.is_some(),
            secret_ref_valid,
            &dialog.selected_model,
            dialog.current_model.as_deref(),
            listed_models,
        );
        if !validation.can_submit {
            return;
        }
        let destination = if routed {
            VoiceTranscriptionDestination::openrouter(
                dialog.environment_id.clone(),
                dialog.selected_model.clone(),
            )
        } else {
            VoiceTranscriptionDestination::cloud(dialog.environment_id.clone(), dialog.provider)
        };
        let model_changed = dialog.selected_model != dialog.current_model.as_deref().unwrap_or("");
        let grant_changes = !dialog.active || model_changed;

        if validation.key_valid {
            let key_input = if let Some(src) = dialog.secret_source.clone() {
                CloudSpeechKeyInput::secret_reference(src, key_draft)
            } else {
                match CloudSpeechKeyInput::api_key(&key_draft) {
                    Ok(k) => k,
                    Err(e) => {
                        if let Some(d) = self.voice.cloud_dialog.as_mut() {
                            d.error = Some(e.to_string());
                        }
                        cx.notify();
                        return;
                    }
                }
            };
            self.save_cloud_dialog_key(key_input, grant_changes.then_some(destination), cx);
        } else {
            if grant_changes {
                let cleanup = voice_cleanup_granted(cx);
                choose_voice_destination(Some(destination), cleanup, cx);
            }
            self.voice.cloud_dialog = None;
            cx.notify();
        }
    }

    fn save_cloud_dialog_key(
        &mut self,
        key_input: CloudSpeechKeyInput,
        enable_destination: Option<VoiceTranscriptionDestination>,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.voice.cloud_dialog.as_mut() else {
            return;
        };
        if dialog.busy || !dialog.can_manage {
            return;
        }
        dialog.busy = true;
        dialog.error = None;
        let credentials = dialog.credentials.clone();
        let provider = dialog.provider;
        let seq = dialog.seq;
        let pending_id = enable_destination.map(|dest| begin_pending_cloud_choice(dest, cx));
        cx.notify();

        let runtime = self.state.read(cx).runtime().clone();
        let task = runtime.spawn(async move {
            let client = VoiceClient::new(credentials).map_err(|e| e.to_string())?;
            client
                .set_cloud_key(provider, key_input)
                .await
                .map_err(|_| "Couldn\u{2019}t save this change. Check your connection and try again.".to_owned())
        });
        cx.spawn(async move |this, cx| {
            let res = task.await.unwrap_or_else(|_| {
                Err("Couldn\u{2019}t save this change. Check your connection and try again.".to_owned())
            });
            let _ = this.update(cx, |this, cx| match res {
                Ok(()) => {
                    if let Some(id) = pending_id.as_deref() {
                        acknowledge_pending_cloud_choice(id, cx);
                    }
                    if this.voice.cloud_dialog.as_ref().is_some_and(|d| d.seq == seq) {
                        this.voice.cloud_dialog = None;
                    }
                    cx.notify();
                }
                Err(msg) => {
                    if let Some(id) = pending_id.as_deref() {
                        fail_pending_cloud_choice(id, cx);
                    }
                    if let Some(d) = this.voice.cloud_dialog.as_mut().filter(|d| d.seq == seq) {
                        d.busy = false;
                        d.error = Some(msg);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn add_personal_dictionary_terms(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.voice.personal_term.read(cx).value().to_string();
        let words = store::client(cx).speech_custom_words();
        if let Some(next) = add_dictionary_terms(&words, &draft) {
            store::update_client(&self.state, ClientSettingChange::SpeechCustomWords(next), cx);
            self.voice.personal_term.update(cx, |input, cx| input.set_value("", window, cx));
            cx.notify();
        }
    }

    fn commit_personal_alias(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(term) = self.voice.personal_alias_target.clone() else {
            return;
        };
        let draft = self.voice.personal_alias_input.read(cx).value().to_string();
        let words = store::client(cx).speech_custom_words();
        if let Some(next) = add_dictionary_alias(&words, &term, &draft) {
            store::update_client(&self.state, ClientSettingChange::SpeechCustomWords(next), cx);
        }
        self.voice.personal_alias_target = None;
        self.voice.personal_alias_input.update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn add_project_dictionary_terms(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.voice.project_term.read(cx).value().to_string();
        let words = self.current_project_dictionary_words(cx);
        if let Some(next) = add_dictionary_terms(&words, &draft) {
            self.save_project_dictionary_words(next, cx);
            self.voice.project_term.update(cx, |input, cx| input.set_value("", window, cx));
            cx.notify();
        }
    }

    fn commit_project_alias(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(term) = self.voice.project_alias_target.clone() else {
            return;
        };
        let draft = self.voice.project_alias_input.read(cx).value().to_string();
        let words = self.current_project_dictionary_words(cx);
        if let Some(next) = add_dictionary_alias(&words, &term, &draft) {
            self.save_project_dictionary_words(next, cx);
        }
        self.voice.project_alias_target = None;
        self.voice.project_alias_input.update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn current_project_dictionary_words(&self, cx: &App) -> Vec<SpeechCustomWordEntry> {
        let Some(config) = self.scoped_raw_representative_config(cx) else {
            return Vec::new();
        };
        config
            .settings
            .project_speech_custom_words(self.scope_project_id(cx).as_deref())
    }

    fn save_project_dictionary_words(&self, words: Vec<SpeechCustomWordEntry>, cx: &mut App) {
        self.update_scoped_server(ServerSettingChange::SpeechProjectCustomWords(words), cx);
    }

    fn commit_voice_custom_prompt(&mut self, cx: &mut Context<Self>) {
        let next = self.voice.custom_prompt.read(cx).value().to_string();
        let current = store::client(cx).speech_post_processing_prompt();
        if current.mode == SpeechPostProcessingPromptMode::Custom && current.custom_instructions != next {
            self.voice.custom_prompt_saved = Some(next.clone());
            store::update_client(
                &self.state,
                ClientSettingChange::SpeechPostProcessingPrompt(SpeechPostProcessingPromptSettings {
                    mode: SpeechPostProcessingPromptMode::Custom,
                    custom_instructions: next,
                }),
                cx,
            );
        }
    }

    fn commit_voice_correction_word(&mut self, cx: &mut Context<Self>) {
        let next = self.voice.correction_word.read(cx).value().trim().to_owned();
        if next != store::client(cx).speech_correction_word() {
            store::update_client(&self.state, ClientSettingChange::SpeechCorrectionWord(next), cx);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_dictionary_editor(
        &self,
        id_prefix: &'static str,
        words: &[SpeechCustomWordEntry],
        term_input: &Entity<InputState>,
        alias_target: Option<&str>,
        alias_input: &Entity<InputState>,
        disabled: bool,
        on_add: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
        on_start_alias: impl Fn(&mut SettingsView, String, &mut Window, &mut Context<SettingsView>) + 'static,
        on_remove: impl Fn(&mut SettingsView, String, &mut Context<SettingsView>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let draft = term_input.read(cx).value().to_string();
        let can_add = can_add_dictionary_terms(words, &draft, disabled);
        let on_add = Rc::new(on_add);
        let on_start_alias = Rc::new(on_start_alias);
        let on_remove = Rc::new(on_remove);

        let input_row = h_flex()
            .gap(r(8.))
            .items_center()
            .child(div().flex_1().min_w_0().child(Input::new(term_input).small()))
            .child(
                Button::new(SharedString::from(format!("{id_prefix}-add")), ButtonVariant::Outline, ButtonSize::Sm)
                    .icon(Icon::Plus)
                    .label("Add")
                    .disabled(!can_add)
                    .on_click(cx.listener({
                        let on_add = on_add.clone();
                        move |this, _, window, cx| on_add(this, window, cx)
                    })),
            );

        let chips = (!words.is_empty()).then(|| {
            h_flex()
                .flex_wrap()
                .gap(r(6.))
                .mt(r(10.))
                .children(words.iter().enumerate().map(|(idx, entry)| {
                    let term = entry.term.clone();
                    let aliases = entry.aliases.clone();
                    let editing_alias = alias_target == Some(term.as_str());
                    let start_term = term.clone();
                    let remove_term = term.clone();
                    let on_start = on_start_alias.clone();
                    let on_rem = on_remove.clone();
                    h_flex()
                        .gap(r(6.))
                        .px(r(8.))
                        .py(r(3.))
                        .rounded(r(6.))
                        .border_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .bg(hsla(p.muted).opacity(0.35))
                        .text_xs()
                        .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(term))
                        .when(!aliases.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(format!("\u{2190} {}", aliases.join(", "))),
                            )
                        })
                        .child(if editing_alias {
                            div().w(r(120.)).child(Input::new(alias_input).small()).into_any_element()
                        } else {
                            Button::new(
                                SharedString::from(format!("{id_prefix}-alias-{idx}")),
                                ButtonVariant::GhostMuted,
                                ButtonSize::Xs,
                            )
                            .label("Alias")
                            .disabled(disabled || aliases.len() >= 8)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                on_start(this, start_term.clone(), window, cx);
                            }))
                            .into_any_element()
                        })
                        .child(
                            Button::new(
                                SharedString::from(format!("{id_prefix}-remove-{idx}")),
                                ButtonVariant::GhostMuted,
                                ButtonSize::IconMicro,
                            )
                            .icon(Icon::X)
                            .disabled(disabled)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                on_rem(this, remove_term.clone(), cx);
                            })),
                        )
                }))
        });

        v_flex()
            .mt(r(10.))
            .pb(r(8.))
            .child(input_row)
            .children(chips)
            .into_any_element()
    }

    pub(super) fn voice_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        if !self.voice.devices_loaded {
            self.refresh_microphones(cx);
        }
        let p = palette(cx);
        let doc = store::client(cx).clone();
        let state_entity = self.state.clone();
        let choice = voice_audio_choice(cx);
        let cleanup_granted = voice_cleanup_granted(cx);
        let candidates = list_voice_candidates(self.state.read(cx));

        // Keep correction word and custom prompt inputs synced without clobbering focused edits.
        Self::sync_input(&self.voice.correction_word, doc.speech_correction_word(), window, cx);
        let prompt_settings = doc.speech_post_processing_prompt();
        if self.voice.custom_prompt_saved.as_deref() != Some(prompt_settings.custom_instructions.as_str())
            && !self.voice.custom_prompt.focus_handle(cx).is_focused(window)
        {
            self.voice.custom_prompt_saved = Some(prompt_settings.custom_instructions.clone());
            let val = prompt_settings.custom_instructions.clone();
            self.voice.custom_prompt.update(cx, |t, cx| t.set_value(val, window, cx));
        }

        // Resolve active destination details
        let (dest_env_id, dest_cloud_provider) = match &choice {
            SavedAudioChoice::Granted(grant) => match &grant.destination {
                VoiceTranscriptionDestination::Environment { environment_id } => {
                    (Some(environment_id.clone()), None)
                }
                VoiceTranscriptionDestination::Cloud { environment_id, provider, .. } => {
                    (Some(environment_id.clone()), Some(*provider))
                }
            },
            SavedAudioChoice::Unset | SavedAudioChoice::Off => (None, None),
        };
        let chosen_candidate = dest_env_id
            .as_deref()
            .and_then(|id| candidates.iter().find(|c| c.environment_id == id || c.saved_id == id))
            .cloned();
        let local_prepared = (dest_cloud_provider.is_none())
            .then(|| {
                chosen_candidate
                    .as_ref()
                    .filter(|c| c.local_transcription)
                    .map(|c| (c.environment_id.clone(), c.credentials.clone(), c.label.clone()))
            })
            .flatten();
        self.sync_voice_catalog(
            local_prepared.as_ref().map(|(id, _, _)| id.as_str()),
            local_prepared.as_ref().map(|(_, creds, _)| creds.clone()),
            cx,
        );

        // -------------------------------------------------------------------
        // Section 1: Input (MicrophoneOrderList + MicrophoneTest + Shortcut)
        // -------------------------------------------------------------------
        let mic_order = microphone_order(&doc.voice_microphones(), doc.legacy_voice_microphone());
        let selected_mic_id = preferred_microphone_id(&mic_order, &self.voice.devices);
        let in_use_key = microphone_entry_key(&preferred_microphone(&mic_order, &self.voice.devices));
        let devices_known = self.voice.devices.iter().any(|d| !d.device_id.is_empty());
        let perm_status = audio_platform(cx).permission_status();

        if self.voice.mic_test_device_id.as_deref().is_some_and(|id| id != selected_mic_id) {
            self.voice.stop_mic_test_resources();
            self.voice.mic_test_samples.clear();
            self.voice.mic_test_error = None;
            self.voice.mic_test_phase = MicrophoneTestPhase::Idle;
            self.voice.mic_test_device_id = Some(selected_mic_id.clone());
        }

        let mic_list = v_flex()
            .mt(r(10.))
            .mb(r(8.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.7))
            .children(mic_order.iter().enumerate().map(|(index, entry)| {
                let key = microphone_entry_key(entry);
                let live_device = match entry {
                    t3_core::voice_input::VoiceMicrophoneEntry::Device { device_id, .. } => {
                        self.voice.devices.iter().find(|d| &d.device_id == device_id)
                    }
                    t3_core::voice_input::VoiceMicrophoneEntry::Default => None,
                };
                let name = match entry {
                    t3_core::voice_input::VoiceMicrophoneEntry::Default => "System default".to_owned(),
                    t3_core::voice_input::VoiceMicrophoneEntry::Device { label, .. } => {
                        if !label.is_empty() {
                            label.clone()
                        } else if let Some(d) = live_device.filter(|d| !d.label.is_empty()) {
                            d.label.clone()
                        } else {
                            "Saved microphone".to_owned()
                        }
                    }
                };
                let is_in_use = key == in_use_key;
                let not_connected = matches!(
                    entry,
                    t3_core::voice_input::VoiceMicrophoneEntry::Device { .. }
                ) && devices_known
                    && live_device.is_none();
                let is_device = matches!(entry, t3_core::voice_input::VoiceMicrophoneEntry::Device { .. });
                let count = mic_order.len();
                let up_order = mic_order.clone();
                let down_order = mic_order.clone();
                let forget_order = mic_order.clone();
                let up_key = key.clone();
                let down_key = key.clone();
                let forget_key = key.clone();
                let st_up = state_entity.clone();
                let st_down = state_entity.clone();
                let st_forget = state_entity.clone();

                h_flex()
                    .px(r(12.))
                    .py(r(6.))
                    .gap(r(8.))
                    .items_center()
                    .when(index > 0, |el| el.border_t_1().border_color(hsla(p.border).opacity(0.5)))
                    .child(
                        div()
                            .w(r(16.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(format!("{}", index + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(hsla(p.foreground))
                            .truncate()
                            .child(name),
                    )
                    .when(is_in_use, |el| {
                        el.child(
                            div()
                                .px(r(6.))
                                .py(r(2.))
                                .rounded(r(4.))
                                .bg(hsla(p.success).opacity(0.14))
                                .text_size(r(11.))
                                .text_color(hsla(p.success_foreground))
                                .child("In use"),
                        )
                    })
                    .when(!is_in_use && not_connected, |el| {
                        el.child(
                            div()
                                .text_size(r(11.))
                                .text_color(hsla(p.muted_foreground))
                                .child("Not connected"),
                        )
                    })
                    .child(
                        h_flex()
                            .gap(r(2.))
                            .child(
                                Button::new(
                                    SharedString::from(format!("mic-up-{index}")),
                                    ButtonVariant::GhostMuted,
                                    ButtonSize::IconXs,
                                )
                                .icon(Icon::ArrowUp)
                                .disabled(index == 0)
                                .on_click(move |_, _, cx| {
                                    let next = move_microphone(&up_order, &up_key, -1);
                                    store::update_client(
                                        &st_up,
                                        ClientSettingChange::VoiceMicrophones(next),
                                        cx,
                                    );
                                }),
                            )
                            .child(
                                Button::new(
                                    SharedString::from(format!("mic-down-{index}")),
                                    ButtonVariant::GhostMuted,
                                    ButtonSize::IconXs,
                                )
                                .icon(Icon::ArrowDown)
                                .disabled(index + 1 >= count)
                                .on_click(move |_, _, cx| {
                                    let next = move_microphone(&down_order, &down_key, 1);
                                    store::update_client(
                                        &st_down,
                                        ClientSettingChange::VoiceMicrophones(next),
                                        cx,
                                    );
                                }),
                            )
                            .child(if is_device {
                                Button::new(
                                    SharedString::from(format!("mic-forget-{index}")),
                                    ButtonVariant::GhostMuted,
                                    ButtonSize::IconXs,
                                )
                                .icon(Icon::X)
                                .on_click(move |_, _, cx| {
                                    let next = forget_microphone(&forget_order, &forget_key);
                                    store::update_client(
                                        &st_forget,
                                        ClientSettingChange::VoiceMicrophones(next),
                                        cx,
                                    );
                                })
                                .into_any_element()
                            } else {
                                div().size(r(24.)).into_any_element()
                            }),
                    )
            }));

        let mic_controls = h_flex()
            .gap(r(6.))
            .when(
                matches!(perm_status, MicPermissionStatus::Denied | MicPermissionStatus::Restricted),
                |el| {
                    el.child(
                        Button::new("voice-open-mic-settings", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("System Settings")
                            .on_click(|_, _, cx| {
                                audio_platform(cx).open_microphone_settings();
                            }),
                    )
                },
            )
            .child(
                Button::new("voice-refresh-mics", ButtonVariant::Outline, ButtonSize::IconSm)
                    .icon(Icon::RefreshCw)
                    .tooltip("Refresh microphones")
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_microphones(cx))),
            );

        let mic_row = Row::new("voice-microphone", "Microphone")
            .description("Voice input uses the first connected microphone in this list.")
            .highlighted(self.is_target("voice-microphone"))
            .control(mic_controls)
            .below(mic_list);

        // MicrophoneTest row
        let mic_test_device = selected_mic_id.clone();
        let mic_test_control = match &self.voice.mic_test_phase {
            MicrophoneTestPhase::Recording { .. } | MicrophoneTestPhase::Starting => {
                let starting = matches!(self.voice.mic_test_phase, MicrophoneTestPhase::Starting);
                Button::new("mic-test-stop", ButtonVariant::Outline, ButtonSize::Sm)
                    .icon(Icon::Square)
                    .label("Stop")
                    .disabled(starting)
                    .on_click(cx.listener(|this, _, _, cx| this.stop_mic_test_recording(cx)))
            }
            MicrophoneTestPhase::Ready { .. } => {
                Button::new("mic-test-again", ButtonVariant::Outline, ButtonSize::Sm)
                    .icon(Icon::RotateCcw)
                    .label("Record again")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_mic_test(mic_test_device.clone(), cx);
                    }))
            }
            MicrophoneTestPhase::Idle => {
                Button::new("mic-test-record", ButtonVariant::Outline, ButtonSize::Sm)
                    .icon(Icon::Mic)
                    .label("Record")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_mic_test(mic_test_device.clone(), cx);
                    }))
            }
        };

        let mic_test_panel: Option<AnyElement> = if let Some(err) = &self.voice.mic_test_error {
            Some(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(hsla(p.destructive_foreground))
                    .child(err.clone())
                    .into_any_element(),
            )
        } else {
            match &self.voice.mic_test_phase {
                MicrophoneTestPhase::Idle => None,
                MicrophoneTestPhase::Starting => Some(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child("Opening microphone\u{2026}")
                        .into_any_element(),
                ),
                MicrophoneTestPhase::Recording { elapsed_secs, level } => Some(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(r(10.))
                        .items_center()
                        .child(div().size(r(6.)).rounded_full().bg(hsla(p.destructive)))
                        .child(render_waveform(*level, false, cx))
                        .child(
                            div()
                                .ml_auto()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(format!(
                                    "{} / {}",
                                    format_loopback_time(*elapsed_secs),
                                    format_loopback_time(LOOPBACK_SECONDS as f32)
                                )),
                        )
                        .into_any_element(),
                ),
                MicrophoneTestPhase::Ready {
                    duration_secs,
                    playing,
                    position_secs,
                    playback_failed,
                } => {
                    let progress = self.voice.mic_test_phase.progress_fraction();
                    Some(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(r(10.))
                            .items_center()
                            .child(
                                Button::new("mic-test-play", ButtonVariant::Outline, ButtonSize::IconSm)
                                    .icon(if *playing { Icon::Square } else { Icon::Play })
                                    .tooltip(if *playing { "Pause recording" } else { "Play recording" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_mic_test_playback(cx);
                                    })),
                            )
                            .child(if *playback_failed {
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(hsla(p.destructive_foreground))
                                    .child("Could not play this recording. Try recording again.")
                                    .into_any_element()
                            } else {
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(r(2.))
                                    .items_center()
                                    .children((0..20usize).map(|step| {
                                        let frac = (step as f32 + 0.5) / 20.0;
                                        let filled = frac <= progress;
                                        div()
                                            .id(("mic-test-scrub", step))
                                            .flex_1()
                                            .h(r(6.))
                                            .rounded(r(2.))
                                            .cursor_pointer()
                                            .bg(if filled {
                                                hsla(p.primary)
                                            } else {
                                                hsla(p.muted)
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.seek_mic_test_playback(frac, cx);
                                            }))
                                    }))
                                    .into_any_element()
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(format!(
                                        "{} / {}",
                                        format_loopback_time(*position_secs),
                                        format_loopback_time(*duration_secs)
                                    )),
                            )
                            .into_any_element(),
                    )
                }
            }
        };

        let mut mic_test_row = Row::new("microphone-test", "Test microphone")
            .description("Record ten seconds and play it back. The audio stays on this device.")
            .highlighted(self.is_target("microphone-test"))
            .control(mic_test_control);
        if let Some(panel) = mic_test_panel {
            mic_test_row = mic_test_row.below(
                h_flex()
                    .mt(r(10.))
                    .mb(r(8.))
                    .px(r(10.))
                    .py(r(6.))
                    .gap(r(8.))
                    .items_center()
                    .rounded(r(8.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(hsla(p.muted).opacity(0.3))
                    .child(panel)
                    .child(
                        Button::new("mic-test-cancel", ButtonVariant::GhostMuted, ButtonSize::IconSm)
                            .icon(Icon::X)
                            .tooltip("Cancel microphone test")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_mic_test(cx))),
                    ),
            );
        }

        // Dictation shortcut mode row
        let current_shortcut = doc.voice_shortcut_mode();
        let (shortcut_label, shortcut_hint) = shortcut_mode_meta(current_shortcut);
        let shortcut_state = state_entity.clone();
        let shortcut_row = Row::new("dictation-shortcut-mode", "Dictation shortcut")
            .description(format!("{shortcut_hint}."))
            .highlighted(self.is_target("dictation-shortcut-mode"))
            .control(
                Select::new(
                    "voice-shortcut-select",
                    SHORTCUT_MODES
                        .iter()
                        .map(|(mode, label, hint)| {
                            SelectOption::new(mode.as_str(), *label).description(*hint)
                        })
                        .collect(),
                )
                .selected(current_shortcut.as_str())
                .label(shortcut_label)
                .width(180.)
                .on_change(move |value, _, cx| {
                    if let Some(mode) = VoiceShortcutMode::parse(&value) {
                        store::update_client(
                            &shortcut_state,
                            ClientSettingChange::VoiceShortcutMode(mode),
                            cx,
                        );
                    }
                }),
            );

        let input_section = section(
            "Input",
            vec![
                mic_row.into_any_element(),
                mic_test_row.into_any_element(),
                shortcut_row.into_any_element(),
            ],
            cx,
        );

        // -------------------------------------------------------------------
        // Section 2: Transcription
        // -------------------------------------------------------------------
        let mut dest_options = vec![SelectOption::new(OFF, "Off")];
        let mut env_dest_map: Vec<(String, String)> = Vec::new();
        let mut cloud_dest_map: Vec<(String, String, String, Credentials, CloudSpeechProvider)> = Vec::new();

        for candidate in &candidates {
            let key = format!("env:{}", candidate.environment_id);
            dest_options.push(
                SelectOption::new(key.clone(), candidate.label.clone()).group("On an environment"),
            );
            env_dest_map.push((key, candidate.environment_id.clone()));
        }
        for candidate in &candidates {
            if !candidate.cloud_transcription {
                continue;
            }
            let group_label: SharedString = format!("Cloud via {}", candidate.label).into();
            for &provider in CloudSpeechProvider::ALL {
                let key = format!("cloud:{}:{}", candidate.environment_id, provider.as_str());
                dest_options.push(
                    SelectOption::new(key.clone(), provider.label()).group(group_label.clone()),
                );
                cloud_dest_map.push((
                    key,
                    candidate.environment_id.clone(),
                    candidate.label.clone(),
                    candidate.credentials.clone(),
                    provider,
                ));
            }
        }
        let selected_dest_value = match (dest_env_id.as_deref(), dest_cloud_provider) {
            (Some(env_id), Some(provider)) => format!("cloud:{env_id}:{}", provider.as_str()),
            (Some(env_id), None) => format!("env:{env_id}"),
            _ => OFF.to_owned(),
        };
        let selected_dest_label = match (dest_env_id.as_deref(), dest_cloud_provider) {
            (Some(_), Some(provider)) => format!(
                "{} via {}",
                provider.label(),
                chosen_candidate
                    .as_ref()
                    .map(|c| c.label.as_str())
                    .unwrap_or("unavailable environment")
            ),
            (Some(_), None) => chosen_candidate
                .as_ref()
                .map(|c| c.label.clone())
                .unwrap_or_else(|| "Chosen environment (unavailable)".to_owned()),
            _ => "Off".to_owned(),
        };
        if selected_dest_value != OFF
            && !dest_options.iter().any(|opt| opt.value.as_ref() == selected_dest_value)
        {
            dest_options.push(SelectOption::new(
                selected_dest_value.clone(),
                "Chosen environment (unavailable)",
            ));
        }

        let chosen_unsupported = dest_cloud_provider.is_none()
            && chosen_candidate.as_ref().is_some_and(|c| !c.local_transcription);
        let dest_desc = if chosen_unsupported {
            format!(
                "{} can\u{2019}t transcribe audio locally. Choose another destination.",
                chosen_candidate.as_ref().map(|c| c.label.as_str()).unwrap_or("This environment")
            )
        } else {
            "Where your speech is turned into text.".to_owned()
        };

        let can_manage_dest = self.can_write_settings_on(dest_env_id.as_deref(), cx);
        let weak_dest = cx.entity().downgrade();
        let active_cloud_for_manage = dest_cloud_provider.zip(chosen_candidate.clone());
        let dest_control = h_flex()
            .gap(r(6.))
            .child(
                Select::new("voice-destination-select", dest_options)
                    .selected(selected_dest_value)
                    .label(selected_dest_label)
                    .width(240.)
                    .popup_width(280.)
                    .render_icon(|value, cx| {
                        let provider = value.strip_prefix("cloud:")?.rsplit_once(':')?.1;
                        CloudSpeechProvider::ALL.iter().copied().find(|item| item.as_str() == provider)
                            .map(|provider| cloud_speech_icon(provider, cx))
                    })
                    .on_change(move |value, window, cx| {
                        let _ = weak_dest.update(cx, |this, cx| {
                            if value.as_ref() == OFF {
                                choose_voice_destination(None, false, cx);
                                cx.notify();
                                return;
                            }
                            if let Some((_, env_id, label, creds, provider)) =
                                cloud_dest_map.iter().find(|(k, ..)| k == value.as_ref())
                            {
                                let can_manage = this.can_write_settings_on(Some(env_id.as_str()), cx);
                                let is_active = dest_env_id.as_deref() == Some(env_id.as_str())
                                    && dest_cloud_provider == Some(*provider);
                                this.open_cloud_dialog(
                                    env_id.clone(),
                                    label.clone(),
                                    creds.clone(),
                                    *provider,
                                    is_active,
                                    can_manage,
                                    window,
                                    cx,
                                );
                                return;
                            }
                            if let Some((_, env_id)) =
                                env_dest_map.iter().find(|(k, _)| k == value.as_ref())
                            {
                                let cleanup = voice_cleanup_granted(cx);
                                choose_voice_destination(
                                    Some(VoiceTranscriptionDestination::environment(env_id.clone())),
                                    cleanup,
                                    cx,
                                );
                                cx.notify();
                            }
                        });
                    }),
            )
            .when_some(
                active_cloud_for_manage.filter(|(provider, _)| {
                    can_manage_dest || *provider == CloudSpeechProvider::Openrouter
                }),
                |el, (provider, cand)| {
                    let manage_label = if provider == CloudSpeechProvider::Openrouter {
                        "Manage OpenRouter"
                    } else {
                        "Manage key"
                    };
                    el.child(
                        Button::new("voice-manage-cloud", ButtonVariant::Ghost, ButtonSize::IconSm)
                            .icon(Icon::KeyRound)
                            .tooltip(manage_label)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_cloud_dialog(
                                    cand.environment_id.clone(),
                                    cand.label.clone(),
                                    cand.credentials.clone(),
                                    provider,
                                    true,
                                    can_manage_dest,
                                    window,
                                    cx,
                                );
                            })),
                    )
                },
            );

        let mut transcription_rows = vec![
            Row::new("voice-transcription-destination", "Transcribe with")
                .description(dest_desc)
                .highlighted(self.is_target("voice-transcription-destination"))
                .control(dest_control)
                .into_any_element(),
        ];

        // SpeechLanguageSelect row
        let local_supported_status = local_prepared
            .as_ref()
            .and(self.voice.catalog_status.as_ref())
            .filter(|s| s.is_supported())
            .cloned();

        if dest_cloud_provider.is_some() || local_supported_status.is_some() {
            let (lang_codes, supports_detection, lang_desc) = if dest_cloud_provider.is_some() {
                (
                    cloud_speech_language_list(),
                    true,
                    "The provider detects your language automatically.".to_owned(),
                )
            } else if let Some(EnvironmentSpeechStatus::Supported {
                model,
                languages,
                supports_language_detection,
                ..
            }) = &local_supported_status
            {
                let detect = supports_language_detection.unwrap_or(false);
                let desc = if detect {
                    format!("{model} detects the language automatically.")
                } else {
                    format!("{model} uses one fixed language.")
                };
                (languages.clone().unwrap_or_default(), detect, desc)
            } else {
                (Vec::new(), false, String::new())
            };
            let pref = doc.speech_language();
            let display = resolve_speech_language_display(&lang_codes, supports_detection, pref);
            let mut lang_opts = vec![SelectOption::new("auto", "Auto")];
            for (code, label) in display.sorted_languages {
                lang_opts.push(SelectOption::new(code, label));
            }
            let lang_state = state_entity.clone();
            transcription_rows.push(
                Row::new("speech-language", "Language")
                    .description(lang_desc)
                    .highlighted(self.is_target("speech-language"))
                    .control(
                        Select::new("speech-language-select", lang_opts)
                            .selected(pref)
                            .label(display.trigger_label)
                            .width(200.)
                            .on_change(move |value, _, cx| {
                                store::update_client(
                                    &lang_state,
                                    ClientSettingChange::SpeechLanguage(value.to_string()),
                                    cx,
                                );
                            }),
                    )
                    .into_any_element(),
            );
        }

        // Translate to English row
        if matches!(
            local_supported_status,
            Some(EnvironmentSpeechStatus::Supported { supports_translation: true, .. })
        ) {
            let tr_state = state_entity.clone();
            transcription_rows.push(
                Row::new("speech-translate-to-english", "Translate to English")
                    .description("Speak any language and get English text.")
                    .control(
                        Switch::new("speech-translate-switch", doc.speech_translate_to_english())
                            .on_change(move |checked, _, cx| {
                                store::update_client(
                                    &tr_state,
                                    ClientSettingChange::SpeechTranslateToEnglish(checked),
                                    cx,
                                );
                            }),
                    )
                    .into_any_element(),
            );
        }

        // Silence filtering row
        let openai_cloud = dest_cloud_provider == Some(CloudSpeechProvider::Openai);
        let vad_state = state_entity.clone();
        transcription_rows.push(
            Row::new("speech-silence-filtering", "Silence filtering")
                .description(if openai_cloud {
                    "OpenAI handles silence itself."
                } else {
                    "Skip silence and background noise before transcribing."
                })
                .highlighted(self.is_target("speech-silence-filtering"))
                .control(
                    Switch::new("speech-silence-switch", doc.speech_silence_filtering())
                        .disabled(openai_cloud)
                        .on_change(move |checked, _, cx| {
                            store::update_client(
                                &vad_state,
                                ClientSettingChange::SpeechSilenceFiltering(checked),
                                cx,
                            );
                        }),
                )
                .into_any_element(),
        );

        // Remove filler words row
        let filler_state = state_entity.clone();
        transcription_rows.push(
            Row::new("remove-filler-words", "Remove filler words")
                .description("Drop \u{201c}um\u{201d} and \u{201c}uh\u{201d} from transcripts.")
                .highlighted(self.is_target("remove-filler-words"))
                .control(
                    Switch::new("speech-filler-switch", doc.speech_remove_filler_words())
                        .on_change(move |checked, _, cx| {
                            store::update_client(
                                &filler_state,
                                ClientSettingChange::SpeechRemoveFillerWords(checked),
                                cx,
                            );
                        }),
                )
                .into_any_element(),
        );

        // SpeechModelBrowser & AdvancedSpeechSettings (when local environment is prepared)
        if let Some((env_id, creds, _)) = local_prepared.clone()
            && let Some(status) = self.voice.catalog_status.clone()
        {
            match status {
                    EnvironmentSpeechStatus::Unsupported { reason } => {
                        transcription_rows.push(
                            Row::new("speech-models", "Models")
                                .description(format!(
                                    "This environment can\u{2019}t transcribe audio: {reason}."
                                ))
                                .highlighted(self.is_target("speech-models"))
                                .into_any_element(),
                        );
                    }
                    EnvironmentSpeechStatus::Supported {
                        supports_streaming,
                        benchmark_tier,
                        supports_model_linking,
                        acceleration,
                        model_unload_timeout,
                        gpu_devices,
                        ..
                    } => {
                        let browser_row = self.render_speech_model_browser(
                            &env_id,
                            &creds,
                            Some(benchmark_tier),
                            supports_model_linking == Some(true),
                            cx,
                        );
                        transcription_rows.push(browser_row);

                        // Collapsible Advanced (`speech-streaming` + `speech-hardware` + `Unload model`)
                        let adv_open = self.voice.advanced_open
                            || self.is_target("speech-hardware")
                            || self.is_target("speech-streaming");
                        transcription_rows.push(
                            div()
                                .px(r(16.))
                                .py(r(8.))
                                .child(
                                    Button::new("voice-advanced-toggle", ButtonVariant::GhostMuted, ButtonSize::Sm)
                                        .icon(if adv_open {
                                            Icon::ChevronDown
                                        } else {
                                            Icon::ChevronRight
                                        })
                                        .label("Advanced")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.voice.advanced_open = !this.voice.advanced_open;
                                            if this.is_target("speech-hardware")
                                                || this.is_target("speech-streaming")
                                            {
                                                this.target = None;
                                            }
                                            cx.notify();
                                        })),
                                )
                                .into_any_element(),
                        );

                        if adv_open {
                            let streaming_state = self.state.clone();
                            let streaming_checked = store::client(cx).speech_streaming();
                            transcription_rows.push(
                                Row::new("speech-streaming", "Streaming transcription")
                                    .description(if supports_streaming {
                                        "Transcribe while recording. Turn off to transcribe after stopping, which may help when live transcription falls behind."
                                    } else {
                                        "This model doesn\u{2019}t support native streaming."
                                    })
                                    .highlighted(self.is_target("speech-streaming"))
                                    .control(
                                        Switch::new("speech-streaming-switch", streaming_checked)
                                            .disabled(!supports_streaming)
                                            .on_change(move |checked, _, cx| {
                                                store::update_client(
                                                    &streaming_state,
                                                    ClientSettingChange::SpeechStreaming(checked),
                                                    cx,
                                                );
                                            }),
                                    )
                                    .into_any_element(),
                            );

                            let can_manage = self.can_write_settings_on(Some(&env_id), cx);
                            let controls_disabled = !can_manage
                                || self.voice.catalog_busy
                                || self.voice.catalog_requested_download.is_some();

                            let mut accel_options = vec![
                                SelectOption::new("auto", "Auto"),
                                SelectOption::new("cpu", "CPU"),
                            ];
                            for dev in &gpu_devices {
                                accel_options.push(SelectOption::new(
                                    format!("gpu:{}", dev.id),
                                    dev.name.clone(),
                                ));
                            }
                            let accel_label = match &acceleration {
                                SpeechAcceleration::Auto => "Auto".to_owned(),
                                SpeechAcceleration::Cpu => "CPU".to_owned(),
                                SpeechAcceleration::Gpu(id) => gpu_devices
                                    .iter()
                                    .find(|d| format!("gpu:{}", d.id) == *id)
                                    .map(|d| d.name.clone())
                                    .unwrap_or_else(|| "Selected GPU".to_owned()),
                            };
                            let weak_accel = cx.entity().downgrade();
                            let env_accel = env_id.clone();
                            let creds_accel = creds.clone();
                            transcription_rows.push(
                                Row::new("speech-hardware", "Hardware")
                                    .description("Auto uses the environment\u{2019}s GPU when it has one.")
                                    .highlighted(self.is_target("speech-hardware"))
                                    .control(
                                        Select::new("speech-hardware-select", accel_options)
                                            .selected(acceleration.as_str().to_owned())
                                            .label(accel_label)
                                            .width(200.)
                                            .disabled(controls_disabled)
                                            .on_change(move |value, _, cx| {
                                                let Ok(next) = SpeechAcceleration::parse(&value) else {
                                                    return;
                                                };
                                                let _ = weak_accel.update(cx, |this, cx| {
                                                    this.run_voice_model_action(
                                                        env_accel.clone(),
                                                        creds_accel.clone(),
                                                        "Could not change transcription hardware",
                                                        None,
                                                        move |client| {
                                                            Box::pin(async move {
                                                                client
                                                                    .update_acceleration(&next)
                                                                    .await
                                                                    .map(|_| ())
                                                                    .map_err(|e| e.to_string())
                                                            })
                                                        },
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                    .into_any_element(),
                            );

                            let weak_unload = cx.entity().downgrade();
                            let env_unload = env_id.clone();
                            let creds_unload = creds.clone();
                            transcription_rows.push(
                                Row::new("speech-unload-model", "Unload model")
                                    .description(
                                        "Release the model\u{2019}s memory on the environment after it sits idle.",
                                    )
                                    .control(
                                        Select::new(
                                            "speech-unload-select",
                                            UNLOAD_TIMEOUT_OPTIONS
                                                .iter()
                                                .map(|(t, label)| SelectOption::new(t.as_str(), *label))
                                                .collect(),
                                        )
                                        .selected(model_unload_timeout.as_str())
                                        .label(unload_timeout_label(model_unload_timeout))
                                        .width(200.)
                                        .disabled(controls_disabled)
                                        .on_change(move |value, _, cx| {
                                            let Some(next) = parse_unload_timeout(&value) else {
                                                return;
                                            };
                                            let _ = weak_unload.update(cx, |this, cx| {
                                                this.run_voice_model_action(
                                                    env_unload.clone(),
                                                    creds_unload.clone(),
                                                    "Could not change when the model unloads",
                                                    None,
                                                    move |client| {
                                                        Box::pin(async move {
                                                            client
                                                                .update_model_unload_timeout(next)
                                                                .await
                                                                .map(|_| ())
                                                                .map_err(|e| e.to_string())
                                                        })
                                                    },
                                                    cx,
                                                );
                                            });
                                        }),
                                    )
                                    .into_any_element(),
                            );
                        }
                    }
                }
        }

        let transcription_section = section("Transcription", transcription_rows, cx);

        // -------------------------------------------------------------------
        // Section 3: Dictionary (Personal + Shared/Project)
        // -------------------------------------------------------------------
        let personal_words = doc.speech_custom_words();
        let personal_editor = self.render_dictionary_editor(
            "personal-dict",
            &personal_words,
            &self.voice.personal_term,
            self.voice.personal_alias_target.as_deref(),
            &self.voice.personal_alias_input,
            false,
            |this, window, cx| this.add_personal_dictionary_terms(window, cx),
            |this, term, window, cx| {
                this.voice.personal_alias_target = Some(term);
                this.voice
                    .personal_alias_input
                    .update(cx, |i, cx| {
                        i.set_value("", window, cx);
                        i.focus(window, cx);
                    });
                cx.notify();
            },
            |this, term, cx| {
                let words = store::client(cx).speech_custom_words();
                let next = remove_dictionary_term(&words, &term);
                store::update_client(&this.state, ClientSettingChange::SpeechCustomWords(next), cx);
            },
            cx,
        );
        let mut dict_rows = vec![
            Row::new("speech-dictionary", "Your dictionary")
                .description("Names and terms to spell your way, saved on this device.")
                .highlighted(self.is_target("speech-dictionary"))
                .below(personal_editor)
                .into_any_element(),
        ];

        let has_server = self.scoped_representative_config(cx).is_some();
        let proj_dict_lock = self.managed_setting_lock(&["speechProjectCustomWords"], cx);
        let scoped_project_label = self.scope_project_label(cx);
        let project_words = self.current_project_dictionary_words(cx);
        let project_editor = self.render_dictionary_editor(
            "project-dict",
            &project_words,
            &self.voice.project_term,
            self.voice.project_alias_target.as_deref(),
            &self.voice.project_alias_input,
            !has_server || proj_dict_lock.is_some(),
            |this, window, cx| this.add_project_dictionary_terms(window, cx),
            |this, term, window, cx| {
                this.voice.project_alias_target = Some(term);
                this.voice
                    .project_alias_input
                    .update(cx, |i, cx| {
                        i.set_value("", window, cx);
                        i.focus(window, cx);
                    });
                cx.notify();
            },
            |this, term, cx| {
                let words = this.current_project_dictionary_words(cx);
                let next = remove_dictionary_term(&words, &term);
                this.save_project_dictionary_words(next, cx);
            },
            cx,
        );
        let (proj_dict_title, proj_dict_desc) = match scoped_project_label {
            Some(label) => (
                "Project dictionary",
                format!("Used by everyone who dictates in {label}."),
            ),
            None => (
                "Shared dictionary",
                "Used by everyone who dictates on this environment.".to_owned(),
            ),
        };
        let mut proj_dict_row = Row::new("speech-project-dictionary", proj_dict_title)
            .description(proj_dict_desc)
            .highlighted(self.is_target("speech-project-dictionary"))
            .unavailable(proj_dict_lock.clone())
            .below(project_editor);
        if self.is_project_scope(cx) {
            let reset_view = cx.entity().downgrade();
            proj_dict_row = proj_dict_row.reset_tooltip("Reset to inherited value").reset(
                proj_dict_lock.is_none() && self.has_project_override(&["speechProjectCustomWords"], cx),
                move |_, _, cx| {
                    let _ = reset_view.update(cx, |this, cx| {
                        this.clear_project_overrides(&["speechProjectCustomWords"], cx);
                    });
                },
            );
        }
        dict_rows.push(proj_dict_row.into_any_element());
        let dictionary_section = section("Dictionary", dict_rows, cx);

        // -------------------------------------------------------------------
        // Section 4: Cleanup (`speech-post-processing`)
        // -------------------------------------------------------------------
        let mut cleanup_rows = vec![
            Row::new("speech-post-processing", "Clean up transcripts")
                .description(
                    "Off by default. Sends the transcript and nearby draft text, never audio, to the cleanup model.",
                )
                .highlighted(self.is_target("speech-post-processing"))
                .control(
                    Switch::new("voice-cleanup-switch", cleanup_granted).on_change(
                        |checked, _, cx| {
                            set_voice_cleanup_enabled(checked, cx);
                        },
                    ),
                )
                .into_any_element(),
        ];

        if cleanup_granted {
            let prompt_mode = prompt_settings.mode;
            let prompt_state = state_entity.clone();
            let existing_custom = prompt_settings.custom_instructions.clone();
            let mut instructions_row = Row::new("speech-post-processing-instructions", "Instructions")
                .description(if prompt_mode == SpeechPostProcessingPromptMode::Custom {
                    "Your own instructions for cleanup."
                } else {
                    "Fix punctuation, numbers, lists, and spoken corrections."
                })
                .control(
                    Select::new(
                        "voice-cleanup-instructions-select",
                        vec![
                            SelectOption::new("default", "Built-in"),
                            SelectOption::new("custom", "Custom"),
                        ],
                    )
                    .selected(match prompt_mode {
                        SpeechPostProcessingPromptMode::Default => "default",
                        SpeechPostProcessingPromptMode::Custom => "custom",
                    })
                    .width(160.)
                    .on_change(move |value, _, cx| {
                        let mode = match value.as_ref() {
                            "custom" => SpeechPostProcessingPromptMode::Custom,
                            _ => SpeechPostProcessingPromptMode::Default,
                        };
                        let custom_instructions = if mode == SpeechPostProcessingPromptMode::Custom
                            && existing_custom.trim().is_empty()
                        {
                            DEFAULT_SPEECH_POST_PROCESSING_PROMPT.to_owned()
                        } else {
                            existing_custom.clone()
                        };
                        store::update_client(
                            &prompt_state,
                            ClientSettingChange::SpeechPostProcessingPrompt(
                                SpeechPostProcessingPromptSettings { mode, custom_instructions },
                            ),
                            cx,
                        );
                    }),
                );
            if prompt_mode == SpeechPostProcessingPromptMode::Custom {
                instructions_row = instructions_row.below(
                    div()
                        .mt(r(10.))
                        .pb(r(8.))
                        .max_w(r(672.))
                        .child(Textarea::new(&self.voice.custom_prompt)),
                );
            }
            cleanup_rows.push(instructions_row.into_any_element());

            // CleanupModelRow (`speech-post-processing-model`)
            let cleanup_model_row = self.render_cleanup_model_row(cx);
            cleanup_rows.push(cleanup_model_row);

            // Correction cue (`speech-correction-word`)
            cleanup_rows.push(
                Row::new("speech-correction-word", "Correction cue")
                    .description("The phrase you say to fix a mistake, like \u{201c}scratch that\u{201d}.")
                    .highlighted(self.is_target("speech-correction-word"))
                    .control(div().w(r(160.)).child(Input::new(&self.voice.correction_word).small()))
                    .into_any_element(),
            );
        }

        let cleanup_section = section("Cleanup", cleanup_rows, cx);

        vec![
            Block::new(
                &[
                    "voice-input",
                    "voice-microphone",
                    "microphone-test",
                    "dictation-shortcut-mode",
                ],
                input_section,
            ),
            Block::new(
                &[
                    "voice-transcription",
                    "voice-transcription-destination",
                    "speech-language",
                    "speech-silence-filtering",
                    "remove-filler-words",
                    "speech-models",
                    "speech-streaming",
                    "speech-hardware",
                    "transcription-test",
                ],
                transcription_section,
            ),
            Block::new(
                &["voice-dictionary", "speech-dictionary", "speech-project-dictionary"],
                dictionary_section,
            ),
            Block::new(
                &[
                    "speech-post-processing",
                    "speech-post-processing-model",
                    "speech-correction-word",
                ],
                cleanup_section,
            ),
        ]
    }

    fn render_cleanup_model_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(config) = self.scoped_representative_config(cx) else {
            return Row::new("speech-post-processing-model", "Cleanup model")
                .description(
                    "Model for transcript cleanup, such as a cheaper or faster one. When disabled, use the text generation model configured for this environment.",
                )
                .highlighted(self.is_target("speech-post-processing-model"))
                .control(muted_text("Connect an environment first.", cx))
                .into_any_element();
        };
        self.sync_text_generator_models(&config, cx);
        let env_only = self.env_only_unavailable(cx);
        let row_unavail = self.scoped_unavailable(&["speechPostProcessingModelSelection"], env_only, cx);
        let locked = row_unavail.is_some();
        let saved_cleanup = speech_post_processing_model_selection(&config.settings);
        let dedicated = saved_cleanup.is_some();
        let fallback_text_gen =
            resolve_default_text_generation_selection(&config, None, self.text_gen_models_for(&config));
        let shown = saved_cleanup.clone().or_else(|| fallback_text_gen.clone());
        let options = self.apply_scoped_model_availability(
            &config,
            self.text_gen_select_options(&config, None, &[saved_cleanup.as_ref(), fallback_text_gen.as_ref()]),
            true,
            cx,
        );
        let disabled_reason = saved_cleanup.as_ref().and_then(|sel| {
            self.scoped_model_disabled_reason(&config, &sel.instance_id, &sel.model, true, cx)
        });
        let has_provider = !options.is_empty();
        let switch_view = cx.entity().downgrade();
        let switch_default = fallback_text_gen.clone();
        let select_view = cx.entity().downgrade();

        let mut control = h_flex().flex_wrap().justify_end().gap(r(8.));
        if dedicated {
            if let Some(sel) = &shown
                && has_provider
            {
                let label = self.text_gen_selection_label(&config, sel);
                let trait_pickers = model_trait_controls(
                    "speech-cleanup-model",
                    &config,
                    sel,
                    locked,
                    cx.entity().downgrade(),
                    ServerSettingChange::SpeechPostProcessingModelSelection,
                );
                control = control
                    .child(
                        Select::new("speech-cleanup-model-select", options)
                            .selected(selection_value(sel))
                            .label(label)
                            .width(200.)
                            .popup_width(260.)
                            .show_check()
                            .disabled(locked)
                            .on_change(move |value, _, cx| {
                                if let Some(next) = parse_selection(&value) {
                                    let _ = select_view.update(cx, |this, cx| {
                                        this.update_scoped_server(
                                            ServerSettingChange::SpeechPostProcessingModelSelection(Some(next)),
                                            cx,
                                        );
                                    });
                                }
                            }),
                    )
                    .children(trait_pickers);
            } else {
                control = control.child(muted_text("No text generation providers available.", cx));
            }
        }
        control = control.child(
            Switch::new("speech-cleanup-model-switch", dedicated)
                .disabled(locked || (!dedicated && !has_provider))
                .on_change(move |checked, _, cx| {
                    let next = if checked { switch_default.clone() } else { None };
                    let _ = switch_view.update(cx, |this, cx| {
                        this.update_scoped_server(
                            ServerSettingChange::SpeechPostProcessingModelSelection(next),
                            cx,
                        );
                    });
                }),
        );

        let mut row = Row::new("speech-post-processing-model", "Cleanup model")
            .description(
                "Model for transcript cleanup, such as a cheaper or faster one. When disabled, use the text generation model configured for this environment.",
            )
            .highlighted(self.is_target("speech-post-processing-model"))
            .unavailable(row_unavail);
        if let Some(reason) = disabled_reason {
            row = row.status(reason);
        }
        row.control(control).into_any_element()
    }

    fn voice_language_filter(
        &self, ordered: &[String], selected: &[String], cx: &Context<Self>,
    ) -> AnyElement {
        let label = if selected.is_empty() {
            t(cx, "All languages").to_owned()
        } else {
            selected.iter().map(|code| speech_language_label(code).into_owned()).collect::<Vec<_>>().join(", ")
        };
        let open = self.voice.model_languages_open;
        let trigger = Button::new("voice-models-lang-filter", ButtonVariant::Outline, ButtonSize::Sm)
            .icon(Icon::Globe).trailing(Icon::ChevronDown).label(label)
            .justify_between()
            .on_click(cx.listener(move |view, _, window, cx| {
                view.voice.model_languages_open = !open;
                if !open {
                    view.voice.model_language_search.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                        input.focus(window, cx);
                    });
                }
                cx.notify();
            }));
        let popup = open.then(|| {
            let p = palette(cx);
            let input = &self.voice.model_language_search;
            let query = input.read(cx).value().trim().to_lowercase();
            let visible: Vec<_> = ordered.iter().filter_map(|code| {
                let label = speech_language_label(code);
                (query.is_empty() || code.contains(&query) || label.to_lowercase().contains(&query))
                    .then(|| (code.clone(), label.into_owned()))
            }).collect();
            let no_matches = visible.is_empty();
            let selection = selected.to_vec();
            let items = visible.into_iter().map(|(language, label)| {
                let checked = selection.contains(&language);
                let selection = selection.clone();
                Button::new(SharedString::from(format!("voice-language-option-{language}")), ButtonVariant::GhostMuted, ButtonSize::Xs)
                    .label(label).justify_between()
                    .when(checked, |button| button.trailing(Icon::Check))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        let choices = view.voice.model_selected_languages.get_or_insert_with(|| selection.clone());
                        if let Some(index) = choices.iter().position(|item| item == &language) {
                            choices.remove(index);
                        } else {
                            choices.push(language.clone());
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            });
            let all = query.is_empty().then(|| {
                Button::new("voice-language-all", ButtonVariant::GhostMuted, ButtonSize::Xs)
                    .label("All languages").justify_between()
                    .when(selected.is_empty(), |button| button.trailing(Icon::Check))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.voice.model_selected_languages = Some(Vec::new());
                        cx.notify();
                    }))
            });
            div().absolute().bottom_0().right_0().child(
                gpui::deferred(
                    gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
                        v_flex().id("voice-language-popup").relative().occlude().mt(r(4.)).w(r(250.)).p(r(6.)).gap(r(4.))
                            .rounded(r(8.)).border_1().border_color(hsla(p.dropdown_border)).bg(hsla(p.popover))
                            .theme_slot(ThemeSlot::MenuPopup, cx)
                            .children(theme_slot_clipped_background_layer(ThemeSlot::MenuPopup, r(8.), cx))
                            .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                                view.voice.model_languages_open = false;
                                cx.notify();
                            }))
                            .child(Input::new(input).small().aria_label(t(cx, "Search languages")).theme_slot(ThemeSlot::Input, cx))
                            .child(v_flex().id("voice-language-options").max_h(r(240.)).overflow_y_scroll()
                                .children(all).children(items)
                                .when(no_matches, |list| list.child(muted_text(t(cx, "No languages match."), cx)))),
                    ),
                ).with_priority(2),
            )
        });
        div().relative().w(r(150.)).child(trigger).children(popup).into_any_element()
    }

    fn render_speech_model_browser(
        &mut self,
        env_id: &str,
        creds: &Credentials,
        benchmark_tier: Option<SpeechBenchmarkTier>,
        supports_model_linking: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let preferred = system_preferred_languages();
        let mut supported_langs: Vec<String> = Vec::new();
        for model in &self.voice.catalog_models {
            for lang in &model.languages {
                if !supported_langs.contains(lang) {
                    supported_langs.push(lang.clone());
                }
            }
        }
        let ordered_langs = order_speech_languages(&supported_langs, &preferred);
        let default_lang = preferred
            .iter()
            .find(|code| supported_langs.contains(code))
            .cloned();
        let selected_langs: Vec<String> = self
            .voice
            .model_selected_languages
            .clone()
            .unwrap_or_else(|| default_lang.into_iter().collect());
        let accuracy_lang = selected_langs
            .first()
            .map(String::as_str)
            .unwrap_or("en")
            .to_owned();
        let search_query = self.voice.model_search.read(cx).value().to_string();

        let recommendations =
            recommend_speech_models(&self.voice.catalog_models, &selected_langs, benchmark_tier);
        let sorted_models: Vec<EnvironmentSpeechModel> =
            filter_speech_models_with_recommendations(
                &self.voice.catalog_models,
                &SpeechModelFilterOptions {
                    search: &search_query,
                    languages: &selected_langs,
                    downloaded_only: self.voice.model_downloaded_only,
                    sort: self.voice.model_sort,
                    accuracy_language: &accuracy_lang,
                    benchmark_tier,
                },
                &recommendations,
            )
            .into_iter()
            .cloned()
            .collect();
        let active_model = self.voice.catalog_models.iter().find(|m| m.active).cloned();
        let mut rendered_models = sorted_models.clone();
        if let Some(active) = &active_model
            && !rendered_models.iter().any(|m| m.id == active.id)
        {
            rendered_models.insert(0, active.clone());
        }

        let sort_options: Vec<SelectOption> = SpeechModelSort::ALL
            .iter()
            .map(|sort| SelectOption::new(sort.as_str(), sort.menu_label(&accuracy_lang)))
            .collect();

        let weak_sort = cx.entity().downgrade();
        let downloaded_only = self.voice.model_downloaded_only;
        let toolbar = h_flex()
            .mt(r(10.))
            .gap(r(8.))
            .items_center()
            .child(div().flex_1().min_w_0().child(Input::new(&self.voice.model_search).small()))
            .child(self.voice_language_filter(&ordered_langs, &selected_langs, cx))
            .child(
                Select::new("voice-models-sort", sort_options)
                    .selected(self.voice.model_sort.as_str())
                    .label(self.voice.model_sort.label())
                    .width(140.)
                    .on_change(move |value, _, cx| {
                        if let Some(sort) = SpeechModelSort::parse(&value) {
                            let _ = weak_sort.update(cx, |this, cx| {
                                this.voice.model_sort = sort;
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(
                Button::new(
                    "voice-models-downloaded-only",
                    if downloaded_only { ButtonVariant::Secondary } else { ButtonVariant::Outline },
                    ButtonSize::Sm,
                )
                .label("Downloaded")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.voice.model_downloaded_only = !this.voice.model_downloaded_only;
                    cx.notify();
                })),
            );

        let can_manage = self.can_write_settings_on(Some(env_id), cx);
        let downloading_any = self.voice.catalog_requested_download.is_some()
            || self.voice.catalog_models.iter().any(|m| {
                matches!(
                    m.state,
                    EnvironmentSpeechModelState::Downloading | EnvironmentSpeechModelState::Verifying
                )
            });
        let controls_disabled = !can_manage || self.voice.catalog_busy || downloading_any;
        let can_cancel = can_manage && !self.voice.catalog_busy;

        // Pre-ensure model test controller if there is an active model.
        let test_controller = active_model
            .as_ref()
            .map(|_| self.ensure_model_test_controller(env_id, cx));

        let mut list_items: Vec<AnyElement> = Vec::new();
        if sorted_models.is_empty() {
            list_items.push(
                div()
                    .px(r(12.))
                    .py(r(12.))
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .child("No models match.")
                    .into_any_element(),
            );
        }
        for (idx, mut model) in rendered_models.into_iter().enumerate() {
            if self.voice.catalog_requested_download.as_deref() == Some(model.id.as_str())
                && model.state == EnvironmentSpeechModelState::Downloadable
            {
                model.state = EnvironmentSpeechModelState::Downloading;
            }
            let is_downloading = matches!(
                model.state,
                EnvironmentSpeechModelState::Downloading | EnvironmentSpeechModelState::Verifying
            );
            let pct = model
                .downloaded
                .map(|d| ((d / model.size.max(1.0)) * 100.0).clamp(0.0, 100.0))
                .unwrap_or(0.0);
            let bench = model_benchmark_display(&model, &selected_langs, benchmark_tier);
            let recommendation: Option<SpeechRecommendationRole> = recommendations
                .iter()
                .find(|(id, _)| id == &model.id)
                .map(|(_, role)| *role);

            let mut badges = h_flex().flex_wrap().gap(r(6.)).items_center().child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .child(model.name.clone()),
            );
            if let Some(role) = recommendation {
                let tip: SharedString = role.tooltip().into();
                badges = badges.child(
                    div()
                        .id(SharedString::from(format!("voice-model-rec-{idx}")))
                        .px(r(6.))
                        .py(r(1.))
                        .rounded(r(4.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .text_size(r(11.))
                        .text_color(hsla(p.muted_foreground))
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                        })
                        .child(role.label()),
                );
            }
            for (cond, label) in [
                (model.supports_streaming, "Live preview"),
                (model.supports_translation, "Translate"),
            ] {
                if cond {
                    badges = badges.child(
                        div()
                            .px(r(6.))
                            .py(r(1.))
                            .rounded(r(4.))
                            .border_1()
                            .border_color(hsla(p.border))
                            .text_size(r(11.))
                            .text_color(hsla(p.muted_foreground))
                            .child(label),
                    );
                }
            }

            let mut bench_row = h_flex().flex_wrap().gap_x(r(10.)).gap_y(r(2.)).mt(r(4.)).text_xs().text_color(hsla(p.muted_foreground));
            for acc in &bench.accuracies {
                let text = match (&acc.grade, &acc.error_percent_label) {
                    (Some(g), Some(err)) => format!("{}: {g} ({err}% error)", acc.language_label),
                    _ => format!("{}: not benchmarked", acc.language_label),
                };
                bench_row = bench_row.child(div().child(text));
            }
            match &bench.speed {
                ModelSpeedSummary::Hidden => {}
                ModelSpeedSummary::UpdateServer => {
                    bench_row = bench_row.child(div().child("Speed: update server for this benchmark"));
                }
                ModelSpeedSummary::NotBenchmarked => {
                    bench_row = bench_row.child(div().child("Speed: not benchmarked"));
                }
                ModelSpeedSummary::Seconds(s) => {
                    bench_row = bench_row.child(div().child(format!("Speed: {s} s / 30 s")));
                }
            }
            for fact in &bench.facts {
                bench_row = bench_row.child(div().child(fact.clone()));
            }

            let mut actions = h_flex().flex_wrap().gap(r(6.)).items_center();
            let mut test_transcript_box: Option<AnyElement> = None;
            if model.active
                && let Some(ctrl) = test_controller.clone()
            {
                let phase = ctrl.read(cx).state().phase;
                let pill = render_speech_recording_pill(
                    "voice-settings-model-test",
                    &ctrl,
                    VoiceUiSize::Compact,
                    false,
                    cx,
                );
                actions = actions.child(pill);
                if phase == VoiceInputPhase::Idle {
                    let ctrl_start = ctrl.clone();
                    actions = actions.child(
                        Button::new("voice-model-test-btn", ButtonVariant::Outline, ButtonSize::Sm)
                            .icon(Icon::Mic)
                            .label("Test model")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.voice.model_test_open = true;
                                ctrl_start.update(cx, |c, cx| c.start(window, cx));
                                cx.notify();
                            })),
                    );
                }
                if self.voice.model_test_open {
                    let preview = ctrl.read(cx).preview().cloned();
                    let transcript = self.voice.model_test_transcript.clone();
                    let ctrl_close = ctrl.clone();
                    let body = if let Some(prev) = preview {
                        h_flex()
                            .flex_wrap()
                            .text_sm()
                            .child(div().text_color(hsla(p.foreground)).child(prev.committed))
                            .child(div().text_color(hsla(p.muted_foreground)).child(prev.tentative))
                            .into_any_element()
                    } else if let Some(text) = transcript {
                        div()
                            .text_sm()
                            .text_color(hsla(p.foreground))
                            .child(if text.is_empty() {
                                "No speech recognized. Try again.".to_owned()
                            } else {
                                text
                            })
                            .into_any_element()
                    } else {
                        div()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child("Say a sentence. Cleanup is skipped, so you see exactly what the model heard.")
                            .into_any_element()
                    };
                    test_transcript_box = Some(
                        h_flex()
                            .mt(r(8.))
                            .px(r(10.))
                            .py(r(6.))
                            .gap(r(8.))
                            .items_start()
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.border).opacity(0.6))
                            .bg(hsla(p.muted).opacity(0.3))
                            .child(div().flex_1().min_w_0().child(body))
                            .child(
                                Button::new("voice-model-test-close", ButtonVariant::GhostMuted, ButtonSize::IconSm)
                                    .icon(Icon::X)
                                    .tooltip("Close model test")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        ctrl_close.update(cx, |c, cx| c.cancel(cx));
                                        this.voice.model_test_open = false;
                                        cx.notify();
                                    })),
                            )
                            .into_any_element(),
                    );
                }
            }

            let model_id = model.id.clone();
            let env_action = env_id.to_owned();
            let creds_action = creds.clone();
            if is_downloading {
                let id_cancel = model_id.clone();
                actions = actions.child(
                    Button::new(
                        SharedString::from(format!("voice-model-cancel-{idx}")),
                        ButtonVariant::Outline,
                        ButtonSize::IconSm,
                    )
                    .icon(Icon::X)
                    .disabled(!can_cancel)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id_cancel.clone();
                        this.run_voice_model_action(
                            env_action.clone(),
                            creds_action.clone(),
                            "Could not cancel the download",
                            None,
                            move |client| {
                                Box::pin(async move {
                                    client
                                        .cancel_model_download(&id)
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                })
                            },
                            cx,
                        );
                    })),
                );
            } else if model.state == EnvironmentSpeechModelState::Downloadable {
                let id_dl = model_id.clone();
                actions = actions.child(
                    Button::new(
                        SharedString::from(format!("voice-model-download-{idx}")),
                        ButtonVariant::Outline,
                        ButtonSize::Sm,
                    )
                    .icon(Icon::Download)
                    .label("Download")
                    .disabled(controls_disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id_dl.clone();
                        let dl_id = id.clone();
                        this.run_voice_model_action(
                            env_action.clone(),
                            creds_action.clone(),
                            "Could not download the model",
                            Some(dl_id),
                            move |client| {
                                Box::pin(async move {
                                    client
                                        .download_model(&id)
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                })
                            },
                            cx,
                        );
                    })),
                );
            } else if model.active {
                actions = actions.child(
                    Button::new(
                        SharedString::from(format!("voice-model-selected-{idx}")),
                        ButtonVariant::Secondary,
                        ButtonSize::Sm,
                    )
                    .icon(Icon::Check)
                    .label("Selected")
                    .disabled(true),
                );
            } else {
                let id_sel = model_id.clone();
                actions = actions.child(
                    Button::new(
                        SharedString::from(format!("voice-model-use-{idx}")),
                        ButtonVariant::Outline,
                        ButtonSize::Sm,
                    )
                    .label("Use model")
                    .disabled(controls_disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id_sel.clone();
                        this.run_voice_model_action(
                            env_action.clone(),
                            creds_action.clone(),
                            "Could not select the model",
                            None,
                            move |client| {
                                Box::pin(async move {
                                    client
                                        .select_model(&id)
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                })
                            },
                            cx,
                        );
                    })),
                );
            }

            if model.state == EnvironmentSpeechModelState::Installed {
                let id_del = model.id.clone();
                let env_del = env_id.to_owned();
                let creds_del = creds.clone();
                let is_linked = model.linked_from.is_some();
                actions = actions.child(
                    Button::new(
                        SharedString::from(format!("voice-model-delete-{idx}")),
                        ButtonVariant::Outline,
                        ButtonSize::IconSm,
                    )
                    .icon(if is_linked { Icon::Unlink2 } else { Icon::Trash2 })
                    .tooltip(if is_linked { "Stop using linked file" } else { "Delete model" })
                    .disabled(controls_disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id_del.clone();
                        this.run_voice_model_action(
                            env_del.clone(),
                            creds_del.clone(),
                            "Could not delete the model",
                            None,
                            move |client| {
                                Box::pin(async move {
                                    client
                                        .remove_model(&id)
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                })
                            },
                            cx,
                        );
                    })),
                );
            }

            let card = v_flex()
                .px(r(12.))
                .py(r(10.))
                .when(idx > 0, |el| el.border_t_1().border_color(hsla(p.border).opacity(0.5)))
                .child(
                    h_flex()
                        .items_start()
                        .justify_between()
                        .gap(r(12.))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(badges)
                                .when(!model.description.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .mt(r(2.))
                                            .text_xs()
                                            .text_color(hsla(p.muted_foreground))
                                            .child(model.description.clone()),
                                    )
                                })
                                .when_some(model.linked_from.clone(), |el, path| {
                                    el.child(
                                        div()
                                            .mt(r(2.))
                                            .text_size(r(11.))
                                            .text_color(hsla(p.muted_foreground))
                                            .child(format!("Using your file at {path}")),
                                    )
                                })
                                .child(bench_row)
                                .when(is_downloading, |el| {
                                    let status_text = if model.state == EnvironmentSpeechModelState::Verifying {
                                        "Verifying download\u{2026}".to_owned()
                                    } else {
                                        format!("{}% downloaded", pct.round() as u32)
                                    };
                                    el.child(
                                        v_flex()
                                            .mt(r(6.))
                                            .gap(r(3.))
                                            .child(
                                                div()
                                                    .h(r(4.))
                                                    .w_full()
                                                    .rounded_full()
                                                    .bg(hsla(p.muted))
                                                    .overflow_hidden()
                                                    .child(
                                                        div()
                                                            .h_full()
                                                            .w(gpui::relative((pct as f32) / 100.0))
                                                            .bg(hsla(p.primary)),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_size(r(11.))
                                                    .text_color(hsla(p.muted_foreground))
                                                    .child(status_text),
                                            ),
                                    )
                                }),
                        )
                        .child(actions),
                )
                .children(test_transcript_box);

            list_items.push(card.into_any_element());
        }

        let mut below = v_flex()
            .pb(r(8.))
            .child(toolbar)
            .child(
                v_flex()
                    .mt(r(8.))
                    .rounded(r(8.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .children(list_items),
            );

        if supports_model_linking {
            let link_val = self.voice.model_link_path.read(cx).value().trim().to_owned();
            let env_link = env_id.to_owned();
            let creds_link = creds.clone();
            below = below.child(
                h_flex()
                    .mt(r(8.))
                    .gap(r(8.))
                    .items_center()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.voice.model_link_path).small()))
                    .child(
                        Button::new("voice-model-link-submit", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Use existing")
                            .disabled(controls_disabled || link_val.is_empty())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let path = this.voice.model_link_path.read(cx).value().trim().to_owned();
                                if path.is_empty() {
                                    return;
                                }
                                this.voice
                                    .model_link_path
                                    .update(cx, |i, cx| i.set_value("", window, cx));
                                this.run_voice_model_action(
                                    env_link.clone(),
                                    creds_link.clone(),
                                    "Could not use a model from that path",
                                    None,
                                    move |client| {
                                        Box::pin(async move {
                                            client
                                                .link_models(&path)
                                                .await
                                                .map(|_| ())
                                                .map_err(|e| e.to_string())
                                        })
                                    },
                                    cx,
                                );
                            })),
                    ),
            );
        }

        let tier_label = if benchmark_tier == Some(SpeechBenchmarkTier::Cpu) { "CPU" } else { "GPU" };
        Row::new("speech-models", "Models")
            .description(format!(
                "Grades are error rates and speed is seconds per 30 s of audio, from Handy\u{2019}s benchmark {tier_label}."
            ))
            .highlighted(self.is_target("speech-models") || self.is_target("transcription-test"))
            .below(below)
            .into_any_element()
    }

    pub(super) fn voice_dialog_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.voice.cloud_dialog.as_ref()?;
        let p = palette(cx);
        let provider = dialog.provider;
        let name = provider.label();
        let company = provider.provider_name();
        let routed = provider == CloudSpeechProvider::Openrouter;
        let saved_key = match &dialog.keys_state {
            CloudLoad::Loaded(keys) => match provider {
                CloudSpeechProvider::Openai => Some(keys.openai),
                CloudSpeechProvider::Xai => Some(keys.xai),
                CloudSpeechProvider::Openrouter => keys.openrouter,
            },
            _ => None,
        };
        let editing_key = dialog.can_manage
            && saved_key.is_some()
            && (routed || dialog.active || saved_key == Some(false));
        let listed_models = match &dialog.models_state {
            CloudLoad::Loaded(models) => Some(models.as_slice()),
            _ => None,
        };
        let key_draft = dialog.key_input.read(cx).value().trim().to_owned();
        let secret_ref_valid = dialog
            .secret_source
            .as_deref()
            .is_some_and(|src| SecretValue::reference(src, &key_draft).is_some());
        let validation = validate_cloud_voice_config(
            provider,
            dialog.active,
            saved_key,
            &key_draft,
            dialog.secret_source.is_some(),
            secret_ref_valid,
            &dialog.selected_model,
            dialog.current_model.as_deref(),
            listed_models,
        );

        let title = if dialog.remove_confirm {
            format!("Remove the {company} key?")
        } else if dialog.active {
            if routed {
                "Manage OpenRouter dictation".to_owned()
            } else {
                format!("Manage {company} key")
            }
        } else {
            format!("Use {name} for dictation?")
        };
        let description = if dialog.remove_confirm {
            format!(
                "Everyone using {name} for dictation on {} will lose access.",
                dialog.environment_label
            )
        } else if dialog.active && !routed {
            format!("Anyone with dictation access to {} can use this API key.", dialog.environment_label)
        } else {
            cloud_speech_disclosure(provider, &dialog.environment_label)
        };

        let mut body = v_flex().gap(r(12.));
        if !dialog.remove_confirm {
            match &dialog.keys_state {
                CloudLoad::Loading => {
                    body = body.child(muted_text("Checking for an API key\u{2026}", cx));
                }
                CloudLoad::Failed(err) => {
                    let creds = dialog.credentials.clone();
                    let seq = dialog.seq;
                    body = body
                        .child(
                            div()
                                .text_xs()
                                .text_color(hsla(p.destructive_foreground))
                                .child(err.clone()),
                        )
                        .child(
                            Button::new("voice-cloud-retry-keys", ButtonVariant::Outline, ButtonSize::Sm)
                                .label("Retry loading API key")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.load_cloud_dialog_keys(creds.clone(), seq, cx);
                            })),
                    );
                }
                CloudLoad::Loaded(_) if routed && saved_key.is_none() => {
                    body = body.child(muted_text(
                        format!("Update {} to use OpenRouter dictation.", dialog.environment_label),
                        cx,
                    ));
                }
                CloudLoad::Loaded(_) => {
                    if routed {
                        body = body.child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.foreground))
                                .child("Model"),
                        );
                        match &dialog.models_state {
                            CloudLoad::Loading => {
                                body = body.child(muted_text("Loading transcription models\u{2026}", cx));
                            }
                            CloudLoad::Failed(err) => {
                                let creds = dialog.credentials.clone();
                                let seq = dialog.seq;
                                body = body
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(hsla(p.destructive_foreground))
                                            .child(err.clone()),
                                    )
                                    .child(
                                        Button::new("voice-cloud-retry-models", ButtonVariant::Outline, ButtonSize::Sm)
                                            .label("Retry loading models")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.load_cloud_dialog_models(creds.clone(), seq, cx);
                                        })),
                                );
                            }
                            CloudLoad::Loaded(models) => {
                                let query = dialog.model_search.read(cx).value().trim().to_lowercase();
                                let filtered: Vec<_> = models.iter().filter(|model| {
                                    query.is_empty() || model.name.to_lowercase().contains(&query)
                                        || model.id.to_lowercase().contains(&query)
                                }).cloned().collect();
                                let selected = dialog.selected_model.clone();
                                let results = if filtered.is_empty() {
                                    div().px(r(10.)).py(r(8.)).text_xs().text_color(hsla(p.muted_foreground))
                                        .child(t(cx, "No transcription model matches.")).into_any_element()
                                } else {
                                    let count = filtered.len();
                                    uniform_list("voice-cloud-openrouter-models", count,
                                        cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                                            range.map(|index| {
                                                let model = &filtered[index];
                                                let is_selected = selected == model.id;
                                                let model_id = model.id.clone();
                                                h_flex().id(("voice-cloud-model-row", index)).h(r(42.))
                                                    .px(r(10.)).justify_between().cursor_pointer()
                                                    .when(is_selected, |row| row.bg(hsla(p.accent)))
                                                    .hover(|row| row.bg(hsla(p.accent).opacity(0.6)))
                                                    .on_click(cx.listener(move |view, _, _, cx| {
                                                        if let Some(dialog) = view.voice.cloud_dialog.as_mut() {
                                                            dialog.selected_model = model_id.clone();
                                                            cx.notify();
                                                        }
                                                    }))
                                                    .child(v_flex().min_w_0()
                                                        .child(div().text_xs().font_weight(FontWeight::MEDIUM)
                                                            .text_color(hsla(p.foreground)).truncate().child(model.name.clone()))
                                                        .child(div().text_size(r(11.)).text_color(hsla(p.muted_foreground))
                                                            .truncate().child(model.id.clone())))
                                                    .when(is_selected, |row| row.child(icon(Icon::Check, 14., hsla(p.foreground))))
                                                    .into_any_element()
                                            }).collect()
                                        }),
                                    ).h(r(42. * count.min(5) as f32)).into_any_element()
                                };
                                body = body
                                    .child(Input::new(&dialog.model_search).small().theme_slot(ThemeSlot::Input, cx))
                                    .child(div().rounded(r(8.)).border_1().border_color(hsla(p.border)).child(results));
                                if let Some(delisted) = &validation.delisted_model {
                                    body = body.child(
                                        div()
                                            .text_xs()
                                            .text_color(hsla(p.destructive_foreground))
                                            .child(format!(
                                                "OpenRouter no longer lists {delisted}. Dictation stays off until you choose another model."
                                            )),
                                    );
                                }
                            }
                        }
                    }

                    if editing_key {
                        let app = self.state.read(cx);
                        let config = app.fleet.environments.iter().filter_map(|entry| entry.config.config.as_ref())
                            .chain(app.server_config.config.as_ref())
                            .find(|config| config.environment.environment_id == dialog.environment_id);
                        let secret_sources = if config.is_some_and(|c| c.has_secret_providers_feature() && c.secret_references) {
                            config.map_or(&[][..], |config| config.settings.secret_providers.0.as_slice())
                        } else {
                            &[]
                        };
                        let weak_src = cx.entity().downgrade();
                        let api_key = SecretSourceInput::new("voice-cloud-secret-source", "API key",
                            &dialog.key_input, secret_sources, dialog.secret_source.as_deref())
                            .disabled(dialog.busy)
                            .on_source_change(move |source, window, cx| {
                                let _ = weak_src.update(cx, |view, cx| {
                                    if let Some(dialog) = view.voice.cloud_dialog.as_mut() {
                                        dialog.secret_source = source.map(|source| source.to_string());
                                        let masked = dialog.secret_source.is_none();
                                        dialog.key_input.update(cx, |input, cx| {
                                            input.set_value("", window, cx);
                                            input.set_masked(masked, window, cx);
                                        });
                                        dialog.error = None;
                                        cx.notify();
                                    }
                                });
                            });
                        body = body
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.foreground))
                                    .child(if routed && saved_key == Some(true) {
                                        "Replace API key"
                                    } else {
                                        "API key"
                                    }),
                            )
                            .child(api_key)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(if let Some(invalid) = validation.invalid_key_message {
                                        invalid.to_owned()
                                    } else if dialog.active && !routed {
                                        "Updating the key keeps dictation enabled.".to_owned()
                                    } else if routed && saved_key == Some(true) {
                                        "Leave blank to keep the saved key.".to_owned()
                                    } else {
                                        format!(
                                            "Anyone with dictation access to {} can bill this key.",
                                            dialog.environment_label
                                        )
                                    }),
                            );
                    } else if saved_key == Some(false) {
                        body = body.child(muted_text(
                            format!("Ask an administrator on {} to add an API key.", dialog.environment_label),
                            cx,
                        ));
                    } else {
                        body = body.child(muted_text(
                            if routed {
                                format!(
                                    "Uses the API key saved on {}. This choice applies only to this device.",
                                    dialog.environment_label
                                )
                            } else {
                                "This choice applies only to this device. Choose Off to stop sharing."
                                    .to_owned()
                            },
                            cx,
                        ));
                    }
                }
            }
        }
        if let Some(err) = &dialog.error {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(hsla(p.destructive_foreground))
                    .child(err.clone()),
            );
        }

        let busy = dialog.busy;
        let mut footer_buttons = Vec::new();
        if dialog.can_manage && saved_key == Some(true) && (dialog.active || routed) && !dialog.remove_confirm {
            footer_buttons.push(
                Button::new("voice-cloud-remove-trigger", ButtonVariant::Ghost, ButtonSize::Sm)
                    .label("Remove key\u{2026}")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(d) = this.voice.cloud_dialog.as_mut() {
                            d.remove_confirm = true;
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            );
        }
        footer_buttons.push(
            Button::new("voice-cloud-cancel", ButtonVariant::Outline, ButtonSize::Sm)
                .label("Cancel")
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.dismiss_voice_overlay(window, cx);
                }))
                .into_any_element(),
        );
        if dialog.remove_confirm {
            footer_buttons.push(
                Button::new("voice-cloud-remove-confirm", ButtonVariant::DestructiveOutline, ButtonSize::Sm)
                    .label("Remove key")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_cloud_dialog_key(CloudSpeechKeyInput::Clear, None, cx);
                    }))
                    .into_any_element(),
            );
        } else if routed {
            if saved_key.is_some() && (saved_key == Some(true) || dialog.can_manage) {
                footer_buttons.push(
                    Button::new("voice-cloud-submit", ButtonVariant::Default, ButtonSize::Sm)
                        .label(if dialog.active { "Save" } else { "Save and enable" })
                        .disabled(busy || !validation.can_submit)
                        .on_click(cx.listener(|this, _, _, cx| this.submit_cloud_dialog(cx)))
                        .into_any_element(),
                );
            }
        } else if editing_key {
            footer_buttons.push(
                Button::new("voice-cloud-submit", ButtonVariant::Default, ButtonSize::Sm)
                    .label(if dialog.active { "Save key" } else { "Save and enable" })
                    .disabled(busy || !validation.key_valid)
                    .on_click(cx.listener(|this, _, _, cx| this.submit_cloud_dialog(cx)))
                    .into_any_element(),
            );
        } else if saved_key == Some(true) && !dialog.active {
            let env_id = dialog.environment_id.clone();
            footer_buttons.push(
                Button::new("voice-cloud-use", ButtonVariant::Default, ButtonSize::Sm)
                    .label(format!("Use {name}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let cleanup = voice_cleanup_granted(cx);
                        choose_voice_destination(
                            Some(VoiceTranscriptionDestination::cloud(env_id.clone(), provider)),
                            cleanup,
                            cx,
                        );
                        this.voice.cloud_dialog = None;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }

        let weak_dismiss = cx.entity().downgrade();
        let frame = DialogFrame {
            id: "voice-cloud-config-dialog",
            title: title.into(),
            description: description.into(),
            max_width: 448.,
            body: body.into_any_element(),
            footer: dialog_footer(false, footer_buttons, cx).into_any_element(),
            focus: dialog.focus.clone(),
        };
        Some(frame.render(
            window,
            move |window, cx| {
                let _ = weak_dismiss.update(cx, |this, cx| {
                    this.dismiss_voice_overlay(window, cx);
                });
            },
            cx,
        ))
    }

    fn can_write_settings_on(&self, env_id: Option<&str>, cx: &App) -> bool {
        let app = self.state.read(cx);
        if let Some(target) = env_id {
            for view in &app.fleet.environments {
                if view.status != ConnectionStatus::Connected {
                    continue;
                }
                if view
                    .config
                    .config
                    .as_ref()
                    .is_some_and(|c| c.environment.environment_id == target)
                    || view.summary.owner.saved_id == target
                {
                    return view
                        .grants
                        .as_ref()
                        .is_none_or(|g| g.grants("settings:write"));
                }
            }
        }
        app.client_handle()
            .and_then(|c| c.session_state().borrow().clone())
            .is_none_or(|g| g.grants("settings:write"))
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use gpui::{Render, div, prelude::*};
    use serde_json::json;
    use t3_core::settings::search::Section;

    use super::*;
    use crate::voice::{MockAudioPlatform, install_audio_platform, install_memory_voice_consent};

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().children(crate::settings::overlay(window, cx))
        }
    }

    #[gpui::test]
    fn voice_settings_mic_test_and_dictionary_round_trip(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
        let mock_audio = Arc::new(MockAudioPlatform::new());
        let (state, _fixture) = cx.update(|cx| {
            install_audio_platform(mock_audio.clone(), cx);
            install_memory_voice_consent(SavedAudioChoice::Off, false, cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": {
                            "environmentId": "mac-env",
                            "label": "Mac",
                            "capabilities": {
                                "voiceTranscription": true,
                                "cloudVoiceTranscription": true
                            }
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
            crate::settings::open(Section::Voice, cx);
        });

        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let view = vcx.read(|cx| cx.global::<crate::settings::Host>().view.clone().expect("settings view"));

        // Page renders and enumerates mock microphones.
        view.read_with(&vcx, |view, _| {
            assert!(view.voice.devices_loaded);
            assert!(view.voice.devices.iter().any(|d| d.device_id == "mock-mic-1"));
        });

        // MicrophoneTest: start -> feed PCM -> stop -> ready -> toggle playback -> cancel.
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.start_mic_test(String::new(), cx);
                assert!(matches!(view.voice.mic_test_phase, MicrophoneTestPhase::Recording { .. }));
            });
        });
        mock_audio.emit_pcm(vec![0.25; 1600]);
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.stop_mic_test_recording(cx);
                assert!(matches!(view.voice.mic_test_phase, MicrophoneTestPhase::Ready { playing: false, .. }));
                assert_eq!(view.voice.mic_test_samples.len(), 1600);
                view.toggle_mic_test_playback(cx);
                assert!(matches!(view.voice.mic_test_phase, MicrophoneTestPhase::Ready { playing: true, .. }));
                view.cancel_mic_test(cx);
                assert_eq!(view.voice.mic_test_phase, MicrophoneTestPhase::Idle);
            });
        });

        // Personal dictionary add + alias persists to client settings.
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.voice
                    .personal_term
                    .update(cx, |i, cx| i.set_value("GPUI, Antigravity", window, cx));
                view.add_personal_dictionary_terms(window, cx);
            });
        });
        vcx.read(|cx| {
            let words = store::client(cx).speech_custom_words();
            assert_eq!(words.len(), 2);
            assert_eq!(words[0].term, "GPUI");
        });

        // Search target fallback resolves unmounted targets to their parent anchor.
        vcx.read(|cx| {
            assert_eq!(
                view.read(cx).resolve_voice_target("speech-models", cx),
                "voice-transcription-destination"
            );
            assert_eq!(
                view.read(cx).resolve_voice_target("speech-correction-word", cx),
                "speech-post-processing"
            );
        });

        // Switching away from "System default" ("") to a named mic resets phase, samples, and error.
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.start_mic_test(String::new(), cx);
            });
            mock_audio.emit_pcm(vec![0.25; 800]);
            view.update(cx, |view, cx| {
                view.stop_mic_test_recording(cx);
                view.voice.mic_test_error = Some("stale error".into());
                assert!(matches!(view.voice.mic_test_phase, MicrophoneTestPhase::Ready { .. }));
                assert!(!view.voice.mic_test_samples.is_empty());
            });
            store::update_client(
                &state,
                ClientSettingChange::VoiceMicrophones(vec![
                    t3_core::voice_input::VoiceMicrophoneEntry::Device {
                        device_id: "mock-mic-1".into(),
                        label: "Studio USB Mic".into(),
                    },
                    t3_core::voice_input::VoiceMicrophoneEntry::Default,
                ]),
                cx,
            );
            window.refresh();
            window.draw(cx).clear();
        });
        view.read_with(&vcx, |view, _| {
            assert_eq!(view.voice.mic_test_phase, MicrophoneTestPhase::Idle);
            assert!(view.voice.mic_test_samples.is_empty());
            assert!(view.voice.mic_test_error.is_none());
            assert_eq!(view.voice.mic_test_device_id.as_deref(), Some("mock-mic-1"));
        });

        // Cloud dialog subscribes to InputEvent::Change on model_search and key_input, notifying
        // SettingsView so OpenRouter filtering and Save validation stay live.
        let notify_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let notify_counter = notify_count.clone();
        let _obs = vcx.update(|_, cx| {
            cx.observe(&view, move |_, _| {
                notify_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_cloud_dialog(
                    "mac-env".into(),
                    "Mac".into(),
                    Credentials {
                        http_base: "http://127.0.0.1:1".into(),
                        access_token: "tok".into(),
                    },
                    CloudSpeechProvider::Openrouter,
                    false,
                    true,
                    window,
                    cx,
                );
                let d = view.voice.cloud_dialog.as_mut().unwrap();
                d.keys_state = CloudLoad::Loaded(CloudSpeechKeys {
                    openai: false,
                    xai: false,
                    openrouter: Some(false),
                });
                d.models_state = CloudLoad::Loaded(vec![
                    CloudSpeechModel {
                        id: "openai/whisper-large".into(),
                        name: "Whisper Large".into(),
                    },
                    CloudSpeechModel {
                        id: "google/gemini-2.5-flash".into(),
                        name: "Gemini Flash".into(),
                    },
                ]);
                d.selected_model = "google/gemini-2.5-flash".into();
            });
        });
        vcx.run_until_parked();
        notify_count.store(0, std::sync::atomic::Ordering::SeqCst);
        vcx.update(|window, cx| {
            let model_search = view.read(cx).voice.cloud_dialog.as_ref().unwrap().model_search.clone();
            // Before typing a key, validation rejects submit.
            let before = {
                let d = view.read(cx).voice.cloud_dialog.as_ref().unwrap();
                let models = match &d.models_state {
                    CloudLoad::Loaded(m) => Some(m.as_slice()),
                    _ => None,
                };
                validate_cloud_voice_config(
                    d.provider,
                    d.active,
                    Some(false),
                    d.key_input.read(cx).value().as_str(),
                    false,
                    d.can_manage,
                    &d.selected_model,
                    None,
                    models,
                )
            };
            assert!(!before.can_submit);

            model_search.update(cx, |i, cx| i.insert("gemini", window, cx));
        });
        vcx.run_until_parked();
        assert!(notify_count.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        notify_count.store(0, std::sync::atomic::Ordering::SeqCst);
        vcx.update(|window, cx| {
            let key_input = view.read(cx).voice.cloud_dialog.as_ref().unwrap().key_input.clone();
            key_input.update(cx, |i, cx| i.insert("sk-or-test-key-1234", window, cx));
        });
        vcx.run_until_parked();
        assert!(notify_count.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        vcx.update(|window, cx| {
            let d = view.read(cx).voice.cloud_dialog.as_ref().unwrap();
            let query = d.model_search.read(cx).value().trim().to_lowercase();
            let models = match &d.models_state {
                CloudLoad::Loaded(m) => m.as_slice(),
                _ => &[],
            };
            let matched: Vec<&str> = models
                .iter()
                .filter(|m| m.name.to_lowercase().contains(&query) || m.id.to_lowercase().contains(&query))
                .map(|m| m.id.as_str())
                .collect();
            assert_eq!(matched, vec!["google/gemini-2.5-flash"]);
            let after = validate_cloud_voice_config(
                d.provider,
                d.active,
                Some(false),
                d.key_input.read(cx).value().as_str(),
                false,
                d.can_manage,
                &d.selected_model,
                None,
                Some(models),
            );
            assert!(after.can_submit);
            // Render overlay with scrollable OpenRouter model list cleanly.
            window.refresh();
            window.draw(cx).clear();
            view.update(cx, |v, cx| {
                v.dismiss_voice_overlay(window, cx);
            });
        });

        // Leaving Voice cancels any active model_test_controller and closes model_test_open.
        vcx.update(|window, cx| {
            let ctrl = view.update(cx, |view, cx| {
                view.voice.model_test_open = true;
                view.ensure_model_test_controller("mac-env", cx)
            });
            ctrl.update(cx, |c, cx| c.start(window, cx));
            view.update(cx, |view, cx| view.leave_voice(cx));
            assert!(!view.read(cx).voice.model_test_open);
            assert_eq!(ctrl.read(cx).state().phase, VoiceInputPhase::Idle);
        });

        drop(_obs);
        drop(view);
        vcx.update(crate::settings::finish_close);
        state.update(&mut vcx, |state, _| state.stop_pump());
        vcx.run_until_parked();
        drop(vcx);
        drop(runtime);
    }
}
