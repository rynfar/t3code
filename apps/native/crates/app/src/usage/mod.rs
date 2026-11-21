//! Usage: a modal over the workspace (`web/src/routes/_chat.usage.tsx`, `UsagePage.tsx`) with
//! cost, tokens and subscription limits (keys C, T, L), a period (`cmd-shift-1..4`), and
//! environment and provider filters. Opened with `cmd-u`, the palette, or the sidebar footer;
//! Esc closes it. The view exists only while open. Summaries load on the network runtime and
//! stay cached per window while it is open; nothing repaints until an answer or input arrives.

mod chart;
mod controls;
pub(crate) use controls::brand_mark;
mod data;
mod dialog;
mod header;
mod limits;
mod overview;
mod prices;

use std::collections::HashMap;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, Global, IntoElement, KeyBinding, MouseButton, ParentElement, Render,
    Styled, Subscription, Task, Window, actions, div, point, prelude::*, px,
};
use gpui_component::{ActiveTheme, v_flex};
use t3_core::usage::limits::Redeem;
use t3_core::usage::{UsageMetric, UsagePeriod};

use crate::app_state::{AppEvent, AppState};
use crate::theme::{hsla, palette};
use data::UsageData;

actions!(usage, [OpenUsage, CloseUsage, ShowCost, ShowTokens, ShowLimits, PeriodDay, PeriodWeek, PeriodMonth, PeriodQuarter]);

struct Host {
    state: Entity<AppState>,
    view: Option<Entity<UsageView>>,
    pending: bool,
    /// When Limits may ask each environment's providers again on its own (`refreshUsageLimits`, five minutes).
    limits_refresh_after: HashMap<String, i64>,
    /// Never asks providers to refresh, for the snapshot tool, which must not write.
    read_only: bool,
    /// The metric, period and whether to open the top model that the next view starts on.
    initial: Option<(UsageMetric, Option<UsagePeriod>, bool)>,
}

impl Global for Host {}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    cx.set_global(Host { state, view: None, pending: false, limits_refresh_after: HashMap::new(), read_only: false, initial: None });
    cx.on_action(|_: &OpenUsage, cx| open(cx));
    cx.bind_keys([KeyBinding::new("escape", CloseUsage, Some("UsageModal"))]);
}

pub fn open(cx: &mut App) {
    cx.global_mut::<Host>().pending = true;
    cx.refresh_windows();
}

/// Opens Usage on `metric` (and `period`) without asking providers to refresh, optionally with
/// the top model's breakdown open once it loads: the snapshot tool's entry.
#[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
pub fn open_read_only(metric: UsageMetric, period: Option<UsagePeriod>, model: bool, cx: &mut App) {
    let host = cx.global_mut::<Host>();
    host.read_only = true;
    host.initial = Some((metric, period, model));
    open(cx);
}

/// Whether an open page has its answer (or its error), and any requested model is open.
#[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
pub fn is_settled(cx: &mut App) -> bool {
    let Some(view) = cx.global::<Host>().view.clone() else { return false };
    let view = view.read(cx);
    let limits = view.data.preferences.metric == UsageMetric::Limits;
    limits || (view.data.is_settled() && !view.open_top_model)
}

fn close(cx: &mut App) {
    cx.global_mut::<Host>().view = None;
    cx.refresh_windows();
}

/// The modal layer the workspace draws above everything while Usage is open.
pub fn overlay(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if std::mem::take(&mut cx.global_mut::<Host>().pending) && cx.global::<Host>().view.is_none() {
        let state = cx.global::<Host>().state.clone();
        let view = cx.new(|cx| UsageView::new(state, window, cx));
        view.read(cx).focus.clone().focus(window, cx);
        cx.global_mut::<Host>().view = Some(view);
    }
    let view = cx.global::<Host>().view.clone()?;
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let inset = if window.viewport_size().width >= px(1024.) { 40. } else { 24. };
    let modal = div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(px(inset))
        .bg(hsla(p.dialog_backdrop))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| close(cx))
        .child(
            div()
                .size_full()
                .max_w(px(1280.))
                .max_h(px(960.))
                .flex()
                .rounded(px(16.))
                .border_1()
                .border_color(hsla(p.border))
                .bg(hsla(p.background))
                .text_color(hsla(p.foreground))
                .overflow_hidden()
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::black().opacity(if dark { 0.6 } else { 0.4 }),
                    offset: point(px(0.), px(25.)),
                    blur_radius: px(50.),
                    spread_radius: px(-12.),
                    inset: false,
                }])
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(view),
        );
    Some(gpui::deferred(modal).with_priority(1).into_any_element())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    Environment,
    Provider,
    Period,
    Metric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Breakdown {
    Model,
    Time,
}

