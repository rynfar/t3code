//! The thread details card's controls (`ThreadDetailsControl.tsx`, `threadDetailsPanelStyles.ts`,
//! `ThreadDetailsSection.tsx`, design-system §7.11): 32px rows, split rows whose hairline shows
//! while in use, section frames, and the rows their menus list. Callers set layout only.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, Hsla, InteractiveElement, Interactivity, IntoElement, MouseMoveEvent, ParentElement,
    RenderOnce, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::*, px,
};
use gpui_component::menu::DropdownMenu;
use gpui_component::{ActiveTheme, Selectable, h_flex};

use crate::icons::Icon;
use crate::right_panel::{focusable, tooltip_view};
use crate::theme::{hsla, metrics, palette};

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type MouseMoveHandler = Rc<dyn Fn(&MouseMoveEvent, &mut Window, &mut App)>;

pub const ROW_HEIGHT: f32 = 32.;
/// Sections pad 8px on each side, so a popup anchored to a row is the card minus 16.
pub const ROW_WIDTH: f32 = metrics::THREAD_DETAILS_PANEL_WIDTH - 16.;
const DISABLED_OPACITY: f32 = 0.64;

/// The soft fill rows take on hover: black at 3.5% (white at 8% in dark), from `foreground`.
pub fn hover_fill(cx: &App) -> Hsla {
    hsla(palette(cx).foreground).opacity(if cx.theme().is_dark() { 0.08 } else { 0.035 })
}

fn label_color(cx: &App) -> Hsla {
    hsla(palette(cx).foreground).opacity(0.8)
}

/// A full-width row (`part="row"`; `part="select"` adds the chevron).
#[derive(IntoElement)]
pub struct Row {
    id: ElementId,
    base: Stateful<Div>,
    icon: Option<Icon>,
    label: SharedString,
    trailing: Option<AnyElement>,
    chevron: bool,
    opens_menu: bool,
    open: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<Handler>,
}

impl Row {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let id = id.into();
        Row { base: div().id(id.clone()), id, icon: None, label: label.into(), trailing: None, chevron: false, opens_menu: false, open: false, disabled: false, tooltip: None, on_click: None }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    pub fn select(mut self) -> Self {
        self.chevron = true;
        self
    }

    /// A row whose menu (`dropdown_menu`) opens on click, so it takes the hover fill.
    pub fn opens_menu(mut self) -> Self {
        self.opens_menu = true;
        self
    }

