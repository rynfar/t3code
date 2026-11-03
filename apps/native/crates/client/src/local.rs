//! One-click pairing with T3 Code on this Mac: find the running server the way `t3 pair` does
//! (`t3_core::local_server`, `discoverPairTarget`), check it speaks our protocol and that the
//! installed CLI is its exact version, mint a token with `t3 pair` (the supported way to pair a
//! device), and redeem it at the server's own origin.
//!
//! `T3CODE_HOME` moves the T3 home, as it does for the CLI. `T3_NATIVE_T3_BIN` replaces the CLI
//! (tests point it at a wrapper around a dev server's `bin.ts`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use t3_core::compatibility::{ProtocolMismatch, protocol_mismatch};
use t3_core::local_server::{
    CliVersionRefusal, ServerRuntimeState, check_cli_version, cli_candidates, cli_failure_summary, pair_args, parse_pair_output, parse_runtime_state,
    runtime_state_paths, t3_home,
};
use t3_core::pairing::PairingLink;
use t3_core::server_config::EnvironmentDescriptor;

use crate::auth::{self, AuthError, Paired};

/// Booting the CLI and opening the server's database takes a few seconds; a hung one gives up.
const PAIR_TIMEOUT: Duration = Duration::from_secs(45);
/// `t3 --version` answers before loading the server.
pub(crate) const VERSION_TIMEOUT: Duration = Duration::from_secs(15);

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
    let current = discover_in(&server.home).await.ok_or(LocalPairError::NotRunning)?;
    if let Some(mismatch) = current.mismatch() {
        return Err(LocalPairError::Incompatible(mismatch.message(&current.descriptor.label)));
    }
    let cli = find_cli(&current.home).ok_or(LocalPairError::CliMissing)?;
    check_version(&cli, &current).await?;
    let token = run_pair(&cli, &current.home).await?;
    let credentials = auth::redeem_pairing(&PairingLink { http_base: current.state.origin.clone(), token }).await?;
    Ok(Paired { credentials, descriptor: Some(current.descriptor) })
}

/// `t3 pair` migrates the database it opens, so it only runs when `t3 --version` is exactly the
/// version the running server recorded.
async fn check_version(cli: &Path, server: &LocalServer) -> Result<(), LocalPairError> {
    let output = run_cli(cli, &["--version".to_owned()], &server.home, VERSION_TIMEOUT).await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
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
        return Err(LocalPairError::CliFailed(cli_failure_summary(&String::from_utf8_lossy(&output.stderr), &stdout)));
    }
    parse_pair_output(&stdout).map(|output| output.token).ok_or(LocalPairError::NoToken)
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

    #[cfg(unix)]
    struct SpawningCli {
        script: PathBuf,
        status: tokio::net::TcpListener,
    }

    #[cfg(unix)]
    impl SpawningCli {
        async fn new(dir: &Path) -> Self {
            use std::os::unix::fs::PermissionsExt;

            // Socket EOF readiness works on macOS, where named FIFOs can miss that event.
            let status = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("binds status listener");
            let port = status.local_addr().expect("status address").port();
            let script = dir.join("fake-t3");
            let script_text = format!(
                r#"#!/bin/sh
/bin/bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}
printf "%s\n" "$$" >&3
read -r release <&3
printf "survived\n" >&3' &
wait
"#
            );
            std::fs::write(&script, script_text).expect("writes script");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            Self { script, status }
        }

        async fn grandchild(&self) -> tokio::io::BufReader<tokio::net::TcpStream> {
            use tokio::io::AsyncBufReadExt;
            let (stream, _) = self.status.accept().await.expect("accepts grandchild");
            let mut events = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            events.read_line(&mut line).await.expect("reads PID");
            let pid = line.trim().parse().expect("grandchild PID");
            assert!(process_alive(pid), "the grandchild is alive before stopping the CLI");
            events
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

    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_cli_takes_everything_it_started_with_it() {
        let dir = scratch("timeout");
        let cli = SpawningCli::new(&dir).await;
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

    #[cfg(unix)]
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
}
