//! Native counterparts to the web's visibility-gated indicators and size transitions.

use std::cell::Cell;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, ElementId, Hsla, IntoElement, ParentElement, Pixels, RenderOnce,
    Styled, Transformation, TransformationMatrix, Window, canvas, div, fill, linear_color_stop,
    linear_gradient, percentage, point, prelude::*, px, radians, size,
};

use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// Returns whether GPUI or the platform has requested reduced motion.
pub fn prefers_reduced_motion(cx: &App) -> bool {
    cx.reduce_motion() || system_reduces_motion()
}

#[cfg(target_os = "macos")]
fn system_reduces_motion() -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSWorkspace;

    MainThreadMarker::new().is_some()
        && NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

#[cfg(not(target_os = "macos"))]
fn system_reduces_motion() -> bool {
    false
}

struct LoopClock {
    start: Instant,
    pending: Cell<bool>,
}

fn stepped_phase(
    id: ElementId,
    cycle: Duration,
    burst: Duration,
    steps: u32,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> Option<f32> {
    if prefers_reduced_motion(cx)
        || bounds.size.width <= px(0.)
        || bounds.size.height <= px(0.)
        || !bounds.intersects(&window.content_mask().bounds)
    {
        return None;
    }
    let clock = window.use_keyed_state(id, cx, |_, _| LoopClock {
        start: Instant::now(),
        pending: Cell::new(false),
    });
    let timing = clock.read(cx);
    let elapsed = timing.start.elapsed().as_nanos() % cycle.as_nanos();
    let burst = burst.as_nanos();
    let steps = u128::from(steps);
    let step = (elapsed * steps / burst).min(steps);
    let next = if step < steps {
        (burst * (step + 1)).div_ceil(steps)
    } else {
        cycle.as_nanos()
    };
    let delay = Duration::from_nanos((next - elapsed) as u64);
    if !timing.pending.replace(true) {
        let clock = clock.downgrade();
        let owner = window.current_view();
        window
            .spawn(cx, async move |cx| {
                cx.background_executor().timer(delay).await;
                let _ = cx.update(|window, _| {
                    // Occluded macOS windows suspend frame delivery, leaving at most one queued step.
                    window.on_next_frame(move |_, cx| {
                        if let Some(clock) = clock.upgrade() {
                            clock.read(cx).pending.set(false);
                            cx.notify(owner);
                        }
                    });
                });
            })
            .detach();
    }
    Some(step as f32 / steps as f32)
}

/// A one-second LoaderCircle that advances at twelve steps per turn.
#[derive(IntoElement)]
pub struct Spinner {
    id: ElementId,
    size: Pixels,
    color: Hsla,
}

impl Spinner {
    /// Creates a loading glyph with the specified size and color.
    pub fn new(id: impl Into<ElementId>, size: Pixels, color: Hsla) -> Self {
        Self {
            id: id.into(),
            size,
            color,
        }
    }
}

impl RenderOnce for Spinner {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let label = crate::i18n::t(cx, "Loading").to_owned();
        let clock_id = (self.id.clone(), "spinner-clock").into();
        let cycle = Duration::from_secs(1);
        div()
            .id(self.id)
            .role(gpui::Role::Status)
            .aria_label(label)
            .size(self.size)
            .flex_none()
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        let phase = stepped_phase(clock_id, cycle, cycle, 12, bounds, window, cx)
                            .unwrap_or_default();
                        let scale = window.scale_factor();
                        let center = bounds.center();
                        let transform = TransformationMatrix::unit()
                            .translate(center.scale(scale))
                            .rotate(radians(phase * std::f32::consts::TAU))
                            .translate(center.scale(-scale));
                        let _ = window.paint_svg(
                            bounds,
                            Icon::LoaderCircle.path().into(),
                            None,
                            transform,
                            self.color,
                            cx,
                        );
                    },
                )
                .size_full(),
            )
    }
}

/// A solid connection or activity dot with an optional two-second ping.
#[derive(IntoElement)]
pub struct StatusDot {
    id: ElementId,
    size: Pixels,
    color: Hsla,
    live: bool,
}

