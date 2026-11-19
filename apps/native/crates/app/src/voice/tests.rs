use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::{
    AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, VisualTestContext,
    Window, div,
};
use serde_json::json;
use t3_client::environments::{EnvironmentView, FleetView};
use t3_client::store::{SavedEnvironment, SavedEnvironments};
use t3_client::supervisor::ConnectionStatus;
use t3_core::server_config::ServerConfigState;
use t3_core::shell::ShellState;
use t3_core::sidebar::fleet::EnvironmentSummary;
use t3_core::sidebar::grouping::EnvironmentOwner;
use t3_core::voice_input::{
    AudioGrant, CloudSpeechProvider, SavedAudioChoice, SpeechStreamCommand, SpeechStreamEvent,
    SpeechStreamText, VoiceDraftSnapshot, VoiceErrorAction, VoiceInputPhase, VoiceSelection,
    VoiceTranscriptionDestination, decode_stream_command, encode_stream_event, voice_error_label,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::consent::install_memory_voice_consent;
use super::controller::{VoiceControllerEvent, VoiceSpeechConfig, VoiceSpeechController};
use super::platform::{MicPermissionStatus, MockAudioPlatform, install_audio_platform};
use crate::app_state::AppState;

async fn spawn_mock_voice_server(
    runtime: &tokio::runtime::Runtime,
    fail_transcribe: Arc<AtomicBool>,
    transcript: &'static str,
) -> String {
    let listener = runtime
        .spawn(async move { TcpListener::bind("127.0.0.1:0").await.unwrap() })
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();

    runtime.spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let fail = fail_transcribe.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut total = 0usize;
                loop {
                    let Ok(n) = stream.read(&mut buf[total..]).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    total += n;
                    if let Some(hdr_end) = buf[..total].windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&buf[..hdr_end]);
                        let content_len = headers
                            .lines()
                            .find_map(|line| {
                                let (k, v) = line.split_once(':')?;
                                k.trim()
                                    .eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        let body_start = hdr_end + 4;
                        while total < body_start + content_len {
                            let Ok(m) = stream.read(&mut buf[total..]).await else {
                                return;
                            };
                            if m == 0 {
                                break;
                            }
                            total += m;
                            if total >= buf.len() {
                                buf.resize(buf.len() * 2, 0);
                            }
                        }
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&buf[..total]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");

                let (status, body) = if path.starts_with("/api/voice/status") {
                    (
                        "200 OK",
                        json!({
                            "supported": true,
                            "state": "ready",
                            "modelId": "whisper-base",
                            "model": "Whisper Base",
                            "size": 142000000u64,
                            "supportsStreaming": false,
                            "supportsTranslation": false,
                            "supportsLanguageDetection": true,
                            "languages": ["en"],
                            "acceleration": "cpu",
                            "benchmarkTier": "cpu",
                            "modelUnloadTimeout": "min_5",
                            "gpuDevices": []
                        })
                        .to_string(),
                    )
                } else if path.starts_with("/api/voice/transcribe") {
                    if fail.load(Ordering::SeqCst) {
                        ("500 Internal Server Error", r#"{"error":"boom"}"#.to_owned())
                    } else {
                        ("200 OK", json!({ "text": transcript }).to_string())
                    }
                } else if path.starts_with("/api/voice/cloud/keys") {
                    (
                        "200 OK",
                        r#"{"openai":false,"xai":false,"openrouter":false}"#.to_owned(),
                    )
                } else {
                    ("404 Not Found", "{}".to_owned())
                };

                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    format!("http://127.0.0.1:{}", addr.port())
}

struct VoiceHarness {
    voice: Entity<VoiceSpeechController>,
    committed: Rc<RefCell<Vec<(String, VoiceSelection)>>>,
    submitted: Rc<RefCell<Vec<String>>>,
}

impl Render for VoiceHarness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn setup_harness<'a>(
    http_base: &str,
    platform: Arc<MockAudioPlatform>,
    owner_key: &str,
    initial_draft: VoiceDraftSnapshot,
    runtime: tokio::runtime::Handle,
    cx: &'a mut TestAppContext,
) -> (
    Entity<AppState>,
    Entity<VoiceHarness>,
    &'a mut VisualTestContext,
    Arc<Mutex<VoiceDraftSnapshot>>,
) {
    cx.executor().allow_parking();
    let mut config = ServerConfigState::default();
    config.apply(
        serde_json::from_value(json!({
            "type": "snapshot",
            "config": {
                "environment": {
                    "environmentId": "env-1",
                    "label": "Local",
                    "capabilities": { "voiceTranscription": true, "cloudVoiceTranscription": true }
                },
                "settings": {}
            }
        }))
        .unwrap(),
    );

    let owner = EnvironmentOwner {
        saved_id: "env-1".into(),
        canonical_id: Some("env-1".into()),
        generation: 1,
        connection_generation: 1,
        shell_generation: 1,
    };
    let summary = Arc::new(EnvironmentSummary::from_shell(
        owner,
        &ShellState::default(),
        &config,
    ));
    let fleet = Arc::new(FleetView {
        revision: 1,
        environments: vec![Arc::new(EnvironmentView {
            summary,
            status: ConnectionStatus::Connected,
            config,
            grants: None,
            settings_revision: 0,
            keybindings_revision: 0,
        })],
    });

    let state = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        crate::settings::store::init(cx);
        install_audio_platform(platform, cx);
        install_memory_voice_consent(
            SavedAudioChoice::Granted(AudioGrant {
                id: "grant-1".into(),
                destination: VoiceTranscriptionDestination::environment("env-1"),
            }),
            false,
            cx,
        );
        cx.new(|cx| {
            let mut state = AppState::new(runtime, cx);
            #[cfg(feature = "snapshot")]
            state.stop_pump();
            state.environments = SavedEnvironments {
                active: Some("env-1".into()),
                environments: vec![SavedEnvironment {
                    id: "env-1".into(),
                    environment_id: Some("env-1".into()),
                    label: "Local".into(),
                    http_base: http_base.to_owned(),
                    access_token: Some("tok".into()),
                    server_version: None,
                    disabled: false,
                    local: true,
                    ssh: None,
                }],
            };
            state.apply_fleet_view(fleet, cx);
            state
        })
    });

    let draft = Arc::new(Mutex::new(initial_draft));
    let draft_for_harness = draft.clone();
    let owner_key = owner_key.to_owned();
    let (harness, vcx) = cx.add_window_view(|window, cx| {
        let draft_read = draft_for_harness.clone();
        let voice = cx.new(|cx| {
            VoiceSpeechController::new(
                VoiceSpeechConfig {
                    app: state.clone(),
                    owner_key,
                    offer_setup: true,
                    draft_reader: Rc::new(move |_| Some(draft_read.lock().unwrap().clone())),
                    hints_reader: Rc::new(|_| Vec::new()),
                    project_id_reader: Rc::new(|_| None),
                    environment_id_reader: Rc::new(|_| Some("env-1".to_owned())),
                    on_open_voice_settings: Rc::new(|_, _| {}),
                },
                cx,
            )
        });
        let committed = Rc::new(RefCell::new(Vec::<(String, VoiceSelection)>::new()));
        let submitted = Rc::new(RefCell::new(Vec::<String>::new()));
        let committed_c = committed.clone();
        let submitted_c = submitted.clone();
        let draft_write = draft_for_harness.clone();
        cx.subscribe_in(&voice, window, move |_, _, event: &VoiceControllerEvent, _, _| match event {
            VoiceControllerEvent::CommitDraft { text, selection } => {
                committed_c.borrow_mut().push((text.clone(), *selection));
                let mut d = draft_write.lock().unwrap();
                d.text = text.clone();
                d.selection = *selection;
            }
            VoiceControllerEvent::SubmitAfterDictation { text } => {
                submitted_c.borrow_mut().push(text.clone());
            }
            VoiceControllerEvent::StateChanged => {}
        })
        .detach();
        VoiceHarness { voice, committed, submitted }
    });
    vcx.run_until_parked();
    (state, harness, vcx, draft)
}

#[gpui::test]
fn capture_preserves_edits_and_submits_after_dictation(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let base_url = runtime.block_on(spawn_mock_voice_server(&runtime, fail, "dictated words"));
    let platform = Arc::new(MockAudioPlatform::new());
    let (_, harness, vcx, draft) = setup_harness(
        &base_url,
        platform.clone(),
        "thread-1",
        VoiceDraftSnapshot {
            owner_key: "thread-1".into(),
            text: "hello ".into(),
            selection: VoiceSelection { start: 6, end: 6 },
        },
        runtime.handle().clone(),
        cx,
    );

    let voice = vcx.read(|cx| harness.read(cx).voice.clone());
    vcx.update(|window, cx| {
        voice.update(cx, |voice, cx| voice.start(window, cx));
    });
    for _ in 0..40 {
        vcx.run_until_parked();
        if vcx.read(|cx| voice.read(cx).state().phase) == VoiceInputPhase::Recording {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(vcx.read(|cx| voice.read(cx).state().phase), VoiceInputPhase::Recording);

    // Simulate user editing prefix while recording + hardware PCM/level frames arriving.
    {
        let mut d = draft.lock().unwrap();
        d.text = "prefix hello ".into();
        d.selection = VoiceSelection { start: 13, end: 13 };
    }
    platform.emit_level(0.7);
    platform.emit_pcm(vec![0.1; 480]);
    vcx.run_until_parked();

    vcx.update(|_, cx| {
        voice.update(cx, |voice, cx| {
            voice.submit_after_dictation(cx);
        });
    });

    for _ in 0..80 {
        vcx.run_until_parked();
        if !vcx.read(|cx| harness.read(cx).submitted.borrow().is_empty()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    vcx.read(|cx| {
        let h = harness.read(cx);
        let committed = h.committed.borrow();
        let submitted = h.submitted.borrow();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].0, "prefix hello dictated words");
        assert_eq!(submitted.as_slice(), &["prefix hello dictated words"]);
        assert_eq!(voice.read(cx).state().phase, VoiceInputPhase::Idle);
    });
    assert!(platform.is_stopped());
}

#[gpui::test]
fn permission_error_cancel_and_retry_flow(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let fail = Arc::new(AtomicBool::new(true));
    let base_url = runtime.block_on(spawn_mock_voice_server(&runtime, fail.clone(), "retried ok"));
    let platform = Arc::new(MockAudioPlatform::new());
    platform.set_permission(MicPermissionStatus::Denied);

    let (_, harness, vcx, _) = setup_harness(
        &base_url,
        platform.clone(),
        "thread-2",
        VoiceDraftSnapshot {
            owner_key: "thread-2".into(),
            text: String::new(),
            selection: VoiceSelection { start: 0, end: 0 },
        },
        runtime.handle().clone(),
        cx,
    );
    let voice = vcx.read(|cx| harness.read(cx).voice.clone());

    // Denied permission produces Settings error state ("Mic blocked").
    vcx.update(|window, cx| {
        voice.update(cx, |voice, cx| voice.start(window, cx));
    });
    for _ in 0..20 {
        vcx.run_until_parked();
        if vcx.read(|cx| voice.read(cx).state().phase) == VoiceInputPhase::Error {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    vcx.read(|cx| {
        let state = voice.read(cx).state();
        assert_eq!(state.phase, VoiceInputPhase::Error);
        assert_eq!(state.error_action, Some(VoiceErrorAction::Settings));
        assert_eq!(voice_error_label(state.error.as_deref()), "Mic blocked");
    });

    // Grant permission, record, fail transcription (500), then retry without re-recording!
    platform.set_permission(MicPermissionStatus::Granted);
    vcx.update(|window, cx| {
        voice.update(cx, |voice, cx| voice.start(window, cx));
    });
    for _ in 0..40 {
        vcx.run_until_parked();
        if vcx.read(|cx| voice.read(cx).state().phase) == VoiceInputPhase::Recording {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(vcx.read(|cx| voice.read(cx).state().phase), VoiceInputPhase::Recording);
    platform.emit_pcm(vec![0.2; 480]);
    vcx.run_until_parked();
    vcx.update(|_, cx| voice.update(cx, |voice, cx| voice.stop(cx)));

    for _ in 0..40 {
        vcx.run_until_parked();
        if vcx.read(|cx| voice.read(cx).state().phase) == VoiceInputPhase::Error {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    vcx.read(|cx| {
        let state = voice.read(cx).state();
        assert_eq!(state.phase, VoiceInputPhase::Error);
        assert_eq!(state.error_action, Some(VoiceErrorAction::Retry));
    });

    // Clear server error and click Retry: commits transcript cleanly without calling start_capture again.
    fail.store(false, Ordering::SeqCst);
    vcx.update(|window, cx| {
        voice.update(cx, |voice, cx| voice.start(window, cx));
    });
    for _ in 0..60 {
        vcx.run_until_parked();
        if !vcx.read(|cx| harness.read(cx).committed.borrow().is_empty()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    vcx.read(|cx| {
        let h = harness.read(cx);
        let committed = h.committed.borrow();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].0, "retried ok");
        assert_eq!(voice.read(cx).state().phase, VoiceInputPhase::Idle);
    });
}

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut msg = data.to_vec();
    let bit_len = (msg.len() as u64) * 8;
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = *chunk.get(1).unwrap_or(&0) as usize;
        let b2 = *chunk.get(2).unwrap_or(&0) as usize;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) & 63] as char);
        out.push(TABLE[(n >> 12) & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n & 63] as char } else { '=' });
    }
    out
}

async fn read_ws_frame(stream: &mut tokio::net::TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr).await.ok()?;
    let opcode = hdr[0] & 0x0f;
    let masked = (hdr[1] & 0x80) != 0;
    let mut len = (hdr[1] & 0x7f) as usize;
    if len == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext).await.ok()?;
        len = u16::from_be_bytes(ext) as usize;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext).await.ok()?;
        len = u64::from_be_bytes(ext) as usize;
    }
    let mut mask = [0u8; 4];
    if masked {
        stream.read_exact(&mut mask).await.ok()?;
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await.ok()?;
    if masked {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i % 4];
        }
    }
    Some((opcode, payload))
}

