use super::*;

fn draft(owner_key: &str, text: &str, start: usize, end: usize) -> VoiceDraftSnapshot {
    VoiceDraftSnapshot {
        owner_key: owner_key.to_owned(),
        text: text.to_owned(),
        selection: VoiceSelection { start, end },
    }
}

fn default_draft() -> VoiceDraftSnapshot {
    draft("environment:thread", "hello world", 6, 11)
}

// --- stream.test.ts parity ---

#[test]
fn stream_drains_ordered_pcm_before_finishing_and_replaces_tentative_hypotheses() {
    let mut machine = VoiceStreamMachine::new(SpeechTranscriptionOptions::default());
    let open_actions = machine.on_open(true);
    assert_eq!(open_actions.len(), 1);
    match &open_actions[0] {
        VoiceStreamAction::SendText(raw) => {
            let cmd = decode_stream_command(raw).unwrap();
            assert_eq!(cmd, SpeechStreamCommand::Start { options: SpeechTranscriptionOptions::default() });
        }
        other => panic!("expected SendText, got {other:?}"),
    }

    // Audio buffered before ready is queued and not sent yet.
    let feed1 = machine.feed_f32(&[0.1], true);
    assert!(feed1.is_empty());
    assert_eq!(machine.queued_bytes(), 4);

    let ready_actions = machine.on_event(SpeechStreamEvent::Ready, true);
    assert!(ready_actions.iter().any(|a| matches!(a, VoiceStreamAction::SendBinary(b) if b.len() == 4)));

    let feed2 = machine.feed_f32(&[0.2], true);
    assert!(feed2.iter().any(|a| matches!(a, VoiceStreamAction::SendBinary(b) if {
        f32::from_le_bytes([b[0], b[1], b[2], b[3]]) == 0.2_f32
    })));
    assert_eq!(machine.in_flight_count(), 2);

    // Finish waits until queued and in-flight chunks drain.
    let finish_actions = machine.finish(true);
    assert!(!finish_actions.iter().any(|a| matches!(a, VoiceStreamAction::SendText(_))));

    let upd1 = machine.on_event(
        SpeechStreamEvent::Update {
            revision: 1,
            text: Some(SpeechStreamText { committed: "hello ".into(), tentative: "were".into() }),
        },
        true,
    );
    assert!(upd1.contains(&VoiceStreamAction::Text(SpeechStreamText {
        committed: "hello ".into(),
        tentative: "were".into(),
    })));
    assert!(!upd1.iter().any(|a| matches!(a, VoiceStreamAction::SendText(_))));

    let upd2 = machine.on_event(
        SpeechStreamEvent::Update {
            revision: 2,
            text: Some(SpeechStreamText { committed: "hello ".into(), tentative: "world".into() }),
        },
        true,
    );
    assert!(upd2.contains(&VoiceStreamAction::Text(SpeechStreamText {
        committed: "hello ".into(),
        tentative: "world".into(),
    })));
    assert!(upd2.iter().any(|a| matches!(a, VoiceStreamAction::SendText(s) if s == r#"{"type":"finish"}"#)));

    let finished = machine.on_event(SpeechStreamEvent::Finished { text: "hello world".into() }, true);
    assert!(finished.contains(&VoiceStreamAction::Finished("hello world".into())));
    assert!(finished.contains(&VoiceStreamAction::CloseSocket));
    assert!(machine.is_closed());
    assert_eq!(machine.final_result(), Some(&Ok("hello world".to_owned())));
}

#[test]
fn stream_rejects_recordings_beyond_duration_limit() {
    let mut machine = VoiceStreamMachine::new(SpeechTranscriptionOptions::default());
    machine.on_open(true);
    machine.on_event(SpeechStreamEvent::Ready, true);

    let too_many_bytes = vec![0u8; SPEECH_STREAM_MAX_QUEUED_BYTES + 4];
    let actions = machine.feed_bytes(&too_many_bytes, true);
    assert!(actions.iter().any(|a| matches!(
        a,
        VoiceStreamAction::Failed { message, notify: true } if message.contains("limited to five minutes")
    )));
    assert!(actions.contains(&VoiceStreamAction::CloseSocket));
    assert!(machine.is_closed());
}

#[test]
fn stream_does_not_send_queued_frames_after_cancel_or_revoke() {
    for action in ["cancel", "revoke"] {
        let mut machine = VoiceStreamMachine::new(SpeechTranscriptionOptions::default());
        machine.on_open(true);
        machine.on_event(SpeechStreamEvent::Ready, true);

        let a1 = machine.feed_f32(&[0.1], true);
        let a2 = machine.feed_f32(&[0.2], true);
        let a3 = machine.feed_f32(&[0.3], true);
        assert_eq!(a1.iter().filter(|a| matches!(a, VoiceStreamAction::SendBinary(_))).count(), 1);
        assert_eq!(a2.iter().filter(|a| matches!(a, VoiceStreamAction::SendBinary(_))).count(), 1);
        assert_eq!(a3.iter().filter(|a| matches!(a, VoiceStreamAction::SendBinary(_))).count(), 0);

        let can_send = if action == "cancel" {
            let cancel_actions = machine.cancel();
            assert!(cancel_actions.contains(&VoiceStreamAction::Failed {
                message: "Voice transcription was cancelled.".into(),
                notify: false,
            }));
            true
        } else {
            false
        };

        let late = machine.on_event(
            SpeechStreamEvent::Update {
                revision: 1,
                text: Some(SpeechStreamText { committed: "late".into(), tentative: String::new() }),
            },
            can_send,
        );
        assert!(!late.iter().any(|a| matches!(a, VoiceStreamAction::Text(_))));
        assert!(!late.iter().any(|a| matches!(a, VoiceStreamAction::SendBinary(_))));
        assert!(machine.is_closed());
        let err = machine.final_result().unwrap().as_ref().unwrap_err();
        if action == "cancel" {
            assert!(err.contains("cancelled"));
        } else {
            assert!(err.contains("revoked"));
        }
    }
}

#[test]
fn stream_fails_on_disconnect_and_times_out_stalled_inference() {
    let mut first = VoiceStreamMachine::new(SpeechTranscriptionOptions::default());
    first.on_open(true);
    first.on_event(SpeechStreamEvent::Ready, true);
    let closed = first.on_socket_closed();
    assert!(closed.iter().any(|a| matches!(
        a,
        VoiceStreamAction::Failed { message, notify: true } if message.contains("connection closed")
    )));
    assert!(first.is_closed());

    let mut second = VoiceStreamMachine::new(SpeechTranscriptionOptions::default());
    second.on_open(true);
    second.on_event(SpeechStreamEvent::Ready, true);
    let feed = second.feed_f32(&[0.1], true);
    assert!(feed.contains(&VoiceStreamAction::ArmTimeout { timeout_ms: STREAM_RESPONSE_TIMEOUT_MS }));
    let timed_out = second.on_timeout();
    assert!(timed_out.iter().any(|a| matches!(
        a,
        VoiceStreamAction::Failed { message, notify: true } if message.contains("stopped responding")
    )));
    assert!(timed_out.contains(&VoiceStreamAction::CloseSocket));
    assert!(second.is_closed());
}

// --- controller.test.ts resolveTranscriptCommit parity ---

#[test]
fn resolve_transcript_commit_preserves_script_boundaries_with_auto_language() {
    let cases = [
        ("Hello", "world", "Hello world"),
        ("Hello", "世界", "Hello世界"),
        ("Hello", "𝟚", "Hello 𝟚"),
        ("你好", "世界", "你好世界"),
    ];
    for (text, spoken, expected) in cases {
        let len = text.encode_utf16().count();
        let captured = draft("environment:thread", text, len, len);
        assert_eq!(
            resolve_transcript_commit(&captured, Some(&captured), spoken, "auto"),
            TranscriptCommitResult::Commit {
                text: expected.to_owned(),
                selection: VoiceSelection {
                    start: expected.encode_utf16().count(),
                    end: expected.encode_utf16().count(),
                },
            }
        );
    }
}

#[test]
fn resolve_transcript_commit_inserts_with_recorded_language_word_spacing() {
    let cases = [
        ("Korean", "코드를", 3, "검토해 주세요", "ko_KR", "코드를 검토해 주세요"),
        ("supplementary letter", "𞤀", 2, "𞤦", "ff-Adlm", "𞤀 𞤦"),
        ("trailing combining mark", "नमस्ते", 6, "दुनिया", "hi-IN", "नमस्ते दुनिया"),
        ("non-ASCII word after caret", "Änderungen", 0, "Weitere", "de_DE", "Weitere Änderungen"),
        ("spaceless Japanese", "修正キャッシュ", 7, "テストも", "ja-JP", "修正キャッシュテストも"),
        ("spaceless Thai", "โลก", 0, "สวัสดี", "th-TH", "สวัสดีโลก"),
    ];
    for (label, text, caret, spoken, locale, expected) in cases {
        let captured = draft("environment:thread", text, caret, caret);
        match resolve_transcript_commit(&captured, Some(&captured), spoken, locale) {
            TranscriptCommitResult::Commit { text: actual, .. } => {
                assert_eq!(actual, expected, "case: {label}");
            }
            other => panic!("expected commit for {label}, got {other:?}"),
        }
    }
}

#[test]
fn resolve_transcript_commit_replaces_utf16_selection_around_emoji_and_tokens() {
    let text = "Fix 🧪 then $review please";
    let token_start_u16 = text[..text.find("$review").unwrap()].encode_utf16().count();
    let token_end_u16 = token_start_u16 + "$review".encode_utf16().count();
    let captured = draft("environment:thread", text, token_start_u16, token_end_u16);
    let spoken = "use the mobile skill";
    let expected_caret = token_start_u16 + spoken.encode_utf16().count();
    assert_eq!(
        resolve_transcript_commit(&captured, Some(&captured), spoken, "en-US"),
        TranscriptCommitResult::Commit {
            text: "Fix 🧪 then use the mobile skill please".to_owned(),
            selection: VoiceSelection { start: expected_caret, end: expected_caret },
        }
    );
}

#[test]
fn resolve_transcript_commit_keeps_text_inserted_while_recording() {
    let cases = [
        (6, 11, "hello [cite]world", "hello [cite]world new text"),
        (6, 11, "hello wo[cite]rld", "hello wo[cite]rld new text"),
        (6, 6, "hello [cite] world", "hello [cite] new text world"),
        (0, 5, "hello there hello world", "hello there hello new text world"),
        (0, 5, "hello hello world", "hello hello new text world"),
    ];
    for (start, end, current_text, expected) in cases {
        let captured = draft("environment:thread", "hello world", start, end);
        let current = draft("environment:thread", current_text, 0, 0);
        match resolve_transcript_commit(&captured, Some(&current), "new text", "en-US") {
            TranscriptCommitResult::Commit { text, .. } => assert_eq!(text, expected),
            other => panic!("expected commit, got {other:?}"),
        }
    }
}

#[test]
fn resolve_transcript_commit_keeps_multiple_citations_and_surrogate_pairs_whole() {
    let captured = draft("environment:thread", "[todo", 0, 1);
    let current_text = "[A](t3-citation://v1/a)[todo[B](t3-citation://v1/b)";
    let current = draft("environment:thread", current_text, 0, 0);
    match resolve_transcript_commit(&captured, Some(&current), "spoken", "en-US") {
        TranscriptCommitResult::Commit { text, .. } => {
            assert_eq!(text, format!("{current_text} spoken"));
        }
        other => panic!("expected commit, got {other:?}"),
    }

    let bell = draft("environment:thread", "🔔", 0, 0);
    let thinking = draft("environment:thread", "🤔", 0, 0);
    match resolve_transcript_commit(&bell, Some(&thinking), "hello", "en-US") {
        TranscriptCommitResult::Commit { text, .. } => assert_eq!(text, "🤔hello"),
        other => panic!("expected commit, got {other:?}"),
    }
}

#[test]
fn resolve_transcript_commit_rejects_missing_or_different_owner_draft() {
    let captured = default_draft();
    assert_eq!(resolve_transcript_commit(&captured, None, "text", "en-US"), TranscriptCommitResult::Stale);
    let other = draft("other", "hello world", 6, 11);
    assert_eq!(
        resolve_transcript_commit(&captured, Some(&other), "text", "en-US"),
        TranscriptCommitResult::Stale
    );
    assert_eq!(
        resolve_transcript_commit(&captured, Some(&captured), "   ", "en-US"),
        TranscriptCommitResult::Empty
    );
}

// --- controller state machine parity ---

#[test]
fn controller_retries_failed_batch_audio_without_recording_again_then_deletes() {
    let mut ctrl = VoiceInputControllerMachine::new(true, true);
    let initial = default_draft();
    ctrl.start(Some(initial.clone()));
    let tok1 = ctrl.operation_token();
    ctrl.on_permission(tok1, true, true);
    ctrl.on_transcriber_prepared(tok1, Ok(("en-US".into(), false, false)));
    ctrl.on_recorder_prepared(tok1, Ok(Some("file:///voice.m4a".into())), Some(initial), false);
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Recording);

    ctrl.stop();
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Transcribing);
    let fail_effects = ctrl.on_transcription_finished(
        tok1,
        Err(VoiceTranscriptionError::new(VoiceTranscriptionErrorCode::TranscriptionFailed, "inference failed")),
        false,
    );
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Error);
    assert!(!fail_effects.iter().any(|e| matches!(e, VoiceControllerEffect::DeleteRecording(_))));

    let updated = draft("environment:thread", "updated ", 8, 8);
    let retry_start = ctrl.start(Some(updated.clone()));
    let tok2 = ctrl.operation_token();
    assert!(retry_start.contains(&VoiceControllerEffect::FinishTranscription {
        operation_token: tok2,
        uri: Some("file:///voice.m4a".into()),
    }));
    ctrl.on_transcription_finished(tok2, Ok("recovered text".into()), false);
    let commit_effects = ctrl.finalize_commit(tok2, Some(&updated));
    assert!(commit_effects.contains(&VoiceControllerEffect::CommitDraft {
        text: "updated recovered text".into(),
        selection: VoiceSelection { start: 22, end: 22 },
    }));
    assert!(commit_effects.contains(&VoiceControllerEffect::DeleteRecording("file:///voice.m4a".into())));
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Idle);
}

