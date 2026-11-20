//! One-click pairing with T3 Code on this Mac: find the running server the way `t3 pair` does
//! (`t3_core::local_server`, `discoverPairTarget`), check it speaks our protocol and that the
//! installed CLI is its exact version, mint a token with `t3 pair` (the supported way to pair a
//! device), and redeem it at the server's own origin.
//!
//! `T3CODE_HOME` moves the T3 home, as it does for the CLI. `T3_NATIVE_T3_BIN` replaces the CLI
//! (tests point it at a wrapper around a dev server's `bin.ts`).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use t3_core::compatibility::{ProtocolMismatch, protocol_mismatch};
use t3_core::local_server::{
    CliVersionRefusal, DEFAULT_LOCAL_SERVER_HOST, LocalServerEvent, LocalServerFailure, LocalServerFailureKind, LocalServerLaunchSpec,
    LocalServerLifecycle, LocalServerPhase, SERVER_EXIT_CODE_MANAGED_POLICY, ServerRuntimeState, StartupExitKind, TERMINATE_GRACE_MS,
    check_cli_version, classify_server_exit, cli_candidates, cli_failure_summary, managed_policy_diagnostic, matches_hosted_runtime, pair_args,
    parse_pair_output, parse_runtime_state, runtime_state_paths, serve_args, server_origin, t3_home,
};
use t3_core::pairing::PairingLink;
use t3_core::server_config::EnvironmentDescriptor;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Notify, oneshot, watch};

use crate::auth::{self, AuthError, Paired};

/// Booting the CLI and opening the server's database takes a few seconds; a hung one gives up.
const PAIR_TIMEOUT: Duration = Duration::from_secs(45);
/// `t3 --version` answers before loading the server.
pub(crate) const VERSION_TIMEOUT: Duration = Duration::from_secs(15);
/// Default budget for a hosted local server to bind and answer its descriptor endpoint.
const DEFAULT_HOST_READINESS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub struct LocalServer {
    pub home: PathBuf,
    pub state: ServerRuntimeState,
    pub descriptor: EnvironmentDescriptor,
}

impl LocalServer {
    pub fn server_version(&self) -> Option<&str> {
        Some(self.descriptor.server_version.as_str()).filter(|v| !v.is_empty()).or(self.state.server_version.as_deref())
    }

    pub fn label(&self) -> &str {
        Some(self.descriptor.label.trim()).filter(|label| !label.is_empty()).unwrap_or("this Mac")
    }

    pub fn environment_id(&self) -> Option<&str> {
        Some(self.descriptor.environment_id.as_str()).filter(|id| !id.is_empty())
    }

