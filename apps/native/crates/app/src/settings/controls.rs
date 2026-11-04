//! Settings primitives: buttons, switch, select, and the page, section and row layout of
//! `web/src/components/settings/settingsLayout.tsx` and `SettingsGroup.tsx` (design system §5, §6,
//! §7.11). Each owns its look; callers set layout only. Sizes are rems, so the modal follows the
//! interface font size like the web.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, ElementId, FocusHandle, FontWeight, Hsla, IntoElement, KeyBinding, KeyDownEvent,
    ParentElement, Rems, RenderOnce, SharedString, Styled, Svg, Window, div, point, prelude::*, px, rems,
    svg,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, h_flex, v_flex};

use crate::icons::Icon;
use crate::theme::{hsla, palette};

const DISABLED_OPACITY: f32 = 0.64;

gpui::actions!(settings_select, [CloseSelect]);

/// Esc closes an open select before it reaches the modal.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", CloseSelect, Some("SettingsSelect"))]);
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type ValueHandler<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;

/// A web pixel length at the 16px root, as rems.
pub fn r(px: f32) -> Rems {
    rems(px / 16.)
}

pub fn icon(glyph: Icon, size: f32, color: Hsla) -> Svg {
    svg().path(glyph.path()).size(r(size)).flex_none().text_color(color)
}

pub fn tooltip(text: SharedString) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView {
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

fn control_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow { color: gpui::black().opacity(0.05), offset: point(px(0.), px(1.)), blur_radius: px(2.), spread_radius: px(0.), inset: false }]
}

/// The 2px focus ring keyboard focus paints around a control.
pub(super) fn focus_ring(color: Hsla) -> Vec<BoxShadow> {
    vec![BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(2.), inset: false }]
}