#[test]
fn streaming_without_live_start_emits_start_and_finish_without_hanging() {
    let mut ctrl = VoiceInputControllerMachine::new(true, true);
    let draft = default_draft();
    ctrl.start(Some(draft.clone()));
    let tok = ctrl.operation_token();
    ctrl.on_permission(tok, true, true);
    ctrl.on_transcriber_prepared(tok, Ok(("en".into(), true, false)));
    let rec_effects = ctrl.on_recorder_prepared(
        tok,
        Ok(Some("memory://voice/1".into())),
        Some(draft),
        false,
    );
    assert!(rec_effects.contains(&VoiceControllerEffect::StartLiveTranscription {
        uri: "memory://voice/1".into(),
        operation_token: tok,
    }));
    let stop_effects = ctrl.stop();
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Transcribing);
    assert!(stop_effects.contains(&VoiceControllerEffect::FinishTranscription {
        operation_token: tok,
        uri: Some("memory://voice/1".into()),
    }));
}

#[test]
fn controller_deletes_retained_audio_when_cancelled_and_handles_skip_post_processing() {
    let mut ctrl = VoiceInputControllerMachine::new(true, true);
    let initial = default_draft();
    ctrl.start(Some(initial.clone()));
    let tok = ctrl.operation_token();
    ctrl.on_permission(tok, true, true);
    ctrl.on_transcriber_prepared(tok, Ok(("en-US".into(), false, false)));
    ctrl.on_recorder_prepared(tok, Ok(Some("file:///voice.m4a".into())), Some(initial.clone()), false);
    ctrl.stop();
    ctrl.on_transcription_finished(
        tok,
        Err(VoiceTranscriptionError::new(VoiceTranscriptionErrorCode::TranscriptionFailed, "failed")),
        false,
    );
    let cancel_effects = ctrl.cancel();
    assert!(cancel_effects.contains(&VoiceControllerEffect::DeleteRecording("file:///voice.m4a".into())));
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Idle);

    // Post-processing skip commits raw transcript.
    ctrl.start(Some(initial.clone()));
    let tok2 = ctrl.operation_token();
    ctrl.on_permission(tok2, true, true);
    ctrl.on_transcriber_prepared(tok2, Ok(("en-US".into(), false, false)));
    ctrl.on_recorder_prepared(tok2, Ok(Some("file:///voice2.m4a".into())), Some(initial.clone()), false);
    ctrl.stop();
    ctrl.on_transcription_finished(tok2, Ok("new text".into()), true);
    assert_eq!(ctrl.state().phase, VoiceInputPhase::PostProcessing);
    let skip_effects = ctrl.skip_post_processing();
    assert!(skip_effects.contains(&VoiceControllerEffect::AbortPostProcessing { operation_token: tok2 }));
    let pp_err = ctrl.on_post_processing_finished(tok2, Err("aborted".into()));
    assert_eq!(pp_err, vec![VoiceControllerEffect::ReadyToCommit { operation_token: tok2 }]);
    let committed = ctrl.finalize_commit(tok2, Some(&initial));
    assert!(committed.contains(&VoiceControllerEffect::CommitDraft {
        text: "hello new text".into(),
        selection: VoiceSelection { start: 14, end: 14 },
    }));
}

