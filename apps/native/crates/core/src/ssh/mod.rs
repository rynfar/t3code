//! SSH environments, the pure half: targets, `ssh` arguments, what the remote scripts print, and
//! how failures read. Ports `packages/ssh/src/command.ts` (`parseSshResolveOutput`,
//! `targetConnectionKey`, `remoteStateKey`, `baseSshArgs`), `tunnel.ts` (launch and pairing
//! output, tunnel arguments), `auth.ts` (askpass environment, auth failure classes), and the
//! SSH helpers of `apps/web/src/components/settings/ConnectionsSettings.logic.ts`,
//! `EnvironmentRow.tsx` (`formatDesktopSshTarget`) and `state/desktopSshHosts.ts`.
//! The processes run in `t3_client::ssh`.

pub mod config;
pub mod scripts;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `DesktopSshEnvironmentTarget`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshTarget {
    pub alias: String,
    pub hostname: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSource {
    SshConfig,
    KnownHosts,
}

/// `DesktopDiscoveredSshHost`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSshHost {
    pub alias: String,
    pub hostname: String,
    pub username: Option<String>,
    pub port: Option<u16>,
    pub source: HostSource,
}

impl DiscoveredSshHost {
    pub fn target(&self) -> SshTarget {
        SshTarget { alias: self.alias.clone(), hostname: self.hostname.clone(), username: self.username.clone(), port: self.port }
    }
}

/// The default `ssh` command budget.
pub const COMMAND_TIMEOUT_MS: u64 = 60_000;
/// Cold installs download and extract the release archive, possibly behind another install.
pub const ARCHIVE_LAUNCH_TIMEOUT_MS: u64 = 900_000;
/// How long a new tunnel has to answer HTTP.
pub const TUNNEL_READY_TIMEOUT_MS: u64 = 20_000;
pub const READY_PROBE_TIMEOUT_MS: u64 = 1_000;
/// A reused tunnel must answer this quickly, or it is replaced.
pub const TUNNEL_REUSE_TIMEOUT_MS: u64 = 2_000;
pub const TUNNEL_SHUTDOWN_TIMEOUT_MS: u64 = 2_000;
pub const TUNNEL_STDERR_TAIL_CHARS: usize = 16_384;
const MAX_ERROR_OUTPUT_LENGTH: usize = 4_000;
/// `DEFAULT_SSH_PASSWORD_PROMPT_TIMEOUT_MS`.
pub const PASSWORD_PROMPT_TIMEOUT_MS: u64 = 3 * 60 * 1000;
/// Prompts for one operation before its auth failure stands.
pub const MAX_PASSWORD_PROMPTS: u32 = 2;

/// `parseSshResolveOutput`: the first `hostname`, `user` and `port` lines of `ssh -G`.
pub fn parse_ssh_resolve_output(alias: &str, stdout: &str) -> SshTarget {
    let mut values: Vec<(&str, String)> = Vec::new();
    for line in stdout.lines() {
        let mut words = line.split_whitespace();
        let Some(key) = words.next() else { continue };
        let rest: Vec<&str> = words.collect();
        if rest.is_empty() || values.iter().any(|(existing, _)| *existing == key) {
            continue;
        }
        values.push((key, rest.join(" ")));
    }
    let value = |key: &str| values.iter().find(|(existing, _)| *existing == key).map(|(_, value)| value.trim().to_owned()).filter(|value| !value.is_empty());
    SshTarget {
        alias: alias.to_owned(),
        hostname: value("hostname").unwrap_or_else(|| alias.to_owned()),
        username: value("user"),
        port: value("port").and_then(|port| port.parse().ok()),
    }
}

/// `targetConnectionKey`: one tunnel per distinct target.
pub fn target_connection_key(target: &SshTarget) -> String {
    let port = target.port.map(|port| port.to_string()).unwrap_or_default();
    format!("{}\u{0}{}\u{0}{}\u{0}{port}", target.alias, target.hostname, target.username.as_deref().unwrap_or_default())
}

