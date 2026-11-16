//! Glass surfaces (design-system §4.6, §10.8): the backdrop blur, then the translucent fill and
//! border over it. The fill and border sit in an overlay, not on the panel, so the blur captures
//! only what lies beneath. Renderers without blur still draw the translucent fill.

use gpui::prelude::FluentBuilder;
use gpui::{App, Corners, Div, Hsla, ParentElement, Styled, canvas, div, px};

use crate::theme::{hsla, metrics, palette, preferences};

/// The layer to put first inside a `relative` panel of corner radius `radius`.
pub fn backdrop(radius: f32, fill: Hsla, border: Hsla, cx: &App) -> Div {
    backdrop_with_highlight(radius, fill, border, None, cx)
}

pub fn backdrop_with_highlight(radius: f32, fill: Hsla, border: Hsla, highlight: Option<Hsla>, cx: &App) -> Div {
    backdrop_inner(Corners::all(px(radius)), fill, Some(border), false, highlight, cx)
}

/// Top-rounded backdrop blur + top/side border (for banners tucked directly onto `composer-card`).
pub fn backdrop_top(radius: f32, fill: Hsla, border: Hsla, cx: &App) -> Div {
    let corners = Corners { top_left: px(radius), top_right: px(radius), bottom_left: px(0.), bottom_right: px(0.) };
    backdrop_inner(corners, fill, Some(border), true, None, cx)
}

/// Page-colored translucent glass (`surface-glass`, `ComposerSurface.ContextStrip`).
pub fn page_backdrop(radius: f32, cx: &App) -> Div {
    let fill = hsla(palette(cx).background).opacity(preferences(cx).glass as f32 / 100.);
    backdrop_inner(Corners::all(px(radius)), fill, None, false, None, cx)
}

fn backdrop_inner(corners: Corners<gpui::Pixels>, fill: Hsla, border: Option<Hsla>, top_only: bool, highlight: Option<Hsla>, cx: &App) -> Div {
    let dark = gpui_component::ActiveTheme::theme(cx).is_dark();
    let blur = px(if dark { metrics::GLASS_BLUR_DARK } else { metrics::GLASS_BLUR });
    let radius = corners.top_left;
    let overlay = div()
        .absolute()
        .inset_0()
        .map(|this| if top_only { this.rounded_t(radius) } else { this.rounded(radius) })
        .when_some(border, |this, border| {
            if top_only {
                this.border_t_1().border_l_1().border_r_1().border_color(border)
            } else {
                this.border_1().border_color(border)
            }
        })
        .bg(fill)
        .when_some(highlight, |this, color| {
            this.shadow(vec![gpui::BoxShadow {
                color,
                offset: gpui::point(px(0.), px(1.)),
                blur_radius: px(0.),
                spread_radius: px(0.),
                inset: true,
            }])
        });
    div()
        .absolute()
        .inset_0()
        .child(canvas(|_, _, _| {}, move |bounds, _, window, _| window.paint_backdrop_blur(bounds, corners, blur)).size_full())
        .child(overlay)
}
