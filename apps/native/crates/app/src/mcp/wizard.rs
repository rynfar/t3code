//! "Use T3 Code in another app (MCP)" setup wizard modal.
//! Ports `apps/web/src/components/mcp/UseInAnotherAppDialog.tsx` and
//! `apps/web/src/components/ui/wizard.tsx`.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    AnyElement, App, BoxShadow, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, SharedString, Styled,
    Subscription, Task, Window, deferred, div, point, prelude::*, px,
};
use gpui_component::input::Escape;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use serde_json::Value;
use t3_client::supervisor::{ClientHandle, ConnectionStatus};
use t3_core::auth::AuthSessionState;
use t3_core::managed_policy::{self, Capability};
use t3_core::mcp::apps::{
    McpApp, OTHER_MCP_APP_STEPS, is_safe_mcp_handoff_url, mcp_handoff_action, mcp_handoff_commands,
    mcp_handoff_message, visible_mcp_apps,
};
use t3_core::mcp::gate::{
    CloudLinkState, ConnectReachability, DisconnectedRelay, McpCapabilities, McpGate, McpGateInput,
    ReachabilityCheckState, RelayEndpoint, RelayGateState, T3ConnectTurnOn, after_failed_check,
    is_managed_tunnel_active, resolve_mcp_gate, resolve_t3_connect_turn_on,
};
use t3_core::mcp::{
    AccessEnvironmentCandidate, McpClientAccess, MintedApprovalCode, PeerLinkEntry,
    RELAY_READ_SCOPE, RELAY_WRITE_SCOPE, approval_code_request_body, default_wizard_access,
    format_code_expiry, format_linked_line, resolve_access_environment_id, ungrantable_levels,
};
use t3_core::settings::search::Section;
use t3_core::sidebar::time::parse_iso;

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

use super::{access_option_card, command_block, copy_row, mcp_app_badge};