/// A control's Tab stop, kept per element id across renders.
pub(crate) fn focus_handle(id: &ElementId, window: &mut Window, cx: &mut App) -> FocusHandle {
    window.use_keyed_state((id.clone(), "focus"), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone()
}

/// Enter or Space, unmodified: what presses a focused control.
pub(crate) fn activates(event: &KeyDownEvent) -> bool {
    matches!(event.keystroke.key.as_str(), "enter" | "space")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    /// `primary` fill: the screen's main action.
    Default,
    Secondary,
    Ghost,
    GhostMuted,
    Outline,
    DestructiveOutline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSize {
    /// 20 square, 12px icon, radius 4.
    IconMicro,
    /// 24 square, 14px icon.
    IconXs,
    /// 28 square, 16px icon.
    IconSm,
    /// 32 square, 16px icon (`icon`).
    Icon,
    /// 36 square, 16px icon.
    IconLg,
    /// 24 tall, 12px text, 14px icon.
    Xs,
    /// 28 tall, 12px text, radius 6.
    Compact,
    /// 28 tall, 14px text.
    Sm,
    /// 32 tall, 14px text (`default`).
    Default,
}

/// `button.tsx` in the variants and sizes settings use.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    focus: Option<FocusHandle>,
    variant: ButtonVariant,
    size: ButtonSize,
    icon: Option<Icon>,
    trailing: Option<Icon>,
    label: Option<SharedString>,
    tooltip: Option<SharedString>,
    disabled: bool,
    full_width: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, variant: ButtonVariant, size: ButtonSize) -> Self {
        Button { id: id.into(), focus: None, variant, size, icon: None, trailing: None, label: None, tooltip: None, disabled: false, full_width: false, on_click: None }
    }

    pub fn focus_handle(mut self, focus: FocusHandle) -> Self {
        self.focus = Some(focus);
        self
    }

    /// Spans the parent's width with its content centered (`w-full`), which a parent cannot set
    /// on a component's own box.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// An icon after the label, such as a menu trigger's chevron.
    pub fn trailing(mut self, icon: Icon) -> Self {
        self.trailing = Some(icon);
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
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

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        // Focusable like a web button; GPUI turns Enter and Space on a focused element into a click.
        let focus = (!self.disabled).then(|| self.focus.unwrap_or_else(|| focus_handle(&self.id, window, cx)));
        let ring = focus.as_ref().is_some_and(|focus| focus.is_focused(window) && window.last_input_was_keyboard());
        let (height, pad, radius, icon_size, square) = match self.size {
            ButtonSize::IconMicro => (20., 0., 4., 12., true),
            ButtonSize::IconXs => (24., 0., 8., 14., true),
            ButtonSize::IconSm => (28., 0., 8., 16., true),
            ButtonSize::Icon => (32., 0., 8., 16., true),
            ButtonSize::IconLg => (36., 0., 8., 16., true),
            ButtonSize::Xs => (24., 7., 8., 14., false),
            ButtonSize::Compact => (28., 7., 6., 14., false),
            ButtonSize::Sm => (28., 9., 8., 16., false),
            ButtonSize::Default => (32., 11., 8., 16., false),
        };
        let transparent = gpui::transparent_black();
        let (fill, border, text, icon_color, hover_fill, hover_text) = match self.variant {
            ButtonVariant::Default => {
                let fill = hsla(p.primary);
                let text = hsla(p.primary_foreground);
                (fill, fill, text, text, fill.opacity(0.9), text)
            }
            ButtonVariant::Secondary => (hsla(p.secondary), transparent, hsla(p.secondary_foreground), hsla(p.secondary_foreground), hsla(p.secondary).opacity(0.9), hsla(p.secondary_foreground)),
            ButtonVariant::Ghost => (transparent, transparent, hsla(p.foreground), hsla(p.muted_foreground), hsla(p.accent), hsla(p.foreground)),
            ButtonVariant::GhostMuted => {
                (transparent, transparent, hsla(p.muted_foreground), hsla(p.muted_foreground), hsla(p.accent), hsla(p.foreground))
            }
            ButtonVariant::Outline | ButtonVariant::DestructiveOutline => {
                let fill = if dark { hsla(p.input).opacity(0.32) } else { hsla(p.popover) };
                let destructive = self.variant == ButtonVariant::DestructiveOutline;
                let hover = match (destructive, dark) {
                    (true, _) => hsla(p.destructive).opacity(0.04),
                    (false, true) => hsla(p.input).opacity(0.64),
                    (false, false) => hsla(p.accent).opacity(0.5),
                };
                let text = if destructive { hsla(p.destructive_foreground) } else { hsla(p.foreground) };
                let icon = if destructive { text } else { hsla(p.muted_foreground) };
                (fill, hsla(p.input), text, icon, hover, text)
            }
        };
        let outlined = matches!(self.variant, ButtonVariant::Outline | ButtonVariant::DestructiveOutline);
        let compact_text = matches!(self.size, ButtonSize::Compact | ButtonSize::Xs);
        h_flex()
            .id(self.id)
            .flex_none()
            .h(r(height))
            .when(square, |this| this.w(r(height)).justify_center())
            .when(self.full_width, |this| this.w_full().justify_center())
            .px(r(pad))
            .gap(r(match self.size {
                ButtonSize::Compact | ButtonSize::Xs => 4.,
                ButtonSize::Default => 8.,
                _ => 6.,
            }))
            .rounded(r(radius))
            .border_1()
            .border_color(border)
            .bg(fill)
            .text_color(text)
            .font_weight(FontWeight::MEDIUM)
            .map(|this| if compact_text { this.text_xs() } else { this.text_sm() })
            .when(outlined, |this| this.shadow(control_shadow()))
            .when_some(focus, |this, focus| this.track_focus(&focus))
            .when(ring, |this| this.shadow(focus_ring(hsla(p.ring))))
            .when(self.disabled, |this| this.opacity(DISABLED_OPACITY))
            .when(!self.disabled, |this| this.cursor_pointer().hover(move |this| this.bg(hover_fill).text_color(hover_text)))
            .children(self.icon.map(|glyph| icon(glyph, icon_size, icon_color)))
            .children(self.label.map(|label| div().truncate().child(label)))
            .children(self.trailing.map(|glyph| icon(glyph, icon_size, icon_color)))
            .when_some(self.tooltip, |this, text| this.tooltip(tooltip(text)))
            .when_some(self.on_click.filter(|_| !self.disabled), |this, handler| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx)
                })
            })
    }
}

