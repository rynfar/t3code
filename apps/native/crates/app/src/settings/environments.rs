//! Environments (`ST-ENVS`): saved environments, `LinkedEnvironmentsSettings.tsx`,
//! `LoadBalancingSettings.tsx`, and `PullRequestRoutingSettings.tsx`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, Task,
    ParentElement, SharedString, Styled, Subscription, Window, div, prelude::*,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use serde_json::{Value, json};
use t3_client::routes::{CandidateRoute, ConnectionRouteKind, merge_learned_routes};
use t3_client::status::{self, Diagnosis, DotTone, HopResult};
use t3_client::updates::{self as update_client, UpdateDestination, UpdateEvent};
use t3_client::store::{SavedEnvironment, alternate_routes_snapshot, has_saved_relay_route, load_alternate_routes, load_ui_state, save_ui_state};
use t3_client::supervisor::ConnectionStatus;
use t3_core::client_settings::ClientSettingChange;
use t3_core::relay::EnvironmentHealth;
use t3_core::settings::ServerSettingChange;
use t3_core::updates::{self, ServerTarget, ServerUpdateStage};
use t3_core::pairing::{ANOTHER_ADDRESS, LinkableMachine, collect_known_machine_urls};
use t3_core::server_config::ServerConfig;
use t3_core::settings::access::ACCESS_READ as ACCESS_READ_SCOPE;
use t3_core::settings::{
    LoadPreference, PEER_LINKS_LINK_METHOD, PEER_LINKS_LIST_METHOD, PEER_LINKS_UNLINK_METHOD,
    PeerLinkHealth, PeerLinkSummary, PullRequestRoutingPermission,
    StoredPullRequestRoutingPermission, load_balancing_enabled_patch, load_balancing_weight_patch,
    load_preference_for_weight, peer_link_create_payload, peer_link_health, peer_link_status_text,
    peer_link_unlink_payload, pull_request_routing_connection_key,
    pull_request_routing_permission_for, read_load_balancing_enabled, read_load_balancing_weights,
    set_pull_request_routing_permission, summarize_load_preferences, summarize_pull_request_routing,
};
use t3_core::timeline::{can_manage_peer_links, link_access_description, link_access_label};

use super::controls::{
    Button, ButtonSize, ButtonVariant, DialogFrame, Row, Select, SelectOption, Switch,
    dialog_footer, group, icon, muted_text, r, section, section_title,
};
use super::{Block, SettingsView, folded_section, store};
use crate::app_state::Phase;
use crate::components::motion::{Spinner, StatusDot};
use crate::environments::PairingSource;
use crate::i18n::t;
use crate::theme::{ThemeSlot, ThemeSlotExt};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const PEER_LINKS_STALE_AFTER: Duration = Duration::from_secs(10);
const PR_ROUTING_STORE_KEY: &str = "pull-request-routing-permissions";
const ACCESS_LEVELS: [&str; 4] = ["read-only", "approval-required", "auto-accept-edits", "full-access"];

#[derive(Debug, Clone)]
enum PeerLinksLoad {
    Loading,
    Loaded { links: Vec<PeerLinkSummary>, fetched_at: Instant },
    Failed(String),
}

struct LinkEnvironmentDialog {
    host_saved_id: String,
    host_env_id: String,
    choice: Option<String>,
    address: Entity<InputState>,
    pairing_code: Entity<InputState>,
    access: String,
    busy: bool,
    error: Option<String>,
    mint_failed: HashSet<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

struct ForgetPeerLinkConfirm {
    host_saved_id: String,
    target_env_id: String,
    target_label: String,
    busy: bool,
    error: Option<String>,
    focus: FocusHandle,
}

enum CatalogDialog {
    Add {
        address: Entity<InputState>,
        code: Entity<InputState>,
        username: Entity<InputState>,
        port: Entity<InputState>,
        source: PairingSource,
        submitted: bool,
        focus: FocusHandle,
        _subscriptions: Vec<Subscription>,
    },
    Confirm { action: CatalogAction, focus: FocusHandle },
    AddRoute {
        saved_id: String,
        url: Entity<InputState>,
        code: Entity<InputState>,
        error: Option<String>,
        focus: FocusHandle,
        _subscriptions: Vec<Subscription>,
    },
}

enum CatalogAction {
    Local { enable: bool },
    Remove { id: String, label: String },
    RemoveRoute { id: String, route: CandidateRoute },
    Update(Vec<ServerTarget>),
}

#[derive(Default)]
struct EnvironmentSections {
    routes: bool,
    permissions: bool,
    diagnosis: bool,
    menu: bool,
    icon: bool,
}

#[derive(Default)]
struct CatalogState {
    rows: HashMap<String, EnvironmentSections>,
    dialog: Option<CatalogDialog>,
    update_stages: HashMap<String, ServerUpdateStage>,
    retry_ticker: Option<Task<()>>,
}

pub(crate) struct EnvironmentsState {
    catalog: CatalogState,
    peer_links: HashMap<String, PeerLinksLoad>,
    peer_links_seq: HashMap<String, u64>,
    link_dialog: Option<LinkEnvironmentDialog>,
    forget_confirm: Option<ForgetPeerLinkConfirm>,
    pub(super) load_balancing_open: bool,
    pub(super) pr_routing_open: bool,
    pr_routing_permissions: Vec<StoredPullRequestRoutingPermission>,
}

impl EnvironmentsState {
    pub(super) fn new() -> Self {
        let pr_routing_permissions = load_ui_state(PR_ROUTING_STORE_KEY)
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        Self {
            catalog: CatalogState::default(),
            peer_links: HashMap::new(),
            peer_links_seq: HashMap::new(),
            link_dialog: None,
            forget_confirm: None,
            load_balancing_open: false,
            pr_routing_open: false,
            pr_routing_permissions,
        }
    }

    pub(super) fn leave(&mut self) {
        self.catalog.dialog = None;
        self.catalog.retry_ticker = None;
        self.link_dialog = None;
        self.forget_confirm = None;
    }

    pub(super) fn reveal(&mut self, target: &str) {
        match target {
            "load-balancing" => self.load_balancing_open = true,
            "pull-request-routing" => self.pr_routing_open = true,
            _ => {}
        }
    }

    pub(super) fn inputs(&self) -> Vec<&Entity<InputState>> {
        let mut out = Vec::new();
        match &self.catalog.dialog {
            Some(CatalogDialog::Add { address, code, username, port, source, .. }) => {
                out.push(address);
                if *source == PairingSource::Link {
                    out.push(code);
                } else {
                    out.push(username);
                    out.push(port);
                }
            }
            Some(CatalogDialog::AddRoute { url, code, .. }) => {
                out.push(url);
                out.push(code);
            }
            _ => {}
        }
        if let Some(dialog) = &self.link_dialog {
            out.push(&dialog.address);
            out.push(&dialog.pairing_code);
        }
        out
    }
}

struct PeerLinkHost {
    saved_id: String,
    environment_id: String,
    label: String,
    can_manage: bool,
}

struct EligibleEnvironment {
    saved_id: String,
    environment_id: String,
    label: String,
    subtitle: String,
    icon: Icon,
    connection_key: Option<String>,
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn saved_env_connection_key(env_id: &str, saved: &SavedEnvironment) -> Option<String> {
    let ssh = saved.ssh.as_ref().map(|target| {
        (
            target.alias.as_str(),
            target.hostname.as_str(),
            target.username.as_deref(),
            target.port,
        )
    });
    pull_request_routing_connection_key(env_id, &saved.http_base, ssh)
}

fn transport_subtitle(saved: &SavedEnvironment, config: Option<&ServerConfig>) -> String {
    if saved.local {
        return "This machine".to_owned();
    }
    if saved.ssh.is_some() {
        return saved.address();
    }
    if saved.is_relay_managed() {
        return "T3 Connect".to_owned();
    }
    let route = CandidateRoute::saved(&saved.http_base);
    if route.kind() == ConnectionRouteKind::Tailnet {
        return "via Tailscale".to_owned();
    }
    // Linux only advertises a Windows file manager when the server detects WSL interop.
    if config.is_some_and(|config| {
        config.shell_reveal_in_file_manager_kind.as_deref() == Some("file-explorer")
            && config.environment.platform.as_ref().is_some_and(|platform| platform.os == "linux")
    }) {
        return "WSL".to_owned();
    }
    saved.address()
}

const MACHINE_KINDS: [(&str, &str); 7] = [
    ("server", "Server"),
    ("cloud", "Cloud VM"),
    ("linux", "Linux/WSL"),
    ("desktop", "Desktop"),
    ("laptop", "Laptop"),
    ("mac-mini", "Mini PC"),
    ("mac-studio", "Workstation"),
];

fn machine_icon_options(config: Option<&ServerConfig>) -> (&str, &str, bool) {
    let reported = config.and_then(|config| config.environment.platform.as_ref())
        .map(|platform| platform.machine.as_str())
        .filter(|machine| MACHINE_KINDS.iter().any(|(kind, _)| kind == machine));
    let detected = reported.unwrap_or("server");
    let selected = config.and_then(|config| config.settings.environment_icon.as_deref())
        .filter(|machine| MACHINE_KINDS.iter().any(|(kind, _)| kind == machine))
        .unwrap_or(detected);
    (selected, detected, reported.is_some())
}

pub(crate) fn can_add_direct_route(saved: &SavedEnvironment) -> bool {
    !saved.local && saved.ssh.is_none()
}

pub(crate) fn parse_direct_route_url(raw_url: &str, code: Option<&str>) -> Result<String, String> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return Err("Enter a server URL or pairing link.".to_owned());
    }
    if let Ok(link) = t3_core::pairing::parse_pairing_link(trimmed, code) {
        return Ok(link.http_base);
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("http://{trimmed}")
    };
    t3_core::pairing::origin_of(&with_scheme)
        .ok_or_else(|| "Enter a valid http:// or https:// server URL.".to_owned())
}

fn route_choices(saved: &SavedEnvironment, routes: &[CandidateRoute], reported: &[t3_client::routes::ReportedEndpoint]) -> Vec<CandidateRoute> {
    let Some(environment_id) = saved.environment_id.as_deref() else { return Vec::new() };
    let primary = CandidateRoute::saved(&saved.http_base);
    let current: Vec<_> = std::iter::once(primary.clone()).chain(routes.iter().cloned()).collect();
    let offered = merge_learned_routes(environment_id, &current, &primary.id, reported, true).unwrap_or(current);
    let advertised = |candidate: &CandidateRoute| reported.iter().any(|endpoint| {
        t3_core::pairing::origin_of(&endpoint.http_base_url).as_deref() == Some(&candidate.http_base)
    });
    offered.into_iter().filter(|candidate| candidate.learned && advertised(candidate)).collect()
}

fn move_alternate_route(routes: &mut [CandidateRoute], id: &str, offset: isize) -> bool {
    let Some(index) = routes.iter().position(|route| route.id == id) else { return false };
    let Some(destination) = index.checked_add_signed(offset).filter(|next| *next < routes.len()) else { return false };
    routes.swap(index, destination);
    true
}

fn catalog_button(id: impl Into<gpui::ElementId>, label: &str, cx: &App) -> Button {
    Button::new(id, ButtonVariant::GhostMuted, ButtonSize::Xs).label(t(cx, label))
}

fn catalog_input(input: &Entity<InputState>, busy: bool, cx: &App) -> impl IntoElement {
    let background = crate::theme::theme_slot_clipped_background_layer(ThemeSlot::Input, r(8.), cx);
    div().relative().children(background)
        .child(Input::new(input).small().disabled(busy).theme_slot(ThemeSlot::Input, cx))
}

