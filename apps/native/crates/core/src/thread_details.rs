//! Pure layout, visibility, servers, automations, and header rules for the thread header and
//! details card (`ChatHeader.tsx`, `threadDetailsItems.ts`, `threadDetailsCardLayout.ts`,
//! `ThreadDetailsPanel.tsx`, `ThreadServersPanel.tsx`, `ThreadAutomationsPanel.tsx`).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use url::Url;

use crate::preview::DiscoveredServer;
use crate::preview::address::normalize_preview_url;
use crate::settings::scheduled_tasks::{Schedule, ScheduledTask, relative_run_label};
use crate::shell::LinkOrigin;
use crate::sidebar::time::parse_iso;

/// `THREAD_DETAILS_ITEMS` from `apps/web/src/components/chat/threadDetailsItems.ts`.
/// Ids are persisted, so never rename one.
pub const ITEMS: &[(&str, &str, bool)] = &[
    ("environment", "Environment and worktree", true),
    ("openIn", "Open in editor", true),
    ("scripts", "Project scripts", true),
    ("branch", "Branch and pull request", true),
    ("gitActions", "Git actions", true),
    ("changes", "Changes", true),
    ("servers", "Servers", true),
    ("automations", "Automations", true),
    ("lineage", "Lineage", true),
    ("sideChats", "Side chats", true),
    ("usage", "Usage limits", false),
    ("context", "Context window", false),
];

pub const SERVER_PAGE_COUNT: usize = 4;

fn default_visible(id: &str) -> bool {
    ITEMS.iter().find(|(item, _, _)| *item == id).is_some_and(|(_, _, visible)| *visible)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Visibility {
    overrides: HashMap<String, bool>,
}

impl Visibility {
    pub fn from_overrides(overrides: HashMap<String, bool>) -> Self {
        Visibility { overrides }
    }

    pub fn overrides(&self) -> &HashMap<String, bool> {
        &self.overrides
    }

    /// `resolveThreadDetailsItemVisible` with legacy `threadDetailsUsageEnabled` fallback.
    pub fn shows_with_legacy_usage(&self, id: &str, legacy_usage_enabled: Option<bool>) -> bool {
        if let Some(value) = self.overrides.get(id).copied() {
            return value;
        }
        if matches!(id, "usage" | "context")
            && let Some(enabled) = legacy_usage_enabled
        {
            return enabled;
        }
        default_visible(id)
    }

    pub fn shows(&self, id: &str) -> bool {
        self.shows_with_legacy_usage(id, None)
    }

    pub fn set_with_legacy_usage(&mut self, id: &str, visible: bool, legacy_usage_enabled: Option<bool>) {
        let default = if matches!(id, "usage" | "context") {
            legacy_usage_enabled.unwrap_or_else(|| default_visible(id))
        } else {
            default_visible(id)
        };
        if visible == default {
            self.overrides.remove(id);
        } else {
            self.overrides.insert(id.to_owned(), visible);
        }
    }

    pub fn set(&mut self, id: &str, visible: bool) {
        self.set_with_legacy_usage(id, visible, None);
    }

    pub fn is_default_with_legacy_usage(&self, legacy_usage_enabled: Option<bool>) -> bool {
        ITEMS.iter().all(|(id, _, visible)| self.shows_with_legacy_usage(id, legacy_usage_enabled) == *visible)
    }

    pub fn is_default(&self) -> bool {
        self.is_default_with_legacy_usage(None)
    }

    pub fn reset(&mut self) {
        self.overrides.clear();
    }
}

/// `ThreadDetailsCardDensity`: cascades `Full` -> `Compact` -> `Essential` when vertical room
/// tightens (`threadDetailsCardLayout.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardDensity {
    Full,
    Compact,
    Essential,
}

impl CardDensity {
    pub fn is_full(self) -> bool {
        self == CardDensity::Full
    }

    pub fn is_essential(self) -> bool {
        self == CardDensity::Essential
    }
}

/// Whether an item renders at `density`. Mirrors `ThreadDetailsPanel.tsx`:
/// - `Full`: every enabled item.
/// - `Compact`: drops `environment`, `servers`, `automations`, `lineage`, `sideChats`.
/// - `Essential`: drops `openIn` too, keeping workspace `scripts`, version control, and telemetry.
pub fn item_visible_at_density(id: &str, density: CardDensity) -> bool {
    match density {
        CardDensity::Full => true,
        CardDensity::Compact => !matches!(id, "environment" | "servers" | "automations" | "lineage" | "sideChats"),
        CardDensity::Essential => !matches!(id, "environment" | "openIn" | "servers" | "automations" | "lineage" | "sideChats"),
    }
}

/// Counts of fixed rows per section and list section presence used to estimate non-scrolling height
/// for `resolve_card_density`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DensityInputs {
    pub environment: bool,
    pub open_in: bool,
    pub scripts: bool,
    pub branch: bool,
    pub pr_row: bool,
    pub git_actions: bool,
    pub changes: bool,
    pub servers_rows: usize,
    pub automations_rows: usize,
    pub lineage_rows: usize,
    pub side_chats_rows: usize,
    pub usage_rows: usize,
    pub context: bool,
}

const ROW_HEIGHT: f32 = 32.;
const SECTION_PAD: f32 = 16.;
const SECTION_TITLE_HEIGHT: f32 = 24.;
const SEPARATOR_HEIGHT: f32 = 9.;
const CARD_PADDING: f32 = 8.;

