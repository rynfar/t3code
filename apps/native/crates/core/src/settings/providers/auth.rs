//! Provider sign-in and runtime installs: `ProviderAuthState` and `ProviderInstallState` from
//! `contracts/src/providerSetup.ts`, the payloads of the `provider.auth.*` and
//! `provider.install.*` methods in `contracts/src/rpc.ts`, and what the Account and Runtime rows
//! show and offer (`web/src/components/settings/ProviderAuthenticationSection.tsx`,
//! `ProviderSetupSection.tsx`).

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use crate::rpc::RpcFailure;
use crate::server_config::ServerProvider;

pub const AUTH_START: &str = "provider.auth.start";
pub const AUTH_RESPOND: &str = "provider.auth.respond";
pub const AUTH_COMPLETE: &str = "provider.auth.complete";
pub const AUTH_CANCEL: &str = "provider.auth.cancel";
pub const AUTH_LOGOUT: &str = "provider.auth.logout";
pub const AUTH_SUBSCRIBE: &str = "provider.auth.subscribe";
pub const INSTALL_START: &str = "provider.install.start";
pub const INSTALL_CANCEL: &str = "provider.install.cancel";
pub const INSTALL_SUBSCRIBE: &str = "provider.install.subscribe";
pub const INSTALL_REMOVE: &str = "provider.install.remove";

/// `ProviderAuthMethod`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub account_email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CredentialField {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub secret: bool,
}

/// `ProviderAuthInteraction`: what the flow waits on.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub enum Interaction {
    #[serde(rename = "browser")]
    Browser {
        id: String,
        url: String,
        #[serde(default, rename = "requiresConsent")]
        requires_consent: bool,
        #[serde(default, rename = "acceptsCallback")]
        accepts_callback: bool,
    },
    #[serde(rename = "deviceCode")]
    DeviceCode {
        id: String,
        url: String,
        #[serde(rename = "userCode")]
        user_code: String,
    },
    #[serde(rename = "terminal")]
    Terminal { id: String, output: String },
    #[serde(rename = "credentials")]
    Credentials { id: String, fields: Vec<CredentialField> },
}

impl Interaction {
    pub fn id(&self) -> &str {
        match self {
            Interaction::Browser { id, .. }
            | Interaction::DeviceCode { id, .. }
            | Interaction::Terminal { id, .. }
            | Interaction::Credentials { id, .. } => id,
        }
    }
}

/// `ProviderAuthState`. Unknown interactions and methods are dropped, never the whole state.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthState {
    pub phase: String,
    #[serde(default)]
    pub flow_id: Option<String>,
    #[serde(default)]
    pub authorization_url: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// Absent while an ACP agent's methods are still being discovered.
    #[serde(default, deserialize_with = "lenient_methods")]
    pub methods: Option<Vec<AuthMethod>>,
    #[serde(default, deserialize_with = "lenient")]
    pub interaction: Option<Interaction>,
}

impl AuthState {
    pub fn is_active(&self) -> bool {
        matches!(self.phase.as_str(), "starting" | "waiting" | "verifying")
    }

    /// The page to open: the interaction's, else the flow's authorization URL.
    pub fn url(&self) -> Option<&str> {
        match &self.interaction {
            Some(Interaction::Browser { url, .. } | Interaction::DeviceCode { url, .. }) => Some(url),
            _ => self.authorization_url.as_deref(),
        }
    }

    /// Opening the page needs the server to record consent first.
    pub fn requires_consent(&self) -> bool {
        matches!(self.interaction, Some(Interaction::Browser { requires_consent: true, .. }))
    }

    /// Whether the user may paste the final localhost URL when its page does not load.
    pub fn accepts_callback(&self) -> bool {
        self.url().is_some()
            && match &self.interaction {
                Some(Interaction::Browser { accepts_callback, .. }) => *accepts_callback,
                None => true,
                Some(_) => false,
            }
    }
}

/// Decodes one `provider.auth.*` reply or stream item.
pub fn decode_auth_state(value: Value) -> Option<AuthState> {
    serde_json::from_value(value).ok()
}

pub fn subscribe_payload(instance_id: &str) -> Value {
    json!({ "instanceId": instance_id })
}

