//! The connect screen: connect to T3 Code on this Mac in one click, pick a saved environment, or
//! add one with a pairing link or over SSH. Over a live connection it is the environment switcher.
//! Web: `settings/SavedEnvironmentRows.tsx` (rows and their status copy),
//! `settings/AddEnvironmentDialog.tsx` (the Remote link and SSH modes), `ConnectionStatusDot.tsx`.
//! T3 Connect rows mirror `components/cloud/CloudEnvironmentConnectList.tsx`.
//! The SSH host suggestions list inline under the field rather than in a popup.

use std::collections::{HashMap, HashSet};

use gpui::{
    App, ClipboardItem, Context, Div, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div,
    prelude::*, px, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::local::LocalServer;
use t3_client::ssh::{DiscoveredSshHost, SshTarget};
use t3_client::status::{DotTone, HopResult, connection_diagnosis_text, diagnose, host_freeze_notice};
use t3_client::store::SavedEnvironment;
use t3_core::compatibility::resolve_version_mismatch;
use t3_core::connect::{FirstFactorKind, SecondFactorKind, SocialProvider};
use t3_core::local_server::{LocalServerFailure, LocalServerFailureKind, LocalServerPhase, describe_startup_recovery_offer};
use t3_core::pairing::{
    ANOTHER_ADDRESS, LinkableMachine, link_destination, link_machine_badge as resolve_link_machine_badge,
    link_machine_select_label, session_has_legacy_permissions,
};
use t3_core::relay::EnvironmentHealth;
use t3_core::ssh::{filter_hosts, format_target, parse_manual_target};

use crate::app_state::{AppEvent, AppState};
use crate::cloud::CloudAuthFlow;
use crate::environments::{ConnectScreen, PairingSource};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

/// How "Add environment" pairs (`savedBackendMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddMode {
    Link,
    Ssh,
}

/// `max-h-72` of the host list, in rows of `h-8`.
const HOST_ROW_HEIGHT: f32 = 32.;
const HOST_LIST_MAX_HEIGHT: f32 = 288.;

pub struct ConnectView {
    state: Entity<AppState>,
    link: Entity<InputState>,
    code: Entity<InputState>,
    cloud_email: Entity<InputState>,
    cloud_code: Entity<InputState>,
    cloud_password: Entity<InputState>,
    cloud_second_factor: Entity<InputState>,
    cloud_first_factor: FirstFactorKind,
    cloud_second_factor_kind: SecondFactorKind,
    mode: AddMode,
    ssh_host: Entity<InputState>,
    ssh_username: Entity<InputState>,
    ssh_port: Entity<InputState>,
    /// The unsaved discovered hosts that match the host field, as last rendered.
    ssh_matches: Vec<DiscoveredSshHost>,
    /// The saved environment whose Remove awaits confirmation.
    confirm_remove: Option<String>,
    /// Saved environment IDs whose connection diagnosis panel is expanded (`CN-STATUS`).
    expanded_diagnosis: HashSet<String>,
    /// Whether the hosted local server failure output tail is expanded (`CN-LOCAL`).
    local_diagnostics_expanded: bool,
    /// Cached dismissal state per environment ID for the legacy-permission update notice (`CN-REPAIR`).
    /// Loaded from UI state at most once per key so `render()` never hits disk every frame.
    dismissed_permission_notice: HashMap<String, bool>,
    /// A pasted link is pairing; the field clears once it succeeds.
    submitted: bool,
    /// An SSH host is being added; the fields clear once it succeeds.
    ssh_submitted: bool,
    _subscriptions: Vec<Subscription>,
}

/// Extracts a human-readable address from a `CandidateRoute::id` (`direct:<url>`,
/// `learned:<env>:<origin>@<cred>`, `ssh:<url>`) for refused-route diagnostics (`pingdotgg/t3code#33199`).
pub(crate) fn refused_route_display_address(route_id: &str) -> String {
    if let Some(rest) = route_id.strip_prefix("direct:") {
        return rest.to_owned();
    }
    if let Some(rest) = route_id.strip_prefix("ssh:") {
        return rest.to_owned();
    }
    if let Some(rest) = route_id.strip_prefix("learned:") {
        let without_cred = rest.rsplit_once('@').map_or(rest, |(left, _)| left);
        if let Some((_, origin)) = without_cred.split_once(':') {
            return origin.to_owned();
        }
        return without_cred.to_owned();
    }
    route_id.to_owned()
}

/// Renders routes whose host refused this device's sign-in (`EnvironmentRoutesList.tsx`,
/// `pingdotgg/t3code#33199`).
fn render_refused_routes_list(refused_route_ids: &[String], cx: &App) -> Div {
    let p = palette(cx);
    v_flex()
        .mt(px(8.))
        .pt(px(8.))
        .border_t_1()
        .border_color(hsla(p.border).opacity(0.5))
        .gap(px(6.))
        .children(refused_route_ids.iter().map(|route_id| {
            let address = refused_route_display_address(route_id);
            v_flex()
                .gap(px(2.))
                .child(
                    h_flex()
                        .gap(px(6.))
                        .items_center()
                        .child(
                            div()
                                .px(px(4.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .bg(hsla(p.destructive).opacity(0.1))
                                .text_xs()
                                .text_color(hsla(p.destructive_foreground))
                                .child(crate::i18n::t(cx, "Sign-in refused")),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(hsla(p.muted_foreground))
                                .child(address),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(crate::i18n::t(cx, "The host refused this device's sign-in. Pair this address again with Add route.")),
                )
        }))
}

/// `ConnectionStatusDot`: an 8px dot in a 12px box. Static: the web's ping would repaint.
pub fn status_dot(tone: Option<DotTone>, cx: &App) -> Div {
    let p = palette(cx);
    let color = match tone {
        Some(DotTone::Success) => hsla(p.success),
        Some(DotTone::Warning) => hsla(p.warning),
        Some(DotTone::Destructive) => hsla(p.destructive),
        None => hsla(p.muted_foreground).opacity(0.4),
    };
    div().size(px(12.)).flex_none().flex().items_center().justify_center().child(div().size(px(8.)).rounded_full().bg(color))
}

/// `EnvironmentMachineIcon` (`apps/web/src/components/EnvironmentMachineIcon.tsx`).
pub fn environment_machine_icon(kind: Option<&str>) -> Icon {
    match kind {
        Some("cloud") => Icon::Cloud,
        Some("linux") => Icon::Linux,
        Some("desktop") => Icon::Monitor,
        Some("laptop") => Icon::Laptop,
        Some("mac-mini") => Icon::MacMini,
        Some("mac-studio") => Icon::MacStudio,
        _ => Icon::Server,
    }
}

/// `LinkAddressWarning` (`apps/web/src/components/settings/LinkMachineSelect.tsx`):
/// shown wherever a link goes to an address this app does not already know for a machine.
pub fn link_address_warning(address: &str, cx: &App) -> Div {
    let p = palette(cx);
    let destination = link_destination(address);
    v_flex()
        .px(px(12.))
        .py(px(10.))
        .gap(px(4.))
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.warning).opacity(0.35))
        .bg(hsla(p.warning_surface))
        .child(
            h_flex()
                .gap(px(6.))
                .items_center()
                .child(Icon::TriangleAlert.render(px(14.), hsla(p.warning_foreground)))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.warning_foreground)).child(crate::i18n::t(cx, "Check this address"))),
        )
        .child(
            div()
                .text_xs()
                .line_height(px(18.))
                .text_color(hsla(p.muted_foreground))
                .child(crate::i18n::t(cx, "This app cannot confirm it is one of your machines. The pairing code you paste is sent there.")),
        )
        .child(
            div()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(hsla(p.foreground))
                .child(destination),
        )
}

/// `LinkMachineSelect` (`apps/web/src/components/settings/LinkMachineSelect.tsx`):
/// the user's other machines, including those on T3 Connect, followed by `"Another address…"`.
pub(crate) fn render_link_machine_select(
    id: impl Into<SharedString>,
    machines: &[LinkableMachine],
    selected: Option<&str>,
    disabled: bool,
    on_select: impl Fn(String, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    use crate::i18n::t;
    use crate::settings::controls::{Select, SelectOption};

    let options = machines.iter().map(|machine| {
        let mut option = SelectOption::new(machine.environment_id.clone(), machine.label.clone())
            .icon(environment_machine_icon(machine.machine.as_deref()));
        if let Some(badge) = resolve_link_machine_badge(machine) {
            option = option.description(t(cx, badge.label()));
        }
        option
    }).chain(std::iter::once(SelectOption::new(ANOTHER_ADDRESS, t(cx, "Another address…")))).collect();
    let mut select = Select::new(id.into(), options)
        .label(t(cx, &link_machine_select_label(machines, selected)).to_owned())
        .popup_width(320.)
        .disabled(disabled)
        .show_check()
        .on_change(move |choice, window, cx| on_select(choice.to_string(), window, cx));
    if let Some(selected) = selected {
        select = select.selected(selected.to_owned());
    }
    v_flex().gap(px(6.))
        .child(label(t(cx, "Machine"), cx))
        .child(select)
}

pub(crate) fn link_access_picker(
    id: impl Into<gpui::ElementId>,
    selected: &str,
    disabled: bool,
    on_select: impl Fn(SharedString, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    use crate::i18n::t;
    use crate::settings::controls::{Select, SelectOption};
    use t3_core::timeline::{link_access_description, link_access_label};

    let options = [
        ("read-only", Icon::Eye),
        ("approval-required", Icon::Shield),
        ("auto-accept-edits", Icon::Check),
        ("full-access", Icon::LockOpen),
    ].into_iter().map(|(level, icon)| {
        SelectOption::new(level, t(cx, link_access_label(level)))
            .icon(icon)
            .description(t(cx, link_access_description(level)))
    }).collect();
    v_flex().gap(px(6.))
        .child(label(t(cx, "Access level"), cx))
        .child(Select::new(id, options).selected(selected.to_owned()).show_check()
            .popup_width(360.).disabled(disabled).on_change(on_select))
        .child(muted(t(cx, "An agent never gets more access there than its own mode here."), cx))
}

fn label(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(palette(cx).muted_foreground)).child(text.into())
}

fn muted(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_xs().line_height(px(18.)).text_color(hsla(palette(cx).muted_foreground)).child(text.into())
}

fn error_text(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_xs().line_height(px(18.)).text_color(hsla(palette(cx).destructive_foreground)).child(text.into())
}

/// The last pairing failure from `source`, shown beside the control that started it.
pub(crate) fn pairing_error(connect: &ConnectScreen, source: PairingSource) -> Option<String> {
    connect.error.as_ref().filter(|(failed, _)| *failed == source).map(|(_, message)| message.clone())
}

/// Why T3 Code on this Mac isn't offered: still looking, or not running.
pub(crate) fn discovery_message(discovering: bool) -> &'static str {
    if discovering { "Looking for T3 Code on this Mac…" } else { "T3 Code isn't running on this Mac. Open it to connect with one click." }
}

