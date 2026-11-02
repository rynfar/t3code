//! Sharing & access: `ConnectionsSettings view="access"` (`web/src/components/settings/ConnectionsSettings.tsx`)
//! for the connected environment: its version and this connection's permissions, then
//! `AuthorizedClientsSettings.tsx`'s pairing links and authorized clients over
//! `subscribeAuthAccess` and the `/api/auth` access endpoints. Pairing links carry a QR code of
//! the pairing URL. The network access, Tailscale, WSL and T3 Connect rows are not ported.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, canvas, div, fill, point, prelude::FluentBuilder, px, size,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::{Value, json};
use t3_core::auth::AuthSessionState;
use t3_core::settings::access::{
    self, ACCESS_READ, ACCESS_WRITE, AccessState, ClientSession, PairingLink, READ_ONLY_SCOPES, SCOPE_OPTIONS, expires_label, pairing_token_body,
    pairing_url, scope_count, shown_scopes, standard_scopes, toggle_scope,
};

use super::controls::{Button, ButtonSize, ButtonVariant, Row, group, muted_text, r, section, section_title};
use super::providers::Stream;
use super::source_control::{Tone, tone_badge};
use super::worktrees::disclosure;
use super::{Block, SettingsView, store};
use crate::composer::controls::checkbox;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const QR_SIZE: f32 = 168.;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// The create dialog's state, open in the page.
struct CreateLink {
    scopes: Vec<String>,
    creating: bool,
    error: Option<String>,
}

pub(crate) struct AccessPageState {
    pub(crate) label: Entity<InputState>,
    access: AccessState,
    error: Option<String>,
    stream: Option<(u64, Stream)>,
    create: Option<CreateLink>,
    /// Credentials of links this client created, known only in memory (`credentialsById`).
    credentials: HashMap<String, String>,
    sharing: Option<String>,
    revoking: Option<String>,
    revoking_others: bool,
    permissions_open: bool,
    /// The last QR encoded, by URL, so countdown ticks and hovers do not re-encode it.
    qr: RefCell<Option<(String, Rc<QrMatrix>)>>,
    /// Ticks once a second while a pairing link counts down.
    ticker: Option<Task<()>>,
}

