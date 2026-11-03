//! SSH environments, the processes, as the desktop app runs them: `packages/ssh/src/command.ts`
//! (`runSshCommand`, `resolveSshTarget`), `tunnel.ts` (`SshEnvironmentManager`: launch or reuse
//! the remote T3 server, forward a local port to it, mint a pairing credential, stop it),
//! `auth.ts` (the askpass helper and the password retry), and the SSH gateway of
//! `apps/web/src/connection/platform.ts` (`provisionDesktopSshEnvironment`,
//! `prepareDesktopSshEnvironment`). The pure parts live in `t3_core::ssh`.
//!
//! Every `ssh` runs in a process group of its own and is stopped through the PID this module
//! spawned, never by name; quitting stops all of them, tunnels and commands still running alike.
//! A target keeps one local port for the app session, so the supervisor's address stays valid
//! while a tunnel is replaced underneath it, unless another process took the port.
//!
//! The remote runs the T3 release of the `t3` CLI on this Mac (the desktop runs its own version),
//! else the latest release. `T3_NATIVE_SSH_BIN` replaces the `ssh` executable.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use t3_core::compatibility::protocol_mismatch;
use t3_core::local_server::parse_cli_version;
use t3_core::pairing::PairingLink;
use t3_core::server_config::EnvironmentDescriptor;
pub use t3_core::ssh::{DiscoveredSshHost, SshTarget};
use t3_core::ssh::{self as core, LaunchResult, scripts};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::auth::{self, Paired};
use crate::local::{self, ProcessGroup};
use crate::supervisor::PrepareError;

/// What a failed `ssh`, remote script, or prompt reports: stderr, else stdout, else a summary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SshError(pub String);

const LATEST_RELEASE_URL: &str = "https://github.com/pingdotgg/t3code/releases/latest";
/// Bytes of tunnel stderr kept, enough for `TUNNEL_STDERR_TAIL_CHARS` of any text.
const TUNNEL_STDERR_TAIL_BYTES: usize = core::TUNNEL_STDERR_TAIL_CHARS * 4;

fn millis(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

struct DiskFiles;

impl core::config::ConfigFiles for DiskFiles {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn list(&self, dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir).map(|entries| entries.filter_map(|entry| entry.ok()?.file_name().into_string().ok()).collect()).unwrap_or_default()
    }
}

/// The hosts in `~/.ssh/config` and `known_hosts`. Reads the disk: run it off the UI thread.
pub fn discover_hosts() -> Vec<DiscoveredSshHost> {
    dirs::home_dir().map(|home| core::config::discover_ssh_hosts(&home, &DiskFiles)).unwrap_or_default()
}

/// One in-app password prompt (`DesktopSshPasswordPromptRequest`). The connection that asked
/// waits for `respond` until `expires_at`.
pub struct PasswordRequest {
    pub destination: String,
    pub username: Option<String>,
    pub prompt: String,
    pub attempt: u32,
    pub expires_at: Instant,
    reply: oneshot::Sender<Option<String>>,
}

impl std::fmt::Debug for PasswordRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordRequest").field("destination", &self.destination).field("attempt", &self.attempt).finish_non_exhaustive()
    }
}

impl PasswordRequest {
    /// Whether the connection that asked still waits for an answer.
    pub fn is_pending(&self) -> bool {
        !self.reply.is_closed() && Instant::now() < self.expires_at
    }

    /// Answers with the password or code, or `None` to cancel. False when nothing waits anymore.
    pub fn respond(self, answer: Option<String>) -> bool {
        self.reply.send(answer).is_ok()
    }
}

/// Quitting stops every `ssh` this module started, and waits until each one is gone.
#[derive(Clone)]
struct Lifecycle {
    stopping: watch::Receiver<bool>,
    in_flight: Arc<watch::Sender<usize>>,
    /// The process groups of the `ssh` children alive now, which quitting kills at once.
    groups: Arc<Mutex<std::collections::HashSet<u32>>>,
}

/// Lists one `ssh` process group until its leader is reaped or killed.
struct Registered(Arc<Mutex<std::collections::HashSet<u32>>>, Option<u32>);

impl Drop for Registered {
    fn drop(&mut self) {
        if let (Some(pid), Ok(mut groups)) = (self.1, self.0.lock()) {
            groups.remove(&pid);
        }
    }
}

/// Counts one `ssh` until it is killed or reaped.
struct InFlight(Arc<watch::Sender<usize>>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count = count.saturating_sub(1));
    }
}