#[test]
fn controller_permission_denied_and_background_and_owner_change() {
    let mut denied = VoiceInputControllerMachine::new(false, true);
    let initial = default_draft();
    denied.start(Some(initial.clone()));
    let tok = denied.operation_token();
    denied.on_permission(tok, false, false);
    assert_eq!(denied.state().phase, VoiceInputPhase::Error);
    assert_eq!(denied.state().error_action, Some(VoiceErrorAction::Settings));

    denied.owner_changed();
    assert_eq!(*denied.state(), VoiceInputState::idle());

    let mut prep = VoiceInputControllerMachine::new(false, true);
    prep.start(Some(initial));
    let bg = prep.app_moved_to_background();
    assert!(bg.iter().any(|e| matches!(e, VoiceControllerEffect::AbortOperation { .. })));
    assert_eq!(prep.state().phase, VoiceInputPhase::Error);
    assert!(prep.state().error.as_deref().unwrap().contains("background"));
}

fn start_live_recording(
    enable_post_processing: bool,
    draft_snap: &VoiceDraftSnapshot,
) -> (VoiceInputControllerMachine, u64) {
    let mut ctrl = VoiceInputControllerMachine::new(true, true);
    ctrl.set_enable_post_processing(enable_post_processing);
    ctrl.start(Some(draft_snap.clone()));
    let tok = ctrl.operation_token();
    ctrl.on_permission(tok, true, true);
    ctrl.on_transcriber_prepared(tok, Ok(("en-US".into(), true, true)));
    let rec_effects = ctrl.on_recorder_prepared(
        tok,
        Ok(Some("file:///live.m4a".into())),
        Some(draft_snap.clone()),
        false,
    );
    assert!(rec_effects.contains(&VoiceControllerEffect::StartLiveTranscription {
        uri: "file:///live.m4a".into(),
        operation_token: tok,
    }));
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Recording);
    (ctrl, tok)
}

