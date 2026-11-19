//! Usage limit source configuration and ID derivation:
//! `apps/web/src/components/settings/AddUsageLimitSourceDialog.logic.ts`,
//! `AddUsageLimitSourceDialog.tsx`, and `UsageProviderSettings.tsx` (`pingdotgg/t3code#27760`).

use serde_json::{Map, Value, json};

/// A configured usage limit source entry from `ServerSettings.usageLimitSources`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredUsageLimitSource {
    pub id: String,
    pub kind: String,
    pub url: String,
    pub label: Option<String>,
    pub enabled: bool,
}

impl ConfiguredUsageLimitSource {
    /// Display title (`UsageProviderSettings.tsx:52`): trimmed label, else URL.
    pub fn display_label(&self) -> &str {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .unwrap_or(&self.url)
    }

    /// Secondary description (`UsageProviderSettings.tsx:59-61`):
    /// `CLI Proxy[ · Disabled][ · <url>]`.
    pub fn description(&self) -> String {
        let mut text = String::from("CLI Proxy");
        if !self.enabled {
            text.push_str(" · Disabled");
        }
        if self.display_label() != self.url {
            text.push_str(" · ");
            text.push_str(&self.url);
        }
        text
    }
}

/// Management key written into `usageLimitSources.<id>.managementKey`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagementKeyValue {
    Literal(String),
    Reference { provider: String, reference: String },
}

impl ManagementKeyValue {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Literal(value) => json!(value),
            Self::Reference { provider, reference } => json!({
                "provider": provider,
                "reference": reference,
            }),
        }
    }
}

/// Decodes `ServerSettings.usage_limit_sources` into configured rows.
pub fn decode_usage_limit_sources(value: &Value) -> Vec<ConfiguredUsageLimitSource> {
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(id, entry)| {
            let obj = entry.as_object()?;
            let url = obj.get("url").and_then(Value::as_str)?.to_owned();
            let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("cliproxy").to_owned();
            let label = obj.get("label").and_then(Value::as_str).map(str::to_owned);
            let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            Some(ConfiguredUsageLimitSource { id: id.clone(), kind, url, label, enabled })
        })
        .collect()
}