    pub fn mismatch(&self) -> Option<ProtocolMismatch> {
        protocol_mismatch(&self.descriptor)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LocalPairError {
    #[error("T3 Code is no longer running on this Mac. Open it and try again.")]
    NotRunning,
    #[error("{0}")]
    Incompatible(String),
    /// Managed policy exit 77 (`pingdotgg/t3code#30319`): never misreport as a version mismatch
    /// or point at another release.
    #[error("{0}")]
    ManagedPolicy(String),
    #[error("Could not find the t3 command. Open the T3 Code desktop app once to install it, or paste a pairing link below.")]
    CliMissing,
    #[error(transparent)]
    Version(#[from] CliVersionRefusal),
    #[error("t3 pair failed: {0}")]
    CliFailed(String),
    #[error("t3 pair did not print a pairing token.")]
    NoToken,
    #[error(transparent)]
    Auth(#[from] AuthError),
}

fn user_home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// The T3 home the CLI would use: `T3CODE_HOME`, else `~/.t3`. Resolved once, against the
/// directory the app was launched in, so discovery and `--base-dir` always name the same place.
pub fn home() -> Option<PathBuf> {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    HOME.get_or_init(|| {
        let user = user_home()?;
        let cwd = std::env::current_dir().unwrap_or_else(|_| user.clone());
        Some(t3_home(std::env::var("T3CODE_HOME").ok().as_deref(), &user, &cwd))
    })
    .clone()
}

/// Every live server recorded under `home`, in `t3 pair`'s order. A dead server's state file is
/// skipped: its port may belong to something else now.
fn live_states(home: &Path) -> Vec<ServerRuntimeState> {
    runtime_state_paths(home)
        .iter()
        .filter_map(|path| parse_runtime_state(&std::fs::read_to_string(path).ok()?))
        .filter(|state| process_alive(state.pid))
        .collect()
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: signal 0 sends nothing; it only checks that the process exists.
    let result = unsafe { libc::kill(pid, 0) };
    // EPERM: alive, but owned by another user.
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// The first live server under `home` that answers with its descriptor. As `discoverPairTarget`
/// probes, a reused PID or a server that stopped answering falls through to the next candidate.
pub async fn discover_in(home: &Path) -> Option<LocalServer> {
    for state in live_states(home) {
        match auth::fetch_descriptor(&state.origin).await {
            Ok(descriptor) => return Some(LocalServer { home: home.to_path_buf(), state, descriptor }),
            Err(error) => tracing::info!(origin = %state.origin, %error, "skipping a recorded T3 Code that does not answer"),
        }
    }
    None
}

/// T3 Code running on this machine.
pub async fn discover() -> Option<LocalServer> {
    discover_in(&home()?).await
}

/// The first `t3` that exists. With `T3_NATIVE_T3_BIN` set, only that one.
pub fn find_cli(home: &Path) -> Option<PathBuf> {
    let user_home = user_home()?;
    let override_path = std::env::var("T3_NATIVE_T3_BIN").ok();
    let path_var = std::env::var("PATH").ok();
    cli_candidates(override_path.as_deref(), home, &user_home, path_var.as_deref()).into_iter().find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    metadata.is_file()
}

/// Pairs with T3 Code on this Mac: finds the server again, refuses one on another protocol or a
/// CLI of another version, runs `t3 pair`, and redeems the token at the server's origin, which
/// is always reachable from here. Dropping the future stops the CLI.
pub async fn pair(server: &LocalServer) -> Result<Paired, LocalPairError> {
    pair_with_cli(server, None).await
}

/// Like `pair`, optionally pinning the `t3` CLI binary instead of discovering it from `PATH`.
pub async fn pair_with_cli(server: &LocalServer, cli_override: Option<&Path>) -> Result<Paired, LocalPairError> {
    let current = discover_in(&server.home).await.ok_or(LocalPairError::NotRunning)?;
    if let Some(mismatch) = current.mismatch() {
        return Err(LocalPairError::Incompatible(mismatch.message(&current.descriptor.label)));
    }
    let cli = match cli_override {
        Some(path) if is_executable(path) => path.to_path_buf(),
        Some(_) => return Err(LocalPairError::CliMissing),
        None => find_cli(&current.home).ok_or(LocalPairError::CliMissing)?,
    };
    check_version(&cli, &current).await?;
    let token = run_pair(&cli, &current.home).await?;
    let credentials = auth::redeem_pairing(&PairingLink { http_base: current.state.origin.clone(), token }).await?;
    Ok(Paired { credentials, descriptor: Some(current.descriptor) })
}

/// `t3 pair` migrates the database it opens, so it only runs when `t3 --version` is exactly the
/// version the running server recorded. Managed-policy exit 77 is reported directly without
/// blaming the release version (`pingdotgg/t3code#30319`).
async fn check_version(cli: &Path, server: &LocalServer) -> Result<(), LocalPairError> {
    let output = run_cli(cli, &["--version".to_owned()], &server.home, VERSION_TIMEOUT).await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(SERVER_EXIT_CODE_MANAGED_POLICY) {
        return Err(LocalPairError::ManagedPolicy(managed_policy_diagnostic(&stderr, &stdout)));
    }
    check_cli_version(&stdout, server.state.server_version.as_deref()).map_err(|refusal| {
        tracing::warn!(cli = %cli.display(), %refusal, "not running t3 pair");
        LocalPairError::from(refusal)
    })
}

async fn run_pair(cli: &Path, home: &Path) -> Result<String, LocalPairError> {
    let default_home = user_home().map(|user| user.join(".t3")).unwrap_or_default();
    let output = run_cli(cli, &pair_args(home, &default_home), home, PAIR_TIMEOUT).await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return match classify_server_exit(output.status.code(), &stderr, &stdout) {
            StartupExitKind::ManagedPolicy { detail } => Err(LocalPairError::ManagedPolicy(detail)),
            StartupExitKind::StateDirOwned => {
                Err(LocalPairError::CliFailed("Another T3 Code process already owns this state directory.".to_owned()))
            }
            StartupExitKind::Other { .. } => Err(LocalPairError::CliFailed(cli_failure_summary(&stderr, &stdout))),
        };
    }
    parse_pair_output(&stdout).map(|output| output.token).ok_or(LocalPairError::NoToken)
}

/// Detects the local `t3` CLI installation & PATH status (`pingdotgg/t3code#33150`, `pingdotgg/t3code#33153`).
pub fn cli_command_status() -> Option<t3_core::onboarding::CliCommandStatus> {
    let home = home()?;
    let bin_dir = home.join("bin");
    let installed = is_executable(&bin_dir.join("t3"));
    let path_var = std::env::var("PATH").unwrap_or_default();
    let entries: Vec<(&str, bool)> = path_var
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| (dir, is_executable(&Path::new(dir).join("t3"))))
        .collect();
    let user_home = user_home().map(|p| p.to_string_lossy().into_owned());
    let shell = std::env::var("SHELL").ok();
    let config_home = std::env::var("XDG_CONFIG_HOME").ok();
    let zdotdir = std::env::var("ZDOTDIR").ok();
    Some(t3_core::onboarding::detect_cli_command_status(
        &bin_dir,
        installed,
        &entries,
        &t3_core::onboarding::ShellEnv {
            platform: std::env::consts::OS,
            home: user_home.as_deref(),
            shell: shell.as_deref(),
            config_home: config_home.as_deref(),
            zdotdir: zdotdir.as_deref(),
        },
    ))
}

/// Runs the CLI from the user's home (from a linked worktree it would pair with that worktree's
/// server) with the absolute T3 home, in a process group of its own. A timeout, or dropping the
/// future, kills the whole group, so nothing the CLI started outlives it.
pub(crate) async fn run_cli(cli: &Path, args: &[String], home: &Path, limit: Duration) -> Result<std::process::Output, LocalPairError> {
    let mut command = tokio::process::Command::new(cli);
    command
        .args(args)
        .env("T3CODE_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    if let Some(user) = user_home() {
        command.current_dir(user);
    }
    let child = command.spawn().map_err(|error| LocalPairError::CliFailed(error.to_string()))?;
    let pid = child.id();
    let output = child.wait_with_output();
    tokio::pin!(output);
    // Declared after `output`, so it drops first: the group dies while its leader is unreaped.
    let mut group = ProcessGroup(pid);
    match tokio::time::timeout(limit, &mut output).await {
        Ok(Ok(output)) => {
            // Reaped: its id may be reused from here on.
            group.0 = None;
            Ok(output)
        }
        Ok(Err(error)) => Err(LocalPairError::CliFailed(error.to_string())),
        Err(_) => Err(LocalPairError::CliFailed("it did not finish in time.".to_owned())),
    }
}

/// Kills a CLI's process group when dropped, unless the CLI already exited.
pub(crate) struct ProcessGroup(pub(crate) Option<u32>);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
            // SAFETY: a plain syscall. The group's leader is our own unreaped child, so the id
            // cannot have been reused.
            unsafe { libc::killpg(pid, libc::SIGKILL) };
        }
    }
}