/// `switch.tsx`: an 18×30 track with a 14px thumb that travels 12px. Snaps; no frame loop.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    disabled: bool,
    on_change: Option<ValueHandler<bool>>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Switch { id: id.into(), checked, disabled: false, on_change: None }
    }

    /// Shown at 64% and inert, as a read-only status.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let focus = (!self.disabled).then(|| focus_handle(&self.id, window, cx));
        let focused = focus.as_ref().is_some_and(|focus| focus.is_focused(window));
        let thumb = if dark && !self.checked { hsla(p.muted_foreground) } else { hsla(p.background) };
        let next = !self.checked;
        let disabled = self.disabled;
        let handler = self.on_change.filter(|_| !disabled);
        h_flex()
            .id(self.id)
            .when_some(focus, |this, focus| this.track_focus(&focus))
            .flex_none()
            .w(r(30.))
            .h(r(18.))
            .p(r(2.))
            .rounded_full()
            .bg(if self.checked { hsla(p.primary) } else { hsla(p.input) })
            .when(focused, |this| this.shadow(focus_ring(hsla(p.ring))))
            .when(disabled, |this| this.opacity(0.64))
            .when(!disabled, |this| this.cursor_pointer())
            .child(
                div()
                    .size(r(14.))
                    .rounded_full()
                    .bg(thumb)
                    .when(self.checked, |this| this.ml(r(12.)))
                    .shadow(vec![BoxShadow {
                        color: gpui::black().opacity(0.05),
                        offset: point(px(0.), px(1.)),
                        blur_radius: px(3.),
                        spread_radius: px(0.),
                        inset: false,
                    }]),
            )
            .when_some(handler, |this, handler| {
                let on_key = handler.clone();
                this.on_click(move |event, window, cx| {
                    if !event.is_keyboard() {
                        handler(next, window, cx)
                    }
                })
                .on_key_down(move |event, window, cx| {
                    if activates(event) {
                        cx.stop_propagation();
                        on_key(next, window, cx);
                    }
                })
            })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectOption {
    pub value: SharedString,
    pub label: SharedString,
    pub description: Option<SharedString>,
    /// A group heading shown above the first option of each group.
    pub group: Option<SharedString>,
    pub icon: Option<Icon>,
}

impl SelectOption {
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        SelectOption { value: value.into(), label: label.into(), description: None, group: None, icon: None }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn group(mut self, group: impl Into<SharedString>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
}

#[derive(Default)]
struct SelectState {
    open: bool,
    highlighted: Option<usize>,
    closed_at: Option<Instant>,
}

type SelectIconRenderer = Rc<dyn Fn(&str, &App) -> Option<AnyElement>>;

/// `select.tsx`: an input-box trigger with a 14px chevron, and a popup list below it aligned to
/// its end. Keyboard: Enter or Space opens, arrows move, Enter picks, Escape closes.
#[derive(IntoElement)]
pub struct Select {
    id: ElementId,
    options: Vec<SelectOption>,
    render_icon: Option<SelectIconRenderer>,
    selected: Option<SharedString>,
    label: Option<SharedString>,
    width: Option<f32>,
    popup_width: f32,
    show_check: bool,
    disabled: bool,
    on_change: Option<ValueHandler<SharedString>>,
}

impl Select {
    pub fn new(id: impl Into<ElementId>, options: Vec<SelectOption>) -> Self {
        Select {
            id: id.into(),
            options,
            render_icon: None,
            selected: None,
            label: None,
            width: None,
            popup_width: 160.,
            show_check: false,
            disabled: false,
            on_change: None,
        }
    }

    pub fn render_icon(mut self, render: impl Fn(&str, &App) -> Option<AnyElement> + 'static) -> Self {
        self.render_icon = Some(Rc::new(render));
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self { self.disabled = disabled; self }

    pub fn selected(mut self, value: impl Into<SharedString>) -> Self {
        self.selected = Some(value.into());
        self
    }

    /// What the trigger says when it is not the selected option's label ("Mixed", "None").
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self.popup_width = self.popup_width.max(width);
        self
    }

    pub fn popup_width(mut self, width: f32) -> Self {
        self.popup_width = width;
        self
    }

