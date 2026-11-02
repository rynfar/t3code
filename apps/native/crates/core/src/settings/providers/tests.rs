use serde_json::{Value, json};

use super::auth::{self, AccountAction};
use super::background::ProviderPanelSettings;
use super::*;
use crate::server_config::{ServerConfig, ServerConfigState, ServerConfigStreamEvent};

fn settings(value: Value) -> ServerSettings {
    serde_json::from_value(value).expect("settings decode")
}

fn provider(value: Value) -> ServerProvider {
    let mut base = json!({
        "instanceId": "codex", "driver": "codex", "enabled": true, "installed": true, "status": "ready",
        "auth": { "status": "authenticated" }, "version": "0.50.0", "checkedAt": "2026-11-01T10:00:00.000Z", "models": [],
    });
    for (key, value) in value.as_object().expect("object") {
        base[key] = value.clone();
    }
    serde_json::from_value(base).expect("provider decode")
}

fn ids(rows: &[InstanceRow]) -> Vec<&str> {
    rows.iter().map(|row| row.instance_id.as_str()).collect()
}

#[test]
fn rows_list_running_defaults_then_custom_instances_by_driver() {
    let settings = settings(json!({ "providerInstances": {
        "a_codex_work": { "driver": "codex", "displayName": "Work" },
        "b_grok": { "driver": "grok" },
        "c_future": { "driver": "someFutureDriver" },
        "grok": { "driver": "grok", "enabled": true },
    } }));
    let rows = instance_rows(&settings, &[], None);
    // Codex and Claude always show; Grok's default shows because it was configured; Cursor is
    // hidden without a server instance; unknown drivers come last.
    assert_eq!(ids(&rows), ["codex", "a_codex_work", "claudeAgent", "grok", "b_grok", "c_future"]);
    let grok = rows.iter().find(|row| row.instance_id == "grok").expect("grok row");
    assert!(grok.is_default && grok.is_dirty);
    assert!(!rows.iter().find(|row| row.instance_id == "codex").expect("codex").is_dirty);
}

#[test]
fn rows_show_cursor_only_where_the_server_has_it_and_the_requested_default_slot() {
    let cursor = provider(json!({ "instanceId": "cursor", "driver": "cursor" }));
    assert!(!ids(&instance_rows(&settings(json!({})), &[], None)).contains(&"cursor"));
    // Cursor is off by default, so its slot shows only when targeted (or enabled).
    assert!(!ids(&instance_rows(&settings(json!({})), std::slice::from_ref(&cursor), None)).contains(&"cursor"));
    assert!(ids(&instance_rows(&settings(json!({})), &[cursor], Some("cursor"))).contains(&"cursor"));
    // The ACP Registry has no default slot even when targeted.
    assert!(!ids(&instance_rows(&settings(json!({})), &[], Some("acpRegistry"))).contains(&"acpRegistry"));
}

#[test]
fn summaries_follow_the_web_copy() {
    let ready = provider(json!({ "auth": { "status": "authenticated", "label": "ChatGPT Pro" } }));
    assert_eq!(summary(Some(&ready), true).headline, "Authenticated \u{00b7} ChatGPT Pro");
    assert_eq!(summary(Some(&ready), false).headline, "Disabled");
    assert_eq!(summary(None, true).headline, "Checking provider status");
    let missing = provider(json!({ "installed": false, "status": "error" }));
    assert_eq!(summary(Some(&missing), true), Summary { headline: "Not found".into(), detail: Some("CLI not detected on PATH.".into()) });
    let unauthenticated = provider(json!({ "status": "error", "auth": { "status": "unauthenticated" }, "message": "Run login." }));
    assert_eq!(summary(Some(&unauthenticated), true).headline, "Not authenticated");
    assert_eq!(status_key(Some(&unauthenticated), true), StatusKey::Error);
    let antigravity =
        provider(json!({ "driver": "antigravity", "instanceId": "antigravity", "status": "warning", "auth": { "status": "unknown" } }));
    assert_eq!(status_key(Some(&antigravity), true), StatusKey::Neutral);
    assert_eq!(summary(Some(&antigravity), true).headline, "Installed \u{00b7} Sign-in not checked");
}

