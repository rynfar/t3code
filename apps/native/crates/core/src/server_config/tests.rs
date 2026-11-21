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
                            "platform": {"os": "darwin"}, "capabilities": {"threadStop": true, "questionAttachments": true, "issues": true}},
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
    assert_eq!(config.environment.capabilities.question_attachments, Some(true));
    assert_eq!(config.environment.capabilities.issues, Some(true));
    assert_eq!(serde_json::from_value::<EnvironmentCapabilities>(json!({"issues": false})).unwrap().issues, Some(false));
    assert_eq!(EnvironmentCapabilities::default().issues, None);
    assert_eq!(serde_json::from_value::<EnvironmentCapabilities>(json!({})).unwrap().question_attachments, None);
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

    let published = json!({"id":"office","name":"Office","appearance":"dark","canvas":"#123","accent":"#abc"});
    let themes:ServerConfigStreamEvent=serde_json::from_value(json!({"type":"environmentThemesUpdated","payload":{"themes":[published.clone()]}})).unwrap();
    assert!(state.apply(themes.clone()));assert!(!state.apply(themes));
    assert!(!state.apply(snapshot(json!({"defaultRuntimeMode":"auto"}))));
    assert_eq!(state.config.as_ref().unwrap().environment_themes,vec![published]);
    let unknown: ServerConfigStreamEvent =
        serde_json::from_value(json!({"version": 1, "type": "environmentThemesUpdated", "payload": {"themes": []}})).unwrap();
    assert!(state.apply(unknown));
    assert!(state.config.as_ref().unwrap().environment_themes.is_empty());
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
    let sent_at = state.keybindings_revision();
    let reply = json!({"keybindings": [{"key": "mod+j", "command": "terminal.toggle"}], "issues": []});
    assert!(state.apply_keybindings_reply(&reply, sent_at));
    assert!(!state.apply_keybindings_reply(&reply, sent_at));
    assert!(!state.apply_keybindings_reply(&json!({}), sent_at));
    assert_eq!(state.config.as_ref().unwrap().keybindings[0]["command"], "terminal.toggle");

    // A keymap streamed while an edit was in flight is newer than that edit's reply.
    let sent_at = state.keybindings_revision();
    let newer: ServerConfigStreamEvent = serde_json::from_value(json!({
        "version": 1, "type": "keybindingsUpdated",
        "payload": {"keybindings": [{"key": "mod+l", "command": "terminal.toggle"}], "issues": []}
    }))
    .unwrap();
    assert!(state.apply(newer));
    let stale = json!({"keybindings": [{"key": "mod+m", "command": "terminal.toggle"}], "issues": []});
    assert!(!state.apply_keybindings_reply(&stale, sent_at));
    assert_eq!(state.config.unwrap().keybindings[0]["key"], "mod+l");
}

#[test]
fn without_environment_themes_scrubs_cached_state_and_tracks_arrival() {
    let cached: ServerConfig = serde_json::from_value(json!({
        "environment": { "environmentId": "e1", "label": "Mac", "serverVersion": "0.1.0", "capabilities": { "environmentThemes": true } },
        "providers": [provider("devReplay", "acp")],
        "environmentThemes": [{"id": "stale", "name": "Stale"}],
        "usageLimitSources": [{"id": "src-1", "label": "Proxy", "accounts": []}],
        "settings": {
            "defaultTheme": "ocean",
            "defaultThemeSetAt": "2026-11-19T00:00:00Z",
            "defaultAppearance": "dark",
            "defaultAppearanceSetAt": "2026-11-19T00:00:01Z"
        }
    }))
    .unwrap();

    let mut state = ServerConfigState::from_cached(cached);
    assert!(!state.is_live());
    let cfg = state.config.as_ref().unwrap();
    assert!(cfg.environment_themes.is_empty(), "cached environmentThemes must be scrubbed");
    assert!(cfg.usage_limit_sources.is_empty(), "cached usageLimitSources must be scrubbed");
    assert!(!cfg.environment_themes_received, "cached state has not received live themes");
    assert_eq!(cfg.environment.capabilities.environment_themes, Some(true));
    assert_eq!(cfg.settings.default_theme, "ocean");
    assert_eq!(cfg.settings.default_theme_set_at, "2026-11-19T00:00:00Z");
    assert_eq!(cfg.settings.default_appearance, "dark");
    assert_eq!(cfg.settings.default_appearance_set_at, "2026-11-19T00:00:01Z");

    // First live snapshot transitions source to live and leaves environment_themes_received false.
    assert!(state.apply(snapshot(json!({}))));
    assert!(state.is_live());
    assert!(!state.config.as_ref().unwrap().environment_themes_received);

    // An authoritative empty EnvironmentThemesUpdated flips environment_themes_received to true
    // and reports a change so subscribers can clear any persisted theme cache.
    let empty_themes: ServerConfigStreamEvent = serde_json::from_value(json!({
        "version": 1, "type": "environmentThemesUpdated", "payload": { "themes": [] }
    }))
    .unwrap();
    assert!(state.apply(empty_themes.clone()));
    assert!(state.config.as_ref().unwrap().environment_themes_received);
    assert!(state.config.as_ref().unwrap().environment_themes.is_empty());
    // Repeating the same empty set reports unchanged.
    assert!(!state.apply(empty_themes));
    // Reconnect snapshot preserves environment_themes_received.
    assert!(!state.apply(snapshot(json!({}))));
    assert!(state.config.as_ref().unwrap().environment_themes_received);
}

