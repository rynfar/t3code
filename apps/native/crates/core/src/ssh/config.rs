//! SSH host discovery from `~/.ssh/config` (with `Include`) and `known_hosts`:
//! `packages/ssh/src/config.ts`. The files come through `ConfigFiles`, so the parsing stays pure.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use super::{DiscoveredSshHost, HostSource};
use crate::thread::locale_compare;

/// The file system discovery reads. The app reads the disk; tests pass a map.
pub trait ConfigFiles {
    fn exists(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    /// The file's text, when it can be read.
    fn read(&self, path: &Path) -> Option<String>;
    /// The entry names in a directory.
    fn list(&self, dir: &Path) -> Vec<String>;
}

/// `parseSshConfigLine`, mirroring OpenSSH readconf: the keyword may be followed by one optional
/// `=`, then the arguments split like `argv_split()`, with quotes, the escapes `\\ \" \'` and an
/// unquoted `\ `, and `#` starting a comment only at the start of a token. Invalid quoting leaves
/// the arguments uncertain (`None`).
fn parse_config_line(line: &str) -> Option<(String, Option<Vec<String>>)> {
    let line = line.trim_start();
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    if end == 0 {
        return None;
    }
    let directive = line[..end].to_lowercase();
    let mut rest = line[end..].trim_start();
    if let Some(after) = rest.strip_prefix('=') {
        rest = after.trim_start();
    }
    let chars: Vec<char> = rest.chars().collect();
    let mut args = Vec::new();
    let mut token: Option<String> = None;
    let mut quote: Option<char> = None;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        let next = chars.get(index + 1).copied();
        if c == '\\' && matches!(next, Some('"' | '\'' | '\\')) || (c == '\\' && quote.is_none() && next == Some(' ')) {
            token.get_or_insert_with(String::new).push(next.unwrap_or_default());
            index += 1;
        } else if let Some(open) = quote {
            if c == open {
                quote = None;
            } else {
                token.get_or_insert_with(String::new).push(c);
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            token.get_or_insert_with(String::new);
        } else if c.is_whitespace() {
            args.extend(token.take());
        } else if c == '#' && token.is_none() {
            break;
        } else {
            token.get_or_insert_with(String::new).push(c);
        }
        index += 1;
    }
    if quote.is_some() {
        return Some((directive, None));
    }
    args.extend(token);
    Some((directive, Some(args)))
}

/// `~` alone or before a separator is the home directory.
fn tilde_is_home(input: &str) -> bool {
    input.strip_prefix('~').is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\'))
}

fn expand_home(input: &str, home: &Path) -> String {
    if tilde_is_home(input) { format!("{}{}", home.display(), &input[1..]) } else { input.to_owned() }
}

/// `path.resolve`: absolute, with `.` and `..` folded away.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Relative includes resolve against `~/.ssh`, as OpenSSH does for the user's config.
fn resolve_include(pattern: &str, home: &Path) -> PathBuf {
    let expanded = PathBuf::from(expand_home(pattern, home));
    if expanded.is_absolute() { normalize(&expanded) } else { normalize(&home.join(".ssh").join(expanded)) }
}

pub(crate) fn has_ssh_pattern(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.starts_with('!')
}

/// `globToRegExp(pattern).test(text)`: `*` is any run, `?` any one character.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let (pattern, text): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut p, mut t, mut star, mut mark) = (0, 0, None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) && pattern[p] != '*' {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(star) = star {
            p = star + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

fn expand_glob(pattern: &Path, files: &impl ConfigFiles) -> Vec<PathBuf> {
    let text = pattern.to_string_lossy();
    if !text.contains('*') && !text.contains('?') {
        return if files.exists(pattern) { vec![pattern.to_path_buf()] } else { Vec::new() };
    }
    let (Some(directory), Some(base)) = (pattern.parent(), pattern.file_name()) else { return Vec::new() };
    if !files.exists(directory) {
        return Vec::new();
    }
    let base = base.to_string_lossy();
    let mut matched: Vec<PathBuf> =
        files.list(directory).into_iter().filter(|entry| glob_matches(&base, entry)).map(|entry| directory.join(entry)).filter(|path| files.exists(path)).collect();
    matched.sort_by_key(|path| path.to_string_lossy().into_owned());
    matched
}

/// The `Host` patterns a line sits under; `None` under `Match`, whose outcome is unknown.
type HostCondition = Option<Vec<String>>;

struct HostNameRule {
    conditions: Vec<HostCondition>,
    hostname: Option<String>,
}

fn matches_host_patterns(alias: &str, patterns: &[String]) -> bool {
    let mut matched = false;
    for pattern in patterns {
        let (negated, host_pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern.as_str()),
        };
        let matches = host_pattern == "*" || if has_ssh_pattern(host_pattern) { glob_matches(host_pattern, alias) } else { host_pattern == alias };
        if !matches {
            continue;
        }
        if negated {
            return false;
        }
        matched = true;
    }
    matched
}

