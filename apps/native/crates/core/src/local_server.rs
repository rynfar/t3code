//! This Mac's own T3 Code server, found the way `t3 pair` finds it (`discoverPairTarget` in
//! `apps/server/src/cli/pair.ts`): the runtime state a running server writes
//! (`PersistedServerRuntimeState` in `apps/server/src/serverRuntimeState.ts`) under the T3 home,
//! and the pairing link `t3 pair` prints (`formatPairOutput`).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::pairing::parse_pairing_link;

/// What the server's connections list calls this device.
pub const PAIR_LABEL: &str = "T3 Code Native";
/// The token is redeemed right away; a short life keeps an unused one harmless.
pub const PAIR_TTL: &str = "2m";
/// `SERVER_EXIT_CODE_MANAGED_POLICY` (`packages/contracts/src/desktopBootstrap.ts`, `pingdotgg/t3code#30319`):
/// the server refused to start under this machine's managed policy, so diagnostics blame the
/// policy rather than pointing at another release.
pub const SERVER_EXIT_CODE_MANAGED_POLICY: i32 = 77;
/// `SERVER_EXIT_CODE_STATE_DIR_OWNED`: a supervisor must not restart a server refused by the
/// state-directory owner.
pub const SERVER_EXIT_CODE_STATE_DIR_OWNED: i32 = 78;

/// Default loopback interface bound when hosting a local server (`DesktopBackendConfiguration.ts`).
pub const DEFAULT_LOCAL_SERVER_HOST: &str = "127.0.0.1";
/// Wildcard bind host when sharing the embedded local server on the local network.
pub const LAN_BIND_HOST: &str = "0.0.0.0";
/// Default HTTPS port used when exposing the local server with `--tailscale-serve`.
pub const DEFAULT_TAILSCALE_SERVE_PORT: u16 = 443;
/// Default HTTP/WebSocket port (`ServerConfig.DEFAULT_PORT`).
pub const DEFAULT_LOCAL_SERVER_PORT: u16 = 3773;
/// Unauthenticated readiness & descriptor probe path (`BACKEND_READINESS_PATH`).
pub const LOCAL_SERVER_READINESS_PATH: &str = "/.well-known/t3/environment";
/// Initial exponential restart backoff (`INITIAL_RESTART_DELAY` in `DesktopBackendManager.ts`).
pub const INITIAL_RESTART_DELAY_MS: u64 = 500;
/// Cap on exponential restart backoff (`MAX_RESTART_DELAY` in `DesktopBackendManager.ts`).
pub const MAX_RESTART_DELAY_MS: u64 = 10_000;
/// Startup crash restart limit (`STARTUP_FAILURE_RESTART_ATTEMPTS` in `DesktopBackendManager.ts`).
pub const MAX_STARTUP_RESTART_ATTEMPTS: u32 = 3;
/// Grace period between SIGTERM and SIGKILL (`DEFAULT_BACKEND_TERMINATE_GRACE`).
pub const TERMINATE_GRACE_MS: u64 = 5_000;
/// Bounded diagnostic tail retained from subprocess stdout/stderr (`STARTUP_FAILURE_OUTPUT_CHARS`).
pub const OUTPUT_TAIL_MAX_CHARS: usize = 4_000;

/// Copy shown when exit 77 stops local startup (`DesktopStartupRecovery.ts`, `pingdotgg/t3code#30319`).
pub const MANAGED_POLICY_STARTUP_MESSAGE: &str =
    "Your organization's managed policy stopped T3 Code from starting. Send the details below to your IT administrator.";

/// Classification of a local server or CLI process exit (`DesktopBackendManager.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupExitKind {
    ManagedPolicy { detail: String },
    StateDirOwned,
    Other { exit_code: Option<i32>, detail: String },
}

/// Classifies an unsuccessful local T3 process exit.
pub fn classify_server_exit(exit_code: Option<i32>, stderr: &str, stdout: &str) -> StartupExitKind {
    match exit_code {
        Some(SERVER_EXIT_CODE_MANAGED_POLICY) => StartupExitKind::ManagedPolicy {
            detail: managed_policy_diagnostic(stderr, stdout),
        },
        Some(SERVER_EXIT_CODE_STATE_DIR_OWNED) => StartupExitKind::StateDirOwned,
        other => StartupExitKind::Other {
            exit_code: other,
            detail: cli_failure_summary(stderr, stdout),
        },
    }
}

/// Whether an automatic restart loop should be suppressed for `exit_code`.
/// Exit 78 (state dir owned by another process) and exit 77 (managed policy refusal)
/// cannot be cured by restarting the same process in a tight loop.
pub fn should_suppress_restart(exit_code: Option<i32>) -> bool {
    matches!(exit_code, Some(SERVER_EXIT_CODE_MANAGED_POLICY | SERVER_EXIT_CODE_STATE_DIR_OWNED))
}

/// User-facing diagnostic for a managed-policy exit 77 refusal, combining the policy message
/// with the underlying error line when present.
pub fn managed_policy_diagnostic(stderr: &str, stdout: &str) -> String {
    let summary = cli_failure_summary(stderr, stdout);
    if summary.is_empty() || summary == "it exited without a message." {
        MANAGED_POLICY_STARTUP_MESSAGE.to_owned()
    } else if summary.contains("managed policy") {
        format!("{MANAGED_POLICY_STARTUP_MESSAGE} ({summary})")
    } else {
        format!("{MANAGED_POLICY_STARTUP_MESSAGE} Details: {summary}")
    }
}

/// Recovery offer when the local backend fails to start (`StartupRecoveryOffer` in
/// `apps/desktop/src/app/DesktopStartupRecovery.ts`, `pingdotgg/t3code#30319`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupRecoveryOffer {
    Update { version: String },
    ManagedPolicy,
    Reinstall { version: String },
    Releases,
}