/// The local server's details line (label, version, address), and why it cannot connect, if so.
pub(crate) fn local_details(server: &LocalServer) -> (String, Option<String>) {
    let mut details = vec![server.label().to_owned()];
    details.extend(server.server_version().map(|version| format!("version {version}")));
    details.push(server.state.origin.clone());
    (details.join(" · "), server.mismatch().map(|mismatch| mismatch.message(server.label())))
}

/// `Alert variant="error"`: radius 12, `error` border at 32%, `error_surface`.
fn error_alert(text: impl Into<SharedString>, cx: &App) -> Div {
    let p = palette(cx);
    div()
        .px(px(14.))
        .py(px(12.))
        .rounded(px(12.))
        .border_1()
        .border_color(hsla(p.error).opacity(0.32))
        .bg(hsla(p.error_surface))
        .text_sm()
        .text_color(hsla(p.error_foreground))
        .child(text.into())
}

/// A field label: `text-xs font-medium text-foreground`, 6px above the control.
fn field(label: &str, control: impl IntoElement, cx: &App) -> Div {
    v_flex().gap(px(6.)).child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(palette(cx).foreground)).child(crate::i18n::t(cx, label).to_owned())).child(control)
}

/// `SettingsGroup`: radius 12, `border` at 60%, `card` at 40%.
fn card(cx: &App) -> Div {
    let p = palette(cx);
    v_flex().rounded(px(12.)).border_1().border_color(hsla(p.border).opacity(0.6)).bg(hsla(p.card).opacity(0.4))
}

