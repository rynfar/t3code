use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{App, Context, Entity, EntityId, EventEmitter, Global, Subscription, Task, WeakEntity, Window};
use t3_client::auth::Credentials;
use t3_client::voice::{VoiceClient, VoiceClientError};
use t3_core::client_settings::ClientSettingChange;
use t3_core::voice_input::{
    CLOUD_SPEECH_LANGUAGES, CloudSpeechError, CloudSpeechErrorReason, CloudSpeechProvider,
    DictationSubmissionDecision, DictationSubmissionGate, EnvironmentSpeechModel,
    EnvironmentSpeechModelState, EnvironmentSpeechPostProcessingRequest, EnvironmentSpeechState,
    EnvironmentSpeechStatus, MICROPHONE_PERMISSION_ERROR, SpeechPostProcessingDraft,
    SpeechPostProcessingOptions, SpeechPostProcessingSelection, SpeechStreamText,
    SpeechTranscriptionOptions, VoiceControllerEffect, VoiceDraftSnapshot, VoiceErrorAction,
    VoiceInputControllerMachine, VoiceInputPhase, VoiceInputState, VoiceSelection,
    VoiceTranscriptionDestination, VoiceTranscriptionError, VoiceTranscriptionErrorCode,
    choose_setup_speech_model, cleanup_context, f32_slice_to_le_bytes, preferred_microphone_id,
    preferred_speech_languages, remember_microphones, resolve_speech_vocabulary,
};

use super::consent::{
    ResolvedVoiceDestination, resolve_cleanup_credentials, resolve_voice_destination,
    voice_consent_generation,
};
use super::platform::{
    AudioCaptureCallbacks, AudioCaptureSession, CaptureStartError, MicPermissionStatus,
    PermissionOutcome, audio_platform,
};
use crate::app_state::{AppEvent, AppState};

