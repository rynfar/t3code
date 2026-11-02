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

/// `T3CODE_HOME`, else `~/.t3` (`resolveBaseDir`), expanding a leading `~`.
pub fn t3_home(configured: Option<&str>, user_home: &Path) -> PathBuf {
    match configured.map(str::trim).filter(|value| !value.is_empty()) {
        Some("~") => user_home.to_path_buf(),
        Some(value) => value.strip_prefix("~/").map_or_else(|| PathBuf::from(value), |rest| user_home.join(rest)),
        None => user_home.join(".t3"),
    }
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
        let user = Path::new("/Users/me");
        assert_eq!(t3_home(None, user), Path::new("/Users/me/.t3"));
        assert_eq!(t3_home(Some("  "), user), Path::new("/Users/me/.t3"));
        assert_eq!(t3_home(Some("~/alt"), user), Path::new("/Users/me/alt"));
        assert_eq!(t3_home(Some("/srv/t3"), user), Path::new("/srv/t3"));
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
}
