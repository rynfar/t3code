use super::*;
use crate::composer::{FollowUpBehavior, SendShortcut};

#[test]
fn missing_or_corrupt_files_read_as_contract_defaults() {
    for text in ["", "not json", "[1, 2]", "null"] {
        let doc = ClientSettingsDoc::from_json(text);
        assert_eq!(doc.notification_mode(), NotificationMode::Off);
        assert!(!doc.in_app_notifications());
        assert_eq!(doc.project_grouping(), ProjectGroupingMode::Repository);
        assert_eq!(doc.project_sort_order(), ProjectSortOrder::UpdatedAt);
        assert!(doc.working_shelf());
        assert!(!doc.boring_sidebar_enabled());
        assert!(!doc.boring_sidebar_multiline_rows());
        assert!(!doc.confirm_thread_unpin());
        assert!(!doc.confirm_thread_archive());
        assert!(doc.confirm_thread_delete());
        assert_eq!(doc.confirm_quit(), QuitConfirmationMode::Hold);
        assert_eq!(doc.timestamp_format(), TimestampFormat::Locale);
        assert_eq!(doc.chat_width(), ChatWidth::Comfortable);
        assert_eq!(doc.interface_font_size(), 16);
        assert!(doc.composer_rich_text());
        assert_eq!(doc.theme(), ThemePreference::Default(AppearanceMode::System));
        assert_eq!(doc.appearance_mode(), AppearanceMode::System);
    }
}

#[test]
fn invalid_values_fall_back_one_key_at_a_time() {
    let doc = ClientSettingsDoc::from_json(
        r#"{"notificationMode": "loud", "chatWidth": "wide", "fontSizeInterface": 42,
            "timestampFormat": 24, "confirmThreadDelete": false}"#,
    );
    assert_eq!(doc.notification_mode(), NotificationMode::Off);
    assert_eq!(doc.chat_width(), ChatWidth::Wide);
    assert_eq!(doc.interface_font_size(), 16);
    assert_eq!(doc.timestamp_format(), TimestampFormat::Locale);
    assert!(!doc.confirm_thread_delete());
}

