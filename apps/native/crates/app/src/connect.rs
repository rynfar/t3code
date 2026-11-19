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
use t3_core::pairing::{
    ANOTHER_ADDRESS, LinkMachineBadge, LinkableMachine, link_destination, link_machine_badge as resolve_link_machine_badge,
    link_machine_select_label, session_has_legacy_permissions,
};
use t3_core::ssh::{filter_hosts, format_target, parse_manual_target};

use crate::app_state::{AppEvent, AppState};
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
                                .child("Sign-in refused"),
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
                        .child("The host refused this device's sign-in. Pair this address again with Add route."),
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
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.warning_foreground)).child("Check this address")),
        )
        .child(
            div()
                .text_xs()
                .line_height(px(18.))
                .text_color(hsla(p.muted_foreground))
                .child("This app cannot confirm it is one of your machines. The pairing code you paste is sent there."),
        )
        .child(
            div()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(hsla(p.foreground))
                .child(destination),
        )
}

/// `LinkMachineBadge` (`apps/web/src/components/settings/LinkMachineSelect.tsx`).
pub fn link_machine_badge(machine: &LinkableMachine, cx: &App) -> Option<Div> {
    let badge = resolve_link_machine_badge(machine)?;
    let p = palette(cx);
    let (bg, border, fg) = match badge {
        LinkMachineBadge::Linked => (hsla(p.success).opacity(0.14), hsla(p.success).opacity(0.35), hsla(p.success_foreground)),
        LinkMachineBadge::T3Connect | LinkMachineBadge::NotConnected => (hsla(p.background), hsla(p.border), hsla(p.muted_foreground)),
    };
    Some(
        div()
            .flex_none()
            .px(px(6.))
            .py(px(2.))
            .rounded(px(999.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(fg)
            .child(badge.label()),
    )
}

/// `LinkMachineSelect` (`apps/web/src/components/settings/LinkMachineSelect.tsx`):
/// the user's other machines, including those on T3 Connect, followed by `"Another address…"`.
#[allow(clippy::too_many_arguments)]
pub fn render_link_machine_select(
    id: impl Into<SharedString>,
    machines: &[LinkableMachine],
    selected: Option<&str>,
    open: bool,
    disabled: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    on_select: impl Fn(String, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> Div {
    let id = id.into();
    let p = palette(cx);
    let selected_machine = selected
        .filter(|value| *value != ANOTHER_ADDRESS)
        .and_then(|value| machines.iter().find(|candidate| candidate.environment_id == value));
    let trigger_label = link_machine_select_label(machines, selected);

    let trigger = h_flex()
        .id(SharedString::from(format!("{id}-trigger")))
        .w_full()
        .h(px(32.))
        .px(px(10.))
        .gap(px(8.))
        .items_center()
        .justify_between()
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.input))
        .bg(hsla(p.background))
        .when(disabled, |this| this.opacity(0.6))
        .when(!disabled, |this| {
            this.cursor_pointer()
                .hover(|this| this.bg(hsla(p.accent)))
                .on_click(move |_, window, cx| on_toggle(window, cx))
        })
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(8.))
                .items_center()
                .when_some(selected_machine, |this, machine| {
                    this.child(
                        environment_machine_icon(machine.machine.as_deref())
                            .render(px(14.), hsla(p.muted_foreground).opacity(0.8)),
                    )
                })
                .child(div().min_w_0().truncate().text_sm().text_color(hsla(p.foreground)).child(trigger_label)),
        )
        .child(
            (if open { Icon::ChevronUp } else { Icon::ChevronDown })
                .render(px(14.), hsla(p.icon_muted).opacity(0.8)),
        );

    let mut container = v_flex()
        .gap(px(6.))
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Machine"))
        .child(trigger);

    if open && !disabled {
        let mut list = v_flex()
            .p(px(4.))
            .gap(px(2.))
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.popover));

        for (index, machine) in machines.iter().enumerate() {
            let is_selected = selected == Some(machine.environment_id.as_str());
            let env_id = machine.environment_id.clone();
            let select_cb = on_select.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("{id}-opt-{index}")))
                    .w_full()
                    .px(px(8.))
                    .py(px(6.))
                    .gap(px(8.))
                    .items_center()
                    .justify_between()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(hsla(p.accent)))
                    .hover(|this| this.bg(hsla(p.accent)))
                    .on_click(move |_, window, cx| select_cb(env_id.clone(), window, cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(8.))
                            .items_center()
                            .child(
                                environment_machine_icon(machine.machine.as_deref())
                                    .render(px(14.), hsla(p.muted_foreground).opacity(0.8)),
                            )
                            .child(div().min_w_0().truncate().text_sm().text_color(hsla(p.foreground)).child(machine.label.clone())),
                    )
                    .children(link_machine_badge(machine, cx)),
            );
        }

        let another_selected = selected == Some(ANOTHER_ADDRESS);
        let select_another = on_select;
        list = list.child(
            h_flex()
                .id(SharedString::from(format!("{id}-another")))
                .w_full()
                .px(px(8.))
                .py(px(6.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(another_selected, |this| this.bg(hsla(p.accent)))
                .hover(|this| this.bg(hsla(p.accent)))
                .on_click(move |_, window, cx| select_another(ANOTHER_ADDRESS.to_owned(), window, cx))
                .child(div().text_sm().text_color(hsla(p.foreground)).child("Another address\u{2026}")),
        );
        container = container.child(list);
    }

    container
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
fn field(label: &'static str, control: impl IntoElement, cx: &App) -> Div {
    v_flex().gap(px(6.)).child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(palette(cx).foreground)).child(label)).child(control)
}