/// Resolves what recovery action to show on local startup failure. A managed-policy refusal
/// (`exit 77`) never points at an older release or the releases page; only a newer update can help.
pub fn resolve_startup_recovery_offer(
    available_update: Option<&str>,
    last_started_version: Option<&str>,
    managed_policy: bool,
) -> StartupRecoveryOffer {
    if let Some(version) = available_update.map(str::trim).filter(|v| !v.is_empty()) {
        return StartupRecoveryOffer::Update { version: version.to_owned() };
    }
    if managed_policy {
        return StartupRecoveryOffer::ManagedPolicy;
    }
    if let Some(version) = last_started_version.map(str::trim).filter(|v| !v.is_empty()) {
        return StartupRecoveryOffer::Reinstall { version: version.to_owned() };
    }
    StartupRecoveryOffer::Releases
}

pub fn describe_startup_recovery_offer(offer: &StartupRecoveryOffer) -> String {
    match offer {
        StartupRecoveryOffer::Update { version } => format!("Update to T3 Code v{version} to continue."),
        StartupRecoveryOffer::ManagedPolicy => MANAGED_POLICY_STARTUP_MESSAGE.to_owned(),
        StartupRecoveryOffer::Reinstall { version } => format!("Reinstall T3 Code v{version} from the releases page."),
        StartupRecoveryOffer::Releases => "Get a different version from the releases page.".to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRuntimeState {
    pub version: u32,
    pub pid: u32,
    pub port: u16,
    /// The loopback-reachable `http://host:port` the server listens on.
    pub origin: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub dev_url: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub server_version: Option<String>,
}

/// Version 1 state with an http(s) origin; anything else reads as no server.
pub fn parse_runtime_state(text: &str) -> Option<ServerRuntimeState> {
    let state: ServerRuntimeState = serde_json::from_str(text).ok()?;
    let origin = url::Url::parse(&state.origin).ok()?;
    let valid = state.version == 1 && state.pid > 0 && matches!(origin.scheme(), "http" | "https") && origin.host_str().is_some();
    valid.then(|| ServerRuntimeState { origin: state.origin.trim_end_matches('/').to_owned(), ..state })
}

/// `T3CODE_HOME`, else `~/.t3` (`resolveBaseDir`), expanding a leading `~`. A relative value is
/// taken from `cwd`, the directory the app was launched in, so discovery and the CLI (which runs
/// elsewhere) agree on one absolute home.
pub fn t3_home(configured: Option<&str>, user_home: &Path, cwd: &Path) -> PathBuf {
    let home = match configured.map(str::trim).filter(|value| !value.is_empty()) {
        Some("~") => user_home.to_path_buf(),
        Some(value) => value.strip_prefix("~/").map_or_else(|| PathBuf::from(value), |rest| user_home.join(rest)),
        None => user_home.join(".t3"),
    };
    if home.is_absolute() { home } else { cwd.join(home) }
}

/// Where a running server records itself, in the order `t3 pair` checks: the release state
/// directory, then the dev one.
pub fn runtime_state_paths(home: &Path) -> [PathBuf; 2] {
    [home.join("userdata").join("server-runtime.json"), home.join("dev").join("server-runtime.json")]
}

/// Where to look for the `t3` command, best first. An explicit override is the only candidate.
/// Apps opened from the Finder don't inherit the shell's PATH, so the launcher the desktop app
/// installs under the T3 home (`DesktopCliShim.ts`) and the usual Homebrew prefixes come first.
pub fn cli_candidates(override_path: Option<&str>, home: &Path, user_home: &Path, path_var: Option<&str>) -> Vec<PathBuf> {
    if let Some(path) = override_path.map(str::trim).filter(|path| !path.is_empty()) {
        return vec![PathBuf::from(path)];
    }
    let mut candidates = vec![home.join("bin").join("t3"), user_home.join(".t3").join("bin").join("t3")];
    candidates.extend(path_var.unwrap_or_default().split(':').filter(|dir| !dir.is_empty()).map(|dir| Path::new(dir).join("t3")));
    candidates.extend(["/opt/homebrew/bin/t3", "/usr/local/bin/t3"].map(PathBuf::from));
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|path| seen.insert(path.clone()));
    candidates
}

/// `t3 pair` arguments: a short-lived token labelled for this app, aimed at `home` unless it is
/// the home the CLI resolves by itself.
pub fn pair_args(home: &Path, default_home: &Path) -> Vec<String> {
    let mut args = ["pair", "--label", PAIR_LABEL, "--ttl", PAIR_TTL].map(str::to_owned).to_vec();
    if home != default_home {
        args.extend(["--base-dir".to_owned(), home.to_string_lossy().into_owned()]);
    }
    args
}

/// Why the installed CLI may not run `t3 pair` against the running server.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CliVersionRefusal {
    #[error("The t3 CLI (v{cli}) doesn't match the running T3 Code (v{server}). {PASTE_INSTEAD}")]
    Mismatch { cli: String, server: String },
    #[error("Could not read the t3 CLI's version, so it can't safely pair with the running T3 Code. {PASTE_INSTEAD}")]
    UnknownCli,
    #[error("The running T3 Code did not record its version, so the t3 CLI can't safely pair with it. {PASTE_INSTEAD}")]
    UnknownServer,
}

const PASTE_INSTEAD: &str = "Create a pairing link in T3 Code → Settings → Sharing & access instead.";

/// The version `t3 --version` prints (`t3 v0.1.7-nightly.20261102.4061`), without its `v`.
pub fn parse_cli_version(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let (_name, version, rest) = (words.next()?, words.next()?, words.next());
        let version = version.strip_prefix('v')?;
        (rest.is_none() && version.starts_with(|c: char| c.is_ascii_digit())).then(|| version.to_owned())
    })
}

