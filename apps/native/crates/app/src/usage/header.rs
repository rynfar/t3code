//! The Usage header (`UsagePage.tsx` topbar): a breadcrumb of Usage, the environment filter and
//! the provider filter, then the period and metric toggles and Refresh. Narrow pages swap the
//! toggles for compact menus, as the web's container query does.

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, anchored, deferred, div, prelude::*, px};
use gpui_component::h_flex;
use t3_core::usage::format::{format_usage_contract_mismatch, window_label};
use t3_core::usage::{self, UsageMetric, UsagePeriod, UsageProvider};

use super::controls::{brand_mark, check_item, icon, menu_note, menu_panel, menu_separator, menu_trigger, r, segmented, text_segment};
use super::{MenuKind, UsageView, close};
use crate::icons::Icon;
use crate::keymap;
use crate::right_panel::{Button, Size, Variant};
use crate::sidebar::controller::SystemZone;
use crate::theme::{hsla, palette};

const WIDE_HEADER: f32 = 896.;

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
        let refresh_label = if limits { "Refresh limits" } else { "Refresh usage" };
        let refresh_disabled = self.data.refreshing || (!limits && !self.data.access.can_read_diagnostics);
        let refresh = Button::new("usage-refresh")
            .variant(Variant::Ghost)
            .size(Size::IconSm)
            .icon(Icon::RefreshCw)
            .icon_size(14.)
            .tooltip(refresh_label)
            .disabled(refresh_disabled)
            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)));
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
                            .child(div().flex_none().text_color(hsla(p.muted_foreground)).child("Usage"))
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
                    .tooltip("Close (Esc)")
                    .on_click(|_, _, cx| close(cx)),
            )
    }

    fn render_period_toggles(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.data.preferences.period;
        segmented("usage-period", cx).children(UsagePeriod::ALL.map(|period| {
            let tooltip = shortcut_title(period.label(), period.command(), cx);
            text_segment(("usage-period", period.days() as usize), period.label(), period == current, Some(tooltip), window, cx)
                .on_click(cx.listener(move |this, _, _, cx| this.select_period(period, cx)))
        }))
    }

    fn render_metric_toggles(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.data.preferences.metric;
        segmented("usage-metric", cx).children(UsageMetric::ALL.iter().enumerate().map(|(index, metric)| {
            let metric = *metric;
            let tooltip = shortcut_title(metric.label(), metric.command(), cx);
            text_segment(("usage-metric", index), metric.label(), metric == current, Some(tooltip), window, cx)
                .on_click(cx.listener(move |this, _, _, cx| this.select_metric(metric, cx)))
        }))
    }

    /// The narrow header's ghost select for the period or metric.
    fn render_compact_menu(&mut self, kind: MenuKind, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let open = self.menu == Some(kind);
        let label = match kind {
            MenuKind::Period => self.data.preferences.period.label(),
            _ => self.data.preferences.metric.label(),
        };
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
                        check_item(("usage-period-item", period.days() as usize), checked, div().child(period.label()).into_any_element(), cx)
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
                        check_item(("usage-metric-item", index), metric == self.data.preferences.metric, div().child(metric.label()).into_any_element(), cx)
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
        let environment = self.data.environment(self.state.read(cx));
        let mismatch = self.data.merged.contract_mismatches.first().cloned();
        let has_issue = (self.data.selected && environment.error.is_some()) || mismatch.is_some();
        let label = if self.data.selected { "All environments".to_owned() } else { "0 environments".to_owned() };
        let leading = (show_status && has_issue).then(|| icon(Icon::CircleAlert, 14., hsla(p.warning_foreground)).into_any_element());
        let trigger = menu_trigger("usage-environment-filter", label, leading, open, window, cx)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(MenuKind::Environment, cx)));
        let popup = open.then(|| {
            let selected = self.data.selected;
            let status = usage::environment_status_label(&environment, self.data.refreshing);
            let mut coverage: Vec<String> = Vec::new();
            if selected && let Some(error) = &environment.error {
                coverage.push(format!("{}: {error}", environment.label));
            }
            if let Some(mismatch) = &mismatch {
                coverage.push(format_usage_contract_mismatch(&environment.label, mismatch));
            }
            if !self.data.merged.duplicate_sources.is_empty() {
                coverage.push(format!(
                    "Counted once across environments sharing a transcript directory: {}",
                    self.data.merged.duplicate_sources.join(", ")
                ));
            }
            let row = h_flex()
                .min_w_0()
                .gap(r(12.))
                .child(div().flex_1().min_w_0().truncate().child(environment.label.clone()))
                .when(show_status, |this| {
                    let color = if environment.error.is_some() { p.destructive } else { p.muted_foreground };
                    this.child(div().flex_none().text_size(r(12.)).text_color(hsla(color)).child(status))
                });
            let panel = menu_panel("usage-environment-menu", cx)
                .child(
                    check_item("usage-environment-all", selected, div().child("All environments").into_any_element(), cx)
                        .on_click(cx.listener(|this, _, _, cx| this.set_environment_selected(!this.data.selected, cx))),
                )
                .child(menu_separator(cx))
                .child(
                    check_item("usage-environment-this", selected, row.into_any_element(), cx)
                        .on_click(cx.listener(|this, _, _, cx| this.set_environment_selected(!this.data.selected, cx))),
                )
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
                .when(self.data.label.is_empty(), |this| this.child(menu_note("No environments connected.", cx)));
            popup(gpui::Anchor::TopLeft, panel)
        });
        div().relative().min_w_0().child(trigger).children(popup).into_any_element()
    }

    fn render_provider_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let open = self.menu == Some(MenuKind::Provider);
        let hidden = self.data.hidden().clone();
        let trigger = menu_trigger("usage-provider-filter", usage::provider_filter_label(&hidden), None, open, window, cx)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(MenuKind::Provider, cx)));
        let popup = open.then(|| {
            let all = hidden.is_empty();
            let panel = menu_panel("usage-provider-menu", cx)
                .child(check_item("usage-provider-all", all, div().child("All providers").into_any_element(), cx).on_click(cx.listener(move |this, _, _, cx| {
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

    fn set_hidden(&mut self, hidden: std::collections::BTreeSet<UsageProvider>, cx: &mut Context<Self>) {
        self.data.preferences.hidden_providers = hidden;
        self.data.save_preferences();
        self.data.invalidate_limits();
        cx.notify();
    }

    fn set_environment_selected(&mut self, selected: bool, cx: &mut Context<Self>) {
        self.data.selected = selected;
        self.data.invalidate_limits();
        if selected && self.data.summary.is_none() {
            self.load(false, cx);
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
