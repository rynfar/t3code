//! Ports `contracts/src/managedPolicy.ts` and `web/src/state/managedPolicy.ts`.
use serde_json::Value;

pub fn organization(policy: &Value) -> &str {
    policy["organization"]
        .as_str()
        .unwrap_or("Your organization")
}

pub fn attribution(policy: &Value) -> String {
    format!("This environment is managed by {}.", organization(policy))
}

pub fn setting_locked(policy: Option<&Value>, path: &str) -> bool {
    policy
        .and_then(|p| p["lockedSettings"].as_array())
        .is_some_and(|paths| {
            paths.iter().filter_map(Value::as_str).any(|locked| {
                locked == path
                    || path
                        .strip_prefix(locked)
                        .is_some_and(|tail| tail.starts_with('.'))
                    || locked
                        .strip_prefix(path)
                        .is_some_and(|tail| tail.starts_with('.'))
            })
        })
}

#[derive(Clone, Copy)]
pub enum Capability {
    RemoteClients,
    Connect,
    WebhookTasks,
    BrowserCookieImport,
}

pub fn capability_denial(policy: Option<&Value>, capability: Capability) -> Option<String> {
    let p = policy?;
    let (key, label) = match capability {
        Capability::RemoteClients => ("clients", "connecting from other devices"),
        Capability::Connect => ("connect", "T3 Connect"),
        Capability::WebhookTasks => ("webhookTasks", "webhook tasks"),
        Capability::BrowserCookieImport => ("browserCookieImport", "importing browser cookies"),
    };
    let denied = match capability {
        Capability::RemoteClients => p["limits"][key]
            .as_array()
            .is_some_and(|clients| !clients.iter().any(|v| v == "remote")),
        _ => p["limits"][key] == false,
    };
    denied.then(|| format!("{}'s policy does not allow {label}.", organization(p)))
}

pub fn driver_allowed(policy: Option<&Value>, driver: &str) -> bool {
    policy
        .and_then(|p| p["limits"]["providers"]["allowedDrivers"].as_array())
        .is_none_or(|drivers| drivers.iter().any(|v| v == driver))
}

pub fn driver_denial(policy: Option<&Value>, driver: &str, label: &str) -> Option<String> {
    let p = policy?;
    (!driver_allowed(Some(p), driver)).then(|| {
        format!(
            "{}'s policy does not allow the {label} provider.",
            organization(p)
        )
    })
}

pub fn instance_field_locked(policy: Option<&Value>, driver: &str, path: &str) -> bool {
    policy
        .and_then(|p| p["limits"]["providers"]["lockedInstanceFields"][driver].as_array())
        .is_some_and(|paths| {
            paths.iter().filter_map(Value::as_str).any(|locked| {
                locked == path
                    || path
                        .strip_prefix(locked)
                        .is_some_and(|tail| tail.starts_with('.'))
            })
        })
}

pub fn instance_reason(policy: &Value) -> String {
    format!("Managed by {}.", organization(policy))
}