impl ConnectView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let link = cx.new(|cx| InputState::new(window, cx).placeholder(crate::i18n::t(cx, "https://your-server:3773/pair#token=… or host:3773")));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder(crate::i18n::t(cx, "Optional pairing code")));
        let cloud_email = cx.new(|cx| InputState::new(window, cx).placeholder(crate::i18n::t(cx, "you@example.com")));
        let cloud_code = cx.new(|cx| InputState::new(window, cx).placeholder(crate::i18n::t(cx, "6-digit code")));
        let cloud_password = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(crate::i18n::t(cx, "Password")));
        let cloud_second_factor = cx.new(|cx| InputState::new(window, cx).placeholder(crate::i18n::t(cx, "Verification code")));
        let ssh_host = cx.new(|cx| InputState::new(window, cx).placeholder(crate::i18n::t(cx, "Search hosts or type devbox")));
        let ssh_username = cx.new(|cx| InputState::new(window, cx).placeholder("root"));
        let ssh_port = cx.new(|cx| InputState::new(window, cx).placeholder("22"));
        let mut subscriptions = vec![
            cx.subscribe_in(&link, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.add(cx);
                }
            }),
            cx.subscribe_in(&code, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.add(cx);
                }
            }),
            cx.subscribe_in(&cloud_email, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_cloud_email(cx);
                }
            }),
            cx.subscribe_in(&cloud_code, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_cloud_first_factor(FirstFactorKind::EmailCode, cx);
                }
            }),
            cx.subscribe_in(&cloud_password, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_cloud_first_factor(FirstFactorKind::Password, cx);
                }
            }),
            cx.subscribe_in(&cloud_second_factor, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let kind = this.effective_second_factor_kind(cx);
                    this.submit_cloud_second_factor(kind, cx);
                }
            }),
            cx.subscribe_in(&state, window, |this, state, event: &AppEvent, window, cx| {
                if !matches!(event, AppEvent::Phase) {
                    return;
                }
                let (pairing, failed, clerk_signed_in, flow_idle) = {
                    let app = state.read(cx);
                    (
                        app.connect.pairing.is_some(),
                        app.connect.error.is_some(),
                        app.cloud.has_clerk_session,
                        matches!(app.cloud.auth_flow, CloudAuthFlow::Idle),
                    )
                };
                if clerk_signed_in && flow_idle {
                    for input in [&this.cloud_email, &this.cloud_code, &this.cloud_password, &this.cloud_second_factor] {
                        if !input.read(cx).value().is_empty() {
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                }
                if this.submitted && !pairing {
                    this.submitted = false;
                    if !failed {
                        this.link.update(cx, |input, cx| input.set_value("", window, cx));
                        this.code.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                }
                if this.ssh_submitted && !pairing {
                    this.ssh_submitted = false;
                    if !failed {
                        for input in [&this.ssh_host, &this.ssh_username, &this.ssh_port] {
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                }
                cx.notify();
            }),
        ];
        // Typing filters the hosts; Enter adds the typed host (`handleSavedBackendSshFieldKeyDown`).
        for input in [&ssh_host, &ssh_username, &ssh_port] {
            subscriptions.push(cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } if !this.ssh_host.read(cx).value().trim().is_empty() => this.add_ssh(cx),
                _ => {}
            }));
        }
        ConnectView {
            state,
            link,
            code,
            cloud_email,
            cloud_code,
            cloud_password,
            cloud_second_factor,
            cloud_first_factor: FirstFactorKind::EmailCode,
            cloud_second_factor_kind: SecondFactorKind::Totp,
            mode: AddMode::Link,
            ssh_host,
            ssh_username,
            ssh_port,
            ssh_matches: Vec::new(),
            confirm_remove: None,
            expanded_diagnosis: HashSet::new(),
            local_diagnostics_expanded: false,
            dismissed_permission_notice: HashMap::new(),
            submitted: false,
            ssh_submitted: false,
            _subscriptions: subscriptions,
        }
    }

    fn submit_cloud_email(&mut self, cx: &mut Context<Self>) {
        let email = self.cloud_email.read(cx).value().to_string();
        self.state.update(cx, |state, cx| state.start_email_cloud_sign_in(email, cx));
    }

    fn submit_cloud_first_factor(&mut self, kind: FirstFactorKind, cx: &mut Context<Self>) {
        let secret = match kind {
            FirstFactorKind::EmailCode => self.cloud_code.read(cx).value().to_string(),
            FirstFactorKind::Password => self.cloud_password.read(cx).value().to_string(),
        };
        self.state
            .update(cx, |state, cx| state.submit_email_cloud_first_factor(kind, secret, cx));
    }

    fn effective_second_factor_kind(&self, cx: &App) -> SecondFactorKind {
        if let CloudAuthFlow::SecondFactor(pending) = &self.state.read(cx).cloud.auth_flow {
            let supports_totp = pending.challenge.supports_totp;
            let supports_backup = pending.challenge.supports_backup_code;
            if self.cloud_second_factor_kind == SecondFactorKind::BackupCode && supports_backup {
                return SecondFactorKind::BackupCode;
            }
            if supports_totp {
                return SecondFactorKind::Totp;
            }
            if supports_backup {
                return SecondFactorKind::BackupCode;
            }
        }
        self.cloud_second_factor_kind
    }

    fn submit_cloud_second_factor(&mut self, kind: SecondFactorKind, cx: &mut Context<Self>) {
        let code = self.cloud_second_factor.read(cx).value().to_string();
        self.state
            .update(cx, |state, cx| state.submit_cloud_second_factor(kind, code, cx));
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).value().to_string();
        let code = self.code.read(cx).value().to_string();
        let code = (!code.trim().is_empty()).then_some(code);
        self.state.update(cx, |state, cx| state.pair_link_with_code(&link, code.as_deref(), cx));
        self.submitted = self.state.read(cx).connect.pairing == Some(PairingSource::Link);
    }

    /// The SSH fields as a target (`parseManualDesktopSshTarget`), added over SSH.
    fn add_ssh(&mut self, cx: &mut Context<Self>) {
        let value = |input: &Entity<InputState>, cx: &App| input.read(cx).value().to_string();
        let parsed = parse_manual_target(&value(&self.ssh_host, cx), &value(&self.ssh_username, cx), &value(&self.ssh_port, cx));
        self.state.update(cx, |state, cx| match parsed {
            Ok(target) => state.add_ssh(target, cx),
            Err(message) => state.fail_pairing(PairingSource::Ssh, message, cx),
        });
        self.ssh_submitted = self.state.read(cx).connect.pairing == Some(PairingSource::Ssh);
    }

    /// A suggested host connects at once, preserving discovered `hostname`, `username`, and `port`
    /// (e.g. `[host]:port` from `known_hosts`, `AddEnvironmentDialog.tsx:301`).
    fn add_ssh_host(&mut self, target: SshTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).connect.pairing.is_some() {
            return;
        }
        let alias = target.alias.clone();
        self.ssh_host.update(cx, |input, cx| input.set_value(alias, window, cx));
        self.state.update(cx, |state, cx| state.add_ssh(target, cx));
        self.ssh_submitted = self.state.read(cx).connect.pairing == Some(PairingSource::Ssh);
    }

    fn set_mode(&mut self, mode: AddMode, cx: &mut Context<Self>) {
        self.mode = mode;
        // The SSH config is read again each time the SSH form shows.
        if mode == AddMode::Ssh {
            self.state.update(cx, |state, cx| state.refresh_ssh_hosts(cx));
        }
        cx.notify();
    }

    fn render_header(&self, switching: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let subtitle = crate::i18n::t(
            cx,
            if switching {
                "Switch to another environment, or pair a new one."
            } else {
                "Pick an environment, or pair with a T3 Code server."
            },
        );
        h_flex()
            .w_full()
            .items_start()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .gap_1()
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(crate::i18n::t(cx, "Connect to T3 Code")))
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(subtitle)),
            )
            .when(switching, |this| {
                this.child(Button::new("connect-cancel", ButtonVariant::Outline, ButtonSize::Sm).label(crate::i18n::t(cx, "Cancel")).on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |state, cx| state.close_switcher(cx));
                })))
            })
    }

    /// The re-pair prompt after a server rejected this device (`pingdotgg/t3code#38041`: asks to
    /// pair again and names the rejected request's trace ID when present).
    fn render_rejected(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let state = self.state.read(cx);
        let rejected = state.connect.rejected.clone()?;
        let can_reconnect_relay = state.cloud.has_clerk_session
            && state
                .environments
                .get(&rejected.id)
                .is_some_and(SavedEnvironment::is_relay_managed);
        let can_pair_again = can_reconnect_relay
            || rejected.ssh
            || (rejected.local && state.local_saved_environment().is_some_and(|local| local.id == rejected.id));
        let busy = state.connect.pairing.is_some() || state.cloud.connecting_env_id.is_some();
        let p = palette(cx);
        let trace_id = rejected
            .trace_id
            .clone()
            .or_else(|| t3_client::status::extract_trace_id(&rejected.reason).map(str::to_owned));
        let heading = crate::i18n::t_fmt(
            cx,
            "T3 Code on {host} did not accept this device",
            &[("host", t3_core::i18n::IcuArg::Text(std::borrow::Cow::Borrowed(&rejected.label)))],
        );
        let body = if can_reconnect_relay {
            crate::i18n::t_fmt(
                cx,
                "This device's sign-in to {host} expired or was revoked. Reconnect through T3 Connect.",
                &[("host", t3_core::i18n::IcuArg::Text(std::borrow::Cow::Borrowed(&rejected.label)))],
            )
        } else if can_pair_again {
            crate::i18n::t_fmt(
                cx,
                "This device's sign-in to {host} expired or was revoked. Pair again to reconnect.",
                &[("host", t3_core::i18n::IcuArg::Text(std::borrow::Cow::Borrowed(&rejected.label)))],
            )
        } else {
            crate::i18n::t_fmt(
                cx,
                "This device's sign-in to {host} expired or was revoked. Pair it again with a new pairing link from {host}.",
                &[("host", t3_core::i18n::IcuArg::Text(std::borrow::Cow::Borrowed(&rejected.label)))],
            )
        };
        let trace_prefix = crate::i18n::t(cx, "Trace ID");
        let repair_label = crate::i18n::t(
            cx,
            if can_reconnect_relay {
                if busy { "Connecting…" } else { "Reconnect" }
            } else if busy {
                "Pairing…"
            } else {
                "Pair again"
            },
        );
        Some(
            v_flex()
                .px(px(14.))
                .py(px(12.))
                .gap(px(6.))
                .rounded(px(12.))
                .border_1()
                .border_color(hsla(p.error).opacity(0.32))
                .bg(hsla(p.error_surface))
                .text_sm()
                .text_color(hsla(p.error_foreground))
                .child(div().font_weight(FontWeight::MEDIUM).child(heading))
                .child(div().opacity(0.85).child(body))
                .when_some(trace_id, |this, trace| {
                    this.child(
                        div()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .opacity(0.85)
                            .child(format!("{trace_prefix}: {trace}")),
                    )
                })
                .when(can_pair_again, |this| {
                    let (id, ssh) = (rejected.id.clone(), rejected.ssh);
                    this.child(div().pt(px(2.)).child(
                        Button::new("connect-repair", ButtonVariant::Default, ButtonSize::Sm)
                            .label(repair_label)
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.update(cx, |state, cx| {
                                    if can_reconnect_relay {
                                        state.connect_environment(&id, cx);
                                    } else if ssh {
                                        state.pair_ssh_again(&id, cx);
                                    } else {
                                        state.pair_local(cx);
                                    }
                                })
                            })),
                    ))
                }),
        )
    }

    /// Structured diagnostic panel when hosting the local server fails (`CN-LOCAL`).
    fn render_local_failure(&self, failure: &LocalServerFailure, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let recovery = (failure.kind != LocalServerFailureKind::ManagedPolicy)
            .then(|| describe_startup_recovery_offer(&failure.recovery));
        let has_output = !failure.output_tail.trim().is_empty();
        let expanded = self.local_diagnostics_expanded;
        let copy_text = self
            .state
            .read(cx)
            .local_server_diagnostics()
            .unwrap_or_else(|| failure.message.clone());
        let details_label = crate::i18n::t(cx, if expanded { "Hide details" } else { "Details" });
        let copy_label = crate::i18n::t(cx, "Copy diagnostics");

        v_flex()
            .px(px(12.))
            .py(px(10.))
            .gap(px(6.))
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.error).opacity(0.32))
            .bg(hsla(p.error_surface))
            .text_xs()
            .child(div().line_height(px(18.)).text_color(hsla(p.error_foreground)).child(failure.message.clone()))
            .children(recovery.map(|text| div().line_height(px(18.)).text_color(hsla(p.muted_foreground)).child(text)))
            .when(failure.restart_suppressed, |this| {
                this.child(
                    div()
                        .line_height(px(18.))
                        .text_color(hsla(p.muted_foreground))
                        .child(crate::i18n::t(cx, "Automatic restart is disabled for this error.")),
                )
            })
            .child(
                h_flex()
                    .gap(px(6.))
                    .pt(px(2.))
                    .when(has_output, |this| {
                        this.child(
                            Button::new("connect-local-diag-toggle", ButtonVariant::GhostMuted, ButtonSize::Compact)
                                .label(details_label)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.local_diagnostics_expanded = !this.local_diagnostics_expanded;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("connect-local-diag-copy", ButtonVariant::Outline, ButtonSize::Compact)
                            .icon(Icon::Copy)
                            .label(copy_label)
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                            }),
                    ),
            )
            .when(has_output && expanded, |this| {
                this.child(
                    div()
                        .mt(px(4.))
                        .p(px(8.))
                        .rounded(px(6.))
                        .bg(hsla(p.background).opacity(0.7))
                        .border_1()
                        .border_color(hsla(p.border).opacity(0.5))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .line_height(px(16.))
                        .text_color(hsla(p.muted_foreground))
                        .child(failure.output_tail.trim().to_owned()),
                )
            })
    }

    /// "Connect to T3 Code on this Mac", embedded local server hosting controls, or diagnostics.
    fn render_local(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let connect = &state.connect;
        let error = pairing_error(connect, PairingSource::Local);
        let local_phase = state.local_host_phase.clone();
        let auto_start = state.local_server_auto_start;
        let hosted = state.is_hosting_local_server();
        let starting = local_phase.is_starting();
        let stopping = local_phase.is_stopping();
        let failure = local_phase.failure().cloned();
        let p = palette(cx);

        let status_badge = match &local_phase {
            LocalServerPhase::Running { .. } => Some((DotTone::Success, crate::i18n::t(cx, "Running"))),
            LocalServerPhase::Starting { .. } => Some((DotTone::Warning, crate::i18n::t(cx, "Starting…"))),
            LocalServerPhase::Stopping { .. } => Some((DotTone::Warning, crate::i18n::t(cx, "Stopping…"))),
            LocalServerPhase::Failed(_) => Some((DotTone::Destructive, crate::i18n::t(cx, "Stopped"))),
            LocalServerPhase::Stopped => None,
        };
        let auto_start_toggle = {
            let button = Button::new(
                "connect-local-autostart",
                if auto_start { ButtonVariant::Outline } else { ButtonVariant::GhostMuted },
                ButtonSize::Compact,
            )
            .label(crate::i18n::t(cx, "Auto-start"))
            .tooltip(crate::i18n::t(cx, "Start the local T3 Code server automatically when opening this app."))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.update(cx, |state, cx| state.set_local_server_auto_start(!auto_start, cx));
            }));
            if auto_start { button.icon(Icon::Check) } else { button }
        };

        let local_title = crate::i18n::t(cx, "T3 Code on this Mac");
        let local_icon = state.local_saved_environment()
            .or_else(|| state.active_environment().filter(|environment| environment.local))
            .and_then(|environment| state.fleet.environment(&environment.id))
            .and_then(|view| view.config.config.as_ref())
            .map_or(Icon::Server, Icon::machine);
        let Some(server) = connect.local_server.clone() else {
            let message = crate::i18n::t(
                cx,
                if starting {
                    "Starting T3 Code on this Mac…"
                } else if stopping {
                    "Stopping T3 Code on this Mac…"
                } else {
                    discovery_message(connect.discovering)
                },
            );
            let primary_start = state.environments.environments.is_empty();
            let start_label = crate::i18n::t(cx, if starting { "Starting…" } else { "Start local server" });
            let check_label = crate::i18n::t(cx, "Check again");
            return card(cx)
                .px(px(16.))
                .py(px(14.))
                .gap(px(10.))
                .child(
                    h_flex()
                        .gap(px(10.))
                        .items_center()
                        .child(local_icon.render(px(16.), hsla(p.muted_foreground)))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    h_flex()
                                        .gap(px(6.))
                                        .items_center()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(hsla(p.foreground))
                                                .child(local_title),
                                        )
                                        .when_some(status_badge, |this, (tone, text)| {
                                            this.child(
                                                h_flex()
                                                    .gap(px(4.))
                                                    .items_center()
                                                    .child(status_dot(Some(tone), cx))
                                                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(text)),
                                            )
                                        }),
                                )
                                .child(muted(message, cx)),
                        ),
                )
                .child(
                    h_flex()
                        .gap(px(8.))
                        .items_center()
                        .flex_wrap()
                        .child(
                            Button::new(
                                "connect-local-start",
                                if primary_start { ButtonVariant::Default } else { ButtonVariant::Outline },
                                ButtonSize::Sm,
                            )
                            .icon(Icon::Play)
                            .label(start_label)
                            .disabled(starting || stopping || connect.pairing.is_some())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.update(cx, |state, cx| state.start_local_server(true, cx));
                            })),
                        )
                        .child(
                            Button::new("connect-local-refresh", ButtonVariant::GhostMuted, ButtonSize::Compact)
                                .icon(Icon::RefreshCw)
                                .label(check_label)
                                .disabled(connect.discovering || starting || stopping)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.update(cx, |state, cx| state.refresh_local_server(cx));
                                })),
                        )
                        .child(auto_start_toggle),
                )
                .children(failure.as_ref().map(|failure| self.render_local_failure(failure, cx)))
                .when(failure.is_none(), |this| this.children(error.map(|error| error_text(error, cx))))
                .into_any_element();
        };

        let busy = connect.pairing == Some(PairingSource::Local);
        let (details, blocked) = local_details(&server);
        let connect_label = crate::i18n::t(cx, if busy { "Connecting…" } else { "Connect to T3 Code on this Mac" });
        let stop_label = crate::i18n::t(cx, if stopping { "Stopping…" } else { "Stop server" });
        card(cx)
            .px(px(16.))
            .py(px(14.))
            .gap(px(12.))
            .child(
                h_flex()
                    .gap(px(10.))
                    .child(local_icon.render(px(16.), hsla(p.muted_foreground)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .items_center()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(hsla(p.foreground))
                                            .child(local_title),
                                    )
                                    .when_some(status_badge, |this, (tone, text)| {
                                        this.child(
                                            h_flex()
                                                .gap(px(4.))
                                                .items_center()
                                                .child(status_dot(Some(tone), cx))
                                                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(text)),
                                        )
                                    }),
                            )
                            .child(div().truncate().child(muted(details, cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .items_center()
                    .flex_wrap()
                    .child(
                        Button::new("connect-local", ButtonVariant::Default, ButtonSize::Default)
                            .label(connect_label)
                            .disabled(busy || blocked.is_some() || connect.pairing.is_some() || stopping)
                            .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.connect_local(cx)))),
                    )
                    .when(hosted || stopping, |this| {
                        this.child(
                            Button::new("connect-local-stop", ButtonVariant::Outline, ButtonSize::Sm)
                                .icon(Icon::Square)
                                .label(stop_label)
                                .disabled(stopping)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.update(cx, |state, cx| state.stop_local_server(cx));
                                })),
                        )
                    })
                    .child(auto_start_toggle),
            )
            .children(blocked.or(error).map(|message| error_text(message, cx)))
            .into_any_element()
    }

    fn render_cloud_auth_controls(&self, cx: &mut Context<Self>) -> Div {
        let cloud = &self.state.read(cx).cloud;
        let busy = cloud.busy;
        let flow = cloud.auth_flow.clone();
        match flow {
            CloudAuthFlow::OAuth(provider) => {
                let waiting = crate::i18n::t_fmt(
                    cx,
                    "Signing in with {provider}…",
                    &[("provider", t3_core::i18n::IcuArg::Text(std::borrow::Cow::Borrowed(provider.label())))],
                );
                h_flex()
                    .gap(px(8.))
                    .items_center()
                    .child(muted(waiting, cx))
                    .child(
                        Button::new("cloud-cancel", ButtonVariant::GhostMuted, ButtonSize::Sm)
                            .label(crate::i18n::t(cx, "Cancel"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.update(cx, |state, cx| state.cancel_cloud_sign_in(cx));
                            })),
                    )
            }
            CloudAuthFlow::EmailFirstFactor { pending, code_sent } => {
                let supports_code = pending.challenge.supports_email_code;
                let supports_password = pending.challenge.supports_password;
                let active_kind = if self.cloud_first_factor == FirstFactorKind::Password && supports_password {
                    FirstFactorKind::Password
                } else if supports_code {
                    FirstFactorKind::EmailCode
                } else {
                    FirstFactorKind::Password
                };
                let mut panel = v_flex()
                    .gap(px(8.))
                    .child(muted(pending.challenge.identifier.clone(), cx));
                if supports_code && supports_password {
                    panel = panel.child(
                        h_flex()
                            .gap(px(6.))
                            .child(
                                Button::new(
                                    "cloud-factor-code",
                                    if active_kind == FirstFactorKind::EmailCode {
                                        ButtonVariant::Default
                                    } else {
                                        ButtonVariant::Outline
                                    },
                                    ButtonSize::Compact,
                                )
                                .label(crate::i18n::t(cx, "Email code"))
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cloud_first_factor = FirstFactorKind::EmailCode;
                                    cx.notify();
                                })),
                            )
                            .child(
                                Button::new(
                                    "cloud-factor-password",
                                    if active_kind == FirstFactorKind::Password {
                                        ButtonVariant::Default
                                    } else {
                                        ButtonVariant::Outline
                                    },
                                    ButtonSize::Compact,
                                )
                                .label(crate::i18n::t(cx, "Password"))
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cloud_first_factor = FirstFactorKind::Password;
                                    cx.notify();
                                })),
                            ),
                    );
                }
                match active_kind {
                    FirstFactorKind::EmailCode => {
                        if !code_sent {
                            panel = panel.child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(
                                        Button::new("cloud-send-code", ButtonVariant::Default, ButtonSize::Sm)
                                            .label(crate::i18n::t(cx, "Send verification code"))
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.state.update(cx, |state, cx| state.send_email_cloud_code(cx));
                                            })),
                                    )
                                    .child(
                                        Button::new("cloud-email-back", ButtonVariant::GhostMuted, ButtonSize::Sm)
                                            .label(crate::i18n::t(cx, "Back"))
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.state.update(cx, |state, cx| state.cancel_cloud_sign_in(cx));
                                            })),
                                    ),
                            );
                        } else {
                            panel = panel
                                .child(field("Verification code", Input::new(&self.cloud_code).disabled(busy), cx))
                                .child(
                                    h_flex()
                                        .gap(px(8.))
                                        .child(
                                            Button::new("cloud-verify-code", ButtonVariant::Default, ButtonSize::Sm)
                                                .label(crate::i18n::t(cx, "Verify"))
                                                .disabled(busy)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.submit_cloud_first_factor(FirstFactorKind::EmailCode, cx);
                                                })),
                                        )
                                        .child(
                                            Button::new("cloud-resend-code", ButtonVariant::Outline, ButtonSize::Sm)
                                                .label(crate::i18n::t(cx, "Resend code"))
                                                .disabled(busy)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.state.update(cx, |state, cx| state.send_email_cloud_code(cx));
                                                })),
                                        )
                                        .child(
                                            Button::new("cloud-code-back", ButtonVariant::GhostMuted, ButtonSize::Sm)
                                                .label(crate::i18n::t(cx, "Back"))
                                                .disabled(busy)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.state.update(cx, |state, cx| state.cancel_cloud_sign_in(cx));
                                                })),
                                        ),
                                );
                        }
                    }
                    FirstFactorKind::Password => {
                        panel = panel
                            .child(field("Password", Input::new(&self.cloud_password).disabled(busy), cx))
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(
                                        Button::new("cloud-submit-password", ButtonVariant::Default, ButtonSize::Sm)
                                            .label(crate::i18n::t(cx, "Sign in"))
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.submit_cloud_first_factor(FirstFactorKind::Password, cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("cloud-password-back", ButtonVariant::GhostMuted, ButtonSize::Sm)
                                            .label(crate::i18n::t(cx, "Back"))
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.state.update(cx, |state, cx| state.cancel_cloud_sign_in(cx));
                                            })),
                                    ),
                            );
                    }
                }
                panel
            }
            CloudAuthFlow::SecondFactor(pending) => {
                let supports_totp = pending.challenge.supports_totp;
                let supports_backup = pending.challenge.supports_backup_code;
                let active_kind = self.effective_second_factor_kind(cx);
                let mut panel = v_flex()
                    .gap(px(8.))
                    .child(muted(crate::i18n::t(cx, "Two-factor verification"), cx));
                if supports_totp && supports_backup {
                    panel = panel.child(
                        h_flex()
                            .gap(px(6.))
                            .child(
                                Button::new(
                                    "cloud-2fa-totp",
                                    if active_kind == SecondFactorKind::Totp {
                                        ButtonVariant::Default
                                    } else {
                                        ButtonVariant::Outline
                                    },
                                    ButtonSize::Compact,
                                )
                                .label(crate::i18n::t(cx, "Authenticator app"))
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cloud_second_factor_kind = SecondFactorKind::Totp;
                                    cx.notify();
                                })),
                            )
                            .child(
                                Button::new(
                                    "cloud-2fa-backup",
                                    if active_kind == SecondFactorKind::BackupCode {
                                        ButtonVariant::Default
                                    } else {
                                        ButtonVariant::Outline
                                    },
                                    ButtonSize::Compact,
                                )
                                .label(crate::i18n::t(cx, "Backup code"))
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cloud_second_factor_kind = SecondFactorKind::BackupCode;
                                    cx.notify();
                                })),
                            ),
                    );
                }
                panel
                    .child(field(
                        if active_kind == SecondFactorKind::BackupCode {
                            "Backup code"
                        } else {
                            "Authenticator code"
                        },
                        Input::new(&self.cloud_second_factor).disabled(busy),
                        cx,
                    ))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                Button::new("cloud-2fa-verify", ButtonVariant::Default, ButtonSize::Sm)
                                    .label(crate::i18n::t(cx, "Verify"))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.submit_cloud_second_factor(active_kind, cx);
                                    })),
                            )
                            .child(
                                Button::new("cloud-2fa-cancel", ButtonVariant::GhostMuted, ButtonSize::Sm)
                                    .label(crate::i18n::t(cx, "Cancel"))
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.update(cx, |state, cx| state.cancel_cloud_sign_in(cx));
                                    })),
                            ),
                    )
            }
            CloudAuthFlow::Idle => {
                let mut oauth_row = h_flex().gap(px(6.)).flex_wrap();
                for provider in SocialProvider::ALL {
                    oauth_row = oauth_row.child(
                        Button::new(
                            SharedString::from(format!("cloud-oauth-{}", provider.strategy())),
                            ButtonVariant::Outline,
                            ButtonSize::Sm,
                        )
                        .label(provider.label())
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state
                                .update(cx, |state, cx| state.sign_in_cloud_oauth(provider, cx));
                        })),
                    );
                }
                v_flex()
                    .gap(px(10.))
                    .child(oauth_row)
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .items_end()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(field("Email", Input::new(&self.cloud_email).disabled(busy), cx)),
                            )
                            .child(
                                Button::new("cloud-email-continue", ButtonVariant::Outline, ButtonSize::Default)
                                    .label(crate::i18n::t(cx, "Continue"))
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.submit_cloud_email(cx))),
                            ),
                    )
            }
        }
    }

    fn render_cloud(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (busy, identity, has_clerk_session, cloud_error, refreshing, listed, env_count) = {
            let cloud = &self.state.read(cx).cloud;
            (
                cloud.busy,
                cloud.identity.clone(),
                cloud.has_clerk_session,
                cloud.error.clone(),
                cloud.discovery.refreshing,
                cloud.discovery.listed,
                cloud.discovery.environments.len(),
            )
        };
        let p = palette(cx);
        let signed_in = identity.is_some();
        let legacy_oauth_only = signed_in && !has_clerk_session;

        let heading = h_flex()
            .gap(px(8.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child(crate::i18n::t(cx, "T3 Connect")),
                    )
                    .child(muted(
                        identity.unwrap_or_else(|| {
                            crate::i18n::t(cx, "Sign in to T3 Connect to connect this environment.").into()
                        }),
                        cx,
                    )),
            )
            .when(signed_in, |this| {
                this.child(
                    Button::new("cloud-sign-out", ButtonVariant::Outline, ButtonSize::Compact)
                        .label(crate::i18n::t(cx, "Sign out"))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |state, cx| state.sign_out_cloud(cx));
                        })),
                )
            })
            .child(
                Button::new("cloud-refresh", ButtonVariant::GhostMuted, ButtonSize::IconSm)
                    .icon(Icon::RefreshCw)
                    .tooltip(crate::i18n::t(cx, "Refresh T3 Connect environments"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.refresh_cloud(cx)))),
            );

        let auth_controls = (!has_clerk_session).then(|| self.render_cloud_auth_controls(cx));

        let mut body = card(cx)
            .px(px(16.))
            .py(px(14.))
            .gap(px(12.))
            .child(heading)
            .when(legacy_oauth_only, |this| {
                this.child(muted(crate::i18n::t(cx, t3_core::relay::LEGACY_OAUTH_NOTICE), cx))
            })
            .children(auth_controls)
            .children(cloud_error.map(|error| error_text(error, cx)));

        if signed_in && refreshing {
            body = body.child(muted(crate::i18n::t(cx, "Loading T3 Connect environments…"), cx));
        }
        if signed_in && listed && env_count == 0 {
            body = body.child(muted(
                crate::i18n::t(
                    cx,
                    "No other environments are published to your account yet. Publish one from another device to connect here.",
                ),
                cx,
            ));
        }
        if env_count > 0 {
            body = body.child(
                uniform_list(
                    "cloud-environments",
                    env_count,
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        range.map(|index| this.render_cloud_row(index, cx)).collect::<Vec<_>>()
                    }),
                )
                .w_full()
                .h(px((env_count as f32 * 64.).min(288.))),
            );
        }
        body
    }

    fn render_cloud_row(&self, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = self.state.read(cx);
        let cloud = &state.cloud;
        let Some(entry) = cloud.discovery.environments.get(index) else {
            return div().h(px(64.)).into_any_element();
        };
        let environment = entry.clone();
        let has_clerk_session = cloud.has_clerk_session;
        let connecting = cloud.connecting_env_id.as_deref() == Some(environment.environment_id.as_str());
        let any_connecting = cloud.connecting_env_id.is_some();
        let active_connected = state.active_environment().is_some_and(|saved| {
            saved.environment_id.as_deref() == Some(environment.environment_id.as_str())
                && state.client_handle().is_some()
        });

        let (dot_tone, status_text, tooltip_text): (Option<DotTone>, String, Option<String>) = if !has_clerk_session {
            let notice = crate::i18n::t(cx, t3_core::relay::LEGACY_OAUTH_NOTICE).to_owned();
            (
                None,
                format!("{} · {}", crate::i18n::t(cx, "T3 Connect"), crate::i18n::t(cx, "Sign in required")),
                Some(notice),
            )
        } else {
            match cloud.statuses.get(&environment.environment_id) {
                Some(EnvironmentHealth::Online) => (
                    Some(DotTone::Success),
                    format!("{} · {}", crate::i18n::t(cx, "T3 Connect"), crate::i18n::t(cx, "Online")),
                    None,
                ),
                Some(EnvironmentHealth::Offline {
                    offline_reason,
                    error,
                    trace_id,
                }) => {
                    let mut tip = t3_core::relay::offline_reason_message(offline_reason.as_deref())
                        .map(|m| crate::i18n::t(cx, m).to_owned())
                        .or_else(|| error.clone())
                        .unwrap_or_else(|| crate::i18n::t(cx, "Offline").to_owned());
                    if let Some(trace) = trace_id.as_deref().filter(|t| !t.is_empty()) {
                        tip.push_str(&format!(" ({}: {trace})", crate::i18n::t(cx, "Trace ID")));
                    }
                    (
                        Some(DotTone::Destructive),
                        format!("{} · {}", crate::i18n::t(cx, "T3 Connect"), crate::i18n::t(cx, "Offline")),
                        Some(tip),
                    )
                }
                Some(EnvironmentHealth::Error(err)) => (
                    Some(DotTone::Destructive),
                    format!("{} · {}", crate::i18n::t(cx, "T3 Connect"), crate::i18n::t(cx, "Status unavailable")),
                    Some(err.clone()),
                ),
                Some(EnvironmentHealth::Checking) | None => (
                    Some(DotTone::Warning),
                    format!("{} · {}", crate::i18n::t(cx, "T3 Connect"), crate::i18n::t(cx, "Checking…")),
                    None,
                ),
            }
        };

        let status_div = {
            let base = div()
                .id(SharedString::from(format!("relay-status-{}", environment.environment_id)))
                .truncate()
                .text_xs()
                .text_color(hsla(palette(cx).muted_foreground))
                .child(status_text);
            if let Some(tip) = tooltip_text.clone() {
                base.tooltip(crate::settings::controls::tooltip(tip.into()))
            } else {
                base
            }
        };

        let online = matches!(cloud.statuses.get(&environment.environment_id), Some(EnvironmentHealth::Online));
        let config = state.environments.find(Some(&environment.environment_id), &environment.endpoint.http_base_url)
            .and_then(|saved| state.fleet.environment(&saved.id))
            .and_then(|view| view.config.config.as_ref());
        let compatible = config.is_none_or(|config| t3_core::compatibility::protocol_mismatch(&config.environment).is_none());
        let live = has_clerk_session && matches!(cloud.statuses.get(&environment.environment_id), Some(EnvironmentHealth::Checking) | None);
        let action_button = if !has_clerk_session {
            Button::new(
                SharedString::from(format!("relay-{}", environment.environment_id)),
                ButtonVariant::Outline,
                ButtonSize::Compact,
            )
            .label(crate::i18n::t(cx, "Sign in"))
            .disabled(true)
            .tooltip(crate::i18n::t(cx, t3_core::relay::LEGACY_OAUTH_NOTICE))
        } else if active_connected {
            Button::new(
                SharedString::from(format!("relay-{}", environment.environment_id)),
                ButtonVariant::GhostMuted,
                ButtonSize::Compact,
            )
            .label(crate::i18n::t(cx, "Connected"))
            .disabled(true)
        } else {
            let env_to_connect = environment.clone();
            Button::new(
                SharedString::from(format!("relay-{}", environment.environment_id)),
                ButtonVariant::Outline,
                ButtonSize::Compact,
            )
            .label(crate::i18n::t(cx, if connecting { "Connecting…" } else { "Connect" }))
            .disabled(any_connecting || !online || !compatible)
            .tooltip(crate::i18n::t(cx, if !compatible { "This environment uses an incompatible server version." }
                else if !online { "This environment must be online to connect." } else { "Connect to this environment" }))
            .on_click(cx.listener(move |this, _, _, cx| {
                let env = env_to_connect.clone();
                this.state
                    .update(cx, |state, cx| state.connect_cloud_environment(env, cx));
            }))
        };

        h_flex()
            .w_full()
            .min_w_0()
            .h(px(64.))
            .gap(px(10.))
            .child(config.map_or(Icon::Cloud, Icon::machine).render(px(16.), hsla(palette(cx).muted_foreground)))
            .child(crate::components::motion::StatusDot::new(
                SharedString::from(format!("connect-status-{}", environment.environment_id)),
                px(8.),
                hsla(match dot_tone {
                    Some(DotTone::Success) => palette(cx).success,
                    Some(DotTone::Warning) => palette(cx).warning,
                    Some(DotTone::Destructive) => palette(cx).destructive,
                    None => palette(cx).muted_foreground,
                }),
                live,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap(px(4.))
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(palette(cx).foreground))
                            .child(environment.label.clone()),
                    )
                    .child(status_div),
            )
            .child(action_button)
            .into_any_element()
    }

    /// Legacy-permission re-pair notice (`PermissionUpdateNotice.ts`, `CN-REPAIR`).
    fn render_legacy_permissions(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let state = self.state.read(cx);
        let env = state.active_environment()?;
        let env_key = env.environment_id.as_deref().unwrap_or(env.id.as_str()).to_owned();
        let ui_key = format!("permission-update-dismissed-{env_key}");
        let dismissed = *self.dismissed_permission_notice.entry(env_key.clone()).or_insert_with(|| {
            t3_client::store::load_ui_state(&ui_key).and_then(|v| v.as_bool()) == Some(true)
        });
        if dismissed {
            return None;
        }
        let session = state
            .fleet
            .environment(&env.id)
            .and_then(|view| view.grants.as_ref())
            .cloned()
            .or_else(|| state.client_handle().and_then(|client| client.session_state().borrow().clone()))?;
        if !session_has_legacy_permissions(&session) {
            return None;
        }
        let p = palette(cx);
        let label = env.label.clone();
        let dismiss_env = env_key;
        let dismiss_key = ui_key;
        Some(
            v_flex()
                .px(px(14.))
                .py(px(12.))
                .gap(px(6.))
                .rounded(px(12.))
                .border_1()
                .border_color(hsla(p.warning).opacity(0.35))
                .bg(hsla(p.warning).opacity(0.08))
                .text_sm()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .child(crate::i18n::t_fmt(
                            cx,
                            "Permissions have changed for {environment}",
                            &[("environment", t3_core::i18n::IcuArg::Text(std::borrow::Cow::Borrowed(&label)))],
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .line_height(px(18.))
                        .text_color(hsla(p.muted_foreground))
                        .child(crate::i18n::t(cx, "This connection still uses the old permissions, so some actions may no longer be available. Pair again using a new link with the permissions you need.")),
                )
                .child(
                    h_flex().gap(px(8.)).pt(px(2.)).child(
                        Button::new("connect-legacy-perms-dismiss", ButtonVariant::GhostMuted, ButtonSize::Compact)
                            .label(crate::i18n::t(cx, "Dismiss"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.dismissed_permission_notice.insert(dismiss_env.clone(), true);
                                t3_client::store::save_ui_state(&dismiss_key, &serde_json::Value::Bool(true));
                                let toast_key = format!("permission-update:{dismiss_env}");
                                this.state.update(cx, |state, cx| state.close_toast_by_key(&toast_key, cx));
                                cx.notify();
                            })),
                    ),
                ),
        )
    }

    fn render_saved_row(&self, environment: &SavedEnvironment, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let active = state.environments.active.as_deref() == Some(environment.id.as_str()) && state.client_handle().is_some();
        let status_opt = state.environment_statuses.get(&environment.id).cloned();
        let (tone, status, error_tone) = match &status_opt {
            _ if environment.access_token.is_none() => (Some(DotTone::Destructive), crate::i18n::t(cx, "Needs pairing again").to_owned(), true),
            Some(status) => {
                let (text, error) = status.summary();
                (Some(status.dot()), crate::i18n::t(cx, &text).to_owned(), error)
            }
            _ => (None, crate::i18n::t(cx, "Not connected").to_owned(), false),
        };
        let diagnosis = status_opt.as_ref().and_then(|s| diagnose(s, &environment.label));
        let expanded = self.expanded_diagnosis.contains(&environment.id);
        let freeze = active
            .then(|| state.client_handle())
            .flatten()
            .and_then(|client| client.server_config_extras().borrow().last_event_loop_stall.clone())
            .and_then(|stall| host_freeze_notice(&environment.label, Some(&stall)));
        let refused_routes = t3_client::supervisor::refused_routes_for(&environment.http_base)
            .borrow()
            .clone();
        let version_skew = resolve_version_mismatch(environment.server_version.as_deref(), env!("CARGO_PKG_VERSION"));
        let busy = state.connect.pairing.is_some();
        let confirming = self.confirm_remove.as_deref() == Some(environment.id.as_str());
        let p = palette(cx);
        let id = environment.id.clone();
        let actions = if confirming {
            let (remove, keep) = (id.clone(), id.clone());
            h_flex()
                .gap_1()
                .child(Button::new(SharedString::from(format!("remove-confirm-{remove}")), ButtonVariant::DestructiveOutline, ButtonSize::Compact).label(crate::i18n::t(cx, "Remove")).on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm_remove = None;
                    this.state.update(cx, |state, cx| state.remove_environment(&remove, cx));
                })))
                .child(Button::new(SharedString::from(format!("remove-cancel-{keep}")), ButtonVariant::GhostMuted, ButtonSize::Compact).label(crate::i18n::t(cx, "Keep")).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.confirm_remove = None;
                        cx.notify();
                    },
                )))
        } else {
            let (connect, remove, diag_id) = (id.clone(), id.clone(), id.clone());
            let action = if active {
                None
            } else if environment.access_token.is_some() || environment.is_relay_managed() {
                Some(crate::i18n::t(cx, "Connect"))
            } else {
                Some(crate::i18n::t(cx, "Pair again"))
            };
            h_flex()
                .gap_1()
                .when(diagnosis.is_some(), |this| {
                    this.child(
                        Button::new(SharedString::from(format!("details-{diag_id}")), ButtonVariant::GhostMuted, ButtonSize::Compact)
                            .label(crate::i18n::t(cx, if expanded { "Hide details" } else { "Details" }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.expanded_diagnosis.remove(&diag_id) {
                                    this.expanded_diagnosis.insert(diag_id.clone());
                                }
                                cx.notify();
                            })),
                    )
                })
                .children(action.map(|action| {
                    Button::new(SharedString::from(format!("connect-{connect}")), ButtonVariant::Outline, ButtonSize::Compact).label(action).disabled(busy).on_click(cx.listener(
                        move |this, _, _, cx| this.state.update(cx, |state, cx| state.connect_environment(&connect, cx)),
                    ))
                }))
                .child(Button::new(SharedString::from(format!("remove-{remove}")), ButtonVariant::GhostMuted, ButtonSize::IconSm).icon(Icon::Trash2).tooltip(crate::i18n::t(cx, "Remove from this device…")).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.confirm_remove = Some(remove.clone());
                        cx.notify();
                    }),
                ))
        };
        let diag_panel = diagnosis.filter(|_| expanded).map(|diag| {
            let retry_id = id.clone();
            let copy_text = connection_diagnosis_text(&environment.label, &diag);
            let can_retry = status_opt.as_ref().is_some_and(|s| s.can_retry());
            v_flex()
                .mt(px(8.))
                .pt(px(8.))
                .border_t_1()
                .border_color(hsla(p.border).opacity(0.5))
                .gap(px(8.))
                .text_xs()
                .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.destructive_foreground)).child(diag.headline.clone()))
                .child(v_flex().gap(px(2.)).children(diag.hops.iter().map(|hop| {
                    let color = match hop.result {
                        HopResult::Failed => hsla(p.destructive_foreground),
                        HopResult::Ok => hsla(p.foreground),
                        HopResult::Unchecked => hsla(p.muted_foreground),
                    };
                    let note = hop.result.note();
                    let line = if note.is_empty() {
                        format!("{} {}", hop.result.mark(), hop.label)
                    } else {
                        format!("{} {} ({note})", hop.result.mark(), hop.label)
                    };
                    div().text_color(color).child(line)
                })))
                .when(!diag.steps.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .gap(px(2.))
                            .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(crate::i18n::t(cx, "What to try")))
                            .children(diag.steps.iter().map(|step| div().text_color(hsla(p.muted_foreground)).child(format!("• {step}")))),
                    )
                })
                .when(!diag.facts.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .gap(px(2.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .children(diag.facts.iter().map(|(k, v)| {
                                div().text_color(hsla(p.muted_foreground)).child(format!("{k}: {v}"))
                            })),
                    )
                })
                .child(
                    h_flex()
                        .gap(px(6.))
                        .when(can_retry, |this| {
                            this.child(
                                Button::new(SharedString::from(format!("diag-retry-{retry_id}")), ButtonVariant::Outline, ButtonSize::Compact)
                                    .label(crate::i18n::t(cx, "Retry now"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.read(cx).retry_environment(&retry_id);
                                    })),
                            )
                        })
                        .child(
                            Button::new(SharedString::from(format!("diag-copy-{id}")), ButtonVariant::Outline, ButtonSize::Compact)
                                .icon(Icon::Copy)
                                .label(crate::i18n::t(cx, "Copy diagnostics"))
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                                }),
                        ),
                )
        });
        v_flex()
            .px(px(16.))
            .py(px(10.))
            .child(
                h_flex()
                    .gap(px(10.))
                    .child(status_dot(tone, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(environment.label.clone()))
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .line_height(px(18.))
                                    .text_color(if error_tone { hsla(p.destructive_foreground) } else { hsla(p.muted_foreground) })
                                    .child(format!("{} · {status}", environment.address())),
                            )
                            .children(freeze.map(|notice| {
                                div()
                                    .text_xs()
                                    .line_height(px(18.))
                                    .text_color(hsla(p.warning_foreground))
                                    .child(format!("{} — {}", notice.title, notice.description))
                            }))
                            .children(version_skew.map(|skew| {
                                div()
                                    .text_xs()
                                    .line_height(px(18.))
                                    .text_color(hsla(p.muted_foreground))
                                    .child(format!("Server v{} (client v{}) · {}", skew.server_version, skew.client_version, skew.hint))
                            })),
                    )
                    .child(actions),
            )
            .children(diag_panel)
            .when(!refused_routes.is_empty(), |this| {
                this.child(render_refused_routes_list(&refused_routes, cx))
            })
    }

    fn render_saved(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let state = self.state.read(cx);
        let environments = state.environments.environments.clone();
        if environments.is_empty() {
            return None;
        }
        let connected_count = state
            .fleet
            .environments
            .iter()
            .filter(|view| view.status == t3_client::supervisor::ConnectionStatus::Connected)
            .count();
        let lb_doc = crate::settings::client_settings(cx);
        let lb_enabled = t3_core::settings::read_load_balancing_enabled(lb_doc);
        let lb_weights = t3_core::settings::read_load_balancing_weights(lb_doc);
        let divider = hsla(palette(cx).border).opacity(0.5);
        let rows: Vec<_> = environments
            .iter()
            .enumerate()
            .map(|(index, environment)| div().when(index > 0, |this| this.border_t_1().border_color(divider)).child(self.render_saved_row(environment, cx)))
            .collect();
        let header = h_flex()
            .justify_between()
            .items_center()
            .child(label(crate::i18n::t(cx, "Saved environments"), cx))
            .when(lb_enabled && connected_count >= 2, |this| {
                let weights: HashMap<String, f64> = lb_weights.into_iter().map(|(k, v)| (k, f64::from(v))).collect();
                this.child(
                    Button::new("connect-auto-balance", ButtonVariant::Outline, ButtonSize::Compact)
                        .label(crate::i18n::t(cx, "Auto balance"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let weights = weights.clone();
                            this.state.update(cx, |state, cx| {
                                state.route_draft_load_balanced(None, None, false, Vec::new(), weights, None, cx);
                            });
                        })),
                )
            });
        Some(v_flex().gap(px(8.)).child(header).child(card(cx).children(rows)))
    }

    /// `renderConnectionModeCard`.
    fn mode_card(&self, mode: AddMode, title: &str, description: &str, glyph: Icon, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let selected = self.mode == mode;
        let disabled = self.state.read(cx).connect.pairing.is_some();
        let title = crate::i18n::t(cx, title).to_owned();
        let description = crate::i18n::t(cx, description).to_owned();
        h_flex()
            .id(match mode {
                AddMode::Link => "connect-mode-link",
                AddMode::Ssh => "connect-mode-ssh",
            })
            .flex_1()
            .min_w_0()
            .min_h(px(96.))
            .items_start()
            .gap(px(12.))
            .p(px(16.))
            .rounded(px(8.))
            .border_1()
            .border_color(if selected { hsla(p.primary).opacity(0.5) } else { hsla(p.border).opacity(0.6) })
            .when(selected, |this| this.bg(hsla(p.primary).opacity(0.05)))
            .when(!selected && !disabled, |this| this.cursor_pointer().hover(|this| this.bg(hsla(p.muted).opacity(0.4))))
            .when(disabled, |this| this.opacity(0.64))
            .child(
                div()
                    .mt(px(2.))
                    .size(px(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if selected { hsla(p.primary).opacity(0.3) } else { hsla(p.border).opacity(0.7) })
                    .bg(if selected { hsla(p.primary).opacity(0.1) } else { hsla(p.background) })
                    .child(glyph.render(px(16.), if selected { hsla(p.primary) } else { hsla(p.muted_foreground) })),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .gap(px(4.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
                    .child(div().text_xs().line_height(px(19.5)).text_color(hsla(p.muted_foreground)).child(description)),
            )
            .when(!disabled, |this| this.on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx))))
    }

    fn render_link_fields(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let busy = state.connect.pairing == Some(PairingSource::Link);
        let primary = state.connect.local_server.is_none() && state.environments.environments.is_empty();
        let error = pairing_error(&state.connect, PairingSource::Link);
        v_flex()
            .gap(px(8.))
            .child(muted(
                crate::i18n::t(
                    cx,
                    "On the machine running T3 Code, open Settings → Sharing & access and create a pairing link, or run `t3 pair` there. Paste the pairing URL, or enter a host and code.",
                ),
                cx,
            ))
            .child(field("Pairing link or host", Input::new(&self.link).disabled(state.connect.pairing.is_some()), cx))
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(div().flex_1().min_w_0().child(field("Pairing code (if not in URL)", Input::new(&self.code).disabled(state.connect.pairing.is_some()), cx)))
                    .child(
                        Button::new("connect-add", if primary { ButtonVariant::Default } else { ButtonVariant::Outline }, ButtonSize::Default)
                            .label(crate::i18n::t(cx, if busy { "Adding…" } else { "Add environment" }))
                            .disabled(state.connect.pairing.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                    ),
            )
            .children(error.map(|error| error_text(error, cx)))
    }

    fn render_host_row(&self, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(host) = self.ssh_matches.get(index) else { return div().h(px(HOST_ROW_HEIGHT)).into_any_element() };
        let p = palette(cx);
        let busy = self.state.read(cx).connect.pairing.is_some();
        let target = host.target();
        let address = format_target(&target);
        h_flex()
            .id(SharedString::from(format!("ssh-host-{index}")))
            .w_full()
            .h(px(HOST_ROW_HEIGHT))
            .px(px(8.))
            .gap(px(8.))
            .rounded(px(4.))
            .whitespace_nowrap()
            .when(!busy, |this| this.cursor_pointer().hover(|this| this.bg(hsla(p.accent))))
            .child(div().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(host.alias.clone()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .when(address != host.alias, |this| this.child(address)),
            )
            .when(!busy, |this| this.on_click(cx.listener(move |this, _, window, cx| this.add_ssh_host(target.clone(), window, cx))))
            .into_any_element()
    }

    /// The discovered hosts that match the field, or why there are none to show.
    fn render_host_list(&mut self, busy: bool, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let p = palette(cx);
        let query = self.ssh_host.read(cx).value().to_string();
        let state = self.state.read(cx);
        let loading = state.connect.ssh_discovering && state.connect.ssh_hosts.is_none();
        let unsaved = state.unsaved_ssh_hosts();
        self.ssh_matches = filter_hosts(&unsaved, &query).into_iter().cloned().collect();
        let empty = |text: String| div().px(px(12.)).py(px(8.)).text_xs().text_color(hsla(p.muted_foreground)).child(text);
        let body = if loading {
            empty(crate::i18n::t(cx, "Loading hosts…").to_owned()).into_any_element()
        } else if unsaved.is_empty() {
            return None;
        } else if self.ssh_matches.is_empty() {
            empty(crate::i18n::t_fmt(
                cx,
                "No hosts match \"{query}\".",
                &[("query", t3_core::i18n::IcuArg::Text(std::borrow::Cow::Borrowed(query.trim())))],
            ))
            .into_any_element()
        } else {
            let count = self.ssh_matches.len();
            uniform_list("ssh-hosts", count, cx.processor(|this, range: std::ops::Range<usize>, _, cx| range.map(|index| this.render_host_row(index, cx)).collect::<Vec<_>>()))
                .h(px((count as f32 * HOST_ROW_HEIGHT).min(HOST_LIST_MAX_HEIGHT)))
                .into_any_element()
        };
        Some(
            div()
                .p(px(4.))
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(p.border).opacity(0.6))
                .bg(hsla(p.card).opacity(0.4))
                .when(busy, |this| this.opacity(0.64))
                .child(body)
                .into_any_element(),
        )
    }

    fn render_ssh_fields(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (busy, error) = {
            let connect = &self.state.read(cx).connect;
            (connect.pairing.is_some(), pairing_error(connect, PairingSource::Ssh))
        };
        let adding = self.state.read(cx).connect.pairing == Some(PairingSource::Ssh);
        let hosts = self.render_host_list(busy, cx);
        v_flex()
            .gap(px(12.))
            .child(field("SSH host or alias", Input::new(&self.ssh_host).disabled(busy), cx))
            .children(hosts)
            .child(
                h_flex()
                    .gap(px(12.))
                    .items_start()
                    .child(div().flex_1().min_w_0().child(field("Username", Input::new(&self.ssh_username).disabled(busy), cx)))
                    .child(div().w(px(112.)).flex_none().child(field("Port", Input::new(&self.ssh_port).disabled(busy), cx))),
            )
            .children(error.map(|error| error_alert(error, cx)))
            .child(
                Button::new("connect-add-ssh", ButtonVariant::Outline, ButtonSize::Default)
                    .icon(Icon::Plus)
                    .label(crate::i18n::t(cx, if adding { "Adding…" } else { "Add environment" }))
                    .disabled(busy)
                    .full_width()
                    .on_click(cx.listener(|this, _, _, cx| this.add_ssh(cx))),
            )
    }

    fn render_add(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.mode {
            AddMode::Link => self.render_link_fields(cx).into_any_element(),
            AddMode::Ssh => self.render_ssh_fields(cx).into_any_element(),
        };
        v_flex()
            .gap(px(12.))
            .child(label(crate::i18n::t(cx, "Add environment"), cx))
            .child(
                h_flex()
                    .gap(px(12.))
                    .items_stretch()
                    .child(self.mode_card(AddMode::Link, "Remote link", "Paste a pairing link from the backend.", Icon::ChevronsLeftRightEllipsis, cx))
                    .child(self.mode_card(AddMode::Ssh, "SSH", "Use local SSH config, agent, and tunnels for the backend.", Icon::Terminal, cx)),
            )
            .child(body)
    }
}

