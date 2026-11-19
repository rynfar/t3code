//! Orchestration protocol negotiation (`client-runtime/src/connection/compatibility.ts`) and
//! client/server version skew (`apps/web/src/versionSkew.ts`).

use std::cmp::Ordering;

use crate::server_config::EnvironmentDescriptor;

/// `ORCHESTRATION_PROTOCOL_VERSION` in `packages/contracts/src/environment.ts`.
pub const ORCHESTRATION_PROTOCOL_VERSION: u32 = 2;
/// `ORCHESTRATION_PROTOCOL_QUERY_PARAM` in `packages/contracts/src/environment.ts`.
pub const ORCHESTRATION_PROTOCOL_QUERY_PARAM: &str = "orchestrationProtocol";
/// `CLIENT_CAPABILITIES_QUERY_PARAM` in `packages/contracts/src/environment.ts`.
pub const CLIENT_CAPABILITIES_QUERY_PARAM: &str = "clientCapabilities";
/// `SETTINGS_SECRET_REFERENCES_CAPABILITY` in `packages/contracts/src/environment.ts`.
pub const SETTINGS_SECRET_REFERENCES_CAPABILITY: &str = "settingsSecretReferences";

/// `appendOrchestrationProtocol` (`packages/client-runtime/src/connection/compatibility.ts`):
/// sets `orchestrationProtocol=2` and `clientCapabilities=settingsSecretReferences` on a socket URL.
pub fn append_orchestration_protocol(socket_url: &str) -> Result<String, url::ParseError> {
    append_orchestration_protocol_with_capabilities(socket_url, &[SETTINGS_SECRET_REFERENCES_CAPABILITY])
}

/// Sets `orchestrationProtocol=2` and comma-separated `clientCapabilities` (omitted when empty)
/// on `socket_url`, replacing any prior values of those query parameters.
pub fn append_orchestration_protocol_with_capabilities(
    socket_url: &str,
    capabilities: &[&str],
) -> Result<String, url::ParseError> {
    let mut url = url::Url::parse(socket_url)?;
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k != ORCHESTRATION_PROTOCOL_QUERY_PARAM && k != CLIENT_CAPABILITIES_QUERY_PARAM)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        for (k, v) in retained {
            pairs.append_pair(&k, &v);
        }
        pairs.append_pair(ORCHESTRATION_PROTOCOL_QUERY_PARAM, &ORCHESTRATION_PROTOCOL_VERSION.to_string());
        let joined: Vec<&str> = capabilities.iter().copied().filter(|cap| !cap.is_empty()).collect();
        if !joined.is_empty() {
            pairs.append_pair(CLIENT_CAPABILITIES_QUERY_PARAM, &joined.join(","));
        }
    }
    Ok(url.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolMismatch {
    /// The server speaks an older protocol: updating it fixes the connection.
    ServerOutdated,
    /// The server speaks a newer protocol than this app.
    ClientOutdated,
}

impl ProtocolMismatch {
    /// The `ConnectionBlockedError` detail the web shows.
    pub fn message(self, label: &str) -> String {
        let label = if label.trim().is_empty() { "this environment" } else { label };
        match self {
            ProtocolMismatch::ServerOutdated => format!("This client requires a newer server. Update T3 Code on {label} to connect."),
            ProtocolMismatch::ClientOutdated => {
                format!("This client is not supported by this server. Update your app or use a compatible release to connect to {label}.")
            }
        }
    }
}

/// `orchestrationProtocolCompatibilityError`. Servers shipped before negotiation speak protocol 1.
pub fn protocol_mismatch(descriptor: &EnvironmentDescriptor) -> Option<ProtocolMismatch> {
    mismatch_with(descriptor.orchestration_protocol_version.unwrap_or(1))
}

/// How a server speaking `server` relates to this client's protocol.
pub fn mismatch_with(server: u32) -> Option<ProtocolMismatch> {
    match server.cmp(&ORCHESTRATION_PROTOCOL_VERSION) {
        Ordering::Equal => None,
        Ordering::Less => Some(ProtocolMismatch::ServerOutdated),
        Ordering::Greater => Some(ProtocolMismatch::ClientOutdated),
    }
}

/// `VersionMismatch` (`apps/web/src/versionSkew.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMismatch {
    pub client_version: String,
    pub server_version: String,
    pub hint: &'static str,
}