/// `fittingHeight` (`ThreadDetailsCard.tsx:82-87`): measures fixed sections and excludes scrolling
/// list containers (`pr_row`, `automations`, `lineage`, `side_chats`, and server overflow).
fn estimate_height(inputs: &DensityInputs, density: CardDensity) -> f32 {
    let mut height = CARD_PADDING;
    let mut sections = 0usize;

    let ws_rows = usize::from(inputs.environment && item_visible_at_density("environment", density))
        + usize::from(inputs.open_in && item_visible_at_density("openIn", density))
        + usize::from(inputs.scripts && item_visible_at_density("scripts", density));
    if ws_rows > 0 {
        sections += 1;
        height += if density.is_essential() { 8. } else { SECTION_PAD } + ws_rows as f32 * ROW_HEIGHT;
    }

    let vc_rows = usize::from(inputs.branch && item_visible_at_density("branch", density))
        + usize::from(inputs.git_actions && item_visible_at_density("gitActions", density))
        + usize::from(inputs.changes && item_visible_at_density("changes", density));
    if vc_rows > 0 {
        sections += 1;
        height += if density.is_essential() { 8. } else { SECTION_PAD } + vc_rows as f32 * ROW_HEIGHT;
    }

    if inputs.servers_rows > 0 && item_visible_at_density("servers", density) {
        sections += 1;
        height += SECTION_PAD + SECTION_TITLE_HEIGHT + inputs.servers_rows.min(SERVER_PAGE_COUNT) as f32 * 40.;
    }

    let usage_rows = inputs.usage_rows * usize::from(item_visible_at_density("usage", density));
    if usage_rows > 0 {
        sections += 1;
        height += if density.is_essential() { 8. } else { SECTION_PAD } + usage_rows as f32 * 28.;
    }
    let ctx_rows = usize::from(inputs.context && item_visible_at_density("context", density));
    if ctx_rows > 0 {
        sections += 1;
        height += if density.is_essential() { 8. } else { SECTION_PAD } + ctx_rows as f32 * 30.;
    }

    if sections > 1 && density.is_full() {
        height += (sections - 1) as f32 * SEPARATOR_HEIGHT;
    }
    height
}

/// `resolveThreadDetailsCardDensity`: stays `Full` until estimated height exceeds `available_height`,
/// then cascades to `Compact` and `Essential`.
pub fn resolve_card_density(available_height: f32, inputs: &DensityInputs) -> CardDensity {
    if available_height <= 0. {
        return CardDensity::Full;
    }
    if estimate_height(inputs, CardDensity::Full) <= available_height {
        CardDensity::Full
    } else if estimate_height(inputs, CardDensity::Compact) <= available_height {
        CardDensity::Compact
    } else {
        CardDensity::Essential
    }
}

/// Discovered local servers belonging to `thread_id` (via agent or terminal attribution), sorted by port.
pub fn filter_thread_servers<'a>(servers: &'a [DiscoveredServer], thread_id: &str) -> Vec<&'a DiscoveredServer> {
    let mut matching: Vec<&DiscoveredServer> = servers
        .iter()
        .filter(|server| {
            server.agent.as_ref().is_some_and(|agent| agent.thread_id == thread_id)
                || server.terminal.as_ref().is_some_and(|terminal| terminal.thread_id == thread_id)
        })
        .collect();
    matching.sort_by_key(|server| (server.port, server.pid.unwrap_or(0)));
    matching
}

/// `"Agent"` when attributed to the agent, else `"Terminal"` (`ThreadServersPanel.tsx`).
pub fn server_source_label(server: &DiscoveredServer) -> &'static str {
    if server.agent.is_some() { "Agent" } else { "Terminal" }
}

pub fn server_subtitle(server: &DiscoveredServer) -> String {
    let source = server_source_label(server);
    match server.process_name.as_deref().map(str::trim).filter(|name| !name.is_empty()) {
        Some(process) => format!("{source} \u{00b7} {process}"),
        None => source.to_owned(),
    }
}

pub fn can_stop_server(server: &DiscoveredServer, can_operate_terminal: bool, local_server_stop_supported: Option<bool>) -> bool {
    can_operate_terminal && local_server_stop_supported == Some(true) && server.pid.is_some_and(|pid| pid > 0)
}

fn is_loopback_host(host: &str) -> bool {
    let bare = host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if bare == "localhost" || bare.ends_with(".localhost") || bare == "0.0.0.0" || bare == "::" {
        return true;
    }
    bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn is_private_network_host(host: &str) -> bool {
    let bare = host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if bare.ends_with(".local") {
        return true;
    }
    match bare.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            let [a, b, ..] = ip.octets();
            a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168) || (a == 100 && (64..=127).contains(&b)) || (a == 169 && b == 254)
        }
        Ok(IpAddr::V6(ip)) => {
            let seg = ip.segments()[0];
            (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
}

/// `resolveDiscoveredServerUrl` (`apps/web/src/browser/browserTargetResolver.ts`): rewrites a
/// loopback discovered server URL to the remote environment's private-network hostname when paired
/// to a private remote environment, keeping scheme, port, path, query, and fragment intact.
pub fn resolve_discovered_server_url(raw_url: &str, environment_http_base: Option<&str>) -> String {
    let normalized = normalize_preview_url(raw_url).unwrap_or_else(|_| raw_url.trim().to_owned());
    let Ok(mut server_url) = Url::parse(&normalized) else {
        return normalized;
    };
    let Some(server_host) = server_url.host_str() else {
        return normalized;
    };
    if !is_loopback_host(server_host) {
        return normalized;
    }
    let Some(env_base) = environment_http_base else {
        return normalized;
    };
    let Ok(env_url) = Url::parse(env_base.trim()) else {
        return normalized;
    };
    let Some(env_host) = env_url.host_str() else {
        return normalized;
    };
    if is_loopback_host(env_host) || !is_private_network_host(env_host) {
        return normalized;
    }
    let _ = server_url.set_host(Some(env_host));
    server_url.into()
}

pub fn stop_server_payload(pid: i64, port: u16) -> Value {
    json!({ "pid": pid, "port": port })
}

/// Maps `PreviewStopLocalServerError.reason` or transport error strings to web-parity copy.
pub fn stop_server_error_message(error: &str) -> String {
    if error.contains("not-found") {
        "This server is no longer running.".to_owned()
    } else if error.contains("shared-group") {
        "This server shares a process group with T3 Code, an agent, or a terminal shell, so it can't be stopped on its own.".to_owned()
    } else if error.contains("unsupported") {
        "Stopping servers is not supported on this environment's platform.".to_owned()
    } else if error.contains("signal-failed") {
        "The server's processes could not be signaled.".to_owned()
    } else if error.trim().is_empty() {
        "Could not stop local server.".to_owned()
    } else {
        error.to_owned()
    }
}

/// Returns `(visible_count, hidden_count)` for incremental `+ SERVER_PAGE_COUNT` paging.
pub fn visible_server_page(total: usize, visible_limit: usize) -> (usize, usize) {
    let visible = total.min(visible_limit.max(SERVER_PAGE_COUNT));
    (visible, total.saturating_sub(visible))
}

/// Scheduled tasks bound to `thread_id` (`ThreadAutomationsPanel.tsx`).
pub fn tasks_for_thread<'a>(tasks: &'a [ScheduledTask], thread_id: &str) -> Vec<&'a ScheduledTask> {
    tasks.iter().filter(|task| task.thread_id.as_deref() == Some(thread_id)).collect()
}