/// `remoteStateKey`: the first 16 hex digits of the key's SHA-256.
pub fn remote_state_key(target: &SshTarget) -> String {
    let digest = Sha256::digest(target_connection_key(target).as_bytes());
    digest.iter().take(8).map(|byte| format!("{byte:02x}")).collect()
}

/// The directory under `~/.t3/ssh-launch` this app keeps its server's state in. It is not the
/// desktop's: their runner scripts differ, and on a shared key each app would replace the other's
/// server on every reconnect. Either app adopts a server the other started through the default
/// runtime file instead.
pub fn native_state_key(target: &SshTarget) -> String {
    format!("native-{}", remote_state_key(target))
}

/// `buildSshHostSpec`: `[user@]alias`.
pub fn host_spec(target: &SshTarget) -> Result<String, String> {
    let destination = Some(target.alias.trim()).filter(|alias| !alias.is_empty()).unwrap_or(target.hostname.trim());
    if destination.is_empty() {
        return Err("SSH target is missing its alias/hostname.".to_owned());
    }
    Ok(match &target.username {
        Some(username) if !username.is_empty() => format!("{username}@{destination}"),
        _ => destination.to_owned(),
    })
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// `baseSshArgs`. A target `ssh -G` resolved to another host name keeps reaching that host
/// (`-o HostName=`) while the app runs, even if the SSH config changes meanwhile; the desktop
/// resolves the alias again for each command.
pub fn base_ssh_args(target: &SshTarget, batch_mode: bool) -> Vec<String> {
    let mut args = strings(&["-o", if batch_mode { "BatchMode=yes" } else { "BatchMode=no" }, "-o", "ConnectTimeout=10"]);
    let hostname = target.hostname.trim();
    if !hostname.is_empty() && hostname != target.alias.trim() {
        args.extend(["-o".to_owned(), format!("HostName={hostname}")]);
    }
    if let Some(port) = target.port {
        args.extend(["-p".to_owned(), port.to_string()]);
    }
    args
}

/// `runSshCommand`'s arguments. The host spec follows `--`, so an alias shaped like an option
/// stays a destination.
pub fn command_args(target: &SshTarget, batch_mode: bool, pre_host: &[&str], remote: &[&str]) -> Result<Vec<String>, String> {
    let host = host_spec(target)?;
    let mut args = base_ssh_args(target, batch_mode);
    // Cold installs need a long command budget; a lost connection must fail sooner.
    args.extend(strings(&["-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"]));
    args.extend(strings(pre_host));
    args.extend(["--".to_owned(), host]);
    args.extend(strings(remote));
    Ok(args)
}

/// `startSshTunnel`'s arguments: forward `local_port` to the remote server's loopback port.
pub fn tunnel_args(target: &SshTarget, batch_mode: bool, local_port: u16, remote_port: u16) -> Result<Vec<String>, String> {
    let host = host_spec(target)?;
    let mut args = base_ssh_args(target, batch_mode);
    args.extend(strings(&[
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        "-o",
        "ControlPersist=no",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-n",
        "-N",
        "-L",
    ]));
    args.extend([format!("{local_port}:127.0.0.1:{remote_port}"), "--".to_owned(), host]);
    Ok(args)
}

/// The tunnel's local port is taken (`ExitOnForwardFailure`): another port may work.
pub fn is_forward_port_taken(message: &str) -> bool {
    message.contains("Address already in use") || message.contains("cannot listen to port") || message.contains("Could not request local forwarding")
}

/// `getLastNonEmptyOutputLine`.
pub fn last_non_empty_line(stdout: &str) -> Option<&str> {
    stdout.lines().map(str::trim).rfind(|line| !line.is_empty())
}

/// `redactSshErrorOutput`: credentials masked, then the head (or the tail, where a failure's
/// final cause usually is) kept.
pub fn redact_error_output(output: &str, keep_tail: bool) -> String {
    let redacted = redact_credentials(output);
    let chars: Vec<char> = redacted.chars().collect();
    if chars.len() <= MAX_ERROR_OUTPUT_LENGTH {
        return redacted;
    }
    if keep_tail {
        format!("[truncated]\n{}", chars[chars.len() - MAX_ERROR_OUTPUT_LENGTH..].iter().collect::<String>())
    } else {
        format!("{}\n[truncated]", chars[..MAX_ERROR_OUTPUT_LENGTH].iter().collect::<String>())
    }
}

/// Masks `"credential": "…"` and the other token fields, case-insensitively.
fn redact_credentials(output: &str) -> String {
    const FIELDS: [&str; 5] = ["access_token", "bearertoken", "credential", "pairingtoken", "token"];
    let lower = output.to_ascii_lowercase();
    let mut result = String::with_capacity(output.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(found) = lower[search..].find('"').map(|offset| search + offset) {
        search = found + 1;
        let Some(field) = FIELDS.iter().find(|field| lower[found + 1..].starts_with(&format!("{field}\""))) else { continue };
        let mut cursor = found + field.len() + 2;
        cursor += output[cursor..].len() - output[cursor..].trim_start().len();
        if !output[cursor..].starts_with(':') {
            continue;
        }
        cursor += 1;
        cursor += output[cursor..].len() - output[cursor..].trim_start().len();
        if !output[cursor..].starts_with('"') {
            continue;
        }
        let value_start = cursor + 1;
        let Some(value_len) = output[value_start..].find('"').filter(|len| *len > 0) else { continue };
        result.push_str(&output[copied..value_start]);
        result.push_str("[redacted]");
        copied = value_start + value_len;
        search = copied + 1;
    }
    result.push_str(&output[copied..]);
    result
}

/// `normalizeSshErrorMessage`: stderr, else stdout, else the fallback.
pub fn error_message(stdout: &str, stderr: &str, fallback: &str) -> String {
    [stderr.trim(), stdout.trim()].into_iter().find(|text| !text.is_empty()).unwrap_or(fallback).to_owned()
}

/// `extractJsonObject`: the first balanced `{…}` in output that a login shell may have prefixed
/// with noise.
pub fn extract_json_object(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(start) = trimmed.find('{') else { return trimmed };
    let (mut depth, mut in_string, mut escaping) = (0i32, false, false);
    for (offset, c) in trimmed[start..].char_indices() {
        if in_string {
            match c {
                _ if escaping => escaping = false,
                '\\' => escaping = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &trimmed[start..start + offset + 1];
                }
            }
            _ => {}
        }
    }
    &trimmed[start..]
}

/// `decodeRemoteJsonOutput`: the whole output as JSON, else the object inside it.
fn decode_remote_json<T: serde::de::DeserializeOwned>(stdout: &str) -> Option<T> {
    serde_json::from_str(stdout.trim()).ok().or_else(|| serde_json::from_str(extract_json_object(stdout)).ok())
}

/// Whether the remote server is one the launch started or one it adopted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerKind {
    External,
    Managed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchResult {
    pub remote_port: u16,
    pub server_kind: Option<ServerKind>,
}

/// The launch script's `{"remotePort":…,"serverKind":…}`, after any login-shell noise.
pub fn parse_launch_output(stdout: &str) -> Result<LaunchResult, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Raw {
        remote_port: serde_json::Number,
        #[serde(default)]
        server_kind: Option<ServerKind>,
    }
    if last_non_empty_line(stdout).is_none() {
        return Err("SSH launch did not return a remote port.".to_owned());
    }
    let raw: Raw = decode_remote_json(stdout).ok_or_else(|| "SSH launch returned unparseable output.".to_owned())?;
    let remote_port = raw.remote_port.as_u64().and_then(|port| u16::try_from(port).ok());
    let remote_port = remote_port.ok_or_else(|| format!("SSH launch returned an invalid remote port: {}.", raw.remote_port))?;
    Ok(LaunchResult { remote_port, server_kind: raw.server_kind })
}

