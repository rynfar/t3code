//! Cloud device hosts as settings hold them. Ports `EasDeviceHostConfig` and
//! `DeviceEasAccountInput` / `DeviceEasAccount` from `packages/contracts/src/device.ts`, whose
//! `accessToken` is a `SettingsSecretValue` (`packages/contracts/src/settingsSecretValue.ts`,
//! `secretReference.ts`): the literal token, kept in the server's secret store and redacted for
//! clients, or the secret provider reference to read it from (`pingdotgg/t3code#27763`). Also
//! `newEasDeviceHost`, `updateEasDeviceHosts` and `easSessionLimits` from
//! `apps/web/src/components/settings/easDeviceHostsSettings.logic.ts`, and the token state of
//! `EasDeviceHostEditor.tsx` (saved, replaced or cleared; a rejected reference disables Check and
//! Save until it is fixed or cleared).

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::LOCAL_HOST_ID;
use crate::settings::secrets::is_slug;

/// `SecretReferencePath`'s limit.
const MAX_REFERENCE_LENGTH: usize = 1024;
/// `DeviceHostId`'s limit.
const MAX_HOST_ID_LENGTH: usize = 128;

/// `SettingsSecretValue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretValue {
    /// The token itself (trimmed); `""` when none is set. Saved ones arrive redacted.
    Literal(String),
    /// Where a secret provider holds it; not itself a secret, so shown as written.
    Reference { provider: String, reference: String },
}

impl Default for SecretValue {
    fn default() -> Self {
        SecretValue::Literal(String::new())
    }
}

impl SecretValue {
    /// Decodes and validates as the schema does: a string is trimmed; a reference needs a
    /// provider slug and a single-line path of at most 1024 characters.
    pub fn decode(value: &Value) -> Option<SecretValue> {
        match value {
            Value::String(literal) => Some(SecretValue::Literal(literal.trim().to_owned())),
            Value::Object(object) => {
                let provider = object.get("provider")?.as_str()?.trim();
                let reference = object.get("reference")?.as_str()?.trim();
                SecretValue::reference(provider, reference)
            }
            _ => None,
        }
    }

    /// A reference, when both parts are valid (`SecretReference`).
    pub fn reference(provider: &str, reference: &str) -> Option<SecretValue> {
        let (provider, reference) = (provider.trim(), reference.trim());
        let valid_path = !reference.is_empty() && reference.chars().count() <= MAX_REFERENCE_LENGTH && !reference.contains(['\r', '\n']);
        (is_slug(provider) && valid_path).then(|| SecretValue::Reference { provider: provider.to_owned(), reference: reference.to_owned() })
    }

    pub fn encode(&self) -> Value {
        match self {
            SecretValue::Literal(literal) => Value::String(literal.clone()),
            SecretValue::Reference { provider, reference } => json!({ "provider": provider, "reference": reference }),
        }
    }

    /// `settingsSecretLiteral`: the literal, or `""` when it names a secret instead.
    pub fn literal(&self) -> &str {
        match self {
            SecretValue::Literal(literal) => literal,
            SecretValue::Reference { .. } => "",
        }
    }

    pub fn is_reference(&self) -> bool {
        matches!(self, SecretValue::Reference { .. })
    }

    /// Nothing set: the server falls back to `EXPO_TOKEN`, then the `eas login` session.
    pub fn is_empty(&self) -> bool {
        matches!(self, SecretValue::Literal(literal) if literal.is_empty())
    }
}

/// `ConfiguredDeviceHostId`: a letter or digit, then letters, digits, `_` or `-`; never `local`.
pub fn is_configured_host_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= MAX_HOST_ID_LENGTH
        && id != LOCAL_HOST_ID
        && chars.next().is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `EasDeviceHostConfig`: cloud simulators from EAS Simulator. Sessions bill the Expo account
/// until they stop, so every one is bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasHostConfig {
    pub id: String,
    pub label: String,
    /// The EAS project sessions run under when the project's app config names none.
    pub app_id: Option<String>,
    pub access_token: SecretValue,
    /// EAS allows at most 40 minutes on free accounts and 115 on paid ones.
    pub max_session_minutes: i64,
    pub idle_minutes: i64,
    pub max_concurrent_sessions: i64,
}

pub const IDLE_LIMIT_PROBLEM: &str = "Idle minutes must be smaller than session minutes.";
pub const LIMITS_PROBLEM: &str = "Give the host a label and limits within the ranges shown.";
pub const REFERENCE_PROBLEM: &str = "Enter a valid secret reference.";

fn int_field(object: &Map<String, Value>, key: &str, default: i64, range: std::ops::RangeInclusive<i64>) -> Result<i64, String> {
    let Some(value) = object.get(key) else { return Ok(default) };
    let number = value.as_f64().filter(|number| number.fract() == 0.).ok_or_else(|| format!("{key} must be a whole number."))?;
    // Whole and in range, so the cast is exact.
    let number = number as i64;
    if range.contains(&number) { Ok(number) } else { Err(format!("{key} must be between {} and {}.", range.start(), range.end())) }
}