#[test]
fn applies_provider_status_deltas_and_holds_on_demand_catalogs() {
    let mut state = ServerConfigState::default();
    let initial: ServerConfigStreamEvent = serde_json::from_value(json!({
        "version": 1,
        "type": "snapshot",
        "config": {
            "environment": { "environmentId": "e1", "label": "Mac", "serverVersion": "0.1.0" },
            "providers": [{
                "instanceId": "codex",
                "driver": "codex",
                "enabled": true,
                "installed": true,
                "status": "ready",
                "catalogDigest": "dig-1",
                "loadedCatalogDigest": "dig-1",
                "models": [{"slug": "gpt-5.4", "name": "GPT-5.4"}],
                "slashCommands": [{"name": "review", "description": "Review"}],
                "skills": [{"name": "rust", "enabled": true}]
            }]
        }
    }))
    .unwrap();
    assert!(state.apply(initial));

    // ProviderStatusDeltas omits models/slashCommands/skills (and sends null for one),
    // updating only status while inheriting held lists + dropping unrebuildable new providers.
    let delta: ServerConfigStreamEvent = serde_json::from_value(json!({
        "version": 1,
        "type": "providerStatusDeltas",
        "payload": {
            "providers": [
                {
                    "instanceId": "codex",
                    "driver": "codex",
                    "enabled": true,
                    "installed": true,
                    "status": "warning",
                    "catalogDigest": "dig-2",
                    "slashCommands": null
                },
                {
                    "instanceId": "unknown-partial",
                    "driver": "codex",
                    "enabled": true,
                    "installed": true,
                    "status": "ready"
                }
            ]
        }
    }))
    .unwrap();
    assert!(state.apply(delta));
    let providers = &state.config.as_ref().unwrap().providers;
    assert_eq!(providers.len(), 1, "delta without prior models/commands/skills is dropped");
    assert_eq!(providers[0].status, "warning");
    assert_eq!(providers[0].models.len(), 1);
    assert_eq!(providers[0].slash_commands.len(), 1, "held slash commands survive digest bump");
    assert_eq!(providers[0].skills.len(), 1, "held skills survive digest bump");
    assert_eq!(providers[0].catalog_digest.as_deref(), Some("dig-2"));
    assert_eq!(providers[0].loaded_catalog_digest.as_deref(), Some("dig-1"));

    // Stale catalog reply for dig-1 is ignored; live dig-2 reply replaces lists and updates digest.
    assert!(!state.apply_fetched_machine_catalogs(&[FetchedMachineCatalog {
        instance_id: "codex".into(),
        digest: "dig-1".into(),
        slash_commands: vec![],
        skills: vec![],
    }]));
    assert!(state.apply_fetched_machine_catalogs(&[FetchedMachineCatalog {
        instance_id: "codex".into(),
        digest: "dig-2".into(),
        slash_commands: vec![ProviderSlashCommand { name: "plan".into(), description: None, input: None }],
        skills: vec![],
    }]));
    let p = &state.config.as_ref().unwrap().providers[0];
    assert_eq!(p.loaded_catalog_digest.as_deref(), Some("dig-2"));
    assert_eq!(p.slash_commands[0].name, "plan");
    assert!(p.skills.is_empty());
}

