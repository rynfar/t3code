use super::*;
use serde_json::json;

fn provider(instance_id: &str, driver: &str) -> Value {
    json!({
        "instanceId": instance_id, "driver": driver, "enabled": true, "installed": true,
        "version": "1.0.0", "status": "ready", "auth": {"status": "authenticated"},
        "checkedAt": "2026-11-01T00:00:00.000Z", "slashCommands": [], "skills": [],
        "supportedRuntimeModes": ["approval-required", "full-access", "future-mode"],
        "models": [
            {"slug": "showcase", "name": "Showcase", "isCustom": false, "isDefault": true,
             "capabilities": {"optionDescriptors": [
                {"id": "reasoningEffort", "label": "Reasoning", "type": "select",
                 "options": [{"id": "low", "label": "Low"}, {"id": "high", "label": "High", "isDefault": true}]},
                {"id": "fastMode", "label": "Fast mode", "type": "boolean", "currentValue": false},
                {"id": "future", "label": "Future", "type": "slider"}
             ]}},
            {"slug": "broken"}
        ]
    })
}

fn snapshot(settings: Value) -> ServerConfigStreamEvent {
    serde_json::from_value(json!({
        "version": 1, "type": "snapshot",
        "config": {
            "environment": {"environmentId": "e1", "label": "Mac", "serverVersion": "0.1.0",
                            "platform": {"os": "darwin"}, "capabilities": {"threadStop": true}},
            "auth": {}, "cwd": "/w", "keybindingsConfigPath": "/k.json",
            "keybindings": [{"key": "mod+n", "command": "chat.new"}], "issues": [],
            "providers": [provider("devReplay", "acp"), {"instanceId": "half"}],
            "availableEditors": [], "observability": {}, "settings": settings
        }
    }))
    .unwrap()
}

#[test]
fn snapshot_decodes_providers_and_drops_what_it_cannot_read() {
    let mut state = ServerConfigState::default();
    assert!(state.apply(snapshot(json!({}))));
    let config = state.config.as_ref().unwrap();
    assert!(config.environment.capabilities.thread_stop == Some(true));
    assert_eq!(config.providers.len(), 1);
    let provider = &config.providers[0];
    assert_eq!(provider.supported_runtime_modes, Some(vec![RuntimeMode::ApprovalRequired, RuntimeMode::FullAccess]));
    assert_eq!(provider.models.len(), 1);
    let descriptors = &provider.models[0].capabilities.as_ref().unwrap().option_descriptors;
    assert_eq!(descriptors.len(), 2);
    assert!(matches!(&descriptors[0], OptionDescriptor::Select(select) if select.options[1].is_default));
    assert_eq!(config.settings.default_runtime_mode, RuntimeMode::FullAccess);
    assert_eq!(config.settings.favorites, None);
}

#[test]
fn settings_decode_defaults_and_project_overrides() {
    let mut state = ServerConfigState::default();
    state.apply(snapshot(json!({
        "favorites": [{"provider": "devReplay", "model": "showcase"}, {"bad": true}],
        "defaultRuntimeMode": "approval-required",
        "defaultModelSelection": {"instanceId": "devReplay", "model": "showcase"},
        "projectSettingsOverrides": {
            "p1": {"defaultModelSelection": null, "defaultRuntimeMode": "auto",
                   "providerInstances": {"devReplay": {"enabled": false}}},
            "p2": {"defaultRuntimeMode": "not-a-mode"}
        },
        "providerModelPreferences": {"devReplay": {"hiddenModels": ["old"]}}
    })));
    let settings = &state.config.as_ref().unwrap().settings;
    assert_eq!(settings.favorites.as_ref().map(Vec::len), Some(1));
    assert_eq!(settings.default_runtime_mode, RuntimeMode::ApprovalRequired);
    let p1 = &settings.project_settings_overrides["p1"];
    assert_eq!(p1.default_model_selection, Some(None));
    assert_eq!(p1.default_runtime_mode, Some(RuntimeMode::Auto));
    assert_eq!(p1.provider_instances["devReplay"].enabled, Some(false));
    assert_eq!(settings.project_settings_overrides["p2"].default_runtime_mode, None);
    assert_eq!(settings.provider_model_preferences.as_ref().unwrap()["devReplay"].hidden_models, vec!["old"]);
}

#[test]
fn updates_before_a_snapshot_are_dropped_and_unchanged_resends_are_quiet() {
    let mut state = ServerConfigState::default();
    let providers: ServerConfigStreamEvent = serde_json::from_value(json!({
        "version": 1, "type": "providerStatuses", "payload": {"providers": [provider("devReplay", "acp")]}
    }))
    .unwrap();
    assert!(!state.apply(providers.clone()));
    assert!(state.config.is_none());

    assert!(state.apply(snapshot(json!({}))));
    assert!(!state.apply(snapshot(json!({}))));
    assert!(!state.apply(providers));

    let settings: ServerConfigStreamEvent = serde_json::from_value(json!({
        "version": 1, "type": "settingsUpdated", "payload": {"settings": {"defaultRuntimeMode": "auto"}}
    }))
    .unwrap();
    assert!(state.apply(settings));
    assert_eq!(state.config.as_ref().unwrap().settings.default_runtime_mode, RuntimeMode::Auto);

    let unknown: ServerConfigStreamEvent =
        serde_json::from_value(json!({"version": 1, "type": "environmentThemesUpdated", "payload": {"themes": []}})).unwrap();
    assert!(!state.apply(unknown));
}

#[test]
fn keybindings_update_replaces_the_raw_payload() {
    let mut state = ServerConfigState::default();
    state.apply(snapshot(json!({})));
    let event: ServerConfigStreamEvent = serde_json::from_value(json!({
        "version": 1, "type": "keybindingsUpdated",
        "payload": {"keybindings": [{"key": "mod+k", "command": "commandPalette.toggle"}], "issues": []}
    }))
    .unwrap();
    assert!(state.apply(event));
    assert_eq!(state.config.as_ref().unwrap().keybindings[0]["command"], "commandPalette.toggle");
    assert_eq!(state.config.as_ref().unwrap().keybindings_config_path.as_deref(), Some("/k.json"));

    // An edit's reply applies its set at once; the same set streamed after it changes nothing.
    let reply = json!({"keybindings": [{"key": "mod+j", "command": "terminal.toggle"}], "issues": []});
    assert!(state.apply_keybindings_reply(&reply));
    assert!(!state.apply_keybindings_reply(&reply));
    assert!(!state.apply_keybindings_reply(&json!({})));
    assert_eq!(state.config.unwrap().keybindings[0]["command"], "terminal.toggle");
}
