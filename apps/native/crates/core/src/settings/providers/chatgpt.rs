//! Managed Codex setup: `CodexSetupSection{,.logic}.tsx`, `ChatGptAccountPicker.tsx`,
//! `AddCodexAccountDialog.tsx` and the ChatGPT schemas in `contracts/src/providerSetup.ts`.

use super::auth::{AuthState, InstallState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const RECONNECT: &str = "provider.chatgpt.reconnect-profile";
pub const IMPORT: &str = "provider.chatgpt.import-profile";
pub const HANDOFF: &str = "provider.chatgpt.handoff.subscribe";
pub const MULTIPLE_ACCOUNTS: &str = "We do not recommend managing multiple Codex accounts in T3 Code.";
pub const WELCOME: &str = "Eligible usage in T3 Code uses your ChatGPT plan. Manage your shared usage and any credit settings in ChatGPT.";

// These types deliberately have no Debug: transferred credentials must never enter a log.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectProfile {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token_hint: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferredProfile {
    pub registration: ReconnectProfile,
    pub credentials: TransferredCredentials,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferredCredentials {
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: String,
    pub issuer: String,
    pub expires_at: f64,
    pub earliest_refresh_at: Option<f64>,
    pub scopes: Vec<String>,
    pub subject: String,
    pub email: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum HandoffState {
    Auth {
        state: AuthState,
    },
    Finished {
        profile: TransferredProfile,
    },
    #[serde(other)]
    Unknown,
}

pub fn reconnect_payload(instance: &str, method: &str) -> Value {
    json!({"instanceId":instance,"methodId":method})
}
pub fn import_payload(instance: &str, profile: &TransferredProfile) -> Value {
    json!({"instanceId":instance,"profile":profile})
}
pub fn handoff_payload(instance: &str, environment: &str, attempt: &str, return_url: &str, profile: Option<&ReconnectProfile>) -> Value {
    json!({"instanceId":instance,"environmentId":environment,"attemptId":attempt,"returnUrl":return_url,"profile":profile})
}

pub fn managed(config: Option<&Value>) -> bool {
    config.is_some_and(|c| c["setupMode"] == "managed")
}
pub fn existing_chosen(config: Option<&Value>) -> bool {
    config.is_some_and(|c| c["setupMode"] == "existing")
}
pub fn local_server(http_base: &str, ssh: bool) -> bool {
    !ssh && url::Url::parse(http_base).ok().and_then(|url| url.host_str().map(str::to_owned)).is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]"))
}
pub fn start_payload(instance: &str, method: &str, return_url: &str, local: bool) -> Value {
    json!({"instanceId":instance,"methodId":method,"returnUrl":return_url,"callbackMode":if local {"server"} else {"client"}})
}
pub fn return_url(http_base: &str, environment: &str, instance: &str, onboarding: bool) -> Option<String> {
    let mut url = url::Url::parse(http_base).ok()?;
    url.set_path(if onboarding { "/welcome" } else { "/settings/providers" });
    url.set_query(None);
    if onboarding {
        url.set_fragment(Some(&format!("agents:{environment}")));
    } else {
        url.query_pairs_mut().append_pair("instanceId", instance);
        url.set_fragment(None);
    }
    Some(url.to_string())
}
pub fn settle_install(started: &InstallState, current: Option<&InstallState>) -> Option<bool> {
    let state = if started.is_active() { current.filter(|s| s.operation_id == started.operation_id)? } else { started };
    (!state.is_active()).then(|| !matches!(state.phase.as_str(), "failed" | "cancelled") && state.installed_version.is_some())
}
pub fn settle_page(state: Option<&AuthState>, flow: Option<&str>) -> Option<Option<String>> {
    let state = state.filter(|s| s.flow_id.is_some() && flow.is_none_or(|f| s.flow_id.as_deref() == Some(f)))?;
    if matches!(state.phase.as_str(), "succeeded" | "failed" | "cancelled") {
        return Some(None);
    }
    (state.phase == "waiting").then(|| state.url().map(str::to_owned)).flatten().map(Some)
}
pub fn needs_install(install: &InstallState) -> bool {
    install.installed_version.is_none() || (install.source.as_deref() != Some("local") && install.version.is_some() && install.version != install.installed_version)
}

#[derive(Default, Clone)]
pub struct SetupState {
    pub auth: Option<AuthState>,
    pub install: Option<InstallState>,
    pub install_operation: Option<String>,
    pub pending: bool,
    pub finishing: bool,
    pub handoff: bool,
    pub transfer_failed: bool,
    pub error: Option<String>,
    pub requested_method: String,
}
impl SetupState {
    pub fn started_install(&mut self, state: InstallState) {
        self.install_operation = state.is_active().then(|| state.operation_id.clone()).flatten();
        self.install = Some(state);
    }
    pub fn observe_install(&mut self, state: InstallState) {
        if self.install_operation.as_ref().is_some_and(|id| state.operation_id.as_ref() != Some(id)) {
            return;
        }
        if !state.is_active() {
            self.install_operation = None;
        }
        self.install = Some(state);
    }
    pub fn auth_active(&self) -> bool {
        self.handoff || self.finishing || self.pending || self.auth.as_ref().is_some_and(AuthState::is_active)
    }
    pub fn install_active(&self) -> bool {
        self.install.as_ref().is_some_and(InstallState::is_active)
    }
    pub fn busy(&self) -> bool {
        self.auth_active() || self.install_active()
    }
    pub fn has_profiles(&self) -> bool {
        self.auth.as_ref().and_then(|a| a.methods.as_ref()).is_some_and(|m| m.iter().any(|m| m.id.starts_with("chatgpt-profile:")))
    }
    pub fn runtime_description(&self) -> String {
        let Some(i) = &self.install else {
            return "Preparing managed setup.".into();
        };
        match i.phase.as_str() {
            "downloading" => {
                format!("Downloading {:.1}{} MB.", i.downloaded_bytes as f64 / 1_000_000., i.total_bytes.map(|b| format!(" of {:.1}", b as f64 / 1_000_000.)).unwrap_or_default())
            }
            "extracting" => "Installing Codex.".into(),
            "verifying" => "Checking Codex.".into(),
            _ if i.installed_version.is_some() => {
                format!(
                    "{}{}.",
                    if i.source.as_deref() == Some("local") { "Using your installed Codex" } else { "Managed by T3 Code" },
                    i.installed_version.as_ref().map(|v| format!(" · v{v}")).unwrap_or_default()
                )
            }
            _ => i.message.clone().unwrap_or_else(|| "T3 Code downloads and manages Codex for you.".into()),
        }
    }
    pub fn account_description(&self, authenticated: bool, email: Option<&str>) -> String {
        let auth = self.auth.as_ref();
        if self.finishing {
            return "Finishing sign-in...".into();
        }
        if self.install_active() {
            return self.runtime_description();
        }
        let method_email = |method: &str| auth.and_then(|a| a.methods.as_ref()).and_then(|m| m.iter().find(|m| m.id == method)).and_then(|m| m.account_email.clone());
        if self.auth_active() || auth.is_some_and(|a| matches!(a.phase.as_str(), "failed" | "cancelled")) {
            if auth.is_some_and(|a| a.phase == "waiting")
                && let Some(email) = method_email(&self.requested_method)
            {
                return format!("Continue as {email} on OpenAI.");
            }
            return auth.and_then(|a| a.message.clone()).unwrap_or_else(|| "Finish signing in in your browser.".into());
        }
        if authenticated {
            return email.filter(|s| !s.trim().is_empty()).map_or_else(|| "Signed in with ChatGPT.".into(), |e| format!("Signed in as {}.", e.trim()));
        }
        method_email("chatgpt").unwrap_or_else(|| "Use your ChatGPT subscription.".into())
    }
}

#[cfg(test)]
mod tests;

pub fn resume_environment(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    let welcome = matches!(url.scheme(), "t3code" | "t3code-dev") && url.host_str() == Some("welcome") || matches!(url.scheme(), "http" | "https") && url.path() == "/welcome";
    if !welcome || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    url.fragment()?.strip_prefix("agents:").filter(|v| !v.is_empty() && v.len() <= 128).map(str::to_owned)
}

pub fn mode_payload(settings: &crate::server_config::ServerSettings, instance: &str, managed: bool) -> Value {
    use crate::server_config::ProviderInstanceConfig;
    let previous = settings.provider_instances.get(instance).cloned().unwrap_or_else(|| ProviderInstanceConfig { driver: "codex".into(), ..Default::default() });
    let mut next = previous.clone();
    next.enabled = Some(true);
    let config = next.config.get_or_insert_with(|| json!({}));
    if !config.is_object() {
        *config = json!({});
    }
    config["enabled"] = json!(true);
    config["setupMode"] = json!(if managed { "managed" } else { "existing" });
    super::upsert_payload(settings, instance, &previous, &next)
}

pub fn new_account_payload(instance: &str, name: &str) -> Option<Value> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let config = crate::server_config::ProviderInstanceConfig {
        driver: "codex".into(),
        display_name: Some(format!("ChatGPT - {name}")),
        enabled: Some(true),
        config: Some(json!({"enabled":true,"setupMode":"managed"})),
        ..Default::default()
    };
    Some(super::create_payload(instance, &config))
}

pub fn picker_selection(methods: &[super::auth::AuthMethod], selection: &str) -> String {
    if selection == "chatgpt-change-account" {
        return selection.to_owned();
    }
    methods
        .iter()
        .filter(|m| m.id.starts_with("chatgpt-profile:"))
        .find(|m| m.id == selection)
        .or_else(|| methods.iter().find(|m| m.id.starts_with("chatgpt-profile:")))
        .map_or_else(|| "chatgpt-change-account".into(), |m| m.id.clone())
}
