//! Sharing & access: `ConnectionsSettings view="access"` (`web/src/components/settings/ConnectionsSettings.tsx`)
//! for the selected saved environment (even when Local is off, `pingdotgg/t3code#26464`): its
//! version, network reachability (`pingdotgg/t3code#35543`), this connection's permissions,
//! `AuthorizedClientsSettings.tsx`'s folded pairing links & authorized clients with newest link
//! shown while folded (`pingdotgg/t3code#30498`), and T3 Connect device approval & trusted device
//! management (`pingdotgg/t3code#36419`) over `subscribeAuthAccess` and `/api/auth` endpoints.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    PromptLevel, StatefulInteractiveElement, Styled, Subscription, Task, Window, canvas, div, fill, point, prelude::FluentBuilder, px, size,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::{Value, json};
use t3_client::supervisor::{ClientHandle, ConnectionStatus, ServerConfigExtras};
use t3_core::auth::AuthSessionState;
use t3_core::commands::method;
use t3_core::connect::{
    ConnectDevice, ConnectDevicesState, PendingConnectDeviceRequest, connect_device_name, describe_connect_device_client,
};
use t3_core::pairing::{
    ANOTHER_ADDRESS, AccessEnvironmentCandidate, LinkableMachine, collect_known_machine_urls,
    is_server_network_accessible, resolve_access_environment_id, session_has_legacy_permissions,
};
use t3_core::server_config::ServerConfig;
use t3_core::settings::access::{
    self, ACCESS_READ, ACCESS_WRITE, AccessState, ClientSession, CloudLinkState, PairingLink,
    READ_ONLY_SCOPES, SCOPE_OPTIONS, expires_label, pairing_token_body, pairing_url,
    scope_count, scope_warning, shown_scopes, standard_scopes,
    toggle_scope,
};
use t3_core::settings::peer_link_create_payload;

use super::controls::{Button, ButtonSize, ButtonVariant, DialogFrame, Row, Select, SelectOption, Switch, dialog_footer, group, muted_text, r, section, section_title};
use super::source_control::{Tone, tone_badge};
use super::worktrees::disclosure;
use super::{Block, SettingsView, store};
use crate::composer::controls::checkbox;
use crate::components::motion::StatusDot;
use crate::icons::Icon;
use crate::i18n::t;
use crate::theme::{ThemeSlot, ThemeSlotExt};
use crate::theme::{hsla, palette};

type SwitchHandler = Box<dyn Fn(bool, &mut Window, &mut App)>;

const QR_SIZE: f32 = 168.;
const RELAY_READ: &str = "relay:read";
const RELAY_WRITE: &str = "relay:write";
const CLOUD_CONNECT_SESSION_SUBJECT: &str = "cloud-connect";

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// Human-readable relative/date label for `ConnectDevice.approved_at` instead of raw ISO text.
fn format_approved_since(approved_at: &str, now_ms: i64) -> String {
    let Some(ms) = t3_core::sidebar::time::parse_iso(approved_at) else {
        return approved_at.split('T').next().unwrap_or(approved_at).to_owned();
    };
    let delta_secs = (now_ms - ms).max(0) / 1000;
    if delta_secs < 60 {
        "just now".to_owned()
    } else if delta_secs < 3_600 {
        format!("{}m ago", delta_secs / 60)
    } else if delta_secs < 86_400 {
        format!("{}h ago", delta_secs / 3_600)
    } else if delta_secs < 30 * 86_400 {
        format!("{}d ago", delta_secs / 86_400)
    } else {
        approved_at.split('T').next().unwrap_or(approved_at).to_owned()
    }
}

/// `ConnectionsSettings.tsx:1304-1311` + `SavedEnvironmentRows.tsx:746`: the approval toggle
/// renders only when T3 Connect's managed tunnel is active and the server advertises the setting.
fn connect_approval_row_visible(managed_tunnel_active: bool, require_approval: Option<bool>) -> bool {
    managed_tunnel_active && require_approval.is_some()
}

/// `ConnectionsSettings.tsx:1379-1389`: the T3 Connect devices section renders only when the
/// session can read access and there is at least one pending request or trusted device.
fn connect_devices_section_visible(can_read: bool, request_count: usize, device_count: usize) -> bool {
    can_read && (request_count > 0 || device_count > 0)
}

/// `ConnectionsSettings.tsx:758-769`: "Revoke others" triggers the Connect approval-off
/// confirmation when any trusted Connect device or non-current `cloud-connect` session exists.
fn revokes_connect_devices(devices: &[ConnectDevice], sessions: &[ClientSession]) -> bool {
    !devices.is_empty()
        || sessions
            .iter()
            .any(|session| !session.current && session.subject == CLOUD_CONNECT_SESSION_SUBJECT)
}

/// Keeps a `subscribeAuthAccess` stream alive for the chosen target environment.
struct TargetStream {
    _pump: Task<()>,
    abort: tokio::task::AbortHandle,
}

impl Drop for TargetStream {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

/// The inline peer-link panel state (`LinkedEnvironmentsSettings.tsx` + `LinkMachineSelect.tsx`).
struct LinkMachineForm {
    choice: Option<String>,
    address: Entity<InputState>,
    pairing_code: Entity<InputState>,
    access: String,
    mint_failed: HashSet<String>,
    submitting: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// The create dialog's state, open in the page.
struct CreateLink {
    focus: FocusHandle,
    scopes: Vec<String>,
    creating: bool,
    error: Option<String>,
}

/// Which revoke action is awaiting confirmation while T3 Connect device approval is off
/// (`ConnectionsSettings.tsx:737-769`, `RevokeConnectDeviceDialog.tsx`, `pingdotgg/t3code#36419`).
#[derive(Debug, Clone)]
enum PendingConnectRevokeAction {
    ConnectDevice(String),
    ClientSession(String),
    OtherClients,
}

/// Inline confirmation when revoking a trusted T3 Connect device, a `cloud-connect` client
/// session, or "Revoke others" while approval is turned off (`RevokeConnectDeviceDialog.tsx`).
#[derive(Debug, Clone)]
struct RevokeConnectDeviceConfirm {
    action: PendingConnectRevokeAction,
    title: String,
    turn_on_approval: bool,
    busy: bool,
}

/// Resolved target environment for Sharing & access (`pingdotgg/t3code#26464`).
#[derive(Clone)]
struct ResolvedAccessTarget {
    saved_id: String,
    label: String,
    address: String,
    is_primary: bool,
    config: Option<ServerConfig>,
    grants: Option<AuthSessionState>,
    client: Option<ClientHandle>,
    extras: ServerConfigExtras,
}

pub(crate) struct AccessPageState {
    pub(crate) label: Entity<InputState>,
    /// Explicitly chosen saved environment ID from the `"Showing access for"` picker.
    selected_env: Option<String>,
    /// Saved environment ID that the per-target state below currently belongs to.
    active_target: Option<String>,
    /// Supervised `ClientHandle` resolved for a non-active saved environment.
    target_client: Option<(String, ClientHandle)>,
    target_client_task: Option<(String, Task<()>)>,
    access: AccessState,
    connect_devices: ConnectDevicesState,
    error: Option<String>,
    /// Keyed by `(saved_id, connection_generation)`.
    stream: Option<(String, u64, TargetStream)>,
    /// Watches `server_config_extras` and `session_state` on the target client so non-active
    /// targets (`is_active == false`) repaint immediately on remote updates.
    extras_watcher: Option<(String, TargetStream)>,
    /// Whether `GET /api/connect/link-state` reports an active managed T3 Connect tunnel.
    managed_tunnel_active: bool,
    /// Full decoded `GET /api/connect/link-state` (`pingdotgg/t3code#37823`, `pingdotgg/t3code#38892`).
    cloud_link_state: Option<CloudLinkState>,
    cloud_details: t3_core::mcp::gate::CloudLinkState,
    relay_saving: bool,
    endpoints_open: bool,
    endpoint_preference: Option<String>,
    qr_endpoint: Option<String>,
    dialog_target: Option<ResolvedAccessTarget>,
    revoke_focus: Option<FocusHandle>,
    /// Keyed by `(saved_id, connection_generation)`.
    link_state_task: Option<(String, u64, Task<()>)>,
    create: Option<CreateLink>,
    link_machine: Option<LinkMachineForm>,
    /// Peer environment IDs already linked on the active access target (`peerLinks.list`).
    linked_peer_ids: HashSet<String>,
    linked_peers_task: Option<Task<()>>,
    /// Credentials of links this client created, known only in memory (`credentialsById`).
    credentials: HashMap<String, String>,
    sharing: Option<String>,
    /// Newest link created on this target: stays visible while the client list is folded (`pingdotgg/t3code#30498`).
    newest_pairing_link_id: Option<String>,
    /// Whether the Authorized clients section is expanded (`FoldedSettingsSection` defaults folded).
    clients_open: bool,
    revoking: Option<String>,
    revoking_others: bool,
    connect_device_busy: Option<String>,
    revoke_device_confirm: Option<RevokeConnectDeviceConfirm>,
    approval_saving: bool,
    permissions_open: bool,
    /// Cached dismissal state per environment key so `render()` reads disk at most once per key.
    dismissed_legacy_notice: HashMap<String, bool>,
    /// The last QR encoded, by URL, so countdown ticks and hovers do not re-encode it.
    qr: RefCell<Option<(String, Rc<QrMatrix>)>>,
    /// Ticks once a second while a pairing link counts down.
    ticker: Option<Task<()>>,
}

impl AccessPageState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        AccessPageState {
            label: cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Living room iPad")),
            selected_env: None,
            active_target: None,
            target_client: None,
            target_client_task: None,
            access: AccessState::default(),
            connect_devices: ConnectDevicesState::default(),
            error: None,
            stream: None,
            extras_watcher: None,
            managed_tunnel_active: false,
            cloud_link_state: None,
            cloud_details: Default::default(),
            relay_saving: false,
            endpoints_open: false,
            endpoint_preference: None,
            qr_endpoint: None,
            dialog_target: None,
            revoke_focus: None,
            link_state_task: None,
            create: None,
            link_machine: None,
            linked_peer_ids: HashSet::new(),
            linked_peers_task: None,
            credentials: HashMap::new(),
            sharing: None,
            newest_pairing_link_id: None,
            clients_open: false,
            revoking: None,
            revoking_others: false,
            connect_device_busy: None,
            revoke_device_confirm: None,
            approval_saving: false,
            permissions_open: false,
            dismissed_legacy_notice: HashMap::new(),
            qr: RefCell::new(None),
            ticker: None,
        }
    }

    /// Resets per-target state when switching to a different environment (`pingdotgg/t3code#26464`).
    fn reset_target(&mut self, next_target: Option<String>) {
        self.endpoint_preference = next_target.as_ref().and_then(|id| {
            t3_client::store::load_ui_state(&format!("preferred-endpoint:{id}")).and_then(|value| value.as_str().map(str::to_owned))
        });
        self.active_target = next_target;
        self.target_client = None;
        self.target_client_task = None;
        self.access = AccessState::default();
        self.connect_devices = ConnectDevicesState::default();
        self.error = None;
        self.stream = None;
        self.extras_watcher = None;
        self.managed_tunnel_active = false;
        self.cloud_link_state = None;
        self.cloud_details = Default::default();
        self.relay_saving = false;
        self.qr_endpoint = None;
        self.dialog_target = None;
        self.revoke_focus = None;
        self.endpoints_open = false;
        self.link_state_task = None;
        self.create = None;
        self.link_machine = None;
        self.linked_peer_ids.clear();
        self.linked_peers_task = None;
        self.credentials.clear();
        self.sharing = None;
        self.newest_pairing_link_id = None;
        self.clients_open = false;
        self.revoking = None;
        self.revoking_others = false;
        self.connect_device_busy = None;
        self.revoke_device_confirm = None;
        self.approval_saving = false;
        self.qr.borrow_mut().take();
    }

    /// Leaving the page stops the stream and the countdown.
    pub(super) fn close(&mut self) {
        self.stream = None;
        self.extras_watcher = None;
        self.cloud_details = Default::default();
        self.relay_saving = false;
        self.qr_endpoint = None;
        self.dialog_target = None;
        self.create = None;
        self.revoke_device_confirm = None;
        self.revoke_focus = None;
        self.link_state_task = None;
        self.ticker = None;
        self.target_client_task = None;
        self.linked_peers_task = None;
    }
}

/// A QR code's modules, row-major, `true` for dark.
pub(crate) struct QrMatrix {
    width: usize,
    dark: Vec<bool>,
}

/// The QR of `text` at ECC level M.
fn qr_matrix(text: &str) -> Option<QrMatrix> {
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    Some(QrMatrix { width: code.width(), dark: code.to_colors().into_iter().map(|color| color == qrcode::Color::Dark).collect() })
}

/// The matrix with a one-module margin on white, drawn as one quad per run of dark modules.
fn qr_element(matrix: Rc<QrMatrix>) -> AnyElement {
    let element = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let width = matrix.width;
            let unit = f32::from(bounds.size.width) / (width + 2) as f32;
            window.paint_quad(fill(bounds, gpui::white()));
            for y in 0..width {
                let row = &matrix.dark[y * width..(y + 1) * width];
                let mut x = 0;
                while x < width {
                    if !row[x] {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    while x < width && row[x] {
                        x += 1;
                    }
                    let origin = point(bounds.origin.x + px(unit * (start + 1) as f32), bounds.origin.y + px(unit * (y + 1) as f32));
                    window.paint_quad(fill(Bounds::new(origin, size(px(unit * (x - start) as f32), px(unit))), gpui::black()));
                }
            }
        },
    )
    .size(r(QR_SIZE));
    div().flex_none().rounded(r(6.)).overflow_hidden().child(element).into_any_element()
}

