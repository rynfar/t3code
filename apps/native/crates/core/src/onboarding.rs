//! First-run onboarding (ledger CN-WELCOME), the pure parts. The gate ports
//! `apps/web/src/onboarding/firstRun.logic.ts`; agent readiness and the setup commands port
//! `providerReadiness.logic.ts`; the agent list follows `ConnectedAgentsStep` in
//! `components/onboarding/WelcomeWizard.tsx`, filtered by `isManagedDriverAllowed`
//! (`contracts/src/managedPolicy.ts`); the privacy line uses `managedTelemetryOffMessage`
//! (`web/src/state/managedPolicy.ts`). The welcome is `ServerLifecycleWelcomePayload`.
//!
//! Native has one environment connected at a time and reads its settings and saved environments
//! synchronously, so the web gate's hydration, catalog and multi-environment inputs fall away:
//! "live" (connected, shell replayed) stands for its bootstrapped and authoritative checks. With
//! nothing saved the wizard opens at once, since there is no workspace to inspect
//! (`resolve_unpaired`).

pub mod projects;
pub mod terminal;

use serde::Deserialize;
use serde_json::Value;

use crate::server_config::{ServerProvider, ServerSettings};
use crate::shell::{ProjectShell, ThreadShell};
use crate::sidebar::grouping::normalize_project_path;

pub const SUBSCRIBE_SERVER_LIFECYCLE: &str = "subscribeServerLifecycle";

/// `PRIVACY_POLICY_URL` (`web/src/legalLinks.ts`).
pub const PRIVACY_POLICY_URL: &str = "https://t3.codes/privacy-policy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRunDecision {
    Pending,
    App,
    Wizard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstRunResolution {
    pub decision: FirstRunDecision,
    /// The gate settled on the app for an established workspace: save onboarding as done.
    pub persist_completion: bool,
}

const fn resolution(decision: FirstRunDecision, persist_completion: bool) -> FirstRunResolution {
    FirstRunResolution { decision, persist_completion }
}

/// What the connected environment shows about its workspace (`FirstRunDecisionInput`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FirstRunEvidence {
    pub completed: bool,
    /// Connected, with the shell replayed, so its projects and threads are the server's word.
    pub live: bool,
    pub provenance_authoritative: bool,
    pub server_config_available: bool,
    pub workspace_fresh: bool,
    pub project_count: usize,
    pub thread_count: usize,
}

/// `resolveFirstRunDecision`.
pub fn resolve_first_run_decision(evidence: &FirstRunEvidence) -> FirstRunResolution {
    if evidence.completed {
        return resolution(FirstRunDecision::App, false);
    }
    if evidence.project_count > 1 || evidence.thread_count > 1 {
        return resolution(FirstRunDecision::App, evidence.live && evidence.server_config_available);
    }
    if !evidence.live || !evidence.provenance_authoritative || !evidence.server_config_available {
        return resolution(FirstRunDecision::Pending, false);
    }
    if evidence.workspace_fresh { resolution(FirstRunDecision::Wizard, false) } else { resolution(FirstRunDecision::App, true) }
}

/// The gate with no environment connected: a launch with nothing saved opens the wizard; with
/// saved environments it waits for one to connect, showing the connect screen meanwhile.
pub fn resolve_unpaired(completed: bool, saved_environments: usize) -> FirstRunDecision {
    match (completed, saved_environments) {
        (true, _) => FirstRunDecision::App,
        (false, 0) => FirstRunDecision::Wizard,
        (false, _) => FirstRunDecision::Pending,
    }
}

/// `ServerLifecycleWelcomePayload`: what the server bootstrapped from its working directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerWelcome {
    #[serde(default)]
    pub bootstrap_status: Option<String>,
    #[serde(default)]
    pub bootstrap_project_id: Option<String>,
    #[serde(default)]
    pub bootstrap_thread_id: Option<String>,
    #[serde(default)]
    pub bootstrap_project_created: Option<bool>,
    #[serde(default)]
    pub bootstrap_thread_created: Option<bool>,
}