impl StatusDot {
    /// Creates a dot that pings when live and stays solid otherwise.
    pub fn new(id: impl Into<ElementId>, size: Pixels, color: Hsla, live: bool) -> Self {
        Self {
            id: id.into(),
            size,
            color,
            live,
        }
    }
}

impl RenderOnce for StatusDot {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let clock_id = (self.id, "status-ping").into();
        canvas(
            |_, _, _| (),
            move |bounds, (), window, cx| {
                if self.live
                    && let Some(phase) = stepped_phase(
                        clock_id,
                        Duration::from_secs(2),
                        Duration::from_millis(800),
                        8,
                        bounds,
                        window,
                        cx,
                    )
                    && phase < 1.
                {
                    let diameter = self.size * (0.75 + 1.25 * phase);
                    let ping = Bounds::centered_at(bounds.center(), size(diameter, diameter));
                    window.paint_quad(
                        fill(ping, self.color.opacity(0.9 * (1. - phase)))
                            .corner_radii(diameter / 2.),
                    );
                }
                window.paint_quad(fill(bounds, self.color).corner_radii(self.size / 2.));
            },
        )
        .size(self.size)
        .flex_none()
    }
}

/// A stepped live-tool shine over content with a static muted foreground under reduced motion.
#[derive(IntoElement)]
pub struct Shimmer {
    id: ElementId,
    active: bool,
    content: AnyElement,
}

impl Shimmer {
    /// Wraps content and sweeps a highlight across it while active.
    pub fn new(id: impl Into<ElementId>, active: bool, content: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            active,
            content: content.into_any_element(),
        }
    }
}

impl RenderOnce for Shimmer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let color = hsla(p.foreground).opacity(0.16);
        let half_width = window.rem_size() * 2.25;
        let cycle = Duration::from_millis(2200);
        let clock_id = (self.id.clone(), "shine-clock").into();
        div()
            .id(self.id)
            .relative()
            .when(self.active, |view| view.text_color(hsla(p.secondary_label)))
            .child(self.content)
            .children(self.active.then(|| {
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        let Some(phase) =
                            stepped_phase(clock_id, cycle, cycle, 30, bounds, window, cx)
                        else {
                            return;
                        };
                        let center = bounds.left() - half_width
                            + (bounds.size.width + half_width * 2.) * phase;
                        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                            for (left, from, to) in [
                                (center - half_width, color.opacity(0.), color),
                                (center, color, color.opacity(0.)),
                            ] {
                                let beam = Bounds::new(
                                    point(left, bounds.top()),
                                    size(half_width, bounds.size.height),
                                );
                                let gradient = linear_gradient(
                                    90.,
                                    linear_color_stop(from, 0.),
                                    linear_color_stop(to, 1.),
                                );
                                window.paint_quad(fill(beam, gradient));
                            }
                        });
                    },
                )
                .absolute()
                .inset_0()
            }))
    }
}

#[derive(Clone, Copy)]
struct Transition {
    from: f32,
    target: f32,
    started: Instant,
}

impl Transition {
    fn at(self, now: Instant, duration: Duration) -> f32 {
        let progress =
            (now.duration_since(self.started).as_secs_f32() / duration.as_secs_f32()).min(1.);
        if progress >= 1. {
            self.target
        } else {
            let eased = 1. - (1. - progress).powi(3);
            self.from + (self.target - self.from) * eased
        }
    }
}

pub(crate) fn transition(
    id: impl Into<ElementId>,
    target: f32,
    duration: Duration,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let now = Instant::now();
    let settled = || Transition {
        from: target,
        target,
        started: now,
    };
    let state = window.use_keyed_state(id, cx, |_, _| Cell::new(settled()));
    let state = state.read(cx);
    if duration.is_zero() || prefers_reduced_motion(cx) {
        state.set(settled());
        return target;
    }
    let mut tween = state.get();
    if tween.target != target {
        tween = Transition {
            from: tween.at(now, duration),
            target,
            started: now,
        };
        state.set(tween);
    }
    let value = tween.at(now, duration);
    if value != target {
        window.request_animation_frame();
    }
    value
}