/// `t3 auth pairing create --json`'s credential.
pub fn parse_pairing_output(stdout: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Raw {
        credential: String,
    }
    if last_non_empty_line(stdout).is_none() {
        return Err("SSH pairing did not return a credential.".to_owned());
    }
    let raw: Raw = decode_remote_json(stdout).ok_or_else(|| "SSH pairing returned unparseable output.".to_owned())?;
    if raw.credential.trim().is_empty() {
        return Err("SSH pairing command returned an invalid credential.".to_owned());
    }
    Ok(raw.credential)
}

/// `isSshAuthFailure`: a password or code could get past this.
pub fn is_auth_failure(message: &str) -> bool {
    const METHODS: [&str; 5] = ["publickey", "password", "keyboard-interactive", "hostbased", "gssapi-with-mic"];
    let lower = message.to_lowercase();
    let denied = lower.match_indices("permission denied (").any(|(index, needle)| {
        let rest = &lower[index + needle.len()..];
        METHODS.iter().any(|method| rest.strip_prefix(method).is_some_and(|after| after.contains(')')))
    });
    denied || lower.contains("authentication failed") || lower.contains("too many authentication failures")
}

/// `isSshAgentSigningFailure`: the local agent could not sign an accepted key, which the remote
/// account's password cannot fix.
pub fn is_agent_signing_failure(message: &str) -> bool {
    const NEEDLE: &str = "sign_and_send_pubkey: signing failed";
    message.match_indices(NEEDLE).any(|(index, _)| {
        let rest = &message[index + NEEDLE.len()..];
        rest.starts_with(": agent refused operation")
            || rest.starts_with(": communication with agent failed")
            || rest.strip_prefix(" for ").is_some_and(|line| line.split('\n').next().unwrap_or_default().contains(" from agent:"))
    })
}