#[test]
fn controller_live_transcription_cached_and_in_flight_and_errors() {
    let initial = default_draft();

    // 1. Live transcript cached before stop(), post-processing disabled -> emits ReadyToCommit.
    let (mut ctrl, tok) = start_live_recording(false, &initial);
    ctrl.on_live_transcription_result(tok, Ok(Some("cached live".into())));
    let stop_effects = ctrl.stop();
    assert!(!stop_effects.iter().any(|e| matches!(e, VoiceControllerEffect::FinishTranscription { .. })));
    assert!(stop_effects.contains(&VoiceControllerEffect::ReadyToCommit { operation_token: tok }));
    let commit_effects = ctrl.finalize_commit(tok, Some(&initial));
    assert!(commit_effects.contains(&VoiceControllerEffect::CommitDraft {
        text: "hello cached live".into(),
        selection: VoiceSelection { start: 17, end: 17 },
    }));
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Idle);

    // 2. Live transcript cached before stop(), post-processing enabled -> transitions to PostProcessing.
    let (mut ctrl, tok) = start_live_recording(true, &initial);
    ctrl.on_live_transcription_result(tok, Ok(Some("raw live".into())));
    let stop_effects = ctrl.stop();
    assert_eq!(ctrl.state().phase, VoiceInputPhase::PostProcessing);
    assert!(stop_effects.contains(&VoiceControllerEffect::StartPostProcessing {
        operation_token: tok,
        transcript: "raw live".into(),
        draft: initial.clone(),
    }));
    let pp_effects = ctrl.on_post_processing_finished(tok, Ok("cleaned live".into()));
    assert!(pp_effects.contains(&VoiceControllerEffect::ReadyToCommit { operation_token: tok }));
    let commit_effects = ctrl.finalize_commit(tok, Some(&initial));
    assert!(commit_effects.contains(&VoiceControllerEffect::CommitDraft {
        text: "hello cleaned live".into(),
        selection: VoiceSelection { start: 18, end: 18 },
    }));

    // 3. Live transcript settles during Transcribing (after stop()); and Ok(None) falls back to FinishTranscription.
    let (mut ctrl, tok) = start_live_recording(true, &initial);
    let stop_effects = ctrl.stop();
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Transcribing);
    assert!(!stop_effects.iter().any(|e| matches!(
        e,
        VoiceControllerEffect::FinishTranscription { .. }
            | VoiceControllerEffect::ReadyToCommit { .. }
            | VoiceControllerEffect::StartPostProcessing { .. }
    )));
    let settled = ctrl.on_live_transcription_result(tok, Ok(Some("late live".into())));
    assert_eq!(ctrl.state().phase, VoiceInputPhase::PostProcessing);
    assert!(settled.contains(&VoiceControllerEffect::StartPostProcessing {
        operation_token: tok,
        transcript: "late live".into(),
        draft: initial.clone(),
    }));

    let (mut ctrl, tok) = start_live_recording(false, &initial);
    ctrl.stop();
    let fallback = ctrl.on_live_transcription_result(tok, Ok(None));
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Transcribing);
    assert!(fallback.contains(&VoiceControllerEffect::FinishTranscription {
        operation_token: tok,
        uri: Some("file:///live.m4a".into()),
    }));

    // 4. Live error before stop() (Recording) and after stop() (Transcribing).
    let (mut ctrl, tok) = start_live_recording(false, &initial);
    let err = VoiceTranscriptionError::new(
        VoiceTranscriptionErrorCode::TranscriptionFailed,
        "live dropped early",
    );
    let early_err = ctrl.on_live_transcription_result(tok, Err(err));
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Error);
    assert_eq!(ctrl.state().error.as_deref(), Some("live dropped early"));
    assert!(early_err.contains(&VoiceControllerEffect::AbortOperation { operation_token: tok }));
    assert!(early_err.contains(&VoiceControllerEffect::DeleteRecording("file:///live.m4a".into())));

    let (mut ctrl, tok) = start_live_recording(false, &initial);
    ctrl.stop();
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Transcribing);
    let err_late = VoiceTranscriptionError::new(
        VoiceTranscriptionErrorCode::TranscriptionFailed,
        "live failed while transcribing",
    );
    let late_err = ctrl.on_live_transcription_result(tok, Err(err_late));
    assert_eq!(ctrl.state().phase, VoiceInputPhase::Error);
    assert_eq!(ctrl.state().error.as_deref(), Some("live failed while transcribing"));
    assert_eq!(ctrl.state().error_action, Some(VoiceErrorAction::Retry));
    assert!(late_err.contains(&VoiceControllerEffect::DeleteRecording("file:///live.m4a".into())));
}