#[test]
fn version_labels_and_advisories() {
    assert_eq!(version_label(Some("0.50.0")).as_deref(), Some("v0.50.0"));
    assert_eq!(version_label(Some("nightly")).as_deref(), Some("nightly"));
    assert_eq!(version_label(Some("agy_acp_server_20260131_42_rc1")).as_deref(), Some("2026-01-31 rc1"));
    assert_eq!(version_label(Some("agy_acp_server_20260131_42")).as_deref(), Some("2026-01-31"));
    assert_eq!(version_label(None), None);

    let behind: VersionAdvisory = serde_json::from_value(json!({
        "status": "behind_latest", "latestVersion": "0.51.0", "updateCommand": "npm i -g @openai/codex", "canUpdate": true, "message": null,
    }))
    .expect("advisory");
    let view = advisory(Some(&behind), None, true).expect("update advisory");
    assert_eq!(view.title, "Update available");
    assert_eq!(view.detail, "Update available: install v0.51.0.");
    assert_eq!(view.update_command.as_deref(), Some("npm i -g @openai/codex"));

    let broken: CompatibilityAdvisory = serde_json::from_value(json!({
        "status": "broken", "message": null, "recommendedVersion": "0.49.0", "recommendedRange": null,
    }))
    .expect("compat");
    let view = advisory(Some(&behind), Some(&broken), true).expect("compat advisory");
    assert_eq!((view.title.as_str(), view.detail.as_str(), view.strong), ("Known broken version", "Use v0.49.0 for full support.", true));
    // A recommended version replaces the generic update command with "Install v0.49.0".
    assert_eq!(view.update_command, None);
    assert_eq!(view.target_version.as_deref(), Some("0.49.0"));
    // Disabled instances never show compatibility.
    assert_eq!(advisory(Some(&behind), Some(&broken), false).map(|view| view.title), Some("Update available".into()));
}

#[test]
fn instance_view_reports_update_progress_and_one_click_updates() {
    let live = provider(json!({
        "versionAdvisory": { "status": "behind_latest", "latestVersion": "0.51.0", "updateCommand": "npm i -g x", "canUpdate": true },
        "updateState": { "status": "running", "message": "Installing 0.51.0" },
    }));
    let row = InstanceRow { instance_id: "codex".into(), driver: "codex".into(), explicit: None, is_default: true, is_dirty: false };
    let view = instance_view(&row, Some(&live), false, false);
    assert_eq!(view.update_progress.as_deref(), Some("Installing 0.51.0"));
    assert_eq!(view.version_action, Some(VersionAction::UpdateNow));
    assert_eq!(view.version.as_deref(), Some("v0.50.0"));
    assert_eq!(instance_view(&row, Some(&live), false, true).version_action, None);

    let failed = provider(json!({ "updateState": { "status": "failed", "message": "npm exited 1" } }));
    let view = instance_view(&row, Some(&failed), false, false);
    assert_eq!((view.update_progress, view.update_problem.as_deref()), (None, Some("npm exited 1")));
    assert_eq!(instance_view(&row, Some(&failed), true, false).update_progress.as_deref(), Some("Starting update"));
}