/// `t3 pair` opens the server's database and runs its migrations (`EnvironmentAuth`, `Sqlite`),
/// so a CLI of any other version could migrate a live database under the running server. Only
/// the exact version recorded in `server-runtime.json` may run it.
pub fn check_cli_version(cli_stdout: &str, server_version: Option<&str>) -> Result<(), CliVersionRefusal> {
    let server = server_version.filter(|version| !version.is_empty()).ok_or(CliVersionRefusal::UnknownServer)?;
    let cli = parse_cli_version(cli_stdout).ok_or(CliVersionRefusal::UnknownCli)?;
    if cli == server { Ok(()) } else { Err(CliVersionRefusal::Mismatch { cli, server: server.to_owned() }) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairOutput {
    pub pairing_url: Option<String>,
    pub token: String,
}

/// The `Token:` line of `t3 pair` output, or the token inside its `Pairing URL:` line.
pub fn parse_pair_output(stdout: &str) -> Option<PairOutput> {
    let field = |name: &str| {
        stdout.lines().find_map(|line| line.trim().strip_prefix(name).map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned))
    };
    let pairing_url = field("Pairing URL:");
    let token = field("Token:").or_else(|| pairing_url.as_deref().and_then(|url| parse_pairing_link(url, None).ok()).map(|link| link.token))?;
    Some(PairOutput { pairing_url, token })
}

/// The line worth showing from a failed `t3 pair`: the message of its logged error
/// (`[time] ERROR (#2): NoRunningServerError: …`), else the first line that is not a Node
/// warning, a stack frame, or a bare `ERROR` heading.
pub fn cli_failure_summary(stderr: &str, stdout: &str) -> String {
    let without_tag = |message: &str| {
        let message = message.trim();
        match message.split_once(": ") {
            Some((tag, rest)) if tag.ends_with("Error") && !tag.contains(' ') => rest.trim().to_owned(),
            _ => message.to_owned(),
        }
    };
    let logged = stdout.lines().chain(stderr.lines()).find_map(|line| line.split_once("] ERROR (")?.1.split_once("): ").map(|(_, message)| without_tag(message)));
    fn meaningful(text: &str) -> Option<&str> {
        text.lines().map(str::trim).find(|line| {
            !line.is_empty()
                && !line.eq_ignore_ascii_case("error")
                && !line.starts_with("at ")
                && !line.starts_with("checked ")
                && !line.starts_with("(node:")
                && !line.starts_with("(Use `node")
        })
    }
    logged
        .or_else(|| meaningful(stderr).or_else(|| meaningful(stdout)).map(without_tag))
        .unwrap_or_else(|| "it exited without a message.".to_owned())
}

/// Persisted exposure preferences for the embedded local `t3 serve` subprocess.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalServerExposure {
    #[serde(default)]
    pub network_access: bool,
    #[serde(default)]
    pub tailscale_serve: bool,
    #[serde(default)]
    pub tailscale_serve_port: Option<u16>,
}

impl LocalServerExposure {
    pub fn bind_host(&self) -> &'static str {
        if self.network_access { LAN_BIND_HOST } else { DEFAULT_LOCAL_SERVER_HOST }
    }

    pub fn effective_tailscale_port(&self) -> Option<u16> {
        self.tailscale_serve
            .then_some(self.tailscale_serve_port.filter(|port| *port > 0).unwrap_or(DEFAULT_TAILSCALE_SERVE_PORT))
    }
}

pub fn parse_tailscale_serve_port(raw: &str) -> Result<u16, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_TAILSCALE_SERVE_PORT);
    }
    let port: u16 = trimmed
        .parse()
        .map_err(|_| "Enter a valid port between 1 and 65535.".to_owned())?;
    if port == 0 {
        return Err("Enter a valid port between 1 and 65535.".to_owned());
    }
    Ok(port)
}

/// Parameters for launching an embedded local `t3 serve` subprocess (`apps/server/src/cli/server.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalServerLaunchSpec {
    pub home: PathBuf,
    pub default_home: PathBuf,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub tailscale_serve: bool,
    pub tailscale_serve_port: Option<u16>,
}

/// Builds `t3 serve` CLI arguments for hosting the local server without opening a browser or
/// auto-bootstrapping the current working directory as a project.
pub fn serve_args(spec: &LocalServerLaunchSpec) -> Vec<String> {
    let mut args = vec!["serve".to_owned(), "--no-browser".to_owned()];
    if spec.home != spec.default_home {
        args.extend(["--base-dir".to_owned(), spec.home.to_string_lossy().into_owned()]);
    }
    if let Some(host) = spec.host.as_deref().map(str::trim).filter(|host| !host.is_empty()) {
        args.extend(["--host".to_owned(), host.to_owned()]);
    }
    if let Some(port) = spec.port.filter(|port| *port > 0) {
        args.extend(["--port".to_owned(), port.to_string()]);
    }
    if spec.tailscale_serve {
        args.push("--tailscale-serve".to_owned());
        if let Some(ts_port) = spec.tailscale_serve_port.filter(|port| *port > 0) {
            args.extend(["--tailscale-serve-port".to_owned(), ts_port.to_string()]);
        }
    }
    args
}

/// Loopback-reachable HTTP origin for a configured bind host and port (`runtimeOriginForConfig`
/// in `apps/server/src/serverRuntimeState.ts`).
pub fn server_origin(host: Option<&str>, port: u16) -> String {
    let trimmed = host.map(str::trim).unwrap_or("");
    let wildcard = trimmed.is_empty() || matches!(trimmed, "0.0.0.0" | "::" | "[::]");
    if wildcard {
        return format!("http://{DEFAULT_LOCAL_SERVER_HOST}:{port}");
    }
    if trimmed.contains(':') && !(trimmed.starts_with('[') && trimmed.ends_with(']')) {
        format!("http://[{trimmed}]:{port}")
    } else {
        format!("http://{trimmed}:{port}")
    }
}

/// Unauthenticated readiness URL for `origin` (`BACKEND_READINESS_PATH`).
pub fn readiness_url(origin: &str) -> String {
    format!("{}{LOCAL_SERVER_READINESS_PATH}", origin.trim_end_matches('/'))
}