// --- pcmWorklet.test.ts parity ---

#[test]
fn pcm_capture_meters_microphone_only_like_transcription_without_emitting_pcm() {
    let mut transcription = PcmCaptureProcessor::new(48_000, false).unwrap();
    let mut microphone = PcmCaptureProcessor::new(48_000, true).unwrap();
    transcription.start();
    microphone.start();

    let mut t_events = Vec::new();
    let mut m_events = Vec::new();
    let levels = |events: &[PcmCaptureEvent]| -> Vec<f32> {
        events
            .iter()
            .filter_map(|e| match e {
                PcmCaptureEvent::Level(l) => Some(*l),
                _ => None,
            })
            .collect()
    };

    let s1 = vec![0.1_f32; 2_399];
    t_events.extend(transcription.process(&[&s1]));
    m_events.extend(microphone.process(&[&s1]));
    assert_eq!(levels(&m_events).len(), 0);

    t_events.extend(transcription.process(&[&[0.1_f32]]));
    m_events.extend(microphone.process(&[&[0.1_f32]]));
    assert_eq!(levels(&m_events).len(), 1);

    let s2 = vec![0.25_f32; 48_000];
    t_events.extend(transcription.process(&[&s2]));
    m_events.extend(microphone.process(&[&s2]));

    let s3 = vec![0.0_f32; 2_400];
    t_events.extend(transcription.process(&[&s3]));
    m_events.extend(microphone.process(&[&s3]));

    let measured = levels(&m_events);
    assert_eq!(measured, levels(&t_events));
    assert_eq!(measured.len(), 22);
    assert!(measured[0] > 0.0);
    assert!(measured[0] < 0.4);
    let last = *measured.last().unwrap();
    let second_last = measured[measured.len() - 2];
    assert!(last > 0.0);
    assert!(last < second_last);

    assert!(t_events.iter().any(|e| matches!(e, PcmCaptureEvent::PcmChunk(_))));
    assert!(m_events.iter().all(|e| matches!(e, PcmCaptureEvent::Level(_))));

    let stop_events = microphone.stop();
    assert_eq!(stop_events.last(), Some(&PcmCaptureEvent::Stopped));
}

