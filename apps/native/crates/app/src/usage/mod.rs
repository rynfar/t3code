//! Usage: a modal over the workspace (`web/src/routes/_chat.usage.tsx`, `UsagePage.tsx`) with
//! cost, tokens and subscription limits (keys C, T, L), a period (`cmd-shift-1..4`), and
//! environment and provider filters. Opened with `cmd-u`, the palette, or the sidebar footer;
//! Esc closes it. The view exists only while open. Summaries load on the network runtime and
//! stay cached per window while it is open; nothing repaints until an answer or input arrives.

mod chart;
mod controls;
mod data;
mod dialog;
mod header;
mod limits;
pub mod meter;
mod overview;

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
    /// When Limits may ask providers again on its own (`refreshUsageLimits`, five minutes).
    limits_refresh_after: i64,
}

impl Global for Host {}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    cx.set_global(Host { state, view: None, pending: false, limits_refresh_after: 0 });
    cx.on_action(|_: &OpenUsage, cx| open(cx));
    cx.bind_keys([KeyBinding::new("escape", CloseUsage, Some("UsageModal"))]);
}

pub fn open(cx: &mut App) {
    cx.global_mut::<Host>().pending = true;
    cx.refresh_windows();
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
    data: UsageData,
    menu: Option<MenuKind>,
    breakdown: Breakdown,
    hover: Option<usize>,
    selected_model: Option<String>,
    dialog_hover: Option<usize>,
    segment: Option<SegmentKey>,
    confirm_reset: Option<Redeem>,
    /// The account a reset credit is being used for, and what came of it.
    reset_status: Option<(String, Option<String>)>,
    _subscriptions: Vec<Subscription>,
    _grants: Option<Task<()>>,
}

impl Focusable for UsageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl UsageView {
    fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
            AppEvent::ServerConfig => {
                this.data.config_changed();
                this.auto_refresh_limits(cx);
                cx.notify();
            }
            AppEvent::Phase => {
                this.sync_access(cx);
                cx.notify();
            }
            _ => {}
        })];
        let mut view = UsageView {
            state,
            focus: cx.focus_handle(),
            data: UsageData::new(),
            menu: None,
            breakdown: Breakdown::Model,
            hover: None,
            selected_model: None,
            dialog_hover: None,
            segment: None,
            confirm_reset: None,
            reset_status: None,
            _subscriptions: subscriptions,
            _grants: None,
        };
        view.watch_grants(cx);
        view.sync_access(cx);
        view.auto_refresh_limits(cx);
        view
    }

    /// Follows this device's grants, which decide whether usage is readable at all.
    fn watch_grants(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.state.read(cx).client_handle() else { return };
        let mut grants = client.session_state();
        self.data.session = grants.borrow_and_update().clone();
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
        if self.data.session != session {
            self.data.session = session;
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
        self.selected_model.is_some() || self.menu.is_some() || self.confirm_reset.is_some()
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

    /// Esc peels one layer: the confirm, a menu or popover, the model dialog, then the page.
    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.confirm_reset.take().is_some() || self.menu.take().is_some() || self.segment.take().is_some() || self.selected_model.take().is_some() {
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
            .children(self.render_reset_confirm(window, cx))
            .text_color(hsla(p.foreground))
    }
}