#[test]
fn instance_edits_build_upsert_mutations_and_keep_other_keys() {
    let settings = settings(json!({ "providerInstances": { "codex_work": {
        "driver": "codex", "displayName": "Work", "accentColor": "#2563eb",
        "environment": [{ "name": "OPENAI_API_KEY", "value": "", "sensitive": true, "valueRedacted": true }],
        "config": { "binaryPath": "/opt/codex" },
    } } }));
    let previous = settings.provider_instances["codex_work"].clone();
    let next = with_display_name(&with_enabled(&previous, false), "  Personal ");
    let payload = upsert_payload(&settings, "codex_work", &previous, &next);
    assert_eq!(
        payload,
        json!({ "patch": {}, "providerInstanceMutation": { "operation": "upsert", "instanceId": "codex_work", "instance": {
            "driver": "codex", "displayName": "Personal", "accentColor": "#2563eb",
            "environment": [{ "name": "OPENAI_API_KEY", "value": "", "sensitive": true, "valueRedacted": true }],
            "enabled": false, "config": { "binaryPath": "/opt/codex" },
        } } })
    );
    let cleared = with_accent(&with_display_name(&previous, " "), "blue");
    assert_eq!((cleared.display_name, cleared.accent_color), (None, None));
    assert_eq!(remove_payload("codex_work"), json!({ "patch": {}, "providerInstanceMutation": { "operation": "remove", "instanceId": "codex_work" } }));
}

#[test]
fn disabling_the_text_generation_instance_resets_that_model() {
    let settings = settings(json!({ "textGenerationModelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku" } }));
    let previous = ProviderInstanceConfig { driver: "claudeAgent".into(), ..Default::default() };
    let payload = upsert_payload(&settings, "claudeAgent", &previous, &with_enabled(&previous, false));
    assert_eq!(payload["patch"]["textGenerationModelSelection"]["instanceId"], "codex");
    let payload = upsert_payload(&settings, "claudeAgent", &previous, &with_display_name(&previous, "Claude"));
    assert_eq!(payload["patch"], json!({}));
}

#[test]
fn custom_models_add_and_remove_without_rewriting_saved_entries() {
    let instance: ProviderInstanceConfig = serde_json::from_value(json!({ "driver": "codex", "config": { "customModels": [
        " gpt-x ", { "slug": "gpt-y", "name": "GPT Y", "capabilities": { "optionDescriptors": [] } }, "gpt-x",
    ], "binaryPath": "/opt/codex" } }))
    .expect("instance");
    let models = custom_models(&instance);
    assert_eq!(models.iter().map(|model| (model.slug.as_str(), model.name.as_str())).collect::<Vec<_>>(), [("gpt-x", "gpt-x"), ("gpt-y", "GPT Y")]);
    let added = with_custom_model_added(&instance, "gpt-z");
    assert_eq!(added.config.as_ref().unwrap()["customModels"][3], "gpt-z");
    assert_eq!(added.config.as_ref().unwrap()["binaryPath"], "/opt/codex");
    let removed = with_custom_model_removed(&instance, "gpt-x");
    assert_eq!(removed.config.as_ref().unwrap()["customModels"], json!([{ "slug": "gpt-y", "name": "GPT Y", "capabilities": { "optionDescriptors": [] } }]));
    let emptied = with_custom_model_removed(&with_custom_model_removed(&instance, "gpt-x"), "gpt-y");
    assert_eq!(emptied.config.as_ref().unwrap()["customModels"], json!([]));

    let live: Vec<ProviderModel> = serde_json::from_value(json!([{ "slug": "gpt-5", "name": "GPT-5", "isCustom": false }])).expect("models");
    assert_eq!(validate_custom_model("  ", &live, &models), Err("Enter a model slug.".into()));
    assert_eq!(validate_custom_model("gpt-5", &live, &models), Err("That model is already built in.".into()));
    assert_eq!(validate_custom_model("gpt-y", &live, &models), Err("That custom model is already saved.".into()));
    assert_eq!(validate_custom_model(" gpt-6 ", &live, &models), Ok("gpt-6".into()));
    assert!(custom_models(&ProviderInstanceConfig { driver: "antigravity".into(), ..instance }).is_empty());
}

