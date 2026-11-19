//! Environments and About. Environments shows the connected environment (label, address, server
//! and protocol versions, status) with Switch environment, Disconnect, and Remove; the saved list
//! lives on the connect screen the switcher opens, alongside the T3 Connect account.
//! About ports `AboutSettingsPanel`, `AboutVersionTitle`, `MobileAppSettingsRow`, and
//! `OpenSourceLicensesPanel` (`SettingsPanels.tsx`, `AboutVersionTitle.tsx`,
//! `MobileAppSettingsRow.tsx`, `OpenSourceLicenses.tsx`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, AppContext, Bounds, ClipboardItem, Context, Entity, Focusable, FontWeight,
    IntoElement, ParentElement, SharedString, Styled, Subscription, Window, canvas, div, fill,
    point, prelude::*, px, size,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use t3_client::auth::ORCHESTRATION_PROTOCOL_VERSION;
use t3_client::store::SavedEnvironment;
use t3_core::managed_policy;
use t3_core::settings::{
    ThirdPartyLicenseManifest, decode_third_party_license_manifest, filter_third_party_license_entries,
    format_license_bundles, third_party_license_entry_key,
};

use super::controls::{
    Button, ButtonSize, ButtonVariant, InlineButton, InlineTone, Row, group, icon, muted_text, r,
    section, section_title,
};
use super::{Block, SettingsView, close};
use crate::app_state::Phase;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const PRIVACY_POLICY_URL: &str = "https://t3.codes/privacy-policy";
const APP_STORE_URL: &str = "https://apps.apple.com/us/app/t3-code-remote-claude-more/id6787819824";
const PLAY_STORE_URL: &str = "https://play.google.com/store/apps/details?id=com.t3tools.t3code";
const LICENSE_MANIFEST_MAX_BYTES: usize = 16 * 1024 * 1024;
const QR_SIZE: f32 = 128.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MobileStore {
    Ios,
    Android,
}

impl MobileStore {
    const ALL: [MobileStore; 2] = [MobileStore::Ios, MobileStore::Android];

    fn id(self) -> &'static str {
        match self {
            MobileStore::Ios => "ios",
            MobileStore::Android => "android",
        }
    }

    fn label(self) -> &'static str {
        match self {
            MobileStore::Ios => "iPhone and iPad",
            MobileStore::Android => "Android",
        }
    }

    fn store(self) -> &'static str {
        match self {
            MobileStore::Ios => "App Store",
            MobileStore::Android => "Google Play",
        }
    }

    fn url(self) -> &'static str {
        match self {
            MobileStore::Ios => APP_STORE_URL,
            MobileStore::Android => PLAY_STORE_URL,
        }
    }
}

struct QrMatrix {
    width: usize,
    dark: Vec<bool>,
}

fn qr_matrix(text: &str) -> Option<QrMatrix> {
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    Some(QrMatrix {
        width: code.width(),
        dark: code.to_colors().into_iter().map(|color| color == qrcode::Color::Dark).collect(),
    })
}

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
                    let origin = point(
                        bounds.origin.x + px(unit * (start + 1) as f32),
                        bounds.origin.y + px(unit * (y + 1) as f32),
                    );
                    window.paint_quad(
                        fill(Bounds::new(origin, size(px(unit * (x - start) as f32), px(unit))), gpui::black()),
                    );
                }
            }
        },
    )
    .size(r(QR_SIZE));
    div().flex_none().rounded(r(6.)).overflow_hidden().child(element).into_any_element()
}

#[derive(Default)]
enum LicensesStatus {
    #[default]
    Idle,
    Loading,
    Loaded(ThirdPartyLicenseManifest),
    Failed(String),
}

pub(crate) struct AboutState {
    copied_version: bool,
    mobile_popover: Option<MobileStore>,
    qr_cache: RefCell<Option<(MobileStore, Rc<QrMatrix>)>>,
    pub(super) licenses_open: bool,
    licenses_status: LicensesStatus,
    pub(super) licenses_search: Entity<InputState>,
    licenses_search_open: bool,
    licenses_expanded: Option<String>,
    licenses_load_id: u64,
    licenses_connection: Option<u64>,
    _subscription: Subscription,
}

