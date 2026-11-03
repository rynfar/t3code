//! Cost and Tokens (`UsageOverview` in `UsagePage.tsx`): the headline and per-provider summary,
//! the provider chart, totals, share bars by type and speed, and the breakdown by model or by
//! period. Figures still loading fade to 40% (`figureClass`); nothing animates.

use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px, relative};
use gpui_component::{h_flex, v_flex};
use t3_core::usage::format::{
    format_count, format_day_short, format_hour_short, format_percent, format_relative_hour_short, format_tokens, format_usd, usage_source_messages,
    window_periods,
};
use t3_core::usage::merge::{self, ChartMetric, Ink, MergedUsage, ShareSegment};
use t3_core::usage::{self, LoadingState, UsageMetric, UsageProvider, UsageResolution, UsageSummaryInput};

use super::chart::{ChartData, provider_chart};
use super::controls::{
    ActionButton, ButtonSize, ButtonVariant, LOADING_OPACITY, brand_mark, dot, heading, icon, provider_color, r, segmented, skeleton, text_segment,
};
use super::{Breakdown, UsageView};
use crate::icons::Icon;
use crate::right_panel::{focusable, tooltip_view};
use crate::sidebar::controller::SystemZone;
use crate::theme::{Palette, hsla, palette};

const CURSOR_KEYCHAIN_COPY: &str = "Requires access to your Cursor login in macOS Keychain.";

fn fade<E: Styled>(element: E, loading: bool) -> E {
    if loading { element.opacity(LOADING_OPACITY) } else { element }
}

pub(super) fn ink(ink: Ink, p: Palette) -> Hsla {
    hsla(match ink {
        Ink::P30 => p.usage_ink_30,
        Ink::P34 => p.usage_ink_34,
        Ink::P44 => p.usage_ink_44,
        Ink::P60 => p.usage_ink_60,
        Ink::P66 => p.usage_ink_66,
        Ink::P72 => p.usage_ink_72,
        Ink::P100 => p.usage_ink_100,
    })
}

fn chart_metric(metric: UsageMetric) -> ChartMetric {
    if metric == UsageMetric::Tokens { ChartMetric::Tokens } else { ChartMetric::Cost }
}

fn value_text(metric: ChartMetric, value: f64) -> String {
    super::chart::format_value(metric, value)
}

/// The chart's data for a window: hourly windows label hours, tooltips relative to the window end.
pub(super) fn chart_data(
    window: &UsageSummaryInput,
    merged: &MergedUsage,
    metric: ChartMetric,
    providers: Vec<UsageProvider>,
    loading: BTreeSet<UsageProvider>,
    hour12: bool,
) -> ChartData {
    let hourly = window.resolution == UsageResolution::Hour;
    let periods = window_periods(window);
    let reference = window.until_time.clone();
    let label = |period: &str| if hourly { format_hour_short(period, &SystemZone, hour12) } else { format_day_short(period) };
    let tooltip = |period: &str| match (&reference, hourly) {
        (Some(reference), true) => format_relative_hour_short(period, reference, &SystemZone, hour12),
        _ => label(period),
    };
    let totals = if hourly { &merged.hourly } else { &merged.daily };
    ChartData::new(&periods, totals, metric, providers, loading, &|period| (label(period), tooltip(period)))
}

