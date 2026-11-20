//! Collapsible Phone section on the Welcome page (`PhoneSection` + `PhoneConnectLink` in
//! `apps/web/src/components/onboarding/WelcomePage.tsx`).

use std::sync::OnceLock;
use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, Context, FontWeight, IntoElement, ParentElement, Styled, Task, canvas,
    div, fill, point, prelude::*, px, size,
};
use gpui_component::{h_flex, v_flex};
use t3_client::supervisor::ConnectionStatus;
use t3_core::mcp::gate::CloudLinkState;
use t3_core::onboarding::{
    MOBILE_APP_URL, PHONE_LINK_REFRESH_INTERVAL_SECS, PhoneConnectInput, PhoneConnectState,
    WelcomePhoneOfferInput, phone_account_for_identity, resolve_phone_connect_state,
    welcome_offers_phone,
};
use t3_core::settings::search::Section;

use super::{WelcomeWizard, row_card};
use crate::app_state::{AppState, Phase};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, r, tooltip};
use crate::theme::{hsla, palette};

const PHONE_QR_SIZE: f32 = 124.;
const PHONE_QR_MARGIN_MODULES: usize = 2;

struct PhoneQrMatrix {
    width: usize,
    dark: Vec<bool>,
}

fn mobile_app_qr() -> Option<&'static PhoneQrMatrix> {
    static CACHE: OnceLock<Option<PhoneQrMatrix>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let code = qrcode::QrCode::with_error_correction_level(
                MOBILE_APP_URL.as_bytes(),
                qrcode::EcLevel::M,
            )
            .ok()?;
            Some(PhoneQrMatrix {
                width: code.width(),
                dark: code
                    .to_colors()
                    .into_iter()
                    .map(|color| color == qrcode::Color::Dark)
                    .collect(),
            })
        })
        .as_ref()
}

fn qr_element(matrix: &'static PhoneQrMatrix, title: &str) -> AnyElement {
    let title: gpui::SharedString = title.to_owned().into();
    let element = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let width = matrix.width;
            let margin = PHONE_QR_MARGIN_MODULES;
            let unit = f32::from(bounds.size.width) / (width + margin * 2) as f32;
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
                    let origin = point(
                        bounds.origin.x + px(unit * (start + margin) as f32),
                        bounds.origin.y + px(unit * (y + margin) as f32),
                    );
                    window.paint_quad(fill(
                        Bounds::new(origin, size(px(unit * (x - start) as f32), px(unit))),
                        gpui::black(),
                    ));
                }
            }
        },
    )
    .size(r(PHONE_QR_SIZE));
    div()
        .id("welcome-phone-qr")
        .debug_selector(|| "welcome-phone-qr".to_owned())
        .flex_none()
        .rounded(r(8.))
        .bg(gpui::white())
        .p(r(6.))
        .tooltip(tooltip(title))
        .child(element)
        .into_any_element()
}

fn is_primary_environment(app: &AppState) -> bool {
    if let Some(active) = app.active_environment() {
        if let Some(local) = app.local_saved_environment() {
            return active.id == local.id;
        }
        return active.local;
    }
    app.environments.environments.is_empty() && app.server_config.config.is_some()
}

fn suppress_background_phone_io(cx: &App) -> bool {
    crate::snapshot_capture::is_os_side_effects_disabled() || crate::chatgpt_setup::fixture_active(cx)
}

pub(super) struct PhoneState {
    pub(super) open: bool,
    pub(super) generation: u64,
    pub(super) cloud_configured: bool,
    pub(super) cloud_checked: bool,
    pub(super) link_state: Option<CloudLinkState>,
    pub(super) link_pending: bool,
    pub(super) link_error: Option<String>,
    pub(super) fetch_task: Option<Task<()>>,
    pub(super) poll_task: Option<Task<()>>,
    #[cfg(test)]
    pub(super) sign_in_clicks: usize,
}

impl PhoneState {
    pub(super) fn new() -> Self {
        Self {
            open: false,
            generation: 0,
            cloud_configured: t3_client::connect::Config::load().is_ok(),
            cloud_checked: false,
            link_state: None,
            link_pending: false,
            link_error: None,
            fetch_task: None,
            poll_task: None,
            #[cfg(test)]
            sign_in_clicks: 0,
        }
    }
}

fn phone_step(step: usize, content: AnyElement, cx: &App) -> AnyElement {
    let p = palette(cx);
    h_flex()
        .items_start()
        .gap(r(12.))
        .child(
            div()
                .w(r(12.))
                .flex_none()
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child(step.to_string()),
        )
        .child(v_flex().flex_1().min_w_0().items_start().gap(r(8.)).child(content))
        .into_any_element()
}

