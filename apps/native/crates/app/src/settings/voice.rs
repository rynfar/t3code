use gpui::{Context, IntoElement};
use t3_core::client_settings::ClientSettingChange;
use t3_core::voice_input::{
    CloudSpeechProvider, SavedAudioChoice, VoiceShortcutMode, VoiceTranscriptionDestination,
};

use super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, section};
use super::{Block, SettingsView, store};
use crate::voice::{
    MicPermissionStatus, audio_platform, choose_voice_destination, list_voice_candidates,
    set_voice_cleanup_enabled, voice_audio_choice, voice_cleanup_granted,
};

impl SettingsView {
    pub(super) fn voice_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let state = self.state.clone();
        let candidates = list_voice_candidates(state.read(cx));
        let choice = voice_audio_choice(cx);
        let cleanup_granted = voice_cleanup_granted(cx);
        let doc = store::client(cx).clone();

        let mut dest_options = vec![SelectOption::new("off", "Off")];
        let mut dest_map: Vec<(String, VoiceTranscriptionDestination)> = Vec::new();
        for candidate in &candidates {
            if candidate.local_transcription {
                let key = format!("env:{}", candidate.environment_id);
                let label = format!("{} (on-device)", candidate.label);
                dest_options.push(SelectOption::new(key.clone(), label));
                dest_map.push((
                    key,
                    VoiceTranscriptionDestination::environment(&candidate.environment_id),
                ));
            }
            if candidate.cloud_transcription {
                for (provider, slug, title) in [
                    (CloudSpeechProvider::Openai, "openai", "OpenAI cloud"),
                    (CloudSpeechProvider::Xai, "xai", "xAI cloud"),
                    (CloudSpeechProvider::Openrouter, "openrouter", "OpenRouter cloud"),
                ] {
                    let key = format!("cloud:{}:{slug}", candidate.environment_id);
                    let label = format!("{} ({title})", candidate.label);
                    let model = match provider {
                        CloudSpeechProvider::Openai => "gpt-4o-mini-transcribe",
                        CloudSpeechProvider::Xai => "whisper-large-v3",
                        CloudSpeechProvider::Openrouter => "openai/whisper-1",
                    };
                    dest_options.push(SelectOption::new(key.clone(), label));
                    dest_map.push((
                        key,
                        VoiceTranscriptionDestination::Cloud {
                            environment_id: candidate.environment_id.clone(),
                            provider,
                            model: Some(model.to_owned()),
                        },
                    ));
                }
            }
        }

        let selected_dest = match &choice {
            SavedAudioChoice::Unset | SavedAudioChoice::Off => "off".to_owned(),
            SavedAudioChoice::Granted(grant) => match &grant.destination {
                VoiceTranscriptionDestination::Environment { environment_id } => {
                    format!("env:{environment_id}")
                }
                VoiceTranscriptionDestination::Cloud {
                    environment_id,
                    provider,
                    ..
                } => {
                    let slug = match provider {
                        CloudSpeechProvider::Openai => "openai",
                        CloudSpeechProvider::Xai => "xai",
                        CloudSpeechProvider::Openrouter => "openrouter",
                    };
                    format!("cloud:{environment_id}:{slug}")
                }
            },
        };

        let cleanup_enabled_now = cleanup_granted;
        let has_active_grant = matches!(choice, SavedAudioChoice::Granted(_));

        let perm_status = audio_platform(cx).permission_status();
        let perm_label = match perm_status {
            MicPermissionStatus::Granted => "Allowed",
            MicPermissionStatus::NotDetermined => "Will prompt on first recording",
            MicPermissionStatus::Denied | MicPermissionStatus::Restricted => {
                "Blocked in System Settings"
            }
        };

        let shortcut_value = match doc.voice_shortcut_mode() {
            VoiceShortcutMode::Auto => "auto",
            VoiceShortcutMode::Hold => "hold",
            VoiceShortcutMode::Toggle => "toggle",
        };
        let shortcut_state = state.clone();

        let destination_section = section(
            "Transcription destination",
            vec![
                Row::new("voice-transcription-destination", "Voice transcription")
                    .description(
                        "Choose which connected environment transcribes recorded audio.",
                    )
                    .highlighted(self.is_target("voice-transcription-destination"))
                    .control(
                        Select::new("voice-destination-select", dest_options)
                            .selected(selected_dest)
                            .width(240.)
                            .on_change(move |value, _, cx| {
                                if value.as_ref() == "off" {
                                    choose_voice_destination(None, false, cx);
                                } else if let Some((_, dest)) =
                                    dest_map.iter().find(|(key, _)| key == value.as_ref())
                                {
                                    choose_voice_destination(
                                        Some(dest.clone()),
                                        cleanup_enabled_now,
                                        cx,
                                    );
                                }
                            }),
                    )
                    .into_any_element(),
                Row::new("voice-cleanup-consent", "Clean up transcripts")
                    .description(
                        "Send dictation text and nearby prompt context to clean up punctuation and custom words.",
                    )
                    .control(
                        Switch::new("voice-cleanup-switch", cleanup_granted)
                            .disabled(!has_active_grant)
                            .on_change(|checked, _, cx| {
                                set_voice_cleanup_enabled(checked, cx);
                            }),
                    )
                    .into_any_element(),
            ],
            cx,
        );
        let input_section = section(
            "Microphone & shortcut",
            vec![
                Row::new("voice-microphone-permission", "Microphone access")
                    .description(perm_label)
                    .control(
                        Button::new(
                            "voice-open-mic-settings",
                            ButtonVariant::Outline,
                            ButtonSize::Sm,
                        )
                        .label("System Settings")
                        .on_click(|_, _, cx| {
                            audio_platform(cx).open_microphone_settings();
                        }),
                    )
                    .into_any_element(),
                Row::new("voice-shortcut-mode", "Dictation shortcut mode")
                    .description("Hold to dictate transiently, or tap to start and stop.")
                    .control(
                        Select::new(
                            "voice-shortcut-select",
                            vec![
                                SelectOption::new("auto", "Auto (tap or hold)"),
                                SelectOption::new("hold", "Hold to dictate"),
                                SelectOption::new("toggle", "Press to toggle"),
                            ],
                        )
                        .selected(shortcut_value)
                        .width(200.)
                        .on_change(move |value, _, cx| {
                            let mode = match value.as_ref() {
                                "hold" => VoiceShortcutMode::Hold,
                                "toggle" => VoiceShortcutMode::Toggle,
                                _ => VoiceShortcutMode::Auto,
                            };
                            store::update_client(
                                &shortcut_state,
                                ClientSettingChange::VoiceShortcutMode(mode),
                                cx,
                            );
                        }),
                    )
                    .into_any_element(),
            ],
            cx,
        );
        vec![
            Block::new(
                &["voice-transcription-destination", "voice-cleanup-consent"],
                destination_section,
            ),
            Block::new(
                &["voice-microphone-permission", "voice-shortcut-mode"],
                input_section,
            ),
        ]
    }
}
