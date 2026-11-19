//! Shared server settings splitting and per-environment compatibility filtering.
//! Ports `packages/client-runtime/src/state/sharedSettings.ts` and
//! `isModelSelectionProviderEnabled` from `packages/shared/src/serverSettings.ts`.

use serde_json::{Map, Value};

use crate::providers;
use crate::server_config::ServerSettings;
use crate::settings::providers::text_generators::decode_text_generators;

/// `SHARED_SERVER_SETTING_KEYS` in `packages/client-runtime/src/state/sharedSettings.ts`.
pub const SHARED_SERVER_SETTING_KEYS: &[&str] = &[
    "continueThreadsAfterServerUpdate",
    "sidebarAutoSettleAfterDays",
    "sidebarAutoSettleOnMerge",
    "autoResumeLimitedThreads",
    "snoozeLimitedThreads",
    "snoozePresets",
    "newWorktreesStartFromOrigin",
    "sourceControlWritingStyle",
    "textGenerationModelSelection",
];

/// Default `textGenerationModelSelection.instanceId` when absent on the wire (`contracts/src/settings.ts`).
const DEFAULT_TEXT_GENERATION_INSTANCE_ID: &str = "codex";

/// Result of splitting a `ServerSettingsPatch` into shared (multi-environment fan-out) and
/// local (target-environment only) entries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SplitServerPatch {
    pub shared_patch: Map<String, Value>,
    pub local_patch: Map<String, Value>,
}

/// Returns true when `key` is a shared server setting that fans out to connected environments.
pub fn is_shared_server_setting_key(key: &str) -> bool {
    SHARED_SERVER_SETTING_KEYS.contains(&key)
}

/// `splitSharedServerPatch` (`packages/client-runtime/src/state/sharedSettings.ts`).
pub fn split_shared_server_patch(patch: &Map<String, Value>) -> SplitServerPatch {
    let mut shared_patch = Map::new();
    let mut local_patch = Map::new();
    for (key, value) in patch {
        if is_shared_server_setting_key(key) {
            shared_patch.insert(key.clone(), value.clone());
        } else {
            local_patch.insert(key.clone(), value.clone());
        }
    }
    SplitServerPatch { shared_patch, local_patch }
}

/// `isModelSelectionProviderEnabled` (`packages/shared/src/serverSettings.ts`):
/// a text generator entry takes precedence over a provider instance; unconfigured instances
/// fall back to `isUnconfiguredDefaultInstanceEnabled` (`codex`, `claudeAgent`).
pub fn is_model_selection_provider_enabled(settings: &ServerSettings, instance_id: &str) -> bool {
    let generators = decode_text_generators(&settings.text_generators);
    if let Some(generator) = generators.get(instance_id) {
        return generator.enabled;
    }
    if let Some(raw_gen) = settings
        .text_generators
        .as_object()
        .and_then(|map| map.get(instance_id))
        .and_then(Value::as_object)
        && let Some(enabled) = raw_gen.get("enabled").and_then(Value::as_bool)
    {
        return enabled;
    }
    providers::instance_enabled(settings, None, instance_id)
}

fn driver_for_instance<'a>(settings: Option<&'a ServerSettings>, instance_id: &'a str) -> &'a str {
    settings
        .and_then(|s| s.provider_instances.get(instance_id))
        .map(|cfg| cfg.driver.as_str())
        .filter(|driver| !driver.is_empty())
        .unwrap_or(instance_id)
}

fn current_text_gen_instance_id(settings: &ServerSettings) -> String {
    super::text_generation_model_selection(settings)
        .map(|sel| sel.instance_id)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| DEFAULT_TEXT_GENERATION_INSTANCE_ID.to_owned())
}