impl Render for ConnectView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let switching = self.state.read(cx).connect.switching;
        div()
            .id("connect-screen")
            .size_full()
            .overflow_y_scroll()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.state.read(cx).connect.switching {
                    this.state.update(cx, |state, cx| state.close_switcher(cx));
                    cx.stop_propagation();
                }
            }))
            .child(
                div().w_full().min_h_full().flex().items_center().justify_center().py(px(48.)).child(
                    v_flex()
                        .w(px(480.))
                        .gap(px(20.))
                        .child(self.render_header(switching, cx))
                        .children(self.render_rejected(cx))
                        .children(self.render_legacy_permissions(cx))
                        .child(self.render_cloud(cx))
                        .child(self.render_local(cx))
                        .children(self.render_saved(cx))
                        .child(self.render_add(cx)),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_route_display_address_extracts_direct_learned_and_ssh_origins() {
        assert_eq!(
            refused_route_display_address("direct:http://192.168.1.20:3773"),
            "http://192.168.1.20:3773"
        );
        assert_eq!(
            refused_route_display_address("learned:env-1:https://studio.ts.net:3773@direct:http://10.0.0.2:3773"),
            "https://studio.ts.net:3773"
        );
        assert_eq!(
            refused_route_display_address("ssh:http://127.0.0.1:41234"),
            "http://127.0.0.1:41234"
        );
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn connect_view_renders_expired_session_trace_and_refused_routes(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            crate::settings::init(state.clone(), cx);
            state
        });
        let http_base = "http://127.0.0.1:48811";
        t3_client::supervisor::set_refused_routes_for(
            http_base,
            vec!["direct:http://192.168.1.44:3773".to_owned()],
        );
        state.update(cx, |state, _| {
            state.environments.environments.push(SavedEnvironment {
                id: "saved-1".into(),
                environment_id: Some("env-1".into()),
                label: "Studio".into(),
                disabled: false,
                http_base: http_base.into(),
                access_token: Some("tok".into()),
                server_version: Some("0.1.9".into()),
                local: false,
                ssh: None,
            });
            state.connect.rejected = Some(crate::environments::Rejected {
                id: "saved-1".into(),
                label: "Studio".into(),
                reason: "This device is no longer authorized. Pair again. (Trace ID: trace-xyz)".into(),
                trace_id: Some("trace-xyz".into()),
                local: false,
                ssh: false,
            });
        });
        let window = cx.open_window(gpui::size(px(640.), px(720.)), |window, cx| {
            ConnectView::new(state.clone(), window, cx)
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();
        t3_client::supervisor::set_refused_routes_for(http_base, Vec::new());
        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        runtime.shutdown_background();
        cx.run_until_parked();
    }

    #[test]
    fn environment_machine_icon_maps_every_machine_kind() {
        assert_eq!(environment_machine_icon(Some("cloud")), Icon::Cloud);
        assert_eq!(environment_machine_icon(Some("linux")), Icon::Linux);
        assert_eq!(environment_machine_icon(Some("desktop")), Icon::Monitor);
        assert_eq!(environment_machine_icon(Some("laptop")), Icon::Laptop);
        assert_eq!(environment_machine_icon(Some("mac-mini")), Icon::MacMini);
        assert_eq!(environment_machine_icon(Some("mac-studio")), Icon::MacStudio);
        assert_eq!(environment_machine_icon(Some("server")), Icon::Server);
        assert_eq!(environment_machine_icon(None), Icon::Server);
    }

    #[gpui::test]
    async fn link_machine_select_renders_badges_and_address_warning(cx: &mut gpui::TestAppContext) {
        struct Preview(Vec<LinkableMachine>);
        impl Render for Preview {
            fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                v_flex()
                    .gap_2()
                    .child(render_link_machine_select(
                        "test-link-machine",
                        &self.0,
                        Some(ANOTHER_ADDRESS),
                        false,
                        |_, _, _| {},
                        cx,
                    ))
                    .child(link_address_warning("https://box.example.ts.net:3773/pair#token=abc", cx))
            }
        }
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let machines = vec![
            LinkableMachine {
                environment_id: "env-a".into(),
                label: "Studio".into(),
                machine: Some("mac-studio".into()),
                urls: vec!["https://studio.ts.net:3773".into()],
                connected: true,
                connect_only: false,
                linked: true,
            },
            LinkableMachine {
                environment_id: "env-b".into(),
                label: "Cloud Box".into(),
                machine: Some("cloud".into()),
                urls: vec!["https://cloud.example:3773".into()],
                connected: false,
                connect_only: true,
                linked: false,
            },
        ];
        let window = cx.open_window(gpui::size(px(480.), px(360.)), |_, _| Preview(machines));
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }

    #[gpui::test]
    fn link_access_picker_selects_the_next_level_from_the_keyboard(cx: &mut gpui::TestAppContext) {
        struct Preview(SharedString);
        impl Render for Preview {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let weak = cx.entity().downgrade();
                link_access_picker("test-link-access", &self.0, false, move |next, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.0 = next;
                        cx.notify();
                    });
                }, cx)
            }
        }

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let window = cx.open_window(gpui::size(px(480.), px(360.)), |_, _| Preview("approval-required".into()));
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
            window.focus_next(cx);
        }).unwrap();
        cx.simulate_keystrokes(window.into(), "enter");
        cx.simulate_keystrokes(window.into(), "down");
        cx.simulate_keystrokes(window.into(), "enter");
        window.update(cx, |view, _, _| assert_eq!(view.0.as_ref(), "auto-accept-edits")).unwrap();
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn connect_view_renders_local_server_host_controls_and_policy_diagnostics(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            crate::settings::init(state.clone(), cx);
            state
        });
        let window = cx.open_window(gpui::size(px(640.), px(780.)), |window, cx| {
            ConnectView::new(state.clone(), window, cx)
        });

        // 1. Stopped + auto-start toggled on.
        state.update(cx, |state, cx| {
            state.connect.local_server = None;
            state.set_local_server_auto_start(true, cx);
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();

        // 2. Managed policy exit-77 failure with diagnostics output tail.
        state.update(cx, |state, cx| {
            state.apply_local_host_phase(
                LocalServerPhase::Failed(LocalServerFailure {
                    kind: LocalServerFailureKind::ManagedPolicy,
                    message: t3_core::local_server::MANAGED_POLICY_STARTUP_MESSAGE.to_owned(),
                    recovery: t3_core::local_server::StartupRecoveryOffer::ManagedPolicy,
                    output_tail: "ManagedPolicyFileError: invalid policy JSON".to_owned(),
                    restart_suppressed: true,
                }),
                cx,
            );
        });
        state.read_with(cx, |state, _| {
            let diag = state.local_server_diagnostics().expect("diagnostics text");
            assert!(diag.contains("Automatic restart: suppressed"));
            assert!(diag.contains("ManagedPolicyFileError"));
        });
        cx.update_window(window.into(), |view, window, cx| {
            let view = view.downcast::<ConnectView>().expect("ConnectView");
            view.update(cx, |this, _| this.local_diagnostics_expanded = true);
            window.draw(cx).clear();
        })
        .unwrap();

        // 3. Hosted local server running with Stop server control.
        state.update(cx, |state, cx| {
            state.connect.local_server = Some(LocalServer {
                home: std::path::PathBuf::from("/tmp/t3-connect-test"),
                state: serde_json::from_value(serde_json::json!({
                    "version": 1,
                    "pid": std::process::id(),
                    "port": 3773,
                    "origin": "http://127.0.0.1:3773",
                    "serverVersion": "0.1.9"
                }))
                .unwrap(),
                descriptor: t3_core::server_config::EnvironmentDescriptor {
                    environment_id: "env-local-hosted".into(),
                    label: "Hosted Mac".into(),
                    server_version: "0.1.9".into(),
                    orchestration_protocol_version: Some(2),
                    ..Default::default()
                },
            });
            state.apply_local_host_phase(
                LocalServerPhase::Running {
                    pid: std::process::id(),
                    origin: "http://127.0.0.1:3773".into(),
                    server_version: Some("0.1.9".into()),
                },
                cx,
            );
            assert!(state.is_hosting_local_server());
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        runtime.shutdown_background();
        cx.run_until_parked();
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn connect_view_renders_clerk_relay_environments_and_auth_flows(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            crate::settings::init(state.clone(), cx);
            state
        });
        let window = cx.open_window(gpui::size(px(640.), px(780.)), |window, cx| {
            ConnectView::new(state.clone(), window, cx)
        });

        // 1. Signed out (social OAuth + inline email form).
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();

        // 2. Signed in with Clerk session + online/offline discovered T3 Connect environments.
        state.update(cx, |state, _| {
            state.cloud.identity = Some("alice@example.test".into());
            state.cloud.has_clerk_session = true;
            state.cloud.discovery = t3_core::relay::Discovery {
                listed: true,
                refreshing: false,
                environments: vec![
                    t3_core::relay::Environment {
                        environment_id: "env-online".into(),
                        label: "Mac Studio".into(),
                        endpoint: t3_core::relay::Endpoint {
                            http_base_url: "https://studio.t3.codes".into(),
                            ws_base_url: "wss://studio.t3.codes/ws".into(),
                            provider_kind: "cloudflare_tunnel".into(),
                        },
                    },
                    t3_core::relay::Environment {
                        environment_id: "env-offline".into(),
                        label: "Devbox".into(),
                        endpoint: t3_core::relay::Endpoint {
                            http_base_url: "https://devbox.t3.codes".into(),
                            ws_base_url: "wss://devbox.t3.codes/ws".into(),
                            provider_kind: "cloudflare_tunnel".into(),
                        },
                    },
                ],
            };
            state
                .cloud
                .statuses
                .insert("env-online".into(), EnvironmentHealth::Online);
            state.cloud.statuses.insert(
                "env-offline".into(),
                EnvironmentHealth::Offline {
                    offline_reason: Some("tunnel_released".into()),
                    error: None,
                    trace_id: Some("trace-off".into()),
                },
            );
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();

        // 3. Backup-code-only 2FA resolves effective factor to BackupCode for both Enter & Verify.
        state.update(cx, |state, _| {
            state.cloud.has_clerk_session = false;
            state.cloud.auth_flow = CloudAuthFlow::SecondFactor(t3_client::connect::PendingSecondFactor::for_test(
                t3_core::connect::SecondFactorChallenge {
                    sign_in_id: "sia-backup".into(),
                    supports_totp: false,
                    supports_backup_code: true,
                },
            ));
        });
        cx.update_window(window.into(), |view, window, cx| {
            let view = view.downcast::<ConnectView>().expect("ConnectView");
            view.read_with(cx, |this, cx| {
                assert_eq!(this.effective_second_factor_kind(cx), SecondFactorKind::BackupCode);
            });
            window.draw(cx).clear();
        })
        .unwrap();

        // 4. Legacy OAuth session without Clerk session.
        state.update(cx, |state, _| {
            state.cloud.auth_flow = CloudAuthFlow::Idle;
            state.cloud.has_clerk_session = false;
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        runtime.shutdown_background();
        cx.run_until_parked();
    }
}