impl WelcomeWizard {
    pub(super) fn show_phone(&self, cx: &App) -> bool {
        let app = self.state.read(cx);
        if app.phase != Phase::Online(ConnectionStatus::Connected) {
            return false;
        }
        let managed_policy = app
            .server_config
            .config
            .as_ref()
            .and_then(|config| config.managed_policy.as_ref());
        welcome_offers_phone(WelcomePhoneOfferInput {
            is_primary: is_primary_environment(app),
            cloud_configured: self.phone.cloud_configured,
            can_share: self.can_read_relay && self.can_write_relay,
            managed_policy,
        })
    }

    pub(super) fn stop_phone_tasks(&mut self) {
        self.phone.generation = self.phone.generation.wrapping_add(1);
        self.phone.fetch_task = None;
        self.phone.poll_task = None;
    }

    pub(super) fn reset_phone(&mut self, collapse: bool) {
        self.stop_phone_tasks();
        self.phone.link_state = None;
        self.phone.link_pending = false;
        self.phone.link_error = None;
        if collapse {
            self.phone.open = false;
        }
    }

    pub(super) fn on_phone_phase_change(
        &mut self,
        connection_changed: bool,
        connected: bool,
        cx: &mut Context<Self>,
    ) {
        let cloud_ready = {
            let cloud = &self.state.read(cx).cloud;
            cloud.identity.is_some() || !cloud.busy
        };
        if cloud_ready {
            self.phone.cloud_checked = true;
        }
        if !connected || connection_changed {
            self.reset_phone(false);
            return;
        }
        if self.open && self.phone.open && !suppress_background_phone_io(cx) {
            let signed_in = self.state.read(cx).cloud.identity.is_some();
            if signed_in
                && self.phone.link_state.is_none()
                && !self.phone.link_pending
                && self.phone.link_error.is_none()
            {
                self.refresh_phone_link_state(cx);
            }
            self.ensure_phone_poll(cx);
        }
    }

    fn toggle_phone(&mut self, cx: &mut Context<Self>) {
        self.phone.open = !self.phone.open;
        if self.phone.open {
            if !suppress_background_phone_io(cx) {
                let need_cloud_check = {
                    let cloud = &self.state.read(cx).cloud;
                    !self.phone.cloud_checked && cloud.identity.is_none() && !cloud.busy
                };
                if need_cloud_check && self.phone.cloud_configured {
                    self.state.update(cx, |state, cx| state.refresh_cloud(cx));
                }
                self.refresh_phone_link_state(cx);
                self.ensure_phone_poll(cx);
            }
        } else {
            self.stop_phone_tasks();
        }
        cx.notify();
    }