    /// Keeps the hover fill while the row's popup is open (`data-popup-open`).
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
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

fn row_icon(icon: Icon, cx: &App) -> impl IntoElement {
    icon.sized(px(16.)).text_color(hsla(palette(cx).muted_foreground))
}

fn row_label(label: SharedString) -> impl IntoElement {
    div().flex_1().min_w_0().truncate().child(label)
}

impl Styled for Row {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for Row {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for Row {}

impl Selectable for Row {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

/// A row that opens a menu, such as "Choose what to show".
impl DropdownMenu for Row {}

impl RenderOnce for Row {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let fill = hover_fill(cx);
        let interactive = !self.disabled && (self.on_click.is_some() || self.opens_menu);
        let p = palette(cx);
        focusable(self.base, self.id, interactive, window, cx)
            .flex()
            .items_center()
            .w_full()
            .min_w_0()
            .h(px(ROW_HEIGHT))
            .pl(px(10.))
            .pr(px(if self.chevron { 8. } else { 10. }))
            .gap(px(10.))
            .rounded(px(8.))
            .text_size(px(13.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(label_color(cx))
            .when(self.open, |this| this.bg(fill))
            .when(self.disabled, |this| this.opacity(DISABLED_OPACITY))
            .when(interactive, |this| this.cursor_pointer().hover(move |style| style.bg(fill)))
            .children(self.icon.map(|icon| row_icon(icon, cx)))
            .child(row_label(self.label))
            .children(self.trailing)
            .when(self.chevron, |this| this.child(Icon::ChevronDown.sized(px(16.)).text_color(hsla(p.muted_foreground))))
            .when_some(self.tooltip, |this, text| this.tooltip(move |window, cx| tooltip_view(text.clone(), window, cx)))
            .when_some(self.on_click.filter(|_| interactive), |this, handler| this.on_click(move |event, window, cx| handler(event, window, cx)))
    }
}

/// A split row (`THREAD_DETAILS_PANEL_SPLIT_*`): the primary action, a hairline that shows
/// while the pair is hovered, and the secondary half (a `MenuTrigger`, or the elapsed time).
pub fn split_row(id: impl Into<SharedString>, primary: impl IntoElement, secondary: impl IntoElement, cx: &App) -> impl IntoElement {
    let id = id.into();
    let fill = hover_fill(cx);
    let group: SharedString = format!("{id}-split").into();
    let separator = hsla(palette(cx).border).opacity(0.65);
    h_flex()
        .id(SharedString::from(format!("{id}-group")))
        .group(group.clone())
        .w_full()
        .min_w_0()
        .rounded(px(8.))
        .hover(move |style| style.bg(fill))
        .child(div().flex_1().min_w_0().child(primary))
        .child(div().flex_none().w(px(1.)).h(px(16.)).group_hover(group, move |style| style.bg(separator)))
        .child(secondary)
}

/// The chevron half of a split row that opens its menu (`part="secondary"`): 32px square.
#[derive(IntoElement)]
pub struct MenuTrigger {
    id: ElementId,
    base: Stateful<Div>,
    label: SharedString,
    selected: bool,
    disabled: bool,
}

impl MenuTrigger {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let id = id.into();
        MenuTrigger { base: div().id(id.clone()), id, label: label.into(), selected: false, disabled: false }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Styled for MenuTrigger {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for MenuTrigger {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for MenuTrigger {}

impl Selectable for MenuTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl DropdownMenu for MenuTrigger {}

impl RenderOnce for MenuTrigger {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let fill = hover_fill(cx);
        let enabled = !self.disabled;
        let label = self.label;
        focusable(self.base, self.id, enabled, window, cx)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(ROW_HEIGHT))
            .h(px(ROW_HEIGHT))
            .rounded_r(px(8.))
            .when(self.selected, |this| this.bg(fill))
            .when(self.disabled, |this| this.opacity(DISABLED_OPACITY))
            .when(enabled, |this| this.cursor_pointer().hover(move |style| style.bg(fill)))
            .child(Icon::ChevronDown.sized(px(16.)).text_color(hsla(p.muted_foreground)))
            .tooltip(move |window, cx| tooltip_view(label.clone(), window, cx))
    }
}

/// Our icon as a `gpui-component` menu icon.
pub fn menu_icon(icon: Icon) -> gpui_component::Icon {
    gpui_component::Icon::default().path(icon.path())
}

/// `ThreadDetailsSection`: 8px sides, 8 above and 10 below, an optional 11px heading, and a
/// hairline above when it follows another section.
pub fn section(separated: bool, heading: Option<SharedString>, cx: &App) -> gpui::Div {
    let p = palette(cx);
    div()
        .flex()
        .flex_col()
        .px(px(8.))
        .pt(px(8.))
        .pb(px(10.))
        .when(separated, |this| this.border_t_1().border_color(hsla(p.border).opacity(0.65)))
        .children(heading.map(|title| {
            div()
                .flex()
                .items_center()
                .min_h(px(32.))
                .mb(px(4.))
                .px(px(10.))
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.muted_foreground))
                .child(div().truncate().child(title))
        }))
}

/// A row in the branch picker (`menu.tsx` item): 28 min, 8 / 4 padding, highlighted `accent`.
#[derive(IntoElement)]
pub struct MenuRow {
    id: ElementId,
    label: SharedString,
    trailing: Option<AnyElement>,
    on_click: Option<Handler>,
    highlighted: Option<bool>,
    on_mouse_move: Option<MouseMoveHandler>,
}

impl MenuRow {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        MenuRow { id: id.into(), label: label.into(), trailing: None, on_click: None, highlighted: None, on_mouse_move: None }
    }

    /// End-aligned content: a shortcut, a badge, a running indicator.
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    pub fn highlighted(mut self, highlighted: bool) -> Self {
        self.highlighted = Some(highlighted);
        self
    }

    /// Movement, rather than entry, restores pointer selection after keyboard navigation.
    pub fn on_mouse_move(mut self, handler: impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_mouse_move = Some(Rc::new(handler));
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for MenuRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let enabled = self.on_click.is_some();
        focusable(div().id(self.id.clone()), self.id, enabled, window, cx)
            .flex()
            .items_center()
            .min_h(px(28.))
            .px(px(8.))
            .py(px(4.))
            .gap(px(8.))
            .rounded(px(4.))
            .text_sm()
            .text_color(hsla(p.foreground))
            .when(enabled, |this| this.cursor_pointer())
            .when(self.highlighted.is_none() && enabled, |this| this.hover(|style| style.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground))))
            .when(self.highlighted == Some(true), |this| this.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
            .when_some(self.on_mouse_move.filter(|_| enabled), |this, handler| {
                this.on_mouse_move(move |event, window, cx| handler(event, window, cx))
            })
            .child(div().flex_1().min_w_0().truncate().child(self.label))
            .children(self.trailing)
            .when_some(self.on_click.filter(|_| enabled), |this, handler| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx)
                })
            })
    }
}

/// A menu shortcut label: 12px, 500 weight, `secondary_label`.
pub fn menu_shortcut(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(palette(cx).secondary_label)).child(text.into())
}
