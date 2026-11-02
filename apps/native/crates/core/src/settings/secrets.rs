//! Secrets: the secret providers an environment's provider variables read from.
//! `SecretsSettings.tsx` (states, access), `SecretProviderDialog.logic.ts` (classification, the
//! draft's validation and the id it saves under), `redactedPlaceholder` from
//! `RedactedSensitiveText.tsx`, and the `secretProviders` entries of `contracts/src/settings.ts`
//! with the 1Password config from `secret-provider-onepassword/src/shared/definition.ts`.

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use crate::auth::AuthSessionState;
use crate::usage::PROVIDERS_MANAGE_SCOPE;

pub const ONE_PASSWORD: &str = "one_password";
/// `SecretProviderLabel` and `SecretProviderSlug`.
pub const MAX_NAME_LENGTH: usize = 64;
/// `OnePasswordAccount`.
pub const MAX_ACCOUNT_LENGTH: usize = 253;

pub const DISCONNECTED: &str = "Connect an environment to manage the secret providers its provider variables read from.";
pub const EMPTY_TITLE: &str = "No secret providers configured.";
pub const EMPTY_DESCRIPTION: &str = "Add a 1Password account, then pick it as the source of a provider variable.";
pub const ACCOUNT_HINT: &str = "The account shorthand, sign-in address without https://, or ID that op account list shows.";

/// One `secretProviders` entry: `SecretProviderConfig` under its `SecretProviderId`. The config
/// stays raw, as the web keeps it for kinds it does not know.
#[derive(Debug, Clone, PartialEq)]
pub struct SecretProvider {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub config: Option<Value>,
}

/// `secretProviders` in the server's order. Entries whose id or envelope does not decode are
/// dropped, as `Schema.Record(SecretProviderId, SecretProviderConfig)` drops them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SecretProviders(pub Vec<SecretProvider>);

impl SecretProviders {
    pub fn get(&self, id: &str) -> Option<&SecretProvider> {
        self.0.iter().find(|provider| provider.id == id)
    }

    pub fn ids(&self) -> Vec<String> {
        self.0.iter().map(|provider| provider.id.clone()).collect()
    }
}

impl<'de> Deserialize<'de> for SecretProviders {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let Value::Object(entries) = Value::deserialize(d)? else {
            return Ok(SecretProviders::default());
        };
        Ok(SecretProviders(entries.into_iter().filter_map(|(id, value)| decode_entry(id, &value)).collect()))
    }
}

fn decode_entry(id: String, value: &Value) -> Option<SecretProvider> {
    let kind = value.get("kind")?.as_str()?.trim();
    let label = value.get("label")?.as_str()?.trim();
    if !is_slug(&id) || !is_slug(kind) || label.is_empty() {
        return None;
    }
    Some(SecretProvider { kind: kind.to_owned(), label: label.to_owned(), config: value.get("config").cloned(), id })
}

/// `SecretProviderSlug`: up to 64 characters, a letter or digit first, then letters, digits, `_`
/// or `-`.
pub fn is_slug(value: &str) -> bool {
    let mut chars = value.chars();
    value.len() <= MAX_NAME_LENGTH && chars.next().is_some_and(|first| first.is_ascii_alphanumeric()) && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `ACCOUNT_PATTERN`.
fn is_account(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|first| first.is_ascii_alphanumeric()) && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `classifySecretProvider`: unknown kinds and configs that no longer decode are shown read-only;
/// removal stays available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classified {
    OnePassword { account: String },
    Unsupported,
}

pub fn classify(provider: &SecretProvider) -> Classified {
    if provider.kind != ONE_PASSWORD {
        return Classified::Unsupported;
    }
    let account = provider.config.as_ref().and_then(|config| config.get("account")).and_then(Value::as_str);
    match account.filter(|account| account.len() <= MAX_ACCOUNT_LENGTH && is_account(account)) {
        Some(account) => Classified::OnePassword { account: account.to_owned() },
        None => Classified::Unsupported,
    }
}

/// What an environment without `secretProviderSettings` says.
pub fn update_required(environment: &str) -> String {
    format!("Update T3 Code on {environment} to manage its secret providers.")
}

/// The row's description: "1Password · <account>" (the account redacted until revealed), or
/// what the web says for a provider it cannot edit.
pub fn unsupported_description(provider: &SecretProvider) -> String {
    format!("Unsupported provider ({})", provider.kind)
}

/// `secretProviderIdFromName`: stable and readable in settings.json, so re-adding a provider under
/// the same name restores its variables.
pub fn id_from_name(name: &str) -> Option<String> {
    let mut slug = String::new();
    let mut gap = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            gap = false;
            slug.push(c);
        } else {
            gap = true;
        }
    }
    slug.truncate(MAX_NAME_LENGTH);
    let slug = slug.trim_end_matches('-').to_owned();
    is_slug(&slug).then_some(slug)
}

/// `SecretProviderDraft`: what the dialog's two fields hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub name: String,
    pub account: String,
}

