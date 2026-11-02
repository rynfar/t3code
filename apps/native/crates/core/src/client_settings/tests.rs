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
    doc.apply(&ClientSettingChange::PlanMode(true));
    let composer = doc.composer();
    assert_eq!(composer.send_shortcut, SendShortcut::ModEnterMultiline);
    assert_eq!(composer.follow_up_behavior, FollowUpBehavior::Queue);
    assert!(composer.plan_mode_enabled);
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