/// `provider.auth.start`, with the chosen method when the user picked one the state lists.
pub fn start_payload(instance_id: &str, method_id: Option<&str>, state: Option<&AuthState>) -> Value {
    let listed = |id: &str| state.and_then(|state| state.methods.as_ref()).is_some_and(|methods| methods.iter().any(|method| method.id == id));
    match method_id.filter(|id| !id.is_empty() && listed(id)) {
        Some(method_id) => json!({ "instanceId": instance_id, "methodId": method_id }),
        None => json!({ "instanceId": instance_id }),
    }
}

/// `provider.auth.respond` accepting a browser interaction that asked for consent.
pub fn accept_browser_payload(instance_id: &str, state: &AuthState) -> Option<Value> {
    respond_payload(instance_id, state, json!({ "type": "browser", "action": "accept" }))
}

/// `provider.auth.respond` with the credentials form's values.
pub fn credentials_payload(instance_id: &str, state: &AuthState, values: &[(String, String)]) -> Option<Value> {
    let values: Map<String, Value> = values.iter().map(|(name, value)| (name.clone(), json!(value))).collect();
    respond_payload(instance_id, state, json!({ "type": "credentials", "values": values }))
}

/// `provider.auth.respond` typing one line, then Enter, into a terminal sign-in.
pub fn terminal_line_payload(instance_id: &str, state: &AuthState, line: &str) -> Option<Value> {
    let data = format!("{line}\r");
    if data.encode_utf16().count() > TERMINAL_DATA_MAX || !matches!(state.interaction, Some(Interaction::Terminal { .. })) {
        return None;
    }
    respond_payload(instance_id, state, json!({ "type": "terminal", "data": data }))
}

/// `ProviderAuthResponse` terminal `data` caps at 4,096 characters.
const TERMINAL_DATA_MAX: usize = 4_096;

fn respond_payload(instance_id: &str, state: &AuthState, response: Value) -> Option<Value> {
    let flow_id = state.flow_id.as_deref()?;
    let interaction = state.interaction.as_ref()?;
    Some(json!({ "instanceId": instance_id, "flowId": flow_id, "interactionId": interaction.id(), "response": response }))
}

/// `provider.auth.complete` with the callback URL the user pasted.
pub fn complete_payload(instance_id: &str, state: &AuthState, callback_url: &str) -> Option<Value> {
    let callback_url = callback_url.trim();
    let flow_id = state.flow_id.as_deref()?;
    (!callback_url.is_empty()).then(|| json!({ "instanceId": instance_id, "flowId": flow_id, "callbackUrl": callback_url }))
}

pub fn cancel_payload(instance_id: &str, state: &AuthState) -> Option<Value> {
    Some(json!({ "instanceId": instance_id, "flowId": state.flow_id.as_deref()? }))
}

pub fn logout_payload(instance_id: &str) -> Value {
    json!({ "instanceId": instance_id })
}

/// The Account row's main button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountAction {
    /// The provider has no in-app sign-in; its docs explain the setup.
    OpenDocs(String),
    Cancel,
    Start(String),
}

/// What the Account row shows and offers.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountView {
    pub description: String,
    /// "Signed in as …", which replaces the description once signed in.
    pub signed_in_email: Option<String>,
    /// Why the last attempt failed.
    pub failure: Option<String>,
    /// Methods to choose from, when there is more than one.
    pub methods: Vec<AuthMethod>,
    pub url: Option<String>,
    pub device_code: Option<String>,
    pub terminal_output: Option<String>,
    pub credentials: Vec<CredentialField>,
    pub accepts_callback: bool,
    pub action: Option<AccountAction>,
    /// The Start button also needs an enabled, installed provider and a known state.
    pub action_enabled: bool,
    pub can_sign_out: bool,
    /// Everything is disabled while a call runs, the state failed to load, or methods load.
    pub disabled: bool,
}

