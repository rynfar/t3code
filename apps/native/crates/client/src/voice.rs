//! HTTP (`/api/voice/*`) and WebSocket (`/ws/voice`) client for environment and cloud speech.
//!
//! Mirrors `packages/client-runtime/src/voice-input/{environment,stream}.ts` and
//! `packages/contracts/src/environmentHttp.ts` (`EnvironmentVoiceHttpApi`).

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use t3_core::pairing::websocket_base;
use t3_core::voice_input::{
    CloudSpeechError, CloudSpeechKeys, CloudSpeechModels, CloudSpeechProvider,
    EnvironmentSpeechModels, EnvironmentSpeechPostProcessingRequest,
    EnvironmentSpeechPostProcessingResult, EnvironmentSpeechStatus,
    EnvironmentSpeechTranscriptionResult, SPEECH_STREAM_PATH, SpeechAcceleration,
    SpeechModelUnloadTimeout, SpeechStreamText, SpeechTranscriptionOptions, VoiceProtocolError,
    VoiceStreamAction, VoiceStreamMachine, encode_speech_pcm_request, validate_cloud_speech_api_key,
    validate_cloud_speech_model_id,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::auth::{AuthError, Credentials, issue_media_ticket};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const CLOUD_TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(130);
const LONG_OPERATION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, thiserror::Error)]
pub enum VoiceClientError {
    #[error("This environment session is no longer valid. Pair again.")]
    Unauthorized,
    #[error("Missing required environment scope: {0}")]
    InsufficientScope(String),
    #[error("{}", .0.message())]
    CloudSpeech(CloudSpeechError),
    #[error("Environment rejected the voice request ({reason}).")]
    InvalidRequest { reason: String, trace_id: Option<String> },
    #[error("Unexpected response from voice endpoint ({status}): {body}")]
    UnexpectedStatus { status: u16, body: String },
    #[error("Invalid voice endpoint URL: {0}")]
    InvalidUrl(String),
    #[error("Voice HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Failed to decode voice response JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Voice protocol error: {0}")]
    Protocol(#[from] VoiceProtocolError),
    #[error("Voice authentication error: {0}")]
    Auth(#[from] AuthError),
    #[error("{0}")]
    Stream(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CloudSpeechKeyInput {
    Clear,
    ApiKey(String),
    SecretReference { provider: String, reference: String },
}

impl CloudSpeechKeyInput {
    pub fn api_key(raw: &str) -> Result<Self, VoiceClientError> {
        let validated = validate_cloud_speech_api_key(raw)
            .map_err(|msg| VoiceClientError::InvalidUrl(msg.to_owned()))?;
        Ok(Self::ApiKey(validated))
    }

    pub fn secret_reference(provider: impl Into<String>, reference: impl Into<String>) -> Self {
        Self::SecretReference { provider: provider.into(), reference: reference.into() }
    }

    fn into_json(self) -> Value {
        match self {
            Self::Clear => Value::Null,
            Self::ApiKey(key) => Value::String(key),
            Self::SecretReference { provider, reference } => {
                json!({ "provider": provider, "reference": reference })
            }
        }
    }
}

/// Builds a voice HTTP endpoint URL preserving any base path on `http_base`.
pub fn voice_http_url(
    http_base: &str,
    segments: &[&str],
    query: &[(&str, &str)],
) -> Result<url::Url, VoiceClientError> {
    let mut url =
        url::Url::parse(http_base).map_err(|err| VoiceClientError::InvalidUrl(err.to_string()))?;
    url.path_segments_mut()
        .map_err(|()| VoiceClientError::InvalidUrl("base URL cannot be a base".to_owned()))?
        .pop_if_empty()
        .extend(segments.iter().copied());
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (k, v) in query {
            pairs.append_pair(k, v);
        }
    }
    Ok(url)
}

/// Builds the authenticated `/ws/voice?wsTicket=...` WebSocket URL.
pub fn voice_stream_url(http_base: &str, ticket: &str) -> Result<String, VoiceClientError> {
    let ws_base = websocket_base(http_base);
    let trimmed = ws_base.trim_end_matches("/ws");
    let mut url = url::Url::parse(&format!("{trimmed}{SPEECH_STREAM_PATH}"))
        .map_err(|err| VoiceClientError::InvalidUrl(err.to_string()))?;
    url.query_pairs_mut().append_pair("wsTicket", ticket);
    Ok(url.to_string())
}

#[derive(Debug, Clone)]
pub struct VoiceClient {
    credentials: Credentials,
    http: reqwest::Client,
}

impl VoiceClient {
    pub fn new(credentials: Credentials) -> Result<Self, VoiceClientError> {
        // Audio and credentials never follow a redirect to another host.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                attempt.error("redirects are not allowed for voice requests")
            }))
            .build()?;
        Ok(Self { credentials, http })
    }

