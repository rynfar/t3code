//! Offscreen GPUI integration tests for `PN-PREVIEW` (`#222`, `#390`, `#249`):
//! - Default zoom & appearance forwarded on `preview.open` (`pingdotgg/t3code#34943`, `pingdotgg/t3code#25874`)
//! - Unified `RightPanels` closed-tab reopen preserving browser tab settings (`pingdotgg/t3code#34859`)
//! - Incognito visit & title exclusion (`pingdotgg/t3code#34859`)
//! - Stale file-picker withdrawal in `FrameView` on stream drop/disconnect (`pingdotgg/t3code#29655`)
//! - Device toolbar toggle sizing (`pingdotgg/t3code#35664`)
//! - Preview annotation editor wired to `VoiceSpeechController` dictation (`pingdotgg/t3code#30624`)
//!   with real Escape key/action dispatch canceling dictation first (`pingdotgg/t3code#32682`),
//!   `submit_after_dictation`, commit draft, and composer delivery.

use std::sync::Arc;

use gpui::{AppContext, Entity, Focusable};
use serde_json::json;
use t3_client::preview_stream::StreamEvent;
use t3_client::supervisor::{ClientFixture, FixtureRequest};
use t3_core::preview::annotation::{AnnotationRegion, AnnotationTarget};
use t3_core::preview::stream::{Control, Controller, FileChooserOffer};
use t3_core::preview::{SessionSnapshot, method};
use t3_core::voice_input::{SavedAudioChoice, VoiceInputPhase, VoiceInputState, VoiceSelection};

use super::{PreviewPane, Previews};
use crate::app_state::AppState;
use crate::voice::{MockAudioPlatform, VoiceControllerEvent, install_audio_platform, install_memory_voice_consent};

fn setup(cx: &mut gpui::TestAppContext) -> (Entity<AppState>, Entity<Previews>, ClientFixture, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, fixture) = cx.update(|cx| {
        gpui_component::init(cx);
        install_audio_platform(Arc::new(MockAudioPlatform::default()), cx);
        install_memory_voice_consent(SavedAudioChoice::Unset, false, cx);
        let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
        crate::settings::store::init(cx);
        installed
    });
    state.update(cx, |state, cx| {
        state.panels.panels = t3_core::right_panel::RightPanels::default();
        state.select_thread("menu-thread", cx);
    });
    fixture
        .grants
        .send(Some(
            serde_json::from_value(json!({
                "authenticated": true,
                "permissions": ["orchestration:read", "orchestration:operate", "preview:operate"]
            }))
            .unwrap(),
        ))
        .unwrap();
    let previews = cx.update(|cx| {
        super::init(state.clone(), cx);
        let entity = super::previews(cx).expect("previews global initialized");
        entity.update(cx, |previews, _| previews.set_server_browser_for_test(true));
        entity
    });
    (state, previews, fixture, runtime)
}

async fn next_preview_request(fixture: &mut ClientFixture) -> FixtureRequest {
    loop {
        let (rpc_method, payload, reply) = fixture.next_request().await;
        if rpc_method == method::LIST {
            let _ = reply.send(Ok(json!({ "sessions": [], "serverEpoch": "epoch-1", "revision": 1 })));
            continue;
        }
        if !rpc_method.starts_with("preview.") {
            let _ = reply.send(Ok(json!({})));
            continue;
        }
        return (rpc_method, payload, reply);
    }
}

fn snapshot(tab_id: &str, url: &str, profile_id: Option<&str>) -> SessionSnapshot {
    SessionSnapshot::decode(json!({
        "threadId": "menu-thread",
        "tabId": tab_id,
        "navStatus": { "_tag": "Success", "url": url, "title": "Example Page" },
        "canGoBack": false,
        "canGoForward": false,
        "runtime": "server",
        "headless": true,
        "viewport": { "_tag": "fill" },
        "profileId": profile_id,
        "zoomFactor": 1.0,
        "colorScheme": "system",
        "updatedAt": "2026-11-14T00:00:01Z",
    }))
    .unwrap()
}