impl ServerWelcome {
    /// `isFirstRunWorkspaceProvenanceAuthoritative` for a received welcome.
    pub fn is_authoritative(&self) -> bool {
        self.bootstrap_status.as_deref() != Some("pending")
    }
}

/// The welcome in one `subscribeServerLifecycle` item; `None` for the other events.
pub fn lifecycle_welcome(event: &Value) -> Option<ServerWelcome> {
    if event.get("type")?.as_str()? != "welcome" {
        return None;
    }
    serde_json::from_value(event.get("payload")?.clone()).ok()
}

/// No run, no provider session and no user message yet (`latestRun` and `runtime` are null).
fn thread_is_unused(thread: &ThreadShell) -> bool {
    thread.latest_run_id.is_none()
        && thread.active_provider_thread_id.is_none()
        && thread.pending_background_tasks.is_empty()
        && thread.latest_user_message_at.is_none()
}

/// `isFreshFirstRunWorkspace`: empty, or only the unused project and thread the server created
/// from its working directory (an `npx t3` start).
pub fn is_fresh_first_run_workspace(server_cwd: Option<&str>, welcome: &ServerWelcome, projects: &[ProjectShell], threads: &[&ThreadShell]) -> bool {
    if projects.len() > 1 || threads.len() > 1 {
        return false;
    }
    let project = projects.first();
    if let Some(project) = project {
        let at_cwd = server_cwd.is_some_and(|cwd| normalize_project_path(&project.workspace_root) == normalize_project_path(cwd));
        if welcome.bootstrap_project_created != Some(true) || welcome.bootstrap_project_id.as_deref() != Some(project.id.as_str()) || !at_cwd {
            return false;
        }
    }
    let Some(thread) = threads.first() else { return true };
    project.is_some_and(|project| thread.project_id == project.id)
        && welcome.bootstrap_thread_created == Some(true)
        && welcome.bootstrap_thread_id.as_deref() == Some(thread.id.as_str())
        && thread_is_unused(thread)
}

/// `getOnboardingProviderState`, ordered by `PROVIDER_STATE_PRIORITY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentState {
    Checking,
    Disabled,
    Install,
    Attention,
    SignIn,
    Ready,
}

pub fn agent_state(provider: Option<&ServerProvider>) -> AgentState {
    let Some(provider) = provider else { return AgentState::Checking };
    if !provider.enabled || provider.status == "disabled" {
        return AgentState::Disabled;
    }
    if !provider.installed && provider.status == "warning" && provider.auth.status == "unknown" {
        return AgentState::Checking;
    }
    if !provider.installed {
        return AgentState::Install;
    }
    if provider.auth.status == "unauthenticated" {
        return AgentState::SignIn;
    }
    if provider.status == "ready" { AgentState::Ready } else { AgentState::Attention }
}

/// `selectOnboardingProvidersByDriver(...).get(driver)`: the most usable instance, the first on a tie.
pub fn best_instance<'a>(providers: &'a [ServerProvider], driver: &str) -> Option<&'a ServerProvider> {
    providers.iter().filter(|provider| provider.driver == driver).fold(None, |best: Option<&ServerProvider>, provider| match best {
        Some(best) if agent_state(Some(provider)) <= agent_state(Some(best)) => Some(best),
        _ => Some(provider),
    })
}

/// `PRIMARY_AGENT_DRIVERS`.
pub const PRIMARY_AGENT_DRIVERS: [&str; 2] = ["codex", "claudeAgent"];

/// One card of the agents step: a driver, and the instance the server reports for it if any.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnboardingAgent<'a> {
    pub driver: &'static str,
    pub provider: Option<&'a ServerProvider>,
}

/// The agents step's cards: every Codex instance (or one placeholder) and the best Claude
/// instance, leaving out drivers a managed policy does not allow.
pub fn onboarding_agents<'a>(providers: &'a [ServerProvider], managed_policy: Option<&Value>) -> Vec<OnboardingAgent<'a>> {
    PRIMARY_AGENT_DRIVERS
        .into_iter()
        .filter(|driver| managed_driver_allowed(managed_policy, driver))
        .flat_map(|driver| {
            let instances: Vec<_> =
                if driver == "codex" { providers.iter().filter(|provider| provider.driver == driver).map(Some).collect() } else { Vec::new() };
            if instances.is_empty() { vec![best_instance(providers, driver)] } else { instances }
                .into_iter()
                .map(move |provider| OnboardingAgent { driver, provider })
        })
        .collect()
}