    pub const fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// `GET /api/voice/status`
    pub async fn status(&self) -> Result<EnvironmentSpeechStatus, VoiceClientError> {
        self.get_json(&["api", "voice", "status"], &[], DEFAULT_TIMEOUT).await
    }

    /// `GET /api/voice/models` (`?benchmarks=1` when `include_benchmarks` is true)
    pub async fn models(
        &self,
        include_benchmarks: bool,
    ) -> Result<EnvironmentSpeechModels, VoiceClientError> {
        let query = if include_benchmarks { vec![("benchmarks", "1")] } else { Vec::new() };
        self.get_json(&["api", "voice", "models"], &query, DEFAULT_TIMEOUT).await
    }

    /// `POST /api/voice/models/select`
    pub async fn select_model(
        &self,
        model_id: &str,
    ) -> Result<EnvironmentSpeechModels, VoiceClientError> {
        self.post_json(
            &["api", "voice", "models", "select"],
            &json!({ "modelId": model_id }),
            DEFAULT_TIMEOUT,
        )
        .await
    }

    /// `POST /api/voice/models/download`
    pub async fn download_model(
        &self,
        model_id: &str,
    ) -> Result<EnvironmentSpeechModels, VoiceClientError> {
        self.post_json(
            &["api", "voice", "models", "download"],
            &json!({ "modelId": model_id }),
            LONG_OPERATION_TIMEOUT,
        )
        .await
    }

    /// `POST /api/voice/models/cancel`
    pub async fn cancel_model_download(
        &self,
        model_id: &str,
    ) -> Result<EnvironmentSpeechModels, VoiceClientError> {
        self.post_json(
            &["api", "voice", "models", "cancel"],
            &json!({ "modelId": model_id }),
            DEFAULT_TIMEOUT,
        )
        .await
    }

    /// `POST /api/voice/models/remove`
    pub async fn remove_model(
        &self,
        model_id: &str,
    ) -> Result<EnvironmentSpeechModels, VoiceClientError> {
        self.post_json(
            &["api", "voice", "models", "remove"],
            &json!({ "modelId": model_id }),
            DEFAULT_TIMEOUT,
        )
        .await
    }