#[test]
fn pcm_resampler_and_vad_framing_work_across_48khz_and_i16() {
    assert!(PcmResampler::new(8_000).is_err());

    let mut resampler = PcmResampler::new(48_000).unwrap();
    let input = vec![0.5_f32; 4_800];
    let out_f32 = resampler.push(&input);
    assert_eq!(out_f32.len(), 1_600);
    let flushed = resampler.flush();
    assert!(!flushed.is_empty());

    let i16_samples = f32_slice_to_i16(&out_f32);
    assert!(i16_samples[100] > 15_000 && i16_samples[100] < 17_000);
    let roundtrip = i16_slice_to_f32(&i16_samples);
    assert!((roundtrip[100] - 0.5).abs() < 0.02);

    let bytes = f32_slice_to_le_bytes(&out_f32);
    assert_eq!(le_bytes_to_f32_vec(&bytes).unwrap(), out_f32);

    let mut vad = VadFramingState::new(VadConfig::default());
    // 10 frames silence, 10 frames speech, 25 frames silence
    vad.push(&vec![0.0_f32; 512 * 10]);
    vad.push(&vec![0.2_f32; 512 * 10]);
    vad.push(&vec![0.0_f32; 512 * 25]);
    vad.flush();
    let kept = vad.kept_pcm();
    assert!(kept.len() >= 512 * 10);
    assert!(kept.len() < 512 * 45);
}

// --- contracts, vocabulary, consent, languages, microphone order, shortcuts ---

#[test]
fn contracts_cloud_speech_error_and_pcm_framing() {
    let err = CloudSpeechError {
        provider: CloudSpeechProvider::Xai,
        reason: CloudSpeechErrorReason::RejectedKey,
    };
    assert_eq!(
        err.message(),
        "xAI rejected this API key. Check its permissions and billing in Settings → Voice."
    );
    let json = err.to_tagged_json();
    assert_eq!(CloudSpeechError::from_tagged_value(&json), Some(err));

    assert!(validate_cloud_speech_api_key(" sk-test-123 ").is_ok());
    assert!(validate_cloud_speech_api_key("https://example.com").is_err());
    assert!(validate_cloud_speech_model_id("openai/whisper-1"));
    assert!(!validate_cloud_speech_model_id("openrouter/auto"));

    let options = SpeechTranscriptionOptions::default();
    let encoded = encode_speech_pcm_request(&[1, 2, 3, 4], &options).unwrap();
    let (decoded_opts, decoded_pcm) = decode_speech_pcm_request(&encoded).unwrap();
    assert_eq!(decoded_opts, options);
    assert_eq!(decoded_pcm, &[1, 2, 3, 4]);
    assert!(decode_speech_pcm_request(&[0, 0, 0, 0]).is_err());
}

#[test]
fn custom_words_validation_and_resolution() {
    let personal = vec![
        SpeechCustomWordEntry { term: "T3 Code".into(), aliases: vec!["tee three".into()] },
    ];
    let project = vec![
        SpeechCustomWordEntry { term: "t3 code".into(), aliases: vec!["t3".into()] },
    ];
    let resolved = resolve_speech_vocabulary(&personal, &project, &["  gpui  crate ", "t3 code", "t3", "gpui crate"]);
    assert_eq!(resolved.speech_custom_words.len(), 1);
    assert_eq!(resolved.speech_custom_words[0].term, "t3 code");
    assert_eq!(resolved.speech_custom_words[0].aliases, vec!["t3"]);
    assert_eq!(resolved.speech_hints, vec!["gpui crate"]);

    assert!(validate_speech_custom_words(&resolved.speech_custom_words).is_ok());

    let many_hints: Vec<&str> = (0..12).map(|i| Box::leak(format!("hint-{i}").into_boxed_str()) as &str).collect();
    let capped = resolve_speech_vocabulary(&[], &[], &many_hints);
    assert_eq!(capped.speech_hints.len(), SPEECH_MAX_HINTS);
    let duplicate = vec![
        SpeechCustomWordEntry { term: "foo".into(), aliases: vec!["FOO".into()] },
    ];
    assert!(validate_speech_custom_words(&duplicate).is_err());
}

