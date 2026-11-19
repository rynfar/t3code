use super::*;
use serde_json::{Value, json};

fn provider(instance_id: &str, driver: &str, status: &str, models: Value) -> Value {
    json!({
        "instanceId": instance_id, "driver": driver, "enabled": true, "installed": true,
        "status": status, "auth": {"status": "authenticated"}, "models": models
    })
}

fn model(slug: &str, extra: Value) -> Value {
    let mut value = json!({"slug": slug, "name": slug.to_uppercase(), "isCustom": false});
    if let (Value::Object(base), Value::Object(extra)) = (&mut value, extra) {
        base.extend(extra);
    }
    value
}

fn config(providers: Value, settings: Value) -> ServerConfig {
    serde_json::from_value(json!({"providers": providers, "settings": settings})).unwrap()
}

#[test]
fn entries_group_by_driver_with_the_default_instance_first() {
    let config = config(
        json!([
            provider("codex_work", "codex", "ready", json!([])),
            provider("devReplay", "acp", "ready", json!([])),
            provider("codex", "codex", "ready", json!([])),
        ]),
        json!({"providerInstances": {
            "codex_work": {"driver": "codex"},
            "devReplay": {"driver": "acp", "config": {"enabled": true}}
        }}),
    );
    let entries = entries(&config, None);
    let ids: Vec<&str> = entries.iter().map(ProviderEntry::instance_id).collect();
    assert_eq!(ids, vec!["codex", "codex_work", "devReplay"]);
    assert!(entries.iter().all(|entry| entry.enabled));
    assert_eq!(entries[1].display_name, "Codex Work");
    assert_eq!(entries[2].display_name, "Dev Replay");
}

#[test]
fn enablement_follows_project_overrides_then_config_then_driver_defaults() {
    let settings: ServerSettings = serde_json::from_value(json!({
        "providerInstances": {
            "cursor": {"driver": "cursor"},
            "kiro": {"driver": "kiro", "config": {}},
            "kiro_work": {"driver": "kiro"},
            "kiro_on": {"driver": "kiro", "enabled": true},
            "kiro_config_on": {"driver": "kiro", "config": {"enabled": true}},
            "kiro_off": {"driver": "kiro", "enabled": true, "config": {"enabled": false}},
            "off": {"driver": "codex", "enabled": true, "config": {"enabled": false}}
        },
        "projectSettingsOverrides": {"p1": {"providerInstances": {"cursor": {"enabled": true}}}}
    }))
    .unwrap();
    for id in ["kiro", "kiro_work", "kiro_off"] {
        assert!(!instance_enabled(&settings, None, id), "{id} should be disabled");
    }
    for id in ["kiro_on", "kiro_config_on"] {
        assert!(instance_enabled(&settings, None, id), "{id} should be enabled");
    }
    assert!(!instance_enabled(&settings, None, "cursor"));
    assert!(instance_enabled(&settings, Some("p1"), "cursor"));
    assert!(!instance_enabled(&settings, None, "off"));
    assert!(instance_enabled(&settings, None, "claudeAgent"));
    assert!(!instance_enabled(&settings, None, "unconfigured"));
}

#[test]
fn default_selection_prefers_a_ready_instance_of_the_first_driver() {
    let config = config(
        json!([
            provider("codex", "codex", "error", json!([model("gpt", json!({}))])),
            provider("codex_two", "codex", "ready", json!([model("custom", json!({"isCustom": true})), model("luna", json!({}))])),
            provider("claudeAgent", "claudeAgent", "ready", json!([model("opus", json!({"isDefault": true}))])),
        ]),
        json!({"providerInstances": {"codex_two": {"driver": "codex"}}}),
    );
    let entries = entries(&config, None);
    let selection = project_default_selection(&entries).unwrap();
    assert_eq!((selection.instance_id.as_str(), selection.model.as_str()), ("codex_two", "luna"));
    assert_eq!(selectable_entry(&entries, Some("codex")).map(ProviderEntry::instance_id), Some("codex"));
}

