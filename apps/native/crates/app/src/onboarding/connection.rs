//! `ConnectionStep` and `PairingForm` of `WelcomeWizard.tsx`: the computers this device can set
//! up (T3 Code on this Mac and the saved environments), "Add a computer" with a pairing link,
//! and the privacy line. Picking a computer connects it, since one is connected at a time.

use gpui::{
    AnyElement, App, Context, ElementId, FontWeight, InteractiveText, IntoElement, ParentElement, SharedString, StyledText, Styled, TextRun,
    UnderlineStyle, Window, div, prelude::*, px,
};
use gpui_component::input::Input;
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use t3_client::local::LocalServer;
use t3_client::status::ConnectionPhase;
use t3_client::store::SavedEnvironment;
use t3_client::supervisor::ConnectionStatus;
use t3_core::onboarding::{PRIVACY_POLICY_URL, privacy_notice};

use super::{Step, WelcomeWizard, key_stop, row_card, step_title};
use crate::app_state::{AppState, Phase};
use crate::composer::controls::radio;
use crate::connect::{discovery_message, local_details, pairing_error};
use crate::environments::PairingSource;
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, r};
use crate::theme::{hsla, palette};

/// `presentSavedCloudEnvironmentConnection(...).buttonLabel` for the connected environment.
fn connection_label(status: &ConnectionStatus) -> &'static str {
    match status.phase() {
        ConnectionPhase::Connected => "Connected",
        ConnectionPhase::Connecting => "Connecting…",
        ConnectionPhase::Reconnecting => "Reconnecting…",
        ConnectionPhase::Unsupported => "Client not supported",
        ConnectionPhase::Error => "Connection failed",
    }
}

/// One computer to set up.
#[derive(Debug, PartialEq)]
struct Computer {
    id: SharedString,
    label: String,
    status: &'static str,
    detail: String,
    /// A reason under the address: why it cannot connect, or why it last failed.
    note: Option<String>,
    checked: bool,
    action: Option<Pick>,
}

#[derive(Debug, Clone, PartialEq)]
enum Pick {
    Local,
    Saved(String),
}

/// What the connection step lists, read from the app state.
struct Listing<'a> {
    saved: &'a [SavedEnvironment],
    /// The connected environment's id and status.
    active: Option<(&'a str, &'a ConnectionStatus)>,
    local: Option<&'a LocalServer>,
    /// The saved entry for T3 Code on this Mac. It stands in for the local row, so it also shows
    /// the `t3 pair` that pairs it again and how that failed.
    local_saved: Option<&'a str>,
    pairing: Option<PairingSource>,
    error: Option<(PairingSource, &'a str)>,
}

fn computers(listing: &Listing) -> Vec<Computer> {
    let local_pairing = listing.pairing == Some(PairingSource::Local);
    let local_error = listing.error.filter(|(source, _)| *source == PairingSource::Local).map(|(_, message)| message.to_owned());
    let mut computers: Vec<Computer> = listing
        .saved
        .iter()
        .map(|environment| {
            let online = listing.active.filter(|(id, _)| *id == environment.id).map(|(_, status)| status);
            let local = listing.local_saved == Some(environment.id.as_str());
            let status = match online {
                _ if local && local_pairing => "Connecting…",
                _ if environment.access_token.is_none() => "Needs pairing again",
                Some(status) => connection_label(status),
                None => "Not connected",
            };
            let unsupported = online.filter(|status| status.phase() == ConnectionPhase::Unsupported).and_then(|status| status.error()).map(str::to_owned);
            Computer {
                id: format!("welcome-computer-{}", environment.id).into(),
                label: environment.label.clone(),
                status,
                detail: environment.address(),
                note: unsupported.or_else(|| local_error.clone().filter(|_| local)),
                checked: online.is_some(),
                action: online.is_none().then(|| Pick::Saved(environment.id.clone())),
            }
        })
        .collect();
    if let Some(server) = listing.local.filter(|_| listing.local_saved.is_none()) {
        let (detail, blocked) = local_details(server);
        computers.insert(
            0,
            Computer {
                id: "welcome-computer-local".into(),
                label: "T3 Code on this Mac".into(),
                status: if local_pairing { "Connecting…" } else { "Not connected" },
                detail,
                action: blocked.is_none().then_some(Pick::Local),
                note: blocked.or(local_error),
                checked: false,
            },
        );
    }
    computers
}