#[test]
fn legacy_boolean_quit_confirmation_decodes_to_modes() {
    assert_eq!(ClientSettingsDoc::from_json(r#"{"confirmQuit": true}"#).confirm_quit(), QuitConfirmationMode::Hold);
    assert_eq!(ClientSettingsDoc::from_json(r#"{"confirmQuit": false}"#).confirm_quit(), QuitConfirmationMode::Direct);
    assert_eq!(
        ClientSettingsDoc::from_json(r#"{"confirmQuit": "double-click"}"#).confirm_quit(),
        QuitConfirmationMode::DoubleClick
    );
}

#[test]
fn edits_keep_unknown_keys_and_round_trip() {
    let mut doc = ClientSettingsDoc::from_json(r#"{"futureSetting": {"nested": [1]}, "chatWidth": "full"}"#);
    assert!(doc.apply(&ClientSettingChange::ChatWidth(ChatWidth::Wide)));
    assert!(!doc.apply(&ClientSettingChange::ChatWidth(ChatWidth::Wide)));
    assert!(doc.apply(&ClientSettingChange::InterfaceFontSize(99)));
    assert!(!doc.onboarding_completed());
    assert!(doc.apply(&ClientSettingChange::OnboardingCompletedAt("2026-11-02T00:00:00.000Z".into())));
    let reread = ClientSettingsDoc::from_json(&doc.to_json());
    assert_eq!(reread, doc);
    assert_eq!(reread.chat_width(), ChatWidth::Wide);
    assert_eq!(reread.interface_font_size(), 20);
    assert!(reread.onboarding_completed());
    assert!(!ClientSettingsDoc::from_json(r#"{"onboardingCompletedAt": null}"#).onboarding_completed());
    assert!(reread.to_json().contains("futureSetting"));
}

#[test]
fn edits_save_as_patches_onto_the_latest_file() {
    let mut doc = ClientSettingsDoc::from_json(r#"{"chatWidth": "full"}"#);
    let patch = doc.apply_change(&ClientSettingChange::ChatWidth(ChatWidth::Wide));
    assert_eq!(Value::Object(patch.clone()), serde_json::json!({"chatWidth": "wide"}));
    assert!(doc.apply_change(&ClientSettingChange::ChatWidth(ChatWidth::Wide)).is_empty());

    // Another instance saved a key after this one loaded: the patch lands on its file.
    let latest = r#"{"chatWidth": "full", "sendShortcut": "mod-enter", "futureSetting": [1]}"#;
    let merged = merge_patch(Some(latest), &patch);
    assert!(!merged.backup);
    let value = |text: &str| serde_json::from_str::<Value>(text).unwrap();
    assert_eq!(value(&merged.text), serde_json::json!({"chatWidth": "wide", "sendShortcut": "mod-enter", "futureSetting": [1]}));
    for empty in [None, Some(""), Some("  \n")] {
        let merged = merge_patch(empty, &patch);
        assert!(!merged.backup);
        assert_eq!(value(&merged.text), serde_json::json!({"chatWidth": "wide"}));
    }
    // A file that is not a settings object is kept aside before it is replaced.
    for corrupt in ["not json", "[1, 2]", "null", "{\"chatWidth\": "] {
        let merged = merge_patch(Some(corrupt), &patch);
        assert!(merged.backup, "{corrupt}");
        assert_eq!(value(&merged.text), serde_json::json!({"chatWidth": "wide"}));
    }
}

#[test]
fn composer_view_follows_the_same_file() {
    let mut doc = ClientSettingsDoc::default();
    doc.apply(&ClientSettingChange::SendShortcut(SendShortcut::ModEnterMultiline));
    doc.apply(&ClientSettingChange::FollowUpBehavior(FollowUpBehavior::Queue));
    let composer = doc.composer();
    assert_eq!(composer.send_shortcut, SendShortcut::ModEnterMultiline);
    assert_eq!(composer.follow_up_behavior, FollowUpBehavior::Queue);
}

#[test]
fn choosing_a_theme_pins_the_current_appearance_mode() {
    let mut doc = ClientSettingsDoc::from_json(r#"{"t3code:theme": "dark"}"#);
    assert_eq!(doc.appearance_mode(), AppearanceMode::Dark);
    doc.apply(&ClientSettingChange::Theme(ThemePreference::Named("grove".into())));
    assert_eq!(doc.theme(), ThemePreference::Named("grove".into()));
    assert_eq!(doc.appearance_mode(), AppearanceMode::Dark);
    doc.apply(&ClientSettingChange::AppearanceMode(AppearanceMode::System));
    assert_eq!(doc.theme().named_id(), Some("grove"));
    assert_eq!(doc.appearance_mode(), AppearanceMode::System);
    doc.apply(&ClientSettingChange::ThemeHalves(std::collections::BTreeMap::from([("dark".into(),"iris".into())])));
    doc.apply(&ClientSettingChange::Theme(ThemePreference::Named("grove".into())));
    assert!(crate::themes::halves(&doc).is_empty());
}

#[test]
fn grouping_toggle_restores_the_last_repository_mode() {
    assert_eq!(project_grouping_from_toggle(false, ProjectGroupingMode::RepositoryPath), ProjectGroupingMode::Separate);
    assert_eq!(project_grouping_from_toggle(true, ProjectGroupingMode::RepositoryPath), ProjectGroupingMode::RepositoryPath);
    assert_eq!(project_grouping_from_toggle(true, ProjectGroupingMode::Separate), ProjectGroupingMode::Repository);
    let mut doc = ClientSettingsDoc::default();
    doc.apply(&ClientSettingChange::LastEnabledProjectGrouping(ProjectGroupingMode::RepositoryPath));
    assert_eq!(doc.last_enabled_project_grouping(), ProjectGroupingMode::RepositoryPath);
}

#[test]
fn diff_and_browser_settings_round_trip_and_default_per_key() {
    use super::{BrowserLinkTarget, BrowserSearchEngine, DiffColorScheme};
    use crate::diff::Layout;
    let doc = ClientSettingsDoc::from_json(
        r#"{"diffLayout":"future","diffIgnoreWhitespace":0,"diffFilesCollapsed":null,"diffColorScheme":"unknown","browserSearchEngine":"future","browserCustomSearchUrl":4,"browserLinkTarget":"future","pullRequestLinksOpenInApp":0}"#,
    );
    assert_eq!(doc.diff_layout(), Layout::Stacked);
    assert!(doc.diff_ignore_whitespace());
    assert!(doc.diff_files_collapsed());
    assert_eq!(doc.diff_color_scheme(), DiffColorScheme::RedGreen);
    assert_eq!(doc.browser_search_engine(), BrowserSearchEngine::DuckDuckGo);
    assert_eq!(doc.browser_custom_search_url(), "");
    assert_eq!(doc.browser_link_target(), BrowserLinkTarget::System);
    assert!(doc.pull_request_links_open_in_app());
    let legacy = serde_json::json!({ "diffLayout": "split" });
    let mut migrated = ClientSettingsDoc::default();
    assert!(migrated.migrate_diff_layout(&serde_json::json!({ "diffLayout": "future" })).is_empty());
    assert_eq!(serde_json::Value::Object(migrated.migrate_diff_layout(&legacy)), serde_json::json!({ "diffLayout": "split" }));
    assert_eq!(migrated.diff_layout(), Layout::Split);
    migrated.apply(&ClientSettingChange::DiffLayout(Layout::Stacked));
    assert!(migrated.migrate_diff_layout(&legacy).is_empty());
    assert_eq!(migrated.diff_layout(), Layout::Stacked);
    let mut doc = ClientSettingsDoc::from_json(r#"{"futureSetting":42}"#);
    for change in [
        ClientSettingChange::DiffLayout(Layout::Split),
        ClientSettingChange::DiffIgnoreWhitespace(false),
        ClientSettingChange::DiffFilesCollapsed(false),
        ClientSettingChange::DiffColorScheme(DiffColorScheme::BlueOrange),
        ClientSettingChange::BrowserSearchEngine(BrowserSearchEngine::Custom),
        ClientSettingChange::BrowserLinkTarget(BrowserLinkTarget::App),
        ClientSettingChange::PullRequestLinksOpenInApp(false),
        ClientSettingChange::BrowserCustomSearchUrl("  https://example.com/?q=%s  ".into()),
    ] {
        assert!(!doc.apply_change(&change).is_empty());
        assert!(doc.apply_change(&change).is_empty());
    }
    let doc = ClientSettingsDoc::from_json(&doc.to_json());
    assert_eq!(doc.diff_layout(), Layout::Split);
    assert!(!doc.diff_ignore_whitespace());
    assert!(!doc.diff_files_collapsed());
    assert_eq!(doc.diff_color_scheme(), DiffColorScheme::BlueOrange);
    assert_eq!(doc.browser_search_template(), "https://example.com/?q=%s");
    assert_eq!(doc.browser_link_target(), BrowserLinkTarget::App);
    assert!(!doc.pull_request_links_open_in_app());
    assert_eq!(doc.entries()["futureSetting"], 42);
}

#[test]
fn browser_custom_search_rejects_query_in_authority_and_falls_back() {
    use super::{BrowserSearchEngine, valid_search_template};
    let mut doc = ClientSettingsDoc::default();
    doc.apply(&ClientSettingChange::BrowserSearchEngine(
        BrowserSearchEngine::Custom,
    ));
    for invalid in [
        "",
        "https://example.com/",
        "file:///tmp/%s",
        "https://%s.example.com/",
        "https://%s@example.com/",
        "https://example.com:%s/",
    ] {
        assert!(!valid_search_template(invalid), "{invalid}");
        doc.apply(&ClientSettingChange::BrowserCustomSearchUrl(invalid.into()));
        assert_eq!(
            doc.browser_search_template(),
            crate::preview::address::DEFAULT_SEARCH_TEMPLATE
        );
    }
    for valid in [
        "https://example.com/search?q=%s",
        "http://localhost:8080/%s",
        "https://example.com/?q=%s&again=%s",
    ] {
        assert!(valid_search_template(valid), "{valid}");
    }
    for engine in BrowserSearchEngine::ALL
        .iter()
        .filter(|engine| **engine != BrowserSearchEngine::Custom)
    {
        doc.apply(&ClientSettingChange::BrowserSearchEngine(*engine));
        assert!(valid_search_template(doc.browser_search_template()));
    }
}

#[test]
fn grouping_override_record_matches_contract_and_preserves_raw_settings() {
    use crate::sidebar::grouping::GroupingMode;
    let raw = r#"{"sidebarProjectGroupingOverrides":{" \ufeffenv:/copy ":"separate","unknown-checkout":"repository_path"},"futureSetting":42}"#;
    let mut doc = ClientSettingsDoc::from_json(raw);
    assert_eq!(doc.project_grouping_settings().overrides.get("env:/copy"), Some(&GroupingMode::Separate));
    assert_eq!(doc.project_grouping_settings().overrides.get("unknown-checkout"), Some(&GroupingMode::RepositoryPath));
    doc.apply(&ClientSettingChange::WorkingShelf(false));
    assert_eq!(doc.entries()["sidebarProjectGroupingOverrides"], serde_json::from_str::<Value>(raw).unwrap()["sidebarProjectGroupingOverrides"]);
    let change = ClientSettingChange::ProjectGroupingOverride { key: "env:/copy".into(), mode: Some(ProjectGroupingMode::RepositoryPath) };
    let patch = doc.apply_change(&change);
    assert_eq!(patch["sidebarProjectGroupingOverrides"], serde_json::json!({"env:/copy":"repository_path","unknown-checkout":"repository_path"}));
    assert!(doc.apply_change(&change).is_empty());
    assert!(doc.apply_change(&ClientSettingChange::ProjectGroupingOverride { key: " env:/copy ".into(), mode: Some(ProjectGroupingMode::RepositoryPath) }).is_empty());
    assert!(doc.apply_change(&ClientSettingChange::ProjectGroupingOverride { key: " ".into(), mode: Some(ProjectGroupingMode::Separate) }).is_empty());
    let saved = merge_patch(Some(raw), &patch);
    let mut reread = ClientSettingsDoc::from_json(&saved.text);
    assert_eq!(reread.project_grouping_settings().overrides, doc.project_grouping_settings().overrides);
    assert_eq!(reread.entries()["futureSetting"], 42);
    reread.apply(&ClientSettingChange::ProjectGroupingOverride { key: "env:/copy".into(), mode: None });
    assert_eq!(reread.entries()["sidebarProjectGroupingOverrides"], serde_json::json!({"unknown-checkout":"repository_path"}));
    reread.apply(&ClientSettingChange::ProjectGroupingOverride { key: "unknown-checkout".into(), mode: None });
    assert_eq!(reread.entries()["sidebarProjectGroupingOverrides"], serde_json::json!({}));
    for value in [
        serde_json::json!(null),
        serde_json::json!([]),
        serde_json::json!({"env:/x":"future","valid":"repository"}),
        serde_json::json!({" ":"separate"}),
        serde_json::json!({"env:/x":42}),
    ] {
        let raw = serde_json::json!({"sidebarProjectGroupingOverrides":value,"sidebarProjectGroupingMode":"separate"}).to_string();
        let doc = ClientSettingsDoc::from_json(&raw);
        assert!(doc.project_grouping_settings().overrides.is_empty());
        assert_eq!(doc.project_grouping(), ProjectGroupingMode::Separate);
        assert_eq!(doc.entries()["sidebarProjectGroupingOverrides"], value);
    }
}

#[test]
fn math_rendering_enabled_defaults_to_false_and_toggles() {
    let mut doc = ClientSettingsDoc::default();
    assert!(!doc.math_rendering_enabled());
    assert!(doc.apply(&ClientSettingChange::MathRenderingEnabled(true)));
    assert!(doc.math_rendering_enabled());
    assert_eq!(doc.entries()["mathRenderingEnabled"], serde_json::Value::Bool(true));
}

#[test]
fn tool_call_visibility_defaults_round_trips_and_tolerates_unknown_values() {
    use super::ToolCallVisibility;
    let mut doc = ClientSettingsDoc::default();
    assert_eq!(doc.tool_call_visibility(), ToolCallVisibility::ShowAll);
    for (mode, wire) in [
        (ToolCallVisibility::HideSuccessful, "hideSuccessful"),
        (ToolCallVisibility::HideAll, "hideAll"),
        (ToolCallVisibility::Zen, "zen"),
        (ToolCallVisibility::ShowAll, "showAll"),
    ] {
        assert!(!doc.apply_change(&ClientSettingChange::ToolCallVisibility(mode)).is_empty());
        assert!(doc.apply_change(&ClientSettingChange::ToolCallVisibility(mode)).is_empty());
        assert_eq!(doc.tool_call_visibility(), mode);
        assert_eq!(doc.entries()["toolCallVisibility"], serde_json::json!(wire));
    }
    let unknown = ClientSettingsDoc::from_json(r#"{"toolCallVisibility":"future","futureSetting":7}"#);
    assert_eq!(unknown.tool_call_visibility(), ToolCallVisibility::ShowAll);
    assert_eq!(unknown.entries()["futureSetting"], 7);
}

#[test]
fn voice_settings_round_trip_and_project_custom_words() {
    use crate::voice_input::{
        SpeechCustomWordEntry, SpeechPostProcessingPromptSettings, VoiceMicrophoneEntry,
        VoiceShortcutMode,
    };

    let mut doc = ClientSettingsDoc::default();
    assert_eq!(doc.voice_shortcut_mode(), VoiceShortcutMode::Auto);
    assert_eq!(doc.speech_language(), "auto");
    assert!(!doc.speech_translate_to_english());
    assert!(doc.speech_silence_filtering());
    assert!(doc.speech_remove_filler_words());
    assert!(doc.speech_custom_words().is_empty());

    doc.apply(&ClientSettingChange::VoiceShortcutMode(VoiceShortcutMode::Hold));
    doc.apply(&ClientSettingChange::SpeechLanguage("de".into()));
    doc.apply(&ClientSettingChange::SpeechTranslateToEnglish(true));
    doc.apply(&ClientSettingChange::SpeechSilenceFiltering(false));
    doc.apply(&ClientSettingChange::SpeechRemoveFillerWords(false));
    doc.apply(&ClientSettingChange::SpeechCorrectionWord("wait actually".into()));
    doc.apply(&ClientSettingChange::VoiceMicrophones(vec![
        VoiceMicrophoneEntry::Device { device_id: "usb-1".into(), label: "USB Mic".into() },
        VoiceMicrophoneEntry::Default,
    ]));
    doc.apply(&ClientSettingChange::SpeechCustomWords(vec![
        SpeechCustomWordEntry { term: "Effect-TS".into(), aliases: vec!["effect ts".into()] },
    ]));
    doc.apply(&ClientSettingChange::SpeechPostProcessingPrompt(SpeechPostProcessingPromptSettings {
        custom_instructions: "Keep code terms exact.".into(),
        ..Default::default()
    }));

    let reread = ClientSettingsDoc::from_json(&doc.to_json());
    assert_eq!(reread.voice_shortcut_mode(), VoiceShortcutMode::Hold);
    assert_eq!(reread.speech_language(), "de");
    assert!(reread.speech_translate_to_english());
    assert!(!reread.speech_silence_filtering());
    assert!(!reread.speech_remove_filler_words());
    assert_eq!(reread.speech_correction_word(), "wait actually");
    assert_eq!(reread.voice_microphones().len(), 2);
    assert_eq!(reread.speech_custom_words()[0].term, "Effect-TS");
    assert_eq!(reread.speech_post_processing_prompt().custom_instructions, "Keep code terms exact.");

    let server_settings: crate::server_config::ServerSettings = serde_json::from_value(serde_json::json!({
        "projectSettingsOverrides": {
            "/repo": {
                "speechProjectCustomWords": [
                    { "term": "GPUI", "aliases": ["g p u i"] }
                ]
            }
        }
    }))
    .unwrap();
    let words = server_settings.project_speech_custom_words(Some("/repo"));
    assert_eq!(words.len(), 1);
    assert_eq!(words[0].term, "GPUI");
}

#[test]
fn language_proactive_panels_and_screen_reader_settings_match_contract_defaults() {
    use super::ScreenReaderAnnouncements;

    let mut doc = ClientSettingsDoc::default();
    assert_eq!(doc.language(), "en");
    assert!(!doc.proactive_panels_enabled());
    assert_eq!(doc.screen_reader_announcements(), ScreenReaderAnnouncements::Status);
    assert!(!doc.screen_reader_window_title());

    assert!(doc.apply(&ClientSettingChange::Language("system".into())));
    assert_eq!(doc.language(), "system");
    assert!(doc.apply(&ClientSettingChange::Language("pt-BR".into())));
    assert_eq!(doc.language(), "pt-BR");

    assert!(doc.apply(&ClientSettingChange::ProactivePanelsEnabled(true)));
    assert!(doc.proactive_panels_enabled());

    assert!(doc.apply(&ClientSettingChange::ScreenReaderAnnouncements(
        ScreenReaderAnnouncements::Everything
    )));
    assert_eq!(doc.screen_reader_announcements(), ScreenReaderAnnouncements::Everything);
    assert!(doc.apply(&ClientSettingChange::ScreenReaderWindowTitle(true)));
    assert!(doc.screen_reader_window_title());

    let reread = ClientSettingsDoc::from_json(&doc.to_json());
    assert_eq!(reread.language(), "pt-BR");
    assert!(reread.proactive_panels_enabled());
    assert_eq!(reread.screen_reader_announcements(), ScreenReaderAnnouncements::Everything);
    assert!(reread.screen_reader_window_title());

    // Invalid values fall back to contract defaults without dropping unknown keys.
    let invalid = ClientSettingsDoc::from_json(
        r#"{"language":42,"proactivePanelsEnabled":"yes","screenReaderAnnouncements":"invalid","screenReaderWindowTitle":1,"future":9}"#,
    );
    assert_eq!(invalid.language(), "en");
    assert!(!invalid.proactive_panels_enabled());
    assert_eq!(invalid.screen_reader_announcements(), ScreenReaderAnnouncements::Status);
    assert!(!invalid.screen_reader_window_title());
    assert_eq!(invalid.entries()["future"], 9);
}

#[test]
fn appearance_interface_settings_round_trip_clamp_and_reset() {
    let mut doc = ClientSettingsDoc::default();
    assert_eq!(doc.effort_display(), EffortDisplay::Label);
    assert_eq!(doc.panel_animation_duration_ms(), DEFAULT_PANEL_ANIMATION_DURATION_MS);
    assert_eq!(doc.environment_identification_mode(), EnvironmentIdentificationMode::Artwork);
    assert!(!doc.persist_composer_context_strip());
    assert!(doc.thread_details_is_default());
    assert_eq!(doc.thread_details_enabled_count(), 10);
    assert!(doc.thread_details_item_visible("environment"));
    assert!(!doc.thread_details_item_visible("usage"));
    assert!(!doc.thread_details_item_visible("context"));

    // Effort display
    assert!(!doc.apply_change(&ClientSettingChange::EffortDisplay(EffortDisplay::Bars)).is_empty());
    assert!(doc.apply_change(&ClientSettingChange::EffortDisplay(EffortDisplay::Bars)).is_empty());
    assert_eq!(doc.effort_display(), EffortDisplay::Bars);
    assert_eq!(doc.entries()["effortDisplay"], serde_json::json!("bars"));

    // Panel animation duration clamps to 0..=400
    assert!(!doc.apply_change(&ClientSettingChange::PanelAnimationDurationMs(999)).is_empty());
    assert_eq!(doc.panel_animation_duration_ms(), MAX_PANEL_ANIMATION_DURATION_MS);
    assert!(!doc.apply_change(&ClientSettingChange::PanelAnimationDurationMs(175)).is_empty());
    assert_eq!(doc.panel_animation_duration_ms(), 175);

    // Environment identification mode
    for (mode, wire) in [
        (EnvironmentIdentificationMode::NightlyArtwork, "nightly-artwork"),
        (EnvironmentIdentificationMode::DevArtwork, "dev-artwork"),
        (EnvironmentIdentificationMode::Pill, "pill"),
        (EnvironmentIdentificationMode::None, "none"),
        (EnvironmentIdentificationMode::Artwork, "artwork"),
    ] {
        assert!(!doc.apply_change(&ClientSettingChange::EnvironmentIdentificationMode(mode)).is_empty());
        assert_eq!(doc.environment_identification_mode(), mode);
        assert_eq!(doc.entries()["environmentIdentificationMode"], serde_json::json!(wire));
    }

    // Persist composer context strip
    assert!(!doc.apply_change(&ClientSettingChange::PersistComposerContextStrip(true)).is_empty());
    assert!(doc.persist_composer_context_strip());
    assert_eq!(doc.entries()["persistComposerContextStrip"], serde_json::json!(true));

    // Round-trip through JSON
    let reread = ClientSettingsDoc::from_json(&doc.to_json());
    assert_eq!(reread.effort_display(), EffortDisplay::Bars);
    assert_eq!(reread.panel_animation_duration_ms(), 175);
    assert_eq!(reread.environment_identification_mode(), EnvironmentIdentificationMode::Artwork);
    assert!(reread.persist_composer_context_strip());

    // Invalid / out-of-range values fall back per-key without dropping unknown fields
    let invalid = ClientSettingsDoc::from_json(
        r#"{"effortDisplay":"emoji","panelAnimationDurationMs":500,"environmentIdentificationMode":"badge","persistComposerContextStrip":"yes","threadDetailsItems":{"servers":"nope","usage":true,"futureItem":false},"future":11}"#,
    );
    assert_eq!(invalid.effort_display(), EffortDisplay::Label);
    assert_eq!(invalid.panel_animation_duration_ms(), 0);
    assert_eq!(invalid.environment_identification_mode(), EnvironmentIdentificationMode::Artwork);
    assert!(!invalid.persist_composer_context_strip());
    assert!(invalid.thread_details_item_visible("servers"));
    assert!(invalid.thread_details_item_visible("usage"));
    assert_eq!(invalid.entries()["future"], 11);
}

#[test]
fn thread_details_items_are_sparse_preserve_unknown_keys_and_reset_clears_legacy_usage() {
    // Legacy Labs toggle makes Usage & Context visible by default and marks visibility non-default.
    let mut doc = ClientSettingsDoc::from_json(
        r#"{"threadDetailsUsageEnabled":true,"threadDetailsItems":{"futureItem":true}}"#,
    );
    assert!(doc.thread_details_item_visible("usage"));
    assert!(doc.thread_details_item_visible("context"));
    assert!(!doc.thread_details_is_default());
    assert_eq!(doc.thread_details_enabled_count(), 12);

    // Hiding a default-visible item records only that override and keeps unknown ids.
    let patch = doc.apply_change(&ClientSettingChange::ThreadDetailsItem {
        id: "servers".into(),
        visible: false,
    });
    assert_eq!(
        patch["threadDetailsItems"],
        serde_json::json!({"servers": false, "futureItem": true})
    );
    assert!(!doc.thread_details_item_visible("servers"));
    assert_eq!(doc.thread_details_enabled_count(), 11);

    // Restoring it back to default drops the key from the sparse map.
    let patch = doc.apply_change(&ClientSettingChange::ThreadDetailsItem {
        id: "servers".into(),
        visible: true,
    });
    assert_eq!(patch["threadDetailsItems"], serde_json::json!({"futureItem": true}));

    // Reset clears both overrides and the retired Labs flag, and is idempotent on repeat.
    let reset = doc.apply_change(&ClientSettingChange::ResetThreadDetailsItems);
    assert_eq!(reset["threadDetailsItems"], serde_json::json!({}));
    assert_eq!(reset["threadDetailsUsageEnabled"], serde_json::json!(false));
    assert!(doc.thread_details_is_default());
    assert!(!doc.thread_details_item_visible("usage"));
    assert!(!doc.thread_details_item_visible("context"));
    assert_eq!(doc.thread_details_enabled_count(), 10);
    assert!(doc.apply_change(&ClientSettingChange::ResetThreadDetailsItems).is_empty());
}

#[test]
fn environment_identification_and_composer_context_helpers_match_web() {
    assert!(is_nightly_server_version("0.1.7-nightly.20261102.4061"));
    assert!(is_nightly_server_version("0.1.7-preview.20261102.1"));
    assert!(!is_nightly_server_version("0.1.7"));
    assert!(!is_nightly_server_version("0.1.7-nightly.202611.1"));

    assert_eq!(
        resolve_server_backed_stage_label(Some("0.1.7-nightly.20261102.4061"), "Latest"),
        "Nightly"
    );
    assert_eq!(resolve_server_backed_stage_label(Some("0.1.7"), "Dev"), "Dev");
    assert_eq!(resolve_server_backed_stage_label(None, "Latest"), "Latest");

    assert_eq!(
        resolve_sidebar_stage_backdrop_variant("Dev", EnvironmentIdentificationMode::Artwork),
        Some(StageBackdropVariant::Dev)
    );
    assert_eq!(
        resolve_sidebar_stage_backdrop_variant("Nightly", EnvironmentIdentificationMode::Artwork),
        Some(StageBackdropVariant::Nightly)
    );
    assert_eq!(
        resolve_sidebar_stage_backdrop_variant("Latest", EnvironmentIdentificationMode::Artwork),
        None
    );
    assert_eq!(
        resolve_sidebar_stage_backdrop_variant("Latest", EnvironmentIdentificationMode::NightlyArtwork),
        Some(StageBackdropVariant::Nightly)
    );
    assert_eq!(
        resolve_sidebar_stage_backdrop_variant("Latest", EnvironmentIdentificationMode::DevArtwork),
        Some(StageBackdropVariant::Dev)
    );
    assert_eq!(
        resolve_sidebar_stage_backdrop_variant("Dev", EnvironmentIdentificationMode::Pill),
        None
    );
    assert_eq!(resolve_environment_identification_pill_label("Dev"), Some("Dev"));
    assert_eq!(resolve_environment_identification_pill_label("Nightly"), Some("Nightly"));
    assert_eq!(resolve_environment_identification_pill_label("Latest"), None);

    // Custom/environment palette theme forces artwork modes to pill, leaving pill & none intact.
    assert_eq!(
        resolve_effective_environment_identification_mode(
            EnvironmentIdentificationMode::Artwork,
            true,
            false
        ),
        EnvironmentIdentificationMode::Pill
    );
    assert_eq!(
        resolve_effective_environment_identification_mode(
            EnvironmentIdentificationMode::Artwork,
            true,
            true
        ),
        EnvironmentIdentificationMode::Artwork
    );
    assert_eq!(
        resolve_effective_environment_identification_mode(
            EnvironmentIdentificationMode::None,
            true,
            false
        ),
        EnvironmentIdentificationMode::None
    );

    assert_eq!(environment_identification_options(Some(StageBackdropVariant::Dev)).len(), 4);
    assert_eq!(environment_identification_options(Some(StageBackdropVariant::Nightly)).len(), 4);
    assert_eq!(environment_identification_options(None).len(), 3);
    assert_eq!(environment_identification_options(None)[0], (EnvironmentIdentificationMode::Artwork, "None"));

    // Composer context strip visibility matches BranchToolbar.logic.ts.
    assert!(should_show_composer_context_strip(true, false, true, false, false));
    assert!(!should_show_composer_context_strip(false, false, true, true, true));
    assert!(should_show_composer_context_strip(false, true, true, true, false));
    assert!(should_show_composer_context_strip(false, true, true, false, true));
    assert!(!should_show_composer_context_strip(false, true, true, false, false));
    assert!(!should_show_composer_context_strip(true, true, false, true, true));
}

#[test]
fn confirm_terminal_close_and_speech_streaming_default_true_and_round_trip() {
    let mut doc = ClientSettingsDoc::default();
    assert!(doc.confirm_terminal_close());
    assert!(doc.speech_streaming());

    assert!(doc.apply(&ClientSettingChange::ConfirmTerminalClose(false)));
    assert!(!doc.confirm_terminal_close());
    assert!(doc.apply(&ClientSettingChange::SpeechStreaming(false)));
    assert!(!doc.speech_streaming());

    let reread = ClientSettingsDoc::from_json(&doc.to_json());
    assert!(!reread.confirm_terminal_close());
    assert!(!reread.speech_streaming());

    let invalid = ClientSettingsDoc::from_json(r#"{"confirmTerminalClose":"nope","speechStreaming":0}"#);
    assert!(invalid.confirm_terminal_close());
    assert!(invalid.speech_streaming());
}

#[test]
fn browser_default_viewport_zoom_appearance_and_profiles_round_trip_and_validate() {
    use crate::preview::viewport::{
        AppearancePreference, BROWSER_PROFILE_MAX_COUNT, BrowserProfile, BrowserProfileKind,
        DEFAULT_BROWSER_PROFILE_ID, INCOGNITO_BROWSER_PROFILE_ID, ViewportSetting, preset_setting,
    };

    let mut doc = ClientSettingsDoc::from_json(r#"{"futureSetting": 99}"#);
    assert_eq!(doc.browser_default_viewport(), ViewportSetting::Fill);
    assert_eq!(doc.browser_default_zoom_factor(), crate::preview::DEFAULT_ZOOM);
    assert_eq!(doc.browser_default_appearance(), AppearancePreference::System);
    assert!(doc.browser_user_profiles().is_empty());
    assert_eq!(doc.browser_default_profile_id(), DEFAULT_BROWSER_PROFILE_ID);

    // Preset viewport round-trips and matches BrowserDefaults.
    let preset = preset_setting("iphone-12-pro").unwrap();
    assert!(!doc.apply_change(&ClientSettingChange::BrowserDefaultViewport(preset.clone())).is_empty());
    assert!(doc.apply_change(&ClientSettingChange::BrowserDefaultViewport(preset.clone())).is_empty());
    assert_eq!(doc.browser_default_viewport(), preset);
    assert_eq!(
        doc.entries()["browserDefaultViewport"],
        serde_json::json!({ "_tag": "preset", "width": 390, "height": 844, "presetId": "iphone-12-pro" })
    );

    // Out-of-bounds freeform viewport normalizes back to Fill.
    let invalid_vp = ViewportSetting::Freeform { width: 100, height: 800 };
    assert!(!doc.apply_change(&ClientSettingChange::BrowserDefaultViewport(invalid_vp)).is_empty());
    assert_eq!(doc.browser_default_viewport(), ViewportSetting::Fill);
    let freeform = ViewportSetting::Freeform { width: 1280, height: 800 };
    assert!(!doc.apply_change(&ClientSettingChange::BrowserDefaultViewport(freeform.clone())).is_empty());
    assert_eq!(doc.browser_default_viewport(), freeform);

    // Allowed zoom level persists; unsupported zoom falls back to 1.0.
    assert!(!doc.apply_change(&ClientSettingChange::BrowserDefaultZoomFactor(1.25)).is_empty());
    assert!(doc.apply_change(&ClientSettingChange::BrowserDefaultZoomFactor(1.25)).is_empty());
    assert_eq!(doc.browser_default_zoom_factor(), 1.25);
    assert_eq!(doc.entries()["browserDefaultZoomFactor"], serde_json::json!(1.25));
    assert!(!doc.apply_change(&ClientSettingChange::BrowserDefaultZoomFactor(1.37)).is_empty());
    assert_eq!(doc.browser_default_zoom_factor(), 1.0);
    assert!(!doc.apply_change(&ClientSettingChange::BrowserDefaultZoomFactor(0.8)).is_empty());
    assert_eq!(doc.browser_default_zoom_factor(), 0.8);

    // Appearance preference round-trips across all 3 variants.
    for (pref, wire) in [
        (AppearancePreference::Dark, "dark"),
        (AppearancePreference::Light, "light"),
        (AppearancePreference::System, "system"),
        (AppearancePreference::Dark, "dark"),
    ] {
        assert!(!doc.apply_change(&ClientSettingChange::BrowserDefaultAppearance(pref)).is_empty());
        assert!(doc.apply_change(&ClientSettingChange::BrowserDefaultAppearance(pref)).is_empty());
        assert_eq!(doc.browser_default_appearance(), pref);
        assert_eq!(doc.entries()["browserDefaultAppearance"], serde_json::json!(wire));
    }

    // Profiles sanitize built-ins, duplicates, incognito kind, and cap at BROWSER_PROFILE_MAX_COUNT.
    let mut raw_profiles = vec![
        BrowserProfile { id: "default".into(), name: "Spoof".into(), kind: BrowserProfileKind::Persistent },
        BrowserProfile { id: "incognito".into(), name: "Spoof".into(), kind: BrowserProfileKind::Incognito },
        BrowserProfile { id: " work ".into(), name: " Work ".into(), kind: BrowserProfileKind::Incognito },
        BrowserProfile { id: "work".into(), name: "Duplicate".into(), kind: BrowserProfileKind::Persistent },
        BrowserProfile { id: "bad\nid".into(), name: "Control".into(), kind: BrowserProfileKind::Persistent },
    ];
    for idx in 0..30 {
        raw_profiles.push(BrowserProfile {
            id: format!("p-{idx}"),
            name: format!("Profile {idx}"),
            kind: BrowserProfileKind::Persistent,
        });
    }
    assert!(!doc.apply_change(&ClientSettingChange::BrowserProfiles(raw_profiles)).is_empty());
    let saved_profiles = doc.browser_user_profiles();
    assert_eq!(saved_profiles.len(), BROWSER_PROFILE_MAX_COUNT);
    assert_eq!(saved_profiles[0], BrowserProfile { id: "work".into(), name: "Work".into(), kind: BrowserProfileKind::Persistent });

    // Default profile accepts persistent profile, rejects incognito/control chars.
    assert!(!doc.apply_change(&ClientSettingChange::BrowserDefaultProfileId("work".into())).is_empty());
    assert_eq!(doc.browser_default_profile_id(), "work");
    assert!(!doc.apply_change(&ClientSettingChange::BrowserDefaultProfileId(INCOGNITO_BROWSER_PROFILE_ID.into())).is_empty());
    assert_eq!(doc.browser_default_profile_id(), DEFAULT_BROWSER_PROFILE_ID);
    assert!(!doc.apply_change(&ClientSettingChange::BrowserDefaultProfileId(" work ".into())).is_empty());
    assert_eq!(doc.browser_default_profile_id(), "work");

    // JSON round-trip preserves everything + unknown keys and agrees with BrowserDefaults.
    let reread = ClientSettingsDoc::from_json(&doc.to_json());
    assert_eq!(reread.entries()["futureSetting"], 99);
    let defaults = reread.browser_defaults();
    assert_eq!(defaults.viewport, freeform);
    assert_eq!(defaults.zoom_factor, 0.8);
    assert_eq!(defaults.appearance, AppearancePreference::Dark);
    assert_eq!(defaults.profile_id, "work");
    assert_eq!(defaults.profiles.len(), BROWSER_PROFILE_MAX_COUNT + 2);

    // Invalid stored JSON falls back per-key without dropping unknown fields.
    let invalid = ClientSettingsDoc::from_json(
        r#"{"browserDefaultViewport":{"_tag":"freeform","width":3840,"height":3840},"browserDefaultZoomFactor":99,"browserDefaultAppearance":"sepia","browserProfiles":"nope","browserDefaultProfileId":"missing","futureSetting":7}"#,
    );
    assert_eq!(invalid.browser_default_viewport(), ViewportSetting::Fill);
    assert_eq!(invalid.browser_default_zoom_factor(), 1.0);
    assert_eq!(invalid.browser_default_appearance(), AppearancePreference::System);
    assert!(invalid.browser_user_profiles().is_empty());
    assert_eq!(invalid.browser_default_profile_id(), DEFAULT_BROWSER_PROFILE_ID);
    assert_eq!(invalid.entries()["futureSetting"], 7);
}