    /// Shows a check beside the selected option (the web's default indicator).
    pub fn show_check(mut self) -> Self {
        self.show_check = true;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(SharedString, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Select {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let disabled = self.disabled;
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let focus = focus_handle(&self.id, window, cx);
        let focused = focus.is_focused(window);
        let state = window.use_keyed_state((self.id.clone(), "state"), cx, |_, _| SelectState::default());
        let (open, highlighted) = {
            let state = state.read(cx);
            (state.open && !disabled, state.highlighted)
        };
        let selected_index = self.options.iter().position(|option| Some(&option.value) == self.selected.as_ref());
        let trigger_label = self
            .label
            .clone()
            .or_else(|| selected_index.map(|index| self.options[index].label.clone()))
            .unwrap_or_default();
        let trigger_icon = selected_index.and_then(|index| self.options[index].icon);
        let trigger_content = selected_index.and_then(|index| self.render_icon.as_ref().and_then(|render| render(&self.options[index].value, cx)));
        let options = Rc::new(self.options);
        let on_change = self.on_change;

        let pick = {
            let (state, options, on_change) = (state.clone(), options.clone(), on_change.clone());
            Rc::new(move |index: usize, window: &mut Window, cx: &mut App| {
                state.update(cx, |state, cx| {
                    state.open = false;
                    cx.notify();
                });
                if let (Some(option), Some(handler)) = (options.get(index), on_change.as_ref()) {
                    handler(option.value.clone(), window, cx);
                }
            })
        };
        let toggle = {
            let state = state.clone();
            move |cx: &mut App| {
                state.update(cx, |state, cx| {
                    if state.closed_at.is_some_and(|at| at.elapsed() < Duration::from_millis(250)) {
                        state.closed_at = None;
                        return;
                    }
                    state.open = !state.open;
                    state.highlighted = selected_index.or(Some(0));
                    cx.notify();
                });
            }
        };
        let on_key = {
            let (state, options, pick) = (state.clone(), options.clone(), pick.clone());
            move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                if disabled { return; }
                let count = options.len();
                let (open, highlighted) = {
                    let state = state.read(cx);
                    (state.open, state.highlighted)
                };
                match event.keystroke.key.as_str() {
                    "enter" | "space" if open => {
                        cx.stop_propagation();
                        if let Some(index) = highlighted {
                            pick(index, window, cx);
                        }
                    }
                    "enter" | "space" | "down" | "up" if !open => {
                        cx.stop_propagation();
                        state.update(cx, |state, cx| {
                            state.open = true;
                            state.highlighted = selected_index.or(Some(0));
                            cx.notify();
                        });
                    }
                    "down" | "up" if count > 0 => {
                        cx.stop_propagation();
                        let step = if event.keystroke.key == "down" { 1 } else { count - 1 };
                        state.update(cx, |state, cx| {
                            state.highlighted = Some(state.highlighted.map_or(0, |index| (index + step) % count));
                            cx.notify();
                        });
                    }
                    _ => {}
                }
            }
        };

        let trigger = h_flex()
            .id(self.id.clone())
            .when(!disabled, |view| view.track_focus(&focus))
            .when(disabled, |v| v.opacity(DISABLED_OPACITY))
            .min_h(r(28.))
            .when_some(self.width, |this, width| this.w(r(width)))
            .px(r(9.))
            .gap(r(8.))
            .rounded(r(8.))
            .border_1()
            .border_color(if focused || open { hsla(p.ring) } else { hsla(p.input) })
            .bg(if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) })
            .shadow(if focused { focus_ring(hsla(p.ring).opacity(0.24)) } else { control_shadow() })
            .text_sm()
            .text_color(hsla(p.foreground))
            .cursor_pointer()
            .key_context("SettingsSelect")
            .on_action({
                let state = state.clone();
                move |_: &CloseSelect, _, cx| {
                    if state.read(cx).open {
                        state.update(cx, |state, cx| {
                            state.open = false;
                            cx.notify();
                        });
                    } else {
                        cx.propagate();
                    }
                }
            })
            // Keys are handled on keydown; GPUI's keyup click would toggle the popup back.
            .on_click(move |event, _, cx| {
                if !disabled && !event.is_keyboard() {
                    toggle(cx)
                }
            })
            .on_key_down(on_key)
            .children(trigger_content)
            .children(trigger_icon.map(|glyph| icon(glyph, 14., hsla(p.icon_muted).opacity(0.8))))
            .child(div().flex_1().min_w_0().truncate().child(trigger_label))
            .child(icon(Icon::ChevronDown, 14., hsla(p.foreground).opacity(0.7)));

