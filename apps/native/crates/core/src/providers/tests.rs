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
            "off": {"driver": "codex", "enabled": true, "config": {"enabled": false}}
        },
        "projectSettingsOverrides": {"p1": {"providerInstances": {"cursor": {"enabled": true}}}}
    }))
    .unwrap();
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
