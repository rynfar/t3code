//! Pure layout, visibility, servers, automations, and header rules for the thread header and
//! details card (`ChatHeader.tsx`, `threadDetailsItems.ts`, `threadDetailsCardLayout.ts`,
//! `ThreadDetailsPanel.tsx`, `ThreadServersPanel.tsx`, `ThreadAutomationsPanel.tsx`).

use std::collections::HashMap;
use std::net::IpAddr;

use serde_json::{Value, json};
use url::Url;

use crate::preview::DiscoveredServer;
use crate::preview::address::normalize_preview_url;
use crate::settings::scheduled_tasks::{ScheduledTask, relative_run_label};
use crate::shell::LinkOrigin;

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

/// `taskScheduleLine` (`ThreadAutomationsPanel.tsx`): schedule + `blockedByPolicy`, or `next <rel>`, or `paused`.
pub fn automation_schedule_line(task: &ScheduledTask, once_label: impl Fn(i64) -> String, now_ms: i64) -> String {
    let schedule = task.schedule.label(once_label);
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
}