        let popup = open.then(|| {
            let close_state = state.clone();
            let mut list = v_flex().gap(px(0.));
            let mut previous_group: Option<SharedString> = None;
            for (index, option) in options.iter().enumerate() {
                if option.group.is_some() && option.group != previous_group {
                    previous_group = option.group.clone();
                    list = list.child(
                        div()
                            .px(r(8.))
                            .pt(r(index.min(1) as f32 * 6.))
                            .pb(r(4.))
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.muted_foreground))
                            .children(option.group.clone()),
                    );
                }
                let is_selected = selected_index == Some(index);
                let is_highlighted = highlighted == Some(index);
                let pick = pick.clone();
                let hover_state = state.clone();
                list = list.child(
                    h_flex()
                        .id(("option", index))
                        .min_h(r(28.))
                        .px(r(8.))
                        .py(r(4.))
                        .gap(r(8.))
                        .rounded(r(4.))
                        .text_sm()
                        .cursor_pointer()
                        .text_color(hsla(p.foreground))
                        .when(is_selected, |this| this.bg(hsla(p.foreground).opacity(0.08)))
                        .when(is_highlighted, |this| this.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                        .on_mouse_move(move |_, _, cx| {
                            hover_state.update(cx, |state, cx| {
                                if state.highlighted != Some(index) {
                                    state.highlighted = Some(index);
                                    cx.notify();
                                }
                            });
                        })
                        .on_click(move |_, window, cx| pick(index, window, cx))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(r(2.))
                                .child(
                                    h_flex()
                                        .gap(r(6.))
                                        .when(option.description.is_some(), |this| this.font_weight(FontWeight::MEDIUM))
                                        .children(self.render_icon.as_ref().and_then(|render| render(&option.value, cx)))
                                        .children(option.icon.map(|glyph| icon(glyph, 14., hsla(p.muted_foreground))))
                                        .child(div().truncate().child(option.label.clone())),
                                )
                                .children(option.description.clone().map(|description| {
                                    div().text_xs().line_height(r(16.)).text_color(hsla(p.muted_foreground)).child(description)
                                })),
                        )
                        .when(self.show_check, |this| {
                            this.child(div().size(r(16.)).flex_none().children(is_selected.then(|| icon(Icon::Check, 16., hsla(p.foreground)))))
                        }),
                );
            }
            div().absolute().bottom_0().right_0().child(
                gpui::deferred(
                    gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
                        div()
                            .id((self.id.clone(), "popup"))
                            .occlude()
                            .mt(r(4.))
                            .min_w(r(self.popup_width))
                            .max_h(r(320.))
                            .overflow_y_scroll()
                            .p(r(4.))
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.dropdown_border))
                            .bg(hsla(p.popover))
                            .shadow(vec![BoxShadow {
                                color: gpui::black().opacity(if dark { 0.8 } else { 0.55 }),
                                offset: point(px(0.), px(if dark { 18. } else { 16. })),
                                blur_radius: px(if dark { 44. } else { 40. }),
                                spread_radius: px(-18.),
                                inset: false,
                            }])
                            .on_mouse_down_out(move |_, _, cx| {
                                close_state.update(cx, |state, cx| {
                                    state.open = false;
                                    state.closed_at = Some(Instant::now());
                                    cx.notify();
                                });
                            })
                            .child(list),
                    ),
                )
                .with_priority(2),
            )
        });

        div().relative().flex_none().child(trigger).children(popup)
    }
}

/// `SettingsSection` (grouped): a 16px semibold title above a bordered group whose rows divide.
pub fn section(title: impl Into<SharedString>, rows: Vec<AnyElement>, cx: &App) -> gpui::Div {
    v_flex().gap(r(10.)).child(section_title(title)).child(group(rows, cx))
}

pub fn section_title(title: impl Into<SharedString>) -> gpui::Div {
    h_flex().min_h(r(28.)).px(r(16.)).text_base().font_weight(FontWeight::SEMIBOLD).child(title.into())
}