#[test]
fn models_group_favorites_visible_then_hidden_in_the_users_order() {
    let live: Vec<ProviderModel> = serde_json::from_value(json!([
        { "slug": "a", "name": "A", "isCustom": false },
        { "slug": "b", "name": "B", "isCustom": false },
        { "slug": "c", "name": "C", "isCustom": false },
        { "slug": "d", "name": "D", "isCustom": false },
    ]))
    .expect("models");
    let custom = vec![CustomModel { slug: "z".into(), name: "Z".into(), capabilities: None }];
    let grouped = group_models(models_for_display(&live, &custom), &["c".into()], &["a".into(), "z".into()], &["d".into(), "b".into()]);
    let order: Vec<(&str, ModelGroup)> = grouped.iter().map(|(model, group)| (model.slug.as_str(), *group)).collect();
    // Custom models never hide.
    assert_eq!(
        order,
        [("c", ModelGroup::Favorite), ("d", ModelGroup::Visible), ("b", ModelGroup::Visible), ("z", ModelGroup::Visible), ("a", ModelGroup::Hidden)]
    );
    assert_eq!(models_summary(&grouped), "5 models \u{00b7} 1 favorite \u{00b7} 1 hidden");
    assert_eq!(moved_order(&grouped, "b", false), Some(vec!["c".into(), "b".into(), "d".into(), "z".into(), "a".into()]));
    // Moves stay inside a group.
    assert_eq!(moved_order(&grouped, "d", false), None);
    assert_eq!(moved_order(&grouped, "z", true), None);
}

#[test]
fn model_preference_payloads() {
    assert_eq!(
        hidden_payload("codex", &[("gpt-5".into(), true)]),
        json!({ "patch": { "setModelsHidden": [{ "provider": "codex", "model": "gpt-5", "hidden": true }] } })
    );
    assert_eq!(
        order_payload("codex", &["b".into(), "a".into()]),
        json!({ "patch": { "setProviderModelOrder": { "provider": "codex", "modelOrder": ["b", "a"] } } })
    );
    let models = vec![
        DisplayModel { slug: "a".into(), name: "A".into(), is_custom: false, capabilities: None },
        DisplayModel { slug: "z".into(), name: "Z".into(), is_custom: true, capabilities: None },
    ];
    assert_eq!(bulk_hidden_payload("codex", &models, &[]).unwrap()["patch"]["setModelsHidden"][0]["hidden"], true);
    assert!(all_built_in_hidden(&models, &["a".into()]));
    assert_eq!(bulk_hidden_payload("codex", &models, &["a".into()]).unwrap()["patch"]["setModelsHidden"][0]["hidden"], false);
}

#[test]
fn add_dialog_ids_follow_the_web() {
    let codex = find_driver("codex").expect("codex");
    let existing = reserved_instance_ids(&settings(json!({ "providerInstances": { "codex_work": { "driver": "codex" } } })));
    assert_eq!(proposed_instance_id(codex, "Codex", &existing), "codex_2");
    assert_eq!(proposed_instance_id(codex, "", &existing), "codex_2");
    assert_eq!(proposed_instance_id(codex, "Work", &existing), "codex_work_2");
    assert_eq!(proposed_instance_id(codex, "  Side Project! ", &existing), "codex_side_project");
    let acp = find_driver(ACP_REGISTRY).expect("acp");
    assert_eq!(proposed_instance_id(acp, "", &existing), "acpRegistry_custom");
    assert_eq!(derive_instance_id("grok", "***"), "");

    assert_eq!(validate_instance_id("", &existing).as_deref(), Some("Instance ID is required."));
    assert_eq!(
        validate_instance_id("9lives", &existing).as_deref(),
        Some("Instance ID must start with a letter and use only letters, digits, '-', or '_'.")
    );
    assert_eq!(validate_instance_id("codex", &existing).as_deref(), Some("An instance named 'codex' already exists."));
    assert_eq!(validate_instance_id(&"a".repeat(65), &existing).as_deref(), Some("Instance ID must be 64 characters or fewer."));
    assert_eq!(validate_instance_id("grok_work", &existing), None);

    assert_eq!(
        create_payload("codex_side", &new_instance("codex", " Side ", "#ABCDEF")),
        json!({ "patch": {}, "providerInstanceMutation": { "operation": "create", "instanceId": "codex_side", "instance": {
            "driver": "codex", "displayName": "Side", "accentColor": "#ABCDEF", "enabled": true, "config": { "setupMode": "existing" },
        } } })
    );
    assert_eq!(new_instance("grok", "", "nope"), ProviderInstanceConfig { driver: "grok".into(), enabled: Some(true), ..Default::default() });
}