#[test]
fn kiro_default_model_keeps_the_session_choice_until_models_arrive() {
    let mut kiro: ServerProvider = serde_json::from_value(provider("kiro", "kiro", "ready", json!([]))).unwrap();
    assert_eq!(default_model(&kiro), Some("default"));
    kiro.models = serde_json::from_value(json!([model("live-model", json!({"isDefault": true}))])).unwrap();
    assert_eq!(default_model(&kiro), Some("live-model"));
}

#[test]
fn model_options_hide_models_and_follow_the_user_order() {
    let config = config(
        json!([provider("codex", "codex", "ready", json!([
            model("a", json!({})), model("b", json!({})), model("c", json!({})), model("d", json!({"isCustom": true}))
        ]))]),
        json!({"providerModelPreferences": {"codex": {"hiddenModels": ["b", "d"], "modelOrder": ["c"]}}}),
    );
    let slugs: Vec<&str> = model_options(&config.settings, &config.providers[0]).iter().map(|m| m.slug.as_str()).collect();
    assert_eq!(slugs, vec!["c", "a", "d"]);
}

#[test]
fn model_names_drop_the_sub_provider_qualifier() {
    let model: ProviderModel = serde_json::from_value(json!({
        "slug": "x", "name": "Anthropic: Claude Opus", "shortName": "Opus", "subProvider": "anthropic", "isCustom": false
    }))
    .unwrap();
    assert_eq!(model_name(&model), "Claude Opus");
    assert_eq!(trigger_model_name(&model), "Opus");
    let plain: ProviderModel = serde_json::from_value(json!({"slug": "x", "name": "Anthropically", "subProvider": "anthropic", "isCustom": false})).unwrap();
    assert_eq!(model_name(&plain), "Anthropically");
}

#[test]
fn search_ranks_exact_names_then_prefixes_and_boosts_favorites() {
    let config = config(
        json!([provider("codex", "codex", "ready", json!([
            {"slug": "luna", "name": "GPT Luna", "isCustom": false},
            {"slug": "sol", "name": "GPT Sol", "isCustom": false}
        ]))]),
        json!({}),
    );
    let entries = entries(&config, None);
    let (luna, sol) = (&config.providers[0].models[0], &config.providers[0].models[1]);
    assert_eq!(search_score(luna, &entries[0], false, "gpt luna"), Some(-1000));
    assert!(search_score(sol, &entries[0], false, "lun").is_none());
    let plain = search_score(luna, &entries[0], false, "luna").unwrap();
    assert_eq!(search_score(luna, &entries[0], true, "luna"), Some(plain - 24));
    assert!(search_score(luna, &entries[0], false, "codex").is_some());
    assert!(search_score(luna, &entries[0], false, "gln").is_some());
}

#[test]
fn initials_split_on_punctuation_and_keep_inner_apostrophes() {
    assert_eq!(initials("Codex"), "CO");
    assert_eq!(initials("Replay (dev)"), "RD");
    assert_eq!(initials("Theo's Codex"), "TC");
    assert_eq!(initials("Codex 'hub'"), "CH");
    assert_eq!(initials("  "), "");
}

#[test]
fn exhausted_windows_follow_each_providers_pools() {
    let provider = |driver: &str, windows: Value| -> ServerProvider {
        serde_json::from_value(json!({"instanceId": driver, "driver": driver, "enabled": true, "installed": true, "status": "ready",
            "models": [], "usageLimits": {"checkedAt": "2026-11-02T00:00:00Z", "windows": windows}})).unwrap()
    };
    let spent = |id: &str| json!({"id": id, "kind": "other", "label": id, "usedPercent": 100});
    let now = 1_793_613_600_000;
    let cursor = provider("cursor", json!([spent("apiPercentUsed")]));
    assert!(exhausted_window(&cursor, now, "claude-sonnet").is_some());
    assert!(exhausted_window(&cursor, now, "composer-1").is_none());
    let antigravity = provider("antigravity", json!([spent("gemini-pro")]));
    assert!(exhausted_window(&antigravity, now, "gemini-3").is_some());
    assert!(exhausted_window(&antigravity, now, "claude-opus").is_none());
    let opencode = provider("opencode", json!([spent("go_monthly")]));
    assert!(exhausted_window(&opencode, now, "anthropic/claude").is_none());
    assert!(exhausted_window(&opencode, now, "opencode-go/x").is_some());
    let unparseable = provider("codex", json!([{"id": "w", "kind": "weekly", "label": "w", "usedPercent": 100, "resetsAt": "later"}]));
    assert!(exhausted_window(&unparseable, now, "gpt").is_some());
}