/// Appends `chunk` to `existing`, retaining at most `max_chars` Unicode scalar values from the end.
pub fn append_output_tail(existing: &str, chunk: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let combined_chars = existing.chars().count() + chunk.chars().count();
    if combined_chars <= max_chars {
        let mut out = String::with_capacity(existing.len() + chunk.len());
        out.push_str(existing);
        out.push_str(chunk);
        return out;
    }
    let skip = combined_chars - max_chars;
    existing.chars().chain(chunk.chars()).skip(skip).collect()
}

/// Exponential backoff for restarting an unexpectedly exited local server (`calculateRestartDelay`).
pub fn restart_delay_ms(attempt: u32) -> u64 {
    let shift = attempt.min(16);
    let factor = 1u64 << shift;
    INITIAL_RESTART_DELAY_MS.saturating_mul(factor).min(MAX_RESTART_DELAY_MS)
}

/// Whether a discovered `ServerRuntimeState` matches the port requested for a hosted local server.
pub fn matches_hosted_runtime(state: &ServerRuntimeState, expected_port: Option<u16>) -> bool {
    expected_port.is_none_or(|port| state.port == port)
}

/// Structured reason why the hosted local server stopped or refused to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalServerFailureKind {
    CliMissing,
    SpawnFailed,
    ManagedPolicy,
    StateDirOwned,
    IncompatibleProtocol,
    ReadinessTimeout,
    Crashed { exit_code: Option<i32> },
}

/// Diagnostic payload stored when the hosted local server enters `LocalServerPhase::Failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalServerFailure {
    pub kind: LocalServerFailureKind,
    pub message: String,
    pub recovery: StartupRecoveryOffer,
    pub output_tail: String,
    pub restart_suppressed: bool,
}

/// Observable lifecycle state of the embedded local server host.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LocalServerPhase {
    #[default]
    Stopped,
    Starting { pid: Option<u32>, attempt: u32 },
    Running { pid: u32, origin: String, server_version: Option<String> },
    Stopping { pid: Option<u32> },
    Failed(LocalServerFailure),
}

impl LocalServerPhase {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting { .. } | Self::Running { .. })
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    pub fn is_starting(&self) -> bool {
        matches!(self, Self::Starting { .. })
    }

    pub fn is_stopping(&self) -> bool {
        matches!(self, Self::Stopping { .. })
    }

    pub fn pid(&self) -> Option<u32> {
        match self {
            Self::Starting { pid, .. } | Self::Stopping { pid } => *pid,
            Self::Running { pid, .. } => Some(*pid),
            Self::Stopped | Self::Failed(_) => None,
        }
    }

    pub fn failure(&self) -> Option<&LocalServerFailure> {
        match self {
            Self::Failed(failure) => Some(failure),
            _ => None,
        }
    }
}

/// Inputs driving the pure `LocalServerLifecycle` state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalServerEvent {
    StartRequested,
    StopRequested,
    CliMissing,
    Spawned { pid: u32 },
    SpawnFailed { reason: String },
    OutputChunk(String),
    Ready { pid: u32, origin: String, server_version: Option<String> },
    IncompatibleProtocol { message: String },
    ReadinessTimedOut { origin: Option<String> },
    ProcessExited { exit_code: Option<i32>, stderr: String, stdout: String },
}

/// Side effect the host runtime should perform after applying a `LocalServerEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalServerAction {
    None,
    Spawn,
    Terminate { pid: Option<u32> },
    ScheduleRestart { delay_ms: u64, attempt: u32 },
}

/// Pure lifecycle state machine mirroring `DesktopBackendManager.ts` (startup, readiness,
/// managed-policy exit 77 and state-dir-owned exit 78 restart suppression, bounded crash backoff,
/// and clean shutdown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalServerLifecycle {
    phase: LocalServerPhase,
    desired_running: bool,
    restart_attempt: u32,
    max_restarts: u32,
    last_started_version: Option<String>,
    output_tail: String,
}

impl Default for LocalServerLifecycle {
    fn default() -> Self {
        Self::with_max_restarts(MAX_STARTUP_RESTART_ATTEMPTS)
    }
}

impl LocalServerLifecycle {
    pub fn with_max_restarts(max_restarts: u32) -> Self {
        Self {
            phase: LocalServerPhase::Stopped,
            desired_running: false,
            restart_attempt: 0,
            max_restarts,
            last_started_version: None,
            output_tail: String::new(),
        }
    }

    pub fn phase(&self) -> &LocalServerPhase {
        &self.phase
    }

    pub fn desired_running(&self) -> bool {
        self.desired_running
    }

    pub fn restart_attempt(&self) -> u32 {
        self.restart_attempt
    }

    pub fn output_tail(&self) -> &str {
        &self.output_tail
    }