#[test]
fn resolves_shortcut_keybindings_with_primary_authority_and_connected_fallback() {
    let primary: ServerConfig = serde_json::from_value(json!({
        "keybindings": [{"key": "mod+k", "command": "primary.cmd"}]
    }))
    .unwrap();
    let secondary: ServerConfig = serde_json::from_value(json!({
        "keybindings": [{"key": "mod+k", "command": "secondary.cmd"}]
    }))
    .unwrap();

    // Primary stays authoritative even while its config is still loading.
    assert_eq!(resolve_shortcut_keybindings(true, None, [&secondary]), None);
    assert_eq!(
        resolve_shortcut_keybindings(true, Some(&primary), [&secondary]).unwrap()[0]["command"],
        "primary.cmd"
    );
    // Without a primary, falls back to first connected config.
    assert_eq!(
        resolve_shortcut_keybindings(false, None, [&secondary]).unwrap()[0]["command"],
        "secondary.cmd"
    );
}

#[test]
fn decodes_hostname_provider_subproviders_retention_and_forward_compat_defaults() {
    let config: ServerConfig = serde_json::from_value(json!({
        "hostname": "devbox.local",
        "providers": [{
            "instanceId": "fx",
            "driver": "fx",
            "enabled": true,
            "installed": true,
            "status": "ready",
            "models": [{ "slug": "m1", "name": "M1", "subProvider": "openai" }],
            "subProviders": [
                { "name": "openai" },
                { "name": "anthropic", "unavailableReason": "Signed out" },
                { "broken": true }
            ],
            "modelsRetained": true,
            "workspaceRescannedAt": "2026-10-12T00:00:00Z"
        }],
        "settings": {
            "usageHistoryRetentionDays": "unlimited"
        }
    }))
    .unwrap();
    assert_eq!(config.hostname.as_deref(), Some("devbox.local"));
    assert_eq!(config.settings.usage_history_retention_days, UsageHistoryRetentionDays::Unlimited);
    let provider = &config.providers[0];
    assert_eq!(
        provider.sub_providers.as_deref(),
        Some(&[
            ProviderSubProvider { name: "openai".into(), unavailable_reason: None },
            ProviderSubProvider { name: "anthropic".into(), unavailable_reason: Some("Signed out".into()) },
        ][..])
    );
    assert_eq!(provider.models_retained, Some(true));
    assert_eq!(provider.workspace_rescanned_at.as_deref(), Some("2026-10-12T00:00:00Z"));

    // Provider deltas preserve the new fields while inheriting omitted models/commands/skills.
    let delta: ServerProviderDelta = serde_json::from_value(json!({
        "instanceId": "fx",
        "driver": "fx",
        "enabled": true,
        "installed": true,
        "status": "error",
        "modelsRetained": true,
        "subProviders": [{ "name": "openai" }],
        "workspaceRescannedAt": "2026-10-13T00:00:00Z"
    }))
    .unwrap();
    let merged = apply_server_provider_deltas(&config.providers, vec![delta]);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].models.len(), 1);
    assert_eq!(merged[0].models_retained, Some(true));
    assert_eq!(merged[0].sub_providers.as_ref().map(Vec::len), Some(1));
    assert_eq!(merged[0].workspace_rescanned_at.as_deref(), Some("2026-10-13T00:00:00Z"));

    // Retention days: positive int, default 92 when omitted or invalid.
    let days_settings: ServerSettings = serde_json::from_value(json!({ "usageHistoryRetentionDays": 30 })).unwrap();
    assert_eq!(days_settings.usage_history_retention_days, UsageHistoryRetentionDays::Days(30));
    assert_eq!(serde_json::to_value(UsageHistoryRetentionDays::Days(30)).unwrap(), json!(30));
    assert_eq!(serde_json::to_value(UsageHistoryRetentionDays::Unlimited).unwrap(), json!("unlimited"));
    for invalid in [json!({}), json!({ "usageHistoryRetentionDays": 0 }), json!({ "usageHistoryRetentionDays": -5 }), json!({ "usageHistoryRetentionDays": "forever" })] {
        let parsed: ServerSettings = serde_json::from_value(invalid).unwrap();
        assert_eq!(parsed.usage_history_retention_days, UsageHistoryRetentionDays::Days(92));
    }
    let older: ServerConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(older.hostname, None);
    assert_eq!(older.settings.usage_history_retention_days, UsageHistoryRetentionDays::Days(92));
}
