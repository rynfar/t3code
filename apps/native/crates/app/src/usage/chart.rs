//! The per-provider chart (`UsageProviderChart.tsx`), drawn with GPUI paths: gridlines, a 12%
//! area under each provider's smoothed line, and the 2px line itself, stretched from the 960×260
//! view to the plot. Hovering picks the nearest period; the view repaints only when that changes.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;

use gpui::{AnyElement, App, Bounds, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, Point, SharedString, Styled, Window, canvas, div, point, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use t3_core::usage::UsageProvider;
use t3_core::usage::chart::{self, Chart, PLOT_TOP, VIEW_HEIGHT, VIEW_WIDTH};
use t3_core::usage::format::{format_tokens, format_usd};
use t3_core::usage::merge::ChartMetric;

use super::controls::{LOADING_OPACITY, brand_mark, provider_color, r};
use crate::theme::{hsla, palette};

const PLOT_HEIGHT: f32 = 224.;

/// Everything the chart draws, derived once per summary, metric and window.
pub struct ChartData {
    pub chart: Chart,
    pub metric: ChartMetric,
    pub providers: Vec<UsageProvider>,
    pub loading: BTreeSet<UsageProvider>,
    /// Axis labels: first, middle and last period.
    pub axis: [SharedString; 3],
    /// What the tooltip names each period.
    pub tooltip_labels: Vec<SharedString>,
}

pub fn format_value(metric: ChartMetric, value: f64) -> String {
    match metric {
        ChartMetric::Cost => format_usd(value),
        ChartMetric::Tokens => format_tokens(value),
    }
}

impl ChartData {
    pub fn new(
        periods: &[String],
        totals: &[t3_core::usage::merge::PeriodTotals],
        metric: ChartMetric,
        providers: Vec<UsageProvider>,
        loading: BTreeSet<UsageProvider>,
        labels: &dyn Fn(&str) -> (String, String),
    ) -> ChartData {
        let built = chart::build_chart(periods, totals, metric, &providers, &|provider| loading.contains(&provider));
        let at = |index: Option<usize>| index.and_then(|index| periods.get(index)).map(|period| labels(period).0).unwrap_or_default();
        let axis = [at(Some(0)), at(Some(periods.len() / 2)), at(periods.len().checked_sub(1))].map(|text| SharedString::from(text.to_uppercase()));
        ChartData {
            chart: built,
            metric,
            providers,
            loading,
            axis,
            tooltip_labels: periods.iter().map(|period| labels(period).1.into()).collect(),
        }
    }
}

fn scaled(bounds: &Bounds<Pixels>, x: f64, y: f64) -> Point<Pixels> {
    let sx = f64::from(bounds.size.width) / VIEW_WIDTH;
    let sy = f64::from(bounds.size.height) / VIEW_HEIGHT;
    point(bounds.origin.x + px((x * sx) as f32), bounds.origin.y + px((y * sy) as f32))
}

fn paint_chart(bounds: Bounds<Pixels>, data: &ChartData, hover: Option<usize>, colors: &ChartColors, window: &mut Window) {
    let scale = &data.chart.scale;
    for tick in &scale.ticks {
        let y = scaled(&bounds, 0., chart::value_to_y(*tick, scale.max)).y;
        window.paint_quad(gpui::fill(Bounds::new(point(bounds.origin.x, y - px(0.5)), gpui::size(bounds.size.width, px(1.))), colors.grid));
    }
    for series in &data.chart.series {
        let Some(first) = series.segments.first() else { continue };
        let fade = if series.loading { LOADING_OPACITY } else { 1. };
        let color = colors.series(series.provider);
        let mut area = PathBuilder::fill();
        area.move_to(scaled(&bounds, first.from.x, first.from.y));
        for segment in &series.segments {
            area.cubic_bezier_to(scaled(&bounds, segment.to.x, segment.to.y), scaled(&bounds, segment.c1.x, segment.c1.y), scaled(&bounds, segment.c2.x, segment.c2.y));
        }
        area.line_to(scaled(&bounds, VIEW_WIDTH, VIEW_HEIGHT));
        area.line_to(scaled(&bounds, 0., VIEW_HEIGHT));
        area.close();
        if let Ok(path) = area.build() {
            window.paint_path(path, color.opacity(0.12 * fade));
        }
    }
    for series in &data.chart.series {
        let Some(first) = series.segments.first() else { continue };
        let fade = if series.loading { LOADING_OPACITY } else { 1. };
        let mut line = PathBuilder::stroke(px(2.));
        line.move_to(scaled(&bounds, first.from.x, first.from.y));
        for segment in &series.segments {
            line.cubic_bezier_to(scaled(&bounds, segment.to.x, segment.to.y), scaled(&bounds, segment.c1.x, segment.c1.y), scaled(&bounds, segment.c2.x, segment.c2.y));
        }
        if let Ok(path) = line.build() {
            window.paint_path(path, colors.series(series.provider).opacity(fade));
        }
    }
    if let Some(index) = hover {
        let x = scaled(&bounds, index as f64 * data.chart.step_x, 0.).x;
        let top = scaled(&bounds, 0., PLOT_TOP).y;
        let line = Bounds::new(point(x - px(0.5), top), gpui::size(px(1.), bounds.origin.y + bounds.size.height - top));
        window.paint_quad(gpui::fill(line, colors.hover));
    }
}

#[derive(Clone)]
struct ChartColors {
    grid: Hsla,
    hover: Hsla,
    series: [Hsla; 8],
}

impl ChartColors {
    fn series(&self, provider: UsageProvider) -> Hsla {
        self.series[provider.index()]
    }
}

type HoverHandler = Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>;

/// The chart with its axes; `on_hover` hears the hovered period as it changes.
pub fn provider_chart(
    id: &'static str,
    data: Rc<ChartData>,
    hover: Option<usize>,
    on_hover: impl Fn(Option<usize>, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let colors = ChartColors {
        grid: hsla(p.border),
        hover: hsla(p.muted_foreground),
        series: UsageProvider::ORDER.map(|provider| provider_color(provider, p)),
    };
    let bounds_cell: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
    let on_hover: HoverHandler = Rc::new(on_hover);
    let scale = &data.chart.scale;
    let ticks: Vec<(f32, String)> = if scale.labeled {
        scale
            .ticks
            .iter()
            .map(|tick| {
                let fraction = (chart::value_to_y(*tick, scale.max) / VIEW_HEIGHT) as f32;
                (fraction, if *tick == 0. { "0".to_owned() } else { format_value(data.metric, *tick) })
            })
            .collect()
    } else {
        Vec::new()
    };
    let periods = data.chart.columns.len();
    let paint_data = data.clone();
    let paint_bounds = bounds_cell.clone();
    let plot = canvas(
        move |bounds, _, _| {
            paint_bounds.set(Some(bounds));
        },
        move |bounds, _, window, _| paint_chart(bounds, &paint_data, hover, &colors, window),
    )
    .size_full();
    let move_bounds = bounds_cell.clone();
    let move_handler = on_hover.clone();
    let tooltip = hover.and_then(|index| tooltip(&data, index, cx));
    let right_half = hover.is_some_and(|index| periods > 1 && index * 2 > periods - 1);
    let hover_fraction = hover.map_or(0., |index| if periods < 2 { 0. } else { index as f32 / (periods - 1) as f32 });
    v_flex()
        .gap(r(4.))
        .child(
            h_flex()
                .gap(r(8.))
                .items_start()
                .child(
                    div().relative().h(r(PLOT_HEIGHT)).w(r(56.)).flex_none().children(ticks.into_iter().map(|(fraction, label)| {
                        div()
                            .absolute()
                            .right_0()
                            .top(gpui::relative(fraction))
                            .mt(r(-7.))
                            .text_size(r(10.))
                            .line_height(r(14.))
                            .text_color(hsla(p.muted_foreground))
                            .child(label)
                    })),
                )
                .child(
                    div()
                        .id(id)
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h(r(PLOT_HEIGHT))
                        .child(plot)
                        .on_mouse_move(move |event: &gpui::MouseMoveEvent, window, cx| {
                            let Some(bounds) = move_bounds.get() else { return };
                            let width = f64::from(bounds.size.width);
                            if width <= 0. {
                                return;
                            }
                            let fraction = f64::from(event.position.x - bounds.origin.x) / width;
                            let next = if bounds.contains(&event.position) { chart::hover_index(fraction, periods) } else { None };
                            if next != hover {
                                move_handler(next, window, cx);
                            }
                        })
                        .on_hover(move |hovered, window, cx| {
                            if !hovered && hover.is_some() {
                                on_hover(None, window, cx);
                            }
                        })
                        .children(tooltip.map(|tooltip| {
                            let anchor = div().absolute().top(r(8.)).w(r(176.));
                            let placed = if right_half {
                                anchor.right(gpui::relative(1. - hover_fraction)).mr(r(12.))
                            } else {
                                anchor.left(gpui::relative(hover_fraction)).ml(r(12.))
                            };
                            placed.child(tooltip)
                        })),
                ),
        )
        .child(
            h_flex()
                .justify_between()
                .pl(r(64.))
                .text_size(r(10.))
                .line_height(r(14.))
                .text_color(hsla(p.muted_foreground))
                .children(data.axis.iter().cloned().map(|label| div().child(label))),
        )
        .into_any_element()
}

fn tooltip(data: &ChartData, index: usize, cx: &App) -> Option<impl IntoElement> {
    let p = palette(cx);
    let column = data.chart.columns.get(index)?;
    let label = data.tooltip_labels.get(index).cloned().unwrap_or_default();
    let partial = data.providers.iter().any(|provider| data.loading.contains(provider));
    let figure = |loading: bool| hsla(if loading { p.muted_foreground } else { p.foreground });
    Some(
        v_flex()
            .relative()
            .min_w(r(144.))
            .px(r(10.))
            .py(r(8.))
            .rounded(r(12.))
            .text_size(r(12.))
            .line_height(r(16.))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::black().opacity(0.25),
                offset: point(px(0.), px(10.)),
                blur_radius: px(15.),
                spread_radius: px(-3.),
                inset: false,
            }])
            .child(crate::glass::backdrop(12., hsla(p.glass_surface), hsla(p.border).opacity(0.5), cx))
            .child(div().relative().mb(r(4.)).text_color(hsla(p.muted_foreground)).child(label))
            .children(data.providers.iter().map(|provider| {
                h_flex()
                    .relative()
                    .justify_between()
                    .gap(r(12.))
                    .child(
                        h_flex()
                            .gap(r(6.))
                            .text_color(hsla(p.muted_foreground))
                            .child(brand_mark(provider.driver(), 12., hsla(p.muted_foreground), cx))
                            .child(provider.label()),
                    )
                    .child(div().text_color(figure(data.loading.contains(provider))).child(format_value(data.metric, column.value(*provider))))
            }))
            .child(
                h_flex()
                    .relative()
                    .justify_between()
                    .gap(r(12.))
                    .mt(r(4.))
                    .pt(r(4.))
                    .border_t_1()
                    .border_color(hsla(p.border))
                    .child(div().text_color(hsla(p.muted_foreground)).child("Total"))
                    .child(div().text_color(figure(partial)).child(format_value(data.metric, column.total))),
            ),
    )
}
