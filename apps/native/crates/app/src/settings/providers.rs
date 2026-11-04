//! Providers: `web/src/components/settings/ProviderSettingsPanel.tsx` for the connected
//! environment. A list of provider instances beside the selected instance's editor
//! (`ProviderInstanceCard.tsx`), the header's refresh with its "Checked …" label, the Add provider
//! dialog (`AddProviderInstanceDialog.tsx`), and the Advanced rows (health check interval, compact
//! idle threads). Sign-in and runtime installs stream `provider.auth.subscribe` and
//! `provider.install.subscribe` for the selected instance only, so a closed page subscribes to
//! nothing. Logic lives in `t3_core::settings::providers`.

mod dialog;
mod registry;
mod dialog_config;
mod editor;
mod form;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext, Context, Entity, Focusable, FontWeight, IntoElement, KeyBinding, ListAlignment, ListState, ParentElement, SharedString,
    Styled, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::Value;
use t3_client::session::RpcError;
use t3_core::server_config::ServerProvider;
use t3_core::settings::ServerSettingChange;
use t3_core::settings::providers::auth::{self, AuthState, InstallState};
use t3_core::settings::providers::background::{IDLE_COMPACTION_MODES, ProviderPanelSettings};
use t3_core::settings::providers::{self as model, DisplayModel, InstanceRow, InstanceView, ModelGroup, StatusKey};
use t3_core::settings::{decode_update_reply, search};
use tokio::sync::mpsc;

use super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, icon, muted_text, r, section, section_title};
use super::{Block, SettingsView, store};
use crate::app_state::AppEvent;
use crate::icons::Icon;
use crate::provider_icon::{ProviderIcon, ProviderMark};
use crate::theme::{hsla, palette};

pub(super) fn fixture_active(cx: &App) -> bool {
    #[cfg(feature = "snapshot")]
    if std::env::var_os("T3_NATIVE_PROVIDER_WIZARD_FIXTURE").is_some() { return true; }
    crate::chatgpt_setup::fixture_active(cx)
}

gpui::actions!(provider_settings, [CloseProviderDialog]);

/// Escape closes the Add provider dialog before it reaches the settings modal.
pub(super) fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", CloseProviderDialog, Some("ProviderDialog"))]);
}

type CredentialInputs = Vec<(String, Entity<InputState>)>;

/// Ends the network half of a subscription when its owner drops it.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A live streaming RPC: the network loop that resubscribes after a reconnect, and the pump
/// that hands its items to the view.
pub(super) struct Stream {
    _driver: AbortOnDrop,
    _pump: Task<()>,
}

/// `provider.auth.subscribe` for the selected instance, and the Account row's own state.
struct AuthSession {
    instance_id: String,
    state: Option<AuthState>,
    load_error: Option<String>,
    pending: bool,
    error: Option<String>,
    _stream: Option<Stream>,
}

/// `provider.install.subscribe` for the selected Antigravity instance.
struct InstallSession {
    instance_id: String,
    state: Option<InstallState>,
    load_error: Option<String>,
    pending: bool,
    error: Option<String>,
    _stream: Option<Stream>,
}

