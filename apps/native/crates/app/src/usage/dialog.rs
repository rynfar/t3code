//! One model's breakdown (`UsageModelDialog.tsx`): cost, tokens, cost per million and cache hit,
//! its own trend, and its cost and token mix. An unpriced model's trend shows tokens, since its
//! cost is unknown rather than zero. Setting a price (`UsagePriceOverrides`) is not ported.

use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, Styled, Window, div, prelude::*};
use gpui_component::{h_flex, v_flex};
use t3_core::usage::format::{format_percent, format_tokens, format_usd};
use t3_core::usage::merge::{self, AnsweredUsage, ChartMetric, merge_usage};
use t3_core::usage::{USAGE_CONTRACT_VERSION, UsageMetric};

use super::UsageView;
use super::chart::provider_chart;
use super::controls::{brand_mark, r};
use super::overview::{chart_data, share_bar, speed_premium};
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

impl UsageView {
    pub(super) fn render_model_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = self.selected_model.clone()?;
        let (shown, merged, _) = self.data.shown()?;
        let model = merged.models.iter().find(|model| model.key() == key)?.clone();
        let p = palette(cx);
        let summary = self.data.summary.clone()?;
        let usage = merge_usage(
            &[AnsweredUsage {
                environment_id: self.data.environment_id.clone(),
                label: self.data.label.clone(),
                summary: Arc::new(summary.keep_buckets(|bucket| bucket.provider == model.provider && bucket.model == model.model)),
            }],
            USAGE_CONTRACT_VERSION,
        );
        let unknown = model.is_cost_unknown();
        let metric = if unknown || self.data.preferences.metric == UsageMetric::Tokens { ChartMetric::Tokens } else { ChartMetric::Cost };
        let stats: Vec<(&str, String)> = [
            Some(("Cost", if unknown { "Unpriced".to_owned() } else { format_usd(model.cost_usd) })),
            Some(("Tokens", format_tokens(model.total_tokens as f64))),
            model.cost_per_million_tokens().map(|value| ("Per 1M tokens", format_usd(value))),
            model.cache_hit_rate().map(|rate| ("Cache hit", format_percent(rate))),
        ]
        .into_iter()
        .flatten()
        .collect();
        let description = if unknown { model.provider.label().to_owned() } else { format!("{} · {} of cost", model.provider.label(), format_percent(model.cost_share)) };
        let chart = Rc::new(chart_data(&shown, &usage, metric, vec![model.provider], BTreeSet::new(), self.data.hour12));
        let entity = cx.entity();
        let trend = provider_chart(
            "usage-model-chart",
            chart,
            self.dialog_hover,
            move |hover, _, cx| {
                entity.update(cx, |view, cx| {
                    view.dialog_hover = hover;
                    cx.notify();
                })
            },
            cx,
        );
        let mut shares: Vec<AnyElement> = Vec::new();
        if !unknown {
            shares.extend(share_bar("usage-model-cost-types", "Cost by type", &merge::cost_type_segments(&usage.category_cost), format_usd, None, cx));
        }
        shares.extend(share_bar("usage-model-token-types", "Tokens by type", &merge::token_type_segments(&model.tokens), format_tokens, None, cx));
        if usage.speed_cost.fast + usage.speed_cost.ultrafast > 0. {
            shares.extend(share_bar(
                "usage-model-speed",
                "Cost by speed",
                &merge::speed_cost_segments(&usage.speed_cost),
                format_usd,
                Some(speed_premium(usage.speed_cost.premium, cx)),
                cx,
            ));
        }
        let narrow = Self::page_width(window) < 640.;
        let dialog = v_flex()
            .id("usage-model-dialog")
            .relative()
            .w_full()
            .max_w(r(768.))
            .max_h_full()
            .rounded(r(16.))
            .overflow_hidden()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .shadow(vec![gpui::BoxShadow {
                color: gpui::black().opacity(0.65),
                offset: gpui::point(gpui::px(0.), gpui::px(24.)),
                blur_radius: gpui::px(64.),
                spread_radius: gpui::px(-24.),
                inset: false,
            }])
            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .child(
                div().absolute().top(r(8.)).right(r(8.)).child(
                    Button::new("usage-model-close").variant(Variant::Ghost).size(Size::IconSm).icon(Icon::X).tooltip("Close").on_click(cx.listener(|this, _, _, cx| {
                        this.selected_model = None;
                        this.dialog_hover = None;
                        cx.notify();
                    })),
                ),
            )
            .child(
                v_flex()
                    .relative()
                    .gap(r(8.))
                    .p(r(24.))
                    .pr(r(48.))
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(brand_mark(model.provider.driver(), 20., cx))
                            .child(div().min_w_0().text_size(r(20.)).line_height(r(24.)).font_weight(FontWeight::SEMIBOLD).child(model.model.clone())),
                    )
                    .child(div().text_size(r(14.)).text_color(hsla(p.muted_foreground)).child(description)),
            )
            .child(
                v_flex()
                    .id("usage-model-panel")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(r(32.))
                    .px(r(24.))
                    .pt(r(4.))
                    .pb(r(24.))
                    .child(div().grid().grid_cols(if narrow { 2 } else { 4 }).gap_x(r(24.)).gap_y(r(16.)).children(stats.into_iter().map(|(label, value)| {
                        v_flex()
                            .min_w_0()
                            .gap(r(2.))
                            .child(div().text_size(r(12.)).text_color(hsla(p.muted_foreground)).child(label))
                            .child(div().text_size(r(24.)).line_height(r(32.)).font_weight(FontWeight::SEMIBOLD).child(value))
                    })))
                    .child(trend)
                    .child(div().grid().grid_cols(if narrow { 1 } else { 2 }).gap_x(r(40.)).gap_y(r(32.)).children(shares)),
            )
            .when(model.unpriced_tokens > 0., |this| {
                this.child(
                    h_flex()
                        .relative()
                        .px(r(24.))
                        .pb(r(16.))
                        .text_size(r(12.))
                        .text_color(hsla(p.muted_foreground))
                        .child(format!("{} tokens have no known price", format_tokens(model.unpriced_tokens))),
                )
            });
        Some(
            div()
                .id("usage-model-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(r(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.selected_model = None;
                        this.dialog_hover = None;
                        cx.notify();
                    }),
                )
                .child(dialog)
                .into_any_element(),
        )
    }
}