/// `SettingsUnavailableGroup`: a visible explanation above dimmed, inert rows.
pub fn unavailable_group(message: &'static str, rows: Vec<AnyElement>, cx: &App) -> gpui::Div {
    let p = palette(cx);
    v_flex()
        .border_color(hsla(p.border).opacity(0.6))
        .bg(hsla(p.muted).opacity(0.2))
        .py(r(6.))
        .child(
            h_flex()
                .items_start()
                .gap(r(8.))
                .px(r(16.))
                .py(r(8.))
                .child(icon(Icon::Info, 14., hsla(p.warning)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .text_xs()
                        .line_height(r(18.))
                        .text_color(hsla(p.muted_foreground))
                        .child(message),
                ),
        )
        .child(v_flex().opacity(0.64).children(rows))
}

/// `SettingsGroup`: radius 12, `border` at 60%, `card` at 40%, rows divided by `border` at 50%.
pub fn group(rows: Vec<AnyElement>, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let divider = hsla(p.border).opacity(0.5);
    let count = rows.len();
    v_flex()
        .rounded(r(12.))
        .border_1()
        .border_color(hsla(p.border).opacity(0.6))
        .bg(hsla(p.card).opacity(0.4))
        .shadow(control_shadow())
        .children(rows.into_iter().enumerate().map(move |(index, row)| {
            div().when(index > 0 && index < count, |this| this.border_t_1().border_color(divider)).child(row)
        }))
}

/// `SettingsRow`: title, optional reset and description on the left; the control on the
/// right; optional content below.
#[derive(IntoElement)]
pub struct Row {
    id: SharedString,
    title: SharedString,
    title_icon: Option<Icon>,
    title_tooltip: Option<SharedString>,
    description: Option<SharedString>,
    description_content: Option<AnyElement>,
    status: Option<SharedString>,
    control: Option<AnyElement>,
    reset: Option<ClickHandler>,
    reset_tooltip: Option<SharedString>,
    below: Option<AnyElement>,
    highlighted: bool,
    unavailable: Option<SharedString>,
}

impl Row {
    pub fn new(id: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Row {
            id: id.into(),
            title: title.into(),
            title_icon: None,
            title_tooltip: None,
            description: None,
            description_content: None,
            status: None,
            control: None,
            reset: None,
            reset_tooltip: None,
            below: None,
            highlighted: false,
            unavailable: None,
        }
    }

    /// `SettingsRow`'s inert control: dimmed, unclickable, and saying why on hover (a grant this
    /// connection lacks). The reset button hides too.
    pub fn unavailable(mut self, message: Option<impl Into<SharedString>>) -> Self {
        self.unavailable = message.map(Into::into);
        self
    }

    /// A mark before the title, such as a secret provider's brand (18px, `foreground` at 80%).
    pub fn title_icon(mut self, icon: Icon) -> Self {
        self.title_icon = Some(icon);
        self
    }

    /// `PolicyTooltip`: an info button after the title that explains the setting on hover.
    pub fn title_tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.title_tooltip = Some(text.into());
        self
    }

    /// A description with its own parts, in the description's place and type.
    pub fn description_content(mut self, content: impl IntoElement) -> Self {
        self.description_content = Some(content.into_any_element());
        self
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// `SettingsRow`'s `status`: a muted line under the description.
    pub fn status(mut self, status: impl Into<SharedString>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// `SettingResetButton`, shown while the value differs from its default.
    pub fn reset(mut self, visible: bool, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        if visible {
            self.reset = Some(Rc::new(handler));
        }
        self
    }

    /// The reset button's tooltip when it is not "Reset to default".
    pub fn reset_tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.reset_tooltip = Some(text.into());
        self
    }

    pub fn below(mut self, content: impl IntoElement) -> Self {
        self.below = Some(content.into_any_element());
        self
    }

    /// A settings-search jump landed here.
    pub fn highlighted(mut self, highlighted: bool) -> Self {
        self.highlighted = highlighted;
        self
    }
}

impl RenderOnce for Row {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let stacked = ModalLayout::of(window).rows_stacked;
        let has_below = self.below.is_some();
        let reset_tooltip = self.reset_tooltip.unwrap_or_else(|| "Reset to default".into());
        let unavailable = self.unavailable;
        let reset = self.reset.filter(|_| unavailable.is_none()).map(|handler| {
            Button::new(ElementId::Name(format!("{}-reset", self.id).into()), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                .icon(Icon::Undo2)
                .tooltip(reset_tooltip)
                .on_click(move |event, window, cx| handler(event, window, cx))
        });
        v_flex()
            .id(ElementId::Name(self.id.clone()))
            .px(r(16.))
            .pt(r(12.))
            .pb(r(if has_below { 4. } else { 12. }))
            .rounded(r(12.))
            .when(self.highlighted, |this| this.bg(hsla(p.accent).opacity(0.6)))
            .child(
                div()
                    .w_full()
                    .flex()
                    .map(|this| if stacked { this.flex_col().gap(r(12.)) } else { this.flex_row().items_center().gap(r(32.)) })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(r(4.))
                            .child(
                                h_flex()
                                    .min_h(r(20.))
                                    .gap(r(6.))
                                    .children(self.title_icon.map(|glyph| icon(glyph, 18., hsla(p.foreground).opacity(0.8))))
                                    .child(div().min_w_0().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(self.title))
                                    .children(self.title_tooltip.map(|text| {
                                        Button::new(ElementId::Name(format!("{}-details", self.id).into()), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                                            .icon(Icon::Info)
                                            .tooltip(text)
                                    }))
                                    .child(div().size(r(20.)).flex().items_center().justify_center().children(reset)),
                            )
                            .children(self.description.map(|description| description.into_any_element()).or(self.description_content).map(|description| {
                                div().max_w(r(672.)).text_xs().line_height(r(18.)).text_color(hsla(p.muted_foreground)).child(description)
                            }))
                            .children(self.status.map(|status| div().pt(r(2.)).text_xs().text_color(hsla(p.muted_foreground)).child(status))),
                    )
                    .children(self.control.map(|control| {
                        let id = self.id.clone();
                        h_flex()
                            .gap(r(8.))
                            .map(|this| if stacked { this.w_full().min_w_0() } else { this.flex_none().max_w(r(420.)).justify_end() })
                            .map(|this| match unavailable {
                                Some(message) => this.child(
                                    div()
                                        .id(ElementId::Name(format!("{id}-unavailable").into()))
                                        .relative()
                                        .child(div().opacity(0.5).child(control))
                                        .child(div().absolute().inset_0().occlude())
                                        .tooltip(tooltip(message)),
                                ),
                                None => this.child(control),
                            })
                    })),
            )
            .children(self.below)
    }
}

/// How the modal fits the window (`AppRouteModal.tsx`, `SettingsSidebarNav.tsx`,
/// `settingsLayout.tsx`). Breakpoints are viewport px; sizes are rems.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModalLayout {
    /// Below 640px the modal fills the window without padding, border or radius.
    pub full_bleed: bool,
    pub inset: Rems,
    /// Below 768px the nav collapses to a 56px icon rail.
    pub nav_compact: bool,
    /// A row's content box is under 32rem, so its control drops below the title. GPUI has no
    /// container queries, so the width is derived from the window the way the modal lays out.
    pub rows_stacked: bool,
}

