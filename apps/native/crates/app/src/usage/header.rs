//! The Usage header (`UsagePage.tsx` topbar): a breadcrumb of Usage, the environment filter and
//! the provider filter, then the period and metric toggles and Refresh (with an isolated stepped
//! spinner that repaints only itself while refreshing). Narrow pages swap the toggles for compact
//! menus, as the web's container query does.

use std::collections::BTreeSet;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Task, Transformation, Window, anchored,
    deferred, div, percentage, prelude::*, px,
};
use gpui_component::h_flex;
use t3_core::usage::format::{format_usage_contract_mismatch, window_label};
use t3_core::usage::{self, UsageMetric, UsagePeriod, UsageProvider};

use super::controls::{brand_mark, check_item, icon, menu_note, menu_panel, menu_separator, menu_trigger, r, segmented, text_segment};
use super::{MenuKind, UsageView, close};
use crate::icons::Icon;
use crate::keymap;
use crate::right_panel::{Button, Size, Variant, focusable, tooltip_view};
use crate::sidebar::controller::SystemZone;
use crate::theme::{hsla, palette};

const WIDE_HEADER: f32 = 896.;
const SPINNER_STEPS: u8 = 8;
const SPINNER_INTERVAL: Duration = Duration::from_millis(125);

/// Isolated stepped refresh indicator (`RefreshIcon`): repaints only itself at 8 Hz while a
/// refresh is in flight and runs no timers when idle.
pub(super) struct RefreshSpinner {
    spinning: bool,
    step: u8,
    _tick: Option<Task<()>>,
}

impl RefreshSpinner {
    pub(super) fn new() -> Self {
        Self { spinning: false, step: 0, _tick: None }
    }

    pub(super) fn set_spinning(&mut self, spinning: bool, cx: &mut Context<Self>) {
        if self.spinning == spinning {
            return;
        }
        self.spinning = spinning;
        if spinning {
            self.step = 0;
            self._tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(SPINNER_INTERVAL).await;
                    let keep_going = this.update(cx, |spinner, cx| {
                        if !spinner.spinning {
                            return false;
                        }
                        spinner.step = (spinner.step + 1) % SPINNER_STEPS;
                        cx.notify();
                        true
                    });
                    if !matches!(keep_going, Ok(true)) {
                        return;
                    }
                }
            }));
        } else {
            self._tick = None;
            self.step = 0;
        }
        cx.notify();
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn state_for_test(&self) -> (bool, u8) {
        (self.spinning, self.step)
    }
}

impl Render for RefreshSpinner {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let glyph = icon(Icon::RefreshCw, 14., hsla(p.foreground));
        if self.spinning {
            glyph.with_transformation(Transformation::rotate(percentage(f32::from(self.step) / f32::from(SPINNER_STEPS))))
        } else {
            glyph
        }
    }
}

fn shortcut_title(label: &str, command: &str, cx: &App) -> SharedString {
    keymap::label(command, cx).map_or_else(|| label.to_owned(), |shortcut| format!("{label} ({shortcut})")).into()
}

impl UsageView {
    fn toggle_menu(&mut self, menu: MenuKind, cx: &mut Context<Self>) {
        self.menu = if self.menu == Some(menu) { None } else { Some(menu) };
        self.segment = None;
        cx.notify();
    }

    /// The modal's inner width, which the web's container queries read.
    pub(super) fn page_width(window: &Window) -> f32 {
        let viewport = f32::from(window.viewport_size().width);
        let inset = if viewport >= 1024. { 40. } else { 24. };
        (viewport - inset * 2.).min(1280.)
    }

