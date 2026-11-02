//! Glass surfaces (design-system §4.6, §10.8): the backdrop blur, then the translucent fill and
//! border over it. The fill and border sit in an overlay, not on the panel, so the blur captures
//! only what lies beneath. Renderers without blur still draw the translucent fill.

use gpui::{App, Corners, Div, Hsla, ParentElement, Styled, canvas, div, px};

use crate::theme::metrics;

/// The layer to put first inside a `relative` panel of corner radius `radius`.
pub fn backdrop(radius: f32, fill: Hsla, border: Hsla, cx: &App) -> Div {
    let dark = gpui_component::ActiveTheme::theme(cx).is_dark();
    let blur = px(if dark { metrics::GLASS_BLUR_DARK } else { metrics::GLASS_BLUR });
    div()
        .absolute()
        .inset_0()
        .child(canvas(|_, _, _| {}, move |bounds, _, window, _| window.paint_backdrop_blur(bounds, Corners::all(px(radius)), blur)).size_full())
        .child(div().absolute().inset_0().rounded(px(radius)).border_1().border_color(border).bg(fill))
}