    pub fn apply(&mut self, event: LocalServerEvent) -> LocalServerAction {
        match event {
            LocalServerEvent::StartRequested => {
                if matches!(self.phase, LocalServerPhase::Starting { .. } | LocalServerPhase::Running { .. }) {
                    self.desired_running = true;
                    return LocalServerAction::None;
                }
                self.desired_running = true;
                self.restart_attempt = 0;
                self.output_tail.clear();
                self.phase = LocalServerPhase::Starting { pid: None, attempt: 1 };
                LocalServerAction::Spawn
            }
            LocalServerEvent::StopRequested => {
                self.desired_running = false;
                match self.phase.pid() {
                    pid if self.phase.is_active() => {
                        self.phase = LocalServerPhase::Stopping { pid };
                        LocalServerAction::Terminate { pid }
                    }
                    _ => {
                        self.phase = LocalServerPhase::Stopped;
                        LocalServerAction::None
                    }
                }
            }
            LocalServerEvent::CliMissing => {
                self.desired_running = false;
                let recovery = resolve_startup_recovery_offer(None, self.last_started_version.as_deref(), false);
                self.phase = LocalServerPhase::Failed(LocalServerFailure {
                    kind: LocalServerFailureKind::CliMissing,
                    message: "Could not find the t3 command. Install the t3 CLI or open T3 Code Desktop once to install it.".to_owned(),
                    recovery,
                    output_tail: self.output_tail.clone(),
                    restart_suppressed: true,
                });
                LocalServerAction::None
            }
            LocalServerEvent::Spawned { pid } => {
                let attempt = match self.phase {
                    LocalServerPhase::Starting { attempt, .. } => attempt,
                    _ => self.restart_attempt.saturating_add(1),
                };
                self.phase = LocalServerPhase::Starting { pid: Some(pid), attempt };
                LocalServerAction::None
            }
            LocalServerEvent::SpawnFailed { reason } => {
                self.desired_running = false;
                let recovery = resolve_startup_recovery_offer(None, self.last_started_version.as_deref(), false);
                self.phase = LocalServerPhase::Failed(LocalServerFailure {
                    kind: LocalServerFailureKind::SpawnFailed,
                    message: format!("Could not start the local T3 Code server: {reason}"),
                    recovery,
                    output_tail: self.output_tail.clone(),
                    restart_suppressed: true,
                });
                LocalServerAction::None
            }
            LocalServerEvent::OutputChunk(chunk) => {
                self.output_tail = append_output_tail(&self.output_tail, &chunk, OUTPUT_TAIL_MAX_CHARS);
                LocalServerAction::None
            }
            LocalServerEvent::Ready { pid, origin, server_version } => {
                self.restart_attempt = 0;
                if let Some(version) = server_version.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                    self.last_started_version = Some(version.to_owned());
                }
                self.phase = LocalServerPhase::Running { pid, origin, server_version };
                LocalServerAction::None
            }
            LocalServerEvent::IncompatibleProtocol { message } => {
                let pid = self.phase.pid();
                self.desired_running = false;
                let recovery = resolve_startup_recovery_offer(None, self.last_started_version.as_deref(), false);
                self.phase = LocalServerPhase::Failed(LocalServerFailure {
                    kind: LocalServerFailureKind::IncompatibleProtocol,
                    message,
                    recovery,
                    output_tail: self.output_tail.clone(),
                    restart_suppressed: true,
                });
                if pid.is_some() {
                    LocalServerAction::Terminate { pid }
                } else {
                    LocalServerAction::None
                }
            }
            LocalServerEvent::ReadinessTimedOut { origin } => {
                let pid = self.phase.pid();
                self.desired_running = false;
                let recovery = resolve_startup_recovery_offer(None, self.last_started_version.as_deref(), false);
                let message = match origin {
                    Some(origin) => format!("The local T3 Code server started but did not become ready in time at {origin}."),
                    None => "The local T3 Code server started but did not become ready in time.".to_owned(),
                };
                self.phase = LocalServerPhase::Failed(LocalServerFailure {
                    kind: LocalServerFailureKind::ReadinessTimeout,
                    message,
                    recovery,
                    output_tail: self.output_tail.clone(),
                    restart_suppressed: true,
                });
                LocalServerAction::Terminate { pid }
            }
            LocalServerEvent::ProcessExited { exit_code, stderr, stdout } => {
                if !self.desired_running || matches!(self.phase, LocalServerPhase::Stopping { .. }) {
                    self.phase = LocalServerPhase::Stopped;
                    return LocalServerAction::None;
                }
                match classify_server_exit(exit_code, &stderr, &stdout) {
                    StartupExitKind::ManagedPolicy { detail } => {
                        self.desired_running = false;
                        let recovery = resolve_startup_recovery_offer(None, self.last_started_version.as_deref(), true);
                        self.phase = LocalServerPhase::Failed(LocalServerFailure {
                            kind: LocalServerFailureKind::ManagedPolicy,
                            message: detail,
                            recovery,
                            output_tail: self.output_tail.clone(),
                            restart_suppressed: true,
                        });
                        LocalServerAction::None
                    }
                    StartupExitKind::StateDirOwned => {
                        self.desired_running = false;
                        let recovery = resolve_startup_recovery_offer(None, self.last_started_version.as_deref(), false);
                        self.phase = LocalServerPhase::Failed(LocalServerFailure {
                            kind: LocalServerFailureKind::StateDirOwned,
                            message: "Another T3 Code process already owns this state directory.".to_owned(),
                            recovery,
                            output_tail: self.output_tail.clone(),
                            restart_suppressed: true,
                        });
                        LocalServerAction::None
                    }
                    StartupExitKind::Other { exit_code, detail } => {
                        if !should_suppress_restart(exit_code) && self.restart_attempt < self.max_restarts {
                            let delay_ms = restart_delay_ms(self.restart_attempt);
                            self.restart_attempt += 1;
                            self.phase = LocalServerPhase::Starting { pid: None, attempt: self.restart_attempt + 1 };
                            return LocalServerAction::ScheduleRestart { delay_ms, attempt: self.restart_attempt };
                        }
                        self.desired_running = false;
                        let recovery = resolve_startup_recovery_offer(None, self.last_started_version.as_deref(), false);
                        let code_suffix = exit_code.map_or_else(String::new, |code| format!(" (exit {code})"));
                        self.phase = LocalServerPhase::Failed(LocalServerFailure {
                            kind: LocalServerFailureKind::Crashed { exit_code },
                            message: format!("The local T3 Code server stopped unexpectedly{code_suffix}: {detail}"),
                            recovery,
                            output_tail: self.output_tail.clone(),
                            restart_suppressed: should_suppress_restart(exit_code),
                        });
                        LocalServerAction::None
                    }
                }
            }
        }
    }
}

