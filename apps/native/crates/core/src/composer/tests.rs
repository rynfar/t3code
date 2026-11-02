use super::*;
use serde_json::json;

/// 2026-11-02T10:00:00Z.
const NOW: i64 = 1_793_613_600_000;

#[test]
fn dispatch_follows_the_follow_up_setting_and_its_alternate() {
    assert_eq!(dispatch_mode(false, true, FollowUpBehavior::Steer, true), ComposerDispatch::Auto);
    assert_eq!(dispatch_mode(true, false, FollowUpBehavior::Steer, true), ComposerDispatch::Steer);
    assert_eq!(dispatch_mode(true, true, FollowUpBehavior::Steer, true), ComposerDispatch::Queue);
    assert_eq!(dispatch_mode(true, false, FollowUpBehavior::Queue, true), ComposerDispatch::Queue);
    assert_eq!(dispatch_mode(true, true, FollowUpBehavior::Queue, true), ComposerDispatch::Steer);
    assert_eq!(dispatch_mode(true, false, FollowUpBehavior::Steer, false), ComposerDispatch::Queue);
}

#[test]
fn enter_sends_unless_the_shortcut_needs_a_modifier() {
    use SendShortcut::*;
    let (plain, modified) = (EnterKey::default(), EnterKey { modifier: true, ..EnterKey::default() });
    assert_eq!(submit_intent(plain, false, false, Enter, "hi"), Some(SubmitIntent::Foreground));
    assert_eq!(submit_intent(modified, false, false, Enter, "hi"), Some(SubmitIntent::Foreground));
    assert_eq!(submit_intent(modified, true, false, Enter, "hi"), Some(SubmitIntent::Alternate));
    assert_eq!(submit_intent(modified, false, true, Enter, "hi"), Some(SubmitIntent::Background));
    assert_eq!(submit_intent(plain, false, false, ModEnter, "hi"), None);
    assert_eq!(submit_intent(modified, false, false, ModEnter, "hi"), Some(SubmitIntent::Foreground));
    assert_eq!(submit_intent(plain, false, false, ModEnterMultiline, "one line"), Some(SubmitIntent::Foreground));
    assert_eq!(submit_intent(plain, false, false, ModEnterMultiline, "two\nlines"), None);
}

#[test]
fn a_held_or_composing_enter_never_sends() {
    let repeat = EnterKey { repeat: true, ..EnterKey::default() };
    let composing = EnterKey { modifier: true, composing: true, ..EnterKey::default() };
    assert_eq!(submit_intent(repeat, false, false, SendShortcut::Enter, "hi"), None);
    assert_eq!(submit_intent(composing, true, false, SendShortcut::Enter, "hi"), None);
}

