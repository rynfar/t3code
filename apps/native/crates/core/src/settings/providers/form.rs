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
    pub control: String,
    #[serde(rename = "acceptsSecretReference")]
    pub accepts_secret_reference: bool,
    pub validation: HashMap<String, bool>,
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
    install_failed: bool,
) -> Vec<(Field, String)> {
    let paths = provider.and_then(|p| p.runtime_paths.as_ref());
    fields(&instance.driver, instance.config.as_ref())
        .into_iter()
        .filter(|f| f.key != "launchArgs")
        .map(|mut field| {
            let value = match field.key.as_str() {
                "binaryPath" => {
                    field.description = "Selected by T3 Code.".into();
                    field.placeholder = if install_failed { "Could not read runtime path" } else { "Not installed" }.into();
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
pub fn field_value(instance: &ProviderInstanceConfig, key: &str) -> Value {
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

#[derive(Clone, Copy)]
pub enum EditTarget {
    Runtime,
    Environment,
}
pub fn allows_edit(instance: &ProviderInstanceConfig, target: EditTarget) -> bool {
    !(instance.driver == "codex" && super::chatgpt::managed(instance.config.as_ref()) && matches!(target, EditTarget::Runtime))
}
pub fn variable_sensitive(driver: &str, row: &Value) -> bool {
    row["sensitive"] == true || dedicated_field(driver).is_some_and(|(name, _, _)| row["name"] == name)
}
#[derive(Default)]
pub struct SecretSourceDraft {
    pending_provider: Option<String>,
}
impl SecretSourceDraft {
    pub fn provider<'a>(&'a self, row: &'a Value) -> Option<&'a str> {
        self.pending_provider.as_deref().or_else(|| row["value"]["provider"].as_str())
    }
    pub fn input_value<'a>(&self, row: &'a Value) -> &'a str {
        if self.provider(row).is_none() {
            row["value"].as_str().unwrap_or("")
        } else if self.provider(row) == row["value"]["provider"].as_str() {
            row["value"]["reference"].as_str().unwrap_or("")
        } else {
            ""
        }
    }
    pub fn select(&mut self, row: &mut Value, provider: Option<String>) -> bool {
        let stored = provider.is_none();
        self.pending_provider = provider.filter(|p| row["value"]["provider"].as_str() != Some(p.as_str()));
        if stored && row["value"].is_object() {
            row["value"] = json!("");
            if let Some(row) = row.as_object_mut() {
                row.remove("valueRedacted");
            }
            return true;
        }
        false
    }
    pub fn commit(&mut self, row: &mut Value, text: &str, one_password: bool) -> Result<bool, &'static str> {
        if text.trim().is_empty() {
            if self.pending_provider.is_none() && row["value"].is_object() {
                row["value"] = json!("");
                if let Some(row) = row.as_object_mut() {
                    row.remove("valueRedacted");
                }
                return Ok(true);
            }
            return Ok(false);
        }
        if !valid_reference(text) || (one_password && !valid_one_password_reference(text)) {
            return Err(if one_password {
                "Use a 1Password reference like op://vault/item/field."
            } else {
                "Enter a valid secret reference."
            });
        }
        let Some(provider) = self.provider(row).map(str::to_owned) else {
            return Ok(false);
        };
        row["value"] = json!({"provider":provider,"reference":text.trim()});
        if let Some(row) = row.as_object_mut() {
            row.remove("valueRedacted");
        }
        self.pending_provider = None;
        Ok(true)
    }
}

#[derive(Debug, Clone)]
pub struct ArgumentRow {
    pub id: usize,
    pub value: String,
}
#[derive(Debug, Clone)]
pub struct ArgumentPublication {
    pub id: usize,
    pub values: Vec<String>,
}
#[derive(Default)]
pub struct ArgumentsDraft {
    pub rows: Vec<ArgumentRow>,
    seen: Vec<String>,
    outstanding: Vec<ArgumentPublication>,
    next_id: usize,
    next_publication: usize,
}
pub enum ArgumentEdit {
    Add,
    Remove(usize),
    Value(usize, String),
}
impl ArgumentsDraft {
    pub fn reconcile(&mut self, incoming: Vec<String>) {
        if self.seen == incoming {
            return;
        }
        let echo = self
            .outstanding
            .iter()
            .any(|publication| publication.values == incoming);
        self.seen = incoming.clone();
        if echo {
            return;
        }
        self.rows = incoming
            .into_iter()
            .map(|value| {
                let id = self.next_id;
                self.next_id += 1;
                ArgumentRow { id, value }
            })
            .collect();
    }
    /// Reconciles a successful reply before retiring its echo; `None` is a failed or skipped save.
    pub fn settle(&mut self, id: usize, saved: Option<Vec<String>>) {
        let Some(index) = self
            .outstanding
            .iter()
            .position(|publication| publication.id == id)
        else {
            return;
        };
        if let Some(incoming) = saved {
            self.reconcile(incoming);
            self.outstanding.drain(..=index);
        } else {
            self.outstanding.remove(index);
        }
    }
    pub fn edit(&mut self, edit: ArgumentEdit) -> Option<ArgumentPublication> {
        match edit {
            ArgumentEdit::Add => {
                let id = self.next_id;
                self.next_id += 1;
                self.rows.push(ArgumentRow {
                    id,
                    value: String::new(),
                });
            }
            ArgumentEdit::Remove(id) => {
                let index = self.rows.iter().position(|r| r.id == id)?;
                self.rows.remove(index);
            }
            ArgumentEdit::Value(id, value) => {
                let row = self
                    .rows
                    .iter_mut()
                    .find(|r| r.id == id && r.value != value)?;
                row.value = value;
            }
        }
        let values = self
            .rows
            .iter()
            .map(|r| r.value.clone())
            .collect::<Vec<_>>();
        let publication = ArgumentPublication {
            id: self.next_publication,
            values,
        };
        self.next_publication += 1;
        self.outstanding.push(publication.clone());
        Some(publication)
    }
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
        let managed = ProviderInstanceConfig {
            driver: "codex".into(),
            ..Default::default()
        };
        assert_eq!(managed_runtime(&managed, None, None, true)[0].0.placeholder, "Could not read runtime path");
        let provider:crate::server_config::ServerProvider=serde_json::from_value(json!({"instanceId":"codex","driver":"codex","enabled":true,"installed":true,"status":"ready","runtimePaths":{"homePath":"/shared/codex","shadowHomePath":"/account/codex"}})).unwrap();
        let instance = ProviderInstanceConfig {
            driver: "codex".into(),
            ..Default::default()
        };
        assert_eq!(
            managed_runtime(&instance, Some(&provider), Some("/managed/codex"), false)
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
        for (driver, fields) in FIELDS.iter() {
            for field in fields {
                assert!(!field.accepts_secret_reference);
                assert!(matches!(field.control.as_str(), "text" | "password" | "select"));
                for (input, accepted) in &field.validation {
                    if !field.persist && input.trim().is_empty() {
                        continue;
                    }
                    assert_eq!(
                        validate_field(driver, &field.key, &json!(input)),
                        *accepted,
                        "{driver}.{}: {input:?}",
                        field.key
                    );
                }
            }
        }
        assert!(fields("cursor", None).is_empty());
        assert_eq!(
            fields("acpRegistry", Some(&json!({"source":"local"})))
                .iter()
                .map(|f| f.label.as_str())
                .collect::<Vec<_>>(),
            vec!["ACP source", "Executable"]
        );
    }
    #[test]
    fn dedicated_keys_and_managed_environment_use_their_field_classification() {
        for (driver, name) in [("kiro", "KIRO_API_KEY"), ("cursor", "CURSOR_API_KEY")] {
            assert!(variable_sensitive(driver, &json!({"name":name,"value":"fake","sensitive":false})));
        }
        let managed = ProviderInstanceConfig {
            driver: "codex".into(),
            config: Some(json!({"setupMode":"managed"})),
            ..Default::default()
        };
        assert!(allows_edit(&managed, EditTarget::Environment));
        assert!(!allows_edit(&managed, EditTarget::Runtime));
    }
    #[test]
    fn exploring_another_secret_source_keeps_the_saved_reference_until_commit() {
        let saved = json!({"name":"TOKEN","value":{"provider":"A","reference":"op://v/i/f"},"sensitive":true});
        let mut row = saved.clone();
        let mut draft = SecretSourceDraft::default();
        assert!(!draft.select(&mut row, Some("B".into())));
        assert!(!draft.commit(&mut row, "", true).unwrap());
        assert_eq!(row, saved);
        assert!(draft.commit(&mut row, "invalid", true).is_err());
        assert_eq!(row, saved);
        assert!(!draft.select(&mut row, Some("A".into())));
        assert_eq!(draft.input_value(&row), "op://v/i/f");
        assert!(!draft.select(&mut row, Some("B".into())));
        assert!(draft.commit(&mut row, "op://other/item/field", true).unwrap());
        assert_eq!(row["value"], json!({"provider":"B","reference":"op://other/item/field"}));
        assert!(draft.commit(&mut row, "", true).unwrap());
        assert_eq!(row["value"], json!(""));
        for literal in [
            json!({"name":"TOKEN","value":"legacy literal","sensitive":false}),
            edit_variable(
                &json!({"name":"TOKEN","value":"","sensitive":true,"valueRedacted":true}),
                VariableEdit::Value("replacement literal".into()),
            ),
        ] {
            let mut row = literal.clone();
            assert!(!draft.select(&mut row, Some("B".into())));
            assert_eq!(draft.input_value(&row), "");
            assert!(!draft.select(&mut row, None));
            assert_eq!(draft.input_value(&row), literal["value"].as_str().unwrap());
            assert_eq!(row, literal);
        }
    }
    #[test]
    fn failed_argument_writes_do_not_hide_an_external_settings_change() {
        for (fail_first, fail_second) in [(true, true), (true, false), (false, false)] {
            let mut draft = ArgumentsDraft::default();
            draft.reconcile(vec!["a".into()]);
            let id = draft.rows[0].id;
            let first = draft.edit(ArgumentEdit::Value(id, "b".into())).unwrap();
            let second = draft.edit(ArgumentEdit::Value(id, "c".into())).unwrap();
            if fail_first {
                draft.settle(first.id, None);
            }
            if fail_second {
                draft.settle(second.id, None);
            } else {
                draft.reconcile(second.values.clone());
                assert_eq!(draft.rows[0].id, id);
                draft.settle(second.id, Some(second.values));
            }
            draft.reconcile(vec!["b".into()]);
            assert_eq!(draft.rows[0].value, "b");
            assert_eq!(draft.edit(ArgumentEdit::Add).unwrap().values, vec!["b", ""]);
        }
    }
    #[test]
    fn removing_an_argument_then_editing_the_displayed_next_row_keeps_the_target() {
        for args in [vec!["a", "b", "c"], vec!["a", "b"]] {
            let original = ProviderInstanceConfig {
                driver: "acpRegistry".into(),
                config: Some(json!({"source":"local","commandArgs":args})),
                ..Default::default()
            };
            let mut draft = ArgumentsDraft::default();
            draft.reconcile(command_args(&original));
            let a = draft.rows[0].id;
            let b = draft.rows[1].id;
            let c = draft.rows.get(2).map(|row| row.id);
            let removed_value = draft.edit(ArgumentEdit::Remove(a)).unwrap();
            let edited_value = draft.edit(ArgumentEdit::Value(b, "B".into())).unwrap();
            draft.reconcile(removed_value.values.clone());
            assert_eq!(draft.rows[0].id, b);
            assert_eq!(draft.rows[0].value, "B");
            draft.settle(removed_value.id, Some(removed_value.values.clone()));
            let latest = if let Some(c) = c {
                assert_eq!(draft.rows[1].id, c);
                let publication = draft.edit(ArgumentEdit::Value(c, "C".into())).unwrap();
                assert_eq!(publication.values, vec!["B", "C"]);
                publication
            } else {
                edited_value.clone()
            };
            draft.settle(edited_value.id, Some(edited_value.values.clone()));
            draft.reconcile(edited_value.values);
            let removed = with_field(&original, "commandArgs", json!(removed_value.values));
            let edited = with_field(&removed, "commandArgs", json!(latest.values));
            draft.settle(latest.id, Some(command_args(&edited)));
            draft.reconcile(command_args(&edited));
            assert_eq!(draft.rows[0].id, b);
            assert!(
                draft
                    .edit(ArgumentEdit::Value(a, "removed".into()))
                    .is_none()
            );
            let lower = draft.edit(ArgumentEdit::Value(b, "b".into())).unwrap();
            let upper = draft.edit(ArgumentEdit::Value(b, "B".into())).unwrap();
            draft.reconcile(command_args(&edited));
            draft.reconcile(lower.values.clone());
            assert_eq!(draft.rows[0].id, b);
            assert_eq!(draft.rows[0].value, "B");
            draft.settle(lower.id, Some(lower.values));
            draft.reconcile(upper.values.clone());
            assert_eq!(draft.rows[0].id, b);
            draft.settle(upper.id, Some(upper.values));
            let mut expected = args[1..].to_vec();
            expected[0] = "B";
            if args.len() == 3 {
                expected[1] = "C";
            }
            assert_eq!(command_args(&edited), expected);
        }
    }
}