/// Configuration for hosting a local `t3 serve` subprocess (`LocalServerHost`).
#[derive(Debug, Clone)]
pub struct LocalServerHostConfig {
    pub home: PathBuf,
    pub cli_override: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub readiness_timeout: Duration,
    pub terminate_grace: Duration,
}

impl LocalServerHostConfig {
    pub fn for_home(home: PathBuf) -> Self {
        Self {
            home,
            cli_override: None,
            host: Some(DEFAULT_LOCAL_SERVER_HOST.to_owned()),
            port: None,
            readiness_timeout: DEFAULT_HOST_READINESS_TIMEOUT,
            terminate_grace: Duration::from_millis(TERMINATE_GRACE_MS),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LocalServerHostError {
    #[error("Could not determine the T3 home directory.")]
    HomeUnavailable,
    #[error("Could not find the t3 command. Open the T3 Code desktop app once to install it, or paste a pairing link below.")]
    CliMissing,
    #[error("{0}")]
    ManagedPolicy(String),
    #[error("Another T3 Code process already owns this state directory.")]
    StateDirOwned,
    #[error("{0}")]
    Incompatible(String),
    #[error("{0}")]
    ReadinessTimeout(String),
    #[error("{0}")]
    ProcessFailed(String),
    #[error("The local server was stopped before it became ready.")]
    Stopped,
    #[error(transparent)]
    Pair(#[from] LocalPairError),
}

fn failure_to_error(failure: &LocalServerFailure) -> LocalServerHostError {
    match failure.kind {
        LocalServerFailureKind::CliMissing => LocalServerHostError::CliMissing,
        LocalServerFailureKind::ManagedPolicy => LocalServerHostError::ManagedPolicy(failure.message.clone()),
        LocalServerFailureKind::StateDirOwned => LocalServerHostError::StateDirOwned,
        LocalServerFailureKind::IncompatibleProtocol => LocalServerHostError::Incompatible(failure.message.clone()),
        LocalServerFailureKind::ReadinessTimeout => LocalServerHostError::ReadinessTimeout(failure.message.clone()),
        LocalServerFailureKind::SpawnFailed | LocalServerFailureKind::Crashed { .. } => {
            LocalServerHostError::ProcessFailed(failure.message.clone())
        }
    }
}

#[cfg(unix)]
fn signal_managed_child(pid: u32, signal: libc::c_int) {
    if let Ok(pgid) = libc::pid_t::try_from(pid) {
        // SAFETY: the child was spawned with `process_group(0)`, so `pgid` contains only the
        // exact child process we spawned and its own descendants.
        unsafe {
            libc::killpg(pgid, signal);
            libc::kill(pgid, signal);
        }
    }
}

#[cfg(not(unix))]
fn signal_managed_child(_pid: u32, _signal: i32) {}

async fn discover_matching(home: &Path, expected_port: Option<u16>) -> Option<LocalServer> {
    for state in live_states(home) {
        if !matches_hosted_runtime(&state, expected_port) {
            continue;
        }
        match auth::fetch_descriptor(&state.origin).await {
            Ok(descriptor) => return Some(LocalServer { home: home.to_path_buf(), state, descriptor }),
            Err(error) => tracing::info!(origin = %state.origin, %error, "skipping a recorded T3 Code that does not answer"),
        }
    }
    None
}

struct ManagedRun {
    stop_tx: Option<oneshot::Sender<()>>,
    stopped_rx: watch::Receiver<bool>,
    home: PathBuf,
    cli: PathBuf,
}

struct LocalServerHostInner {
    phase_tx: watch::Sender<LocalServerPhase>,
    lifecycle: Mutex<LocalServerLifecycle>,
    /// Exact PID of our currently unreaped child process (leader of its own process group).
    /// Cleared immediately once `child.wait()` reaps it so a reused PID can never be signaled.
    active_pid: Mutex<Option<u32>>,
    run: Mutex<Option<ManagedRun>>,
    start_gate: tokio::sync::Mutex<()>,
}

/// Owns a managed local `t3 serve` subprocess for this app session: spawns the discovered CLI,
/// monitors stdout/stderr and exit status (suppressing restart loops on exit 77 managed policy and
/// exit 78 state-dir-owned), waits for readiness without sleep-polling loops, pairs via `t3 pair`,
/// and cleanly terminates ONLY the exact child PID it spawned on `stop()` and `Drop`.
pub struct LocalServerHost {
    inner: Arc<LocalServerHostInner>,
}

impl Default for LocalServerHost {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalServerHost {
    pub fn new() -> Self {
        let (phase_tx, _) = watch::channel(LocalServerPhase::Stopped);
        Self {
            inner: Arc::new(LocalServerHostInner {
                phase_tx,
                lifecycle: Mutex::new(LocalServerLifecycle::with_max_restarts(0)),
                active_pid: Mutex::new(None),
                run: Mutex::new(None),
                start_gate: tokio::sync::Mutex::new(()),
            }),
        }
    }

    pub fn phase(&self) -> LocalServerPhase {
        self.inner.phase_tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<LocalServerPhase> {
        self.inner.phase_tx.subscribe()
    }

    pub fn child_pid(&self) -> Option<u32> {
        *self.inner.active_pid.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn active_home_and_cli(&self) -> Option<(PathBuf, PathBuf)> {
        self.inner
            .run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|run| (run.home.clone(), run.cli.clone()))
    }

    /// Starts the local server using the default `T3CODE_HOME` (`home()`).
    pub async fn start(&self) -> Result<LocalServer, LocalServerHostError> {
        let home = home().ok_or(LocalServerHostError::HomeUnavailable)?;
        start_inner(self.inner.clone(), LocalServerHostConfig::for_home(home)).await
    }

    /// Starts the local server under `config.home` and waits until its runtime state and HTTP
    /// descriptor endpoint answer, or until the child process exits / times out.
    pub async fn start_with_config(&self, config: LocalServerHostConfig) -> Result<LocalServer, LocalServerHostError> {
        start_inner(self.inner.clone(), config).await
    }

    /// Returns a `'static` future that starts the local server using the default `T3CODE_HOME`,
    /// allowing callers (e.g. `AppState`) to keep unique ownership of `LocalServerHost` for `Drop`.
    pub fn start_future(&self) -> impl Future<Output = Result<LocalServer, LocalServerHostError>> + Send + 'static {
        let inner = self.inner.clone();
        async move {
            let home = home().ok_or(LocalServerHostError::HomeUnavailable)?;
            start_inner(inner, LocalServerHostConfig::for_home(home)).await
        }
    }

    /// Returns a `'static` future that starts the local server with `config`.
    pub fn start_with_config_future(
        &self,
        config: LocalServerHostConfig,
    ) -> impl Future<Output = Result<LocalServer, LocalServerHostError>> + Send + 'static {
        let inner = self.inner.clone();
        async move { start_inner(inner, config).await }
    }

    /// Starts the local server and immediately pairs with it via `t3 pair`.
    pub async fn start_and_pair(&self, config: LocalServerHostConfig) -> Result<(LocalServer, Paired), LocalServerHostError> {
        let cli_override = config.cli_override.clone();
        let server = self.start_with_config(config).await?;
        let paired = pair_with_cli(&server, cli_override.as_deref()).await?;
        Ok((server, paired))
    }

    /// Cleanly terminates ONLY the exact child process spawned by this host (`SIGTERM` -> grace ->
    /// `SIGKILL`) and waits until it has been reaped.
    pub async fn stop(&self) {
        stop_inner(&self.inner).await;
    }

    /// Quit hook future compatible with GPUI `cx.on_app_quit`.
    pub fn shutdown(&self) -> impl Future<Output = ()> + Send + use<> {
        let inner = self.inner.clone();
        async move {
            stop_inner(&inner).await;
        }
    }

    fn terminate_immediately(&self) {
        if let Some(run) = self.inner.run.lock().unwrap_or_else(|e| e.into_inner()).as_mut()
            && let Some(stop_tx) = run.stop_tx.take()
        {
            let _ = stop_tx.send(());
        }
        if let Some(pid) = self.inner.active_pid.lock().unwrap_or_else(|e| e.into_inner()).take() {
            #[cfg(unix)]
            signal_managed_child(pid, libc::SIGKILL);
            #[cfg(not(unix))]
            let _ = pid;
        }
        let mut lc = self.inner.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        if lc.phase().is_active() || lc.phase().is_stopping() {
            lc.apply(LocalServerEvent::StopRequested);
            lc.apply(LocalServerEvent::ProcessExited {
                exit_code: None,
                stderr: String::new(),
                stdout: String::new(),
            });
            self.inner.phase_tx.send_replace(lc.phase().clone());
        }
    }
}

impl Drop for LocalServerHost {
    fn drop(&mut self) {
        self.terminate_immediately();
    }
}

async fn start_inner(
    inner: Arc<LocalServerHostInner>,
    config: LocalServerHostConfig,
) -> Result<LocalServer, LocalServerHostError> {
    let _gate = inner.start_gate.lock().await;

    // If our managed server is already running and responsive, return it directly.
    if inner.phase_tx.borrow().is_running()
        && let Some(server) = discover_matching(&config.home, config.port).await
    {
        return Ok(server);
    }

    // If an external server already owns this home, adopt it without claiming its PID.
    let has_managed_run = inner.run.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    if !has_managed_run
        && let Some(existing) = discover_matching(&config.home, config.port).await
    {
        if let Some(mismatch) = existing.mismatch() {
            return Err(LocalServerHostError::Incompatible(mismatch.message(&existing.descriptor.label)));
        }
        return Ok(existing);
    }

    // Stop any stale run before launching a fresh process.
    if has_managed_run {
        stop_inner(&inner).await;
    }

    let cli = match config.cli_override.clone() {
        Some(path) if is_executable(&path) => path,
        Some(_) => {
            apply_event(&inner, LocalServerEvent::CliMissing);
            return Err(LocalServerHostError::CliMissing);
        }
        None => match find_cli(&config.home) {
            Some(path) => path,
            None => {
                apply_event(&inner, LocalServerEvent::CliMissing);
                return Err(LocalServerHostError::CliMissing);
            }
        },
    };

    apply_event(&inner, LocalServerEvent::StartRequested);

    let default_home = user_home().map(|user| user.join(".t3")).unwrap_or_default();
    let launch_args = serve_args(&LocalServerLaunchSpec {
        home: config.home.clone(),
        default_home,
        host: config.host.clone(),
        port: config.port,
    });

    let mut command = tokio::process::Command::new(&cli);
    command
        .args(&launch_args)
        .env("T3CODE_HOME", &config.home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    if let Some(user) = user_home() {
        command.current_dir(user);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let reason = error.to_string();
            apply_event(&inner, LocalServerEvent::SpawnFailed { reason: reason.clone() });
            return Err(LocalServerHostError::ProcessFailed(format!(
                "Could not start the local T3 Code server: {reason}"
            )));
        }
    };

    let Some(pid) = child.id() else {
        apply_event(&inner, LocalServerEvent::SpawnFailed { reason: "process exited immediately".to_owned() });
        return Err(LocalServerHostError::ProcessFailed(
            "Could not start the local T3 Code server: process exited immediately".to_owned(),
        ));
    };

    *inner.active_pid.lock().unwrap_or_else(|e| e.into_inner()) = Some(pid);
    apply_event(&inner, LocalServerEvent::Spawned { pid });

    let readiness_notify = Arc::new(Notify::new());
    let stdout_task = spawn_pipe_reader(child.stdout.take(), inner.clone(), readiness_notify.clone());
    let stderr_task = spawn_pipe_reader(child.stderr.take(), inner.clone(), readiness_notify.clone());

    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
    let (stopped_tx, stopped_rx) = watch::channel(false);
    *inner.run.lock().unwrap_or_else(|e| e.into_inner()) = Some(ManagedRun {
        stop_tx: Some(stop_tx),
        stopped_rx,
        home: config.home.clone(),
        cli: cli.clone(),
    });

    // Subscribe before spawning the monitor task so an immediate exit cannot be missed.
    let mut phase_rx = inner.phase_tx.subscribe();
    let monitor_inner = inner.clone();
    let terminate_grace = config.terminate_grace;
    tokio::spawn(async move {
        let mut stop_requested = false;
        let exit_status = tokio::select! {
            biased;
            _ = &mut stop_rx => {
                stop_requested = true;
                #[cfg(unix)]
                signal_managed_child(pid, libc::SIGTERM);
                #[cfg(not(unix))]
                let _ = child.start_kill();
                match tokio::time::timeout(terminate_grace, child.wait()).await {
                    Ok(status) => status.ok(),
                    Err(_) => {
                        #[cfg(unix)]
                        signal_managed_child(pid, libc::SIGKILL);
                        let _ = child.start_kill();
                        child.wait().await.ok()
                    }
                }
            }
            status = child.wait() => status.ok(),
        };

        // Reaped: clear `active_pid` immediately so a reused OS PID can never be signaled.
        {
            let mut active = monitor_inner.active_pid.lock().unwrap_or_else(|e| e.into_inner());
            if *active == Some(pid) {
                *active = None;
            }
        }

        let stdout_text = tokio::time::timeout(Duration::from_secs(2), stdout_task)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        let stderr_text = tokio::time::timeout(Duration::from_secs(2), stderr_task)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();

        {
            let mut lc = monitor_inner.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
            // Preserve an already-recorded ReadinessTimeout / IncompatibleProtocol failure
            // when stopping the child as cleanup for that failure.
            if !matches!(lc.phase(), LocalServerPhase::Failed(_)) {
                if stop_requested {
                    lc.apply(LocalServerEvent::StopRequested);
                }
                lc.apply(LocalServerEvent::ProcessExited {
                    exit_code: exit_status.and_then(|status| status.code()),
                    stderr: stderr_text,
                    stdout: stdout_text,
                });
                monitor_inner.phase_tx.send_replace(lc.phase().clone());
            }
        }
        *monitor_inner.run.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = stopped_tx.send(true);
    });

    let deadline = tokio::time::Instant::now() + config.readiness_timeout;
    let mut retry_delay_ms = 50u64;
    loop {
        match phase_rx.borrow_and_update().clone() {
            LocalServerPhase::Failed(failure) => return Err(failure_to_error(&failure)),
            LocalServerPhase::Stopped | LocalServerPhase::Stopping { .. } => {
                return Err(LocalServerHostError::Stopped);
            }
            _ => {}
        }

        if let Some(server) = discover_matching(&config.home, config.port).await {
            if let Some(mismatch) = server.mismatch() {
                let message = mismatch.message(&server.descriptor.label);
                apply_event(&inner, LocalServerEvent::IncompatibleProtocol { message: message.clone() });
                stop_inner(&inner).await;
                return Err(LocalServerHostError::Incompatible(message));
            }
            apply_event(
                &inner,
                LocalServerEvent::Ready {
                    pid,
                    origin: server.state.origin.clone(),
                    server_version: server.server_version().map(str::to_owned),
                },
            );
            return Ok(server);
        }

        let next_probe = (tokio::time::Instant::now() + Duration::from_millis(retry_delay_ms)).min(deadline);
        retry_delay_ms = retry_delay_ms.saturating_mul(2).min(250);
        tokio::select! {
            biased;
            changed = phase_rx.changed() => {
                if changed.is_err() {
                    return Err(LocalServerHostError::Stopped);
                }
            }
            _ = readiness_notify.notified() => {}
            _ = tokio::time::sleep_until(deadline) => {
                let expected_origin = config.port.map(|port| server_origin(config.host.as_deref(), port));
                apply_event(&inner, LocalServerEvent::ReadinessTimedOut { origin: expected_origin });
                let message = inner
                    .phase_tx
                    .borrow()
                    .failure()
                    .map(|failure| failure.message.clone())
                    .unwrap_or_else(|| "The local T3 Code server started but did not become ready in time.".to_owned());
                stop_inner(&inner).await;
                return Err(LocalServerHostError::ReadinessTimeout(message));
            }
            _ = tokio::time::sleep_until(next_probe) => {}
        }
    }
}

fn apply_event(inner: &LocalServerHostInner, event: LocalServerEvent) {
    let mut lc = inner.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
    lc.apply(event);
    inner.phase_tx.send_replace(lc.phase().clone());
}

fn spawn_pipe_reader<R>(
    pipe: Option<R>,
    inner: Arc<LocalServerHostInner>,
    readiness_notify: Arc<Notify>,
) -> tokio::task::JoinHandle<String>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let Some(pipe) = pipe else {
            return String::new();
        };
        let mut reader = BufReader::new(pipe);
        let mut collected = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    collected.push_str(&line);
                    {
                        let mut lc = inner.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
                        lc.apply(LocalServerEvent::OutputChunk(line.clone()));
                    }
                    readiness_notify.notify_one();
                }
            }
        }
        collected
    })
}

