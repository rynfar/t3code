//! Environments (`ST-ENVS`): saved environments, `LinkedEnvironmentsSettings.tsx`,
//! `LoadBalancingSettings.tsx`, and `PullRequestRoutingSettings.tsx`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, FontWeight, IntoElement,
    ParentElement, SharedString, Styled, Subscription, Window, div, prelude::*,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use serde_json::{Value, json};
use t3_client::auth::{ORCHESTRATION_PROTOCOL_VERSION, post_access};
use t3_client::store::{SavedEnvironment, load_ui_state, save_ui_state};
use t3_client::supervisor::ConnectionStatus;
use t3_core::client_settings::ClientSettingChange;
use t3_core::server_config::ServerConfig;
use t3_core::settings::access::ACCESS_READ as ACCESS_READ_SCOPE;
use t3_core::settings::{
    LoadPreference, PEER_LINKS_LINK_METHOD, PEER_LINKS_LIST_METHOD, PEER_LINKS_UNLINK_METHOD,
    PeerLinkHealth, PeerLinkSummary, PullRequestRoutingPermission,
    StoredPullRequestRoutingPermission, load_balancing_enabled_patch, load_balancing_weight_patch,
    load_preference_for_weight, mcp_client_scopes, peer_link_create_payload, peer_link_health,
    peer_link_status_text, peer_link_unlink_payload, pull_request_routing_connection_key,
    pull_request_routing_permission_for, read_load_balancing_enabled, read_load_balancing_weights,
    set_pull_request_routing_permission, summarize_load_preferences, summarize_pull_request_routing,
};
use t3_core::timeline::{
    ANOTHER_ADDRESS, ExistingPeerLink, MachineCandidate, can_manage_peer_links,
    link_access_description, link_access_label, link_destination, linkable_machines,
};

use super::controls::{
    Button, ButtonSize, ButtonVariant, DialogFrame, Row, Select, SelectOption, Switch,
    dialog_footer, group, icon, muted_text, r, section, section_title,
};
use super::{Block, SettingsView, close, folded_section, store};
use crate::app_state::Phase;
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