const MAX_RETAINED_RECORDINGS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceControllerEvent {
    StateChanged,
    CommitDraft {
        text: String,
        selection: VoiceSelection,
    },
    SubmitAfterDictation {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceSetupStep {
    Confirm,
    Downloading,
    Ready,
    Error,
}

#[derive(Debug, Clone)]
pub struct VoiceSetupState {
    pub open: bool,
    pub step: VoiceSetupStep,
    pub model: Option<EnvironmentSpeechModel>,
    pub language: Option<String>,
    pub downloading: bool,
    pub error: Option<String>,
    pub catalog_failed: bool,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

impl Default for VoiceSetupState {
    fn default() -> Self {
        Self {
            open: false,
            step: VoiceSetupStep::Confirm,
            model: None,
            language: None,
            downloading: false,
            error: None,
            catalog_failed: false,
            downloaded_bytes: None,
            total_bytes: None,
        }
    }
}

pub type DraftReader = Rc<dyn Fn(&App) -> Option<VoiceDraftSnapshot>>;
pub type HintsReader = Rc<dyn Fn(&App) -> Vec<String>>;
pub type ProjectIdReader = Rc<dyn Fn(&App) -> Option<String>>;
pub type VoiceSettingsHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Default)]
struct ActiveVoiceSessionLock {
    owner: Option<(EntityId, WeakEntity<VoiceSpeechController>)>,
}

impl Global for ActiveVoiceSessionLock {}

fn try_acquire_voice_session(
    entity_id: EntityId,
    weak: WeakEntity<VoiceSpeechController>,
    cx: &mut App,
) -> bool {
    if cx.try_global::<ActiveVoiceSessionLock>().is_none() {
        cx.set_global(ActiveVoiceSessionLock::default());
    }
    let existing = cx
        .global::<ActiveVoiceSessionLock>()
        .owner
        .as_ref()
        .and_then(|(id, w)| w.upgrade().map(|e| (*id, e)));
    if let Some((active_id, active_entity)) = existing
        && active_id != entity_id
        && active_entity.read(cx).state().is_busy()
    {
        return false;
    }
    cx.global_mut::<ActiveVoiceSessionLock>().owner = Some((entity_id, weak));
    true
}

fn release_voice_session(entity_id: EntityId, cx: &mut App) {
    if cx.try_global::<ActiveVoiceSessionLock>().is_some() {
        let lock = cx.global_mut::<ActiveVoiceSessionLock>();
        if lock.owner.as_ref().is_some_and(|(id, _)| *id == entity_id) {
            lock.owner = None;
        }
    }
}

pub struct VoiceSpeechConfig {
    pub app: Entity<AppState>,
    pub owner_key: String,
    pub offer_setup: bool,
    pub draft_reader: DraftReader,
    pub hints_reader: HintsReader,
    pub project_id_reader: ProjectIdReader,
    pub environment_id_reader: ProjectIdReader,
    pub on_open_voice_settings: VoiceSettingsHandler,
}

struct PreparedStart {
    owner_key: String,
    destination: ResolvedVoiceDestination,
    consent_grant_id: String,
    consent_gen_ref: Arc<AtomicU64>,
    consent_generation: u64,
    draft_environment_id: Option<String>,
    cleanup_requested: bool,
    cleanup_grant_id: Option<String>,
    options: SpeechTranscriptionOptions,
    cleanup_options: SpeechPostProcessingOptions,
    can_post_process: bool,
}

enum StreamCommand {
    Feed(Vec<u8>),
    Finish(tokio::sync::oneshot::Sender<Result<String, VoiceClientError>>),
    Cancel,
}

#[derive(Clone)]
struct ActiveStreamHandle {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<StreamCommand>,
    can_send: Arc<AtomicBool>,
}

pub struct VoiceSpeechController {
    app: Entity<AppState>,
    owner_key: String,
    offer_setup: bool,
    draft_reader: DraftReader,
    hints_reader: HintsReader,
    project_id_reader: ProjectIdReader,
    environment_id_reader: ProjectIdReader,
    on_open_voice_settings: VoiceSettingsHandler,

    machine: VoiceInputControllerMachine,
    submission_gate: DictationSubmissionGate,
    setup: VoiceSetupState,
    status: Option<EnvironmentSpeechStatus>,
    status_env_id: Option<String>,
    start_request_id: u64,

    live_preview: Option<SpeechStreamText>,
    audio_level: f32,
    elapsed_seconds: u64,
    recording_started_at: Option<Instant>,

    recordings: HashMap<String, Vec<u8>>,
    recording_order: Vec<String>,
    active_pcm_buffer: Option<Arc<Mutex<Vec<u8>>>>,
    active_capture: Option<Box<dyn AudioCaptureSession>>,
    active_stream: Option<ActiveStreamHandle>,

    prepared: Option<PreparedStart>,
    setup_credentials: Option<Credentials>,
    setup_selected_model_id: Option<String>,
    aborted_tokens: HashSet<u64>,
    aborted_post_process_tokens: HashSet<u64>,

    _app_sub: Subscription,
    _prepare_task: Option<Task<()>>,
    _op_task: Option<Task<()>>,
    _stream_updates_task: Option<Task<()>>,
    _capture_events_task: Option<Task<()>>,
    _timer_task: Option<Task<()>>,
    _setup_task: Option<Task<()>>,
    _download_poll_task: Option<Task<()>>,
}

impl EventEmitter<VoiceControllerEvent> for VoiceSpeechController {}

impl VoiceSpeechController {
    pub fn new(config: VoiceSpeechConfig, cx: &mut Context<Self>) -> Self {
        let app_sub = cx.subscribe(&config.app, |this, _, event, cx| {
            if matches!(event, AppEvent::Phase | AppEvent::ServerConfig) {
                this.refresh_status_if_needed(cx);
                cx.notify();
            }
        });

        let mut controller = Self {
            app: config.app,
            owner_key: config.owner_key,
            offer_setup: config.offer_setup,
            draft_reader: config.draft_reader,
            hints_reader: config.hints_reader,
            project_id_reader: config.project_id_reader,
            environment_id_reader: config.environment_id_reader,
            on_open_voice_settings: config.on_open_voice_settings,
            machine: VoiceInputControllerMachine::new(true, true),
            submission_gate: DictationSubmissionGate::new(),
            setup: VoiceSetupState::default(),
            status: None,
            status_env_id: None,
            start_request_id: 0,
            live_preview: None,
            audio_level: 0.0,
            elapsed_seconds: 0,
            recording_started_at: None,
            recordings: HashMap::new(),
            recording_order: Vec::new(),
            active_pcm_buffer: None,
            active_capture: None,
            active_stream: None,
            prepared: None,
            setup_credentials: None,
            setup_selected_model_id: None,
            aborted_tokens: HashSet::new(),
            aborted_post_process_tokens: HashSet::new(),
            _app_sub: app_sub,
            _prepare_task: None,
            _op_task: None,
            _stream_updates_task: None,
            _capture_events_task: None,
            _timer_task: None,
            _setup_task: None,
            _download_poll_task: None,
        };
        controller.refresh_status_if_needed(cx);
        controller
    }

    pub fn owner_key(&self) -> &str {
        &self.owner_key
    }

    pub fn set_owner_key(&mut self, next: impl Into<String>, cx: &mut Context<Self>) {
        let next = next.into();
        if self.owner_key == next {
            return;
        }
        self.owner_key = next;
        self.start_request_id = self.start_request_id.wrapping_add(1);
        self.setup = VoiceSetupState::default();
        self._prepare_task = None;
        self._setup_task = None;
        self._download_poll_task = None;
        let effects = self.machine.owner_changed();
        self.submission_gate.clear();
        release_voice_session(cx.entity_id(), cx);
        self.apply_effects(effects, cx);
        cx.notify();
    }

    pub fn state(&self) -> &VoiceInputState {
        self.machine.state()
    }

    pub fn setup(&self) -> &VoiceSetupState {
        &self.setup
    }

    pub fn blocks_submission(&self) -> bool {
        self.machine.state().blocks_submission()
    }

    pub fn freezes_editor(&self) -> bool {
        self.machine.state().blocks_submission()
    }

    pub fn preparing(&self) -> bool {
        self.machine.state().phase == VoiceInputPhase::Preparing
    }

    pub fn preview(&self) -> Option<&SpeechStreamText> {
        matches!(
            self.machine.state().phase,
            VoiceInputPhase::Recording | VoiceInputPhase::Transcribing
        )
        .then_some(self.live_preview.as_ref())
        .flatten()
    }

    pub fn level(&self) -> f32 {
        self.audio_level
    }

    pub fn elapsed_seconds(&self) -> u64 {
        self.elapsed_seconds
    }

    pub fn progress(&self) -> Option<(u64, u64)> {
        let downloaded = self.setup.downloaded_bytes?;
        let total = self.setup.total_bytes?.max(1);
        Some((downloaded, total))
    }

    pub fn transcription_environment_label(&self, cx: &App) -> Option<String> {
        let state = self.app.read(cx);
        let resolution = resolve_voice_destination(state, cx);
        resolution.active.map(|active| active.environment_label)
    }

    pub fn available(&self, cx: &App) -> bool {
        let app_state = self.app.read(cx);
        let resolution = resolve_voice_destination(app_state, cx);
        if resolution.offer_first_run_setup && self.offer_setup {
            return true;
        }
        let Some(active) = resolution.active else {
            return false;
        };
        if matches!(active.destination, VoiceTranscriptionDestination::Cloud { .. }) {
            return true;
        }
        if self.status_env_id.as_deref() == Some(active.destination.environment_id()) {
            return !matches!(self.status, Some(EnvironmentSpeechStatus::Unsupported { .. }));
        }
        true
    }

    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.setup.open && self.setup.downloading {
            return;
        }
        if !matches!(
            self.machine.state().phase,
            VoiceInputPhase::Idle | VoiceInputPhase::Error
        ) {
            return;
        }
        let resolution = resolve_voice_destination(self.app.read(cx), cx);
        if resolution.active.is_none() {
            if resolution.offer_first_run_setup && self.offer_setup {
                (self.on_open_voice_settings)(window, cx);
            } else {
                self.machine.set_has_transcriber(false);
                let draft = (self.draft_reader)(cx);
                let effects = self.machine.start(draft);
                self.apply_effects(effects, cx);
            }
            return;
        }

        if !try_acquire_voice_session(cx.entity_id(), cx.entity().downgrade(), cx) {
            let effects = self.machine.fail(
                "Another voice recording is already active.",
                Some(VoiceErrorAction::Retry),
            );
            self.apply_effects(effects, cx);
            return;
        }

        self.start_request_id = self.start_request_id.wrapping_add(1);
        self.setup = VoiceSetupState::default();
        self.machine.set_has_transcriber(true);
        let draft = (self.draft_reader)(cx);
        let effects = self.machine.start(draft);
        self.apply_effects(effects, cx);
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let effects = self.machine.stop();
        self.apply_effects(effects, cx);
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.start_request_id = self.start_request_id.wrapping_add(1);
        self.submission_gate.clear();
        self.setup = VoiceSetupState::default();
        self._prepare_task = None;
        self._op_task = None;
        self._stream_updates_task = None;
        self._capture_events_task = None;
        self._timer_task = None;
        release_voice_session(cx.entity_id(), cx);
        let effects = self.machine.cancel();
        self.apply_effects(effects, cx);
        cx.notify();
    }

    pub fn skip_post_processing(&mut self, cx: &mut Context<Self>) {
        let effects = self.machine.skip_post_processing();
        self.apply_effects(effects, cx);
    }

    pub fn submit_after_dictation(&mut self, cx: &mut Context<Self>) -> DictationSubmissionDecision {
        let state = self.machine.state().clone();
        let decision = self
            .submission_gate
            .request_submit(&self.owner_key, false, &state);
        if matches!(decision, DictationSubmissionDecision::QueueAndStopRecording) {
            self.stop(cx);
        }
        decision
    }

    pub fn open_voice_settings(&self, window: &mut Window, cx: &mut App) {
        (self.on_open_voice_settings)(window, cx);
    }

    pub fn open_system_microphone_settings(&self, cx: &App) -> bool {
        audio_platform(cx).open_microphone_settings()
    }

    pub fn setup_set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.setup.open == open {
            return;
        }
        self.setup.open = open;
        if !open && !self.setup.downloading {
            self._download_poll_task = None;
        }
        cx.notify();
    }

    pub fn setup_download(&mut self, cx: &mut Context<Self>) {
        let (Some(credentials), Some(model)) =
            (self.setup_credentials.clone(), self.setup.model.clone())
        else {
            return;
        };
        let model_id = model.id.clone();
        let runtime = self.app.read(cx).runtime().clone();
        self.setup.step = VoiceSetupStep::Downloading;
        self.setup.downloading = true;
        self.setup.error = None;
        self.setup.downloaded_bytes = Some(0);
        self.setup.total_bytes = Some(model.size as u64);
        cx.notify();

        let creds_for_poll = credentials.clone();
        let model_id_for_poll = model_id.clone();
        let rt_poll = runtime.clone();
        self._download_poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let creds = creds_for_poll.clone();
                let res = rt_poll
                    .spawn(async move { VoiceClient::new(creds)?.models(false).await })
                    .await;
                let Ok(Ok(catalog)) = res else { continue };
                let Some(entry) = catalog.models.into_iter().find(|m| m.id == model_id_for_poll)
                else {
                    continue;
                };
                let stop = this
                    .update(cx, |this, cx| {
                        this.setup.downloaded_bytes = entry.downloaded.map(|d| d as u64);
                        this.setup.total_bytes = Some(entry.size as u64);
                        this.setup.model = Some(entry.clone());
                        cx.notify();
                        entry.state == EnvironmentSpeechModelState::Installed
                    })
                    .unwrap_or(true);
                if stop {
                    break;
                }
            }
        }));

        let setup_token = self.start_request_id;
        self._setup_task = Some(cx.spawn(async move |this, cx| {
            let res = runtime
                .spawn(async move {
                    let client = VoiceClient::new(credentials)?;
                    let catalog = client.download_model(&model_id).await?;
                    let installed = catalog
                        .models
                        .iter()
                        .any(|m| m.id == model_id && m.state == EnvironmentSpeechModelState::Installed);
                    if installed {
                        client.select_model(&model_id).await?;
                    }
                    Ok::<_, VoiceClientError>((catalog, installed))
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                if this.start_request_id != setup_token {
                    return;
                }
                this._download_poll_task = None;
                this.setup.downloading = false;
                match res {
                    Ok(Ok((catalog, true))) => {
                        this.setup.model = catalog
                            .models
                            .into_iter()
                            .find(|m| m.id == this.setup.model.as_ref().map(|m| m.id.as_str()).unwrap_or(""));
                        this.setup.step = VoiceSetupStep::Ready;
                        this.setup.error = None;
                    }
                    Ok(Ok((_, false))) => {
                        this.setup.step = VoiceSetupStep::Confirm;
                        this.setup.error = None;
                    }
                    Ok(Err(err)) => {
                        this.setup.step = VoiceSetupStep::Error;
                        this.setup.error = Some(err.to_string());
                    }
                    Err(err) => {
                        this.setup.step = VoiceSetupStep::Error;
                        this.setup.error = Some(err.to_string());
                    }
                }
                cx.notify();
            });
        }));
    }

    pub fn setup_cancel_download(&mut self, cx: &mut Context<Self>) {
        self.start_request_id = self.start_request_id.wrapping_add(1);
        let (Some(credentials), Some(model)) =
            (self.setup_credentials.clone(), self.setup.model.clone())
        else {
            self.setup.open = false;
            cx.notify();
            return;
        };
        let model_id = model.id;
        let runtime = self.app.read(cx).runtime().clone();
        self._download_poll_task = None;
        self.setup.downloading = false;
        self.setup.step = VoiceSetupStep::Confirm;
        cx.notify();
        self._setup_task = Some(cx.spawn(async move |this, cx| {
            let _ = runtime
                .spawn(async move { VoiceClient::new(credentials)?.cancel_model_download(&model_id).await })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.setup.downloading = false;
                this.setup.step = VoiceSetupStep::Confirm;
                cx.notify();
            });
        }));
    }

    pub fn setup_retry_catalog(&mut self, cx: &mut Context<Self>) {
        if let (Some(creds), Some(model_id)) =
            (self.setup_credentials.clone(), self.setup_selected_model_id.clone())
        {
            self.load_setup_catalog(creds, model_id, cx);
        }
    }

    pub fn setup_start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.setup.open = false;
        self.start(window, cx);
    }

    #[cfg(feature = "snapshot")]
    pub fn set_snapshot_state_for_preview(
        &mut self,
        state: VoiceInputState,
        preview: Option<SpeechStreamText>,
        level: f32,
        elapsed_seconds: u64,
        setup: Option<VoiceSetupState>,
        cx: &mut Context<Self>,
    ) {
        self.machine = VoiceInputControllerMachine::new(true, true);
        self.live_preview = preview;
        self.audio_level = level;
        self.elapsed_seconds = elapsed_seconds;
        if let Some(setup) = setup {
            self.setup = setup;
        }
        self.machine.force_state_for_preview(state);
        cx.notify();
    }

    fn refresh_status_if_needed(&mut self, cx: &mut Context<Self>) {
        let resolution = resolve_voice_destination(self.app.read(cx), cx);
        let Some(active) = resolution.active else {
            self.status = None;
            self.status_env_id = None;
            return;
        };
        if matches!(active.destination, VoiceTranscriptionDestination::Cloud { .. }) {
            self.status = None;
            self.status_env_id = Some(active.destination.environment_id().to_owned());
            return;
        }
        let env_id = active.destination.environment_id().to_owned();
        if self.status_env_id.as_deref() == Some(env_id.as_str()) && self.status.is_some() {
            return;
        }
        self.status_env_id = Some(env_id.clone());
        let runtime = self.app.read(cx).runtime().clone();
        let creds = active.credentials;
        cx.spawn(async move |this, cx| {
            let res = runtime
                .spawn(async move { VoiceClient::new(creds)?.status().await })
                .await;
            if let Ok(Ok(status)) = res {
                let _ = this.update(cx, |this, cx| {
                    if this.status_env_id.as_deref() == Some(env_id.as_str()) {
                        this.status = Some(status);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn load_setup_catalog(
        &mut self,
        credentials: Credentials,
        selected_model_id: String,
        cx: &mut Context<Self>,
    ) {
        self.setup_credentials = Some(credentials.clone());
        self.setup_selected_model_id = Some(selected_model_id.clone());
        let runtime = self.app.read(cx).runtime().clone();
        let intent = crate::settings::store::try_client(cx)
            .map(|doc| doc.speech_language().to_owned())
            .unwrap_or_else(|| "auto".to_owned());
        let preferred = preferred_speech_languages(&[], None, &["en"]);
        self.setup = VoiceSetupState {
            open: true,
            step: VoiceSetupStep::Confirm,
            ..Default::default()
        };
        cx.notify();

        self.start_request_id = self.start_request_id.wrapping_add(1);
        let setup_token = self.start_request_id;
        self._setup_task = Some(cx.spawn(async move |this, cx| {
            let res = runtime
                .spawn(async move { VoiceClient::new(credentials)?.models(false).await })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.start_request_id != setup_token {
                    return;
                }
                match res {
                    Ok(Ok(catalog)) => {
                        if let Some((model, language)) = choose_setup_speech_model(
                            &catalog.models,
                            &selected_model_id,
                            &intent,
                            &preferred,
                        ) {
                            this.setup = VoiceSetupState {
                                open: true,
                                step: if model.state == EnvironmentSpeechModelState::Downloading {
                                    VoiceSetupStep::Downloading
                                } else {
                                    VoiceSetupStep::Confirm
                                },
                                model: Some(model.clone()),
                                language,
                                downloading: model.state == EnvironmentSpeechModelState::Downloading,
                                downloaded_bytes: model.downloaded.map(|d| d as u64),
                                total_bytes: Some(model.size as u64),
                                error: None,
                                catalog_failed: false,
                            };
                        } else {
                            this.setup = VoiceSetupState {
                                open: true,
                                step: VoiceSetupStep::Error,
                                catalog_failed: false,
                                error: Some(
                                    "No model matches your language. Choose a model or change the language in Voice settings."
                                        .to_owned(),
                                ),
                                ..Default::default()
                            };
                        }
                    }
                    Ok(Err(_)) | Err(_) => {
                        this.setup = VoiceSetupState {
                            open: true,
                            step: VoiceSetupStep::Error,
                            catalog_failed: true,
                            error: Some(
                                "Could not load transcription models. Check the connection and try again."
                                    .to_owned(),
                            ),
                            ..Default::default()
                        };
                    }
                }
                cx.notify();
            });
        }));
    }

    fn apply_effects(&mut self, effects: Vec<VoiceControllerEffect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                VoiceControllerEffect::RequestPermission => {
                    let token = self.machine.operation_token();
                    let platform = audio_platform(cx);
                    self._prepare_task = Some(cx.spawn(async move |this, cx| {
                        let outcome = platform.request_permission().await;
                        let (granted, can_ask_again) = match outcome {
                            PermissionOutcome::Granted => (true, true),
                            PermissionOutcome::Denied { .. } => {
                                let status = platform.permission_status();
                                (false, matches!(status, MicPermissionStatus::NotDetermined))
                            }
                        };
                        let _ = this.update(cx, |this, cx| {
                            let effects = this.machine.on_permission(token, granted, can_ask_again);
                            this.apply_effects(effects, cx);
                        });
                    }));
                }

                VoiceControllerEffect::PrepareTranscriber { operation_token } => {
                    self.run_prepare_transcriber(operation_token, cx);
                }

                VoiceControllerEffect::ConfigureRecording => {}

                VoiceControllerEffect::PrepareRecorder { operation_token } => {
                    let platform = audio_platform(cx);
                    let devices = platform.list_input_devices();
                    if let Some(doc) = crate::settings::store::try_client(cx) {
                        let current_order = doc.voice_microphones();
                        let updated_order = remember_microphones(&current_order, &devices);
                        if updated_order != current_order {
                            crate::settings::store::update_client(
                                &self.app,
                                ClientSettingChange::VoiceMicrophones(updated_order),
                                cx,
                            );
                        }
                    }
                    let status = platform.permission_status();
                    let denied = matches!(
                        status,
                        MicPermissionStatus::Denied | MicPermissionStatus::Restricted
                    );
                    let uri = format!("memory://voice/{}", uuid::Uuid::new_v4());
                    let current_draft = (self.draft_reader)(cx);
                    let result = if denied {
                        Err(MICROPHONE_PERMISSION_ERROR.to_owned())
                    } else {
                        Ok(Some(uri))
                    };
                    let effects = self.machine.on_recorder_prepared(
                        operation_token,
                        result,
                        current_draft,
                        denied,
                    );
                    self.apply_effects(effects, cx);
                }

                VoiceControllerEffect::StartLiveTranscription { uri: _, operation_token } => {
                    self.start_live_transcription(operation_token, cx);
                }

                VoiceControllerEffect::StartRecorder { .. } => {
                    self.start_audio_capture(cx);
                }

                VoiceControllerEffect::StopRecorder => {
                    self.stop_audio_capture(cx);
                }

                VoiceControllerEffect::ReleaseRecording => {
                    self.release_audio_capture();
                }

                VoiceControllerEffect::AbortOperation { operation_token } => {
                    self.aborted_tokens.insert(operation_token);
                    self.release_audio_capture();
                    self.cancel_active_stream();
                    self._op_task = None;
                    self._stream_updates_task = None;
                }

                VoiceControllerEffect::AbortPostProcessing { operation_token } => {
                    self.aborted_post_process_tokens.insert(operation_token);
                    self._op_task = None;
                    let effects = self
                        .machine
                        .on_post_processing_finished(operation_token, Err("skipped".to_owned()));
                    self.apply_effects(effects, cx);
                }

                VoiceControllerEffect::FinishTranscription { operation_token, uri } => {
                    self.finish_transcription(operation_token, uri, cx);
                }

                VoiceControllerEffect::StartPostProcessing {
                    operation_token,
                    transcript,
                    draft,
                } => {
                    self.start_post_processing(operation_token, transcript, draft, cx);
                }

                VoiceControllerEffect::ReadyToCommit { operation_token } => {
                    let current = (self.draft_reader)(cx);
                    let effects = self.machine.finalize_commit(operation_token, current.as_ref());
                    self.apply_effects(effects, cx);
                }

                VoiceControllerEffect::CommitDraft { text, selection } => {
                    self.submission_gate.on_commit_draft(&text);
                    self.live_preview = None;
                    cx.emit(VoiceControllerEvent::CommitDraft { text, selection });
                }

                VoiceControllerEffect::DeleteRecording(uri) => {
                    self.recordings.remove(&uri);
                    self.recording_order.retain(|item| item != &uri);
                }

                VoiceControllerEffect::ReportPostProcessingError(msg) => {
                    self.app.update(cx, |app, cx| {
                        app.show_toast(
                            crate::toast::Toast::new(
                                crate::toast::ToastKind::Error,
                                "Voice cleanup failed",
                            )
                            .description(msg),
                            cx,
                        );
                    });
                }

                VoiceControllerEffect::StateChanged(next_state) => {
                    let submit_text = self
                        .submission_gate
                        .on_state_changed(&next_state, &self.owner_key, false);
                    if !matches!(
                        next_state.phase,
                        VoiceInputPhase::Recording | VoiceInputPhase::Transcribing
                    ) {
                        self.live_preview = None;
                    }
                    if next_state.phase != VoiceInputPhase::Recording {
                        self._timer_task = None;
                        self.recording_started_at = None;
                        self.audio_level = 0.0;
                    }
                    if !next_state.is_busy() {
                        release_voice_session(cx.entity_id(), cx);
                    }
                    cx.emit(VoiceControllerEvent::StateChanged);
                    if let Some(text) = submit_text {
                        cx.emit(VoiceControllerEvent::SubmitAfterDictation { text });
                    }
                    cx.notify();
                }
            }
        }
    }

    fn run_prepare_transcriber(&mut self, operation_token: u64, cx: &mut Context<Self>) {
        let (consent_gen_ref, consent_generation) = voice_consent_generation(cx);
        let draft_env_id = (self.environment_id_reader)(cx);
        let project_id = (self.project_id_reader)(cx);
        let hint_strings = (self.hints_reader)(cx);
        let settings_doc = crate::settings::store::try_client(cx).cloned().unwrap_or_default();
        let app_state = self.app.read(cx);
        let resolution = resolve_voice_destination(app_state, cx);
        let Some(active) = resolution.active else {
            let effects = self.machine.on_transcriber_prepared(
                operation_token,
                Err(VoiceTranscriptionError::new(
                    VoiceTranscriptionErrorCode::Unavailable,
                    "Voice transcription is not available.",
                )),
            );
            self.apply_effects(effects, cx);
            return;
        };

        let project_words = app_state
            .server_config
            .config
            .as_ref()
            .map(|cfg| cfg.settings.project_speech_custom_words(project_id.as_deref()))
            .unwrap_or_default();
        let personal_words = settings_doc.speech_custom_words();
        let cleanup_creds = resolve_cleanup_credentials(app_state, draft_env_id.as_deref());
        let cleanup_granted = active.cleanup_granted;
        let correction_word = if cleanup_granted && cleanup_creds.is_some() {
            let trimmed = settings_doc.speech_correction_word().trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        } else {
            None
        };
        let mut hints: Vec<&str> = hint_strings.iter().map(String::as_str).collect();
        if let Some(cue) = correction_word.as_deref() {
            hints.insert(0, cue);
        }
        let vocabulary = resolve_speech_vocabulary(&personal_words, &project_words, &hints);
        let intent_lang = settings_doc.speech_language().to_owned();
        let translate = settings_doc.speech_translate_to_english();
        let remove_fillers = settings_doc.speech_remove_filler_words();
        let silence_filter = settings_doc.speech_silence_filtering();
        let post_prompt = settings_doc.speech_post_processing_prompt();
        let consent_grant_id = active.grant_id.clone();
        let cleanup_grant_id = active.cleanup_grant_id.clone();
        let owner_key = self.owner_key.clone();
        let runtime = app_state.runtime().clone();

        if let VoiceTranscriptionDestination::Cloud { provider, model, .. } = &active.destination {
            let provider = *provider;
            if provider == CloudSpeechProvider::Openrouter
                && model.as_deref().is_none_or(|m| m.trim().is_empty())
            {
                let effects = self.machine.on_transcriber_prepared(
                    operation_token,
                    Err(VoiceTranscriptionError::new(
                        VoiceTranscriptionErrorCode::Unavailable,
                        "Choose an OpenRouter model in Settings \u{2192} Voice.",
                    )),
                );
                self.apply_effects(effects, cx);
                return;
            }
            let creds = active.credentials.clone();
            let request_id = self.start_request_id;
            let cloud_langs: Vec<String> =
                CLOUD_SPEECH_LANGUAGES.iter().map(|s| (*s).to_owned()).collect();
            let locale = t3_core::voice_input::effective_speech_language(
                &cloud_langs,
                true,
                &intent_lang,
            );
            let options = SpeechTranscriptionOptions {
                speech_language: intent_lang,
                speech_translate_to_english: translate,
                speech_custom_words: vocabulary.clone(),
                speech_remove_filler_words: remove_fillers,
                speech_correction_word: correction_word.clone(),
                speech_silence_filtering: silence_filter,
            };
            let cleanup_options = SpeechPostProcessingOptions {
                speech_custom_words: vocabulary,
                speech_post_processing_prompt: post_prompt,
                speech_correction_word: correction_word,
            };
            self._prepare_task = Some(cx.spawn(async move |this, cx| {
                let keys_res = runtime
                    .spawn(async move { VoiceClient::new(creds)?.cloud_keys().await })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.aborted_tokens.contains(&operation_token)
                        || this.start_request_id != request_id
                    {
                        return;
                    }
                    let keys = match keys_res {
                        Ok(Ok(keys)) => keys,
                        Ok(Err(err)) => {
                            let effects = this.machine.on_transcriber_prepared(
                                operation_token,
                                Err(VoiceTranscriptionError::new(
                                    VoiceTranscriptionErrorCode::PreparationFailed,
                                    err.to_string(),
                                )),
                            );
                            this.apply_effects(effects, cx);
                            return;
                        }
                        Err(err) => {
                            let effects = this.machine.on_transcriber_prepared(
                                operation_token,
                                Err(VoiceTranscriptionError::new(
                                    VoiceTranscriptionErrorCode::PreparationFailed,
                                    err.to_string(),
                                )),
                            );
                            this.apply_effects(effects, cx);
                            return;
                        }
                    };
                    if !keys.is_configured(provider) {
                        let msg = CloudSpeechError {
                            provider,
                            reason: CloudSpeechErrorReason::MissingKey,
                        }
                        .message();
                        let effects = this.machine.on_transcriber_prepared(
                            operation_token,
                            Err(VoiceTranscriptionError::new(
                                VoiceTranscriptionErrorCode::Unavailable,
                                msg,
                            )),
                        );
                        this.apply_effects(effects, cx);
                        return;
                    }
                    this.machine.set_enable_post_processing(cleanup_granted);
                    this.prepared = Some(PreparedStart {
                        owner_key,
                        destination: active,
                        consent_grant_id,
                        consent_gen_ref,
                        consent_generation,
                        draft_environment_id: draft_env_id,
                        cleanup_requested: cleanup_granted,
                        cleanup_grant_id,
                        options,
                        cleanup_options,
                        can_post_process: cleanup_granted,
                    });
                    let effects = this
                        .machine
                        .on_transcriber_prepared(operation_token, Ok((locale, false, false)));
                    this.apply_effects(effects, cx);
                });
            }));
            return;
        }

        let creds = active.credentials.clone();
        let env_id = active.destination.environment_id().to_owned();
        let offer_setup = self.offer_setup;
        let request_id = self.start_request_id;

        self._prepare_task = Some(cx.spawn(async move |this, cx| {
            let creds_clone = creds.clone();
            let status_res = runtime
                .spawn(async move { VoiceClient::new(creds_clone)?.status().await })
                .await;

            let _ = this.update(cx, |this, cx| {
                if this.aborted_tokens.contains(&operation_token)
                    || this.start_request_id != request_id
                {
                    return;
                }
                let latest_status = match status_res {
                    Ok(Ok(status)) => status,
                    Ok(Err(err)) => {
                        let effects = this.machine.on_transcriber_prepared(
                            operation_token,
                            Err(VoiceTranscriptionError::new(
                                VoiceTranscriptionErrorCode::PreparationFailed,
                                err.to_string(),
                            )),
                        );
                        this.apply_effects(effects, cx);
                        return;
                    }
                    Err(err) => {
                        let effects = this.machine.on_transcriber_prepared(
                            operation_token,
                            Err(VoiceTranscriptionError::new(
                                VoiceTranscriptionErrorCode::PreparationFailed,
                                err.to_string(),
                            )),
                        );
                        this.apply_effects(effects, cx);
                        return;
                    }
                };

                this.status = Some(latest_status.clone());
                this.status_env_id = Some(env_id);

                match &latest_status {
                    EnvironmentSpeechStatus::Unsupported { reason } => {
                        let effects = this.machine.on_transcriber_prepared(
                            operation_token,
                            Err(VoiceTranscriptionError::new(
                                VoiceTranscriptionErrorCode::Unavailable,
                                reason.clone(),
                            )),
                        );
                        this.apply_effects(effects, cx);
                    }
                    EnvironmentSpeechStatus::Supported {
                        state: EnvironmentSpeechState::MissingModel,
                        model_id,
                        ..
                    } => {
                        let model_id = model_id.clone();
                        let cancel_effects = this.machine.cancel();
                        this.apply_effects(cancel_effects, cx);
                        if offer_setup {
                            this.load_setup_catalog(creds, model_id, cx);
                        } else {
                            let effects = this.machine.on_transcriber_prepared(
                                operation_token,
                                Err(VoiceTranscriptionError::new(
                                    VoiceTranscriptionErrorCode::Unavailable,
                                    "Voice transcription is not available.",
                                )),
                            );
                            this.apply_effects(effects, cx);
                        }
                    }
                    EnvironmentSpeechStatus::Supported {
                        supports_streaming,
                        supports_buffered_streaming,
                        supports_translation,
                        languages,
                        supports_language_detection,
                        ..
                    } => {
                        let effective_lang = t3_core::voice_input::effective_speech_language(
                            languages.as_deref().unwrap_or(&[]),
                            supports_language_detection.unwrap_or(true),
                            &intent_lang,
                        );
                        let locale = if !*supports_streaming && *supports_translation && translate {
                            "en".to_owned()
                        } else {
                            effective_lang.clone()
                        };
                        let can_stream =
                            *supports_streaming || supports_buffered_streaming.unwrap_or(false);
                        let options = SpeechTranscriptionOptions {
                            speech_language: effective_lang,
                            speech_translate_to_english: translate,
                            speech_custom_words: vocabulary.clone(),
                            speech_remove_filler_words: remove_fillers,
                            speech_correction_word: correction_word.clone(),
                            speech_silence_filtering: silence_filter,
                        };
                        let cleanup_options = SpeechPostProcessingOptions {
                            speech_custom_words: vocabulary,
                            speech_post_processing_prompt: post_prompt,
                            speech_correction_word: correction_word,
                        };
                        this.machine.set_enable_post_processing(cleanup_granted);
                        this.prepared = Some(PreparedStart {
                            owner_key,
                            destination: active,
                            consent_grant_id,
                            consent_gen_ref,
                            consent_generation,
                            draft_environment_id: draft_env_id,
                            cleanup_requested: cleanup_granted,
                            cleanup_grant_id,
                            options,
                            cleanup_options,
                            can_post_process: cleanup_granted,
                        });
                        // `/ws/voice` uses `StreamCommand::Finish` rather than a live-start promise,
                        // so `has_live_start` is always `false` here.
                        let effects = this.machine.on_transcriber_prepared(
                            operation_token,
                            Ok((locale, can_stream, false)),
                        );
                        this.apply_effects(effects, cx);
                    }
                }
            });
        }));
    }

    fn start_live_transcription(&mut self, operation_token: u64, cx: &mut Context<Self>) {
        let Some(prepared) = self.prepared.as_ref() else {
            return;
        };
        let creds = prepared.destination.credentials.clone();
        let options = prepared.options.clone();
        let consent_gen_ref = prepared.consent_gen_ref.clone();
        let expected_consent_gen = prepared.consent_generation;
        let runtime = self.app.read(cx).runtime().clone();

        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<StreamCommand>();
        let can_send = Arc::new(AtomicBool::new(true));
        self.active_stream = Some(ActiveStreamHandle {
            cmd_tx,
            can_send: can_send.clone(),
        });

        let (update_tx, mut update_rx) = futures::channel::mpsc::unbounded::<SpeechStreamText>();
        let (stream_err_tx, stream_err_rx) = tokio::sync::oneshot::channel::<String>();

        self._stream_updates_task = Some(cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            let mut stream_err_rx = Some(stream_err_rx);
            loop {
                tokio::select! {
                    maybe_text = update_rx.next() => {
                        let Some(text) = maybe_text else {
                            break;
                        };
                        let keep = this
                            .update(cx, |this, cx| {
                                if this.aborted_tokens.contains(&operation_token) {
                                    return false;
                                }
                                this.live_preview = Some(text);
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !keep {
                            break;
                        }
                    }
                    err = async {
                        match stream_err_rx.as_mut() {
                            Some(rx) => rx.await.ok(),
                            None => std::future::pending().await,
                        }
                    }, if stream_err_rx.is_some() => {
                        stream_err_rx = None;
                        if let Some(msg) = err {
                            let _ = this.update(cx, |this, cx| {
                                if !this.aborted_tokens.contains(&operation_token)
                                    && this.machine.state().phase == VoiceInputPhase::Recording
                                {
                                    let effects = this.machine.interrupt_recording(Some(&msg), None);
                                    this.apply_effects(effects, cx);
                                }
                            });
                        }
                    }
                }
            }
        }));

        runtime.spawn(async move {
            let can_send_flag = can_send.clone();
            let gen_ref = consent_gen_ref.clone();
            let open_fut = async move {
                let client = VoiceClient::new(creds)?;
                client
                    .open_stream(
                        options,
                        Arc::new(move || {
                            can_send_flag.load(Ordering::SeqCst)
                                && gen_ref.load(Ordering::SeqCst) == expected_consent_gen
                        }),
                    )
                    .await
            };
            tokio::pin!(open_fut);

            let mut pending_chunks: Vec<Vec<u8>> = Vec::new();
            let mut pending_finish: Option<
                tokio::sync::oneshot::Sender<Result<String, VoiceClientError>>,
            > = None;
            let mut cancelled = false;

            let opened = loop {
                tokio::select! {
                    res = &mut open_fut => break Some(res),
                    cmd = cmd_rx.recv() => match cmd {
                        Some(StreamCommand::Feed(bytes)) => {
                            if pending_finish.is_none() && !cancelled {
                                pending_chunks.push(bytes);
                            }
                        }
                        Some(StreamCommand::Finish(reply)) => {
                            pending_finish = Some(reply);
                        }
                        Some(StreamCommand::Cancel) | None => {
                            cancelled = true;
                            if let Some(reply) = pending_finish.take() {
                                let _ = reply.send(Err(VoiceClientError::Stream(
                                    "Voice transcription was cancelled.".to_owned(),
                                )));
                            }
                            break None;
                        }
                    }
                }
            };

            let Some(opened) = opened else {
                return;
            };

            let mut session = match opened {
                Ok(session) => session,
                Err(err) => {
                    let msg = err.to_string();
                    if let Some(reply) = pending_finish.take() {
                        let _ = reply.send(Err(err));
                        return;
                    }
                    let _ = stream_err_tx.send(msg.clone());
                    while let Some(cmd) = cmd_rx.recv().await {
                        match cmd {
                            StreamCommand::Feed(_) => {}
                            StreamCommand::Finish(reply) => {
                                let _ = reply.send(Err(VoiceClientError::Stream(msg)));
                                break;
                            }
                            StreamCommand::Cancel => break,
                        }
                    }
                    return;
                }
            };

            if cancelled {
                session.cancel();
                return;
            }

            if let Some(mut updates) = session.take_updates() {
                tokio::spawn(async move {
                    while let Some(text) = updates.recv().await {
                        if update_tx.unbounded_send(text).is_err() {
                            break;
                        }
                    }
                });
            }

            for chunk in pending_chunks {
                if !can_send.load(Ordering::SeqCst)
                    || consent_gen_ref.load(Ordering::SeqCst) != expected_consent_gen
                {
                    session.cancel();
                    if let Some(reply) = pending_finish.take() {
                        let _ = reply.send(Err(VoiceClientError::Stream(
                            "Voice transcription consent was turned off.".to_owned(),
                        )));
                    }
                    return;
                }
                let _ = session.feed_bytes(chunk);
            }

            if let Some(reply) = pending_finish.take() {
                let res = session.finish().await;
                let _ = reply.send(res);
                return;
            }

            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    StreamCommand::Feed(bytes) => {
                        if can_send.load(Ordering::SeqCst)
                            && consent_gen_ref.load(Ordering::SeqCst) == expected_consent_gen
                        {
                            let _ = session.feed_bytes(bytes);
                        }
                    }
                    StreamCommand::Finish(reply) => {
                        let res = session.finish().await;
                        let _ = reply.send(res);
                        return;
                    }
                    StreamCommand::Cancel => {
                        session.cancel();
                        return;
                    }
                }
            }
            session.cancel();
        });
    }

    fn start_audio_capture(&mut self, cx: &mut Context<Self>) {
        enum CaptureMsg {
            Level(f32),
            Limit,
            Error(String),
        }

        let platform = audio_platform(cx);
        let devices = platform.list_input_devices();
        let microphones = crate::settings::store::try_client(cx)
            .map(|doc| doc.voice_microphones())
            .unwrap_or_default();
        let device_id = preferred_microphone_id(&microphones, &devices);

        let pcm_buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
        self.active_pcm_buffer = Some(pcm_buffer.clone());
        self.audio_level = 0.0;
        self.elapsed_seconds = 0;
        self.recording_started_at = Some(Instant::now());

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<CaptureMsg>();
        let tx_level = tx.clone();
        let tx_limit = tx.clone();
        let tx_error = tx;
        let stream_tx = self.active_stream.as_ref().map(|handle| handle.cmd_tx.clone());
        let stream_can_send = self.active_stream.as_ref().map(|handle| handle.can_send.clone());
        let consent_check = self
            .prepared
            .as_ref()
            .map(|p| (p.consent_gen_ref.clone(), p.consent_generation));

        let callbacks = AudioCaptureCallbacks {
            on_level: Arc::new(move |level| {
                let _ = tx_level.unbounded_send(CaptureMsg::Level(level));
            }),
            on_pcm: Arc::new(move |samples| {
                if let Some((gen_ref, expected_gen)) = &consent_check
                    && gen_ref.load(Ordering::SeqCst) != *expected_gen
                {
                    return;
                }
                if let Some(can_send) = &stream_can_send
                    && !can_send.load(Ordering::SeqCst)
                {
                    return;
                }
                let bytes = f32_slice_to_le_bytes(&samples);
                pcm_buffer
                    .lock()
                    .expect("pcm buffer lock")
                    .extend_from_slice(&bytes);
                if let Some(cmd_tx) = &stream_tx {
                    let _ = cmd_tx.send(StreamCommand::Feed(bytes));
                }
            }),
            on_limit: Arc::new(move || {
                let _ = tx_limit.unbounded_send(CaptureMsg::Limit);
            }),
            on_error: Arc::new(move |msg| {
                let _ = tx_error.unbounded_send(CaptureMsg::Error(msg));
            }),
        };

        let capture = match platform.start_capture(&device_id, false, callbacks) {
            Ok(session) => session,
            Err(err) => {
                let msg = match err {
                    CaptureStartError::PermissionDenied(msg) => msg,
                    CaptureStartError::Unsupported(msg) | CaptureStartError::Failed(msg) => msg,
                };
                let effects = self.machine.interrupt_recording(Some(&msg), None);
                self.apply_effects(effects, cx);
                return;
            }
        };
        self.active_capture = Some(capture);

        self._capture_events_task = Some(cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(msg) = rx.next().await {
                match msg {
                    CaptureMsg::Level(level) => {
                        let _ = this.update(cx, |this, cx| {
                            this.audio_level = level;
                            cx.notify();
                        });
                    }
                    CaptureMsg::Limit => {
                        let _ = this.update(cx, |this, cx| {
                            this.stop(cx);
                        });
                    }
                    CaptureMsg::Error(err_msg) => {
                        let _ = this.update(cx, |this, cx| {
                            let effects = this.machine.interrupt_recording(Some(&err_msg), None);
                            this.apply_effects(effects, cx);
                        });
                    }
                }
            }
        }));

        self._timer_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let keep = this
                    .update(cx, |this, cx| {
                        let Some(started) = this.recording_started_at else {
                            return false;
                        };
                        let secs = started.elapsed().as_secs();
                        if secs != this.elapsed_seconds {
                            this.elapsed_seconds = secs;
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn stop_audio_capture(&mut self, _cx: &mut Context<Self>) {
        self._timer_task = None;
        self.recording_started_at = None;
        self.audio_level = 0.0;
        if let Some(mut capture) = self.active_capture.take() {
            let _ = capture.stop();
        }
        if let (Some(uri), Some(buf)) = (
            self.machine.recording_uri().map(str::to_owned),
            self.active_pcm_buffer.take(),
        ) {
            let bytes = std::mem::take(&mut *buf.lock().expect("pcm buffer lock"));
            self.store_recording(uri, bytes);
        }
    }

    fn release_audio_capture(&mut self) {
        self._timer_task = None;
        self._capture_events_task = None;
        self.recording_started_at = None;
        self.audio_level = 0.0;
        if let Some(mut capture) = self.active_capture.take() {
            let _ = capture.stop();
        }
        self.active_pcm_buffer = None;
    }

    fn cancel_active_stream(&mut self) {
        if let Some(handle) = self.active_stream.take() {
            handle.can_send.store(false, Ordering::SeqCst);
            let _ = handle.cmd_tx.send(StreamCommand::Cancel);
        }
    }

    fn store_recording(&mut self, uri: String, bytes: Vec<u8>) {
        self.recordings.insert(uri.clone(), bytes);
        self.recording_order.retain(|existing| existing != &uri);
        self.recording_order.push(uri);
        while self.recording_order.len() > MAX_RETAINED_RECORDINGS {
            let oldest = self.recording_order.remove(0);
            self.recordings.remove(&oldest);
        }
    }

    fn has_matching_active_consent(&self, prepared: &PreparedStart, cx: &App) -> bool {
        resolve_voice_destination(self.app.read(cx), cx)
            .active
            .is_some_and(|active| active.grant_id == prepared.consent_grant_id)
    }

    fn finish_transcription(
        &mut self,
        operation_token: u64,
        uri: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(prepared) = self.prepared.as_ref() else {
            let effects = self.machine.on_transcription_finished(
                operation_token,
                Err(VoiceTranscriptionError::new(
                    VoiceTranscriptionErrorCode::Unavailable,
                    "Voice transcription is not available.",
                )),
                false,
            );
            self.apply_effects(effects, cx);
            return;
        };

        if !self.has_matching_active_consent(prepared, cx) {
            self.cancel_active_stream();
            let effects = self.machine.on_transcription_finished(
                operation_token,
                Err(VoiceTranscriptionError::new(
                    VoiceTranscriptionErrorCode::Unavailable,
                    "Voice transcription was turned off before finishing.",
                )),
                false,
            );
            self.apply_effects(effects, cx);
            return;
        }

        let creds = prepared.destination.credentials.clone();
        let dest = prepared.destination.destination.clone();
        let options = prepared.options.clone();
        let can_post_process = prepared.can_post_process;
        let consent_gen_ref = prepared.consent_gen_ref.clone();
        let expected_consent_gen = prepared.consent_generation;
        let stream_handle = self.active_stream.take();
        let pcm_bytes = uri
            .as_deref()
            .and_then(|u| self.recordings.get(u).cloned())
            .unwrap_or_default();
        let runtime = self.app.read(cx).runtime().clone();

        self._op_task = Some(cx.spawn(async move |this, cx| {
            let res: Result<String, VoiceTranscriptionError> = if let Some(handle) = stream_handle {
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                let finish_res = if handle.cmd_tx.send(StreamCommand::Finish(reply_tx)).is_err() {
                    Err(VoiceClientError::Stream(
                        "Could not finish voice recording.".to_owned(),
                    ))
                } else {
                    match reply_rx.await {
                        Ok(res) => res,
                        Err(_) => Err(VoiceClientError::Stream(
                            "Could not finish voice recording.".to_owned(),
                        )),
                    }
                };
                match finish_res {
                    Ok(text) => Ok(text),
                    Err(err) => Err(VoiceTranscriptionError::new(
                        VoiceTranscriptionErrorCode::TranscriptionFailed,
                        err.to_string(),
                    )),
                }
            } else if pcm_bytes.is_empty() {
                Err(VoiceTranscriptionError::new(
                    VoiceTranscriptionErrorCode::TranscriptionFailed,
                    "No speech was detected.",
                ))
            } else if consent_gen_ref.load(Ordering::SeqCst) != expected_consent_gen {
                Err(VoiceTranscriptionError::new(
                    VoiceTranscriptionErrorCode::Unavailable,
                    "Voice transcription was turned off before finishing.",
                ))
            } else {
                let transcribe_res = runtime
                    .spawn(async move {
                        let client = VoiceClient::new(creds)?;
                        match dest {
                            VoiceTranscriptionDestination::Environment { .. } => {
                                client.transcribe(&pcm_bytes, &options).await
                            }
                            VoiceTranscriptionDestination::Cloud {
                                provider, model, ..
                            } => {
                                client
                                    .transcribe_cloud(
                                        provider,
                                        &pcm_bytes,
                                        &options,
                                        model.as_deref().filter(|m| !m.trim().is_empty()),
                                    )
                                    .await
                            }
                        }
                    })
                    .await;
                match transcribe_res {
                    Ok(Ok(out)) => Ok(out.text),
                    Ok(Err(err)) => Err(VoiceTranscriptionError::new(
                        VoiceTranscriptionErrorCode::TranscriptionFailed,
                        err.to_string(),
                    )),
                    Err(err) => Err(VoiceTranscriptionError::new(
                        VoiceTranscriptionErrorCode::TranscriptionFailed,
                        err.to_string(),
                    )),
                }
            };

            let _ = this.update(cx, |this, cx| {
                if this.aborted_tokens.contains(&operation_token) {
                    return;
                }
                let effects = this.machine.on_transcription_finished(
                    operation_token,
                    res,
                    can_post_process,
                );
                this.apply_effects(effects, cx);
            });
        }));
    }

    fn start_post_processing(
        &mut self,
        operation_token: u64,
        transcript: String,
        draft: VoiceDraftSnapshot,
        cx: &mut Context<Self>,
    ) {
        let Some(prepared) = self.prepared.as_ref() else {
            let effects = self
                .machine
                .on_post_processing_finished(operation_token, Ok(transcript));
            self.apply_effects(effects, cx);
            return;
        };

        let app_state = self.app.read(cx);
        let active_resolution = resolve_voice_destination(app_state, cx);
        let still_consented = active_resolution.active.as_ref().is_some_and(|active| {
            active.grant_id == prepared.consent_grant_id
                && prepared.cleanup_requested
                && active.cleanup_grant_id.is_some()
                && active.cleanup_grant_id == prepared.cleanup_grant_id
        });
        if !still_consented || prepared.owner_key != draft.owner_key {
            let effects = self
                .machine
                .on_post_processing_finished(operation_token, Ok(transcript));
            self.apply_effects(effects, cx);
            return;
        }

        let Some(creds) =
            resolve_cleanup_credentials(app_state, prepared.draft_environment_id.as_deref())
        else {
            let effects = self.machine.on_post_processing_finished(
                operation_token,
                Err("The draft's environment is unavailable for transcript cleanup.".to_owned()),
            );
            self.apply_effects(effects, cx);
            return;
        };

        let options = prepared.cleanup_options.clone();
        let (ctx_text, ctx_sel) = cleanup_context(&draft.text, draft.selection);
        let req = EnvironmentSpeechPostProcessingRequest {
            options,
            transcript,
            draft: SpeechPostProcessingDraft {
                text: ctx_text,
                selection: SpeechPostProcessingSelection {
                    start: ctx_sel.start,
                    end: ctx_sel.end,
                },
            },
        };
        let runtime = app_state.runtime().clone();

        self._op_task = Some(cx.spawn(async move |this, cx| {
            let res = runtime
                .spawn(async move { VoiceClient::new(creds)?.post_process(&req).await })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.aborted_tokens.contains(&operation_token)
                    || this.aborted_post_process_tokens.contains(&operation_token)
                {
                    return;
                }
                let outcome = match res {
                    Ok(Ok(out)) => Ok(out.text),
                    Ok(Err(err)) => Err(err.to_string()),
                    Err(err) => Err(err.to_string()),
                };
                let effects = this
                    .machine
                    .on_post_processing_finished(operation_token, outcome);
                this.apply_effects(effects, cx);
            });
        }));
    }
}
