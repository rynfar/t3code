//! Ports `ProviderSettingsForm.tsx`, `ProviderInstanceCard.tsx` and the driver settings schemas.
use crate::server_config::ProviderInstanceConfig;
use serde_json::Value;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::json;
use std::{collections::HashMap, sync::LazyLock};

#[derive(Debug, Clone, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub description: String,
    pub placeholder: String,
    pub secret: bool,
    pub persist: bool,
    pub options: Vec<(String, String)>,
}

static FIELDS: LazyLock<HashMap<String, Vec<Field>>> = LazyLock::new(|| serde_json::from_str(include_str!("fields.json")).expect("provider field metadata"));

pub fn fields(driver: &str, config: Option<&Value>) -> Vec<Field> {
    let local = driver == "acpRegistry" && config.is_some_and(|c| c["source"] == "local");
    FIELDS
        .get(driver)
        .into_iter()
        .flatten()
        .filter(|f| !local || matches!(f.key.as_str(), "source" | "commandPath"))
        .cloned()
        .map(|mut f| {
            if local && f.key == "commandPath" {
                f.label = "Executable".into();
                f.description = "Executable name or path on this environment.".into();
                f.placeholder = "e.g. dsh".into();
            }
            f
        })
        .collect()
}

pub fn managed_runtime(
    instance: &ProviderInstanceConfig,
    provider: Option<&crate::server_config::ServerProvider>,
    executable: Option<&str>,
) -> Vec<(Field, String)> {
    let paths = provider.and_then(|p| p.runtime_paths.as_ref());
    fields(&instance.driver, instance.config.as_ref())
        .into_iter()
        .filter(|f| f.key != "launchArgs")
        .map(|mut field| {
            let value = match field.key.as_str() {
                "binaryPath" => {
                    field.description = "Selected by T3 Code.".into();
                    field.placeholder = "Not installed".into();
                    executable.unwrap_or("").to_owned()
                }
                "homePath" => {
                    field.description = "Shared Codex config, sessions, and state.".into();
                    field.placeholder = "Unavailable".into();
                    paths.and_then(|p| p["homePath"].as_str()).unwrap_or("").to_owned()
                }
                _ => {
                    let path = paths.and_then(|p| p["shadowHomePath"].as_str());
                    field.description = if path.is_some() {
                        "Account-specific home sharing the Codex state above."
                    } else {
                        "This instance uses the shared Codex home directly."
                    }
                    .into();
                    field.placeholder = if paths.is_some() { "Not used" } else { "Unavailable" }.into();
                    path.unwrap_or("").to_owned()
                }
            };
            (field, value)
        })
        .collect()
}

pub fn validate_field(driver: &str, key: &str, value: &Value) -> bool {
    if key.starts_with("commandArgs:") {
        return driver == "acpRegistry" && value.is_string();
    }
    if key == "commandArgs" {
        return value.as_array().is_some_and(|args| args.iter().all(Value::is_string));
    }
    let Some(field) = FIELDS.get(driver).and_then(|fields| fields.iter().find(|f| f.key == key)) else {
        return false;
    };
    let Some(text) = value.as_str() else { return false };
    if !field.options.is_empty() && !text.is_empty() {
        return field.options.iter().any(|(v, _)| v == text);
    }
    if driver == "claudeAgent" && key == "autoCompactWindow" {
        let text = text.trim();
        return text.is_empty()
            || (text.bytes().all(|b| b.is_ascii_digit()) && !text.starts_with('0') && text.parse::<u32>().is_ok_and(|n| (100_000..=1_000_000).contains(&n)));
    }
    true
}

pub fn with_field(instance: &ProviderInstanceConfig, key: &str, value: Value) -> ProviderInstanceConfig {
    if !validate_field(&instance.driver, key, &value) {
        return instance.clone();
    }
    let mut config = instance.config.as_ref().and_then(Value::as_object).cloned().unwrap_or_default();
    if let Some(index) = key.strip_prefix("commandArgs:").and_then(|s| s.parse::<usize>().ok()) {
        let mut args = command_args(instance);
        if let Some(arg) = args.get_mut(index) {
            *arg = value.as_str().unwrap_or("").to_owned();
        } else {
            return instance.clone();
        }
        config.insert("commandArgs".into(), json!(args));
        return ProviderInstanceConfig {
            config: Some(Value::Object(config)),
            ..instance.clone()
        };
    }
    let persist = FIELDS.get(&instance.driver).into_iter().flatten().any(|f| f.key == key && f.persist);
    if !persist && value.as_str().is_some_and(|s| s.trim().is_empty()) {
        config.remove(key);
    } else {
        config.insert(key.into(), value);
    }
    ProviderInstanceConfig {
        config: (!config.is_empty()).then_some(Value::Object(config)),
        ..instance.clone()
    }
}