/// Short label for the server zone a fixed-time schedule runs in (`scheduleTimeZoneLabel`).
pub fn schedule_time_zone_label(time_zone: Option<&str>, run_at: Option<&str>) -> Option<String> {
    let time_zone = time_zone?.trim();
    if time_zone.is_empty() {
        return None;
    }
    if matches!(time_zone, "Etc/UTC" | "UTC" | "GMT") {
        return Some("UTC".to_owned());
    }
    let parsed: chrono_tz::Tz = time_zone.parse().ok()?;
    let ms = run_at.and_then(parse_iso).unwrap_or(0);
    use chrono::TimeZone;
    let abbr = parsed.timestamp_millis_opt(ms).single().map(|at| at.format("%Z").to_string())?;
    if run_at.is_some() {
        Some(abbr)
    } else {
        // Without a target instant, return a non-DST-biased short generic name when possible.
        let winter = parsed.timestamp_millis_opt(1_799_999_999_000).single().map(|at| at.format("%Z").to_string());
        let summer = parsed.timestamp_millis_opt(1_815_900_000_000).single().map(|at| at.format("%Z").to_string());
        match (winter, summer) {
            (Some(w), Some(s)) if w == s => Some(w),
            (Some(w), Some(s)) if w.len() == 3 && s.len() == 3 && w.chars().next() == s.chars().next() && w.ends_with('T') && s.ends_with('T') => {
                Some(format!("{}T", w.chars().next()?))
            }
            _ => Some(abbr),
        }
    }
}

/// `taskScheduleLine` (`ThreadAutomationsPanel.tsx`): schedule + `blockedByPolicy`, or `next <rel>`, or `paused`.
pub fn automation_schedule_line(task: &ScheduledTask, once_label: impl Fn(i64) -> String, now_ms: i64) -> String {
    automation_schedule_line_in(task, once_label, None, now_ms)
}

/// `taskScheduleLine` with server `timeZone` forwarded to fixed-time labels.
pub fn automation_schedule_line_in(
    task: &ScheduledTask,
    once_label: impl Fn(i64) -> String,
    time_zone: Option<&str>,
    now_ms: i64,
) -> String {
    let base = task.schedule.label(once_label);
    let schedule = if matches!(task.schedule, Schedule::FixedTime { .. })
        && let Some(zone) = schedule_time_zone_label(time_zone, task.next_run_at.as_deref())
    {
        format!("{base} {zone}")
    } else {
        base
    };
    finish_automation_schedule_line(schedule, task, now_ms)
}

/// `taskScheduleLine` formatted per user `TimestampFormat` (`pingdotgg/t3code#37314`).
pub fn automation_schedule_line_with_format(
    task: &ScheduledTask,
    once_label: impl Fn(i64) -> String,
    time_zone: Option<&str>,
    now_ms: i64,
    format: crate::client_settings::TimestampFormat,
    locale: &dyn crate::timeline::TimestampLocale,
) -> String {
    let schedule = task.schedule.label_with_format(
        format,
        locale,
        time_zone,
        task.next_run_at.as_deref(),
        now_ms,
        once_label,
    );
    finish_automation_schedule_line(schedule, task, now_ms)
}

fn finish_automation_schedule_line(schedule: String, task: &ScheduledTask, now_ms: i64) -> String {
    if let Some(reason) = &task.blocked_by_policy {
        format!("{schedule} \u{00b7} {reason}")
    } else if !task.enabled {
        format!("{schedule} \u{00b7} paused")
    } else if let Some(next_run_at) = task.next_run_at.as_deref() {
        format!("{schedule} \u{00b7} next {}", relative_run_label(Some(next_run_at), now_ms))
    } else {
        schedule
    }
}

/// `ChatHeader`: `"No project"` when `is_scratch` (`ChatView.tsx`).
pub fn header_project_title(title: &str, is_scratch: bool) -> &str {
    if is_scratch { "No project" } else { title }
}