#[test]
fn client_settings_read_web_keys_and_ignore_bad_values() {
    let settings = ClientSettings::from_json(
        r#"{"sendShortcut": "mod-enter", "followUpBehavior": "queue", "planModeEnabled": true, "notificationMode": "notifications-and-sound", "inAppNotificationsEnabled": true}"#,
    );
    assert_eq!(
        settings,
        ClientSettings {
            send_shortcut: SendShortcut::ModEnter,
            follow_up_behavior: FollowUpBehavior::Queue,
            plan_mode_enabled: true,
            notification_mode: crate::notifications::NotificationMode::NotificationsAndSound,
            in_app_notifications_enabled: true,
        }
    );
    let partial = ClientSettings::from_json(r#"{"sendShortcut": "shift-enter", "planModeEnabled": "yes"}"#);
    assert_eq!(partial, ClientSettings::default());
    assert_eq!(ClientSettings::from_json("not json"), ClientSettings::default());
}

fn provider(value: serde_json::Value) -> ServerProvider {
    let mut base = json!({"instanceId": "codex", "driver": "codex", "enabled": true, "installed": true, "status": "ready", "models": []});
    if let (serde_json::Value::Object(base), serde_json::Value::Object(extra)) = (&mut base, value) {
        base.extend(extra);
    }
    serde_json::from_value(base).unwrap()
}

#[test]
fn runtime_modes_narrow_to_what_the_provider_supports() {
    let all = compatible_runtime_modes(RuntimeMode::Auto, None);
    assert_eq!(all, (RuntimeMode::ALL.to_vec(), RuntimeMode::Auto));
    let limited = provider(json!({"supportedRuntimeModes": ["full-access", "approval-required"]}));
    let (modes, mode) = compatible_runtime_modes(RuntimeMode::Auto, Some(&limited));
    assert_eq!(modes, vec![RuntimeMode::ApprovalRequired, RuntimeMode::FullAccess]);
    assert_eq!(mode, RuntimeMode::ApprovalRequired);
    assert_eq!(runtime_mode_label(RuntimeMode::ApprovalRequired), "Supervised");
}

#[test]
fn plan_mode_needs_the_setting_and_a_provider_toggle() {
    let shows = provider(json!({}));
    let hides = provider(json!({"showInteractionModeToggle": false}));
    assert_eq!(interaction_mode(false, Some(&shows), InteractionMode::Plan), (false, InteractionMode::Default));
    assert_eq!(interaction_mode(true, Some(&shows), InteractionMode::Plan), (true, InteractionMode::Plan));
    assert_eq!(interaction_mode(true, Some(&hides), InteractionMode::Plan), (false, InteractionMode::Default));
    assert_eq!(interaction_mode(true, None, InteractionMode::Plan), (false, InteractionMode::Default));
}

#[test]
fn title_seed_collapses_whitespace_and_truncates_at_fifty_characters() {
    assert_eq!(title_seed("  Fix   the\nbuild  "), "Fix the build");
    assert_eq!(title_seed("   "), "New thread");
    let long = "é".repeat(60);
    assert_eq!(title_seed(&long), format!("{}...", "é".repeat(50)));
}

fn config(settings: serde_json::Value) -> ServerConfig {
    serde_json::from_value(json!({
        "providers": [
            {"instanceId": "devReplay", "driver": "acp", "enabled": true, "installed": true, "status": "ready",
             "models": [{"slug": "showcase", "name": "Showcase", "isCustom": false}]},
            {"instanceId": "codex", "driver": "codex", "enabled": true, "installed": true, "status": "ready",
             "models": [{"slug": "luna", "name": "Luna", "isCustom": false, "isDefault": true}]}
        ],
        "settings": settings
    }))
    .unwrap()
}

#[test]
fn draft_defaults_prefer_project_then_environment_then_carry_then_first_provider() {
    let devreplay = json!({"driver": "acp", "config": {"enabled": true}});
    let carry = ModelSelection { instance_id: "codex".into(), model: "sol".into(), options: None };
    let project = config(json!({
        "providerInstances": {"devReplay": devreplay},
        "defaultModelSelection": {"instanceId": "codex", "model": "luna"},
        "defaultRuntimeMode": "auto",
        "projectSettingsOverrides": {"p1": {"defaultModelSelection": {"instanceId": "devReplay", "model": "showcase"},
                                            "defaultRuntimeMode": "approval-required"}}
    }));
    let defaults = draft_defaults(Some(&project), "p1", Some(&carry), Some(InteractionMode::Plan), NOW);
    assert_eq!(defaults.model_selection.map(|s| s.model), Some("showcase".into()));
    assert_eq!(defaults.runtime_mode, RuntimeMode::ApprovalRequired);
    assert_eq!(defaults.interaction_mode, InteractionMode::Plan);

    let environment = draft_defaults(Some(&project), "p2", Some(&carry), None, NOW);
    assert_eq!(environment.model_selection.map(|s| s.model), Some("luna".into()));
    assert_eq!(environment.runtime_mode, RuntimeMode::Auto);

    let cleared = config(json!({"projectSettingsOverrides": {"p1": {"defaultModelSelection": null}},
                                "defaultModelSelection": {"instanceId": "codex", "model": "luna"}}));
    assert_eq!(draft_defaults(Some(&cleared), "p1", Some(&carry), None, NOW).model_selection, Some(carry.clone()));

    let bare = config(json!({"providerInstances": {"devReplay": devreplay}}));
    let first = draft_defaults(Some(&bare), "p1", None, None, NOW);
    assert_eq!(first.model_selection.map(|s| s.instance_id), Some("devReplay".into()));
    assert_eq!(first.runtime_mode, RuntimeMode::FullAccess);
    assert_eq!(draft_defaults(None, "p1", None, None, NOW).model_selection, None);
}

#[test]
fn effective_selection_keeps_a_usable_choice_and_replaces_an_unusable_one() {
    let config = config(json!({}));
    let entries = providers::entries(&config, None);
    let chosen = ModelSelection { instance_id: "codex".into(), model: "sol".into(), options: None };
    assert_eq!(effective_selection(&entries, Some(&chosen)), Some(chosen));
    let missing = ModelSelection { instance_id: "gone".into(), model: "x".into(), options: None };
    let replaced = effective_selection(&entries, Some(&missing)).unwrap();
    assert_eq!((replaced.instance_id.as_str(), replaced.model.as_str()), ("codex", "luna"));
}

#[test]
fn a_spent_default_falls_back_when_the_fallback_can_take_work() {
    let limits = |used: f64, resets: &str| json!({"checkedAt": "2026-11-02T09:00:00Z", "windows": [{"id": "weekly", "kind": "weekly", "label": "Weekly", "usedPercent": used, "resetsAt": resets}]});
    let config = |codex_limits: serde_json::Value, claude_enabled: bool| -> ServerConfig {
        serde_json::from_value(json!({
            "providers": [
                {"instanceId": "codex", "driver": "codex", "enabled": true, "installed": true, "status": "ready",
                 "auth": {"status": "authenticated"}, "usageLimits": codex_limits,
                 "models": [{"slug": "luna", "name": "Luna", "isCustom": false}]},
                {"instanceId": "claudeAgent", "driver": "claudeAgent", "enabled": claude_enabled, "installed": true, "status": "ready",
                 "auth": {"status": "authenticated"}, "models": [{"slug": "opus", "name": "Opus", "isCustom": false}]}
            ],
            "settings": {"defaultModelSelection": {"instanceId": "codex", "model": "luna"},
                         "fallbackModelSelection": {"instanceId": "claudeAgent", "model": "opus"}}
        }))
        .unwrap()
    };
    let model = |config: &ServerConfig| draft_defaults(Some(config), "p1", None, None, NOW).model_selection.map(|s| s.model);
    assert_eq!(model(&config(limits(100.0, "2026-11-03T00:00:00Z"), true)), Some("opus".into()));
    assert_eq!(model(&config(limits(100.0, "2026-11-01T00:00:00Z"), true)), Some("luna".into()));
    assert_eq!(model(&config(limits(80.0, "2026-11-03T00:00:00Z"), true)), Some("luna".into()));
    assert_eq!(model(&config(limits(100.0, "2026-11-03T00:00:00Z"), false)), Some("luna".into()));
}
