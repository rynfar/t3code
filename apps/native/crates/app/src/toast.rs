//! A single top-right toast (design-system §6.6): dropdown glass, radius 8, title plus an
//! optional muted description and a 16px type icon.

use gpui::{AnyElement, App, BoxShadow, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, hsla as gpui_hsla, point, px};

use crate::icons::Icon;
use crate::theme::{hsla, palette};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub kind: ToastKind,
    pub title: SharedString,
    pub description: Option<SharedString>,
}

impl Toast {
    pub fn new(kind: ToastKind, title: impl Into<SharedString>) -> Toast {
        Toast { kind, title: title.into(), description: None }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Toast {
        self.description = Some(description.into());
        self
    }

    pub fn render(&self, cx: &App) -> AnyElement {
        let p = palette(cx);
        let (icon, color) = match self.kind {
            ToastKind::Success => (Icon::CircleCheck, p.success),
            ToastKind::Error => (Icon::CircleAlert, p.destructive),
            ToastKind::Warning => (Icon::CircleAlert, p.warning),
        };
        div()
            .absolute()
            .top(px(32. + 52.))
            .right(px(32.))
            .max_w(px(360.))
            .flex()
            .gap(px(8.))
            .pl(px(14.))
            .pr(px(16.))
            .py(px(12.))
            .rounded(px(8.))
            .child(crate::glass::backdrop(8., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
            .shadow(vec![
                BoxShadow { color: gpui_hsla(0., 0., 0., 0.25), offset: point(px(0.), px(20.)), blur_radius: px(25.), spread_radius: px(-5.), inset: false },
                BoxShadow { color: gpui_hsla(0., 0., 0., 0.25), offset: point(px(0.), px(8.)), blur_radius: px(10.), spread_radius: px(-6.), inset: false },
            ])
            .text_sm()
            .text_color(hsla(p.popover_foreground))
            .child(div().pt(px(2.)).child(icon.sized(px(16.)).text_color(hsla(color))))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).child(self.title.clone()))
                    .children(self.description.clone().map(|text| div().text_color(hsla(p.muted_foreground)).child(text))),
            )
            .into_any_element()
    }
}
