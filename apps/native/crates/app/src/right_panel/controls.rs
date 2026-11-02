//! The controls the panel chrome and the diff toolbar use: `Button` (`components/ui/button.tsx`
//! ghost, ghost-muted and secondary variants), the ghost `Toggle` (`components/ui/toggle.tsx`)
//! and the segmented `ToggleGroup` (`components/ui/toggle-group.tsx`), at their desktop sizes
//! from design-system §6.1 and §6.7. Callers pick a variant and size; they never restyle.

use std::rc::Rc;

use gpui::{
    AnyView, App, BoxShadow, ClickEvent, Div, ElementId, FontWeight, Hsla, InteractiveElement, Interactivity,
    IntoElement, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Stateful, StyleRefinement,
    Styled, Window, div, point, prelude::*, px,
};
use gpui_component::Selectable;
use gpui_component::menu::DropdownMenu;
use gpui_component::tooltip::Tooltip;

use crate::icons::Icon;
use crate::theme::{hsla, palette};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Ghost,
    GhostMuted,
    Secondary,
    /// The ghost `Toggle`: pressed shows `accent`.
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// 28 square, 16px icon (`icon-sm`; the `sm` toggle).
    IconSm,
    /// 24 square, 14px icon.
    IconXs,
    /// 20 square, 12px icon, radius 4.
    IconMicro,
    /// 24 tall, 12px text (`xs`).
    Xs,
}

struct Metrics {
    height: f32,
    pad_x: f32,
    text: f32,
    icon: f32,
    radius: f32,
    gap: f32,
}

impl Size {
    fn metrics(self) -> Metrics {
        match self {
            Size::IconSm => Metrics { height: 28., pad_x: 0., text: 14., icon: 16., radius: 8., gap: 6. },
            Size::IconXs => Metrics { height: 24., pad_x: 0., text: 12., icon: 14., radius: 6., gap: 4. },
            Size::IconMicro => Metrics { height: 20., pad_x: 0., text: 11., icon: 12., radius: 4., gap: 4. },
            Size::Xs => Metrics { height: 24., pad_x: 7., text: 12., icon: 14., radius: 8., gap: 4. },
        }
    }
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

const CONTROL_GROUP: &str = "panel-control";

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    base: Stateful<Div>,
    variant: Variant,
    size: Size,
    icon: Option<Icon>,
    icon_size: Option<f32>,
    label: Option<SharedString>,
    trailing: Option<Icon>,
    selected: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Button {
            base: div().id(id.clone()),
            id,
            variant: Variant::Ghost,
            size: Size::IconSm,
            icon: None,
            icon_size: None,
            label: None,
            trailing: None,
            selected: false,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The web sets `size-3.5` on most toolbar icons inside 28px controls.
    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = Some(size);
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// A trailing chevron at 0.7 opacity, as on menu triggers.
    pub fn trailing(mut self, icon: Icon) -> Self {
        self.trailing = Some(icon);
        self
    }

    pub fn pressed(mut self, pressed: bool) -> Self {
        self.selected = pressed;
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

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for Button {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for Button {}

impl Selectable for Button {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl DropdownMenu for Button {}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let m = self.size.metrics();
        let disabled = self.disabled;
        let base = focusable(self.base, self.id.clone(), !disabled, window, cx);
        let (text, icon_color, rest, hover, hover_text): (u32, Hsla, Option<u32>, u32, u32) = match self.variant {
            Variant::Ghost => (p.foreground, hsla(p.muted_foreground), None, p.accent, p.foreground),
            Variant::GhostMuted => (p.muted_foreground, hsla(p.muted_foreground), None, p.accent, p.foreground),
            Variant::Secondary => {
                (p.secondary_foreground, hsla(p.secondary_foreground), Some(p.secondary), p.secondary, p.secondary_foreground)
            }
            Variant::Toggle => (p.foreground, hsla(p.foreground).opacity(0.8), None, p.accent, p.foreground),
        };
        let pressed_fill = match self.variant {
            Variant::Secondary => None,
            _ => Some(p.accent),
        };
        let fill = if self.selected { pressed_fill.or(rest) } else { rest };
        let icon_only = self.label.is_none();
        let icon_size = self.icon_size.unwrap_or(m.icon);
        let tooltip = self.tooltip.clone();
        let on_click = self.on_click.clone();
        let follows_hover = self.variant == Variant::GhostMuted;
        base.group(CONTROL_GROUP)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(m.height))
            .when(icon_only, |this| this.w(px(m.height)))
            .when(!icon_only, |this| this.px(px(m.pad_x)).min_w_0())
            .gap(px(m.gap))
            .rounded(px(m.radius))
            .text_size(px(m.text))
            .font_weight(FontWeight::MEDIUM)
            .text_color(hsla(text))
            .when_some(fill, |this, fill| this.bg(hsla(fill)))
            .when(self.disabled, |this| this.opacity(0.64))
            .when(!self.disabled, |this| {
                this.cursor_pointer().hover(move |style| {
                    let style = style.text_color(hsla(hover_text));
                    if self.variant == Variant::Secondary { style.bg(hsla(hover).opacity(0.9)) } else { style.bg(hsla(hover)) }
                })
            })
            .when_some(self.icon, |this, icon| {
                let svg = icon.svg().size(px(icon_size)).text_color(icon_color);
                let hover_icon = hsla(hover_text);
                this.child(svg.when(follows_hover, |svg| svg.group_hover(CONTROL_GROUP, move |style| style.text_color(hover_icon))))
            })
            .when_some(self.label, |this, label| this.child(div().min_w_0().truncate().child(label)))
            .when_some(self.trailing, |this, icon| {
                this.child(icon.svg().size(px(14.)).text_color(hsla(text).opacity(0.7)))
            })
            .when_some(tooltip, |this, text| this.tooltip(move |window, cx| tooltip_view(text.clone(), window, cx)))
            .when_some(on_click.filter(|_| !self.disabled), |this, handler| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx)
                })
            })
    }
}