const STEPS: [&str; 3] = ["App", "Access", "Connect"];
const HOSTED_APPROVAL_STEPS: [&str; 2] = ["App", "Connect"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WizardStep {
    App = 0,
    Access = 1,
    Connect = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppChoice {
    App(&'static McpApp),
    Other,
}

struct ResolvedEnvironment {
    saved_id: String,
    canonical_id: Option<String>,
    label: String,
    this_machine: bool,
    connected: bool,
    grants: Option<AuthSessionState>,
    managed_policy: Option<Value>,
}

struct MintedCode {
    credential: String,
    expires_at_ms: i64,
}

pub struct UseInAnotherAppDialog {
    state: Entity<AppState>,
    open: bool,
    requested_environment: Option<String>,
    cloud_configured: bool,
    cloud_checked: bool,
    /// Generation counter to ignore stale async responses across reopens / environment switches.
    generation: u64,
    active_target_id: Option<String>,
    active_client: Option<ClientHandle>,
    capabilities: Option<McpCapabilities>,
    managed_policy_override: Option<Value>,
    link_state: Option<CloudLinkState>,
    link_state_error: Option<String>,
    check_state: ReachabilityCheckState,
    connecting: bool,
    peer_links: Vec<PeerLinkEntry>,
    peer_links_loaded_for: Option<String>,
    pub(crate) step: WizardStep,
    pub(crate) choice: Option<AppChoice>,
    pub(crate) picked_access: Option<McpClientAccess>,
    pub(crate) another_way: bool,
    minted_code: Option<MintedCode>,
    mint_pending: bool,
    mint_error: Option<String>,
    copied_key: Option<String>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    _load_task: Option<Task<()>>,
    _check_task: Option<Task<()>>,
    _poll_task: Option<Task<()>>,
    _mint_task: Option<Task<()>>,
    _code_ticker: Option<Task<()>>,
    _copy_reset: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for UseInAnotherAppDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl UseInAnotherAppDialog {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe_in(&state, window, |this, _, event: &AppEvent, _, cx| {
            if !this.open {
                return;
            }
            if matches!(event, AppEvent::Phase | AppEvent::ServerConfig) {
                if this.state.read(cx).cloud.identity.is_some() || !this.state.read(cx).cloud.busy {
                    this.cloud_checked = true;
                }
                this.sync_target(cx);
                cx.notify();
            }
        });
        Self {
            state,
            open: false,
            requested_environment: None,
            cloud_configured: t3_client::connect::Config::load().is_ok(),
            cloud_checked: false,
            generation: 0,
            active_target_id: None,
            active_client: None,
            capabilities: None,
            managed_policy_override: None,
            link_state: None,
            link_state_error: None,
            check_state: ReachabilityCheckState::Idle,
            connecting: false,
            peer_links: Vec::new(),
            peer_links_loaded_for: None,
            step: WizardStep::App,
            choice: None,
            picked_access: None,
            another_way: false,
            minted_code: None,
            mint_pending: false,
            mint_error: None,
            copied_key: None,
            focus: cx.focus_handle(),
            previous_focus: None,
            _load_task: None,
            _check_task: None,
            _poll_task: None,
            _mint_task: None,
            _code_ticker: None,
            _copy_reset: None,
            _subscriptions: vec![subscription],
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(
        &mut self,
        requested_environment: Option<String>,
        window: gpui::AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        self.open = true;
        self.requested_environment = requested_environment;
        self.cloud_configured = t3_client::connect::Config::load().is_ok();
        self.generation += 1;
        self.active_target_id = None;
        self.active_client = None;
        self.capabilities = None;
        self.managed_policy_override = None;
        self.link_state = None;
        self.link_state_error = None;
        self.check_state = ReachabilityCheckState::Idle;
        self.connecting = false;
        self.peer_links.clear();
        self.peer_links_loaded_for = None;
        self.step = WizardStep::App;
        self.choice = None;
        self.picked_access = None;
        self.another_way = false;
        self.minted_code = None;
        self.mint_pending = false;
        self.mint_error = None;
        self.copied_key = None;
        self._load_task = None;
        self._check_task = None;
        self._poll_task = None;
        self._mint_task = None;
        self._code_ticker = None;
        self._copy_reset = None;

        // Ensure cloud identity / discovery have been checked at least once.
        let need_cloud_refresh = {
            let state = self.state.read(cx);
            state.cloud.identity.is_none()
                && !state.cloud.discovery.listed
                && !state.cloud.busy
                && state.cloud.error.is_none()
        };
        if need_cloud_refresh && self.cloud_configured {
            self.cloud_checked = false;
            self.state.update(cx, |state, cx| state.refresh_cloud(cx));
        } else {
            self.cloud_checked = !self.state.read(cx).cloud.busy;
        }

        self.sync_target(cx);
        cx.notify();

        let dialog = cx.entity().clone();
        cx.defer(move |cx| {
            let _ = cx.update_window(window, |_, window, cx| {
                dialog.update(cx, |dialog, cx| {
                    dialog.previous_focus = window.focused(cx);
                    window.focus(&dialog.focus, cx);
                });
            });
        });
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.generation += 1;
        self._load_task = None;
        self._check_task = None;
        self._poll_task = None;
        self._mint_task = None;
        self._code_ticker = None;
        self._copy_reset = None;
        if let Some(prev) = self.previous_focus.take() {
            window.focus(&prev, cx);
        }
        cx.notify();
    }

    fn resolve_environment(&self, cx: &App) -> Option<ResolvedEnvironment> {
        let state = self.state.read(cx);
        let active_id = state.environments.active.as_deref();
        let req = self.requested_environment.as_deref();
        let mut candidates = Vec::new();
        for env in &state.environments.environments {
            let is_primary = active_id == Some(env.id.as_str());
            let is_requested = req.is_some_and(|r| {
                r == env.id || env.environment_id.as_deref() == Some(r)
            });
            if !is_primary && !is_requested && env.disabled {
                continue;
            }
            let connected = if is_primary {
                state.phase == crate::app_state::Phase::Online(ConnectionStatus::Connected)
            } else {
                state
                    .fleet
                    .environment(&env.id)
                    .is_some_and(|v| v.status == ConnectionStatus::Connected)
                    || state.environment_statuses.get(&env.id) == Some(&ConnectionStatus::Connected)
            };
            candidates.push(AccessEnvironmentCandidate {
                id: env.id.clone(),
                environment_id: env.environment_id.clone(),
                connected,
            });
        }
        let chosen_id = resolve_access_environment_id(active_id, &candidates, req)?;
        let env = state
            .environments
            .environments
            .iter()
            .find(|e| e.id == chosen_id)?;
        let is_active = active_id == Some(env.id.as_str());
        let fleet_view = state.fleet.environment(&env.id);
        let connected = if is_active {
            state.phase == crate::app_state::Phase::Online(ConnectionStatus::Connected)
        } else {
            fleet_view.is_some_and(|v| v.status == ConnectionStatus::Connected)
                || state.environment_statuses.get(&env.id) == Some(&ConnectionStatus::Connected)
        };
        let grants = if is_active {
            state
                .client_handle()
                .and_then(|c| c.session_state().borrow().clone())
                .or_else(|| fleet_view.and_then(|v| v.grants.clone()))
        } else {
            fleet_view.and_then(|v| v.grants.clone())
        };
        let config = if is_active {
            state
                .server_config
                .config
                .as_ref()
                .or_else(|| fleet_view.and_then(|v| v.config.config.as_ref()))
        } else {
            fleet_view.and_then(|v| v.config.config.as_ref())
        };
        let canonical_id = env
            .environment_id
            .clone()
            .or_else(|| {
                config
                    .map(|c| c.environment.environment_id.clone())
                    .filter(|s| !s.is_empty())
            });
        let label = if !env.label.trim().is_empty() {
            env.label.clone()
        } else if let Some(lbl) = config.map(|c| c.environment.label.as_str()).filter(|s| !s.trim().is_empty()) {
            lbl.to_owned()
        } else {
            "T3 Code".to_owned()
        };
        let managed_policy = config
            .and_then(|c| c.managed_policy.clone())
            .or_else(|| self.managed_policy_override.clone());
        Some(ResolvedEnvironment {
            saved_id: env.id.clone(),
            canonical_id,
            label,
            this_machine: env.local,
            connected,
            grants,
            managed_policy,
        })
    }

    fn relay_gate_state(&self, env: &ResolvedEnvironment, cx: &App) -> RelayGateState {
        let cloud = &self.state.read(cx).cloud;
        if cloud.discovery.refreshing || (cloud.busy && !cloud.discovery.listed) {
            return RelayGateState::Loading;
        }
        if cloud.error.is_some() && !cloud.discovery.listed {
            return RelayGateState::Failed;
        }
        if !cloud.discovery.listed {
            return RelayGateState::Loading;
        }
        let endpoint = cloud
            .discovery
            .environments
            .iter()
            .find(|candidate| {
                env.canonical_id.as_deref() == Some(candidate.environment_id.as_str())
                    || candidate.environment_id == env.saved_id
            })
            .map(|entry| RelayEndpoint::from(&entry.endpoint));
        RelayGateState::Listed {
            endpoint,
            availability: None,
        }
    }

    fn current_gate(&self, env: &ResolvedEnvironment, cx: &App) -> McpGate {
        let cloud = &self.state.read(cx).cloud;
        let signed_in = if cloud.identity.is_some() {
            Some(true)
        } else if self.cloud_checked && !cloud.busy {
            Some(false)
        } else {
            None
        };
        let can_read_relay = env
            .grants
            .as_ref()
            .is_some_and(|session| session.grants(RELAY_READ_SCOPE));
        let relay = self.relay_gate_state(env, cx);
        let reported = self
            .link_state
            .as_ref()
            .and_then(|s| s.reachability.as_ref());
        let reachability = self.check_state.effective(reported);
        resolve_mcp_gate(&McpGateInput {
            signed_in,
            connected: env.connected,
            approval_codes: self.capabilities.map(|c| c.mcp_approval_codes),
            can_read_relay,
            link_state: self.link_state.as_ref(),
            link_state_error: self.link_state_error.as_deref(),
            this_machine: env.this_machine,
            relay: &relay,
            reachability: &reachability,
        })
    }

    /// Ensures capabilities, link state, discovery, reachability check, and peer links are loaded
    /// for the resolved environment.
    fn sync_target(&mut self, cx: &mut Context<Self>) {
        let Some(env) = self.resolve_environment(cx) else {
            return;
        };
        // Ask T3 Connect for its machine list when opening signed in for a saved machine.
        {
            let state = self.state.read(cx);
            if !env.this_machine
                && state.cloud.identity.is_some()
                && !state.cloud.discovery.listed
                && !state.cloud.discovery.refreshing
                && !state.cloud.busy
            {
                self.state.update(cx, |state, cx| state.refresh_cloud(cx));
            }
        }
        if !env.connected {
            self._poll_task = None;
            return;
        }
        let target_changed = self.active_target_id.as_deref() != Some(env.saved_id.as_str());
        if target_changed {
            self.active_target_id = Some(env.saved_id.clone());
            self.active_client = None;
            self.capabilities = None;
            self.managed_policy_override = None;
            self.link_state = None;
            self.link_state_error = None;
            self.check_state = ReachabilityCheckState::Idle;
            self.peer_links.clear();
            self.peer_links_loaded_for = None;
            self.load_environment_data(env.saved_id.clone(), env.grants.as_ref().is_some_and(|s| s.grants(RELAY_READ_SCOPE)), cx);
        } else if self.link_state.is_none()
            && self.link_state_error.is_none()
            && self._load_task.is_none()
            && env.grants.as_ref().is_some_and(|s| s.grants(RELAY_READ_SCOPE))
        {
            self.load_environment_data(env.saved_id.clone(), true, cx);
        }

        // Automatically run reachability check once tunnel is active and check is idle.
        let can_check = env.connected
            && self
                .link_state
                .as_ref()
                .is_some_and(|ls| is_managed_tunnel_active(ls, None));
        if can_check && self.check_state == ReachabilityCheckState::Idle && self.active_client.is_some() {
            self.run_reachability_check(cx);
        }

        // Manage 5s polling while waiting for tunnel to come up.
        let gate = self.current_gate(&env, cx);
        let waiting = matches!(gate, McpGate::Offline { .. })
            || (self.connecting && matches!(gate, McpGate::TurnOn));
        if waiting {
            self.ensure_poll_task(cx);
        } else {
            self._poll_task = None;
        }
    }

    fn resolve_client_future(&self, saved_id: &str, cx: &App) -> Option<ClientHandle> {
        let state = self.state.read(cx);
        if state.environments.active.as_deref() == Some(saved_id) {
            state.client_handle()
        } else {
            None
        }
    }

    fn load_environment_data(&mut self, saved_id: String, can_read_relay: bool, cx: &mut Context<Self>) {
        let generation = self.generation;
        let active_client = self.resolve_client_future(&saved_id, cx);
        let client_future = if active_client.is_none() {
            Some(self.state.read(cx).connections.client(&saved_id))
        } else {
            None
        };
        let runtime = self.state.read(cx).runtime().clone();
        self._load_task = Some(cx.spawn(async move |this, cx| {
            let client = match active_client {
                Some(client) => Some(client),
                None => match client_future {
                    Some(fut) => fut.await,
                    None => None,
                },
            };
            let Some(client) = client else {
                return;
            };
            let net_client = client.clone();
            let fetched = runtime
                .spawn(async move {
                    // Prefer server.getConfig (returns full capabilities + managedPolicy and works in fixtures),
                    // falling back to GET /.well-known/t3/environment.
                    let raw_config = match net_client
                        .request("server.getConfig", serde_json::json!({}))
                        .await
                    {
                        Ok(value) => Some(value),
                        Err(_) => net_client
                            .get_access("/.well-known/t3/environment")
                            .await
                            .ok(),
                    };
                    let caps = raw_config
                        .as_ref()
                        .map(McpCapabilities::from_json)
                        .unwrap_or_default();
                    let managed_policy = raw_config
                        .as_ref()
                        .and_then(|v| v.get("managedPolicy"))
                        .filter(|v| !v.is_null())
                        .cloned();
                    let link_state = if can_read_relay {
                        Some(
                            net_client
                                .get_access("/api/connect/link-state")
                                .await
                                .and_then(|v| {
                                    serde_json::from_value::<CloudLinkState>(v)
                                        .map_err(|_| "Could not read environment cloud link state.".to_owned())
                                }),
                        )
                    } else {
                        None
                    };
                    let peer_links = if caps.peer_links {
                        net_client
                            .request("peerLinks.list", serde_json::json!({}))
                            .await
                            .ok()
                            .and_then(|v| v.get("links").cloned())
                            .and_then(|v| serde_json::from_value::<Vec<PeerLinkEntry>>(v).ok())
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    (caps, managed_policy, link_state, peer_links)
                })
                .await;
            let Ok((caps, managed_policy, link_state, peer_links)) = fetched else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if !this.open || this.generation != generation {
                    return;
                }
                this.active_client = Some(client);
                this.capabilities = Some(caps);
                this.managed_policy_override = managed_policy;
                if let Some(res) = link_state {
                    match res {
                        Ok(ls) => {
                            this.link_state = Some(ls);
                            this.link_state_error = None;
                        }
                        Err(err) => {
                            this.link_state_error = Some(err);
                        }
                    }
                }
                this.peer_links = peer_links;
                this.peer_links_loaded_for = Some(saved_id);
                this._load_task = None;
                this.sync_target(cx);
                cx.notify();
            });
        }));
    }

    fn run_reachability_check(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.active_client.clone() else {
            return;
        };
        let generation = self.generation;
        let prior = self.link_state.clone();
        let this_machine = self
            .resolve_environment(cx)
            .is_some_and(|env| env.this_machine);
        if !this_machine {
            self.state.update(cx, |state, cx| state.refresh_cloud(cx));
        }
        self.check_state = ReachabilityCheckState::Running;
        cx.notify();

        let runtime = self.state.read(cx).runtime().clone();
        self._check_task = Some(cx.spawn(async move |this, cx| {
            let outcome = runtime
                .spawn(async move {
                    let check_res = client
                        .post_access("/api/connect/reachability", None)
                        .await
                        .and_then(|v| {
                            serde_json::from_value::<ConnectReachability>(v)
                                .map_err(|_| "Could not check T3 Connect.".to_owned())
                        });
                    let latest = client
                        .get_access("/api/connect/link-state")
                        .await
                        .ok()
                        .and_then(|v| serde_json::from_value::<CloudLinkState>(v).ok());
                    (check_res, latest)
                })
                .await;
            let Ok((check_res, latest)) = outcome else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if !this.open || this.generation != generation {
                    return;
                }
                if let Some(ls) = latest.clone() {
                    this.link_state = Some(ls);
                    this.link_state_error = None;
                }
                match check_res {
                    Ok(reachability) => {
                        let since = Some(reachability.checked_at().to_owned());
                        this.check_state = ReachabilityCheckState::Done {
                            result: Some(reachability),
                            since,
                        };
                    }
                    Err(_) => {
                        let failed = after_failed_check(prior.as_ref(), latest.as_ref());
                        this.check_state = ReachabilityCheckState::Done {
                            result: failed.result,
                            since: failed.since,
                        };
                    }
                }
                this._check_task = None;
                this.sync_target(cx);
                cx.notify();
            });
        }));
    }

    fn ensure_poll_task(&mut self, cx: &mut Context<Self>) {
        if self._poll_task.is_some() {
            return;
        }
        let Some(client) = self.active_client.clone() else {
            return;
        };
        let generation = self.generation;
        let runtime = self.state.read(cx).runtime().clone();
        self._poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let client_clone = client.clone();
                let fetched = runtime
                    .spawn(async move {
                        client_clone
                            .get_access("/api/connect/link-state")
                            .await
                            .ok()
                            .and_then(|v| serde_json::from_value::<CloudLinkState>(v).ok())
                    })
                    .await;
                let keep_going = this.update(cx, |this, cx| {
                    if !this.open || this.generation != generation {
                        return false;
                    }
                    if let Ok(Some(ls)) = fetched {
                        this.link_state = Some(ls);
                        this.link_state_error = None;
                    }
                    let Some(env) = this.resolve_environment(cx) else {
                        return false;
                    };
                    let gate = this.current_gate(&env, cx);
                    cx.notify();
                    matches!(gate, McpGate::Offline { .. })
                        || (this.connecting && matches!(gate, McpGate::TurnOn))
                });
                if !matches!(keep_going, Ok(true)) {
                    let _ = this.update(cx, |this, _| {
                        this._poll_task = None;
                    });
                    return;
                }
            }
        }));
    }

    fn mint_approval_code(&mut self, access: McpClientAccess, env_name: String, cx: &mut Context<Self>) {
        let Some(client) = self.active_client.clone() else {
            return;
        };
        let generation = self.generation;
        self.mint_pending = true;
        self.mint_error = None;
        cx.notify();

        let body = approval_code_request_body(access);
        let runtime = self.state.read(cx).runtime().clone();
        self._mint_task = Some(cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn(async move {
                    client
                        .post_access("/api/auth/pairing-token", Some(body))
                        .await
                        .and_then(|v| {
                            serde_json::from_value::<MintedApprovalCode>(v)
                                .map_err(|_| "Invalid pairing code response.".to_owned())
                        })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.open || this.generation != generation {
                    return;
                }
                this.mint_pending = false;
                this._mint_task = None;
                match result {
                    Ok(Ok(minted)) => {
                        let expires_at_ms = parse_iso(&minted.expires_at)
                            .unwrap_or_else(|| now_ms() + 5 * 60 * 1000);
                        this.minted_code = Some(MintedCode {
                            credential: minted.credential,
                            expires_at_ms,
                        });
                        this.mint_error = None;
                        this.ensure_code_ticker(cx);
                    }
                    _ => {
                        this.mint_error =
                            Some(format!("Could not get a code from {env_name}. Try again."));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn ensure_code_ticker(&mut self, cx: &mut Context<Self>) {
        if self._code_ticker.is_some() {
            return;
        }
        let generation = self.generation;
        self._code_ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep_going = this.update(cx, |this, cx| {
                    if !this.open || this.generation != generation {
                        return false;
                    }
                    let live = this
                        .minted_code
                        .as_ref()
                        .is_some_and(|code| now_ms() < code.expires_at_ms);
                    cx.notify();
                    live
                });
                if !matches!(keep_going, Ok(true)) {
                    let _ = this.update(cx, |this, _| {
                        this._code_ticker = None;
                    });
                    return;
                }
            }
        }));
    }

    fn copy_text(&mut self, key: String, text: String, toast_title: Option<&'static str>, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied_key = Some(key.clone());
        if let Some(title) = toast_title {
            self.state.update(cx, |state, cx| {
                state.show_toast(Toast::new(ToastKind::Success, title), cx);
            });
        }
        let generation = self.generation;
        self._copy_reset = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1_500))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation && this.copied_key.as_deref() == Some(key.as_str()) {
                    this.copied_key = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn open_external_url(&mut self, url: &str, cx: &mut Context<Self>) {
        if !is_safe_mcp_handoff_url(url) {
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    Toast::new(ToastKind::Error, "Could not open the link")
                        .description(url.to_owned()),
                    cx,
                );
            });
            return;
        }
        cx.open_url(url);
    }

    fn hand_off_app(&mut self, app: &'static McpApp, mcp_url: &str, cx: &mut Context<Self>) {
        let Some(action) = mcp_handoff_action(app, mcp_url) else {
            return;
        };
        if let Some(copy) = &action.copy {
            self.copy_text(
                format!("handoff-{}", app.id),
                copy.text.clone(),
                Some(copy.title),
                cx,
            );
        }
        if let Some(app_link) = &action.app_link {
            self.open_external_url(app_link, cx);
        } else {
            self.open_external_url(&action.link, cx);
        }
    }

    fn hosted_approval(&self) -> bool {
        self.capabilities
            .is_some_and(|c| c.mcp_hosted_approval)
    }

    fn next_step(&mut self, cx: &mut Context<Self>) {
        let hosted = self.hosted_approval();
        self.step = match self.step {
            WizardStep::App => {
                if hosted {
                    self.another_way = self.choice == Some(AppChoice::Other);
                    WizardStep::Connect
                } else {
                    WizardStep::Access
                }
            }
            WizardStep::Access => {
                self.another_way = self.choice == Some(AppChoice::Other);
                WizardStep::Connect
            }
            WizardStep::Connect => WizardStep::Connect,
        };
        cx.notify();
    }

    fn select_access(&mut self, level: McpClientAccess, cx: &mut Context<Self>) {
        if self.picked_access != Some(level) {
            self.picked_access = Some(level);
            self.minted_code = None;
            self.mint_error = None;
            self._code_ticker = None;
            cx.notify();
        }
    }

    fn render_wizard_steps(&self, hosted: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let steps: &[&str] = if hosted { &HOSTED_APPROVAL_STEPS } else { &STEPS };
        let shown_step = if hosted && self.step == WizardStep::Connect {
            1
        } else {
            self.step as usize
        };
        h_flex()
            .gap(px(4.))
            .p(px(4.))
            .rounded(px(12.))
            .bg(if dark {
                hsla(p.foreground).opacity(0.04)
            } else {
                hsla(p.background)
            })
            .border_1()
            .border_color(hsla(p.border))
            .children(steps.iter().enumerate().map(|(index, label)| {
                let is_current = index == shown_step;
                let is_done = index < shown_step;
                let disabled = index > shown_step;
                let target_step = if hosted && index == 1 {
                    WizardStep::Connect
                } else if index == 0 {
                    WizardStep::App
                } else if index == 1 {
                    WizardStep::Access
                } else {
                    WizardStep::Connect
                };
                h_flex()
                    .id(SharedString::from(format!("mcp-step-{index}")))
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .when(is_current, |this| {
                        this.bg(hsla(p.card))
                            .border_1()
                            .border_color(hsla(p.border))
                    })
                    .when(!disabled, |this| {
                        this.cursor_pointer()
                            .hover(|style| {
                                if is_current {
                                    style
                                } else {
                                    style.bg(hsla(p.card).opacity(0.7))
                                }
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.step = target_step;
                                cx.notify();
                            }))
                    })
                    .child(
                        div()
                            .size(px(20.))
                            .flex_none()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .when(is_done, |this| {
                                this.bg(hsla(p.primary))
                                    .text_color(hsla(p.primary_foreground))
                                    .child(Icon::Check.render(px(12.), hsla(p.primary_foreground)))
                            })
                            .when(is_current, |this| {
                                this.bg(hsla(p.primary).opacity(0.12))
                                    .border_1()
                                    .border_color(hsla(p.primary).opacity(0.3))
                                    .text_color(hsla(p.primary))
                                    .child(format!("{}", index + 1))
                            })
                            .when(!is_done && !is_current, |this| {
                                this.bg(hsla(p.card))
                                    .border_1()
                                    .border_color(hsla(p.border))
                                    .text_color(hsla(p.muted_foreground))
                                    .child(format!("{}", index + 1))
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if is_current {
                                hsla(p.foreground)
                            } else {
                                hsla(p.muted_foreground)
                            })
                            .child(*label),
                    )
            }))
            .into_any_element()
    }

    fn render_gate_message(
        title: impl Into<SharedString>,
        lines: Vec<String>,
        extra: Option<AnyElement>,
        cx: &App,
    ) -> AnyElement {
        let p = palette(cx);
        v_flex()
            .gap(px(12.))
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .child(title.into()),
            )
            .when(!lines.is_empty(), |this| {
                this.child(v_flex().gap(px(4.)).children(lines.into_iter().map(|line| {
                    div()
                        .text_sm()
                        .text_color(hsla(p.muted_foreground))
                        .child(line)
                })))
            })
            .children(extra)
            .into_any_element()
    }

    fn render_gate_screen(
        &self,
        env: &ResolvedEnvironment,
        gate: &McpGate,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let name = env.label.clone();
        let check_again_btn = || {
            Button::new("mcp-check-again", ButtonVariant::Outline, ButtonSize::Sm)
                .label("Check again")
                .on_click(cx.listener(|this, _, _, cx| this.run_reachability_check(cx)))
                .into_any_element()
        };
        let refresh_list_btn = || {
            Button::new("mcp-refresh-list", ButtonVariant::Outline, ButtonSize::Sm)
                .label("Check again")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |state, cx| state.refresh_cloud(cx));
                }))
                .into_any_element()
        };

        match gate {
            McpGate::Checking => h_flex()
                .items_center()
                .gap(px(8.))
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child(Icon::LoaderCircle.render(px(14.), hsla(p.muted_foreground)))
                .child("Checking T3 Connect\u{2026}")
                .into_any_element(),
            McpGate::SignIn => Self::render_gate_message(
                "Sign in to T3 Connect",
                vec![
                    format!(
                        "Apps like Poke and Claude run in the cloud. They need a public address to reach {name}."
                    ),
                    format!(
                        "T3 Connect gives {name} that address. Each app still needs your approval."
                    ),
                ],
                Some(
                    h_flex()
                        .child(
                            Button::new("mcp-sign-in", ButtonVariant::Default, ButtonSize::Sm)
                                .label("Sign in to T3 Connect")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close(window, cx);
                                    this.state.update(cx, |state, cx| state.sign_in_cloud(cx));
                                })),
                        )
                        .into_any_element(),
                ),
                cx,
            ),
            McpGate::Disconnected { relay } => match relay {
                None => Self::render_gate_message(
                    format!("Connect to {name} first."),
                    vec![],
                    None,
                    cx,
                ),
                Some(DisconnectedRelay::Offline) => Self::render_gate_message(
                    format!("{name}'s T3 Connect is offline"),
                    vec![],
                    Some(h_flex().child(refresh_list_btn()).into_any_element()),
                    cx,
                ),
                Some(DisconnectedRelay::Failed) => Self::render_gate_message(
                    format!("Can't check T3 Connect on {name}"),
                    vec![],
                    Some(h_flex().child(refresh_list_btn()).into_any_element()),
                    cx,
                ),
            },
            McpGate::Update => Self::render_gate_message(
                format!("Update T3 Code on {name}"),
                vec![],
                None,
                cx,
            ),
            McpGate::NoRelayRead => Self::render_gate_message(
                format!("Can't check T3 Connect on {name}"),
                vec![format!(
                    "This device needs the View relay permission on {name}."
                )],
                None,
                cx,
            ),
            McpGate::Error { message } => Self::render_gate_message(
                format!("Can't check T3 Connect on {name}"),
                vec![message.clone()],
                Some(h_flex().child(check_again_btn()).into_any_element()),
                cx,
            ),
            McpGate::Offline { reachability } if !self.connecting => {
                let (title, lines) = match reachability {
                    Some(info) => (
                        format!("{name}'s T3 Connect is offline"),
                        vec![info.reason.clone(), format!("Fix: {}", info.fix)],
                    ),
                    None => (format!("Can't check T3 Connect on {name}"), vec![]),
                };
                Self::render_gate_message(
                    title,
                    lines,
                    Some(h_flex().child(check_again_btn()).into_any_element()),
                    cx,
                )
            }
            McpGate::TurnOn | McpGate::Offline { .. } => {
                let can_read_relay = env
                    .grants
                    .as_ref()
                    .is_some_and(|s| s.grants(RELAY_READ_SCOPE));
                let can_write_relay = env
                    .grants
                    .as_ref()
                    .is_some_and(|s| s.grants(RELAY_WRITE_SCOPE));
                let connect_denial = managed_policy::capability_denial(
                    env.managed_policy.as_ref(),
                    Capability::Connect,
                );
                // Native uses CLI OAuth without a Clerk JWT DPoP linker, so on-host `t3 connect link`
                // handles tunnel linking unless managed policy denies T3 Connect.
                let turn_on = resolve_t3_connect_turn_on(
                    true,
                    false,
                    self.cloud_configured,
                    can_read_relay,
                    can_write_relay,
                    connect_denial,
                );
                let body = match turn_on {
                    T3ConnectTurnOn::Policy { reason } => div()
                        .text_sm()
                        .text_color(hsla(p.muted_foreground))
                        .child(reason)
                        .into_any_element(),
                    T3ConnectTurnOn::Here | T3ConnectTurnOn::OnHost => {
                        let copied = self.copied_key.as_deref() == Some("cmd-t3-connect-link");
                        let view = cx.entity().clone();
                        v_flex()
                            .gap(px(8.))
                            .child(command_block(
                                "mcp-cmd-turn-on",
                                "t3 connect link",
                                copied,
                                move |_, cx| {
                                    view.update(cx, |this, cx| {
                                        this.copy_text(
                                            "cmd-t3-connect-link".to_owned(),
                                            "t3 connect link".to_owned(),
                                            Some("Command copied"),
                                            cx,
                                        );
                                    });
                                },
                                cx,
                            ))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(hsla(p.foreground))
                                    .child(format!("Restart T3 Code on {name}.")),
                            )
                            .child(h_flex().child(check_again_btn()))
                            .into_any_element()
                    }
                };
                Self::render_gate_message(
                    format!("Turn on T3 Connect for {name}"),
                    vec![
                        format!("{name} gets a public https address, with no router setup."),
                        "Only apps you approve can use it. You can turn it off in Settings."
                            .to_owned(),
                    ],
                    Some(body),
                    cx,
                )
            }
            McpGate::Open { .. } => div().into_any_element(),
        }
    }

    fn render_app_step(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let apps: Vec<&'static McpApp> = visible_mcp_apps().collect();
        let rows: Vec<&[&'static McpApp]> = apps.chunks(4).collect();
        let other_selected = self.choice == Some(AppChoice::Other);

        v_flex()
            .gap(px(12.))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .child("Which app?"),
            )
            .child(
                v_flex()
                    .gap(px(8.))
                    .children(rows.into_iter().map(|row| {
                        h_flex()
                            .gap(px(8.))
                            .children(row.iter().copied().map(|app| {
                                let selected = self.choice == Some(AppChoice::App(app));
                                v_flex()
                                    .id(SharedString::from(format!("mcp-app-{}", app.id)))
                                    .flex_1()
                                    .min_w_0()
                                    .items_center()
                                    .gap(px(8.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(if selected {
                                        hsla(p.primary)
                                    } else {
                                        gpui::transparent_black()
                                    })
                                    .when(selected, |this| this.bg(hsla(p.primary).opacity(0.08)))
                                    .px(px(4.))
                                    .py(px(12.))
                                    .cursor_pointer()
                                    .hover(|style| {
                                        if selected {
                                            style
                                        } else {
                                            style.bg(hsla(p.accent))
                                        }
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.choice = Some(AppChoice::App(app));
                                        cx.notify();
                                    }))
                                    .child(mcp_app_badge(app.id, app.name, cx))
                                    .child(
                                        div()
                                            .max_w_full()
                                            .truncate()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(hsla(p.foreground))
                                            .child(app.name),
                                    )
                            }))
                    })),
            )
            .child(
                div()
                    .id("mcp-app-other")
                    .w_full()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if other_selected {
                        hsla(p.primary)
                    } else {
                        hsla(p.border)
                    })
                    .when(other_selected, |this| this.bg(hsla(p.primary).opacity(0.08)))
                    .px(px(12.))
                    .py(px(10.))
                    .flex()
                    .justify_center()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .cursor_pointer()
                    .hover(|style| {
                        if other_selected {
                            style
                        } else {
                            style.bg(hsla(p.accent))
                        }
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.choice = Some(AppChoice::Other);
                        cx.notify();
                    }))
                    .child("Something else\u{2026}"),
            )
            .into_any_element()
    }

    fn render_access_step(
        &self,
        app_name: &str,
        access: Option<McpClientAccess>,
        disabled: &HashMap<McpClientAccess, String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let view = cx.entity().clone();
        v_flex()
            .gap(px(12.))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .child(format!("What can {app_name} do?")),
            )
            .child(
                v_flex()
                    .gap(px(8.))
                    .children(McpClientAccess::ALL.into_iter().map(|level| {
                        let view = view.clone();
                        access_option_card(
                            SharedString::from(format!("mcp-access-{}", level.as_str())),
                            level,
                            access == Some(level),
                            disabled.get(&level).map(String::as_str),
                            Some(level.wizard_description()),
                            move |_, cx| {
                                view.update(cx, |this, cx| this.select_access(level, cx));
                            },
                            cx,
                        )
                    })),
            )
            .into_any_element()
    }

    fn render_approval_code_section(
        &self,
        env_name: &str,
        app_name: &str,
        access: Option<McpClientAccess>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let Some(access) = access else {
            return div()
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child(format!(
                    "When {app_name} opens the sign-in page, choose what it may do and approve with your T3 Connect account."
                ))
                .into_any_element();
        };

        let now = now_ms();
        let live_code = self
            .minted_code
            .as_ref()
            .filter(|code| now < code.expires_at_ms);
        let remaining_secs = live_code
            .map(|code| ((code.expires_at_ms - now).max(0) / 1000) as u64)
            .unwrap_or(0);
        let env_label = env_name.to_owned();
        let copied = self.copied_key.as_deref() == Some("code");
        let view = cx.entity().clone();

        v_flex()
            .gap(px(6.))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .text_sm()
                            .text_color(hsla(p.foreground))
                            .child("Code to approve")
                            .child(div().font_weight(FontWeight::MEDIUM).child(access.label())),
                    )
                    .when(live_code.is_none(), |this| {
                        let label = if self.mint_pending {
                            "Getting code\u{2026}"
                        } else if self.minted_code.is_none() {
                            "Get code"
                        } else {
                            "New code"
                        };
                        this.child(
                            Button::new("mcp-get-code", ButtonVariant::Outline, ButtonSize::Sm)
                                .label(label)
                                .disabled(self.mint_pending)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mint_approval_code(access, env_label.clone(), cx);
                                })),
                        )
                    }),
            )
            .when_some(live_code, |this, code| {
                let value = code.credential.clone();
                let view = view.clone();
                this.child(copy_row(
                    "mcp-copy-code-row",
                    "Code",
                    &code.credential,
                    copied,
                    move |_, cx| {
                        view.update(cx, |this, cx| {
                            this.copy_text("code".to_owned(), value.clone(), Some("Code copied"), cx);
                        });
                    },
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(format_code_expiry(remaining_secs)),
                )
            })
            .when(self.minted_code.is_some() && live_code.is_none(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child("The code expired."),
                )
            })
            .when_some(self.mint_error.as_ref(), |this, err| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.destructive_foreground))
                        .child(err.clone()),
                )
            })
            .into_any_element()
    }

    fn render_linked_line(&self, access: Option<McpClientAccess>, cx: &App) -> Option<AnyElement> {
        let state = self.state.read(cx);
        let text = format_linked_line(&self.peer_links, access, |env_id| {
            state
                .environments
                .environments
                .iter()
                .find(|e| e.environment_id.as_deref() == Some(env_id) || e.id == env_id)
                .map(|e| e.label.clone())
        })?;
        let p = palette(cx);
        Some(
            div()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(text)
                .into_any_element(),
        )
    }

    fn render_connect_step(
        &self,
        env: &ResolvedEnvironment,
        mcp_url: &str,
        choice: AppChoice,
        access: Option<McpClientAccess>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let app = match choice {
            AppChoice::App(app) => Some(app),
            AppChoice::Other => None,
        };
        let app_name = app.map_or("the app", |a| a.name);
        let url_owned = mcp_url.to_owned();
        let url_copied = self.copied_key.as_deref() == Some("url");
        let view = cx.entity().clone();
        let url_row = copy_row(
            "mcp-copy-url-row",
            "URL",
            mcp_url,
            url_copied,
            move |_, cx| {
                view.update(cx, |this, cx| {
                    this.copy_text("url".to_owned(), url_owned.clone(), Some("URL copied"), cx);
                });
            },
            cx,
        );
        let code_section = self.render_approval_code_section(&env.label, app_name, access, cx);
        let linked = self.render_linked_line(access, cx);

        if self.another_way || app.is_none() {
            let message_row = app
                .and_then(|a| mcp_handoff_message(a, mcp_url))
                .map(|msg| {
                    let msg_copied = self.copied_key.as_deref() == Some("message");
                    let view = cx.entity().clone();
                    let msg_clone = msg.clone();
                    copy_row(
                        "mcp-copy-message-row",
                        "Message",
                        &msg,
                        msg_copied,
                        move |_, cx| {
                            view.update(cx, |this, cx| {
                                this.copy_text(
                                    "message".to_owned(),
                                    msg_clone.clone(),
                                    Some("Message copied"),
                                    cx,
                                );
                            });
                        },
                        cx,
                    )
                });
            let steps: &[&str] = app.map_or(OTHER_MCP_APP_STEPS, |a| a.steps);
            let docs_or_note = match app {
                None => Some(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child("Apps that require their own client ID cannot connect.")
                        .into_any_element(),
                ),
                Some(a) => a.docs.map(|docs_url| {
                    div()
                        .id("mcp-app-docs")
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_external_url(docs_url, cx);
                        }))
                        .child(format!("{} help", a.name))
                        .into_any_element()
                }),
            };
            let back_link = app.map(|a| {
                let label = if mcp_handoff_commands(a, mcp_url).is_some() {
                    "Back to commands"
                } else {
                    "Back to one click"
                };
                div()
                    .id("mcp-back-one-click")
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.another_way = false;
                        cx.notify();
                    }))
                    .child(label)
            });

            return v_flex()
                .gap(px(14.))
                .child(url_row)
                .children(message_row)
                .child(v_flex().gap(px(4.)).children(steps.iter().enumerate().map(
                    |(index, step)| {
                        div()
                            .text_sm()
                            .text_color(hsla(p.foreground))
                            .child(format!("{}. {step}", index + 1))
                    },
                )))
                .children(docs_or_note)
                .child(code_section)
                .children(back_link)
                .children(linked)
                .into_any_element();
        }

        let app = app.expect("checked above");
        let primary = if let Some(action) = mcp_handoff_action(app, mcp_url) {
            let mcp_url_owned = mcp_url.to_owned();
            v_flex()
                .gap(px(8.))
                .child(
                    Button::new("mcp-primary-handoff", ButtonVariant::Default, ButtonSize::Default)
                        .full_width()
                        .label(action.label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.hand_off_app(app, &mcp_url_owned, cx);
                        })),
                )
                .when_some(app.note, |this, note| {
                    this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(note))
                })
                .into_any_element()
        } else {
            let cmds = mcp_handoff_commands(app, mcp_url).unwrap_or_default();
            let view = cx.entity().clone();
            v_flex()
                .gap(px(8.))
                .children(cmds.into_iter().enumerate().map(|(idx, cmd)| {
                    let key = format!("cmd-{idx}");
                    let copied = self.copied_key.as_deref() == Some(key.as_str());
                    let cmd_clone = cmd.clone();
                    let view = view.clone();
                    command_block(
                        SharedString::from(format!("mcp-cmd-{idx}")),
                        &cmd,
                        copied,
                        move |_, cx| {
                            view.update(cx, |this, cx| {
                                this.copy_text(
                                    key.clone(),
                                    cmd_clone.clone(),
                                    Some("Command copied"),
                                    cx,
                                );
                            });
                        },
                        cx,
                    )
                }))
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(format!("Run these where {} runs.", app.name)),
                )
                .when_some(app.note, |this, note| {
                    this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(note))
                })
                .into_any_element()
        };

        v_flex()
            .gap(px(14.))
            .child(primary)
            .child(url_row)
            .child(code_section)
            .child(
                div()
                    .id("mcp-another-way")
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.another_way = true;
                        cx.notify();
                    }))
                    .child("Set up another way"),
            )
            .children(linked)
            .into_any_element()
    }
}