/// The askpass helper (`ASKPASS_POSIX_SCRIPT`): it prints the in-app prompt's answer that the app
/// passes in `T3_SSH_AUTH_SECRET`, and never opens a dialog of its own.
pub const ASKPASS_SCRIPT: &str = r#"#!/bin/sh
# Invoked by ssh via SSH_ASKPASS when T3 Code re-runs ssh with a cached password
# from the renderer's in-app prompt. We never expose a native dialog here - if
# T3_SSH_AUTH_SECRET is missing, that's a caller bug and we fail loudly.
if [ "${T3_SSH_AUTH_SECRET+x}" = "x" ]; then
  printf "%s\n" "$T3_SSH_AUTH_SECRET"
  exit 0
fi
printf 'T3 Code ssh-askpass invoked without T3_SSH_AUTH_SECRET.\n' >&2
exit 1
"#;

/// `buildSshChildEnvironment` for interactive auth: route password prompts to the askpass helper.
pub fn askpass_environment(askpass: &str, secret: Option<&str>, has_display: bool) -> Vec<(String, String)> {
    let mut env = vec![("SSH_ASKPASS".to_owned(), askpass.to_owned()), ("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned())];
    if let Some(secret) = secret {
        env.push(("T3_SSH_AUTH_SECRET".to_owned(), secret.to_owned()));
    }
    if !has_display {
        env.push(("DISPLAY".to_owned(), "t3code".to_owned()));
    }
    env
}

/// The in-app prompt's question for a target.
pub fn password_prompt_text(target: &SshTarget) -> Result<String, String> {
    Ok(format!("Enter the SSH password or verification code for {}.", host_spec(target)?))
}

/// `describeSshTarget` in `SshPasswordPromptDialog.tsx`.
pub fn describe_prompt_target(destination: &str, username: Option<&str>) -> String {
    match username.filter(|username| !username.is_empty()) {
        Some(username) => format!("{username}@{destination}"),
        None => destination.to_owned(),
    }
}

/// `formatRemainingSeconds`: `m:ss`.
pub fn format_remaining_seconds(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// `formatDesktopSshTarget`: `[user@]hostname[:port]`.
pub fn format_target(target: &SshTarget) -> String {
    let authority = match &target.username {
        Some(username) if !username.is_empty() => format!("{username}@{}", target.hostname),
        _ => target.hostname.clone(),
    };
    match target.port {
        Some(port) => format!("{authority}:{port}"),
        None => authority,
    }
}

/// `parseManualDesktopSshTarget`: the Add environment fields, accepting `user@host:port` and
/// `[v6]:port` in the host field. The username and port fields win over the inline ones.
pub fn parse_manual_target(host: &str, username: &str, port: &str) -> Result<SshTarget, String> {
    const REQUIRED: &str = "SSH host or alias is required.";
    let raw_host = host.trim();
    if raw_host.is_empty() {
        return Err(REQUIRED.to_owned());
    }
    let mut hostname = raw_host.to_owned();
    let mut username = Some(username.trim().to_owned()).filter(|username| !username.is_empty());
    let mut parsed_port: Option<i64> = None;
    if let Some(at) = hostname.rfind('@').filter(|at| *at > 0) {
        let inline = hostname[..at].trim().to_owned();
        hostname = hostname[at + 1..].trim().to_owned();
        if username.is_none() && !inline.is_empty() {
            username = Some(inline);
        }
    }
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let bracketed = hostname.strip_prefix('[').and_then(|rest| {
        let (inner, after) = rest.split_once(']')?;
        let port = match after {
            "" => None,
            _ => Some(after.strip_prefix(':').filter(|port| digits(port))?),
        };
        (!inner.is_empty()).then(|| (inner.trim().to_owned(), port.map(str::to_owned)))
    });
    if let Some((inner, port)) = bracketed {
        hostname = inner;
        parsed_port = port.and_then(|port| port.parse().ok());
    } else if let [name, port] = hostname.split(':').collect::<Vec<_>>()[..]
        && digits(port)
    {
        parsed_port = port.parse().ok();
        hostname = name.trim().to_owned();
    }
    let port_field = port.trim();
    if !port_field.is_empty() {
        // `Number.parseInt`: leading digits; anything else is not a port.
        let leading: String = port_field.chars().take_while(char::is_ascii_digit).collect();
        parsed_port = Some(leading.parse().unwrap_or(-1));
    }
    if hostname.is_empty() {
        return Err(REQUIRED.to_owned());
    }
    let port = match parsed_port {
        None => None,
        Some(port) => Some(u16::try_from(port).ok().filter(|port| *port > 0).ok_or_else(|| "SSH port must be between 1 and 65535.".to_owned())?),
    };
    Ok(SshTarget { alias: hostname.clone(), hostname, username, port })
}

/// `filterDiscoveredSshHosts`: alias prefix matches first, then alias or hostname substrings.
pub fn filter_hosts<'a>(hosts: &'a [DiscoveredSshHost], query: &str) -> Vec<&'a DiscoveredSshHost> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return hosts.iter().collect();
    }
    let (mut prefix, mut substring) = (Vec::new(), Vec::new());
    for host in hosts {
        let alias = host.alias.to_lowercase();
        if alias.starts_with(&query) {
            prefix.push(host);
        } else if alias.contains(&query) || host.hostname.to_lowercase().contains(&query) {
            substring.push(host);
        }
    }
    prefix.extend(substring);
    prefix
}

