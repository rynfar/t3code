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
    AudioGrant, SavedAudioChoice, VoiceDraftSnapshot, VoiceErrorAction, VoiceInputPhase,
    VoiceSelection, VoiceTranscriptionDestination, voice_error_label,
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
                    "capabilities": { "voiceTranscription": true, "cloudVoiceTranscription": false }
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
    (harness, vcx, draft)
}

#[gpui::test]
fn capture_preserves_edits_and_submits_after_dictation(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let base_url = runtime.block_on(spawn_mock_voice_server(&runtime, fail, "dictated words"));
    let platform = Arc::new(MockAudioPlatform::new());
    let (harness, vcx, draft) = setup_harness(
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

    let (harness, vcx, _) = setup_harness(
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