/// Rotates a right chevron a quarter turn when expanded.
pub fn animated_chevron(
    id: impl Into<ElementId>,
    expanded: bool,
    size: Pixels,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Svg {
    let turns = transition(
        id,
        if expanded { 0.25 } else { 0. },
        Duration::from_millis(150),
        window,
        cx,
    );
    Icon::ChevronRight
        .render(size, color)
        .with_transformation(Transformation::rotate(percentage(turns)))
}

macro_rules! animated_extent {
    ($(#[$attr:meta])* $name:ident, $extent:ident, $cross:ident, $axis:literal) => {
        #[doc = concat!("An overflow clip that animates ", $axis, " and keeps the full-size child mounted.")]
        #[derive(IntoElement)]
        pub struct $name {
            id: ElementId,
            extent: Pixels,
            open: bool,
            align_end: bool,
            duration: Duration,
            content: AnyElement,
        }

        impl $name {
            /// Wraps a child at its full extent and animates the visible area to or from zero.
            #[allow(dead_code)]
            $(#[$attr])*
            pub fn new(id: impl Into<ElementId>, extent: Pixels, open: bool, duration: Duration, content: impl IntoElement) -> Self {
                Self { id: id.into(), extent: extent.max(px(0.)), open, align_end: false, duration, content: content.into_any_element() }
            }

            /// Aligns the inner full-size child to the trailing edge of the visible clip.
            #[allow(dead_code)]
            pub fn align_end(mut self, align_end: bool) -> Self {
                self.align_end = align_end;
                self
            }

            /// Renders a static or externally driven clip of `visible` along the primary axis while
            /// keeping `full` unshrunk inside an overflow-hidden container.
            #[allow(dead_code)]
            pub fn clipped(visible: Pixels, full: Pixels, align_end: bool, content: impl IntoElement) -> gpui::Div {
                div()
                    .flex()
                    .flex_none()
                    .$cross()
                    .$extent(visible.max(px(0.)))
                    .overflow_hidden()
                    .when(align_end, |clip| clip.justify_end())
                    .child(div().flex_none().$cross().$extent(full.max(px(0.))).child(content))
            }
        }

        impl RenderOnce for $name {
            fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
                let target = if self.open { f32::from(self.extent) } else { 0. };
                let visible = transition((self.id.clone(), $axis), target, self.duration, window, cx);
                div().id(self.id).flex().flex_none().$cross().$extent(px(visible))
                    .overflow_hidden()
                    .when(self.align_end, |clip| clip.justify_end())
                    .child(div().flex_none().$cross().$extent(self.extent).when(visible == 0., |inner| inner.hidden()).child(self.content))
            }
        }
    };
}

animated_extent!(AnimatedHeight, h, w_full, "height");
animated_extent!(AnimatedWidth, w, h_full, "width");

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext};

    #[gpui::test]
    fn honors_gpui_reduce_motion(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            assert!(prefers_reduced_motion(cx));
        });
    }

    #[gpui::test]
    fn animated_width_honors_reduced_motion_and_preserves_inner_width(cx: &mut TestAppContext) {
        struct WidthHost {
            open: bool,
        }
        impl Render for WidthHost {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().flex().flex_row().size_full().child(
                    div()
                        .id("test-animated-width-outer")
                        .debug_selector(|| "test-animated-width-outer".into())
                        .h_full()
                        .child(
                            AnimatedWidth::new(
                                "test-animated-width",
                                px(240.),
                                self.open,
                                Duration::from_millis(200),
                                div()
                                    .id("test-animated-width-inner")
                                    .size_full()
                                    .debug_selector(|| "test-animated-width-inner".into()),
                            )
                            .align_end(true),
                        ),
                )
            }
        }

        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, vcx) = cx.add_window_view(|_, _| WidthHost { open: true });
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert_eq!(vcx.debug_bounds("test-animated-width-inner").unwrap().size.width, px(240.));

        view.update(vcx, |host, cx| {
            host.open = false;
            cx.notify();
        });
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert_eq!(vcx.debug_bounds("test-animated-width-outer").unwrap().size.width, px(0.));
    }
}