pub(crate) struct ProvidersState {
    selected: Option<String>,
    form: form::FormState,
    chatgpt: Option<(Option<u64>, Entity<crate::chatgpt_setup::ChatGptSetup>)>,
    refreshing: bool,
    /// Instances whose `server.updateProvider` call this client has in flight.
    dispatching: HashSet<String>,
    update_error: Option<(String, String)>,
    /// A failure with no row of its own to show it (the web's toasts), as a title and detail.
    notice: Option<(&'static str, String)>,
    advisory_open: Option<String>,
    advisory_closed_at: Option<Instant>,
    auth: Option<AuthSession>,
    install: Option<InstallSession>,
    pub(crate) display_name: Entity<InputState>,
    pub(crate) accent: Entity<InputState>,
    pub(crate) custom_model: Entity<InputState>,
    pub(crate) model_filter: Entity<InputState>,
    pub(crate) health_interval: Entity<InputState>,
    pub(crate) callback_url: Entity<InputState>,
    pub(crate) terminal_line: Entity<InputState>,
    /// The credentials form, keyed by the flow and interaction it answers.
    credentials: Option<(String, CredentialInputs)>,
    adding_custom_model: bool,
    custom_model_error: Option<String>,
    /// The models list renders only the rows in view, from `model_rows`.
    models_list: ListState,
    model_rows: editor::ModelRows,
    /// The account email shows only after a click (`RedactedSensitiveText`).
    email_revealed: bool,
    method_id: SharedString,
    /// The instance the name and accent fields were last filled from.
    synced_instance: Option<String>,
    dialog: Option<dialog::AddDialog>,
    /// Redraws once when the "Checked …" label would next read differently.
    label_timer: Option<(i64, Task<()>)>,
    _subscriptions: Vec<Subscription>,
}

impl ProvidersState {
    pub(super) fn leave(&mut self) {
        self.dialog=None;
        self.chatgpt=None;
        self.auth=None;
        self.install=None;
    }

    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let display_name = cx.new(|cx| InputState::new(window, cx));
        let accent = cx.new(|cx| InputState::new(window, cx).placeholder("#2563eb"));
        let custom_model = cx.new(|cx| InputState::new(window, cx));
        let model_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter models"));
        let health_interval = cx.new(|cx| InputState::new(window, cx));
        let callback_url = cx.new(|cx| InputState::new(window, cx));
        let terminal_line = cx.new(|cx| InputState::new(window, cx).placeholder("Type a reply, then press Enter"));
        let subscriptions = vec![
            cx.subscribe_in(&display_name, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_display_name(cx);
                }
            }),
            cx.subscribe_in(&accent, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_accent(window, cx);
                }
            }),
            cx.subscribe_in(&custom_model, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.add_custom_model(window, cx),
                InputEvent::Change if this.providers.custom_model_error.is_some() => {
                    this.providers.custom_model_error = None;
                    cx.notify();
                }
                InputEvent::Blur if input.read(cx).value().trim().is_empty() => {
                    this.providers.adding_custom_model = false;
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe_in(&model_filter, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&health_interval, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_health_interval(window, cx);
                }
            }),
            cx.subscribe_in(&callback_url, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_callback(cx);
                }
            }),
            cx.subscribe_in(&terminal_line, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_terminal_line(window, cx);
                }
            }),
        ];
        ProvidersState {
            selected: None,
            form: form::FormState::default(),
            chatgpt: None,
            refreshing: false,
            dispatching: HashSet::new(),
            update_error: None,
            notice: None,
            advisory_open: None,
            advisory_closed_at: None,
            auth: None,
            install: None,
            display_name,
            accent,
            custom_model,
            model_filter,
            health_interval,
            callback_url,
            terminal_line,
            credentials: None,
            adding_custom_model: false,
            custom_model_error: None,
            models_list: ListState::new(0, ListAlignment::Top, px(128.)),
            model_rows: editor::ModelRows::default(),
            email_revealed: false,
            method_id: SharedString::default(),
            synced_instance: None,
            dialog: None,
            label_timer: None,
            _subscriptions: subscriptions,
        }
    }

    /// The text fields a `/` keystroke must not steal focus from.
    pub(super) fn inputs(&self) -> Vec<&Entity<InputState>> {
        let mut inputs = vec![&self.display_name, &self.accent, &self.custom_model, &self.model_filter, &self.health_interval, &self.callback_url, &self.terminal_line];
        inputs.extend(self.form.inputs());
        inputs.extend(self.credentials.iter().flat_map(|(_, fields)| fields.iter().map(|(_, input)| input)));
        if let Some(dialog) = &self.dialog {
            inputs.extend(dialog.inputs());
        }
        inputs
    }
}

/// The selected instance, everything its editor shows.
pub(super) struct Selected {
    row: InstanceRow,
    view: InstanceView,
    live: Option<ServerProvider>,
    accent: Option<u32>,
    hidden: Vec<String>,
    models: Vec<(DisplayModel, ModelGroup)>,
}

/// What the page renders, read from the config once per render.
struct PageData {
    environment_label: String,
    rows: Vec<(InstanceRow, InstanceView, Option<u32>)>,
    selected: Option<Selected>,
    checked_label: Option<String>,
    next_label_change: Option<i64>,
    panel: ProviderPanelSettings,
}