#[gpui::test]
async fn open_session_sends_default_zoom_and_appearance_and_reopen_preserves_closed_tab(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let (state, previews, mut fixture, _runtime) = setup(cx);
    cx.update(|cx| {
        crate::settings::store::install_committed_client(
            &state,
            serde_json::from_value(json!({
                "browserDefaultZoomFactor": 0.8,
                "browserDefaultAppearance": "dark",
                "browserProfiles": [{ "id": "work", "name": "Work", "kind": "persistent" }],
                "browserDefaultProfileId": "work"
            }))
            .unwrap(),
            cx,
        );
    });

    let (opened_tx, opened_rx) = tokio::sync::oneshot::channel();
    let mut opened_tx = Some(opened_tx);
    let _opened_sub = previews.update(cx, |_, cx| {
        cx.subscribe(&previews, move |previews, _, event: &super::PreviewsEvent, _| {
            if let super::PreviewsEvent::Thread(thread) = event
                && thread == "menu-thread"
                && previews.session("menu-thread", "tab-open-1").is_some()
                && let Some(tx) = opened_tx.take()
            {
                let _ = tx.send(());
            }
        })
    });

    previews.update(cx, |previews, cx| {
        previews.open_session("menu-thread".into(), Some("https://t3.chat/".into()), cx);
    });

    let (rpc_method, payload, reply) = next_preview_request(&mut fixture).await;
    assert_eq!(rpc_method, method::OPEN);
    assert_eq!(payload["runtime"], "server");
    assert_eq!(payload["zoomFactor"], 0.8);
    assert_eq!(payload["colorScheme"], "dark");
    assert_eq!(payload["profileId"], "work");
    assert_eq!(payload["url"], "https://t3.chat/");
    let _ = reply.send(Ok(json!({
        "threadId": "menu-thread",
        "tabId": "tab-open-1",
        "navStatus": { "_tag": "Success", "url": "https://t3.chat/", "title": "T3" },
        "canGoBack": false,
        "canGoForward": false,
        "runtime": "server",
        "viewport": { "_tag": "freeform", "width": 1200, "height": 800 },
        "profileId": "work",
        "zoomFactor": 1.5,
        "colorScheme": "light",
        "updatedAt": "2026-11-14T00:00:02Z"
    })));
    opened_rx.await.unwrap();
    cx.run_until_parked();

    // Closing the browser tab through AppState records it once in the unified RightPanels
    // closed-view stack; reopening restores its exact settings and consumes that single entry.
    state.update(cx, |state, cx| {
        state.close_surface("browser:tab-open-1", cx);
        assert_eq!(state.panels.panels.closed_views().len(), 1);
    });
    let (close_method, _, close_reply) = next_preview_request(&mut fixture).await;
    assert_eq!(close_method, method::CLOSE);
    let _ = close_reply.send(Ok(json!({})));

    state.update(cx, |state, cx| {
        state.reopen_closed_view(cx);
        assert!(state.panels.panels.closed_views().is_empty());
    });
    let (reopen_method, reopen_payload, reopen_reply) = next_preview_request(&mut fixture).await;
    assert_eq!(reopen_method, method::OPEN);
    assert_eq!(reopen_payload["url"], "https://t3.chat/");
    assert_eq!(reopen_payload["viewport"], json!({ "_tag": "freeform", "width": 1200, "height": 800 }));
    assert_eq!(reopen_payload["profileId"], "work");
    assert_eq!(reopen_payload["zoomFactor"], 1.5);
    assert_eq!(reopen_payload["colorScheme"], "light");
    let _ = reopen_reply.send(Ok(json!({
        "threadId": "menu-thread",
        "tabId": "tab-open-2",
        "navStatus": { "_tag": "Success", "url": "https://t3.chat/", "title": "T3" },
        "canGoBack": false,
        "canGoForward": false,
        "runtime": "server",
        "updatedAt": "2026-11-14T00:00:03Z"
    })));

    state.update(cx, |state, _| state.stop_pump());
    cx.run_until_parked();
}

#[gpui::test]
async fn incognito_visits_and_titles_stay_out_of_project_history(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let (state, previews, _fixture, _runtime) = setup(cx);

    previews.update(cx, |previews, cx| {
        // Seed a normal visit and verify it enters project history.
        previews.record_visit("menu-thread", Some("default"), "https://public.example/", cx);
        // Incognito visit must stay out of history.
        previews.record_visit("menu-thread", Some("incognito"), "https://secret.example/", cx);
        // Settled Incognito snapshot must never title an existing history entry.
        previews.seed_snapshot_for_test(snapshot("tab-inc", "https://public.example/", Some("incognito")), cx);
    });

    previews.read_with(cx, |previews, cx| {
        let recent = previews.recent("menu-thread", cx);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].url, "https://public.example/");
        assert_eq!(recent[0].title, None);
    });

    // Settled normal snapshot titles the history entry cleanly.
    previews.update(cx, |previews, cx| {
        previews.seed_snapshot_for_test(snapshot("tab-pub", "https://public.example/", Some("default")), cx);
    });
    previews.read_with(cx, |previews, cx| {
        let recent = previews.recent("menu-thread", cx);
        assert_eq!(recent[0].title.as_deref(), Some("Example Page"));
    });

    state.update(cx, |state, _| state.stop_pump());
    cx.run_until_parked();
}