fn quitting() -> SshError {
    SshError("T3 Code is quitting.".to_owned())
}

impl Lifecycle {
    /// Counted before the check, so a quit that starts meanwhile still waits for it.
    fn track(&self) -> Result<InFlight, SshError> {
        self.in_flight.send_modify(|count| *count += 1);
        let tracked = InFlight(self.in_flight.clone());
        if *self.stopping.borrow() { Err(quitting()) } else { Ok(tracked) }
    }

    async fn stopped(&self) {
        let mut stopping = self.stopping.clone();
        let _ = stopping.wait_for(|stopping| *stopping).await;
    }

    fn register(&self, pid: Option<u32>) -> Registered {
        if let (Some(pid), Ok(mut groups)) = (pid, self.groups.lock()) {
            groups.insert(pid);
        }
        Registered(self.groups.clone(), pid)
    }
}

/// How one `ssh` authenticates: batch mode, or the askpass helper with the cached answer.
#[derive(Clone)]
struct Auth {
    batch_mode: bool,
    interactive: bool,
    secret: Option<String>,
}

impl Auth {
    const BATCH: Auth = Auth { batch_mode: true, interactive: false, secret: None };

    fn environment(&self) -> Result<Vec<(String, String)>, SshError> {
        if !self.interactive {
            return Ok(Vec::new());
        }
        let askpass = askpass_helper().map_err(|error| SshError(format!("Failed to prepare SSH authentication helpers: {error}")))?;
        let has_display = std::env::var_os("DISPLAY").is_some_and(|display| !display.is_empty());
        Ok(core::askpass_environment(&askpass.to_string_lossy(), self.secret.as_deref(), has_display))
    }
}

/// The askpass helper, written once per session into a private directory of its own. A directory
/// that went missing or is no longer ours is replaced, never reused.
fn askpass_helper() -> std::io::Result<PathBuf> {
    static DIRECTORY: Mutex<Option<PathBuf>> = Mutex::new(None);
    let mut directory = DIRECTORY.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let reusable = directory.as_deref().is_some_and(|dir| std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir() && owned_by_us(&meta)));
    if !reusable {
        let dir = std::env::temp_dir().join(format!("t3code-ssh-runtime-{}", uuid::Uuid::new_v4().simple()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        // Fails on an existing path, so nothing planted there is used.
        builder.create(&dir)?;
        *directory = Some(dir);
    }
    let script = directory.as_deref().map(|dir| dir.join("ssh-askpass.sh")).unwrap_or_default();
    if std::fs::read_to_string(&script).ok().as_deref() != Some(core::ASKPASS_SCRIPT) {
        std::fs::write(&script, core::ASKPASS_SCRIPT)?;
    }
    #[cfg(unix)]
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(script)
}

#[cfg(unix)]
fn owned_by_us(meta: &std::fs::Metadata) -> bool {
    // SAFETY: getuid has no preconditions.
    std::os::unix::fs::MetadataExt::uid(meta) == unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn owned_by_us(_meta: &std::fs::Metadata) -> bool {
    true
}

struct CommandSpec<'a> {
    pre_host: &'a [&'a str],
    remote: &'a [&'a str],
    stdin: Option<String>,
    timeout: Duration,
}

impl Default for CommandSpec<'_> {
    fn default() -> Self {
        CommandSpec { pre_host: &[], remote: &[], stdin: None, timeout: millis(core::COMMAND_TIMEOUT_MS) }
    }
}