pub(crate) struct EnvironmentsState {
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

fn transport_subtitle(saved: &SavedEnvironment) -> String {
    if let Some(ssh) = &saved.ssh {
        return format!("SSH \u{00b7} {}", ssh.alias);
    }
    if saved.local {
        return "Local".to_owned();
    }
    let addr = saved.address();
    if addr.is_empty() { "Remote".to_owned() } else { addr }
}

impl SettingsView {
    pub(super) fn dismiss_environments_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
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
                subtitle: transport_subtitle(saved),
                icon: machine_icon,
                connection_key,
            });
        }
        out
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
            cx.subscribe_in(&address, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
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
        cx.notify();
    }

    fn linkable_machines_for(
        &self,
        host_saved_id: &str,
        host_env_id: &str,
        cx: &App,
    ) -> Vec<t3_core::timeline::requests::LinkableMachine> {
        let app = self.state.read(cx);
        let existing: Vec<ExistingPeerLink> = match self.environments.peer_links.get(host_saved_id) {
            Some(PeerLinksLoad::Loaded { links, .. }) => links
                .iter()
                .map(|l| ExistingPeerLink {
                    environment_id: l.environment_id.clone(),
                    label: l.label.clone(),
                })
                .collect(),
            _ => Vec::new(),
        };
        let candidates: Vec<MachineCandidate> = app
            .environments
            .environments
            .iter()
            .filter(|saved| !saved.disabled)
            .map(|saved| {
                let fleet_view = app.fleet.environment(&saved.id);
                let env_id = fleet_view
                    .and_then(|v| v.config.config.as_ref())
                    .map(|c| c.environment.environment_id.clone())
                    .filter(|id| !id.is_empty())
                    .unwrap_or_else(|| saved.id.clone());
                let label = fleet_view
                    .and_then(|v| v.config.config.as_ref())
                    .map(|c| c.environment.label.clone())
                    .filter(|l| !l.trim().is_empty())
                    .or_else(|| (!saved.label.trim().is_empty()).then(|| saved.label.clone()))
                    .unwrap_or_else(|| saved.address());
                let reachable = fleet_view.is_some_and(|v| v.status == ConnectionStatus::Connected);
                let mut urls = Vec::new();
                if !saved.http_base.trim().is_empty() {
                    urls.push(saved.http_base.clone());
                }
                MachineCandidate { id: env_id, label, urls, reachable }
            })
            .collect();
        linkable_machines(&candidates, &existing, Some(host_env_id))
    }

    fn can_mint_on_machine(&self, target_env_id: &str, cx: &App) -> Option<t3_client::auth::Credentials> {
        let app = self.state.read(cx);
        for view in &app.fleet.environments {
            if view.status != ConnectionStatus::Connected {
                continue;
            }
            let Some(cfg) = view.config.config.as_ref() else {
                continue;
            };
            if cfg.environment.environment_id != target_env_id {
                continue;
            }
            if !can_manage_peer_links(view.grants.as_ref()) {
                return None;
            }
            return app
                .environments
                .get(&view.summary.owner.saved_id)
                .and_then(|s| s.credentials());
        }
        None
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
        let machine = machines.iter().find(|m| m.id == selected).cloned();
        let needs_address = machine.as_ref().is_none_or(|m| m.urls.is_empty());
        let typed_address = dialog.address.read(cx).value().trim().to_owned();
        let typed_code = dialog.pairing_code.read(cx).value().trim().to_owned();
        let access = dialog.access.clone();

        let (url, alternate_urls, expected_id, expected_label) = match (&machine, needs_address) {
            (Some(m), false) => {
                let first = m.urls[0].clone();
                let rest = m.urls[1..].to_vec();
                (first, rest, Some(m.id.clone()), Some(m.label.clone()))
            }
            _ => {
                if typed_address.is_empty() {
                    return;
                }
                (typed_address, Vec::new(), None, None)
            }
        };

        let mint_creds = if !needs_address
            && let Some(m) = &machine
            && !dialog.mint_failed.contains(&m.id)
        {
            self.can_mint_on_machine(&m.id, cx)
        } else {
            None
        };
        if mint_creds.is_none() && typed_code.is_empty() {
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
        let mint_target_id = machine.as_ref().map(|m| m.id.clone());

        let task = runtime.spawn(async move {
            let code = if let Some(creds) = mint_creds {
                let scopes = mcp_client_scopes(&access);
                let body = json!({
                    "label": "T3 Code link",
                    "scopes": scopes,
                });
                match post_access(&creds, "/api/auth/pairing-token", Some(body)).await {
                    Ok(reply) => reply
                        .get("credential")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| (true, "Target environment did not return a pairing code.".to_owned()))?,
                    Err(err) => return Err((true, format!("Could not mint pairing code: {err}"))),
                }
            } else {
                typed_code
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

    pub(super) fn environments(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let p = palette(cx);
        let state = self.state.read(cx);
        let environment = state
            .server_config
            .config
            .as_ref()
            .map(|config| config.environment.clone())
            .unwrap_or_default();
        let saved = state.active_environment().cloned();
        let address = saved.as_ref().map(SavedEnvironment::address).unwrap_or_default();
        let status = match &state.phase {
            Phase::Online(status) => status.summary().0,
            Phase::Unpaired => "Not connected".to_owned(),
        };
        let label = [environment.label.as_str(), saved.as_ref().map_or("", |saved| saved.label.as_str())]
            .into_iter()
            .find(|label| !label.trim().is_empty())
            .unwrap_or("Environment")
            .to_owned();
        let protocol = environment
            .orchestration_protocol_version
            .unwrap_or(ORCHESTRATION_PROTOCOL_VERSION);
        let value = |text: String| muted_text(text, cx);
        let (switch_state, disconnect_state, remove_state) =
            (self.state.clone(), self.state.clone(), self.state.clone());
        let cloud_state = self.state.clone();
        let remove_id = saved.as_ref().map(|saved| saved.id.clone());

        let mut env_rows = vec![
            Row::new("remote-environments", label)
                .description(if address.is_empty() {
                    "This device's paired environment.".to_owned()
                } else {
                    address
                })
                .highlighted(self.is_target("remote-environments"))
                .below(
                    div().children(
                        state
                            .server_config
                            .config
                            .as_ref()
                            .and_then(|c| c.managed_policy.as_ref())
                            .map(|policy| crate::components::managed::attribution(policy, cx)),
                    ),
                )
                .control(value(status))
                .into_any_element(),
            Row::new("server-version", "Server version")
                .control(value(if environment.server_version.is_empty() {
                    "Unknown".to_owned()
                } else {
                    environment.server_version.clone()
                }))
                .into_any_element(),
            Row::new("orchestration-protocol", "Orchestration protocol")
                .control(value(protocol.to_string()))
                .into_any_element(),
            Row::new("cloud-account", "T3 Connect")
                .description("Sign in to view your T3 Connect environments.")
                .control(
                    Button::new("cloud-account-button", ButtonVariant::Outline, ButtonSize::Sm)
                        .label("Manage\u{2026}")
                        .on_click(move |_, window, cx| {
                            close(window, cx);
                            cloud_state.update(cx, |state, cx| state.open_switcher(cx));
                        }),
                )
                .into_any_element(),
            Row::new("switch-environment", "Switch environment")
                .description("Connect to another saved environment, or pair a new one.")
                .control(
                    Button::new("switch-environment-button", ButtonVariant::Outline, ButtonSize::Sm)
                        .label("Switch\u{2026}")
                        .on_click(move |_, window, cx| {
                            close(window, cx);
                            switch_state.update(cx, |state, cx| state.open_switcher(cx));
                        }),
                )
                .into_any_element(),
            Row::new("disconnect", "Disconnect")
                .description("Stop using this environment for now. It stays saved on this device.")
                .control(
                    Button::new("disconnect-server", ButtonVariant::Outline, ButtonSize::Sm)
                        .label("Disconnect")
                        .on_click(move |_, window, cx| {
                            close(window, cx);
                            disconnect_state.update(cx, |state, cx| state.disconnect(cx));
                        }),
                )
                .into_any_element(),
            Row::new("remove-environment", "Remove from this device")
                .description(
                    "Forget this environment and its credential. Pair again with a new pairing link.",
                )
                .control(
                    Button::new(
                        "remove-environment-button",
                        ButtonVariant::DestructiveOutline,
                        ButtonSize::Sm,
                    )
                    .label("Remove")
                    .disabled(remove_id.is_none())
                    .on_click(move |_, window, cx| {
                        let Some(id) = remove_id.clone() else {
                            return;
                        };
                        close(window, cx);
                        remove_state.update(cx, |state, cx| state.remove_environment(&id, cx));
                    }),
                )
                .into_any_element(),
        ];

        // Also surface other saved environments when multiple are saved.
        let active_saved_id = state.environments.active.as_deref();
        let other_saved: Vec<(String, String, String, bool)> = state
            .environments
            .environments
            .iter()
            .filter(|env| Some(env.id.as_str()) != active_saved_id)
            .map(|env| {
                let connected = state
                    .fleet
                    .environment(&env.id)
                    .is_some_and(|v| v.status == ConnectionStatus::Connected);
                let name = if env.label.trim().is_empty() {
                    env.address()
                } else {
                    env.label.clone()
                };
                (env.id.clone(), name, transport_subtitle(env), connected)
            })
            .collect();
        for (saved_id, name, subtitle, connected) in other_saved {
            let connect_state = self.state.clone();
            let connect_id = saved_id.clone();
            env_rows.push(
                Row::new(SharedString::from(format!("saved-env-{saved_id}")), name)
                    .description(format!(
                        "{subtitle} \u{00b7} {}",
                        if connected { "Connected" } else { "Saved" }
                    ))
                    .control(
                        Button::new(
                            SharedString::from(format!("connect-saved-env-{saved_id}")),
                            ButtonVariant::Outline,
                            ButtonSize::Xs,
                        )
                        .label("Switch to")
                        .on_click(move |_, window, cx| {
                            close(window, cx);
                            connect_state.update(cx, |state, cx| state.connect_environment(&connect_id, cx));
                        }),
                    )
                    .into_any_element(),
            );
        }

        let mut blocks = vec![Block::new(
            &["remote-environments", "connections"],
            section("Environments", env_rows, cx),
        )];

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
            .and_then(|sel| machines.iter().find(|m| m.id == sel));
        let needs_address = selected.is_some() && machine.is_none_or(|m| m.urls.is_empty());
        let can_mint = !needs_address
            && machine.is_some_and(|m| {
                !dialog.mint_failed.contains(&m.id) && self.can_mint_on_machine(&m.id, cx).is_some()
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
            .and_then(|sel| machines.iter().find(|m| m.id == sel));
        let needs_address = selected.is_some() && machine.is_none_or(|m| m.urls.is_empty());
        let can_mint = !needs_address
            && machine.is_some_and(|m| {
                !dialog.mint_failed.contains(&m.id) && self.can_mint_on_machine(&m.id, cx).is_some()
            });
        let address_val = dialog.address.read(cx).value().trim().to_owned();
        let ready = self.link_dialog_ready(cx);
        let busy = dialog.busy;
        let weak_choice = cx.entity().downgrade();
        let weak_access = cx.entity().downgrade();
        let weak_dismiss = cx.entity().downgrade();

        let mut body = v_flex().gap(r(14.));
        if !machines.is_empty() {
            let mut machine_options: Vec<SelectOption> = machines
                .iter()
                .map(|m| {
                    let mut opt = SelectOption::new(m.id.clone(), m.label.clone());
                    if m.existing_link_environment_id.is_some() {
                        opt = opt.description("Linked");
                    } else if !m.reachable {
                        opt = opt.description("Not connected");
                    }
                    opt
                })
                .collect();
            machine_options.push(SelectOption::new(ANOTHER_ADDRESS, "Another address\u{2026}"));
            let selected_label = match selected.as_deref() {
                Some(ANOTHER_ADDRESS) => "Another address\u{2026}".to_owned(),
                Some(id) => machines
                    .iter()
                    .find(|m| m.id == id)
                    .map(|m| m.label.clone())
                    .unwrap_or_else(|| "Choose a machine".to_owned()),
                None => "Choose a machine".to_owned(),
            };
            body = body.child(
                v_flex()
                    .gap(r(6.))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child("Machine"),
                    )
                    .child(
                        Select::new("peer-link-machine-select", machine_options)
                            .selected(selected.clone().unwrap_or_default())
                            .label(selected_label)
                            .width(360.)
                            .disabled(busy)
                            .on_change(move |value, _, cx| {
                                let _ = weak_choice.update(cx, |this, cx| {
                                    if let Some(d) = this.environments.link_dialog.as_mut() {
                                        d.choice = Some(value.to_string());
                                        d.error = None;
                                        cx.notify();
                                    }
                                });
                            }),
                    ),
            );
        }

        if machine.is_some_and(|m| m.existing_link_environment_id.is_some()) {
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
                    .child(Input::new(&dialog.address).small())
                    .children(no_reachable_note.map(|note| {
                        div().text_xs().text_color(hsla(p.muted_foreground)).child(note)
                    })),
            );
            if !address_val.is_empty() {
                body = body.child(
                    v_flex()
                        .px(r(12.))
                        .py(r(10.))
                        .gap(r(4.))
                        .rounded(r(8.))
                        .border_1()
                        .border_color(hsla(p.warning).opacity(0.4))
                        .bg(hsla(p.warning).opacity(0.08))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(hsla(p.foreground))
                                .child("Check this address"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(format!(
                                    "This app cannot confirm it is one of your machines. The pairing code you paste is sent there: {}",
                                    link_destination(&address_val)
                                )),
                        ),
                );
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
                    .child(Input::new(&dialog.pairing_code).small())
                    .children(help_note.map(|note| {
                        div().text_xs().text_color(hsla(p.muted_foreground)).child(note)
                    })),
            );
        }

        let access_options: Vec<SelectOption> = ACCESS_LEVELS
            .iter()
            .map(|level| {
                SelectOption::new(*level, link_access_label(level))
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
        vcx.update(|window, cx| crate::settings::finish_close(window, cx));
        state.update(&mut vcx, |state, _| state.stop_pump());
        vcx.run_until_parked();
        drop(vcx);
        drop(fixture);
        drop(runtime);
    }
}