/// `ProviderAuthenticationSection` for a provider and its auth state.
pub fn account_view(provider: &ServerProvider, state: Option<&AuthState>, environment_label: &str, load_error: bool, pending: bool) -> AccountView {
    let active = state.is_some_and(AuthState::is_active);
    let phase = state.map(|state| state.phase.as_str());
    let signed_in = provider.auth.status == "authenticated" || (provider.auth.status == "unknown" && phase == Some("succeeded"));
    let methods = state.and_then(|state| state.methods.as_ref());
    let discovering = provider.driver == super::ACP_REGISTRY && !active && !signed_in && !load_error && methods.is_none();
    let setup_refuses = provider.setup.as_ref().is_some_and(|setup| !setup.can_authenticate);
    let needs_external_setup =
        !active && !signed_in && (setup_refuses || (provider.driver == super::ACP_REGISTRY && methods.is_some_and(Vec::is_empty)));
    let uses_credentials = provider.driver == "antigravity" && matches!(provider.auth.kind.as_deref(), Some("gemini-api-key" | "agent-platform"));
    let interaction = state.and_then(|state| state.interaction.as_ref());
    let description = if active {
        match phase {
            Some("starting") => state
                .and_then(|state| state.message.clone())
                .unwrap_or_else(|| if uses_credentials { "Connecting\u{2026}" } else { "Starting sign-in\u{2026}" }.to_owned()),
            Some("verifying") => {
                if uses_credentials { "Checking credentials and loading models\u{2026}" } else { "Checking your account\u{2026}" }.to_owned()
            }
            _ => match interaction {
                Some(Interaction::Terminal { .. }) => "Complete sign-in in the terminal below.".to_owned(),
                Some(Interaction::Credentials { .. }) => "Enter your credentials below.".to_owned(),
                _ => "Finish signing in in your browser.".to_owned(),
            },
        }
    } else if signed_in {
        "Signed in.".to_owned()
    } else if discovering {
        "Discovering sign-in methods\u{2026}".to_owned()
    } else if needs_external_setup {
        "No in-app sign-in advertised. Follow the provider's docs to finish setup.".to_owned()
    } else if uses_credentials {
        format!("Connect on {environment_label} to load models.")
    } else {
        format!("Sign in on {environment_label}.")
    };
    let action = if needs_external_setup {
        provider.setup.as_ref().and_then(|setup| setup.documentation_url.clone()).map(AccountAction::OpenDocs)
    } else if active && state.is_some_and(|state| state.flow_id.is_some()) {
        Some(AccountAction::Cancel)
    } else if !active && !setup_refuses {
        let label = if signed_in {
            "Change account"
        } else if matches!(phase, Some("failed" | "cancelled")) {
            if uses_credentials { "Retry connection" } else { "Retry sign-in" }
        } else if uses_credentials {
            "Connect"
        } else {
            "Sign in"
        };
        Some(AccountAction::Start(label.to_owned()))
    } else {
        None
    };
    let disabled = pending || load_error || discovering;
    let can_logout = provider.auth.can_logout.or(provider.setup.as_ref().map(|setup| setup.can_authenticate)).unwrap_or(false);
    AccountView {
        description,
        signed_in_email: (signed_in && !active)
            .then(|| provider.auth.email.as_deref().map(str::trim).filter(|email| !email.is_empty()).map(str::to_owned))
            .flatten(),
        failure: state.filter(|state| state.phase == "failed").and_then(|state| state.message.clone()),
        methods: if active { Vec::new() } else { methods.filter(|methods| methods.len() > 1).cloned().unwrap_or_default() },
        url: state.and_then(AuthState::url).map(str::to_owned),
        device_code: match interaction {
            Some(Interaction::DeviceCode { user_code, .. }) => Some(user_code.clone()),
            _ => None,
        },
        terminal_output: match interaction {
            Some(Interaction::Terminal { output, .. }) => Some(output.clone()),
            _ => None,
        },
        credentials: match interaction {
            Some(Interaction::Credentials { fields, .. }) => fields.clone(),
            _ => Vec::new(),
        },
        accepts_callback: state.is_some_and(AuthState::accepts_callback),
        action_enabled: !disabled && provider.enabled && provider.installed && state.is_some(),
        can_sign_out: !active && signed_in && can_logout,
        action,
        disabled,
    }
}

/// What a failed setup call says: `ProviderSetupError` carries its text in `detail`, and its
/// `message` getter never reaches the wire.
pub fn failure_message(failure: &RpcFailure) -> String {
    failure.error.get("detail").and_then(Value::as_str).map(str::trim).filter(|detail| !detail.is_empty()).map_or_else(|| failure.message.clone(), str::to_owned)
}

/// The confirmation signing out asks for.
pub fn sign_out_prompt(provider_name: &str, environment_label: &str) -> String {
    format!(
        "Sign out of {provider_name} on {environment_label}? This stops running threads that share this sign-in. Thread history is kept."
    )
}

/// `ProviderInstallState`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallState {
    pub phase: String,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub downloaded_bytes: u64,
    #[serde(default)]
    pub total_bytes: Option<u64>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub can_remove: bool,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub executable_path: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