/// `UsageShareBar`: proportional segments with a legend.
pub(super) fn share_bar(id: &'static str, label: &str, segments: &[ShareSegment], format: fn(f64) -> String, aside: Option<AnyElement>, cx: &App) -> Option<AnyElement> {
    let p = palette(cx);
    let (visible, total) = merge::visible_segments(segments);
    if total <= 0. {
        return None;
    }
    let count = visible.len();
    Some(
        v_flex()
            .min_w_0()
            .gap(r(10.))
            .child(h_flex().items_baseline().justify_between().gap(r(12.)).child(heading(label.to_owned(), cx)).children(aside))
            .child(h_flex().h(r(8.)).gap(px(2.)).children(visible.iter().enumerate().map(|(index, segment)| {
                let tip: SharedString = format!("{} · {} · {}", segment.label, format(segment.value), format_percent(segment.value / total)).into();
                let mut bar = div()
                    .id((id, index))
                    .h_full()
                    .min_w(r(4.))
                    .flex_basis(relative((segment.value / total) as f32))
                    .flex_shrink(1.)
                    .bg(ink(segment.ink, p))
                    .rounded(px(2.))
                    .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx));
                if index == 0 {
                    bar = bar.rounded_l_full();
                }
                if index + 1 == count {
                    bar = bar.rounded_r_full();
                }
                bar
            })))
            .child(h_flex().flex_wrap().gap_x(r(16.)).gap_y(r(4.)).text_size(r(12.)).children(visible.iter().map(|segment| {
                h_flex()
                    .gap(r(6.))
                    .child(div().size(r(8.)).rounded(px(2.)).bg(ink(segment.ink, p)))
                    .child(div().text_color(hsla(p.muted_foreground)).child(segment.label))
                    .child(div().text_color(hsla(p.foreground)).child(format(segment.value)))
            })))
            .into_any_element(),
    )
}

/// `SpeedPremium`.
pub(super) fn speed_premium(premium: f64, cx: &App) -> AnyElement {
    let p = palette(cx);
    h_flex()
        .gap(r(4.))
        .text_size(r(12.))
        .text_color(hsla(p.muted_foreground))
        .child("Premium")
        .child(div().text_color(hsla(p.foreground)).child(format_usd(premium)))
        .into_any_element()
}