#[test]
fn refresh_and_update_payloads_and_replies() {
    assert_eq!(refresh_payload(), json!({ "refreshModels": true, "reloadSecrets": true, "workspaceSnapshotsOnDemand": true }));
    assert_eq!(update_payload("codex", "codex_work", None), json!({ "provider": "codex", "instanceId": "codex_work" }));
    assert_eq!(update_payload("codex", "codex", Some("0.49.0"))["targetVersion"], "0.49.0");
    let providers = decode_providers_reply(json!({ "providers": [
        { "instanceId": "codex", "driver": "codex", "enabled": true, "installed": true, "status": "ready", "auth": { "status": "unknown" }, "checkedAt": "2026-11-01T10:00:00.000Z", "models": [] },
    ] }))
    .expect("providers");
    assert_eq!(providers.len(), 1);
    assert_eq!(managed_acp_agent(&serde_json::from_value(json!({ "driver": "acpRegistry", "config": { "agentId": "gemini" } })).unwrap()).as_deref(), Some("gemini"));
    assert_eq!(managed_acp_agent(&serde_json::from_value(json!({ "driver": "acpRegistry", "config": { "agentId": "x", "source": "local" } })).unwrap()), None);
}

#[test]
fn last_checked_labels_and_their_next_change() {
    let now = crate::sidebar::time::parse_iso("2026-11-01T10:05:30.000Z").unwrap();
    let live = vec![
        provider(json!({ "checkedAt": "2026-11-01T10:00:00.000Z" })),
        provider(json!({ "instanceId": "claudeAgent", "checkedAt": "2026-11-01T10:02:00.000Z" })),
    ];
    assert_eq!(last_checked_label(&live, now).as_deref(), Some("Checked 3m ago"));
    assert_eq!(ms_until_label_changes(&live, now), Some(30_000));
    assert_eq!(last_checked_label(&[provider(json!({ "checkedAt": "garbage" }))], now).as_deref(), Some("Checked unavailable"));
    assert_eq!(last_checked_label(&[], now), None);
}

#[test]
fn restamped_provider_pushes_keep_fresh_stamps_but_report_no_change() {
    let mut state = ServerConfigState::default();
    let config: ServerConfig = serde_json::from_value(json!({ "providers": [
        { "instanceId": "codex", "driver": "codex", "enabled": true, "installed": true, "status": "ready", "auth": { "status": "unknown" }, "checkedAt": "2026-11-01T10:00:00.000Z", "models": [] },
    ] }))
    .expect("config");
    assert!(state.apply(ServerConfigStreamEvent::Snapshot { config: Box::new(config) }));
    let push = |checked: &str, status: &str| {
        serde_json::from_value::<ServerConfigStreamEvent>(json!({ "type": "providerStatuses", "payload": { "providers": [
            { "instanceId": "codex", "driver": "codex", "enabled": true, "installed": true, "status": status, "auth": { "status": "unknown" }, "checkedAt": checked, "models": [] },
        ] } }))
        .expect("event")
    };
    assert!(!state.apply(push("2026-11-01T10:05:00.000Z", "ready")));
    let providers = &state.config.as_ref().unwrap().providers;
    assert_eq!(last_checked(providers), Some("2026-11-01T10:05:00.000Z"));
    assert!(state.apply(push("2026-11-01T10:06:00.000Z", "warning")));
}

