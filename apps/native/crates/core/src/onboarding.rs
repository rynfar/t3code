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

pub mod phone;
pub mod projects;
pub mod terminal;

pub use phone::{
    MOBILE_APP_URL, PHONE_LINK_REFRESH_INTERVAL_SECS, PhoneCloudAccount, PhoneConnectInput,
    PhoneConnectState, WelcomePhoneOfferInput, phone_account_for_identity, phone_is_shared,
    resolve_phone_connect_state, welcome_offers_phone,
};

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

/// The footer's telemetry notice up to its link ("…see the" / "…See the"), matching
/// `TelemetryNotice` (`WelcomePage.tsx`) and `managedTelemetryOffMessage`
/// (`web/src/state/managedPolicy.ts`).
pub fn privacy_notice(policy: Option<&Value>) -> String {
    match crate::managed_policy::managed_telemetry_off_message(policy) {
        Some(message) => format!("{message} See the"),
        None => "T3 Code collects anonymous usage data to improve the app. To opt out, see the".to_owned(),
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

/// `isBrokenVersion` (`apps/web/src/onboarding/welcomePage.logic.ts`, `pingdotgg/t3code#33209`).
pub fn is_broken_version(provider: &ServerProvider) -> bool {
    provider.installed
        && provider
            .compatibility_advisory
            .as_ref()
            .is_some_and(|advisory| advisory.status == "broken")
}

/// `isProviderAvailable` check: unavailable providers cannot be used without attention.
pub fn is_provider_available(provider: &ServerProvider) -> bool {
    provider.availability.as_deref() != Some("unavailable")
}

/// `welcomeAgentState` (`welcomePage.logic.ts`, `pingdotgg/t3code#33209`).
pub fn welcome_agent_state(provider: &ServerProvider, settings: Option<&ServerSettings>) -> AgentState {
    if !is_provider_available(provider) {
        return AgentState::Attention;
    }
    if let Some(settings) = settings
        && let Some(instance) = settings.provider_instances.get(&provider.instance_id)
        && instance.enabled == Some(false)
    {
        return AgentState::Disabled;
    }
    let state = agent_state(Some(provider));
    if state == AgentState::Ready && is_broken_version(provider) {
        return AgentState::Attention;
    }
    if state == AgentState::Checking && provider.driver == "acpRegistry" {
        return AgentState::Attention;
    }
    state
}

/// Grouped agents for the single Welcome page (`groupWelcomeAgents`, `pingdotgg/t3code#33209`).
#[derive(Debug, Clone, PartialEq)]
pub struct WelcomeAgentGroups<'a> {
    pub checking: bool,
    pub ready_count: usize,
    /// Ready first, then SignIn, then Attention, sorted by display name within each state.
    pub found: Vec<&'a ServerProvider>,
    /// Install, Disabled, or Checking agents the user can add, sorted by display name.
    pub available: Vec<&'a ServerProvider>,
}

/// Splits allowed providers into agents already found on the computer and agents available to add.
pub fn group_welcome_agents<'a>(
    providers: Option<&'a [ServerProvider]>,
    settings: Option<&ServerSettings>,
    managed_policy: Option<&Value>,
) -> WelcomeAgentGroups<'a> {
    let mut checking = providers.is_none();
    let mut ready_count = 0usize;
    let mut found: Vec<(&'a ServerProvider, u8)> = Vec::new();
    let mut available: Vec<&'a ServerProvider> = Vec::new();
    for provider in providers.unwrap_or_default() {
        if !managed_driver_allowed(managed_policy, &provider.driver) {
            continue;
        }
        let state = welcome_agent_state(provider, settings);
        if state == AgentState::Ready {
            ready_count += 1;
        }
        if state == AgentState::Checking {
            checking = true;
        }
        match state {
            AgentState::Ready => found.push((provider, 0)),
            AgentState::SignIn => found.push((provider, 1)),
            AgentState::Attention => found.push((provider, 2)),
            AgentState::Install | AgentState::Disabled | AgentState::Checking => available.push(provider),
        }
    }
    let name_key = |provider: &ServerProvider| agent_display_name(&provider.driver, Some(provider)).to_lowercase();
    found.sort_by(|(a, a_ord), (b, b_ord)| a_ord.cmp(b_ord).then_with(|| name_key(a).cmp(&name_key(b))));
    available.sort_by_key(|a| name_key(a));
    WelcomeAgentGroups {
        checking,
        ready_count,
        found: found.into_iter().map(|(provider, _)| provider).collect(),
        available,
    }
}

