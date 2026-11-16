//! Voice input controller state machine, UTF-16 transcript/caret merging,
//! error classification, and dictation shortcut/submission decision logic.
//!
//! Mirrors `packages/client-runtime/src/voice-input/{controller,transcription}.ts`,
//! `apps/web/src/speech/useDictationShortcut.ts`, and
//! `apps/web/src/speech/useDictationSubmissionShortcut.ts`.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub const MICROPHONE_PERMISSION_ERROR: &str = "Microphone access is required for voice input. Allow access in your browser or system settings and try again.";
pub const HOLD_THRESHOLD_MS: u64 = 300;
pub const CLEANUP_CONTEXT_LIMIT_UTF16: usize = 1_024;

const WORD_SPACELESS_LANGUAGES: &[&str] = &["ja", "zh", "yue", "th", "lo", "km", "my"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoiceInputPhase {
    #[default]
    Idle,
    Preparing,
    Recording,
    Transcribing,
    PostProcessing,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceErrorAction {
    Retry,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceInputState {
    pub phase: VoiceInputPhase,
    pub error: Option<String>,
    pub error_action: Option<VoiceErrorAction>,
}

impl VoiceInputState {
    pub const fn idle() -> Self {
        Self { phase: VoiceInputPhase::Idle, error: None, error_action: None }
    }

    pub const fn preparing() -> Self {
        Self { phase: VoiceInputPhase::Preparing, error: None, error_action: None }
    }

    pub const fn recording() -> Self {
        Self { phase: VoiceInputPhase::Recording, error: None, error_action: None }
    }

    pub const fn transcribing() -> Self {
        Self { phase: VoiceInputPhase::Transcribing, error: None, error_action: None }
    }

    pub const fn post_processing() -> Self {
        Self { phase: VoiceInputPhase::PostProcessing, error: None, error_action: None }
    }

    pub fn error(message: impl Into<String>, action: Option<VoiceErrorAction>) -> Self {
        Self { phase: VoiceInputPhase::Error, error: Some(message.into()), error_action: action }
    }

    pub const fn blocks_submission(&self) -> bool {
        voice_input_blocks_submission(self)
    }

    pub const fn is_busy(&self) -> bool {
        !matches!(self.phase, VoiceInputPhase::Idle | VoiceInputPhase::Error)
    }
}

pub const fn voice_input_blocks_submission(state: &VoiceInputState) -> bool {
    matches!(
        state.phase,
        VoiceInputPhase::Preparing
            | VoiceInputPhase::Recording
            | VoiceInputPhase::Transcribing
            | VoiceInputPhase::PostProcessing
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoiceTranscriptionErrorCode {
    Unavailable,
    UnsupportedLocale,
    PreparationFailed,
    TranscriptionFailed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct VoiceTranscriptionError {
    pub code: VoiceTranscriptionErrorCode,
    pub message: String,
}

impl VoiceTranscriptionError {
    pub fn new(code: VoiceTranscriptionErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    pub fn cancelled() -> Self {
        Self::new(VoiceTranscriptionErrorCode::Cancelled, "Voice transcription was cancelled.")
    }

    pub fn preparation_message(&self) -> &str {
        if self.code == VoiceTranscriptionErrorCode::UnsupportedLocale {
            "Voice transcription is not available for this language."
        } else if self.message.trim().is_empty() {
            "Could not prepare voice transcription."
        } else {
            &self.message
        }
    }

    pub fn transcription_message(&self) -> &str {
        &self.message
    }
}

pub fn preparation_error_message(error: Option<&VoiceTranscriptionError>, busy: bool) -> String {
    if busy {
        return "Voice transcription is still finishing. Try again shortly.".to_owned();
    }
    match error {
        Some(err) => err.preparation_message().to_owned(),
        None => "Could not prepare voice transcription.".to_owned(),
    }
}

pub fn transcription_error_message(error: Option<&VoiceTranscriptionError>, busy: bool) -> String {
    if busy {
        return "Voice transcription is still finishing. Try again shortly.".to_owned();
    }
    match error {
        Some(err) => err.transcription_message().to_owned(),
        None => "Could not transcribe this recording.".to_owned(),
    }
}

/// UTF-16 code unit selection range, matching JS string indices in `VoiceDraftSnapshot`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceSelection {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceDraftSnapshot {
    pub owner_key: String,
    pub text: String,
    pub selection: VoiceSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptCommitResult {
    Commit { text: String, selection: VoiceSelection },
    Stale,
    Empty,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VoiceRecorderStatus {
    pub is_finished: bool,
    pub has_error: bool,
    pub error: Option<String>,
    pub url: Option<String>,
}

/// Converts a UTF-8 byte offset in `text` to a UTF-16 code unit offset.
pub fn utf8_to_utf16_offset(text: &str, byte_offset: usize) -> usize {
    let mut clamped = byte_offset.min(text.len());
    while clamped > 0 && !text.is_char_boundary(clamped) {
        clamped -= 1;
    }
    text[..clamped].encode_utf16().count()
}

/// Converts a UTF-16 code unit offset in `text` to a UTF-8 byte offset, snapping down if
/// `utf16_offset` lands inside a surrogate pair.
pub fn utf16_to_utf8_offset(text: &str, utf16_offset: usize) -> usize {
    let mut units = 0usize;
    for (byte_idx, ch) in text.char_indices() {
        let next_units = units + ch.len_utf16();
        if utf16_offset < next_units {
            return byte_idx;
        }
        units = next_units;
    }
    text.len()
}

/// Replaces `[range_start..range_end]` (UTF-16 code units) in `text` with `replacement`,
/// returning `(updated_text, caret_utf16)`.
pub fn replace_text_range_utf16(
    text: &str,
    range_start: usize,
    range_end: usize,
    replacement: &str,
) -> (String, usize) {
    let text_u16: Vec<u16> = text.encode_utf16().collect();
    let start_u16 = range_start.min(text_u16.len());
    let end_u16 = range_end.min(text_u16.len()).max(start_u16);
    let mut out_u16 = Vec::with_capacity(
        start_u16 + replacement.encode_utf16().count() + (text_u16.len() - end_u16),
    );
    out_u16.extend_from_slice(&text_u16[..start_u16]);
    out_u16.extend(replacement.encode_utf16());
    let caret = out_u16.len();
    out_u16.extend_from_slice(&text_u16[end_u16..]);
    (String::from_utf16_lossy(&out_u16), caret)
}

const fn is_high_surrogate(code: u16) -> bool {
    code >= 0xD800 && code <= 0xDBFF
}

const fn is_low_surrogate(code: u16) -> bool {
    code >= 0xDC00 && code <= 0xDFFF
}

fn map_position_utf16(before: &[u16], text: &[u16], position: usize, suffix_first: bool) -> usize {
    let shared = before.len().min(text.len());
    let mut prefix = 0usize;
    let mut suffix = 0usize;

    let scan_suffix = |limit: usize, suffix: &mut usize| {
        while *suffix < limit
            && before[before.len() - 1 - *suffix] == text[text.len() - 1 - *suffix]
        {
            *suffix += 1;
        }
        if *suffix > 0 && is_low_surrogate(before[before.len() - *suffix]) {
            *suffix -= 1;
        }
    };

    if suffix_first {
        scan_suffix(shared, &mut suffix);
    }
    while prefix < shared - suffix && before[prefix] == text[prefix] {
        prefix += 1;
    }
    if prefix > 0 && is_high_surrogate(before[prefix - 1]) {
        prefix -= 1;
    }
    if !suffix_first {
        scan_suffix(shared - prefix, &mut suffix);
    }

    if position < prefix {
        position
    } else if position >= before.len() - suffix {
        position + text.len() - before.len()
    } else {
        text.len() - suffix
    }
}

static AUTO_LEFT_EDGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[A-Za-z0-9.!?,:;)\]}\'"]$"#).expect("valid regex"));
static AUTO_RIGHT_EDGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[A-Za-z0-9(\[\{\'"]"#).expect("valid regex"));
static UNICODE_LEFT_EDGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:\p{L}|\p{M}|\p{N}|[.!?,:;)\]}\'"])$"#).expect("valid regex"));
static UNICODE_RIGHT_EDGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(?:\p{L}|\p{M}|\p{N}|[(\[\{\'"])"#).expect("valid regex"));
static AUTO_LATIN_OR_NUM_START_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(?:\p{Latin}|\p{N})"#).expect("valid regex"));
static AUTO_LATIN_OR_NUM_END_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:\p{Latin}|\p{N})$"#).expect("valid regex"));
static WS_START_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s"#).expect("valid regex"));
static WS_END_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\s$"#).expect("valid regex"));

/// `resolveTranscriptCommit` from `packages/client-runtime/src/voice-input/controller.ts`.
pub fn resolve_transcript_commit(
    captured: &VoiceDraftSnapshot,
    current: Option<&VoiceDraftSnapshot>,
    transcript: &str,
    locale: &str,
) -> TranscriptCommitResult {
    let Some(current) = current else {
        return TranscriptCommitResult::Stale;
    };
    if current.owner_key != captured.owner_key {
        return TranscriptCommitResult::Stale;
    }

    let replacement = transcript.trim();
    if replacement.is_empty() {
        return TranscriptCommitResult::Empty;
    }

    let text = &current.text;
    let text_u16: Vec<u16> = text.encode_utf16().collect();
    let unchanged = text == &captured.text;
    let end = if unchanged {
        captured.selection.end.min(text_u16.len())
    } else {
        let before_u16: Vec<u16> = captured.text.encode_utf16().collect();
        let pos = captured.selection.end.min(before_u16.len());
        map_position_utf16(&before_u16, &text_u16, pos, false)
            .max(map_position_utf16(&before_u16, &text_u16, pos, true))
    };
    let start = if unchanged { captured.selection.start.min(end) } else { end };
    let is_empty_selection = start == end;
    let language = locale.split(['-', '_']).next().unwrap_or("").to_lowercase();

    let mut insertion = replacement.to_owned();
    if is_empty_selection && !WORD_SPACELESS_LANGUAGES.contains(&language.as_str()) {
        let auto = language == "auto";
        let left_start = start.saturating_sub(2);
        let right_end = (start + 2).min(text_u16.len());
        let left = String::from_utf16_lossy(&text_u16[left_start..start]);
        let right = String::from_utf16_lossy(&text_u16[start..right_end]);

        let left_edge = if auto { AUTO_LEFT_EDGE_RE.is_match(&left) } else { UNICODE_LEFT_EDGE_RE.is_match(&left) };
        let right_edge = if auto { AUTO_RIGHT_EDGE_RE.is_match(&right) } else { UNICODE_RIGHT_EDGE_RE.is_match(&right) };

        let left_needs_boundary = (!auto || AUTO_LATIN_OR_NUM_START_RE.is_match(replacement))
            && left_edge
            && (right.is_empty() || WS_START_RE.is_match(&right));
        let right_needs_boundary = (!auto || AUTO_LATIN_OR_NUM_END_RE.is_match(replacement))
            && right_edge
            && (left.is_empty() || WS_END_RE.is_match(&left));

        insertion = format!(
            "{}{}{}",
            if left_needs_boundary { " " } else { "" },
            replacement,
            if right_needs_boundary { " " } else { "" }
        );
    }

    let (next_text, cursor) = replace_text_range_utf16(text, start, end, &insertion);
    TranscriptCommitResult::Commit {
        text: next_text,
        selection: VoiceSelection { start: cursor, end: cursor },
    }
}

/// `cleanupContext` from `apps/web/src/speech/useEnvironmentSpeechInput.ts`.
pub fn cleanup_context(text: &str, selection: VoiceSelection) -> (String, VoiceSelection) {
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = selection.start.min(units.len());
    let end = selection.end.min(units.len()).max(start);
    let before_start = start.saturating_sub(CLEANUP_CONTEXT_LIMIT_UTF16);
    let selected_end = end.min(start + CLEANUP_CONTEXT_LIMIT_UTF16);
    let after_end = (end + CLEANUP_CONTEXT_LIMIT_UTF16).min(units.len());

    let mut slice = Vec::with_capacity(
        (start - before_start) + (selected_end - start) + (after_end - end),
    );
    slice.extend_from_slice(&units[before_start..start]);
    slice.extend_from_slice(&units[start..selected_end]);
    slice.extend_from_slice(&units[end..after_end]);

    let before_len = start - before_start;
    let selected_len = selected_end - start;
    (
        String::from_utf16_lossy(&slice),
        VoiceSelection { start: before_len, end: before_len + selected_len },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceControllerEffect {
    RequestPermission,
    PrepareTranscriber { operation_token: u64 },
    ConfigureRecording,
    PrepareRecorder { operation_token: u64 },
    StartLiveTranscription { uri: String, operation_token: u64 },
    StartRecorder { for_duration_secs: u64 },
    StopRecorder,
    ReleaseRecording,
    AbortOperation { operation_token: u64 },
    AbortPostProcessing { operation_token: u64 },
    FinishTranscription { operation_token: u64, uri: Option<String> },
    StartPostProcessing { operation_token: u64, transcript: String, draft: VoiceDraftSnapshot },
    ReadyToCommit { operation_token: u64 },
    CommitDraft { text: String, selection: VoiceSelection },
    DeleteRecording(String),
    ReportPostProcessingError(String),
    StateChanged(VoiceInputState),
}

#[derive(Debug, Clone)]
struct RetryRecording {
    uri: String,
    owner_key: String,
    locale: String,
    uses_streaming: bool,
}

/// Pure state machine for `VoiceInputController` (`packages/client-runtime/src/voice-input/controller.ts`).
#[derive(Debug, Clone)]
pub struct VoiceInputControllerMachine {
    state: VoiceInputState,
    operation_token: u64,
    retry_failed_recording: bool,
    has_transcriber: bool,
    enable_post_processing: bool,
    captured_draft: Option<VoiceDraftSnapshot>,
    recording_uri: Option<String>,
    owned_recording_uris: BTreeSet<String>,
    recording_configured: bool,
    finishing: bool,
    locale: String,
    uses_streaming: bool,
    has_live_start: bool,
    live_transcription_started: bool,
    live_transcription_in_flight: bool,
    live_text: Option<String>,
    skip_post_processing_requested: bool,
    pending_raw_transcript: Option<String>,
    retry_recording: Option<RetryRecording>,
}

impl VoiceInputControllerMachine {
    pub fn new(retry_failed_recording: bool, has_transcriber: bool) -> Self {
        Self {
            state: VoiceInputState::idle(),
            operation_token: 0,
            retry_failed_recording,
            has_transcriber,
            enable_post_processing: false,
            captured_draft: None,
            recording_uri: None,
            owned_recording_uris: BTreeSet::new(),
            recording_configured: false,
            finishing: false,
            locale: "auto".to_owned(),
            uses_streaming: false,
            has_live_start: false,
            live_transcription_started: false,
            live_transcription_in_flight: false,
            live_text: None,
            skip_post_processing_requested: false,
            pending_raw_transcript: None,
            retry_recording: None,
        }
    }

    pub fn set_has_transcriber(&mut self, has_transcriber: bool) {
        self.has_transcriber = has_transcriber;
    }

    pub fn set_enable_post_processing(&mut self, enabled: bool) {
        self.enable_post_processing = enabled;
    }

    pub const fn state(&self) -> &VoiceInputState {
        &self.state
    }

    pub const fn operation_token(&self) -> u64 {
        self.operation_token
    }

    pub fn recording_uri(&self) -> Option<&str> {
        self.recording_uri.as_deref()
    }

    pub fn force_state_for_preview(&mut self, state: VoiceInputState) {
        self.state = state;
    }

    pub fn start(&mut self, draft: Option<VoiceDraftSnapshot>) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if !matches!(self.state.phase, VoiceInputPhase::Idle | VoiceInputPhase::Error) {
            return effects;
        }
        let Some(initiating_draft) = draft else {
            self.set_error("This draft is no longer available.", Some(VoiceErrorAction::Retry), &mut effects);
            return effects;
        };

        self.operation_token += 1;
        let token = self.operation_token;
        self.set_state(VoiceInputState::preparing(), &mut effects);

        if let Some(retry) = &self.retry_recording
            && retry.owner_key != initiating_draft.owner_key
        {
            self.retry_recording = None;
            self.delete_owned_recordings(&mut effects);
        }

        if let Some(retry) = self.retry_recording.take() {
            self.recording_uri = Some(retry.uri.clone());
            self.captured_draft = Some(initiating_draft);
            self.locale = retry.locale;
            self.uses_streaming = retry.uses_streaming;
            self.finishing = true;
            self.set_state(VoiceInputState::transcribing(), &mut effects);
            effects.push(VoiceControllerEffect::FinishTranscription {
                operation_token: token,
                uri: Some(retry.uri),
            });
            return effects;
        }

        if !self.has_transcriber {
            self.set_error("Voice transcription is not available.", None, &mut effects);
            return effects;
        }

        self.captured_draft = Some(initiating_draft);
        effects.push(VoiceControllerEffect::RequestPermission);
        effects
    }

    pub fn on_permission(
        &mut self,
        operation_token: u64,
        granted: bool,
        can_ask_again: bool,
    ) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if !self.is_current(operation_token) || self.state.phase != VoiceInputPhase::Preparing {
            return effects;
        }
        if !granted {
            self.set_error(
                "Microphone access is required for voice input.",
                Some(if can_ask_again { VoiceErrorAction::Retry } else { VoiceErrorAction::Settings }),
                &mut effects,
            );
            self.release_resources(&mut effects);
            return effects;
        }
        effects.push(VoiceControllerEffect::PrepareTranscriber { operation_token });
        effects
    }

    pub fn on_transcriber_prepared(
        &mut self,
        operation_token: u64,
        result: Result<(String, bool, bool), VoiceTranscriptionError>,
    ) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if !self.is_current(operation_token) || self.state.phase != VoiceInputPhase::Preparing {
            return effects;
        }
        match result {
            Ok((locale, uses_streaming, has_live_start)) => {
                self.locale = locale;
                self.uses_streaming = uses_streaming;
                self.has_live_start = has_live_start;
                self.recording_configured = true;
                effects.push(VoiceControllerEffect::ConfigureRecording);
                effects.push(VoiceControllerEffect::PrepareRecorder { operation_token });
            }
            Err(err) => {
                let msg = err.preparation_message().to_owned();
                self.set_error(&msg, Some(VoiceErrorAction::Retry), &mut effects);
                self.release_resources(&mut effects);
            }
        }
        effects
    }

    pub fn on_recorder_prepared(
        &mut self,
        operation_token: u64,
        result: Result<Option<String>, String>,
        current_draft: Option<VoiceDraftSnapshot>,
        is_permission_error: bool,
    ) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if !self.is_current(operation_token) || self.state.phase != VoiceInputPhase::Preparing {
            return effects;
        }
        match result {
            Ok(uri) => {
                self.recording_uri = uri.clone();
                if let Some(ref u) = uri {
                    self.owned_recording_uris.insert(u.clone());
                }
                let matches_owner = match (&self.captured_draft, &current_draft) {
                    (Some(initial), Some(curr)) => initial.owner_key == curr.owner_key,
                    _ => false,
                };
                if !matches_owner {
                    self.set_error("This draft is no longer available.", Some(VoiceErrorAction::Retry), &mut effects);
                    self.release_resources(&mut effects);
                    return effects;
                }
                self.captured_draft = current_draft;
                if self.has_live_start
                    && let Some(u) = uri
                {
                    self.live_transcription_started = true;
                    self.live_transcription_in_flight = true;
                    effects.push(VoiceControllerEffect::StartLiveTranscription { uri: u, operation_token });
                }
                effects.push(VoiceControllerEffect::StartRecorder {
                    for_duration_secs: super::contracts::VOICE_RECORDING_LIMIT_SECONDS,
                });
                self.set_state(VoiceInputState::recording(), &mut effects);
            }
            Err(_) => {
                let msg = if is_permission_error {
                    MICROPHONE_PERMISSION_ERROR
                } else {
                    "Could not start voice recording."
                };
                self.set_error(msg, Some(VoiceErrorAction::Retry), &mut effects);
                self.release_resources(&mut effects);
            }
        }
        effects
    }

    pub fn on_live_transcription_result(
        &mut self,
        operation_token: u64,
        result: Result<Option<String>, VoiceTranscriptionError>,
    ) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if !self.is_current(operation_token) {
            return effects;
        }
        self.live_transcription_in_flight = false;
        match result {
            Ok(text) => match self.state.phase {
                VoiceInputPhase::Recording => {
                    self.live_text = text;
                }
                VoiceInputPhase::Transcribing => {
                    if let Some(live) = text {
                        self.complete_transcription_step(
                            operation_token,
                            live,
                            self.enable_post_processing,
                            &mut effects,
                        );
                    } else {
                        effects.push(VoiceControllerEffect::FinishTranscription {
                            operation_token,
                            uri: self.recording_uri.clone(),
                        });
                    }
                }
                _ => {}
            },
            Err(err) => match self.state.phase {
                VoiceInputPhase::Recording => {
                    let msg = err.transcription_message().to_owned();
                    self.discard_recording(Some(&msg), &mut effects);
                }
                VoiceInputPhase::Transcribing => {
                    self.fail_transcription_error(err, false, &mut effects);
                }
                _ => {}
            },
        }
        effects
    }

    pub fn stop(&mut self) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if self.state.phase != VoiceInputPhase::Recording {
            return effects;
        }
        self.begin_finish(false, None, &mut effects);
        effects
    }

    pub fn handle_recorder_status(&mut self, status: &VoiceRecorderStatus) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if self.state.phase != VoiceInputPhase::Recording {
            return effects;
        }
        if let (Some(url), Some(uri)) = (&status.url, &self.recording_uri)
            && url != uri
        {
            return effects;
        }
        if status.has_error {
            let msg = status.error.as_deref().unwrap_or("Voice recording was interrupted.");
            return self.interrupt_recording(Some(msg), status.url.clone());
        }
        if status.is_finished {
            let Some(url) = status.url.clone() else {
                return self.interrupt_recording(None, None);
            };
            self.begin_finish(true, Some(url), &mut effects);
        }
        effects
    }

    pub fn interrupt_recording(
        &mut self,
        message: Option<&str>,
        completed_uri: Option<String>,
    ) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        let msg = message.unwrap_or("Voice recording was interrupted.");
        if self.state.phase == VoiceInputPhase::Preparing {
            self.invalidate_operation(&mut effects);
            self.set_error(msg, Some(VoiceErrorAction::Retry), &mut effects);
            self.release_resources(&mut effects);
            return effects;
        }
        if self.state.phase != VoiceInputPhase::Recording {
            return effects;
        }
        if let Some(uri) = completed_uri {
            self.owned_recording_uris.insert(uri.clone());
            self.recording_uri = Some(uri);
        }
        self.discard_recording(Some(msg), &mut effects);
        effects
    }

    pub fn app_moved_to_background(&mut self) -> Vec<VoiceControllerEffect> {
        let msg = if self.state.phase == VoiceInputPhase::Preparing {
            Some("Voice input stopped when the app moved to the background.")
        } else {
            None
        };
        self.interrupt_recording(msg, None)
    }

    pub fn on_transcription_finished(
        &mut self,
        operation_token: u64,
        result: Result<String, VoiceTranscriptionError>,
        enable_post_processing: bool,
    ) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if !self.is_current(operation_token) || self.state.phase != VoiceInputPhase::Transcribing {
            return effects;
        }
        let transcript = match result {
            Ok(text) => text,
            Err(err) => {
                let allow_retry = !self.live_transcription_started && !self.uses_streaming;
                self.fail_transcription_error(err, allow_retry, &mut effects);
                return effects;
            }
        };

        self.complete_transcription_step(operation_token, transcript, enable_post_processing, &mut effects);
        effects
    }

    pub fn skip_post_processing(&mut self) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if self.state.phase != VoiceInputPhase::PostProcessing {
            return effects;
        }
        self.skip_post_processing_requested = true;
        effects.push(VoiceControllerEffect::AbortPostProcessing {
            operation_token: self.operation_token,
        });
        effects
    }

    pub fn on_post_processing_finished(
        &mut self,
        operation_token: u64,
        result: Result<String, String>,
    ) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if !self.is_current(operation_token) || self.state.phase != VoiceInputPhase::PostProcessing {
            return effects;
        }
        match result {
            Ok(cleaned) => {
                self.pending_raw_transcript = Some(cleaned);
            }
            Err(err) => {
                if !self.skip_post_processing_requested {
                    effects.push(VoiceControllerEffect::ReportPostProcessingError(err));
                }
            }
        }
        effects.push(VoiceControllerEffect::ReadyToCommit { operation_token });
        effects
    }

    pub fn finalize_commit(
        &mut self,
        operation_token: u64,
        current_draft: Option<&VoiceDraftSnapshot>,
    ) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        if !self.is_current(operation_token) {
            return effects;
        }
        let Some(captured) = self.captured_draft.as_ref() else {
            self.set_error("Could not finish voice recording.", Some(VoiceErrorAction::Retry), &mut effects);
            self.finishing = false;
            self.release_resources(&mut effects);
            return effects;
        };
        let transcript = self.pending_raw_transcript.take().unwrap_or_default();
        match resolve_transcript_commit(captured, current_draft, &transcript, &self.locale) {
            TranscriptCommitResult::Stale => {
                self.set_error(
                    "The draft changed while voice input was running. The transcript was not added.",
                    Some(VoiceErrorAction::Retry),
                    &mut effects,
                );
            }
            TranscriptCommitResult::Empty => {
                self.set_error("No speech was detected.", Some(VoiceErrorAction::Retry), &mut effects);
            }
            TranscriptCommitResult::Commit { text, selection } => {
                effects.push(VoiceControllerEffect::CommitDraft { text, selection });
                self.set_state(VoiceInputState::idle(), &mut effects);
            }
        }
        self.finishing = false;
        self.release_resources(&mut effects);
        effects
    }

    pub fn cancel(&mut self) -> Vec<VoiceControllerEffect> {
        let mut effects = Vec::new();
        match self.state.phase {
            VoiceInputPhase::Idle => {}
            VoiceInputPhase::Error => {
                self.retry_recording = None;
                self.delete_owned_recordings(&mut effects);
                self.set_state(VoiceInputState::idle(), &mut effects);
            }
            VoiceInputPhase::Preparing | VoiceInputPhase::Transcribing | VoiceInputPhase::PostProcessing => {
                self.invalidate_operation(&mut effects);
                self.set_state(VoiceInputState::idle(), &mut effects);
                self.release_resources(&mut effects);
            }
            VoiceInputPhase::Recording => {
                self.discard_recording(None, &mut effects);
            }
        }
        effects
    }

    pub fn owner_changed(&mut self) -> Vec<VoiceControllerEffect> {
        if self.state.phase == VoiceInputPhase::Idle {
            return Vec::new();
        }
        self.cancel()
    }

    pub fn dispose(&mut self) -> Vec<VoiceControllerEffect> {
        self.cancel()
    }

    fn begin_finish(
        &mut self,
        already_stopped: bool,
        completed_uri: Option<String>,
        effects: &mut Vec<VoiceControllerEffect>,
    ) {
        if self.finishing
            || !matches!(self.state.phase, VoiceInputPhase::Recording | VoiceInputPhase::Preparing)
        {
            return;
        }
        self.finishing = true;
        let token = self.operation_token;
        self.set_state(VoiceInputState::transcribing(), effects);
        if !already_stopped {
            effects.push(VoiceControllerEffect::StopRecorder);
        }
        if self.recording_configured {
            effects.push(VoiceControllerEffect::ReleaseRecording);
            self.recording_configured = false;
        }
        if let Some(uri) = completed_uri {
            self.owned_recording_uris.insert(uri.clone());
            self.recording_uri = Some(uri);
        }
        if !self.uses_streaming && self.recording_uri.is_none() || self.captured_draft.is_none() {
            self.set_error("Could not finish voice recording.", Some(VoiceErrorAction::Retry), effects);
            self.finishing = false;
            self.release_resources(effects);
            return;
        }
        if let Some(live) = self.live_text.take() {
            self.complete_transcription_step(token, live, self.enable_post_processing, effects);
        } else if !self.live_transcription_in_flight {
            effects.push(VoiceControllerEffect::FinishTranscription {
                operation_token: token,
                uri: self.recording_uri.clone(),
            });
        }
    }

    fn complete_transcription_step(
        &mut self,
        operation_token: u64,
        transcript: String,
        enable_post_processing: bool,
        effects: &mut Vec<VoiceControllerEffect>,
    ) {
        if enable_post_processing
            && !transcript.trim().is_empty()
            && let Some(draft) = self.captured_draft.clone()
        {
            self.skip_post_processing_requested = false;
            self.pending_raw_transcript = Some(transcript.clone());
            self.set_state(VoiceInputState::post_processing(), effects);
            effects.push(VoiceControllerEffect::StartPostProcessing {
                operation_token,
                transcript,
                draft,
            });
            return;
        }

        self.pending_raw_transcript = Some(transcript);
        effects.push(VoiceControllerEffect::ReadyToCommit { operation_token });
    }

    fn fail_transcription_error(
        &mut self,
        err: VoiceTranscriptionError,
        allow_retry_recording: bool,
        effects: &mut Vec<VoiceControllerEffect>,
    ) {
        if self.retry_failed_recording
            && allow_retry_recording
            && let (Some(uri), Some(draft)) = (&self.recording_uri, &self.captured_draft)
        {
            self.retry_recording = Some(RetryRecording {
                uri: uri.clone(),
                owner_key: draft.owner_key.clone(),
                locale: self.locale.clone(),
                uses_streaming: self.uses_streaming,
            });
        }
        let msg = err.transcription_message().to_owned();
        self.set_error(&msg, Some(VoiceErrorAction::Retry), effects);
        self.finishing = false;
        self.release_resources(effects);
    }

    fn discard_recording(&mut self, error: Option<&str>, effects: &mut Vec<VoiceControllerEffect>) {
        self.invalidate_operation(effects);
        if let Some(msg) = error {
            self.set_error(msg, Some(VoiceErrorAction::Retry), effects);
        } else {
            self.set_state(VoiceInputState::idle(), effects);
        }
        self.release_resources(effects);
    }

    fn invalidate_operation(&mut self, effects: &mut Vec<VoiceControllerEffect>) {
        let old = self.operation_token;
        self.operation_token += 1;
        effects.push(VoiceControllerEffect::AbortOperation { operation_token: old });
    }

    fn release_resources(&mut self, effects: &mut Vec<VoiceControllerEffect>) {
        if self.recording_configured {
            effects.push(VoiceControllerEffect::StopRecorder);
            effects.push(VoiceControllerEffect::ReleaseRecording);
            self.recording_configured = false;
        }
        self.recording_uri = None;
        self.delete_owned_recordings(effects);
        self.captured_draft = None;
        self.has_live_start = false;
        self.uses_streaming = false;
        self.live_transcription_started = false;
        self.live_transcription_in_flight = false;
        self.live_text = None;
        self.pending_raw_transcript = None;
        self.skip_post_processing_requested = false;
        self.finishing = false;
    }

    fn delete_owned_recordings(&mut self, effects: &mut Vec<VoiceControllerEffect>) {
        let keep = self.retry_recording.as_ref().map(|r| r.uri.clone());
        let uris: Vec<String> = self.owned_recording_uris.iter().cloned().collect();
        self.owned_recording_uris.clear();
        for uri in uris {
            if keep.as_deref() == Some(uri.as_str()) {
                self.owned_recording_uris.insert(uri);
            } else {
                effects.push(VoiceControllerEffect::DeleteRecording(uri));
            }
        }
    }

    const fn is_current(&self, operation_token: u64) -> bool {
        operation_token == self.operation_token
    }

    fn set_error(
        &mut self,
        message: &str,
        action: Option<VoiceErrorAction>,
        effects: &mut Vec<VoiceControllerEffect>,
    ) {
        self.set_state(VoiceInputState::error(message, action), effects);
    }

    fn set_state(&mut self, state: VoiceInputState, effects: &mut Vec<VoiceControllerEffect>) {
        self.state = state.clone();
        effects.push(VoiceControllerEffect::StateChanged(state));
    }
}

/// `VoiceShortcutMode` from `packages/contracts/src/settings.ts`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceShortcutMode {
    #[default]
    Auto,
    Hold,
    Toggle,
}

impl VoiceShortcutMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Hold => "hold",
            Self::Toggle => "toggle",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "hold" => Some(Self::Hold),
            "toggle" => Some(Self::Toggle),
            _ => None,
        }
    }
}