/// `firstManagedDenial` (`web/src/state/managedPolicy.ts`).
pub fn first_managed_denial<'a, I, F>(policies: I, mut deny: F) -> Option<String>
where
    I: IntoIterator<Item = Option<&'a Value>>,
    F: FnMut(Option<&'a Value>) -> Option<String>,
{
    for policy in policies {
        if let Some(denial) = deny(policy) {
            return Some(denial);
        }
    }
    None
}

/// `managedTelemetryOffMessage` (`web/src/state/managedPolicy.ts`).
pub fn managed_telemetry_off_message(policy: Option<&Value>) -> Option<String> {
    let p = policy?;
    (p["limits"]["telemetry"] == false)
        .then(|| format!("{} turned off anonymous usage data.", organization(p)))
}

/// `settingLockReason`: reason string when `path` is locked by the organization's policy.
pub fn setting_lock_reason(policy: Option<&Value>, path: &str) -> Option<String> {
    let p = policy?;
    setting_locked(Some(p), path).then(|| instance_reason(p))
}

/// `SettingsRow` (`settingsLayout.tsx`): first policy whose `lockedSettings` locks any of `paths`.
pub fn setting_lock_reason_for_any<'a, I>(policies: I, paths: &[&str]) -> Option<String>
where
    I: IntoIterator<Item = Option<&'a Value>>,
{
    if paths.is_empty() {
        return None;
    }
    first_managed_denial(policies, |policy| {
        let p = policy?;
        paths
            .iter()
            .any(|path| setting_locked(Some(p), path))
            .then(|| instance_reason(p))
    })
}

const RUNTIME_MODE_ORDER: [&str; 4] = [
    "approval-required",
    "auto-accept-edits",
    "auto",
    "full-access",
];

fn runtime_mode_rank(mode: &str) -> Option<usize> {
    RUNTIME_MODE_ORDER.iter().position(|candidate| *candidate == mode)
}

fn runtime_mode_policy_label(mode: &str) -> &str {
    match mode {
        "approval-required" => "Supervised",
        "auto-accept-edits" => "Auto-accept edits",
        "auto" => "Auto",
        "full-access" => "Full access",
        other => other,
    }
}

/// `isRuntimeModeAbove` (`contracts/src/providerPolicy.ts`).
pub fn is_runtime_mode_above(mode: &str, ceiling: Option<&str>) -> bool {
    let Some(ceiling) = ceiling else {
        return false;
    };
    matches!(
        (runtime_mode_rank(mode), runtime_mode_rank(ceiling)),
        (Some(m), Some(c)) if m > c
    )
}

/// `managedRuntimeModeCeilingMessage` (`contracts/src/managedPolicy.ts`).
pub fn runtime_mode_ceiling_message(organization: &str, ceiling: &str) -> String {
    format!(
        "{organization} limits threads to {}.",
        runtime_mode_policy_label(ceiling)
    )
}

/// `managedRuntimeModeDenial` (`contracts/src/managedPolicy.ts`).
pub fn runtime_mode_denial(policy: Option<&Value>, mode: &str) -> Option<String> {
    let p = policy?;
    let ceiling = p["limits"]["maxRuntimeMode"].as_str()?;
    is_runtime_mode_above(mode, Some(ceiling))
        .then(|| runtime_mode_ceiling_message(organization(p), ceiling))
}

/// `clampManagedRuntimeMode` (`contracts/src/managedPolicy.ts`) over `RuntimeMode`.
pub fn clamp_typed_runtime_mode(
    policy: Option<&Value>,
    mode: crate::server_config::RuntimeMode,
    modes: &[crate::server_config::RuntimeMode],
) -> Option<crate::server_config::RuntimeMode> {
    let ceiling = policy.and_then(|p| p["limits"]["maxRuntimeMode"].as_str());
    if !is_runtime_mode_above(mode.as_str(), ceiling) {
        return Some(mode);
    }
    modes
        .iter()
        .rev()
        .copied()
        .find(|candidate| !is_runtime_mode_above(candidate.as_str(), ceiling))
}

/// Whether `limits.providers.allowedDrivers` is present and empty (`ProviderSettingsPanel.tsx`).
pub fn providers_allowed_drivers_empty(policy: Option<&Value>) -> bool {
    policy
        .and_then(|p| p["limits"]["providers"]["allowedDrivers"].as_array())
        .is_some_and(|drivers| drivers.is_empty())
}

/// Whether `limits.providers.allowedDrivers` is present at all (`ProviderSettingsPanel.tsx`).
pub fn providers_allowed_drivers_restricted(policy: Option<&Value>) -> bool {
    policy
        .and_then(|p| p["limits"]["providers"]["allowedDrivers"].as_array())
        .is_some()
}

/// `ManagedInstanceLocks` (`web/src/state/managedPolicy.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedInstanceLocks {
    pub config_keys: std::collections::BTreeSet<String>,
    pub environment_names: std::collections::BTreeSet<String>,
    pub whole_environment: bool,
    pub reason: String,
}