pub(super) fn failure_text(error: &RpcError, fallback: &str) -> String {
    match error {
        RpcError::Failure(failure) => Some(auth::failure_message(failure)).filter(|text| !text.trim().is_empty()).unwrap_or_else(|| fallback.to_owned()),
        RpcError::Closed => "The connection closed. Try again once it reconnects.".to_owned(),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// The settings page's content width (`ModalLayout`): the instance list sits beside the editor
/// once it reaches 48rem, as the web's `@min-[48rem]/providers` container query does.
fn side_by_side(window: &Window) -> bool {
    let rem = f32::from(window.rem_size()) / 16.;
    let viewport = f32::from(window.viewport_size().width);
    let inset = if viewport < 640. { 0. } else if viewport >= 1024. { 40. } else { 24. };
    let modal = (viewport - 2. * inset * rem).min(1280. * rem) - 2.;
    let nav = if viewport < 768. { 56. } else { 240. } * rem + 1.;
    (modal - nav).min(896. * rem) - 48. * rem >= 768. * rem
}

/// `min(44rem, 100dvh - 11rem)`, at least 32rem.
fn card_height(window: &Window) -> gpui::Pixels {
    let rem = f32::from(window.rem_size()) / 16.;
    let viewport = f32::from(window.viewport_size().height);
    px((viewport - 176. * rem).min(704. * rem).max(512. * rem))
}

impl SettingsView {
    fn page_data(&self, cx: &App) -> Option<PageData> {
        let app = self.state.read(cx);
        let config = app.server_config.config.as_ref()?;
        let settings = &config.settings;
        let live = &config.providers;
        let rows = model::instance_rows(settings, live, self.providers.selected.as_deref());
        let views: Vec<(InstanceRow, InstanceView, Option<u32>)> = rows
            .into_iter()
            .map(|row| {
                let provider = live.iter().find(|provider| provider.instance_id == row.instance_id);
                let view = model::instance_view(&row, provider, self.providers.dispatching.contains(&row.instance_id), false);
                let accent = row.explicit.as_ref().and_then(|instance| instance.accent_color.as_deref()).and_then(model::accent_rgba);
                (row, view, accent)
            })
            .collect();
        let selected = self
            .providers
            .selected
            .as_deref()
            .and_then(|id| views.iter().find(|(row, ..)| row.instance_id == id))
            .or_else(|| views.first())
            .map(|(row, view, accent)| {
                let provider = live.iter().find(|provider| provider.instance_id == row.instance_id).cloned();
                let instance = row.instance();
                let custom_models = model::custom_models(&instance);
                let favorites = model::instance_favorites(settings, &row.instance_id);
                let (hidden, order) = model::instance_preferences(settings, &row.instance_id);
                let models = model::group_models(model::models_for_display(provider.as_ref().map(|provider| provider.models.as_slice()).unwrap_or_default(), &custom_models), &favorites, &hidden, &order);
                Selected {
                    row: row.clone(),
                    view: view.clone(),
                    live: provider,
                    accent: *accent,
                    hidden,
                    models,
                }
            });
        let now = now_ms();
        Some(PageData {
            environment_label: Some(config.environment.label.trim()).filter(|label| !label.is_empty()).unwrap_or("this environment").to_owned(),
            rows: views,
            selected,
            checked_label: model::last_checked_label(live, now),
            next_label_change: model::ms_until_label_changes(live, now),
            panel: settings.provider_panel.clone(),
        })
    }

    pub(super) fn providers_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        if self.providers.dialog.as_ref().is_some_and(|d|d.connection!=self.state.read(cx).connection_id()) { self.providers.dialog=None; self.providers.auth=None; }
        let Some(data) = self.page_data(cx) else {
            self.providers.auth = None;
            self.providers.install = None;
            return vec![Block::new(&["providers"], muted_text("Reconnect to change this environment's providers.", cx))];
        };
        self.sync_streams(data.selected.as_ref(), cx);
        self.sync_editor_inputs(data.selected.as_ref(), window, cx);
        if let Some(selected)=&data.selected {self.sync_provider_form(selected,window,cx);}
        self.schedule_label_refresh(data.next_label_change, cx);
        let interval = data.panel.health_interval_seconds().to_string();
        if !self.providers.health_interval.focus_handle(cx).is_focused(window) && self.providers.health_interval.read(cx).value().as_ref() != interval {
            self.providers.health_interval.update(cx, |input, cx| input.set_value(interval, window, cx));
        }

        let providers = self.render_providers_section(&data, window, cx);
        let advanced = self.render_advanced(&data.panel, cx);
        let dialog = self.render_add_dialog(window, cx);
        let notice = self.render_notice(cx);
        vec![
            Block::new(&["providers"], v_flex().gap(r(16.)).children(notice).child(providers).children(dialog)),
            Block::new(&["provider-health-check-interval", "compact-idle-threads"], advanced),
        ]
    }

    fn render_providers_section(&mut self, data: &PageData, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let refreshing = self.providers.refreshing;
        let label = if refreshing { Some("Refreshing providers".to_owned()) } else { data.checked_label.clone() };
        let header = h_flex()
            .min_h(r(28.))
            .px(r(16.))
            .gap(r(8.))
            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child("Providers"))
            .child(
                Button::new("add-provider-title", ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                    .icon(Icon::Plus)
                    .tooltip("Add provider")
                    .on_click(cx.listener(|this, _, window, cx| this.open_add_dialog(window, cx))),
            )
            .child(div().flex_1())
            .child(
                h_flex()
                    .id("refresh-providers")
                    .h(r(24.))
                    .px(r(7.))
                    .gap(r(4.))
                    .rounded(r(6.))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.muted_foreground))
                    .when(refreshing, |this| this.opacity(0.64))
                    .when(!refreshing, |this| {
                        this.cursor_pointer()
                            .hover(|this| this.bg(hsla(p.accent)).text_color(hsla(p.foreground)))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_providers(cx)))
                    })
                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Refresh provider status").build(window, cx))
                    .child(icon(if refreshing { Icon::LoaderCircle } else { Icon::RefreshCw }, 14., hsla(p.muted_foreground)))
                    .children(label.map(|label| div().truncate().child(label))),
            );

        let list = v_flex()
            .children(data.rows.iter().enumerate().map(|(index, (row, view, accent))| {
                let selected = data.selected.as_ref().is_some_and(|selected| selected.row.instance_id == row.instance_id);
                self.render_list_row(index, row, view, *accent, selected, cx)
            }))
            .child(
                h_flex()
                    .id("add-provider-row")
                    .w_full()
                    .gap(r(12.))
                    .px(r(16.))
                    .py(r(12.))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.5))
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .cursor_pointer()
                    .hover(|this| this.bg(hsla(p.muted).opacity(0.25)).text_color(hsla(p.foreground)))
                    .child(icon(Icon::Plus, 16., hsla(p.muted_foreground)))
                    .child("Add provider")
                    .on_click(cx.listener(|this, _, window, cx| this.open_add_dialog(window, cx))),
            );

        let editor = match &data.selected {
            Some(selected) => self.render_editor(selected, &data.environment_label, window, cx),
            None => div().p(r(24.)).text_sm().text_color(hsla(p.muted_foreground)).child("No providers configured.").into_any_element(),
        };

        let wide = side_by_side(window);
        let card = div()
            .rounded(r(12.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.6))
            .bg(hsla(p.card).opacity(0.4))
            .overflow_hidden()
            .map(|this| {
                if wide {
                    this.h(card_height(window))
                        .flex()
                        .child(
                            div()
                                .id("provider-instance-list")
                                .w(r(272.))
                                .flex_none()
                                .h_full()
                                .overflow_y_scroll()
                                .border_r_1()
                                .border_color(hsla(p.border).opacity(0.6))
                                .bg(hsla(p.muted).opacity(0.1))
                                .child(list),
                        )
                        .child(div().id("provider-instance-editor").flex_1().min_w_0().h_full().overflow_y_scroll().child(editor))
                } else {
                    this.flex()
                        .flex_col()
                        .child(div().border_b_1().border_color(hsla(p.border).opacity(0.6)).bg(hsla(p.muted).opacity(0.1)).child(list))
                        .child(editor)
                }
            });
        v_flex().gap(r(10.)).child(header).child(card).into_any_element()
    }

    fn render_list_row(&self, index: usize, row: &InstanceRow, view: &InstanceView, accent: Option<u32>, selected: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let instance_id = row.instance_id.clone();
        let toggle_id = row.instance_id.clone();
        let enabled = view.enabled;
        let attention = view.needs_attention && view.update_progress.is_none();
        let status_line = match &view.update_progress {
            Some(progress) => format!("Updating \u{00b7} {progress}"),
            None => match view.inline_detail.as_deref().filter(|_| view.needs_attention) {
                Some(detail) => format!("{} \u{00b7} {detail}", view.headline),
                None => view.headline.clone(),
            },
        };
        let denial = if enabled { None } else { t3_core::managed_policy::driver_denial(self.state.read(cx).server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref()), &row.driver, model::find_driver(&row.driver).map_or(row.driver.as_str(), |d| d.label)) };
        let toggle = cx.listener(move |this, checked: &bool, _, cx| this.update_instance(&toggle_id, model::InstanceEdit::Enabled(*checked), cx));
        h_flex()
            .id(("provider-row", index))
            .min_h(r(72.))
            .px(r(16.))
            .py(r(12.))
            .gap(r(12.))
            .when(index > 0, |this| this.border_t_1().border_color(hsla(p.border).opacity(0.5)))
            .map(|this| if selected { this.bg(hsla(p.foreground).opacity(0.06)) } else { this.hover(|this| this.bg(hsla(p.muted).opacity(0.25))) })
            .child(
                h_flex()
                    .id(("provider-row-select", index))
                    .flex_1()
                    .min_w_0()
                    .items_start()
                    .gap(r(12.))
                    .cursor_pointer()
                    .when(!enabled && !selected, |this| this.opacity(0.6))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.providers.selected = Some(instance_id.clone());
                        cx.notify();
                    }))
                    .child(ProviderIcon::new(ProviderMark::configured(&row.driver, &view.display_name, accent)).size(px(20.)).badge(
                        crate::provider_icon::BadgeSize::Compact,
                        hsla(p.card),
                    ))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap(r(8.))
                                    .min_w_0()
                                    .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(view.display_name.clone()))
                                    .children(view.version.clone().map(|version| {
                                        div()
                                            .flex_none()
                                            .max_w(r(96.))
                                            .truncate()
                                            .text_xs()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_color(hsla(p.muted_foreground))
                                            .child(version)
                                    }))
                                    .children(view.advisory.as_ref().map(|_| self.render_advisory_trigger(&row.instance_id, view, true, cx))),
                            )
                            .child(
                                h_flex()
                                    .mt(r(2.))
                                    .items_start()
                                    .gap(r(6.))
                                    .text_xs()
                                    .line_height(r(18.))
                                    .text_color(hsla(p.muted_foreground).opacity(0.8))
                                    .when(attention, |this| this.child(div().h(r(18.)).flex().items_center().child(status_dot(view.status, cx))))
                                    .child(div().min_w_0().line_clamp(2).child(status_line)),
                            ),
                    ),
            )
            .child(h_flex().h(r(20.)).flex_none().child(crate::components::managed::ManagedControl::new(("managed-provider", index), denial, Switch::new(("provider-enabled", index), enabled).on_change(move |checked, window, cx| toggle(&checked, window, cx)))))
            .into_any_element()
    }

    /// The version advisory's icon and its popover: the advice, the one-click action, progress,
    /// and the manual command.
    fn render_advisory_trigger(&self, instance_id: &str, view: &InstanceView, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(advisory) = view.advisory.clone() else { return div().into_any_element() };
        let open = self.providers.advisory_open.as_deref() == Some(instance_id);
        let size = if compact { 20. } else { 24. };
        let glyph = if view.update_progress.is_some() {
            Icon::LoaderCircle
        } else if view.compatibility_warning {
            Icon::TriangleAlert
        } else {
            Icon::CircleArrowUp
        };
        let color = if view.compatibility_warning { hsla(p.warning) } else { hsla(p.muted_foreground) };
        let toggle_id = instance_id.to_owned();
        let tooltip: SharedString = if view.update_progress.is_some() { "Updating".into() } else { advisory.title.clone().into() };
        let trigger = h_flex()
            .id(SharedString::from(format!("provider-advisory-{instance_id}-{compact}")))
            .size(r(size))
            .flex_none()
            .justify_center()
            .rounded(r(if compact { 4. } else { 6. }))
            .cursor_pointer()
            .hover(|this| this.bg(hsla(p.accent)))
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                let just_closed = this.providers.advisory_closed_at.is_some_and(|at| at.elapsed() < Duration::from_millis(200));
                if this.providers.advisory_open.is_none() && !just_closed {
                    this.providers.advisory_open = Some(toggle_id.clone());
                }
                cx.notify();
            }))
            .child(icon(glyph, 14., color));
        let popup = open.then(|| self.render_advisory_popover(instance_id, view, &advisory, cx));
        div().relative().flex_none().child(trigger).children(popup).into_any_element()
    }

    fn render_advisory_popover(&self, instance_id: &str, view: &InstanceView, advisory: &model::AdvisoryView, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let updating = view.update_progress.is_some();
        let action = view.version_action.clone().map(|action| {
            let id = instance_id.to_owned();
            let target = action.target_version().map(str::to_owned);
            Button::new("provider-version-action", ButtonVariant::Outline, ButtonSize::Compact)
                .icon(if updating { Icon::LoaderCircle } else { Icon::Download })
                .label(if updating { "Updating".to_owned() } else { action.label() })
                .disabled(updating)
                .on_click(cx.listener(move |this, _, _, cx| this.run_update(&id, target.clone(), cx)))
        });
        let progress = view.update_progress.clone().or_else(|| view.update_problem.clone()).map(|text| {
            div()
                .text_xs()
                .line_height(r(16.))
                .text_color(if view.update_problem.is_some() && !updating { hsla(p.warning) } else { hsla(p.muted_foreground) })
                .child(text)
        });
        let error = self.providers.update_error.as_ref().filter(|(id, _)| id == instance_id).map(|(_, message)| {
            div().text_xs().line_height(r(16.)).text_color(hsla(p.destructive_foreground)).child(message.clone())
        });
        let command = advisory.update_command.clone().map(|command| {
            let copy = command.clone();
            h_flex()
                .min_w_0()
                .gap(r(4.))
                .py(r(2.))
                .pr(r(2.))
                .pl(r(8.))
                .rounded(r(6.))
                .border_1()
                .border_color(hsla(p.border).opacity(0.7))
                .bg(hsla(p.muted).opacity(0.4))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(r(11.))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(hsla(p.foreground))
                        .child(command),
                )
                .child(
                    Button::new("copy-update-command", ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                        .icon(Icon::Copy)
                        .tooltip("Copy command")
                        .on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))),
                )
        });
        let divider = (action.is_some() && command.is_some()).then(|| {
            h_flex()
                .gap(r(8.))
                .text_size(r(10.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.muted_foreground))
                .child(div().h(px(1.)).flex_1().bg(hsla(p.border)))
                .child("OR, UPDATE MANUALLY USING")
                .child(div().h(px(1.)).flex_1().bg(hsla(p.border)))
        });
        div()
            .absolute()
            .bottom_0()
            .right_0()
            .child(
                gpui::deferred(
                    gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
                        v_flex()
                            .id("provider-advisory-popover")
                            .occlude()
                            .mt(r(4.))
                            .w(r(320.))
                            .p(r(16.))
                            .gap(r(12.))
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.dropdown_border))
                            .bg(hsla(p.popover))
                            .shadow(crate::composer::controls::floating_shadow(dark))
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.providers.advisory_open = None;
                                this.providers.advisory_closed_at = Some(Instant::now());
                                cx.notify();
                            }))
                            .child(
                                v_flex()
                                    .gap(r(2.))
                                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(advisory.title.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .line_height(r(16.))
                                            .text_color(if advisory.strong { hsla(p.warning) } else { hsla(p.muted_foreground) })
                                            .child(advisory.detail.clone()),
                                    ),
                            )
                            .children(action)
                            .children(progress)
                            .children(error)
                            .children(divider)
                            .children(command),
                    ),
                )
                .with_priority(2),
            )
            .into_any_element()
    }

    fn render_advanced(&mut self, panel: &ProviderPanelSettings, cx: &mut Context<Self>) -> AnyElement {
        let current = panel.health_interval_seconds();
        let default = panel.default_health_interval_seconds();
        let state = self.state.clone();
        let reset_payload = panel.health_interval_payload(None);
        let interval = Row::new("provider-health-check-interval", "Health check interval")
            .description("Refresh provider status, versions, and models in the background. Set to 0 to disable.")
            .highlighted(self.is_target("provider-health-check-interval"))
            .reset(current != default, move |_, _, cx| store::update_server(&state, ServerSettingChange::Prepared(reset_payload.clone()), cx))
            .control(
                h_flex()
                    .gap(r(8.))
                    .child(div().w(r(96.)).child(gpui_component::input::Input::new(&self.providers.health_interval).small()))
                    .child(div().text_xs().text_color(hsla(palette(cx).muted_foreground)).child("seconds")),
            );
        let mode = panel.idle_compaction();
        let options = IDLE_COMPACTION_MODES.iter().map(|(value, label)| SelectOption::new(*value, *label)).collect();
        let state = self.state.clone();
        let select_panel = panel.clone();
        let reset_state = self.state.clone();
        let reset_payload = panel.idle_compaction_payload("onReturn");
        let compact = Row::new("compact-idle-threads", "Compact idle threads")
            .description(
                "When to compact an idle Claude thread over 200,000 tokens: after you return, just before its prompt cache expires, or never. Each Claude provider can override this.",
            )
            .highlighted(self.is_target("compact-idle-threads"))
            .reset(mode != "onReturn", move |_, _, cx| store::update_server(&reset_state, ServerSettingChange::Prepared(reset_payload.clone()), cx))
            .control(Select::new("compact-idle-threads-select", options).selected(mode).width(224.).show_check().on_change(move |value, _, cx| {
                if value.as_ref() != select_panel.idle_compaction() {
                    store::update_server(&state, ServerSettingChange::Prepared(select_panel.idle_compaction_payload(&value)), cx);
                }
            }));
        section("Advanced", vec![interval.into_any_element(), compact.into_any_element()], cx).into_any_element()
    }

    /// Opens or replaces the selected instance's auth and install subscriptions.
    fn sync_streams(&mut self, selected: Option<&Selected>, cx: &mut Context<Self>) {
        #[cfg(feature="snapshot")]
        if let Some(fixture)=cx.try_global::<crate::chatgpt_setup::Fixture>().filter(|f|!f.managed) {
            self.providers.auth=Some(AuthSession {instance_id:fixture.provider.instance_id.clone(),state:Some(fixture.auth.clone()),load_error:None,pending:false,error:None,_stream:None});
            return;
        }
        if fixture_active(cx) { return; }
        if selected.is_none_or(|s| s.row.driver != "codex") { self.providers.chatgpt = None; }
        let wants_auth = self.providers.dialog.as_ref().filter(|d|d.created).map(|d|d.instance_id.read(cx).value().to_string()).or_else(|| selected.filter(|selected| model::chatgpt::uses_generic_account(&selected.row.driver,selected.row.instance().config.as_ref()) && editor::shows_account(selected)).map(|selected| selected.row.instance_id.clone()));
        if self.providers.auth.as_ref().map(|session| &session.instance_id) != wants_auth.as_ref() {
            self.providers.method_id = SharedString::default();
            self.providers.auth = wants_auth.map(|instance_id| {
                let target = instance_id.clone();
                let stream = self.subscribe(auth::AUTH_SUBSCRIBE, auth::subscribe_payload(&instance_id), cx, move |this, item, _| {
                    let Some(session) = this.providers.auth.as_mut().filter(|session| session.instance_id == target) else { return };
                    match item {
                        Ok(value) => {
                            if let Some(state) = auth::decode_auth_state(value) {
                                session.state = Some(state);
                                session.load_error = None;
                            }
                        }
                        Err(message) => session.load_error = Some(message),
                    }
                });
                AuthSession { instance_id, state: None, load_error: None, pending: false, error: None, _stream: stream }
            });
        }
        let wants_install = selected.filter(|selected| selected.row.driver == "antigravity" && selected.live.as_ref().is_some_and(|live| live.setup.is_some())).map(|selected| selected.row.instance_id.clone());
        if self.providers.install.as_ref().map(|session| &session.instance_id) != wants_install.as_ref() {
            self.providers.install = wants_install.map(|instance_id| {
                let target = instance_id.clone();
                let stream = self.subscribe(auth::INSTALL_SUBSCRIBE, auth::subscribe_payload(&instance_id), cx, move |this, item, _| {
                    let Some(session) = this.providers.install.as_mut().filter(|session| session.instance_id == target) else { return };
                    match item {
                        Ok(value) => {
                            if let Some(state) = auth::decode_install_state(value) {
                                session.state = Some(state);
                                session.load_error = None;
                            }
                        }
                        Err(message) => session.load_error = Some(message),
                    }
                });
                InstallSession { instance_id, state: None, load_error: None, pending: false, error: None, _stream: stream }
            });
        }
    }

    /// A streaming RPC whose items reach `on_item` on the UI thread, batched per frame. It
    /// resubscribes after a reconnect and stops on a server failure, which it reports.
    pub(super) fn subscribe(
        &self,
        method: &'static str,
        payload: Value,
        cx: &mut Context<Self>,
        on_item: impl Fn(&mut SettingsView, Result<Value, String>, &mut Context<SettingsView>) + 'static,
    ) -> Option<Stream> {
        let (client, runtime) = {
            let app = self.state.read(cx);
            (app.client_handle()?, app.runtime().clone())
        };
        let (tx, mut rx) = mpsc::unbounded_channel::<Result<Value, String>>();
        let driver = runtime.spawn(async move {
            let mut attempt = 0u32;
            loop {
                match client.stream(method, payload.clone()).await {
                    Ok(mut stream) => {
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(value) => {
                                    attempt = 0;
                                    if tx.send(Ok(value)).is_err() {
                                        return;
                                    }
                                }
                                Err(RpcError::Failure(failure)) => {
                                    let _ = tx.send(Err(auth::failure_message(&failure)));
                                    return;
                                }
                                Err(RpcError::Closed) => break,
                            }
                        }
                    }
                    Err(RpcError::Failure(failure)) => {
                        let _ = tx.send(Err(auth::failure_message(&failure)));
                        return;
                    }
                    Err(RpcError::Closed) => {}
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_millis((250u64 << attempt.min(5)).min(5_000))).await;
            }
        });
        let pump = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(item) = rx.try_recv() {
                    batch.push(item);
                }
                let applied = this.update(cx, |this, cx| {
                    for item in batch {
                        on_item(this, item, cx);
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        Some(Stream { _driver: AbortOnDrop(driver), _pump: pump })
    }

    /// Fills the name and accent fields from the selected instance, unless the user is typing.
    fn sync_editor_inputs(&mut self, selected: Option<&Selected>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = selected else { return };
        let switched = self.providers.synced_instance.as_deref() != Some(selected.row.instance_id.as_str());
        if switched {
            self.providers.synced_instance = Some(selected.row.instance_id.clone());
            self.providers.adding_custom_model = false;
            self.providers.custom_model_error = None;
            self.providers.advisory_open = None;
            self.providers.model_filter.update(cx, |input, cx| input.set_value("", window, cx));
        }
        let instance = selected.row.instance();
        let placeholder = model::find_driver(&selected.row.driver).map_or("Instance label", |driver| driver.label).to_owned();
        let fields = [
            (&self.providers.display_name, instance.display_name.clone().unwrap_or_default()),
            (&self.providers.accent, instance.accent_color.clone().unwrap_or_default()),
        ];
        for (input, value) in fields {
            if (switched || !input.focus_handle(cx).is_focused(window)) && input.read(cx).value().as_ref() != value {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
        if switched {
            self.providers.display_name.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
            let custom = model::find_driver(&selected.row.driver).map_or("model-slug", |driver| driver.custom_model_placeholder);
            self.providers.custom_model.update(cx, |input, cx| input.set_placeholder(custom, window, cx));
        }
    }

    /// The "Checked …" label is relative, so the page redraws once when it would change.
    fn schedule_label_refresh(&mut self, delay: Option<i64>, cx: &mut Context<Self>) {
        let Some(delay) = delay else {
            self.providers.label_timer = None;
            return;
        };
        let due = now_ms() + delay;
        if self.providers.label_timer.as_ref().is_some_and(|(at, _)| (*at - due).abs() < 1_000) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(delay.max(0) as u64 + 50)).await;
            let _ = this.update(cx, |this, cx| {
                this.providers.label_timer = None;
                if this.section == search::Section::Providers {
                    cx.notify();
                }
            });
        });
        self.providers.label_timer = Some((due, task));
    }

    /// Applies a provider list a refresh or update replied with.
    fn apply_providers_reply(&mut self, reply: Value, cx: &mut Context<Self>) {
        if let Some(providers) = model::decode_providers_reply(reply) {
            self.state.update(cx, |state, cx| {
                if state.server_config.apply_providers(providers) {
                    cx.emit(AppEvent::ServerConfig);
                }
            });
        }
    }

    fn refresh_providers(&mut self, cx: &mut Context<Self>) {
        if self.providers.refreshing {
            return;
        }
        self.providers.refreshing = true;
        cx.notify();
        let task = self.state.read(cx).rpc(model::SERVER_REFRESH_PROVIDERS, model::refresh_payload());
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.providers.refreshing = false;
                match result {
                    Ok(reply) => this.apply_providers_reply(reply, cx),
                    Err(error) => tracing::warn!(%error, "failed to refresh providers"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `runProviderUpdate`: one call per instance at a time.
    fn run_update(&mut self, instance_id: &str, target_version: Option<String>, cx: &mut Context<Self>) {
        let driver = {
            let app = self.state.read(cx);
            app.server_config.config.as_ref().and_then(|config| config.provider(instance_id)).map(|provider| provider.driver.clone())
        };
        let Some(driver) = driver else { return };
        if !self.providers.dispatching.insert(instance_id.to_owned()) {
            return;
        }
        self.providers.update_error = None;
        cx.notify();
        let task = self.state.read(cx).rpc(model::SERVER_UPDATE_PROVIDER, model::update_payload(&driver, instance_id, target_version.as_deref()));
        let id = instance_id.to_owned();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.providers.dispatching.remove(&id);
                match result {
                    Ok(reply) => this.apply_providers_reply(reply, cx),
                    Err(error) => {
                        let name = model::find_driver(&driver).map_or(driver.clone(), |driver| driver.label.to_owned());
                        let message = failure_text(&error, "The provider update command could not be started.");
                        this.providers.update_error = Some((id.clone(), format!("Could not update {name}: {message}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Saves an edit of one instance as an upsert.
    fn update_instance(&mut self, instance_id: &str, edit: model::InstanceEdit, cx: &mut Context<Self>) {
        if !self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE,cx) {return;}
        if matches!(edit, model::InstanceEdit::Enabled(true)) {
            let app = self.state.read(cx);
            if let Some(c) = app.server_config.config.as_ref() {
                let driver = c.provider(instance_id).map(|p| p.driver.as_str()).or_else(|| c.settings.provider_instances.get(instance_id).map(|i| i.driver.as_str())).unwrap_or(instance_id);
                if t3_core::managed_policy::driver_denial(c.managed_policy.as_ref(), driver, driver).is_some() { return; }
            }
        }
        store::update_server(&self.state, ServerSettingChange::ProviderInstance { instance_id: instance_id.to_owned(), edit }, cx);
    }

    fn selected_instance_id(&self, cx: &App) -> Option<String> {
        let app = self.state.read(cx);
        let config = app.server_config.config.as_ref()?;
        let rows = model::instance_rows(&config.settings, &config.providers, self.providers.selected.as_deref());
        let selected = self.providers.selected.as_deref().and_then(|id| rows.iter().find(|row| row.instance_id == id)).or(rows.first());
        selected.map(|row| row.instance_id.clone())
    }

    fn commit_display_name(&mut self, cx: &mut Context<Self>) {
        let Some(instance_id) = self.selected_instance_id(cx) else { return };
        let value = self.providers.display_name.read(cx).value().to_string();
        self.update_instance(&instance_id, model::InstanceEdit::DisplayName(value.to_string()), cx);
    }

    /// Saves a typed accent color, or puts the saved one back when it is not a `#rrggbb`.
    fn commit_accent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(instance_id) = self.selected_instance_id(cx) else { return };
        let value = self.providers.accent.read(cx).value().trim().to_owned();
        if !value.is_empty() && model::normalize_accent(&value).is_none() {
            let saved = {
                let app = self.state.read(cx);
                let config = app.server_config.config.as_ref();
                config.and_then(|config| config.settings.provider_instances.get(&instance_id)).and_then(|instance| instance.accent_color.clone())
            };
            self.providers.accent.update(cx, |input, cx| input.set_value(saved.unwrap_or_default(), window, cx));
            return;
        }
        self.update_instance(&instance_id, model::InstanceEdit::Accent(value.to_string()), cx);
    }

    fn commit_health_interval(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.providers.health_interval.read(cx).value().trim().to_owned();
        let panel = self.state.read(cx).server_config.config.as_ref().map(|config| config.settings.provider_panel.clone());
        let Some(panel) = panel else { return };
        match typed.parse::<f64>().ok().filter(|seconds| seconds.is_finite()) {
            Some(seconds) => {
                let seconds = seconds.max(0.).round() as u64;
                if seconds != panel.health_interval_seconds() {
                    store::update_server(&self.state, ServerSettingChange::Prepared(panel.health_interval_payload(Some(seconds))), cx);
                }
            }
            None => {
                let saved = panel.health_interval_seconds().to_string();
                self.providers.health_interval.update(cx, |input, cx| input.set_value(saved, window, cx));
            }
        }
    }

    /// Saves what a commit-on-blur provider field holds before the modal closes.
    pub(super) fn commit_focused_provider_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_focused_form_field(window,cx);
        if self.providers.display_name.focus_handle(cx).is_focused(window) {
            self.commit_display_name(cx);
        }
        if self.providers.accent.focus_handle(cx).is_focused(window) {
            self.commit_accent(window, cx);
        }
        if self.providers.health_interval.focus_handle(cx).is_focused(window) {
            self.commit_health_interval(window, cx);
        }
    }

    /// Applies the settings an `updateSettings` call this page made itself answered with.
    fn apply_settings_reply(&mut self, reply: Value, sent_at: u64, cx: &mut Context<Self>) {
        if let Some(settings) = decode_update_reply(reply) {
            self.state.update(cx, |state, cx| {
                if state.server_config.apply_settings_reply(settings, sent_at) {
                    cx.emit(AppEvent::ServerConfig);
                }
            });
        }
    }
}

/// The 6px dot a warning or error shows (`PROVIDER_STATUS_STYLES`).
impl SettingsView {
    fn render_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, detail) = self.providers.notice.clone()?;
        let p = palette(cx);
        Some(
            h_flex()
                .items_start()
                .gap(r(8.))
                .px(r(14.))
                .py(r(12.))
                .rounded(r(12.))
                .border_1()
                .border_color(hsla(p.error).opacity(0.32))
                .bg(hsla(p.error_surface))
                .text_sm()
                .text_color(hsla(p.error_foreground))
                .child(v_flex().flex_1().min_w_0().gap(r(2.)).child(div().font_weight(FontWeight::MEDIUM).child(title)).child(div().opacity(0.8).child(detail)))
                .child(Button::new("provider-notice-dismiss", ButtonVariant::GhostMuted, ButtonSize::IconMicro).icon(Icon::X).tooltip("Dismiss").on_click(cx.listener(|this, _, _, cx| {
                    this.providers.notice = None;
                    cx.notify();
                })))
                .into_any_element(),
        )
    }
}

fn status_dot(status: StatusKey, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let color = match status {
        StatusKey::Error => hsla(p.destructive),
        StatusKey::Warning => hsla(p.warning),
        StatusKey::Ready => hsla(p.success),
        StatusKey::Neutral => hsla(p.muted_foreground),
        StatusKey::Disabled => hsla(p.muted_foreground).opacity(0.5),
    };
    div().size(r(6.)).flex_none().rounded_full().bg(color)
}

/// A flat section of the editor: a title row, then rows divided edge to edge.
fn flat_section(title: impl Into<SharedString>, rows: Vec<AnyElement>, cx: &App) -> gpui::Div {
    let divider = hsla(palette(cx).border).opacity(0.5);
    v_flex()
        .gap(r(4.))
        .child(section_title(title))
        .child(v_flex().children(rows.into_iter().enumerate().map(move |(index, row)| div().when(index > 0, |this| this.border_t_1().border_color(divider)).child(row))))
}