#[test]
fn accents_and_badges_mark_instances_apart() {
    let config = config(
        json!([
            {"instanceId": "codex", "driver": "codex", "enabled": true, "installed": true, "status": "ready", "models": []},
            {"instanceId": "codex_work", "driver": "codex", "enabled": true, "installed": true, "status": "ready", "models": [], "accentColor": " #ffcc00 "},
            {"instanceId": "claudeAgent", "driver": "claudeAgent", "enabled": true, "installed": true, "status": "ready", "models": [], "accentColor": "red"}
        ]),
        json!({"providerInstances": {"codex_work": {"driver": "codex"}}}),
    );
    let entries = entries(&config, None);
    assert_eq!(accent_color(entries[1].provider), Some(0xffcc00ff));
    assert_eq!(accent_color(entries[2].provider), None);
    assert!(accent_wants_black(0xffcc00ff));
    assert!(!accent_wants_black(0x1b4ed8ff));
    assert!(shows_instance_badge(&entries[0], &entries));
    assert!(!shows_instance_badge(&entries[2], &entries));
}

#[test]
fn status_refresh_matches_the_native_subscription_capabilities() {
    assert_eq!(super::RefreshProviders::Status("work-codex").payload(), serde_json::json!({
        "instanceId": "work-codex", "refreshModels": true, "workspaceSnapshotsOnDemand": true, "usageLimitsCommand": true
    }));
    assert_eq!(super::RefreshProviders::Settings.payload().get("usageLimitsCommand"), Some(&serde_json::json!(true)));
    assert_eq!(super::RefreshProviders::Limits.payload().get("usageLimitsCommand"), Some(&serde_json::json!(true)));
    assert_eq!(
        super::RefreshProviders::Workspace { instance_id: "codex", cwd: "/repo" }.payload().get("usageLimitsCommand"),
        Some(&serde_json::json!(true)),
    );
    assert_eq!(super::RefreshProviders::Status("work-codex").payload().get("usageLimitsCommand"), crate::commands::subscribe_server_config().get("usageLimitsCommand"));
}

#[test]
fn status_refresh_requires_this_environments_authenticated_read_grant() {
    use crate::auth::AuthSessionState;
    let session = |value| serde_json::from_value::<AuthSessionState>(value).unwrap();
    let read = session(serde_json::json!({"authenticated":true,"permissions":["orchestration:read"]}));
    let operate = session(serde_json::json!({"authenticated":true,"permissions":["orchestration:operate","providers:manage"]}));
    let revoked = session(serde_json::json!({"authenticated":true,"permissions":[],"scopes":["orchestration:read"]}));
    let legacy = session(serde_json::json!({"authenticated":true,"scopes":["orchestration:read"]}));
    let signed_out = session(serde_json::json!({"authenticated":false,"permissions":["orchestration:read"]}));
    for (grants, expected) in [(None,false),(Some(&read),true),(Some(&operate),false),(Some(&revoked),false),(Some(&legacy),true),(Some(&signed_out),false)] {
        assert_eq!(super::can_refresh(grants), expected);
    }
}