    /// `POST /api/voice/models/prepare`
    pub async fn prepare_model(&self) -> Result<(), VoiceClientError> {
        let url = voice_http_url(&self.credentials.http_base, &["api", "voice", "models", "prepare"], &[])?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.credentials.access_token)
            .timeout(DEFAULT_TIMEOUT)
            .send()
            .await?;
        self.ensure_success(response).await?;
        Ok(())
    }

    /// `POST /api/voice/models/link`
    pub async fn link_models(
        &self,
        path: &str,
    ) -> Result<EnvironmentSpeechModels, VoiceClientError> {
        self.post_json(
            &["api", "voice", "models", "link"],
            &json!({ "path": path }),
            LONG_OPERATION_TIMEOUT,
        )
        .await
    }

    /// `POST /api/voice/acceleration`
    pub async fn update_acceleration(
        &self,
        acceleration: &SpeechAcceleration,
    ) -> Result<EnvironmentSpeechStatus, VoiceClientError> {
        self.post_json(
            &["api", "voice", "acceleration"],
            &json!({ "acceleration": acceleration }),
            DEFAULT_TIMEOUT,
        )
        .await
    }

    /// `POST /api/voice/model-unload-timeout`
    pub async fn update_model_unload_timeout(
        &self,
        timeout: SpeechModelUnloadTimeout,
    ) -> Result<EnvironmentSpeechStatus, VoiceClientError> {
        self.post_json(
            &["api", "voice", "model-unload-timeout"],
            &json!({ "timeout": timeout }),
            DEFAULT_TIMEOUT,
        )
        .await
    }

    /// `GET /api/voice/cloud/keys`
    pub async fn cloud_keys(&self) -> Result<CloudSpeechKeys, VoiceClientError> {
        self.get_json(&["api", "voice", "cloud", "keys"], &[], DEFAULT_TIMEOUT).await
    }

    /// `POST /api/voice/cloud/:provider/key`
    pub async fn set_cloud_key(
        &self,
        provider: CloudSpeechProvider,
        key: CloudSpeechKeyInput,
    ) -> Result<(), VoiceClientError> {
        let url = voice_http_url(
            &self.credentials.http_base,
            &["api", "voice", "cloud", provider.as_str(), "key"],
            &[],
        )?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.credentials.access_token)
            .timeout(DEFAULT_TIMEOUT)
            .json(&json!({ "key": key.into_json() }))
            .send()
            .await?;
        self.ensure_success(response).await?;
        Ok(())
    }

    /// `GET /api/voice/cloud/openrouter/models`
    pub async fn cloud_models(&self) -> Result<CloudSpeechModels, VoiceClientError> {
        self.get_json(&["api", "voice", "cloud", "openrouter", "models"], &[], DEFAULT_TIMEOUT)
            .await
    }

    /// `POST /api/voice/transcribe`
    pub async fn transcribe(
        &self,
        pcm_bytes: &[u8],
        options: &SpeechTranscriptionOptions,
    ) -> Result<EnvironmentSpeechTranscriptionResult, VoiceClientError> {
        let url = voice_http_url(&self.credentials.http_base, &["api", "voice", "transcribe"], &[])?;
        let body = encode_speech_pcm_request(pcm_bytes, options)?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.credentials.access_token)
            .timeout(LONG_OPERATION_TIMEOUT)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .send()
            .await?;
        self.decode_json(response).await
    }

    /// `POST /api/voice/cloud/:provider/transcribe`
    pub async fn transcribe_cloud(
        &self,
        provider: CloudSpeechProvider,
        pcm_bytes: &[u8],
        options: &SpeechTranscriptionOptions,
        model: Option<&str>,
    ) -> Result<EnvironmentSpeechTranscriptionResult, VoiceClientError> {
        let mut query = Vec::new();
        if let Some(model_id) = model {
            if !validate_cloud_speech_model_id(model_id) {
                return Err(VoiceClientError::InvalidRequest {
                    reason: "invalid_command".to_owned(),
                    trace_id: None,
                });
            }
            query.push(("model", model_id));
        }
        let url = voice_http_url(
            &self.credentials.http_base,
            &["api", "voice", "cloud", provider.as_str(), "transcribe"],
            &query,
        )?;
        let body = encode_speech_pcm_request(pcm_bytes, options)?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.credentials.access_token)
            .timeout(CLOUD_TRANSCRIBE_TIMEOUT)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .send()
            .await?;
        self.decode_json(response).await
    }

    /// `POST /api/voice/post-process`
    pub async fn post_process(
        &self,
        request: &EnvironmentSpeechPostProcessingRequest,
    ) -> Result<EnvironmentSpeechPostProcessingResult, VoiceClientError> {
        self.post_json(&["api", "voice", "post-process"], request, LONG_OPERATION_TIMEOUT).await
    }

    /// Mints a `media` ticket and opens a `/ws/voice` live transcription stream.
    pub async fn open_stream(
        &self,
        options: SpeechTranscriptionOptions,
        can_send: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<VoiceStreamSession, VoiceClientError> {
        let ticket = issue_media_ticket(&self.credentials).await?;
        let url = voice_stream_url(&self.credentials.http_base, &ticket)?;
        open_voice_stream_at(&url, options, can_send).await
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<T, VoiceClientError> {
        let url = voice_http_url(&self.credentials.http_base, segments, query)?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.credentials.access_token)
            .timeout(timeout)
            .send()
            .await?;
        self.decode_json(response).await
    }

    async fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        segments: &[&str],
        body: &B,
        timeout: Duration,
    ) -> Result<T, VoiceClientError> {
        let url = voice_http_url(&self.credentials.http_base, segments, &[])?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.credentials.access_token)
            .timeout(timeout)
            .json(body)
            .send()
            .await?;
        self.decode_json(response).await
    }

    async fn ensure_success(&self, response: reqwest::Response) -> Result<reqwest::Response, VoiceClientError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let code = status.as_u16();
        let bytes = response.bytes().await.unwrap_or_default();
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Err(classify_http_error(code, &bytes, &value))
    }

    async fn decode_json<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, VoiceClientError> {
        let response = self.ensure_success(response).await?;
        let bytes = response.bytes().await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

fn classify_http_error(status: u16, bytes: &[u8], value: &Value) -> VoiceClientError {
    if status == 401 {
        return VoiceClientError::Unauthorized;
    }
    if status == 403 {
        let scope = value
            .get("requiredPermission")
            .or_else(|| value.get("requiredScope"))
            .and_then(Value::as_str)
            .unwrap_or("forbidden")
            .to_owned();
        return VoiceClientError::InsufficientScope(scope);
    }
    if let Some(cloud_error) = CloudSpeechError::from_tagged_value(value) {
        return VoiceClientError::CloudSpeech(cloud_error);
    }
    if value.get("_tag").and_then(Value::as_str) == Some("EnvironmentRequestInvalidError")
        && let Some(reason) = value.get("reason").and_then(Value::as_str)
    {
        let trace_id = value.get("traceId").and_then(Value::as_str).map(str::to_owned);
        return VoiceClientError::InvalidRequest { reason: reason.to_owned(), trace_id };
    }
    VoiceClientError::UnexpectedStatus {
        status,
        body: String::from_utf8_lossy(bytes).into_owned(),
    }
}

enum StreamCommand {
    FeedBytes(Vec<u8>),
    Finish,
    Cancel,
}

pub struct VoiceStreamSession {
    commands: mpsc::UnboundedSender<StreamCommand>,
    updates: Option<mpsc::UnboundedReceiver<SpeechStreamText>>,
    completion: oneshot::Receiver<Result<String, VoiceClientError>>,
}

impl VoiceStreamSession {
    pub fn take_updates(&mut self) -> Option<mpsc::UnboundedReceiver<SpeechStreamText>> {
        self.updates.take()
    }

    pub fn feed_bytes(&self, pcm_le_bytes: Vec<u8>) -> Result<(), VoiceClientError> {
        self.commands
            .send(StreamCommand::FeedBytes(pcm_le_bytes))
            .map_err(|_| VoiceClientError::Stream("Voice stream is closed.".to_owned()))
    }

    pub fn feed_f32(&self, samples: &[f32]) -> Result<(), VoiceClientError> {
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for &sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        self.feed_bytes(bytes)
    }

    pub async fn finish(self) -> Result<String, VoiceClientError> {
        let _ = self.commands.send(StreamCommand::Finish);
        self.completion
            .await
            .unwrap_or_else(|_| Err(VoiceClientError::Stream("Voice stream stopped unexpectedly.".to_owned())))
    }

    pub fn cancel(self) {
        let _ = self.commands.send(StreamCommand::Cancel);
    }
}

/// Connects directly to a `/ws/voice` URL using `VoiceStreamMachine`.
pub async fn open_voice_stream_at(
    ws_url: &str,
    options: SpeechTranscriptionOptions,
    can_send: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<VoiceStreamSession, VoiceClientError> {
    let (socket, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|err| VoiceClientError::Stream(format!("Could not connect to live transcription: {err}")))?;

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let (updates_tx, updates_rx) = mpsc::unbounded_channel();
    let (done_tx, done_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut sink, mut stream) = socket.split();
        let mut machine = VoiceStreamMachine::new(options);
        let mut deadline: Option<Instant> =
            Some(Instant::now() + Duration::from_millis(t3_core::voice_input::STREAM_RESPONSE_TIMEOUT_MS));
        let mut done_tx = Some(done_tx);

        let initial_actions = machine.on_open(can_send());
        if !apply_stream_actions(initial_actions, &mut sink, &updates_tx, &mut done_tx, &mut deadline).await {
            return;
        }

        loop {
            let sleep_fut = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let actions = match cmd {
                        Some(StreamCommand::FeedBytes(bytes)) => machine.feed_bytes(&bytes, can_send()),
                        Some(StreamCommand::Finish) => machine.finish(can_send()),
                        Some(StreamCommand::Cancel) | None => machine.cancel(),
                    };
                    if !apply_stream_actions(actions, &mut sink, &updates_tx, &mut done_tx, &mut deadline).await {
                        break;
                    }
                }
                msg = stream.next() => {
                    let actions = match msg {
                        Some(Ok(Message::Text(text))) => machine.on_text_message(&text, can_send()),
                        Some(Ok(Message::Close(_))) | None => machine.on_socket_closed(),
                        Some(Ok(_)) => Vec::new(),
                        Some(Err(_)) => machine.on_socket_error(),
                    };
                    if !apply_stream_actions(actions, &mut sink, &updates_tx, &mut done_tx, &mut deadline).await {
                        break;
                    }
                }
                () = sleep_fut => {
                    let actions = machine.on_timeout();
                    let _ = apply_stream_actions(actions, &mut sink, &updates_tx, &mut done_tx, &mut deadline).await;
                    break;
                }
            }
        }
    });

    Ok(VoiceStreamSession {
        commands: cmd_tx,
        updates: Some(updates_rx),
        completion: done_rx,
    })
}