pub fn command_args(instance: &ProviderInstanceConfig) -> Vec<String> {
    instance
        .config
        .as_ref()
        .and_then(|c| c["commandArgs"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}
pub fn edit_command_arguments(instance: &ProviderInstanceConfig, remove: Option<usize>) -> ProviderInstanceConfig {
    let mut args = command_args(instance);
    match remove {
        Some(index) if index < args.len() => {
            args.remove(index);
        }
        Some(_) => return instance.clone(),
        None => args.push(String::new()),
    }
    with_field(instance, "commandArgs", json!(args))
}
pub fn argument_field(index: usize) -> Field {
    Field {
        key: format!("commandArgs:{index}"),
        label: format!("Argument {}", index + 1),
        description: String::new(),
        placeholder: String::new(),
        secret: false,
        persist: true,
        options: vec![],
    }
}
pub fn field_value(instance: &ProviderInstanceConfig, key: &str) -> Value {
    if let Some(index) = key.strip_prefix("commandArgs:").and_then(|s| s.parse::<usize>().ok()) {
        return command_args(instance).get(index).map_or(Value::Null, |v| json!(v));
    }
    instance.config.as_ref().and_then(|c| c.get(key)).cloned().unwrap_or(Value::Null)
}

// ProviderInstanceEnvironmentVariable encodes references beside an empty value for older clients.
pub fn decode_environment<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    let mut environment = Option::<Value>::deserialize(deserializer)?;
    if let Some(Value::Array(rows)) = &mut environment {
        for row in rows {
            if let Some(row) = row.as_object_mut() {
                if let Some(secret) = row.remove("secret") {
                    row.insert("value".into(), secret);
                }
                row.entry("value").or_insert(json!(""));
                row.entry("sensitive").or_insert(json!(false));
            }
        }
    }
    Ok(environment)
}
pub fn encode_environment<S: Serializer>(environment: &Option<Value>, serializer: S) -> Result<S::Ok, S::Error> {
    let mut environment = environment.clone();
    if let Some(Value::Array(rows)) = &mut environment {
        for row in rows {
            if let Some(row) = row.as_object_mut() {
                if let Some(reference) = row.get("value").filter(|v| v.is_object()).cloned() {
                    row.insert("secret".into(), reference);
                    row.insert("value".into(), json!(""));
                } else if row.get("value").is_some_and(Value::is_string) {
                    row.remove("secret");
                }
            }
        }
    }
    environment.serialize(serializer)
}

pub fn environment(instance: &ProviderInstanceConfig) -> Vec<Value> {
    instance.environment.as_ref().and_then(Value::as_array).cloned().unwrap_or_default()
}

pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn with_environment(instance: &ProviderInstanceConfig, rows: Vec<Value>) -> ProviderInstanceConfig {
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|row| row["name"].as_str().is_some_and(|n| !n.trim().is_empty()))
        .collect();
    ProviderInstanceConfig {
        environment: (!rows.is_empty()).then_some(Value::Array(rows)),
        ..instance.clone()
    }
}

pub fn dedicated_field(driver: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match driver {
        "cursor" => Some(("CURSOR_API_KEY", "Cursor API key", "Optional. Overrides browser sign-in for this provider.")),
        "kiro" => Some((
            "KIRO_API_KEY",
            "Kiro API key",
            "Optional. Signs in without `kiro-cli login` (Kiro Pro and above).",
        )),
        _ => None,
    }
}

pub fn valid_reference(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && text.chars().count() <= 1024 && !text.contains(['\r', '\n'])
}

pub fn valid_one_password_reference(text: &str) -> bool {
    let text = text.trim();
    let Some(rest) = text.strip_prefix("op://").filter(|_| text.len() <= 1024) else {
        return false;
    };
    let (path, query) = rest.split_once('?').map_or((rest, None), |(p, q)| (p, Some(q)));
    let segments = path.split('/').collect::<Vec<_>>();
    let edge = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    (3..=4).contains(&segments.len())
        && segments
            .iter()
            .all(|s| s.starts_with(edge) && s.ends_with(edge) && s.chars().all(|c| edge(c) || c == ' '))
        && query.is_none_or(|q| {
            q.split_once('=').is_some_and(|(k, v)| {
                !k.is_empty()
                    && k.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                    && !v.is_empty()
                    && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            })
        })
}