/// Filters regular pairing links, excluding pending T3 Connect device approval requests
/// (which render in the dedicated `T3 Connect devices` section).
fn regular_visible_links<'a>(access: &'a AccessState, connect: &ConnectDevicesState, now: i64) -> Vec<&'a PairingLink> {
    let pending_ids: HashSet<&str> = connect.pending_requests.iter().map(|req| req.pairing_link_id.as_str()).collect();
    access
        .visible_links(now)
        .into_iter()
        .filter(|link| !pending_ids.contains(link.id.as_str()))
        .collect()
}

/// Picks the pairing URL base for a link: prefers an advertised reachable HTTP(S) `directEndpoint`
/// (e.g., Tailscale Serve, `pingdotgg/t3code#35543`) when the connected address is loopback or
/// non-HTTP(S) such as desktop `t3code://` (`pingdotgg/t3code#38442`).
fn effective_pairing_address<'a>(address: &'a str, direct_endpoints: &'a [String]) -> &'a str {
    if pairing_url(address, "probe").is_none()
        && let Some(endpoint) = direct_endpoints.iter().find(|url| pairing_url(url, "probe").is_some())
    {
        return endpoint.as_str();
    }
    address
}

fn pairing_endpoints(target: &ResolvedAccessTarget) -> Vec<String> {
    collect_known_machine_urls(target.extras.direct_endpoints.iter().map(String::as_str).chain(std::iter::once(target.address.as_str())))
}

fn use_in_another_app_action(target: &ResolvedAccessTarget, cx: &App) -> Button {
    let mcp_id = target.config.as_ref().map(|config| config.environment.environment_id.clone())
        .filter(|id| !id.is_empty()).unwrap_or_else(|| target.saved_id.clone());
    Button::new("access-use-in-another-app", ButtonVariant::Outline, ButtonSize::Compact)
        .icon(Icon::Blocks)
        .label(t(cx, "Use in another app (MCP)"))
        .on_click(move |_, window, cx| crate::mcp::open_use_in_another_app(Some(mcp_id.clone()), window.window_handle(), cx))
}