#[test]
fn status_refresh_invalidates_pending_when_its_context_changes() {
    let read = serde_json::from_value(json!({"authenticated":true,"permissions":["orchestration:read"]})).unwrap();
    for (connection, thread, instance) in [(Some(2),Some("thread-a"),Some("codex")), (Some(1),Some("thread-b"),Some("codex")), (Some(1),Some("thread-a"),Some("claudeAgent")), (None,None,None)] {
        let mut refresh = StatusRefresh::default();
        refresh.set_target(Some(1), Some("thread-a"), Some("codex"));
        let old = refresh.start(Some(&read)).unwrap();
        assert!(refresh.pending());
        assert!(refresh.start(Some(&read)).is_none());
        refresh.set_target(Some(1), Some("thread-a"), Some("codex"));
        assert!(refresh.pending(), "unchanged context preserves the request");
        refresh.set_target(connection, thread, instance);
        assert!(!refresh.pending(), "a different context must not inherit Refreshing...");
        assert!(!refresh.finish(&old));
    }
}

#[test]
fn status_refresh_ignores_old_completions_even_after_returning_to_the_same_target() {
    let read = serde_json::from_value(json!({"authenticated":true,"permissions":["orchestration:read"]})).unwrap();
    let mut refresh = StatusRefresh::default();
    refresh.set_target(Some(1), Some("thread-a"), Some("codex"));
    assert!(refresh.start(None).is_none());
    let old = refresh.start(Some(&read)).unwrap();
    refresh.reset();
    assert!(!refresh.pending());
    refresh.set_target(Some(1), Some("thread-a"), Some("codex"));
    let current = refresh.start(Some(&read)).unwrap();
    assert_eq!(old.payload(), current.payload());
    assert_ne!(old, current);
    assert!(!refresh.finish(&old));
    assert!(refresh.pending(), "a stale completion cannot release a newer request");
    assert!(refresh.finish(&current));
    assert!(!refresh.pending());
    assert!(!refresh.finish(&current));
    assert!(refresh.start(Some(&read)).is_some(), "success or failure may be retried");
}

#[test]
fn custom_model_slugs_matching_aliases_stay_exact_and_cwd_snapshots_override_models() {
    use crate::composer::effective_selection;
    use crate::model::ModelSelection;
    use crate::server_config::WorkspaceSnapshot;

    // #226 (`pingdotgg/t3code#29789`): custom models whose slug looks like a driver alias keep their exact slug.
    let config = config(
        json!([provider("codex", "codex", "ready", json!([
            model("gpt-5.4", json!({"isDefault": true})),
            model("5.3", json!({"isCustom": true})),
            model("gpt-5-codex", json!({"isCustom": true}))
        ]))]),
        json!({}),
    );
    let entries = entries(&config, None);
    for custom_slug in ["5.3", "gpt-5-codex"] {
        let picked = ModelSelection { instance_id: "codex".into(), model: custom_slug.into(), options: None };
        let resolved = effective_selection(&entries, Some(&picked)).unwrap();
        assert_eq!(resolved.model, custom_slug);
        assert!(model_options(&config.settings, entries[0].provider).iter().any(|m| m.slug == custom_slug));
    }

    // #33511: modelCatalogOnly refresh payload.
    assert_eq!(
        RefreshProviders::ModelCatalog.payload(),
        json!({
            "modelCatalogOnly": true,
            "workspaceSnapshotsOnDemand": true,
            "usageLimitsCommand": true,
        })
    );

    // #24585: workspace snapshot models override machine models for that cwd.
    let opencode: ServerProvider = serde_json::from_value(provider("opencode", "opencode", "ready", json!([model("global-only", json!({}))]))).unwrap();
    let snap: WorkspaceSnapshot = serde_json::from_value(json!({
        "cwd": "/repo/proj",
        "checkedAt": "2026-11-02T00:00:00.000Z",
        "models": [model("proj/custom", json!({}))],
        "modelsPending": true,
        "slashCommands": [],
        "skills": []
    }))
    .unwrap();
    assert_eq!(models_for_cwd(&opencode, None).iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(), ["global-only"]);
    assert_eq!(models_for_cwd(&opencode, Some(&snap)).iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(), ["proj/custom"]);
    assert_eq!(snap.models_pending, Some(true));

    // Multi-model trigger summary formats 2 names cleanly and summarizes 3+.
    let picks = [
        ModelSelection { instance_id: "codex".into(), model: "a".into(), options: None },
        ModelSelection { instance_id: "claudeAgent".into(), model: "b".into(), options: None },
        ModelSelection { instance_id: "cursor".into(), model: "c".into(), options: None },
    ];
    assert_eq!(format_multiple_model_label(&picks[..2], |s| s.model.clone()), "a, b");
    assert_eq!(format_multiple_model_label(&picks, |s| s.model.clone()), "a, b, 1 more");
}