#[test]
fn speech_status_wire_fallback_and_benchmark_wire_formats() {
    let legacy_status = serde_json::json!({
        "supported": true,
        "state": "ready",
        "modelId": "whisper-small",
        "model": "Whisper Small",
        "size": 500,
        "supportsStreaming": true,
        "supportsTranslation": true,
        "acceleration": "auto",
        "modelUnloadTimeout": "min_5",
        "gpuDevices": []
    });
    let parsed: EnvironmentSpeechStatus = serde_json::from_value(legacy_status).unwrap();
    assert!(matches!(
        parsed,
        EnvironmentSpeechStatus::Supported { benchmark_tier: SpeechBenchmarkTier::Cpu, .. }
    ));

    let bench_scalar: SpeechBenchmark = serde_json::from_value(serde_json::json!({
        "accuracy": { "en": { "grade": "A", "errorPercent": 2.1 } },
        "secondsPer30s": 1.5,
        "cpuSecondsPer30s": 4.2
    }))
    .unwrap();
    assert_eq!(bench_scalar.seconds_per_30s.accelerated, Some(1.5));
    assert_eq!(bench_scalar.seconds_per_30s.cpu, Some(Some(4.2)));

    // Missing vs explicit null distinguishes legacy servers (None) from unmeasured current catalogs (Some(None)).
    let bench_missing_cpu: SpeechBenchmark = serde_json::from_value(serde_json::json!({
        "accuracy": { "en": { "grade": "A", "errorPercent": 2.1 } },
        "secondsPer30s": 1.5
    }))
    .unwrap();
    assert_eq!(bench_missing_cpu.seconds_per_30s.cpu, None);

    let bench_null_cpu: SpeechBenchmark = serde_json::from_value(serde_json::json!({
        "accuracy": { "en": { "grade": "A", "errorPercent": 2.1 } },
        "secondsPer30s": 1.5,
        "cpuSecondsPer30s": null
    }))
    .unwrap();
    assert_eq!(bench_null_cpu.seconds_per_30s.cpu, Some(None));

    let times_missing: SpeechBenchmarkTimes =
        serde_json::from_value(serde_json::json!({ "accelerated": 1.2 })).unwrap();
    assert_eq!(times_missing.cpu, None);

    let times_null: SpeechBenchmarkTimes =
        serde_json::from_value(serde_json::json!({ "cpu": null, "accelerated": 1.2 })).unwrap();
    assert_eq!(times_null.cpu, Some(None));

    // Multi-byte UTF-8 model IDs must never panic on byte index 11.
    assert!(!validate_cloud_speech_model_id("éééééé"));
    assert!(!validate_cloud_speech_model_id("éééééé/model"));
    assert!(!validate_cloud_speech_model_id("openrouteré/a"));
    assert!(!validate_cloud_speech_model_id("OpenRouter/whisper-1"));
    assert!(validate_cloud_speech_model_id("openai/whisper-1"));
}

#[test]
fn consent_languages_microphone_order_and_shortcuts() {
    let mut consent = VoiceConsentState::new();
    assert!(consent.offer_first_run_setup());
    consent.choose_audio_destination(
        Some("g1".into()),
        Some(VoiceTranscriptionDestination::environment("env-1")),
    );
    assert!(consent.bind_audio_grant("g1").is_some());
    let pending_id = consent.begin_pending_cloud_choice(
        "g2",
        VoiceTranscriptionDestination::openrouter("env-2", "openai/whisper-1"),
    );
    // Missing marker preserves previous grant until acknowledged.
    assert_eq!(consent.active_grant().unwrap().id, "g1");
    consent.acknowledge_pending_cloud_choice(&pending_id);
    assert_eq!(consent.active_grant().unwrap().id, "g2");

    assert_eq!(
        preferred_speech_languages(&[], Some("de_DE"), &["en-US"]),
        vec!["de", "en"]
    );

    let seeded = microphone_order(&[], "mic-legacy");
    assert_eq!(
        seeded,
        vec![
            VoiceMicrophoneEntry::Device { device_id: "mic-legacy".into(), label: String::new() },
            VoiceMicrophoneEntry::Default,
        ]
    );
    let remembered = remember_microphones(
        &[VoiceMicrophoneEntry::Default],
        &[
            AudioDeviceInfo { device_id: "default".into(), label: "System Default".into() },
            AudioDeviceInfo { device_id: "usb-1".into(), label: "USB Mic".into() },
        ],
    );
    assert_eq!(remembered.len(), 2);
    let preferred = prefer_microphone(
        &remembered,
        VoiceMicrophoneEntry::Device { device_id: "usb-1".into(), label: "USB Mic".into() },
    );
    assert_eq!(
        preferred_microphone_id(
            &preferred,
            &[AudioDeviceInfo { device_id: "usb-1".into(), label: "USB Mic".into() }]
        ),
        "usb-1"
    );
    assert_eq!(forget_microphone(&preferred, "default").len(), 2);
    assert_eq!(forget_microphone(&preferred, "device:usb-1").len(), 1);

    assert!(!should_finish_dictation_on_release(VoiceShortcutMode::Auto, 150));
    assert!(should_finish_dictation_on_release(VoiceShortcutMode::Auto, 300));
    assert!(should_finish_dictation_on_release(VoiceShortcutMode::Hold, 50));
    assert!(!should_finish_dictation_on_release(VoiceShortcutMode::Toggle, 5000));

    let mut shortcut = DictationShortcutState::new();
    assert_eq!(
        shortcut.on_dictation_keydown(DictationKeydown {
            phase: VoiceInputPhase::Idle,
            available: true,
            disabled: false,
            repeat: false,
            key_code: "KeyD",
            now_ms: 1000,
            mode: VoiceShortcutMode::Auto,
        }),
        DictationShortcutAction::Start
    );
    assert_eq!(
        shortcut.on_keyup_or_blur(Some("KeyD"), 1400),
        DictationShortcutAction::Stop
    );
}