fn metric_cell(label: &'static str, value: String, loading: bool, cx: &App) -> AnyElement {
    let p = palette(cx);
    v_flex()
        .min_w_0()
        .gap(r(2.))
        .child(div().text_size(r(12.)).line_height(r(16.)).text_color(hsla(p.muted_foreground)).child(label))
        .child(fade(div().text_size(r(16.)).line_height(r(24.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(value), loading))
        .into_any_element()
}

const TOTAL_LABELS: [&str; 5] = ["Processed tokens", "Cached input", "Uncached input", "Output", "Cache savings"];

impl UsageView {
    pub(super) fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let limits = self.data.preferences.metric == UsageMetric::Limits;
        let noun = if limits { "limits" } else { "usage" };
        let content: AnyElement = if !self.data.selected {
            div().text_size(r(14.)).text_color(hsla(p.muted_foreground)).child(format!("Select an environment to see {noun}.")).into_any_element()
        } else if limits {
            self.render_limits(window, cx).into_any_element()
        } else {
            match self.data.shown() {
                None => self.render_skeleton(cx).into_any_element(),
                Some(_) if !self.data.access.can_read_diagnostics => {
                    let error = self.data.environment(self.state.read(cx)).error.unwrap_or_default();
                    v_flex().py(r(48.)).items_center().text_size(r(14.)).text_color(hsla(p.muted_foreground)).child(error).into_any_element()
                }
                Some((window_shown, merged, kept)) => self.render_overview(window_shown, merged, kept, window, cx).into_any_element(),
            }
        };
        v_flex().w_full().max_w(r(1024.)).mx_auto().gap(r(24.)).px(r(24.)).pt(r(24.)).pb(r(48.)).child(content)
    }

    fn render_overview(&mut self, shown: UsageSummaryInput, merged: Arc<MergedUsage>, kept: bool, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let metric = chart_metric(self.data.preferences.metric);
        let hourly = shown.resolution == UsageResolution::Hour;
        let environment = self.data.environment(self.state.read(cx));
        let loading: LoadingState = usage::loading_state(std::slice::from_ref(&environment), self.data.refreshing, kept, self.data.hidden());
        let with_data = usage::providers_with_usage(&merged.providers);
        let active: Vec<UsageProvider> =
            UsageProvider::ORDER.into_iter().filter(|provider| with_data.contains(provider) || loading.providers.contains(provider)).collect();
        let chart_loading: BTreeSet<UsageProvider> = active.iter().copied().filter(|provider| loading.is_loading(*provider)).collect();
        let hidden = self.data.hidden();
        let messages = usage_source_messages(
            environment.summary.iter().flat_map(|summary| summary.sources.iter()).filter(|source| !hidden.contains(&source.fingerprint.provider)),
        );
        let wide = f32::from(window.viewport_size().width) >= 1024.;
        let chart = Rc::new(chart_data(&shown, &merged, metric, active.clone(), chart_loading, self.data.hour12));
        let entity = cx.entity();
        let chart_view = provider_chart(
            "usage-chart",
            chart,
            self.hover,
            move |hover, _, cx| {
                entity.update(cx, |view, cx| {
                    view.hover = hover;
                    cx.notify();
                })
            },
            cx,
        );
        let summary = self.render_summary(&merged, metric, &loading, &active, &environment, cx);
        let chart_section = v_flex()
            .min_w_0()
            .flex_1()
            .gap(r(12.))
            .child(heading(format!("{} {}", if hourly { "Hourly" } else { "Daily" }, if metric == ChartMetric::Tokens { "processed tokens" } else { "cost" }), cx))
            .child(chart_view);
        let top = if wide {
            h_flex().items_start().gap(r(24.)).child(div().w(r(288.)).flex_none().child(summary)).child(chart_section).into_any_element()
        } else {
            v_flex().gap(r(24.)).child(summary).child(chart_section).into_any_element()
        };
        let totals = [
            format_tokens(merged.total_tokens as f64),
            format_tokens(merged.cached_input_tokens as f64),
            format_tokens(merged.uncached_input_tokens as f64),
            format_tokens(merged.output_tokens as f64),
            format_usd(merged.cost_quality.cache_savings_usd),
        ];
        let columns = if f32::from(window.viewport_size().width) >= 768. { 5 } else { 2 };
        let shares: Vec<AnyElement> = if merged.total_tokens == 0 {
            Vec::new()
        } else if metric == ChartMetric::Tokens {
            share_bar("usage-token-types", "Tokens by type", &merge::token_type_segments(&merged.token_totals()), format_tokens, None, cx)
                .into_iter()
                .collect()
        } else {
            let mut bars: Vec<AnyElement> = share_bar("usage-cost-types", "Cost by type", &merge::cost_type_segments(&merged.category_cost), format_usd, None, cx).into_iter().collect();
            if merged.speed_cost.fast + merged.speed_cost.ultrafast > 0. {
                bars.extend(share_bar(
                    "usage-cost-speed",
                    "Cost by speed",
                    &merge::speed_cost_segments(&merged.speed_cost),
                    format_usd,
                    Some(speed_premium(merged.speed_cost.premium, cx)),
                    cx,
                ));
            }
            bars
        };
        v_flex()
            .gap(r(24.))
            .children(messages.into_iter().map(|message| div().mb(r(16.)).text_size(r(14.)).text_color(hsla(p.muted_foreground)).child(message)))
            .child(top)
            .child(
                v_flex()
                    .gap(r(8.))
                    .child(heading("Totals", cx))
                    .child(
                        div().grid().grid_cols(columns).gap_x(r(24.)).gap_y(r(16.)).py(r(4.)).children(
                            TOTAL_LABELS.iter().zip(totals).map(|(label, value)| metric_cell(label, value, loading.partial, cx)),
                        ),
                    ),
            )
            .when(!shares.is_empty(), |this| {
                this.child(fade(div().grid().grid_cols(if wide { 2 } else { 1 }).gap_x(r(48.)).gap_y(r(32.)).children(shares), loading.partial))
            })
            .child(self.render_breakdown(&shown, &merged, metric, &active, &loading, window, cx))
    }

    fn render_summary(
        &mut self,
        merged: &MergedUsage,
        metric: ChartMetric,
        loading: &LoadingState,
        active: &[UsageProvider],
        environment: &usage::EnvironmentUsage,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let headline = if metric == ChartMetric::Cost { format_usd(merged.cost_usd) } else { format_tokens(merged.total_tokens as f64) };
        let unpriced = merged.cost_quality.unpriced_share;
        let with_data = usage::providers_with_usage(&merged.providers);
        let show_cursor_prompt = !self.data.hidden().contains(&UsageProvider::Cursor)
            && !usage::cursor_keychain_prompt(&[(environment.summary.as_deref(), environment.needs_cursor_keychain_access)]).is_empty();
        let insert_at = active.iter().position(|provider| *provider == UsageProvider::Claude).max(active.iter().position(|provider| *provider == UsageProvider::Codex)).map_or(0, |index| index + 1);
        let state = self.state.read(cx);
        let chatgpt = !self.data.hidden().contains(&UsageProvider::Codex)
            && state.server_config.config.as_ref().is_some_and(|config| config.providers.iter().any(t3_core::usage::limits::uses_chatgpt_sharing));
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, provider) in active.iter().enumerate() {
            if index == insert_at && show_cursor_prompt {
                rows.push(self.render_cursor_row(cx));
            }
            rows.push(self.render_provider_row(merged, *provider, metric, loading.is_loading(*provider), !with_data.contains(provider), cx));
        }
        if insert_at >= active.len() && show_cursor_prompt {
            rows.push(self.render_cursor_row(cx));
        }
        v_flex()
            .min_w_0()
            .gap(r(20.))
            .child(
                v_flex()
                    .gap(r(4.))
                    .child(fade(div().text_size(r(36.)).line_height(r(40.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(headline), loading.partial))
                    .child(
                        h_flex()
                            .gap(r(4.))
                            .text_size(r(12.))
                            .text_color(hsla(p.muted_foreground))
                            .child(fade(div().child(format!("{} sessions", format_count(merged.sessions as f64))), loading.partial))
                            .when(metric == ChartMetric::Cost, |this| this.child("· API estimate"))
                            .when(metric == ChartMetric::Cost && unpriced > 0., |this| {
                                let tip: SharedString = format!("API estimate excludes {} unpriced records.", format_percent(unpriced)).into();
                                this.child(
                                    div()
                                        .id("usage-unpriced")
                                        .child(icon(Icon::Info, 12., hsla(p.muted_foreground)))
                                        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx)),
                                )
                            }),
                    ),
            )
            .when(chatgpt, |this| {
                this.child(
                    h_flex()
                        .justify_between()
                        .gap(r(8.))
                        .text_size(r(12.))
                        .text_color(hsla(p.muted_foreground))
                        .child("ChatGPT shared usage")
                        .child(
                            ActionButton::new("usage-chatgpt", "Manage usage", ButtonVariant::GhostMuted, ButtonSize::Xs)
                                .trailing(Icon::ExternalLink)
                                .on_click(|_, _, cx| cx.open_url(t3_core::usage::limits::CHATGPT_USAGE_URL)),
                        ),
                )
            })
            .children(rows)
            .into_any_element()
    }

    fn render_provider_row(&self, merged: &MergedUsage, provider: UsageProvider, metric: ChartMetric, loading: bool, awaiting: bool, cx: &App) -> AnyElement {
        let p = palette(cx);
        let totals = merged.provider(provider);
        let cost = totals.map_or(0., |totals| totals.cost_usd);
        let tokens = totals.map_or(0, |totals| totals.total_tokens) as f64;
        let sessions = totals.map_or(0, |totals| totals.sessions);
        let share = totals.map_or(0., |totals| if metric == ChartMetric::Cost { totals.cost_share } else { totals.token_share });
        let session_label = format!("{} {}", format_count(sessions as f64), if sessions == 1 { "session" } else { "sessions" });
        let figure = if awaiting { "-".to_owned() } else { value_text(metric, if metric == ChartMetric::Cost { cost } else { tokens }) };
        let detail = if metric == ChartMetric::Cost {
            format!("{} of cost · {} tokens", format_percent(share), format_tokens(tokens))
        } else {
            format!("{} of tokens · {}", format_percent(share), format_usd(cost))
        };
        v_flex()
            .gap(r(4.))
            .child(
                h_flex()
                    .items_baseline()
                    .justify_between()
                    .gap(r(16.))
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap(r(8.))
                            .text_size(r(14.))
                            .text_color(hsla(p.foreground))
                            .child(dot(provider_color(provider, p)))
                            .child(brand_mark(provider.driver(), 16., hsla(p.foreground), cx))
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .items_baseline()
                                    .gap(r(6.))
                                    .child(div().truncate().child(provider.label()))
                                    .child(fade(
                                        div().flex_none().text_size(r(11.)).text_color(hsla(p.muted_foreground)).when(awaiting, |this| this.invisible()).child(session_label),
                                        loading,
                                    )),
                            ),
                    )
                    .child(fade(div().flex_none().text_size(r(14.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(figure), loading)),
            )
            .child(fade(div().text_size(r(12.)).text_color(hsla(p.muted_foreground)).when(awaiting, |this| this.invisible()).child(detail), loading))
            .into_any_element()
    }

    /// `CursorEnableRow`.
    fn render_cursor_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let pending = self.data.keychain_pending;
        h_flex()
            .min_w_0()
            .items_baseline()
            .justify_between()
            .gap(r(16.))
            .text_size(r(14.))
            .child(
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap(r(8.))
                    .child(dot(provider_color(UsageProvider::Cursor, p)))
                    .child(brand_mark("cursor", 16., hsla(p.foreground), cx))
                    .child(div().truncate().child("Cursor")),
            )
            .child(
                div()
                    .id("usage-cursor-enable")
                    .flex_none()
                    .text_color(hsla(p.foreground))
                    .when(pending, |this| this.opacity(0.64))
                    .when(!pending, |this| this.cursor_pointer().hover(|style| style.underline()))
                    .tooltip(|window, cx| tooltip_view(CURSOR_KEYCHAIN_COPY.into(), window, cx))
                    .on_click(cx.listener(|this, _, _, cx| this.enable_cursor_keychain(cx)))
                    .child("Enable"),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_breakdown(
        &mut self,
        shown: &UsageSummaryInput,
        merged: &MergedUsage,
        metric: ChartMetric,
        active: &[UsageProvider],
        loading: &LoadingState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hourly = shown.resolution == UsageResolution::Hour;
        let current = self.breakdown;
        let toggles = segmented("usage-breakdown", cx).children([(Breakdown::Model, "Model"), (Breakdown::Time, if hourly { "Hour" } else { "Day" })].map(|(value, label)| {
            text_segment(("usage-breakdown", value as usize), label, value == current, None, window, cx).on_click(cx.listener(move |this, _, _, cx| {
                this.breakdown = value;
                cx.notify();
            }))
        }));
        let table = match current {
            Breakdown::Model => self.render_model_table(merged, metric, loading, window, cx).into_any_element(),
            Breakdown::Time => render_time_table(merged, hourly, self.data.hour12, active, loading, cx).into_any_element(),
        };
        v_flex().gap(r(12.)).child(h_flex().items_center().justify_between().gap(r(12.)).child(heading("Breakdown", cx)).child(toggles)).child(table)
    }

    fn render_model_table(&mut self, merged: &MergedUsage, metric: ChartMetric, loading: &LoadingState, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let models = if metric == ChartMetric::Tokens { merge::sort_models_by_tokens(&merged.models) } else { merged.models.clone() };
        let peak = models.iter().map(|model| if metric == ChartMetric::Tokens { model.total_tokens as f64 } else { model.cost_usd }).fold(0., f64::max);
        let header = h_flex()
            .py(r(8.))
            .border_b_1()
            .border_color(hsla(p.border))
            .text_size(r(12.))
            .text_color(hsla(p.muted_foreground))
            .child(div().w(r(28.)).flex_none().child("#"))
            .child(div().flex_1().child("Model"))
            .child(div().w(r(104.)).flex_none().text_right().child("Cost"))
            .child(div().w(r(80.)).flex_none().text_right().child("Share"))
            .child(div().w(r(80.)).flex_none().text_right().child("Tokens"));
        let rows: Vec<AnyElement> = if models.is_empty() {
            vec![div().py(r(24.)).flex().justify_center().text_size(r(14.)).text_color(hsla(p.muted_foreground)).child("No activity in this window.").into_any_element()]
        } else {
            models
                .iter()
                .enumerate()
                .map(|(index, model)| {
                    let key = model.key();
                    let value = if metric == ChartMetric::Tokens { model.total_tokens as f64 } else { model.cost_usd };
                    let has_bar = value > 0. && peak > 0.;
                    let row_loading = loading.is_loading(model.provider);
                    let share = model.share(metric).map(format_percent).unwrap_or_default();
                    let cost: AnyElement = if model.is_cost_unknown() {
                        div().text_color(hsla(p.muted_foreground)).child("Unpriced").into_any_element()
                    } else {
                        div().child(format_usd(model.cost_usd)).into_any_element()
                    };
                    let id: SharedString = format!("usage-model-{key}").into();
                    focusable(div().id(id.clone()), id.into(), true, window, cx)
                        .flex()
                        .items_center()
                        .py(r(10.))
                        .border_b_1()
                        .border_color(hsla(p.border).opacity(0.5))
                        .text_size(r(14.))
                        .text_color(hsla(p.muted_foreground))
                        .cursor_pointer()
                        .hover(|style| style.bg(hsla(p.muted).opacity(0.5)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected_model = Some(key.clone());
                            this.dialog_hover = None;
                            cx.notify();
                        }))
                        .child(div().w(r(28.)).flex_none().text_size(r(12.)).child((index + 1).to_string()))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    h_flex()
                                        .gap(r(8.))
                                        .text_color(hsla(p.foreground))
                                        .when(!has_bar, |this| this.py(r(4.)))
                                        .child(brand_mark(model.provider.driver(), 14., hsla(p.foreground), cx))
                                        .child(div().min_w_0().child(model.model.clone())),
                                )
                                .when(has_bar, |this| {
                                    this.child(fade(
                                        div().mt(r(6.)).h(px(2.)).max_w(r(192.)).child(
                                            div()
                                                .h_full()
                                                .min_w(r(8.))
                                                .w(relative((value / peak) as f32))
                                                .rounded_full()
                                                .bg(provider_color(model.provider, p)),
                                        ),
                                        row_loading,
                                    ))
                                }),
                        )
                        .child(fade(div().w(r(104.)).flex_none().flex().justify_end().text_color(hsla(p.foreground)).child(cost), row_loading))
                        .child(fade(div().w(r(80.)).flex_none().text_right().child(share), row_loading))
                        .child(fade(div().w(r(80.)).flex_none().text_right().child(format_tokens(model.total_tokens as f64)), row_loading))
                        .into_any_element()
                })
                .collect()
        };
        v_flex().w_full().child(header).children(rows)
    }

    fn render_skeleton(&self, cx: &App) -> impl IntoElement {
        let p = palette(cx);
        let provider_rows = UsageProvider::ORDER.map(|_| {
            v_flex()
                .gap(r(4.))
                .child(
                    h_flex()
                        .min_h(r(20.))
                        .justify_between()
                        .gap(r(16.))
                        .child(
                            h_flex()
                                .gap(r(8.))
                                .child(skeleton(Some(8.), 8., cx).rounded_full())
                                .child(skeleton(Some(16.), 16., cx).rounded_full())
                                .child(skeleton(Some(80.), 14., cx)),
                        )
                        .child(skeleton(Some(56.), 14., cx)),
                )
                .child(skeleton(Some(144.), 16., cx))
        });
        v_flex()
            .gap(r(24.))
            .child(
                h_flex()
                    .items_start()
                    .gap(r(24.))
                    .child(
                        v_flex()
                            .w(r(288.))
                            .flex_none()
                            .gap(r(20.))
                            .child(v_flex().gap(r(4.)).child(skeleton(Some(144.), 40., cx)).child(skeleton(Some(128.), 16., cx)))
                            .children(provider_rows),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(r(12.))
                            .child(skeleton(Some(96.), 20., cx))
                            .child(v_flex().gap(r(4.)).pl(r(64.)).child(skeleton(None, 224., cx)).child(skeleton(None, 16., cx))),
                    ),
            )
            .child(
                v_flex().gap(r(8.)).child(heading("Totals", cx)).child(div().grid().grid_cols(5).gap_x(r(24.)).gap_y(r(16.)).py(r(4.)).children(TOTAL_LABELS.map(|label| {
                    v_flex()
                        .gap(r(2.))
                        .child(div().text_size(r(12.)).text_color(hsla(p.muted_foreground)).child(label))
                        .child(skeleton(Some(64.), 24., cx))
                }))),
            )
            .child(v_flex().gap(r(10.)).child(skeleton(Some(112.), 20., cx)).child(skeleton(None, 8., cx)).child(skeleton(Some(288.), 16., cx)))
            .child(
                v_flex()
                    .gap(r(12.))
                    .child(h_flex().justify_between().child(heading("Breakdown", cx)).child(skeleton(Some(112.), 28., cx).rounded(r(8.))))
                    .child(skeleton(None, 176., cx)),
            )
    }
}

