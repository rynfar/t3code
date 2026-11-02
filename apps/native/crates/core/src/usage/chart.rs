//! The per-provider chart's geometry: `web/src/components/usage/UsageProviderChart.tsx`. Each
//! provider is a smoothed line over a translucent area, layered from zero (not stacked), in a
//! 960×260 view the chart stretches to its box. Monotone cubic tangents keep a curve from
//! overshooting between points.

use std::collections::HashMap;

use super::UsageProvider;
use super::merge::{ChartMetric, PeriodTotals};

pub const VIEW_WIDTH: f64 = 960.;
pub const VIEW_HEIGHT: f64 = 260.;
pub const TICK_COUNT: usize = 4;
pub const PLOT_TOP: f64 = 8.;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// One period: each provider's value in display order, and their sum.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub bands: [f64; 8],
    pub total: f64,
}

impl Column {
    pub fn value(&self, provider: UsageProvider) -> f64 {
        self.bands[provider.index()]
    }
}

/// `buildPeriodColumns`.
pub fn build_period_columns(periods: &[String], by_period: &HashMap<&str, &PeriodTotals>, metric: ChartMetric) -> Vec<Column> {
    periods
        .iter()
        .map(|period| {
            let entry = by_period.get(period.as_str());
            let mut bands = [0.; 8];
            for provider in UsageProvider::ORDER {
                bands[provider.index()] = entry.and_then(|entry| entry.by_provider.get(&provider)).map_or(0., |totals| totals.value(metric));
            }
            Column { bands, total: bands.iter().sum() }
        })
        .collect()
}

/// `niceScale`: a max and evenly spaced ticks on 1, 2, 5 or 10 times a power of ten.
pub fn nice_scale(peak: f64, count: usize) -> (f64, Vec<f64>) {
    if peak <= 0. {
        return (0., vec![0.]);
    }
    let raw_step = peak / count as f64;
    let magnitude = 10f64.powf(raw_step.log10().floor());
    let normalized = raw_step / magnitude;
    let step = magnitude
        * if normalized > 5. {
            10.
        } else if normalized > 2. {
            5.
        } else if normalized > 1. {
            2.
        } else {
            1.
        };
    let max = (peak / step).ceil() * step;
    let mut ticks = Vec::new();
    let mut value = 0.;
    while value <= max + step * 1e-6 {
        ticks.push(value);
        value += step;
    }
    (max, ticks)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scale {
    pub max: f64,
    pub ticks: Vec<f64>,
    /// False while every series is still loading: placeholder gridlines without numbers.
    pub labeled: bool,
}

/// `chartScale`: series each start at zero, so the scale follows the tallest single series.
pub fn chart_scale(columns: &[Column], any_loading: bool) -> Scale {
    let peak = columns.iter().flat_map(|column| column.bands.iter().copied()).fold(0., f64::max);
    if peak == 0. && any_loading {
        return Scale { max: TICK_COUNT as f64, ticks: (0..=TICK_COUNT).map(|tick| tick as f64).collect(), labeled: false };
    }
    let (max, ticks) = nice_scale(peak, TICK_COUNT);
    Scale { max, ticks, labeled: true }
}

/// `valueToY` in view units.
pub fn value_to_y(value: f64, max: f64) -> f64 {
    if max == 0. { VIEW_HEIGHT } else { VIEW_HEIGHT - value / max * (VIEW_HEIGHT - PLOT_TOP) }
}

fn monotone_tangents(points: &[Point]) -> Vec<f64> {
    let count = points.len();
    if count < 2 {
        return vec![0.];
    }
    let slopes: Vec<f64> = points
        .windows(2)
        .map(|pair| {
            let dx = pair[1].x - pair[0].x;
            if dx == 0. { 0. } else { (pair[1].y - pair[0].y) / dx }
        })
        .collect();
    let mut tangents = vec![0.; count];
    tangents[0] = slopes[0];
    tangents[count - 1] = slopes[count - 2];
    for index in 1..count - 1 {
        let (previous, next) = (slopes[index - 1], slopes[index]);
        tangents[index] = if previous * next <= 0. { 0. } else { (previous + next) / 2. };
    }
    for (index, slope) in slopes.iter().copied().enumerate() {
        if slope == 0. {
            tangents[index] = 0.;
            tangents[index + 1] = 0.;
            continue;
        }
        let a = tangents[index] / slope;
        let b = tangents[index + 1] / slope;
        let magnitude = a * a + b * b;
        if magnitude > 9. {
            let scale = 3. / magnitude.sqrt();
            tangents[index] = scale * a * slope;
            tangents[index + 1] = scale * b * slope;
        }
    }
    tangents
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurveSegment {
    pub from: Point,
    pub c1: Point,
    pub c2: Point,
    pub to: Point,
}

/// `smoothCurve`: cubic segments through every point.
pub fn smooth_curve(points: &[Point]) -> Vec<CurveSegment> {
    if points.len() < 2 {
        return Vec::new();
    }
    let tangents = monotone_tangents(points);
    points
        .windows(2)
        .enumerate()
        .map(|(index, pair)| {
            let (from, to) = (pair[0], pair[1]);
            let dx = to.x - from.x;
            CurveSegment {
                from,
                c1: Point { x: from.x + dx / 3., y: from.y + tangents[index] * dx / 3. },
                c2: Point { x: to.x - dx / 3., y: to.y - tangents[index + 1] * dx / 3. },
                to,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub provider: UsageProvider,
    pub loading: bool,
    pub total: f64,
    pub segments: Vec<CurveSegment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub columns: Vec<Column>,
    pub scale: Scale,
    pub step_x: f64,
    /// Largest first, so the biggest area sits beneath the rest.
    pub series: Vec<Series>,
}

/// `buildChart`.
pub fn build_chart(
    periods: &[String],
    totals: &[PeriodTotals],
    metric: ChartMetric,
    providers: &[UsageProvider],
    loading: &dyn Fn(UsageProvider) -> bool,
) -> Chart {
    let by_period: HashMap<&str, &PeriodTotals> = totals.iter().map(|entry| (entry.key(), entry)).collect();
    let columns = build_period_columns(periods, &by_period, metric);
    let scale = chart_scale(&columns, providers.iter().any(|provider| loading(*provider)));
    let step_x = if periods.len() < 2 { 0. } else { VIEW_WIDTH / (periods.len() - 1) as f64 };
    let mut series: Vec<Series> = providers
        .iter()
        .map(|&provider| {
            let points: Vec<Point> = columns
                .iter()
                .enumerate()
                .map(|(index, column)| Point { x: index as f64 * step_x, y: value_to_y(column.value(provider), scale.max) })
                .collect();
            Series {
                provider,
                loading: loading(provider),
                total: columns.iter().map(|column| column.value(provider)).sum(),
                segments: smooth_curve(&points),
            }
        })
        .collect();
    series.sort_by(|a, b| b.total.total_cmp(&a.total));
    Chart { columns, scale, step_x, series }
}

/// The hovered period for a pointer at `fraction` across the plot.
pub fn hover_index(fraction: f64, periods: usize) -> Option<usize> {
    if periods == 0 {
        return None;
    }
    let index = (fraction.clamp(0., 1.) * (periods - 1) as f64).round() as usize;
    Some(index.min(periods - 1))
}
