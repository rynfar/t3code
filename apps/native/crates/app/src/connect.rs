//! The connect screen: connect to T3 Code on this Mac in one click, pick a saved environment, or
//! add one with a pairing link or over SSH. Over a live connection it is the environment switcher.
//! Web: `settings/SavedEnvironmentRows.tsx` (rows and their status copy),
//! `settings/AddEnvironmentDialog.tsx` (the Remote link and SSH modes), `ConnectionStatusDot.tsx`.
//! T3 Connect rows mirror `components/cloud/CloudEnvironmentConnectList.tsx`.
//! The SSH host suggestions list inline under the field rather than in a popup.

use gpui::{
    App, Context, Div, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, prelude::*, px,
    uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use t3_client::local::LocalServer;
use t3_client::ssh::{DiscoveredSshHost, SshTarget};
use t3_client::status::DotTone;
use t3_client::store::SavedEnvironment;
use t3_core::ssh::{filter_hosts, format_target, parse_manual_target};

use crate::app_state::{AppEvent, AppState, Phase};
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
    mode: AddMode,
    ssh_host: Entity<InputState>,
    ssh_username: Entity<InputState>,
    ssh_port: Entity<InputState>,
    /// The unsaved discovered hosts that match the host field, as last rendered.
    ssh_matches: Vec<DiscoveredSshHost>,
    /// The saved environment whose Remove awaits confirmation.
    confirm_remove: Option<String>,
    /// A pasted link is pairing; the field clears once it succeeds.
    submitted: bool,
    /// An SSH host is being added; the fields clear once it succeeds.
    ssh_submitted: bool,
    _subscriptions: Vec<Subscription>,
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
        let link = cx.new(|cx| InputState::new(window, cx).placeholder("https://your-server:3773/pair#token=…"));
        let ssh_host = cx.new(|cx| InputState::new(window, cx).placeholder("Search hosts or type devbox"));
        let ssh_username = cx.new(|cx| InputState::new(window, cx).placeholder("root"));
        let ssh_port = cx.new(|cx| InputState::new(window, cx).placeholder("22"));
        let mut subscriptions = vec![
            cx.subscribe_in(&link, window, |this, _, event: &InputEvent, _, cx| {
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
            mode: AddMode::Link,
            ssh_host,
            ssh_username,
            ssh_port,
            ssh_matches: Vec::new(),
            confirm_remove: None,
            submitted: false,
            ssh_submitted: false,
            _subscriptions: subscriptions,
        }
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).value().to_string();
        self.state.update(cx, |state, cx| state.pair_link(&link, cx));
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

    /// A suggested host connects at once, as `ssh -G` resolves it.
    fn add_ssh_host(&mut self, alias: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).connect.pairing.is_some() {
            return;
        }
        self.ssh_host.update(cx, |input, cx| input.set_value(alias.clone(), window, cx));
        let target = SshTarget { alias: alias.clone(), hostname: alias, username: None, port: None };
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

    /// The re-pair prompt after a server rejected this device.
    fn render_rejected(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let state = self.state.read(cx);
        let rejected = state.connect.rejected.clone()?;
        let can_pair_again = rejected.ssh || (rejected.local && state.local_saved_environment().is_some_and(|local| local.id == rejected.id));
        let busy = state.connect.pairing.is_some();
        let p = palette(cx);
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
                .child(div().opacity(0.8).child(if can_pair_again {
                    format!("{} Pair again to reconnect.", rejected.reason)
                } else {
                    format!("{} Paste a new pairing link from {} below to reconnect.", rejected.reason, rejected.label)
                }))
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
                        cloud
                            .identity
                            .clone()
                            .unwrap_or_else(|| "Sign in to connect to your environments from anywhere.".into()),
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
        let environment = entry.environment.clone();
        let mismatch = entry
            .status
            .as_ref()
            .and_then(|s| s.descriptor.as_ref())
            .and_then(t3_core::compatibility::protocol_mismatch);
        let status = entry.error.clone().unwrap_or_else(|| {
            entry
                .status
                .as_ref()
                .map(|s| {
                    if s.offline_reason.is_some() { return s.message().to_owned(); }
                    match s.status {
                        t3_core::relay::Availability::Online => "Relay online",
                        t3_core::relay::Availability::Offline => "Relay offline",
                        t3_core::relay::Availability::Unknown => "Checking relay status…",
                    }.to_owned()
                })
                .unwrap_or_else(|| "Checking relay status…".into())
        });
        let disabled = cloud.busy || mismatch.is_some() || state.connect.pairing.is_some();
        let saved = state
            .environments
            .environments
            .iter()
            .any(|e| e.relay.is_some() && e.environment_id.as_deref() == Some(environment.environment_id.as_str()));
        let tone = if entry.error.is_some() {
            Some(DotTone::Destructive)
        } else {
            match entry.status.as_ref().map(|s| &s.status) {
                Some(t3_core::relay::Availability::Online) => Some(DotTone::Success),
                _ => None,
            }
        };
        h_flex()
            .w_full()
            .min_w_0()
            .h(px(64.))
            .gap(px(10.))
            .child(status_dot(tone, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap(px(4.))
                    .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(palette(cx).foreground)).child(environment.label.clone()))
                    .child(
                        div()
                            .id(SharedString::from(format!("relay-status-{}", environment.environment_id)))
                            .truncate()
                            .text_xs()
                            .text_color(hsla(palette(cx).muted_foreground))
                            .child(format!("T3 Connect · {status}"))
                            .tooltip(crate::settings::controls::tooltip(status.into())),
                    ),
            )
            .when(!saved, |this| {
                this.child(
                    Button::new(
                        SharedString::from(format!("relay-{}", environment.environment_id)),
                        ButtonVariant::Outline,
                        ButtonSize::Compact,
                    )
                    .label("Add")
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| this.state.update(cx, |state, cx| state.connect_cloud_environment(environment.clone(), cx)))),
                )
            })
            .into_any_element()
    }

    fn render_saved_row(&self, environment: &SavedEnvironment, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let active = state.environments.active.as_deref() == Some(environment.id.as_str()) && state.client_handle().is_some();
        let (tone, status, error_tone) = match &state.phase {
            _ if environment.access_token.is_none() && environment.relay.is_none() => (Some(DotTone::Destructive), "Needs pairing again".to_owned(), true),
            Phase::Online(status) if active => {
                let (text, error) = status.summary();
                (Some(status.dot()), text, error)
            }
            _ => (None, "Not connected".to_owned(), false),
        };
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
            let (connect, remove) = (id.clone(), id.clone());
            let action = if active { None } else if environment.access_token.is_some() || environment.relay.is_some() { Some("Connect") } else { Some("Pair again") };
            h_flex()
                .gap_1()
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
        h_flex()
            .px(px(16.))
            .py(px(10.))
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
                    ),
            )
            .child(actions)
    }

    fn render_saved(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let environments = self.state.read(cx).environments.environments.clone();
        if environments.is_empty() {
            return None;
        }
        let divider = hsla(palette(cx).border).opacity(0.5);
        let rows: Vec<_> = environments
            .iter()
            .enumerate()
            .map(|(index, environment)| div().when(index > 0, |this| this.border_t_1().border_color(divider)).child(self.render_saved_row(environment, cx)))
            .collect();
        Some(v_flex().gap(px(8.)).child(label("Saved environments", cx)).child(card(cx).children(rows)))
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
                "On the machine running T3 Code, open Settings → Sharing & access and create a pairing link, or run `t3 pair` there. Paste the full pairing URL here.",
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.link)))
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
        let address = format_target(&host.target());
        let alias = host.alias.clone();
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
            .when(!busy, |this| this.on_click(cx.listener(move |this, _, window, cx| this.add_ssh_host(alias.clone(), window, cx))))
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
                        .child(self.render_cloud(cx))
                        .child(self.render_local(cx))
                        .children(self.render_saved(cx))
                        .child(self.render_add(cx)),
                ),
            )
    }
}