fn access_scope_summary(id: SharedString, scopes: &[String], cx: &App) -> AnyElement {
    let labels = scopes.iter().map(|scope| {
        SCOPE_OPTIONS.iter().find(|(key, _, _)| key == scope)
            .map_or(scope.as_str(), |(_, label, _)| t(cx, label))
    }).collect::<Vec<_>>();
    div().id(id)
        .text_xs().text_color(hsla(palette(cx).muted_foreground))
        .when(!labels.is_empty(), |el| el.tooltip(super::controls::tooltip(labels.join("
").into())))
        .child(scope_count(scopes)).into_any_element()
}

pub(super) fn session_permissions(session: Option<&AuthSessionState>, cx: &App) -> AnyElement {
    let p = palette(cx);
    let Some(session) = session else {
        return muted_text(t(cx, "Checking permissions…"), cx).into_any_element();
    };
    v_flex().gap(r(8.))
        .child(muted_text(t(cx, "These permissions apply to this client’s current connection."), cx))
        .child(v_flex().gap(r(4.)).children(SCOPE_OPTIONS.chunks(2).map(|pair| {
            h_flex().items_start().gap(r(14.)).children(pair.iter().map(|(scope, title, _)| {
                let allowed = session.grants(scope);
                h_flex().flex_1().min_w_0().justify_between().gap(r(4.)).text_xs()
                    .child(div().text_color(hsla(p.foreground)).child(t(cx, title)))
                    .child(div().flex_none().text_color(hsla(if allowed { p.success_foreground } else { p.muted_foreground }))
                        .child(t(cx, if allowed { "Allowed" } else { "Not granted" })))
            }))
        })))
        .into_any_element()
}

impl SettingsView {
    /// Eligible saved environments for Sharing & access (`pingdotgg/t3code#26464`):
    /// includes primary (if enabled) and all enabled non-local saved environments so saved services
    /// remain manageable even when Local is off.
    fn access_candidates(&self, cx: &App) -> Vec<(AccessEnvironmentCandidate, String)> {
        let app = self.state.read(cx);
        let primary = app.environments.active.as_deref();
        let mut out = Vec::new();
        for env in &app.environments.environments {
            let is_primary = primary == Some(env.id.as_str());
            if env.disabled && !is_primary {
                continue;
            }
            if env.disabled && env.local {
                continue;
            }
            let connected = app
                .fleet
                .environment(&env.id)
                .is_some_and(|view| view.status == ConnectionStatus::Connected)
                || app.environment_statuses.get(&env.id) == Some(&ConnectionStatus::Connected);
            out.push((
                AccessEnvironmentCandidate { id: env.id.clone(), connected },
                if env.label.trim().is_empty() { env.address() } else { env.label.clone() },
            ));
        }
        out
    }

    /// Resolves the target environment and fetches its supervised `ClientHandle` if needed.
    fn resolve_access_target(&mut self, cx: &mut Context<Self>) -> (Vec<(AccessEnvironmentCandidate, String)>, Option<ResolvedAccessTarget>) {
        let candidates_with_labels = self.access_candidates(cx);
        let candidates: Vec<AccessEnvironmentCandidate> = candidates_with_labels.iter().map(|(c, _)| c.clone()).collect();
        let primary_id = self.state.read(cx).environments.active.clone();
        let target_id = resolve_access_environment_id(
            primary_id.as_deref(),
            &candidates,
            self.access.selected_env.as_deref(),
        )
        .map(str::to_owned);

        if self.access.active_target != target_id {
            self.access.reset_target(target_id.clone());
        }

        let Some(target_id) = target_id else {
            return (candidates_with_labels, None);
        };

        let app = self.state.read(cx);
        let is_active = app.environments.active.as_deref() == Some(target_id.as_str());
        let saved = app.environments.get(&target_id);
        let fleet_view = app.fleet.environment(&target_id);

        let config = fleet_view
            .and_then(|view| view.config.config.clone())
            .or_else(|| is_active.then(|| app.server_config.config.clone()).flatten());
        let address = saved
            .map(|env| env.http_base.clone())
            .or_else(|| is_active.then(|| app.environment().map(str::to_owned)).flatten())
            .unwrap_or_default();
        let label = config
            .as_ref()
            .map(|cfg| cfg.environment.label.clone())
            .filter(|label| !label.trim().is_empty())
            .or_else(|| saved.map(|env| env.label.clone()).filter(|label| !label.trim().is_empty()))
            .unwrap_or_else(|| if is_active { "Primary environment".to_owned() } else { "Saved environment".to_owned() });

        let active_client = is_active.then(|| app.client_handle()).flatten();
        let client = active_client.or_else(|| {
            self.access
                .target_client
                .as_ref()
                .filter(|(id, _)| id == &target_id)
                .map(|(_, client)| client.clone())
        });

        let grants = fleet_view
            .and_then(|view| view.grants.clone())
            .or_else(|| client.as_ref().and_then(|c| c.session_state().borrow().clone()));
        let extras = client
            .as_ref()
            .map(|c| c.server_config_extras().borrow().clone())
            .unwrap_or_else(|| {
                if address.is_empty() {
                    ServerConfigExtras::default()
                } else {
                    t3_client::supervisor::server_config_extras_for(&address).borrow().clone()
                }
            });

        // If this is a non-active saved environment and we haven't fetched its ClientHandle yet, do so now.
        if client.is_none()
            && !is_active
            && !self.access.target_client_task.as_ref().is_some_and(|(id, _)| id == &target_id)
        {
            let lookup = app.connections.client(&target_id);
            let runtime = app.runtime().clone();
            let wanted = target_id.clone();
            let task = cx.spawn(async move |this, cx| {
                let handle = runtime.spawn(lookup).await.ok().flatten();
                let _ = this.update(cx, |this, cx| {
                    if this.access.active_target.as_deref() == Some(wanted.as_str()) {
                        this.access.target_client = handle.map(|client| (wanted, client));
                        this.access.target_client_task = None;
                        cx.notify();
                    }
                });
            });
            self.access.target_client_task = Some((target_id.clone(), task));
        }

        let is_primary = is_active && saved.is_some_and(|env| env.local);
        (
            candidates_with_labels,
            Some(ResolvedAccessTarget {
                saved_id: target_id,
                label,
                address,
                is_primary,
                config,
                grants,
                client,
                extras,
            }),
        )
    }

    fn pairing_denial_for(&self, config: Option<&ServerConfig>) -> Option<String> {
        t3_core::managed_policy::capability_denial(
            config.and_then(|c| c.managed_policy.as_ref()),
            t3_core::managed_policy::Capability::RemoteClients,
        )
    }

    pub(super) fn session(&self, cx: &App) -> Option<AuthSessionState> {
        let handle = self.state.read(cx).client_handle()?;
        handle.session_state().borrow().clone()
    }

    /// Whether this connection may `scope`; unknown grants leave the server to decide.
    pub(super) fn may(&self, scope: &str, cx: &App) -> bool {
        self.session(cx).is_none_or(|session| session.grants(scope))
    }

    fn target_may(grants: Option<&AuthSessionState>, scope: &str) -> bool {
        grants.is_none_or(|session| session.grants(scope))
    }

    fn ensure_target_access_stream(&mut self, target: &ResolvedAccessTarget, cx: &mut Context<Self>) {
        let Some(client) = target.client.clone() else {
            self.access.stream = None;
            return;
        };
        if !Self::target_may(target.grants.as_ref(), ACCESS_READ) {
            return;
        }
        let generation = *client.connection_generation().borrow();
        if self
            .access
            .stream
            .as_ref()
            .is_some_and(|(id, gen_id, _)| id == &target.saved_id && *gen_id == generation)
        {
            return;
        }
        self.access.access = AccessState::default();
        self.access.connect_devices = ConnectDevicesState::default();
        self.access.error = None;
        let runtime = self.state.read(cx).runtime().clone();
        let target_id = target.saved_id.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<Value, String>>();
        let network = runtime.spawn(async move {
            let mut stream = match client.stream(access::SUBSCRIBE, json!({})).await {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = tx.send(Err(error.to_string()));
                    return;
                }
            };
            while let Some(item) = stream.receiver.recv().await {
                match item {
                    Ok(value) => {
                        if tx.send(Ok(value)).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error.to_string()));
                        return;
                    }
                }
            }
        });
        let abort = network.abort_handle();
        let expected_id = target_id.clone();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(item) = rx.recv().await {
                let alive = this.update(cx, |this, cx| {
                    if this.access.active_target.as_deref() != Some(expected_id.as_str()) {
                        return;
                    }
                    match item {
                        Ok(value) => {
                            this.access.access.apply(&value);
                            this.access.connect_devices.apply(&value);
                        }
                        Err(message) => this.access.error = Some(message),
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        self.access.stream = Some((target_id, generation, TargetStream { _pump: pump, abort }));
    }

    /// Watches `server_config_extras`, `session_state`, and `connection_generation` on the target
    /// `ClientHandle`, and reads `GET /api/connect/link-state` (`managedTunnelActive`) once per
    /// `(saved_id, generation)` so non-active targets (`is_active == false`) stay reactive.
    fn ensure_target_watchers(&mut self, target: &ResolvedAccessTarget, cx: &mut Context<Self>) {
        let Some(client) = target.client.clone() else {
            self.access.extras_watcher = None;
            self.access.link_state_task = None;
            return;
        };
        let target_id = target.saved_id.clone();
        if !self.access.extras_watcher.as_ref().is_some_and(|(id, _)| id == &target_id) {
            let runtime = self.state.read(cx).runtime().clone();
            let mut extras_rx = client.server_config_extras();
            let mut grants_rx = client.session_state();
            let mut gen_rx = client.connection_generation();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
            let network = runtime.spawn(async move {
                loop {
                    let changed = tokio::select! {
                        res = extras_rx.changed() => res.is_ok(),
                        res = grants_rx.changed() => res.is_ok(),
                        res = gen_rx.changed() => res.is_ok(),
                    };
                    if !changed || tx.send(()).is_err() {
                        return;
                    }
                }
            });
            let abort = network.abort_handle();
            let expected = target_id.clone();
            let pump = cx.spawn(async move |this, cx| {
                while rx.recv().await.is_some() {
                    let alive = this.update(cx, |this, cx| {
                        if this.access.active_target.as_deref() == Some(expected.as_str()) {
                            cx.notify();
                        }
                    });
                    if alive.is_err() {
                        return;
                    }
                }
            });
            self.access.extras_watcher = Some((target_id.clone(), TargetStream { _pump: pump, abort }));
        }

        let generation = *client.connection_generation().borrow();
        let can_read_relay = Self::target_may(target.grants.as_ref(), RELAY_READ);
        if !can_read_relay {
            self.access.managed_tunnel_active = false;
            self.access.cloud_link_state = None;
            self.access.link_state_task = None;
            return;
        }
        if self
            .access
            .link_state_task
            .as_ref()
            .is_some_and(|(id, gen_id, _)| id == &target_id && *gen_id == generation)
        {
            return;
        }
        let runtime = self.state.read(cx).runtime().clone();
        let expected = target_id.clone();
        let task = cx.spawn(async move |this, cx| {
            loop {
                let client = client.clone();
                let fetch = runtime.spawn(async move { client.get_access("/api/connect/link-state").await });
                let body = fetch.await.ok().and_then(Result::ok);
                let link_state = body.as_ref().and_then(CloudLinkState::from_wire);
                let details = body.and_then(|body| serde_json::from_value(body).ok()).unwrap_or_default();
                if !this.update(cx, |this, cx| {
                    if this.access.active_target.as_deref() != Some(expected.as_str())
                        || !this.access.link_state_task.as_ref().is_some_and(|(_, current, _)| *current == generation)
                    {
                        return false;
                    }
                    this.access.managed_tunnel_active = link_state.as_ref().is_some_and(|state| state.managed_tunnel_active);
                    this.access.cloud_link_state = link_state;
                    this.access.cloud_details = details;
                    cx.notify();
                    true
                }).unwrap_or(false) {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(30)).await;
            }
        });
        self.access.link_state_task = Some((target_id, generation, task));
    }

    fn ensure_ticker(&mut self, counting: bool, cx: &mut Context<Self>) {
        if !counting {
            self.access.ticker = None;
            return;
        }
        if self.access.ticker.is_some() {
            return;
        }
        self.access.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
    }

    /// One access endpoint `POST` against the currently selected target environment's supervised client.
    fn access_post_on(
        &mut self,
        client: Option<ClientHandle>,
        target_id: String,
        path: &'static str,
        body: Option<Value>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut SettingsView, Result<Value, String>, &mut Context<SettingsView>) + 'static,
    ) {
        let Some(client) = client else {
            return done(self, Err("Reconnect to this environment first.".to_owned()), cx);
        };
        self.access_request_on(target_id, async move { client.post_access(path, body).await }, cx, done);
    }

    fn access_request_on<T: Send + 'static>(
        &mut self,
        target_id: String,
        request: impl Future<Output = Result<T, String>> + Send + 'static,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut SettingsView, Result<T, String>, &mut Context<SettingsView>) + 'static,
    ) {
        let task = self.state.read(cx).runtime().spawn(request);
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.access.active_target.as_deref() == Some(target_id.as_str()) {
                    done(this, result, cx);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn create_pairing_link(&mut self, client: Option<ClientHandle>, target_id: String, denied: bool, cx: &mut Context<Self>) {
        if denied {
            return;
        }
        let Some(create) = self.access.create.as_mut().filter(|create| !create.creating && !create.scopes.is_empty()) else {
            return;
        };
        create.creating = true;
        create.error = None;
        let body = pairing_token_body(&self.access.label.read(cx).value(), &create.scopes);
        self.access_post_on(client, target_id, "/api/auth/pairing-token", Some(body), cx, |this, result, _| match result {
            Ok(reply) => {
                if let (Some(id), Some(credential)) = (reply.get("id").and_then(Value::as_str), reply.get("credential").and_then(Value::as_str)) {
                    this.access.credentials.insert(id.to_owned(), credential.to_owned());
                    this.access.sharing = Some(id.to_owned());
                    // `pingdotgg/t3code#30498`: keep newest link visible without expanding the client list.
                    this.access.newest_pairing_link_id = Some(id.to_owned());
                }
                this.access.create = None;
            }
            Err(error) => {
                if let Some(create) = this.access.create.as_mut() {
                    create.creating = false;
                    create.error = Some(format!("Could not create pairing URL: {error}"));
                }
            }
        });
    }

    fn revoke_link(&mut self, client: Option<ClientHandle>, target_id: String, id: String, cx: &mut Context<Self>) {
        self.access.revoking = Some(id.clone());
        self.access_post_on(client, target_id, "/api/auth/pairing-links/revoke", Some(json!({ "id": id })), cx, move |this, result, cx| {
            this.access.revoking = None;
            this.access.credentials.remove(&id);
            if this.access.newest_pairing_link_id.as_deref() == Some(id.as_str()) {
                this.access.newest_pairing_link_id = None;
            }
            if let Err(error) = result {
                store::show_error(format!("Could not revoke pairing link: {error}"), cx);
            }
        });
    }

    fn revoke_client(&mut self, client: Option<ClientHandle>, target_id: String, session_id: String, cx: &mut Context<Self>) {
        self.access.revoking = Some(session_id.clone());
        self.access_post_on(client, target_id, "/api/auth/clients/revoke", Some(json!({ "sessionId": session_id })), cx, |this, result, cx| {
            this.access.revoking = None;
            if let Err(error) = result {
                store::show_error(format!("Could not revoke client access: {error}"), cx);
            }
        });
    }

    fn revoke_others(&mut self, client: Option<ClientHandle>, target_id: String, cx: &mut Context<Self>) {
        self.access.revoking_others = true;
        self.access_post_on(client, target_id, "/api/auth/clients/revoke-others", None, cx, |this, result, cx| {
            this.access.revoking_others = false;
            if let Err(error) = result {
                store::show_error(format!("Could not revoke other clients: {error}"), cx);
            }
        });
    }

    fn toggle_connect_device_approval(&mut self, client: Option<ClientHandle>, target_id: String, next: bool, cx: &mut Context<Self>) {
        let Some(client) = client else {
            return store::show_error("Reconnect to this environment first.".to_owned(), cx);
        };
        self.access.approval_saving = true;
        let runtime = self.state.read(cx).runtime().clone();
        let update = client.request(method::SERVER_UPDATE_SETTINGS, json!({ "patch": { "requireConnectDeviceApproval": next } }));
        let task = runtime.spawn(update);
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.access.active_target.as_deref() != Some(target_id.as_str()) {
                    return;
                }
                this.access.approval_saving = false;
                match result {
                    Ok(_) => client.set_require_connect_device_approval(next),
                    Err(error) => store::show_error(format!("Could not update device approval setting: {error}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn approve_connect_device(&mut self, client: Option<ClientHandle>, target_id: String, pairing_link_id: String, cx: &mut Context<Self>) {
        self.access.connect_device_busy = Some(pairing_link_id.clone());
        self.access_post_on(
            client,
            target_id,
            "/api/auth/connect-devices/approve",
            Some(json!({ "pairingLinkId": pairing_link_id })),
            cx,
            |this, result, cx| {
                this.access.connect_device_busy = None;
                if let Err(error) = result {
                    store::show_error(format!("Could not approve the device: {error}"), cx);
                }
            },
        );
    }

    fn deny_connect_device(&mut self, client: Option<ClientHandle>, target_id: String, pairing_link_id: String, cx: &mut Context<Self>) {
        self.access.connect_device_busy = Some(pairing_link_id.clone());
        self.access_post_on(
            client,
            target_id,
            "/api/auth/pairing-links/revoke",
            Some(json!({ "id": pairing_link_id })),
            cx,
            |this, result, cx| {
                this.access.connect_device_busy = None;
                if let Err(error) = result {
                    store::show_error(format!("Could not deny the device: {error}"), cx);
                }
            },
        );
    }

    fn revoke_connect_device_now(&mut self, client: Option<ClientHandle>, target_id: String, thumbprint: String, cx: &mut Context<Self>) {
        self.access.connect_device_busy = Some(thumbprint.clone());
        self.access_post_on(
            client,
            target_id,
            "/api/auth/connect-devices/revoke",
            Some(json!({ "proofKeyThumbprint": thumbprint })),
            cx,
            |this, result, cx| {
                this.access.connect_device_busy = None;
                this.access.revoke_device_confirm = None;
                if let Err(error) = result {
                    store::show_error(format!("Could not revoke the device: {error}"), cx);
                }
            },
        );
    }

    fn confirm_revoke_connect_device(&mut self, client: Option<ClientHandle>, target_id: String, cx: &mut Context<Self>) {
        let Some(confirm) = self.access.revoke_device_confirm.as_mut() else {
            return;
        };
        if confirm.busy {
            return;
        }
        confirm.busy = true;
        let action = confirm.action.clone();
        let turn_on = confirm.turn_on_approval;
        let Some(client) = client else {
            confirm.busy = false;
            return store::show_error("Reconnect to this environment first.".to_owned(), cx);
        };
        if !turn_on {
            self.access.revoke_device_confirm = None;
            return match action {
                PendingConnectRevokeAction::ConnectDevice(thumbprint) => {
                    self.revoke_connect_device_now(Some(client), target_id, thumbprint, cx)
                }
                PendingConnectRevokeAction::ClientSession(session_id) => {
                    self.revoke_client(Some(client), target_id, session_id, cx)
                }
                PendingConnectRevokeAction::OtherClients => {
                    self.revoke_others(Some(client), target_id, cx)
                }
            };
        }
        let runtime = self.state.read(cx).runtime().clone();
        let update_client = client.clone();
        let task = runtime.spawn(async move {
            update_client
                .request(method::SERVER_UPDATE_SETTINGS, json!({ "patch": { "requireConnectDeviceApproval": true } }))
                .await
                .map_err(|e| e.to_string())?;
            update_client.set_require_connect_device_approval(true);
            match action {
                PendingConnectRevokeAction::ConnectDevice(thumbprint) => {
                    update_client
                        .post_access("/api/auth/connect-devices/revoke", Some(json!({ "proofKeyThumbprint": thumbprint })))
                        .await
                }
                PendingConnectRevokeAction::ClientSession(session_id) => {
                    update_client
                        .post_access("/api/auth/clients/revoke", Some(json!({ "sessionId": session_id })))
                        .await
                }
                PendingConnectRevokeAction::OtherClients => {
                    update_client.post_access("/api/auth/clients/revoke-others", None).await
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.access.active_target.as_deref() != Some(target_id.as_str()) {
                    return;
                }
                if let Err(error) = result {
                    if let Some(confirm) = this.access.revoke_device_confirm.as_mut() {
                        confirm.busy = false;
                    }
                    store::show_error(format!("Could not revoke access: {error}"), cx);
                } else {
                    this.access.revoke_device_confirm = None;
                    this.access.revoke_focus = None;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn access_network_row(&self, target: &ResolvedAccessTarget, cx: &Context<Self>) -> AnyElement {
        let network_accessible = is_server_network_accessible(
            target.extras.auth_policy.as_deref(),
            target.extras.direct_endpoints.len(),
        );
        let network_reason = t(cx, "Change network exposure where this server starts.");
        let tailscale_reason = t(cx, "Configure Tailscale HTTPS on the server host.");
        let tailscale = target.extras.direct_endpoint_entries.iter().any(|endpoint| {
            endpoint.kind.as_deref() == Some("tailnet") && endpoint.http_base_url.starts_with("https://")
        });
        let p = palette(cx);
        let candidates = pairing_endpoints(target);
        let default = t3_core::pairing::origin_of(effective_pairing_address(&target.address, &target.extras.direct_endpoints));
        let current = self.access.endpoint_preference.as_ref()
            .filter(|origin| candidates.contains(origin))
            .or_else(|| default.as_ref().filter(|origin| candidates.contains(origin)))
            .or_else(|| candidates.first());
        let id = target.saved_id.clone();
        let toggle = Button::new("access-show-endpoints", ButtonVariant::GhostMuted, ButtonSize::Xs)
            .label(t(cx, if self.access.endpoints_open { "Hide addresses" } else { "Show addresses" }))
            .icon(if self.access.endpoints_open { Icon::ChevronDown } else { Icon::ChevronRight })
            .on_click(cx.listener(|this, _, _, cx| {
                this.access.endpoints_open = !this.access.endpoints_open;
                cx.notify();
            }));
        let details = v_flex().gap(r(6.))
            .when(!candidates.is_empty(), |el| el.child(div().child(toggle)))
            .when(self.access.endpoints_open, |el| el.children(candidates.iter().map(|origin| {
                let is_default = current == Some(origin);
                let selected = origin.clone();
                let target_id = id.clone();
                h_flex().w_full().gap(r(8.))
                    .child(div().flex_1().min_w_0().truncate().text_xs().child(origin.clone()))
                    .child(Button::new(SharedString::from(format!("prefer-{origin}")), ButtonVariant::Outline, ButtonSize::Xs)
                        .label(t(cx, if is_default { "Default" } else { "Set as default" })).disabled(is_default)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.access.active_target.as_deref() == Some(&target_id) {
                                this.access.endpoint_preference = Some(selected.clone());
                                t3_client::store::save_ui_state(&format!("preferred-endpoint:{target_id}"), &json!(selected));
                                cx.notify();
                            }
                        })))
            })))
            .children(self.pairing_denial_for(target.config.as_ref()).filter(|_| !network_accessible)
                .map(|reason| crate::components::managed::notice(reason, cx)));
        let network = Row::new("access-network", t(cx, "Network access"))
            .description(t(cx, if network_accessible { "Remote access is configured." } else { "No remote address is advertised." }))
            .control(div().id("access-network-control-hint").tooltip(super::controls::tooltip(network_reason.into()))
                .child(Switch::new("access-network-switch", network_accessible).disabled(true)))
            .below(details);
        let tailscale = Row::new("access-tailscale-serve", t(cx, "Tailscale HTTPS"))
            .description(tailscale_reason)
            .control(div().id("access-tailscale-control-hint").tooltip(super::controls::tooltip(tailscale_reason.into()))
                .child(Switch::new("access-tailscale-switch", tailscale).disabled(true)));
        v_flex().child(network).child(div().border_t_1().border_color(hsla(p.border).opacity(0.5)).child(tailscale)).into_any_element()
    }

    fn access_relay_rows(&self, target: &ResolvedAccessTarget, link: &CloudLinkState, cx: &Context<Self>) -> Vec<AnyElement> {
        let policy = target.config.as_ref().and_then(|config| config.managed_policy.as_ref());
        let denial = t3_core::managed_policy::capability_denial(policy, t3_core::managed_policy::Capability::Connect);
        let state = self.state.read(cx);
        let signed_in = state.cloud.has_clerk_session;
        let can_manage = target.client.is_some() && target.grants.as_ref().is_some_and(|grants| {
            grants.grants(RELAY_READ) && grants.grants(RELAY_WRITE)
        });
        let can_configure = can_manage && target.grants.as_ref().is_some_and(access::can_configure_relay);
        let turn_on = access::resolve_t3_connect_turn_on(
            link.linkable_here.unwrap_or(true), state.cloud.service.configured(), can_manage,
            can_manage, can_configure, denial.as_deref(),
        );
        let on_host = matches!(turn_on, access::T3ConnectTurnOn::OnHost);
        let disabled = |needs_link: bool| -> Option<String> {
            if self.access.relay_saving {
                Some(t(cx, "A T3 Connect change is in progress.").to_owned())
            } else if !can_manage {
                Some(t(cx, "This connection cannot manage T3 Connect.").to_owned())
            } else if needs_link {
                match &turn_on {
                    access::T3ConnectTurnOn::Policy(reason) => Some(reason.clone()),
                    access::T3ConnectTurnOn::OnHost => Some(t(cx, "Run `t3 connect link` on the host, then restart T3 Code there.").to_owned()),
                    access::T3ConnectTurnOn::Here if !signed_in => Some(t(cx, "Sign in to T3 Connect before enabling this.").to_owned()),
                    access::T3ConnectTurnOn::Here => None,
                }
            } else {
                None
            }
        };
        let make_switch = |id, checked, reason: Option<String>, handler: SwitchHandler| {
            div().id((gpui::ElementId::from(id), "tooltip"))
                .when_some(reason.clone(), |el, reason| el.tooltip(super::controls::tooltip(reason.into())))
                .child(Switch::new(id, checked).disabled(reason.is_some()).on_change(handler))
        };
        let current_hold = self.access.cloud_details.hold_webhooks_while_offline;
        let (client, id) = (target.client.clone(), target.saved_id.clone());
        let environment_id = self.access_environment_id(target, cx);
        let managed = link.managed_tunnel_active && environment_id.as_ref().is_none_or(|id| {
            state.cloud.discovery.environments.iter().find(|env| &env.environment_id == id)
                .is_none_or(|env| env.endpoint.provider_kind == "cloudflare_tunnel")
        });
        let link = CloudLinkState { managed_tunnel_active: managed, ..link.clone() };
        let tunnel = {
            let entity = cx.entity().downgrade();
            let target = target.clone();
            let current = link.clone();
            Box::new(move |enabled, window: &mut Window, cx: &mut App| {
                let _ = entity.update(cx, |this, cx| {
                    this.reconcile_access_relay(&target, &current, enabled, current.publish_agent_activity, window, cx);
                });
            })
        };
        let publish = {
            let entity = cx.entity().downgrade();
            let target = target.clone();
            let current = link.clone();
            Box::new(move |enabled: bool, window: &mut Window, cx: &mut App| {
                let _ = entity.update(cx, |this, cx| {
                    this.reconcile_access_relay(&target, &current, current.managed_tunnel_active, enabled, window, cx);
                });
            })
        };
        let hold = {
            let entity = cx.entity().downgrade();
            let (client, id) = (client.clone(), id.clone());
            let publish = link.publish_agent_activity;
            Box::new(move |enabled: bool, _: &mut Window, cx: &mut App| {
                let _ = entity.update(cx, |this, cx| {
                    this.mutate_access_relay(client.clone(), id.clone(), "/api/connect/preferences", Some(json!({
                        "publishAgentActivity": publish, "holdWebhooksWhileOffline": enabled,
                    })), None, cx);
                });
            })
        };
        let tunnel_reason = disabled(access::cloud_change_replaces_link(&link, !managed, link.publish_agent_activity));
        let publish_reason = if !link.publish_agent_activity { denial.clone() } else { None }
            .or_else(|| disabled(access::cloud_change_replaces_link(&link, managed, !link.publish_agent_activity)));
        let check_client = target.client.clone();
        let check_id = target.saved_id.clone();
        let can_check = check_client.is_some() && target.grants.as_ref().is_some_and(|grants| grants.grants(RELAY_READ));
        let reachability = match &self.access.cloud_details.reachability {
            Some(t3_core::mcp::gate::ConnectReachability::Reachable { .. }) => vec![t(cx, "Reachable").to_owned()],
            Some(reachability) => reachability.unreachable_info().map(|info| {
                [Some(info.reason), info.hint, Some(info.fix)].into_iter().flatten().filter(|text| !text.is_empty()).collect()
            }).unwrap_or_default(),
            None => vec![t(cx, "No reachability check yet.").to_owned()],
        };
        let sign_in = || Button::new("access-relay-sign-in", ButtonVariant::Outline, ButtonSize::Xs)
            .label(t(cx, "Sign in")).disabled(state.cloud.busy)
            .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.sign_in_cloud(cx))));
        let mut connect = Row::new("access-relay-configure", t(cx, "T3 Connect"))
            .description(if managed { t(cx, "The environment is linked to T3 Connect.").to_owned() }
                else if on_host { t(cx, if link.link_revoked {
                    "This environment was removed from T3 Connect. Run `t3 connect link` on the host to link it again."
                } else { "Run `t3 connect link` on the host, then restart T3 Code there." }).to_owned() }
                else { denial.clone().unwrap_or_else(|| t(cx, "Make this environment available to your other devices through T3 Connect.").to_owned()) })
            .below(v_flex().gap(r(4.)).when(link.managed_tunnel_active, |el| {
                el.children(reachability.into_iter().map(|text| muted_text(text, cx)))
                    .child(div().child(Button::new("access-check-reachability", ButtonVariant::Outline, ButtonSize::Xs)
                        .label(t(cx, "Check reachability")).disabled(!can_check || self.access.relay_saving)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mutate_access_relay(check_client.clone(), check_id.clone(), "/api/connect/reachability", None, None, cx);
                        }))))
            }));
        if !on_host || managed {
            connect = connect.control(
                if !signed_in && !managed && denial.is_none() { sign_in().into_any_element() }
                else { make_switch("access-relay-switch", managed, tunnel_reason, tunnel).into_any_element() },
            );
        }
        let mut rows = vec![connect.into_any_element(), Row::new("access-publish-activity", t(cx, "Publish agent activity"))
            .description(t(cx, "Send activity to mobile notifications and Live Activities."))
            .control(make_switch("access-publish-activity-switch", link.publish_agent_activity, publish_reason, publish))
            .into_any_element()];
        if link.managed_tunnel_active {
            let reason = if current_hold.is_none() {
                Some(t(cx, "This server does not report a webhook queue preference.").to_owned())
            } else {
                disabled(false)
            };
            rows.push(Row::new("access-hold-webhooks", t(cx, "Hold webhooks while offline"))
                .control(make_switch("access-hold-webhooks-switch", current_hold.unwrap_or(false), reason, hold)).into_any_element());
        }
        rows
    }

    fn access_environment_id(&self, target: &ResolvedAccessTarget, cx: &App) -> Option<String> {
        target.config.as_ref().map(|config| config.environment.environment_id.clone())
            .filter(|id| !id.is_empty())
            .or_else(|| self.state.read(cx).environments.get(&target.saved_id).and_then(|saved| saved.environment_id.clone()))
    }

    fn reconcile_access_relay(
        &mut self, target: &ResolvedAccessTarget, current: &CloudLinkState, managed: bool, publish: bool,
        window: &mut Window, cx: &mut Context<Self>,
    ) {
        let Some(client) = target.client.clone().filter(|_| !self.access.relay_saving) else { return };
        let id = target.saved_id.clone();
        if self.access.active_target.as_deref() != Some(&id) { return; }
        let sessions = client.session_state();
        let can_manage = |session: &AuthSessionState| session.grants(RELAY_READ) && session.grants(RELAY_WRITE);
        if !sessions.borrow().as_ref().is_some_and(can_manage) { return; }
        let environment_id = self.access_environment_id(target, cx);
        if !managed && !publish {
            self.mutate_access_relay(Some(client), id, "/api/connect/unlink", None, environment_id, cx);
            return;
        }
        if !access::cloud_change_replaces_link(current, managed, publish) {
            self.mutate_access_relay(Some(client), id, "/api/connect/preferences", Some(json!({
                "publishAgentActivity": publish,
            })), None, cx);
            return;
        }

        let Some(environment_id) = environment_id else { return };
        let cloud = &self.state.read(cx).cloud;
        let policy = target.config.as_ref().and_then(|config| config.managed_policy.as_ref());
        let denied = t3_core::managed_policy::capability_denial(policy, t3_core::managed_policy::Capability::Connect);
        if !current.linkable_here.unwrap_or(true) || !cloud.service.configured() || !cloud.has_clerk_session
            || denied.is_some() || !sessions.borrow().as_ref().is_some_and(access::can_configure_relay)
        {
            return;
        }
        let service = cloud.service.clone();
        let runtime = self.state.read(cx).runtime().clone();
        let generations = client.connection_generation();
        let generation = *generations.borrow();
        let credentials = client.credentials();
        self.access.relay_saving = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let permitted = || *generations.borrow() == generation && sessions.borrow().as_ref().is_some_and(|session| {
                can_manage(session) && access::can_configure_relay(session)
            });
            let result = async {
                if managed {
                    let status = client.relay_client_status().await?;
                    match status["status"].as_str() {
                        Some("available") => {}
                        Some("missing") => {
                            let answer = cx.update(|window, cx| window.prompt(
                                PromptLevel::Info,
                                t(cx, "Install relay client?"),
                                Some(t(cx, "T3 Code needs the relay client to make this environment available through T3 Connect.")),
                                &[t(cx, "Install"), t(cx, "Cancel")],
                                cx,
                            )).map_err(|error| error.to_string())?;
                            if answer.await.ok() != Some(0) {
                                return Ok(());
                            }
                            if !permitted() {
                                return Err("This connection cannot manage T3 Connect.".to_owned());
                            }
                            client.install_relay_client().await?;
                        }
                        _ => return Err("The relay client is unavailable on this environment.".to_owned()),
                    }
                }
                if !permitted() {
                    return Err("This connection cannot manage T3 Connect.".to_owned());
                }
                runtime.spawn(async move { service.link_environment(&credentials, &environment_id, managed, publish).await })
                    .await.map_err(|error| error.to_string())?
            }.await;
            let _ = this.update(cx, |this, cx| {
                this.state.update(cx, |state, cx| state.refresh_cloud(cx));
                if this.access.active_target.as_deref() == Some(&id) {
                    this.access.relay_saving = false;
                    this.access.link_state_task = None;
                    cx.notify();
                }
                if let Err(error) = result {
                    store::show_error(t(cx, &error).to_owned(), cx);
                }
            });
        }).detach();
    }

    fn mutate_access_relay(
        &mut self, client: Option<ClientHandle>, id: String, path: &'static str, body: Option<Value>,
        environment_id: Option<String>, cx: &mut Context<Self>,
    ) {
        let Some(client) = client.filter(|_| !self.access.relay_saving) else { return };
        self.access.relay_saving = true;
        let unlinking = path == "/api/connect/unlink";
        let cloud = self.state.read(cx).cloud.service.clone();
        let relay_error = t(cx, "Disconnected locally, but T3 Connect did not confirm it removed this environment").to_owned();
        self.access_request_on(id, async move {
            let value = client.post_access(path, body).await?;
            let relay_failure = if unlinking && let Some(environment_id) = environment_id {
                cloud.unlink_environment(&environment_id).await.err()
            } else {
                None
            };
            Ok((value, relay_failure))
        }, cx, move |this, result, cx| {
            this.access.relay_saving = false;
            match result {
                Ok((value, _)) if path == "/api/connect/reachability" => {
                    this.access.cloud_details.reachability = serde_json::from_value(value).ok();
                }
                Ok((_, relay_failure)) => {
                    this.access.link_state_task = None;
                    if unlinking {
                        this.state.update(cx, |state, cx| state.refresh_cloud(cx));
                        if let Some(detail) = relay_failure {
                            store::show_error(format!("{relay_error}: {detail}"), cx);
                        }
                    }
                }
                Err(error) => {
                    this.access.link_state_task = None;
                    store::show_error(error, cx);
                }
            }
        });
    }

    pub(super) fn access_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let (candidates, target) = self.resolve_access_target(cx);
        let Some(target) = target else {
            let row = Row::new("connections-environment", "No environment selected")
                .description("Connect an environment to view its settings and access.");
            return vec![Block::new(&["connections-environment"], section("Primary environment", vec![row.into_any_element()], cx))];
        };
        self.access.dialog_target = Some(target.clone());
        if self.access.revoke_device_confirm.is_some() && self.access.revoke_focus.is_none() {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            self.access.revoke_focus = Some(focus);
        }
        self.ensure_target_watchers(&target, cx);
        self.ensure_target_access_stream(&target, cx);
        let now = now_ms();
        let links = regular_visible_links(&self.access.access, &self.access.connect_devices, now);
        let pending_requests: Vec<PendingConnectDeviceRequest> = self.access.connect_devices.visible_pending_requests(now).into_iter().cloned().collect();
        self.ensure_ticker(!links.is_empty() || !pending_requests.is_empty(), cx);

        let mut blocks = Vec::new();

        // Environment picker when more than one saved environment is eligible (`pingdotgg/t3code#26464`).
        if candidates.len() > 1 {
            let options: Vec<SelectOption> = candidates
                .iter()
                .map(|(candidate, label)| {
                    let config = self.state.read(cx).fleet.environment(&candidate.id).and_then(|view| view.config.config.as_ref());
                    SelectOption::new(candidate.id.clone(), label.clone()).icon(config.map_or(Icon::Server, Icon::machine))
                })
                .collect();
            let entity = cx.entity().downgrade();
            let select = Select::new("access-environment-select", options)
                .selected(target.saved_id.clone())
                .show_check()
                .on_change(move |next: SharedString, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.access.selected_env = Some(next.to_string());
                        cx.notify();
                    });
                });
            let picker_row = h_flex()
                .px(r(4.))
                .gap(r(8.))
                .items_center()
                .text_sm()
                .text_color(hsla(palette(cx).muted_foreground))
                .child("Showing access for")
                .child(select);
            blocks.push(Block::new(&["access-environment-picker"], picker_row));
        }

        // Legacy permissions notice (`PermissionUpdateNotice.ts`, `CN-REPAIR`).
        if let Some(notice) = self.legacy_permissions_notice(&target, cx) {
            blocks.push(Block::new(&["permission-update-notice"], notice));
        }

        let server_version = target
            .config
            .as_ref()
            .map(|c| c.environment.server_version.as_str())
            .filter(|v| !v.is_empty())
            .unwrap_or("unknown");
        let mut version = Row::new("access-version", t(cx, "Version"))
            .description(format!("{server_version} \u{00b7} {}", target.address));
        if let Some(update) = self.access_server_update_button(&target.saved_id, cx) {
            version = version.control(update);
        }
        let version = version.into_any_element();

        let network = self.access_network_row(&target, cx);

        let can_read = Self::target_may(target.grants.as_ref(), ACCESS_READ);
        let can_write = Self::target_may(target.grants.as_ref(), ACCESS_WRITE);

        let toggle = cx.listener(|this, _, _, cx| {
            this.access.permissions_open = !this.access.permissions_open;
            cx.notify();
        });
        let permissions = disclosure("access-permissions", "Permissions", self.access.permissions_open, vec![self.permissions_row(target.grants.as_ref(), cx)], toggle, cx);

        let mut machine_rows = vec![version, network];
        if let Some(link) = self.access.cloud_link_state.as_ref() {
            machine_rows.extend(self.access_relay_rows(&target, link, cx));
        } else if target.grants.as_ref().is_some_and(|session| !session.grants(RELAY_READ)) {
            machine_rows.push(Row::new("access-relay-configure", t(cx, "T3 Connect"))
                .description(t(cx, "Run `t3 connect link` on the host, then restart T3 Code there."))
                .into_any_element());
        }
        // `ConnectionsSettings.tsx:1304-1311` + `SavedEnvironmentRows.tsx:746`: render the
        // T3 Connect device-approval switch only when the managed tunnel is active and the server
        // advertises `requireConnectDeviceApproval`.
        if connect_approval_row_visible(self.access.managed_tunnel_active, target.extras.require_connect_device_approval) {
            let require_approval = target.extras.require_connect_device_approval.unwrap_or(false);
            let policy = target.config.as_ref().and_then(|c| c.managed_policy.as_ref());
            let approval_locked = t3_core::managed_policy::setting_lock_reason(
                policy,
                "requireConnectDeviceApproval",
            )
            .or_else(|| {
                t3_core::managed_policy::capability_denial(
                    policy,
                    t3_core::managed_policy::Capability::Connect,
                )
            });
            let toggle_client = target.client.clone();
            let toggle_target = target.saved_id.clone();
            let toggle_entity = cx.entity().downgrade();
            machine_rows.push(
                Row::new("connect-require-approval", "Require approval for new devices")
                    .description("Devices that sign in to your T3 Connect account wait until someone already connected here approves them.")
                    .control(crate::components::managed::ManagedControl::new(
                        "managed-connect-require-approval",
                        approval_locked,
                        Switch::new("connect-require-approval-switch", require_approval)
                            .disabled(!can_write || self.access.approval_saving)
                            .on_change(move |next, _, cx| {
                                let client = toggle_client.clone();
                                let target = toggle_target.clone();
                                let _ = toggle_entity.update(cx, |this, cx| {
                                    this.toggle_connect_device_approval(client, target, next, cx);
                                });
                            }),
                    ))
                    .into_any_element(),
            );
        }
        machine_rows.push(permissions);
        if let Some(policy) = target.config.as_ref().and_then(|c| c.managed_policy.as_ref()) {
            machine_rows.push(div().px(r(16.)).py(r(12.)).child(crate::components::managed::attribution(policy, cx)).into_any_element());
        }
        blocks.push(Block::new(&["connections-environment"], section(target.label.clone(), machine_rows, cx)));

        if can_read || can_write {
            blocks.push(Block::new(&["authorized-clients"], self.authorized_clients(&target, now, window, cx)));
        }
        // `ConnectionsSettings.tsx:1379-1389`: render T3 Connect devices only when the session
        // can read access and there is at least one pending request or trusted device.
        if connect_devices_section_visible(can_read, pending_requests.len(), self.access.connect_devices.devices.len()) {
            blocks.push(Block::new(&["connect-devices"], self.connect_devices_section(&target, &pending_requests, now, can_write, cx)));
        }
        blocks
    }

    fn legacy_permissions_notice(&mut self, target: &ResolvedAccessTarget, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let session = target.grants.as_ref()?;
        if !session_has_legacy_permissions(session) {
            return None;
        }
        let env_key = target
            .config
            .as_ref()
            .map(|c| c.environment.environment_id.as_str())
            .filter(|id| !id.is_empty())
            .unwrap_or(&target.saved_id)
            .to_owned();
        let ui_key = format!("permission-update-dismissed-{env_key}");
        let dismissed = *self.access.dismissed_legacy_notice.entry(env_key.clone()).or_insert_with(|| {
            t3_client::store::load_ui_state(&ui_key).and_then(|v| v.as_bool()) == Some(true)
        });
        if dismissed {
            return None;
        }
        let p = palette(cx);
        let dismiss_env = env_key;
        let dismiss_key = ui_key;
        Some(
            v_flex()
                .px(r(14.))
                .py(r(12.))
                .gap(r(6.))
                .rounded(r(12.))
                .border_1()
                .border_color(hsla(p.warning).opacity(0.35))
                .bg(hsla(p.warning).opacity(0.08))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(format!("Permissions have changed for {}", target.label)))
                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(
                    "This connection still uses the old permissions, so some actions may no longer be available. Pair again using a new link with the permissions you need.",
                ))
                .child(
                    h_flex().pt(r(2.)).child(
                        Button::new("access-legacy-perms-dismiss", ButtonVariant::GhostMuted, ButtonSize::Compact)
                            .label("Dismiss")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.access.dismissed_legacy_notice.insert(dismiss_env.clone(), true);
                                t3_client::store::save_ui_state(&dismiss_key, &serde_json::Value::Bool(true));
                                let toast_key = format!("permission-update:{dismiss_env}");
                                this.state.update(cx, |state, cx| state.close_toast_by_key(&toast_key, cx));
                                cx.notify();
                            })),
                    ),
                ),
        )
    }

    /// The "Permissions" disclosure's content: each scope and whether this connection has it.
    fn permissions_row(&self, session: Option<&AuthSessionState>, cx: &App) -> AnyElement {
        session_permissions(session, cx)
    }

    fn authorized_clients(&mut self, target: &ResolvedAccessTarget, now: i64, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let can_write = Self::target_may(target.grants.as_ref(), ACCESS_WRITE);
        let can_read = Self::target_may(target.grants.as_ref(), ACCESS_READ);
        let denial = self.pairing_denial_for(target.config.as_ref());
        let client_for_revoke = target.client.clone();
        let target_id_for_revoke = target.saved_id.clone();
        let grants_for_create = target.grants.clone();
        let denied = denial.is_some();

        let connect_approval_off = target.extras.require_connect_device_approval == Some(false);
        let confirm_revoke_others = connect_approval_off
            && can_write
            && revokes_connect_devices(&self.access.connect_devices.devices, &self.access.access.clients);
        let peer_links_enabled = target
            .config
            .as_ref()
            .is_some_and(|cfg| cfg.settings.threads.linked_environments_enabled);
        let target_id_for_link = target.saved_id.clone();
        let client_for_link = target.client.clone();
        let actions = can_write.then(|| {
            h_flex()
                .gap(r(6.))
                .when(peer_links_enabled, |this| {
                    this.child(crate::components::managed::ManagedControl::new(
                        "managed-link-machine",
                        denial.clone(),
                        Button::new("access-link-machine", ButtonVariant::Outline, ButtonSize::Compact)
                            .icon(Icon::Link2)
                            .label("Link machine")
                            .disabled(self.access.link_machine.is_some())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if denied {
                                    return;
                                }
                                this.open_link_machine_form(target_id_for_link.clone(), client_for_link.clone(), window, cx);
                            })),
                    ))
                })
                .child(
                    Button::new("access-revoke-others", ButtonVariant::Outline, ButtonSize::Compact)
                        .label(if self.access.revoking_others { "Revoking\u{2026}" } else { "Revoke others" })
                        .disabled(self.access.revoking_others || !self.access.access.can_revoke_others())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if confirm_revoke_others {
                                this.access.revoke_device_confirm = Some(RevokeConnectDeviceConfirm {
                                    action: PendingConnectRevokeAction::OtherClients,
                                    title: "Revoke all other clients?".to_owned(),
                                    turn_on_approval: true,
                                    busy: false,
                                });
                                cx.notify();
                            } else {
                                this.revoke_others(client_for_revoke.clone(), target_id_for_revoke.clone(), cx);
                            }
                        })),
                )
                .child(
                    crate::components::managed::ManagedControl::new(
                        "managed-create-link",
                        denial.clone(),
                        Button::new("access-create-link", ButtonVariant::Outline, ButtonSize::Compact)
                            .icon(Icon::Plus)
                            .label("Create link")
                            .disabled(self.access.create.is_some())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if denied {
                                    return;
                                }
                                let allowed = |scope: &str| grants_for_create.as_ref().is_none_or(|session| session.grants(scope));
                                let scopes = standard_scopes().into_iter().filter(|scope| allowed(scope)).collect();
                                this.access.label.update(cx, |input, cx| input.set_value("", window, cx));
                                let focus = cx.focus_handle();
                                this.access.label.read(cx).focus_handle(cx).focus(window, cx);
                                this.access.create = Some(CreateLink { focus, scopes, creating: false, error: None });
                                cx.notify();
                            })),
                    ),
                )
        });

        let open = self.access.clients_open;
        let links: Vec<PairingLink> = regular_visible_links(&self.access.access, &self.access.connect_devices, now).into_iter().cloned().collect();
        let clients: Vec<ClientSession> = self.access.access.sorted_clients().into_iter().cloned().collect();
        let summary = {
            let c = clients.len();
            let l = links.len();
            let mut s = format!("{c} {}", if c == 1 { "client" } else { "clients" });
            if l > 0 {
                s.push_str(&format!(" \u{00b7} {l} pairing {}", if l == 1 { "link" } else { "links" }));
            }
            s
        };

        let mcp = use_in_another_app_action(target, cx);
        let header = h_flex()
            .justify_between()
            .gap(r(6.))
            .flex_wrap()
            .pr(r(8.))
            .child(
                h_flex()
                    .id("access-clients-fold-toggle")
                    .gap(r(6.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.access.clients_open = !this.access.clients_open;
                        cx.notify();
                    }))
                    .child(
                        (if open { Icon::ChevronDown } else { Icon::ChevronRight })
                            .render(px(14.), hsla(p.muted_foreground)),
                    )
                    .child(section_title("Authorized clients"))
                    .when(self.access.access.loaded, |this| {
                        this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(summary))
                    }),
            )
            .child(h_flex().gap(r(6.)).flex_wrap().child(mcp).children(actions));

        let effective_address = self.access.endpoint_preference.as_deref()
            .filter(|origin| pairing_endpoints(target).iter().any(|candidate| candidate == origin))
            .unwrap_or_else(|| effective_pairing_address(&target.address, &target.extras.direct_endpoints));
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(reason) = denial {
            rows.push(div().px(r(16.)).py(r(12.)).child(crate::components::managed::notice(reason, cx)).into_any_element());
        }
        if self.access.link_machine.is_some() && !denied {
            rows.push(self.link_machine_panel(target, cx));
        }
        if !can_read {
            rows.push(div().px(r(16.)).py(r(12.)).child(muted_text("This connection can create access links but cannot view authorized clients.", cx)).into_any_element());
        }
        if let Some(error) = &self.access.error {
            rows.push(div().px(r(16.)).py(r(12.)).text_sm().text_color(hsla(p.destructive_foreground)).child(error.clone()).into_any_element());
        }

        if open {
            for link in &links {
                rows.push(self.link_row(link, effective_address, now, can_write, target, cx));
            }
            for client in &clients {
                rows.push(self.client_row(client, can_write, target, cx));
            }
            if can_read && links.is_empty() && clients.is_empty() && self.access.error.is_none() {
                let text = if self.access.access.loaded { "No pairing links or client sessions." } else { "Loading\u{2026}" };
                rows.push(div().px(r(16.)).py(r(12.)).child(muted_text(text, cx)).into_any_element());
            }
        } else if can_read
            && let Some(newest_id) = self.access.newest_pairing_link_id.as_deref()
            && let Some(newest) = links.iter().find(|link| link.id == newest_id)
        {
            // `pingdotgg/t3code#30498`: show the newly created pairing link (with share/QR panel open) without expanding the client list.
            rows.push(self.link_row(newest, effective_address, now, can_write, target, cx));
        }

        let mut container = v_flex().gap(r(10.)).child(header);
        if !rows.is_empty() {
            container = container.child(group(rows, cx));
        }
        container.into_any_element()
    }

    pub(super) fn dismiss_access_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.access.create.is_none() && self.access.revoke_device_confirm.is_none() {
            return false;
        }
        if self.access.create.as_ref().is_some_and(|create| create.creating)
            || self.access.revoke_device_confirm.as_ref().is_some_and(|confirm| confirm.busy)
        {
            return true;
        }
        self.access.create = None;
        self.access.revoke_device_confirm = None;
        self.access.revoke_focus = None;
        self.focus.focus(window, cx);
        cx.notify();
        true
    }

    pub(super) fn access_dialog_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target = self.access.dialog_target.as_ref()?;
        let frame = if let Some(create) = self.access.create.as_ref() {
            self.create_link_frame(create, target, cx)
        } else {
            self.revoke_connect_frame(target, cx)?
        };
        let weak = cx.entity().downgrade();
        Some(frame.render(window, move |window, cx| {
            let _ = weak.update(cx, |this, cx| { this.dismiss_access_dialog(window, cx); });
        }, cx))
    }

    fn revoke_connect_frame(&self, target: &ResolvedAccessTarget, cx: &mut Context<Self>) -> Option<DialogFrame> {
        let confirm = self.access.revoke_device_confirm.as_ref()?;
        let p = palette(cx);
        let confirm_client = target.client.clone();
        let confirm_target = target.saved_id.clone();
        let checked = confirm.turn_on_approval;
        let busy = confirm.busy;
        let focus = self.access.revoke_focus.clone()?;
        let body = h_flex()
            .id("revoke-connect-turn-on-approval")
            .items_start().gap(r(8.))
            .when(!busy, |this| this.cursor_pointer())
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(confirm) = this.access.revoke_device_confirm.as_mut().filter(|confirm| !confirm.busy) {
                    confirm.turn_on_approval = !confirm.turn_on_approval;
                }
                cx.notify();
            }))
            .child(div().pt(r(2.)).child(checkbox(checked, cx)))
            .child(v_flex().min_w_0()
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(t(cx, "Require approval for new devices")))
                .child(muted_text(t(cx, "The device has to be approved here before it can connect again."), cx)));
        Some(DialogFrame {
            id: "connect-revoke-confirm",
            title: confirm.title.clone().into(),
            description: t(cx, "Approval for new devices is off, so a revoked device signed in to your T3 Connect account can connect again on its own.").into(),
            max_width: 448.,
            body: body.into_any_element(),
            footer: dialog_footer(true, vec![
                Button::new("revoke-connect-cancel", ButtonVariant::Outline, ButtonSize::Sm)
                    .label(t(cx, "Cancel")).disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| { this.dismiss_access_dialog(window, cx); })).into_any_element(),
                Button::new("revoke-connect-confirm", ButtonVariant::Destructive, ButtonSize::Sm)
                    .label(t(cx, if busy { "Revoking…" } else { "Revoke" })).disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_revoke_connect_device(confirm_client.clone(), confirm_target.clone(), cx);
                    })).into_any_element(),
            ], cx).into_any_element(),
            focus,
        })
    }

    /// Pending T3 Connect approval requests and trusted Connect devices
    /// (`ConnectDeviceApprovalSettings.tsx`, `pingdotgg/t3code#36419`).
    fn connect_devices_section(
        &self,
        target: &ResolvedAccessTarget,
        requests: &[PendingConnectDeviceRequest],
        now: i64,
        can_write: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let connect_approval_off = target.extras.require_connect_device_approval == Some(false);
        let mut rows: Vec<AnyElement> = Vec::new();

        for req in requests {
            let busy = self.access.connect_device_busy.as_deref() == Some(req.pairing_link_id.as_str());
            let approve_client = target.client.clone();
            let approve_target = target.saved_id.clone();
            let approve_id = req.pairing_link_id.clone();
            let deny_client = target.client.clone();
            let deny_target = target.saved_id.clone();
            let deny_id = req.pairing_link_id.clone();
            rows.push(
                h_flex()
                    .px(r(16.))
                    .py(r(12.))
                    .gap(r(12.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(r(2.))
                            .child(
                                h_flex()
                                    .gap(r(6.))
                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Waiting for approval \u{00b7}"))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_color(hsla(p.foreground))
                                            .child(req.verification_code.clone()),
                                    ),
                            )
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(format!(
                                "{}. Approve only if the device in front of you shows this code.",
                                describe_connect_device_client(req.client.as_ref())
                            ))),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(r(6.))
                            .child(
                                Button::new(SharedString::from(format!("connect-deny-{}", req.pairing_link_id)), ButtonVariant::DestructiveOutline, ButtonSize::Compact)
                                    .label("Deny")
                                    .disabled(!can_write || busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.deny_connect_device(deny_client.clone(), deny_target.clone(), deny_id.clone(), cx);
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("connect-approve-{}", req.pairing_link_id)), ButtonVariant::Default, ButtonSize::Compact)
                                    .label(if busy { "Saving\u{2026}" } else { "Approve" })
                                    .disabled(!can_write || busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.approve_connect_device(approve_client.clone(), approve_target.clone(), approve_id.clone(), cx);
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }

        let devices: Vec<ConnectDevice> = self.access.connect_devices.devices.clone();
        for device in &devices {
            let busy = self.access.connect_device_busy.as_deref() == Some(device.proof_key_thumbprint.as_str());
            let revoke_client = target.client.clone();
            let revoke_target = target.saved_id.clone();
            let thumbprint = device.proof_key_thumbprint.clone();
            let confirm_title = format!("Revoke {}?", connect_device_name(device.client.as_ref()));
            rows.push(
                h_flex()
                    .px(r(16.))
                    .py(r(12.))
                    .gap(r(12.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(r(2.))
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.foreground))
                                    .child(describe_connect_device_client(device.client.as_ref())),
                            )
                            .child(
                                h_flex()
                                    .gap(r(4.))
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(div().font_family(cx.theme().mono_font_family.clone()).child(device.verification_code.clone()))
                                    .child(format!("\u{00b7} Trusted {}", format_approved_since(&device.approved_at, now))),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("connect-revoke-{}", device.proof_key_thumbprint)), ButtonVariant::DestructiveOutline, ButtonSize::Compact)
                            .label(if busy { "Revoking\u{2026}" } else { "Revoke" })
                            .disabled(!can_write || busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if connect_approval_off && can_write {
                                    this.access.revoke_device_confirm = Some(RevokeConnectDeviceConfirm {
                                        action: PendingConnectRevokeAction::ConnectDevice(thumbprint.clone()),
                                        title: confirm_title.clone(),
                                        turn_on_approval: true,
                                        busy: false,
                                    });
                                    cx.notify();
                                } else {
                                    this.revoke_connect_device_now(revoke_client.clone(), revoke_target.clone(), thumbprint.clone(), cx);
                                }
                            })),
                    )
                    .into_any_element(),
            );
        }

        section("T3 Connect devices", rows, cx).into_any_element()
    }

    /// Builds `LinkableMachine` entries for the `"Link machine"` selector (`LinkMachineSelect.tsx`).
    fn access_linkable_machines(&self, target: &ResolvedAccessTarget, cx: &App) -> Vec<LinkableMachine> {
        let app = self.state.read(cx);
        let host_env_id = target
            .config
            .as_ref()
            .map(|cfg| cfg.environment.environment_id.as_str())
            .filter(|id| !id.is_empty())
            .unwrap_or(target.saved_id.as_str());
        let mut machines = Vec::new();
        let mut seen = HashSet::new();
        for env in &app.environments.environments {
            if env.disabled || env.id == target.saved_id {
                continue;
            }
            let fleet_view = app.fleet.environment(&env.id);
            let env_id = fleet_view
                .and_then(|view| view.config.config.as_ref())
                .map(|cfg| cfg.environment.environment_id.clone())
                .filter(|id| !id.is_empty())
                .or_else(|| env.environment_id.clone())
                .unwrap_or_else(|| env.id.clone());
            if env_id == host_env_id || !seen.insert(env_id.clone()) {
                continue;
            }
            let extras = t3_client::supervisor::server_config_extras_for(&env.http_base).borrow().clone();
            let mut raw_urls: Vec<&str> = extras.direct_endpoints.iter().map(String::as_str).collect();
            raw_urls.push(env.http_base.as_str());
            let urls = collect_known_machine_urls(raw_urls);
            let connected = fleet_view.is_some_and(|view| view.status == ConnectionStatus::Connected);
            let linked = self.access.linked_peer_ids.contains(&env_id);
            machines.push(LinkableMachine {
                environment_id: env_id,
                label: if env.label.trim().is_empty() { env.address() } else { env.label.clone() },
                machine: fleet_view.and_then(|view| view.config.config.as_ref()).and_then(|config| {
                    config.settings.environment_icon.clone().or_else(|| config.environment.platform.as_ref().map(|platform| platform.machine.clone()))
                }),
                urls,
                connected,
                connect_only: false,
                linked,
            });
        }
        for cloud_env in &app.cloud.discovery.environments {
            if cloud_env.environment_id == host_env_id || !seen.insert(cloud_env.environment_id.clone()) {
                continue;
            }
            let linked = self.access.linked_peer_ids.contains(&cloud_env.environment_id);
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

    fn open_link_machine_form(
        &mut self,
        target_saved_id: String,
        client: Option<ClientHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("https://box.example.ts.net"));
        let pairing_code = cx.new(|cx| InputState::new(window, cx).placeholder("From Sharing & access there, or t3 pair"));
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
        self.access.link_machine = Some(LinkMachineForm {
            choice: None,
            address,
            pairing_code,
            access: "approval-required".to_owned(),
            mint_failed: HashSet::new(),
            submitting: false,
            error: None,
            _subscriptions: subscriptions,
        });
        self.refresh_linked_peers(target_saved_id, client, cx);
        cx.notify();
    }

    fn refresh_linked_peers(
        &mut self,
        target_saved_id: String,
        client: Option<ClientHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = client else {
            return;
        };
        let runtime = self.state.read(cx).runtime().clone();
        let task = runtime.spawn(async move { client.request("peerLinks.list", json!({})).await });
        self.access.linked_peers_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(value)) = task.await else {
                return;
            };
            let ids: HashSet<String> = value
                .get("links")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| item.get("environmentId").and_then(Value::as_str))
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned)
                .collect();
            let _ = this.update(cx, |this, cx| {
                if this.access.active_target.as_deref() == Some(target_saved_id.as_str())
                    || this.access.active_target.is_none()
                {
                    this.access.linked_peer_ids = ids;
                    cx.notify();
                }
            });
        }));
    }

    fn submit_access_link_machine(&mut self, target: &ResolvedAccessTarget, cx: &mut Context<Self>) {
        let Some(client) = target.client.clone() else {
            if let Some(form) = self.access.link_machine.as_mut() {
                form.error = Some("Reconnect to this environment first.".to_owned());
            }
            return cx.notify();
        };
        let machines = self.access_linkable_machines(target, cx);
        let Some(form) = self.access.link_machine.as_mut() else {
            return;
        };
        if form.submitting {
            return;
        }
        let selected = form
            .choice
            .clone()
            .or_else(|| machines.is_empty().then(|| ANOTHER_ADDRESS.to_owned()));
        let Some(selected) = selected else {
            return;
        };
        let machine = machines.iter().find(|candidate| candidate.environment_id == selected).cloned();
        let needs_address = machine.as_ref().is_none_or(|m| m.urls.is_empty());
        let typed_address = form.address.read(cx).value().trim().to_owned();
        let typed_code = form.pairing_code.read(cx).value().trim().to_owned();
        let access = form.access.clone();
        let can_mint = machine
            .as_ref()
            .is_some_and(|m| !needs_address && m.connected && !form.mint_failed.contains(&m.environment_id));

        let (url, alternate_urls, expected_id, expected_label) = match (&machine, needs_address) {
            (Some(m), false) => (
                m.urls[0].clone(),
                m.urls[1..].to_vec(),
                Some(m.environment_id.clone()),
                Some(m.label.clone()),
            ),
            _ => {
                if typed_address.is_empty() {
                    return;
                }
                (typed_address, Vec::new(), None, None)
            }
        };
        if !can_mint && typed_code.is_empty() {
            return;
        }
        form.submitting = true;
        form.error = None;
        let target_saved_id = target.saved_id.clone();
        let mint_future = can_mint.then(|| {
            let env_id = machine.as_ref().expect("machine present").environment_id.clone();
            let fut = self.state.read(cx).mint_pairing_code_on(&env_id, &access);
            (env_id, fut)
        });
        let linked_env_id = expected_id.clone();
        let client_for_refresh = client.clone();
        let target_saved_id_for_refresh = target_saved_id.clone();
        let runtime = self.state.read(cx).runtime().clone();
        let task = runtime.spawn(async move {
            let code = match mint_future {
                Some((env_id, fut)) => match fut.await {
                    Ok(code) => code,
                    Err(error) => return Err((Some(env_id), error.to_string())),
                },
                None => typed_code,
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
                .request("peerLinks.link", payload)
                .await
                .map_err(|error| (None, error.to_string()))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| Err((None, e.to_string())));
            let _ = this.update(cx, |this, cx| {
                if this.access.active_target.as_deref().is_some_and(|active| active != target_saved_id.as_str()) {
                    return;
                }
                match result {
                    Ok(value) => {
                        if let Some(id) = value
                            .get("link")
                            .and_then(|link| link.get("environmentId"))
                            .and_then(Value::as_str)
                            .or_else(|| value.get("environmentId").and_then(Value::as_str))
                            .map(str::to_owned)
                            .or(linked_env_id)
                        {
                            this.access.linked_peer_ids.insert(id);
                        }
                        this.access.link_machine = None;
                        this.refresh_linked_peers(target_saved_id_for_refresh, Some(client_for_refresh), cx);
                    }
                    Err((mint_env_id, message)) => {
                        if let Some(form) = this.access.link_machine.as_mut() {
                            form.submitting = false;
                            if let Some(env_id) = mint_env_id {
                                form.mint_failed.insert(env_id);
                            }
                            form.error = Some(message);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Inline `"Link machine"` panel using `LinkMachineSelect`, `LinkAddressWarning`, and
    /// `mint_pairing_code_on` (`LinkedEnvironmentsSettings.tsx` + `LinkMachineSelect.tsx`).
    fn link_machine_panel(&self, target: &ResolvedAccessTarget, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.access.link_machine.as_ref() else {
            return div().into_any_element();
        };
        let p = palette(cx);
        let machines = self.access_linkable_machines(target, cx);
        let selected = form
            .choice
            .as_deref()
            .or_else(|| machines.is_empty().then_some(ANOTHER_ADDRESS));
        let machine = selected.and_then(|id| machines.iter().find(|candidate| candidate.environment_id == id));
        let needs_address = selected.is_some() && machine.is_none_or(|m| m.urls.is_empty());
        let typed_address = form.address.read(cx).value().trim().to_owned();
        let typed_code = form.pairing_code.read(cx).value().trim().to_owned();
        let can_mint = machine.is_some_and(|m| !needs_address && m.connected && !form.mint_failed.contains(&m.environment_id));
        let ready = selected.is_some() && (!needs_address || !typed_address.is_empty()) && (can_mint || !typed_code.is_empty());
        let entity_select = cx.entity().downgrade();
        let submit_target = ResolvedAccessTarget {
            saved_id: target.saved_id.clone(),
            label: target.label.clone(),
            address: target.address.clone(),
            is_primary: target.is_primary,
            config: target.config.clone(),
            grants: target.grants.clone(),
            client: target.client.clone(),
            extras: target.extras.clone(),
        };

        v_flex()
            .px(r(16.))
            .py(r(14.))
            .gap(r(10.))
            .child(
                v_flex()
                    .gap(r(2.))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Link an environment"))
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(
                        "Agents here can then launch, message and wait on threads there.",
                    )),
            )
            .when(!machines.is_empty(), |this| {
                this.child(crate::connect::render_link_machine_select(
                    "access-link-machine-select",
                    &machines,
                    selected,
                    form.submitting,
                    move |next, _, cx| {
                        let _ = entity_select.update(cx, |this, cx| {
                            if let Some(form) = this.access.link_machine.as_mut() {
                                form.choice = Some(next);
                                form.error = None;
                            }
                            cx.notify();
                        });
                    },
                    cx,
                ))
            })
            .when(needs_address, |this| {
                this.child(
                    v_flex()
                        .gap(r(6.))
                        .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Address"))
                        .child(Input::new(&form.address).small().disabled(form.submitting).theme_slot(ThemeSlot::Input, cx)),
                )
            })
            .when(needs_address && !typed_address.is_empty(), |this| {
                this.child(crate::connect::link_address_warning(&typed_address, cx))
            })
            .when(selected.is_some() && !can_mint, |this| {
                this.child(
                    v_flex()
                        .gap(r(6.))
                        .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Pairing code"))
                        .child(Input::new(&form.pairing_code).small().disabled(form.submitting).theme_slot(ThemeSlot::Input, cx)),
                )
            })
            .child(crate::connect::link_access_picker("access-link-level", &form.access, form.submitting,
                {
                    let weak = cx.entity().downgrade();
                    move |next, _, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            if let Some(form) = this.access.link_machine.as_mut() {
                                form.access = next.to_string();
                                cx.notify();
                            }
                        });
                    }
                }, cx))
            .children(form.error.clone().map(|error| div().text_xs().text_color(hsla(p.destructive_foreground)).child(error)))
            .child(
                h_flex()
                    .justify_end()
                    .gap(r(8.))
                    .child(
                        Button::new("access-link-machine-cancel", ButtonVariant::Ghost, ButtonSize::Sm)
                            .label("Cancel")
                            .disabled(form.submitting)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.access.link_machine = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("access-link-machine-submit", ButtonVariant::Default, ButtonSize::Sm)
                            .label(if form.submitting { "Linking\u{2026}" } else { "Link" })
                            .disabled(form.submitting || !ready)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.submit_access_link_machine(&submit_target, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn create_link_frame(&self, create: &CreateLink, target: &ResolvedAccessTarget, cx: &mut Context<Self>) -> DialogFrame {
        let p = palette(cx);
        let grants = target.grants.clone();
        let allowed = move |scope: &str| grants.as_ref().is_none_or(|session| session.grants(scope));
        let preset = |id: &'static str, label: &'static str, scopes: Vec<String>, cx: &mut Context<Self>| {
            let selected = create.scopes == scopes;
            Button::new(id, if selected { ButtonVariant::Default } else { ButtonVariant::Outline }, ButtonSize::Compact).label(label).disabled(create.creating).on_click(cx.listener(
                move |this, _, _, cx| {
                    if let Some(create) = this.access.create.as_mut().filter(|create| !create.creating) {
                        create.scopes = scopes.clone();
                    }
                    cx.notify();
                },
            ))
        };
        let read_only: Vec<String> = READ_ONLY_SCOPES.iter().filter(|scope| allowed(scope)).map(|scope| (*scope).to_owned()).collect();
        let standard: Vec<String> = standard_scopes().into_iter().filter(|scope| allowed(scope)).collect();
        let notice = access::scopes_notice(&create.scopes);
        let submit_client = target.client.clone();
        let submit_target = target.saved_id.clone();
        let denied = self.pairing_denial_for(target.config.as_ref()).is_some();
        let body = v_flex().gap(r(12.))
            .child(
                v_flex()
                    .gap(r(6.))
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Client label (optional)"))
                    .child(div().max_w(r(320.)).child(Input::new(&self.access.label).small().disabled(create.creating).theme_slot(ThemeSlot::Input, cx))),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        v_flex()
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Permissions"))
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Limit what the paired client can do.")),
                    )
                    .child(h_flex().gap(r(4.)).child(preset("access-preset-read", "Read only", read_only, cx)).child(preset("access-preset-standard", "Standard", standard, cx))),
            )
            .child(div().grid().grid_cols(2).gap(r(8.)).children(SCOPE_OPTIONS.iter().filter(|(scope, _, _)| allowed(scope)).map(|(scope, title, description)| {
                let checked = create.scopes.iter().any(|candidate| candidate == scope);
                let warning = scope_warning(scope, &create.scopes);
                let scope = *scope;
                h_flex()
                    .id(scope)
                    .items_start()
                    .gap(r(8.))
                    .when(create.creating, |this| this.opacity(0.64))
                    .when(!create.creating, |this| this.cursor_pointer())
                    .child(div().pt(r(2.)).child(checkbox(checked, cx)))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(*title))
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(*description))
                            .when_some(warning, |this, text| {
                                this.child(div().text_xs().text_color(hsla(p.warning_foreground)).child(text))
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(create) = this.access.create.as_mut().filter(|create| !create.creating) {
                            toggle_scope(&mut create.scopes, scope, !checked);
                        }
                        cx.notify();
                    }))
            })))
            .children(notice.map(|(text, error)| {
                div().text_xs().text_color(if error { hsla(p.destructive_foreground) } else { hsla(p.warning_foreground) }).child(text)
            }))
            .children(create.error.clone().map(|error| div().text_xs().text_color(hsla(p.destructive_foreground)).child(error)))
            .into_any_element();
        DialogFrame {
            id: "access-create-link-dialog",
            title: t(cx, "Create pairing link").into(),
            description: t(cx, "Generate a one-time link that another device can use to pair with this backend as an authorized client.").into(),
            max_width: 620.,
            body,
            footer: dialog_footer(true, vec![
                Button::new("access-create-cancel", ButtonVariant::Outline, ButtonSize::Sm)
                    .label(t(cx, "Cancel")).disabled(create.creating)
                    .on_click(cx.listener(|this, _, window, cx| { this.dismiss_access_dialog(window, cx); })).into_any_element(),
                Button::new("access-create-submit", ButtonVariant::Default, ButtonSize::Sm)
                    .label(t(cx, if create.creating { "Creating…" } else { "Create link" }))
                    .disabled(create.creating || create.scopes.is_empty())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.create_pairing_link(submit_client.clone(), submit_target.clone(), denied, cx);
                    })).into_any_element(),
            ], cx).into_any_element(),
            focus: create.focus.clone(),
        }
    }

    fn link_row(&self, link: &PairingLink, address: &str, now: i64, can_write: bool, target: &ResolvedAccessTarget, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let credential = self.access.credentials.get(&link.id).cloned();
        let sharing = self.access.sharing.as_deref() == Some(link.id.as_str()) && credential.is_some();
        let revoking = self.access.revoking.as_deref() == Some(link.id.as_str());
        let scopes = shown_scopes(&link.permissions, &link.scopes);
        let subline = expires_label(&link.expires_at, now);
        let scope_badge = access_scope_summary(SharedString::from(format!("link-scopes-{}", link.id)), scopes, cx);
        let share_id = link.id.clone();
        let revoke_id = link.id.clone();
        let revoke_client = target.client.clone();
        let revoke_target = target.saved_id.clone();
        let actions = h_flex()
            .flex_none()
            .gap(r(6.))
            .when(credential.is_some(), |this| {
                this.child(Button::new(SharedString::from(format!("access-share-{}", link.id)), ButtonVariant::Outline, ButtonSize::Compact).icon(Icon::QrCode).label("Share").on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.access.sharing = if this.access.sharing.as_deref() == Some(share_id.as_str()) { None } else { Some(share_id.clone()) };
                        cx.notify();
                    }),
                ))
            })
            .when(can_write, |this| {
                this.child(
                    Button::new(SharedString::from(format!("access-revoke-{}", link.id)), ButtonVariant::DestructiveOutline, ButtonSize::Compact)
                        .label(if revoking { "Revoking\u{2026}" } else { "Revoke" })
                        .disabled(revoking)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.revoke_link(revoke_client.clone(), revoke_target.clone(), revoke_id.clone(), cx);
                        })),
                )
            });
        let share = credential.filter(|_| sharing).map(|credential| self.share_panel(address, target, &credential, cx));
        v_flex()
            .px(r(16.))
            .py(r(12.))
            .gap(r(10.))
            .child(
                h_flex()
                    .gap(r(12.))
                    .child(StatusDot::new(SharedString::from(format!("pairing-link-{}", link.id)), px(8.), hsla(p.warning), false))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(r(2.))
                            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(link.title().to_owned()))
                            .child(h_flex().gap(r(6.)).child(muted_text(subline, cx)).child(scope_badge))
                            .when(!self.access.credentials.contains_key(&link.id), |this| {
                                this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Create a new link to share from this client."))
                            }),
                    )
                    .child(actions),
            )
            .children(share)
            .into_any_element()
    }

    fn cached_qr(&self, url: &str) -> Option<Rc<QrMatrix>> {
        let mut cache = self.access.qr.borrow_mut();
        if let Some((_, matrix)) = cache.as_ref().filter(|(key, _)| key == url) {
            return Some(matrix.clone());
        }
        let matrix = Rc::new(qr_matrix(url)?);
        *cache = Some((url.to_owned(), matrix.clone()));
        Some(matrix)
    }

    /// The inline share panel: the URL, copy buttons and its QR, or the code alone for a
    /// loopback server another device cannot reach.
    fn share_panel(&self, address: &str, target: &ResolvedAccessTarget, credential: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let endpoints = pairing_endpoints(target);
        let address = self.access.qr_endpoint.as_deref()
            .filter(|origin| endpoints.iter().any(|endpoint| endpoint == origin))
            .unwrap_or(address);
        let code = credential.to_owned();
        let copy_code = Button::new("access-copy-code", ButtonVariant::Outline, ButtonSize::Compact).icon(Icon::Copy).label("Copy code only").on_click(move |_, _, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(code.clone()));
        });
        let Some(url) = pairing_url(address, credential) else {
            return v_flex()
                .gap(r(8.))
                .pl(r(20.))
                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Copy the token and pair from another client using this backend's reachable host."))
                .child(h_flex().child(copy_code))
                .into_any_element();
        };
        let copy_url = url.clone();
        let picker = (endpoints.len() > 1).then(|| {
            let weak = cx.entity().downgrade();
            Select::new("access-qr-endpoint", endpoints.iter().map(|origin| SelectOption::new(origin.clone(), origin.clone())).collect())
                .selected(address.to_owned()).show_check().popup_width(320.)
                .on_change(move |next, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.access.qr_endpoint = Some(next.to_string());
                        cx.notify();
                    });
                })
        });
        h_flex()
            .id("access-qr-panel")
            .max_h(r(260.))
            .overflow_y_scroll()
            .pl(r(20.))
            .gap(r(16.))
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(r(8.))
                    .children(picker)
                    .child(
                        div()
                            .px(r(8.))
                            .py(r(6.))
                            .rounded(r(6.))
                            .bg(hsla(p.muted).opacity(0.5))
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(hsla(p.foreground))
                            .child(url.clone()),
                    )
                    .child(
                        h_flex()
                            .gap(r(6.))
                            .child(Button::new("access-copy-link", ButtonVariant::Outline, ButtonSize::Compact).icon(Icon::Copy).label("Copy link").on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_url.clone()));
                            }))
                            .child(copy_code),
                    ),
            )
            .children(self.cached_qr(&url).map(qr_element))
            .into_any_element()
    }

    fn client_row(&self, client: &ClientSession, can_write: bool, target: &ResolvedAccessTarget, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let revoking = self.access.revoking.as_deref() == Some(client.session_id.as_str());
        let scopes = shown_scopes(&client.permissions, &client.scopes);
        let subline = client.subline();
        let scope_badge = access_scope_summary(SharedString::from(format!("session-scopes-{}", client.session_id)), scopes, cx);
        let session_id = client.session_id.clone();
        let revoke_client = target.client.clone();
        let revoke_target = target.saved_id.clone();
        let confirm_cloud_session = client.subject == CLOUD_CONNECT_SESSION_SUBJECT
            && target.extras.require_connect_device_approval == Some(false)
            && can_write;
        let confirm_title = format!("Revoke {}?", client.title());
        h_flex()
            .px(r(16.))
            .py(r(12.))
            .gap(r(12.))
            .child(div().size(r(8.)).flex_none().rounded_full().bg(if client.online() { hsla(p.success) } else { hsla(p.muted_foreground).opacity(0.35) }))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(r(2.))
                    .child(
                        h_flex()
                            .gap(r(6.))
                            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(client.title()))
                            .when(client.current, |this| this.child(tone_badge("This device", Tone::Secondary, cx))),
                    )
                    .child(h_flex().gap(r(6.)).child(muted_text(subline, cx)).child(scope_badge)),
            )
            .when(can_write && !client.current, |this| {
                this.child(
                    Button::new(SharedString::from(format!("access-revoke-client-{}", client.session_id)), ButtonVariant::DestructiveOutline, ButtonSize::Compact)
                        .label(if revoking { "Revoking\u{2026}" } else { "Revoke" })
                        .disabled(revoking)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if confirm_cloud_session {
                                this.access.revoke_device_confirm = Some(RevokeConnectDeviceConfirm {
                                    action: PendingConnectRevokeAction::ClientSession(session_id.clone()),
                                    title: confirm_title.clone(),
                                    turn_on_approval: true,
                                    busy: false,
                                });
                                cx.notify();
                            } else {
                                this.revoke_client(revoke_client.clone(), revoke_target.clone(), session_id.clone(), cx);
                            }
                        })),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn authorized_clients_mcp_action_opens_the_existing_wizard(cx: &mut gpui::TestAppContext) {
        struct Preview {
            target: ResolvedAccessTarget,
            mcp: Entity<crate::mcp::McpDialogs>,
        }

        impl gpui::Render for Preview {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().debug_selector(|| "access-mcp-fixture".to_owned())
                    .child(use_in_another_app_action(&self.target, cx))
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let state = cx.new(|cx| crate::app_state::AppState::new(runtime.handle().clone(), cx));
            crate::settings::init(state.clone(), cx);
            state
        });
        let window = cx.open_window(gpui::size(px(600.), px(480.)), |window, cx| Preview {
            target: ResolvedAccessTarget {
                saved_id: "saved".into(),
                label: "Fixture".into(),
                address: "https://example.invalid".into(),
                is_primary: false,
                config: None,
                grants: None,
                client: None,
                extras: ServerConfigExtras::default(),
            },
            mcp: cx.new(|cx| crate::mcp::McpDialogs::new(state.clone(), window, cx)),
        });
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| { window.draw(cx).clear(); });
        let bounds = visual.debug_bounds("access-mcp-fixture").unwrap();
        let position = point(bounds.origin.x + px(12.), bounds.origin.y + px(12.));
        visual.simulate_mouse_move(position, None::<gpui::MouseButton>, Default::default());
        visual.simulate_click(position, Default::default());
        window.update(&mut visual, |view, _, cx| {
            assert!(view.mcp.read(cx).is_open(cx));
        }).unwrap();
    }

    #[test]
    fn regular_visible_links_excludes_connect_device_requests_and_prefers_tailscale_direct_endpoint() {
        let mut access = AccessState::default();
        let mut connect = ConnectDevicesState::default();
        let snap = json!({
            "type": "snapshot",
            "payload": {
                "pairingLinks": [
                    {
                        "id": "normal-link",
                        "label": "iPad",
                        "createdAt": "2026-11-18T00:00:00.000Z",
                        "expiresAt": "2099-01-01T00:00:00.000Z",
                        "scopes": ["orchestration:read"]
                    },
                    {
                        "id": "connect-req",
                        "createdAt": "2026-11-18T00:01:00.000Z",
                        "expiresAt": "2099-01-01T00:00:00.000Z",
                        "connectDeviceRequest": {
                            "verificationCode": "2345-6789",
                            "client": { "label": "iPhone" }
                        }
                    }
                ],
                "clientSessions": [],
                "connectDevices": []
            }
        });
        access.apply(&snap);
        connect.apply(&snap);
        let visible = regular_visible_links(&access, &connect, 0);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "normal-link");
        assert_eq!(connect.visible_pending_requests(0).len(), 1);

        let direct = vec!["https://studio.ts.net:3773".to_owned()];
        assert_eq!(effective_pairing_address("http://127.0.0.1:3773", &direct), "https://studio.ts.net:3773");
        assert_eq!(effective_pairing_address("https://box.example:3773", &direct), "https://box.example:3773");
    }

    #[test]
    fn connect_device_gating_formatting_and_revoke_detection_match_web() {
        assert!(!connect_approval_row_visible(false, Some(true)));
        assert!(!connect_approval_row_visible(true, None));
        assert!(connect_approval_row_visible(true, Some(false)));

        assert!(!connect_devices_section_visible(false, 2, 1));
        assert!(!connect_devices_section_visible(true, 0, 0));
        assert!(connect_devices_section_visible(true, 1, 0));
        assert!(connect_devices_section_visible(true, 0, 1));

        let now = t3_core::sidebar::time::parse_iso("2026-11-18T12:00:00.000Z").unwrap();
        assert_eq!(format_approved_since("2026-11-18T11:59:30.000Z", now), "just now");
        assert_eq!(format_approved_since("2026-11-18T11:45:00.000Z", now), "15m ago");
        assert_eq!(format_approved_since("2026-11-18T09:00:00.000Z", now), "3h ago");
        assert_eq!(format_approved_since("2026-11-15T12:00:00.000Z", now), "3d ago");
        assert_eq!(format_approved_since("2026-09-01T12:00:00.000Z", now), "2026-09-01");

        let normal_session: ClientSession = serde_json::from_value(json!({
            "sessionId": "s1",
            "subject": "pairing",
            "current": false
        }))
        .unwrap();
        let cloud_session: ClientSession = serde_json::from_value(json!({
            "sessionId": "s2",
            "subject": CLOUD_CONNECT_SESSION_SUBJECT,
            "current": false
        }))
        .unwrap();
        let current_cloud_session: ClientSession = serde_json::from_value(json!({
            "sessionId": "s3",
            "subject": CLOUD_CONNECT_SESSION_SUBJECT,
            "current": true
        }))
        .unwrap();
        assert!(!revokes_connect_devices(&[], &[normal_session.clone(), current_cloud_session]));
        assert!(revokes_connect_devices(&[], &[normal_session, cloud_session]));
    }

    #[cfg(feature = "snapshot")]
    struct AccessSettingsFrame;

    #[cfg(feature = "snapshot")]
    impl gpui::Render for AccessSettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
            div().size_full().children(crate::settings::overlay(window, cx))
        }
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn link_machine_form_tracks_peer_links_input_changes_and_submission(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
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
                state.environments.environments.push(t3_client::store::SavedEnvironment {
                    id: "host-mac".into(),
                    environment_id: Some("mac-env".into()),
                    label: "Mac".into(),
                    http_base: "http://127.0.0.1:3773".into(),
                    access_token: Some("tok-host".into()),
                    local: true,
                    disabled: false,
                    ssh: None,
                    server_version: None,
                });
                state.environments.environments.push(t3_client::store::SavedEnvironment {
                    id: "peer-vps".into(),
                    environment_id: Some("vps-1".into()),
                    label: "Build VPS".into(),
                    http_base: "https://vps.example.ts.net".into(),
                    access_token: Some("tok-peer".into()),
                    local: false,
                    disabled: false,
                    ssh: None,
                    server_version: None,
                });
                state.environments.active = Some("host-mac".into());
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": { "environmentId": "mac-env", "label": "Mac" },
                        "settings": { "threads": { "linkedEnvironmentsEnabled": true } }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
            crate::settings::open(super::super::Section::Access, cx);
        });
        let window = cx.add_window(|_, _| AccessSettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let view = vcx.read(|cx| cx.global::<super::super::Host>().view.clone().expect("settings view"));

        // 1. Opening `LinkMachineForm` queries `peerLinks.list` and attaches `InputEvent::Change` observers.
        let notify_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let notify_counter = notify_count.clone();
        let _observer = vcx.update(|_, cx| {
            cx.observe(&view, move |_, _| {
                notify_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let target = view.resolve_access_target(cx).1.expect("target");
                view.open_link_machine_form(target.saved_id, target.client, window, cx);
            });
        });
        let (method, _, reply) = loop {
            let req = fixture.next_request().await;
            if req.0 == "peerLinks.list" {
                break req;
            }
            let _ = req.2.send(Ok(json!({})));
        };
        assert_eq!(method, "peerLinks.list");
        let (linked_tx, linked_rx) = tokio::sync::oneshot::channel::<()>();
        let mut linked_tx = Some(linked_tx);
        let _linked_obs = vcx.update(|_, cx| {
            cx.observe(&view, move |view, cx| {
                if view.read(cx).access.linked_peer_ids.contains("vps-1")
                    && let Some(tx) = linked_tx.take()
                {
                    let _ = tx.send(());
                }
            })
        });
        reply
            .send(Ok(json!({
                "links": [{ "environmentId": "vps-1", "label": "Build VPS" }]
            })))
            .unwrap();
        linked_rx.await.unwrap();
        drop(_linked_obs);
        vcx.run_until_parked();

        // `access_linkable_machines` marks `vps-1` as `linked: true` from `peerLinks.list`.
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let target = view.resolve_access_target(cx).1.expect("target");
                let machines = view.access_linkable_machines(&target, cx);
                let vps = machines.iter().find(|m| m.environment_id == "vps-1").expect("vps machine");
                assert!(vps.linked, "already-linked peer must report linked: true");
            });
        });

        // 2. Typing into `address` and `pairing_code` fires `InputEvent::Change` -> `cx.notify()`.
        let (address_input, code_input) = vcx.update(|_, cx| {
            view.update(cx, |view, _| {
                let form = view.access.link_machine.as_mut().expect("form open");
                form.choice = Some(ANOTHER_ADDRESS.to_owned());
                (form.address.clone(), form.pairing_code.clone())
            })
        });
        let before_type = notify_count.load(std::sync::atomic::Ordering::SeqCst);
        vcx.update(|window, cx| {
            address_input.update(cx, |input, cx| input.insert("https://other.example:3773", window, cx));
            code_input.update(cx, |input, cx| input.insert("code-1234", window, cx));
        });
        vcx.run_until_parked();
        assert!(
            notify_count.load(std::sync::atomic::Ordering::SeqCst) > before_type,
            "typing into address/pairing_code must notify SettingsView"
        );

        // 3. Submitting `"Another address…"` sends `peerLinks.link` and closes the form on success.
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let target = view.resolve_access_target(cx).1.expect("target");
                view.submit_access_link_machine(&target, cx);
            });
        });
        let (link_method, payload, reply) = loop {
            let req = fixture.next_request().await;
            if req.0 == "peerLinks.link" {
                break req;
            }
            let _ = req.2.send(Ok(json!({})));
        };
        assert_eq!(link_method, "peerLinks.link");
        assert_eq!(payload["url"], "https://other.example:3773");
        assert_eq!(payload["pairingCode"], "code-1234");
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();
        let mut closed_tx = Some(closed_tx);
        let _closed_obs = vcx.update(|_, cx| {
            cx.observe(&view, move |view, cx| {
                if view.read(cx).access.link_machine.is_none()
                    && let Some(tx) = closed_tx.take()
                {
                    let _ = tx.send(());
                }
            })
        });
        reply.send(Ok(json!({ "link": { "environmentId": "other-env" } }))).unwrap();
        closed_rx.await.unwrap();
        drop(_closed_obs);
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let view_ref = view.read(cx);
            assert!(view_ref.access.link_machine.is_none(), "successful link closes the form");
            assert!(view_ref.access.linked_peer_ids.contains("other-env"));
        });

        // 4. `mint_pairing_code_on` returns `Unavailable` for an unconnected peer environment.
        let mint_fut = vcx.read(|cx| state.read(cx).mint_pairing_code_on("vps-1", "approval-required"));
        let mint_err = runtime.block_on(mint_fut).expect_err("disconnected peer cannot auto-mint");
        assert_eq!(mint_err.reason, t3_core::pairing::PeerPairingCodeErrorReason::Unavailable);

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(_observer);
        drop(vcx);
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[test]
    fn effective_pairing_address_skips_loopback_and_t3code_scheme() {
        let endpoints = vec![
            "t3code://app/settings".to_owned(),
            "http://127.0.0.1:3773".to_owned(),
            "https://studio.tail.ts.net:3773".to_owned(),
        ];
        assert_eq!(
            effective_pairing_address("t3code://app/settings/connections", &endpoints),
            "https://studio.tail.ts.net:3773"
        );
        assert_eq!(
            effective_pairing_address("http://127.0.0.1:3773", &endpoints),
            "https://studio.tail.ts.net:3773"
        );
        assert_eq!(pairing_url("t3code://app/settings/connections", "CODE1"), None);
    }
}