async fn write_ws_text(stream: &mut tokio::net::TcpStream, text: &str) {
    let bytes = text.as_bytes();
    let mut frame = vec![0x81u8];
    if bytes.len() < 126 {
        frame.push(bytes.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(bytes);
    let _ = stream.write_all(&frame).await;
}

async fn spawn_streaming_voice_server(
    runtime: &tokio::runtime::Runtime,
    received_samples: Arc<Mutex<Vec<f32>>>,
) -> String {
    let listener = runtime
        .spawn(async move { TcpListener::bind("127.0.0.1:0").await.unwrap() })
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();

    runtime.spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let samples_out = received_samples.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let mut total = 0usize;
                let hdr_end = loop {
                    let Ok(n) = stream.read(&mut buf[total..]).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    total += n;
                    if let Some(pos) = buf[..total].windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos;
                    }
                };
                let headers = String::from_utf8_lossy(&buf[..hdr_end]).to_string();
                let path = headers
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                if path.starts_with("/ws/voice") {
                    let ws_key = headers
                        .lines()
                        .find_map(|line| {
                            let (k, v) = line.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("sec-websocket-key")
                                .then(|| v.trim().to_owned())
                        })
                        .unwrap();
                    let accept_src = format!("{ws_key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
                    let accept = base64_encode(&sha1_digest(accept_src.as_bytes()));
                    let upgrade = format!(
                        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                    );
                    let _ = stream.write_all(upgrade.as_bytes()).await;

                    if let Some((1, payload)) = read_ws_frame(&mut stream).await {
                        let start_txt = String::from_utf8_lossy(&payload);
                        assert!(matches!(
                            decode_stream_command(&start_txt).unwrap(),
                            SpeechStreamCommand::Start { .. }
                        ));
                        write_ws_text(
                            &mut stream,
                            &encode_stream_event(&SpeechStreamEvent::Ready).unwrap(),
                        )
                        .await;
                    }

                    while let Some((opcode, payload)) = read_ws_frame(&mut stream).await {
                        match opcode {
                            0x2 => {
                                {
                                    let mut lock = samples_out.lock().unwrap();
                                    for chunk in payload.chunks_exact(4) {
                                        lock.push(f32::from_le_bytes([
                                            chunk[0], chunk[1], chunk[2], chunk[3],
                                        ]));
                                    }
                                }
                                write_ws_text(
                                    &mut stream,
                                    &encode_stream_event(&SpeechStreamEvent::Update {
                                        revision: 1,
                                        text: Some(SpeechStreamText {
                                            committed: "stream ".into(),
                                            tentative: "live".into(),
                                        }),
                                    })
                                    .unwrap(),
                                )
                                .await;
                            }
                            0x1 => {
                                let txt = String::from_utf8_lossy(&payload);
                                if matches!(
                                    decode_stream_command(&txt),
                                    Ok(SpeechStreamCommand::Finish)
                                ) {
                                    write_ws_text(
                                        &mut stream,
                                        &encode_stream_event(&SpeechStreamEvent::Finished {
                                            text: "streamed final transcript".into(),
                                        })
                                        .unwrap(),
                                    )
                                    .await;
                                    break;
                                }
                            }
                            0x8 => break,
                            _ => {}
                        }
                    }
                    return;
                }

                let (status, body) = if path.starts_with("/api/voice/status") {
                    (
                        "200 OK",
                        json!({
                            "supported": true,
                            "state": "ready",
                            "modelId": "whisper-base",
                            "model": "Whisper Base",
                            "size": 142000000u64,
                            "supportsStreaming": true,
                            "supportsTranslation": false,
                            "supportsLanguageDetection": true,
                            "languages": ["en"],
                            "acceleration": "cpu",
                            "benchmarkTier": "cpu",
                            "modelUnloadTimeout": "min_5",
                            "gpuDevices": []
                        })
                        .to_string(),
                    )
                } else if path.starts_with("/api/auth/websocket-ticket/media") {
                    ("200 OK", r#"{"ticket":"media-tkt"}"#.to_owned())
                } else if path.starts_with("/api/voice/cloud/keys") {
                    (
                        "200 OK",
                        r#"{"openai":false,"xai":false,"openrouter":false}"#.to_owned(),
                    )
                } else {
                    ("404 Not Found", "{}".to_owned())
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    format!("http://127.0.0.1:{}", addr.port())
}

#[gpui::test]
fn streaming_ws_preserves_ordered_chunks_and_finishes_cleanly(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let received_samples = Arc::new(Mutex::new(Vec::<f32>::new()));
    let base_url =
        runtime.block_on(spawn_streaming_voice_server(&runtime, received_samples.clone()));
    let platform = Arc::new(MockAudioPlatform::new());
    let (_, harness, vcx, _) = setup_harness(
        &base_url,
        platform.clone(),
        "thread-stream",
        VoiceDraftSnapshot {
            owner_key: "thread-stream".into(),
            text: String::new(),
            selection: VoiceSelection { start: 0, end: 0 },
        },
        runtime.handle().clone(),
        cx,
    );
    let voice = vcx.read(|cx| harness.read(cx).voice.clone());

    vcx.update(|window, cx| voice.update(cx, |v, cx| v.start(window, cx)));
    for _ in 0..40 {
        vcx.run_until_parked();
        if vcx.read(|cx| voice.read(cx).state().phase) == VoiceInputPhase::Recording {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(vcx.read(|cx| voice.read(cx).state().phase), VoiceInputPhase::Recording);

    // Emit two distinct chunks immediately (even while WS handshake may still be opening)
    // and confirm FIFO ordering and non-hanging Finish.
    platform.emit_pcm(vec![0.25, 0.5]);
    platform.emit_pcm(vec![0.75]);
    vcx.update(|_, cx| voice.update(cx, |v, cx| v.stop(cx)));

    for _ in 0..80 {
        vcx.run_until_parked();
        if !vcx.read(|cx| harness.read(cx).committed.borrow().is_empty()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    vcx.read(|cx| {
        let h = harness.read(cx);
        let committed = h.committed.borrow();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].0, "streamed final transcript");
        assert_eq!(voice.read(cx).state().phase, VoiceInputPhase::Idle);
    });
    assert_eq!(*received_samples.lock().unwrap(), vec![0.25, 0.5, 0.75]);
}

#[gpui::test]
fn shared_session_lock_and_cloud_preflight_guards(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let base_url = runtime.block_on(spawn_mock_voice_server(&runtime, fail, "ok"));
    let platform = Arc::new(MockAudioPlatform::new());
    let (state, harness, vcx, _) = setup_harness(
        &base_url,
        platform.clone(),
        "thread-lock",
        VoiceDraftSnapshot {
            owner_key: "thread-lock".into(),
            text: String::new(),
            selection: VoiceSelection { start: 0, end: 0 },
        },
        runtime.handle().clone(),
        cx,
    );
    let voice1 = vcx.read(|cx| harness.read(cx).voice.clone());
    let voice2 = vcx.update(|_, cx| {
        cx.new(|cx| {
            VoiceSpeechController::new(
                VoiceSpeechConfig {
                    app: state.clone(),
                    owner_key: "second-editor".into(),
                    offer_setup: false,
                    draft_reader: Rc::new(|_| {
                        Some(VoiceDraftSnapshot {
                            owner_key: "second-editor".into(),
                            text: String::new(),
                            selection: VoiceSelection { start: 0, end: 0 },
                        })
                    }),
                    hints_reader: Rc::new(|_| Vec::new()),
                    project_id_reader: Rc::new(|_| None),
                    environment_id_reader: Rc::new(|_| Some("env-1".to_owned())),
                    on_open_voice_settings: Rc::new(|_, _| {}),
                },
                cx,
            )
        })
    });

    // 1. Start recording on controller 1; controller 2 must fail cleanly with shared-lock error.
    vcx.update(|window, cx| voice1.update(cx, |v, cx| v.start(window, cx)));
    for _ in 0..40 {
        vcx.run_until_parked();
        if vcx.read(|cx| voice1.read(cx).state().phase) == VoiceInputPhase::Recording {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(vcx.read(|cx| voice1.read(cx).state().phase), VoiceInputPhase::Recording);

    vcx.update(|window, cx| voice2.update(cx, |v, cx| v.start(window, cx)));
    vcx.run_until_parked();
    vcx.read(|cx| {
        let s = voice2.read(cx).state();
        assert_eq!(s.phase, VoiceInputPhase::Error);
        assert_eq!(s.error.as_deref(), Some("Another voice recording is already active."));
        assert_eq!(voice_error_label(s.error.as_deref()), "Already recording");
    });

    vcx.update(|_, cx| {
        voice1.update(cx, |v, cx| v.cancel(cx));
        voice2.update(cx, |v, cx| v.cancel(cx));
    });
    vcx.run_until_parked();

    // 2. OpenRouter empty-model validation before recording starts.
    vcx.update(|window, cx| {
        install_memory_voice_consent(
            SavedAudioChoice::Granted(AudioGrant {
                id: "grant-or".into(),
                destination: VoiceTranscriptionDestination::Cloud {
                    environment_id: "env-1".into(),
                    provider: CloudSpeechProvider::Openrouter,
                    model: None,
                },
            }),
            false,
            cx,
        );
        voice1.update(cx, |v, cx| v.start(window, cx));
    });
    for _ in 0..40 {
        vcx.run_until_parked();
        if vcx.read(|cx| voice1.read(cx).state().phase) == VoiceInputPhase::Error {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    vcx.read(|cx| {
        let s = voice1.read(cx).state();
        assert_eq!(s.phase, VoiceInputPhase::Error);
        assert_eq!(s.error_action, Some(VoiceErrorAction::Settings));
        assert_eq!(
            s.error.as_deref(),
            Some("Choose an OpenRouter model in Settings \u{2192} Voice.")
        );
    });

    // 3. Missing cloud API key preflight check before recording starts.
    vcx.update(|window, cx| {
        voice1.update(cx, |v, cx| v.cancel(cx));
        install_memory_voice_consent(
            SavedAudioChoice::Granted(AudioGrant {
                id: "grant-openai".into(),
                destination: VoiceTranscriptionDestination::Cloud {
                    environment_id: "env-1".into(),
                    provider: CloudSpeechProvider::Openai,
                    model: None,
                },
            }),
            false,
            cx,
        );
        voice1.update(cx, |v, cx| v.start(window, cx));
    });
    for _ in 0..40 {
        vcx.run_until_parked();
        if vcx.read(|cx| voice1.read(cx).state().phase) == VoiceInputPhase::Error {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    vcx.read(|cx| {
        let s = voice1.read(cx).state();
        assert_eq!(s.phase, VoiceInputPhase::Error);
        assert_eq!(s.error_action, Some(VoiceErrorAction::Settings));
        assert_eq!(
            s.error.as_deref(),
            Some("Add your OpenAI API key in Settings \u{2192} Voice.")
        );
    });
}

#[gpui::test]
fn cleanup_routes_to_draft_environment_and_setup_download_respects_cancel(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let audio_post_process_hits = Arc::new(AtomicBool::new(false));
    let draft_post_process_hits = Arc::new(AtomicBool::new(false));
    let select_hits = Arc::new(AtomicBool::new(false));
    let missing_model = Arc::new(AtomicBool::new(false));

    let spawn_env_server = |pp_hits: Arc<AtomicBool>,
                            sel_hits: Arc<AtomicBool>,
                            missing_flag: Arc<AtomicBool>,
                            transcript: &'static str,
                            cleaned: &'static str| {
        let rt = &runtime;
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    let pp = pp_hits.clone();
                    let sel = sel_hits.clone();
                    let miss = missing_flag.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 32768];
                        let mut total = 0usize;
                        loop {
                            let Ok(n) = stream.read(&mut buf[total..]).await else {
                                return;
                            };
                            if n == 0 {
                                break;
                            }
                            total += n;
                            if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        let req = String::from_utf8_lossy(&buf[..total]);
                        let path = req
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/");
                        let (status, body) = if path.starts_with("/api/voice/status") {
                            let state_str = if miss.load(Ordering::SeqCst) {
                                "missing-model"
                            } else {
                                "ready"
                            };
                            (
                                "200 OK",
                                json!({
                                    "supported": true,
                                    "state": state_str,
                                    "modelId": "whisper-base",
                                    "model": "Whisper Base",
                                    "size": 142000000u64,
                                    "supportsStreaming": false,
                                    "supportsTranslation": false,
                                    "supportsLanguageDetection": true,
                                    "languages": ["en"],
                                    "acceleration": "cpu",
                                    "benchmarkTier": "cpu",
                                    "modelUnloadTimeout": "min_5",
                                    "gpuDevices": []
                                })
                                .to_string(),
                            )
                        } else if path.starts_with("/api/voice/transcribe") {
                            ("200 OK", json!({ "text": transcript }).to_string())
                        } else if path.starts_with("/api/voice/post-process") {
                            pp.store(true, Ordering::SeqCst);
                            ("200 OK", json!({ "text": cleaned }).to_string())
                        } else if path.starts_with("/api/voice/models/download") {
                            (
                                "200 OK",
                                json!({
                                    "models": [{
                                        "id": "whisper-base",
                                        "name": "Whisper Base",
                                        "description": "Base",
                                        "size": 142000000.0,
                                        "languages": ["en"],
                                        "accuracy": 0.9,
                                        "speed": 0.9,
                                        "recommended": true,
                                        "supportsStreaming": false,
                                        "supportsLanguageDetection": true,
                                        "supportsTranslation": false,
                                        "active": false,
                                        "state": "downloadable"
                                    }]
                                })
                                .to_string(),
                            )
                        } else if path.starts_with("/api/voice/models/select") {
                            sel.store(true, Ordering::SeqCst);
                            ("200 OK", json!({ "models": [] }).to_string())
                        } else if path.starts_with("/api/voice/models") {
                            (
                                "200 OK",
                                json!({
                                    "models": [{
                                        "id": "whisper-base",
                                        "name": "Whisper Base",
                                        "description": "Base",
                                        "size": 142000000.0,
                                        "languages": ["en"],
                                        "accuracy": 0.9,
                                        "speed": 0.9,
                                        "recommended": true,
                                        "supportsStreaming": false,
                                        "supportsLanguageDetection": true,
                                        "supportsTranslation": false,
                                        "active": false,
                                        "state": "downloadable"
                                    }]
                                })
                                .to_string(),
                            )
                        } else {
                            ("404 Not Found", "{}".to_owned())
                        };
                        let reply = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(reply.as_bytes()).await;
                        let _ = stream.shutdown().await;
                    });
                }
            });
            format!("http://127.0.0.1:{}", addr.port())
        })
    };

    let audio_url = spawn_env_server(
        audio_post_process_hits.clone(),
        select_hits.clone(),
        missing_model.clone(),
        "raw from audio env",
        "wrong cleanup env",
    );
    let draft_url = spawn_env_server(
        draft_post_process_hits.clone(),
        select_hits.clone(),
        Arc::new(AtomicBool::new(false)),
        "unused",
        "cleaned by draft env",
    );

    let make_env_view = |id: &str, label: &str| {
        let mut config = ServerConfigState::default();
        config.apply(
            serde_json::from_value(json!({
                "type": "snapshot",
                "config": {
                    "environment": {
                        "environmentId": id,
                        "label": label,
                        "capabilities": { "voiceTranscription": true, "cloudVoiceTranscription": false }
                    },
                    "settings": {}
                }
            }))
            .unwrap(),
        );
        let owner = EnvironmentOwner {
            saved_id: id.into(),
            canonical_id: Some(id.into()),
            generation: 1,
            connection_generation: 1,
            shell_generation: 1,
        };
        let summary = Arc::new(EnvironmentSummary::from_shell(
            owner,
            &ShellState::default(),
            &config,
        ));
        Arc::new(EnvironmentView {
            summary,
            status: ConnectionStatus::Connected,
            config,
            grants: None,
            settings_revision: 0,
            keybindings_revision: 0,
        })
    };

    cx.executor().allow_parking();
    let platform = Arc::new(MockAudioPlatform::new());
    let fleet = Arc::new(FleetView {
        revision: 1,
        environments: vec![
            make_env_view("env-audio", "Audio Env"),
            make_env_view("env-draft", "Draft Env"),
        ],
    });
    let state = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        crate::settings::store::init(cx);
        install_audio_platform(platform.clone(), cx);
        install_memory_voice_consent(
            SavedAudioChoice::Granted(AudioGrant {
                id: "grant-cross".into(),
                destination: VoiceTranscriptionDestination::environment("env-audio"),
            }),
            true,
            cx,
        );
        cx.new(|cx| {
            let mut state = AppState::new(runtime.handle().clone(), cx);
            #[cfg(feature = "snapshot")]
            state.stop_pump();
            state.environments = SavedEnvironments {
                active: Some("env-draft".into()),
                environments: vec![
                    SavedEnvironment {
                        id: "env-audio".into(),
                        environment_id: Some("env-audio".into()),
                        label: "Audio Env".into(),
                        http_base: audio_url.clone(),
                        access_token: Some("tok".into()),
                        server_version: None,
                        disabled: false,
                        local: true,
                        ssh: None,
                    },
                    SavedEnvironment {
                        id: "env-draft".into(),
                        environment_id: Some("env-draft".into()),
                        label: "Draft Env".into(),
                        http_base: draft_url.clone(),
                        access_token: Some("tok".into()),
                        server_version: None,
                        disabled: false,
                        local: true,
                        ssh: None,
                    },
                ],
            };
            state.apply_fleet_view(fleet, cx);
            state
        })
    });

    let committed = Rc::new(RefCell::new(Vec::<String>::new()));
    let committed_c = committed.clone();
    let (harness, vcx) = cx.add_window_view(|window, cx| {
        let voice = cx.new(|cx| {
            VoiceSpeechController::new(
                VoiceSpeechConfig {
                    app: state.clone(),
                    owner_key: "cross-env".into(),
                    offer_setup: true,
                    draft_reader: Rc::new(|_| {
                        Some(VoiceDraftSnapshot {
                            owner_key: "cross-env".into(),
                            text: String::new(),
                            selection: VoiceSelection { start: 0, end: 0 },
                        })
                    }),
                    hints_reader: Rc::new(|_| Vec::new()),
                    project_id_reader: Rc::new(|_| None),
                    environment_id_reader: Rc::new(|_| Some("env-draft".to_owned())),
                    on_open_voice_settings: Rc::new(|_, _| {}),
                },
                cx,
            )
        });
        cx.subscribe_in(&voice, window, move |_, _, event: &VoiceControllerEvent, _, _| {
            if let VoiceControllerEvent::CommitDraft { text, .. } = event {
                committed_c.borrow_mut().push(text.clone());
            }
        })
        .detach();
        VoiceHarness {
            voice,
            committed: Rc::new(RefCell::new(Vec::new())),
            submitted: Rc::new(RefCell::new(Vec::new())),
        }
    });
    vcx.run_until_parked();
    let voice = vcx.read(|cx| harness.read(cx).voice.clone());

    vcx.update(|window, cx| voice.update(cx, |v, cx| v.start(window, cx)));
    for _ in 0..40 {
        vcx.run_until_parked();
        if vcx.read(|cx| voice.read(cx).state().phase) == VoiceInputPhase::Recording {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    platform.emit_pcm(vec![0.1; 320]);
    vcx.update(|_, cx| voice.update(cx, |v, cx| v.stop(cx)));
    for _ in 0..80 {
        vcx.run_until_parked();
        if !committed.borrow().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert_eq!(committed.borrow().as_slice(), &["cleaned by draft env"]);
    assert!(draft_post_process_hits.load(Ordering::SeqCst));
    assert!(!audio_post_process_hits.load(Ordering::SeqCst));

    // Verify setup_download does NOT call select_model when download resolves without Installed state.
    missing_model.store(true, Ordering::SeqCst);
    vcx.update(|window, cx| voice.update(cx, |v, cx| v.start(window, cx)));
    for _ in 0..40 {
        vcx.run_until_parked();
        if vcx.read(|cx| voice.read(cx).setup().open && voice.read(cx).setup().model.is_some()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(vcx.read(|cx| voice.read(cx).setup().open && voice.read(cx).setup().model.is_some()));

    vcx.update(|_, cx| voice.update(cx, |v, cx| v.setup_download(cx)));
    for _ in 0..40 {
        vcx.run_until_parked();
        if !vcx.read(|cx| voice.read(cx).setup().downloading) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!select_hits.load(Ordering::SeqCst));
    assert_eq!(
        vcx.read(|cx| voice.read(cx).setup().step),
        super::controller::VoiceSetupStep::Confirm
    );
}

#[gpui::test]
async fn workspace_overlay_repaints_on_floating_indicator_and_setup_modal_updates(
    cx: &mut TestAppContext,
) {
    use std::sync::atomic::AtomicUsize;
    use futures::StreamExt as _;
    use gpui::ParentElement as _;
    use super::controller::BackgroundVoiceTarget;
    use super::shortcuts::{register_dictation_target, unregister_dictation_target};
    use super::ui::render_active_voice_setup_modal;

    struct OverlayHostView {
        renders: Arc<AtomicUsize>,
        overlay_present: Arc<AtomicBool>,
    }

    impl Render for OverlayHostView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.renders.fetch_add(1, Ordering::SeqCst);
            let overlay = render_active_voice_setup_modal(cx);
            self.overlay_present.store(overlay.is_some(), Ordering::SeqCst);
            div().children(overlay)
        }
    }

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let base_url = runtime.block_on(spawn_mock_voice_server(&runtime, fail, "overlay ok"));
    let platform = Arc::new(MockAudioPlatform::new());
    let (_, harness, vcx, _) = setup_harness(
        &base_url,
        platform.clone(),
        "thread-overlay-1",
        VoiceDraftSnapshot {
            owner_key: "thread-overlay-1".into(),
            text: String::new(),
            selection: VoiceSelection { start: 0, end: 0 },
        },
        runtime.handle().clone(),
        cx,
    );

    let voice = vcx.read(|cx| harness.read(cx).voice.clone());
    let (phase_tx, mut phase_rx) = futures::channel::mpsc::unbounded::<VoiceInputPhase>();
    let _phase_sub = vcx.update(|_, cx| {
        cx.subscribe(&voice, move |voice, event, cx| {
            if matches!(event, VoiceControllerEvent::StateChanged) {
                let _ = phase_tx.unbounded_send(voice.read(cx).session_state().phase);
            }
        })
    });

    // Mount a separate window view mimicking `Workspace::render` — it does NOT observe
    // `VoiceSpeechController` and only invokes `render_active_voice_setup_modal(cx)`.
    let renders = Arc::new(AtomicUsize::new(0));
    let overlay_present = Arc::new(AtomicBool::new(false));
    let renders_view = renders.clone();
    let overlay_present_view = overlay_present.clone();
    let (_host, host_vcx) = cx.add_window_view(|_, _| OverlayHostView {
        renders: renders_view,
        overlay_present: overlay_present_view,
    });
    let target_id = host_vcx.update(|_, cx| {
        register_dictation_target(
            &voice,
            true,
            Rc::new(|_, _| true),
            Rc::new(|_| false),
            cx,
        )
    });
    host_vcx.run_until_parked();
    assert!(!overlay_present.load(Ordering::SeqCst));

    // Opening and closing the voice setup modal must repaint the non-observing host window.
    let before_open = renders.load(Ordering::SeqCst);
    host_vcx.update(|_, cx| voice.update(cx, |v, cx| v.setup_set_open(true, cx)));
    host_vcx.run_until_parked();
    assert!(
        renders.load(Ordering::SeqCst) > before_open,
        "opening setup modal must refresh host windows"
    );
    assert!(overlay_present.load(Ordering::SeqCst));

    let before_close = renders.load(Ordering::SeqCst);
    host_vcx.update(|_, cx| voice.update(cx, |v, cx| v.setup_set_open(false, cx)));
    host_vcx.run_until_parked();
    assert!(
        renders.load(Ordering::SeqCst) > before_close,
        "closing setup modal must refresh host windows"
    );
    assert!(!overlay_present.load(Ordering::SeqCst));

    // Configure a persistent target, start recording on thread-overlay-1, then switch away so
    // the floating indicator renders in the host window.
    host_vcx.update(|_, cx| {
        voice.update(cx, |v, _| {
            v.set_target_meta_reader(Rc::new(|_| {
                Some(BackgroundVoiceTarget {
                    owner_key: "thread-overlay-1".into(),
                    label: Some("Overlay thread".into()),
                    persistent: true,
                    open: None,
                    read_stored_draft: Some(Rc::new(|_| {
                        Some(VoiceDraftSnapshot {
                            owner_key: "thread-overlay-1".into(),
                            text: String::new(),
                            selection: VoiceSelection { start: 0, end: 0 },
                        })
                    })),
                    commit_stored_draft: Some(Rc::new(|_, _, _| {})),
                })
            }));
        });
    });
    host_vcx.update(|window, cx| voice.update(cx, |v, cx| v.start(window, cx)));
    while let Some(phase) = phase_rx.next().await {
        if phase == VoiceInputPhase::Recording {
            break;
        }
    }

    let before_bg = renders.load(Ordering::SeqCst);
    host_vcx.update(|_, cx| voice.update(cx, |v, cx| v.set_owner_key("thread-overlay-2", cx)));
    host_vcx.run_until_parked();
    assert!(
        renders.load(Ordering::SeqCst) > before_bg,
        "switching away from an active recording must repaint the floating indicator"
    );
    assert!(overlay_present.load(Ordering::SeqCst));

    // Live audio level updates while backgrounded must also repaint the floating indicator.
    let before_level = renders.load(Ordering::SeqCst);
    platform.emit_level(0.8);
    host_vcx.run_until_parked();
    assert!(
        host_vcx.read(|cx| voice.read(cx).level()) > 0.5,
        "controller should have received emitted audio level"
    );
    assert!(
        renders.load(Ordering::SeqCst) > before_level,
        "audio level updates while backgrounded must repaint the floating indicator"
    );

    // Cancelling the background recording must clear the floating indicator and repaint.
    let before_cancel = renders.load(Ordering::SeqCst);
    host_vcx.update(|_, cx| voice.update(cx, |v, cx| v.cancel(cx)));
    host_vcx.run_until_parked();
    assert!(
        renders.load(Ordering::SeqCst) > before_cancel,
        "cancelling background dictation must refresh host windows"
    );
    assert!(!overlay_present.load(Ordering::SeqCst));

    host_vcx.update(|_, cx| unregister_dictation_target(target_id, cx));
}

#[gpui::test]
async fn starting_dictation_on_presented_thread_clears_offscreen_error_while_indicator_retry_targets_background(
    cx: &mut TestAppContext,
) {
    use futures::StreamExt as _;
    use super::controller::BackgroundVoiceTarget;

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let fail = Arc::new(AtomicBool::new(true));
    let base_url =
        runtime.block_on(spawn_mock_voice_server(&runtime, fail.clone(), "captured speech"));
    let platform = Arc::new(MockAudioPlatform::new());

    let presented_owner = Arc::new(Mutex::new("thread-a".to_owned()));
    let draft_a = Arc::new(Mutex::new(VoiceDraftSnapshot {
        owner_key: "thread-a".into(),
        text: "alpha ".into(),
        selection: VoiceSelection { start: 6, end: 6 },
    }));
    let draft_b = Arc::new(Mutex::new(VoiceDraftSnapshot {
        owner_key: "thread-b".into(),
        text: "beta ".into(),
        selection: VoiceSelection { start: 5, end: 5 },
    }));
    let stored_commits = Arc::new(Mutex::new(Vec::<(String, String)>::new()));

    let (state, _, vcx, _) = setup_harness(
        &base_url,
        platform.clone(),
        "unused",
        VoiceDraftSnapshot {
            owner_key: "unused".into(),
            text: String::new(),
            selection: VoiceSelection { start: 0, end: 0 },
        },
        runtime.handle().clone(),
        cx,
    );

    let presented_commits = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let (phase_tx, mut phase_rx) = futures::channel::mpsc::unbounded::<VoiceInputPhase>();

    let owner_for_draft = presented_owner.clone();
    let draft_a_for_draft = draft_a.clone();
    let draft_b_for_draft = draft_b.clone();
    let owner_for_meta = presented_owner.clone();
    let draft_a_for_meta = draft_a.clone();
    let draft_b_for_meta = draft_b.clone();
    let stored_commits_for_meta = stored_commits.clone();
    let owner_for_sub = presented_owner.clone();
    let draft_a_for_sub = draft_a.clone();
    let draft_b_for_sub = draft_b.clone();
    let presented_commits_for_sub = presented_commits.clone();

    let voice = vcx.update(|_, cx| {
        let voice = cx.new(|cx| {
            let mut controller = VoiceSpeechController::new(
                VoiceSpeechConfig {
                    app: state.clone(),
                    owner_key: "thread-a".into(),
                    offer_setup: true,
                    draft_reader: Rc::new(move |_| {
                        let owner = owner_for_draft.lock().unwrap().clone();
                        let snap = if owner == "thread-a" {
                            draft_a_for_draft.lock().unwrap().clone()
                        } else {
                            draft_b_for_draft.lock().unwrap().clone()
                        };
                        Some(snap)
                    }),
                    hints_reader: Rc::new(|_| Vec::new()),
                    project_id_reader: Rc::new(|_| None),
                    environment_id_reader: Rc::new(|_| Some("env-1".to_owned())),
                    on_open_voice_settings: Rc::new(|_, _| {}),
                },
                cx,
            );
            controller.set_target_meta_reader(Rc::new(move |_| {
                let owner = owner_for_meta.lock().unwrap().clone();
                let target_draft = if owner == "thread-a" {
                    draft_a_for_meta.clone()
                } else {
                    draft_b_for_meta.clone()
                };
                let read_draft = target_draft.clone();
                let write_draft = target_draft;
                let commits = stored_commits_for_meta.clone();
                let commit_owner = owner.clone();
                Some(BackgroundVoiceTarget {
                    owner_key: owner.clone(),
                    label: Some(owner),
                    persistent: true,
                    open: None,
                    read_stored_draft: Some(Rc::new(move |_| {
                        Some(read_draft.lock().unwrap().clone())
                    })),
                    commit_stored_draft: Some(Rc::new(move |text, selection, _| {
                        commits.lock().unwrap().push((commit_owner.clone(), text.clone()));
                        let mut d = write_draft.lock().unwrap();
                        d.text = text;
                        d.selection = selection;
                    })),
                })
            }));
            controller
        });
        cx.subscribe(&voice, move |voice, event, cx| match event {
            VoiceControllerEvent::CommitDraft { text, selection } => {
                let owner = owner_for_sub.lock().unwrap().clone();
                presented_commits_for_sub
                    .lock()
                    .unwrap()
                    .push((owner.clone(), text.clone()));
                let target = if owner == "thread-a" {
                    &draft_a_for_sub
                } else {
                    &draft_b_for_sub
                };
                let mut d = target.lock().unwrap();
                d.text = text.clone();
                d.selection = *selection;
            }
            VoiceControllerEvent::StateChanged => {
                let _ = phase_tx.unbounded_send(voice.read(cx).session_state().phase);
            }
            VoiceControllerEvent::SubmitAfterDictation { .. } => {}
        })
        .detach();
        voice
    });
    vcx.run_until_parked();

    // 1. Start recording on thread-a, then navigate to thread-b and fail transcription off-screen.
    vcx.update(|window, cx| voice.update(cx, |v, cx| v.start(window, cx)));
    while let Some(phase) = phase_rx.next().await {
        if phase == VoiceInputPhase::Recording {
            break;
        }
    }
    platform.emit_pcm(vec![0.2; 480]);
    *presented_owner.lock().unwrap() = "thread-b".to_owned();
    vcx.update(|_, cx| voice.update(cx, |v, cx| v.set_owner_key("thread-b", cx)));
    vcx.update(|_, cx| voice.update(cx, |v, cx| v.stop(cx)));
    while let Some(phase) = phase_rx.next().await {
        if phase == VoiceInputPhase::Error {
            break;
        }
    }

    vcx.read(|cx| {
        let v = voice.read(cx);
        assert!(v.shows_floating_indicator());
        assert!(!v.is_busy_elsewhere());
        assert_eq!(v.state().phase, VoiceInputPhase::Idle);
        assert_eq!(v.session_state().phase, VoiceInputPhase::Error);
    });

    // 2. Clicking Retry on the floating indicator retries off-screen thread-a's recording.
    fail.store(false, Ordering::SeqCst);
    vcx.update(|window, cx| voice.update(cx, |v, cx| v.retry_active_target(window, cx)));
    while let Some(phase) = phase_rx.next().await {
        if phase == VoiceInputPhase::Idle {
            break;
        }
    }
    assert_eq!(
        stored_commits.lock().unwrap().as_slice(),
        &[("thread-a".to_owned(), "alpha captured speech".to_owned())]
    );
    assert!(presented_commits.lock().unwrap().is_empty());
    assert_eq!(draft_b.lock().unwrap().text, "beta ");

    // 3. Reproduce a second off-screen error on thread-a, then invoke `start()` from thread-b.
    *presented_owner.lock().unwrap() = "thread-a".to_owned();
    vcx.update(|_, cx| voice.update(cx, |v, cx| v.set_owner_key("thread-a", cx)));
    fail.store(true, Ordering::SeqCst);
    vcx.update(|window, cx| voice.update(cx, |v, cx| v.start(window, cx)));
    while let Some(phase) = phase_rx.next().await {
        if phase == VoiceInputPhase::Recording {
            break;
        }
    }
    platform.emit_pcm(vec![0.2; 480]);
    *presented_owner.lock().unwrap() = "thread-b".to_owned();
    vcx.update(|_, cx| voice.update(cx, |v, cx| v.set_owner_key("thread-b", cx)));
    vcx.update(|_, cx| voice.update(cx, |v, cx| v.stop(cx)));
    while let Some(phase) = phase_rx.next().await {
        if phase == VoiceInputPhase::Error {
            break;
        }
    }
    assert!(vcx.read(|cx| voice.read(cx).shows_floating_indicator()));

    // Starting on thread-b must dismiss thread-a's stale off-screen error and dictate into thread-b.
    fail.store(false, Ordering::SeqCst);
    vcx.update(|window, cx| voice.update(cx, |v, cx| v.start(window, cx)));
    while let Some(phase) = phase_rx.next().await {
        if phase == VoiceInputPhase::Recording {
            break;
        }
    }
    vcx.read(|cx| {
        let v = voice.read(cx);
        assert!(v.is_recording_owner_presented());
        assert!(!v.shows_floating_indicator());
        assert_eq!(v.state().phase, VoiceInputPhase::Recording);
    });

    platform.emit_pcm(vec![0.2; 480]);
    vcx.update(|_, cx| voice.update(cx, |v, cx| v.stop(cx)));
    while let Some(phase) = phase_rx.next().await {
        if phase == VoiceInputPhase::Idle {
            break;
        }
    }

    assert_eq!(
        presented_commits.lock().unwrap().as_slice(),
        &[("thread-b".to_owned(), "beta captured speech".to_owned())]
    );
    assert_eq!(
        stored_commits.lock().unwrap().len(),
        1,
        "thread-a must not receive a second commit when starting dictation on thread-b"
    );
    assert_eq!(draft_a.lock().unwrap().text, "alpha captured speech");
    assert_eq!(draft_b.lock().unwrap().text, "beta captured speech");
}
