//! Shared badges from `web/src/components/ui/badge.tsx`.

use gpui::{App, Hsla, ParentElement, SharedString, Styled, div, prelude::*, px, relative};
use gpui::FontWeight;
use gpui_component::ActiveTheme;

use crate::theme::{hsla, palette};

/// Shared `badge.tsx` variants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BadgeKind {
    Outline,
    Secondary,
    /// A host label's own color (`variant="label"`): 8% fill (12% dark), text mixed toward it.
    Label(u32),
    Info,
    Warning,
}

/// A `sm` badge (16 tall, 10px) or default (18 tall, 12px).
pub fn badge(text: impl Into<SharedString>, kind: BadgeKind, large: bool, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let (fill, color, border) = match kind {
        BadgeKind::Outline => (if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) }, hsla(p.foreground), Some(hsla(p.input))),
        BadgeKind::Secondary => (hsla(p.secondary), hsla(p.secondary_foreground), None),
        BadgeKind::Label(rgb) => {
            let base: Hsla = gpui::rgb(rgb).into();
            (base.opacity(if dark { 0.12 } else { 0.08 }), mix(base, hsla(p.foreground), if dark { 0.45 } else { 0.30 }), None)
        }
        BadgeKind::Info => (hsla(p.info).opacity(if dark { 0.16 } else { 0.08 }), hsla(p.info_foreground), None),
        BadgeKind::Warning => (hsla(p.warning).opacity(if dark { 0.16 } else { 0.08 }), hsla(p.warning_foreground), None),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .min_w_0()
        .max_w(px(160.))
        .h(px(if large { 18. } else { 16. }))
        .px(px(3.))
        .rounded(px(4.))
        .border_1()
        .border_color(border.unwrap_or(gpui::transparent_black()))
        .bg(fill)
        .text_size(px(if large { 12. } else { 10. }))
        .when(!large, |this| this.line_height(relative(1.)))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(div().min_w_0().truncate().child(text.into()))
}

/// The label badge's text color: the label hue mixed `amount` into the foreground.
fn mix(color: Hsla, foreground: Hsla, amount: f32) -> Hsla {
    let a = color.to_rgb();
    let b = foreground.to_rgb();
    let channel = |x: f32, y: f32| x * amount + y * (1. - amount);
    gpui::Rgba { r: channel(a.r, b.r), g: channel(a.g, b.g), b: channel(a.b, b.b), a: 1. }.into()
}