const VERSION_MISMATCH_HINT: &str = "Version mismatch. Try syncing the client and server to the same T3 Code version.";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSemver<'a> {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Vec<&'a str>,
}

fn parse_semver(input: &str) -> Option<ParsedSemver<'_>> {
    let trimmed = input.trim().strip_prefix('v').unwrap_or(input.trim());
    let without_build = trimmed.split('+').next().unwrap_or(trimmed);
    let (core, pre) = match without_build.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (without_build, None),
    };
    let mut nums = core.split('.');
    let major = nums.next()?.parse::<u64>().ok()?;
    let minor = nums.next()?.parse::<u64>().ok()?;
    let patch = nums.next()?.parse::<u64>().ok()?;
    if nums.next().is_some() {
        return None;
    }
    let prerelease = match pre {
        Some(pre) if !pre.is_empty() => pre.split('.').collect(),
        Some(_) => return None,
        None => Vec::new(),
    };
    Some(ParsedSemver { major, minor, patch, prerelease })
}

fn compare_semver(left: &ParsedSemver<'_>, right: &ParsedSemver<'_>) -> Ordering {
    (left.major, left.minor, left.patch)
        .cmp(&(right.major, right.minor, right.patch))
        .then_with(|| match (left.prerelease.is_empty(), right.prerelease.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in left.prerelease.iter().zip(&right.prerelease) {
                    let ord = match (a.parse::<u64>(), b.parse::<u64>()) {
                        (Ok(an), Ok(bn)) => an.cmp(&bn),
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => a.cmp(b),
                    };
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                left.prerelease.len().cmp(&right.prerelease.len())
            }
        })
}

fn version_core(version: &str) -> &str {
    version.split(['-', '+']).next().unwrap_or(version)
}

/// `resolveVersionMismatch` (`apps/web/src/versionSkew.ts`): reports a mismatch only when the
/// server is behind the client (comparing nightly builds against nightly builds, else core semver).
pub fn resolve_version_mismatch(server_version: Option<&str>, client_version: &str) -> Option<VersionMismatch> {
    let client = client_version.trim();
    let server = server_version?.trim();
    if client.is_empty() || server.is_empty() {
        return None;
    }
    let client_parsed = parse_semver(client);
    let server_parsed = parse_semver(server);
    let compare_nightly = client_parsed.as_ref().and_then(|v| v.prerelease.first().copied()) == Some("nightly")
        && server_parsed.as_ref().and_then(|v| v.prerelease.first().copied()) == Some("nightly");
    let server_behind = match (parse_semver(version_core(client)), parse_semver(version_core(server))) {
        (Some(cc), Some(sc)) => {
            if compare_nightly {
                compare_semver(server_parsed.as_ref()?, client_parsed.as_ref()?) == Ordering::Less
            } else {
                compare_semver(&sc, &cc) == Ordering::Less
            }
        }
        _ => server != client,
    };
    server_behind.then(|| VersionMismatch {
        client_version: client.to_owned(),
        server_version: server.to_owned(),
        hint: VERSION_MISMATCH_HINT,
    })
}

/// `buildVersionMismatchDismissalKey` (`apps/web/src/versionSkew.ts`).
pub fn version_mismatch_dismissal_key(environment_id: &str, mismatch: &VersionMismatch) -> String {
    format!("{environment_id}:{}:{}", mismatch.client_version, mismatch.server_version)
}

