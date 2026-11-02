//! The composer's context window meter (`ContextWindowMeter.tsx`): a 20px ring beside Send that
//! fills with the open thread's context use, red past 90%, with the details on hover. It shows
//! only with the legacy "Context window indicator" setting on, as on the web. Compacting from the
//! meter is not offered: `/compact` belongs to the composer's commands.

use gpui::{
    AnyElement, App, Bounds, Context, Entity, FontWeight, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, Render, Styled, Window, canvas,
    div, point, prelude::*, px,
};
use gpui_component::{h_flex, v_flex};
use t3_core::providers;
use t3_core::usage::context_window::{
    ContextWindowSnapshot, compaction_message, context_window_snapshot, format_context_cost, format_context_tokens, format_meter_percentage,
};

use super::controls::r;
use crate::app_state::AppState;
use crate::right_panel::focusable;
use crate::theme::{hsla, palette};

const RING: f32 = 20.;
const RADIUS: f32 = RING * 9.75 / 24.;
const STROKE: f32 = RING * 3. / 24.;

/// The meter for the open thread, or nothing when it is off or there is nothing to show.
pub fn context_meter(state: &Entity<AppState>, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let app = state.read(cx);
    if !app.client_settings.context_window_meter {
        return None;
    }
    let projection = app.thread.projection.as_ref()?;
    let snapshot = context_window_snapshot(projection)?;
    let selection = &projection.thread.model_selection;
    let model_name = app
        .server_config
        .config
        .as_ref()
        .and_then(|config| config.provider(&selection.instance_id))
        .and_then(|provider| provider.models.iter().find(|model| model.slug == selection.model))
        .map_or_else(|| selection.model.clone(), |model| providers::model_name(model).to_owned());
    let p = palette(cx);
    let used = snapshot.used_percentage.unwrap_or(0.).clamp(0., 100.);
    let color = if used > 90. { hsla(p.error) } else { hsla(p.muted_foreground).opacity(0.72) };
    let track = hsla(p.muted_foreground).opacity(0.24);
    let tooltip = MeterDetails { snapshot, model_name };
    Some(
        focusable(div().id("context-window-meter"), "context-window-meter".into(), true, window, cx)
            .size(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .hover(|style| style.bg(hsla(p.accent)))
            .child(canvas(|_, _, _| {}, move |bounds, _, window, _| paint_ring(bounds, used, track, color, window)).size(px(RING)))
            .tooltip(move |_, cx| cx.new(|_| tooltip.clone()).into())
            .into_any_element(),
    )
}

fn arc_point(center: gpui::Point<Pixels>, fraction: f32) -> gpui::Point<Pixels> {
    let angle = fraction * std::f32::consts::TAU;
    point(center.x + px(RADIUS * angle.sin()), center.y - px(RADIUS * angle.cos()))
}

fn circle(center: gpui::Point<Pixels>, radius: f32, window: &mut Window, color: Hsla) {
    let mut dot = PathBuilder::fill();
    dot.move_to(point(center.x, center.y - px(radius)));
    dot.arc_to(point(px(radius), px(radius)), px(0.), false, true, point(center.x, center.y + px(radius)));
    dot.arc_to(point(px(radius), px(radius)), px(0.), false, true, point(center.x, center.y - px(radius)));
    dot.close();
    if let Ok(path) = dot.build() {
        window.paint_path(path, color);
    }
}

/// The track, then the used share clockwise from the top with round ends.
fn paint_ring(bounds: Bounds<Pixels>, used: f64, track: Hsla, color: Hsla, window: &mut Window) {
    let center = bounds.center();
    let radii = point(px(RADIUS), px(RADIUS));
    let mut ring = PathBuilder::stroke(px(STROKE));
    ring.move_to(arc_point(center, 0.));
    ring.arc_to(radii, px(0.), false, true, arc_point(center, 0.5));
    ring.arc_to(radii, px(0.), false, true, arc_point(center, 1.));
    if let Ok(path) = ring.build() {
        window.paint_path(path, track);
    }
    let fraction = (used / 100.) as f32;
    if fraction <= 0. {
        return;
    }
    let mut arc = PathBuilder::stroke(px(STROKE));
    arc.move_to(arc_point(center, 0.));
    if fraction >= 0.999 {
        arc.arc_to(radii, px(0.), false, true, arc_point(center, 0.5));
        arc.arc_to(radii, px(0.), false, true, arc_point(center, 1.));
    } else {
        arc.arc_to(radii, px(0.), fraction > 0.5, true, arc_point(center, fraction));
    }
    if let Ok(path) = arc.build() {
        window.paint_path(path, color);
    }
    circle(arc_point(center, 0.), STROKE / 2., window, color);
    circle(arc_point(center, fraction.min(1.)), STROKE / 2., window, color);
}

/// The hover details: usage, the bar against the window, processed tokens, cost, compaction.
#[derive(Clone)]
struct MeterDetails {
    snapshot: ContextWindowSnapshot,
    model_name: String,
}

impl Render for MeterDetails {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let snapshot = &self.snapshot;
        let percent = format_meter_percentage(snapshot.used_percentage);
        let used = snapshot.used_percentage.unwrap_or(0.).clamp(0., 100.);
        let color = if used > 90. { hsla(p.error) } else { hsla(p.muted_foreground).opacity(0.72) };
        let figure = match (snapshot.max_tokens, &percent) {
            (Some(max), Some(percent)) => {
                format!("{percent} · {}/{}", format_context_tokens(Some(snapshot.used_tokens)), format_context_tokens(Some(max)))
            }
            _ => format_context_tokens(Some(snapshot.used_tokens)),
        };
        let line = |label: &'static str, value: String| {
            h_flex()
                .justify_between()
                .gap(r(12.))
                .text_size(r(11.))
                .line_height(r(16.))
                .text_color(hsla(p.secondary_label))
                .child(label)
                .child(div().font_weight(FontWeight::MEDIUM).child(value))
        };
        v_flex()
            .w(r(256.))
            .gap(r(8.))
            .p(r(12.))
            .rounded(r(6.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.popover))
            .text_color(hsla(p.popover_foreground))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::black().opacity(0.05),
                offset: point(px(0.), px(4.)),
                blur_radius: px(6.),
                spread_radius: px(-1.),
                inset: false,
            }])
            .child(
                h_flex()
                    .justify_between()
                    .gap(r(12.))
                    .child(div().text_size(r(12.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Context Window"))
                    .child(div().text_size(r(11.)).text_color(hsla(p.secondary_label)).child(figure)),
            )
            .when(snapshot.max_tokens.is_some(), |this| {
                this.child(
                    div()
                        .h(r(6.))
                        .w_full()
                        .rounded_full()
                        .overflow_hidden()
                        .bg(hsla(p.muted).opacity(0.6))
                        .child(div().h_full().rounded_full().w(gpui::relative((used / 100.) as f32)).bg(color)),
                )
            })
            .when_some(snapshot.total_processed_tokens.filter(|total| *total > 0.), |this, total| {
                this.child(line("Total processed", format_context_tokens(Some(total))))
            })
            .when_some(snapshot.cost.clone(), |this, (amount, currency)| this.child(line("Cost", format_context_cost(amount, &currency))))
            .when(snapshot.compacts_automatically == Some(true), |this| {
                this.child(
                    div()
                        .mt(r(4.))
                        .text_size(r(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.secondary_label))
                        .child(compaction_message(Some(&self.model_name), snapshot.auto_compact_threshold)),
                )
            })
    }
}