/// Makes a control a tab stop: Enter and Space click it (GPUI turns them into a click on the
/// focused element), and keyboard focus draws the ring from design-system §4.2, 2px of `ring`
/// inside a 1px `background` gap (`ring-2 ring-inset ring-offset-1`).
pub fn focusable(element: Stateful<Div>, key: ElementId, enabled: bool, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let handle = window.use_keyed_state(key, cx, |_, cx| cx.focus_handle()).read(cx).clone().tab_stop(enabled);
    let p = palette(cx);
    let ring = |color: Hsla, spread: f32| BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(spread),
        inset: true,
    };
    let shadows = vec![ring(hsla(p.ring), 3.), ring(hsla(p.background), 1.)];
    element.track_focus(&handle).focus_visible(move |style| style.shadow(shadows))
}

pub fn tooltip_view(text: SharedString, window: &mut Window, cx: &mut App) -> AnyView {
    Tooltip::new(text).build(window, cx)
}

/// The segmented `ToggleGroup`: a quiet track with one pressed segment.
pub fn segmented(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let p = palette(cx);
    div().id(id).flex().flex_none().items_center().p(px(2.)).gap(px(2.)).rounded(px(8.)).bg(hsla(p.input).opacity(0.4))
}

/// One segment of `segmented`: 24 tall, 14px icon.
pub fn segment(id: impl Into<ElementId>, icon: Icon, pressed: bool, label: &'static str, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let id = id.into();
    let p = palette(cx);
    let dark = gpui_component::ActiveTheme::theme(cx).is_dark();
    let (rest_text, pressed_fill, hover_fill) = if dark {
        (p.muted_foreground, hsla(p.input).opacity(0.72), hsla(p.input).opacity(0.32))
    } else {
        (p.muted_foreground, hsla(p.background), hsla(p.background).opacity(0.55))
    };
    let icon_color = hsla(if pressed { p.foreground } else { rest_text });
    let hover_icon = hsla(p.foreground);
    focusable(div().id(id.clone()), id, true, window, cx)
        .group(CONTROL_GROUP)
        .flex()
        .items_center()
        .justify_center()
        .h(px(24.))
        .px(px(10.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_color(hsla(if pressed { p.foreground } else { rest_text }))
        .when(pressed, |this| this.bg(pressed_fill))
        .when(!pressed, |this| this.hover(move |style| style.bg(hover_fill).text_color(hsla(p.foreground))))
        .tooltip(move |window, cx| tooltip_view(label.into(), window, cx))
        .child(icon.svg().size(px(14.)).text_color(icon_color).group_hover(CONTROL_GROUP, move |style| style.text_color(hover_icon)))
}