/// A pool segment whose details are open: driver, window id, and account key.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentKey {
    driver: String,
    window: String,
    account: String,
}

pub struct UsageView {
    state: Entity<AppState>,
    focus: FocusHandle,
    refresh_spinner: Entity<header::RefreshSpinner>,
    data: UsageData,
    menu: Option<MenuKind>,
    breakdown: Breakdown,
    hover: Option<chart::ChartHover>,
    selected_model: Option<String>,
    dialog_hover: Option<chart::ChartHover>,
    prices: Option<prices::PriceOverridesDialog>,
    segment: Option<SegmentKey>,
    revealed_account_email: Option<String>,
    confirm_reset: Option<(Redeem, String)>,
    /// The account a reset credit is being used for, and what came of it.
    reset_status: Option<(String, Option<String>)>,
    /// Opens the top model's breakdown once a summary arrives (snapshot tool).
    open_top_model: bool,
    _subscriptions: Vec<Subscription>,
    _grants: Option<Task<()>>,
    /// The active connection whose grants stream `_grants` is watching.
    connection: Option<u64>,
}

impl Focusable for UsageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl UsageView {
    fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let connection = state.read(cx).connection_id();
        let refresh_spinner = cx.new(|_| header::RefreshSpinner::new());
        let subscriptions = vec![cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
            AppEvent::ServerConfig | AppEvent::Shell => {
                this.data.config_changed();
                this.sync_access(cx);
                this.auto_refresh_limits(cx);
                cx.notify();
            }
            AppEvent::Phase => {
                let next_connection = this.state.read(cx).connection_id();
                if next_connection != this.connection {
                    this.connection = next_connection;
                    this.watch_grants(cx);
                }
                this.sync_access(cx);
                this.auto_refresh_limits(cx);
                cx.notify();
            }
            _ => {}
        })];
        let mut view = UsageView {
            state,
            focus: cx.focus_handle(),
            refresh_spinner,
            data: UsageData::new(),
            menu: None,
            breakdown: Breakdown::Model,
            hover: None,
            selected_model: None,
            dialog_hover: None,
            prices: None,
            segment: None,
            revealed_account_email: None,
            confirm_reset: None,
            reset_status: None,
            open_top_model: false,
            _subscriptions: subscriptions,
            _grants: None,
            connection,
        };
        if let Some((metric, period, model)) = cx.global_mut::<Host>().initial.take() {
            view.data.preferences.metric = metric;
            if let Some(period) = period {
                view.data.preferences.period = period;
                view.data.set_window(crate::sidebar::controller::now_ms());
            }
            view.open_top_model = model;
        }
        view.watch_grants(cx);
        view.sync_access(cx);
        view.auto_refresh_limits(cx);
        view
    }

    pub(super) fn set_refreshing(&mut self, refreshing: bool, cx: &mut Context<Self>) {
        if self.data.refreshing == refreshing {
            return;
        }
        self.data.refreshing = refreshing;
        self.refresh_spinner.update(cx, |spinner, cx| spinner.set_spinning(refreshing, cx));
        cx.notify();
    }

    /// Follows the active connection's grants (fallback when fleet grants are absent).
    fn watch_grants(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.state.read(cx).client_handle() else {
            self.data.active_session = None;
            self._grants = None;
            return;
        };
        let mut grants = client.session_state();
        self.data.active_session = grants.borrow_and_update().clone();
        self._grants = Some(cx.spawn(async move |this, cx| {
            while grants.changed().await.is_ok() {
                let session = grants.borrow_and_update().clone();
                if this.update(cx, |view, cx| view.set_session(session, cx)).is_err() {
                    return;
                }
            }
        }));
    }

    fn set_session(&mut self, session: Option<t3_core::auth::AuthSessionState>, cx: &mut Context<Self>) {
        if self.data.active_session != session {
            self.data.active_session = session;
            self.sync_access(cx);
            cx.notify();
        }
    }

    fn select_metric(&mut self, metric: UsageMetric, cx: &mut Context<Self>) {
        if metric == UsageMetric::Limits {
            self.data.limits_now = crate::sidebar::controller::now_ms();
            self.data.invalidate_limits();
        }
        self.data.preferences.metric = metric;
        self.data.save_preferences();
        self.menu = None;
        self.auto_refresh_limits(cx);
        cx.notify();
    }

    fn select_period(&mut self, period: UsagePeriod, cx: &mut Context<Self>) {
        self.data.preferences.period = period;
        self.data.save_preferences();
        self.menu = None;
        self.hover = None;
        self.data.set_window(crate::sidebar::controller::now_ms());
        self.load(false, cx);
        cx.notify();
    }

    /// Shortcuts stand down while a dialog or menu inside the page has the keys.
    fn shortcut_blocked(&self) -> bool {
        self.prices.is_some() || self.selected_model.is_some() || self.menu.is_some() || self.confirm_reset.is_some()
    }

    fn on_metric_key(&mut self, metric: UsageMetric, cx: &mut Context<Self>) {
        if !self.shortcut_blocked() {
            self.select_metric(metric, cx);
        }
    }

    fn on_period_key(&mut self, period: UsagePeriod, cx: &mut Context<Self>) {
        if !self.shortcut_blocked() && self.data.preferences.metric != UsageMetric::Limits {
            self.select_period(period, cx);
        }
    }

    /// Esc peels one layer: the confirm, prices dialog (unless saving), a menu or popover, the model dialog, then the page.
    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.confirm_reset.take().is_some() {
            self.dialog_hover = None;
            cx.notify();
            return;
        }
        if let Some(dialog) = self.prices.as_ref() {
            if !dialog.is_saving() {
                self.prices = None;
                self.dialog_hover = None;
                cx.notify();
            }
            return;
        }
        if self.menu.take().is_some()
            || self.segment.take().is_some()
            || self.selected_model.take().is_some()
        {
            self.dialog_hover = None;
            cx.notify();
            return;
        }
        close(cx);
    }
}