impl AboutState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let licenses_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search licenses"));
        let subscription = cx.subscribe_in(&licenses_search, window, |this, input, event: &InputEvent, _, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::Blur if input.read(cx).value().is_empty() => {
                this.about.licenses_search_open = false;
                cx.notify();
            }
            _ => {}
        });
        AboutState {
            copied_version: false,
            mobile_popover: None,
            qr_cache: RefCell::new(None),
            licenses_open: false,
            licenses_status: LicensesStatus::Idle,
            licenses_search,
            licenses_search_open: false,
            licenses_expanded: None,
            licenses_load_id: 0,
            licenses_connection: None,
            _subscription: subscription,
        }
    }

    pub(super) fn leave(&mut self) {
        self.mobile_popover = None;
        self.licenses_open = false;
        self.licenses_search_open = false;
        self.licenses_expanded = None;
        if matches!(self.licenses_status, LicensesStatus::Failed(_)) {
            self.licenses_status = LicensesStatus::Idle;
        }
    }

    fn cached_qr(&self, store: MobileStore) -> Option<Rc<QrMatrix>> {
        let mut cache = self.qr_cache.borrow_mut();
        if let Some((cached, matrix)) = cache.as_ref()
            && *cached == store
        {
            return Some(matrix.clone());
        }
        let matrix = Rc::new(qr_matrix(store.url())?);
        *cache = Some((store, matrix.clone()));
        Some(matrix)
    }
}