pub fn publish_environment(driver: &str, rows: Vec<Value>) -> Result<Vec<Value>, &'static str> {
    let dedicated = dedicated_field(driver).map(|(name, _, _)| name);
    let mut published = Vec::new();
    for mut row in rows {
        let name = row["name"].as_str().unwrap_or("").trim().to_owned();
        if Some(name.as_str()) == dedicated {
            if row["valueRedacted"] != true && row["value"].as_str().is_some_and(|s| s.trim().is_empty()) {
                continue;
            }
            if let Some(value) = row["value"].as_str() {
                row["value"] = json!(value.trim());
            }
            row["sensitive"] = json!(true);
            if row["valueRedacted"] == false
                && let Some(row) = row.as_object_mut()
            {
                row.remove("valueRedacted");
            }
        }
        if !valid_name(&name) {
            if name.is_empty() && row["value"] == "" && row["sensitive"] == true && row.get("valueRedacted").is_none() {
                continue;
            }
            return Err("Enter a valid environment variable name.");
        }
        row["name"] = json!(name);
        published.push(row);
    }
    Ok(published)
}

#[derive(Clone)]
pub enum VariableEdit {
    Name(String),
    Value(String),
    Sensitive(bool),
}
pub fn edit_variable(row: &Value, edit: VariableEdit) -> Value {
    let redacted = row["valueRedacted"] == true;
    let mut next = row.clone();
    match edit {
        VariableEdit::Name(name) if !redacted => next["name"] = json!(name.trim()),
        VariableEdit::Value(value) if !redacted || !value.is_empty() => {
            next["value"] = json!(value);
            next["valueRedacted"] = json!(false);
        }
        VariableEdit::Sensitive(sensitive) if !redacted && row["value"].is_string() => {
            next["sensitive"] = json!(sensitive);
            if !sensitive {
                next["valueRedacted"] = json!(false);
            }
        }
        _ => {}
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{server_config::ServerSettings, settings::providers::upsert_payload};
    use serde_json::json;

    #[test]
    fn config_edits_preserve_unknown_keys_and_omit_or_persist_cleared_fields() {
        let original: ProviderInstanceConfig =
            serde_json::from_value(json!({"driver":"kiro", "config":{"binaryPath":"custom", "forkOwned":{"nested":1}}})).unwrap();
        let next = with_field(&original, "binaryPath", json!(" \t"));
        assert_eq!(
            upsert_payload(&ServerSettings::default(), "kiro_work", &original, &next),
            json!({"patch":{},"providerInstanceMutation":{"operation":"upsert","instanceId":"kiro_work","instance":{"driver":"kiro","config":{"forkOwned":{"nested":1}}}}})
        );
        let mut empty = ProviderInstanceConfig {
            driver: "kiro".into(),
            config: Some(json!({"binaryPath":"custom"})),
            ..Default::default()
        };
        assert_eq!(with_field(&empty, "binaryPath", json!("")).config, None);
        empty.driver = "antigravity".into();
        assert_eq!(with_field(&empty, "binaryPath", json!("")).config, Some(json!({"binaryPath":""})));
        let password = ProviderInstanceConfig {
            driver: "opencode".into(),
            config: Some(json!({"serverPassword":"__T3_SECRET_REDACTED__","future":true})),
            ..Default::default()
        };
        assert_eq!(
            with_field(&password, "serverUrl", json!("http://localhost:4096")).config,
            Some(json!({"serverPassword":"__T3_SECRET_REDACTED__","future":true,"serverUrl":"http://localhost:4096"}))
        );
        empty.driver = "acpRegistry".into();
        empty.config = None;
        assert_eq!(with_field(&empty, "agentId", json!("")).config, Some(json!({"agentId":""})));
    }

    #[test]
    fn environment_upsert_round_trips_redaction_references_and_literal_empty_values() {
        let rows = vec![
            json!({"name":"SECRET","value":"","sensitive":true,"valueRedacted":true}),
            json!({"name":"PLAIN","value":"","sensitive":false}),
            json!({"name":"REF","value":{"provider":"vault","reference":"op://v/i/f"},"sensitive":true}),
        ];
        let mut wire_rows = rows.clone();
        wire_rows[2] = json!({"name":"REF","value":"","secret":{"provider":"vault","reference":"op://v/i/f"},"sensitive":true});
        let original: ProviderInstanceConfig =
            serde_json::from_value(json!({"driver":"cursor","config":{"binaryPath":"legacy","unknown":7},"environment":wire_rows})).unwrap();
        assert_eq!(environment(&original), rows);
        let defaults: ProviderInstanceConfig = serde_json::from_value(json!({"driver":"kiro","environment":[{"name":"EMPTY"}]})).unwrap();
        assert_eq!(environment(&defaults), vec![json!({"name":"EMPTY","value":"","sensitive":false})]);
        let mut edited = rows.clone();
        edited[1]["value"] = json!("literal  ");
        let next = with_environment(&original, edited.clone());
        wire_rows[1]["value"] = json!("literal  ");
        assert_eq!(
            upsert_payload(&ServerSettings::default(), "cursor", &original, &next),
            json!({"patch":{},"providerInstanceMutation":{"operation":"upsert","instanceId":"cursor","instance":{"driver":"cursor","config":{"binaryPath":"legacy","unknown":7},"environment":wire_rows}}})
        );
        assert_eq!(with_environment(&next, vec![]).environment, None);
        let provider:crate::server_config::ServerProvider=serde_json::from_value(json!({"instanceId":"codex","driver":"codex","enabled":true,"installed":true,"status":"ready","runtimePaths":{"homePath":"/shared/codex","shadowHomePath":"/account/codex"}})).unwrap();
        let instance = ProviderInstanceConfig {
            driver: "codex".into(),
            ..Default::default()
        };
        assert_eq!(
            managed_runtime(&instance, Some(&provider), Some("/managed/codex"))
                .iter()
                .map(|(f, value)| (f.key.as_str(), value.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("binaryPath", "/managed/codex"),
                ("homePath", "/shared/codex"),
                ("shadowHomePath", "/account/codex")
            ]
        );
    }
    #[test]
    fn stored_secrets_require_replacement_before_renaming_or_unlocking() {
        let saved = json!({"name":"SECRET","value":"","sensitive":true,"valueRedacted":true});
        assert_eq!(edit_variable(&saved, VariableEdit::Name("RENAMED".into())), saved);
        assert_eq!(edit_variable(&saved, VariableEdit::Sensitive(false)), saved);
        assert_eq!(edit_variable(&saved, VariableEdit::Value("".into())), saved);
        let next = edit_variable(&saved, VariableEdit::Value("replacement".into()));
        assert_eq!(next, json!({"name":"SECRET","value":"replacement","sensitive":true,"valueRedacted":false}));
        assert_eq!(edit_variable(&next, VariableEdit::Name("RENAMED".into()))["name"], "RENAMED");
        assert_eq!(edit_variable(&next, VariableEdit::Sensitive(false))["sensitive"], false);
    }

    #[test]
    fn environment_drafts_keep_literals_and_validate_before_publishing() {
        assert_eq!(
            publish_environment(
                "kiro",
                vec![
                    json!({"name":"KIRO_API_KEY","value":"  fake  ","sensitive":true}),
                    json!({"name":" PLAIN ","value":"  ","sensitive":false})
                ]
            )
            .unwrap(),
            vec![
                json!({"name":"KIRO_API_KEY","value":"fake","sensitive":true}),
                json!({"name":"PLAIN","value":"  ","sensitive":false})
            ]
        );
        assert!(publish_environment("kiro", vec![json!({"name":"BAD-NAME","value":"value","sensitive":true})]).is_err());
        assert!(
            publish_environment(
                "kiro",
                vec![
                    json!({"name":"","value":"","sensitive":true}),
                    json!({"name":"KIRO_API_KEY","value":"  ","sensitive":true})
                ]
            )
            .unwrap()
            .is_empty()
        );
        for bad in ["99999", "1000001", "0300000", "300.0"] {
            assert!(!validate_field("claudeAgent", "autoCompactWindow", &json!(bad)));
        }
        for good in ["", "100000", "300000", "1000000"] {
            assert!(validate_field("claudeAgent", "autoCompactWindow", &json!(good)));
        }
        assert!(valid_reference("future://reference"));
        assert!(!valid_reference("line\nbreak"));
        assert!(valid_one_password_reference("op://vault/item/field"));
        assert!(!valid_one_password_reference("op://vault//field"));
        let local = ProviderInstanceConfig {
            driver: "acpRegistry".into(),
            config: Some(json!({"source":"local","commandArgs":["literal space", ""],"future":1})),
            ..Default::default()
        };
        assert_eq!(
            with_field(&local, "commandArgs:1", json!(" ")).config,
            Some(json!({"source":"local","commandArgs":["literal space"," "],"future":1}))
        );
        let added = super::super::InstanceEdit::AddCommandArgument.apply(&local);
        let twice = super::super::InstanceEdit::AddCommandArgument.apply(&added);
        assert_eq!(command_args(&twice), vec!["literal space", "", "", ""]);
        assert_eq!(
            command_args(&super::super::InstanceEdit::RemoveCommandArgument(0).apply(&twice)),
            vec!["", "", ""]
        );
        assert!(fields("cursor", None).is_empty());
        assert_eq!(
            fields("acpRegistry", Some(&json!({"source":"local"})))
                .iter()
                .map(|f| f.label.as_str())
                .collect::<Vec<_>>(),
            vec!["ACP source", "Executable"]
        );
    }
}