impl Render for UsageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.data.prepare(&self.state, cx);
        if self.open_top_model && self.data.answered_count() > 0 && self.data.is_settled() {
            self.open_top_model = false;
            self.selected_model = self.data.merged.models.first().map(|model| model.key());
        }
        let p = palette(cx);
        v_flex()
            .key_context("UsageModal usagePageOpen")
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .relative()
            .on_action(cx.listener(|this, _: &CloseUsage, _, cx| this.escape(cx)))
            .on_action(cx.listener(|this, _: &ShowCost, _, cx| this.on_metric_key(UsageMetric::Cost, cx)))
            .on_action(cx.listener(|this, _: &ShowTokens, _, cx| this.on_metric_key(UsageMetric::Tokens, cx)))
            .on_action(cx.listener(|this, _: &ShowLimits, _, cx| this.on_metric_key(UsageMetric::Limits, cx)))
            .on_action(cx.listener(|this, _: &PeriodDay, _, cx| this.on_period_key(UsagePeriod::Day, cx)))
            .on_action(cx.listener(|this, _: &PeriodWeek, _, cx| this.on_period_key(UsagePeriod::Week, cx)))
            .on_action(cx.listener(|this, _: &PeriodMonth, _, cx| this.on_period_key(UsagePeriod::Month, cx)))
            .on_action(cx.listener(|this, _: &PeriodQuarter, _, cx| this.on_period_key(UsagePeriod::Quarter, cx)))
            .on_action(|_: &OpenUsage, _, _| {})
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus.focus(window, cx);
                    if this.menu.is_some() || this.segment.is_some() {
                        this.menu = None;
                        this.segment = None;
                        cx.notify();
                    }
                }),
            )
            .child(self.render_header(window, cx))
            .child(
                div()
                    .id("usage-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.render_body(window, cx)),
            )
            .children(self.render_model_dialog(window, cx))
            .children(self.render_price_overrides_dialog(window, cx))
            .children(self.render_reset_confirm(window, cx))
            .text_color(hsla(p.foreground))
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use serde_json::json;
    use t3_client::environments::Environments;
    use t3_client::supervisor::{ClientFixture, ConnectionStatus};
    use t3_core::server_config::ServerConfigState;
    use t3_core::shell::ShellState;

    fn sample_summary(host: &str, model: &str, cost_usd: f64, tokens: u64) -> serde_json::Value {
        json!({
            "contractVersion": t3_core::usage::USAGE_CONTRACT_VERSION,
            "readAt": "2026-11-02T12:00:00.000Z",
            "timeZone": "UTC",
            "sinceDay": "2026-11-01",
            "untilDay": "2026-11-02",
            "pricing": { "status": "fresh", "source": "litellm", "fetchedAt": null, "knownModels": 10 },
            "scanDurationMs": 1,
            "sources": [{
                "fingerprint": {
                    "hostId": host,
                    "provider": "claude",
                    "resolvedHomePath": format!("/{host}/.claude"),
                    "volumeId": host
                },
                "status": "ok",
                "scannedFiles": 1,
                "skippedFiles": 0,
                "malformedRecords": 0,
                "distinctSessions": 2,
                "message": null
            }],
            "buckets": [{
                "day": "2026-11-02",
                "provider": "claude",
                "model": model,
                "totals": {
                    "uncachedInputTokens": tokens,
                    "cachedInputTokens": 0,
                    "cacheCreationTokens": 0,
                    "outputTokens": 0,
                    "reasoningTokens": 0
                },
                "costUsd": cost_usd,
                "costSource": "modelPriced",
                "records": 1,
                "unpricedRecords": 0,
                "sessions": 2,
                "cacheSavingsUsd": 0.0
            }]
        })
    }

    fn server_config_for(env_id: &str, label: &str) -> ServerConfigState {
        let config: t3_core::server_config::ServerConfig = serde_json::from_value(json!({
            "environment": { "environmentId": env_id, "label": label },
            "settings": {},
            "providers": []
        }))
        .unwrap();
        let mut state = ServerConfigState::default();
        state.config = Some(config);
        state
    }

    #[gpui::test]
    async fn multi_environment_usage_fans_out_merges_filters_and_spins_during_refresh(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _orig) =
            cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.update(|cx| init(state.clone(), cx));

        let (connections, _output) = Environments::start(runtime.handle(), false);
        state.update(cx, |state, _| {
            state.stop_pump();
            state.connections = connections;
            state.environments.active = Some("env-mac".into());
            state.environments.environments = serde_json::from_value(json!([
                {
                    "id": "env-mac", "environmentId": "env-mac-id", "label": "Mac Studio",
                    "httpBase": "http://mac.invalid", "accessToken": "", "disabled": false
                },
                {
                    "id": "env-linux", "environmentId": "env-linux-id", "label": "Linux Box",
                    "httpBase": "http://linux.invalid", "accessToken": "", "disabled": false
                }
            ]))
            .unwrap();
        });
        let mut fleet_rx = state.read_with(cx, |state, _| state.connections.view());
        let grants: t3_core::auth::AuthSessionState = serde_json::from_value(json!({
            "authenticated": true,
            "permissions": ["diagnostics:read", "providers:manage"]
        }))
        .unwrap();
        let (client_a, mut fixture_a) = ClientFixture::handle(grants.clone());
        let (client_b, mut fixture_b) = ClientFixture::handle(grants);
        state.update(cx, |state, cx| {
            state.install_active_environment(
                client_a.clone(),
                ShellState::default(),
                server_config_for("env-mac-id", "Mac Studio"),
                ConnectionStatus::Connected,
                cx,
            );
        });
        let install_a = state.update(cx, |state, _| {
            state.connections.install_fixture(
                "env-mac",
                client_a,
                ShellState::default(),
                server_config_for("env-mac-id", "Mac Studio"),
                ConnectionStatus::Connected,
            )
        });
        let install_b = state.update(cx, |state, _| {
            state.connections.install_fixture(
                "env-linux",
                client_b,
                ShellState::default(),
                server_config_for("env-linux-id", "Linux Box"),
                ConnectionStatus::Connected,
            )
        });
        let _events_a = runtime.spawn(install_a).await.unwrap();
        let _events_b = runtime.spawn(install_b).await.unwrap();
        let fleet = fleet_rx.borrow_and_update().clone();
        state.update(cx, |state, cx| state.apply_fleet_view(fleet, cx));

        let (view, vcx) = cx.add_window_view(|window, cx| UsageView::new(state.clone(), window, cx));
        vcx.run_until_parked();

        // Observe `view` until both environments finish decoding and applying their summaries.
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let done_tx = std::rc::Rc::new(std::cell::RefCell::new(Some(done_tx)));
        let _settled_obs = vcx.update(|_, cx| {
            let done_tx = done_tx.clone();
            cx.observe(&view, move |view, cx| {
                if view.read(cx).data.answered_count() == 2
                    && let Some(tx) = done_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });

        // Both environments receive provisional `server.getUsageSummary` requests in parallel.
        async fn next_summary_request(fixture: &mut ClientFixture) -> t3_client::supervisor::FixtureRequest {
            loop {
                let (method, payload, reply) = fixture.next_request().await;
                if method != t3_core::usage::method::REFRESH_PROVIDERS {
                    return (method, payload, reply);
                }
                reply.send(Ok(json!({}))).unwrap();
            }
        }
        let ((method_a, _, reply_a), (method_b, _, reply_b)) = futures::future::join(
            next_summary_request(&mut fixture_a),
            next_summary_request(&mut fixture_b),
        ).await;
        assert_eq!(method_a, t3_core::usage::method::GET_USAGE_SUMMARY);
        assert_eq!(method_b, t3_core::usage::method::GET_USAGE_SUMMARY);
        let _ = reply_a.send(Ok(sample_summary("host-mac", "claude-sonnet-4", 12.5, 1_000)));
        let _ = reply_b.send(Ok(sample_summary("host-linux", "claude-opus-4", 7.5, 2_000)));
        done_rx.await.expect("both environments settled");
        vcx.run_until_parked();

        // Trigger prepare/render and assert merged totals across both environments.
        view.update(vcx, |view, cx| view.data.prepare(&view.state, cx));
        view.read_with(vcx, |view, cx| {
            assert_eq!(view.data.envs.len(), 2);
            assert_eq!(view.data.environment_filter_label(cx), "All environments");
            assert!((view.data.merged.cost_usd - 20.0).abs() < 1e-6);
            assert_eq!(view.data.merged.total_tokens, 3_000);
            assert_eq!(view.data.merged.sessions, 4);
            assert_eq!(view.data.merged.models.len(), 2);
        });

        // Unchecking Linux Box filters totals down to Mac Studio only.
        view.update(vcx, |view, cx| {
            view.toggle_environment("env-linux", cx);
            view.data.prepare(&view.state, cx);
        });
        view.read_with(vcx, |view, cx| {
            assert_eq!(view.data.environment_filter_label(cx), "Mac Studio");
            assert!((view.data.merged.cost_usd - 12.5).abs() < 1e-6);
            assert_eq!(view.data.merged.total_tokens, 1_000);
        });

        // Unchecking all shows "0 environments" and clears merged totals.
        view.update(vcx, |view, cx| {
            view.set_all_environments(false, cx);
            view.data.prepare(&view.state, cx);
        });
        view.read_with(vcx, |view, cx| {
            assert_eq!(view.data.environment_filter_label(cx), "0 environments");
            assert_eq!(view.data.merged.total_tokens, 0);
        });

        // Stepped refresh spinner starts when refreshing=true, steps, and resets when refreshing=false.
        view.update(vcx, |view, cx| view.set_refreshing(true, cx));
        view.read_with(vcx, |view, cx| {
            assert_eq!(view.refresh_spinner.read(cx).state_for_test().0, true);
        });
        vcx.executor().advance_clock(std::time::Duration::from_millis(260));
        vcx.run_until_parked();
        view.read_with(vcx, |view, cx| {
            let (spinning, step) = view.refresh_spinner.read(cx).state_for_test();
            assert!(spinning);
            assert!(step >= 1, "spinner should advance steps while refreshing");
        });
        view.update(vcx, |view, cx| view.set_refreshing(false, cx));
        view.read_with(vcx, |view, cx| {
            assert_eq!(view.refresh_spinner.read(cx).state_for_test(), (false, 0));
        });

        state.update(vcx, |state, _| state.stop_pump());
        runtime.shutdown_background();
    }

    #[gpui::test]
    async fn multi_environment_price_overrides_loads_all_targets_guards_and_retries_partial_failure(
        cx: &mut gpui::TestAppContext,
    ) {
        use super::prices::TargetSaveStatus;
        use serde_json::Value;
        use t3_client::supervisor::{ClientFixture, ConnectionStatus};
        use t3_core::shell::ShellState;

        struct Host(Entity<UsageView>);
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _active_fixture) =
            cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.update(|cx| init(state.clone(), cx));

        let (connections, _output) = t3_client::environments::Environments::start(runtime.handle(), false);
        state.update(cx, |state, _| {
            state.stop_pump();
            state.connections = connections;
            state.environments.active = Some("env-mac".to_owned());
            state.environments.environments = serde_json::from_value(json!([
                {
                    "id": "env-mac", "environmentId": "env-mac-id", "label": "Mac Studio",
                    "httpBase": "http://mac.invalid", "accessToken": "", "disabled": false
                },
                {
                    "id": "env-linux", "environmentId": "env-linux-id", "label": "Linux Box",
                    "httpBase": "http://linux.invalid", "accessToken": "", "disabled": false
                }
            ]))
            .unwrap();
        });
        let mut fleet_rx = state.read_with(cx, |s, _| s.connections.view());
        let rw_grants: t3_core::auth::AuthSessionState =
            serde_json::from_value(json!({ "authenticated": true, "permissions": ["diagnostics:read", "settings:write"] })).unwrap();
        let ro_grants: t3_core::auth::AuthSessionState =
            serde_json::from_value(json!({ "authenticated": true, "permissions": ["diagnostics:read"] })).unwrap();
        let (client_a, mut fixture_a) = ClientFixture::handle(rw_grants.clone());
        let (client_b, mut fixture_b) = ClientFixture::handle(rw_grants.clone());
        state.update(cx, |state, cx| {
            state.install_active_environment(
                client_a.clone(),
                ShellState::default(),
                server_config_for("env-mac-id", "Mac Studio"),
                ConnectionStatus::Connected,
                cx,
            );
        });
        let install_a = state.update(cx, |state, _| {
            state.connections.install_fixture(
                "env-mac",
                client_a,
                ShellState::default(),
                server_config_for("env-mac-id", "Mac Studio"),
                ConnectionStatus::Connected,
            )
        });
        let install_b = state.update(cx, |state, _| {
            state.connections.install_fixture(
                "env-linux",
                client_b,
                ShellState::default(),
                server_config_for("env-linux-id", "Linux Box"),
                ConnectionStatus::Connected,
            )
        });
        let _events_a = runtime.spawn(install_a).await.unwrap();
        let _events_b = runtime.spawn(install_b).await.unwrap();
        let fleet = fleet_rx.borrow_and_update().clone();
        state.update(cx, |state, cx| state.apply_fleet_view(fleet, cx));

        let window = cx.add_window(|window, cx| Host(cx.new(|cx| UsageView::new(state.clone(), window, cx))));
        let view = window.read_with(cx, |host, _| host.0.clone()).unwrap();
        cx.run_until_parked();

        async fn next_matching(
            fixture: &mut ClientFixture,
            host: &str,
            model: &str,
            expected: &str,
        ) -> (String, Value, tokio::sync::oneshot::Sender<Result<Value, t3_client::session::RpcError>>) {
            loop {
                let (method, payload, reply) = fixture.next_request().await;
                if method == expected {
                    return (method.to_owned(), payload, reply);
                }
                let _ = reply.send(Ok(sample_summary(host, model, 1.0, 100)));
            }
        }

        // 1. Open Model prices dialog: MUST fetch server.getConfig from BOTH environments.
        let (loaded_tx, loaded_rx) = tokio::sync::oneshot::channel::<()>();
        let loaded_tx = std::rc::Rc::new(std::cell::RefCell::new(Some(loaded_tx)));
        let _load_obs = cx.update(|cx| {
            let loaded_tx = loaded_tx.clone();
            cx.observe(&view, move |view, cx| {
                if view.read(cx).prices.as_ref().is_some_and(|d| !d.loading)
                    && let Some(tx) = loaded_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        window.update(cx, |host, window, cx| host.0.update(cx, |v, cx| v.open_price_overrides(None, window, cx))).unwrap();
        cx.run_until_parked();

        let (req_a, req_b) = futures::future::join(
            next_matching(&mut fixture_a, "host-mac", "model-a", t3_core::usage::price_overrides::GET_CONFIG),
            next_matching(&mut fixture_b, "host-linux", "model-b", t3_core::usage::price_overrides::GET_CONFIG),
        )
        .await;
        assert_eq!(req_a.0, t3_core::usage::price_overrides::GET_CONFIG);
        assert_eq!(req_b.0, t3_core::usage::price_overrides::GET_CONFIG);
        // Mac has model-a (aliases supported); Linux has model-b (aliases NOT supported).
        let _ = req_a.2.send(Ok(json!({
            "environment": { "capabilities": { "usageModelAliases": true } },
            "settings": {
                "usagePriceOverrides": {
                    "model-a": { "inputCostPerMillionTokens": 3.0, "outputCostPerMillionTokens": 15.0 }
                },
                "usageModelAliases": {}
            }
        })));
        let _ = req_b.2.send(Ok(json!({
            "environment": { "capabilities": { "usageModelAliases": false } },
            "settings": {
                "usagePriceOverrides": {
                    "model-b": { "inputCostPerMillionTokens": 5.0, "outputCostPerMillionTokens": 25.0 }
                },
                "usageModelAliases": {}
            }
        })));
        loaded_rx.await.expect("both target configs loaded");
        cx.run_until_parked();

        // Both environments' models appear in the dialog (not just targets[0]).
        view.read_with(cx, |view, cx| {
            let dialog = view.prices.as_ref().unwrap();
            assert_eq!(dialog.targets.len(), 2);
            assert_eq!(dialog.row_models_for_test(cx), vec!["model-a".to_owned(), "model-b".to_owned()]);
        });

        // 2. Editing model-a alias: Mac saves, Linux fails with "Update server to map models" (no RPC sent to Linux)
        // and the dialog stays open!
        let (partial_tx, partial_rx) = tokio::sync::oneshot::channel::<()>();
        let partial_tx = std::rc::Rc::new(std::cell::RefCell::new(Some(partial_tx)));
        let _partial_obs = cx.update(|cx| {
            let partial_tx = partial_tx.clone();
            cx.observe(&view, move |view, cx| {
                if view.read(cx).prices.as_ref().is_some_and(|d| !d.saving && d.has_failed_destinations())
                    && let Some(tx) = partial_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        window
            .update(cx, |host, window, cx| {
                host.0.update(cx, |view, cx| {
                    view.prices.as_ref().unwrap().set_row_alias_for_test("model-a", "claude-sonnet-4", window, cx);
                    view.save_price_overrides(false, cx);
                });
            })
            .unwrap();
        cx.run_until_parked();

        let (method_a, payload_a, reply_a) = next_matching(
            &mut fixture_a,
            "host-mac",
            "model-a",
            t3_core::commands::method::SERVER_UPDATE_SETTINGS,
        )
        .await;
        assert_eq!(method_a, t3_core::commands::method::SERVER_UPDATE_SETTINGS);
        assert_eq!(payload_a["patch"]["usageModelAliases"]["model-a"], json!("claude-sonnet-4"));
        assert_eq!(payload_a["patch"]["usagePriceOverrides"]["model-a"], Value::Null);
        let _ = reply_a.send(Ok(json!({})));
        partial_rx.await.expect("partial failure settled");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            let dialog = view.prices.as_ref().expect("dialog must stay open on partial failure");
            assert_eq!(dialog.results.get("env-mac"), Some(&TargetSaveStatus::Saved));
            assert_eq!(
                dialog.results.get("env-linux"),
                Some(&TargetSaveStatus::Failed("Update server to map models".to_owned()))
            );
        });

        // 3. Edit model-b price: Linux lacks settings:write -> Linux fails "Read-only access" while Mac saves,
        // then granting settings:write and calling Retry failed targets ONLY Linux and closes dialog.
        fixture_b.grants.send(Some(ro_grants.clone())).unwrap();
        state.update(cx, |state, cx| {
            let fleet = std::sync::Arc::make_mut(&mut state.fleet);
            if let Some(env) = fleet.environments.iter_mut().find(|e| e.summary.owner.saved_id == "env-linux") {
                std::sync::Arc::make_mut(env).grants = Some(ro_grants.clone());
            }
            cx.emit(AppEvent::Shell);
        });
        cx.run_until_parked();

        let (ro_tx, ro_rx) = tokio::sync::oneshot::channel::<()>();
        let ro_tx = std::rc::Rc::new(std::cell::RefCell::new(Some(ro_tx)));
        let _ro_obs = cx.update(|cx| {
            let ro_tx = ro_tx.clone();
            cx.observe(&view, move |view, cx| {
                if view.read(cx).prices.as_ref().is_some_and(|d| {
                    !d.saving && d.results.get("env-linux") == Some(&TargetSaveStatus::Failed("Read-only access".to_owned()))
                }) && let Some(tx) = ro_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        window
            .update(cx, |host, window, cx| {
                host.0.update(cx, |view, cx| {
                    view.prices.as_ref().unwrap().set_row_alias_for_test("model-a", "", window, cx);
                    view.prices.as_ref().unwrap().set_row_price_for_test("model-a", "3", "15", window, cx);
                    view.prices.as_ref().unwrap().set_row_price_for_test("model-b", "6", "30", window, cx);
                    view.save_price_overrides(false, cx);
                });
            })
            .unwrap();
        cx.run_until_parked();

        let (method_a, payload_a, reply_a) = next_matching(
            &mut fixture_a,
            "host-mac",
            "model-a",
            t3_core::commands::method::SERVER_UPDATE_SETTINGS,
        )
        .await;
        assert_eq!(method_a, t3_core::commands::method::SERVER_UPDATE_SETTINGS);
        assert_eq!(payload_a["patch"]["usagePriceOverrides"]["model-b"]["inputCostPerMillionTokens"], json!(6.0));
        let _ = reply_a.send(Ok(json!({})));
        ro_rx.await.expect("read-only failure recorded");
        cx.run_until_parked();

        // Grant settings:write on Linux and retry ONLY failed destinations -> closes dialog on success!
        fixture_b.grants.send(Some(rw_grants.clone())).unwrap();
        state.update(cx, |state, cx| {
            let fleet = std::sync::Arc::make_mut(&mut state.fleet);
            if let Some(env) = fleet.environments.iter_mut().find(|e| e.summary.owner.saved_id == "env-linux") {
                std::sync::Arc::make_mut(env).grants = Some(rw_grants.clone());
            }
            cx.emit(AppEvent::Shell);
        });
        let _ = &mut fleet_rx;
        cx.run_until_parked();

        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();
        let closed_tx = std::rc::Rc::new(std::cell::RefCell::new(Some(closed_tx)));
        let _close_obs = cx.update(|cx| {
            let closed_tx = closed_tx.clone();
            cx.observe(&view, move |view, cx| {
                if view.read(cx).prices.is_none()
                    && let Some(tx) = closed_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        window.update(cx, |host, _, cx| host.0.update(cx, |view, cx| view.save_price_overrides(true, cx))).unwrap();
        cx.run_until_parked();

        let (method_b, payload_b, reply_b) = next_matching(
            &mut fixture_b,
            "host-linux",
            "model-b",
            t3_core::commands::method::SERVER_UPDATE_SETTINGS,
        )
        .await;
        assert_eq!(method_b, t3_core::commands::method::SERVER_UPDATE_SETTINGS);
        assert_eq!(payload_b["patch"]["usagePriceOverrides"]["model-b"]["inputCostPerMillionTokens"], json!(6.0));
        let _ = reply_b.send(Ok(json!({})));
        closed_rx.await.expect("dialog closes once retry succeeds");

        state.update(cx, |state, _| state.stop_pump());
        runtime.shutdown_background();
    }
}
