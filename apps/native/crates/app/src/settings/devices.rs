//! Devices: `DevicesSettingsPanel` (`DeviceIntegrationControls` in `IntegrationsSettings.tsx`),
//! `DeviceToolVersions`, `DeviceSourcesSettings`, `DeviceHostEditor` (SSH),
//! `EasDeviceHostEditor` (`#220` secret-provider token editor over `t3_core::device::hosts::EasTokenEditor`),
//! and `SimlockDeviceHostEditor`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, SharedString,
    RenderOnce, StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::{Value, json};
use t3_core::device::hosts::{
    self, EasAccount, EasHostConfig, EasTokenEditor, SecretValue, SimlockHostConfig, SimlockIdentity, SimlockTokenEditor, SshHostConfig,
    decode_eas_hosts, decode_simlock_hosts, decode_ssh_hosts, eas_hosts_patch, simlock_hosts_patch, ssh_hosts_patch, update_eas_hosts,
    update_simlock_hosts, update_ssh_hosts,
};
use t3_core::device::{
    Host, HostKind, HostStatus, LOCAL_HOST_ID, Platform, PlatformAvailability, SETTINGS_WRITE_SCOPE, ServiceState, ToolVersion, configure_hub_payload,
    inspect_only_payload, method as device_method, retry_host_payload, update_tool_payload,
};
use t3_core::server_config::ServerConfig;
use t3_core::sidebar::grouping::EnvironmentOwner;
use t3_client::session::RpcError;
use t3_core::settings::ServerSettingChange;
use t3_core::settings::search::Section;

use super::controls::{Button, ButtonSize, ButtonVariant, DialogFrame, Row, Select, SelectOption, Switch, group, r, section, section_title};
use super::source_control::{Tone, tone_badge};
use super::{Block, SettingsView};
use crate::composer::controls::floating_shadow;
use crate::device::{DevicesEvent, HostRetry, host_updates, setup_status};
use crate::components::secret_source::{SecretSourceCheck, SecretSourceInput};
use crate::i18n::t;
use crate::icons::Icon;
use crate::sidebar::controller::uuid;
use crate::theme::{ThemeSlot, ThemeSlotExt, hsla, palette, theme_slot_clipped_background_layer};
use crate::toast::{Toast, ToastKind};

const DEVICE_READ_SCOPE: &str = "orchestration:read";
const STORED_SECRET: &str = "__stored__";
const AUTO_EAS_APP: &str = "__auto__";
const ADD_MENU_REOPEN_GUARD: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Hub,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPopover {
    Hub,
    Agent,
}

#[derive(Clone)]
enum SshCheckState {
    Pending,
    Local,
    Connected(Vec<Platform>),
    Failed(String),
}

struct SshEditor {
    is_new: bool,
    host_id: String,
    label: Entity<InputState>,
    target: Entity<InputState>,
    identity_file: Entity<InputState>,
    port: Entity<InputState>,
    options_open: bool,
    check: Option<SshCheckState>,
    check_seq: u64,
    saving: bool,
    _subscriptions: Vec<Subscription>,
}

struct EasEditor {
    is_new: bool,
    host_id: String,
    token_editor: EasTokenEditor,
    source: String,
    app_id: Option<String>,
    label: Entity<InputState>,
    token: Entity<InputState>,
    max_sessions: Entity<InputState>,
    max_minutes: Entity<InputState>,
    idle_minutes: Entity<InputState>,
    checking: bool,
    account: Option<Result<EasAccount, String>>,
    check_seq: u64,
    saving: bool,
    _subscriptions: Vec<Subscription>,
}

struct SimlockEditor {
    is_new: bool,
    host_id: String,
    token_editor: SimlockTokenEditor,
    source: String,
    label: Entity<InputState>,
    gateway_url: Entity<InputState>,
    token: Entity<InputState>,
    max_leases: Entity<InputState>,
    max_lease_minutes: Entity<InputState>,
    checking: bool,
    identity: Option<Result<SimlockIdentity, String>>,
    check_seq: u64,
    saving: bool,
    _subscriptions: Vec<Subscription>,
}

enum HostEditor {
    Ssh(SshEditor),
    Eas(EasEditor),
    Simlock(SimlockEditor),
}

struct DeviceFeed {
    owner: EnvironmentOwner,
    driver: tokio::task::JoinHandle<()>,
    _reader: Task<()>,
    service: Option<ServiceState>,
}

impl Drop for DeviceFeed {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

#[derive(IntoElement)]
struct HostEditorFrame {
    view: WeakEntity<SettingsView>,
    frame: DialogFrame,
}

impl RenderOnce for HostEditorFrame {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.frame.render_modal(window, 1, move |window, cx| {
            let _ = self.view.update(cx, |view, cx| view.dismiss_device_editor(window, cx));
        }, cx)
    }
}

pub(crate) struct DevicesState {
    pending: Option<Pending>,
    /// The host whose Retry runs.
    retrying: Option<String>,
    tool_popover: Option<ToolPopover>,
    tool_busy: Option<&'static str>,
    tool_error: Option<String>,
    add_menu_open: bool,
    add_menu_dismissed: Option<Instant>,
    row_checks: HashMap<String, SshCheckState>,
    editor: Option<HostEditor>,
    _subscription: Option<Subscription>,
    _lifecycle: Vec<Subscription>,
    editor_focus: Option<FocusHandle>,
    editor_target: Option<EnvironmentOwner>,
    selected_environment: Option<String>,
    previous_focus: Option<FocusHandle>,
    action_menu: Option<String>,
    platform_menu: Option<(String, Platform)>,
    feeds: HashMap<String, DeviceFeed>,
    check_revision: u64,
}

impl DevicesState {
    pub(super) fn new(cx: &mut Context<SettingsView>) -> Self {
        let subscription = crate::device::devices(cx).map(|devices| {
            cx.subscribe(&devices, |view: &mut SettingsView, _, _: &DevicesEvent, cx| {
                if view.section == Section::Devices {
                    cx.notify();
                }
            })
        });
        let cleanup = cx.observe_self(|view, _| {
            if view.section != Section::Devices {
                view.devices.feeds.clear();
                view.devices.editor = None;
                view.devices.editor_target = None;
                view.devices.platform_menu = None;
                view.devices.action_menu = None;
            }
        });
        let view = cx.entity().downgrade();
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            if event.keystroke.key != "escape" || crate::dialog::is_open(cx) {
                return;
            }
            let _ = view.update(cx, |view, cx| {
                if view.section == Section::Devices && (view.devices.editor.is_some()
                    || view.devices.platform_menu.is_some() || view.devices.action_menu.is_some()
                    || view.devices.add_menu_open)
                {
                    cx.stop_propagation();
                    view.devices.platform_menu = None;
                    view.devices.action_menu = None;
                    view.devices.add_menu_open = false;
                    view.dismiss_device_editor(window, cx);
                }
            });
        });
        DevicesState {
            pending: None,
            retrying: None,
            tool_popover: None,
            tool_busy: None,
            tool_error: None,
            add_menu_open: false,
            add_menu_dismissed: None,
            row_checks: HashMap::new(),
            editor: None,
            _subscription: subscription,
            _lifecycle: vec![cleanup, escape],
            editor_focus: None,
            editor_target: None,
            selected_environment: None,
            previous_focus: None,
            action_menu: None,
            platform_menu: None,
            feeds: HashMap::new(),
            check_revision: 0,
        }
    }

    /// Toggles the `Add source` menu, suppressing the GPUI mouse-down-outside -> mouse-up-click
    /// reopen race when clicking the trigger button while the menu is already open.
    fn toggle_add_menu(&mut self, now: Instant) {
        if self.add_menu_open {
            self.add_menu_open = false;
            self.add_menu_dismissed = None;
            return;
        }
        if self
            .add_menu_dismissed
            .is_some_and(|at| now.saturating_duration_since(at) < ADD_MENU_REOPEN_GUARD)
        {
            self.add_menu_dismissed = None;
            return;
        }
        self.add_menu_open = true;
    }

    fn dismiss_add_menu_outside(&mut self, now: Instant) {
        self.add_menu_open = false;
        self.add_menu_dismissed = Some(now);
    }
}

impl HostRetry for SettingsView {
    fn set_retrying(&mut self, host: Option<String>) {
        self.devices.retrying = host;
    }
}

fn stop_slash_shortcut(event: &KeyDownEvent, _: &mut Window, cx: &mut App) {
    if event.keystroke.key == "/" && !event.keystroke.modifiers.modified() {
        cx.stop_propagation();
    }
}

fn new_host_id(prefix: &str) -> String {
    let raw = uuid();
    let suffix: String = raw.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
    format!("{prefix}-{suffix}")
}

fn status_badge(status: HostStatus, cx: &App) -> Option<AnyElement> {
    match status {
        HostStatus::Ready => Some(tone_badge("Connected", Tone::Success, cx).into_any_element()),
        HostStatus::Installing => Some(tone_badge("Installing", Tone::Info, cx).into_any_element()),
        HostStatus::Starting => Some(tone_badge("Starting", Tone::Info, cx).into_any_element()),
        HostStatus::Failed => Some(tone_badge("Failed", Tone::Error, cx).into_any_element()),
        HostStatus::Idle => Some(tone_badge("Idle", Tone::Secondary, cx).into_any_element()),
        HostStatus::Disabled | HostStatus::Unknown => None,
    }
}

fn platform_availability(host: Option<&Host>, platform: Platform) -> Option<&PlatformAvailability> {
    host?.platforms.iter().find(|entry| entry.platform == platform)
}

fn tool_trigger_label(tool: Option<&ToolVersion>, error: Option<&str>) -> String {
    match tool {
        Some(t) => match t.display_version() {
            Some(version) => format!("v{version}"),
            None => "Not installed".to_owned(),
        },
        None if error.is_some() => "Versions unavailable".to_owned(),
        None => "Version unknown".to_owned(),
    }
}

impl SettingsView {
    fn device_owner(&self, cx: &App) -> Option<EnvironmentOwner> {
        if let Some(owner) = &self.devices.editor_target {
            return Some(owner.clone());
        }
        if let Some(selected) = &self.devices.selected_environment
            && !self.is_project_scope(cx)
            && let Some(owner) = self.scoped_environment_owners(cx).into_iter().find(|owner| &owner.saved_id == selected)
        {
            return Some(owner);
        }
        self.scoped_primary_owner(cx)
    }