/// `runSshCommand`: one `ssh`, its stdin written and closed, its stdout returned. A timeout, a
/// quit, or dropping the future kills its process group.
async fn run_command(program: &Path, life: &Lifecycle, target: &SshTarget, spec: CommandSpec<'_>, auth: &Auth) -> Result<String, SshError> {
    let host = core::host_spec(target).map_err(SshError)?;
    let args = core::command_args(target, auth.batch_mode, spec.pre_host, spec.remote).map_err(SshError)?;
    let _in_flight = life.track()?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(&args)
        .envs(auth.environment()?)
        .stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    tracing::debug!(alias = %target.alias, ?args, "ssh command");
    let mut child = command.spawn().map_err(|error| SshError(format!("Failed to spawn SSH command for {host}: {error}")))?;
    let pid = child.id();
    let _registered = life.register(pid);
    let (stdin, input) = (child.stdin.take(), spec.stdin);
    let output = async move {
        let write = async move {
            if let (Some(mut pipe), Some(input)) = (stdin, input) {
                // A remote that exits early closes the pipe; its exit status tells why.
                let _ = pipe.write_all(input.as_bytes()).await;
            }
        };
        tokio::join!(write, child.wait_with_output()).1
    };
    tokio::pin!(output);
    // Declared after `output`, so it drops first: the group dies while its leader is unreaped.
    let mut group = ProcessGroup(pid);
    let finished = tokio::select! {
        biased;
        () = life.stopped() => return Err(quitting()),
        finished = tokio::time::timeout(spec.timeout, &mut output) => finished,
    };
    let output = match finished {
        Ok(Ok(output)) => {
            group.0 = None;
            output
        }
        Ok(Err(error)) => return Err(SshError(format!("Failed to run SSH command for {host}: {error}"))),
        Err(_) => {
            tracing::warn!(alias = %target.alias, remote = ?spec.remote, "ssh command timed out");
            return Err(SshError(format!("SSH command timed out after {}ms.", spec.timeout.as_millis())));
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code().map_or_else(|| "signal".to_owned(), |code| code.to_string());
    let diagnostic = core::redact_error_output(&stdout, false);
    tracing::warn!(alias = %target.alias, %code, stderr = %stderr.trim(), "ssh command failed");
    Err(SshError(core::error_message(&diagnostic, &stderr, &format!("SSH command failed for {host} (exit {code})."))))
}

/// A running `ssh -N -L` and what it forwards.
struct Tunnel {
    /// The target as resolved when it started: where its remote server runs.
    target: SshTarget,
    local_port: u16,
    http_base: String,
    /// `Some(exit code)` once the `ssh` is gone.
    exit: watch::Receiver<Option<Option<i32>>>,
    /// Dropping it stops the `ssh`.
    stop: Option<oneshot::Sender<()>>,
    monitor: Option<JoinHandle<()>>,
}

impl Tunnel {
    fn running(&self) -> bool {
        self.exit.borrow().is_none()
    }

    /// Stops the `ssh`; the handle finishes once it is reaped and its port is free.
    fn stop(mut self) -> Option<JoinHandle<()>> {
        self.stop.take();
        self.monitor.take()
    }
}

#[cfg(unix)]
fn signal_group(pid: Option<u32>, signal: libc::c_int) {
    if let Some(pid) = pid.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        // SAFETY: a plain syscall. The group's leader is the monitor's own unreaped child.
        unsafe { libc::killpg(pid, signal) };
    }
}

/// SIGTERM, then SIGKILL after `TUNNEL_SHUTDOWN_TIMEOUT_MS`.
async fn stop_child(child: &mut tokio::process::Child, pid: Option<u32>) -> Option<i32> {
    #[cfg(unix)]
    signal_group(pid, libc::SIGTERM);
    #[cfg(not(unix))]
    let _ = (child.start_kill(), pid);
    match tokio::time::timeout(millis(core::TUNNEL_SHUTDOWN_TIMEOUT_MS), child.wait()).await {
        Ok(status) => status.ok().and_then(|status| status.code()),
        Err(_) => {
            #[cfg(unix)]
            signal_group(pid, libc::SIGKILL);
            let _ = child.start_kill();
            child.wait().await.ok().and_then(|status| status.code())
        }
    }
}

fn readiness_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().no_proxy().build().expect("reqwest client builds"))
}