#[gpui::test]
async fn frame_view_withdraws_stale_file_picker_on_disconnect_and_accepts_reoffered_id(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let (state, previews, _fixture, _runtime) = setup(cx);
    previews.update(cx, |previews, cx| {
        previews.seed_snapshot_for_test(snapshot("tab-1", "https://example.com/", Some("default")), cx);
    });

    let (pane, window_cx) = cx.add_window_view(|window, cx| {
        PreviewPane::new(
            state.clone(),
            previews.clone(),
            "menu-thread".into(),
            "browser:tab-1".into(),
            Some("tab-1".into()),
            window,
            cx,
        )
    });

    let frame = pane.read_with(window_cx, |pane, _| pane.frame_for_test().expect("frame view exists"));
    frame.update(window_cx, |frame, cx| {
        frame.inject_stream_event_for_test(
            StreamEvent::Message(t3_core::preview::stream::StreamMessage::Control(Control {
                can_operate: true,
                controller: Controller::You,
                generation: 1,
                dialog: None,
            })),
            cx,
        );
        frame.inject_stream_event_for_test(
            StreamEvent::FileChooser(Some(FileChooserOffer { id: "c1".into(), multiple: false, accept: ".csv".into() })),
            cx,
        );
        assert_eq!(frame.file_chooser_for_test().map(|o| o.id.as_str()), Some("c1"));

        // Dropping the socket withdraws c1.
        frame.inject_stream_event_for_test(StreamEvent::Connected(false), cx);
        assert_eq!(frame.file_chooser_for_test(), None);

        // Reconnecting offers new chooser id c2.
        frame.inject_stream_event_for_test(
            StreamEvent::FileChooser(Some(FileChooserOffer { id: "c2".into(), multiple: true, accept: "".into() })),
            cx,
        );
        assert_eq!(frame.file_chooser_for_test().map(|o| o.id.as_str()), Some("c2"));

        // Explicit withdrawal clears c2.
        frame.inject_stream_event_for_test(StreamEvent::FileChooser(None), cx);
        assert_eq!(frame.file_chooser_for_test(), None);
    });

    state.update(window_cx, |state, _| state.stop_pump());
    window_cx.run_until_parked();
}

struct AnnotationHost {
    pane: Entity<PreviewPane>,
}

impl gpui::Render for AnnotationHost {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::ParentElement;
        gpui::div().children(self.pane.read(cx).annotation_for_test())
    }
}

