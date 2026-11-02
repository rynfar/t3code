//! The Usage page's own pieces (design-system §6): text segments for its toggle groups, the
//! breadcrumb menu triggers and their checkbox menus, outline and solid buttons, skeleton blocks,
//! and the provider brand mark. Each owns its look; callers set layout only. Sizes are rems so
//! the page follows the interface font size.

use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Div, ElementId, FontWeight, Hsla, IntoElement, ParentElement, Rems, RenderOnce, SharedString,
    Stateful, Styled, Window, div, img, point, prelude::*, px, rems, svg,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::usage::UsageProvider;

use crate::icons::{Icon, ProviderLogo, provider_logo};
use crate::right_panel::{focusable, tooltip_view};
use crate::theme::{Palette, hsla, palette};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A web pixel length at the 16px root, as rems.
pub fn r(px: f32) -> Rems {
    rems(px / 16.)
}

/// Figures the page is still reading fade back while they wait (`figureClass`).
pub const LOADING_OPACITY: f32 = 0.4;

/// A provider's series color.
pub fn provider_color(provider: UsageProvider, p: &Palette) -> Hsla {
    hsla(match provider {
        UsageProvider::Codex => p.usage_codex,
        UsageProvider::Claude => p.usage_claude,
        UsageProvider::Grok => p.usage_grok,
        UsageProvider::Cursor => p.usage_cursor,
        UsageProvider::Opencode => p.usage_opencode,
        UsageProvider::Antigravity => p.usage_antigravity,
        UsageProvider::Pi => p.usage_pi,
        UsageProvider::Muse => p.usage_muse,
    })
}

/// A driver's bar color on the Limits page (`barColor`): its provider's hue, else foreground.
pub fn driver_color(driver: &str, p: &Palette) -> Hsla {
    UsageProvider::from_driver(driver).map_or(hsla(p.foreground), |provider| provider_color(provider, p))
}

/// The provider's brand mark (`ProviderInstanceIcon` without a badge); one-color marks take `color`.
pub fn brand_mark(driver: &str, size: f32, color: Hsla, cx: &App) -> AnyElement {
    match provider_logo(driver) {
        Some(ProviderLogo::Mono(path)) => svg().path(path).size(r(size)).flex_none().text_color(color).into_any_element(),
        Some(ProviderLogo::Image { light, dark }) => img(if cx.theme().is_dark() { dark } else { light }).size(r(size)).flex_none().into_any_element(),
        None => div().size(r(size)).flex_none().into_any_element(),
    }
}

pub fn icon(glyph: Icon, size: f32, color: Hsla) -> gpui::Svg {
    glyph.svg().size(r(size)).text_color(color)
}

/// A 2px rounded dot in a provider's color, as the summary rows lead with.
pub fn dot(color: Hsla) -> Div {
    div().size(r(8.)).flex_none().rounded_full().bg(color)
}

/// A static skeleton block (`skeleton.tsx`): `muted_foreground` at 15%.
pub fn skeleton(width: Option<f32>, height: f32, cx: &App) -> Div {
    let p = palette(cx);
    div().h(r(height)).when_some(width, |this, width| this.w(r(width))).rounded(r(4.)).bg(hsla(p.muted_foreground).opacity(0.15))
}

/// The segmented `ToggleGroup` track.
pub fn segmented(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let p = palette(cx);
    div().id(id).flex().flex_none().items_center().p(px(2.)).gap(px(2.)).rounded(r(8.)).bg(hsla(p.input).opacity(0.4))
}