#[test]
fn health_interval_resolves_presets_overrides_and_legacy_fields() {
    let panel = |value: Value| serde_json::from_value::<ProviderPanelSettings>(value).expect("panel");
    assert_eq!(panel(json!({})).health_interval_seconds(), 300);
    assert_eq!(panel(json!({ "backgroundActivity": { "profile": "performance" } })).health_interval_seconds(), 60);
    let custom = panel(json!({ "backgroundActivity": { "profile": "custom", "baseProfile": "battery-saver", "overrides": { "providerHealthRefreshInterval": 120000 } } }));
    assert_eq!((custom.health_interval_seconds(), custom.default_health_interval_seconds()), (120, 900));
    // Legacy fields count while `backgroundActivity` is untouched.
    let legacy = panel(json!({ "providerHealthRefreshInterval": 90000 }));
    assert_eq!((legacy.health_interval_seconds(), legacy.default_health_interval_seconds()), (90, 300));

    assert_eq!(
        panel(json!({ "backgroundActivity": { "profile": "performance" } })).health_interval_payload(Some(30)),
        json!({ "patch": { "backgroundActivity": { "schemaVersion": 1, "profile": "custom", "baseProfile": "performance", "overrides": {
            "automaticGitFetchInterval": 15000, "providerHealthRefreshInterval": 30000, "hostPowerMonitorActiveInterval": 30000,
            "hostPowerMonitorIdleInterval": 120000, "pauseWhenHostLocked": true, "pauseWhenHostLowPower": false,
            "pauseWhenClientLowPower": false, "pauseWhenOnBattery": false,
        } } } })
    );
    let reset = custom.health_interval_payload(None);
    assert!(reset["patch"]["backgroundActivity"]["overrides"].get("providerHealthRefreshInterval").is_none());
    assert_eq!(reset["patch"]["backgroundActivity"]["baseProfile"], "battery-saver");
}

#[test]
fn idle_compaction_reads_and_writes_the_field_the_server_uses() {
    let panel = |value: Value| serde_json::from_value::<ProviderPanelSettings>(value).expect("panel");
    assert_eq!(panel(json!({})).idle_compaction(), "onReturn");
    assert_eq!(panel(json!({ "compactIdleThreads": true })).idle_compaction(), "beforeCacheExpires");
    assert_eq!(panel(json!({ "compactIdleThreads": "off" })).idle_compaction(), "off");
    assert_eq!(panel(json!({ "compactIdleThreads": false, "idleCompaction": "off" })).idle_compaction(), "off");
    assert_eq!(panel(json!({ "idleCompaction": "onReturn" })).idle_compaction_payload("off"), json!({ "patch": { "idleCompaction": "off" } }));
    assert_eq!(panel(json!({ "compactIdleThreads": "onReturn" })).idle_compaction_payload("off"), json!({ "patch": { "compactIdleThreads": "off" } }));
    assert_eq!(
        panel(json!({ "compactIdleThreads": false })).idle_compaction_payload("beforeCacheExpires"),
        json!({ "patch": { "compactIdleThreads": true } })
    );
}

fn auth_state(value: Value) -> auth::AuthState {
    auth::decode_auth_state(value).expect("auth state")
}

#[test]
fn auth_states_decode_forward_compatibly() {
    let state = auth_state(json!({
        "instanceId": "cursor", "phase": "waiting", "flowId": "f1", "authorizationUrl": null, "expiresAt": null, "message": null,
        "methods": [{ "id": "browser", "name": "Sign in with Cursor", "description": null, "type": "agent" }, { "id": 7 }],
        "interaction": { "type": "deviceCode", "id": "i1", "url": "https://example.com/device", "userCode": "ABCD-1234" },
    }));
    assert_eq!(state.methods.as_ref().map(Vec::len), Some(1));
    assert_eq!(state.url(), Some("https://example.com/device"));
    let unknown = auth_state(json!({ "instanceId": "x", "phase": "waiting", "flowId": "f", "authorizationUrl": "https://a", "expiresAt": null, "message": null, "interaction": { "type": "hologram", "id": "i" } }));
    assert_eq!(unknown.interaction, None);
    assert_eq!(unknown.url(), Some("https://a"));
    let setup_error = crate::rpc::RpcFailure {
        tag: Some("ProviderSetupError".into()),
        message: "ProviderSetupError".into(),
        error: json!({ "_tag": "ProviderSetupError", "instanceId": "codex", "operation": "auth.subscribe", "detail": "This provider does not support sign-in in T3 Code." }),
    };
    assert_eq!(auth::failure_message(&setup_error), "This provider does not support sign-in in T3 Code.");
    assert_eq!(auth::failure_message(&crate::rpc::RpcFailure { tag: None, message: "Interrupted".into(), error: Value::Null }), "Interrupted");
}