fn is_http_url(value: &str) -> bool {
    let trimmed = value.trim();
    let Some(rest) = trimmed.strip_prefix("https://").or_else(|| trimmed.strip_prefix("http://")) else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty() && !trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn telemetry_off_message(policy: Option<&serde_json::Value>) -> Option<String> {
    let policy = policy?;
    (policy["limits"]["telemetry"] == false)
        .then(|| format!("{} turned off anonymous usage data.", managed_policy::organization(policy)))
}

impl SettingsView {
    pub(super) fn dismiss_about_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.about.mobile_popover.take().is_some() {
            cx.notify();
            return true;
        }
        if self.about.licenses_open {
            if self.about.licenses_search_open
                && (self.about.licenses_search.focus_handle(cx).is_focused(window)
                    || !self.about.licenses_search.read(cx).value().is_empty())
            {
                self.about.licenses_search.update(cx, |input, cx| input.set_value("", window, cx));
                self.about.licenses_search_open = false;
                self.focus.focus(window, cx);
                cx.notify();
                return true;
            }
            self.about.leave();
            self.focus.focus(window, cx);
            cx.notify();
            return true;
        }
        false
    }

    fn copy_app_version(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(env!("CARGO_PKG_VERSION").to_owned()));
        self.about.copied_version = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |this, cx| {
                this.about.copied_version = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_licenses(&mut self, cx: &mut Context<Self>) {
        self.about.mobile_popover = None;
        self.about.licenses_open = true;
        self.about.licenses_expanded = None;
        let connection = self.state.read(cx).connection_id();
        if !matches!(self.about.licenses_status, LicensesStatus::Loaded(_))
            || self.about.licenses_connection != connection
        {
            self.load_licenses(cx);
        }
        cx.notify();
    }

    fn load_licenses(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let connection = state.connection_id();
        self.about.licenses_connection = connection;
        self.about.licenses_load_id += 1;
        let load_id = self.about.licenses_load_id;
        let Some(http_base) = state.active_environment().map(|env| env.http_base.clone()) else {
            self.about.licenses_status = LicensesStatus::Failed("Connect an environment to load open-source notices.".to_owned());
            cx.notify();
            return;
        };
        self.about.licenses_status = LicensesStatus::Loading;
        let runtime = state.runtime().clone();
        let task = runtime.spawn(async move {
            let bytes = t3_client::asset::fetch_asset(&http_base, "/third-party-licenses.json", LICENSE_MANIFEST_MAX_BYTES)
                .await
                .map_err(|_| "The license manifest could not load.".to_owned())?;
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| "The license manifest could not load.".to_owned())?;
            decode_third_party_license_manifest(&value)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("The license manifest could not load.".to_owned()));
            let _ = this.update(cx, |this, cx| {
                if this.about.licenses_load_id != load_id || this.about.licenses_connection != connection {
                    return;
                }
                this.about.licenses_status = match result {
                    Ok(manifest) => LicensesStatus::Loaded(manifest),
                    Err(message) => LicensesStatus::Failed(message),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn environments(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let state = self.state.read(cx);
        let environment = state.server_config.config.as_ref().map(|config| config.environment.clone()).unwrap_or_default();
        let saved = state.active_environment().cloned();
        let address = saved.as_ref().map(SavedEnvironment::address).unwrap_or_default();
        let status = match &state.phase {
            Phase::Online(status) => status.summary().0,
            Phase::Unpaired => "Not connected".to_owned(),
        };
        let label = [environment.label.as_str(), saved.as_ref().map_or("", |saved| saved.label.as_str())]
            .into_iter()
            .find(|label| !label.trim().is_empty())
            .unwrap_or("Environment")
            .to_owned();
        let protocol = environment.orchestration_protocol_version.unwrap_or(ORCHESTRATION_PROTOCOL_VERSION);
        let value = |text: String| muted_text(text, cx);
        let (switch_state, disconnect_state, remove_state) = (self.state.clone(), self.state.clone(), self.state.clone());
        let cloud_state = self.state.clone();
        let remove_id = saved.as_ref().map(|saved| saved.id.clone());
        let connection = section(
            "Environments",
            vec![
                Row::new("remote-environments", label)
                    .description(if address.is_empty() { "This device's paired environment.".to_owned() } else { address })
                    .highlighted(self.is_target("remote-environments"))
                    .below(div().children(state.server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref()).map(|policy| crate::components::managed::attribution(policy, cx))))
                    .control(value(status))
                    .into_any_element(),
                Row::new("server-version", "Server version")
                    .control(value(if environment.server_version.is_empty() { "Unknown".to_owned() } else { environment.server_version.clone() }))
                    .into_any_element(),
                Row::new("orchestration-protocol", "Orchestration protocol").control(value(protocol.to_string())).into_any_element(),
                Row::new("cloud-account", "T3 Connect")
                    .description("Sign in to view your T3 Connect environments.")
                    .control(Button::new("cloud-account-button", ButtonVariant::Outline, ButtonSize::Sm).label("Manage…").on_click(move |_, window, cx| {
                        close(window, cx);
                        cloud_state.update(cx, |state, cx| state.open_switcher(cx));
                    }))
                    .into_any_element(),
                Row::new("switch-environment", "Switch environment")
                    .description("Connect to another saved environment, or pair a new one.")
                    .control(Button::new("switch-environment-button", ButtonVariant::Outline, ButtonSize::Sm).label("Switch\u{2026}").on_click(move |_, window, cx| {
                        close(window, cx);
                        switch_state.update(cx, |state, cx| state.open_switcher(cx));
                    }))
                    .into_any_element(),
                Row::new("disconnect", "Disconnect")
                    .description("Stop using this environment for now. It stays saved on this device.")
                    .control(Button::new("disconnect-server", ButtonVariant::Outline, ButtonSize::Sm).label("Disconnect").on_click(move |_, window, cx| {
                        close(window, cx);
                        disconnect_state.update(cx, |state, cx| state.disconnect(cx));
                    }))
                    .into_any_element(),
                Row::new("remove-environment", "Remove from this device")
                    .description("Forget this environment and its credential. Pair again with a new pairing link.")
                    .control(Button::new("remove-environment-button", ButtonVariant::DestructiveOutline, ButtonSize::Sm).label("Remove").disabled(remove_id.is_none()).on_click(
                        move |_, window, cx| {
                            let Some(id) = remove_id.clone() else { return };
                            close(window, cx);
                            remove_state.update(cx, |state, cx| state.remove_environment(&id, cx));
                        },
                    ))
                    .into_any_element(),
            ],
            cx,
        );
        vec![Block::new(&["remote-environments", "connections"], connection)]
    }

    pub(super) fn about(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        if self.about.licenses_open {
            return self.open_source_licenses_page(cx);
        }
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let policy = self
            .state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .and_then(|config| config.managed_policy.clone());

        let mut app_rows = Vec::new();
        if let Some(policy) = policy.as_ref() {
            let org = managed_policy::organization(policy).to_owned();
            let support_url = policy
                .get("supportUrl")
                .and_then(|v| v.as_str())
                .filter(|url| is_http_url(url))
                .map(str::to_owned);
            let mut row = Row::new("about-organization", "Organization")
                .description(format!("This environment is managed by {org}. Some settings and features follow its policy."));
            if let Some(url) = support_url {
                row = row.control(
                    div().debug_selector(|| "about-support-link".to_owned()).child(
                        Button::new("about-org-help", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Get help")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
                );
            }
            app_rows.push(row.into_any_element());
        }

        let version = env!("CARGO_PKG_VERSION");
        let copied = self.about.copied_version;
        app_rows.push(
            Row::new("app-version", "Version")
                .description("Current version of the application.")
                .highlighted(self.is_target("app-version"))
                .control(
                    h_flex()
                        .gap(r(8.))
                        .child(
                            div()
                                .debug_selector(|| "about-version-copy".to_owned())
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(
                                    InlineButton::new("copy-app-version", version, InlineTone::Muted)
                                        .tooltip(if copied { "Copied" } else { "Copy version" })
                                        .on_click(cx.listener(|this, _, _, cx| this.copy_app_version(cx))),
                                ),
                        ),
                )
                .into_any_element(),
        );

        let open_store = self.about.mobile_popover;
        let store_buttons = h_flex().gap(r(8.)).children(MobileStore::ALL.into_iter().map(|store| {
            let is_open = open_store == Some(store);
            let popover = is_open.then(|| {
                let qr = self.about.cached_qr(store);
                let url = store.url();
                let store_name = store.store();
                gpui::deferred(
                    gpui::anchored()
                        .anchor(gpui::Anchor::TopRight)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            v_flex()
                                .occlude()
                                .mt(r(4.))
                                .p(r(16.))
                                .gap(r(12.))
                                .items_center()
                                .rounded(r(12.))
                                .border_1()
                                .border_color(hsla(p.dropdown_border))
                                .bg(hsla(p.popover))
                                .shadow(crate::composer::controls::floating_shadow(dark))
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                    this.about.mobile_popover = None;
                                    cx.notify();
                                }))
                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child(store_name))
                                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Scan with your phone camera."))
                                .children(qr.map(|matrix| div().rounded(r(8.)).bg(gpui::white()).p(r(6.)).child(qr_element(matrix))))
                                .child(
                                    Button::new(SharedString::from(format!("open-store-{}", store.id())), ButtonVariant::Outline, ButtonSize::Sm)
                                        .label(format!("Open {store_name}"))
                                        .on_click(move |_, _, cx| cx.open_url(url)),
                                ),
                        ),
                )
                .with_priority(2)
            });
            div()
                .relative()
                .child(
                    Button::new(SharedString::from(format!("mobile-app-{}", store.id())), ButtonVariant::Outline, ButtonSize::Sm)
                        .icon(Icon::Smartphone)
                        .label(store.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.about.mobile_popover = if this.about.mobile_popover == Some(store) { None } else { Some(store) };
                            cx.notify();
                        })),
                )
                .children(popover)
        }));
        app_rows.push(
            Row::new("mobile-app", "Mobile app")
                .description("Connect to your server from your phone or tablet.")
                .highlighted(self.is_target("mobile-app"))
                .control(store_buttons)
                .into_any_element(),
        );
        let app = section("App", app_rows, cx);

        let privacy_description = match telemetry_off_message(policy.as_ref()) {
            Some(off) => format!("How we handle your data. {off}"),
            None => "How we handle your data, including the anonymous usage data T3 Code collects.".to_owned(),
        };
        let legal = section(
            "Legal",
            vec![
                Row::new("privacy-policy", "Privacy policy")
                    .description(privacy_description)
                    .highlighted(self.is_target("privacy-policy"))
                    .control(
                        Button::new("view-privacy-policy", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("View policy")
                            .on_click(|_, _, cx| cx.open_url(PRIVACY_POLICY_URL)),
                    )
                    .into_any_element(),
                Row::new("open-source-licenses", "Open-source licenses")
                    .description("Notices for dependencies, assets, and optional tools used by T3 Code.")
                    .highlighted(self.is_target("open-source-licenses"))
                    .control(
                        Button::new("view-open-source-licenses", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("View licenses")
                            .on_click(cx.listener(|this, _, _, cx| this.open_licenses(cx))),
                    )
                    .into_any_element(),
            ],
            cx,
        );

        vec![
            Block::new(&["about", "app-version", "mobile-app"], app),
            Block::new(&["legal", "privacy-policy", "open-source-licenses"], legal),
        ]
    }

    fn open_source_licenses_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let query = self.about.licenses_search.read(cx).value().to_string();

        let back = h_flex().px(r(16.)).child(
            Button::new("licenses-back", ButtonVariant::GhostMuted, ButtonSize::Sm)
                .icon(Icon::ChevronLeft)
                .label("Back to About")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.about.leave();
                    cx.notify();
                })),
        );

        let (header_action, rows): (Option<AnyElement>, Vec<AnyElement>) = match &self.about.licenses_status {
            LicensesStatus::Loaded(manifest) => {
                let total = manifest.entries.len();
                let filtered = filter_third_party_license_entries(&manifest.entries, &query);
                let filtered_count = filtered.len();
                let count_label = if filtered_count == total {
                    format!("{total} notices")
                } else {
                    format!("{filtered_count} of {total}")
                };
                let action = if !self.about.licenses_search_open {
                    h_flex()
                        .gap(r(6.))
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(count_label))
                        .child(
                            Button::new("licenses-open-search", ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                                .icon(Icon::Search)
                                .tooltip("Search licenses")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.about.licenses_search_open = true;
                                    this.about.licenses_search.update(cx, |input, cx| input.focus(window, cx));
                                    cx.notify();
                                })),
                        )
                        .into_any_element()
                } else {
                    h_flex()
                        .gap(r(8.))
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(count_label))
                        .child(div().w(r(176.)).child(Input::new(&self.about.licenses_search).small()))
                        .into_any_element()
                };
                let content = if filtered.is_empty() {
                    vec![
                        div()
                            .px(r(16.))
                            .py(r(32.))
                            .text_center()
                            .text_sm()
                            .text_color(hsla(p.muted_foreground))
                            .child("No licenses match that search.")
                            .into_any_element(),
                    ]
                } else {
                    filtered
                        .into_iter()
                        .enumerate()
                        .map(|(index, entry)| {
                            let key = third_party_license_entry_key(entry);
                            let is_open = self.about.licenses_expanded.as_deref() == Some(key.as_str());
                            let toggle_key = key.clone();
                            let meta = format!("{} \u{00b7} {}", entry.license, format_license_bundles(&entry.bundles));
                            let source_url = entry.source_url.clone();
                            let notice_text = entry.notice_text.clone();
                            v_flex()
                                .child(
                                    h_flex()
                                        .min_h(r(36.))
                                        .px(r(12.))
                                        .gap(r(8.))
                                        .hover(|this| this.bg(hsla(p.muted).opacity(0.35)))
                                        .child(
                                            h_flex()
                                                .id(("license-entry", index))
                                                .flex_1()
                                                .min_w_0()
                                                .gap(r(10.))
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if this.about.licenses_expanded.as_deref() == Some(toggle_key.as_str()) {
                                                        this.about.licenses_expanded = None;
                                                    } else {
                                                        this.about.licenses_expanded = Some(toggle_key.clone());
                                                    }
                                                    cx.notify();
                                                }))
                                                .child(icon(
                                                    if is_open { Icon::ChevronDown } else { Icon::ChevronRight },
                                                    14.,
                                                    hsla(p.muted_foreground),
                                                ))
                                                .child(
                                                    h_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap(r(8.))
                                                        .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(entry.name.clone()))
                                                        .children(entry.version.clone().map(|version| {
                                                            div().flex_none().font_family(mono.clone()).text_xs().text_color(hsla(p.muted_foreground)).child(version)
                                                        })),
                                                )
                                                .child(div().max_w(r(260.)).truncate().text_xs().text_color(hsla(p.muted_foreground)).child(meta)),
                                        )
                                        .children(source_url.map(|url| {
                                            Button::new(("license-source", index), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                                                .icon(Icon::ExternalLink)
                                                .tooltip("Project source")
                                                .on_click(move |_, _, cx| cx.open_url(&url))
                                        })),
                                )
                                .when(is_open, |this| {
                                    this.child(
                                        div()
                                            .pl(r(40.))
                                            .pr(r(16.))
                                            .pt(r(4.))
                                            .pb(r(16.))
                                            .font_family(mono.clone())
                                            .text_xs()
                                            .line_height(r(20.))
                                            .text_color(hsla(p.foreground).opacity(0.8))
                                            .child(notice_text),
                                    )
                                })
                                .into_any_element()
                        })
                        .collect()
                };
                (Some(action), content)
            }
            LicensesStatus::Failed(message) => (
                None,
                vec![
                    v_flex()
                        .gap(r(12.))
                        .px(r(16.))
                        .py(r(20.))
                        .items_start()
                        .child(
                            v_flex()
                                .gap(r(4.))
                                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Open-source notices are unavailable"))
                                .child(div().text_xs().text_color(hsla(p.muted_foreground).opacity(0.8)).child(message.clone())),
                        )
                        .child(
                            Button::new("licenses-retry", ButtonVariant::Outline, ButtonSize::Xs)
                                .label("Try again")
                                .on_click(cx.listener(|this, _, _, cx| this.load_licenses(cx))),
                        )
                        .into_any_element(),
                ],
            ),
            LicensesStatus::Idle | LicensesStatus::Loading => (
                None,
                vec![
                    div()
                        .px(r(16.))
                        .py(r(20.))
                        .text_sm()
                        .text_color(hsla(p.muted_foreground))
                        .child("Loading open-source notices\u{2026}")
                        .into_any_element(),
                ],
            ),
        };

        let notices = v_flex()
            .gap(r(10.))
            .child(
                h_flex()
                    .justify_between()
                    .gap(r(16.))
                    .child(section_title("Third-party notices"))
                    .children(header_action.map(|action| div().pr(r(16.)).child(action))),
            )
            .child(group(rows, cx));

        vec![Block::new(&[], back), Block::new(&["open-source-licenses"], notices)]
    }
}