fn render_time_table(merged: &MergedUsage, hourly: bool, hour12: bool, active: &[UsageProvider], loading: &LoadingState, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let periods: Vec<&merge::PeriodTotals> = if hourly { merged.hourly.iter().rev().collect() } else { merged.daily.iter().rev().collect() };
    let cell = || div().flex_1().min_w(r(72.)).pl(r(16.)).text_right();
    let header = h_flex()
        .py(r(8.))
        .border_b_1()
        .border_color(hsla(p.border))
        .text_size(r(12.))
        .text_color(hsla(p.muted_foreground))
        .child(div().w(relative(0.4)).flex_none().child(if hourly { "Hour" } else { "Day" }))
        .children(active.iter().map(|provider| cell().child(provider.label())))
        .child(cell().child("Total"))
        .child(cell().child("Tokens"));
    let rows: Vec<AnyElement> = if periods.is_empty() {
        vec![div().py(r(24.)).flex().justify_center().text_size(r(14.)).text_color(hsla(p.muted_foreground)).child("No activity in this window.").into_any_element()]
    } else {
        periods
            .into_iter()
            .map(|period| {
                let label = match &period.hour_start {
                    Some(hour) => format_hour_short(hour, &SystemZone, hour12),
                    None => format_day_short(&period.day),
                };
                h_flex()
                    .id(SharedString::from(format!("usage-period-row-{}", period.key())))
                    .py(r(8.))
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.5))
                    .text_size(r(14.))
                    .hover(|style| style.bg(hsla(p.muted).opacity(0.5)))
                    .child(div().w(relative(0.4)).flex_none().text_color(hsla(p.foreground)).child(label))
                    .children(active.iter().map(|provider| {
                        let cost = period.by_provider.get(provider).map_or(0., |totals| totals.cost_usd);
                        fade(cell().text_color(hsla(p.muted_foreground)).child(format_usd(cost)), loading.is_loading(*provider))
                    }))
                    .child(fade(cell().text_color(hsla(p.foreground)).child(format_usd(period.cost_usd)), loading.partial))
                    .child(fade(cell().text_color(hsla(p.muted_foreground)).child(format_tokens(period.total_tokens as f64)), loading.partial))
                    .into_any_element()
            })
            .collect()
    };
    v_flex().w_full().child(header).children(rows)
}