#[test]
fn fx_sub_providers_retained_models_catalog_gaps_and_dedup() {
    // `pingdotgg/t3code#29388`: fx models group under subProviders with unavailableReason.
    let mut fx_provider = provider(
        "fx",
        "fx",
        "ready",
        json!([
            model("openai/gpt-5", json!({"name": "GPT-5", "subProvider": "openai"})),
            model("anthropic/claude-4", json!({"name": "Claude 4", "subProvider": "anthropic"})),
            model("other-model", json!({"name": "Other"}))
        ]),
    );
    fx_provider["subProviders"] = json!([
        {"name": "openai", "label": "OpenAI", "available": true},
        {"name": "anthropic", "label": "Anthropic", "available": false, "unavailableReason": "API key missing"}
    ]);
    let fx: ServerProvider = serde_json::from_value(fx_provider).unwrap();
    let opts = model_options(&ServerSettings::default(), &fx);
    let sub_providers = fx.sub_providers.as_deref().unwrap();
    let (ungrouped, groups) = group_models_by_sub_provider(&opts, sub_providers, |m| m.sub_provider.as_deref());
    assert_eq!(ungrouped.len(), 1);
    assert_eq!(ungrouped[0].slug, "other-model");
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].sub_provider.name, "openai");
    assert!(groups[0].sub_provider.unavailable_reason.is_none());
    assert_eq!(groups[0].models.len(), 1);
    assert_eq!(groups[1].sub_provider.name, "anthropic");
    assert_eq!(groups[1].sub_provider.unavailable_reason.as_deref(), Some("API key missing"));

    // `pingdotgg/t3code#39114`: warning provider with `modelsRetained: true` stays selectable.
    let mut retained_val = provider("codex", "codex", "warning", json!([model("gpt-5.4", json!({}))]));
    retained_val["modelsRetained"] = json!(true);
    let retained_cfg = config(json!([retained_val]), json!({}));
    let retained_entries = entries(&retained_cfg, None);
    assert!(!retained_entries[0].is_healthy());
    assert!(retained_entries[0].is_ready());

    // `pingdotgg/t3code#37528`: Pi & ACP preserve unavailable model selection during catalog gaps.
    let pi_cfg = config(json!([provider("pi", "pi", "ready", json!([model("pi-a", json!({}))]))]), json!({}));
    let pi_entries = entries(&pi_cfg, None);
    assert!(should_preserve_unavailable_selection(
        &pi_entries[0].provider.driver,
        "pi-old",
        &pi_entries[0].provider.models,
        &[],
    ));
    let codex_cfg = config(json!([provider("codex", "codex", "ready", json!([model("gpt-5.4", json!({}))]))]), json!({}));
    let codex_entries = entries(&codex_cfg, None);
    assert!(!should_preserve_unavailable_selection(
        &codex_entries[0].provider.driver,
        "gpt-4",
        &codex_entries[0].provider.models,
        &[],
    ));

    // `pingdotgg/t3code#31590`: duplicate model slugs are deduplicated.
    let dup: ServerProvider = serde_json::from_value(provider(
        "opencode",
        "opencode",
        "ready",
        json!([model("dup-slug", json!({"name": "First"})), model("dup-slug", json!({"name": "Second"}))]),
    ))
    .unwrap();
    let deduped = model_options_for_cwd(&ServerSettings::default(), &dup, None);
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].name, "First");
}
