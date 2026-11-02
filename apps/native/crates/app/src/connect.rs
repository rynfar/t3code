//! The connect screen: connect to T3 Code on this Mac in one click, pick a saved environment, or
//! add one with a pairing link. Over a live connection it is the environment switcher.
//! Web: `settings/SavedEnvironmentRows.tsx` (rows and their status copy),
//! `settings/AddEnvironmentDialog.tsx` (paste a pairing URL), `ConnectionStatusDot.tsx`.

use gpui::{App, Context, Div, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use t3_client::local::LocalServer;
use t3_client::status::DotTone;
use t3_client::store::SavedEnvironment;

use crate::app_state::{AppEvent, AppState, Phase};
use crate::environments::{ConnectScreen, PairingSource};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

pub struct ConnectView {
    state: Entity<AppState>,
    link: Entity<InputState>,
    /// The saved environment whose Remove awaits confirmation.
    confirm_remove: Option<String>,
    /// A pasted link is pairing; the field clears once it succeeds.
    submitted: bool,
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

/// `SettingsGroup`: radius 12, `border` at 60%, `card` at 40%.
fn card(cx: &App) -> Div {
    let p = palette(cx);
    v_flex().rounded(px(12.)).border_1().border_color(hsla(p.border).opacity(0.6)).bg(hsla(p.card).opacity(0.4))
}

impl ConnectView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let link = cx.new(|cx| InputState::new(window, cx).placeholder("https://your-server:3773/pair#token=…"));
        let subscriptions = vec![
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
                cx.notify();
            }),
        ];
        ConnectView { state, link, confirm_remove: None, submitted: false, _subscriptions: subscriptions }
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).value().to_string();
        self.state.update(cx, |state, cx| state.pair_link(&link, cx));
        self.submitted = self.state.read(cx).connect.pairing == Some(PairingSource::Link);
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
        let can_pair_locally = rejected.local && state.local_saved_environment().is_some_and(|local| local.id == rejected.id);
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
                .child(div().opacity(0.8).child(if can_pair_locally {
                    format!("{} Pair again to reconnect.", rejected.reason)
                } else {
                    format!("{} Paste a new pairing link from {} below to reconnect.", rejected.reason, rejected.label)
                }))
                .when(can_pair_locally, |this| {
                    this.child(div().pt(px(2.)).child(
                        Button::new("connect-repair", ButtonVariant::Default, ButtonSize::Sm)
                            .label(if busy { "Pairing…" } else { "Pair again" })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.pair_local(cx)))),
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

    fn render_saved_row(&self, environment: &SavedEnvironment, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let active = state.environments.active.as_deref() == Some(environment.id.as_str()) && state.client_handle().is_some();
        let (tone, status, error_tone) = match &state.phase {
            _ if environment.access_token.is_none() => (Some(DotTone::Destructive), "Needs pairing again".to_owned(), true),
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
            let action = if active { None } else if environment.access_token.is_some() { Some("Connect") } else { Some("Pair again") };
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
                            .child(format!("{} · {status}", environment.http_base)),
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

    fn render_add(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let busy = state.connect.pairing == Some(PairingSource::Link);
        let primary = state.connect.local_server.is_none() && state.environments.environments.is_empty();
        let error = pairing_error(&state.connect, PairingSource::Link);
        v_flex()
            .gap(px(8.))
            .child(label("Add environment", cx))
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
                        .child(self.render_local(cx))
                        .children(self.render_saved(cx))
                        .child(self.render_add(cx)),
                ),
            )
    }
}