/// `sourceIdFromUrl` (`AddUsageLimitSourceDialog.logic.ts`): reuses the existing ID when a saved
/// source already has the exact same URL, otherwise derives a slugged `cliproxy-<host>` ID with
/// `-2`, `-3`, … suffixes to avoid clobbering another hub.
pub fn source_id_from_url(url: &str, sources: &Map<String, Value>) -> String {
    if let Some((existing_id, _)) = sources
        .iter()
        .find(|(_, source)| source.get("url").and_then(Value::as_str) == Some(url))
    {
        return existing_id.clone();
    }
    let host = match url::Url::parse(url) {
        Ok(parsed) => match (parsed.host_str(), parsed.port()) {
            (Some(h), Some(port)) => format!("{h}:{port}"),
            (Some(h), None) => h.to_owned(),
            (None, _) => String::new(),
        },
        Err(_) => url.to_owned(),
    };
    let mut slug = String::with_capacity(host.len());
    let mut in_dash = false;
    for ch in host.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() || matches!(lower, '.' | '-') {
            slug.push(lower);
            in_dash = false;
        } else if !in_dash {
            slug.push('-');
            in_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    let base = format!("cliproxy-{}", if slug.is_empty() { "hub" } else { slug });
    let mut id = base.clone();
    let mut suffix = 2u32;
    while sources.contains_key(&id) {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}

/// Builds the `server.updateSettings` patch to add or update a CLIProxyAPI usage limit source.
pub fn upsert_usage_limit_source_patch(
    id: &str,
    url: &str,
    label: Option<&str>,
    management_key: &ManagementKeyValue,
) -> Value {
    let mut entry = Map::new();
    entry.insert("kind".into(), json!("cliproxy"));
    if let Some(trimmed) = label.map(str::trim).filter(|l| !l.is_empty()) {
        entry.insert("label".into(), json!(trimmed));
    }
    entry.insert("url".into(), json!(url.trim()));
    entry.insert("managementKey".into(), management_key.to_json());
    entry.insert("enabled".into(), json!(true));
    let mut sources = Map::new();
    sources.insert(id.to_owned(), Value::Object(entry));
    json!({ "usageLimitSources": Value::Object(sources) })
}

/// Builds the `server.updateSettings` patch to remove a usage limit source (`{ [id]: null }`).
pub fn remove_usage_limit_source_patch(id: &str) -> Value {
    let mut sources = Map::new();
    sources.insert(id.to_owned(), Value::Null);
    json!({ "usageLimitSources": Value::Object(sources) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_id_from_url_retains_distinct_hubs_and_updates_matching_url() {
        for (first_url, second_url) in [
            ("http://proxy.test:8318/hub-a", "http://proxy.test:8318/hub-b"),
            ("localhost:8318", "localhost:9318"),
        ] {
            let mut sources = Map::new();
            let third_url = format!("{second_url}/extra");
            let entries = [
                (first_url, "First hub", "first-key"),
                (second_url, "Second hub", "second-key"),
                (third_url.as_str(), "Third hub", "third-key"),
            ];
            let mut ids = Vec::new();
            for (url, label, key) in entries {
                let id = source_id_from_url(url, &sources);
                ids.push(id.clone());
                sources.insert(
                    id,
                    json!({
                        "kind": "cliproxy",
                        "url": url,
                        "label": label,
                        "managementKey": key,
                        "enabled": true,
                    }),
                );
            }
            assert_eq!(sources.len(), 3, "all three hubs must stay distinct for {first_url}");
            // Re-adding the first URL reuses its exact ID instead of creating a duplicate.
            assert_eq!(source_id_from_url(first_url, &sources), ids[0]);
        }
        // Check exact generated IDs for host + port vs bare host:port.
        let empty = Map::new();
        assert_eq!(source_id_from_url("http://proxy.test:8318/hub-a", &empty), "cliproxy-proxy.test-8318");
        assert_eq!(source_id_from_url("https://hub.example.ts.net:443/x", &empty), "cliproxy-hub.example.ts.net");
        assert_eq!(source_id_from_url("localhost:8318", &empty), "cliproxy-hub");
        assert_eq!(source_id_from_url("not a url", &empty), "cliproxy-not-a-url");
    }

    #[test]
    fn decode_and_patch_literal_and_secret_reference_sources() {
        let raw = json!({
            "cliproxy-hub": {
                "kind": "cliproxy",
                "label": "Team Hub",
                "url": "https://hub.example.ts.net:8318",
                "enabled": true
            },
            "cliproxy-disabled": {
                "kind": "cliproxy",
                "url": "http://10.0.0.2:8318",
                "enabled": false
            }
        });
        let decoded = decode_usage_limit_sources(&raw);
        assert_eq!(decoded.len(), 2);
        let team = decoded.iter().find(|s| s.id == "cliproxy-hub").unwrap();
        assert_eq!(team.display_label(), "Team Hub");
        assert_eq!(team.description(), "CLI Proxy · https://hub.example.ts.net:8318");
        let disabled = decoded.iter().find(|s| s.id == "cliproxy-disabled").unwrap();
        assert_eq!(disabled.display_label(), "http://10.0.0.2:8318");
        assert_eq!(disabled.description(), "CLI Proxy · Disabled");

        let ref_patch = upsert_usage_limit_source_patch(
            "cliproxy-hub",
            " https://hub.example.ts.net:8318 ",
            Some("  "),
            &ManagementKeyValue::Reference {
                provider: "op-work".into(),
                reference: "op://Vault/Hub/key".into(),
            },
        );
        assert_eq!(
            ref_patch,
            json!({
                "usageLimitSources": {
                    "cliproxy-hub": {
                        "kind": "cliproxy",
                        "url": "https://hub.example.ts.net:8318",
                        "managementKey": { "provider": "op-work", "reference": "op://Vault/Hub/key" },
                        "enabled": true
                    }
                }
            })
        );
        assert_eq!(
            remove_usage_limit_source_patch("cliproxy-hub"),
            json!({ "usageLimitSources": { "cliproxy-hub": null } })
        );
    }
}