#[test]
fn auth_payloads() {
    let state = auth_state(json!({
        "instanceId": "cursor", "phase": "waiting", "flowId": "f1", "authorizationUrl": null, "expiresAt": null, "message": null,
        "methods": [{ "id": "browser", "name": "Browser", "description": null, "type": "agent" }],
        "interaction": { "type": "browser", "id": "i1", "url": "https://cursor.com/login", "requiresConsent": true, "acceptsCallback": true },
    }));
    assert_eq!(auth::start_payload("cursor", Some("browser"), Some(&state)), json!({ "instanceId": "cursor", "methodId": "browser" }));
    assert_eq!(auth::start_payload("cursor", Some("unlisted"), Some(&state)), json!({ "instanceId": "cursor" }));
    assert_eq!(
        auth::accept_browser_payload("cursor", &state),
        Some(json!({ "instanceId": "cursor", "flowId": "f1", "interactionId": "i1", "response": { "type": "browser", "action": "accept" } }))
    );
    assert_eq!(auth::cancel_payload("cursor", &state), Some(json!({ "instanceId": "cursor", "flowId": "f1" })));
    assert_eq!(auth::complete_payload("cursor", &state, "  "), None);
    assert_eq!(
        auth::complete_payload("cursor", &state, " http://localhost:1/cb?code=x "),
        Some(json!({ "instanceId": "cursor", "flowId": "f1", "callbackUrl": "http://localhost:1/cb?code=x" }))
    );
    assert_eq!(
        auth::credentials_payload("cursor", &state, &[("apiKey".into(), "k".into())]).unwrap()["response"],
        json!({ "type": "credentials", "values": { "apiKey": "k" } })
    );
    assert!(state.requires_consent() && state.accepts_callback());
    assert_eq!(auth::logout_payload("cursor"), json!({ "instanceId": "cursor" }));
}

fn cursor(value: Value) -> ServerProvider {
    let mut fields = json!({ "instanceId": "cursor", "driver": "cursor", "setup": { "canAuthenticate": true, "canInstall": false } });
    for (key, value) in value.as_object().expect("object") {
        fields[key] = value.clone();
    }
    provider(fields)
}