/// `filterSharedServerPatch` (`packages/client-runtime/src/state/sharedSettings.ts`):
/// filters a shared patch for `target_settings` relative to `source_settings`.
///
/// - When `!target_is_source` and `patch` sets `textGenerationModelSelection`:
///   drops `textGenerationModelSelection` if `target_settings` is missing, if the provider
///   driver for the selected `instanceId` differs between source and target, or if the target's
///   provider/text generator for the merged selection is disabled.
/// - When `target_settings` is missing, also drops `continueThreadsAfterServerUpdate`.
pub fn filter_shared_server_patch(
    patch: &Map<String, Value>,
    target_settings: Option<&ServerSettings>,
    source_settings: Option<&ServerSettings>,
    target_is_source: bool,
) -> Map<String, Value> {
    let mut filtered = patch.clone();
    let effective_source = source_settings.or(target_settings);

    if !target_is_source
        && let Some(selection_patch) = filtered.get("textGenerationModelSelection")
        && !selection_patch.is_null()
    {
        let patch_instance_id = selection_patch
            .get("instanceId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let source_instance_id = patch_instance_id
            .map(str::to_owned)
            .or_else(|| effective_source.map(current_text_gen_instance_id));

        let drop_selection = match target_settings {
            None => true,
            Some(target) => {
                let driver_mismatch = source_instance_id.as_deref().is_some_and(|id| {
                    driver_for_instance(effective_source, id) != driver_for_instance(Some(target), id)
                });
                let target_instance_id = patch_instance_id
                    .map(str::to_owned)
                    .unwrap_or_else(|| current_text_gen_instance_id(target));
                driver_mismatch || !is_model_selection_provider_enabled(target, &target_instance_id)
            }
        };

        if drop_selection {
            filtered.remove("textGenerationModelSelection");
        }
    }

    if target_settings.is_none() {
        filtered.remove("continueThreadsAfterServerUpdate");
    }

    filtered
}

/// `supportsSharedSettingsSync` (`packages/client-runtime/src/state/sharedSettings.ts`).
pub fn supports_shared_settings_sync(connected: bool, has_server_config: bool) -> bool {
    connected && has_server_config
}

/// `requiredScopesForServerSettingsPatch` (`packages/contracts/src/settings.ts:1990-2019`):
/// determines the exact auth scopes a `server.updateSettings` payload requires.
pub fn required_scopes_for_server_settings_payload(payload: &Value) -> Vec<&'static str> {
    let mut changes_settings = false;
    let mut changes_providers = payload
        .get("providerInstanceMutation")
        .is_some_and(|v| !v.is_null());
    let mut changes_access = false;

    if let Some(patch) = payload.get("patch").and_then(Value::as_object) {
        for (key, value) in patch {
            match key.as_str() {
                "requireConnectDeviceApproval" => changes_access = true,
                "providers" | "providerInstances" | "usageLimitSources" | "textGenerators" => {
                    changes_providers = true;
                }
                "secretProviders" => {
                    changes_providers = true;
                    if value
                        .as_object()
                        .is_some_and(|entries| entries.values().any(Value::is_null))
                    {
                        changes_settings = true;
                    }
                }
                _ => changes_settings = true,
            }
        }
    }

    let mut scopes = Vec::new();
    if changes_settings || (!changes_providers && !changes_access) {
        scopes.push("settings:write");
    }
    if changes_providers {
        scopes.push("providers:manage");
    }
    if changes_access {
        scopes.push("access:write");
    }
    scopes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings_from(value: Value) -> ServerSettings {
        serde_json::from_value(value).unwrap()
    }

    fn map_from(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn splits_all_shared_keys_from_local_keys() {
        let patch = map_from(json!({
            "continueThreadsAfterServerUpdate": true,
            "sidebarAutoSettleAfterDays": 14,
            "sidebarAutoSettleOnMerge": false,
            "autoResumeLimitedThreads": true,
            "snoozeLimitedThreads": true,
            "snoozePresets": [],
            "newWorktreesStartFromOrigin": false,
            "sourceControlWritingStyle": { "mode": "concise" },
            "textGenerationModelSelection": { "instanceId": "codex", "model": "gpt-5.4-mini" },
            "addProjectBaseDirectory": "/Users/me/src",
            "guestSharingEnabled": true,
            "defaultRuntimeMode": "auto"
        }));

        let split = split_shared_server_patch(&patch);
        assert_eq!(split.shared_patch.len(), SHARED_SERVER_SETTING_KEYS.len());
        for key in SHARED_SERVER_SETTING_KEYS {
            assert!(split.shared_patch.contains_key(*key), "missing shared key {key}");
            assert!(!split.local_patch.contains_key(*key), "shared key leaked to local: {key}");
        }
        assert_eq!(split.local_patch.len(), 3);
        assert_eq!(split.local_patch["addProjectBaseDirectory"], json!("/Users/me/src"));
        assert_eq!(split.local_patch["guestSharingEnabled"], json!(true));
        assert_eq!(split.local_patch["defaultRuntimeMode"], json!("auto"));
    }

    #[test]
    fn checks_model_selection_provider_enabled_with_text_generator_precedence() {
        let defaults = settings_from(json!({}));
        assert!(is_model_selection_provider_enabled(&defaults, "codex"));
        assert!(is_model_selection_provider_enabled(&defaults, "claudeAgent"));
        assert!(!is_model_selection_provider_enabled(&defaults, "cursor"));
        assert!(!is_model_selection_provider_enabled(&defaults, "custom-openai"));

        let configured = settings_from(json!({
            "providerInstances": {
                "codex": { "driver": "codex", "enabled": false },
                "cursor": { "driver": "cursor", "enabled": true },
                "work-claude": { "driver": "claudeAgent" }
            },
            "textGenerators": {
                "codex": { "kind": "openaiCompatible", "baseUrl": "https://api.openai.com/v1", "enabled": true, "label": "Shadow Gen" },
                "disabled-gen": { "kind": "anthropic", "baseUrl": "https://api.anthropic.com/v1", "enabled": false }
            }
        }));
        // Text generator takes precedence over disabled provider instance of the same id.
        assert!(is_model_selection_provider_enabled(&configured, "codex"));
        assert!(!is_model_selection_provider_enabled(&configured, "disabled-gen"));
        assert!(is_model_selection_provider_enabled(&configured, "cursor"));
        assert!(is_model_selection_provider_enabled(&configured, "work-claude"));
    }

    #[test]
    fn filters_text_generation_selection_on_driver_mismatch_or_disabled_provider() {
        let source = settings_from(json!({
            "providerInstances": {
                "team": { "driver": "claudeAgent", "enabled": true },
                "cursor": { "driver": "cursor", "enabled": true }
            }
        }));
        let compatible_target = settings_from(json!({
            "providerInstances": {
                "team": { "driver": "claudeAgent", "enabled": true },
                "cursor": { "driver": "cursor", "enabled": true }
            }
        }));
        let mismatched_driver_target = settings_from(json!({
            "providerInstances": {
                "team": { "driver": "codex", "enabled": true }
            }
        }));
        let disabled_target = settings_from(json!({
            "providerInstances": {
                "team": { "driver": "claudeAgent", "enabled": false }
            }
        }));

        let patch = map_from(json!({
            "sidebarAutoSettleOnMerge": false,
            "textGenerationModelSelection": { "instanceId": "team", "model": "claude-haiku-4-5" }
        }));

        // Compatible target keeps both keys.
        let kept = filter_shared_server_patch(&patch, Some(&compatible_target), Some(&source), false);
        assert!(kept.contains_key("textGenerationModelSelection"));
        assert_eq!(kept["sidebarAutoSettleOnMerge"], json!(false));

        // Mismatched driver drops textGenerationModelSelection but keeps other shared keys.
        let mismatch = filter_shared_server_patch(&patch, Some(&mismatched_driver_target), Some(&source), false);
        assert!(!mismatch.contains_key("textGenerationModelSelection"));
        assert_eq!(mismatch["sidebarAutoSettleOnMerge"], json!(false));

        // Disabled target provider drops textGenerationModelSelection.
        let disabled = filter_shared_server_patch(&patch, Some(&disabled_target), Some(&source), false);
        assert!(!disabled.contains_key("textGenerationModelSelection"));
        assert_eq!(disabled["sidebarAutoSettleOnMerge"], json!(false));

        // Source target always keeps textGenerationModelSelection even if disabled on target.
        let self_target = filter_shared_server_patch(&patch, Some(&disabled_target), Some(&source), true);
        assert!(self_target.contains_key("textGenerationModelSelection"));
    }

    #[test]
    fn omits_continue_threads_and_text_gen_when_target_settings_missing() {
        let patch = map_from(json!({
            "continueThreadsAfterServerUpdate": true,
            "textGenerationModelSelection": { "instanceId": "codex", "model": "gpt-5.4-mini" },
            "snoozeLimitedThreads": true
        }));

        let filtered = filter_shared_server_patch(&patch, None, None, false);
        assert!(!filtered.contains_key("continueThreadsAfterServerUpdate"));
        assert!(!filtered.contains_key("textGenerationModelSelection"));
        assert_eq!(filtered["snoozeLimitedThreads"], json!(true));

        // Even when target_is_source, missing settings omits continueThreadsAfterServerUpdate.
        let source_missing = filter_shared_server_patch(&patch, None, None, true);
        assert!(!source_missing.contains_key("continueThreadsAfterServerUpdate"));
        assert!(source_missing.contains_key("textGenerationModelSelection"));
    }

    #[test]
    fn supports_shared_settings_sync_requires_connected_and_config() {
        assert!(supports_shared_settings_sync(true, true));
        assert!(!supports_shared_settings_sync(true, false));
        assert!(!supports_shared_settings_sync(false, true));
        assert!(!supports_shared_settings_sync(false, false));
    }

    #[test]
    fn required_scopes_match_contracts_settings_patch_rules() {
        assert_eq!(
            required_scopes_for_server_settings_payload(&json!({ "patch": { "sidebarAutoSettleOnMerge": false } })),
            vec!["settings:write"]
        );
        assert_eq!(
            required_scopes_for_server_settings_payload(&json!({ "patch": { "usageLimitSources": [] } })),
            vec!["providers:manage"]
        );
        assert_eq!(
            required_scopes_for_server_settings_payload(&json!({ "patch": { "requireConnectDeviceApproval": true } })),
            vec!["access:write"]
        );
        assert_eq!(
            required_scopes_for_server_settings_payload(&json!({ "patch": { "secretProviders": { "op": null } } })),
            vec!["settings:write", "providers:manage"]
        );
        assert_eq!(
            required_scopes_for_server_settings_payload(&json!({
                "patch": {},
                "providerInstanceMutation": { "kind": "remove", "instanceId": "codex-2" }
            })),
            vec!["providers:manage"]
        );
    }
}