/// `ServerInstallation` subset for `manualServerUpdateCommand`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerInstallationKind {
    NpmGlobal { prefix: String },
    T3Launcher { launcher: String, base_dir: String },
    PnpmDlx,
    Bunx,
    Npx,
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// `manualServerUpdateCommand` (`apps/web/src/versionSkew.ts`).
pub fn manual_server_update_command(target_version: &str, installation: Option<&ServerInstallationKind>) -> String {
    match installation {
        Some(ServerInstallationKind::NpmGlobal { prefix }) => {
            format!("npm install --global --prefix {} t3@{target_version}", shell_quote(prefix))
        }
        Some(ServerInstallationKind::T3Launcher { launcher, base_dir }) => {
            format!("{} update --base-dir {} {target_version}", shell_quote(launcher), shell_quote(base_dir))
        }
        Some(ServerInstallationKind::PnpmDlx) => format!("pnpm dlx t3@{target_version}"),
        Some(ServerInstallationKind::Bunx) => format!("bunx t3@{target_version}"),
        Some(ServerInstallationKind::Npx) | None => format!("npx t3@{target_version}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(version: Option<u32>) -> EnvironmentDescriptor {
        EnvironmentDescriptor { label: "Studio".into(), orchestration_protocol_version: version, ..Default::default() }
    }

    #[test]
    fn only_the_same_protocol_is_compatible_and_a_missing_version_is_protocol_one() {
        assert_eq!(protocol_mismatch(&descriptor(Some(ORCHESTRATION_PROTOCOL_VERSION))), None);
        assert_eq!(protocol_mismatch(&descriptor(None)), Some(ProtocolMismatch::ServerOutdated));
        assert_eq!(protocol_mismatch(&descriptor(Some(ORCHESTRATION_PROTOCOL_VERSION + 1))), Some(ProtocolMismatch::ClientOutdated));
        assert_eq!(ProtocolMismatch::ServerOutdated.message("Studio"), "This client requires a newer server. Update T3 Code on Studio to connect.");
    }

    #[test]
    fn version_skew_reports_server_behind_including_nightly_builds() {
        assert_eq!(resolve_version_mismatch(Some("0.1.7"), "0.1.7"), None);
        assert_eq!(resolve_version_mismatch(Some("0.1.8"), "0.1.7"), None);
        let mismatch = resolve_version_mismatch(Some("0.1.6"), "0.1.7").expect("server behind");
        assert_eq!(mismatch.server_version, "0.1.6");
        assert_eq!(version_mismatch_dismissal_key("env-1", &mismatch), "env-1:0.1.7:0.1.6");

        let nightly_behind = resolve_version_mismatch(
            Some("0.1.7-nightly.20261101.4000"),
            "0.1.7-nightly.20261102.4061",
        );
        assert!(nightly_behind.is_some());
        assert_eq!(
            resolve_version_mismatch(
                Some("0.1.7-nightly.20261103.4100"),
                "0.1.7-nightly.20261102.4061",
            ),
            None
        );
    }

    #[test]
    fn manual_update_command_quotes_installation_paths() {
        assert_eq!(
            manual_server_update_command(
                "0.1.8",
                Some(&ServerInstallationKind::T3Launcher {
                    launcher: "/Users/me/.t3/bin/t3".into(),
                    base_dir: "/Users/me/.t3".into(),
                })
            ),
            "'/Users/me/.t3/bin/t3' update --base-dir '/Users/me/.t3' 0.1.8"
        );
        assert_eq!(manual_server_update_command("0.1.8", None), "npx t3@0.1.8");
    }

    #[test]
    fn append_orchestration_protocol_negotiates_settings_secret_references_capability() {
        let negotiated = append_orchestration_protocol("ws://127.0.0.1:3777/ws?wsTicket=abc&orchestrationProtocol=1").unwrap();
        let parsed = url::Url::parse(&negotiated).unwrap();
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(params.get("wsTicket").map(String::as_str), Some("abc"));
        assert_eq!(params.get(ORCHESTRATION_PROTOCOL_QUERY_PARAM).map(String::as_str), Some("2"));
        assert_eq!(
            params.get(CLIENT_CAPABILITIES_QUERY_PARAM).map(String::as_str),
            Some(SETTINGS_SECRET_REFERENCES_CAPABILITY)
        );
        // No duplicate orchestrationProtocol params when replacing an older value.
        assert_eq!(parsed.query_pairs().filter(|(k, _)| k == ORCHESTRATION_PROTOCOL_QUERY_PARAM).count(), 1);

        let omitted = append_orchestration_protocol_with_capabilities("ws://127.0.0.1:3777/ws", &[]).unwrap();
        let omitted_url = url::Url::parse(&omitted).unwrap();
        assert!(omitted_url.query_pairs().all(|(k, _)| k != CLIENT_CAPABILITIES_QUERY_PARAM));
    }
}