impl ModalLayout {
    pub fn of(window: &Window) -> Self {
        let rem = f32::from(window.rem_size()) / 16.;
        let viewport = f32::from(window.viewport_size().width);
        let full_bleed = viewport < 640.;
        let inset = if full_bleed { 0. } else if viewport >= 1024. { 40. } else { 24. };
        let nav_compact = viewport < 768.;
        let modal = (viewport - 2. * inset * rem).min(1280. * rem) - 2.;
        let nav = if nav_compact { 56. } else { 240. } * rem + 1.;
        let block = (modal - nav).min(896. * rem) - 48. * rem;
        let row_content = block - 2. - 32. * rem;
        ModalLayout { full_bleed, inset: r(inset), nav_compact, rows_stacked: row_content < 512. * rem }
    }
}

/// `InlineButton`'s tones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineTone {
    Muted,
    Destructive,
}

/// `InlineButton`: text-sized, 500 weight, underlined on hover; dimmed to 64% when disabled.
#[derive(IntoElement)]
pub struct InlineButton {
    id: ElementId,
    label: SharedString,
    tone: InlineTone,
    tooltip: Option<SharedString>,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl InlineButton {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, tone: InlineTone) -> Self {
        InlineButton { id: id.into(), label: label.into(), tone, tooltip: None, disabled: false, on_click: None }
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
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

impl RenderOnce for InlineButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let (text, hover) = match self.tone {
            InlineTone::Muted => (hsla(p.muted_foreground), hsla(p.foreground)),
            InlineTone::Destructive => (hsla(p.destructive_foreground), hsla(p.destructive_foreground)),
        };
        div()
            .id(self.id)
            .flex_none()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(text)
            .when(self.disabled, |this| this.opacity(DISABLED_OPACITY))
            .when(!self.disabled, |this| this.cursor_pointer().hover(move |this| this.text_color(hover).underline()))
            .child(self.label)
            .when_some(self.tooltip, |this, text| this.tooltip(tooltip(text)))
            .when_some(self.on_click.filter(|_| !self.disabled), |this, handler| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx)
                })
            })
    }
}

/// A dialog over the settings modal (design system §6.4): backdrop, dialog glass, a header with
/// title and description, a scrolling panel and a footer. A click on the backdrop dismisses;
/// Escape reaches `SettingsView::on_close`, which closes the open dialog first.
pub struct DialogFrame {
    pub id: &'static str,
    pub title: SharedString,
    pub description: SharedString,
    pub max_width: f32,
    pub body: AnyElement,
    pub footer: AnyElement,
    pub focus: FocusHandle,
}