/// `resolveRenameCommit`: trimmed non-empty string only when it differs from `initial.trim()`.
pub fn resolve_rename_commit(initial: &str, draft: &str) -> Option<String> {
    let trimmed = draft.trim();
    if trimmed.is_empty() || trimmed == initial.trim() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// `(badge_label, tooltip)` for a delegated thread's `linkOrigin` (`ChatHeader.tsx:423-428`).
pub fn link_origin_badge(origin: &LinkOrigin) -> Option<(String, String)> {
    let label = origin.label.trim();
    if label.is_empty() {
        return None;
    }
    let badge = format!("From {label}");
    let tooltip = format!("Started by an agent on {label} through a linked environment.");
    Some((badge, tooltip))
}

/// Collapses the parent breadcrumb label to `"..."` when the measured breadcrumb content exceeds
/// the available header width (`ChatHeader.tsx` overflow observer).
pub fn should_collapse_parent_crumb(available_width: f32, content_width: f32) -> bool {
    available_width > 0. && content_width > available_width
}

/// Default remote-capable editor fallback on desktop clients (`REMOTE_FALLBACK_EDITORS`).
pub const REMOTE_FALLBACK_EDITORS: &[&str] = &["vscode"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteOpenHost {
    pub host: String,
    pub username: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteOpenState {
    LocalExec,
    RemoteLinks(RemoteOpenHost),
    RemoteUnavailable,
}

pub struct RemoteOpenInput<'a> {
    pub active_environment_id: Option<&'a str>,
    pub host_environment_id: Option<&'a str>,
    pub is_local_saved: bool,
    pub http_base: Option<&'a str>,
    /// `(alias, username)` from an SSH saved environment.
    pub ssh_target: Option<(&'a str, Option<&'a str>)>,
    pub remote_open_targets: &'a [RemoteOpenHost],
}

fn is_loopback_http(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else { return false };
    let Some(host) = parsed.host_str() else { return false };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.eq_ignore_ascii_case("localhost")
        || bare.to_ascii_lowercase().ends_with(".localhost")
        || bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `resolveRemoteOpenState` (`apps/web/src/remoteOpen.ts`), including `pingdotgg/t3code#21661`
/// where a background service sharing the host environment runs locally.
pub fn resolve_remote_open_state(input: &RemoteOpenInput<'_>) -> RemoteOpenState {
    if input.active_environment_id.is_none()
        || input.host_environment_id.is_some_and(|host| input.active_environment_id == Some(host))
        || input.is_local_saved
        || input.http_base.is_some_and(is_loopback_http)
    {
        return RemoteOpenState::LocalExec;
    }
    if let Some((alias, explicit_user)) = input.ssh_target
        && !alias.is_empty()
    {
        let (host, alias_user) = match alias.rfind('@') {
            Some(at) => (&alias[at + 1..], Some(&alias[..at])),
            None => (alias, None),
        };
        let username = explicit_user
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .or_else(|| alias_user.filter(|u| !u.is_empty()))
            .map(str::to_owned);
        return RemoteOpenState::RemoteLinks(RemoteOpenHost { host: host.to_owned(), username });
    }
    if let Some(advertised) = input.remote_open_targets.first() {
        return RemoteOpenState::RemoteLinks(advertised.clone());
    }
    RemoteOpenState::RemoteUnavailable
}

/// `shouldShowOpenInPicker` (`OpenInPicker.logic.ts`, `pingdotgg/t3code#21661`).
pub fn should_show_open_in_picker(
    has_active_project: bool,
    active_thread_environment_id: Option<&str>,
    primary_environment_id: Option<&str>,
    host_environment_id: Option<&str>,
    remote_state: &RemoteOpenState,
) -> bool {
    if !has_active_project {
        return false;
    }
    if let Some(active) = active_thread_environment_id
        && (Some(active) == primary_environment_id || Some(active) == host_environment_id)
    {
        return true;
    }
    !matches!(remote_state, RemoteOpenState::LocalExec)
}

/// Returns `(remote_scheme, jetbrains_product_code)` for editors supporting remote SSH URLs.
pub fn remote_scheme_for_editor(id: &str) -> Option<(&'static str, Option<&'static str>)> {
    match id {
        "cursor" => Some(("cursor", None)),
        "vscode" => Some(("vscode", None)),
        "vscode-insiders" => Some(("vscode-insiders", None)),
        "vscodium" => Some(("vscodium", None)),
        "zed" => Some(("zed", None)),
        "idea" => Some(("jetbrains", Some("IU"))),
        "aqua" => Some(("jetbrains", Some("QA"))),
        "clion" => Some(("jetbrains", Some("CL"))),
        "datagrip" => Some(("jetbrains", Some("DB"))),
        "dataspell" => Some(("jetbrains", Some("DS"))),
        "goland" => Some(("jetbrains", Some("GO"))),
        "phpstorm" => Some(("jetbrains", Some("PS"))),
        "pycharm" => Some(("jetbrains", Some("PY"))),
        "rider" => Some(("jetbrains", Some("RD"))),
        "rubymine" => Some(("jetbrains", Some("RM"))),
        "rustrover" => Some(("jetbrains", Some("RR"))),
        "webstorm" => Some(("jetbrains", Some("WS"))),
        _ => None,
    }
}

/// `REMOTE_CAPABLE_EDITOR_IDS` (`packages/contracts/src/editor.ts`) in display order.
pub const REMOTE_CAPABLE_EDITOR_IDS: &[&str] = &[
    "cursor",
    "vscode",
    "vscode-insiders",
    "vscodium",
    "zed",
    "idea",
    "aqua",
    "clion",
    "datagrip",
    "dataspell",
    "goland",
    "phpstorm",
    "pycharm",
    "rider",
    "rubymine",
    "rustrover",
    "webstorm",
];

fn remote_editor_probe_spec(id: &str) -> Option<(&'static [&'static str], &'static [&'static str], bool)> {
    // `(commands, mac_bundle_names, is_jetbrains)` mirroring `EDITORS` + `installNames` in `packages/shared/src/editor.ts`.
    match id {
        "cursor" => Some((&["cursor"], &["Cursor"], false)),
        "vscode" => Some((&["code"], &["Visual Studio Code"], false)),
        "vscode-insiders" => Some((&["code-insiders"], &["Visual Studio Code - Insiders"], false)),
        "vscodium" => Some((&["codium"], &["VSCodium"], false)),
        "zed" => Some((&["zed", "zeditor"], &["Zed"], false)),
        "idea" => Some((&["idea"], &["IntelliJ IDEA", "IntelliJ IDEA CE", "IntelliJ IDEA Ultimate"], true)),
        "aqua" => Some((&["aqua"], &["Aqua"], true)),
        "clion" => Some((&["clion"], &["CLion"], true)),
        "datagrip" => Some((&["datagrip"], &["DataGrip"], true)),
        "dataspell" => Some((&["dataspell"], &["DataSpell"], true)),
        "goland" => Some((&["goland"], &["GoLand"], true)),
        "phpstorm" => Some((&["phpstorm"], &["PhpStorm"], true)),
        "pycharm" => Some((&["pycharm"], &["PyCharm", "PyCharm CE"], true)),
        "rider" => Some((&["rider"], &["Rider", "JetBrains Rider"], true)),
        "rubymine" => Some((&["rubymine"], &["RubyMine"], true)),
        "rustrover" => Some((&["rustrover"], &["RustRover"], true)),
        "webstorm" => Some((&["webstorm"], &["WebStorm"], true)),
        _ => None,
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn is_versioned_macos_bundle(entry: &str, name: &str) -> bool {
    let Some(rest) = entry.strip_prefix(name).and_then(|s| s.strip_prefix(' ')) else { return false };
    let Some(ver) = rest.strip_suffix(".app") else { return false };
    !ver.is_empty() && ver.as_bytes()[0].is_ascii_digit() && ver.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

fn resolve_on_path(command: &str, path_dirs: &[PathBuf]) -> Option<PathBuf> {
    path_dirs.iter().map(|dir| dir.join(command)).find(|candidate| is_executable_file(candidate))
}

fn is_zed_cli_binary(path: &Path) -> bool {
    std::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .is_some_and(|output| output.status.success() && String::from_utf8_lossy(&output.stdout).starts_with("Zed"))
}

fn has_macos_or_toolbox_candidate(
    id: &str,
    command: &str,
    bundle_names: &[&str],
    is_jetbrains: bool,
    macos_app_roots: &[PathBuf],
    toolbox_dir: Option<&Path>,
) -> bool {
    for root in macos_app_roots {
        let mut bundles: Vec<String> = bundle_names.iter().map(|name| format!("{name}.app")).collect();
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && bundle_names.iter().any(|base| name == format!("{base}.app") || is_versioned_macos_bundle(name, base))
                    && !bundles.iter().any(|existing| existing == name)
                {
                    bundles.push(name.to_owned());
                }
            }
        }
        for bundle in bundles {
            let contents = root.join(bundle).join("Contents");
            let found = if is_jetbrains || id == "zed" {
                is_executable_file(&contents.join("MacOS").join(if id == "zed" { "cli" } else { command }))
            } else {
                is_executable_file(&contents.join("Resources/app/bin").join(command)) || is_executable_file(&contents.join("Resources/app/bin/code"))
            };
            if found {
                return true;
            }
        }
    }
    let Some(toolbox_dir) = toolbox_dir.filter(|_| is_jetbrains) else {
        return false;
    };
    if is_executable_file(&toolbox_dir.join(command)) {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(toolbox_dir) else { return false };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { return false };
        name.strip_prefix(command).is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())) && is_executable_file(&entry.path())
    })
}

