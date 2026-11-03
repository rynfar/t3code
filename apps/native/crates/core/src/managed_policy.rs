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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn locks_and_copy_match_web() {
        let p = json!({"organization":"Example Corp", "lockedSettings":["storage.logs", "providers"], "limits":{"clients":["desktop"],"webhookTasks":false,"providers":{"allowedDrivers":["codex"],"lockedInstanceFields":{"codex":["config.binaryPath","environment"]}}}});
        for path in [
            "storage",
            "storage.logs",
            "storage.logs.days",
            "providers.codex",
        ] {
            assert!(setting_locked(Some(&p), path));
        }
        for path in ["storage.log", "storage.logsOther", "provider"] {
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
    }
}