/// Text with mono spans, as the web's inline `<code>`.
fn with_code(segments: &[(&str, bool)], cx: &App) -> StyledText {
    let p = palette(cx);
    let (sans, mono) = (cx.theme().font_family.clone(), cx.theme().mono_font_family.clone());
    let runs = segments
        .iter()
        .map(|(text, code)| TextRun {
            len: text.len(),
            font: gpui::font(if *code { mono.clone() } else { sans.clone() }),
            color: hsla(p.muted_foreground),
            background_color: None,
            underline: None,
            strikethrough: None,
        })
        .collect();
    StyledText::new(segments.iter().map(|(text, _)| *text).collect::<String>()).with_runs(runs)
}

impl WelcomeWizard {
    fn computers(&self, app: &AppState) -> Vec<Computer> {
        let connect = &app.connect;
        let active = match (&app.phase, app.environments.active.as_deref(), app.client_handle()) {
            (Phase::Online(status), Some(id), Some(_)) => Some((id, status)),
            _ => None,
        };
        computers(&Listing {
            saved: &app.environments.environments,
            active,
            local: connect.local_server.as_ref(),
            local_saved: app.local_saved_environment().map(|environment| environment.id.as_str()),
            pairing: connect.pairing,
            error: connect.error.as_ref().map(|(source, message)| (*source, message.as_str())),
        })
    }

    fn render_computer(&self, computer: Computer, busy: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let action = computer.action.filter(|_| !busy);
        let id = ElementId::Name(computer.id);
        let row = row_card(cx).id(id.clone()).gap(r(12.)).px(r(12.)).py(r(12.));
        let row = match action {
            Some(pick) => {
                let connect = cx.listener(move |this, _: &(), _, cx| {
                    this.state.update(cx, |state, cx| match &pick {
                        Pick::Local => state.connect_local(cx),
                        Pick::Saved(id) => state.connect_environment(id, cx),
                    })
                });
                key_stop(row, &id, connect, window, cx)
            }
            None => row,
        };
        row.child(radio(computer.checked, cx))
            .child(Icon::Monitor.render(px(16.), hsla(p.muted_foreground)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .items_baseline()
                            .justify_between()
                            .gap(r(12.))
                            .child(div().min_w_0().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(computer.label))
                            .child(div().flex_none().text_xs().text_color(hsla(p.muted_foreground)).child(computer.status)),
                    )
                    .child(div().mt(r(2.)).text_xs().text_color(hsla(p.muted_foreground)).child(computer.detail))
                    .children(computer.note.map(|note| div().mt(r(2.)).text_xs().text_color(hsla(p.destructive_foreground)).child(note))),
            )
            .into_any_element()
    }

