//! Pure state machine for the `/ws/voice` streaming protocol.
//!
//! Mirrors `packages/client-runtime/src/voice-input/stream.ts`:
//! - bounds queued + in-flight PCM to five minutes (`SPEECH_STREAM_MAX_QUEUED_BYTES`)
//! - splits/coalesces PCM into chunks of at most `SPEECH_STREAM_MAX_CHUNK_BYTES`
//! - pipelines at most 2 in-flight binary audio chunks before waiting for `update` acks
//! - drains all queued and in-flight audio before sending `{ type: "finish" }`
//! - enforces the 120-second response timeout and live consent gating (`can_send`)

use std::collections::VecDeque;

use super::contracts::{
    SPEECH_STREAM_MAX_CHUNK_BYTES, SPEECH_STREAM_MAX_QUEUED_BYTES, SpeechStreamCommand,
    SpeechStreamEvent, SpeechStreamText, SpeechTranscriptionOptions, decode_stream_event,
    encode_stream_command,
};

pub const STREAM_RESPONSE_TIMEOUT_MS: u64 = 120_000;
pub const STREAM_MAX_IN_FLIGHT_CHUNKS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceStreamAction {
    SendText(String),
    SendBinary(Vec<u8>),
    ArmTimeout { timeout_ms: u64 },
    DisarmTimeout,
    Text(SpeechStreamText),
    Finished(String),
    Failed { message: String, notify: bool },
    CloseSocket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceStreamPhase {
    Connecting,
    Streaming,
    Finishing,
    Finished,
    Failed,
}

#[derive(Debug, Clone)]
pub struct VoiceStreamMachine {
    options: SpeechTranscriptionOptions,
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    in_flight: VecDeque<usize>,
    in_flight_bytes: usize,
    finishing: bool,
    finish_sent: bool,
    closed: bool,
    is_ready: bool,
    revision: i64,
    timeout_armed: bool,
    phase: VoiceStreamPhase,
    final_result: Option<Result<String, String>>,
}

impl VoiceStreamMachine {
    pub fn new(options: SpeechTranscriptionOptions) -> Self {
        Self {
            options,
            queue: VecDeque::new(),
            queued_bytes: 0,
            in_flight: VecDeque::new(),
            in_flight_bytes: 0,
            finishing: false,
            finish_sent: false,
            closed: false,
            is_ready: false,
            revision: -1,
            timeout_armed: true,
            phase: VoiceStreamPhase::Connecting,
            final_result: None,
        }
    }

    pub const fn phase(&self) -> VoiceStreamPhase {
        self.phase
    }

    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    pub const fn is_ready(&self) -> bool {
        self.is_ready
    }

    pub const fn is_timeout_armed(&self) -> bool {
        self.timeout_armed
    }

    pub const fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }

    pub const fn in_flight_bytes(&self) -> usize {
        self.in_flight_bytes
    }

    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    pub fn final_result(&self) -> Option<&Result<String, String>> {
        self.final_result.as_ref()
    }

    pub fn on_open(&mut self, can_send: bool) -> Vec<VoiceStreamAction> {
        let mut actions = Vec::new();
        if self.closed {
            return actions;
        }
        if !can_send {
            self.fail("Voice consent was revoked.", true, &mut actions);
            return actions;
        }
        match encode_stream_command(&SpeechStreamCommand::Start { options: self.options.clone() }) {
            Ok(json) => actions.push(VoiceStreamAction::SendText(json)),
            Err(err) => self.fail(&err.to_string(), true, &mut actions),
        }
        actions
    }

    pub fn feed_f32(&mut self, samples: &[f32], can_send: bool) -> Vec<VoiceStreamAction> {
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for &sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        self.feed_bytes(&bytes, can_send)
    }

    pub fn feed_bytes(&mut self, bytes: &[u8], can_send: bool) -> Vec<VoiceStreamAction> {
        let mut actions = Vec::new();
        if self.closed || self.finishing {
            return actions;
        }
        if self.queued_bytes + self.in_flight_bytes + bytes.len() > SPEECH_STREAM_MAX_QUEUED_BYTES {
            self.fail("Voice recordings are limited to five minutes.", true, &mut actions);
            return actions;
        }

        let mut offset = 0;
        while offset < bytes.len() {
            if let Some(tail) = self.queue.back_mut()
                && tail.len() < SPEECH_STREAM_MAX_CHUNK_BYTES
            {
                let available = SPEECH_STREAM_MAX_CHUNK_BYTES - tail.len();
                let appended = available.min(bytes.len() - offset);
                tail.extend_from_slice(&bytes[offset..offset + appended]);
                self.queued_bytes += appended;
                offset += appended;
                continue;
            }
            let end = (offset + SPEECH_STREAM_MAX_CHUNK_BYTES).min(bytes.len());
            let chunk = bytes[offset..end].to_vec();
            self.queued_bytes += chunk.len();
            self.queue.push_back(chunk);
            offset = end;
        }

        self.pump(can_send, &mut actions);
        actions
    }

    pub fn finish(&mut self, can_send: bool) -> Vec<VoiceStreamAction> {
        let mut actions = Vec::new();
        if self.closed {
            return actions;
        }
        self.finishing = true;
        if self.is_ready {
            self.phase = VoiceStreamPhase::Finishing;
        }
        self.pump(can_send, &mut actions);
        actions
    }

    pub fn cancel(&mut self) -> Vec<VoiceStreamAction> {
        let mut actions = Vec::new();
        self.fail("Voice transcription was cancelled.", false, &mut actions);
        actions
    }

    pub fn on_timeout(&mut self) -> Vec<VoiceStreamAction> {
        let mut actions = Vec::new();
        self.fail("Live transcription stopped responding.", true, &mut actions);
        actions
    }

    pub fn on_socket_closed(&mut self) -> Vec<VoiceStreamAction> {
        let mut actions = Vec::new();
        self.fail("The voice connection closed. Please record again.", true, &mut actions);
        actions
    }

    pub fn on_socket_error(&mut self) -> Vec<VoiceStreamAction> {
        let mut actions = Vec::new();
        self.fail("Could not connect to live transcription.", true, &mut actions);
        actions
    }

    pub fn on_text_message(&mut self, raw: &str, can_send: bool) -> Vec<VoiceStreamAction> {
        let mut actions = Vec::new();
        if self.closed {
            return actions;
        }
        if !can_send {
            self.fail("Voice consent was revoked.", true, &mut actions);
            return actions;
        }
        match decode_stream_event(raw) {
            Ok(event) => self.handle_event(event, can_send, &mut actions),
            Err(err) => self.fail(&err.to_string(), true, &mut actions),
        }
        actions
    }

    pub fn on_event(
        &mut self,
        event: SpeechStreamEvent,
        can_send: bool,
    ) -> Vec<VoiceStreamAction> {
        let mut actions = Vec::new();
        if self.closed {
            return actions;
        }
        if !can_send {
            self.fail("Voice consent was revoked.", true, &mut actions);
            return actions;
        }
        self.handle_event(event, can_send, &mut actions);
        actions
    }

    fn handle_event(
        &mut self,
        event: SpeechStreamEvent,
        can_send: bool,
        actions: &mut Vec<VoiceStreamAction>,
    ) {
        match event {
            SpeechStreamEvent::Ready => {
                if self.is_ready {
                    self.fail("Unexpected speech stream response.", true, actions);
                    return;
                }
                self.is_ready = true;
                self.phase = if self.finishing {
                    VoiceStreamPhase::Finishing
                } else {
                    VoiceStreamPhase::Streaming
                };
                self.disarm_timeout(actions);
                self.pump(can_send, actions);
            }
            SpeechStreamEvent::Update { revision, text } => {
                if self.in_flight.is_empty() || revision < self.revision {
                    self.fail("Unexpected speech stream response.", true, actions);
                    return;
                }
                self.revision = revision;
                if let Some(bytes) = self.in_flight.pop_front() {
                    self.in_flight_bytes -= bytes;
                }
                self.disarm_timeout(actions);
                if let Some(text) = text {
                    actions.push(VoiceStreamAction::Text(text));
                }
                self.pump(can_send, actions);
                if !self.closed && !self.in_flight.is_empty() {
                    self.arm_timeout(actions);
                }
            }
            SpeechStreamEvent::Finished { text } => {
                if !self.finish_sent {
                    self.fail("Unexpected speech stream response.", true, actions);
                    return;
                }
                self.closed = true;
                self.phase = VoiceStreamPhase::Finished;
                self.final_result = Some(Ok(text.clone()));
                self.cleanup(actions);
                actions.push(VoiceStreamAction::Finished(text));
                actions.push(VoiceStreamAction::CloseSocket);
            }
            SpeechStreamEvent::Error { message } => {
                self.fail(&message, true, actions);
            }
        }
    }

    fn pump(&mut self, can_send: bool, actions: &mut Vec<VoiceStreamAction>) {
        if !self.is_ready || self.closed || self.finish_sent {
            return;
        }
        if !can_send {
            self.fail("Voice consent was revoked.", true, actions);
            return;
        }
        while self.in_flight.len() < STREAM_MAX_IN_FLIGHT_CHUNKS {
            let Some(chunk) = self.queue.pop_front() else {
                break;
            };
            self.in_flight.push_back(chunk.len());
            self.in_flight_bytes += chunk.len();
            self.queued_bytes -= chunk.len();
            actions.push(VoiceStreamAction::SendBinary(chunk));
            self.arm_timeout(actions);
        }
        if self.finishing && self.queue.is_empty() && self.in_flight.is_empty() {
            self.finish_sent = true;
            match encode_stream_command(&SpeechStreamCommand::Finish) {
                Ok(json) => {
                    actions.push(VoiceStreamAction::SendText(json));
                    self.arm_timeout(actions);
                }
                Err(err) => self.fail(&err.to_string(), true, actions),
            }
        }
    }

    fn arm_timeout(&mut self, actions: &mut Vec<VoiceStreamAction>) {
        self.timeout_armed = true;
        actions.push(VoiceStreamAction::ArmTimeout { timeout_ms: STREAM_RESPONSE_TIMEOUT_MS });
    }

    fn disarm_timeout(&mut self, actions: &mut Vec<VoiceStreamAction>) {
        if self.timeout_armed {
            self.timeout_armed = false;
            actions.push(VoiceStreamAction::DisarmTimeout);
        }
    }

    fn cleanup(&mut self, actions: &mut Vec<VoiceStreamAction>) {
        self.disarm_timeout(actions);
        self.queue.clear();
        self.queued_bytes = 0;
    }

    fn fail(&mut self, message: &str, notify: bool, actions: &mut Vec<VoiceStreamAction>) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.phase = VoiceStreamPhase::Failed;
        let msg = message.to_owned();
        self.final_result = Some(Err(msg.clone()));
        self.cleanup(actions);
        actions.push(VoiceStreamAction::CloseSocket);
        actions.push(VoiceStreamAction::Failed { message: msg, notify });
    }
}