#[cfg(test)]
mod unit_tests {
    use super::is_http_url;

    #[test]
    fn http_url_filter_accepts_http_and_rejects_unsafe_schemes() {
        assert!(is_http_url("https://help.acme.example/support"));
        assert!(is_http_url("http://localhost:3000/help"));
        assert!(!is_http_url("javascript:alert(1)"));
        assert!(!is_http_url("file:///etc/passwd"));
        assert!(!is_http_url("https://"));
        assert!(!is_http_url("https://help.acme.example/with space"));
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use gpui::{Render, Window, div, prelude::*};
    use serde_json::json;
    use t3_core::settings::search::Section;

    use super::*;

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().children(crate::settings::overlay(window, cx))
        }
    }

    #[gpui::test]
    fn about_renders_managed_org_and_escape_dismisses_qr_before_licenses(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": { "environmentId": "fixture-env", "label": "Mac" },
                        "managedPolicy": {
                            "organization": "Acme Corp",
                            "supportUrl": "https://support.acme.example",
                            "limits": { "telemetry": false }
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|_, cx| crate::settings::open(Section::About, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let view = vcx.read(|cx| cx.global::<crate::settings::Host>().view.clone().expect("settings view open"));
        assert!(vcx.debug_bounds("about-version-copy").is_some());
        assert!(vcx.debug_bounds("about-support-link").is_some());

        // Escape dismisses mobile QR popover first, then licenses subview, without closing Settings.
        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.about.mobile_popover = Some(MobileStore::Ios);
                view.about.licenses_open = true;
                assert!(view.dismiss_about_overlay(window, cx));
                assert!(view.about.mobile_popover.is_none());
                assert!(view.about.licenses_open);
                assert!(view.dismiss_about_overlay(window, cx));
                assert!(!view.about.licenses_open);
                assert!(!view.dismiss_about_overlay(window, cx));
            });
        });

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        drop(runtime);
    }
}