impl EasHostConfig {
    /// `newEasDeviceHost`.
    pub fn new(id: &str) -> EasHostConfig {
        EasHostConfig {
            id: id.to_owned(),
            label: "EAS Simulator".to_owned(),
            app_id: None,
            access_token: SecretValue::default(),
            max_session_minutes: 30,
            idle_minutes: 10,
            max_concurrent_sessions: 2,
        }
    }

    /// `parseEasDeviceHostDraft`: decodes with the schema's defaults and checks.
    pub fn decode(value: &Value) -> Result<EasHostConfig, String> {
        let object = value.as_object().ok_or("An EAS host must be an object.")?;
        if object.get("kind").and_then(Value::as_str) != Some("eas") {
            return Err("Not an EAS host.".to_owned());
        }
        let text = |key: &str| object.get(key).and_then(Value::as_str).map(str::trim);
        let id = text("id").filter(|id| is_configured_host_id(id)).ok_or("The host id is invalid.")?;
        let label = text("label").filter(|label| !label.is_empty()).ok_or(LIMITS_PROBLEM)?;
        let app_id = match object.get("appId") {
            None => None,
            Some(_) => Some(text("appId").filter(|app| !app.is_empty()).ok_or("The EAS project id is invalid.")?.to_owned()),
        };
        let access_token = match object.get("accessToken") {
            None => SecretValue::default(),
            Some(token) => SecretValue::decode(token).ok_or(REFERENCE_PROBLEM)?,
        };
        let host = EasHostConfig {
            id: id.to_owned(),
            label: label.to_owned(),
            app_id,
            access_token,
            max_session_minutes: int_field(object, "maxSessionMinutes", 30, 1..=115)?,
            idle_minutes: int_field(object, "idleMinutes", 10, 1..=60)?,
            max_concurrent_sessions: int_field(object, "maxConcurrentSessions", 2, 1..=20)?,
        };
        host.validate()?;
        Ok(host)
    }

    /// The schema's checks on an already typed host.
    pub fn validate(&self) -> Result<(), String> {
        if !is_configured_host_id(self.id.trim()) {
            return Err("The host id is invalid.".to_owned());
        }
        if self.label.trim().is_empty() || self.app_id.as_deref().is_some_and(|app| app.trim().is_empty()) {
            return Err(LIMITS_PROBLEM.to_owned());
        }
        if let SecretValue::Reference { provider, reference } = &self.access_token
            && SecretValue::reference(provider, reference).is_none()
        {
            return Err(REFERENCE_PROBLEM.to_owned());
        }
        let in_range = (1..=115).contains(&self.max_session_minutes) && (1..=60).contains(&self.idle_minutes) && (1..=20).contains(&self.max_concurrent_sessions);
        if !in_range {
            return Err(LIMITS_PROBLEM.to_owned());
        }
        if self.idle_minutes >= self.max_session_minutes {
            return Err(IDLE_LIMIT_PROBLEM.to_owned());
        }
        Ok(())
    }

    pub fn encode(&self) -> Value {
        let mut out = json!({
            "kind": "eas",
            "id": self.id,
            "label": self.label,
            "accessToken": self.access_token.encode(),
            "maxSessionMinutes": self.max_session_minutes,
            "idleMinutes": self.idle_minutes,
            "maxConcurrentSessions": self.max_concurrent_sessions,
        });
        if let Some(app_id) = &self.app_id {
            out["appId"] = Value::String(app_id.clone());
        }
        out
    }

    /// `easSessionLimits`.
    pub fn session_limits(&self) -> String {
        let sessions = if self.max_concurrent_sessions == 1 { "1 session".to_owned() } else { format!("{} sessions", self.max_concurrent_sessions) };
        format!("Up to {sessions} at once, each stopped after {} min or {} idle min", self.max_session_minutes, self.idle_minutes)
    }
}

/// `EasDeviceHostConfigs`: settings' `easDeviceHosts`, which fail as a whole on a bad entry or a
/// repeated id, as the server's schema does.
pub fn decode_eas_hosts(value: &Value) -> Result<Vec<EasHostConfig>, String> {
    let Some(entries) = value.as_array() else { return Ok(Vec::new()) };
    let hosts = entries.iter().map(EasHostConfig::decode).collect::<Result<Vec<_>, _>>()?;
    let mut ids: Vec<&str> = hosts.iter().map(|host| host.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != hosts.len() {
        return Err("Device host ids must be unique.".to_owned());
    }
    Ok(hosts)
}

/// `updateEasDeviceHosts`: adds, replaces, or removes one host. With `keep_token`, the host keeps
/// the token the environment holds (a saved one arrives redacted); a host new to it uses its own
/// Expo login.
pub fn update_eas_hosts(hosts: &[EasHostConfig], host: &EasHostConfig, remove: bool, keep_token: bool) -> Vec<EasHostConfig> {
    if remove {
        return hosts.iter().filter(|candidate| candidate.id != host.id).cloned().collect();
    }
    let existing = hosts.iter().find(|candidate| candidate.id == host.id);
    let mut next = host.clone();
    if keep_token {
        next.access_token = existing.map(|existing| existing.access_token.clone()).unwrap_or_default();
    }
    if existing.is_some() {
        hosts.iter().map(|candidate| if candidate.id == host.id { next.clone() } else { candidate.clone() }).collect()
    } else {
        hosts.iter().cloned().chain(std::iter::once(next)).collect()
    }
}

/// The editor's token: a saved one arrives redacted, so only a replacement or a clear changes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenDraft {
    Saved,
    New(SecretValue),
    Cleared,
}