#[gpui::test]
async fn preview_annotation_editor_wires_voice_dictation_and_delivers_to_composer(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let (state, previews, _fixture, _runtime) = setup(cx);
    previews.update(cx, |previews, cx| {
        previews.seed_snapshot_for_test(snapshot("tab-1", "http://localhost:5173/app", Some("default")), cx);
    });

    let (host, window_cx) = cx.add_window_view(|window, cx| {
        let pane = cx.new(|cx| {
            PreviewPane::new(
                state.clone(),
                previews.clone(),
                "menu-thread".into(),
                "browser:tab-1".into(),
                Some("tab-1".into()),
                window,
                cx,
            )
        });
        AnnotationHost { pane }
    });

    let (editor, voice) = window_cx.update(|window, cx| {
        host.update(cx, |host, cx| {
            host.pane.update(cx, |pane, cx| {
                pane.open_annotation_for_test(
                    AnnotationTarget::Region(AnnotationRegion { x: 10., y: 20., width: 200., height: 100. }),
                    window,
                    cx,
                );
                let editor = pane.annotation_for_test().expect("annotation editor open");
                // Focus the editor container rather than the native macOS Textarea input so GPUI's
                // TestWindow renders the real AnnotationEditor view tree without hitting
                // `TestWindow::window_handle` in `set_text_content_type`.
                let focus = editor.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
                let voice = editor.read(cx).voice().clone();
                (editor, voice)
            })
        })
    });
    let _ = window_cx.refresh();

    // Simulate active recording: real Escape action & key dispatch cancel dictation first without
    // closing the annotation editor (`pingdotgg/t3code#32682`).
    voice.update(window_cx, |voice, cx| {
        voice.set_snapshot_state_for_preview(
            VoiceInputState { phase: VoiceInputPhase::Recording, ..VoiceInputState::default() },
            None,
            0.5,
            3,
            None,
            cx,
        );
    });
    window_cx.dispatch_action(gpui_component::input::Escape);
    window_cx.run_until_parked();
    assert!(host.read_with(window_cx, |host, cx| host.pane.read(cx).annotation_for_test().is_some()));
    assert_eq!(voice.read_with(window_cx, |voice, _| voice.state().phase), VoiceInputPhase::Idle);

    // Submitting while recording defers to `submit_after_dictation` instead of silently dropping.
    voice.update(window_cx, |voice, cx| {
        voice.set_snapshot_state_for_preview(
            VoiceInputState { phase: VoiceInputPhase::Recording, ..VoiceInputState::default() },
            None,
            0.5,
            3,
            None,
            cx,
        );
    });
    editor.update(window_cx, |editor, cx| editor.submit(cx));
    assert!(host.read_with(window_cx, |host, cx| host.pane.read(cx).annotation_for_test().is_some()));
    assert!(state.update(window_cx, |state, _| state.take_composer_restore("menu-thread")).is_none());

    // Simulate voice dictation commit + SubmitAfterDictation delivering into the composer (`pingdotgg/t3code#30624`).
    voice.update(window_cx, |voice, cx| {
        voice.set_snapshot_state_for_preview(VoiceInputState::default(), None, 0.0, 0, None, cx);
        cx.emit(VoiceControllerEvent::CommitDraft {
            text: "Make the header sticky on scroll".into(),
            selection: VoiceSelection { start: 32, end: 32 },
        });
        cx.emit(VoiceControllerEvent::SubmitAfterDictation { text: "Make the header sticky on scroll".into() });
    });
    window_cx.run_until_parked();
    assert!(host.read_with(window_cx, |host, cx| host.pane.read(cx).annotation_for_test().is_none()));
    let restored = state.update(window_cx, |state, _| state.take_composer_restore("menu-thread"));
    let (prompt, _attachments) = restored.expect("annotation delivered to composer");
    assert!(prompt.contains("http://localhost:5173/app"));
    assert!(prompt.contains("Region 200×100 at (10, 20)"));
    assert!(prompt.contains("Make the header sticky on scroll"));

    // Reopen and verify a real Escape action when idle cancels and closes the editor.
    window_cx.update(|window, cx| {
        host.update(cx, |host, cx| {
            host.pane.update(cx, |pane, cx| {
                pane.open_annotation_for_test(
                    AnnotationTarget::Region(AnnotationRegion { x: 10., y: 20., width: 200., height: 100. }),
                    window,
                    cx,
                );
                let editor2 = pane.annotation_for_test().expect("annotation editor reopened");
                let focus = editor2.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            })
        })
    });
    let _ = window_cx.refresh();
    window_cx.dispatch_action(gpui_component::input::Escape);
    window_cx.run_until_parked();
    assert!(host.read_with(window_cx, |host, cx| host.pane.read(cx).annotation_for_test().is_none()));

    state.update(window_cx, |state, _| state.stop_pump());
    window_cx.run_until_parked();
}