/// `waitForHttpReady`: `GET /` answers 2xx within `limit`.
async fn wait_http_ready(http_base: &str, limit: Duration) -> Result<(), String> {
    let url = format!("{http_base}/");
    let deadline = Instant::now() + limit;
    loop {
        let probe = readiness_client().get(&url).timeout(millis(core::READY_PROBE_TIMEOUT_MS)).send().await;
        if probe.is_ok_and(|response| response.status().is_success()) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("Timed out waiting {}ms for backend readiness at {url}.", limit.as_millis()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn tail_text(tail: &Mutex<Vec<u8>>) -> String {
    let bytes = tail.lock().map(|tail| tail.clone()).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(core::TUNNEL_STDERR_TAIL_CHARS)).collect()
}

/// `startSshTunnel`: forwards `local_port` and waits until the remote server answers through it.
/// An `ssh` that exits first fails with the tail of what it printed.
async fn start_tunnel(program: &Path, life: &Lifecycle, target: &SshTarget, local_port: u16, remote_port: u16, auth: Auth) -> Result<Tunnel, SshError> {
    let args = core::tunnel_args(target, auth.batch_mode, local_port, remote_port).map_err(SshError)?;
    let in_flight = life.track()?;
    let mut command = tokio::process::Command::new(program);
    command.args(&args).envs(auth.environment()?).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| SshError(format!("Failed to spawn SSH tunnel for {}: {error}", target.alias)))?;
    let pid = child.id();
    let registered = life.register(pid);
    tracing::debug!(alias = %target.alias, ?pid, local_port, remote_port, "ssh tunnel started");
    let tail = Arc::new(Mutex::new(Vec::new()));
    let drain = child.stderr.take().map(|mut stderr| {
        let tail = tail.clone();
        tokio::spawn(async move {
            let mut buffer = [0u8; 4096];
            while let Ok(read) = stderr.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                if let Ok(mut tail) = tail.lock() {
                    tail.extend_from_slice(&buffer[..read]);
                    let excess = tail.len().saturating_sub(TUNNEL_STDERR_TAIL_BYTES);
                    tail.drain(..excess);
                }
            }
        })
    });
    let (exit_tx, exit) = watch::channel(None);
    let (stop, stop_rx) = oneshot::channel::<()>();
    let life = life.clone();
    let monitor = tokio::spawn(async move {
        let _in_flight = in_flight;
        let stop = async {
            tokio::select! {
                _ = stop_rx => {}
                () = life.stopped() => {}
            }
        };
        let code = tokio::select! {
            biased;
            status = child.wait() => status.ok().and_then(|status| status.code()),
            () = stop => stop_child(&mut child, pid).await,
        };
        // The leader is reaped now, so its group ID may be reused: quit must no longer signal it.
        drop(registered);
        // Diagnostics drain until the tunnel closes, unless a helper it started holds stderr.
        if let Some(drain) = drain {
            let _ = tokio::time::timeout(Duration::from_millis(500), drain).await;
        }
        let _ = exit_tx.send(Some(code));
    });
    let tunnel = Tunnel { target: target.clone(), local_port, http_base: format!("http://127.0.0.1:{local_port}"), exit, stop: Some(stop), monitor: Some(monitor) };
    let mut exit = tunnel.exit.clone();
    let exited = async move { exit.wait_for(Option::is_some).await.ok().and_then(|code| *code).flatten() };
    tokio::select! {
        ready = wait_http_ready(&tunnel.http_base, millis(core::TUNNEL_READY_TIMEOUT_MS)) => match ready {
            Ok(()) => Ok(tunnel),
            Err(message) => {
                tracing::warn!(alias = %target.alias, local_port, remote_port, %message, "ssh tunnel never answered");
                tunnel.stop();
                Err(SshError(message))
            }
        },
        code = exited => {
            let code = code.map_or_else(|| "signal".to_owned(), |code| code.to_string());
            let stderr = tail_text(&tail);
            tracing::warn!(alias = %target.alias, local_port, remote_port, %code, stderr = %stderr.trim(), "ssh tunnel exited");
            Err(SshError(core::error_message("", &stderr, &format!("SSH tunnel exited unexpectedly for {} (exit {code}).", target.alias))))
        }
    }
}

/// `launchOrReuseRemoteServer`.
async fn launch_remote(program: &Path, life: &Lifecycle, target: &SshTarget, auth: Auth, version: &str) -> Result<LaunchResult, SshError> {
    let state_key = core::native_state_key(target);
    let spec = CommandSpec {
        remote: &["sh", "-l", "-s", "--", state_key.as_str()],
        stdin: Some(scripts::launch_script(version).map_err(SshError)?),
        timeout: millis(core::ARCHIVE_LAUNCH_TIMEOUT_MS),
        ..Default::default()
    };
    let stdout = run_command(program, life, target, spec, &auth).await?;
    let launched = core::parse_launch_output(&stdout).map_err(SshError)?;
    tracing::info!(alias = %target.alias, remote_port = launched.remote_port, kind = ?launched.server_kind, "remote T3 server ready");
    Ok(launched)
}

/// `issueRemotePairingToken`.
async fn issue_pairing_token(program: &Path, life: &Lifecycle, target: &SshTarget, auth: Auth, version: &str) -> Result<String, SshError> {
    let state_key = core::native_state_key(target);
    let spec = CommandSpec {
        remote: &["sh", "-s"],
        stdin: Some(scripts::pairing_script(&state_key, version).map_err(SshError)?),
        timeout: millis(core::ARCHIVE_LAUNCH_TIMEOUT_MS),
        ..Default::default()
    };
    core::parse_pairing_output(&run_command(program, life, target, spec, &auth).await?).map_err(SshError)
}