/// `SettingsGroup`: radius 12, `border` at 60%, `card` at 40%.
fn card(cx: &App) -> Div {
    let p = palette(cx);
    v_flex().rounded(px(12.)).border_1().border_color(hsla(p.border).opacity(0.6)).bg(hsla(p.card).opacity(0.4))
}

impl ConnectView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let link = cx.new(|cx| InputState::new(window, cx).placeholder("https://your-server:3773/pair#token=… or host:3773"));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("Optional pairing code"));
        let ssh_host = cx.new(|cx| InputState::new(window, cx).placeholder("Search hosts or type devbox"));
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
            cx.subscribe_in(&state, window, |this, state, event: &AppEvent, window, cx| {
                if !matches!(event, AppEvent::Phase) {
                    return;
                }
                let (pairing, failed) = {
                    let connect = &state.read(cx).connect;
                    (connect.pairing.is_some(), connect.error.is_some())
                };
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
            mode: AddMode::Link,
            ssh_host,
            ssh_username,
            ssh_port,
            ssh_matches: Vec::new(),
            confirm_remove: None,
            expanded_diagnosis: HashSet::new(),
            dismissed_permission_notice: HashMap::new(),
            submitted: false,
            ssh_submitted: false,
            _subscriptions: subscriptions,
        }
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
        let subtitle = if switching { "Switch to another environment, or pair a new one." } else { "Pick an environment, or pair with a T3 Code server." };
        h_flex()
            .w_full()
            .items_start()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .gap_1()
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child("Connect to T3 Code"))
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(subtitle)),
            )
            .when(switching, |this| {
                this.child(Button::new("connect-cancel", ButtonVariant::Outline, ButtonSize::Sm).label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |state, cx| state.close_switcher(cx));
                })))
            })
    }

    /// The re-pair prompt after a server rejected this device (`pingdotgg/t3code#38041`: asks to
    /// pair again and names the rejected request's trace ID when present).
    fn render_rejected(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let state = self.state.read(cx);
        let rejected = state.connect.rejected.clone()?;
        let can_pair_again = rejected.ssh || (rejected.local && state.local_saved_environment().is_some_and(|local| local.id == rejected.id));
        let busy = state.connect.pairing.is_some();
        let p = palette(cx);
        let trace_id = rejected
            .trace_id
            .clone()
            .or_else(|| t3_client::status::extract_trace_id(&rejected.reason).map(str::to_owned));
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
                .child(div().font_weight(FontWeight::MEDIUM).child(format!("T3 Code on {} did not accept this device", rejected.label)))
                .child(div().opacity(0.85).child(if can_pair_again {
                    format!("This device's sign-in to {} expired or was revoked. Pair again to reconnect.", rejected.label)
                } else {
                    format!(
                        "This device's sign-in to {} expired or was revoked. Pair it again with a new pairing link from {}.",
                        rejected.label, rejected.label
                    )
                }))
                .when_some(trace_id, |this, trace| {
                    this.child(
                        div()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .opacity(0.85)
                            .child(format!("Trace ID: {trace}")),
                    )
                })
                .when(can_pair_again, |this| {
                    let (id, ssh) = (rejected.id.clone(), rejected.ssh);
                    this.child(div().pt(px(2.)).child(
                        Button::new("connect-repair", ButtonVariant::Default, ButtonSize::Sm)
                            .label(if busy { "Pairing…" } else { "Pair again" })
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.update(cx, |state, cx| if ssh { state.pair_ssh_again(&id, cx) } else { state.pair_local(cx) })
                            })),
                    ))
                }),
        )
    }

    /// "Connect to T3 Code on this Mac", or why it isn't offered.
    fn render_local(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let connect = &state.connect;
        let error = pairing_error(connect, PairingSource::Local);
        let Some(server) = connect.local_server.clone() else {
            let message = discovery_message(connect.discovering);
            return card(cx)
                .px(px(16.))
                .py(px(12.))
                .gap(px(6.))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(muted(message, cx)))
                        .child(
                            Button::new("connect-local-refresh", ButtonVariant::GhostMuted, ButtonSize::Compact)
                                .icon(Icon::RefreshCw)
                                .label("Check again")
                                .disabled(connect.discovering)
                                .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.refresh_local_server(cx)))),
                        ),
                )
                .children(error.map(|error| error_text(error, cx)))
                .into_any_element();
        };
        let busy = connect.pairing == Some(PairingSource::Local);
        let (details, blocked) = local_details(&server);
        let p = palette(cx);
        card(cx)
            .px(px(16.))
            .py(px(14.))
            .gap(px(12.))
            .child(
                h_flex()
                    .gap(px(10.))
                    .child(Icon::Server.render(px(16.), hsla(p.muted_foreground)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("T3 Code on this Mac"))
                            .child(div().truncate().child(muted(details, cx))),
                    ),
            )
            .child(
                h_flex().child(
                    Button::new("connect-local", ButtonVariant::Default, ButtonSize::Default)
                        .label(if busy { "Connecting…" } else { "Connect to T3 Code on this Mac" })
                        .disabled(busy || blocked.is_some() || connect.pairing.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.connect_local(cx)))),
                ),
            )
            .children(blocked.or(error).map(|message| error_text(message, cx)))
            .into_any_element()
    }

    fn render_cloud(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cloud = &self.state.read(cx).cloud;
        let p = palette(cx);
        let busy = cloud.busy;
        let signing_in = cloud.signing_in;
        let signed_in = cloud.identity.is_some();
        let auth = Button::new("cloud-auth", ButtonVariant::Outline, ButtonSize::Sm)
            .label(if signing_in {
                "Waiting for browser…"
            } else if signed_in {
                "Sign out"
            } else {
                "Sign in"
            })
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.update(cx, |state, cx| {
                    if signed_in {
                        state.sign_out_cloud(cx);
                    } else {
                        state.sign_in_cloud(cx);
                    }
                })
            }));
        let actions = h_flex().gap(px(8.)).child(auth).when(signing_in, |this| {
            this.child(
                Button::new("cloud-cancel", ButtonVariant::GhostMuted, ButtonSize::Sm)
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.cancel_cloud_sign_in(cx)))),
            )
        });
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
                            .child("T3 Connect"),
                    )
                    .child(muted(
                        cloud.identity.clone().unwrap_or_else(|| "Sign in to view your T3 Connect environments.".into()),
                        cx,
                    )),
            )
            .child(
                Button::new("cloud-refresh", ButtonVariant::GhostMuted, ButtonSize::IconSm)
                    .icon(Icon::RefreshCw)
                    .tooltip("Refresh T3 Connect environments")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.refresh_cloud(cx)))),
            );
        let mut body = card(cx)
            .px(px(16.))
            .py(px(14.))
            .gap(px(12.))
            .child(heading)
            .child(actions)
            .when(signed_in, |this| this.child(muted(t3_core::relay::CONNECTION_UNAVAILABLE, cx)))
            .children(cloud.error.clone().map(|error| error_text(error, cx)));
        if signed_in && cloud.discovery.refreshing {
            body = body.child(muted("Loading T3 Connect environments…", cx));
        }
        if signed_in && cloud.discovery.listed && cloud.discovery.environments.is_empty() {
            body = body.child(muted(
                "No other environments are published to your account yet. Publish one from another device to connect here.",
                cx,
            ));
        }
        let count = cloud.discovery.environments.len();
        if count > 0 {
            body = body.child(
                uniform_list(
                    "cloud-environments",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| range.map(|index| this.render_cloud_row(index, cx)).collect::<Vec<_>>()),
                )
                .w_full()
                .h(px((count as f32 * 64.).min(288.))),
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
        let status = "T3 Connect · Connection unavailable";
        h_flex()
            .w_full()
            .min_w_0()
            .h(px(64.))
            .gap(px(10.))
            .child(status_dot(None, cx))
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
                    .child(
                        div()
                            .id(SharedString::from(format!("relay-status-{}", environment.environment_id)))
                            .truncate()
                            .text_xs()
                            .text_color(hsla(palette(cx).muted_foreground))
                            .child(status)
                            .tooltip(crate::settings::controls::tooltip(t3_core::relay::CONNECTION_UNAVAILABLE.into())),
                    ),
            )
            .child(
                Button::new(
                    SharedString::from(format!("relay-{}", environment.environment_id)),
                    ButtonVariant::Outline,
                    ButtonSize::Compact,
                )
                .label("Unavailable")
                .disabled(true)
                .tooltip(t3_core::relay::CONNECTION_UNAVAILABLE),
            )
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
                .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(format!("Permissions have changed for {label}")))
                .child(
                    div()
                        .text_xs()
                        .line_height(px(18.))
                        .text_color(hsla(p.muted_foreground))
                        .child("This connection still uses the old permissions, so some actions may no longer be available. Pair again using a new link with the permissions you need."),
                )
                .child(
                    h_flex().gap(px(8.)).pt(px(2.)).child(
                        Button::new("connect-legacy-perms-dismiss", ButtonVariant::GhostMuted, ButtonSize::Compact)
                            .label("Dismiss")
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
            _ if environment.access_token.is_none() => (Some(DotTone::Destructive), "Needs pairing again".to_owned(), true),
            Some(status) => {
                let (text, error) = status.summary();
                (Some(status.dot()), text, error)
            }
            _ => (None, "Not connected".to_owned(), false),
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
                .child(Button::new(SharedString::from(format!("remove-confirm-{remove}")), ButtonVariant::DestructiveOutline, ButtonSize::Compact).label("Remove").on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm_remove = None;
                    this.state.update(cx, |state, cx| state.remove_environment(&remove, cx));
                })))
                .child(Button::new(SharedString::from(format!("remove-cancel-{keep}")), ButtonVariant::GhostMuted, ButtonSize::Compact).label("Keep").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.confirm_remove = None;
                        cx.notify();
                    },
                )))
        } else {
            let (connect, remove, diag_id) = (id.clone(), id.clone(), id.clone());
            let action = if active { None } else if environment.access_token.is_some() { Some("Connect") } else { Some("Pair again") };
            h_flex()
                .gap_1()
                .when(diagnosis.is_some(), |this| {
                    this.child(
                        Button::new(SharedString::from(format!("details-{diag_id}")), ButtonVariant::GhostMuted, ButtonSize::Compact)
                            .label(if expanded { "Hide details" } else { "Details" })
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
                .child(Button::new(SharedString::from(format!("remove-{remove}")), ButtonVariant::GhostMuted, ButtonSize::IconSm).icon(Icon::Trash2).tooltip("Remove from this device…").on_click(
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
                            .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("What to try"))
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
                                    .label("Retry now")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.read(cx).retry_environment(&retry_id);
                                    })),
                            )
                        })
                        .child(
                            Button::new(SharedString::from(format!("diag-copy-{id}")), ButtonVariant::Outline, ButtonSize::Compact)
                                .icon(Icon::Copy)
                                .label("Copy diagnostics")
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
            .child(label("Saved environments", cx))
            .when(lb_enabled && connected_count >= 2, |this| {
                let weights: HashMap<String, f64> = lb_weights.into_iter().map(|(k, v)| (k, f64::from(v))).collect();
                this.child(
                    Button::new("connect-auto-balance", ButtonVariant::Outline, ButtonSize::Compact)
                        .label("Auto balance")
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
    fn mode_card(&self, mode: AddMode, title: &'static str, description: &'static str, glyph: Icon, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let selected = self.mode == mode;
        let disabled = self.state.read(cx).connect.pairing.is_some();
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
                "On the machine running T3 Code, open Settings → Sharing & access and create a pairing link, or run `t3 pair` there. Paste the pairing URL, or enter a host and code.",
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
                            .label(if busy { "Adding…" } else { "Add environment" })
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
            empty("Loading hosts…".to_owned()).into_any_element()
        } else if unsaved.is_empty() {
            return None;
        } else if self.ssh_matches.is_empty() {
            empty(format!("No hosts match \"{}\".", query.trim())).into_any_element()
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
                    .label(if adding { "Adding…" } else { "Add environment" })
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
            .child(label("Add environment", cx))
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
                        true,
                        false,
                        |_, _| {},
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
}