/// `cliTerminalStep` (`welcomePage.logic.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliTerminalStep {
    Wait,
    Open,
    Done,
}

pub fn cli_terminal_step(provider: &ServerProvider, settings: Option<&ServerSettings>) -> CliTerminalStep {
    if provider.setup.as_ref().is_some_and(|setup| setup.can_authenticate) {
        return CliTerminalStep::Wait;
    }
    let state = welcome_agent_state(provider, settings);
    if state == AgentState::Checking {
        return CliTerminalStep::Wait;
    }
    if is_broken_version(provider) {
        return CliTerminalStep::Done;
    }
    if provider.installed && provider.auth.status == "authenticated" {
        return CliTerminalStep::Done;
    }
    if matches!(state, AgentState::Install | AgentState::SignIn)
        || (state == AgentState::Attention && is_provider_available(provider))
    {
        CliTerminalStep::Open
    } else {
        CliTerminalStep::Done
    }
}

/// `codexNeedsSetup` (`welcomePage.logic.ts`).
pub fn codex_needs_setup(provider: &ServerProvider, settings: Option<&ServerSettings>) -> bool {
    if provider.driver != "codex"
        || provider.auth.status == "authenticated"
        || is_broken_version(provider)
    {
        return false;
    }
    let state = welcome_agent_state(provider, settings);
    matches!(state, AgentState::Install | AgentState::SignIn)
        || (state == AgentState::Attention && is_provider_available(provider))
}

/// Status of the local `t3` CLI command (`pingdotgg/t3code#33150`, `pingdotgg/t3code#33153`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliCommandStatus {
    pub installed_path: Option<String>,
    pub on_path: bool,
    pub shadowed_by: Option<String>,
    pub path_dir: String,
    /// Shell profile path (`~/.zshrc`, `~/.bashrc`, `~/.config/fish/config.fish`) when safe to edit
    /// (`packages/shared/src/shell.ts` `pathSetup`). `None` on Windows, macOS `bash`, unknown shells,
    /// or unsafe paths.
    pub profile_file: Option<String>,
    /// Line to append to `profile_file` (`fish_add_path --append --path …` for fish,
    /// `export PATH="$PATH:…"` for zsh / non-macOS bash).
    pub path_line: Option<String>,
}

impl CliCommandStatus {
    pub fn description(&self) -> String {
        if let Some(shadowed) = &self.shadowed_by {
            return format!("Another t3 at {shadowed} runs first in a new terminal. Remove it to use T3 Code's.");
        }
        match &self.installed_path {
            None => "Run T3 Code's CLI as `t3` from any terminal.".to_owned(),
            Some(path) if self.on_path => format!("Installed at {path}. Open a new terminal to use it."),
            Some(path) => match &self.profile_file {
                Some(profile) => format!("Installed at {path}, which is not on your PATH yet. Add {} to {profile}.", self.path_dir),
                None => format!("Installed at {path}, which is not on your PATH yet. Add {} to your shell PATH.", self.path_dir),
            },
        }
    }

    /// Shell command to paste that appends `path_line` to `profile_file` (`pathSetupCommand`).
    pub fn path_setup_command(&self) -> Option<String> {
        let file = self.profile_file.as_deref()?;
        let line = self.path_line.as_deref()?;
        let append = format!("printf '\\n%s\\n' '{line}' >> {file}");
        if file.ends_with(".fish") {
            let dir = file.rsplit_once('/').map_or(file, |(dir, _)| dir);
            Some(format!("mkdir -p {dir} && {append}"))
        } else {
            Some(append)
        }
    }
}

fn is_safe_shell_path(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-'))
}