/// `stopRemoteServer`: only a server this app started.
async fn stop_remote(program: &Path, life: &Lifecycle, target: &SshTarget, auth: Auth) -> Result<(), SshError> {
    let spec = CommandSpec { remote: &["sh", "-s"], stdin: Some(scripts::stop_script(&core::native_state_key(target))), ..Default::default() };
    run_command(program, life, target, spec, &auth).await.map(drop)
}

/// The installed `t3` CLI's release, the one the desktop app on this Mac would run remotely.
async fn installed_cli_version() -> Option<String> {
    let home = local::home()?;
    let cli = local::find_cli(&home)?;
    let output = local::run_cli(&cli, &["--version".to_owned()], &home, local::VERSION_TIMEOUT).await.ok()?;
    parse_cli_version(&String::from_utf8_lossy(&output.stdout)).filter(|version| core::is_exact_archive_version(version))
}

/// GitHub answers `releases/latest` with a redirect to the tag.
async fn latest_release_version() -> Option<String> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().ok()?;
    let response = client.get(LATEST_RELEASE_URL).send().await.ok()?;
    let tag = response.url().path_segments()?.next_back()?.to_owned();
    tag.strip_prefix('v').filter(|version| core::is_exact_archive_version(version)).map(str::to_owned)
}

fn reserve_loopback_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// What `ensure` readied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bootstrap {
    /// The target as `ssh -G` resolved it, with the caller's username and port.
    pub target: SshTarget,
    pub http_base: String,
    pub pairing_token: Option<String>,
}

/// A pairing made over SSH, and the resolved target to save with it.
#[derive(Debug, Clone)]
pub struct SshPairing {
    pub paired: Paired,
    pub target: SshTarget,
}

#[derive(Default)]
struct State {
    /// By the key of the target as resolved when the tunnel started.
    tunnels: HashMap<String, Tunnel>,
    /// The tunnel each caller's target reached last, by key.
    routes: HashMap<String, String>,
    /// Each target's local port for the session.
    ports: HashMap<String, u16>,
    /// Answers from the password prompt, kept in memory for the session only.
    secrets: HashMap<String, String>,
}

struct Inner {
    program: PathBuf,
    prompts: Option<mpsc::UnboundedSender<PasswordRequest>>,
    stopping: watch::Sender<bool>,
    in_flight: Arc<watch::Sender<usize>>,
    groups: Arc<Mutex<std::collections::HashSet<u32>>>,
    version: Mutex<Option<String>>,
    state: Mutex<State>,
    /// One per target, so a reconnect never reuses a server while its stop is pending.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// The app's SSH environments: their tunnels, ports, and cached answers (`SshEnvironmentManager`).
#[derive(Clone)]
pub struct SshEnvironments {
    inner: Arc<Inner>,
}

fn preparation_error(error: SshError) -> PrepareError {
    // `sshPreparationError`: a cancelled prompt waits for the user; anything else retries.
    if error.0.to_lowercase().contains("cancel") {
        PrepareError::Blocked(error.0)
    } else {
        PrepareError::Transient(format!("Could not prepare the SSH environment: {}", error.0))
    }
}

fn check_identity(descriptor: &EnvironmentDescriptor, expected: Option<&str>) -> Result<(), PrepareError> {
    match expected.filter(|expected| !expected.is_empty()) {
        Some(expected) if descriptor.environment_id != expected => Err(PrepareError::Blocked(format!(
            "That host reaches {}, a different machine. Add it as its own environment instead.",
            descriptor.label
        ))),
        _ => Ok(()),
    }
}

impl SshEnvironments {
    /// `prompts` receives the in-app password prompts; without a listener, a host that wants a
    /// password fails as it would in batch mode.
    pub fn new(prompts: Option<mpsc::UnboundedSender<PasswordRequest>>) -> Self {
        let program = std::env::var_os("T3_NATIVE_SSH_BIN").filter(|program| !program.is_empty()).map_or_else(|| PathBuf::from("ssh"), PathBuf::from);
        Self::with_program(program, prompts, None)
    }

    fn with_program(program: PathBuf, prompts: Option<mpsc::UnboundedSender<PasswordRequest>>, version: Option<String>) -> Self {
        let inner = Inner {
            program,
            prompts,
            stopping: watch::Sender::new(false),
            in_flight: Arc::new(watch::Sender::new(0)),
            groups: Arc::default(),
            version: Mutex::new(version),
            state: Mutex::default(),
            locks: Mutex::default(),
        };
        SshEnvironments { inner: Arc::new(inner) }
    }