async fn apply_stream_actions<S>(
    actions: Vec<VoiceStreamAction>,
    sink: &mut S,
    updates_tx: &mpsc::UnboundedSender<SpeechStreamText>,
    done_tx: &mut Option<oneshot::Sender<Result<String, VoiceClientError>>>,
    deadline: &mut Option<Instant>,
) -> bool
where
    S: futures::Sink<Message> + Unpin,
{
    for action in actions {
        match action {
            VoiceStreamAction::SendText(text) => {
                if sink.send(Message::Text(text)).await.is_err() {
                    if let Some(tx) = done_tx.take() {
                        let _ = tx.send(Err(VoiceClientError::Stream(
                            "The voice connection closed. Please record again.".to_owned(),
                        )));
                    }
                    return false;
                }
            }
            VoiceStreamAction::SendBinary(bytes) => {
                if sink.send(Message::Binary(bytes)).await.is_err() {
                    if let Some(tx) = done_tx.take() {
                        let _ = tx.send(Err(VoiceClientError::Stream(
                            "The voice connection closed. Please record again.".to_owned(),
                        )));
                    }
                    return false;
                }
            }
            VoiceStreamAction::ArmTimeout { timeout_ms } => {
                *deadline = Some(Instant::now() + Duration::from_millis(timeout_ms));
            }
            VoiceStreamAction::DisarmTimeout => {
                *deadline = None;
            }
            VoiceStreamAction::Text(text) => {
                let _ = updates_tx.send(text);
            }
            VoiceStreamAction::Finished(text) => {
                let _ = sink.close().await;
                if let Some(tx) = done_tx.take() {
                    let _ = tx.send(Ok(text));
                }
                return false;
            }
            VoiceStreamAction::Failed { message, .. } => {
                let _ = sink.close().await;
                if let Some(tx) = done_tx.take() {
                    let _ = tx.send(Err(VoiceClientError::Stream(message)));
                }
                return false;
            }
            VoiceStreamAction::CloseSocket => {
                let _ = sink.close().await;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use t3_core::voice_input::{
        CloudSpeechErrorReason, EnvironmentSpeechState, SpeechBenchmarkTier,
        SpeechPostProcessingDraft, SpeechPostProcessingOptions,
        SpeechPostProcessingPromptSettings, SpeechPostProcessingSelection, SpeechStreamCommand,
        SpeechStreamEvent, decode_speech_pcm_request, decode_stream_command, encode_stream_event,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
    use tokio_tungstenite::tungstenite::protocol::Role;

    use super::*;

    struct RecordedHttpRequest {
        method: String,
        path: String,
        auth: Option<String>,
        body: Vec<u8>,
    }

    async fn read_http_request(stream: &mut TcpStream) -> Option<(RecordedHttpRequest, Option<String>)> {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.ok()?;
            buf.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&buf).into_owned();
        let mut first = head.lines().next()?.split_whitespace();
        let method = first.next()?.to_owned();
        let path = first.next()?.to_owned();
        let header = |name: &str| -> Option<String> {
            head.lines().find_map(|line| {
                let (k, v) = line.split_once(':')?;
                k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
            })
        };
        let auth = header("authorization");
        let ws_key = header("sec-websocket-key");
        let len = header("content-length").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
        let mut body = vec![0u8; len];
        if len > 0 {
            stream.read_exact(&mut body).await.ok()?;
        }
        Some((RecordedHttpRequest { method, path, auth, body }, ws_key))
    }

    async fn send_http(mut stream: TcpStream, status: u16, body: &str) {
        let resp = format!(
            "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes()).await;
        let _ = stream.shutdown().await;
    }

    #[tokio::test]
    async fn http_endpoints_and_error_mapping() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recorded = Arc::new(std::sync::Mutex::new(Vec::<(String, String, Vec<u8>)>::new()));
        let recorded_srv = recorded.clone();

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let recorded_srv = recorded_srv.clone();
                tokio::spawn(async move {
                    let Some((req, _)) = read_http_request(&mut stream).await else { return };
                    assert_eq!(req.auth.as_deref(), Some("Bearer tok-123"));
                    recorded_srv
                        .lock()
                        .unwrap()
                        .push((req.method.clone(), req.path.clone(), req.body.clone()));

                    match (req.method.as_str(), req.path.as_str()) {
                        ("GET", "/api/voice/status") => {
                            let body = json!({
                                "supported": true,
                                "state": "ready",
                                "modelId": "whisper-small",
                                "model": "Whisper Small",
                                "size": 500,
                                "supportsStreaming": true,
                                "supportsTranslation": true,
                                "acceleration": "auto",
                                "benchmarkTier": "cpu",
                                "modelUnloadTimeout": "min_5",
                                "gpuDevices": []
                            });
                            send_http(stream, 200, &body.to_string()).await;
                        }
                        ("GET", "/api/voice/models?benchmarks=1")
                        | ("POST", "/api/voice/models/select")
                        | ("POST", "/api/voice/models/download")
                        | ("POST", "/api/voice/models/cancel")
                        | ("POST", "/api/voice/models/remove")
                        | ("POST", "/api/voice/models/link") => {
                            send_http(stream, 200, r#"{"models":[]}"#).await;
                        }
                        ("POST", "/api/voice/models/prepare") => {
                            send_http(stream, 204, "").await;
                        }
                        ("POST", "/api/voice/acceleration")
                        | ("POST", "/api/voice/model-unload-timeout") => {
                            let body = json!({ "supported": false, "reason": "updating" });
                            send_http(stream, 200, &body.to_string()).await;
                        }
                        ("GET", "/api/voice/cloud/keys") => {
                            send_http(stream, 200, r#"{"openai":true,"xai":false}"#).await;
                        }
                        ("POST", "/api/voice/cloud/openai/key") => {
                            send_http(stream, 204, "").await;
                        }
                        ("GET", "/api/voice/cloud/openrouter/models") => {
                            send_http(
                                stream,
                                200,
                                r#"{"models":[{"id":"acme/transcribe-1","name":"Acme"}]}"#,
                            )
                            .await;
                        }
                        ("POST", "/api/voice/transcribe") => {
                            let (opts, pcm) = decode_speech_pcm_request(&req.body).unwrap();
                            assert_eq!(opts.speech_language, "auto");
                            assert_eq!(pcm, &[1, 2, 3, 4]);
                            send_http(stream, 200, r#"{"text":"local ok"}"#).await;
                        }
                        ("POST", "/api/voice/cloud/openrouter/transcribe?model=acme%2Ftranscribe-1") => {
                            let err = CloudSpeechError {
                                provider: CloudSpeechProvider::Openrouter,
                                reason: CloudSpeechErrorReason::RejectedKey,
                            };
                            send_http(stream, 422, &err.to_tagged_json().to_string()).await;
                        }
                        ("POST", "/api/voice/post-process") => {
                            send_http(stream, 200, r#"{"text":"cleaned ok"}"#).await;
                        }
                        ("GET", "/api/voice/models") => {
                            let resp = "HTTP/1.1 302 Found\r\nlocation: https://evil.example/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
                            let _ = stream.write_all(resp.as_bytes()).await;
                        }
                        _ => send_http(stream, 404, "{}").await,
                    }
                });
            }
        });

        let client = VoiceClient::new(Credentials {
            http_base: format!("http://{addr}"),
            access_token: "tok-123".into(),
        })
        .unwrap();

        let status = client.status().await.unwrap();
        assert!(matches!(
            status,
            EnvironmentSpeechStatus::Supported {
                state: EnvironmentSpeechState::Ready,
                benchmark_tier: SpeechBenchmarkTier::Cpu,
                ..
            }
        ));

        assert!(client.models(true).await.unwrap().models.is_empty());
        assert!(client.select_model("m1").await.unwrap().models.is_empty());
        assert!(client.download_model("m1").await.unwrap().models.is_empty());
        assert!(client.cancel_model_download("m1").await.unwrap().models.is_empty());
        assert!(client.remove_model("m1").await.unwrap().models.is_empty());
        client.prepare_model().await.unwrap();
        assert!(client.link_models("/tmp/models").await.unwrap().models.is_empty());
        assert!(!client.update_acceleration(&SpeechAcceleration::Cpu).await.unwrap().is_supported());
        assert!(
            !client
                .update_model_unload_timeout(SpeechModelUnloadTimeout::Min10)
                .await
                .unwrap()
                .is_supported()
        );

        let keys = client.cloud_keys().await.unwrap();
        assert!(keys.is_configured(CloudSpeechProvider::Openai));
        assert!(!keys.is_configured(CloudSpeechProvider::Xai));
        client
            .set_cloud_key(
                CloudSpeechProvider::Openai,
                CloudSpeechKeyInput::api_key("sk-test-123").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(client.cloud_models().await.unwrap().models[0].id, "acme/transcribe-1");

        let opts = SpeechTranscriptionOptions::default();
        let tx = client.transcribe(&[1, 2, 3, 4], &opts).await.unwrap();
        assert_eq!(tx.text, "local ok");

        let cloud_err = client
            .transcribe_cloud(
                CloudSpeechProvider::Openrouter,
                &[1, 2, 3, 4],
                &opts,
                Some("acme/transcribe-1"),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            cloud_err,
            VoiceClientError::CloudSpeech(CloudSpeechError {
                provider: CloudSpeechProvider::Openrouter,
                reason: CloudSpeechErrorReason::RejectedKey,
            })
        ));

        let pp = client
            .post_process(&EnvironmentSpeechPostProcessingRequest {
                options: SpeechPostProcessingOptions {
                    speech_custom_words: Vec::new(),
                    speech_post_processing_prompt: SpeechPostProcessingPromptSettings::default(),
                    speech_correction_word: None,
                },
                transcript: "raw".into(),
                draft: SpeechPostProcessingDraft {
                    text: String::new(),
                    selection: SpeechPostProcessingSelection::default(),
                },
            })
            .await
            .unwrap();
        assert_eq!(pp.text, "cleaned ok");

        // Redirects must never be followed.
        assert!(client.models(false).await.is_err());
    }

    #[tokio::test]
    async fn ws_voice_stream_mints_ticket_streams_audio_and_receives_updates() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let Some((req, ws_key)) = read_http_request(&mut stream).await else { return };
                    if req.method == "POST" && req.path == "/api/auth/websocket-ticket/media" {
                        send_http(stream, 200, r#"{"ticket":"media-tkt"}"#).await;
                        return;
                    }
                    if req.path == "/ws/voice?wsTicket=media-tkt"
                        && let Some(key) = ws_key
                    {
                        let accept = derive_accept_key(key.as_bytes());
                        let upgrade = format!(
                            "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {accept}\r\n\r\n"
                        );
                        stream.write_all(upgrade.as_bytes()).await.unwrap();
                        let mut ws = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
                        let start = ws.next().await.unwrap().unwrap().into_text().unwrap();
                        assert!(matches!(
                            decode_stream_command(&start).unwrap(),
                            SpeechStreamCommand::Start { .. }
                        ));
                        ws.send(Message::Text(encode_stream_event(&SpeechStreamEvent::Ready).unwrap()))
                            .await
                            .unwrap();

                        let mut rev = 0;
                        let mut samples = Vec::new();
                        while let Some(Ok(msg)) = ws.next().await {
                            match msg {
                                Message::Binary(bytes) => {
                                    assert!(bytes.len().is_multiple_of(4));
                                    for chunk in bytes.chunks_exact(4) {
                                        samples.push(f32::from_le_bytes([
                                            chunk[0], chunk[1], chunk[2], chunk[3],
                                        ]));
                                    }
                                    rev += 1;
                                    let evt = SpeechStreamEvent::Update {
                                        revision: rev,
                                        text: Some(SpeechStreamText {
                                            committed: "hello ".into(),
                                            tentative: format!("rev{rev}"),
                                        }),
                                    };
                                    ws.send(Message::Text(encode_stream_event(&evt).unwrap()))
                                        .await
                                        .unwrap();
                                }
                                Message::Text(raw) => {
                                    assert_eq!(
                                        decode_stream_command(&raw).unwrap(),
                                        SpeechStreamCommand::Finish
                                    );
                                    assert_eq!(samples, vec![0.25, 0.5]);
                                    ws.send(Message::Text(
                                        encode_stream_event(&SpeechStreamEvent::Finished {
                                            text: "hello world".into(),
                                        })
                                        .unwrap(),
                                    ))
                                    .await
                                    .unwrap();
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                });
            }
        });

        let client = VoiceClient::new(Credentials {
            http_base: format!("http://{addr}"),
            access_token: "tok-123".into(),
        })
        .unwrap();
        let allowed = Arc::new(AtomicBool::new(true));
        let flag = allowed.clone();
        let mut session = client
            .open_stream(
                SpeechTranscriptionOptions::default(),
                Arc::new(move || flag.load(Ordering::Relaxed)),
            )
            .await
            .unwrap();
        let mut updates = session.take_updates().unwrap();

        session.feed_f32(&[0.25]).unwrap();
        let first = updates.recv().await.unwrap();
        assert_eq!(first, SpeechStreamText { committed: "hello ".into(), tentative: "rev1".into() });
        session.feed_f32(&[0.5]).unwrap();
        let second = updates.recv().await.unwrap();
        assert_eq!(
            second,
            SpeechStreamText { committed: "hello ".into(), tentative: "rev2".into() }
        );
        let final_text = session.finish().await.unwrap();
        assert_eq!(final_text, "hello world");
        assert!(updates.recv().await.is_none());
    }
}