/// The discovered hosts the Add environment list offers: those no saved SSH environment already
/// reaches, by alias, or by address for a `known_hosts` entry.
pub fn unsaved_hosts(hosts: &[DiscoveredSshHost], saved: &[SshTarget]) -> Vec<DiscoveredSshHost> {
    let saved_keys: std::collections::HashSet<String> = saved.iter().flat_map(|target| [target.alias.clone(), format_target(target)]).collect();
    hosts
        .iter()
        .filter(|host| !saved_keys.contains(&host.alias) && (host.source == HostSource::SshConfig || !saved_keys.contains(&format_target(&host.target()))))
        .cloned()
        .collect()
}

/// `EXACT_ARCHIVE_VERSION`: one exact SemVer segment, since the version names a directory the
/// runner removes and recreates.
pub fn is_exact_archive_version(version: &str) -> bool {
    let (core, prerelease) = match version.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (version, None),
    };
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) && (part == "0" || !part.starts_with('0'));
    let parts: Vec<&str> = core.split('.').collect();
    let identifiers_ok = prerelease.is_none_or(|prerelease| {
        prerelease.split('.').all(|identifier| !identifier.is_empty() && identifier.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    });
    parts.len() == 3 && parts.iter().all(|part| numeric(part)) && identifiers_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(alias: &str, hostname: &str, username: Option<&str>, port: Option<u16>) -> SshTarget {
        SshTarget { alias: alias.into(), hostname: hostname.into(), username: username.map(Into::into), port }
    }

    #[test]
    fn parses_resolved_ssh_config_output_into_a_target() {
        assert_eq!(
            parse_ssh_resolve_output("devbox", "hostname devbox.example.com\nuser julius\nport 2222\n"),
            target("devbox", "devbox.example.com", Some("julius"), Some(2222))
        );
        // The first value of a key wins, and missing values fall back to the alias.
        assert_eq!(parse_ssh_resolve_output("box", "user a\nuser b\nport x\n"), target("box", "box", Some("a"), None));
    }

    // Remote servers store state under this key, so it must not change across releases.
    #[test]
    fn derives_a_stable_remote_state_key() {
        assert_eq!(remote_state_key(&target("devbox", "devbox.example.com", Some("julius"), Some(2222))), "711bc738002d72fd");
        assert_eq!(remote_state_key(&target("fixture", "fixture", None, None)), "326264c4f08c8a0c");
        assert_eq!(native_state_key(&target("fixture", "fixture", None, None)), "native-326264c4f08c8a0c");
    }

    #[test]
    fn passes_an_option_shaped_alias_to_ssh_as_the_destination() {
        let args = command_args(&target("-oProxyCommand=false", "devbox", None, None), true, &[], &["sh", "-s"]).unwrap();
        assert_eq!(&args[args.len() - 4..], ["--", "-oProxyCommand=false", "sh", "-s"]);
        assert_eq!(&args[..4], ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
        assert!(args.windows(2).any(|pair| pair == ["-o", "ServerAliveInterval=15"]));
        assert!(host_spec(&target(" ", " ", None, None)).is_err());
    }

    #[test]
    fn tunnels_forward_the_local_port_to_the_remote_loopback() {
        let args = tunnel_args(&target("devbox", "devbox.example.com", Some("julius"), Some(2222)), false, 41773, 3774).unwrap();
        assert_eq!(&args[..8], ["-o", "BatchMode=no", "-o", "ConnectTimeout=10", "-o", "HostName=devbox.example.com", "-p", "2222"]);
        assert_eq!(&args[args.len() - 5..], ["-N", "-L", "41773:127.0.0.1:3774", "--", "julius@devbox"]);
        assert!(args.windows(2).any(|pair| pair == ["-o", "ExitOnForwardFailure=yes"]));
        // An alias `ssh -G` could not resolve further is left to ssh.
        assert!(!base_ssh_args(&target("devbox", "devbox", None, None), true).iter().any(|arg| arg.starts_with("HostName=")));
        assert!(is_forward_port_taken("bind [127.0.0.1]:41773: Address already in use\nchannel_setup_fwd_listener_tcpip: cannot listen to port: 41773"));
        assert!(!is_forward_port_taken("Permission denied (publickey)."));
    }

    #[test]
    fn redacts_credentials_and_keeps_the_head_or_the_tail() {
        assert_eq!(redact_error_output("{\"credential\":\"pairing-secret\"}\n", false), "{\"credential\":\"[redacted]\"}\n");
        assert_eq!(redact_error_output("{\"Token\" : \"x\", \"other\":\"y\"}", false), "{\"Token\" : \"[redacted]\", \"other\":\"y\"}");
        let prefix = format!("HEAD\n{}", "verbose-prefix ".repeat(400));
        let output = format!("{prefix}{{\"credential\":\"{}\"}}\nPANIC: emulator SDK is broken\n", "secret".repeat(400));
        let redacted = format!("{prefix}{{\"credential\":\"[redacted]\"}}\nPANIC: emulator SDK is broken\n");
        let tail: String = redacted.chars().skip(redacted.chars().count() - 4_000).collect();
        assert_eq!(redact_error_output(&output, true), format!("[truncated]\n{tail}"));
        assert_eq!(redact_error_output(&output, false), format!("{}\n[truncated]", &redacted[..4_000]));
        assert_eq!(error_message("out", " err \n", "fallback"), "err");
        assert_eq!(error_message(" ", "", "fallback"), "fallback");
    }

    #[test]
    fn accepts_launch_and_pairing_json_after_remote_shell_startup_noise() {
        assert_eq!(parse_launch_output("loaded nvm default\n{\"remotePort\":3774}\n"), Ok(LaunchResult { remote_port: 3774, server_kind: None }));
        assert_eq!(
            parse_launch_output("{\"remotePort\":3773,\"serverKind\":\"external\"}"),
            Ok(LaunchResult { remote_port: 3773, server_kind: Some(ServerKind::External) })
        );
        assert_eq!(parse_launch_output("\n\n"), Err("SSH launch did not return a remote port.".to_owned()));
        assert_eq!(parse_launch_output("nope"), Err("SSH launch returned unparseable output.".to_owned()));
        assert_eq!(parse_launch_output("{\"remotePort\":3.5}"), Err("SSH launch returned an invalid remote port: 3.5.".to_owned()));
        let pairing = "loaded nvm default\n{\n  \"id\": \"88941235\",\n  \"credential\": \"LCL4R2TPHDKQ\",\n  \"scopes\": [\"orchestration:read\"]\n}\n\n";
        assert_eq!(parse_pairing_output(pairing), Ok("LCL4R2TPHDKQ".to_owned()));
        assert_eq!(parse_pairing_output("{\"credential\":\" \"}"), Err("SSH pairing command returned an invalid credential.".to_owned()));
    }

    #[test]
    fn detects_ssh_auth_failures_from_common_permission_denied_messages() {
        assert!(is_auth_failure("julius@100.65.180.100: Permission denied (publickey,password,keyboard-interactive)."));
        assert!(is_auth_failure("Permission denied (publickey)."));
        assert!(!is_auth_failure("Connection timed out"));
        assert!(!is_auth_failure("mkdir: Permission denied"));
    }

    #[test]
    fn detects_agent_signing_failures_from_current_and_pre_8_2_openssh() {
        let denied = "julius@devbox: Permission denied (publickey,password).";
        assert!(is_agent_signing_failure(&format!(
            "sign_and_send_pubkey: signing failed for ED25519 \"/home/julius/.ssh/id_ed25519\" from agent: communication with agent failed\r\n{denied}"
        )));
        assert!(is_agent_signing_failure(&format!("sign_and_send_pubkey: signing failed: agent refused operation\n{denied}")));
        assert!(is_agent_signing_failure(&format!("sign_and_send_pubkey: signing failed: communication with agent failed\n{denied}")));
        assert!(!is_agent_signing_failure(&format!(
            "sign_and_send_pubkey: signing failed for ED25519-SK \"/home/julius/.ssh/id_ed25519_sk\": device not found\n{denied}"
        )));
        assert!(!is_agent_signing_failure(denied));
    }

    #[test]
    fn askpass_environment_forces_the_helper_and_passes_the_secret() {
        let env = askpass_environment("/tmp/x/ssh-askpass.sh", Some("super-secret"), false);
        let get = |key: &str| env.iter().find(|(name, _)| name == key).map(|(_, value)| value.as_str());
        assert_eq!((get("SSH_ASKPASS"), get("SSH_ASKPASS_REQUIRE")), (Some("/tmp/x/ssh-askpass.sh"), Some("force")));
        assert_eq!((get("T3_SSH_AUTH_SECRET"), get("DISPLAY")), (Some("super-secret"), Some("t3code")));
        assert_eq!(askpass_environment("a", None, true).len(), 2);
        assert!(ASKPASS_SCRIPT.contains(r#"printf "%s\n" "$T3_SSH_AUTH_SECRET""#));
    }

    #[test]
    fn manual_targets_read_like_the_web_fields() {
        assert_eq!(parse_manual_target(" julius@devbox:2222 ", "", ""), Ok(target("devbox", "devbox", Some("julius"), Some(2222))));
        assert_eq!(parse_manual_target("julius@devbox", "root", "22"), Ok(target("devbox", "devbox", Some("root"), Some(22))));
        assert_eq!(parse_manual_target("[::1]:2200", "", ""), Ok(target("::1", "::1", None, Some(2200))));
        assert_eq!(parse_manual_target("2001:db8::1", "", ""), Ok(target("2001:db8::1", "2001:db8::1", None, None)));
        assert_eq!(parse_manual_target("  ", "", ""), Err("SSH host or alias is required.".to_owned()));
        assert_eq!(parse_manual_target("devbox", "", "70000"), Err("SSH port must be between 1 and 65535.".to_owned()));
        assert_eq!(parse_manual_target("devbox", "", "abc"), Err("SSH port must be between 1 and 65535.".to_owned()));
        assert_eq!(format_target(&target("devbox", "devbox.example.com", Some("julius"), Some(2222))), "julius@devbox.example.com:2222");
        assert_eq!(format_target(&target("devbox", "devbox", None, None)), "devbox");
    }

    #[test]
    fn host_suggestions_put_alias_prefixes_first_and_hide_saved_hosts() {
        let host = |alias: &str, hostname: &str, source| DiscoveredSshHost { alias: alias.into(), hostname: hostname.into(), username: None, port: None, source };
        let hosts = [
            host("buildbox", "build.example.com", HostSource::SshConfig),
            host("devbox", "devbox.example.com", HostSource::SshConfig),
            host("staging", "dev-staging.example.com", HostSource::SshConfig),
            host("known.example.com", "known.example.com", HostSource::KnownHosts),
        ];
        let aliases = |hosts: Vec<&DiscoveredSshHost>| hosts.into_iter().map(|host| host.alias.clone()).collect::<Vec<_>>();
        assert_eq!(aliases(filter_hosts(&hosts, " DEV ")), ["devbox", "staging"]);
        assert_eq!(filter_hosts(&hosts, "").len(), 4);
        let saved = [target("devbox", "devbox.example.com", None, None), target("known.example.com", "known.example.com", None, None)];
        let unsaved = unsaved_hosts(&hosts, &saved);
        assert_eq!(unsaved.iter().map(|host| host.alias.as_str()).collect::<Vec<_>>(), ["buildbox", "staging"]);
    }

    #[test]
    fn rejects_archive_versions_that_are_not_a_single_exact_version_segment() {
        for version in ["../other", "1.2.3/evil", "1.2.3\\evil", "1.2.3-preview.1 x", "1.2.3-preview.1\nrm -rf /", "v1.2.3", "1.2", "01.2.3", "1.2.3-"] {
            assert!(!is_exact_archive_version(version), "{version}");
        }
        for version in ["1.2.3-preview.20260911.4", "0.1.7", "0.1.7-nightly.20261102.4061"] {
            assert!(is_exact_archive_version(version), "{version}");
        }
    }
}