    fn life(&self) -> Lifecycle {
        Lifecycle { stopping: self.inner.stopping.subscribe(), in_flight: self.inner.in_flight.clone(), groups: self.inner.groups.clone() }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_for(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.inner.locks.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).entry(key.to_owned()).or_default().clone()
    }

    /// The local port `target` is forwarded on for the rest of the session.
    pub fn session_port(&self, target: &SshTarget) -> std::io::Result<u16> {
        self.session_port_for(&core::target_connection_key(target))
    }

    fn session_port_for(&self, key: &str) -> std::io::Result<u16> {
        if let Some(port) = self.state().ports.get(key) {
            return Ok(*port);
        }
        let port = reserve_loopback_port()?;
        Ok(*self.state().ports.entry(key.to_owned()).or_insert(port))
    }

    /// `resolveSshTarget`: `ssh -G`'s view of an alias, or the alias as typed when that fails.
    pub async fn resolve_target(&self, alias: &str) -> Result<SshTarget, SshError> {
        let alias = alias.trim();
        if alias.is_empty() {
            return Err(SshError("SSH host alias is required.".to_owned()));
        }
        let raw = SshTarget { alias: alias.to_owned(), hostname: alias.to_owned(), username: None, port: None };
        // No host name to pin: `ssh -G` reports the one the config gives.
        let ask = SshTarget { hostname: String::new(), ..raw.clone() };
        match run_command(&self.inner.program, &self.life(), &ask, CommandSpec { pre_host: &["-G"], ..Default::default() }, &Auth::BATCH).await {
            Ok(stdout) => Ok(core::parse_ssh_resolve_output(alias, &stdout)),
            Err(error) => {
                tracing::debug!(%alias, %error, "ssh -G failed; using the alias as typed");
                Ok(raw)
            }
        }
    }

    async fn resolve_with_overrides(&self, target: &SshTarget) -> Result<SshTarget, SshError> {
        let base = self.resolve_target(if target.alias.trim().is_empty() { &target.hostname } else { &target.alias }).await?;
        Ok(SshTarget { username: target.username.clone().or(base.username), port: target.port.or(base.port), ..base })
    }

    async fn runner_version(&self) -> Result<String, SshError> {
        if let Some(version) = self.inner.version.lock().ok().and_then(|version| version.clone()) {
            return Ok(version);
        }
        let version = match installed_cli_version().await {
            Some(version) => version,
            None => latest_release_version()
                .await
                .ok_or_else(|| SshError("Could not find a T3 Code release to run on the remote host. Install T3 Code on this Mac and try again.".to_owned()))?,
        };
        tracing::info!(%version, "SSH environments run this T3 Code release");
        if let Ok(mut cached) = self.inner.version.lock() {
            *cached = Some(version.clone());
        }
        Ok(version)
    }

    async fn prompt(&self, target: &SshTarget, attempt: u32) -> Result<String, SshError> {
        let host = core::host_spec(target).map_err(SshError)?;
        let unavailable = || SshError("T3 Code window is not available for SSH authentication.".to_owned());
        let prompts = self.inner.prompts.as_ref().ok_or_else(unavailable)?;
        let destination = Some(target.alias.trim()).filter(|alias| !alias.is_empty()).unwrap_or(target.hostname.trim()).to_owned();
        let limit = millis(core::PASSWORD_PROMPT_TIMEOUT_MS);
        let (reply, answer) = oneshot::channel();
        let prompt = core::password_prompt_text(target).map_err(SshError)?;
        let request = PasswordRequest { destination: destination.clone(), username: target.username.clone(), prompt, attempt, expires_at: Instant::now() + limit, reply };
        prompts.send(request).map_err(|_| unavailable())?;
        match tokio::time::timeout(limit, answer).await {
            Ok(Ok(Some(answer))) => Ok(answer),
            Ok(Ok(None)) => Err(SshError(format!("SSH authentication cancelled for {host}."))),
            Ok(Err(_)) => Err(SshError("SSH authentication was cancelled because the app window closed.".to_owned())),
            Err(_) => Err(SshError(format!("SSH authentication timed out for {destination}."))),
        }
    }

