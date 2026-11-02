//! The composer's primitives (design-system §6 and §7.9): footer controls, the send and stop
//! buttons, message-action pills, small buttons, menus, radios and checkboxes. Each owns its look;
//! callers set layout only.

use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, ElementId, FontWeight, Hsla, IntoElement, ParentElement, RenderOnce,
    SharedString, Styled, Window, div, point, prelude::*, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, h_flex};

use crate::icons::Icon;
use crate::theme::{hsla, palette};

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

const DISABLED_OPACITY: f32 = 0.64;

fn tooltip(text: SharedString) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView {
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

/// A footer control (`ComposerControl.tsx`): 28 tall, 16px icon, label at 80% foreground.
#[derive(IntoElement)]
pub struct ComposerControl {
    id: ElementId,
    icon: Option<Icon>,
    leading: Option<AnyElement>,
    label: Option<SharedString>,
    max_label_width: Option<f32>,
    suffix: Option<AnyElement>,
    pressed: bool,
    emphasized: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<Handler>,
}

impl ComposerControl {
    pub fn new(id: impl Into<ElementId>) -> Self {
        ComposerControl {
            id: id.into(),
            icon: None,
            leading: None,
            label: None,
            max_label_width: None,
            suffix: None,
            pressed: false,
            emphasized: false,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A leading element in place of the icon, such as a provider's monogram.
    pub fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn max_label_width(mut self, width: f32) -> Self {
        self.max_label_width = Some(width);
        self
    }

    pub fn suffix(mut self, suffix: impl IntoElement) -> Self {
        self.suffix = Some(suffix.into_any_element());
        self
    }

    /// Filled while its popup is open.
    pub fn pressed(mut self, pressed: bool) -> Self {
        self.pressed = pressed;
        self
    }

    /// Draws the icon in the label color, for a mode that differs from the default.
    pub fn emphasized(mut self, emphasized: bool) -> Self {
        self.emphasized = emphasized;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ComposerControl {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let label_color = hsla(p.foreground).opacity(0.8);
        let icon_color = if self.emphasized || self.pressed { label_color } else { hsla(p.muted_foreground) };
        h_flex()
            .id(self.id)
            .h(px(28.))
            .min_w_0()
            .px(px(8.))
            .gap(px(6.))
            .rounded(px(8.))
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(if self.pressed { hsla(p.foreground) } else { label_color })
            .when(self.pressed, |this| this.bg(hsla(p.accent)))
            .when(self.disabled, |this| this.opacity(DISABLED_OPACITY))
            .when(!self.disabled, |this| this.cursor_pointer().hover(|this| this.bg(hsla(p.accent)).text_color(hsla(p.foreground))))
            .children(self.icon.map(|glyph| glyph.render(px(16.), icon_color)))
            .children(self.leading)
            .children(self.label.map(|label| {
                div().min_w_0().truncate().when_some(self.max_label_width, |this, width| this.max_w(px(width))).child(label)
            }))
            .children(self.suffix)
            .when_some(self.tooltip, |this, text| this.tooltip(tooltip(text)))
            .when_some(self.on_click.filter(|_| !self.disabled), |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundKind {
    Send,
    Steer,
    Queue,
    Stop,
}

/// The send and stop buttons (`ComposerPrimaryActions.tsx`): 32px circles.
#[derive(IntoElement)]
pub struct RoundButton {
    id: ElementId,
    kind: RoundKind,
    disabled: bool,
    tooltip: SharedString,
    on_click: Option<Handler>,
}

impl RoundButton {
    pub fn new(id: impl Into<ElementId>, kind: RoundKind, tooltip: impl Into<SharedString>) -> Self {
        RoundButton { id: id.into(), kind, disabled: false, tooltip: tooltip.into(), on_click: None }
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

impl RenderOnce for RoundButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let (fill, hover, glyph, glyph_size, glyph_color) = match self.kind {
            RoundKind::Stop => {
                (hsla(p.destructive).opacity(0.9), hsla(p.destructive), Icon::Stop, 12., gpui::white())
            }
            kind if self.disabled => {
                let glyph = if kind == RoundKind::Send { Icon::Send } else { Icon::ArrowUp };
                let muted = hsla(p.foreground).opacity(0.12);
                (muted, muted, glyph, 14., hsla(p.muted_foreground))
            }
            kind => {
                let (glyph, size) = match kind {
                    RoundKind::Steer => (Icon::CornerUpRight, 16.),
                    RoundKind::Queue => (Icon::ListPlus, 16.),
                    _ => (Icon::Send, 14.),
                };
                (hsla(p.message_action), hsla(p.message_action_hover), glyph, size, hsla(p.message_action_foreground))
            }
        };
        div()
            .id(self.id)
            .size(px(32.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(fill)
            .when(!self.disabled, |this| {
                this.cursor_pointer().hover(|this| this.bg(hover)).shadow(vec![BoxShadow {
                    color: fill.opacity(0.24),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(2.),
                    spread_radius: px(0.),
                    inset: false,
                }])
            })
            .child(glyph.render(px(glyph_size), glyph_color))
            .tooltip(tooltip(self.tooltip))
            .when_some(self.on_click.filter(|_| !self.disabled), |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

/// A message-action pill (Submit answer, Next question): 28 tall, full radius, pad-x 16.
#[derive(IntoElement)]
pub struct Pill {
    id: ElementId,
    label: SharedString,
    disabled: bool,
    on_click: Option<Handler>,
}

impl Pill {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Pill { id: id.into(), label: label.into(), disabled: false, on_click: None }
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

impl RenderOnce for Pill {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        h_flex()
            .id(self.id)
            .h(px(28.))
            .px(px(16.))
            .flex_none()
            .justify_center()
            .rounded_full()
            .bg(hsla(p.message_action))
            .text_color(hsla(p.message_action_foreground))
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .when(self.disabled, |this| this.opacity(DISABLED_OPACITY))
            .when(!self.disabled, |this| this.cursor_pointer().hover(|this| this.bg(hsla(p.message_action_hover))))
            .child(self.label)
            .when_some(self.on_click.filter(|_| !self.disabled), |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    Default,
    Outline,
    /// No fill or border until hovered (`ghost`).
    Ghost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSize {
    /// 24 tall, 12px text (`xs`).
    Xs,
    /// 28 tall, 14px text (`sm`).
    Sm,
    /// 24 square, icon only (`icon-xs`).
    IconXs,
    /// 28 square, icon only (`icon-sm`).
    IconSm,
}

/// `button.tsx` in the variants and sizes the composer uses.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    variant: ButtonVariant,
    size: ButtonSize,
    label: Option<SharedString>,
    icon: Option<Icon>,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<Handler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, variant: ButtonVariant, size: ButtonSize) -> Self {
        Button { id: id.into(), variant, size, label: None, icon: None, disabled: false, tooltip: None, on_click: None }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let (height, pad, icon_size) = match self.size {
            ButtonSize::Xs => (24., 7., 14.),
            ButtonSize::Sm => (28., 9., 16.),
            ButtonSize::IconXs => (24., 0., 14.),
            ButtonSize::IconSm => (28., 0., 16.),
        };
        let (fill, hover, text, border, icon_color) = match self.variant {
            ButtonVariant::Default => {
                let fill = hsla(p.primary);
                (fill, fill.opacity(0.9), hsla(p.primary_foreground), fill, hsla(p.primary_foreground))
            }
            ButtonVariant::Outline => {
                let fill = if dark { hsla(p.input).opacity(0.32) } else { hsla(p.popover) };
                let hover = if dark { hsla(p.input).opacity(0.64) } else { hsla(p.accent).opacity(0.5) };
                (fill, hover, hsla(p.foreground), hsla(p.input), hsla(p.muted_foreground))
            }
            ButtonVariant::Ghost => {
                let clear = hsla(p.accent).opacity(0.);
                (clear, hsla(p.accent), hsla(p.foreground), clear, hsla(p.muted_foreground))
            }
        };
        h_flex()
            .id(self.id)
            .h(px(height))
            .when(matches!(self.size, ButtonSize::IconXs | ButtonSize::IconSm), |this| this.w(px(height)).justify_center())
            .px(px(pad))
            .flex_none()
            .gap(px(if self.size == ButtonSize::Sm { 6. } else { 4. }))
            .rounded(px(8.))
            .border_1()
            .border_color(border)
            .bg(fill)
            .text_color(text)
            .font_weight(FontWeight::MEDIUM)
            .map(|this| if self.size == ButtonSize::Sm { this.text_sm() } else { this.text_xs() })
            .when(self.variant == ButtonVariant::Outline, |this| {
                this.shadow(vec![BoxShadow {
                    color: gpui::black().opacity(0.05),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(2.),
                    spread_radius: px(0.),
                    inset: false,
                }])
            })
            .when(self.disabled, |this| this.opacity(DISABLED_OPACITY))
            .when(!self.disabled, |this| this.cursor_pointer().hover(|this| this.bg(hover)))
            .children(self.icon.map(|glyph| glyph.render(px(icon_size), icon_color)))
            .children(self.label.map(|label| div().max_w(px(160.)).truncate().child(label)))
            .when_some(self.tooltip, |this, text| this.tooltip(tooltip(text)))
            .when_some(self.on_click.filter(|_| !self.disabled), |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

/// Where a popup floats from its trigger, whose wrapper must be `relative()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Float {
    /// Above the trigger, start edges aligned: footer popups open upward.
    AboveStart,
    AboveEnd,
    BelowStart,
    BelowEnd,
}

/// A popup painted above everything (layer 130), kept inside the window.
pub fn floating(position: Float, content: impl IntoElement) -> AnyElement {
    let (anchor, host) = match position {
        Float::AboveStart => (gpui::Anchor::BottomLeft, div().absolute().top_0().left_0()),
        Float::AboveEnd => (gpui::Anchor::BottomRight, div().absolute().top_0().right_0()),
        Float::BelowStart => (gpui::Anchor::TopLeft, div().absolute().bottom_0().left_0()),
        Float::BelowEnd => (gpui::Anchor::TopRight, div().absolute().bottom_0().right_0()),
    };
    host.child(gpui::deferred(gpui::anchored().anchor(anchor).snap_to_window_with_margin(px(8.)).child(content)).with_priority(1))
        .into_any_element()
}

/// The floating shadow menus and popovers cast (§4.3).
pub fn floating_shadow(dark: bool) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: gpui::black().opacity(if dark { 0.8 } else { 0.55 }),
        offset: point(px(0.), px(if dark { 18. } else { 16. })),
        blur_radius: px(if dark { 44. } else { 40. }),
        spread_radius: px(-18.),
        inset: false,
    }]
}

/// A menu popup surface (`menu.tsx`): opaque `popover` fallback for the dropdown glass.
pub fn menu_surface(cx: &App) -> gpui::Div {
    let p = palette(cx);
    div()
        .flex()
        .flex_col()
        .min_w(px(160.))
        .p(px(4.))
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.dropdown_border))
        .bg(hsla(p.popover))
        .text_color(hsla(p.popover_foreground))
        .shadow(floating_shadow(cx.theme().is_dark()))
}

/// A menu item: min 28, pad 8 / 4, radius 4, highlighted `accent`.
#[derive(IntoElement)]
pub struct MenuItem {
    id: ElementId,
    label: SharedString,
    icon: Option<(Icon, Hsla)>,
    checked: Option<bool>,
    on_click: Option<Handler>,
}

impl MenuItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        MenuItem { id: id.into(), label: label.into(), icon: None, checked: None, on_click: None }
    }

    pub fn icon(mut self, icon: Icon, color: Hsla) -> Self {
        self.icon = Some((icon, color));
        self
    }

    /// Reserves the check column and fills it when `checked`.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for MenuItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        h_flex()
            .id(self.id)
            .min_h(px(28.))
            .px(px(8.))
            .py(px(4.))
            .gap(px(8.))
            .rounded(px(4.))
            .text_sm()
            .text_color(hsla(p.foreground))
            .cursor_pointer()
            .hover(|this| this.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
            .children(self.checked.map(|checked| {
                div().size(px(16.)).flex_none().children(checked.then(|| Icon::Check.render(px(16.), hsla(p.foreground))))
            }))
            .children(self.icon.map(|(glyph, color)| glyph.render(px(16.), color.opacity(0.8))))
            .child(div().min_w_0().flex_1().child(self.label))
            .when_some(self.on_click, |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

/// `kbd.tsx`: a key cap for shortcuts.
pub fn kbd(label: impl Into<SharedString>, cx: &App) -> gpui::Div {
    let p = palette(cx);
    div()
        .h(px(20.))
        .min_w(px(20.))
        .px(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(hsla(p.foreground).opacity(0.07))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.muted_foreground))
        .child(label.into())
}

/// The pending-question radio: 16px circle, `primary` with a 6px dot when checked.
pub fn radio(checked: bool, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .map(|this| match checked {
            true => this.border_color(hsla(p.primary)).bg(hsla(p.primary)).child(
                div().size(px(6.)).rounded_full().bg(hsla(p.primary_foreground)),
            ),
            false => this.border_color(hsla(p.input)).bg(if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) }),
        })
}

/// `checkbox.tsx`: 16px, radius 4, a 12px check on `primary` when checked.
pub fn checkbox(checked: bool, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .map(|this| match checked {
            true => this.border_color(hsla(p.primary)).bg(hsla(p.primary)).child(Icon::Check.render(px(12.), hsla(p.primary_foreground))),
            false => this.border_color(hsla(p.input)).bg(if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) }),
        })
}

/// A segmented radio track (permissions, speed): `input` at 40%, 2px padding.
pub fn segmented_track(cx: &App) -> gpui::Div {
    div().flex().p(px(2.)).rounded(px(8.)).bg(hsla(palette(cx).input).opacity(0.4))
}

/// One segment of a segmented track: 24 tall, checked `input` at 72%.
#[derive(IntoElement)]
pub struct Segment {
    id: ElementId,
    icon: Option<Icon>,
    label: Option<SharedString>,
    checked: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<Handler>,
}

impl Segment {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Segment { id: id.into(), icon: None, label: None, checked, disabled: false, tooltip: None, on_click: None }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Segment {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let color = if self.checked { hsla(p.foreground) } else { hsla(p.muted_foreground) };
        h_flex()
            .id(self.id)
            .h(px(24.))
            .flex_1()
            .px(px(8.))
            .gap(px(4.))
            .justify_center()
            .rounded(px(6.))
            .text_xs()
            .text_color(color)
            .when(self.checked, |this| this.bg(hsla(p.input).opacity(0.72)))
            .when(self.disabled, |this| this.opacity(0.4))
            .when(!self.disabled, |this| {
                this.cursor_pointer().hover(|this| {
                    let this = this.text_color(hsla(p.foreground));
                    if self.checked { this } else { this.bg(hsla(p.accent)) }
                })
            })
            .children(self.icon.map(|glyph| glyph.render(px(14.), color)))
            .children(self.label)
            .when_some(self.tooltip, |this, text| this.tooltip(tooltip(text)))
            .when_some(self.on_click.filter(|_| !self.disabled), |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}