/// Probes locally installed remote-capable editors (`probeRemoteEditors` / `resolveEditorCommand`
/// from `apps/desktop/src/ipc/methods/window.ts` + `packages/shared/src/editor.ts`).
pub fn probe_remote_capable_editors() -> Vec<String> {
    let path_var = std::env::var_os("PATH");
    let path_dirs: Vec<PathBuf> = path_var.as_deref().map(std::env::split_paths).into_iter().flatten().collect();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut app_roots = Vec::new();
    if cfg!(target_os = "macos") {
        if let Some(home) = &home {
            app_roots.push(home.join("Applications"));
        }
        app_roots.push(PathBuf::from("/Applications"));
    }
    let toolbox_dir = if cfg!(target_os = "macos") {
        home.as_ref().map(|h| h.join("Library/Application Support/JetBrains/Toolbox/scripts"))
    } else if cfg!(target_os = "linux") {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")))
            .map(|d| d.join("JetBrains/Toolbox/scripts"))
    } else {
        None
    };
    probe_remote_capable_editors_in(&path_dirs, &app_roots, toolbox_dir.as_deref(), cfg!(target_os = "macos"), &is_zed_cli_binary)
}

fn probe_remote_capable_editors_in(
    path_dirs: &[PathBuf],
    macos_app_roots: &[PathBuf],
    toolbox_dir: Option<&Path>,
    prefer_zed_bundle: bool,
    check_zed: &dyn Fn(&Path) -> bool,
) -> Vec<String> {
    let mut found = Vec::new();
    for &id in REMOTE_CAPABLE_EDITOR_IDS {
        let Some((commands, bundle_names, is_jetbrains)) = remote_editor_probe_spec(id) else { continue };
        let on_path = if id == "zed" && prefer_zed_bundle {
            false
        } else {
            commands.iter().any(|cmd| {
                let Some(resolved) = resolve_on_path(cmd, path_dirs) else { return false };
                id != "zed" || *cmd != "zed" || check_zed(&resolved)
            })
        };
        let in_bundle_or_toolbox = has_macos_or_toolbox_candidate(id, commands[0], bundle_names, is_jetbrains, macos_app_roots, toolbox_dir);
        let zed_fallback = if id == "zed" && prefer_zed_bundle && !in_bundle_or_toolbox {
            commands.iter().any(|cmd| resolve_on_path(cmd, path_dirs).is_some_and(|p| *cmd != "zed" || check_zed(&p)))
        } else {
            false
        };
        if on_path || in_bundle_or_toolbox || zed_fallback {
            found.push(id.to_owned());
        }
    }
    found
}

pub fn effective_open_in_editors(remote_state: &RemoteOpenState, server_available: &[String], remote_capable: &[String]) -> Vec<String> {
    match remote_state {
        RemoteOpenState::LocalExec => server_available.to_vec(),
        RemoteOpenState::RemoteLinks(_) => remote_capable.iter().filter(|id| remote_scheme_for_editor(id).is_some()).cloned().collect(),
        RemoteOpenState::RemoteUnavailable => Vec::new(),
    }
}

/// Disabled picker reason (`OpenInPicker.tsx`), shown when remote SSH links are missing or no
/// computer editors are available on a remote connection.
pub fn open_in_unavailable_reason(remote_state: &RemoteOpenState, effective_editor_count: usize, environment_label: &str) -> Option<String> {
    let label = if environment_label.trim().is_empty() { "this machine" } else { environment_label.trim() };
    match remote_state {
        RemoteOpenState::RemoteUnavailable => Some(format!("No SSH route to {label}")),
        RemoteOpenState::RemoteLinks(_) if effective_editor_count == 0 => Some(format!("Open from a computer to edit on {label}")),
        _ => None,
    }
}

