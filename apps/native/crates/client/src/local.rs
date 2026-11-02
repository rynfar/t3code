//! One-click pairing with T3 Code on this Mac: find the running server the way `t3 pair` does
//! (`t3_core::local_server`), check it speaks our protocol, mint a token with the installed
//! `t3 pair` (the supported way to pair a device), and redeem it at the server's own origin.
//!
//! `T3CODE_HOME` moves the T3 home, as it does for the CLI. `T3_NATIVE_T3_BIN` replaces the CLI
//! (tests point it at a wrapper around a dev server's `bin.ts`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use t3_core::compatibility::{ProtocolMismatch, protocol_mismatch};
use t3_core::local_server::{ServerRuntimeState, cli_candidates, cli_failure_summary, pair_args, parse_pair_output, parse_runtime_state, runtime_state_paths, t3_home};
use t3_core::pairing::PairingLink;
use t3_core::server_config::EnvironmentDescriptor;

use crate::auth::{self, AuthError, Paired};

/// Booting the CLI and opening the server's database takes a few seconds; a hung one gives up.
const PAIR_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, PartialEq)]
pub struct LocalServer {
    pub home: PathBuf,
    pub state: ServerRuntimeState,
    /// `None` while the server is not answering yet.
    pub descriptor: Option<EnvironmentDescriptor>,
}

impl LocalServer {
    pub fn server_version(&self) -> Option<&str> {
        self.descriptor.as_ref().map(|d| d.server_version.as_str()).filter(|v| !v.is_empty()).or(self.state.server_version.as_deref())
    }

    pub fn label(&self) -> &str {
        self.descriptor.as_ref().map(|d| d.label.trim()).filter(|label| !label.is_empty()).unwrap_or("this Mac")
    }

    pub fn mismatch(&self) -> Option<ProtocolMismatch> {
        protocol_mismatch(self.descriptor.as_ref()?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LocalPairError {
    #[error("T3 Code is no longer running on this Mac. Open it and try again.")]
    NotRunning,
    #[error("T3 Code on this Mac is not answering at {0}. Wait for it to finish starting, then try again.")]
    NotAnswering(String),
    #[error("{0}")]
    Incompatible(String),
    #[error("Could not find the t3 command. Open the T3 Code desktop app once to install it, or paste a pairing link below.")]
    CliMissing,
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

/// The T3 home the CLI would use: `T3CODE_HOME`, else `~/.t3`.
pub fn home() -> Option<PathBuf> {
    Some(t3_home(std::env::var("T3CODE_HOME").ok().as_deref(), &user_home()?))
}

/// A live server recorded under `home`, in `t3 pair`'s order. A dead server's state file is
/// skipped: its port may belong to something else now.
pub fn find_running(home: &Path) -> Option<ServerRuntimeState> {
    runtime_state_paths(home)
        .iter()
        .filter_map(|path| parse_runtime_state(&std::fs::read_to_string(path).ok()?))
        .find(|state| process_alive(state.pid))
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

/// T3 Code running on this machine, with its descriptor when it answers.
pub async fn discover() -> Option<LocalServer> {
    let home = home()?;
    let state = find_running(&home)?;
    let descriptor = auth::fetch_descriptor(&state.origin).await.ok();
    Some(LocalServer { home, state, descriptor })
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

/// Pairs with the server `discover` found: re-checks it is alive and compatible, runs
/// `t3 pair`, and redeems the token at the server's origin, which is always reachable from here.
pub async fn pair(server: &LocalServer) -> Result<Paired, LocalPairError> {
    let state = find_running(&server.home).ok_or(LocalPairError::NotRunning)?;
    let descriptor = auth::fetch_descriptor(&state.origin).await.map_err(|_| LocalPairError::NotAnswering(state.origin.clone()))?;
    if let Some(mismatch) = protocol_mismatch(&descriptor) {
        return Err(LocalPairError::Incompatible(mismatch.message(&descriptor.label)));
    }
    let cli = find_cli(&server.home).ok_or(LocalPairError::CliMissing)?;
    let token = run_pair(&cli, &server.home).await?;
    let credentials = auth::redeem_pairing(&PairingLink { http_base: state.origin.clone(), token }).await?;
    Ok(Paired { credentials, descriptor: Some(descriptor) })
}

async fn run_pair(cli: &Path, home: &Path) -> Result<String, LocalPairError> {
    let default_home = user_home().map(|user| user.join(".t3")).unwrap_or_default();
    let mut command = tokio::process::Command::new(cli);
    command.args(pair_args(home, &default_home)).stdin(std::process::Stdio::null()).kill_on_drop(true);
    // From a linked worktree the CLI would pair with that worktree's server instead.
    if let Some(user) = user_home() {
        command.current_dir(user);
    }
    let output = tokio::time::timeout(PAIR_TIMEOUT, command.output())
        .await
        .map_err(|_| LocalPairError::CliFailed("it did not finish in time.".to_owned()))?
        .map_err(|error| LocalPairError::CliFailed(error.to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(LocalPairError::CliFailed(cli_failure_summary(&String::from_utf8_lossy(&output.stderr), &stdout)));
    }
    parse_pair_output(&stdout).map(|output| output.token).ok_or(LocalPairError::NoToken)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_live_server_counts() {
        let home = std::env::temp_dir().join(format!("t3-local-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(find_running(&home), None);
        let write = |dir: &str, pid: u32, port: u16| {
            let _ = std::fs::create_dir_all(home.join(dir));
            let state = format!(r#"{{"version":1,"pid":{pid},"port":{port},"origin":"http://127.0.0.1:{port}","startedAt":"x"}}"#);
            let _ = std::fs::write(home.join(dir).join("server-runtime.json"), state);
        };
        // A dead release server falls through to a live dev one.
        write("userdata", i32::MAX as u32, 1);
        write("dev", std::process::id(), 2);
        assert_eq!(find_running(&home).map(|state| state.port), Some(2));
        write("userdata", std::process::id(), 3);
        assert_eq!(find_running(&home).map(|state| state.port), Some(3));
        let _ = std::fs::remove_dir_all(&home);
    }
}