impl Render for UseInAnotherAppDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let p = palette(cx);
        let env = self.resolve_environment(cx);
        let label = env.as_ref().map(|e| e.label.as_str()).unwrap_or("T3 Code");
        let title = format!("Use T3 Code in another app (MCP) \u{00b7} {label}");

        let (header_steps, panel_body, footer) = match env {
            None => (
                None,
                Self::render_gate_message("Connect to an environment first.", vec![], None, cx),
                None,
            ),
            Some(_) if !self.cloud_configured => (
                None,
                Self::render_gate_message(
                    "T3 Connect is not available in this build.",
                    vec![],
                    None,
                    cx,
                ),
                None,
            ),
            Some(ref env) => {
                let gate = self.current_gate(env, cx);
                let hosted = self.hosted_approval();
                let disabled_levels = ungrantable_levels(
                    &env.label,
                    env.grants.as_ref(),
                    env.managed_policy.as_ref(),
                );
                let access = self
                    .picked_access
                    .or_else(|| default_wizard_access(&disabled_levels));
                let app_name = match self.choice {
                    Some(AppChoice::App(app)) => app.name,
                    _ => "the app",
                };

                match &gate {
                    McpGate::Open { mcp_url } => {
                        let steps_el = Some(self.render_wizard_steps(hosted, cx));
                        let body = match self.step {
                            WizardStep::App => self.render_app_step(cx),
                            WizardStep::Access => {
                                self.render_access_step(app_name, access, &disabled_levels, cx)
                            }
                            WizardStep::Connect => match self.choice {
                                Some(choice) if hosted || access.is_some() => self
                                    .render_connect_step(
                                        env,
                                        mcp_url,
                                        choice,
                                        if hosted { None } else { access },
                                        cx,
                                    ),
                                _ => self.render_app_step(cx),
                            },
                        };
                        let leading = (self.step == WizardStep::Connect).then(|| {
                            Button::new("mcp-manage-access", ButtonVariant::Ghost, ButtonSize::Sm)
                                .label("Manage access")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close(window, cx);
                                    crate::settings::open(Section::Access, cx);
                                }))
                        });
                        let can_next = match self.step {
                            WizardStep::App => self.choice.is_some(),
                            WizardStep::Access => access.is_some(),
                            WizardStep::Connect => true,
                        };
                        let footer_el = h_flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.))
                            .px(px(24.))
                            .py(px(16.))
                            .border_t_1()
                            .border_color(hsla(p.border))
                            .rounded_b(px(15.))
                            .bg(hsla(p.muted).opacity(0.72))
                            .child(div().children(leading))
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .when(self.step == WizardStep::Access, |this| {
                                        this.child(
                                            Button::new(
                                                "mcp-back",
                                                ButtonVariant::Outline,
                                                ButtonSize::Sm,
                                            )
                                            .label("Back")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.step = WizardStep::App;
                                                cx.notify();
                                            })),
                                        )
                                    })
                                    .when(self.step == WizardStep::Connect, |this| {
                                        this.child(
                                            Button::new(
                                                "mcp-done",
                                                ButtonVariant::Default,
                                                ButtonSize::Sm,
                                            )
                                            .label("Done")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.close(window, cx);
                                            })),
                                        )
                                    })
                                    .when(self.step != WizardStep::Connect, |this| {
                                        this.child(
                                            Button::new(
                                                "mcp-next",
                                                ButtonVariant::Default,
                                                ButtonSize::Sm,
                                            )
                                            .label("Next")
                                            .disabled(!can_next)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.next_step(cx);
                                            })),
                                        )
                                    }),
                            )
                            .into_any_element();
                        (steps_el, body, Some(footer_el))
                    }
                    other => (None, self.render_gate_screen(env, other, cx), None),
                }
            }
        };

        deferred(
            div()
                .id("mcp-wizard-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close(window, cx)),
                )
                .child(
                    v_flex()
                        .id("mcp-wizard-dialog")
                        .track_focus(&self.focus)
                        .w_full()
                        .max_w(px(576.))
                        .max_h(px(740.))
                        .rounded(px(16.))
                        .relative()
                        .child(crate::glass::backdrop(
                            16.,
                            hsla(p.dialog_surface),
                            hsla(p.dialog_border),
                            cx,
                        ))
                        .shadow(vec![BoxShadow {
                            color: gpui::black().opacity(0.65),
                            offset: point(px(0.), px(24.)),
                            blur_radius: px(64.),
                            spread_radius: px(-24.),
                            inset: false,
                        }])
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                            this.close(window, cx);
                            cx.stop_propagation();
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                this.close(window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(
                            v_flex()
                                .p(px(24.))
                                .pb(px(16.))
                                .gap(px(14.))
                                .child(
                                    h_flex()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(8.))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_xl()
                                                .line_height(px(24.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(hsla(p.popover_foreground))
                                                .child(title),
                                        )
                                        .child(
                                            Button::new(
                                                "mcp-wizard-close",
                                                ButtonVariant::Ghost,
                                                ButtonSize::IconXs,
                                            )
                                            .icon(Icon::X)
                                            .tooltip("Close")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.close(window, cx);
                                            })),
                                        ),
                                )
                                .children(header_steps),
                        )
                        .child(
                            div()
                                .id("mcp-wizard-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .px(px(24.))
                                .py(px(16.))
                                .border_t_1()
                                .border_color(hsla(p.border).opacity(0.5))
                                .child(panel_body),
                        )
                        .children(footer),
                ),
        )
        .with_priority(50)
        .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
impl UseInAnotherAppDialog {
    pub(crate) fn set_test_state(
        &mut self,
        capabilities: Option<McpCapabilities>,
        link_state: Option<CloudLinkState>,
        check_state: ReachabilityCheckState,
        peer_links: Vec<PeerLinkEntry>,
        cx: &mut Context<Self>,
    ) {
        self.capabilities = capabilities;
        self.link_state = link_state;
        self.check_state = check_state;
        self.peer_links = peer_links;
        self.cloud_checked = true;
        cx.notify();
    }

    pub(crate) fn test_gate(&self, cx: &App) -> Option<McpGate> {
        self.resolve_environment(cx)
            .map(|env| self.current_gate(&env, cx))
    }

    pub(crate) fn test_mint_code(&mut self, access: McpClientAccess, cx: &mut Context<Self>) {
        if let Some(env) = self.resolve_environment(cx) {
            self.mint_approval_code(access, env.label, cx);
        }
    }

    pub(crate) fn test_minted_credential(&self) -> Option<&str> {
        self.minted_code.as_ref().map(|c| c.credential.as_str())
    }
}