/// Formats a copyable diagnostics bundle for a local server startup/runtime failure.
pub fn format_local_server_diagnostics(failure: &LocalServerFailure, home: Option<&Path>, cli: Option<&Path>) -> String {
    let mut lines = vec![
        format!("Error: {}", failure.message),
        format!("Recovery: {}", describe_startup_recovery_offer(&failure.recovery)),
    ];
    if failure.restart_suppressed {
        lines.push("Automatic restart: suppressed".to_owned());
    }
    if let Some(home) = home {
        lines.push(format!("T3 home: {}", home.display()));
    }
    if let Some(cli) = cli {
        lines.push(format!("t3 CLI: {}", cli.display()));
    }
    let tail = failure.output_tail.trim();
    if !tail.is_empty() {
        lines.push(String::new());
        lines.push("Server output:".to_owned());
        lines.push(tail.to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_state_reads_the_live_format_and_rejects_anything_else() {
        let live = r#"{"version":1,"pid":50344,"host":"0.0.0.0","port":3773,"origin":"http://127.0.0.1:3773","startedAt":"2026-11-02T05:05:56.869Z","serverVersion":"0.1.7-nightly.20261102.4061","managedPolicy":null,"ownerId":"x"}"#;
        let state = parse_runtime_state(live).expect("decodes");
        assert_eq!((state.pid, state.port, state.origin.as_str()), (50344, 3773, "http://127.0.0.1:3773"));
        assert_eq!(state.server_version.as_deref(), Some("0.1.7-nightly.20261102.4061"));
        let dev = r#"{"version":1,"pid":7,"port":14658,"origin":"http://127.0.0.1:14658/","devUrl":"http://localhost:6618/","startedAt":"x"}"#;
        assert_eq!(parse_runtime_state(dev).map(|state| state.origin), Some("http://127.0.0.1:14658".to_owned()));
        assert_eq!(parse_runtime_state(&live.replace("\"version\":1", "\"version\":2")), None);
        assert_eq!(parse_runtime_state(&live.replace("http://127.0.0.1:3773", "file:///tmp")), None);
        assert_eq!(parse_runtime_state("{\"version\":1"), None);
    }

    #[test]
    fn home_follows_t3code_home_and_paths_follow_the_cli_order() {
        let (user, cwd) = (Path::new("/Users/me"), Path::new("/Volumes/work"));
        assert_eq!(t3_home(None, user, cwd), Path::new("/Users/me/.t3"));
        assert_eq!(t3_home(Some("  "), user, cwd), Path::new("/Users/me/.t3"));
        assert_eq!(t3_home(Some("~/alt"), user, cwd), Path::new("/Users/me/alt"));
        assert_eq!(t3_home(Some("/srv/t3"), user, cwd), Path::new("/srv/t3"));
        // The CLI runs from the user's home; a relative home still means the launch directory.
        assert_eq!(t3_home(Some(".t3-smoke"), user, cwd), Path::new("/Volumes/work/.t3-smoke"));
        let [release, dev] = runtime_state_paths(Path::new("/h"));
        assert_eq!((release.as_path(), dev.as_path()), (Path::new("/h/userdata/server-runtime.json"), Path::new("/h/dev/server-runtime.json")));
    }

    #[test]
    fn cli_lookup_prefers_the_override_then_the_installed_launcher() {
        let (home, user) = (Path::new("/Users/me/.t3"), Path::new("/Users/me"));
        assert_eq!(cli_candidates(Some("/tmp/fake-t3"), home, user, Some("/usr/bin")), vec![PathBuf::from("/tmp/fake-t3")]);
        let found = cli_candidates(None, home, user, Some("/opt/homebrew/bin::/usr/bin"));
        let found: Vec<_> = found.iter().map(|path| path.to_string_lossy().into_owned()).collect();
        assert_eq!(found, ["/Users/me/.t3/bin/t3", "/opt/homebrew/bin/t3", "/usr/bin/t3", "/usr/local/bin/t3"]);
    }

    #[test]
    fn pair_arguments_name_the_home_only_when_it_is_not_the_default() {
        let default = Path::new("/Users/me/.t3");
        assert_eq!(pair_args(default, default), ["pair", "--label", "T3 Code Native", "--ttl", "2m"]);
        assert_eq!(pair_args(Path::new("/tmp/home"), default)[5..], ["--base-dir", "/tmp/home"]);
    }

    #[test]
    fn only_a_cli_of_the_running_servers_exact_version_may_pair() {
        let live = "0.1.7-nightly.20261102.4061";
        assert_eq!(parse_cli_version("t3 v0.1.7-nightly.20261102.4061\n").as_deref(), Some(live));
        assert_eq!(parse_cli_version("(node:1) ExperimentalWarning: x\nt3 v0.1.6\n").as_deref(), Some("0.1.6"));
        assert_eq!(parse_cli_version(""), None);
        assert_eq!(parse_cli_version("t3 version unknown\n"), None);
        assert_eq!(parse_cli_version("t3 vnext\n"), None);

        assert_eq!(check_cli_version("t3 v0.1.7-nightly.20261102.4061\n", Some(live)), Ok(()));
        let newer = check_cli_version("t3 v0.1.7-nightly.20261103.4100\n", Some(live));
        assert_eq!(newer, Err(CliVersionRefusal::Mismatch { cli: "0.1.7-nightly.20261103.4100".into(), server: live.into() }));
        assert_eq!(
            newer.unwrap_err().to_string(),
            "The t3 CLI (v0.1.7-nightly.20261103.4100) doesn't match the running T3 Code (v0.1.7-nightly.20261102.4061). Create a pairing link in T3 Code → Settings → Sharing & access instead."
        );
        // Exactly equal: a prefix, a build suffix, or an older CLI never passes.
        assert!(check_cli_version("t3 v0.1.7\n", Some(live)).is_err());
        assert!(check_cli_version("t3 v0.1.7-nightly.20261102.40610\n", Some(live)).is_err());
        assert!(check_cli_version("t3 v0.1.6\n", Some("0.1.7")).is_err());
        assert_eq!(check_cli_version("garbage", Some(live)), Err(CliVersionRefusal::UnknownCli));
        assert_eq!(check_cli_version("t3 v0.1.7\n", None), Err(CliVersionRefusal::UnknownServer));
        assert_eq!(check_cli_version("t3 v0.1.7\n", Some("")), Err(CliVersionRefusal::UnknownServer));
    }

    #[test]
    fn pair_output_yields_the_token_line_or_the_link_token() {
        let output = "Pairing with MacBook Pro (http://127.0.0.1:3773).\n\n█▀▀▀▀▀█ ▄ █\n\nPairing URL: http://192.168.1.4:3773/pair#token=ABCD2345\nApp link: t3code://pair?u=x\nToken: ABCD2345\nExpires: 2026-11-02T09:02:00.000Z\n\nNote: This URL is only reachable from this machine.\n";
        let parsed = parse_pair_output(output).expect("parses");
        assert_eq!(parsed.token, "ABCD2345");
        assert_eq!(parsed.pairing_url.as_deref(), Some("http://192.168.1.4:3773/pair#token=ABCD2345"));
        assert_eq!(parse_pair_output("Pairing URL: http://h:1/pair#token=XYZ\n").map(|p| p.token), Some("XYZ".to_owned()));
        assert_eq!(parse_pair_output("No running T3 Code server found.\n"), None);
    }

    #[test]
    fn a_failed_pair_reports_its_message_line() {
        let stderr = "No running T3 Code server found.\n  checked /h/userdata/server-runtime.json\nStart one with `npx t3 serve`.";
        assert_eq!(cli_failure_summary(stderr, ""), "No running T3 Code server found.");
        // What the CLI prints: the error is a log line on stdout, Node warnings on stderr.
        let warnings = "(node:30589) ExperimentalWarning: SQLite is an experimental feature\n(Use `node --trace-warnings ...` to show where)\n";
        let logged = "[05:07:29.081] ERROR (#2): NoRunningServerError: No running T3 Code server found.\n  checked /h/userdata/server-runtime.json\n    at file:///pair.ts:295:17\n";
        assert_eq!(cli_failure_summary(warnings, logged), "No running T3 Code server found.");
        assert_eq!(cli_failure_summary("\nERROR\n  Unexpected positional argument: \"pair\"\n", ""), "Unexpected positional argument: \"pair\"");
        assert_eq!(cli_failure_summary("\n", "Error: boom\n    at x (y.js:1)"), "boom");
        assert_eq!(cli_failure_summary("", ""), "it exited without a message.");
    }

    #[test]
    fn exit_77_managed_policy_refusal_suppresses_restart_and_never_points_at_another_release() {
        assert!(should_suppress_restart(Some(SERVER_EXIT_CODE_MANAGED_POLICY)));
        assert!(should_suppress_restart(Some(SERVER_EXIT_CODE_STATE_DIR_OWNED)));
        assert!(!should_suppress_restart(Some(1)));
        assert!(!should_suppress_restart(None));

        let kind = classify_server_exit(
            Some(SERVER_EXIT_CODE_MANAGED_POLICY),
            "ManagedPolicyFileError: T3 Code cannot use the managed policy at /etc/t3/policy.json: invalid JSON.",
            "",
        );
        match kind {
            StartupExitKind::ManagedPolicy { detail } => {
                assert!(detail.contains(MANAGED_POLICY_STARTUP_MESSAGE));
                assert!(detail.contains("invalid JSON"));
            }
            other => panic!("expected ManagedPolicy, got {other:?}"),
        }
        assert_eq!(classify_server_exit(Some(SERVER_EXIT_CODE_STATE_DIR_OWNED), "", ""), StartupExitKind::StateDirOwned);

        // Managed policy refusal never offers reinstall or releases page, unless a newer update exists.
        assert_eq!(
            resolve_startup_recovery_offer(None, Some("0.1.7"), true),
            StartupRecoveryOffer::ManagedPolicy
        );
        assert_eq!(
            resolve_startup_recovery_offer(Some("0.1.8"), Some("0.1.7"), true),
            StartupRecoveryOffer::Update { version: "0.1.8".into() }
        );
        assert_eq!(
            resolve_startup_recovery_offer(None, Some("0.1.7"), false),
            StartupRecoveryOffer::Reinstall { version: "0.1.7".into() }
        );
        assert_eq!(
            describe_startup_recovery_offer(&StartupRecoveryOffer::ManagedPolicy),
            MANAGED_POLICY_STARTUP_MESSAGE
        );
    }

    #[test]
    fn serve_args_and_origin_match_server_cli_conventions() {
        let default = PathBuf::from("/Users/me/.t3");
        let args = serve_args(&LocalServerLaunchSpec {
            home: default.clone(),
            default_home: default.clone(),
            host: Some("127.0.0.1".into()),
            port: None,
            tailscale_serve: false,
            tailscale_serve_port: None,
        });
        assert_eq!(args, ["serve", "--no-browser", "--host", "127.0.0.1"]);

        let custom = serve_args(&LocalServerLaunchSpec {
            home: PathBuf::from("/tmp/t3-custom"),
            default_home: default.clone(),
            host: Some("  ".into()),
            port: Some(4123),
            tailscale_serve: true,
            tailscale_serve_port: Some(8443),
        });
        assert_eq!(
            custom,
            [
                "serve",
                "--no-browser",
                "--base-dir",
                "/tmp/t3-custom",
                "--port",
                "4123",
                "--tailscale-serve",
                "--tailscale-serve-port",
                "8443",
            ]
        );
        assert_eq!(parse_tailscale_serve_port(""), Ok(DEFAULT_TAILSCALE_SERVE_PORT));
        assert_eq!(parse_tailscale_serve_port("8443"), Ok(8443));
        assert!(parse_tailscale_serve_port("0").is_err());
        assert!(parse_tailscale_serve_port("99999").is_err());

        assert_eq!(server_origin(None, 3773), "http://127.0.0.1:3773");
        assert_eq!(server_origin(Some("0.0.0.0"), 3773), "http://127.0.0.1:3773");
        assert_eq!(server_origin(Some("::"), 3773), "http://127.0.0.1:3773");
        assert_eq!(server_origin(Some("::1"), 3773), "http://[::1]:3773");
        assert_eq!(server_origin(Some("[::1]"), 3773), "http://[::1]:3773");
        assert_eq!(readiness_url("http://127.0.0.1:3773/"), "http://127.0.0.1:3773/.well-known/t3/environment");
    }

    #[test]
    fn output_tail_truncates_on_unicode_boundaries_and_backoff_caps() {
        assert_eq!(append_output_tail("abc", "def", 4), "cdef");
        assert_eq!(append_output_tail("héllo", "🚀world", 6), "🚀world");
        assert_eq!(append_output_tail("abc", "def", 0), "");

        assert_eq!(restart_delay_ms(0), 500);
        assert_eq!(restart_delay_ms(1), 1_000);
        assert_eq!(restart_delay_ms(2), 2_000);
        assert_eq!(restart_delay_ms(3), 4_000);
        assert_eq!(restart_delay_ms(4), 8_000);
        assert_eq!(restart_delay_ms(5), 10_000);
        assert_eq!(restart_delay_ms(30), 10_000);
    }

    #[test]
    fn lifecycle_transitions_start_ready_stop_and_suppresses_policy_restarts() {
        let mut lc = LocalServerLifecycle::default();
        assert_eq!(lc.phase(), &LocalServerPhase::Stopped);
        assert_eq!(lc.apply(LocalServerEvent::StartRequested), LocalServerAction::Spawn);
        assert_eq!(lc.phase(), &LocalServerPhase::Starting { pid: None, attempt: 1 });
        // Idempotent while starting.
        assert_eq!(lc.apply(LocalServerEvent::StartRequested), LocalServerAction::None);

        assert_eq!(lc.apply(LocalServerEvent::Spawned { pid: 4242 }), LocalServerAction::None);
        assert_eq!(lc.phase().pid(), Some(4242));

        assert_eq!(
            lc.apply(LocalServerEvent::Ready {
                pid: 4242,
                origin: "http://127.0.0.1:3773".into(),
                server_version: Some("0.1.9".into()),
            }),
            LocalServerAction::None
        );
        assert!(lc.phase().is_running());

        assert_eq!(lc.apply(LocalServerEvent::StopRequested), LocalServerAction::Terminate { pid: Some(4242) });
        assert!(lc.phase().is_stopping());
        // Exiting during intentional stop transitions to Stopped even with a signal/non-zero code.
        assert_eq!(
            lc.apply(LocalServerEvent::ProcessExited { exit_code: None, stderr: String::new(), stdout: String::new() }),
            LocalServerAction::None
        );
        assert_eq!(lc.phase(), &LocalServerPhase::Stopped);

        // Managed-policy exit 77 suppresses restart and never offers reinstall/releases.
        assert_eq!(lc.apply(LocalServerEvent::StartRequested), LocalServerAction::Spawn);
        lc.apply(LocalServerEvent::OutputChunk("ManagedPolicyFileError: invalid policy\n".into()));
        let action = lc.apply(LocalServerEvent::ProcessExited {
            exit_code: Some(SERVER_EXIT_CODE_MANAGED_POLICY),
            stderr: "ManagedPolicyFileError: invalid policy".into(),
            stdout: String::new(),
        });
        assert_eq!(action, LocalServerAction::None);
        assert!(!lc.desired_running());
        let failure = lc.phase().failure().expect("failed state");
        assert_eq!(failure.kind, LocalServerFailureKind::ManagedPolicy);
        assert!(failure.restart_suppressed);
        assert_eq!(failure.recovery, StartupRecoveryOffer::ManagedPolicy);
        assert!(failure.message.contains(MANAGED_POLICY_STARTUP_MESSAGE));
        let diag = format_local_server_diagnostics(failure, Some(Path::new("/tmp/h")), Some(Path::new("/tmp/t3")));
        assert!(diag.contains("Automatic restart: suppressed"));
        assert!(diag.contains("Server output:"));

        // State-dir-owned exit 78 also suppresses restart.
        assert_eq!(lc.apply(LocalServerEvent::StartRequested), LocalServerAction::Spawn);
        let owned_action = lc.apply(LocalServerEvent::ProcessExited {
            exit_code: Some(SERVER_EXIT_CODE_STATE_DIR_OWNED),
            stderr: String::new(),
            stdout: String::new(),
        });
        assert_eq!(owned_action, LocalServerAction::None);
        assert_eq!(lc.phase().failure().map(|f| &f.kind), Some(&LocalServerFailureKind::StateDirOwned));
    }

    #[test]
    fn lifecycle_exhausts_bounded_crash_restarts_then_fails() {
        let mut lc = LocalServerLifecycle::with_max_restarts(2);
        assert_eq!(lc.apply(LocalServerEvent::StartRequested), LocalServerAction::Spawn);
        assert_eq!(
            lc.apply(LocalServerEvent::ProcessExited { exit_code: Some(1), stderr: "boom 1".into(), stdout: String::new() }),
            LocalServerAction::ScheduleRestart { delay_ms: 500, attempt: 1 }
        );
        assert_eq!(
            lc.apply(LocalServerEvent::ProcessExited { exit_code: Some(1), stderr: "boom 2".into(), stdout: String::new() }),
            LocalServerAction::ScheduleRestart { delay_ms: 1_000, attempt: 2 }
        );
        assert_eq!(
            lc.apply(LocalServerEvent::ProcessExited { exit_code: Some(1), stderr: "boom 3".into(), stdout: String::new() }),
            LocalServerAction::None
        );
        let failure = lc.phase().failure().expect("exhausted restarts");
        assert_eq!(failure.kind, LocalServerFailureKind::Crashed { exit_code: Some(1) });
        assert!(failure.message.contains("boom 3"));
    }
}