#[test]
fn account_rows_offer_sign_in_cancel_and_sign_out() {
    let idle = auth_state(json!({ "instanceId": "cursor", "phase": "idle", "flowId": null, "authorizationUrl": null, "expiresAt": null, "message": null }));
    let signed_out = cursor(json!({ "auth": { "status": "unauthenticated" } }));
    let view = auth::account_view(&signed_out, Some(&idle), "MacBook", false, false);
    assert_eq!(view.description, "Sign in on MacBook.");
    assert_eq!(view.action, Some(AccountAction::Start("Sign in".into())));
    assert!(view.action_enabled && !view.can_sign_out);
    // No state yet: the button waits for it.
    assert!(!auth::account_view(&signed_out, None, "MacBook", false, false).action_enabled);

    let waiting = auth_state(json!({ "instanceId": "cursor", "phase": "waiting", "flowId": "f1", "authorizationUrl": "https://cursor.com/a", "expiresAt": null, "message": null,
        "interaction": { "type": "deviceCode", "id": "i", "url": "https://cursor.com/a", "userCode": "WXYZ" } }));
    let view = auth::account_view(&signed_out, Some(&waiting), "MacBook", false, false);
    assert_eq!(view.description, "Finish signing in in your browser.");
    assert_eq!((view.action, view.device_code.as_deref()), (Some(AccountAction::Cancel), Some("WXYZ")));
    assert!(!view.accepts_callback);

    let failed = auth_state(json!({ "instanceId": "cursor", "phase": "failed", "flowId": null, "authorizationUrl": null, "expiresAt": null, "message": "Timed out." }));
    let view = auth::account_view(&signed_out, Some(&failed), "MacBook", false, false);
    assert_eq!((view.action, view.failure.as_deref()), (Some(AccountAction::Start("Retry sign-in".into())), Some("Timed out.")));

    let signed_in = cursor(json!({ "auth": { "status": "authenticated", "email": "dev@example.com" } }));
    let view = auth::account_view(&signed_in, Some(&idle), "MacBook", false, false);
    assert_eq!((view.signed_in_email.as_deref(), view.action), (Some("dev@example.com"), Some(AccountAction::Start("Change account".into()))));
    assert!(view.can_sign_out);

    let api_key = cursor(json!({ "auth": { "status": "unauthenticated" }, "setup": { "canAuthenticate": false, "canInstall": false, "documentationUrl": "https://docs" } }));
    let view = auth::account_view(&api_key, Some(&idle), "MacBook", false, false);
    assert_eq!(view.action, Some(AccountAction::OpenDocs("https://docs".into())));

    let acp = provider(json!({ "instanceId": "gem", "driver": "acpRegistry", "auth": { "status": "unknown" } }));
    let view = auth::account_view(&acp, Some(&idle), "MacBook", false, false);
    assert_eq!(view.description, "Discovering sign-in methods\u{2026}");
    assert!(view.disabled);
}

#[test]
fn runtime_rows_follow_install_progress() {
    let antigravity = provider(json!({ "instanceId": "antigravity", "driver": "antigravity", "installed": false, "setup": { "canAuthenticate": true, "canInstall": true } }));
    let install = |value: Value| auth::decode_install_state(value).expect("install");
    let view = auth::runtime_view(&antigravity, None, None, true);
    assert_eq!(view.status, "Checking the Antigravity runtime.");
    let idle = install(json!({ "driver": "antigravity", "operationId": null, "phase": "idle", "downloadedBytes": 0, "totalBytes": 120_400_000, "version": "1.2", "installedVersion": null, "canRemove": false, "message": null }));
    let view = auth::runtime_view(&antigravity, Some(&idle), None, true);
    assert_eq!((view.status.as_str(), view.install_label.as_deref()), ("121 MB download.", Some("Install Antigravity")));
    let downloading = install(json!({ "driver": "antigravity", "operationId": "op", "phase": "downloading", "downloadedBytes": 30_000_000, "totalBytes": 120_000_000, "version": "1.2", "installedVersion": null, "canRemove": false, "message": null }));
    let view = auth::runtime_view(&antigravity, Some(&downloading), None, true);
    assert_eq!(view.status, "Downloading 30.0 MB of 120.0 MB.");
    assert_eq!(view.progress, Some(0.25));
    assert!(view.can_cancel && view.install_label.is_none());
    assert_eq!(auth::install_cancel_payload("antigravity", &downloading), Some(json!({ "instanceId": "antigravity", "operationId": "op" })));
    let outdated = install(json!({ "driver": "antigravity", "operationId": null, "phase": "succeeded", "downloadedBytes": 0, "totalBytes": null, "version": "1.3", "installedVersion": "1.2", "canRemove": true, "message": null }));
    let view = auth::runtime_view(&antigravity, Some(&outdated), None, true);
    assert_eq!((view.status.as_str(), view.install_label.as_deref(), view.can_remove), ("Installed.", Some("Update Antigravity"), true));
}