impl AccessPageState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        AccessPageState {
            label: cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Living room iPad")),
            access: AccessState::default(),
            error: None,
            stream: None,
            create: None,
            credentials: HashMap::new(),
            sharing: None,
            revoking: None,
            revoking_others: false,
            permissions_open: false,
            qr: RefCell::new(None),
            ticker: None,
        }
    }

    /// Leaving the page stops the stream and the countdown.
    pub(super) fn close(&mut self) {
        self.stream = None;
        self.ticker = None;
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

impl SettingsView {
    fn session(&self, cx: &App) -> Option<AuthSessionState> {
        let handle = self.state.read(cx).client_handle()?;
        handle.session_state().borrow().clone()
    }

    /// Whether this connection may `scope`; unknown grants leave the server to decide.
    pub(super) fn may(&self, scope: &str, cx: &App) -> bool {
        self.session(cx).is_none_or(|session| session.grants(scope))
    }

    fn ensure_access_stream(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.state.read(cx).connection_id() else {
            self.access.stream = None;
            return;
        };
        if self.access.stream.as_ref().is_some_and(|(id, _)| *id == connection) || !self.may(ACCESS_READ, cx) {
            return;
        }
        self.access.access = AccessState::default();
        self.access.error = None;
        let stream = self.subscribe(access::SUBSCRIBE, json!({}), cx, |this, item, _| match item {
            Ok(value) => this.access.access.apply(&value),
            Err(message) => this.access.error = Some(message),
        });
        self.access.stream = stream.map(|stream| (connection, stream));
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

    /// One access endpoint `POST`, then `done` on the UI thread with the reply.
    fn access_post(&mut self, path: &'static str, body: Option<Value>, cx: &mut Context<Self>, done: impl FnOnce(&mut SettingsView, Result<Value, String>, &mut Context<SettingsView>) + 'static) {
        let (client, runtime) = {
            let app = self.state.read(cx);
            (app.client_handle(), app.runtime().clone())
        };
        let Some(client) = client else { return done(self, Err("Reconnect to this environment first.".to_owned()), cx) };
        let task = runtime.spawn(async move { client.post_access(path, body).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                done(this, result, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn create_pairing_link(&mut self, cx: &mut Context<Self>) {
        let Some(create) = self.access.create.as_mut().filter(|create| !create.creating && !create.scopes.is_empty()) else { return };
        create.creating = true;
        create.error = None;
        let body = pairing_token_body(&self.access.label.read(cx).value(), &create.scopes);
        self.access_post("/api/auth/pairing-token", Some(body), cx, |this, result, _| match result {
            Ok(reply) => {
                if let (Some(id), Some(credential)) = (reply.get("id").and_then(Value::as_str), reply.get("credential").and_then(Value::as_str)) {
                    this.access.credentials.insert(id.to_owned(), credential.to_owned());
                    this.access.sharing = Some(id.to_owned());
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

    fn revoke_link(&mut self, id: String, cx: &mut Context<Self>) {
        self.access.revoking = Some(id.clone());
        self.access_post("/api/auth/pairing-links/revoke", Some(json!({ "id": id })), cx, move |this, result, cx| {
            this.access.revoking = None;
            this.access.credentials.remove(&id);
            if let Err(error) = result {
                store::show_error(format!("Could not revoke pairing link: {error}"), cx);
            }
        });
    }

    fn revoke_client(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.access.revoking = Some(session_id.clone());
        self.access_post("/api/auth/clients/revoke", Some(json!({ "sessionId": session_id })), cx, |this, result, cx| {
            this.access.revoking = None;
            if let Err(error) = result {
                store::show_error(format!("Could not revoke client access: {error}"), cx);
            }
        });
    }

    fn revoke_others(&mut self, cx: &mut Context<Self>) {
        self.access.revoking_others = true;
        self.access_post("/api/auth/clients/revoke-others", None, cx, |this, result, cx| {
            this.access.revoking_others = false;
            if let Err(error) = result {
                store::show_error(format!("Could not revoke other clients: {error}"), cx);
            }
        });
    }

    pub(super) fn access_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let Some(config) = self.thread_settings(cx).cloned() else {
            let row = Row::new("connections-environment", "No environment selected").description("Connect an environment to view its settings and access.");
            return vec![Block::new(&["connections-environment"], section("Primary environment", vec![row.into_any_element()], cx))];
        };
        self.ensure_access_stream(cx);
        let now = now_ms();
        self.ensure_ticker(!self.access.access.visible_links(now).is_empty(), cx);
        let label = if config.environment.label.is_empty() { "Primary environment".to_owned() } else { config.environment.label.clone() };
        let address = self.state.read(cx).environment().unwrap_or_default().to_owned();
        let version = Row::new("access-version", "Version")
            .description(format!("{} \u{00b7} {address}", config.environment.server_version))
            .into_any_element();
        let toggle = cx.listener(|this, _, _, cx| {
            this.access.permissions_open = !this.access.permissions_open;
            cx.notify();
        });
        let permissions = disclosure("access-permissions", "Permissions", self.access.permissions_open, vec![self.permissions_row(cx)], toggle, cx);
        let machine = section(label, vec![version, permissions], cx);
        let mut blocks = vec![Block::new(&["connections-environment"], machine)];
        if self.may(ACCESS_READ, cx) || self.may(ACCESS_WRITE, cx) {
            blocks.push(Block::new(&["authorized-clients"], self.authorized_clients(&address, now, window, cx)));
        }
        blocks
    }

    /// The "Permissions" disclosure's content: each scope and whether this connection has it.
    fn permissions_row(&self, cx: &App) -> AnyElement {
        let p = palette(cx);
        let session = self.session(cx);
        let body: AnyElement = match &session {
            None => muted_text("Checking permissions\u{2026}", cx).into_any_element(),
            Some(session) => v_flex()
                .gap(r(4.))
                .children(SCOPE_OPTIONS.iter().map(|(scope, title, _)| {
                    let allowed = session.grants(scope);
                    h_flex()
                        .justify_between()
                        .text_xs()
                        .child(div().text_color(hsla(p.foreground)).child(*title))
                        .child(div().text_color(if allowed { hsla(p.success_foreground) } else { hsla(p.muted_foreground) }).child(if allowed { "Allowed" } else { "Not granted" }))
                }))
                .into_any_element(),
        };
        v_flex()
            .px(r(16.))
            .py(r(12.))
            .gap(r(8.))
            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("These permissions apply to this client\u{2019}s current connection."))
            .child(div().max_w(r(420.)).child(body))
            .into_any_element()
    }

    fn authorized_clients(&mut self, address: &str, now: i64, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let can_write = self.may(ACCESS_WRITE, cx);
        let can_read = self.may(ACCESS_READ, cx);
        let actions = can_write.then(|| {
            h_flex()
                .gap(r(6.))
                .child(
                    Button::new("access-revoke-others", ButtonVariant::Outline, ButtonSize::Compact)
                        .label(if self.access.revoking_others { "Revoking\u{2026}" } else { "Revoke others" })
                        .disabled(self.access.revoking_others || !self.access.access.can_revoke_others())
                        .on_click(cx.listener(|this, _, _, cx| this.revoke_others(cx))),
                )
                .child(
                    Button::new("access-create-link", ButtonVariant::Outline, ButtonSize::Compact)
                        .icon(Icon::Plus)
                        .label("Create link")
                        .disabled(self.access.create.is_some())
                        .on_click(cx.listener(|this, _, window, cx| {
                            let grants = this.session(cx);
                            let allowed = |scope: &str| grants.as_ref().is_none_or(|session| session.grants(scope));
                            let scopes = standard_scopes().into_iter().filter(|scope| allowed(scope)).collect();
                            this.access.label.update(cx, |input, cx| input.set_value("", window, cx));
                            this.access.create = Some(CreateLink { scopes, creating: false, error: None });
                            cx.notify();
                        })),
                )
        });
        let header = h_flex()
            .justify_between()
            .pr(r(8.))
            .child(
                h_flex()
                    .gap(r(8.))
                    .child(section_title("Authorized clients"))
                    .when(self.access.access.loaded, |this| this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(self.access.access.summary(now)))),
            )
            .children(actions);

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(create) = &self.access.create {
            rows.push(self.create_link_panel(create, cx));
        }
        if !can_read {
            rows.push(div().px(r(16.)).py(r(12.)).child(muted_text("This connection can create access links but cannot view authorized clients.", cx)).into_any_element());
        }
        if let Some(error) = &self.access.error {
            rows.push(div().px(r(16.)).py(r(12.)).text_sm().text_color(hsla(p.destructive_foreground)).child(error.clone()).into_any_element());
        }
        let links: Vec<PairingLink> = self.access.access.visible_links(now).into_iter().cloned().collect();
        let clients: Vec<ClientSession> = self.access.access.sorted_clients().into_iter().cloned().collect();
        for link in &links {
            rows.push(self.link_row(link, address, now, can_write, cx));
        }
        for client in &clients {
            rows.push(self.client_row(client, can_write, cx));
        }
        if can_read && links.is_empty() && clients.is_empty() && self.access.error.is_none() {
            let text = if self.access.access.loaded { "No pairing links or client sessions." } else { "Loading\u{2026}" };
            rows.push(div().px(r(16.)).py(r(12.)).child(muted_text(text, cx)).into_any_element());
        }
        v_flex().gap(r(10.)).child(header).child(group(rows, cx)).into_any_element()
    }

    fn create_link_panel(&self, create: &CreateLink, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let grants = self.session(cx);
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
        v_flex()
            .px(r(16.))
            .py(r(14.))
            .gap(r(12.))
            .child(
                v_flex()
                    .gap(r(2.))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Create pairing link"))
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(
                        "Generate a one-time link that another device can use to pair with this backend as an authorized client.",
                    )),
            )
            .child(
                v_flex()
                    .gap(r(6.))
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Client label (optional)"))
                    .child(div().max_w(r(320.)).child(Input::new(&self.access.label).small().disabled(create.creating))),
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
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(*description)),
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
            .child(
                h_flex()
                    .justify_end()
                    .gap(r(8.))
                    .child(Button::new("access-create-cancel", ButtonVariant::Ghost, ButtonSize::Sm).label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.access.create = None;
                        cx.notify();
                    })))
                    .child(
                        Button::new("access-create-submit", ButtonVariant::Default, ButtonSize::Sm)
                            .label(if create.creating { "Creating\u{2026}" } else { "Create link" })
                            .disabled(create.creating || create.scopes.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.create_pairing_link(cx))),
                    ),
            )
            .into_any_element()
    }

    fn link_row(&self, link: &PairingLink, address: &str, now: i64, can_write: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let credential = self.access.credentials.get(&link.id).cloned();
        let sharing = self.access.sharing.as_deref() == Some(link.id.as_str()) && credential.is_some();
        let revoking = self.access.revoking.as_deref() == Some(link.id.as_str());
        let scopes = shown_scopes(&link.permissions, &link.scopes);
        let subline = format!("{} \u{00b7} {}", expires_label(&link.expires_at, now), scope_count(scopes));
        let share_id = link.id.clone();
        let revoke_id = link.id.clone();
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
                        .on_click(cx.listener(move |this, _, _, cx| this.revoke_link(revoke_id.clone(), cx))),
                )
            });
        let share = credential.filter(|_| sharing).map(|credential| self.share_panel(address, &credential, cx));
        v_flex()
            .px(r(16.))
            .py(r(12.))
            .gap(r(10.))
            .child(
                h_flex()
                    .gap(r(12.))
                    .child(div().size(r(8.)).flex_none().rounded_full().bg(hsla(p.warning)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(r(2.))
                            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(link.title().to_owned()))
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(subline))
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
    fn share_panel(&self, address: &str, credential: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
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
        h_flex()
            .pl(r(20.))
            .gap(r(16.))
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(r(8.))
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

    fn client_row(&self, client: &ClientSession, can_write: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let revoking = self.access.revoking.as_deref() == Some(client.session_id.as_str());
        let scopes = shown_scopes(&client.permissions, &client.scopes);
        let subline = [client.subline(), scope_count(scopes)].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" \u{00b7} ");
        let session_id = client.session_id.clone();
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
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(subline)),
            )
            .when(can_write && !client.current, |this| {
                this.child(
                    Button::new(SharedString::from(format!("access-revoke-client-{}", client.session_id)), ButtonVariant::DestructiveOutline, ButtonSize::Compact)
                        .label(if revoking { "Revoking\u{2026}" } else { "Revoke" })
                        .disabled(revoking)
                        .on_click(cx.listener(move |this, _, _, cx| this.revoke_client(session_id.clone(), cx))),
                )
            })
            .into_any_element()
    }
}