impl DialogFrame {
    pub fn render(self, window: &Window, on_dismiss: impl Fn(&mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
        self.render_inner(window,3,false,on_dismiss,cx)
    }

    pub fn render_modal(self,window:&Window,priority:usize,on_dismiss:impl Fn(&mut Window,&mut App)+'static,cx:&App)->AnyElement {
        self.render_inner(window,priority,true,on_dismiss,cx)
    }

    fn render_inner(self,window:&Window,priority:usize,close:bool,on_dismiss:impl Fn(&mut Window,&mut App)+'static,cx:&App)->AnyElement {
        let on_dismiss=std::rc::Rc::new(on_dismiss);
        let dismiss_button=on_dismiss.clone();
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let viewport = window.viewport_size();
        let dialog = v_flex()
            .id(self.id)
            .track_focus(&self.focus)
            .occlude()
            .relative()
            .w_full()
            .max_w(r(self.max_width))
            .max_h(viewport.height - px(32.))
            .rounded(r(16.))
            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .shadow(vec![BoxShadow {
                color: gpui::black().opacity(if dark { 0.9 } else { 0.65 }),
                offset: point(px(0.), px(24.)),
                blur_radius: px(64.),
                spread_radius: px(-24.),
                inset: false,
            }])
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .p(r(24.))
                    .gap(r(8.))
                    .child(div().text_xl().line_height(r(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child(self.title))
                    .when(!self.description.is_empty(), |this| this.child(div().text_sm().text_color(hsla(p.muted_foreground)).child(self.description))),
            )
            .child(div().id(ElementId::Name(format!("{}-panel", self.id).into())).flex_1().min_h_0().overflow_y_scroll().px(r(24.)).pt(r(4.)).pb(r(24.)).child(self.body))
            .child(self.footer)
            .when(close,|v|v.child(div().absolute().top(r(8.)).right(r(8.)).child(
                Button::new("modal-dialog-close",ButtonVariant::Ghost,ButtonSize::Icon).icon(Icon::X).tooltip("Close")
                    .on_click(move|_,window,cx|dismiss_button(window,cx)))));
        gpui::deferred(
            gpui::anchored().position_mode(gpui::AnchoredPositionMode::Window).position(point(px(0.), px(0.))).child(
                div()
                    .id(ElementId::Name(format!("{}-backdrop", self.id).into()))
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(16.))
                    .bg(hsla(p.dialog_backdrop))
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| on_dismiss(window, cx))
                    .child(dialog),
            ),
        )
        .with_priority(priority)
        .into_any_element()
    }
}

/// The dialog footer: end-aligned buttons, 16 / 24 padding, a top border over `muted` at 72%
/// (`bare` drops the border and fill).
pub fn dialog_footer(bare: bool, buttons: Vec<AnyElement>, cx: &App) -> gpui::Div {
    let p = palette(cx);
    h_flex()
        .justify_end()
        .gap(r(8.))
        .px(r(24.))
        .py(r(16.))
        .when(!bare, |this| this.border_t_1().border_color(hsla(p.border)).rounded_b(r(15.)).bg(hsla(p.muted).opacity(0.72)))
        .children(buttons)
}

/// A row's muted plain-text stand-in for a control ("No providers available").
pub fn muted_text(text: impl Into<SharedString>, cx: &App) -> gpui::Div {
    div().text_sm().text_color(hsla(palette(cx).muted_foreground)).child(text.into())
}

#[derive(IntoElement)]
pub struct PercentSlider {
    state: gpui::Entity<gpui_component::slider::SliderState>,
    value: u32,
}
impl PercentSlider {
    pub fn new(state: gpui::Entity<gpui_component::slider::SliderState>, value:u32) -> Self {Self {state,value}}
}
impl RenderOnce for PercentSlider {
    fn render(self,window:&mut Window,cx:&mut App)->impl IntoElement {
        let p=palette(cx);
        if self.state.read(cx).value().start()!=self.value as f32 {
            self.state.update(cx,|s,cx|s.set_value(self.value as f32,window,cx));
        }
        h_flex().w(r(208.)).gap(r(12.)).child(
            div().min_w(r(48.)).rounded(r(6.)).bg(hsla(p.muted)).px(r(8.)).py(r(4.))
                .font_family(crate::theme::font_family(t3_core::client_settings::typography::FontRole::Code,cx))
                .text_xs().text_color(hsla(p.foreground)).child(format!("{}%",self.value))
        ).child(div().flex_1().child(gpui_component::slider::Slider::new(&self.state)))
    }
}