/// One labelled segment: 24 tall, 12px text, pad 10.
pub fn text_segment(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    pressed: bool,
    tooltip: Option<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let id = id.into();
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let (pressed_fill, hover_fill) =
        if dark { (hsla(p.input).opacity(0.72), hsla(p.input).opacity(0.32)) } else { (hsla(p.background), hsla(p.background).opacity(0.55)) };
    let foreground = hsla(p.foreground);
    focusable(div().id(id.clone()), id, true, window, cx)
        .flex()
        .items_center()
        .justify_center()
        .h(r(24.))
        .px(r(10.))
        .rounded(r(6.))
        .text_size(r(12.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .text_color(if pressed { foreground } else { hsla(p.muted_foreground) })
        .when(pressed, |this| {
            this.bg(pressed_fill).shadow(vec![BoxShadow {
                color: gpui::black().opacity(0.1),
                offset: point(px(0.), px(1.)),
                blur_radius: px(2.),
                spread_radius: px(0.),
                inset: false,
            }])
        })
        .when(!pressed, |this| this.hover(move |style| style.bg(hover_fill).text_color(foreground)))
        .when_some(tooltip, |this, text| this.tooltip(move |window, cx| tooltip_view(text.clone(), window, cx)))
        .child(label.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    /// `primary` fill.
    Default,
    Outline,
    GhostMuted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSize {
    /// 32 tall, 14px text.
    Default,
    /// 28 tall, 14px text.
    Sm,
    /// 24 tall, 12px text.
    Xs,
}

/// `button.tsx` in the variants the page uses beyond the panel's ghost icon buttons.
#[derive(IntoElement)]
pub struct ActionButton {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    size: ButtonSize,
    trailing: Option<Icon>,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl ActionButton {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, variant: ButtonVariant, size: ButtonSize) -> Self {
        ActionButton { id: id.into(), label: label.into(), variant, size, trailing: None, disabled: false, on_click: None }
    }

    pub fn trailing(mut self, icon: Icon) -> Self {
        self.trailing = Some(icon);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ActionButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let (height, pad, text, icon_size, gap) = match self.size {
            ButtonSize::Default => (32., 11., 14., 16., 8.),
            ButtonSize::Sm => (28., 9., 14., 16., 6.),
            ButtonSize::Xs => (24., 7., 12., 14., 4.),
        };
        let transparent = gpui::transparent_black();
        let (fill, border, text_color, icon_color, hover_fill) = match self.variant {
            ButtonVariant::Default => (hsla(p.primary), hsla(p.primary), hsla(p.primary_foreground), hsla(p.primary_foreground), hsla(p.primary).opacity(0.9)),
            ButtonVariant::Outline => {
                let fill = if dark { hsla(p.input).opacity(0.32) } else { hsla(p.popover) };
                let hover = if dark { hsla(p.input).opacity(0.64) } else { hsla(p.accent).opacity(0.5) };
                (fill, hsla(p.input), hsla(p.foreground), hsla(p.muted_foreground), hover)
            }
            ButtonVariant::GhostMuted => (transparent, transparent, hsla(p.muted_foreground), hsla(p.muted_foreground), hsla(p.accent)),
        };
        let foreground = hsla(p.foreground);
        let ghost = self.variant == ButtonVariant::GhostMuted;
        let disabled = self.disabled;
        focusable(div().id(self.id.clone()), self.id, !disabled, window, cx)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(r(height))
            .px(r(pad))
            .gap(r(gap))
            .rounded(r(8.))
            .border_1()
            .border_color(border)
            .bg(fill)
            .text_size(r(text))
            .font_weight(FontWeight::MEDIUM)
            .text_color(text_color)
            .when(disabled, |this| this.opacity(0.64))
            .when(!disabled, |this| {
                this.cursor_pointer().hover(move |style| {
                    let style = style.bg(hover_fill);
                    if ghost { style.text_color(foreground) } else { style }
                })
            })
            .child(div().min_w_0().truncate().child(self.label))
            .children(self.trailing.map(|glyph| icon(glyph, icon_size, icon_color)))
            .when_some(self.on_click.filter(|_| !disabled), |this, handler| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx)
                })
            })
    }
}

/// An `InlineButton` menu trigger: text with a trailing chevron that brightens on hover.
pub fn menu_trigger(id: impl Into<ElementId>, label: impl Into<SharedString>, leading: Option<AnyElement>, open: bool, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let id = id.into();
    let p = palette(cx);
    let group: SharedString = format!("usage-trigger-{id}").into();
    let chevron_rest = if open { 1. } else { 0.6 };
    focusable(div().id(id.clone()), id, true, window, cx)
        .group(group.clone())
        .flex()
        .min_w_0()
        .items_center()
        .gap(r(4.))
        .rounded(r(4.))
        .cursor_pointer()
        .child(div().min_w_0().truncate().child(label.into()))
        .child(
            div()
                .size(r(14.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(leading.unwrap_or_else(|| {
                    icon(Icon::ChevronDown, 14., hsla(p.muted_foreground))
                        .opacity(chevron_rest)
                        .group_hover(group, |style| style.opacity(1.))
                        .into_any_element()
                })),
        )
}

/// The dropdown panel (`menu.tsx`): opaque popover fallback over the page, floating shadow.
pub fn menu_panel(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    v_flex()
        .id(id)
        .occlude()
        .min_w(r(220.))
        .max_w(r(360.))
        .p(r(4.))
        .rounded(r(8.))
        .border_1()
        .border_color(hsla(p.dropdown_border))
        .bg(hsla(p.popover))
        .text_color(hsla(p.popover_foreground))
        .shadow(vec![BoxShadow {
            color: gpui::black().opacity(if dark { 0.8 } else { 0.55 }),
            offset: point(px(0.), px(if dark { 18. } else { 16. })),
            blur_radius: px(if dark { 44. } else { 40. }),
            spread_radius: px(-18.),
            inset: false,
        }])
}

/// A checkbox menu item: a 16px check column, then the label.
pub fn check_item(id: impl Into<ElementId>, checked: bool, label: AnyElement, cx: &App) -> Stateful<Div> {
    let p = palette(cx);
    h_flex()
        .id(id)
        .min_h(r(28.))
        .px(r(8.))
        .py(r(4.))
        .gap(r(8.))
        .rounded(r(4.))
        .text_size(r(14.))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
        .child(div().w(r(16.)).flex_none().children(checked.then(|| icon(Icon::Check, 14., hsla(p.foreground)))))
        .child(div().flex_1().min_w_0().child(label))
}

pub fn menu_separator(cx: &App) -> Div {
    let p = palette(cx);
    div().h(px(1.)).my(r(4.)).mx(r(-4.)).bg(hsla(p.border))
}

/// Small print inside a menu.
pub fn menu_note(text: impl Into<SharedString>, cx: &App) -> Div {
    let p = palette(cx);
    div().px(r(8.)).py(r(8.)).text_size(r(12.)).text_color(hsla(p.muted_foreground)).child(text.into())
}

/// A section title: 14px, 500 weight.
pub fn heading(text: impl Into<SharedString>, cx: &App) -> Div {
    let p = palette(cx);
    div().text_size(r(14.)).line_height(r(20.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(text.into())
}