/// The `HostName` OpenSSH would use for `alias`, when the config makes it certain.
fn resolve_configured_hostname(alias: &str, rules: &[HostNameRule]) -> Option<String> {
    for HostNameRule { conditions, hostname } in rules {
        if conditions.iter().any(|condition| condition.as_ref().is_some_and(|patterns| !matches_host_patterns(alias, patterns))) {
            continue;
        }
        // SSH takes the first value; an uncertain earlier value blocks later fallbacks.
        let hostname = hostname.as_ref().filter(|_| !conditions.iter().any(Option::is_none))?;
        let other_token = hostname.match_indices('%').any(|(index, _)| !hostname[index + 1..].starts_with('h'));
        if other_token || hostname.contains('$') {
            return None;
        }
        return Some(hostname.replace("%h", alias));
    }
    None
}

struct Collector<'a, F> {
    files: &'a F,
    home: &'a Path,
    visited: HashSet<PathBuf>,
    aliases: BTreeSet<String>,
    rules: Vec<HostNameRule>,
}

impl<F: ConfigFiles> Collector<'_, F> {
    fn collect(&mut self, file: &Path, inherited: &[HostCondition]) {
        let resolved = normalize(file);
        if self.visited.contains(&resolved) {
            // Read each file once, but never reuse its rules under a different Include context.
            self.rules.push(HostNameRule { conditions: inherited.to_vec(), hostname: None });
            return;
        }
        if !self.files.exists(&resolved) || self.files.is_dir(&resolved) {
            return;
        }
        self.visited.insert(resolved.clone());
        let Some(raw) = self.files.read(&resolved) else { return };
        let mut condition: HostCondition = Some(vec!["*".to_owned()]);
        for line in raw.split('\n') {
            let Some((directive, args)) = parse_config_line(line.strip_suffix('\r').unwrap_or(line)) else { continue };
            let conditions: Vec<HostCondition> = inherited.iter().cloned().chain(std::iter::once(condition.clone())).collect();
            match directive.as_str() {
                "host" => {
                    for alias in args.iter().flatten() {
                        if !alias.is_empty() && !has_ssh_pattern(alias) {
                            self.aliases.insert(alias.clone());
                        }
                    }
                    condition = args;
                }
                "match" => condition = None,
                "hostname" => {
                    let hostname = args.filter(|args| args.len() == 1).and_then(|args| args.into_iter().next());
                    self.rules.push(HostNameRule { conditions, hostname });
                }
                "include" => {
                    let Some(patterns) = args else {
                        self.rules.push(HostNameRule { conditions, hostname: None });
                        continue;
                    };
                    for pattern in patterns {
                        let resolved_pattern = resolve_include(&pattern, self.home);
                        let directory = resolved_pattern.parent().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
                        if pattern.contains('%') || pattern.contains("${") || pattern.contains('[') || (pattern.starts_with('~') && !tilde_is_home(&pattern)) || has_ssh_pattern(&directory) {
                            self.rules.push(HostNameRule { conditions: conditions.clone(), hostname: None });
                            continue;
                        }
                        for included in expand_glob(&resolved_pattern, self.files) {
                            self.collect(&included, &conditions);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn normalize_known_hosts_hostname(raw: &str) -> &str {
    if let Some(inner) = raw.strip_prefix('[')
        && let Some((host, port)) = inner.split_once("]:")
        && !host.is_empty()
        && !host.contains(']')
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
    {
        return host;
    }
    match (raw.find(':'), raw.rfind(':')) {
        (Some(first), Some(last)) if first == last => &raw[..last],
        _ => raw,
    }
}

/// `parseKnownHostsHostnames`: plain host names, skipping hashed entries and patterns.
pub fn parse_known_hosts_hostnames(raw: &str) -> Vec<String> {
    let mut hostnames = BTreeSet::new();
    for line in raw.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut fields = trimmed.split_whitespace();
        if trimmed.starts_with('@') {
            fields.next();
        }
        let host_field = fields.next().unwrap_or_default();
        if host_field.is_empty() || host_field.starts_with('|') {
            continue;
        }
        for raw_host in host_field.split(',') {
            let host = normalize_known_hosts_hostname(raw_host).trim();
            if !host.is_empty() && !has_ssh_pattern(host) {
                hostnames.insert(host.to_owned());
            }
        }
    }
    let mut hostnames: Vec<String> = hostnames.into_iter().collect();
    hostnames.sort_by(|left, right| locale_compare(left, right));
    hostnames
}

/// `discoverSshHosts`: every concrete alias in the user's SSH config, with the `HostName` it
/// certainly resolves to, then the `known_hosts` names no alias already reaches. Sorted by alias.
pub fn discover_ssh_hosts(home: &Path, files: &impl ConfigFiles) -> Vec<DiscoveredSshHost> {
    if home.as_os_str().is_empty() {
        return Vec::new();
    }
    let ssh = home.join(".ssh");
    let mut collector = Collector { files, home, visited: HashSet::new(), aliases: BTreeSet::new(), rules: Vec::new() };
    collector.collect(&ssh.join("config"), &[]);
    let known_hosts = files.read(&ssh.join("known_hosts")).map(|raw| parse_known_hosts_hostnames(&raw)).unwrap_or_default();

    let mut discovered: HashMap<String, DiscoveredSshHost> = HashMap::new();
    let mut configured = HashSet::new();
    for alias in &collector.aliases {
        let hostname = resolve_configured_hostname(alias, &collector.rules);
        if let Some(hostname) = &hostname {
            configured.insert(hostname.to_lowercase());
        }
        let host = DiscoveredSshHost { alias: alias.clone(), hostname: hostname.unwrap_or_else(|| alias.clone()), username: None, port: None, source: HostSource::SshConfig };
        discovered.insert(alias.clone(), host);
    }
    for hostname in known_hosts {
        if discovered.contains_key(&hostname) || configured.contains(&hostname.to_lowercase()) {
            continue;
        }
        discovered.insert(hostname.clone(), DiscoveredSshHost { alias: hostname.clone(), hostname, username: None, port: None, source: HostSource::KnownHosts });
    }
    let mut hosts: Vec<DiscoveredSshHost> = discovered.into_values().collect();
    hosts.sort_by(|left, right| locale_compare(&left.alias, &right.alias));
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory home: files by absolute path; a directory exists when a file sits under it
    /// or it is listed in `dirs`.
    #[derive(Default)]
    struct Files {
        files: HashMap<PathBuf, String>,
        dirs: HashSet<PathBuf>,
    }

    impl Files {
        fn file(mut self, path: &str, text: &str) -> Self {
            self.files.insert(PathBuf::from(path), text.to_owned());
            self
        }

        fn dir(mut self, path: &str) -> Self {
            self.dirs.insert(PathBuf::from(path));
            self
        }
    }

    impl ConfigFiles for Files {
        fn exists(&self, path: &Path) -> bool {
            self.files.contains_key(path) || self.is_dir(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.dirs.contains(path) || self.files.keys().chain(self.dirs.iter()).any(|file| file.starts_with(path) && file != path)
        }

        fn read(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }

        fn list(&self, dir: &Path) -> Vec<String> {
            let mut names: Vec<String> = self
                .files
                .keys()
                .chain(self.dirs.iter())
                .filter_map(|path| path.strip_prefix(dir).ok()?.components().next().map(|first| first.as_os_str().to_string_lossy().into_owned()))
                .filter(|name| !name.is_empty())
                .collect();
            names.sort();
            names.dedup();
            names
        }
    }

    const HOME: &str = "/home/julius";

    fn summary(hosts: &[DiscoveredSshHost]) -> Vec<(&str, &str, HostSource)> {
        hosts.iter().map(|host| (host.alias.as_str(), host.hostname.as_str(), host.source)).collect()
    }

    #[test]
    fn deduplicates_hosts_through_quoted_and_escaped_include_paths_like_openssh() {
        let files = Files::default()
            .file("/home/julius/quoted dir/team#1=a.conf", "Host quoted\n  HostName quoted\".example.com\"\nHost *\n")
            .file("/home/julius/escaped space.conf", "Host escaped\n  HostName escaped.example.com\nHost *\n")
            .file(
                "/home/julius/.ssh/config",
                "Include \"/home/julius/quoted dir/team#1=a.conf\" # comment\nInclude /home/julius/escaped\\ space.conf\n",
            )
            .file("/home/julius/.ssh/known_hosts", "escaped.example.com ssh-ed25519 AAAA\nquoted.example.com ssh-ed25519 AAAA\n");
        let hosts = discover_ssh_hosts(Path::new(HOME), &files);
        assert_eq!(
            summary(&hosts),
            [("escaped", "escaped.example.com", HostSource::SshConfig), ("quoted", "quoted.example.com", HostSource::SshConfig)]
        );
    }

    #[test]
    fn skips_directory_include_matches_without_losing_file_aliases_or_known_hosts() {
        let files = Files::default()
            .dir("/home/julius/.ssh/config.d/ignored")
            .dir("/home/julius/.ssh/explicit-directory")
            .file("/home/julius/.ssh/config", "Include config.d/*\nInclude explicit-directory\nHost real\n  HostName real.example.com\n")
            .file("/home/julius/.ssh/config.d/team.conf", "Host team\n  HostName team.example.com\n")
            .file("/home/julius/.ssh/known_hosts", "known.example.com ssh-ed25519 AAAA\n");
        assert_eq!(
            summary(&discover_ssh_hosts(Path::new(HOME), &files)),
            [
                ("known.example.com", "known.example.com", HostSource::KnownHosts),
                ("real", "real.example.com", HostSource::SshConfig),
                ("team", "team.example.com", HostSource::SshConfig),
            ]
        );
    }

    #[test]
    fn discovers_ssh_config_hosts_across_included_files() {
        let files = Files::default()
            .file("/home/julius/.ssh/config", "Host devbox devbox-admin\n  HostName 'devbox.example.com'\nHost=equalsbox\nHost *\nInclude=config.d/*.conf\n")
            .file("/home/julius/.ssh/config.d/team.conf", "Host staging\n  HostName staging.example.com\nHost *\n  ServerAliveInterval 30\n")
            .file(
                "/home/julius/.ssh/known_hosts",
                "devbox.example.com ssh-ed25519 AAAA\nstaging.example.com ssh-ed25519 AAAA\nknown.example.com ssh-ed25519 AAAA\n|1|hashed|entry ssh-ed25519 AAAA\n[bastion.example.com]:2222 ssh-ed25519 AAAA\n",
            );
        assert_eq!(
            summary(&discover_ssh_hosts(Path::new(HOME), &files)),
            [
                ("bastion.example.com", "bastion.example.com", HostSource::KnownHosts),
                ("devbox", "devbox.example.com", HostSource::SshConfig),
                ("devbox-admin", "devbox.example.com", HostSource::SshConfig),
                ("equalsbox", "equalsbox", HostSource::SshConfig),
                ("known.example.com", "known.example.com", HostSource::KnownHosts),
                ("staging", "staging.example.com", HostSource::SshConfig),
            ]
        );
    }

    #[test]
    fn retains_raw_targets_when_included_hostnames_are_inactive_or_uncertain() {
        let files = Files::default()
            .file(
                "/home/julius/.ssh/config",
                "Host WORK*\n  HostName case-sensitive.example.com\nHost work\nHost dev\n  Include conditional.conf\nHost prod\nHost matched\nMatch originalhost matched\n  Include match.conf\nHost matched\n  HostName fallback.example.com\nHost bracketed\n  Include bracket-[ab].conf\n  HostName bracket-fallback.example.com\n",
            )
            .file("/home/julius/.ssh/conditional.conf", "Host prod\n  HostName blocked.example.com\n")
            .file("/home/julius/.ssh/match.conf", "Host matched\n  HostName match.example.com\n")
            .file("/home/julius/.ssh/bracket-a.conf", "HostName bracket-real.example.com\n")
            .file(
                "/home/julius/.ssh/known_hosts",
                "bracket-real.example.com ssh-ed25519 AAAA\nbracket-fallback.example.com ssh-ed25519 AAAA\nblocked.example.com ssh-ed25519 AAAA\ncase-sensitive.example.com ssh-ed25519 AAAA\nmatch.example.com ssh-ed25519 AAAA\nfallback.example.com ssh-ed25519 AAAA\n",
            );
        let hosts = discover_ssh_hosts(Path::new(HOME), &files);
        let known: Vec<&str> = hosts.iter().filter(|host| host.source == HostSource::KnownHosts).map(|host| host.hostname.as_str()).collect();
        assert_eq!(
            known,
            ["blocked.example.com", "bracket-fallback.example.com", "bracket-real.example.com", "case-sensitive.example.com", "fallback.example.com", "match.example.com"]
        );
        let hostname = |alias: &str| hosts.iter().find(|host| host.alias == alias).map(|host| host.hostname.as_str());
        for alias in ["bracketed", "prod", "work", "matched"] {
            assert_eq!(hostname(alias), Some(alias), "{alias} keeps its raw target");
        }
    }

    #[test]
    fn parses_known_hosts_entries_without_returning_hashed_hosts() {
        let raw = "github.com ssh-ed25519 AAAA\ngitlab.com,gitlab-alias ssh-ed25519 BBBB\n|1|hashed|entry ssh-ed25519 CCCC\n@cert-authority *.example.com ssh-ed25519 DDDD\n[ssh.example.com]:2200 ssh-ed25519 EEEE\nport.example.com:22 ssh-ed25519 HHHH\n::1 ssh-ed25519 FFFF\n2001:db8::1 ssh-ed25519 GGGG\n";
        assert_eq!(
            parse_known_hosts_hostnames(raw),
            ["::1", "2001:db8::1", "github.com", "gitlab-alias", "gitlab.com", "port.example.com", "ssh.example.com"]
        );
    }

    #[test]
    fn config_lines_split_like_openssh() {
        let args = parse_config_line;
        assert_eq!(args("  HostName = 'a b' c#d # e"), Some(("hostname".to_owned(), Some(vec!["a b".to_owned(), "c#d".to_owned()]))));
        assert_eq!(args("Host \"open"), Some(("host".to_owned(), None)));
        assert_eq!(args("   "), None);
        assert!(glob_matches("dev*box?", "dev-a-box1") && !glob_matches("dev*", "prod"));
    }
}