/// `AgentCard`'s name: the server's display name, else Claude Code or the driver's label.
pub fn agent_display_name(driver: &str, provider: Option<&ServerProvider>) -> String {
    match provider.and_then(|provider| provider.display_name.as_deref()).filter(|name| !name.is_empty()) {
        Some(name) => name.to_owned(),
        None if driver == "claudeAgent" => "Claude Code".to_owned(),
        None => crate::settings::providers::find_driver(driver).map_or_else(|| driver.to_owned(), |meta| meta.label.to_owned()),
    }
}

/// `isManagedDriverAllowed`: no `allowedDrivers` list allows every driver.
pub fn managed_driver_allowed(policy: Option<&Value>, driver: &str) -> bool {
    match policy.and_then(|policy| policy.pointer("/limits/providers/allowedDrivers")).and_then(Value::as_array) {
        Some(allowed) => allowed.iter().any(|allowed| allowed.as_str() == Some(driver)),
        None => true,
    }
}

/// The connection step's privacy line up to its link ("…see our"), with
/// `managedTelemetryOffMessage` when an administrator turned usage data off.
pub fn privacy_notice(policy: Option<&Value>) -> String {
    let telemetry_off = policy
        .filter(|policy| policy.pointer("/limits/telemetry") == Some(&Value::Bool(false)))
        .map(|policy| format!("{} turned off anonymous usage data.", policy.get("organization").and_then(Value::as_str).unwrap_or_default()));
    match telemetry_off {
        Some(message) => format!("{message} To read more about how your data is used, see our"),
        None => "T3 Code collects anonymous usage data to help us improve it. To read more about how your data is used and how to opt out, see our".to_owned(),
    }
}

/// `NATIVE_INSTALL_COMMANDS`: installers that need no Node or npm.
pub fn install_command(driver: &str, os: &str) -> Option<&'static str> {
    let windows = os == "windows";
    match driver {
        "claudeAgent" if windows => Some("irm https://claude.ai/install.ps1 | iex"),
        "claudeAgent" => Some("curl -fsSL https://claude.ai/install.sh | bash"),
        "codex" if windows => Some("irm https://chatgpt.com/codex/install.ps1 | iex"),
        "codex" => Some("curl -fsSL https://chatgpt.com/codex/install.sh | sh"),
        _ => None,
    }
}

/// `SAFE_SHELL_BINARY_PATTERN`.
fn is_safe_binary(path: &str) -> bool {
    !path.is_empty() && path.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_./:\\-".contains(&byte))
}

/// `quoteProviderBinary`: quotes for the server's shell, keeping `~/` expandable.
fn quote_binary(path: &str, fallback: &str, os: &str) -> String {
    if is_safe_binary(path) && (os == "windows" || !path.contains('\\')) {
        return path.to_owned();
    }
    match os {
        "windows" => format!("& '{}'", path.replace('\'', "''")),
        "darwin" | "linux" => {
            let quote = |text: &str| format!("'{}'", text.replace('\'', r#"'"'"'"#));
            match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
                Some(rest) => format!("~/{}", quote(rest)),
                None => quote(path),
            }
        }
        _ => fallback.to_owned(),
    }
}

/// `resolveOnboardingProviderLoginCommand`, with the instance's configured binary.
pub fn login_command(provider: &ServerProvider, settings: &ServerSettings, os: &str) -> String {
    let (fallback, verb) = match provider.driver.as_str() {
        "claudeAgent" => ("claude", "auth login"),
        "codex" => ("codex", "login"),
        other => return other.to_owned(),
    };
    let binary = settings
        .provider_instances
        .get(&provider.instance_id)
        .and_then(|instance| instance.config.as_ref())
        .and_then(|config| config.get("binaryPath"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(fallback);
    format!("{} {verb}", quote_binary(binary, fallback, os))
}

#[cfg(test)]
mod tests;