impl InstallState {
    pub fn is_active(&self) -> bool {
        matches!(self.phase.as_str(), "downloading" | "extracting" | "verifying")
    }

    /// The download's progress from 0 to 1, while the size is known.
    pub fn progress(&self) -> Option<f32> {
        let total = self.total_bytes.filter(|total| *total > 0 && self.phase == "downloading")?;
        Some((self.downloaded_bytes as f64 / total as f64).clamp(0., 1.) as f32)
    }
}

pub fn decode_install_state(value: Value) -> Option<InstallState> {
    serde_json::from_value(value).ok()
}

pub fn install_cancel_payload(instance_id: &str, state: &InstallState) -> Option<Value> {
    Some(json!({ "instanceId": instance_id, "operationId": state.operation_id.as_deref()? }))
}

/// What the Antigravity Runtime row shows and offers.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeView {
    pub status: String,
    /// A server message worth showing beside the status.
    pub message: Option<String>,
    pub progress: Option<f32>,
    pub uses_custom_binary: bool,
    pub cannot_install: bool,
    /// "Install Antigravity", "Update Antigravity"… when an install can start.
    pub install_label: Option<String>,
    pub can_cancel: bool,
    pub can_remove: bool,
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / 1_000_000.
}

/// `ProviderSetupActions`' runtime half. `binary_path` is the instance's custom binary, if any.
pub fn runtime_view(provider: &ServerProvider, install: Option<&InstallState>, binary_path: Option<&str>, enabled: bool) -> RuntimeView {
    let active = install.is_some_and(InstallState::is_active);
    let uses_custom_binary = binary_path.is_some_and(|path| !path.trim().is_empty());
    let installed = provider.installed || (!uses_custom_binary && install.is_some_and(|install| install.installed_version.is_some()));
    let status = match install {
        None => "Checking the Antigravity runtime.".to_owned(),
        Some(install) if install.phase == "downloading" => match install.total_bytes {
            Some(total) => format!("Downloading {:.1} MB of {:.1} MB.", megabytes(install.downloaded_bytes), megabytes(total)),
            None => format!("Downloading {:.1} MB.", megabytes(install.downloaded_bytes)),
        },
        Some(install) if install.phase == "extracting" => "Extracting Antigravity.".to_owned(),
        Some(install) if install.phase == "verifying" => "Checking the downloaded runtime.".to_owned(),
        Some(_) if installed => "Installed.".to_owned(),
        Some(_) if uses_custom_binary => {
            if enabled { "The configured Antigravity runtime is unavailable." } else { "The configured Antigravity runtime has not been checked." }.to_owned()
        }
        Some(install) => match install.total_bytes.filter(|total| *total > 0) {
            Some(total) => format!("{} MB download.", (megabytes(total)).ceil() as u64),
            None => "Not installed.".to_owned(),
        },
    };
    let can_install = provider.setup.as_ref().is_some_and(|setup| setup.can_install);
    let install_label = (!active && can_install).then(|| {
        match install {
            Some(install) if install.installed_version.is_some() => {
                if install.version.is_some() && install.version != install.installed_version {
                    "Update Antigravity"
                } else {
                    "Reinstall Antigravity"
                }
            }
            Some(install) if matches!(install.phase.as_str(), "failed" | "cancelled") => "Retry installation",
            _ if installed => "Install managed runtime",
            _ => "Install Antigravity",
        }
        .to_owned()
    });
    RuntimeView {
        message: install.filter(|_| !active).and_then(|install| install.message.clone()).filter(|message| *message != status),
        progress: install.and_then(InstallState::progress),
        uses_custom_binary,
        cannot_install: !installed && !can_install,
        install_label,
        can_cancel: active && install.is_some_and(|install| install.operation_id.is_some()),
        can_remove: !active && install.is_some_and(|install| install.can_remove),
        status,
    }
}

/// The confirmation removing the runtime asks for.
pub fn remove_runtime_prompt(environment_label: &str) -> String {
    format!("Remove the downloaded Antigravity runtime from {environment_label}? Google sign-in and thread history are kept.")
}

fn lenient<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(d)?).ok())
}

fn lenient_methods<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<AuthMethod>>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(values) => Some(values.into_iter().filter_map(|value| serde_json::from_value(value).ok()).collect()),
        _ => None,
    })
}