/// `draftFromSecretProvider`.
pub fn draft_from(provider: &SecretProvider) -> Draft {
    Draft {
        name: provider.label.clone(),
        account: match classify(provider) {
            Classified::OnePassword { account } => account,
            Classified::Unsupported => String::new(),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Add,
    Edit(String),
}

impl Target {
    /// Whether the dialog may stay open: an edit closes once its provider is gone (another client
    /// removed it), so Save never writes it back.
    pub fn is_open_in(&self, providers: &SecretProviders) -> bool {
        match self {
            Target::Add => true,
            Target::Edit(id) => providers.get(id).is_some(),
        }
    }
}

/// `SecretProviderDraftIssues`: one message per field that blocks saving.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Issues {
    pub name: Option<String>,
    pub account: Option<String>,
}

fn decode_field(value: &str, max: usize, empty: &str, extra: impl Fn(&str) -> Option<&'static str>) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(empty.to_owned());
    }
    if trimmed.chars().count() > max {
        return Err(format!("Expected a value with a length of at most {max}"));
    }
    match extra(trimmed) {
        Some(message) => Err(message.to_owned()),
        None => Ok(trimmed.to_owned()),
    }
}

/// `secretProviderFromDraft`: the settings entry a draft adds or replaces, as `(id, config)`,
/// or every field that blocks it.
pub fn provider_from_draft(draft: &Draft, existing_ids: &[String], target: &Target) -> Result<(String, Value), Issues> {
    let label = decode_field(&draft.name, MAX_NAME_LENGTH, "Enter a name.", |_| None);
    let account = decode_field(&draft.account, MAX_ACCOUNT_LENGTH, "Enter the 1Password account.", |account| {
        (!is_account(account)).then_some("1Password account must be a shorthand, sign-in address, or ID with no spaces.")
    });
    let mut issues = Issues::default();
    let id = match &label {
        Err(message) => {
            issues.name = Some(message.clone());
            None
        }
        Ok(label) => {
            let id = match target {
                Target::Edit(id) => Some(id.clone()),
                Target::Add => id_from_name(label),
            };
            match &id {
                None => issues.name = Some("The name needs at least one letter or digit.".to_owned()),
                Some(id) if *target == Target::Add && existing_ids.contains(id) => issues.name = Some("A secret provider with this name exists.".to_owned()),
                Some(_) => {}
            }
            id
        }
    };
    if let Err(message) = &account {
        issues.account = Some(message.clone());
    }
    match (id, label, account) {
        (Some(id), Ok(label), Ok(account)) if issues.name.is_none() => Ok((id, json!({ "kind": ONE_PASSWORD, "label": label, "config": { "account": account } }))),
        _ => Err(issues),
    }
}

/// The `server.updateSettings` payload that adds or replaces one provider.
pub fn save_payload(id: &str, config: Value) -> Value {
    let mut providers = Map::new();
    providers.insert(id.to_owned(), config);
    json!({ "patch": { "secretProviders": providers } })
}

/// The payload that removes one provider; its secrets are untouched.
pub fn remove_payload(id: &str) -> Value {
    let mut providers = Map::new();
    providers.insert(id.to_owned(), Value::Null);
    json!({ "patch": { "secretProviders": providers } })
}

/// The dialog's title.
pub fn dialog_title(target: &Target, label: Option<&str>) -> String {
    match (target, label) {
        (Target::Edit(_), Some(label)) => format!("Edit {label}"),
        _ => "Add a 1Password provider".to_owned(),
    }
}

pub fn dialog_description(environment: &str) -> String {
    format!("Provider variables on {environment} can read values from it by name. Values are read with the op CLI on that server when a provider starts.")
}

/// The `RemoveSecretProviderButton` confirmation: title and description.
pub fn remove_confirmation(label: &str) -> (String, String) {
    (
        format!("Remove {label}?"),
        format!("Provider variables that read from {label} stop resolving until a provider with the same name is added again. The secrets themselves are untouched."),
    )
}

/// What the page shows for the connected environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageState {
    Disconnected,
    /// The server predates `secretProviderSettings`.
    UpdateRequired,
    Providers {
        editable: bool,
    },
}

/// `SecretsSettingsPanel` with `classifyProviderEnvironmentAccess` and `resolveRemoteOperateAccess`:
/// editing needs `providers:manage`; an unknown grant (still loading) keeps the page read-only.
pub fn page_state(connected: bool, has_config: bool, secret_provider_settings: Option<bool>, session: Option<&AuthSessionState>) -> PageState {
    if !connected {
        return PageState::Disconnected;
    }
    if has_config && secret_provider_settings != Some(true) {
        return PageState::UpdateRequired;
    }
    PageState::Providers { editable: has_config && session.is_some_and(|session| session.grants(PROVIDERS_MANAGE_SCOPE)) }
}

const REDACTED_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// `redactedPlaceholder`: a stable stand-in of the same shape, keeping `@`, `.`, `-` and `_`.
pub fn redacted_placeholder(value: &str) -> String {
    let mut state = 0x811c_9dc5_u32 as i32;
    for unit in value.encode_utf16() {
        state ^= i32::from(unit);
        state = state.wrapping_mul(0x0100_0193);
    }
    value
        .chars()
        .map(|c| {
            if matches!(c, '@' | '.' | '-' | '_') {
                return c;
            }
            state = (state ^ ((state as u32 >> 13) as i32)).wrapping_mul(0x85eb_ca6b_u32 as i32);
            state = (state ^ ((state as u32 >> 16) as i32)).wrapping_mul(0xc2b2_ae35_u32 as i32);
            REDACTED_ALPHABET[(i64::from(state).unsigned_abs() % REDACTED_ALPHABET.len() as u64) as usize] as char
        })
        .collect()
}

#[cfg(test)]
mod tests;