    /// `runWithSshAuth`: batch mode first; on an auth failure a password could fix, ask in the
    /// app (twice at most) and retry through the askpass helper. Answers are reused for the
    /// session.
    async fn with_auth<T, F, Fut>(&self, key: &str, target: &SshTarget, operation: F) -> Result<T, SshError>
    where
        F: Fn(Auth) -> Fut,
        Fut: Future<Output = Result<T, SshError>>,
    {
        let available = self.inner.prompts.as_ref().is_some_and(|prompts| !prompts.is_closed());
        let mut secret = self.state().secrets.get(key).cloned();
        let mut prompts = 0;
        loop {
            let auth = match &secret {
                None => Auth { batch_mode: available, interactive: !available, secret: None },
                Some(secret) => Auth { batch_mode: false, interactive: true, secret: Some(secret.clone()) },
            };
            let error = match operation(auth).await {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            if !core::is_auth_failure(&error.0) || !available {
                return Err(error);
            }
            tracing::warn!(alias = %target.alias, prompts, "ssh authentication failed");
            if secret.take().is_some() {
                self.state().secrets.remove(key);
            }
            if prompts >= core::MAX_PASSWORD_PROMPTS || core::is_agent_signing_failure(&error.0) {
                return Err(error);
            }
            prompts += 1;
            let answer = self.prompt(target, prompts).await?;
            self.state().secrets.insert(key.to_owned(), answer.clone());
            secret = Some(answer);
        }
    }

    /// The tunnel `target` reached last, and the target as resolved when it started.
    fn recorded_route(&self, target: &SshTarget) -> Option<(String, SshTarget)> {
        let state = self.state();
        let key = state.routes.get(&core::target_connection_key(target))?;
        state.tunnels.get(key).map(|tunnel| (key.clone(), tunnel.target.clone()))
    }

    /// `ensureEnvironment`: the remote server running and forwarded to this target's session
    /// port, reusing a tunnel that still answers; with `issue_pairing`, a fresh credential too.
    /// A running tunnel keeps the resolution it started with.
    pub async fn ensure(&self, target: &SshTarget, issue_pairing: bool) -> Result<Bootstrap, SshError> {
        let caller = core::target_connection_key(target);
        let recorded = self.recorded_route(target).filter(|(key, _)| self.state().tunnels.get(key).is_some_and(Tunnel::running));
        let resolved = match recorded {
            Some((_, resolved)) => resolved,
            None => self.resolve_with_overrides(target).await?,
        };
        let key = core::target_connection_key(&resolved);
        let version = self.runner_version().await?;
        let lock = self.lock_for(&key);
        let _guard = lock.lock().await;
        let http_base = self.ensure_tunnel(&caller, &key, &resolved, &version).await?;
        {
            // The caller's target and the resolved one, which the app saves, both lead here.
            let mut state = self.state();
            state.routes.insert(caller, key.clone());
            state.routes.insert(key.clone(), key.clone());
        }
        let (program, life) = (&self.inner.program, self.life());
        let pairing_token = if issue_pairing {
            Some(self.with_auth(&key, &resolved, |auth| issue_pairing_token(program, &life, &resolved, auth, &version)).await?)
        } else {
            None
        };
        Ok(Bootstrap { target: resolved, http_base, pairing_token })
    }

    async fn ensure_tunnel(&self, caller: &str, key: &str, target: &SshTarget, version: &str) -> Result<String, SshError> {
        let reserve_error = |error: std::io::Error| SshError(format!("Could not reserve a local port for the SSH tunnel: {error}"));
        let mut port = self.session_port_for(caller).map_err(reserve_error)?;
        let reusable = self.state().tunnels.get(key).filter(|tunnel| tunnel.running() && tunnel.local_port == port).map(|tunnel| tunnel.http_base.clone());
        if let Some(http_base) = reusable {
            if wait_http_ready(&http_base, millis(core::TUNNEL_REUSE_TIMEOUT_MS)).await.is_ok() {
                return Ok(http_base);
            }
            tracing::warn!(alias = %target.alias, port, "ssh tunnel stopped answering; replacing it");
        }
        // A stale forward closes without stopping the remote server.
        let stale = self.state().tunnels.remove(key);
        if let Some(monitor) = stale.and_then(Tunnel::stop) {
            let _ = monitor.await;
        }
        let (program, life) = (&self.inner.program, self.life());
        let launched = self.with_auth(key, target, |auth| launch_remote(program, &life, target, auth, version)).await?;
        let mut moved = false;
        let tunnel = loop {
            match self.with_auth(key, target, |auth| start_tunnel(program, &life, target, port, launched.remote_port, auth)).await {
                // Another process took the port while no tunnel held it: move to a free one.
                Err(error) if !moved && core::is_forward_port_taken(&error.0) => {
                    port = reserve_loopback_port().map_err(reserve_error)?;
                    tracing::warn!(alias = %target.alias, port, "the SSH tunnel's local port was taken; moving it");
                    self.state().ports.insert(caller.to_owned(), port);
                    moved = true;
                }
                result => break result?,
            }
        };
        // The resolved target shares the port, so a later call with it reaches the same tunnel.
        self.state().ports.insert(key.to_owned(), port);
        let http_base = tunnel.http_base.clone();
        self.state().tunnels.insert(key.to_owned(), tunnel);
        Ok(http_base)
    }

    /// `disconnectEnvironment`: closes the tunnel `target` reached and stops the remote server
    /// this app started where that tunnel led, even if the SSH config has changed since.
    pub async fn disconnect(&self, target: &SshTarget) -> Result<(), SshError> {
        let (key, resolved) = match self.recorded_route(target) {
            Some(route) => route,
            None => {
                let resolved = self.resolve_with_overrides(target).await?;
                (core::target_connection_key(&resolved), resolved)
            }
        };
        let lock = self.lock_for(&key);
        let _guard = lock.lock().await;
        self.state().routes.remove(&core::target_connection_key(target));
        let tunnel = self.state().tunnels.remove(&key);
        if let Some(monitor) = tunnel.and_then(Tunnel::stop) {
            let _ = monitor.await;
        }
        let (program, life) = (&self.inner.program, self.life());
        self.with_auth(&key, &resolved, |auth| stop_remote(program, &life, &resolved, auth)).await?;
        tracing::info!(alias = %resolved.alias, "SSH environment disconnected");
        Ok(())
    }

    /// Quitting: kills every `ssh` this app started, tunnels and commands still running alike,
    /// before returning, since quit handlers get only a moment; and refuses new ones. Remote
    /// servers keep running for the next launch to reuse. The future finishes once each `ssh` is
    /// gone.
    pub fn shutdown(&self) -> impl Future<Output = ()> + Send + use<> {
        self.inner.stopping.send_replace(true);
        #[cfg(unix)]
        if let Ok(groups) = self.inner.groups.lock() {
            for pid in groups.iter() {
                // The whole group: ssh's own helpers (a ProxyCommand) die with it. A group ID
                // outlives a reaped leader only through members this ssh started.
                signal_group(Some(*pid), libc::SIGKILL);
            }
        }
        let tunnels: Vec<Tunnel> = self.state().tunnels.drain().map(|(_, tunnel)| tunnel).collect();
        drop(tunnels);
        let mut in_flight = self.inner.in_flight.subscribe();
        async move {
            let _ = in_flight.wait_for(|count| *count == 0).await;
        }
    }

    /// `provisionDesktopSshEnvironment`: readies the target, checks who answers, and redeems a
    /// pairing credential minted over SSH. `expected_environment_id` refuses a different machine.
    pub async fn provision(&self, target: &SshTarget, expected_environment_id: Option<&str>) -> Result<SshPairing, PrepareError> {
        let bootstrap = self.ensure(target, true).await.map_err(preparation_error)?;
        let Some(token) = bootstrap.pairing_token.clone() else {
            return Err(PrepareError::Blocked("The SSH environment did not issue a pairing credential.".to_owned()));
        };
        let descriptor = auth::fetch_descriptor(&bootstrap.http_base).await.map_err(|error| preparation_error(SshError(error.to_string())))?;
        check_identity(&descriptor, expected_environment_id)?;
        if let Some(mismatch) = protocol_mismatch(&descriptor) {
            return Err(PrepareError::Blocked(mismatch.message(&descriptor.label)));
        }
        let link = PairingLink { http_base: bootstrap.http_base.clone(), token };
        let credentials = auth::redeem_pairing(&link).await.map_err(|error| preparation_error(SshError(error.to_string())))?;
        Ok(SshPairing { paired: Paired { credentials, descriptor: Some(descriptor) }, target: bootstrap.target })
    }

    /// The step before each connection attempt (`prepareDesktopSshEnvironment`): the tunnel is up
    /// at `http_base`, and it still reaches the saved environment before the saved credential is
    /// sent. A tunnel that had to move to another port reports where.
    pub async fn prepare(&self, target: &SshTarget, expected_environment_id: Option<&str>, http_base: &str) -> Result<(), PrepareError> {
        let bootstrap = self.ensure(target, false).await.map_err(preparation_error)?;
        if bootstrap.http_base != http_base {
            return Err(PrepareError::Moved(bootstrap.http_base));
        }
        let descriptor = auth::fetch_descriptor(&bootstrap.http_base).await.map_err(|error| preparation_error(SshError(error.to_string())))?;
        check_identity(&descriptor, expected_environment_id)
    }
}

#[cfg(test)]
mod tests;