    pub(super) fn render_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let width = Self::page_width(window);
        let limits = self.data.preferences.metric == UsageMetric::Limits;
        let separator = || div().flex_none().text_color(hsla(p.icon_muted)).child("/");
        let refresh_label: SharedString = crate::i18n::t(cx, if limits { "Refresh limits" } else { "Refresh usage" }).into();
        let refresh_disabled = self.data.refreshing || (!limits && !self.data.any_can_read_diagnostics());
        let refresh = focusable(div().id("usage-refresh"), "usage-refresh".into(), !refresh_disabled, window, cx)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(28.))
            .rounded(px(8.))
            .text_color(hsla(p.foreground))
            .when(refresh_disabled, |this| this.opacity(0.64))
            .when(!refresh_disabled, |this| {
                this.cursor_pointer()
                    .hover(move |style| style.bg(hsla(p.accent)))
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
            })
            .tooltip(move |window, cx| tooltip_view(refresh_label.clone(), window, cx))
            .child(self.refresh_spinner.clone());
        let controls = if width >= WIDE_HEADER {
            h_flex()
                .flex_none()
                .gap(r(8.))
                .when(!limits, |this| this.child(self.render_period_toggles(window, cx)))
                .child(self.render_metric_toggles(window, cx))
                .child(refresh)
                .into_any_element()
        } else {
            h_flex()
                .flex_none()
                .gap(r(4.))
                .when(!limits, |this| this.child(self.render_compact_menu(MenuKind::Period, window, cx)))
                .child(self.render_compact_menu(MenuKind::Metric, window, cx))
                .child(refresh)
                .into_any_element()
        };
        let label = (!limits && width >= 1280.).then(|| window_label(&self.data.window, &SystemZone, self.data.hour12));
        h_flex()
            .min_h(r(48.))
            .flex_none()
            .items_center()
            .gap(r(4.))
            .pl(r(20.))
            .pr(r(8.))
            .border_b_1()
            .border_color(hsla(p.border))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(r(12.))
                    .py(r(8.))
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap(r(12.))
                            .text_size(r(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(div().flex_none().text_color(hsla(p.muted_foreground)).child(crate::i18n::t(cx, "Usage")))
                            .child(separator())
                            .child(div().min_w(r(40.)).text_color(hsla(p.muted_foreground)).child(self.render_environment_filter(!limits, window, cx)))
                            .child(separator())
                            .child(div().min_w(r(40.)).text_color(hsla(p.foreground)).child(self.render_provider_filter(window, cx))),
                    )
                    .children(label.map(|label| div().min_w_0().truncate().text_size(r(12.)).text_color(hsla(p.muted_foreground)).child(label)))
                    .child(div().flex_1())
                    .child(controls),
            )
            .child(
                Button::new("usage-close")
                    .variant(Variant::Ghost)
                    .size(Size::IconSm)
                    .icon(Icon::X)
                    .tooltip(crate::i18n::t(cx, "Close (Esc)"))
                    .on_click(|_, _, cx| close(cx)),
            )
    }

    fn render_period_toggles(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.data.preferences.period;
        segmented("usage-period", cx).children(UsagePeriod::ALL.map(|period| {
            let localized = crate::i18n::t(cx, period.label());
            let tooltip = shortcut_title(localized, period.command(), cx);
            text_segment(("usage-period", period.days() as usize), localized, period == current, Some(tooltip), window, cx)
                .on_click(cx.listener(move |this, _, _, cx| this.select_period(period, cx)))
        }))
    }

    fn render_metric_toggles(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.data.preferences.metric;
        segmented("usage-metric", cx).children(UsageMetric::ALL.iter().enumerate().map(|(index, metric)| {
            let metric = *metric;
            let localized = crate::i18n::t(cx, metric.label());
            let tooltip = shortcut_title(localized, metric.command(), cx);
            text_segment(("usage-metric", index), localized, metric == current, Some(tooltip), window, cx)
                .on_click(cx.listener(move |this, _, _, cx| this.select_metric(metric, cx)))
        }))
    }

    /// The narrow header's ghost select for the period or metric.
    fn render_compact_menu(&mut self, kind: MenuKind, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let open = self.menu == Some(kind);
        let label = crate::i18n::t(
            cx,
            match kind {
                MenuKind::Period => self.data.preferences.period.label(),
                _ => self.data.preferences.metric.label(),
            },
        );
        let trigger = menu_trigger(if kind == MenuKind::Period { "usage-period-menu" } else { "usage-metric-menu" }, label, None, open, window, cx)
            .h(r(28.))
            .px(r(9.))
            .text_size(r(12.))
            .text_color(hsla(p.secondary_label))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_menu(kind, cx)));
        let popup = open.then(|| {
            let items: Vec<AnyElement> = match kind {
                MenuKind::Period => UsagePeriod::ALL
                    .map(|period| {
                        let checked = period == self.data.preferences.period;
                        check_item(("usage-period-item", period.days() as usize), checked, div().child(crate::i18n::t(cx, period.label())).into_any_element(), cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.select_period(period, cx)))
                            .into_any_element()
                    })
                    .into_iter()
                    .collect(),
                _ => UsageMetric::ALL
                    .iter()
                    .enumerate()
                    .map(|(index, metric)| {
                        let metric = *metric;
                        check_item(("usage-metric-item", index), metric == self.data.preferences.metric, div().child(crate::i18n::t(cx, metric.label())).into_any_element(), cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.select_metric(metric, cx)))
                            .into_any_element()
                    })
                    .collect(),
            };
            popup(gpui::Anchor::TopRight, menu_panel("usage-compact-menu", cx).min_w(r(160.)).children(items))
        });
        div().relative().flex_none().child(trigger).children(popup).into_any_element()
    }

    fn render_environment_filter(&mut self, show_status: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let open = self.menu == Some(MenuKind::Environment);
        let selected_usages = self.data.selected_environment_usages();
        let has_issue = selected_usages.iter().any(|env| env.error.is_some()) || !self.data.merged.contract_mismatches.is_empty();
        let label = self.data.environment_filter_label(cx);
        let leading = (show_status && has_issue).then(|| icon(Icon::CircleAlert, 14., hsla(p.warning_foreground)).into_any_element());
        let trigger = menu_trigger("usage-environment-filter", label, leading, open, window, cx)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(MenuKind::Environment, cx)));
        let popup = open.then(|| {
            let all_selected = self.data.all_selected();
            let refreshing_usage = self.data.refreshing && show_status;
            let is_partial = self.data.is_partial();
            let env_rows: Vec<AnyElement> = self
                .data
                .envs
                .iter()
                .enumerate()
                .map(|(index, env)| {
                    let checked = self.data.is_env_selected(&env.saved_id);
                    let usage_env = env.to_environment_usage();
                    let status = usage::environment_status_label(&usage_env, refreshing_usage);
                    let saved_id = env.saved_id.clone();
                    let row = h_flex()
                        .min_w_0()
                        .gap(r(12.))
                        .child(div().flex_1().min_w_0().truncate().child(env.label.clone()))
                        .when(show_status, |this| {
                            let color = if usage_env.error.is_some() { p.destructive } else { p.muted_foreground };
                            this.child(div().flex_none().text_size(r(12.)).text_color(hsla(color)).child(status))
                        });
                    check_item(("usage-environment-item", index), checked, row.into_any_element(), cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_environment(&saved_id, cx)))
                        .into_any_element()
                })
                .collect();

            let mut coverage: Vec<String> = Vec::new();
            for env in &selected_usages {
                if let Some(error) = &env.error {
                    coverage.push(format!("{}: {error}", env.label));
                }
            }
            for mismatch in &self.data.merged.contract_mismatches {
                let env_label = selected_usages
                    .iter()
                    .find(|e| e.environment_id == mismatch.environment_id)
                    .map(|e| e.label.as_str())
                    .unwrap_or(&mismatch.environment_id);
                coverage.push(format_usage_contract_mismatch(env_label, mismatch));
            }
            if !self.data.merged.duplicate_sources.is_empty() {
                coverage.push(format!(
                    "Counted once across environments sharing a transcript directory: {}",
                    self.data.merged.duplicate_sources.join(", ")
                ));
            }
            let empty_envs = env_rows.is_empty();
            let panel = menu_panel("usage-environment-menu", cx)
                .child(
                    check_item("usage-environment-all", all_selected, div().child(crate::i18n::t(cx, "All environments")).into_any_element(), cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_all_environments(!all_selected, cx))),
                )
                .child(menu_separator(cx))
                .children(env_rows)
                .when(empty_envs, |this| this.child(menu_note(crate::i18n::t(cx, "No environments connected."), cx)))
                .when(show_status && is_partial, |this| {
                    this.child(menu_note(crate::i18n::t(cx, "Totals are partial while selected environments scan."), cx))
                })
                .when(show_status && !coverage.is_empty(), |this| {
                    this.child(
                        gpui_component::v_flex()
                            .gap(r(4.))
                            .mt(r(4.))
                            .px(r(8.))
                            .py(r(8.))
                            .border_t_1()
                            .border_color(hsla(p.border))
                            .text_size(r(12.))
                            .text_color(hsla(p.muted_foreground))
                            .children(coverage.into_iter().map(|line| div().child(line))),
                    )
                })
                .when(show_status && self.data.any_connected(), |this| {
                    this.child(menu_separator(cx)).child(
                        div()
                            .id("usage-environment-model-prices")
                            .flex()
                            .items_center()
                            .gap(r(8.))
                            .min_h(r(30.))
                            .px(r(8.))
                            .rounded(r(6.))
                            .text_size(r(13.))
                            .text_color(hsla(p.foreground))
                            .cursor_pointer()
                            .hover(|style| style.bg(hsla(p.accent)))
                            .on_click(cx.listener(|this, _, window, cx| this.open_price_overrides(None, window, cx)))
                            .child(crate::i18n::t(cx, "Model prices")),
                    )
                });
            popup(gpui::Anchor::TopLeft, panel)
        });
        div().relative().min_w_0().child(trigger).children(popup).into_any_element()
    }

    fn render_provider_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let open = self.menu == Some(MenuKind::Provider);
        let hidden = self.data.hidden().clone();
        let visible: Vec<UsageProvider> = UsageProvider::ORDER.into_iter().filter(|provider| !hidden.contains(provider)).collect();
        let filter_label = if visible.len() == UsageProvider::ORDER.len() {
            crate::i18n::t(cx, "All providers").to_owned()
        } else if visible.is_empty() {
            crate::i18n::t(cx, "No providers").to_owned()
        } else if visible.len() == 1 {
            visible[0].label().to_owned()
        } else {
            crate::i18n::t_fmt(
                cx,
                "{count, plural, one {{count} providers} other {{count} providers}}",
                &[("count", visible.len().into())],
            )
        };
        let trigger = menu_trigger("usage-provider-filter", filter_label, None, open, window, cx)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(MenuKind::Provider, cx)));
        let popup = open.then(|| {
            let all = hidden.is_empty();
            let panel = menu_panel("usage-provider-menu", cx)
                .child(check_item("usage-provider-all", all, div().child(crate::i18n::t(cx, "All providers")).into_any_element(), cx).on_click(cx.listener(move |this, _, _, cx| {
                    let next = if all { UsageProvider::ORDER.into_iter().collect() } else { Default::default() };
                    this.set_hidden(next, cx);
                })))
                .child(menu_separator(cx))
                .children(UsageProvider::ORDER.map(|provider| {
                    let visible = !hidden.contains(&provider);
                    let label = h_flex().min_w_0().gap(r(8.)).child(brand_mark(provider.driver(), 14., hsla(p.foreground), cx)).child(div().truncate().child(provider.label()));
                    check_item(("usage-provider", provider.index()), visible, label.into_any_element(), cx).on_click(cx.listener(move |this, _, _, cx| {
                        let mut next = this.data.hidden().clone();
                        if !next.remove(&provider) {
                            next.insert(provider);
                        }
                        this.set_hidden(next, cx);
                    }))
                }));
            popup(gpui::Anchor::TopLeft, panel)
        });
        div().relative().min_w_0().child(trigger).children(popup).into_any_element()
    }

    fn set_hidden(&mut self, hidden: BTreeSet<UsageProvider>, cx: &mut Context<Self>) {
        self.data.preferences.hidden_providers = hidden;
        self.data.save_preferences();
        self.data.invalidate_limits();
        cx.notify();
    }

    pub(super) fn set_all_environments(&mut self, select_all: bool, cx: &mut Context<Self>) {
        self.data.selected_ids = if select_all { None } else { Some(BTreeSet::new()) };
        self.data.invalidate_limits();
        if select_all {
            self.load(false, cx);
            self.auto_refresh_limits(cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_environment(&mut self, saved_id: &str, cx: &mut Context<Self>) {
        let mut next: BTreeSet<String> = self
            .data
            .selected_envs()
            .into_iter()
            .map(|env| env.saved_id.clone())
            .collect();
        let added = if next.remove(saved_id) {
            false
        } else {
            next.insert(saved_id.to_owned());
            true
        };
        let all_known = !self.data.envs.is_empty() && next.len() == self.data.envs.len();
        self.data.selected_ids = if all_known { None } else { Some(next) };
        self.data.invalidate_limits();
        if added {
            self.load_env(saved_id.to_owned(), false, cx);
            self.auto_refresh_limits(cx);
        }
        cx.notify();
    }
}

fn popup(anchor: gpui::Anchor, panel: gpui::Stateful<gpui::Div>) -> impl IntoElement {
    let right = anchor == gpui::Anchor::TopRight;
    div()
        .absolute()
        .bottom_0()
        .when(right, |this| this.right_0())
        .when(!right, |this| this.left_0())
        .child(
            deferred(anchored().anchor(anchor).snap_to_window_with_margin(px(8.)).child(
                panel.mt(r(4.)).on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()),
            ))
            .with_priority(3),
        )
}