    fn device_config<'a>(&self, cx: &'a App) -> Option<&'a ServerConfig> {
        let owner = self.device_owner(cx)?;
        self.state.read(cx).fleet.environment(&owner.saved_id)?.config.config.as_ref()
    }

    fn save_device_settings(
        &self, change: ServerSettingChange,
        done: impl FnOnce(Option<String>, &mut App) + 'static,
        cx: &mut App,
    ) {
        if let Some(owner) = self.device_owner(cx) {
            super::store::update_scoped_environments_then(&self.state, vec![owner], change, done, cx);
        }
    }

    fn device_environment_picker(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.is_project_scope(cx) {
            return None;
        }
        let owners = self.scoped_environment_owners(cx);
        if owners.len() < 2 {
            return None;
        }
        let app = self.state.read(cx);
        let options = owners.iter().filter_map(|owner| {
            let label = &app.fleet.environment(&owner.saved_id)?.config.config.as_ref()?.environment.label;
            Some(SelectOption::new(owner.saved_id.clone(), label.clone()))
        }).collect();
        let selected = self.device_owner(cx)?.saved_id;
        let view = cx.entity().downgrade();
        Some(Row::new("device-settings-environment", "Environment").control(
            Select::new("device-settings-environment-select", options).selected(selected).show_check()
                .on_change(move |id, _, cx| {
                    let _ = view.update(cx, |view, cx| {
                        view.devices.selected_environment = Some(id.to_string());
                        view.devices.row_checks.clear();
                        view.devices.tool_popover = None;
                        view.devices.tool_error = None;
                        view.devices.platform_menu = None;
                        view.devices.action_menu = None;
                        cx.notify();
                    });
                }),
        ).into_any_element())
    }

    fn scoped_device_request(
        &self, scope: &'static str, method: &'static str, payload: Value, cx: &App,
    ) -> impl std::future::Future<Output = Result<Value, RpcError>> + use<> {
        let owner = self.device_owner(cx);
        let app = self.state.read(cx);
        let lookup = owner.map(|owner| app.connections.destination(&owner, scope));
        let runtime = app.runtime().clone();
        let fleet = app.connections.view();
        async move {
            let target = lookup.ok_or(RpcError::Closed)?.await.map_err(|_| RpcError::Closed)?;
            if !target.is_current(&fleet.borrow()) {
                return Err(RpcError::Closed);
            }
            runtime.spawn(async move { target.client.request(method, payload).await })
                .await.unwrap_or(Err(RpcError::Closed))
        }
    }

    fn ensure_device_feeds(&mut self, cx: &mut Context<Self>) {
        let owners = self.scoped_environment_owners(cx);
        let app = self.state.read(cx);
        let active = app.environments.active.as_deref();
        self.devices.feeds.retain(|id, feed| active != Some(id.as_str()) && owners.contains(&feed.owner));
        for owner in owners {
            if active == Some(owner.saved_id.as_str()) || self.devices.feeds.contains_key(&owner.saved_id)
                || !app.fleet.environment(&owner.saved_id).is_some_and(|entry| {
                    entry.summary.synchronized && entry.grants.as_ref().is_some_and(|grants| grants.grants(DEVICE_READ_SCOPE))
                })
            {
                continue;
            }
            let target = app.connections.destination(&owner, DEVICE_READ_SCOPE);
            let (tx, mut rx) = tokio::sync::watch::channel(None);
            let driver = app.runtime().spawn(async move {
                let Ok(target) = target.await else { return };
                let Ok(mut stream) = target.client.stream(device_method::SUBSCRIBE_STATE, json!({})).await else { return };
                while let Some(Ok(value)) = stream.receiver.recv().await {
                    if let Ok(service) = ServiceState::decode(value)
                        && tx.send(Some(service)).is_err()
                    {
                        return;
                    }
                }
                let _ = tx.send(None);
            });
            let id = owner.saved_id.clone();
            let reader_id = id.clone();
            let reader = cx.spawn(async move |view, cx| {
                while rx.changed().await.is_ok() {
                    let service = rx.borrow_and_update().clone();
                    if view.update(cx, |view, cx| {
                        if let Some(feed) = view.devices.feeds.get_mut(&reader_id) {
                            feed.service = service;
                            cx.notify();
                        }
                    }).is_err() {
                        break;
                    }
                }
            });
            self.devices.feeds.insert(id, DeviceFeed { owner, driver, _reader: reader, service: None });
        }
    }

    fn device_editor_writable(&self, cx: &App) -> bool {
        !self.is_project_scope(cx) && self.device_owner(cx).is_some_and(|owner| {
            self.state.read(cx).fleet.environment(&owner.saved_id).is_some_and(|entry| {
                entry.summary.owner == owner
                    && entry.status == t3_client::supervisor::ConnectionStatus::Connected
                    && entry.grants.as_ref().is_some_and(|grants| grants.grants(SETTINGS_WRITE_SCOPE))
            })
        })
    }

    fn capture_device_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.device_editor_writable(cx) {
            return false;
        }
        self.devices.platform_menu = None;
        self.devices.action_menu = None;
        self.devices.add_menu_open = false;
        self.devices.editor_target = self.device_owner(cx);
        self.devices.previous_focus = window.focused(cx);
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.devices.editor_focus = Some(focus);
        true
    }

    fn dismiss_device_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.devices.editor.as_ref().is_some_and(|editor| match editor {
            HostEditor::Ssh(ed) => ed.saving,
            HostEditor::Eas(ed) => ed.saving,
            HostEditor::Simlock(ed) => ed.saving,
        }) {
            return;
        }
        self.devices.editor = None;
        self.devices.editor_target = None;
        self.devices.editor_focus = None;
        if let Some(focus) = self.devices.previous_focus.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn device_platforms(&self, host: &str, cx: &Context<Self>) -> AnyElement {
        let app = self.state.read(cx);
        let active = app.environments.active.as_deref();
        let device_controller = crate::device::devices(cx);
        let active_service = device_controller.as_ref().and_then(|controller| controller.read(cx).service());
        let environments: Vec<_> = self.scoped_environment_owners(cx).into_iter().filter_map(|owner| {
            let entry = app.fleet.environment(&owner.saved_id)?;
            let config = entry.config.config.as_ref()?;
            let service = if active == Some(owner.saved_id.as_str()) {
                active_service
            } else {
                self.devices.feeds.get(&owner.saved_id).and_then(|feed| feed.service.as_ref())
            };
            if service.is_some_and(|service| !service.hub_enabled()) {
                return None;
            }
            Some((&config.environment.label, service.and_then(|service| service.host(host))))
        }).collect();
        let p = palette(cx);
        h_flex().gap(r(3.)).children([(Platform::Ios, Icon::Apple), (Platform::Android, Icon::Android)]
            .into_iter().filter_map(|(platform, icon)| {
                if !environments.iter().any(|(_, host)| platform_availability(*host, platform).is_some()) {
                    return None;
                }
                let is_open = self.devices.platform_menu.as_ref().is_some_and(|(id, selected)| id == host && *selected == platform);
                let key = host.to_owned();
                let any_available = environments.iter().any(|(_, host)| platform_availability(*host, platform).is_some_and(|entry| entry.available));
                let trigger = Button::new((ElementId::Name(key.clone().into()), platform.as_str()), ButtonVariant::GhostMuted, ButtonSize::IconXs)
                    .icon(icon)
                    .icon_color(hsla(if any_available { p.foreground } else { p.muted_foreground }))
                    .tooltip(platform.name())
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.devices.platform_menu = (!is_open).then(|| (key.clone(), platform));
                        cx.notify();
                    }));
                let popup = is_open.then(|| {
                    let rows: Vec<_> = environments.iter().map(|(label, host)| {
                        let status = match platform_availability(*host, platform) {
                            Some(entry) if entry.available => t(cx, "Available"),
                            Some(entry) => entry.reason.as_deref().unwrap_or(t(cx, "Not available")),
                            None => t(cx, "Status unavailable"),
                        };
                        v_flex().gap(r(2.)).py(r(4.))
                            .child(div().text_sm().child((*label).clone()))
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(status.to_owned()))
                    }).collect();
                    self.device_popup(rows.into_iter().map(IntoElement::into_any_element).collect(), cx)
                });
                Some(div().relative().child(trigger).children(popup))
            })).into_any_element()
    }

    fn device_popup(&self, items: Vec<AnyElement>, cx: &Context<Self>) -> AnyElement {
        let p = palette(cx);
        div().absolute().bottom_0().right_0().child(
            gpui::deferred(gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
                v_flex().id("device-sources-menu").relative().occlude().mt(r(4.)).min_w(r(180.)).max_w(r(280.))
                    .p(r(6.)).rounded(r(8.)).border_1().border_color(hsla(p.dropdown_border)).bg(hsla(p.popover))
                    .theme_slot(ThemeSlot::MenuPopup, cx)
                    .children(theme_slot_clipped_background_layer(ThemeSlot::MenuPopup, r(8.), cx))
                    .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                        view.devices.platform_menu = None;
                        view.devices.action_menu = None;
                        cx.notify();
                    }))
                    .children(items),
            )).with_priority(2),
        ).into_any_element()
    }

    fn device_source_actions(&self, key: &str, can_write: bool, actions: Vec<AnyElement>, cx: &Context<Self>) -> AnyElement {
        let is_open = self.devices.action_menu.as_deref() == Some(key);
        let key = key.to_owned();
        let trigger = Button::new((ElementId::Name(key.clone().into()), "device-source-actions"), ButtonVariant::GhostMuted, ButtonSize::IconXs)
            .icon(Icon::Ellipsis)
            .tooltip("More")
            .disabled(!can_write)
            .on_click(cx.listener(move |view, _, _, cx| {
                view.devices.action_menu = (!is_open).then(|| key.clone());
                cx.notify();
            }));
        div().relative().child(trigger).children(is_open.then(|| self.device_popup(actions, cx))).into_any_element()
    }

    pub(super) fn devices_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let Some(devices) = crate::device::devices(cx) else { return Vec::new() };
        self.ensure_device_feeds(cx);
        let active = self.state.read(cx).environments.active.as_deref();
        let owner = self.device_owner(cx);
        let active_target = owner.as_ref().is_some_and(|owner| active == Some(owner.saved_id.as_str()));
        let service = if active_target {
            devices.read(cx).service().cloned()
        } else {
            owner.as_ref().and_then(|owner| self.devices.feeds.get(&owner.saved_id))
                .and_then(|feed| feed.service.clone())
        };
        let can_configure = self.device_editor_writable(cx);
        let loaded = service.is_some();
        let service = service.unwrap_or_else(ServiceState::empty);
        let config = self.device_config(cx).cloned();
        let is_project_scope = self.is_project_scope(cx);
        let env_only = if is_project_scope {
            Some("Environment-wide setting. Select an environment to change it.".to_owned())
        } else {
            (!can_configure).then(|| "Reconnect with settings write access to change device settings.".to_owned())
        };
        let scope_project = is_project_scope.then(|| self.scope_project_id(cx)).flatten();
        let agent_access_effective = config
            .as_ref()
            .map(|c| {
                t3_core::settings::effective_agent_device_access(
                    &c.settings,
                    scope_project.as_deref(),
                    service.agent_access_enabled,
                )
            })
            .unwrap_or(service.agent_access_enabled);
        let pending = self.devices.pending;
        let busy = service.hub_readiness().busy;
        let enabled = service.hub_enabled();
        let local_host = service.host(LOCAL_HOST_ID);
        let local_tools = local_host.and_then(|h| h.tools.as_ref());
        let local_tool_err = local_host.and_then(|h| h.tool_inspection_error.as_deref());

        let policy = config.as_ref().and_then(|config| config.managed_policy.as_ref());
        let hub_unavail = t3_core::managed_policy::setting_lock_reason(policy, "enableDeviceSupport").or(env_only);
        let agent_lock = if is_project_scope {
            self.managed_setting_lock(&["enableAgentDeviceAccess"], cx)
        } else {
            t3_core::managed_policy::setting_lock_reason(policy, "enableAgentDeviceAccess")
        };
        let view = cx.entity().downgrade();
        let hub = Switch::new("device-hub-switch", enabled).disabled(hub_unavail.is_some() || !can_configure || !loaded || busy || pending.is_some()).on_change({
            let view = view.clone();
            move |on, _, cx| {
                let input = if on { json!({ "enabled": true, "onboardingCompleted": true }) } else { configure_hub_payload(false) };
                let _ = view.update(cx, |view, cx| view.configure_devices(Pending::Hub, input, cx));
            }
        });
        let agent = Switch::new("agent-device-access-switch", agent_access_effective)
            .disabled(
                agent_lock.is_some()
                    || if is_project_scope {
                        !self.scoped_session(cx).is_some_and(|session| session.grants(SETTINGS_WRITE_SCOPE)) || config.is_none()
                    } else {
                        !can_configure || !loaded || !enabled || busy || pending.is_some()
                    },
            )
            .on_change(move |on, _, cx| {
                let _ = view.update(cx, |view, cx| {
                    if view.is_project_scope(cx) {
                        view.update_scoped_server(
                            ServerSettingChange::Prepared(json!({ "patch": { "enableAgentDeviceAccess": on } })),
                            cx,
                        );
                    } else {
                        view.configure_devices(Pending::Agent, json!({ "agentAccessEnabled": on }), cx);
                    }
                });
            });

        let hub_status = setup_status((pending == Some(Pending::Hub)).then(|| service.hub_setup_status(true)).flatten(), cx);
        let agent_status = setup_status((pending == Some(Pending::Agent)).then(|| service.agent_setup_status(true)).flatten(), cx);
        let local = ServiceState { hosts: service.hosts.iter().filter(|host| host.kind == HostKind::Local).cloned().collect(), ..service.clone() };
        let updates = active_target.then(|| host_updates(&local, &devices, self.devices.retrying.as_deref(), cx.entity().downgrade(), cx)).flatten();

        let hub_tool_trigger = (loaded && (local_tools.is_some() || service.supports_tool_inspection)).then(|| {
            let label = tool_trigger_label(local_tools.map(|t| &t.hub), local_tool_err);
            Button::new("device-hub-version-trigger", ButtonVariant::GhostMuted, ButtonSize::Compact)
                .label(label)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.devices.tool_popover = if this.devices.tool_popover == Some(ToolPopover::Hub) { None } else { Some(ToolPopover::Hub) };
                    cx.notify();
                }))
        });
        let agent_tool_trigger = (loaded && (local_tools.is_some() || service.supports_tool_inspection)).then(|| {
            let label = tool_trigger_label(local_tools.map(|t| &t.agent), local_tool_err);
            Button::new("agent-device-version-trigger", ButtonVariant::GhostMuted, ButtonSize::Compact)
                .label(label)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.devices.tool_popover = if this.devices.tool_popover == Some(ToolPopover::Agent) { None } else { Some(ToolPopover::Agent) };
                    cx.notify();
                }))
        });

        let hub_row = Row::new("device-hub", "Use devices")
            .description("Open simulators and emulators in the Device panel.")
            .highlighted(self.is_target("device-hub"))
            .unavailable(hub_unavail)
            .control(h_flex().gap(r(8.)).children(hub_tool_trigger).children(hub_status).child(hub))
            .when(self.devices.tool_popover == Some(ToolPopover::Hub), |row| {
                row.below(self.tool_version_details(ToolPopover::Hub, local_tools.map(|t| &t.hub), local_tool_err, &service, can_configure, cx))
            });
        let agent_reset_view = cx.entity().downgrade();
        let agent_row = Row::new("agent-device-access", "Agents can use devices")
            .description("New agent sessions can open and drive these devices. Projects can override it.")
            .highlighted(self.is_target("agent-device-access"))
            .unavailable(agent_lock.clone())
            .reset_tooltip("Reset to inherited value")
            .reset(agent_lock.is_none() && is_project_scope && self.has_project_override(&["enableAgentDeviceAccess"], cx), move |_, _, cx| {
                let _ = agent_reset_view.update(cx, |this, cx| {
                    this.clear_project_overrides(&["enableAgentDeviceAccess"], cx);
                });
            })
            .control(h_flex().gap(r(8.)).children(agent_tool_trigger).children(agent_status).child(agent))
            .when(self.devices.tool_popover == Some(ToolPopover::Agent), |row| {
                row.below(self.tool_version_details(ToolPopover::Agent, local_tools.map(|t| &t.agent), local_tool_err, &service, can_configure, cx))
            });

        let mut rows = Vec::new();
        rows.extend(self.device_environment_picker(cx));
        rows.extend([hub_row.into_any_element(), agent_row.into_any_element()]);
        rows.extend(updates.map(|updates| div().px(r(16.)).py(r(12.)).child(updates).into_any_element()));

        let mut blocks = vec![Block::new(&["devices", "device-hub", "agent-device-access"], section("Devices", rows, cx))];
        if let Some(config) = config.as_ref() {
            let sources = self.device_sources_section(&service, config, can_configure, cx);
            let editor = self.host_editor_panel(config, cx);
            blocks.push(Block::new(&["device-sources"], v_flex().child(sources).children(editor)));
        }
        blocks
    }

    fn tool_version_details(
        &self,
        kind: ToolPopover,
        tool: Option<&ToolVersion>,
        inspection_error: Option<&str>,
        service: &ServiceState,
        can_configure: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let (title, rpc_key) = match kind {
            ToolPopover::Hub => ("Device hub", "hub"),
            ToolPopover::Agent => ("Agent device", "agent"),
        };
        let busy = self.devices.tool_busy.is_some();
        let mut card = v_flex()
            .mt(r(8.))
            .mb(r(12.))
            .p(r(12.))
            .gap(r(8.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.7))
            .bg(hsla(p.card).opacity(0.5))
            .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title));

        if let Some(tool) = tool {
            let installed = if tool.installed_versions.is_empty() {
                "None".to_owned()
            } else {
                tool.installed_versions.join(", ")
            };
            let detail_row = |label: &'static str, value: String| {
                h_flex()
                    .justify_between()
                    .text_xs()
                    .child(div().text_color(hsla(p.muted_foreground)).child(label))
                    .child(div().font_family(mono.clone()).text_color(hsla(p.foreground)).child(value))
            };
            card = card
                .child(detail_row("Running", tool.running_version.clone().unwrap_or_else(|| "Not running".to_owned())))
                .child(detail_row("Required", tool.required_version.clone()))
                .child(detail_row("Installed", installed));
        } else {
            card = card.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Versions have not been checked."));
        }

        card = card.child(
            div()
                .text_size(r(11.))
                .text_color(hsla(p.muted_foreground))
                .child("Tools update automatically on this host when needed."),
        );

        if let Some(err) = self.devices.tool_error.as_deref().or(inspection_error) {
            card = card.child(div().text_xs().text_color(hsla(p.error_foreground)).child(err.to_owned()));
        }

        let mut actions = h_flex().gap(r(8.));
        if service.supports_tool_update && tool.is_some_and(ToolVersion::needs_update) && can_configure {
            actions = actions.child(
                Button::new(SharedString::from(format!("device-tool-update-{rpc_key}")), ButtonVariant::Default, ButtonSize::Compact)
                    .label(if self.devices.tool_busy == Some(rpc_key) { "Updating…" } else { "Update" })
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.run_device_list_action(rpc_key, update_tool_payload(rpc_key), cx))),
            );
        }
        if service.supports_tool_inspection {
            actions = actions.child(
                Button::new(SharedString::from(format!("device-tool-inspect-{rpc_key}")), ButtonVariant::Outline, ButtonSize::Compact)
                    .label(if self.devices.tool_busy == Some("inspect") { "Checking…" } else { "Check versions" })
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.run_device_list_action("inspect", inspect_only_payload(), cx))),
            );
        }
        card.child(actions).into_any_element()
    }

    fn run_device_list_action(&mut self, tag: &'static str, payload: Value, cx: &mut Context<Self>) {
        if self.devices.tool_busy.is_some() {
            return;
        }
        self.devices.tool_busy = Some(tag);
        self.devices.tool_error = None;
        cx.notify();
        let rpc = self.scoped_device_request(DEVICE_READ_SCOPE, device_method::LIST, payload, cx);
        cx.spawn(async move |this, cx| {
            let result = rpc.await;
            let _ = this.update(cx, |this, cx| {
                this.devices.tool_busy = None;
                if let Err(err) = result {
                    this.devices.tool_error = Some(err.to_string());
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn device_sources_section(&mut self, service: &ServiceState, config: &ServerConfig, can_configure: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let can_write = can_configure;
        let ssh_hosts = decode_ssh_hosts(&config.settings.device_hosts).unwrap_or_default();
        let eas_hosts = decode_eas_hosts(&config.settings.eas_device_hosts).unwrap_or_default();
        let simlock_hosts = decode_simlock_hosts(&config.settings.simlock_device_hosts).unwrap_or_default();

        let add_open = self.devices.add_menu_open;
        let show_eas = service.supports_eas_hosts;
        let show_simlock = service.supports_simlock_hosts && config.settings.threads.simlock_device_hosts_enabled;

        let add_menu = div()
            .relative()
            .child(
                Button::new("device-add-source", ButtonVariant::Outline, ButtonSize::Sm)
                    .icon(Icon::Plus)
                    .label("Add source")
                    .disabled(!can_write)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.devices.toggle_add_menu(Instant::now());
                        cx.notify();
                    })),
            )
            .when(add_open, |wrap| {
                let menu_item = |id: &'static str, label: &'static str| {
                    h_flex()
                        .id(id)
                        .min_h(r(28.))
                        .px(r(8.))
                        .py(r(4.))
                        .rounded(r(4.))
                        .text_sm()
                        .cursor_pointer()
                        .text_color(hsla(p.foreground))
                        .hover(|this| this.bg(hsla(p.accent)))
                        .child(label)
                };
                wrap.child(
                    div().absolute().bottom_0().right_0().child(
                        gpui::deferred(
                            gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
                                v_flex()
                                    .relative()
                                    .occlude()
                                    .mt(r(4.))
                                    .min_w(r(190.))
                                    .p(r(4.))
                                    .rounded(r(8.))
                                    .border_1()
                                    .border_color(hsla(p.dropdown_border))
                                    .bg(hsla(p.popover))
                                    .shadow(floating_shadow(cx.theme().is_dark()))
                                    .theme_slot(ThemeSlot::MenuPopup, cx)
                                    .children(theme_slot_clipped_background_layer(ThemeSlot::MenuPopup, r(8.), cx))
                                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                        this.devices.dismiss_add_menu_outside(Instant::now());
                                        cx.notify();
                                    }))
                                    .child(menu_item("add-device-ssh", "Machine over SSH").on_click(cx.listener(|this, _, window, cx| {
                                        this.devices.add_menu_open = false;
                                        this.open_ssh_editor(SshHostConfig::new(&new_host_id("ssh")), true, window, cx);
                                    })))
                                    .when(show_eas, |menu| {
                                        menu.child(menu_item("add-device-eas", "Expo Simulators").on_click(cx.listener(|this, _, window, cx| {
                                            this.devices.add_menu_open = false;
                                            this.open_eas_editor(EasHostConfig::new(&new_host_id("eas")), true, window, cx);
                                        })))
                                    })
                                    .when(show_simlock, |menu| {
                                        menu.child(menu_item("add-device-simlock", "Simlock pool (alpha)").on_click(cx.listener(|this, _, window, cx| {
                                            this.devices.add_menu_open = false;
                                            this.open_simlock_editor(SimlockHostConfig::new(&new_host_id("simlock")), true, window, cx);
                                        })))
                                    }),
                            ),
                        )
                        .with_priority(2),
                    ),
                )
            });

        let mut rows: Vec<AnyElement> = Vec::new();

        // Local simulators row
        rows.push(
            Row::new("device-source-local", "Local simulators")
                .description("Simulators and emulators installed on each environment's own machine.")
                .control(h_flex().gap(r(8.)).child(self.device_platforms(LOCAL_HOST_ID, cx)).children(status_badge(service.host_status, cx)))
                .into_any_element(),
        );

        // SSH hosts
        for host in ssh_hosts {
            let status_entry = service.host_statuses.get(&host.id);
            let status = status_entry.map(|e| e.status).unwrap_or(HostStatus::Idle);
            let detail = status_entry.and_then(|e| e.detail.clone());
            let mut desc = host.target.clone();
            if let Some(port) = host.port {
                desc.push_str(&format!(":{port}"));
            }
            if let Some(detail) = detail {
                desc.push_str(&format!(" · {detail}"));
            }
            let check_badge = self.devices.row_checks.get(&host.id).map(|check| match check {
                SshCheckState::Pending => tone_badge("Testing…", Tone::Info, cx),
                SshCheckState::Local => tone_badge("This machine", Tone::Secondary, cx),
                SshCheckState::Connected(platforms) => {
                    let label = if platforms.is_empty() {
                        "Reachable".to_owned()
                    } else {
                        platforms.iter().map(|pl| pl.name()).collect::<Vec<_>>().join(" & ")
                    };
                    tone_badge(label, Tone::Success, cx)
                }
                SshCheckState::Failed(err) => tone_badge(err.clone(), Tone::Error, cx),
            });
            let host_test = host.clone();
            let host_edit = host.clone();
            let host_rm = host.clone();
            let retry_id = host.id.clone();
            let show_retry = service.supports_host_retry && status == HostStatus::Failed;
            rows.push(
                Row::new(SharedString::from(format!("device-source-ssh-{}", host.id)), host.label.clone())
                    .description(desc)
                    .control(
                        h_flex()
                            .gap(r(6.))
                            .child(self.device_platforms(&host.id, cx))
                            .children(check_badge)
                            .children(status_badge(status, cx))
                            .when(show_retry, |h| {
                                h.child(
                                    Button::new(SharedString::from(format!("device-retry-{}", host.id)), ButtonVariant::Outline, ButtonSize::Compact)
                                        .label("Retry")
                                        .on_click(cx.listener(move |this, _, _, cx| this.retry_device_host(retry_id.clone(), cx))),
                                )
                            })
                            .child(
                                Button::new(SharedString::from(format!("device-edit-ssh-{}", host.id)), ButtonVariant::Outline, ButtonSize::Compact)
                                    .label("Edit")
                                    .disabled(!can_write)
                                    .on_click(cx.listener(move |this, _, window, cx| this.open_ssh_editor(host_edit.clone(), false, window, cx))),
                            )
                            .child(self.device_source_actions(&host.id, can_write, vec![
                                Button::new((ElementId::Name(host.id.clone().into()), "test-host"), ButtonVariant::GhostMuted, ButtonSize::Xs)
                                    .label("Test connection").justify_between()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.devices.action_menu = None;
                                        this.test_ssh_row(host_test.clone(), cx);
                                    })).into_any_element(),
                                Button::new((ElementId::Name(host.id.clone().into()), "remove-host"), ButtonVariant::GhostDestructive, ButtonSize::Xs)
                                    .label("Remove").justify_between()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.devices.action_menu = None;
                                        this.confirm_remove_ssh_host(host_rm.clone(), cx);
                                        cx.notify();
                                    })).into_any_element(),
                            ], cx)),
                    )
                    .into_any_element(),
            );
        }

        // EAS Simulator hosts
        for host in eas_hosts {
            let status_entry = service.host_statuses.get(&host.id);
            let status = status_entry.map(|e| e.status).unwrap_or(HostStatus::Idle);
            let mut desc = host.session_limits();
            if let Some(detail) = status_entry.and_then(|e| e.detail.as_deref()) {
                desc.push_str(&format!(" · {detail}"));
            }
            let host_edit = host.clone();
            let host_rm = host.clone();
            let retry_id = host.id.clone();
            let show_retry = service.supports_host_retry && status == HostStatus::Failed;
            rows.push(
                Row::new(SharedString::from(format!("device-source-eas-{}", host.id)), host.label.clone())
                    .description(desc)
                    .control(
                        h_flex()
                            .gap(r(6.))
                            .child(self.device_platforms(&host.id, cx))
                            .child(tone_badge("Expo", Tone::Secondary, cx))
                            .children(status_badge(status, cx))
                            .when(show_retry, |h| {
                                h.child(
                                    Button::new(SharedString::from(format!("device-retry-{}", host.id)), ButtonVariant::Outline, ButtonSize::Compact)
                                        .label("Retry")
                                        .on_click(cx.listener(move |this, _, _, cx| this.retry_device_host(retry_id.clone(), cx))),
                                )
                            })
                            .child(
                                Button::new(SharedString::from(format!("device-edit-eas-{}", host.id)), ButtonVariant::Outline, ButtonSize::Compact)
                                    .label("Edit")
                                    .disabled(!can_write)
                                    .on_click(cx.listener(move |this, _, window, cx| this.open_eas_editor(host_edit.clone(), false, window, cx))),
                            )
                            .child(self.device_source_actions(&host.id, can_write, vec![
                                Button::new((ElementId::Name(host.id.clone().into()), "remove-host"), ButtonVariant::GhostDestructive, ButtonSize::Xs)
                                    .label("Remove").justify_between()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.devices.action_menu = None;
                                        this.confirm_remove_eas_host(host_rm.clone(), cx);
                                        cx.notify();
                                    })).into_any_element(),
                            ], cx)),
                    )
                    .into_any_element(),
            );
        }

        // Simlock pool hosts
        for host in simlock_hosts {
            let status_entry = service.host_statuses.get(&host.id);
            let status = status_entry.map(|e| e.status).unwrap_or(HostStatus::Idle);
            let mut desc = format!("{} · {}", host.gateway_url, host.lease_limits());
            if let Some(detail) = status_entry.and_then(|e| e.detail.as_deref()) {
                desc.push_str(&format!(" · {detail}"));
            }
            let host_edit = host.clone();
            let host_rm = host.clone();
            let retry_id = host.id.clone();
            let show_retry = service.supports_host_retry && status == HostStatus::Failed;
            rows.push(
                Row::new(SharedString::from(format!("device-source-simlock-{}", host.id)), host.label.clone())
                    .description(desc)
                    .control(
                        h_flex()
                            .gap(r(6.))
                            .child(self.device_platforms(&host.id, cx))
                            .child(tone_badge("Simlock", Tone::Secondary, cx))
                            .children(status_badge(status, cx))
                            .when(show_retry, |h| {
                                h.child(
                                    Button::new(SharedString::from(format!("device-retry-{}", host.id)), ButtonVariant::Outline, ButtonSize::Compact)
                                        .label("Retry")
                                        .on_click(cx.listener(move |this, _, _, cx| this.retry_device_host(retry_id.clone(), cx))),
                                )
                            })
                            .child(
                                Button::new(SharedString::from(format!("device-edit-simlock-{}", host.id)), ButtonVariant::Outline, ButtonSize::Compact)
                                    .label("Edit")
                                    .disabled(!can_write)
                                    .on_click(cx.listener(move |this, _, window, cx| this.open_simlock_editor(host_edit.clone(), false, window, cx))),
                            )
                            .child(self.device_source_actions(&host.id, can_write, vec![
                                Button::new((ElementId::Name(host.id.clone().into()), "remove-host"), ButtonVariant::GhostDestructive, ButtonSize::Xs)
                                    .label("Remove").justify_between()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.devices.action_menu = None;
                                        this.confirm_remove_simlock_host(host_rm.clone(), cx);
                                        cx.notify();
                                    })).into_any_element(),
                            ], cx)),
                    )
                    .into_any_element(),
            );
        }

        v_flex()
            .gap(r(10.))
            .child(h_flex().justify_between().pr(r(8.)).child(section_title("Device sources")).child(add_menu))
            .child(group(rows, cx))
            .into_any_element()
    }

    fn retry_device_host(&mut self, host_id: String, cx: &mut Context<Self>) {
        let rpc = self.scoped_device_request(DEVICE_READ_SCOPE, device_method::LIST, retry_host_payload(&host_id), cx);
        cx.spawn(async move |this, cx| {
            let _ = rpc.await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    fn test_ssh_row(&mut self, host: SshHostConfig, cx: &mut Context<Self>) {
        let Ok(valid) = host.validate() else { return };
        let Some(owner) = self.device_owner(cx) else { return };
        let id = valid.id.clone();
        self.devices.row_checks.insert(id.clone(), SshCheckState::Pending);
        cx.notify();
        let rpc = self.scoped_device_request(SETTINGS_WRITE_SCOPE, device_method::TEST_HOST, valid.encode(), cx);
        let decode = |reply| serde_json::from_value::<Host>(reply).map_err(|error| error.to_string());
        cx.spawn(async move |this, cx| {
            let outcome = match rpc.await.map_err(|error| error.to_string()).and_then(decode) {
                Ok(summary) if summary.kind == HostKind::Local => SshCheckState::Local,
                Ok(summary) => SshCheckState::Connected(summary.platforms.into_iter().filter(|p| p.available).map(|p| p.platform).collect()),
                Err(err) => SshCheckState::Failed(err),
            };
            let _ = this.update(cx, |this, cx| {
                if this.device_owner(cx).as_ref() == Some(&owner) {
                    this.devices.row_checks.insert(id, outcome);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // ---- SSH editor ----

    fn open_ssh_editor(&mut self, host: SshHostConfig, is_new: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.capture_device_editor(window, cx) {
            return;
        }
        let label = cx.new(|cx| InputState::new(window, cx).default_value(host.label.clone()).placeholder("e.g. Mac mini"));
        let target = cx.new(|cx| InputState::new(window, cx).default_value(host.target.clone()).placeholder("e.g. mac-mini or user@host"));
        let identity_file = cx.new(|cx| InputState::new(window, cx).default_value(host.identity_file.clone().unwrap_or_default()).placeholder("~/.ssh/id_ed25519"));
        let port = cx.new(|cx| InputState::new(window, cx).default_value(host.port.map_or(String::new(), |p| p.to_string())).placeholder("22"));
        let subscriptions = [&label, &target, &identity_file, &port]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        let options_open = host.identity_file.is_some() || host.port.is_some();
        self.devices.editor = Some(HostEditor::Ssh(SshEditor {
            is_new,
            host_id: host.id,
            label,
            target,
            identity_file,
            port,
            options_open,
            check: None,
            check_seq: 0,
            saving: false,
            _subscriptions: subscriptions,
        }));
        cx.notify();
    }

    fn read_ssh_draft(ed: &SshEditor, cx: &App) -> Result<SshHostConfig, String> {
        let port_text = ed.port.read(cx).value().trim().to_owned();
        let port = if port_text.is_empty() {
            None
        } else {
            let p = port_text.parse::<u16>().ok().filter(|p| *p >= 1).ok_or("Port must be between 1 and 65535.")?;
            Some(p)
        };
        let identity = ed.identity_file.read(cx).value().trim().to_owned();
        SshHostConfig {
            id: ed.host_id.clone(),
            label: ed.label.read(cx).value().to_string(),
            target: ed.target.read(cx).value().to_string(),
            identity_file: (!identity.is_empty()).then_some(identity),
            port,
        }
        .validate()
    }

    fn test_ssh_editor(&mut self, cx: &mut Context<Self>) {
        let Some(HostEditor::Ssh(ed)) = self.devices.editor.as_mut() else { return };
        let Ok(valid) = Self::read_ssh_draft(ed, cx) else { return };
        self.devices.check_revision += 1;
        let seq = self.devices.check_revision;
        ed.check_seq = seq;
        ed.check = Some(SshCheckState::Pending);
        cx.notify();
        let rpc = self.scoped_device_request(SETTINGS_WRITE_SCOPE, device_method::TEST_HOST, valid.encode(), cx);
        let decode = |reply| serde_json::from_value::<Host>(reply).map_err(|error| error.to_string());
        cx.spawn(async move |this, cx| {
            let outcome = match rpc.await.map_err(|error| error.to_string()).and_then(decode) {
                Ok(summary) if summary.kind == HostKind::Local => SshCheckState::Local,
                Ok(summary) => SshCheckState::Connected(summary.platforms.into_iter().filter(|p| p.available).map(|p| p.platform).collect()),
                Err(err) => SshCheckState::Failed(err),
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(HostEditor::Ssh(ed)) = this.devices.editor.as_mut()
                    && ed.check_seq == seq
                {
                    ed.check = Some(outcome);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save_ssh_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.device_editor_writable(cx) {
            return;
        }
        let valid = {
            let Some(HostEditor::Ssh(ed)) = self.devices.editor.as_ref() else { return };
            if ed.saving {
                return;
            }
            let Ok(valid) = Self::read_ssh_draft(ed, cx) else { return };
            valid
        };
        let existing = self
            .device_config(cx)
            .and_then(|c| decode_ssh_hosts(&c.settings.device_hosts).ok())
            .unwrap_or_default();
        let next = update_ssh_hosts(&existing, &valid, false);
        if let Some(HostEditor::Ssh(ed)) = self.devices.editor.as_mut() {
            ed.saving = true;
        }
        cx.notify();
        let view = cx.entity().downgrade();
        let window = window.window_handle();
        self.save_device_settings(
            ServerSettingChange::Prepared(ssh_hosts_patch(&next)),
            move |error, cx| {
                cx.defer(move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        if error.is_none() {
                            let focus = this.devices.previous_focus.take().unwrap_or_else(|| this.focus.clone());
                            let _ = window.update(cx, |_, window, cx| focus.focus(window, cx));
                            this.devices.editor = None;
                            this.devices.editor_target = None;
                            this.devices.editor_focus = None;
                            this.devices.previous_focus = None;
                        } else if let Some(HostEditor::Ssh(ed)) = this.devices.editor.as_mut() {
                            ed.saving = false;
                        }
                        cx.notify();
                    });
                });
            },
            cx,
        );
    }

    fn confirm_remove_ssh_host(&mut self, host: SshHostConfig, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        let owner = self.device_owner(cx);
        let title = crate::i18n::t_fmt(cx, "Remove “{name}”?", &[("name", host.label.as_str().into())]);
        crate::dialog::ask(
            title,
            "Simulators and emulators on this SSH host will no longer appear in the Device panel.",
            "Remove",
            move |_, cx| cx.defer(move |cx| {
                let _ = view.update(cx, |this, cx| {
                    if this.device_owner(cx) != owner || !this.device_editor_writable(cx) {
                        return;
                    }
                    let existing = this
                        .device_config(cx)
                        .and_then(|c| decode_ssh_hosts(&c.settings.device_hosts).ok())
                        .unwrap_or_default();
                    let next = update_ssh_hosts(&existing, &host, true);
                    this.save_device_settings(ServerSettingChange::Prepared(ssh_hosts_patch(&next)), |_, _| {}, cx);
                });
            }),
            cx,
        );
    }

    // ---- EAS Simulator editor (#220) ----

    fn open_eas_editor(&mut self, host: EasHostConfig, is_new: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.capture_device_editor(window, cx) {
            return;
        }
        let token_editor = EasTokenEditor::new(&host, is_new);
        let (source, initial_token, masked) = match token_editor.field_value() {
            SecretValue::Reference { provider, reference } => (provider, reference, false),
            SecretValue::Literal(lit) => (STORED_SECRET.to_owned(), lit, true),
        };
        let label = cx.new(|cx| InputState::new(window, cx).default_value(host.label.clone()).placeholder("Expo Simulators"));
        let token = cx.new(|cx| {
            let input = InputState::new(window, cx).default_value(initial_token).placeholder(if token_editor.configured() {
                "Token saved — enter a new token to replace"
            } else {
                "Expo access token (optional)"
            });
            if masked { input.masked(true) } else { input }
        });
        let max_sessions = cx.new(|cx| InputState::new(window, cx).default_value(host.max_concurrent_sessions.to_string()).placeholder("2"));
        let max_minutes = cx.new(|cx| InputState::new(window, cx).default_value(host.max_session_minutes.to_string()).placeholder("30"));
        let idle_minutes = cx.new(|cx| InputState::new(window, cx).default_value(host.idle_minutes.to_string()).placeholder("10"));
        let mut subscriptions: Vec<Subscription> = [&label, &max_sessions, &max_minutes, &idle_minutes]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        subscriptions.push(cx.subscribe(&token, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let raw = input.read(cx).value().to_string();
                if let Some(HostEditor::Eas(ed)) = this.devices.editor.as_mut() {
                    if ed.source == STORED_SECRET {
                        let trimmed = raw.trim();
                        if !trimmed.is_empty() {
                            ed.token_editor.set_token(Some(SecretValue::Literal(trimmed.to_owned())));
                        } else if !ed.token_editor.configured() {
                            ed.token_editor.set_token(None);
                        }
                        ed.token_editor.set_validity(true);
                    } else if let Some(secret) = SecretValue::reference(&ed.source, &raw) {
                        ed.token_editor.set_token(Some(secret));
                        ed.token_editor.set_validity(true);
                    } else {
                        ed.token_editor.set_validity(false);
                    }
                }
                cx.notify();
            }
        }));
        let open_payload = token_editor.open_check_payload(&host.id);
        self.devices.editor = Some(HostEditor::Eas(EasEditor {
            is_new,
            host_id: host.id,
            token_editor,
            source,
            app_id: host.app_id,
            label,
            token,
            max_sessions,
            max_minutes,
            idle_minutes,
            checking: false,
            account: None,
            check_seq: 0,
            saving: false,
            _subscriptions: subscriptions,
        }));
        self.run_eas_account_check(open_payload, cx);
        cx.notify();
    }

    fn read_eas_draft(ed: &EasEditor, cx: &App) -> EasHostConfig {
        EasHostConfig {
            id: ed.host_id.clone(),
            label: ed.label.read(cx).value().to_string(),
            app_id: ed.app_id.clone(),
            access_token: ed.token_editor.token_for_save(),
            max_session_minutes: ed.max_minutes.read(cx).value().trim().parse::<i64>().unwrap_or(0),
            idle_minutes: ed.idle_minutes.read(cx).value().trim().parse::<i64>().unwrap_or(0),
            max_concurrent_sessions: ed.max_sessions.read(cx).value().trim().parse::<i64>().unwrap_or(0),
        }
    }

    fn run_eas_account_check(&mut self, payload: Value, cx: &mut Context<Self>) {
        let Some(HostEditor::Eas(ed)) = self.devices.editor.as_mut() else { return };
        self.devices.check_revision += 1;
        let seq = self.devices.check_revision;
        ed.check_seq = seq;
        ed.checking = true;
        cx.notify();
        let rpc = self.scoped_device_request(SETTINGS_WRITE_SCOPE, device_method::EAS_ACCOUNT, payload, cx);
        let decode = |reply| EasAccount::decode(reply);
        cx.spawn(async move |this, cx| {
            let result = rpc.await.map_err(|error| error.to_string()).and_then(decode);
            let _ = this.update(cx, |this, cx| {
                if let Some(HostEditor::Eas(ed)) = this.devices.editor.as_mut()
                    && ed.check_seq == seq
                {
                    ed.checking = false;
                    ed.account = Some(result);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save_eas_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.device_editor_writable(cx) {
            return;
        }
        let (next_host, keep_token) = {
            let Some(HostEditor::Eas(ed)) = self.devices.editor.as_ref() else { return };
            if ed.saving {
                return;
            }
            let draft = Self::read_eas_draft(ed, cx);
            if ed.token_editor.problem(&draft).is_some() {
                return;
            }
            (ed.token_editor.next(&draft), matches!(ed.token_editor.token(), hosts::TokenDraft::Saved))
        };
        let existing = self
            .device_config(cx)
            .and_then(|c| decode_eas_hosts(&c.settings.eas_device_hosts).ok())
            .unwrap_or_default();
        let next = update_eas_hosts(&existing, &next_host, false, keep_token);
        if let Some(HostEditor::Eas(ed)) = self.devices.editor.as_mut() {
            ed.saving = true;
        }
        cx.notify();
        let view = cx.entity().downgrade();
        let window = window.window_handle();
        self.save_device_settings(
            ServerSettingChange::Prepared(eas_hosts_patch(&next)),
            move |error, cx| {
                cx.defer(move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        if error.is_none() {
                            let focus = this.devices.previous_focus.take().unwrap_or_else(|| this.focus.clone());
                            let _ = window.update(cx, |_, window, cx| focus.focus(window, cx));
                            this.devices.editor = None;
                            this.devices.editor_target = None;
                            this.devices.editor_focus = None;
                            this.devices.previous_focus = None;
                        } else if let Some(HostEditor::Eas(ed)) = this.devices.editor.as_mut() {
                            ed.saving = false;
                        }
                        cx.notify();
                    });
                });
            },
            cx,
        );
    }

    fn confirm_remove_eas_host(&mut self, host: EasHostConfig, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        let owner = self.device_owner(cx);
        let title = crate::i18n::t_fmt(cx, "Remove “{name}”?", &[("name", host.label.as_str().into())]);
        crate::dialog::ask(
            title,
            "Expo cloud simulators from this host will no longer appear in the Device panel.",
            "Remove",
            move |_, cx| cx.defer(move |cx| {
                let _ = view.update(cx, |this, cx| {
                    if this.device_owner(cx) != owner || !this.device_editor_writable(cx) {
                        return;
                    }
                    let existing = this
                        .device_config(cx)
                        .and_then(|c| decode_eas_hosts(&c.settings.eas_device_hosts).ok())
                        .unwrap_or_default();
                    let next = update_eas_hosts(&existing, &host, true, false);
                    this.save_device_settings(ServerSettingChange::Prepared(eas_hosts_patch(&next)), |_, _| {}, cx);
                });
            }),
            cx,
        );
    }

    // ---- Simlock pool editor ----

    fn open_simlock_editor(&mut self, host: SimlockHostConfig, is_new: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.capture_device_editor(window, cx) {
            return;
        }
        let token_editor = SimlockTokenEditor::new(&host, is_new);
        let (source, initial_token, masked) = match token_editor.field_value() {
            SecretValue::Reference { provider, reference } => (provider, reference, false),
            SecretValue::Literal(lit) => (STORED_SECRET.to_owned(), lit, true),
        };
        let label = cx.new(|cx| InputState::new(window, cx).default_value(host.label.clone()).placeholder("Simlock pool"));
        let gateway_url = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(host.gateway_url.clone())
                .placeholder("https://gateway.example.ts.net:4700")
        });
        let token = cx.new(|cx| {
            let input = InputState::new(window, cx).default_value(initial_token).placeholder(if token_editor.configured() {
                "Token saved — enter a new token to replace"
            } else {
                "Simlock agent token (optional)"
            });
            if masked { input.masked(true) } else { input }
        });
        let max_leases = cx.new(|cx| InputState::new(window, cx).default_value(host.max_leases.to_string()).placeholder("2"));
        let max_lease_minutes = cx.new(|cx| InputState::new(window, cx).default_value(host.max_lease_minutes.to_string()).placeholder("60"));
        let mut subscriptions: Vec<Subscription> = [&label, &gateway_url, &max_leases, &max_lease_minutes]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        subscriptions.push(cx.subscribe(&token, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let raw = input.read(cx).value().to_string();
                if let Some(HostEditor::Simlock(ed)) = this.devices.editor.as_mut() {
                    if ed.source == STORED_SECRET {
                        let trimmed = raw.trim();
                        if !trimmed.is_empty() {
                            ed.token_editor.set_token(Some(SecretValue::Literal(trimmed.to_owned())));
                        } else if !ed.token_editor.configured() {
                            ed.token_editor.set_token(None);
                        }
                        ed.token_editor.set_validity(true);
                    } else if let Some(secret) = SecretValue::reference(&ed.source, &raw) {
                        ed.token_editor.set_token(Some(secret));
                        ed.token_editor.set_validity(true);
                    } else {
                        ed.token_editor.set_validity(false);
                    }
                }
                cx.notify();
            }
        }));
        let check_payload = if is_new && host.gateway_url.is_empty() {
            json!({})
        } else {
            token_editor.check_payload(&host.id, &host.gateway_url)
        };
        self.devices.editor = Some(HostEditor::Simlock(SimlockEditor {
            is_new,
            host_id: host.id,
            token_editor,
            source,
            label,
            gateway_url,
            token,
            max_leases,
            max_lease_minutes,
            checking: false,
            identity: None,
            check_seq: 0,
            saving: false,
            _subscriptions: subscriptions,
        }));
        self.run_simlock_identity_check(check_payload, is_new, window, cx);
        cx.notify();
    }

    fn read_simlock_draft(ed: &SimlockEditor, cx: &App) -> SimlockHostConfig {
        SimlockHostConfig {
            id: ed.host_id.clone(),
            label: ed.label.read(cx).value().to_string(),
            gateway_url: ed.gateway_url.read(cx).value().to_string(),
            agent_token: ed.token_editor.token_for_save(),
            max_leases: ed.max_leases.read(cx).value().trim().parse::<i64>().unwrap_or(0),
            max_lease_minutes: ed.max_lease_minutes.read(cx).value().trim().parse::<i64>().unwrap_or(0),
        }
    }

    fn run_simlock_identity_check(&mut self, payload: Value, prefill_gateway: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(HostEditor::Simlock(ed)) = self.devices.editor.as_mut() else { return };
        self.devices.check_revision += 1;
        let seq = self.devices.check_revision;
        ed.check_seq = seq;
        ed.checking = true;
        cx.notify();
        let rpc = self.scoped_device_request(SETTINGS_WRITE_SCOPE, device_method::SIMLOCK_IDENTITY, payload, cx);
        let decode = |reply| SimlockIdentity::decode(reply);
        cx.spawn_in(window, async move |this, cx| {
            let result = rpc.await.map_err(|error| error.to_string()).and_then(decode);
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(HostEditor::Simlock(ed)) = this.devices.editor.as_mut()
                    && ed.check_seq == seq
                {
                    ed.checking = false;
                    if prefill_gateway
                        && let Ok(ident) = &result
                        && ed.gateway_url.read(cx).value().trim().is_empty()
                        && !ident.gateway_url.is_empty()
                    {
                        let url = ident.gateway_url.clone();
                        ed.gateway_url.update(cx, |input, cx| input.set_value(url, window, cx));
                    }
                    ed.identity = Some(result);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save_simlock_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.device_editor_writable(cx) {
            return;
        }
        let (next_host, keep_token) = {
            let Some(HostEditor::Simlock(ed)) = self.devices.editor.as_ref() else { return };
            if ed.saving {
                return;
            }
            let draft = Self::read_simlock_draft(ed, cx);
            if ed.token_editor.problem(&draft).is_some() {
                return;
            }
            (ed.token_editor.next(&draft), ed.token_editor.configured())
        };
        let existing = self
            .device_config(cx)
            .and_then(|c| decode_simlock_hosts(&c.settings.simlock_device_hosts).ok())
            .unwrap_or_default();
        let next = update_simlock_hosts(&existing, &next_host, false, keep_token);
        if let Some(HostEditor::Simlock(ed)) = self.devices.editor.as_mut() {
            ed.saving = true;
        }
        cx.notify();
        let view = cx.entity().downgrade();
        let window = window.window_handle();
        self.save_device_settings(
            ServerSettingChange::Prepared(simlock_hosts_patch(&next)),
            move |error, cx| {
                cx.defer(move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        if error.is_none() {
                            let focus = this.devices.previous_focus.take().unwrap_or_else(|| this.focus.clone());
                            let _ = window.update(cx, |_, window, cx| focus.focus(window, cx));
                            this.devices.editor = None;
                            this.devices.editor_target = None;
                            this.devices.editor_focus = None;
                            this.devices.previous_focus = None;
                        } else if let Some(HostEditor::Simlock(ed)) = this.devices.editor.as_mut() {
                            ed.saving = false;
                        }
                        cx.notify();
                    });
                });
            },
            cx,
        );
    }

    fn confirm_remove_simlock_host(&mut self, host: SimlockHostConfig, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        let owner = self.device_owner(cx);
        let title = crate::i18n::t_fmt(cx, "Remove “{name}”?", &[("name", host.label.as_str().into())]);
        crate::dialog::ask(
            title,
            "Devices leased from this Simlock pool will no longer appear in the Device panel.",
            "Remove",
            move |_, cx| cx.defer(move |cx| {
                let _ = view.update(cx, |this, cx| {
                    if this.device_owner(cx) != owner || !this.device_editor_writable(cx) {
                        return;
                    }
                    let existing = this
                        .device_config(cx)
                        .and_then(|c| decode_simlock_hosts(&c.settings.simlock_device_hosts).ok())
                        .unwrap_or_default();
                    let next = update_simlock_hosts(&existing, &host, true, false);
                    this.save_device_settings(ServerSettingChange::Prepared(simlock_hosts_patch(&next)), |_, _| {}, cx);
                });
            }),
            cx,
        );
    }

    // ---- Editor renderers ----

    fn host_editor_panel(&self, config: &ServerConfig, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = palette(cx);
        let writable = self.device_editor_writable(cx);
        let (title, body, footer) = match self.devices.editor.as_ref()? {
            HostEditor::Ssh(ed) => {
                let title = if ed.is_new { "Add machine over SSH" } else { "Edit SSH host" };
                let valid = Self::read_ssh_draft(ed, cx);
                let check_el: Option<AnyElement> = ed.check.as_ref().map(|c| match c {
                    SshCheckState::Pending => div().text_xs().text_color(hsla(p.muted_foreground)).child("Testing connection…").into_any_element(),
                    SshCheckState::Local => div().text_xs().text_color(hsla(p.muted_foreground)).child("This target resolves to this machine.").into_any_element(),
                    SshCheckState::Connected(platforms) => {
                        let names = if platforms.is_empty() {
                            "Connected (no simulator platforms detected)".to_owned()
                        } else {
                            format!("Connected · {}", platforms.iter().map(|pl| pl.name()).collect::<Vec<_>>().join(", "))
                        };
                        div().text_xs().text_color(hsla(p.success_foreground)).child(names).into_any_element()
                    }
                    SshCheckState::Failed(err) => div().text_xs().text_color(hsla(p.error_foreground)).child(err.clone()).into_any_element(),
                });
                let mut rows = vec![
                    Row::new("ssh-ed-label", "Name")
                        .description("Shown in the Device panel. Defaults to the SSH target.")
                        .control(div().w(r(260.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.label).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                    Row::new("ssh-ed-target", "SSH target")
                        .description("Host alias from ~/.ssh/config or user@hostname.")
                        .control(div().w(r(260.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.target).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                    Row::new("ssh-ed-options-toggle", "SSH options")
                        .control(
                            Button::new("ssh-ed-opts-btn", ButtonVariant::GhostMuted, ButtonSize::Compact)
                                .label(if ed.options_open { "Hide options" } else { "Identity file & port" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(HostEditor::Ssh(ed)) = this.devices.editor.as_mut() {
                                        ed.options_open = !ed.options_open;
                                        cx.notify();
                                    }
                                })),
                        )
                        .into_any_element(),
                ];
                if ed.options_open {
                    rows.push(
                        Row::new("ssh-ed-idfile", "Identity file")
                            .control(div().w(r(260.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.identity_file).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                            .into_any_element(),
                    );
                    rows.push(
                        Row::new("ssh-ed-port", "Port")
                            .control(div().w(r(120.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.port).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                            .into_any_element(),
                    );
                }
                let testing = matches!(ed.check, Some(SshCheckState::Pending));
                let can_save = valid.is_ok() && !ed.saving;
                let footer = h_flex()
                    .justify_between()
                    .gap(r(8.))
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(
                                Button::new("ssh-ed-test", ButtonVariant::Outline, ButtonSize::Sm)
                                    .label(if testing { "Testing…" } else { "Test connection" })
                                    .disabled(valid.is_err() || testing || ed.saving || !writable)
                                    .on_click(cx.listener(|this, _, _, cx| this.test_ssh_editor(cx))),
                            )
                            .children(check_el),
                    )
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(Button::new("ssh-ed-cancel", ButtonVariant::Ghost, ButtonSize::Sm).label("Cancel").disabled(ed.saving).on_click(cx.listener(|this, _, window, cx| this.dismiss_device_editor(window, cx))))
                            .child(
                                Button::new("ssh-ed-save", ButtonVariant::Default, ButtonSize::Sm)
                                    .label(if ed.saving { "Saving…" } else { "Save" })
                                    .disabled(!can_save || !writable)
                                    .on_click(cx.listener(|this, _, window, cx| this.save_ssh_editor(window, cx))),
                            ),
                    );
                (title, group(rows, cx).into_any_element(), footer.into_any_element())
            }
            HostEditor::Eas(ed) => {
                let title = if ed.is_new { "Add Expo host" } else { "Edit Expo host" };
                let draft = Self::read_eas_draft(ed, cx);
                let problem = ed.token_editor.problem(&draft);
                let can_check = ed.token_editor.can_check() && !ed.checking && !ed.saving;
                let has_token_or_ref = ed.token_editor.configured() || !ed.token_editor.field_value().is_empty() || ed.source != STORED_SECRET;

                let view_source = cx.entity().downgrade();
                let token_input = SecretSourceInput::new(
                    "eas-token-source", "Expo access token", &ed.token, &config.settings.secret_providers.0,
                    (ed.source != STORED_SECRET).then_some(ed.source.as_str()),
                ).disabled(ed.saving || !writable).on_source_change(move |next, window, cx| {
                    let _ = view_source.update(cx, |this, cx| {
                        if let Some(HostEditor::Eas(ed)) = this.devices.editor.as_mut() {
                            let is_stored = next.is_none();
                            ed.source = next.map_or_else(|| STORED_SECRET.to_owned(), |value| value.to_string());
                            if is_stored {
                                ed.token_editor.switch_to_stored();
                            } else {
                                ed.token_editor.set_validity(false);
                            }
                            ed.token.update(cx, |input, cx| {
                                input.set_masked(is_stored, window, cx);
                                input.set_value("", window, cx);
                            });
                            cx.notify();
                        }
                    });
                });
                let token_input = self.device_token_check(token_input, config, &ed.source, &ed.token, cx);

                let account_status: Option<AnyElement> = if ed.checking {
                    Some(div().text_xs().text_color(hsla(p.muted_foreground)).child("Checking Expo account…").into_any_element())
                } else {
                    ed.account.as_ref().map(|res| match res {
                        Ok(acc) => div()
                            .text_xs()
                            .text_color(hsla(p.success_foreground))
                            .child(format!("Signed in as {} via {}.", acc.signed_in_as, acc.credential_source_label()))
                            .into_any_element(),
                        Err(err) => div().text_xs().text_color(hsla(p.error_foreground)).child(err.clone()).into_any_element(),
                    })
                };

                let mut app_options = vec![SelectOption::new(AUTO_EAS_APP, "From the thread's project")];
                if let Some(Ok(acc)) = &ed.account {
                    for app in &acc.apps {
                        app_options.push(SelectOption::new(app.id.clone(), app.full_name.clone()));
                    }
                }
                if let Some(app_id) = &ed.app_id
                    && !app_options.iter().any(|o| o.value.as_ref() == app_id)
                {
                    app_options.push(SelectOption::new(app_id.clone(), app_id.clone()));
                }
                let view_app = cx.entity().downgrade();
                let app_select = Select::new("eas-app-select", app_options)
                    .selected(ed.app_id.clone().unwrap_or_else(|| AUTO_EAS_APP.to_owned()))
                    .width(260.)
                    .on_change(move |value, _, cx| {
                        let next_app = if value.as_ref() == AUTO_EAS_APP { None } else { Some(value.to_string()) };
                        let _ = view_app.update(cx, |this, cx| {
                            if let Some(HostEditor::Eas(ed)) = this.devices.editor.as_mut() {
                                ed.app_id = next_app;
                                cx.notify();
                            }
                        });
                    });

                let rows = vec![
                    Row::new("eas-ed-label", "Name")
                        .control(div().w(r(260.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.label).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                    Row::new("eas-ed-token", "Expo access token")
                        .description("Optional. Leave blank to use EXPO_TOKEN or the server's eas login.")
                        .below(
                            v_flex()
                                .gap(r(6.))
                                .mt(r(8.))
                                .pb(r(10.))
                                .on_key_down(stop_slash_shortcut)
                                .child(
                                    h_flex()
                                        .gap(r(6.))
                                        .child(token_input)
                                        .when(has_token_or_ref, |row| {
                                            row.child(
                                                Button::new("eas-token-clear", ButtonVariant::Outline, ButtonSize::Sm)
                                                    .label("Clear")
                                                    .disabled(ed.saving)
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        if let Some(HostEditor::Eas(ed)) = this.devices.editor.as_mut() {
                                                            ed.source = STORED_SECRET.to_owned();
                                                            ed.token_editor.clear();
                                                            ed.token.update(cx, |input, cx| {
                                                                input.set_masked(true, window, cx);
                                                                input.set_value(String::new(), window, cx);
                                                            });
                                                            cx.notify();
                                                        }
                                                    })),
                                            )
                                        })
                                        .child(
                                            Button::new("eas-check-account", ButtonVariant::Outline, ButtonSize::Sm)
                                                .label(if ed.checking { "Checking…" } else { "Check account" })
                                                .disabled(!can_check || !writable)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    let payload = match this.devices.editor.as_ref() {
                                                        Some(HostEditor::Eas(ed)) => ed.token_editor.check_payload(&ed.host_id),
                                                        _ => return,
                                                    };
                                                    this.run_eas_account_check(payload, cx);
                                                })),
                                        ),
                                )
                                .children(account_status),
                        )
                        .into_any_element(),
                    Row::new("eas-ed-app", "Expo project")
                        .description("Used when the thread's project has no app.json / app.config Expo project id.")
                        .control(app_select)
                        .into_any_element(),
                    Row::new("eas-ed-sessions", "Max concurrent sessions")
                        .description("1 to 20 cloud simulators at once.")
                        .control(div().w(r(100.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.max_sessions).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                    Row::new("eas-ed-max-min", "Max session length (minutes)")
                        .description("1 to 115 minutes (40 min max on free Expo accounts).")
                        .control(div().w(r(100.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.max_minutes).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                    Row::new("eas-ed-idle-min", "Idle timeout (minutes)")
                        .description("1 to 60 minutes; must be smaller than max session length.")
                        .control(div().w(r(100.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.idle_minutes).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                ];

                let footer = h_flex()
                    .justify_between()
                    .gap(r(8.))
                    .child(
                        div()
                            .text_xs()
                            .text_color(hsla(p.error_foreground))
                            .children(problem.map(|msg| msg.to_owned())),
                    )
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(Button::new("eas-ed-cancel", ButtonVariant::Ghost, ButtonSize::Sm).label("Cancel").disabled(ed.saving).on_click(cx.listener(|this, _, window, cx| this.dismiss_device_editor(window, cx))))
                            .child(
                                Button::new("eas-ed-save", ButtonVariant::Default, ButtonSize::Sm)
                                    .label(if ed.saving { "Saving…" } else { "Save" })
                                    .disabled(problem.is_some() || ed.saving || !writable)
                                    .on_click(cx.listener(|this, _, window, cx| this.save_eas_editor(window, cx))),
                            ),
                    );
                (title, group(rows, cx).into_any_element(), footer.into_any_element())
            }
            HostEditor::Simlock(ed) => {
                let title = if ed.is_new { "Add Simlock pool (alpha)" } else { "Edit Simlock pool" };
                let draft = Self::read_simlock_draft(ed, cx);
                let problem = ed.token_editor.problem(&draft);
                let can_check = ed.token_editor.can_check() && !ed.checking && !ed.saving;
                let has_token_or_ref = ed.token_editor.configured() || !ed.token_editor.field_value().is_empty() || ed.source != STORED_SECRET;

                let view_source = cx.entity().downgrade();
                let token_input = SecretSourceInput::new(
                    "simlock-token-source", "Simlock agent token", &ed.token, &config.settings.secret_providers.0,
                    (ed.source != STORED_SECRET).then_some(ed.source.as_str()),
                ).disabled(ed.saving || !writable).on_source_change(move |next, window, cx| {
                    let _ = view_source.update(cx, |this, cx| {
                        if let Some(HostEditor::Simlock(ed)) = this.devices.editor.as_mut() {
                            let is_stored = next.is_none();
                            ed.source = next.map_or_else(|| STORED_SECRET.to_owned(), |value| value.to_string());
                            if is_stored {
                                ed.token_editor.switch_to_stored();
                            } else {
                                ed.token_editor.set_validity(false);
                            }
                            ed.token.update(cx, |input, cx| {
                                input.set_masked(is_stored, window, cx);
                                input.set_value("", window, cx);
                            });
                            cx.notify();
                        }
                    });
                });
                let token_input = self.device_token_check(token_input, config, &ed.source, &ed.token, cx);

                let ident_status: Option<AnyElement> = if ed.checking {
                    Some(div().text_xs().text_color(hsla(p.muted_foreground)).child("Checking Simlock gateway…").into_any_element())
                } else {
                    ed.identity.as_ref().map(|res| match res {
                        Ok(ident) => div()
                            .text_xs()
                            .text_color(hsla(p.success_foreground))
                            .child(format!("Authenticated as {} via {}.", ident.signed_in_as, ident.token_source_label()))
                            .into_any_element(),
                        Err(err) => div().text_xs().text_color(hsla(p.error_foreground)).child(err.clone()).into_any_element(),
                    })
                };

                let rows = vec![
                    Row::new("simlock-ed-label", "Name")
                        .control(div().w(r(260.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.label).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                    Row::new("simlock-ed-gateway", "Gateway URL")
                        .description("Origin of your Simlock gateway (e.g. https://gateway.example.ts.net:4700).")
                        .control(div().w(r(300.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.gateway_url).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                    Row::new("simlock-ed-token", "Agent token")
                        .description("Optional if AGENT_DEVICE_DAEMON_AUTH_TOKEN is configured on the server.")
                        .below(
                            v_flex()
                                .gap(r(6.))
                                .mt(r(8.))
                                .pb(r(10.))
                                .on_key_down(stop_slash_shortcut)
                                .child(
                                    h_flex()
                                        .gap(r(6.))
                                        .child(token_input)
                                        .when(has_token_or_ref, |row| {
                                            row.child(
                                                Button::new("simlock-token-clear", ButtonVariant::Outline, ButtonSize::Sm)
                                                    .label("Clear")
                                                    .disabled(ed.saving)
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        if let Some(HostEditor::Simlock(ed)) = this.devices.editor.as_mut() {
                                                            ed.source = STORED_SECRET.to_owned();
                                                            ed.token_editor.clear();
                                                            ed.token.update(cx, |input, cx| {
                                                                input.set_masked(true, window, cx);
                                                                input.set_value(String::new(), window, cx);
                                                            });
                                                            cx.notify();
                                                        }
                                                    })),
                                            )
                                        })
                                        .child(
                                            Button::new("simlock-check", ButtonVariant::Outline, ButtonSize::Sm)
                                                .label(if ed.checking { "Checking…" } else { "Check connection" })
                                                .disabled(!can_check || !writable)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    let payload = match this.devices.editor.as_ref() {
                                                        Some(HostEditor::Simlock(ed)) => {
                                                            ed.token_editor.check_payload(&ed.host_id, &ed.gateway_url.read(cx).value())
                                                        }
                                                        _ => return,
                                                    };
                                                    this.run_simlock_identity_check(payload, false, window, cx);
                                                })),
                                        ),
                                )
                                .children(ident_status),
                        )
                        .into_any_element(),
                    Row::new("simlock-ed-leases", "Max concurrent leases")
                        .description("1 to 20 leased devices at once.")
                        .control(div().w(r(100.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.max_leases).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                    Row::new("simlock-ed-lease-min", "Max lease duration (minutes)")
                        .description("1 to 240 minutes per lease.")
                        .control(div().w(r(100.)).on_key_down(stop_slash_shortcut).child(Input::new(&ed.max_lease_minutes).small().disabled(ed.saving).theme_slot(ThemeSlot::Input, cx)))
                        .into_any_element(),
                ];

                let footer = h_flex()
                    .justify_between()
                    .gap(r(8.))
                    .child(
                        div()
                            .text_xs()
                            .text_color(hsla(p.error_foreground))
                            .children(problem.map(|msg| msg.to_owned())),
                    )
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(Button::new("simlock-ed-cancel", ButtonVariant::Ghost, ButtonSize::Sm).label("Cancel").disabled(ed.saving).on_click(cx.listener(|this, _, window, cx| this.dismiss_device_editor(window, cx))))
                            .child(
                                Button::new("simlock-ed-save", ButtonVariant::Default, ButtonSize::Sm)
                                    .label(if ed.saving { "Saving…" } else { "Save" })
                                    .disabled(problem.is_some() || ed.saving || !writable)
                                    .on_click(cx.listener(|this, _, window, cx| this.save_simlock_editor(window, cx))),
                            ),
                    );
                (title, group(rows, cx).into_any_element(), footer.into_any_element())
            }
        };
        Some(HostEditorFrame {
            view: cx.entity().downgrade(),
            frame: DialogFrame {
                id: "device-host-editor", title: title.into(), description: "".into(),
                max_width: 620., body,
                footer: div().px(r(24.)).py(r(16.)).border_t_1().border_color(hsla(p.border)).child(footer).into_any_element(),
                focus: self.devices.editor_focus.clone()?,
            },
        }.into_any_element())
    }

    fn device_token_check(
        &self, input: SecretSourceInput, config: &ServerConfig, source: &str,
        reference: &Entity<InputState>, cx: &Context<Self>,
    ) -> SecretSourceInput {
        if source == STORED_SECRET || config.environment.capabilities.secret_source_check != Some(true) {
            return input;
        }
        let Some(owner) = self.device_owner(cx) else { return input };
        let view = cx.entity().downgrade();
        input.check(SecretSourceCheck::new(
            owner.saved_id, source.to_owned(), reference.read(cx).value().trim().to_owned(),
            self.device_editor_writable(cx),
            move |method, payload, cx| {
                let request = view.upgrade().map(|view| {
                    view.read(cx).scoped_device_request(SETTINGS_WRITE_SCOPE, method, payload, cx)
                });
                async move {
                    match request {
                        Some(request) => request.await,
                        None => Err(RpcError::Closed),
                    }
                }
            },
        ))
    }

    /// `update`: one `device.configure`; a failure says which environment kept its settings.
    fn configure_devices(&mut self, pending: Pending, input: Value, cx: &mut Context<Self>) {
        if self.devices.pending.is_some() || !self.device_editor_writable(cx) {
            return;
        }
        self.devices.pending = Some(pending);
        cx.notify();
        let label = self.device_config(cx).map_or_else(|| self.environment_label(cx), |config| config.environment.label.clone());
        let task = self.scoped_device_request(SETTINGS_WRITE_SCOPE, device_method::CONFIGURE, input, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.devices.pending = None;
                if result.is_err() {
                    let title = crate::i18n::t_fmt(cx, "Could not update {name}.", &[("name", label.as_str().into())]);
                    let toast = Toast::new(ToastKind::Error, title);
                    this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_eas_and_simlock_secret_source_to_stored_never_leaks_stale_reference() {
        let reference = SecretValue::reference("op", "op://Vault/Expo/token").unwrap();

        // EAS host originally saved with a secret reference: switching to Stored secret clears it.
        let eas_ref = EasHostConfig { access_token: reference.clone(), ..EasHostConfig::new("eas-1") };
        let mut eas_ed = EasTokenEditor::new(&eas_ref, false);
        assert_eq!(eas_ed.token_for_save(), reference);
        eas_ed.switch_to_stored();
        assert!(eas_ed.token_for_save().is_empty());
        assert_eq!(eas_ed.check_payload("eas-1"), json!({ "accessToken": "" }));
        assert!(eas_ed.valid());

        // EAS host originally saved with a literal, user committed a reference then switched back to Stored:
        // restores the saved literal instead of leaking the committed reference.
        let eas_lit = EasHostConfig { access_token: SecretValue::Literal("__REDACTED__".into()), ..EasHostConfig::new("eas-2") };
        let mut eas_lit_ed = EasTokenEditor::new(&eas_lit, false);
        eas_lit_ed.set_token(Some(reference.clone()));
        eas_lit_ed.switch_to_stored();
        assert!(eas_lit_ed.configured());
        assert_eq!(eas_lit_ed.token_for_save(), SecretValue::Literal("__REDACTED__".into()));

        // Simlock host originally saved with a secret reference: switching to Stored secret clears it.
        let sim_ref = SimlockHostConfig {
            gateway_url: "https://gw.example.ts.net:4700".into(),
            agent_token: reference,
            ..SimlockHostConfig::new("sim-1")
        };
        let mut sim_ed = SimlockTokenEditor::new(&sim_ref, false);
        sim_ed.switch_to_stored();
        assert!(sim_ed.token_for_save().is_empty());
        assert_eq!(
            sim_ed.check_payload("sim-1", "https://gw.example.ts.net:4700"),
            json!({ "gatewayUrl": "https://gw.example.ts.net:4700", "agentToken": "" })
        );
    }

    #[test]
    fn add_source_menu_reopen_guard_blocks_immediate_mouse_up_reopen() {
        let mut state = DevicesState {
            pending: None,
            retrying: None,
            tool_popover: None,
            tool_busy: None,
            tool_error: None,
            add_menu_open: false,
            add_menu_dismissed: None,
            row_checks: HashMap::new(),
            editor: None,
            _subscription: None,
            _lifecycle: Vec::new(),
            editor_focus: None,
            editor_target: None,
            selected_environment: None,
            previous_focus: None,
            action_menu: None,
            platform_menu: None,
            feeds: HashMap::new(),
            check_revision: 0,
        };
        let t0 = Instant::now();
        state.toggle_add_menu(t0);
        assert!(state.add_menu_open);

        // Mouse down on trigger fires on_mouse_down_out, then mouse up fires on_click 20ms later.
        state.dismiss_add_menu_outside(t0 + Duration::from_millis(10));
        assert!(!state.add_menu_open);
        state.toggle_add_menu(t0 + Duration::from_millis(30));
        assert!(!state.add_menu_open, "reopen guard keeps menu closed on mouse-up");

        // Clicking again after the guard window opens the menu normally.
        state.toggle_add_menu(t0 + Duration::from_millis(300));
        assert!(state.add_menu_open);
    }
}