/// `shouldFinishDictationOnRelease` from `apps/web/src/speech/useDictationShortcut.ts`.
pub const fn should_finish_dictation_on_release(mode: VoiceShortcutMode, elapsed_ms: u64) -> bool {
    matches!(mode, VoiceShortcutMode::Hold)
        || (matches!(mode, VoiceShortcutMode::Auto) && elapsed_ms >= HOLD_THRESHOLD_MS)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationTargetSnapshot {
    pub target_id: u64,
    pub phase: VoiceInputPhase,
    pub focused: bool,
    pub targeted: bool,
    pub available: bool,
    pub disabled: bool,
}

/// `ownsShortcut` from `apps/web/src/speech/useDictationShortcut.ts`:
/// the busy editor wins, then the focused editor, then the untargeted fallback editor.
pub fn resolve_dictation_shortcut_owner(targets: &[DictationTargetSnapshot]) -> Option<u64> {
    if let Some(busy) =
        targets.iter().find(|t| !matches!(t.phase, VoiceInputPhase::Idle | VoiceInputPhase::Error))
    {
        return Some(busy.target_id);
    }
    if let Some(focused) = targets.iter().find(|t| t.focused) {
        return Some(focused.target_id);
    }
    targets.iter().find(|t| !t.targeted).map(|t| t.target_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationShortcutAction {
    None,
    Start,
    Stop,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveDictationPress {
    generation: u64,
    key_code: String,
    started_at_ms: u64,
    mode: VoiceShortcutMode,
}

/// Tracks keydown / keyup / blur / Escape decision state matching `useDictationShortcut.ts`.
#[derive(Debug, Clone, Default)]
pub struct DictationShortcutState {
    generation: u64,
    pressed: Option<ActiveDictationPress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictationKeydown<'a> {
    pub phase: VoiceInputPhase,
    pub available: bool,
    pub disabled: bool,
    pub repeat: bool,
    pub key_code: &'a str,
    pub now_ms: u64,
    pub mode: VoiceShortcutMode,
}

impl DictationShortcutState {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn has_active_press(&self) -> bool {
        self.pressed.is_some()
    }

    pub fn on_escape(
        &mut self,
        phase: VoiceInputPhase,
        in_other_target: bool,
    ) -> DictationShortcutAction {
        if in_other_target {
            return DictationShortcutAction::None;
        }
        let active = self.pressed.is_some()
            || !matches!(phase, VoiceInputPhase::Idle | VoiceInputPhase::Error);
        if !active {
            return DictationShortcutAction::None;
        }
        self.pressed = None;
        self.generation += 1;
        DictationShortcutAction::Cancel
    }

    pub fn on_dictation_keydown(&mut self, event: DictationKeydown<'_>) -> DictationShortcutAction {
        let can_start = matches!(event.phase, VoiceInputPhase::Idle | VoiceInputPhase::Error);
        if can_start && (!event.available || event.disabled) {
            return DictationShortcutAction::None;
        }
        if event.repeat || (!can_start && event.phase != VoiceInputPhase::Recording) {
            return DictationShortcutAction::None;
        }
        // macOS may omit the previous non-modifier keyup while Command is held.
        self.pressed = None;
        self.generation += 1;
        if event.phase == VoiceInputPhase::Recording {
            return DictationShortcutAction::Stop;
        }
        self.pressed = Some(ActiveDictationPress {
            generation: self.generation,
            key_code: event.key_code.to_owned(),
            started_at_ms: event.now_ms,
            mode: event.mode,
        });
        DictationShortcutAction::Start
    }

    pub fn on_keyup_or_blur(
        &mut self,
        key_code: Option<&str>,
        now_ms: u64,
    ) -> DictationShortcutAction {
        let Some(press) = &self.pressed else {
            return DictationShortcutAction::None;
        };
        if let Some(code) = key_code
            && code != press.key_code
        {
            return DictationShortcutAction::None;
        }
        let press = self.pressed.take().expect("checked");
        if press.generation == self.generation
            && should_finish_dictation_on_release(press.mode, now_ms.saturating_sub(press.started_at_ms))
        {
            DictationShortcutAction::Stop
        } else {
            DictationShortcutAction::None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictationSubmissionDecision {
    Ignore,
    SubmitImmediately,
    QueueAndStopRecording,
    WaitForCompletion,
}

/// Queued post-dictation submission state (`submitAfterDictation` in `useEnvironmentSpeechInput.ts`).
#[derive(Debug, Clone, Default)]
pub struct DictationSubmissionGate {
    pending_owner_key: Option<String>,
    committed_text: Option<String>,
}

impl DictationSubmissionGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn has_pending(&self) -> bool {
        self.pending_owner_key.is_some()
    }

    pub fn request_submit(
        &mut self,
        owner_key: &str,
        disabled: bool,
        state: &VoiceInputState,
    ) -> DictationSubmissionDecision {
        if disabled || state.phase == VoiceInputPhase::Preparing {
            return DictationSubmissionDecision::Ignore;
        }
        if !voice_input_blocks_submission(state) {
            return DictationSubmissionDecision::SubmitImmediately;
        }
        if self.pending_owner_key.is_some() {
            return DictationSubmissionDecision::WaitForCompletion;
        }
        self.pending_owner_key = Some(owner_key.to_owned());
        self.committed_text = None;
        if state.phase == VoiceInputPhase::Recording {
            DictationSubmissionDecision::QueueAndStopRecording
        } else {
            DictationSubmissionDecision::WaitForCompletion
        }
    }

    pub fn on_commit_draft(&mut self, text: &str) {
        if self.pending_owner_key.is_some() {
            self.committed_text = Some(text.to_owned());
        }
    }

    pub fn on_state_changed(
        &mut self,
        state: &VoiceInputState,
        current_owner_key: &str,
        disabled: bool,
    ) -> Option<String> {
        if state.phase == VoiceInputPhase::Error {
            self.clear();
            return None;
        }
        if state.phase != VoiceInputPhase::Idle {
            return None;
        }
        let pending_owner = self.pending_owner_key.take()?;
        let text = self.committed_text.take()?;
        if !disabled && pending_owner == current_owner_key {
            Some(text)
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.pending_owner_key = None;
        self.committed_text = None;
    }
}

pub const WAVEFORM_BAR_COUNT: usize = 24;
pub const WAVEFORM_REST_SCALE: f32 = 0.2;

/// `resolveSpeechPresentation` from `apps/web/src/components/chat/ComposerSpeechButton.tsx`.
pub fn resolve_speech_presentation(
    state: &VoiceInputState,
    progress: Option<(u64, u64)>,
) -> (Option<String>, bool) {
    match state.phase {
        VoiceInputPhase::Idle => (None, false),
        VoiceInputPhase::Error => (
            Some(state.error.clone().unwrap_or_else(|| "Voice input failed".to_owned())),
            false,
        ),
        VoiceInputPhase::Preparing => {
            let status = match progress {
                Some((downloaded, total)) => {
                    let pct = ((downloaded as f64 / (total.max(1) as f64)) * 100.0).round() as u64;
                    format!("Downloading model {pct}%")
                }
                None => "Starting\u{2026}".to_owned(),
            };
            (Some(status), true)
        }
        VoiceInputPhase::Recording => (Some("Recording".to_owned()), true),
        VoiceInputPhase::Transcribing => (Some("Transcribing\u{2026}".to_owned()), true),
        VoiceInputPhase::PostProcessing => (Some("Cleaning up\u{2026}".to_owned()), true),
    }
}

/// `formatElapsed` from `apps/web/src/components/chat/ComposerSpeechButton.tsx`.
pub fn format_elapsed_mm_ss(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// `VOICE_ERROR_LABELS` from `apps/web/src/components/chat/VoiceInputErrorPill.tsx`.
pub fn voice_error_label(error: Option<&str>) -> &'static str {
    let Some(msg) = error else {
        return "Voice input failed";
    };
    if msg == MICROPHONE_PERMISSION_ERROR {
        return "Mic blocked";
    }
    match msg {
        "Microphone access is required for voice input." => "Mic blocked",
        "This draft is no longer available." => "Draft unavailable",
        "Another voice recording is already active." => "Already recording",
        "Voice transcription is not available." => "Voice input unavailable",
        "Voice transcription is still finishing. Try again shortly." => "Still transcribing",
        "Voice transcription is not available for this language." => "Language unsupported",
        "Could not prepare voice transcription." | "Could not start voice recording." => {
            "Couldn\u{2019}t start"
        }
        "Voice recording was interrupted." => "Recording interrupted",
        "Could not finish voice recording." => "Couldn\u{2019}t finish",
        "Could not transcribe this recording."
        | "Voice transcription on this environment failed." => "Transcription failed",
        "The draft changed while voice input was running. The transcript was not added." => {
            "Draft changed"
        }
        "No speech was detected." => "No speech heard",
        "Microphone processing failed." => "Mic failed",
        "The microphone disconnected." => "Mic disconnected",
        _ => "Voice input failed",
    }
}

/// `RETRYABLE_TRANSCRIPTIONS` from `apps/web/src/components/chat/VoiceInputErrorPill.tsx`.
pub fn is_retryable_transcription_error(error: Option<&str>) -> bool {
    matches!(
        error,
        Some(
            "Could not transcribe this recording."
                | "Voice transcription on this environment failed."
        )
    )
}
