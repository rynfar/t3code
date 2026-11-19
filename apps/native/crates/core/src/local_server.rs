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
}