#[test]
fn saved_audio_choice_serde_and_presentation_helpers() {
    assert_eq!(serde_json::from_str::<SavedAudioChoice>("null").unwrap(), SavedAudioChoice::Unset);
    assert_eq!(serde_json::to_value(SavedAudioChoice::Unset).unwrap(), serde_json::json!(null));
    assert_eq!(serde_json::from_str::<SavedAudioChoice>(r#""off""#).unwrap(), SavedAudioChoice::Off);
    assert_eq!(serde_json::to_value(SavedAudioChoice::Off).unwrap(), serde_json::json!("off"));

    let granted = SavedAudioChoice::Granted(AudioGrant {
        id: "g1".into(),
        destination: VoiceTranscriptionDestination::openrouter("env-1", "openai/whisper-1"),
    });
    let raw = serde_json::to_string(&granted).unwrap();
    assert_eq!(serde_json::from_str::<SavedAudioChoice>(&raw).unwrap(), granted);

    assert_eq!(speech_language_label("auto"), "Auto-detect");
    assert_eq!(speech_language_label("en-US"), "English");
    assert_eq!(speech_language_label("nb_NO"), "Norwegian");
    assert_eq!(speech_language_label("xx"), "xx");

    assert_eq!(format_elapsed_mm_ss(0), "0:00");
    assert_eq!(format_elapsed_mm_ss(65), "1:05");

    let mut state = VoiceInputState::default();
    assert_eq!(resolve_speech_presentation(&state, None), (None, false));
    state.phase = VoiceInputPhase::Preparing;
    assert_eq!(resolve_speech_presentation(&state, Some((3, 4))), (Some("Downloading model 75%".into()), true));
    state.phase = VoiceInputPhase::Recording;
    assert_eq!(resolve_speech_presentation(&state, None), (Some("Recording".into()), true));
    state.phase = VoiceInputPhase::Error;
    state.error = Some("The microphone disconnected.".into());
    assert_eq!(voice_error_label(state.error.as_deref()), "Mic disconnected");
    assert!(!is_retryable_transcription_error(state.error.as_deref()));
    assert!(is_retryable_transcription_error(Some("Could not transcribe this recording.")));
}

#[test]
fn speech_model_recommendations_and_modifier_release_stop() {
    // `pingdotgg/t3code#33336`: recommendations by language, speed, and accuracy.
    let make_model = |id: &str, name: &str, langs: &[&str], en_wer: f64, ja_wer: Option<f64>, acc_s: f64, cpu_s: f64, rec: bool| {
        let mut accuracy = std::collections::BTreeMap::new();
        accuracy.insert("en".to_owned(), SpeechBenchmarkAccuracyEntry { grade: "A".into(), error_percent: en_wer });
        if let Some(ja) = ja_wer {
            accuracy.insert("ja".to_owned(), SpeechBenchmarkAccuracyEntry { grade: "A".into(), error_percent: ja });
        }
        EnvironmentSpeechModel {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            size: 100_000_000.0,
            languages: langs.iter().map(|s| (*s).to_owned()).collect(),
            accuracy: 0.9,
            speed: 0.9,
            benchmark: Some(SpeechBenchmark {
                accuracy,
                seconds_per_30s: SpeechBenchmarkTimes { accelerated: Some(acc_s), cpu: Some(Some(cpu_s)) },
            }),
            recommended: rec,
            supports_streaming: true,
            supports_language_detection: true,
            supports_translation: false,
            active: false,
            state: EnvironmentSpeechModelState::Downloadable,
            downloaded: None,
            linked_from: None,
        }
    };
    let models = vec![
        make_model("fast-en", "Fast EN", &["en"], 7.0, None, 0.4, 1.2, false),
        make_model("balanced-multi", "Balanced Multi", &["en", "de", "fr", "es"], 3.2, None, 0.8, 3.5, true),
        make_model("accurate-multi", "Accurate Multi", &["en", "de", "fr", "es", "ja"], 2.1, Some(2.9), 8.0, 18.0, false),
    ];

    let recs = recommend_speech_models(&models, &["en".into()], Some(SpeechBenchmarkTier::Accelerated));
    assert!(recs.iter().any(|(_, role)| *role == SpeechRecommendationRole::Recommended));
    let picked_en = choose_setup_speech_model_for_tier(
        &models,
        "",
        "en",
        &[],
        Some(SpeechBenchmarkTier::Accelerated),
    );
    assert_eq!(picked_en.map(|(m, _, _)| m.id.as_str()), Some("balanced-multi"));
    // Multilingual query that includes Japanese picks a model supporting `ja`.
    let ja_pick = choose_setup_speech_model_for_tier(
        &models,
        "fast-en",
        "ja",
        &[],
        Some(SpeechBenchmarkTier::Accelerated),
    );
    assert_eq!(ja_pick.map(|(m, _, _)| m.id.as_str()), Some("accurate-multi"));

    // `pingdotgg/t3code#32648`: releasing a held shortcut modifier stops held dictation.
    let mut shortcut = DictationShortcutState::new();
    assert_eq!(
        shortcut.on_dictation_keydown_with_modifiers(
            DictationKeydown {
                phase: VoiceInputPhase::Idle,
                available: true,
                disabled: false,
                repeat: false,
                key_code: "KeyD",
                now_ms: 1000,
                mode: VoiceShortcutMode::Hold,
            },
            &["meta", "shift"],
        ),
        DictationShortcutAction::Start
    );
    // Still holding both modifiers -> no action.
    assert_eq!(
        shortcut.on_modifiers_changed(&["meta", "shift"], 1100),
        DictationShortcutAction::None
    );
    // Releasing `meta` ends the held dictation press.
    assert_eq!(
        shortcut.on_modifiers_changed(&["shift"], 1200),
        DictationShortcutAction::Stop
    );
}