fn encode_uri_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let keep = matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')');
        if keep {
            out.push(char::from(*byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn encode_slash_segments(path: &str) -> String {
    path.split('/').map(encode_uri_component).collect::<Vec<_>>().join("/")
}

/// `buildRemoteOpenUrl` (`packages/contracts/src/editor.ts`).
pub fn build_remote_open_url(editor: &str, host: &RemoteOpenHost, absolute_path: &str, is_file: bool) -> Option<String> {
    let (scheme, jetbrains_code) = remote_scheme_for_editor(editor)?;
    let posix_path = absolute_path.replace('\\', "/");
    if let Some(code) = jetbrains_code {
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        ser.append_pair("h", &host.host)
            .append_pair("launchIde", "true")
            .append_pair("ideHint", code)
            .append_pair("projectHint", &posix_path);
        if let Some(user) = &host.username {
            ser.append_pair("u", user);
        }
        return Some(format!("{scheme}://gateway/ssh/environment?{}", ser.finish()));
    }
    let rooted_path = if posix_path.starts_with('/') { posix_path } else { format!("/{posix_path}") };
    if editor == "zed" {
        let is_win_drive = absolute_path.as_bytes().get(1) == Some(&b':')
            && absolute_path.as_bytes().first().is_some_and(|b| matches!(b, b'c' | b'C'))
            && absolute_path.as_bytes().get(2).is_some_and(|b| matches!(b, b'\\' | b'/'));
        let zed_path = if is_win_drive { &rooted_path[3..] } else { &rooted_path };
        let encoded_zed_path = encode_slash_segments(zed_path);
        let encoded_host = encode_uri_component(&host.host);
        let zed_dest = match &host.username {
            Some(user) => format!("{}@{encoded_host}", encode_uri_component(user)),
            None => encoded_host,
        };
        return Some(format!("{scheme}://ssh/{zed_dest}{encoded_zed_path}"));
    }
    let encoded_path = encode_slash_segments(&rooted_path);
    let plain_dest = match &host.username {
        Some(user) => format!("{user}@{}", host.host),
        None => host.host.clone(),
    };
    let destination = if plain_dest.contains('+') {
        let mut json_obj = serde_json::Map::new();
        json_obj.insert("hostName".into(), Value::String(host.host.clone()));
        if let Some(user) = &host.username {
            json_obj.insert("user".into(), Value::String(user.clone()));
        }
        let encoded_json = Value::Object(json_obj).to_string();
        let mut hex = String::with_capacity(encoded_json.len() * 2);
        for byte in encoded_json.as_bytes() {
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    } else {
        plain_dest
    };
    let suffix = if is_file { ":1:1" } else { "" };
    Some(format!("{scheme}://vscode-remote/ssh-remote+{}{encoded_path}{suffix}", encode_uri_component(&destination)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn server(port: u16, pid: Option<i64>, agent_thread: Option<&str>, terminal_thread: Option<&str>, process_name: Option<&str>) -> DiscoveredServer {
        DiscoveredServer {
            host: "127.0.0.1".to_owned(),
            port,
            url: format!("http://127.0.0.1:{port}"),
            process_name: process_name.map(str::to_owned),
            pid,
            terminal: terminal_thread.map(|id| crate::preview::DiscoveredServerTerminal { thread_id: id.to_owned(), terminal_id: "term-1".to_owned() }),
            agent: agent_thread.map(|id| crate::preview::DiscoveredServerAgent { thread_id: id.to_owned() }),
        }
    }

    #[test]
    fn visibility_covers_all_twelve_items_and_legacy_usage_fallback() {
        assert_eq!(ITEMS.len(), 12);
        let mut visibility = Visibility::default();
        assert!(visibility.is_default());
        assert!(visibility.shows("servers"));
        assert!(visibility.shows("automations"));
        assert!(!visibility.shows("usage"));
        assert!(!visibility.shows("context"));

        assert!(visibility.shows_with_legacy_usage("usage", Some(true)));
        assert!(visibility.shows_with_legacy_usage("context", Some(true)));
        assert!(!visibility.is_default_with_legacy_usage(Some(true)));
        visibility.set_with_legacy_usage("usage", false, Some(true));
        assert!(!visibility.shows_with_legacy_usage("usage", Some(true)));
        visibility.set("usage", true);
        assert!(visibility.shows("usage"));
        assert!(!visibility.is_default());
        visibility.reset();
        assert!(visibility.is_default());
    }

    #[test]
    fn card_density_cascades_full_compact_essential() {
        let rich = DensityInputs {
            environment: true,
            open_in: true,
            scripts: true,
            branch: true,
            pr_row: false,
            git_actions: true,
            changes: true,
            servers_rows: 2,
            automations_rows: 0,
            lineage_rows: 0,
            side_chats_rows: 0,
            usage_rows: 2,
            context: true,
        };
        assert_eq!(resolve_card_density(900., &rich), CardDensity::Full);
        assert_eq!(resolve_card_density(450., &rich), CardDensity::Compact);
        assert_eq!(resolve_card_density(310., &rich), CardDensity::Essential);
        let with_scrolling_lists = DensityInputs {
            pr_row: true,
            servers_rows: 20,
            automations_rows: 10,
            lineage_rows: 10,
            side_chats_rows: 10,
            ..rich
        };
        assert_eq!(
            resolve_card_density(estimate_height(&with_scrolling_lists, CardDensity::Full), &with_scrolling_lists),
            CardDensity::Full,
        );
        assert_eq!(
            estimate_height(&DensityInputs { servers_rows: 4, ..rich }, CardDensity::Full),
            estimate_height(&with_scrolling_lists, CardDensity::Full),
        );
        assert!(item_visible_at_density("servers", CardDensity::Full));
        assert!(!item_visible_at_density("servers", CardDensity::Compact));
        assert!(item_visible_at_density("openIn", CardDensity::Compact));
        assert!(!item_visible_at_density("openIn", CardDensity::Essential));
        assert!(item_visible_at_density("usage", CardDensity::Essential));
        assert!(item_visible_at_density("context", CardDensity::Essential));
    }

    #[test]
    fn filters_thread_servers_and_gates_stop_capability() {
        let servers = vec![
            server(5173, Some(200), None, Some("t-1"), Some("vite")),
            server(3000, Some(100), Some("t-1"), None, Some("next")),
            server(8080, Some(300), Some("t-2"), None, None),
            server(4000, None, None, Some("t-1"), Some("node")),
        ];
        let matched = filter_thread_servers(&servers, "t-1");
        let ports: Vec<u16> = matched.iter().map(|s| s.port).collect();
        assert_eq!(ports, vec![3000, 4000, 5173]);
        assert_eq!(server_subtitle(matched[0]), "Agent \u{00b7} next");
        assert_eq!(server_subtitle(matched[2]), "Terminal \u{00b7} vite");
        assert!(can_stop_server(matched[0], true, Some(true)));
        assert!(!can_stop_server(matched[0], false, Some(true)));
        assert!(!can_stop_server(matched[0], true, None));
        assert!(!can_stop_server(matched[1], true, Some(true)));
        assert_eq!(visible_server_page(10, 4), (4, 6));
        assert_eq!(visible_server_page(10, 8), (8, 2));
        assert_eq!(visible_server_page(10, 12), (10, 0));
        assert!(stop_server_error_message("PreviewStopLocalServerError: shared-group").contains("process group"));
    }

    #[test]
    fn resolves_loopback_discovered_server_urls_for_private_remote_environments() {
        assert_eq!(
            resolve_discovered_server_url("http://127.0.0.1:5173/app?tab=1#top", Some("https://192.168.1.40:3773")),
            "http://192.168.1.40:5173/app?tab=1#top"
        );
        assert_eq!(
            resolve_discovered_server_url("http://localhost:3000", Some("http://mac-studio.local:3773")),
            "http://mac-studio.local:3000/"
        );
        assert_eq!(
            resolve_discovered_server_url("http://[::1]:4000", Some("http://[fd00::1]:3773")),
            "http://[fd00::1]:4000/"
        );
        // Public hosts or loopback environments keep the URL unchanged.
        assert_eq!(
            resolve_discovered_server_url("http://127.0.0.1:5173", Some("https://relay.example.com")),
            "http://127.0.0.1:5173/"
        );
        assert_eq!(
            resolve_discovered_server_url("http://127.0.0.1:5173", Some("http://localhost:3773")),
            "http://127.0.0.1:5173/"
        );
    }

    #[test]
    fn tasks_filter_by_thread_id_and_format_schedule_line() {
        let list = crate::settings::scheduled_tasks::decode_list(&json!({
            "tasks": [
                {
                    "id": "a",
                    "title": "Run A",
                    "prompt": "do a",
                    "enabled": true,
                    "schedule": { "type": "interval", "everyMs": 60000 },
                    "projectId": "p1",
                    "threadId": "t1",
                    "modelSelection": { "provider": "codex", "model": "gpt-5" }
                },
                {
                    "id": "b",
                    "title": "Run B",
                    "prompt": "do b",
                    "enabled": false,
                    "schedule": { "type": "webhook" },
                    "projectId": "p1",
                    "threadId": "t2",
                    "lastRunError": "boom",
                    "modelSelection": { "provider": "codex", "model": "gpt-5" }
                }
            ]
        }))
        .unwrap();
        let filtered = tasks_for_thread(&list, "t1");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "a");
        assert_eq!(automation_schedule_line(&list[0], |_| String::new(), 0), "Every 1 min");
        assert_eq!(automation_schedule_line(&list[1], |_| String::new(), 0), "On webhook \u{00b7} paused");
        let blocked = ScheduledTask { blocked_by_policy: Some("Turned off by organization".into()), ..list[0].clone() };
        assert_eq!(automation_schedule_line(&blocked, |_| String::new(), 0), "Every 1 min \u{00b7} Turned off by organization");
        let upcoming = ScheduledTask { next_run_at: Some("2026-01-01T00:10:00Z".into()), ..list[0].clone() };
        assert_eq!(
            automation_schedule_line(&upcoming, |_| String::new(), crate::sidebar::time::parse_iso("2026-01-01T00:00:00Z").unwrap()),
            "Every 1 min \u{00b7} next in 10m"
        );
        assert_eq!(schedule_time_zone_label(None, None), None);
        assert_eq!(schedule_time_zone_label(Some("Unsupported/Zone"), None), None);
        assert_eq!(schedule_time_zone_label(Some("Etc/UTC"), None).as_deref(), Some("UTC"));
        assert_eq!(schedule_time_zone_label(Some("America/Los_Angeles"), None).as_deref(), Some("PT"));
        assert_eq!(
            schedule_time_zone_label(Some("America/Los_Angeles"), Some("2027-01-15T17:00:00Z")).as_deref(),
            Some("PST")
        );
        assert_eq!(
            schedule_time_zone_label(Some("America/Los_Angeles"), Some("2027-07-15T16:00:00Z")).as_deref(),
            Some("PDT")
        );
        let fixed = ScheduledTask {
            id: "c".into(),
            title: "Morning".into(),
            prompt: "hi".into(),
            enabled: true,
            schedule: Schedule::FixedTime { time_of_day: "09:00".into(), weekdays: vec![1, 2, 3, 4, 5] },
            schedule_raw: json!({ "type": "fixed_time", "timeOfDay": "09:00", "weekdays": [1, 2, 3, 4, 5] }),
            project_id: "p1".into(),
            thread_id: Some("t1".into()),
            thread_mode: None,
            workspace_strategy: json!({}),
            model_selection: list[0].model_selection.clone(),
            runtime_mode: "full-access".into(),
            interaction_mode: "default".into(),
            next_run_at: Some("2027-01-15T17:00:00Z".into()),
            last_run_status: crate::settings::scheduled_tasks::RunStatus::Never,
            last_run_error: None,
            webhook: None,
            blocked_by_policy: None,
        };
        assert_eq!(
            automation_schedule_line_in(&fixed, |_| String::new(), Some("America/Los_Angeles"), crate::sidebar::time::parse_iso("2027-01-15T16:50:00Z").unwrap()),
            "Weekdays at 09:00 PST \u{00b7} next in 10m"
        );
    }

    #[test]
    fn header_helpers_handle_scratch_rename_link_origin_and_collapse() {
        assert_eq!(header_project_title("workspace", false), "workspace");
        assert_eq!(header_project_title("workspace", true), "No project");
        assert_eq!(resolve_rename_commit("Hello", "  Hello "), None);
        assert_eq!(resolve_rename_commit("Hello", "   "), None);
        assert_eq!(resolve_rename_commit("Hello", "  New Title "), Some("New Title".to_owned()));

        let origin = LinkOrigin { session_id: "s1".into(), label: "Mac Studio".into(), access: Some("read-only".into()) };
        assert_eq!(
            link_origin_badge(&origin),
            Some(("From Mac Studio".into(), "Started by an agent on Mac Studio through a linked environment.".into()))
        );
        assert!(should_collapse_parent_crumb(300., 360.));
        assert!(!should_collapse_parent_crumb(400., 360.));
    }

    #[test]
    fn remote_open_checks_host_environment_id_and_unavailable_reasons() {
        // Background service sharing desktop's T3 home (`pingdotgg/t3code#21661`).
        let bg_service = resolve_remote_open_state(&RemoteOpenInput {
            active_environment_id: Some("env-host"),
            host_environment_id: Some("env-host"),
            is_local_saved: false,
            http_base: Some("https://box.tailnet.ts.net"),
            ssh_target: None,
            remote_open_targets: &[],
        });
        assert_eq!(bg_service, RemoteOpenState::LocalExec);
        assert!(should_show_open_in_picker(true, Some("env-host"), Some("env-primary"), Some("env-host"), &bg_service));
        assert!(!should_show_open_in_picker(false, Some("env-host"), Some("env-primary"), Some("env-host"), &bg_service));

        // Remote SSH connection resolves to RemoteLinks and falls back when no desktop editors exist.
        let ssh_remote = resolve_remote_open_state(&RemoteOpenInput {
            active_environment_id: Some("env-remote"),
            host_environment_id: Some("env-host"),
            is_local_saved: false,
            http_base: Some("https://box.tailnet.ts.net"),
            ssh_target: Some(("alice@devbox", None)),
            remote_open_targets: &[],
        });
        let RemoteOpenState::RemoteLinks(host) = &ssh_remote else {
            panic!("expected RemoteLinks, got {ssh_remote:?}");
        };
        assert_eq!(host, &RemoteOpenHost { host: "devbox".into(), username: Some("alice".into()) });
        assert_eq!(
            open_in_unavailable_reason(&ssh_remote, 0, "Test host").as_deref(),
            Some("Open from a computer to edit on Test host")
        );
        assert_eq!(open_in_unavailable_reason(&ssh_remote, 1, "Test host"), None);

        let unavailable = resolve_remote_open_state(&RemoteOpenInput {
            active_environment_id: Some("env-remote"),
            host_environment_id: Some("env-host"),
            is_local_saved: false,
            http_base: Some("https://box.tailnet.ts.net"),
            ssh_target: None,
            remote_open_targets: &[],
        });
        assert_eq!(unavailable, RemoteOpenState::RemoteUnavailable);
        assert_eq!(
            open_in_unavailable_reason(&unavailable, 0, "Test host").as_deref(),
            Some("No SSH route to Test host")
        );
        assert_eq!(
            build_remote_open_url("vscode", host, "/repo/file.rs", true).as_deref(),
            Some("vscode://vscode-remote/ssh-remote+alice%40devbox/repo/file.rs:1:1")
        );
        assert_eq!(
            build_remote_open_url("zed", host, "/repo", false).as_deref(),
            Some("zed://ssh/alice@devbox/repo")
        );

        // Non-SSH remote (e.g. Tailscale/mDNS advertised via ServerConfig.remoteOpenTargets).
        let advertised = resolve_remote_open_state(&RemoteOpenInput {
            active_environment_id: Some("env-remote"),
            host_environment_id: Some("env-host"),
            is_local_saved: false,
            http_base: Some("https://box.tailnet.ts.net"),
            ssh_target: None,
            remote_open_targets: &[RemoteOpenHost { host: "box.tailnet.ts.net".into(), username: Some("dev".into()) }],
        });
        assert_eq!(advertised, RemoteOpenState::RemoteLinks(RemoteOpenHost { host: "box.tailnet.ts.net".into(), username: Some("dev".into()) }));
    }

    #[test]
    fn probe_remote_capable_editors_finds_path_bundles_and_rejects_openzfs_zed() {
        let dir = std::env::temp_dir().join(format!("t3-editor-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        let apps = dir.join("Applications");
        let toolbox = dir.join("toolbox");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(apps.join("Cursor.app/Contents/Resources/app/bin")).unwrap();
        std::fs::create_dir_all(apps.join("IntelliJ IDEA 2025.1.app/Contents/MacOS")).unwrap();
        std::fs::create_dir_all(&toolbox).unwrap();

        let make_exec = |path: &Path| {
            std::fs::write(path, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        };
        make_exec(&bin.join("code"));
        make_exec(&bin.join("zed"));
        make_exec(&apps.join("Cursor.app/Contents/Resources/app/bin/cursor"));
        make_exec(&apps.join("IntelliJ IDEA 2025.1.app/Contents/MacOS/idea"));
        make_exec(&toolbox.join("rustrover1"));

        // `check_zed` returns false to simulate OpenZFS `zed` on PATH when Zed.app is absent.
        let found = probe_remote_capable_editors_in(&[bin], &[apps], Some(&toolbox), true, &|_| false);
        assert_eq!(found, vec!["cursor", "vscode", "idea", "rustrover"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