    /// Why T3 Code on this Mac is missing from the list, with a way to look again.
    fn render_discovery(&self, discovering: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        row_card(cx)
            .gap(r(12.))
            .px(r(12.))
            .py(r(8.))
            .child(Icon::Monitor.render(px(16.), hsla(p.muted_foreground)))
            .child(div().flex_1().min_w_0().text_xs().line_height(r(18.)).text_color(hsla(p.muted_foreground)).child(discovery_message(discovering)))
            .child(
                Button::new("welcome-local-refresh", ButtonVariant::GhostMuted, ButtonSize::Compact)
                    .icon(Icon::RefreshCw)
                    .label("Check again")
                    .disabled(discovering)
                    .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.refresh_local_server(cx)))),
            )
            .into_any_element()
    }

    /// `PairingForm`: a pairing link, its error, and how to get one.
    fn render_pairing_form(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let app = self.state.read(cx);
        let busy = app.connect.pairing.is_some();
        let pairing = app.connect.pairing == Some(PairingSource::Link);
        let error = pairing_error(&app.connect, PairingSource::Link);
        let empty = self.link.read(cx).value().trim().is_empty();
        let help_open = self.pairing_help_open;
        let help_id = ElementId::from("welcome-pairing-help");
        let help = h_flex()
            .id(help_id.clone())
            .gap(r(4.))
            .rounded(r(4.))
            .text_sm()
            .text_color(hsla(p.muted_foreground))
            .hover(move |style| style.text_color(hsla(p.foreground)))
            .child(if help_open { Icon::ChevronDown } else { Icon::ChevronRight }.render(px(14.), hsla(p.muted_foreground)))
            .child("Need a pairing link?");
        let toggle = cx.listener(|this, _: &(), _, cx| {
            this.pairing_help_open = !this.pairing_help_open;
            cx.notify();
        });
        let help = key_stop(help, &help_id, toggle, window, cx);
        v_flex()
            .gap(r(12.))
            .child(
                v_flex()
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child("Pairing link"))
                    .child(div().mt(r(8.)).child(Input::new(&self.link).large().disabled(busy))),
            )
            .children(error.map(|error| {
                div()
                    .px(r(14.))
                    .py(r(12.))
                    .rounded(r(12.))
                    .border_1()
                    .border_color(hsla(p.error).opacity(0.32))
                    .bg(hsla(p.error_surface))
                    .text_sm()
                    .text_color(hsla(p.error_foreground))
                    .child(error)
            }))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(r(12.))
                    .child(help)
                    .child(
                        Button::new("welcome-pair", ButtonVariant::Default, ButtonSize::Default)
                            .label(if pairing { "Pairing..." } else { "Pair" })
                            .disabled(busy || empty)
                            .on_click(cx.listener(|this, _, _, cx| this.pair(cx))),
                    ),
            )
            .when(help_open, |this| {
                this.child(
                    v_flex()
                        .text_sm()
                        .child(with_code(&[("On the computer with your code, start T3 Code or run ", false), ("npx t3 serve", true), (", then run:", false)], cx))
                        .child(div().mt(r(8.)).child(self.command_block("welcome-copy-pair", "npx t3 pair", cx)))
                        .child(
                            div()
                                .mt(r(8.))
                                .text_xs()
                                .line_height(r(19.5))
                                .child(with_code(&[("On a tailnet, run ", false), ("npx t3 pair --tailscale", true), (" instead.", false)], cx)),
                        ),
                )
            })
            .into_any_element()
    }

    /// "Add a computer": a collapsible pairing form.
    fn render_add_computer(&self, open: bool, busy: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let form = open.then(|| div().px(r(12.)).pb(r(12.)).child(self.render_pairing_form(window, cx)));
        let id = ElementId::from("welcome-add-computer");
        let trigger = h_flex()
            .id(id.clone())
            .min_h(r(56.))
            .px(r(9.))
            .gap(r(6.))
            .rounded(r(7.))
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(hsla(p.foreground))
            .child(Icon::Link.render(px(16.), hsla(p.muted_foreground)))
            .child(div().flex_1().child("Add a computer"))
            .child(if open { Icon::ChevronDown } else { Icon::ChevronRight }.render(px(16.), hsla(p.muted_foreground)));
        let trigger = if busy {
            trigger.opacity(0.64)
        } else {
            let toggle = cx.listener(move |this, _: &(), _, cx| {
                this.pairing_open = Some(!open);
                cx.notify();
            });
            key_stop(trigger, &id, toggle, window, cx).hover(move |style| style.bg(hsla(p.accent)))
        };
        row_card(cx).flex_col().items_stretch().child(trigger).children(form).into_any_element()
    }

    pub(super) fn render_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let app = self.state.read(cx);
        let busy = app.connect.pairing.is_some();
        let ready = app.phase == Phase::Online(ConnectionStatus::Connected) && !busy;
        let (discovering, local_found) = (app.connect.discovering, app.connect.local_server.is_some());
        let computers = self.computers(app);
        let policy = app.server_config.config.as_ref().and_then(|config| config.managed_policy.clone());
        // `expandPairingInitially`: open while there is nothing else to connect to. A pairing-link
        // failure (pasted, or a saved computer that needs a new link) keeps the form and its error open.
        let link_failed = pairing_error(&app.connect, PairingSource::Link).is_some();
        let pairing_open = link_failed || self.pairing_open.unwrap_or(computers.is_empty() && !discovering);
        let rows: Vec<AnyElement> = computers.into_iter().map(|computer| self.render_computer(computer, busy, window, cx)).collect();
        let notice = privacy_notice(policy.as_ref());
        let link = "privacy policy";
        let text = format!("{notice} {link}.");
        let link_range = notice.len() + 1..notice.len() + 1 + link.len();
        let privacy = InteractiveText::new(
            "welcome-privacy",
            StyledText::new(text).with_highlights([(link_range.clone(), gpui::HighlightStyle { underline: Some(UnderlineStyle { thickness: px(1.), ..Default::default() }), ..Default::default() })]),
        )
        .on_click(vec![link_range], |_, _, cx| cx.open_url(PRIVACY_POLICY_URL));
        v_flex()
            .child(step_title("Connect your computers", "Choose a computer to set up. We’ll check its agents next.", cx))
            .when(!rows.is_empty(), |this| this.child(v_flex().mt(r(20.)).gap(r(8.)).children(rows)))
            .child(
                v_flex()
                    .mt(r(16.))
                    .gap(r(8.))
                    .when(!local_found, |this| this.child(self.render_discovery(discovering, cx)))
                    .child(self.render_add_computer(pairing_open, busy, window, cx)),
            )
            .child(
                h_flex()
                    .mt(r(24.))
                    .items_end()
                    .justify_between()
                    .gap(r(24.))
                    .child(div().flex_1().min_w_0().text_xs().line_height(r(19.5)).text_color(hsla(p.muted_foreground)).child(privacy))
                    .child(
                        Button::new("welcome-continue", ButtonVariant::Default, ButtonSize::Default)
                            .label("Continue")
                            .trailing(Icon::ArrowRight)
                            .disabled(!ready)
                            .on_click(cx.listener(|this, _, _, cx| this.go_to(Step::Agents, cx))),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use t3_client::local::LocalServer;
    use t3_client::store::SavedEnvironment;
    use t3_client::supervisor::ConnectionStatus;

    use super::{Listing, Pick, computers};
    use crate::environments::PairingSource;

    fn saved(id: &str, token: Option<&str>) -> SavedEnvironment {
        SavedEnvironment {
            id: id.into(),
            environment_id: Some(format!("env-{id}")),
            label: format!("Computer {id}"),
            http_base: format!("http://127.0.0.1:{}", if id == "mac" { 3773 } else { 4000 }),
            access_token: token.map(str::to_owned),
            server_version: Some("0.1.6".into()),
            local: id == "mac",
            ssh: None,
            relay: None,
        }
    }

    fn local_server() -> LocalServer {
        LocalServer {
            home: PathBuf::from("/tmp/t3"),
            state: serde_json::from_value(serde_json::json!({"version": 1, "pid": 1, "port": 3773, "origin": "http://127.0.0.1:3773", "serverVersion": "0.1.6"})).unwrap(),
            descriptor: serde_json::from_value(serde_json::json!({"environmentId": "env-mac", "label": "Mac", "serverVersion": "0.1.6", "orchestrationProtocolVersion": t3_core::compatibility::ORCHESTRATION_PROTOCOL_VERSION})).unwrap(),
        }
    }

    #[test]
    fn a_saved_local_computer_shows_its_own_repairing_and_why_it_failed() {
        let environments = [saved("mac", None), saved("lab", Some("token"))];
        let server = local_server();
        let listing = |pairing, error| Listing {
            saved: &environments,
            active: None,
            local: Some(&server),
            local_saved: Some("mac"),
            pairing,
            error,
        };
        // The saved entry stands in for T3 Code on this Mac: no second row for it.
        let idle = computers(&listing(None, None));
        assert_eq!(idle.len(), 2);
        assert_eq!((idle[0].status, idle[0].action.clone()), ("Needs pairing again", Some(Pick::Saved("mac".into()))));
        // Picking it runs `t3 pair`; the row says so, then shows why it failed.
        assert_eq!(computers(&listing(Some(PairingSource::Local), None))[0].status, "Connecting…");
        let failed = computers(&listing(None, Some((PairingSource::Local, "The t3 CLI could not pair."))));
        assert_eq!(failed[0].note.as_deref(), Some("The t3 CLI could not pair."));
        assert_eq!(failed[1].note, None);
    }

    #[test]
    fn an_unsaved_local_server_has_its_own_row_with_its_pairing_failure() {
        let environments = [saved("lab", Some("token"))];
        let server = local_server();
        let status = ConnectionStatus::Connected;
        let listed = computers(&Listing {
            saved: &environments,
            active: Some(("lab", &status)),
            local: Some(&server),
            local_saved: None,
            pairing: None,
            error: Some((PairingSource::Local, "The t3 CLI could not pair.")),
        });
        assert_eq!(listed[0].action, Some(Pick::Local));
        assert_eq!(listed[0].note.as_deref(), Some("The t3 CLI could not pair."));
        assert_eq!((listed[1].status, listed[1].checked, listed[1].action.clone()), ("Connected", true, None));
    }
}