/// `managedInstanceLocks` (`web/src/state/managedPolicy.ts`).
pub fn managed_instance_locks(policy: Option<&Value>, driver: &str) -> Option<ManagedInstanceLocks> {
    let p = policy?;
    let paths = p["limits"]["providers"]["lockedInstanceFields"][driver].as_array()?;
    if paths.is_empty() {
        return None;
    }
    let mut config_keys = std::collections::BTreeSet::new();
    let mut environment_names = std::collections::BTreeSet::new();
    let mut whole_environment = false;
    for path in paths.iter().filter_map(Value::as_str) {
        if let Some(key) = path.strip_prefix("config.") {
            config_keys.insert(key.to_owned());
        } else if let Some(name) = path.strip_prefix("environment.") {
            environment_names.insert(name.to_owned());
        } else if path == "environment" {
            whole_environment = true;
        }
    }
    Some(ManagedInstanceLocks {
        config_keys,
        environment_names,
        whole_environment,
        reason: instance_reason(p),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn locks_and_copy_match_web() {
        let p = json!({"organization":"Example Corp", "lockedSettings":["storageCleanup.logsAfterDays", "providerInstances"], "limits":{"clients":["desktop"],"webhookTasks":false,"telemetry":false,"providers":{"allowedDrivers":["codex"],"lockedInstanceFields":{"codex":["config.binaryPath","environment","environment.FOO"]}}}});
        for path in [
            "storageCleanup",
            "storageCleanup.logsAfterDays",
            "storageCleanup.logsAfterDays.nested",
            "providerInstances.codex",
        ] {
            assert!(setting_locked(Some(&p), path));
        }
        for path in ["storageCleanup.log", "storageCleanup.logsAfterDaysOther", "providerInstance"] {
            assert!(!setting_locked(Some(&p), path));
        }
        assert_eq!(
            capability_denial(Some(&p), Capability::RemoteClients).as_deref(),
            Some("Example Corp's policy does not allow connecting from other devices.")
        );
        assert_eq!(
            capability_denial(Some(&p), Capability::WebhookTasks).as_deref(),
            Some("Example Corp's policy does not allow webhook tasks.")
        );
        assert!(capability_denial(Some(&p), Capability::Connect).is_none());
        assert!(driver_denial(Some(&p), "codex", "Codex").is_none());
        assert_eq!(
            driver_denial(Some(&p), "cursor", "Cursor").as_deref(),
            Some("Example Corp's policy does not allow the Cursor provider.")
        );
        assert!(instance_field_locked(Some(&p), "codex", "environment.NEW"));
        assert!(instance_field_locked(
            Some(&p),
            "codex",
            "config.binaryPath"
        ));
        assert!(!instance_field_locked(
            Some(&p),
            "codex",
            "config.launchArgs"
        ));
        assert!(!setting_locked(None, "providers"));
        assert_eq!(
            attribution(&p),
            "This environment is managed by Example Corp."
        );
        assert_eq!(
            managed_telemetry_off_message(Some(&p)).as_deref(),
            Some("Example Corp turned off anonymous usage data.")
        );
        assert_eq!(
            setting_lock_reason(Some(&p), "storageCleanup.logsAfterDays").as_deref(),
            Some("Managed by Example Corp.")
        );
        assert_eq!(
            first_managed_denial([None, Some(&p)], |pol| capability_denial(pol, Capability::WebhookTasks)).as_deref(),
            Some("Example Corp's policy does not allow webhook tasks.")
        );
        let locks = managed_instance_locks(Some(&p), "codex").expect("codex locks");
        assert!(locks.config_keys.contains("binaryPath"));
        assert!(locks.environment_names.contains("FOO"));
        assert!(locks.whole_environment);
        assert_eq!(locks.reason, "Managed by Example Corp.");
        assert!(managed_instance_locks(Some(&p), "cursor").is_none());
    }

    #[test]
    fn runtime_mode_ceiling_and_provider_restrictions_match_web() {
        use crate::server_config::RuntimeMode;
        let policy = json!({
            "organization": "Example Corp",
            "lockedSettings": ["guestSharingEnabled", "linkedEnvironmentsEnabled"],
            "limits": {
                "maxRuntimeMode": "auto-accept-edits",
                "providers": { "allowedDrivers": [] }
            }
        });
        assert!(!is_runtime_mode_above("approval-required", Some("auto-accept-edits")));
        assert!(!is_runtime_mode_above("auto-accept-edits", Some("auto-accept-edits")));
        assert!(is_runtime_mode_above("auto", Some("auto-accept-edits")));
        assert!(is_runtime_mode_above("full-access", Some("auto-accept-edits")));
        assert!(!is_runtime_mode_above("full-access", None));

        assert_eq!(
            runtime_mode_denial(Some(&policy), "full-access").as_deref(),
            Some("Example Corp limits threads to Auto-accept edits.")
        );
        assert!(runtime_mode_denial(Some(&policy), "auto-accept-edits").is_none());
        assert!(runtime_mode_denial(None, "full-access").is_none());

        assert_eq!(
            clamp_typed_runtime_mode(
                Some(&policy),
                RuntimeMode::FullAccess,
                &[RuntimeMode::ApprovalRequired, RuntimeMode::AutoAcceptEdits, RuntimeMode::FullAccess],
            ),
            Some(RuntimeMode::AutoAcceptEdits)
        );
        assert_eq!(
            clamp_typed_runtime_mode(
                Some(&policy),
                RuntimeMode::FullAccess,
                &[RuntimeMode::ApprovalRequired, RuntimeMode::FullAccess],
            ),
            Some(RuntimeMode::ApprovalRequired)
        );
        assert_eq!(
            clamp_typed_runtime_mode(
                Some(&policy),
                RuntimeMode::FullAccess,
                &[RuntimeMode::Auto, RuntimeMode::FullAccess],
            ),
            None
        );
        assert_eq!(
            clamp_typed_runtime_mode(Some(&policy), RuntimeMode::ApprovalRequired, &RuntimeMode::ALL),
            Some(RuntimeMode::ApprovalRequired)
        );

        assert!(providers_allowed_drivers_empty(Some(&policy)));
        assert!(providers_allowed_drivers_restricted(Some(&policy)));
        assert!(!providers_allowed_drivers_empty(None));
        assert!(!providers_allowed_drivers_restricted(None));

        let non_empty = json!({"organization": "Acme", "limits": {"providers": {"allowedDrivers": ["codex"]}}});
        assert!(!providers_allowed_drivers_empty(Some(&non_empty)));
        assert!(providers_allowed_drivers_restricted(Some(&non_empty)));

        assert_eq!(
            setting_lock_reason_for_any([None, Some(&non_empty), Some(&policy)], &["linkedEnvironmentsEnabled"]).as_deref(),
            Some("Managed by Example Corp.")
        );
        assert!(setting_lock_reason_for_any([Some(&policy)], &["promptSuggestionsEnabled"]).is_none());
    }
}