/// The token half of `EasDeviceHostEditor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasTokenEditor {
    saved: SecretValue,
    is_new: bool,
    token: TokenDraft,
    /// Whether the picked source and committed field can be saved or used.
    valid: bool,
}

impl EasTokenEditor {
    pub fn new(host: &EasHostConfig, is_new: bool) -> EasTokenEditor {
        let token = if host.access_token.is_empty() { TokenDraft::Cleared } else { TokenDraft::Saved };
        EasTokenEditor { saved: host.access_token.clone(), is_new, token, valid: true }
    }

    pub fn token(&self) -> &TokenDraft {
        &self.token
    }

    pub fn valid(&self) -> bool {
        self.valid
    }

    /// The field shows a saved literal as configured, never its (redacted) value.
    pub fn configured(&self) -> bool {
        self.token == TokenDraft::Saved && !self.saved.is_reference()
    }

    /// What the field shows: a new value, a saved reference as written, else nothing.
    pub fn field_value(&self) -> SecretValue {
        match &self.token {
            TokenDraft::New(value) => value.clone(),
            TokenDraft::Saved if self.saved.is_reference() => self.saved.clone(),
            _ => SecretValue::default(),
        }
    }

    /// `onSecretChange`: a committed reference, or the stored literal typed in; `None` clears.
    pub fn set_token(&mut self, secret: Option<SecretValue>) {
        self.token = match secret {
            Some(secret) if !secret.is_empty() => TokenDraft::New(secret),
            _ => TokenDraft::Cleared,
        };
    }

    /// `onValidityChange`: a picked provider without a reference yet, or a rejected one, is
    /// invalid until fixed.
    pub fn set_validity(&mut self, valid: bool) {
        self.valid = valid;
    }

    /// The Clear button: drops the token and whatever the field rejected.
    pub fn clear(&mut self) {
        self.token = TokenDraft::Cleared;
        self.valid = true;
    }

    /// The token Save writes.
    pub fn token_for_save(&self) -> SecretValue {
        match &self.token {
            TokenDraft::Saved => self.saved.clone(),
            TokenDraft::New(value) => value.clone(),
            TokenDraft::Cleared => SecretValue::default(),
        }
    }

    /// Check account is off while the token is invalid.
    pub fn can_check(&self) -> bool {
        self.valid
    }

    /// The host Save writes: `draft` with its label trimmed and this token.
    pub fn next(&self, draft: &EasHostConfig) -> EasHostConfig {
        EasHostConfig { label: draft.label.trim().to_owned(), access_token: self.token_for_save(), ..draft.clone() }
    }

    /// Why Save is off, for the limits the fields' ranges do not show; `None` when it is on.
    pub fn problem(&self, draft: &EasHostConfig) -> Option<&'static str> {
        if !self.valid {
            return Some(REFERENCE_PROBLEM);
        }
        self.next(draft).validate().err()?;
        Some(if draft.max_session_minutes > 0 && draft.idle_minutes >= draft.max_session_minutes { IDLE_LIMIT_PROBLEM } else { LIMITS_PROBLEM })
    }

    /// `device.easAccount` as the editor opens: the saved credential, or none.
    pub fn open_check_payload(&self, host_id: &str) -> Value {
        match (self.saved.is_empty(), self.is_new) {
            (true, _) => json!({ "accessToken": "" }),
            (false, true) => json!({}),
            (false, false) => json!({ "hostId": host_id }),
        }
    }

    /// `device.easAccount` for Check account. A cleared token is sent empty, so the check does
    /// not fall back to the saved one Save is about to drop.
    pub fn check_payload(&self, host_id: &str) -> Value {
        match &self.token {
            TokenDraft::Saved if !self.is_new => json!({ "hostId": host_id }),
            TokenDraft::Saved => json!({}),
            TokenDraft::New(value) => json!({ "accessToken": value.encode() }),
            TokenDraft::Cleared => json!({ "accessToken": "" }),
        }
    }
}

/// `DeviceEasAccount`: who an Expo credential signs in as.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EasAccount {
    pub signed_in_as: String,
    pub credential_source: String,
    #[serde(default)]
    pub apps: Vec<EasApp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EasApp {
    pub id: String,
    pub full_name: String,
}

impl EasAccount {
    pub fn decode(value: Value) -> Result<EasAccount, String> {
        serde_json::from_value(value).map_err(|error| format!("Invalid EAS account: {error}"))
    }

    /// `CREDENTIAL_SOURCES`: where the server found the credential.
    pub fn credential_source_label(&self) -> &str {
        match self.credential_source.as_str() {
            "settings" => "the access token saved here",
            "env" => "EXPO_TOKEN on the server",
            "eas-login" => "the server's eas login",
            other => other,
        }
    }
}