async fn stop_inner(inner: &Arc<LocalServerHostInner>) {
    let stopped_rx = {
        let mut guard = inner.run.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = guard.as_mut() else {
            let mut lc = inner.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
            if !matches!(lc.phase(), LocalServerPhase::Failed(_)) && lc.phase() != &LocalServerPhase::Stopped {
                lc.apply(LocalServerEvent::StopRequested);
                inner.phase_tx.send_replace(lc.phase().clone());
            }
            return;
        };
        {
            let mut lc = inner.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
            if !matches!(lc.phase(), LocalServerPhase::Failed(_)) {
                lc.apply(LocalServerEvent::StopRequested);
                inner.phase_tx.send_replace(lc.phase().clone());
            }
        }
        if let Some(stop_tx) = run.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        run.stopped_rx.clone()
    };
    let mut stopped_rx = stopped_rx;
    let _ = stopped_rx.wait_for(|done| *done).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::FakeServer;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("t3-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn write_state(home: &Path, dir: &str, pid: u32, origin: &str) {
        let _ = std::fs::create_dir_all(home.join(dir));
        let state = format!(r#"{{"version":1,"pid":{pid},"port":1,"origin":"{origin}","startedAt":"x","serverVersion":"0.1.6"}}"#);
        std::fs::write(home.join(dir).join("server-runtime.json"), state).expect("writes state");
    }

    fn closed_origin() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        format!("http://{}", listener.local_addr().expect("address"))
    }

    #[test]
    fn only_a_live_server_counts() {
        let home = scratch("live");
        assert_eq!(live_states(&home), Vec::new());
        // A dead release server falls through to a live dev one.
        write_state(&home, "userdata", i32::MAX as u32, "http://127.0.0.1:1");
        write_state(&home, "dev", std::process::id(), "http://127.0.0.1:2");
        let origins = |home: &Path| live_states(home).into_iter().map(|state| state.origin).collect::<Vec<_>>();
        assert_eq!(origins(&home), ["http://127.0.0.1:2"]);
        write_state(&home, "userdata", std::process::id(), "http://127.0.0.1:3");
        assert_eq!(origins(&home), ["http://127.0.0.1:3", "http://127.0.0.1:2"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn a_live_pid_whose_origin_does_not_answer_falls_through_to_the_next_server() {
        let home = scratch("discover");
        let server = FakeServer::start().await;
        server.set_descriptor(Some(serde_json::json!({ "environmentId": "env-dev", "label": "Dev", "serverVersion": "0.1.6", "orchestrationProtocolVersion": 2 })));
        let answering = server.credentials().http_base;
        // The release state's PID was reused by another process; nothing listens on its port.
        write_state(&home, "userdata", std::process::id(), &closed_origin());
        write_state(&home, "dev", std::process::id(), &answering);
        let found = discover_in(&home).await.expect("finds the server that answers");
        assert_eq!((found.state.origin.as_str(), found.environment_id()), (answering.as_str(), Some("env-dev")));

        write_state(&home, "dev", std::process::id(), &closed_origin());
        assert_eq!(discover_in(&home).await, None);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    struct SpawningCli {
        script: PathBuf,
        status: tokio::net::TcpListener,
        nonce: String,
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    impl SpawningCli {
        async fn require_bash_tcp() {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("binds preflight listener");
            let port = listener.local_addr().expect("preflight address").port();
            let mut child = tokio::process::Command::new("/bin/bash")
                .args(["-c", &format!("exec 3<>/dev/tcp/127.0.0.1/{port}")])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("bash /dev/tcp is unavailable");
            tokio::select! {
                biased;
                connection = listener.accept() => {
                    connection.expect("bash /dev/tcp is unavailable");
                }
                status = child.wait() => {
                    status.expect("bash /dev/tcp is unavailable");
                    // A successful probe may exit before Tokio delivers the queued connection.
                    listener.into_std().expect("preflight listener").accept().expect("bash /dev/tcp is unavailable");
                }
            }
            let status = child.wait().await.expect("bash /dev/tcp is unavailable");
            assert!(status.success(), "bash /dev/tcp is unavailable: {status}");
        }

        async fn new(dir: &Path) -> Self {
            use std::os::unix::fs::PermissionsExt;

            Self::require_bash_tcp().await;
            // Socket EOF readiness works on macOS, where named FIFOs can miss that event.
            let status = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("binds status listener");
            let port = status.local_addr().expect("status address").port();
            let nonce = dir.file_name().expect("scratch directory name").to_str().expect("UTF-8 scratch directory name").to_owned();
            let script = dir.join("fake-t3");
            let script_text = format!(
                r#"#!/bin/sh
/bin/bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}
printf "%s %s\n" "{nonce}" "$$" >&3
read -r release <&3
printf "survived\n" >&3' &
wait
"#
            );
            std::fs::write(&script, script_text).expect("writes script");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            Self { script, status, nonce }
        }

        async fn grandchild(&self) -> tokio::io::BufReader<tokio::net::TcpStream> {
            use tokio::io::AsyncBufReadExt;
            let mut readers = tokio::task::JoinSet::new();
            let prefix = format!("{} ", self.nonce);
            loop {
                tokio::select! {
                    connection = self.status.accept() => {
                        let (stream, _) = connection.expect("accepts grandchild");
                        readers.spawn(async move {
                            let mut events = tokio::io::BufReader::new(stream);
                            let mut line = String::new();
                            if events.read_line(&mut line).await.ok()? == 0 {
                                return None;
                            }
                            Some((events, line))
                        });
                    }
                    finished = readers.join_next(), if !readers.is_empty() => {
                        let Some(Ok(Some((events, line)))) = finished else { continue };
                        let Some(pid) = line.strip_prefix(&prefix).and_then(|line| line.strip_suffix('\n')).and_then(|pid| pid.parse().ok()) else {
                            continue;
                        };
                        if line != format!("{prefix}{pid}\n") {
                            continue;
                        }
                        assert!(process_alive(pid), "the grandchild is alive before stopping the CLI");
                        return events;
                    }
                }
            }
        }

        async fn exits(mut events: tokio::io::BufReader<tokio::net::TcpStream>) {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut remaining = Vec::new();
            // Half-close to release a surviving grandchild: it must report itself before EOF,
            // so neither natural expiry nor delayed readiness can satisfy the exit assertion.
            events.get_mut().shutdown().await.expect("releases grandchild");
            events.read_to_end(&mut remaining).await.expect("reads EOF");
            assert!(remaining.is_empty(), "the CLI's own children are killed too: {}", String::from_utf8_lossy(&remaining));
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn a_timed_out_cli_takes_everything_it_started_with_it() {
        use tokio::io::AsyncWriteExt;

        let dir = scratch("timeout");
        let cli = SpawningCli::new(&dir).await;
        let address = cli.status.local_addr().expect("status address");
        for line in ["GET / HTTP/1.1\r\n".to_owned(), String::new(), format!("{}\n", std::process::id())] {
            let mut peer = tokio::net::TcpStream::connect(address).await.expect("connects unrelated peer");
            peer.write_all(line.as_bytes()).await.expect("writes unrelated line");
        }
        let _silent = tokio::net::TcpStream::connect(address).await.expect("connects silent peer");
        let result = run_cli(&cli.script, &[], &dir, Duration::from_secs(1));
        tokio::pin!(result);
        // Spawn once, then leave the timeout future unpolled until the grandchild is ready.
        assert!(futures::poll!(&mut result).is_pending());
        let events = cli.grandchild().await;
        let result = result.await;
        assert!(matches!(result, Err(LocalPairError::CliFailed(ref message)) if message.contains("did not finish")), "{result:?}");
        SpawningCli::exits(events).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn cancelling_a_running_cli_kills_its_process_group() {
        let dir = scratch("cancel");
        let cli = SpawningCli::new(&dir).await;
        let mut task = Box::pin(run_cli(&cli.script, &[], &dir, PAIR_TIMEOUT));
        assert!(futures::poll!(&mut task).is_pending());
        let events = cli.grandchild().await;
        drop(task);
        SpawningCli::exits(events).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn managed_policy_exit_77_reports_policy_refusal_instead_of_version_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("exit77");
        let script = dir.join("fake-t3-policy");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'ManagedPolicyFileError: T3 Code cannot use the managed policy at /etc/t3/policy.json: invalid JSON.' >&2\nexit 77\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = LocalServer {
            home: dir.clone(),
            state: serde_json::from_value(serde_json::json!({
                "version": 1, "pid": std::process::id(), "port": 3773,
                "origin": "http://127.0.0.1:3773", "serverVersion": "0.1.7"
            }))
            .unwrap(),
            descriptor: EnvironmentDescriptor::default(),
        };
        let err = check_version(&script, &server).await.unwrap_err();
        assert!(
            matches!(err, LocalPairError::ManagedPolicy(ref msg) if msg.contains("managed policy") && msg.contains("IT administrator")),
            "expected ManagedPolicy, got {err:?}"
        );
        let pair_err = run_pair(&script, &dir).await.unwrap_err();
        assert!(
            matches!(pair_err, LocalPairError::ManagedPolicy(ref msg) if msg.contains("managed policy")),
            "expected ManagedPolicy from run_pair, got {pair_err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn write_mock_host_cli(
        dir: &Path,
        origin: &str,
        port: u16,
        version: &str,
    ) -> (PathBuf, tokio::net::TcpListener, String) {
        use std::os::unix::fs::PermissionsExt;

        SpawningCli::require_bash_tcp().await;
        let status = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("binds status listener");
        let status_port = status.local_addr().expect("status address").port();
        let nonce = dir.file_name().expect("scratch name").to_str().expect("UTF-8 scratch name").to_owned();
        let script = dir.join("fake-t3-host");
        let script_text = format!(
            r#"#!/bin/sh
case "$1" in
  --version)
    printf "t3 v{version}\n"
    exit 0
    ;;
  pair)
    printf "Token: HOSTEDPAIR123\n"
    exit 0
    ;;
  serve)
    mkdir -p "$T3CODE_HOME/userdata"
    printf '{{"version":1,"pid":%s,"port":{port},"origin":"{origin}","startedAt":"2026-11-20T00:00:00Z","serverVersion":"{version}"}}\n' "$$" > "$T3CODE_HOME/userdata/server-runtime.json"
    printf "ready {origin}\n"
    /bin/bash -c 'exec 3<>/dev/tcp/127.0.0.1/{status_port}
printf "%s %s\n" "{nonce}" "$$" >&3
read -r release <&3
printf "survived\n" >&3' &
    wait
    exit 0
    ;;
  *)
    echo "unexpected args: $*" >&2
    exit 2
    ;;
esac
"#
        );
        std::fs::write(&script, script_text).expect("writes host script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        (script, status, nonce)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn accept_child_socket(
        status: &tokio::net::TcpListener,
        nonce: &str,
    ) -> (u32, tokio::io::BufReader<tokio::net::TcpStream>) {
        use tokio::io::AsyncBufReadExt;
        let prefix = format!("{nonce} ");
        loop {
            let (stream, _) = status.accept().await.expect("accepts hosted child");
            let mut events = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            if events.read_line(&mut line).await.expect("reads child pid") == 0 {
                continue;
            }
            let Some(pid) = line.strip_prefix(&prefix).and_then(|rest| rest.strip_suffix('\n')).and_then(|pid| pid.parse::<u32>().ok()) else {
                continue;
            };
            assert!(process_alive(pid), "hosted child must be alive before stop/drop");
            return (pid, events);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn local_server_host_starts_becomes_ready_pairs_and_stops_cleanly() {
        let dir = scratch("host-lifecycle");
        let fake = FakeServer::start().await;
        let origin = fake.credentials().http_base;
        let port = fake.port();
        fake.set_descriptor(Some(serde_json::json!({
            "environmentId": "env-hosted-mac",
            "label": "Hosted Mac",
            "serverVersion": "0.1.9",
            "orchestrationProtocolVersion": 2
        })));

        let (script, status, nonce) = write_mock_host_cli(&dir, &origin, port, "0.1.9").await;
        let host = LocalServerHost::new();
        let mut config = LocalServerHostConfig::for_home(dir.clone());
        config.cli_override = Some(script);
        config.port = Some(port);
        config.readiness_timeout = Duration::from_secs(10);
        config.terminate_grace = Duration::from_millis(200);

        let (server, paired) = host.start_and_pair(config).await.expect("starts and pairs");
        assert_eq!(server.state.origin, origin);
        assert_eq!(server.environment_id(), Some("env-hosted-mac"));
        assert_eq!(paired.credentials.access_token, "ssh-access");
        assert!(host.phase().is_running());
        assert!(host.child_pid().is_some());

        let (_grandchild_pid, events) = accept_child_socket(&status, &nonce).await;

        host.stop().await;
        assert_eq!(host.phase(), LocalServerPhase::Stopped);
        assert_eq!(host.child_pid(), None);
        SpawningCli::exits(events).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn local_server_host_exit_77_managed_policy_refuses_without_restart() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("host-exit77");
        let log_file = dir.join("invocations.log");
        let script = dir.join("fake-t3-policy-host");
        let script_text = format!(
            "#!/bin/sh\necho invoked >> \"{}\"\necho 'ManagedPolicyFileError: organization policy blocked startup.' >&2\nexit 77\n",
            log_file.display()
        );
        std::fs::write(&script, script_text).expect("writes script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let host = LocalServerHost::new();
        let mut config = LocalServerHostConfig::for_home(dir.clone());
        config.cli_override = Some(script);
        config.readiness_timeout = Duration::from_secs(5);

        let err = host.start_with_config(config).await.unwrap_err();
        assert!(
            matches!(err, LocalServerHostError::ManagedPolicy(ref msg) if msg.contains("managed policy") && msg.contains("IT administrator")),
            "expected ManagedPolicy, got {err:?}"
        );
        let phase = host.phase();
        let failure = phase.failure().expect("host must enter Failed state");
        assert_eq!(failure.kind, LocalServerFailureKind::ManagedPolicy);
        assert!(failure.restart_suppressed);
        assert_eq!(failure.recovery, t3_core::local_server::StartupRecoveryOffer::ManagedPolicy);
        assert!(failure.output_tail.contains("ManagedPolicyFileError"));
        assert_eq!(host.child_pid(), None);

        let invocations = std::fs::read_to_string(&log_file).expect("reads invocation log");
        assert_eq!(invocations.lines().count(), 1, "exit 77 must never trigger a restart loop");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn local_server_host_drop_cleanly_terminates_spawned_child() {
        let dir = scratch("host-drop");
        let fake = FakeServer::start().await;
        let origin = fake.credentials().http_base;
        let port = fake.port();
        fake.set_descriptor(Some(serde_json::json!({
            "environmentId": "env-drop-mac",
            "label": "Drop Mac",
            "serverVersion": "0.1.9",
            "orchestrationProtocolVersion": 2
        })));

        let (script, status, nonce) = write_mock_host_cli(&dir, &origin, port, "0.1.9").await;
        let host = LocalServerHost::new();
        let mut config = LocalServerHostConfig::for_home(dir.clone());
        config.cli_override = Some(script);
        config.port = Some(port);
        config.readiness_timeout = Duration::from_secs(10);

        host.start_with_config(config).await.expect("starts hosted server");
        let (_grandchild_pid, events) = accept_child_socket(&status, &nonce).await;

        drop(host);
        SpawningCli::exits(events).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn local_server_host_reports_startup_crash_with_stderr_diagnostics() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("host-crash");
        let script = dir.join("fake-t3-crash");
        std::fs::write(&script, "#!/bin/sh\necho 'PortBindError: address already in use' >&2\nexit 1\n").expect("writes script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let host = LocalServerHost::new();
        let mut config = LocalServerHostConfig::for_home(dir.clone());
        config.cli_override = Some(script);
        config.readiness_timeout = Duration::from_secs(5);

        let err = host.start_with_config(config).await.unwrap_err();
        assert!(
            matches!(err, LocalServerHostError::ProcessFailed(ref msg) if msg.contains("address already in use")),
            "expected ProcessFailed with stderr detail, got {err:?}"
        );
        let phase = host.phase();
        let failure = phase.failure().expect("failed phase");
        assert_eq!(failure.kind, LocalServerFailureKind::Crashed { exit_code: Some(1) });
        assert!(failure.output_tail.contains("address already in use"));
        assert_eq!(host.child_pid(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