/// Exact line match (ignoring blank lines and `#` comments) so substring hits in comments or
/// unrelated paths never suppress PATH setup.
pub fn profile_contains_line(existing: &str, line: &str) -> bool {
    let wanted = line.trim();
    existing.lines().any(|candidate| {
        let trimmed = candidate.trim();
        !trimmed.is_empty() && !trimmed.starts_with('#') && trimmed == wanted
    })
}

/// Host shell environment inputs for `path_setup` (`packages/shared/src/shell.ts` `PathSetupInput`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ShellEnv<'a> {
    pub platform: &'a str,
    pub home: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub config_home: Option<&'a str>,
    pub zdotdir: Option<&'a str>,
}

/// `pathSetup` (`packages/shared/src/shell.ts`): returns `(profile_file, line)` that puts
/// `directory` last on PATH for new zsh, fish, or non-macOS bash terminals.
pub fn path_setup(directory: &str, env: &ShellEnv<'_>) -> Option<(String, String)> {
    if env.platform == "windows" || env.platform == "win32" || !directory.starts_with('/') {
        return None;
    }
    let home = env.home.filter(|h| !h.is_empty())?;
    let home_prefix = format!("{}/", home.trim_end_matches('/'));
    let (under_home, rest) = match directory.strip_prefix(&home_prefix) {
        Some(tail) => (true, format!("/{tail}")),
        None => (false, directory.to_owned()),
    };
    if !is_safe_shell_path(&rest) {
        return None;
    }
    let dir_expr = if under_home { format!("$HOME{rest}") } else { rest };
    let shell_name = env.shell.and_then(|s| s.rsplit('/').next());
    let read_from = |value: Option<&str>, fallback: &str| -> Option<String> {
        match value.filter(|v| !v.is_empty()) {
            None => Some(fallback.to_owned()),
            Some(v) if v.starts_with('/') && is_safe_shell_path(v) => Some(v.to_owned()),
            Some(_) => None,
        }
    };
    if shell_name == Some("fish") {
        let config = read_from(env.config_home, "~/.config")?;
        return Some((
            format!("{config}/fish/config.fish"),
            format!("fish_add_path --append --path {dir_expr}"),
        ));
    }
    let is_macos = env.platform == "macos" || env.platform == "darwin";
    let file = match shell_name {
        Some("zsh") => format!("{}/.zshrc", read_from(env.zdotdir, "~")?),
        Some("bash") if !is_macos => "~/.bashrc".to_owned(),
        _ => return None,
    };
    Some((file, format!("export PATH=\"$PATH:{dir_expr}\"")))
}

/// Detects local `t3` CLI installation & PATH status from the resolved T3 home and `PATH`.
pub fn detect_cli_command_status(
    t3_bin_dir: &std::path::Path,
    t3_bin_installed: bool,
    path_entries: &[(&str, bool)],
    env: &ShellEnv<'_>,
) -> CliCommandStatus {
    let path_dir = t3_bin_dir.to_string_lossy().into_owned();
    let installed_path = t3_bin_installed.then(|| t3_bin_dir.join("t3").to_string_lossy().into_owned());
    let mut on_path = false;
    let mut shadowed_by = None;
    for (dir, has_t3) in path_entries {
        let same_dir = std::path::Path::new(dir) == t3_bin_dir;
        if same_dir {
            on_path = true;
            break;
        }
        if *has_t3 && shadowed_by.is_none() {
            shadowed_by = Some(std::path::Path::new(dir).join("t3").to_string_lossy().into_owned());
        }
    }
    if !on_path {
        on_path = path_entries.iter().any(|(dir, _)| std::path::Path::new(dir) == t3_bin_dir);
    }
    let (profile_file, path_line) = match path_setup(&path_dir, env) {
        Some((file, line)) => (Some(file), Some(line)),
        None => (None, None),
    };
    CliCommandStatus {
        installed_path,
        on_path,
        shadowed_by,
        path_dir,
        profile_file,
        path_line,
    }
}

/// Shell command to install the `t3` CLI command (`InstallT3Commands.tsx`, `pingdotgg/t3code#33150`).
pub const INSTALL_T3_COMMAND_UNIX: &str = "curl -fsSL https://t3.codes/install.sh | sh";
pub const INSTALL_T3_COMMAND_NPX: &str = "npx t3";

#[cfg(test)]
mod tests;