    fn refresh_phone_link_state(&mut self, cx: &mut Context<Self>) {
        if suppress_background_phone_io(cx)
            || !self.open
            || !self.phone.open
            || !self.can_read_relay
            || !self.phone.cloud_configured
        {
            return;
        }
        let (client, runtime) = {
            let app = self.state.read(cx);
            let Some(client) = app.client_handle() else {
                return;
            };
            (client, app.runtime().clone())
        };
        let generation = self.phone.generation;
        self.phone.link_pending = true;
        self.phone.link_error = None;
        self.phone.fetch_task = Some(cx.spawn(async move |this, cx| {
            let fetched = runtime
                .spawn(async move {
                    client.get_access("/api/connect/link-state").await.and_then(|value| {
                        serde_json::from_value::<CloudLinkState>(value)
                            .map_err(|_| "Could not read environment cloud link state.".to_owned())
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.open || !this.phone.open || this.phone.generation != generation {
                    return;
                }
                this.phone.link_pending = false;
                this.phone.fetch_task = None;
                match fetched {
                    Ok(Ok(link_state)) => {
                        this.phone.link_state = Some(link_state);
                        this.phone.link_error = None;
                    }
                    Ok(Err(error)) => {
                        this.phone.link_state = None;
                        this.phone.link_error = Some(error);
                    }
                    Err(_) => {
                        this.phone.link_state = None;
                        this.phone.link_error =
                            Some("Could not read T3 Connect link state.".to_owned());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn ensure_phone_poll(&mut self, cx: &mut Context<Self>) {
        if suppress_background_phone_io(cx)
            || !self.open
            || !self.phone.open
            || !self.can_read_relay
            || !self.phone.cloud_configured
            || self.phone.poll_task.is_some()
        {
            return;
        }
        let generation = self.phone.generation;
        self.phone.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(PHONE_LINK_REFRESH_INTERVAL_SECS))
                    .await;
                let keep_going = this.update(cx, |this, cx| {
                    if !this.open || !this.phone.open || this.phone.generation != generation {
                        return false;
                    }
                    this.refresh_phone_link_state(cx);
                    true
                });
                if !matches!(keep_going, Ok(true)) {
                    let _ = this.update(cx, |this, _| {
                        this.phone.poll_task = None;
                    });
                    return;
                }
            }
        }));
    }

    fn start_phone_sign_in(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.phone.sign_in_clicks += 1;
        }
        if suppress_background_phone_io(cx) {
            cx.notify();
            return;
        }
        self.state.update(cx, |state, cx| state.sign_in_cloud(cx));
    }

    fn open_access_settings(&mut self, cx: &mut Context<Self>) {
        self.dismiss_for_settings(cx);
        crate::settings::open(Section::Access, cx);
    }

    pub(super) fn phone_connect_state(&self, cx: &App) -> PhoneConnectState {
        let app = self.state.read(cx);
        let mut env_ids: Vec<&str> = Vec::with_capacity(3);
        if let Some(active) = app.active_environment() {
            if let Some(env_id) = active.environment_id.as_deref() {
                env_ids.push(env_id);
            }
            env_ids.push(active.id.as_str());
        }
        if let Some(config) = app.server_config.config.as_ref()
            && !config.environment.environment_id.is_empty()
        {
            env_ids.push(config.environment.environment_id.as_str());
        }
        let account = phone_account_for_identity(
            app.cloud.identity.as_deref(),
            &app.cloud.discovery,
            &env_ids,
        );
        resolve_phone_connect_state(&PhoneConnectInput {
            account,
            link: self.phone.link_state.as_ref(),
            is_pending: self.phone.link_pending,
            has_error: self.phone.link_error.is_some(),
        })
    }

    fn render_phone_connect_link(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let state = self.phone_connect_state(cx);
        let settings_link = |label: &'static str, cx: &mut Context<Self>| {
            div()
                .debug_selector(|| "welcome-phone-settings".to_owned())
                .child(
                    Button::new("welcome-phone-settings", ButtonVariant::Outline, ButtonSize::Xs)
                        .label(crate::i18n::t(cx, label))
                        .on_click(cx.listener(|this, _, _, cx| this.open_access_settings(cx))),
                )
        };

        let content: AnyElement = match state {
            PhoneConnectState::Checking => div()
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child(crate::i18n::t(cx, "Checking T3 Connect\u{2026}"))
                .into_any_element(),
            PhoneConnectState::Error => v_flex()
                .items_start()
                .gap(r(8.))
                .child(
                    div()
                        .text_sm()
                        .text_color(hsla(p.foreground))
                        .child(crate::i18n::t(cx, "Could not check T3 Connect.")),
                )
                .child(settings_link("Check T3 Connect", cx))
                .into_any_element(),
            PhoneConnectState::Reachable => h_flex()
                .items_start()
                .gap(r(6.))
                .child(
                    div()
                        .mt(r(2.))
                        .flex_none()
                        .child(Icon::Check.render(px(16.), hsla(p.success_foreground))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(hsla(p.foreground))
                        .child(crate::i18n::t(
                            cx,
                            "Your phone can reach this computer. Sign in to the app with the same account.",
                        )),
                )
                .into_any_element(),
            PhoneConnectState::SharedUnknownReachability => div()
                .text_sm()
                .text_color(hsla(p.foreground))
                .child(crate::i18n::t(
                    cx,
                    "T3 Connect is on for this computer. Sign in to the app with the same account.",
                ))
                .into_any_element(),
            PhoneConnectState::SharedUnreachable => v_flex()
                .items_start()
                .gap(r(8.))
                .child(
                    div()
                        .text_sm()
                        .text_color(hsla(p.foreground))
                        .child(crate::i18n::t(
                            cx,
                            "T3 Connect is on, but the app cannot reach this computer.",
                        )),
                )
                .child(settings_link("Check T3 Connect", cx))
                .into_any_element(),
            PhoneConnectState::NotShared => v_flex()
                .items_start()
                .gap(r(8.))
                .child(
                    div()
                        .text_sm()
                        .text_color(hsla(p.foreground))
                        .child(crate::i18n::t(
                            cx,
                            "Signed in. Share this computer so the app can reach it.",
                        )),
                )
                .child(settings_link("Set up T3 Connect", cx))
                .into_any_element(),
            PhoneConnectState::SignedOut => {
                let cloud = &self.state.read(cx).cloud;
                let disabled = cloud.signing_in || (!self.phone.cloud_checked && cloud.busy);
                v_flex()
                    .items_start()
                    .gap(r(8.))
                    .child(
                        div()
                            .text_sm()
                            .text_color(hsla(p.foreground))
                            .child(crate::i18n::t(
                                cx,
                                "Sign in to T3 Connect here, then in the app with the same account.",
                            )),
                    )
                    .child(
                        div()
                            .debug_selector(|| "welcome-phone-sign-in".to_owned())
                            .child(
                                Button::new(
                                    "welcome-phone-sign-in",
                                    ButtonVariant::Outline,
                                    ButtonSize::Xs,
                                )
                                .label(crate::i18n::t(cx, "Sign in to T3 Connect"))
                                .disabled(disabled)
                                .on_click(cx.listener(|this, _, _, cx| this.start_phone_sign_in(cx))),
                            ),
                    )
                    .into_any_element()
            }
        };

        div()
            .w_full()
            .debug_selector(|| "welcome-phone-step-2".to_owned())
            .child(content)
            .into_any_element()
    }

    pub(super) fn render_phone(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.show_phone(cx) {
            return None;
        }
        let p = palette(cx);
        let open = self.phone.open;
        let toggle_label = crate::i18n::t(cx, if open { "Hide" } else { "Set up" });
        let header_row = h_flex()
            .w_full()
            .min_h(r(40.))
            .items_center()
            .gap(r(12.))
            .px(r(12.))
            .py(r(8.))
            .child(Icon::Smartphone.render(px(16.), hsla(p.muted_foreground)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .child(crate::i18n::t(cx, "Use T3 Code on your phone")),
            )
            .child(
                div()
                    .debug_selector(|| "welcome-phone-toggle".to_owned())
                    .child(
                        Button::new("welcome-phone-toggle", ButtonVariant::GhostMuted, ButtonSize::Xs)
                            .label(toggle_label)
                            .trailing(if open { Icon::ChevronUp } else { Icon::ChevronDown })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_phone(cx))),
                    ),
            );

        let qr_title = crate::i18n::t(cx, "T3 Code app for iPhone and Android");
        let panel = open.then(|| {
            let step1 = div()
                .text_sm()
                .text_color(hsla(p.foreground))
                .child(crate::i18n::t(cx, "Scan with your phone to get the app."))
                .into_any_element();
            let step2 = self.render_phone_connect_link(cx);
            h_flex()
                .w_full()
                .items_center()
                .gap(r(24.))
                .pl(r(40.))
                .pr(r(16.))
                .pt(r(4.))
                .pb(r(16.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(r(16.))
                        .child(phone_step(1, step1, cx))
                        .child(phone_step(2, step2, cx)),
                )
                .children(mobile_app_qr().map(|matrix| qr_element(matrix, qr_title)))
        });

        Some(
            v_flex()
                .debug_selector(|| "welcome-phone-section".to_owned())
                .gap(r(10.))
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(hsla(p.foreground))
                        .child(crate::i18n::t(cx, "Phone")),
                )
                .child(
                    row_card(cx)
                        .flex_col()
                        .items_stretch()
                        .child(header_row)
                        .children(panel),
                )
                .into_any_element(),
        )
    }

    #[cfg(any(test, feature = "snapshot"))]
    #[allow(dead_code)]
    pub(crate) fn set_phone_fixture_state(
        &mut self,
        fixture: PhoneFixtureState,
        cx: &mut Context<Self>,
    ) {
        self.can_read_relay = fixture.can_read_relay;
        self.can_write_relay = fixture.can_write_relay;
        self.phone.open = fixture.open;
        self.phone.cloud_configured = true;
        self.phone.cloud_checked = true;
        self.phone.link_state = fixture.link_state;
        self.phone.link_pending = fixture.link_pending;
        self.phone.link_error = fixture.link_error;
        cx.notify();
    }

    #[cfg(any(test, feature = "snapshot"))]
    #[allow(dead_code)]
    pub(crate) fn is_phone_open(&self) -> bool {
        self.phone.open
    }

    #[cfg(any(test, feature = "snapshot"))]
    #[allow(dead_code)]
    pub(crate) fn phone_has_background_tasks(&self) -> bool {
        self.phone.fetch_task.is_some() || self.phone.poll_task.is_some()
    }

    #[cfg(test)]
    pub(crate) fn phone_sign_in_clicks(&self) -> usize {
        self.phone.sign_in_clicks
    }
}

#[cfg(any(test, feature = "snapshot"))]
#[allow(dead_code)]
pub(crate) struct PhoneFixtureState {
    pub(crate) open: bool,
    pub(crate) can_read_relay: bool,
    pub(crate) can_write_relay: bool,
    pub(crate) link_state: Option<CloudLinkState>,
    pub(crate) link_pending: bool,
    pub(crate) link_error: Option<String>,
}