impl SettingsView {
    pub(super) fn dismiss_environments_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.dismiss_access_dialog(window, cx) {
            return true;
        }
        if self.environments.catalog.dialog.is_some() {
            if self.state.read(cx).connect.pairing.is_none() {
                self.environments.catalog.dialog = None;
                self.focus.focus(window, cx);
                cx.notify();
            }
            return true;
        }
        if let Some(dialog) = &self.environments.link_dialog {
            if dialog.busy {
                return true;
            }
            self.environments.link_dialog = None;
            self.focus.focus(window, cx);
            cx.notify();
            return true;
        }
        if let Some(confirm) = &self.environments.forget_confirm {
            if confirm.busy {
                return true;
            }
            self.environments.forget_confirm = None;
            self.focus.focus(window, cx);
            cx.notify();
            return true;
        }
        if let Some(id) = self.environments.catalog.rows.iter().find(|(_, row)| row.menu).map(|(id, _)| id.clone()) {
            self.close_environment_menu(&id, cx);
            return true;
        }
        false
    }

    fn peer_link_hosts(&self, cx: &App) -> Vec<PeerLinkHost> {
        let app = self.state.read(cx);
        let mut hosts = Vec::new();
        let mut seen = HashSet::new();
        for view in &app.fleet.environments {
            if view.status != ConnectionStatus::Connected {
                continue;
            }
            let Some(cfg) = view.config.config.as_ref() else {
                continue;
            };
            if !cfg.settings.threads.linked_environments_enabled {
                continue;
            }
            let can_read = view
                .grants
                .as_ref()
                .is_none_or(|g| g.grants(ACCESS_READ_SCOPE));
            if !can_read {
                continue;
            }
            let saved_id = view.summary.owner.saved_id.clone();
            if !seen.insert(saved_id.clone()) {
                continue;
            }
            let label = if !cfg.environment.label.trim().is_empty() {
                cfg.environment.label.clone()
            } else {
                saved_id.clone()
            };
            hosts.push(PeerLinkHost {
                saved_id,
                environment_id: cfg.environment.environment_id.clone(),
                label,
                can_manage: can_manage_peer_links(view.grants.as_ref()),
            });
        }
        if hosts.is_empty()
            && let Some(cfg) = app.server_config.config.as_ref()
            && cfg.settings.threads.linked_environments_enabled
        {
            let grants = app
                .client_handle()
                .and_then(|c| c.session_state().borrow().clone());
            let can_read = grants.as_ref().is_none_or(|g| g.grants(ACCESS_READ_SCOPE));
            if can_read {
                let saved_id = app
                    .environments
                    .active
                    .clone()
                    .unwrap_or_else(|| cfg.environment.environment_id.clone());
                let label = if !cfg.environment.label.trim().is_empty() {
                    cfg.environment.label.clone()
                } else {
                    "Environment".to_owned()
                };
                hosts.push(PeerLinkHost {
                    saved_id,
                    environment_id: cfg.environment.environment_id.clone(),
                    label,
                    can_manage: can_manage_peer_links(grants.as_ref()),
                });
            }
        }
        hosts
    }

    fn eligible_multi_environments(&self, cx: &App) -> Vec<EligibleEnvironment> {
        let app = self.state.read(cx);
        let active_id = app.environments.active.as_deref();
        let mut out = Vec::new();
        for saved in &app.environments.environments {
            if saved.disabled {
                continue;
            }
            let fleet_view = app.fleet.environment(&saved.id);
            let config: Option<&ServerConfig> = fleet_view
                .and_then(|v| v.config.config.as_ref())
                .or_else(|| (active_id == Some(saved.id.as_str())).then_some(app.server_config.config.as_ref()).flatten());
            let env_id = config
                .map(|c| c.environment.environment_id.clone())
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| saved.id.clone());
            let label = config
                .map(|c| c.environment.label.clone())
                .filter(|l| !l.trim().is_empty())
                .or_else(|| (!saved.label.trim().is_empty()).then(|| saved.label.clone()))
                .unwrap_or_else(|| saved.address());
            let machine_icon = config.map(Icon::machine).unwrap_or(Icon::Server);
            let connection_key = saved_env_connection_key(&env_id, saved);
            out.push(EligibleEnvironment {
                saved_id: saved.id.clone(),
                environment_id: env_id,
                label,
                subtitle: transport_subtitle(saved, config),
                icon: machine_icon,
                connection_key,
            });
        }
        out
    }

    /// Refreshes peer links for every peer-link host when showing Connections or returning to the
    /// window so links created outside the app appear immediately without interrupting an in-flight
    /// load (`pingdotgg/t3code#28907`).
    pub(super) fn refresh_visible_peer_links(&mut self, cx: &mut Context<Self>) {
        let hosts: Vec<String> = self
            .peer_link_hosts(cx)
            .into_iter()
            .map(|host| host.saved_id)
            .collect();
        for host_id in hosts {
            if matches!(self.environments.peer_links.get(&host_id), Some(PeerLinksLoad::Loading)) {
                continue;
            }
            self.refresh_peer_links(&host_id, true, cx);
        }
    }

    fn refresh_peer_links(&mut self, host_saved_id: &str, force: bool, cx: &mut Context<Self>) {
        if !force {
            match self.environments.peer_links.get(host_saved_id) {
                Some(PeerLinksLoad::Loading) => return,
                Some(PeerLinksLoad::Loaded { fetched_at, .. })
                    if fetched_at.elapsed() < PEER_LINKS_STALE_AFTER =>
                {
                    return;
                }
                _ => {}
            }
        }
        let seq = self.environments.peer_links_seq.entry(host_saved_id.to_owned()).or_insert(0);
        *seq += 1;
        let current_seq = *seq;
        self.environments
            .peer_links
            .insert(host_saved_id.to_owned(), PeerLinksLoad::Loading);

        let app = self.state.read(cx);
        let is_active = app.environments.active.as_deref() == Some(host_saved_id)
            || app
                .server_config
                .config
                .as_ref()
                .is_some_and(|c| c.environment.environment_id == host_saved_id);
        let active_client = is_active.then(|| app.client_handle()).flatten();
        let lookup = (!is_active).then(|| app.connections.client(host_saved_id));
        let runtime = app.runtime().clone();
        let host_key = host_saved_id.to_owned();

        let task = runtime.spawn(async move {
            let client = match active_client {
                Some(client) => client,
                None => match lookup {
                    Some(fut) => fut.await.ok_or_else(|| "Environment is not connected.".to_owned())?,
                    None => return Err("Environment is not connected.".to_owned()),
                },
            };
            let reply = client
                .request(PEER_LINKS_LIST_METHOD, json!({}))
                .await
                .map_err(|err| err.to_string())?;
            let raw_links = reply
                .get("links")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            serde_json::from_value::<Vec<PeerLinkSummary>>(raw_links)
                .map_err(|err| format!("Invalid peer links payload: {err}"))
        });

        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Failed to load linked environments.".to_owned()));
            let _ = this.update(cx, |this, cx| {
                if this.environments.peer_links_seq.get(&host_key).copied() != Some(current_seq) {
                    return;
                }
                match result {
                    Ok(links) => {
                        this.environments.peer_links.insert(
                            host_key,
                            PeerLinksLoad::Loaded { links, fetched_at: Instant::now() },
                        );
                    }
                    Err(err) => {
                        this.environments.peer_links.insert(host_key, PeerLinksLoad::Failed(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_link_dialog(
        &mut self,
        host_saved_id: String,
        host_env_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("https://box.example.ts.net"));
        let pairing_code = cx.new(|cx| {
            InputState::new(window, cx).placeholder("From Sharing & access there, or t3 pair")
        });
        let subscriptions = vec![
            cx.subscribe_in(&address, window, |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Some(dialog) = this.environments.link_dialog.as_mut()
                        && dialog.choice.is_none()
                        && !input.read(cx).value().is_empty()
                    {
                        dialog.choice = Some(ANOTHER_ADDRESS.to_owned());
                    }
                    cx.notify();
                }
            }),
            cx.subscribe_in(&pairing_code, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let refresh_host = host_saved_id.clone();
        self.environments.link_dialog = Some(LinkEnvironmentDialog {
            host_saved_id,
            host_env_id,
            choice: None,
            address,
            pairing_code,
            access: "approval-required".to_owned(),
            busy: false,
            error: None,
            mint_failed: HashSet::new(),
            focus,
            _subscriptions: subscriptions,
        });
        self.refresh_peer_links(&refresh_host, false, cx);
        let should_discover_cloud = {
            let app = self.state.read(cx);
            app.cloud.identity.is_some()
                && app.cloud.discovery.environments.is_empty()
                && !app.cloud.discovery.refreshing
                && !app.cloud.busy
        };
        if should_discover_cloud {
            self.state.update(cx, |state, cx| state.refresh_cloud(cx));
        }
        cx.notify();
    }

    fn linkable_machines_for(
        &self,
        host_saved_id: &str,
        host_env_id: &str,
        cx: &App,
    ) -> Vec<LinkableMachine> {
        let app = self.state.read(cx);
        let linked_ids: HashSet<&str> = match self.environments.peer_links.get(host_saved_id) {
            Some(PeerLinksLoad::Loaded { links, .. }) => {
                links.iter().map(|link| link.environment_id.as_str()).collect()
            }
            _ => HashSet::new(),
        };
        let mut machines = Vec::new();
        let mut seen = HashSet::new();
        for saved in &app.environments.environments {
            if saved.disabled || saved.id == host_saved_id {
                continue;
            }
            let fleet_view = app.fleet.environment(&saved.id);
            let cfg = fleet_view.and_then(|v| v.config.config.as_ref());
            let env_id = cfg
                .map(|c| c.environment.environment_id.clone())
                .filter(|id| !id.is_empty())
                .or_else(|| saved.environment_id.clone())
                .unwrap_or_else(|| saved.id.clone());
            if env_id == host_env_id || !seen.insert(env_id.clone()) {
                continue;
            }
            let label = cfg
                .map(|c| c.environment.label.clone())
                .filter(|l| !l.trim().is_empty())
                .or_else(|| (!saved.label.trim().is_empty()).then(|| saved.label.clone()))
                .unwrap_or_else(|| saved.address());
            let machine = cfg.and_then(|c| {
                c.settings
                    .environment_icon
                    .clone()
                    .or_else(|| c.environment.platform.as_ref().map(|p| p.machine.clone()))
            });
            let extras = t3_client::supervisor::server_config_extras_for(&saved.http_base)
                .borrow()
                .clone();
            let mut raw_urls: Vec<&str> =
                extras.direct_endpoints.iter().map(String::as_str).collect();
            raw_urls.push(saved.http_base.as_str());
            let urls = collect_known_machine_urls(raw_urls);
            let connected = fleet_view.is_some_and(|v| v.status == ConnectionStatus::Connected);
            let linked = linked_ids.contains(env_id.as_str());
            machines.push(LinkableMachine {
                environment_id: env_id,
                label,
                machine,
                urls,
                connected,
                connect_only: false,
                linked,
            });
        }
        for cloud_env in &app.cloud.discovery.environments {
            if cloud_env.environment_id == host_env_id
                || !seen.insert(cloud_env.environment_id.clone())
            {
                continue;
            }
            let linked = linked_ids.contains(cloud_env.environment_id.as_str());
            machines.push(LinkableMachine {
                environment_id: cloud_env.environment_id.clone(),
                label: cloud_env.label.clone(),
                machine: Some("cloud".to_owned()),
                urls: Vec::new(),
                connected: false,
                connect_only: true,
                linked,
            });
        }
        machines
    }

    fn can_mint_on_machine(&self, machine: &LinkableMachine, needs_address: bool, cx: &App) -> bool {
        !needs_address
            && machine.connected
            && self.state.read(cx).can_mint_pairing_code_on(&machine.environment_id)
    }

    fn submit_link_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.environments.link_dialog.as_ref() else {
            return;
        };
        if dialog.busy {
            return;
        }
        let host_saved_id = dialog.host_saved_id.clone();
        let host_env_id = dialog.host_env_id.clone();
        let machines = self.linkable_machines_for(&host_saved_id, &host_env_id, cx);
        let selected = dialog
            .choice
            .clone()
            .or_else(|| machines.is_empty().then(|| ANOTHER_ADDRESS.to_owned()));
        let Some(selected) = selected else {
            return;
        };
        let machine = machines.iter().find(|m| m.environment_id == selected).cloned();
        let needs_address = machine.as_ref().is_none_or(|m| m.urls.is_empty());
        let typed_address = dialog.address.read(cx).value().trim().to_owned();
        let typed_code = dialog.pairing_code.read(cx).value().trim().to_owned();
        let access = dialog.access.clone();

        let (url, alternate_urls, expected_id, expected_label) = match (&machine, needs_address) {
            (Some(m), false) => {
                let first = m.urls[0].clone();
                let rest = m.urls[1..].to_vec();
                (first, rest, Some(m.environment_id.clone()), Some(m.label.clone()))
            }
            _ => {
                if typed_address.is_empty() {
                    return;
                }
                (typed_address, Vec::new(), None, None)
            }
        };

        let can_mint = machine.as_ref().is_some_and(|m| {
            !dialog.mint_failed.contains(&m.environment_id)
                && self.can_mint_on_machine(m, needs_address, cx)
        });
        if !can_mint && typed_code.is_empty() {
            return;
        }

        if let Some(dialog) = self.environments.link_dialog.as_mut() {
            dialog.busy = true;
            dialog.error = None;
        }
        cx.notify();

        let app = self.state.read(cx);
        let is_active = app.environments.active.as_deref() == Some(host_saved_id.as_str())
            || app
                .server_config
                .config
                .as_ref()
                .is_some_and(|c| c.environment.environment_id == host_saved_id);
        let active_client = is_active.then(|| app.client_handle()).flatten();
        let lookup = (!is_active).then(|| app.connections.client(&host_saved_id));
        let runtime = app.runtime().clone();
        let mint_target_id = machine.as_ref().map(|m| m.environment_id.clone());
        let mint_future = can_mint.then(|| {
            let env_id = machine
                .as_ref()
                .expect("machine present when can_mint is true")
                .environment_id
                .clone();
            app.mint_pairing_code_on(&env_id, &access)
        });

        let task = runtime.spawn(async move {
            let code = match mint_future {
                Some(fut) => fut.await.map_err(|err| (true, err.to_string()))?,
                None => typed_code,
            };

            let client = match active_client {
                Some(client) => client,
                None => match lookup {
                    Some(fut) => fut
                        .await
                        .ok_or_else(|| (false, "Host environment is not connected.".to_owned()))?,
                    None => return Err((false, "Host environment is not connected.".to_owned())),
                },
            };
            let payload = peer_link_create_payload(
                &url,
                &alternate_urls,
                &code,
                &access,
                expected_id.as_deref(),
                expected_label.as_deref(),
            );
            client
                .request(PEER_LINKS_LINK_METHOD, payload)
                .await
                .map_err(|err| (false, err.to_string()))?;
            Ok(())
        });

        let refresh_host = host_saved_id.clone();
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_| Err((false, "Could not link the environment.".to_owned())));
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.environments.link_dialog = None;
                    this.refresh_peer_links(&refresh_host, true, cx);
                    cx.notify();
                }
                Err((mint_err, msg)) => {
                    if let Some(dialog) = this.environments.link_dialog.as_mut() {
                        dialog.busy = false;
                        dialog.error = Some(msg);
                        if mint_err
                            && let Some(id) = mint_target_id
                        {
                            dialog.mint_failed.insert(id);
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn confirm_forget_peer_link(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.environments.forget_confirm.as_mut() else {
            return;
        };
        if confirm.busy {
            return;
        }
        confirm.busy = true;
        confirm.error = None;
        let host_saved_id = confirm.host_saved_id.clone();
        let target_env_id = confirm.target_env_id.clone();
        cx.notify();

        let app = self.state.read(cx);
        let is_active = app.environments.active.as_deref() == Some(host_saved_id.as_str())
            || app
                .server_config
                .config
                .as_ref()
                .is_some_and(|c| c.environment.environment_id == host_saved_id);
        let active_client = is_active.then(|| app.client_handle()).flatten();
        let lookup = (!is_active).then(|| app.connections.client(&host_saved_id));
        let runtime = app.runtime().clone();

        let task = runtime.spawn(async move {
            let client = match active_client {
                Some(client) => client,
                None => match lookup {
                    Some(fut) => fut.await.ok_or_else(|| "Environment is not connected.".to_owned())?,
                    None => return Err("Environment is not connected.".to_owned()),
                },
            };
            client
                .request(PEER_LINKS_UNLINK_METHOD, peer_link_unlink_payload(&target_env_id))
                .await
                .map_err(|err| err.to_string())?;
            Ok(())
        });

        let refresh_host = host_saved_id.clone();
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Could not forget the link.".to_owned()));
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.environments.forget_confirm = None;
                    this.refresh_peer_links(&refresh_host, true, cx);
                    cx.notify();
                }
                Err(err) => {
                    if let Some(confirm) = this.environments.forget_confirm.as_mut() {
                        confirm.busy = false;
                        confirm.error = Some(err);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn catalog_blocks(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let state = self.state.read(cx);
        if let Some(CatalogDialog::Add { source, submitted: true, .. }) = self.environments.catalog.dialog.as_ref()
            && state.connect.pairing.is_none()
            && crate::connect::pairing_error(&state.connect, *source).is_none()
        {
            self.environments.catalog.dialog = None;
        }

        let settings = cx.entity();
        let enabled = state.local_server_auto_start;
        let local = Row::new("local-environment", t(cx, "Local environment"))
            .description(t(cx, if enabled {
                "Run agents on this computer."
            } else {
                "Automatic startup is off for this app."
            }))
            .highlighted(self.is_target("local-environment"))
            .control(Switch::new("local-environment-switch", enabled)
                .disabled(state.local_host_phase.is_starting() || state.local_host_phase.is_stopping())
                .on_change(move |enable, window, cx| {
                    settings.update(cx, |this, cx| {
                        this.show_catalog_confirmation(CatalogAction::Local { enable }, window, cx);
                    });
                }));
        let local = Block::new(&["local-environment"], section(t(cx, "This app"), vec![local.into_any_element()], cx));

        let targets = self.catalog_update_targets(cx);
        let can_update = !targets.is_empty();
        let all_targets = targets.clone();
        let updating = !self.environments.catalog.update_stages.is_empty() || crate::updates::has_active_update_run(cx);
        let actions = h_flex().gap(r(6.))
            .child(Button::new("settings-update-all", ButtonVariant::Outline, ButtonSize::Xs)
                .icon(Icon::RefreshCw)
                .label(t(cx, if updating { "Updating…" } else { "Update all" }))
                .disabled(!can_update || updating)
                .tooltip(t(cx, if updating { "An update is already running." } else if !can_update { "No eligible server updates." } else { "Update eligible environments." }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.show_catalog_confirmation(CatalogAction::Update(all_targets.clone()), window, cx);
                })))
            .child(Button::new("settings-add-environment", ButtonVariant::Default, ButtonSize::Xs)
                .icon(Icon::Plus).label(t(cx, "Add environment"))
                .on_click(cx.listener(|this, _, window, cx| this.open_catalog_add(window, cx))));
        let mut ticking = false;
        let saved = state.environments.environments.clone();
        self.environments.catalog.rows.retain(|id, _| saved.iter().any(|entry| &entry.id == id));
        let alternates = alternate_routes_snapshot();
        let rows: Vec<_> = saved.iter().map(|entry| {
            let configured = alternates.get(&entry.id).map(Vec::as_slice).unwrap_or_default();
            let progress = status::route_progress(&entry.http_base);
            ticking |= !entry.disabled && progress.retry_at.is_some_and(|deadline| deadline > Instant::now());
            self.catalog_environment_row(entry, configured, progress, &targets, cx)
        }).collect();
        let cloud = self.catalog_cloud_rows(cx);
        let heading = h_flex().w_full().gap(r(8.)).justify_between()
            .child(section_title(t(cx, "Environments")))
            .child(actions);
        let catalog = v_flex().gap(r(8.)).child(heading)
            .when(!rows.is_empty(), |this| this.child(group(rows, cx)))
            .child(cloud);
        if ticking && self.environments.catalog.retry_ticker.is_none() {
            self.environments.catalog.retry_ticker = Some(cx.spawn(async |this, cx| {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let _ = this.update(cx, |this, cx| {
                    this.environments.catalog.retry_ticker = None;
                    cx.notify();
                });
            }));
        }
        vec![local, Block::new(&["remote-environments", "connections", "server-version"], catalog)]
    }

    fn catalog_update_targets(&self, cx: &App) -> Vec<ServerTarget> {
        let state = self.state.read(cx);
        let machines = state.fleet.environments.iter().map(|entry| {
            let id = &entry.summary.owner.saved_id;
            let config = entry.config.config.as_ref();
            let saved = state.environments.get(id);
            updates::Machine {
                id,
                label: config.map(|config| config.environment.label.as_str()).filter(|label| !label.is_empty())
                    .or_else(|| saved.map(|saved| saved.label.as_str())).unwrap_or(id),
                config,
                connected: entry.status == ConnectionStatus::Connected && entry.summary.synchronized && entry.summary.config_loaded,
                hosted_by_this_app: saved.is_some_and(|saved| saved.local) && state.is_hosting_local_server(),
                can_maintain: entry.grants.as_ref().is_some_and(|state| state.grants("environment:maintain")),
                can_manage_providers: false,
                updating: self.environments.catalog.update_stages.contains_key(id),
            }
        }).collect::<Vec<_>>();
        updates::collect(&machines, include_str!("../updates/TARGET_SERVER_VERSION").trim()).servers
    }

    fn start_catalog_updates(&mut self, targets: Vec<ServerTarget>, cx: &mut Context<Self>) {
        if !self.environments.catalog.update_stages.is_empty() || crate::updates::has_active_update_run(cx) {
            return;
        }
        let state = self.state.read(cx);
        let plan = updates::Plan { servers: targets.clone(), ..Default::default() };
        let captured = match update_client::capture(&plan, &state.fleet) {
            Ok(captured) => captured,
            Err(error) => return store::show_error(error, cx),
        };
        let scope = "environment:maintain";
        let destinations = captured.into_iter().map(|capture| {
            let destination = state.connections.destination(&capture.owner, scope);
            (capture, destination)
        }).collect::<Vec<_>>();
        let view = state.connections.view();
        let ids = targets.iter().map(|target| target.environment_id.clone()).collect::<Vec<_>>();
        for id in &ids {
            self.environments.catalog.update_stages.insert(id.clone(), ServerUpdateStage::Downloading);
        }
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let task = state.runtime().spawn(async move {
            let mut receipts = Vec::new();
            for (capture, destination) in destinations {
                receipts.push(UpdateDestination { capture, scope, receipt: destination.await });
            }
            update_client::retry_servers_with_progress(
                targets.into_iter().map(|target| (target, None)).collect(), receipts, view, Some(tx)
            ).await
        });
        cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                let _ = this.update(cx, |this, cx| {
                    match event {
                        UpdateEvent::ServerStage { environment_id, stage } => {
                            this.environments.catalog.update_stages.insert(environment_id, stage);
                        }
                        UpdateEvent::MachineSettled { environment_id, .. } => {
                            this.environments.catalog.update_stages.remove(&environment_id);
                        }
                        _ => return,
                    }
                    cx.notify();
                });
            }
            let failures = task.await.unwrap_or_else(|error| vec![error.to_string()]);
            let _ = this.update(cx, |this, cx| {
                for id in &ids {
                    this.environments.catalog.update_stages.remove(id);
                }
                if !failures.is_empty() {
                    store::show_error(failures.join("\n"), cx);
                }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn access_server_update_button(&self, id: &str, cx: &Context<Self>) -> Option<AnyElement> {
        if let Some(stage) = self.environments.catalog.update_stages.get(id).copied() {
            let label = match stage {
                ServerUpdateStage::Downloading => "Downloading update…",
                ServerUpdateStage::Installing => "Installing update…",
                ServerUpdateStage::Resuming => "Waiting for the server to reconnect…",
            };
            return Some(h_flex().gap(r(6.))
                .child(Spinner::new("access-server-update-spinner", gpui::px(13.), hsla(palette(cx).muted_foreground)))
                .child(muted_text(t(cx, label), cx)).into_any_element());
        }
        self.catalog_update_targets(cx).into_iter().find(|target| target.environment_id == id)
            .map(|target| self.catalog_update_button(target, cx).into_any_element())
    }

    fn catalog_update_button(&self, target: ServerTarget, cx: &Context<Self>) -> Button {
        let key = gpui::ElementId::from(SharedString::from(target.environment_id.clone()));
        Button::new((key, "update-server"), ButtonVariant::Outline, ButtonSize::Xs)
            .label(t(cx, "Update"))
            .disabled(!self.environments.catalog.update_stages.is_empty() || crate::updates::has_active_update_run(cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.show_catalog_confirmation(CatalogAction::Update(vec![target.clone()]), window, cx);
            }))
    }

    fn catalog_environment_row(
        &self,
        saved: &SavedEnvironment,
        alternate_routes: &[CandidateRoute],
        progress: status::RouteProgress,
        update_targets: &[ServerTarget],
        cx: &Context<Self>,
    ) -> AnyElement {
        let app = self.state.read(cx);
        let active = app.environments.active.as_deref() == Some(&saved.id);
        let view = app.fleet.environment(&saved.id);
        let status = app.environment_statuses.get(&saved.id).or_else(|| view.map(|view| &view.status))
            .or(match &app.phase { Phase::Online(status) if active => Some(status), _ => None });
        let config = view.and_then(|view| view.config.config.as_ref())
            .or_else(|| active.then_some(app.server_config.config.as_ref()).flatten());
        let grants = view.and_then(|view| view.grants.as_ref());
        let connected = status == Some(&ConnectionStatus::Connected) && !saved.disabled;
        let label = config.map(|config| config.environment.label.as_str()).filter(|label| !label.trim().is_empty()).unwrap_or(&saved.label);
        let version = config.map(|config| config.environment.server_version.as_str())
            .filter(|version| !version.is_empty()).or(saved.server_version.as_deref());
        let version = version.map_or_else(|| t(cx, "Unknown version").to_owned(), |version| format!("v{}", version.trim_start_matches('v')));
        let relay = saved.is_relay_managed();
        let diagnosis = if saved.disabled {
            None
        } else if relay {
            let health = saved.environment_id.as_ref().and_then(|id| app.cloud.statuses.get(id));
            status::diagnose_relay(status, health, label)
        } else {
            status.and_then(|status| status::diagnose(status, label))
        };
        let (tone, status_label, live) = if saved.disabled {
            (None, t(cx, "Off").to_owned(), false)
        } else if !saved.has_connection_route() {
            (Some(DotTone::Destructive), t(cx, "Needs pairing again").to_owned(), false)
        } else if let Some(status) = status {
            (Some(status.dot()), t(cx, &status.title()).to_owned(), matches!(status.phase(), status::ConnectionPhase::Connecting | status::ConnectionPhase::Reconnecting))
        } else {
            (None, t(cx, "Not connected").to_owned(), false)
        };
        let p = palette(cx);
        let dot_color = match tone {
            Some(DotTone::Success) => p.success,
            Some(DotTone::Warning) => p.warning,
            Some(DotTone::Destructive) => p.destructive,
            None => p.muted_foreground,
        };
        let row_state = self.environments.catalog.rows.get(&saved.id);
        let routes_open = row_state.is_some_and(|state| state.routes);
        let permissions_open = row_state.is_some_and(|state| state.permissions);
        let diagnosis_open = row_state.is_some_and(|state| state.diagnosis);
        let menu_open = row_state.is_some_and(|state| state.menu);
        let diagnosis_button = diagnosis.as_ref().map(|_| self.catalog_disclosure(&saved.id, "diagnosis", "Details", diagnosis_open, |row| &mut row.diagnosis, cx));
        let update = self.environments.catalog.update_stages.get(&saved.id).copied();
        let update_target = update_targets.iter().find(|target| target.environment_id == saved.id).cloned();
        let enable_id = saved.id.clone();
        let switch_id = saved.id.clone();
        let state = self.state.clone();
        let can_switch = saved.has_connection_route();
        let trailing = h_flex().gap(r(6.)).flex_none()
            .children(update.map(|_| Spinner::new((gpui::ElementId::from(SharedString::from(saved.id.clone())), "update"), gpui::px(13.), hsla(p.muted_foreground))))
            .children(update_target.map(|target| self.catalog_update_button(target, cx)))
            .when(!active && can_switch && !saved.disabled, |this| this.child(
                catalog_button((gpui::ElementId::from(SharedString::from(saved.id.clone())), "switch-to"), "Switch to", cx)
                    .on_click(move |_, _, cx| state.update(cx, |state, cx| state.connect_environment(&switch_id, cx))),
            ))
            .child(div().id((gpui::ElementId::from(SharedString::from(saved.id.clone())), "enable-hint"))
                .when(!can_switch, |el| el.tooltip(super::controls::tooltip(t(cx, "Pair this environment again to enable it.").into())))
                .child(Switch::new((gpui::ElementId::from(SharedString::from(saved.id.clone())), "enable"), !saved.disabled)
                    .disabled(!can_switch)
                    .on_change({
                        let state = self.state.clone();
                        move |enabled, _, cx| state.update(cx, |state, cx| state.set_saved_environment_enabled(&enable_id, enabled, cx))
                    })))
            .child(div().relative()
                .child(self.catalog_disclosure(&saved.id, "actions", "Environment actions", menu_open, |row| &mut row.menu, cx))
                .children(menu_open.then(|| div().absolute().bottom_0().right_0().child(
                    gpui::deferred(
                        gpui::anchored().anchor(gpui::Anchor::TopRight)
                            .snap_to_window_with_margin(gpui::px(8.))
                            .child(self.catalog_environment_actions(saved, config, status, cx))
                    ).with_priority(2)
                ))));
        let status_line = h_flex().gap(r(6.)).min_w_0()
            .child(StatusDot::new((gpui::ElementId::from(SharedString::from(saved.id.clone())), "connection"), gpui::px(8.), hsla(dot_color), live))
            .child(div().text_xs().truncate().child(status_label))
            .child(muted_text(format!("· {version}"), cx));
        let primary = CandidateRoute::primary(&saved.http_base, saved.ssh.is_some());
        let active_route = progress.active_route_id.as_deref()
            .or_else(|| (connected && alternate_routes.is_empty()).then_some(primary.id.as_str()));
        let subtitle = if !saved.local && !relay && active_route != Some(primary.id.as_str()) {
            active_route.and_then(|id| alternate_routes.iter().find(|route| route.id == id))
                .map(|route| if route.kind() == ConnectionRouteKind::Relay { t(cx, "T3 Connect").to_owned() } else { format!("via {}", route.label()) })
                .unwrap_or_else(|| transport_subtitle(saved, config))
        } else {
            transport_subtitle(saved, config)
        };
        let information = v_flex().min_w_0().flex_1().gap(r(2.))
            .child(div().truncate().font_weight(FontWeight::MEDIUM).text_sm().child(label.to_owned()))
            .child(div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(t(cx, &subtitle).to_owned()))
            .child(status_line)
            .child(h_flex().flex_wrap().gap(r(5.))
                .child(self.catalog_disclosure(&saved.id, "routes", "Routes", routes_open, |row| &mut row.routes, cx))
                .children(grants.filter(|_| connected).map(|_| self.catalog_disclosure(&saved.id, "permissions", "Permissions", permissions_open, |row| &mut row.permissions, cx)))
                .children(diagnosis_button));
        v_flex()
            .debug_selector(|| format!("settings-saved-environment-{}", saved.id))
            .w_full().gap(r(6.)).px(r(16.)).py(r(12.))
            .child(h_flex().w_full().items_start().gap(r(12.))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_start()
                        .gap(r(12.))
                        .when(saved.disabled, |el| el.opacity(0.6))
                        .child(div().pt(r(3.)).child(icon(config.map_or(Icon::Server, Icon::machine), 20., hsla(p.muted_foreground))))
                        .child(information),
                )
                .child(trailing))
            .children(update.map(|stage| muted_text(t(cx, match stage {
                ServerUpdateStage::Downloading => "Downloading update…",
                ServerUpdateStage::Installing => "Installing update…",
                ServerUpdateStage::Resuming => "Waiting for the server to reconnect…",
            }), cx)))
            .children(routes_open.then(|| self.catalog_routes(saved, &primary, alternate_routes, active_route, cx)))
            .children(grants.filter(|_| permissions_open && connected).map(|grants| super::access::session_permissions(Some(grants), cx)))
            .children(diagnosis.map(|diagnosis| self.catalog_diagnosis(saved, label, diagnosis, diagnosis_open, status, progress.retry_at, cx)))
            .into_any_element()
    }

    fn catalog_disclosure(
        &self, id: &str, key: &'static str, label: &str, open: bool,
        section: fn(&mut EnvironmentSections) -> &mut bool, cx: &Context<Self>,
    ) -> Button {
        let id: SharedString = id.to_owned().into();
        let button = if key == "actions" {
            let button = Button::new((gpui::ElementId::from(id.clone()), key), ButtonVariant::Ghost, ButtonSize::IconSm).icon(Icon::Ellipsis);
            // The tooltip would cover the menu's first item while it is open.
            if open { button } else { button.tooltip(t(cx, label)) }
        } else {
            catalog_button((gpui::ElementId::from(id.clone()), key), label, cx).icon(if open { Icon::ChevronDown } else { Icon::ChevronRight })
        };
        button.on_click(cx.listener(move |this, _, _, cx| {
            let value = section(this.environments.catalog.rows.entry(id.to_string()).or_default());
            *value = !open;
            cx.notify();
        }))
    }

    fn catalog_environment_actions(
        &self, saved: &SavedEnvironment, config: Option<&ServerConfig>, connection: Option<&ConnectionStatus>, cx: &Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let key: SharedString = saved.id.clone().into();
        let row = self.environments.catalog.rows.get(&saved.id);
        let icon_open = row.is_some_and(|row| row.icon);
        let routes_open = row.is_some_and(|row| row.routes);
        let id = saved.id.clone();
        let label = saved.label.clone();
        let trace = connection.and_then(ConnectionStatus::error).and_then(status::extract_trace_id).map(str::to_owned);
        let relay_base = self.state.read(cx).cloud.discovery.environments.iter()
            .find(|entry| saved.environment_id.as_deref() == Some(&entry.environment_id))
            .map(|entry| entry.endpoint.http_base_url.as_str());
        let mcp = t3_core::pairing::origin_of(relay_base.unwrap_or(&saved.http_base)).map(|origin| format!("{origin}/mcp"));
        let copy_button = |name: &'static str, label: &'static str, copied_label: &'static str, value: Option<String>| {
            value.map(|text| {
                let saved_id = saved.id.clone();
                catalog_button((gpui::ElementId::from(key.clone()), name), label, cx)
                    .justify_start()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        let title = t(cx, copied_label).to_owned();
                        this.state.update(cx, |state, cx| {
                            state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Success, title), cx);
                        });
                        this.close_environment_menu(&saved_id, cx);
                    }))
            })
        };
        let outside_id = saved.id.clone();
        v_flex().id((gpui::ElementId::from(key.clone()), "catalog-actions-popup"))
            .relative().occlude().mt(r(4.)).w(r(280.)).p(r(6.)).gap(r(3.))
            .rounded(r(8.)).border_1().border_color(hsla(p.dropdown_border)).bg(hsla(p.popover))
            .theme_slot(ThemeSlot::MenuPopup, cx)
            .children(crate::theme::theme_slot_clipped_background_layer(ThemeSlot::MenuPopup, r(8.), cx))
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| this.close_environment_menu(&outside_id, cx)))
            .child(self.catalog_disclosure(&saved.id, "icon", "Icon", icon_open, |row| &mut row.icon, cx).justify_start())
            .children(icon_open.then(|| self.catalog_icon_picker(saved, config, cx)))
            .child(catalog_button((gpui::ElementId::from(key.clone()), "menu-routes"), if routes_open { "Hide routes" } else { "Routes" }, cx)
                .justify_start().on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        let row = this.environments.catalog.rows.entry(id.clone()).or_default();
                        row.routes = !row.routes;
                        this.close_environment_menu(&id, cx);
                    }
                })))
            .children(copy_button("copy-mcp", "Copy MCP URL", "MCP URL copied", mcp))
            .children(copy_button("copy-trace", "Copy trace ID", "Trace ID copied", trace))
            .child(
                div()
                    .mt(r(4.))
                    .pt(r(4.))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .child(
                        Button::new((gpui::ElementId::from(key), "request-remove"), ButtonVariant::GhostDestructive, ButtonSize::Xs)
                            .label(t(cx, "Remove from this device…")).justify_start()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close_environment_menu(&id, cx);
                                this.show_catalog_confirmation(CatalogAction::Remove { id: id.clone(), label: label.clone() }, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn close_environment_menu(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(row) = self.environments.catalog.rows.get_mut(id) {
            row.menu = false;
            row.icon = false;
            cx.notify();
        }
    }

    fn catalog_icon_picker(&self, saved: &SavedEnvironment, config: Option<&ServerConfig>, cx: &Context<Self>) -> AnyElement {
        let app = self.state.read(cx);
        let owner = app.fleet.environment(&saved.id).filter(|entry| {
            entry.status == ConnectionStatus::Connected
                && entry.grants.as_ref().is_some_and(|grants| grants.grants("settings:write"))
        }).map(|entry| entry.summary.owner.clone());
        let lock = if config.is_none() {
            Some(t(cx, "Connect to this environment to change its icon."))
        } else if owner.is_none() {
            Some(t(cx, "Your session on this environment cannot change its settings."))
        } else {
            None
        };
        let (selected, detected, reported) = machine_icon_options(config);
        v_flex().items_start().gap(r(4.)).pt(r(4.))
            .children(lock.map(|reason| muted_text(reason, cx)))
            .child(h_flex().flex_wrap().gap(r(4.)).children(MACHINE_KINDS.iter().map(|&(kind, title)| {
                let label = if kind == detected {
                    format!("{} · {}", t(cx, title), t(cx, if reported { "detected" } else { "default" }))
                } else {
                    t(cx, title).to_owned()
                };
                let state = self.state.clone();
                let owner = owner.clone();
                let choice = (kind != detected).then_some(kind);
                let mut button = Button::new((gpui::ElementId::from(SharedString::from(saved.id.clone())), kind),
                    if kind == selected { ButtonVariant::Outline } else { ButtonVariant::GhostMuted }, ButtonSize::Xs)
                    .icon(crate::connect::environment_machine_icon(Some(kind))).label(label).disabled(lock.is_some());
                if let Some(reason) = lock {
                    button = button.tooltip(reason);
                }
                button.on_click(move |_, _, cx| {
                    if let Some(owner) = owner.clone() {
                        store::update_scoped_environments(&state, vec![owner], ServerSettingChange::Prepared(json!({
                            "patch": { "environmentIcon": choice },
                        })), None, cx);
                    }
                })
            })))
            .into_any_element()
    }

    fn catalog_routes(
        &self, saved: &SavedEnvironment, primary: &CandidateRoute, alternate_routes: &[CandidateRoute],
        active_route: Option<&str>, cx: &Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let key: SharedString = saved.id.clone().into();
        let refused = t3_client::supervisor::refused_routes_for(&saved.http_base).borrow().clone();
        let can_edit = !saved.local;
        let rows = std::iter::once((None, primary)).chain(alternate_routes.iter().enumerate().map(|(i, route)| (Some(i), route)));
        v_flex().pt(r(6.)).gap(r(5.)).border_t_1().border_color(hsla(p.border).opacity(0.5))
            .children(rows.map(|(index, route)| {
                let route_key: SharedString = format!("{key}-{}", route.id).into();
                let active = active_route == Some(&route.id);
                let refused = refused.contains(&route.id);
                let remove = route.clone();
                let id = saved.id.clone();
                let label = if index.is_none() && saved.is_relay_managed() { "T3 Connect".to_owned() } else { route.label() };
                h_flex().gap(r(4.)).w_full()
                    .child(v_flex().flex_1().min_w_0()
                        .child(h_flex().gap(r(5.)).flex_wrap().text_xs()
                            .child(div().font_weight(FontWeight::MEDIUM).child(t(cx, &label).to_owned()))
                            .when(active, |el| el.child(div().text_color(hsla(p.success_foreground)).child(t(cx, "In use"))))
                            .when(refused, |el| el.child(div().text_color(hsla(p.destructive_foreground)).child(t(cx, "Sign-in refused")))))
                        .child(div().text_xs().truncate().text_color(hsla(p.muted_foreground))
                            .child(if route.learned { format!("{} · {}", route.http_base, t(cx, "found automatically")) }
                                else if route.ssh { saved.address() } else { route.http_base.clone() })))
                    .when_some(index.filter(|_| can_edit && saved.ssh.is_none()), |el, index| {
                        el.children([(-1, "Move route up", Icon::ChevronUp), (1, "Move route down", Icon::ChevronDown)].map(|(offset, title, icon)| {
                            let saved_id = saved.id.clone();
                            let route_id = route.id.clone();
                            Button::new((gpui::ElementId::from(route_key.clone()), title), ButtonVariant::GhostMuted, ButtonSize::IconXs)
                                .icon(icon).tooltip(t(cx, title))
                                .disabled(index.checked_add_signed(offset).is_none_or(|destination| destination >= alternate_routes.len()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let mut alternates = load_alternate_routes().remove(&saved_id).unwrap_or_default();
                                    if move_alternate_route(&mut alternates, &route_id, offset) {
                                        this.state.update(cx, |state, cx| state.apply_settings_routes(&saved_id, alternates, cx));
                                    }
                                }))
                        }))
                    })
                    .when(index.is_some() && can_edit && !route.learned, |el| {
                        el.child(Button::new((gpui::ElementId::from(route_key), "remove"), ButtonVariant::GhostDestructive, ButtonSize::IconXs)
                            .icon(Icon::X).tooltip(t(cx, "Remove route"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_catalog_confirmation(CatalogAction::RemoveRoute { id: id.clone(), route: remove.clone() }, window, cx);
                            })))
                    })
            }))
            .child(div().child(self.catalog_add_route_button(saved, "Add route", cx)))
            .into_any_element()
    }

    fn catalog_add_route_button(&self, saved: &SavedEnvironment, label: &str, cx: &Context<Self>) -> Button {
        let available = can_add_direct_route(saved);
        let id = saved.id.clone();
        let mut button = catalog_button((gpui::ElementId::from(SharedString::from(id.clone())), SharedString::from(label.to_owned())), label, cx)
            .icon(Icon::Plus).disabled(!available);
        if !available {
            button = button.tooltip(t(cx, "SSH and local environments already use a fixed transport."));
        }
        button.on_click(cx.listener(move |this, _, window, cx| {
            let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://host:3773 or pairing link"));
            let code = cx.new(|cx| InputState::new(window, cx).placeholder("Optional pairing code"));
            let subscriptions = [&url, &code].map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::Change => {
                            if let Some(CatalogDialog::AddRoute { error, .. }) = this.environments.catalog.dialog.as_mut() {
                                *error = None;
                            }
                            cx.notify();
                        }
                        InputEvent::PressEnter { .. } => this.submit_catalog_add_route(window, cx),
                        _ => {}
                    }
                })
            });
            let focus = cx.focus_handle();
            url.read(cx).focus_handle(cx).focus(window, cx);
            this.environments.catalog.dialog = Some(CatalogDialog::AddRoute {
                saved_id: id.clone(),
                url,
                code,
                error: None,
                focus,
                _subscriptions: subscriptions.into(),
            });
            cx.notify();
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn catalog_diagnosis(
        &self, saved: &SavedEnvironment, label: &str, diagnosis: Diagnosis, expanded: bool,
        connection: Option<&ConnectionStatus>, retry_at: Option<Instant>, cx: &Context<Self>,
    ) -> AnyElement {
        let key: SharedString = saved.id.clone().into();
        let p = palette(cx);
        let report = status::connection_diagnosis_text(label, &diagnosis);
        let retry = retry_at.map(|deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now()).as_millis().div_ceil(1000);
            if remaining == 0 { t(cx, "Retrying…").to_owned() }
            else { format!("{} {remaining}s", t(cx, "Retrying in")) }
        });
        v_flex().gap(r(4.)).text_xs()
            .child(div().text_color(hsla(p.destructive)).child(diagnosis.headline))
            .children(retry.map(|retry| muted_text(retry, cx)))
            .when(expanded, |el| {
                el.child(v_flex().gap(r(6.)).pt(r(6.)).border_t_1().border_color(hsla(p.border).opacity(0.5))
                    .children(diagnosis.hops.into_iter().map(|hop| h_flex().gap(r(6.))
                        .text_color(hsla(if hop.result == HopResult::Failed { p.destructive } else { p.muted_foreground }))
                        .child(hop.result.mark()).child(hop.label).child(t(cx, hop.result.note()))))
                    .child(div().font_weight(FontWeight::MEDIUM).child(t(cx, "What to try")))
                    .children(diagnosis.steps.into_iter().map(|step| muted_text(step, cx)))
                    .children(diagnosis.facts.into_iter().map(|(label, value)| muted_text(format!("{label} · {value}"), cx)))
                    .child(h_flex().flex_wrap().gap(r(4.))
                        .child(catalog_button((gpui::ElementId::from(key.clone()), "retry"), "Retry now", cx)
                            .disabled(!connection.is_some_and(ConnectionStatus::can_retry))
                            .on_click({
                                let state = self.state.clone();
                                let id = saved.id.clone();
                                move |_, _, cx| state.read(cx).retry_environment(&id)
                            }))
                        .child(catalog_button((gpui::ElementId::from(key.clone()), "copy-diagnosis"), "Copy diagnostics", cx)
                            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(report.clone()))))
                        .child(self.catalog_add_route_button(saved, "Add direct route", cx))
                        .children(saved.is_relay_managed().then(|| catalog_button((gpui::ElementId::from(key), "more-info"), "More info", cx)
                            .on_click(|_, _, cx| cx.open_url("https://github.com/pingdotgg/t3code/blob/main/docs/user/t3-connect-troubleshooting.md"))))))
            })
            .into_any_element()
    }

    fn catalog_cloud_rows(&self, cx: &Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let cloud = &state.cloud;
        let p = palette(cx);
        let heading = h_flex().gap(r(6.)).justify_between()
            .child(section_title("T3 Connect"))
            .child(if cloud.has_clerk_session {
                catalog_button("settings-refresh-t3-connect", "Refresh", cx).icon(Icon::RefreshCw)
                    .disabled(cloud.discovery.refreshing)
                    .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.refresh_cloud(cx))))
            } else {
                catalog_button("settings-sign-in-t3-connect", "Sign in", cx).disabled(cloud.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        super::close(window, cx);
                        this.state.update(cx, |state, cx| state.open_switcher(cx));
                    }))
            });
        v_flex().mt(r(10.)).gap(r(5.)).child(heading)
            .children(cloud.error.as_ref().map(|error| muted_text(error.clone(), cx)))
            .children(cloud.discovery.environments.iter().filter_map(|entry| {
                let saved = state.environments.find_by_environment_id(&entry.environment_id);
                if saved.is_some_and(|saved| {
                    saved.local || saved.is_relay_managed() || has_saved_relay_route(&saved.id)
                }) { return None; }
                let known = saved.and_then(|saved| state.fleet.environment(&saved.id)).and_then(|view| view.config.config.as_ref());
                let incompatibility = state.cloud_environment_compatibility_error(&entry.environment_id);
                let (label, color, live) = match cloud.statuses.get(&entry.environment_id) {
                    Some(EnvironmentHealth::Online) => ("Online", p.success, false),
                    Some(EnvironmentHealth::Incompatible(_)) => ("Client not supported", p.destructive, false),
                    Some(EnvironmentHealth::Offline { .. }) => ("Offline", p.destructive, false),
                    Some(EnvironmentHealth::Error(_)) => ("Status unavailable", p.muted_foreground, false),
                    Some(EnvironmentHealth::Checking) | None => ("Checking…", p.warning, cloud.has_clerk_session),
                };
                let adding = cloud.connecting_env_id.as_deref() == Some(&entry.environment_id);
                let disabled_reason = if !cloud.has_clerk_session {
                    Some("Sign in to T3 Connect first.")
                } else if incompatibility.is_some() {
                    Some("This environment uses an incompatible server version.")
                } else if label != "Online" {
                    Some("This environment must be online to add it.")
                } else {
                    None
                };
                let key: SharedString = entry.environment_id.clone().into();
                let env = entry.clone();
                let mut add = Button::new((gpui::ElementId::from(key.clone()), "cloud-add"), ButtonVariant::Outline, ButtonSize::Xs)
                    .label(t(cx, if adding { "Adding…" } else if saved.is_some() { "Add route" } else { "Add" }))
                    .disabled(disabled_reason.is_some() || cloud.connecting_env_id.is_some());
                if let Some(reason) = disabled_reason {
                    add = add.tooltip(t(cx, reason));
                }
                Some(h_flex().w_full().gap(r(10.)).px(r(12.)).py(r(6.))
                    .child(icon(known.map_or(Icon::Cloud, Icon::machine), 16., hsla(p.muted_foreground)))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(entry.label.clone()))
                    .child(StatusDot::new((gpui::ElementId::from(key), "cloud-status"), gpui::px(8.), hsla(color), live))
                    .child(muted_text(t(cx, if incompatibility.is_some() { "Client not supported" } else { label }), cx))
                    .child(add.on_click(cx.listener(move |this, _, _, cx| {
                        this.state.update(cx, |state, cx| state.connect_cloud_environment(env.clone(), false, cx));
                    }))))
            }))
            .into_any_element()
    }

    fn show_catalog_confirmation(&mut self, action: CatalogAction, window: &mut Window, cx: &mut Context<Self>) {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.environments.catalog.dialog = Some(CatalogDialog::Confirm { action, focus });
        cx.notify();
    }

    fn confirm_catalog_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(CatalogDialog::Confirm { action, .. }) = self.environments.catalog.dialog.take() else { return };
        self.focus.focus(window, cx);
        match action {
            CatalogAction::Local { enable } => self.state.update(cx, |state, cx| {
                state.set_local_server_auto_start(enable, cx);
                if enable {
                    if let Some(id) = state.environments.environments.iter().find(|saved| saved.local).map(|saved| saved.id.clone()) {
                        state.set_saved_environment_enabled(&id, true, cx);
                    }
                    state.start_local_server(false, cx);
                } else {
                    if let Some(id) = state.environments.environments.iter().find(|saved| saved.local).map(|saved| saved.id.clone()) {
                        state.set_saved_environment_enabled(&id, false, cx);
                    }
                    state.stop_local_server(cx);
                }
            }),
            CatalogAction::Remove { id, .. } => self.state.update(cx, |state, cx| state.remove_environment(&id, cx)),
            CatalogAction::RemoveRoute { id, route } => {
                let current = load_alternate_routes().remove(&id).unwrap_or_default();
                let routes = t3_client::routes::routes_after_removing(&current, &route.id);
                if routes != current {
                    self.state.update(cx, |state, cx| state.apply_settings_routes(&id, routes, cx));
                }
            }
            CatalogAction::Update(targets) => self.start_catalog_updates(targets, cx),
        }
        cx.notify();
    }

    fn open_catalog_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("https://host:3773/pair#token=…"));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("Pairing code"));
        let username = cx.new(|cx| InputState::new(window, cx).placeholder("Username (optional)"));
        let port = cx.new(|cx| InputState::new(window, cx).placeholder("22"));
        let subscriptions = [&address, &code, &username, &port].map(|input| cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| {
            match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.submit_catalog_add(cx),
                _ => {}
            }
        }));
        let focus = cx.focus_handle();
        address.read(cx).focus_handle(cx).focus(window, cx);
        self.environments.catalog.dialog = Some(CatalogDialog::Add {
            address, code, username, port, source: PairingSource::Link, submitted: false, focus, _subscriptions: subscriptions.into(),
        });
        cx.notify();
    }

    fn submit_catalog_add(&mut self, cx: &mut Context<Self>) {
        let Some(CatalogDialog::Add { address, code, username, port, source, submitted, .. }) = self.environments.catalog.dialog.as_mut() else { return };
        if self.state.read(cx).connect.pairing.is_some() || address.read(cx).value().trim().is_empty() { return; }
        let address = address.read(cx).value().to_string();
        let code = code.read(cx).value().to_string();
        let username = username.read(cx).value().to_string();
        let port = port.read(cx).value().to_string();
        *submitted = true;
        self.state.update(cx, |state, cx| {
            if *source == PairingSource::Ssh {
                let target = if let Some(host) = state.unsaved_ssh_hosts().into_iter().find(|host| host.alias == address.trim()) {
                    let mut target = host.target();
                    if !username.trim().is_empty() {
                        target.username = Some(username.trim().to_owned());
                    }
                    if port.trim().is_empty() {
                        Ok(target)
                    } else if let Some(parsed_port) = port.trim().parse::<u16>().ok().filter(|&p| p >= 1) {
                        target.port = Some(parsed_port);
                        Ok(target)
                    } else {
                        Err("SSH port must be between 1 and 65535.".to_owned())
                    }
                } else {
                    t3_core::ssh::parse_manual_target(&address, &username, &port)
                };
                match target {
                    Ok(target) => state.add_settings_ssh(target, cx),
                    Err(error) => state.fail_pairing(PairingSource::Ssh, error, cx),
                }
            } else {
                state.add_settings_environment(&address, Some(&code).filter(|code| !code.trim().is_empty()).map(String::as_str), cx);
            }
        });
        cx.notify();
    }

    fn submit_catalog_add_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(CatalogDialog::AddRoute { saved_id, url, code, error, .. }) = self.environments.catalog.dialog.as_mut() else {
            return;
        };
        let id = saved_id.clone();
        let raw_url = url.read(cx).value().to_string();
        let raw_code = code.read(cx).value().to_string();
        let opt_code = (!raw_code.trim().is_empty()).then_some(raw_code.as_str());
        let http_base = match parse_direct_route_url(&raw_url, opt_code) {
            Ok(origin) => origin,
            Err(message) => {
                *error = Some(message);
                cx.notify();
                return;
            }
        };
        let Some(saved) = self.state.read(cx).environments.get(&id).cloned() else {
            return;
        };
        let candidate = CandidateRoute::saved(&http_base);
        let mut routes = load_alternate_routes().remove(&id).unwrap_or_default();
        if t3_client::routes::normalize_http_base(&saved.http_base) == candidate.http_base
            || routes.iter().any(|entry| entry.http_base == candidate.http_base && !entry.learned)
        {
            *error = Some(t(cx, "That address is already saved for this environment.").to_owned());
            cx.notify();
            return;
        }
        routes.retain(|entry| entry.id != candidate.id);
        let routes = t3_client::routes::insert_route(&routes, candidate, true);
        self.state.update(cx, |state, cx| state.apply_settings_routes(&id, routes, cx));
        self.environments.catalog.rows.entry(id).or_default().routes = true;
        self.dismiss_environments_overlay(window, cx);
    }

    fn catalog_dialog(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let catalog = self.environments.catalog.dialog.as_ref()?;
        let busy = self.state.read(cx).connect.pairing.is_some();
        let cancel = Button::new("environment-dialog-cancel", ButtonVariant::Outline, ButtonSize::Sm)
            .label(t(cx, "Cancel")).disabled(busy)
            .on_click(cx.listener(|this, _, window, cx| { this.dismiss_environments_overlay(window, cx); }))
            .into_any_element();
        let (id, title, description, body, action, focus) = match catalog {
            CatalogDialog::Confirm { action, focus } => {
                let (title, description, submit, destructive) = match action {
                    CatalogAction::Local { enable: true } => ("Turn on local environment?", "Start the local T3 Code server and enable automatic startup.", "Turn on", false),
                    CatalogAction::Local { enable: false } => ("Turn off local environment?", "Agents and terminals on the server hosted by this app will stop. Other devices will lose access to that server.", "Turn off", true),
                    CatalogAction::Remove { .. } => ("Remove from this device?", "This forgets the saved pairing and credential on this device. To keep it saved, switch the environment off.", "Remove", true),
                    CatalogAction::RemoveRoute { .. } => ("Remove saved route?", "This address can still appear if the server advertises it automatically.", "Remove", true),
                    CatalogAction::Update(_) => ("Update environments?", "Eligible servers may restart while installing the update.", "Update", false),
                };
                let detail = match action {
                    CatalogAction::Remove { label, .. } => Some(label.clone()),
                    CatalogAction::RemoveRoute { route, .. } => Some(route.http_base.clone()),
                    CatalogAction::Update(targets) => Some(targets.iter().map(|target| target.label.as_str()).collect::<Vec<_>>().join(", ")),
                    CatalogAction::Local { .. } => None,
                };
                ("environment-catalog-confirm", title, description, div().children(detail.map(|detail| muted_text(detail, cx))).into_any_element(),
                    Some(Button::new("environment-dialog-confirm", if destructive { ButtonVariant::Destructive } else { ButtonVariant::Default }, ButtonSize::Sm)
                        .label(t(cx, submit)).on_click(cx.listener(|this, _, window, cx| this.confirm_catalog_action(window, cx))).into_any_element()),
                    focus.clone())
            }
            CatalogDialog::Add { address, code, username, port, source, focus, .. } => {
                let ssh = *source == PairingSource::Ssh;
                let p = palette(cx);
                let error = crate::connect::pairing_error(&self.state.read(cx).connect, *source);
                let ssh_hosts = if ssh {
                    self.state.read(cx).unsaved_ssh_hosts()
                } else {
                    Vec::new()
                };
                let mode = |next, label, icon, cx: &mut Context<Self>| {
                    Button::new(label, if next == *source { ButtonVariant::Outline } else { ButtonVariant::GhostMuted }, ButtonSize::Sm)
                        .icon(icon).label(t(cx, label)).disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(CatalogDialog::Add { source, submitted, address, code, username, port, .. }) = this.environments.catalog.dialog.as_mut() {
                                if *source == next { return; }
                                *source = next;
                                *submitted = false;
                                address.update(cx, |input, cx| {
                                    input.set_value("", window, cx);
                                    input.set_placeholder(
                                        if next == PairingSource::Ssh { "host or alias" } else { "https://host:3773/pair#token=…" },
                                        window,
                                        cx,
                                    );
                                });
                                code.update(cx, |input, cx| input.set_value("", window, cx));
                                username.update(cx, |input, cx| input.set_value("", window, cx));
                                port.update(cx, |input, cx| input.set_value("", window, cx));
                                this.state.update(cx, |state, cx| {
                                    state.connect.error = None;
                                    if next == PairingSource::Ssh { state.refresh_ssh_hosts(cx); }
                                });
                                address.read(cx).focus_handle(cx).focus(window, cx);
                                cx.notify();
                            }
                        }))
                };
                let current_addr = address.read(cx).value().to_string();
                let address_handle = address.clone();
                let username_handle = username.clone();
                let port_handle = port.clone();
                let body = v_flex().gap(r(8.))
                    .child(h_flex().gap(r(6.))
                        .child(mode(PairingSource::Link, "Remote link", Icon::Link, cx))
                        .child(mode(PairingSource::Ssh, "SSH", Icon::Terminal, cx)))
                    .when(ssh && !ssh_hosts.is_empty(), |el| {
                        el.child(muted_text(t(cx, "Discovered SSH hosts"), cx)).child(
                            h_flex().flex_wrap().gap(r(6.)).children(ssh_hosts.into_iter().map(|host| {
                                let selected = current_addr.trim() == host.alias;
                                let alias = host.alias.clone();
                                let user = host.username.clone().unwrap_or_default();
                                let p_str = host.port.map(|v| v.to_string()).unwrap_or_default();
                                let address_handle = address_handle.clone();
                                let username_handle = username_handle.clone();
                                let port_handle = port_handle.clone();
                                let btn_label = if host.hostname != host.alias {
                                    format!("{} ({})", host.alias, host.hostname)
                                } else {
                                    host.alias.clone()
                                };
                                Button::new(
                                    SharedString::from(format!("ssh-host-pick-{}", host.alias)),
                                    if selected { ButtonVariant::Outline } else { ButtonVariant::GhostMuted },
                                    ButtonSize::Xs,
                                )
                                .icon(Icon::Terminal)
                                .label(btn_label)
                                .disabled(busy)
                                .on_click(cx.listener(move |_, _, window, cx| {
                                    address_handle.update(cx, |input, cx| input.set_value(alias.clone(), window, cx));
                                    username_handle.update(cx, |input, cx| input.set_value(user.clone(), window, cx));
                                    port_handle.update(cx, |input, cx| input.set_value(p_str.clone(), window, cx));
                                    cx.notify();
                                }))
                            })),
                        )
                    })
                    .child(muted_text(t(cx, if ssh { "SSH host or alias" } else { "Pairing link or server address" }), cx))
                    .child(catalog_input(address, busy, cx))
                    .when(ssh, |el| {
                        el.child(
                            h_flex()
                                .gap(r(8.))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap(r(4.))
                                        .child(muted_text(t(cx, "Username (optional)"), cx))
                                        .child(catalog_input(username, busy, cx)),
                                )
                                .child(
                                    v_flex()
                                        .w(r(96.))
                                        .flex_none()
                                        .gap(r(4.))
                                        .child(muted_text(t(cx, "Port"), cx))
                                        .child(catalog_input(port, busy, cx)),
                                ),
                        )
                    })
                    .when(!ssh, |el| el.child(muted_text(t(cx, "Optional pairing code"), cx)).child(catalog_input(code, busy, cx)))
                    .children(error.map(|message| div().text_xs().text_color(hsla(p.destructive_foreground)).child(message)));
                ("environment-catalog-add", "Add environment", "Choose how to connect to a machine running T3 Code.",
                    body.into_any_element(), Some(Button::new("environment-dialog-add", ButtonVariant::Default, ButtonSize::Sm)
                        .label(t(cx, if busy { "Adding…" } else { "Add environment" }))
                        .disabled(busy || address.read(cx).value().trim().is_empty())
                        .on_click(cx.listener(|this, _, _, cx| this.submit_catalog_add(cx)))
                        .into_any_element()), focus.clone())
            }
            CatalogDialog::AddRoute { saved_id, url, code, error, focus, .. } => {
                let saved = self.state.read(cx).environments.get(saved_id)?;
                let p = palette(cx);
                let reported = t3_client::supervisor::server_config_extras_for(&saved.http_base).borrow().direct_endpoint_entries.clone();
                let choices = route_choices(saved, &load_alternate_routes().remove(saved_id).unwrap_or_default(), &reported);
                let url_empty = url.read(cx).value().trim().is_empty();
                let body = v_flex().gap(r(8.))
                    .when(!choices.is_empty(), |el| {
                        el.child(muted_text(t(cx, "Discovered addresses"), cx)).child(
                            v_flex().gap(r(5.)).children(choices.into_iter().map(|route| {
                                let id = saved_id.clone();
                                let route_id: SharedString = route.id.clone().into();
                                Button::new(route_id, ButtonVariant::Outline, ButtonSize::Sm)
                                    .icon(Icon::Plus).label(format!("{} · {}", route.label(), route.http_base)).justify_between()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let Some(saved) = this.state.read(cx).environments.get(&id) else { return };
                                        let mut routes = load_alternate_routes().remove(&id).unwrap_or_default();
                                        let reported = t3_client::supervisor::server_config_extras_for(&saved.http_base).borrow().direct_endpoint_entries.clone();
                                        let Some(mut candidate) = route_choices(saved, &routes, &reported).into_iter().find(|candidate| candidate.id == route.id) else { return };
                                        routes.retain(|entry| entry.id != candidate.id);
                                        candidate.learned = false;
                                        let routes = t3_client::routes::insert_route(&routes, candidate, true);
                                        this.state.update(cx, |state, cx| state.apply_settings_routes(&id, routes, cx));
                                        this.environments.catalog.rows.entry(id.clone()).or_default().routes = true;
                                        this.dismiss_environments_overlay(window, cx);
                                    }))
                            })),
                        )
                    })
                    .child(muted_text(t(cx, "Server URL or pairing link"), cx))
                    .child(catalog_input(url, busy, cx))
                    .child(muted_text(t(cx, "Optional pairing code"), cx))
                    .child(catalog_input(code, busy, cx))
                    .children(error.as_ref().map(|message| div().text_xs().text_color(hsla(p.destructive_foreground)).child(message.clone())));
                ("environment-catalog-add-route", "Add route", "Save a direct address for this environment. Native tries direct routes before falling back.",
                    body.into_any_element(), Some(Button::new("environment-dialog-add-route", ButtonVariant::Default, ButtonSize::Sm)
                        .label(t(cx, "Add route"))
                        .disabled(url_empty)
                        .on_click(cx.listener(|this, _, window, cx| this.submit_catalog_add_route(window, cx)))
                        .into_any_element()), focus.clone())
            }
        };
        let frame = DialogFrame {
            id,
            title: t(cx, title).into(),
            description: t(cx, description).into(),
            max_width: 448.,
            body,
            footer: dialog_footer(true, std::iter::once(cancel).chain(action).collect(), cx).into_any_element(),
            focus,
        };
        let weak = cx.entity().downgrade();
        Some(frame.render(window, move |window, cx| {
            let _ = weak.update(cx, |this, cx| { this.dismiss_environments_overlay(window, cx); });
        }, cx))
    }

    pub(super) fn environments(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let p = palette(cx);
        let mut blocks = self.catalog_blocks(cx);

        // 1. Linked environments (`LinkedEnvironmentsSettings.tsx`)
        let hosts = self.peer_link_hosts(cx);
        let multi_hosts = hosts.len() > 1;
        let now_ms = now_epoch_ms();
        for (index, host) in hosts.into_iter().enumerate() {
            self.refresh_peer_links(&host.saved_id, false, cx);
            let title = if multi_hosts {
                format!("Linked environments \u{00b7} {}", host.label)
            } else {
                "Linked environments".to_owned()
            };
            let host_saved_for_link = host.saved_id.clone();
            let host_env_for_link = host.environment_id.clone();
            let header_action = host.can_manage.then(|| {
                Button::new(
                    SharedString::from(format!("peer-link-open-{}", host.saved_id)),
                    ButtonVariant::Default,
                    ButtonSize::Xs,
                )
                .icon(Icon::Plus)
                .label("Link")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_link_dialog(
                        host_saved_for_link.clone(),
                        host_env_for_link.clone(),
                        window,
                        cx,
                    );
                }))
                .into_any_element()
            });

            let rows: Vec<AnyElement> = match self.environments.peer_links.get(&host.saved_id) {
                Some(PeerLinksLoad::Failed(err)) => vec![
                    Row::new(
                        SharedString::from(format!("peer-links-err-{}", host.saved_id)),
                        "Could not load linked environments",
                    )
                    .description(err.clone())
                    .into_any_element(),
                ],
                None | Some(PeerLinksLoad::Loading) => vec![
                    Row::new(
                        SharedString::from(format!("peer-links-loading-{}", host.saved_id)),
                        "Loading linked environments\u{2026}",
                    )
                    .into_any_element(),
                ],
                Some(PeerLinksLoad::Loaded { links, .. }) if links.is_empty() => vec![
                    Row::new(
                        SharedString::from(format!("peer-links-empty-{}", host.saved_id)),
                        "No linked environments",
                    )
                    .description(
                        "Link another machine running T3 Code, such as a VPS, so agents here can launch and drive threads there.",
                    )
                    .into_any_element(),
                ],
                Some(PeerLinksLoad::Loaded { links, .. }) => links
                    .iter()
                    .map(|link| {
                        let health = peer_link_health(link, now_ms);
                        let status_text = peer_link_status_text(&health);
                        let dot_color = match &health {
                            PeerLinkHealth::Reachable { expires_in_days: None } => hsla(p.success),
                            PeerLinkHealth::Reachable { expires_in_days: Some(_) } => hsla(p.warning),
                            PeerLinkHealth::Expired | PeerLinkHealth::Failing { .. } => {
                                hsla(p.destructive)
                            }
                            PeerLinkHealth::Unreachable { .. } => hsla(p.muted_foreground).opacity(0.4),
                        };
                        let access_text = link_access_label(&link.access);
                        let urls_text = link.urls.join(", ");
                        let forget_host = host.saved_id.clone();
                        let forget_env = link.environment_id.clone();
                        let forget_label = link.label.clone();
                        h_flex()
                            .px(r(16.))
                            .py(r(12.))
                            .gap(r(12.))
                            .items_center()
                            .justify_between()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(r(3.))
                                    .child(
                                        h_flex()
                                            .gap(r(6.))
                                            .items_center()
                                            .child(
                                                div()
                                                    .size(r(8.))
                                                    .rounded_full()
                                                    .flex_none()
                                                    .bg(dot_color),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(hsla(p.foreground))
                                                    .truncate()
                                                    .child(link.label.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(hsla(p.muted_foreground))
                                            .child(format!("{access_text} \u{00b7} {status_text}")),
                                    )
                                    .when(!urls_text.is_empty(), |el| {
                                        el.child(
                                            div()
                                                .text_xs()
                                                .text_color(hsla(p.muted_foreground).opacity(0.8))
                                                .truncate()
                                                .child(urls_text),
                                        )
                                    }),
                            )
                            .when(host.can_manage, |el| {
                                el.child(
                                    Button::new(
                                        SharedString::from(format!(
                                            "peer-link-forget-{}-{}",
                                            host.saved_id, link.environment_id
                                        )),
                                        ButtonVariant::DestructiveOutline,
                                        ButtonSize::Xs,
                                    )
                                    .label("Forget")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let focus = cx.focus_handle();
                                        focus.focus(window, cx);
                                        this.environments.forget_confirm = Some(ForgetPeerLinkConfirm {
                                            host_saved_id: forget_host.clone(),
                                            target_env_id: forget_env.clone(),
                                            target_label: forget_label.clone(),
                                            busy: false,
                                            error: None,
                                            focus,
                                        });
                                        cx.notify();
                                    })),
                                )
                            })
                            .into_any_element()
                    })
                    .collect(),
            };

            let linked_section = v_flex()
                .gap(r(10.))
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .gap(r(12.))
                        .child(section_title(title))
                        .children(header_action.map(|action| div().pr(r(16.)).child(action))),
                )
                .child(group(rows, cx));
            let anchors: &[&'static str] = if index == 0 { &["linked-environments"] } else { &[] };
            blocks.push(Block::new(anchors, linked_section));
        }

        // 2. Load balancing & Pull request sharing (shown when >= 2 environments)
        let eligible = self.eligible_multi_environments(cx);
        if eligible.len() >= 2 {
            let doc = store::client(cx).clone();
            let lb_enabled = read_load_balancing_enabled(&doc);
            let lb_weights = read_load_balancing_weights(&doc);
            let lb_pairs: Vec<(&str, &str)> = eligible
                .iter()
                .map(|e| (e.environment_id.as_str(), e.label.as_str()))
                .collect();
            let lb_summary = if lb_enabled {
                summarize_load_preferences(&lb_pairs, &lb_weights)
            } else {
                Some("Off".to_owned())
            };
            let lb_switch_state = self.state.clone();
            let mut lb_rows = vec![
                div()
                    .px(r(16.))
                    .py(r(10.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(
                        "New threads in shared projects start on the machine with the most free CPU and memory, weighted by each machine's preference.",
                    )
                    .into_any_element(),
            ];
            for env in &eligible {
                let current_pref = load_preference_for_weight(lb_weights.get(&env.environment_id).copied());
                let weights_snapshot = lb_weights.clone();
                let env_id_for_change = env.environment_id.clone();
                let state_for_change = self.state.clone();
                lb_rows.push(
                    h_flex()
                        .px(r(16.))
                        .py(r(10.))
                        .gap(r(12.))
                        .items_center()
                        .justify_between()
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(r(10.))
                                .items_center()
                                .child(icon(env.icon, 16., hsla(p.muted_foreground)))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(hsla(p.foreground))
                                                .truncate()
                                                .child(env.label.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(hsla(p.muted_foreground))
                                                .truncate()
                                                .child(env.subtitle.clone()),
                                        ),
                                ),
                        )
                        .child(
                            Select::new(
                                SharedString::from(format!("lb-pref-{}", env.saved_id)),
                                LoadPreference::ALL
                                    .iter()
                                    .map(|pref| SelectOption::new(pref.as_str(), pref.label()))
                                    .collect(),
                            )
                            .selected(current_pref.as_str())
                            .show_check()
                            .width(128.)
                            .disabled(!lb_enabled)
                            .on_change(move |value, _, cx| {
                                if let Some(pref) = LoadPreference::parse(&value) {
                                    let patch = load_balancing_weight_patch(
                                        &weights_snapshot,
                                        &env_id_for_change,
                                        pref,
                                    );
                                    store::update_client(
                                        &state_for_change,
                                        ClientSettingChange::ThemePatch(patch),
                                        cx,
                                    );
                                }
                            }),
                        )
                        .into_any_element(),
                );
            }
            let lb_open = self.environments.load_balancing_open || self.is_target("load-balancing");
            let lb_block = folded_section(
                "load-balancing",
                "Load balancing",
                lb_summary,
                lb_open,
                self.is_target("load-balancing"),
                Some(
                    Switch::new("load-balancing-switch", lb_enabled)
                        .on_change(move |checked, _, cx| {
                            store::update_client(
                                &lb_switch_state,
                                ClientSettingChange::ThemePatch(load_balancing_enabled_patch(checked)),
                                cx,
                            );
                        })
                        .into_any_element(),
                ),
                cx.listener(|this, _, _, cx| {
                    this.environments.load_balancing_open = !this.environments.load_balancing_open;
                    if this.is_target("load-balancing") {
                        this.target = None;
                    }
                    cx.notify();
                }),
                lb_rows,
                cx,
            );
            blocks.push(Block::new(&["load-balancing"], lb_block));

            // 3. Pull request sharing (`PullRequestRoutingSettings.tsx`)
            let pr_summary_entries: Vec<(&str, PullRequestRoutingPermission)> = eligible
                .iter()
                .map(|env| {
                    let perm = pull_request_routing_permission_for(
                        env.connection_key.as_deref(),
                        &self.environments.pr_routing_permissions,
                    );
                    (env.label.as_str(), perm)
                })
                .collect();
            let pr_summary = Some(
                summarize_pull_request_routing(&pr_summary_entries).unwrap_or_else(|| "Off".to_owned()),
            );
            let mut pr_rows = vec![
                div()
                    .px(r(16.))
                    .py(r(10.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(
                        "Machines you trust here can read PR data through each other's source control access. Enable both machines. Read and act may use broader permissions than the machine that owns them. This applies only to this device.",
                    )
                    .into_any_element(),
            ];
            for env in &eligible {
                let current_perm = pull_request_routing_permission_for(
                    env.connection_key.as_deref(),
                    &self.environments.pr_routing_permissions,
                );
                let env_id_for_change = env.environment_id.clone();
                let key_for_change = env.connection_key.clone();
                let weak_view = cx.entity().downgrade();
                pr_rows.push(
                    h_flex()
                        .px(r(16.))
                        .py(r(10.))
                        .gap(r(12.))
                        .items_center()
                        .justify_between()
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(r(10.))
                                .items_center()
                                .child(icon(env.icon, 16., hsla(p.muted_foreground)))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(hsla(p.foreground))
                                                .truncate()
                                                .child(env.label.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(hsla(p.muted_foreground))
                                                .truncate()
                                                .child(env.subtitle.clone()),
                                        ),
                                ),
                        )
                        .child(
                            Select::new(
                                SharedString::from(format!("pr-routing-{}", env.saved_id)),
                                PullRequestRoutingPermission::ALL
                                    .iter()
                                    .map(|perm| SelectOption::new(perm.as_str(), perm.label()))
                                    .collect(),
                            )
                            .selected(current_perm.as_str())
                            .show_check()
                            .width(128.)
                            .disabled(env.connection_key.is_none())
                            .on_change(move |value, _, cx| {
                                let (Some(perm), Some(conn_key)) = (
                                    PullRequestRoutingPermission::parse(&value),
                                    key_for_change.as_deref(),
                                ) else {
                                    return;
                                };
                                let _ = weak_view.update(cx, |this, cx| {
                                    this.environments.pr_routing_permissions =
                                        set_pull_request_routing_permission(
                                            &this.environments.pr_routing_permissions,
                                            &env_id_for_change,
                                            conn_key,
                                            perm,
                                        );
                                    save_ui_state(
                                        PR_ROUTING_STORE_KEY,
                                        &this.environments.pr_routing_permissions,
                                    );
                                    cx.notify();
                                });
                            }),
                        )
                        .into_any_element(),
                );
            }
            let pr_open =
                self.environments.pr_routing_open || self.is_target("pull-request-routing");
            let pr_block = folded_section(
                "pull-request-routing",
                "Pull request sharing",
                pr_summary,
                pr_open,
                self.is_target("pull-request-routing"),
                None,
                cx.listener(|this, _, _, cx| {
                    this.environments.pr_routing_open = !this.environments.pr_routing_open;
                    if this.is_target("pull-request-routing") {
                        this.target = None;
                    }
                    cx.notify();
                }),
                pr_rows,
                cx,
            );
            blocks.push(Block::new(&["pull-request-routing"], pr_block));
        }

        blocks
    }

    fn link_dialog_ready(&self, cx: &App) -> bool {
        let Some(dialog) = self.environments.link_dialog.as_ref() else {
            return false;
        };
        let machines = self.linkable_machines_for(&dialog.host_saved_id, &dialog.host_env_id, cx);
        let selected = dialog
            .choice
            .clone()
            .or_else(|| machines.is_empty().then(|| ANOTHER_ADDRESS.to_owned()));
        let machine = selected
            .as_deref()
            .and_then(|sel| machines.iter().find(|m| m.environment_id == sel));
        let needs_address = selected.is_some() && machine.is_none_or(|m| m.urls.is_empty());
        let can_mint = machine.is_some_and(|m| {
            !dialog.mint_failed.contains(&m.environment_id)
                && self.can_mint_on_machine(m, needs_address, cx)
        });
        let address_val = dialog.address.read(cx).value().trim().to_owned();
        let code_val = dialog.pairing_code.read(cx).value().trim().to_owned();
        selected.is_some()
            && (!needs_address || !address_val.is_empty())
            && (can_mint || !code_val.is_empty())
    }

    pub(super) fn environments_dialog_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(dialog) = self.access_dialog_overlay(window, cx) {
            return Some(dialog);
        }
        if let Some(dialog) = self.catalog_dialog(window, cx) {
            return Some(dialog);
        }
        let p = palette(cx);
        if let Some(confirm) = &self.environments.forget_confirm {
            let busy = confirm.busy;
            let label = confirm.target_label.clone();
            let err = confirm.error.clone();
            let weak = cx.entity().downgrade();
            let frame = DialogFrame {
                id: "peer-link-forget-dialog",
                title: SharedString::from(format!("Forget {label}?")),
                description: SharedString::from(format!(
                    "Agents here stop working there. Revoke \"T3 Code \u{00b7} \u{2026}\" in {label}'s Sharing & access to end the session there too."
                )),
                max_width: 448.,
                body: div()
                    .children(err.map(|msg| {
                        div().text_xs().text_color(hsla(p.destructive_foreground)).child(msg)
                    }))
                    .into_any_element(),
                footer: dialog_footer(
                    true,
                    vec![
                        Button::new("peer-link-forget-cancel", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Cancel")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dismiss_environments_overlay(window, cx);
                            }))
                            .into_any_element(),
                        Button::new(
                            "peer-link-forget-submit",
                            ButtonVariant::DestructiveOutline,
                            ButtonSize::Sm,
                        )
                        .label(if busy { "Forgetting\u{2026}" } else { "Forget" })
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_forget_peer_link(cx);
                        }))
                        .into_any_element(),
                    ],
                    cx,
                )
                .into_any_element(),
                focus: confirm.focus.clone(),
            };
            return Some(frame.render(
                window,
                move |window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.dismiss_environments_overlay(window, cx);
                    });
                },
                cx,
            ));
        }

        let dialog = self.environments.link_dialog.as_ref()?;
        let machines = self.linkable_machines_for(&dialog.host_saved_id, &dialog.host_env_id, cx);
        let selected = dialog
            .choice
            .clone()
            .or_else(|| machines.is_empty().then(|| ANOTHER_ADDRESS.to_owned()));
        let machine = selected
            .as_deref()
            .and_then(|sel| machines.iter().find(|m| m.environment_id == sel));
        let needs_address = selected.is_some() && machine.is_none_or(|m| m.urls.is_empty());
        let can_mint = machine.is_some_and(|m| {
            !dialog.mint_failed.contains(&m.environment_id)
                && self.can_mint_on_machine(m, needs_address, cx)
        });
        let address_val = dialog.address.read(cx).value().trim().to_owned();
        let ready = self.link_dialog_ready(cx);
        let busy = dialog.busy;
        let weak_choice = cx.entity().downgrade();
        let weak_access = cx.entity().downgrade();
        let weak_dismiss = cx.entity().downgrade();

        let mut body = v_flex().gap(r(14.));
        if !machines.is_empty() {
            body = body.child(crate::connect::render_link_machine_select(
                "peer-link-machine-select",
                &machines,
                selected.as_deref(),
                busy,
                move |value, _, cx| {
                    let _ = weak_choice.update(cx, |this, cx| {
                        if let Some(d) = this.environments.link_dialog.as_mut() {
                            d.choice = Some(value);
                            d.error = None;
                            cx.notify();
                        }
                    });
                },
                cx,
            ));
        }

        if machine.is_some_and(|m| m.linked) {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child("Already linked. Linking again renews it with the access you pick."),
            );
        }

        if needs_address {
            let no_reachable_note = machine.map(|m| {
                format!(
                    "This app has no address for {} that another machine can reach. Enter its https or Tailscale address.",
                    m.label
                )
            });
            body = body.child(
                v_flex()
                    .gap(r(6.))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child("Address"),
                    )
                    .child(catalog_input(&dialog.address, busy, cx))
                    .children(no_reachable_note.map(|note| {
                        div().text_xs().text_color(hsla(p.muted_foreground)).child(note)
                    })),
            );
            if !address_val.is_empty() {
                body = body.child(crate::connect::link_address_warning(&address_val, cx));
            }
        }

        if selected.is_some() && !can_mint {
            let help_note = machine.map(|m| {
                format!(
                    "Create a link on {}: Settings \u{2192} Sharing & access \u{2192} Authorized clients \u{2192} Create link, then paste its code here.",
                    m.label
                )
            });
            body = body.child(
                v_flex()
                    .gap(r(6.))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child("Pairing code"),
                    )
                    .child(catalog_input(&dialog.pairing_code, busy, cx))
                    .children(help_note.map(|note| {
                        div().text_xs().text_color(hsla(p.muted_foreground)).child(note)
                    })),
            );
        }

        let access_options: Vec<SelectOption> = ACCESS_LEVELS
            .iter()
            .map(|level| {
                let icon = match *level {
                    "read-only" => Icon::Eye,
                    "approval-required" => Icon::Shield,
                    "auto-accept-edits" => Icon::Check,
                    _ => Icon::LockOpen,
                };
                SelectOption::new(*level, link_access_label(level))
                    .icon(icon)
                    .description(link_access_description(level))
            })
            .collect();
        body = body.child(
            v_flex()
                .gap(r(6.))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .child("What agents here may do there"),
                )
                .child(
                    Select::new("peer-link-access-select", access_options)
                        .selected(dialog.access.clone())
                        .show_check()
                        .width(360.)
                        .disabled(busy)
                        .on_change(move |value, _, cx| {
                            let _ = weak_access.update(cx, |this, cx| {
                                if let Some(d) = this.environments.link_dialog.as_mut() {
                                    d.access = value.to_string();
                                    cx.notify();
                                }
                            });
                        }),
                )
                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(format!(
                    "{} An agent never gets more access there than its own mode here. Below full access, it can change only threads this link started, not that machine's own threads, projects or settings.",
                    link_access_description(&dialog.access)
                ))),
        );

        if let Some(err) = &dialog.error {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(hsla(p.destructive_foreground))
                    .child(err.clone()),
            );
        }

        let frame = DialogFrame {
            id: "peer-link-create-dialog",
            title: "Link an environment".into(),
            description: "Agents here can then launch, message and wait on threads there. It appears under Authorized clients in that environment's Sharing & access, where it can be revoked.".into(),
            max_width: 448.,
            body: body.into_any_element(),
            footer: dialog_footer(
                true,
                vec![
                    Button::new("peer-link-cancel", ButtonVariant::Outline, ButtonSize::Sm)
                        .label("Cancel")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.dismiss_environments_overlay(window, cx);
                        }))
                        .into_any_element(),
                    Button::new("peer-link-submit", ButtonVariant::Default, ButtonSize::Sm)
                        .label(if busy { "Linking\u{2026}" } else { "Link" })
                        .disabled(busy || !ready)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.submit_link_dialog(cx);
                        }))
                        .into_any_element(),
                ],
                cx,
            )
            .into_any_element(),
            focus: dialog.focus.clone(),
        };

        Some(frame.render(
            window,
            move |window, cx| {
                let _ = weak_dismiss.update(cx, |this, cx| {
                    this.dismiss_environments_overlay(window, cx);
                });
            },
            cx,
        ))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn linux_with_windows_file_manager_uses_wsl_transport_label() {
        let saved = t3_client::store::SavedEnvironments::from_legacy(t3_client::auth::Credentials {
            http_base: "http://localhost:3773".into(),
            access_token: "fixture".into(),
        });
        let saved = saved.active().unwrap();
        let mut config: ServerConfig = serde_json::from_value(serde_json::json!({
            "environment": { "platform": { "os": "linux", "machine": "linux" } }
        })).unwrap();
        assert_eq!(transport_subtitle(saved, Some(&config)), saved.http_base);
        config.shell_reveal_in_file_manager_kind = Some("file-explorer".into());
        assert_eq!(transport_subtitle(saved, Some(&config)), "WSL");
        config.environment.platform.as_mut().unwrap().os = "windows".into();
        assert_eq!(transport_subtitle(saved, Some(&config)), saved.http_base);
    }

    use t3_client::routes::ReportedEndpoint;

    #[test]
    fn icon_picker_resolves_override_detection_and_default() {
        let config: ServerConfig = serde_json::from_value(json!({
            "environment": { "platform": { "machine": "mac-mini", "os": "darwin" } },
            "settings": { "environmentIcon": "cloud" }
        })).unwrap();
        assert_eq!(machine_icon_options(Some(&config)), ("cloud", "mac-mini", true));
        assert_eq!(Icon::machine(&config), Icon::Cloud);
        assert_eq!(machine_icon_options(None), ("server", "server", false));
    }

    #[test]
    fn route_choices_use_safe_advertised_addresses_and_preserve_a_saved_order() {
        let saved: SavedEnvironment = serde_json::from_value(json!({
            "id": "dev",
            "environmentId": "dev",
            "label": "Dev box",
            "httpBase": "https://dev.example"
        })).unwrap();
        let reported = [
            ReportedEndpoint { http_base_url: "http://192.168.0.23:3773".into(), kind: None },
            ReportedEndpoint { http_base_url: "https://dev.example.ts.net".into(), kind: Some("tailnet".into()) },
            ReportedEndpoint { http_base_url: "https://other.example".into(), kind: None },
        ];
        let mut choices = route_choices(&saved, &[], &reported);
        assert_eq!(choices.iter().map(|route| route.http_base.as_str()).collect::<Vec<_>>(),
            ["https://dev.example.ts.net", "https://other.example"]);
        let id = choices[0].id.clone();
        assert!(!move_alternate_route(&mut choices, &id, -1));
        assert!(move_alternate_route(&mut choices, &id, 1));
        assert_eq!(choices[1].id, id);
        choices[1].learned = false;
        let offered = route_choices(&saved, &choices, &reported);
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].http_base, "https://other.example");
    }

    #[test]
    fn direct_route_availability_and_url_parsing() {
        let relay: SavedEnvironment = serde_json::from_value(json!({
            "id": "relayed",
            "label": "Relay Box",
            "httpBase": "https://relay.example"
        }))
        .unwrap();
        let local: SavedEnvironment = serde_json::from_value(json!({
            "id": "local",
            "label": "Local",
            "httpBase": "http://127.0.0.1:3773",
            "local": true
        }))
        .unwrap();
        let ssh: SavedEnvironment = serde_json::from_value(json!({
            "id": "ssh-box",
            "label": "SSH Box",
            "httpBase": "http://127.0.0.1:41000",
            "ssh": { "alias": "host", "hostname": "host.example.com" }
        }))
        .unwrap();
        assert!(can_add_direct_route(&relay));
        assert!(!can_add_direct_route(&local));
        assert!(!can_add_direct_route(&ssh));

        assert_eq!(parse_direct_route_url(" https://box.ts.net:3773/ ", None).unwrap(), "https://box.ts.net:3773");
        assert_eq!(parse_direct_route_url("100.64.0.1:3773", None).unwrap(), "http://100.64.0.1:3773");
        assert!(parse_direct_route_url("   ", None).is_err());
        assert!(parse_direct_route_url("ftp://bad.example", None).is_err());
    }

    #[gpui::test]
    fn removing_a_saved_environment_requires_confirmation(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let state = cx.new(|cx| crate::app_state::AppState::new(runtime.handle().clone(), cx));
            crate::settings::init(state.clone(), cx);
            state.update(cx, |state, _| {
                state.environments = serde_json::from_value(json!({"environments": [{
                    "id": "fixture-remote",
                    "label": "Remote",
                    "httpBase": "https://example.invalid"
                }]})).unwrap();
            });
            state
        });
        let window = cx.open_window(gpui::size(gpui::px(600.), gpui::px(480.)), |window, cx| {
            SettingsView::new(state.clone(), t3_core::settings::search::Section::Connections, None, window, cx)
        });
        window.update(cx, |view, window, cx| {
            let remove = || CatalogAction::Remove { id: "fixture-remote".into(), label: "Remote".into() };
            view.show_catalog_confirmation(remove(), window, cx);
            assert!(state.read(cx).environments.get("fixture-remote").is_some());
            assert!(view.dismiss_environments_overlay(window, cx));
            assert!(state.read(cx).environments.get("fixture-remote").is_some());
            view.show_catalog_confirmation(remove(), window, cx);
            view.confirm_catalog_action(window, cx);
            assert!(state.read(cx).environments.get("fixture-remote").is_none());
        }).unwrap();
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use gpui::{Render, div, prelude::*};
    use serde_json::json;
    use t3_core::settings::search::Section;

    use super::*;

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().children(crate::settings::overlay(window, cx))
        }
    }

    #[gpui::test]
    fn peer_links_list_link_and_unlink_round_trip(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let drive = |vcx: &mut gpui::VisualTestContext| {
            vcx.run_until_parked();
            runtime.block_on(async {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            });
            vcx.run_until_parked();
        };
        cx.executor().allow_parking();
        let (state, mut fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "access:read", "access:write"]
                }))
                .unwrap(),
            ))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": { "environmentId": "mac-env", "label": "Mac" },
                        "settings": { "linkedEnvironmentsEnabled": true }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
            crate::settings::open(Section::Connections, cx);
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let view = vcx.read(|cx| cx.global::<crate::settings::Host>().view.clone().expect("settings view"));

        // Rendering Environments triggers peerLinks.list on the host.
        drive(&mut vcx);
        let (requests, _) = fixture.take_requests();
        let (_, _, reply) = requests
            .into_iter()
            .find(|(m, _, _)| *m == PEER_LINKS_LIST_METHOD)
            .expect("peerLinks.list sent");
        let _ = reply.send(Ok(json!({
            "links": [{
                "environmentId": "vps-1",
                "label": "Build VPS",
                "urls": ["https://vps.example.ts.net"],
                "access": "approval-required",
                "linkedAt": "2026-11-01T00:00:00Z",
                "expiresAt": "2026-12-01T00:00:00Z",
                "lastReachedAt": null,
                "lastError": null,
                "status": "reachable"
            }]
        })));
        drive(&mut vcx);

        view.read_with(&vcx, |view, _| {
            let loaded = view.environments.peer_links.values().next().expect("peer links loaded");
            assert!(matches!(loaded, PeerLinksLoad::Loaded { links, .. } if links.len() == 1 && links[0].label == "Build VPS"));
        });

        // Link dialog subscribes to InputEvent::Change on address & pairing_code, notifies
        // SettingsView, flips Link button readiness from false -> true, and sends peerLinks.link.
        let notify_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let notify_counter = notify_count.clone();
        let _obs = vcx.update(|_, cx| {
            cx.observe(&view, move |_, _| {
                notify_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_link_dialog("mac-env".into(), "mac-env".into(), window, cx);
                let d = view.environments.link_dialog.as_mut().unwrap();
                d.choice = Some(ANOTHER_ADDRESS.to_owned());
            });
            assert!(!view.read(cx).link_dialog_ready(cx));
        });
        vcx.run_until_parked();
        notify_count.store(0, std::sync::atomic::Ordering::SeqCst);
        vcx.update(|window, cx| {
            let address = view.read(cx).environments.link_dialog.as_ref().unwrap().address.clone();
            address.update(cx, |i, cx| i.insert("https://box2.ts.net", window, cx));
        });
        vcx.run_until_parked();
        assert!(notify_count.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        assert!(!view.read_with(&vcx, |v, cx| v.link_dialog_ready(cx)));
        notify_count.store(0, std::sync::atomic::Ordering::SeqCst);
        vcx.update(|window, cx| {
            let pairing_code = view.read(cx).environments.link_dialog.as_ref().unwrap().pairing_code.clone();
            pairing_code.update(cx, |i, cx| i.insert("PAIR-1234", window, cx));
        });
        vcx.run_until_parked();
        assert!(notify_count.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        vcx.update(|_, cx| {
            assert!(view.read(cx).link_dialog_ready(cx));
            view.update(cx, |view, cx| view.submit_link_dialog(cx));
        });
        drop(_obs);
        drive(&mut vcx);
        let (requests, _) = fixture.take_requests();
        let (_, payload, reply) = requests
            .into_iter()
            .find(|(m, _, _)| *m == PEER_LINKS_LINK_METHOD)
            .expect("peerLinks.link sent");
        assert_eq!(payload["url"], json!("https://box2.ts.net"));
        assert_eq!(payload["pairingCode"], json!("PAIR-1234"));
        assert_eq!(payload["access"], json!("approval-required"));
        let _ = reply.send(Ok(json!({ "label": "Box 2" })));
        drive(&mut vcx);

        // Unlink sends peerLinks.unlink.
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                view.environments.forget_confirm = Some(ForgetPeerLinkConfirm {
                    host_saved_id: "mac-env".into(),
                    target_env_id: "vps-1".into(),
                    target_label: "Build VPS".into(),
                    busy: false,
                    error: None,
                    focus,
                });
                view.confirm_forget_peer_link(cx);
            });
        });
        drive(&mut vcx);
        let (requests, _) = fixture.take_requests();
        let (_, payload, reply) = requests
            .into_iter()
            .find(|(m, _, _)| *m == PEER_LINKS_UNLINK_METHOD)
            .expect("peerLinks.unlink sent");
        assert_eq!(payload["environmentId"], json!("vps-1"));
        let _ = reply.send(Ok(json!({ "removed": true })));
        drive(&mut vcx);

        drop(view);
        vcx.update(crate::settings::finish_close);
        state.update(&mut vcx, |state, _| state.stop_pump());
        vcx.run_until_parked();
        drop(vcx);
        drop(fixture);
        drop(runtime);
    }

    #[gpui::test]
    async fn link_dialog_reuses_link_machine_select_and_auto_mints_via_peer_pairing_code(
        cx: &mut gpui::TestAppContext,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (peer_base, mint_calls, server_task) = runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, Value)>::new()));
            let calls_bg = calls.clone();
            let replies = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from([
                (403u16, json!({ "error": "access:write required" })),
                (
                    200u16,
                    json!({
                        "id": "pair-auto-1",
                        "credential": "MINTED-PEER-CODE",
                        "label": "T3 Code link",
                        "expiresAt": "2099-01-01T00:00:00.000Z"
                    }),
                ),
            ])));
            let task = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    loop {
                        let n = socket.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(head) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&buf[..head]);
                            let len = header
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if buf.len() >= head + 4 + len {
                                break;
                            }
                        }
                    }
                    let raw = String::from_utf8_lossy(&buf);
                    let first_line = raw.lines().next().unwrap_or("").to_owned();
                    let body_str = raw.split("\r\n\r\n").nth(1).unwrap_or("");
                    let body_json = serde_json::from_str::<Value>(body_str).unwrap_or(Value::Null);
                    let (status, reply) = if first_line.starts_with("GET /.well-known/t3-environment") {
                        (
                            200u16,
                            json!({
                                "environmentId": "vps-1",
                                "label": "Build VPS",
                                "serverVersion": "0.1.9",
                                "capabilities": { "peerLinkPairingCodes": true }
                            }),
                        )
                    } else {
                        calls_bg.lock().unwrap().push((first_line, body_json.clone()));
                        let (status, mut reply) = replies
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or((200, json!({})));
                        if status == 200
                            && let Some(obj) = reply.as_object_mut()
                        {
                            obj.entry("purpose".to_owned()).or_insert_with(|| json!("peer-link"));
                            if let Some(access) = body_json.get("access").cloned() {
                                obj.insert("access".to_owned(), access);
                            }
                        }
                        (status, reply)
                    };
                    let body = reply.to_string();
                    let resp = format!(
                        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                }
            });
            (base, calls, task)
        });

        cx.executor().allow_parking();
        let (state, mut fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "access:read", "access:write"]
                }))
                .unwrap(),
            ))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.environments.environments = vec![
                    SavedEnvironment {
                        id: "host-mac".into(),
                        environment_id: Some("mac-env".into()),
                        label: "Mac".into(),
                        http_base: "http://127.0.0.1:3773".into(),
                        access_token: Some("tok-host".into()),
                        local: true,
                        disabled: false,
                        ssh: None,
                        server_version: None,
                    },
                    SavedEnvironment {
                        id: "peer-vps".into(),
                        environment_id: Some("vps-1".into()),
                        label: "Build VPS".into(),
                        http_base: peer_base.clone(),
                        access_token: Some("tok-peer".into()),
                        local: false,
                        disabled: false,
                        ssh: None,
                        server_version: None,
                    },
                ];
                t3_client::supervisor::set_server_config_extras_for(
                    &peer_base,
                    t3_client::supervisor::ServerConfigExtras {
                        direct_endpoints: vec!["https://vps.example.ts.net".to_owned()],
                        ..Default::default()
                    },
                );
                state.environments.active = Some("host-mac".into());
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": { "environmentId": "mac-env", "label": "Mac" },
                        "settings": { "linkedEnvironmentsEnabled": true }
                    }))
                    .unwrap(),
                );
                let peer_cfg: ServerConfig = serde_json::from_value(json!({
                    "environment": {
                        "environmentId": "vps-1",
                        "label": "Build VPS",
                        "platform": { "os": "linux", "arch": "x64", "machine": "linux" }
                    },
                    "auth": { "policy": "remote-reachable" }
                }))
                .unwrap();
                let mut peer_cfg_state = t3_core::server_config::ServerConfigState::default();
                peer_cfg_state.config = Some(peer_cfg);
                let peer_grants: t3_core::auth::AuthSessionState = serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "access:read", "access:write"]
                }))
                .unwrap();
                let mut peer_shell = t3_core::shell::ShellState::default();
                peer_shell.synchronized = true;
                let peer_owner = t3_core::sidebar::grouping::EnvironmentOwner {
                    saved_id: "peer-vps".into(),
                    canonical_id: Some("vps-1".into()),
                    generation: 1,
                    connection_generation: 1,
                    shell_generation: 1,
                };
                state.fleet = std::sync::Arc::new(t3_client::environments::FleetView {
                    revision: 1,
                    environments: vec![std::sync::Arc::new(t3_client::environments::EnvironmentView {
                        summary: std::sync::Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                            peer_owner,
                            &peer_shell,
                            &peer_cfg_state,
                        )),
                        status: ConnectionStatus::Connected,
                        config: peer_cfg_state,
                        grants: Some(peer_grants),
                        settings_revision: 0,
                        keybindings_revision: 0,
                    })],
                });
                state.cloud.discovery.environments.push(t3_core::relay::Environment {
                    environment_id: "cloud-only-1".into(),
                    label: "Cloud Box".into(),
                    endpoint: t3_core::relay::Endpoint {
                        http_base_url: "https://cloud-box.t3coderelay.com".into(),
                        ws_base_url: "wss://cloud-box.t3coderelay.com/ws".into(),
                        provider_kind: "relay".into(),
                    },
                });
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
            crate::settings::open(Section::Connections, cx);
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let view = vcx.read(|cx| cx.global::<crate::settings::Host>().view.clone().expect("settings view"));

        // 1. `linkable_machines_for` includes connected peer (with machine icon + known URL) and T3 Connect discovery machine (`connect_only`).
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.environments.peer_links.insert(
                    "host-mac".into(),
                    PeerLinksLoad::Loaded { links: Vec::new(), fetched_at: Instant::now() },
                );
                let machines = view.linkable_machines_for("host-mac", "mac-env", cx);
                let vps = machines.iter().find(|m| m.environment_id == "vps-1").expect("vps machine");
                assert!(vps.connected);
                assert!(!vps.connect_only);
                assert_eq!(vps.machine.as_deref(), Some("linux"));
                // Loopback `peer_base` is filtered out by `collect_known_machine_urls`; direct endpoint is kept.
                assert_eq!(vps.urls, vec!["https://vps.example.ts.net".to_owned()]);
                let cloud = machines
                    .iter()
                    .find(|m| m.environment_id == "cloud-only-1")
                    .expect("cloud machine");
                assert!(cloud.connect_only);
                assert!(!cloud.connected);
                assert!(cloud.urls.is_empty());

                // Open link dialog and pick the connected peer -> ready without typing a manual code.
                view.open_link_dialog("host-mac".into(), "mac-env".into(), window, cx);
                let dialog = view.environments.link_dialog.as_mut().unwrap();
                dialog.choice = Some("vps-1".into());
                assert!(view.link_dialog_ready(cx), "connected peer with access:write should be ready to auto-mint");
                view.submit_link_dialog(cx);
            });
        });

        // First auto-mint gets HTTP 403 -> marks `vps-1` in `mint_failed`, shows PeerPairingCodeError message, and disables readiness until manual code is entered.
        let (fail_tx, fail_rx) = tokio::sync::oneshot::channel::<()>();
        let mut fail_tx = Some(fail_tx);
        let _fail_obs = vcx.update(|_, cx| {
            cx.observe(&view, move |view, cx| {
                if view
                    .read(cx)
                    .environments
                    .link_dialog
                    .as_ref()
                    .is_some_and(|d| !d.busy && d.mint_failed.contains("vps-1"))
                    && let Some(tx) = fail_tx.take()
                {
                    let _ = tx.send(());
                }
            })
        });
        if view.read_with(&vcx, |view, _| {
            !view
                .environments
                .link_dialog
                .as_ref()
                .is_some_and(|d| !d.busy && d.mint_failed.contains("vps-1"))
        }) {
            fail_rx.await.unwrap();
        }
        drop(_fail_obs);
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let v = view.read(cx);
            let d = v.environments.link_dialog.as_ref().expect("dialog stays open on mint failure");
            assert!(d.mint_failed.contains("vps-1"));
            assert!(
                d.error.as_deref().is_some_and(|msg| msg.contains("cannot create pairing codes")),
                "403 must surface PeerPairingCodeError::NotAllowed message, got: {:?}",
                d.error
            );
            assert!(!v.link_dialog_ready(cx), "after mint failure, manual pairing code is required");
        });

        // Clear `mint_failed` and retry -> second mint gets HTTP 200 and sends `peerLinks.link` with minted credential.
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let d = view.environments.link_dialog.as_mut().unwrap();
                d.mint_failed.clear();
                view.submit_link_dialog(cx);
            });
        });
        let (method, payload, reply) = loop {
            let req = fixture.next_request().await;
            if req.0 == PEER_LINKS_LINK_METHOD {
                break req;
            }
            let _ = req.2.send(Ok(json!({})));
        };
        assert_eq!(method, PEER_LINKS_LINK_METHOD);
        assert_eq!(payload["url"], json!("https://vps.example.ts.net"));
        assert_eq!(payload["pairingCode"], json!("MINTED-PEER-CODE"));
        assert_eq!(payload["expectedEnvironmentId"], json!("vps-1"));
        assert_eq!(payload["expectedLabel"], json!("Build VPS"));
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();
        let mut closed_tx = Some(closed_tx);
        let _closed_obs = vcx.update(|_, cx| {
            cx.observe(&view, move |view, cx| {
                if view.read(cx).environments.link_dialog.is_none()
                    && let Some(tx) = closed_tx.take()
                {
                    let _ = tx.send(());
                }
            })
        });
        let _ = reply.send(Ok(json!({ "label": "Build VPS" })));
        closed_rx.await.unwrap();
        drop(_closed_obs);
        vcx.run_until_parked();
        assert!(view.read_with(&vcx, |v, _| v.environments.link_dialog.is_none()));
        let recorded = mint_calls.lock().unwrap().clone();
        assert!(
            recorded.iter().any(|(req, body)| {
                req.starts_with("POST /api/auth/pairing-token")
                    && body["label"] == "T3 Code link"
                    && body["purpose"] == "peer-link"
                    && body["access"].is_string()
            }),
            "expected POST /api/auth/pairing-token from mint_pairing_code_on, got: {recorded:?}"
        );

        server_task.abort();
        drop(view);
        vcx.update(crate::settings::finish_close);
        state.update(&mut vcx, |state, _| state.stop_pump());
        vcx.run_until_parked();
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }
}