#[gpui::test]
async fn toggle_device_toolbar_dispatches_preview_resize(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let (state, previews, mut fixture, _runtime) = setup(cx);
    previews.update(cx, |previews, cx| {
        previews.seed_snapshot_for_test(snapshot("tab-1", "https://example.com/", Some("default")), cx);
    });

    let (pane, window_cx) = cx.add_window_view(|window, cx| {
        PreviewPane::new(
            state.clone(),
            previews.clone(),
            "menu-thread".into(),
            "browser:tab-1".into(),
            Some("tab-1".into()),
            window,
            cx,
        )
    });

    let (resized_tx, resized_rx) = tokio::sync::oneshot::channel();
    let mut resized_tx = Some(resized_tx);
    let _resized_sub = previews.update(window_cx, |_, cx| {
        cx.subscribe(&previews, move |previews, _, event: &super::PreviewsEvent, _| {
            if let super::PreviewsEvent::Thread(thread) = event
                && thread == "menu-thread"
                && previews.session("menu-thread", "tab-1").is_some_and(|s| !s.fills_panel())
                && let Some(tx) = resized_tx.take()
            {
                let _ = tx.send(());
            }
        })
    });

    pane.update(window_cx, |pane, cx| pane.toggle_device_toolbar_for_test(cx));
    let (rpc_method, payload, reply) = next_preview_request(&mut fixture).await;
    assert_eq!(rpc_method, method::RESIZE);
    assert_eq!(payload["tabId"], "tab-1");
    assert_eq!(payload["viewport"]["_tag"], "freeform");
    let _ = reply.send(Ok(json!({
        "threadId": "menu-thread",
        "tabId": "tab-1",
        "navStatus": { "_tag": "Success", "url": "https://example.com/", "title": "Example Page" },
        "canGoBack": false,
        "canGoForward": false,
        "runtime": "server",
        "viewport": { "_tag": "freeform", "width": 1024, "height": 768 },
        "updatedAt": "2026-11-14T00:00:05Z"
    })));
    resized_rx.await.unwrap();

    // Toggling again returns to Fill.
    pane.update(window_cx, |pane, cx| pane.toggle_device_toolbar_for_test(cx));
    let (rpc_method2, payload2, reply2) = next_preview_request(&mut fixture).await;
    assert_eq!(rpc_method2, method::RESIZE);
    assert_eq!(payload2["viewport"], json!({ "_tag": "fill" }));
    let _ = reply2.send(Ok(json!({
        "threadId": "menu-thread",
        "tabId": "tab-1",
        "navStatus": { "_tag": "Success", "url": "https://example.com/", "title": "Example Page" },
        "canGoBack": false,
        "canGoForward": false,
        "runtime": "server",
        "viewport": { "_tag": "fill" },
        "updatedAt": "2026-11-14T00:00:06Z"
    })));

    state.update(window_cx, |state, _| state.stop_pump());
    window_cx.run_until_parked();
}

#[gpui::test]
async fn preview_find_action_shows_unavailable_info_toast(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let (state, previews, _fixture, _runtime) = setup(cx);
    previews.update(cx, |previews, cx| {
        previews.seed_snapshot_for_test(snapshot("tab-find", "https://example.com/", Some("default")), cx);
    });

    let (pane, window_cx) = cx.add_window_view(|window, cx| {
        PreviewPane::new(
            state.clone(),
            previews.clone(),
            "menu-thread".into(),
            "browser:tab-find".into(),
            Some("tab-find".into()),
            window,
            cx,
        )
    });

    window_cx.update(|window, cx| {
        let frame = pane.read(cx).frame_for_test().expect("frame view exists");
        let focus = frame.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        window.draw(cx).clear();
        window.dispatch_action(Box::new(super::Find), cx);
    });

    state.read_with(window_cx, |state, _| {
        let toast = state.toasts.latest().expect("preview.find shows info toast");
        assert_eq!(toast.kind, crate::toast::ToastKind::Info);
        assert_eq!(toast.title, "Find on page isn't available for this page");
    });

    state.update(window_cx, |state, _| state.stop_pump());
    window_cx.run_until_parked();
}

#[gpui::test]
async fn capture_screenshot_attaches_full_frame_png_to_composer(cx: &mut gpui::TestAppContext) {
    cx.executor().allow_parking();
    let (state, previews, _fixture, _runtime) = setup(cx);
    previews.update(cx, |previews, cx| {
        previews.seed_snapshot_for_test(snapshot("tab-shot", "https://example.com/", Some("default")), cx);
    });

    let mut jpeg = Vec::new();
    let img = image::RgbImage::from_pixel(4, 4, image::Rgb([32, 96, 180]));
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .expect("encode test jpeg");

    let (pane, window_cx) = cx.add_window_view(|window, cx| {
        PreviewPane::new(
            state.clone(),
            previews.clone(),
            "menu-thread".into(),
            "browser:tab-shot".into(),
            Some("tab-shot".into()),
            window,
            cx,
        )
    });

    window_cx.update(|_, cx| {
        let frame = pane.read(cx).frame_for_test().expect("frame view exists");
        frame.update(cx, |frame, _| frame.seed_jpeg_for_test(jpeg));
        pane.update(cx, |pane, cx| pane.capture_screenshot_for_test(cx));
    });

    let (prompt, attachments) = state
        .update(window_cx, |state, _| state.take_composer_restore("menu-thread"))
        .expect("capture_screenshot should restore composer attachment");
    assert!(prompt.is_empty());
    assert_eq!(attachments.len(), 1, "capture_screenshot should queue one full-frame PNG attachment");
    assert_eq!(attachments[0].mime_type, "image/png");
    assert!(attachments[0].name.ends_with(".png"));
    assert!(attachments[0].bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

    state.update(window_cx, |state, _| state.stop_pump());
    window_cx.run_until_parked();
}
