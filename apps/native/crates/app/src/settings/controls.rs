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

use crate::components::motion::{Spinner, transition};
use crate::icons::Icon;
use crate::theme::{
    SlotState, ThemeSlot, ThemeSlotExt, hsla, palette, refine_slot_active, refine_slot_hover,
    slot_border_color, slot_has_active, slot_has_background, slot_has_hover,
    theme_slot_clipped_background_layer, theme_slot_state_background_layer,
};

const DISABLED_OPACITY: f32 = 0.64;
pub(crate) const SELECT_POPUP_PRIORITY: usize = 75;
pub(crate) const DIALOG_MODAL_PRIORITY: usize = 3;

gpui::actions!(settings_select, [CloseSelect, CloseInheritance]);

/// Esc closes an open select before it reaches the modal.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", CloseSelect, Some("SettingsSelect")),
        KeyBinding::new("escape", CloseInheritance, Some("SettingsInheritance")),
    ]);
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
    move |window, cx| {
        if theme_slot_clipped_background_layer(ThemeSlot::TooltipPopup, px(9.), cx).is_some() {
            let label = text.clone();
            return Tooltip::element(move |_, cx| {
                let p = palette(cx);
                div()
                    .relative()
                    .px(px(10.))
                    .py(px(5.))
                    .rounded(px(9.))
                    .text_xs()
                    .text_color(hsla(p.popover_foreground))
                    .theme_slot(ThemeSlot::TooltipPopup, cx)
                    .children(theme_slot_clipped_background_layer(ThemeSlot::TooltipPopup, px(9.), cx))
                    .child(div().relative().child(label.clone()))
            })
            .p_0()
            .border_0()
            .bg(gpui::transparent_black())
            .shadow_none()
            .build(window, cx);
        }
        Tooltip::new(text.clone()).theme_slot(ThemeSlot::TooltipPopup, cx).build(window, cx)
    }
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
    /// Solid destructive fill with white foreground.
    Destructive,
    /// Muted ghost button that turns destructive on hover or press.
    GhostDestructive,
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
    icon_color: Option<Hsla>,
    loading: bool,
    trailing: Option<Icon>,
    label: Option<SharedString>,
    tooltip: Option<SharedString>,
    disabled: bool,
    full_width: bool,
    justify_start: bool,
    justify_between: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, variant: ButtonVariant, size: ButtonSize) -> Self {
        Button { id: id.into(), focus: None, variant, size, icon: None, icon_color: None, loading: false, trailing: None, label: None, tooltip: None, disabled: false, full_width: false, justify_start: false, justify_between: false, on_click: None }
    }

    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn icon_color(mut self, color: Hsla) -> Self {
        self.icon_color = Some(color);
        self
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

    /// Spans the parent's width with its content left-aligned (`w-full justify-start`).
    pub fn justify_start(mut self) -> Self {
        self.justify_start = true;
        self
    }

    /// Fills the available width, aligning the label left and the trailing icon right.
    pub fn justify_between(mut self) -> Self {
        self.justify_between = true;
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
            ButtonVariant::Default | ButtonVariant::Destructive => {
                let (fill, text) = if self.variant == ButtonVariant::Destructive {
                    (hsla(p.destructive), hsla(crate::theme::WHITE))
                } else {
                    (hsla(p.primary), hsla(p.primary_foreground))
                };
                (fill, fill, text, text, fill.opacity(0.9), text)
            }
            ButtonVariant::Secondary => (hsla(p.secondary), transparent, hsla(p.secondary_foreground), hsla(p.secondary_foreground), hsla(p.secondary).opacity(0.9), hsla(p.secondary_foreground)),
            ButtonVariant::Ghost => (transparent, transparent, hsla(p.foreground), hsla(p.muted_foreground), hsla(p.accent), hsla(p.foreground)),
            ButtonVariant::GhostMuted => {
                (transparent, transparent, hsla(p.muted_foreground), hsla(p.muted_foreground), hsla(p.accent), hsla(p.foreground))
            }
            ButtonVariant::GhostDestructive => (transparent, transparent, hsla(p.muted_foreground), hsla(p.muted_foreground), hsla(p.accent), hsla(p.destructive)),
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
        let accessible_label = self.label.as_ref().or(self.tooltip.as_ref()).map(|label| {
            crate::i18n::t(cx, label.as_ref()).to_owned()
        });
        let ghost_destructive = self.variant == ButtonVariant::GhostDestructive;
        let render_icon = |glyph: Icon, color: Option<Hsla>| {
            let svg = glyph.svg().size(r(icon_size));
            match color {
                Some(color) => svg.text_color(color),
                None if ghost_destructive => svg,
                None => svg.text_color(icon_color),
            }
        };
        let spinner_id = (self.id.clone(), "spinner");
        let spinner_size = r(icon_size).to_pixels(window.rem_size());
        let spinner_color = self.icon_color.unwrap_or(icon_color);
        let leading = if self.loading {
            Some(Spinner::new(spinner_id, spinner_size, spinner_color).into_any_element())
        } else {
            self.icon.map(|glyph| render_icon(glyph, self.icon_color).into_any_element())
        };
        h_flex()
            .id(self.id)
            .relative()
            .role(gpui::Role::Button)
            .when_some(accessible_label, |this, label| this.aria_label(label))
            .flex_none()
            .h(r(height))
            .when(square, |this| this.w(r(height)).justify_center())
            .when(self.full_width, |this| this.w_full().justify_center())
            .when(self.justify_start, |this| this.w_full().justify_start())
            .when(self.justify_between, |this| this.w_full().justify_between())
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
            .theme_slot(ThemeSlot::Button, cx)
            .when(self.disabled, |this| this.opacity(DISABLED_OPACITY))
            .when(!self.disabled, |this| {
                this.cursor_pointer()
                    .hover(|this| refine_slot_hover(this.bg(hover_fill).text_color(hover_text), ThemeSlot::Button, cx))
                    .when(ghost_destructive || slot_has_active(ThemeSlot::Button, cx), |this| {
                        this.active(|style| {
                            let style = if ghost_destructive { style.bg(hover_fill).text_color(hover_text) } else { style };
                            refine_slot_active(style, ThemeSlot::Button, cx)
                        })
                    })
            })
            .children(theme_slot_clipped_background_layer(ThemeSlot::Button, r(radius), cx))
            .children(crate::theme::inspector_slot_probe(ThemeSlot::Button, cx))
            .children(leading)
            .children(self.label.map(|label| {
                div().relative().min_w_0().truncate()
                    .when(self.justify_between, |label| label.flex_1())
                    .child(crate::i18n::t(cx, label.as_ref()).to_owned())
            }))
            .children(self.trailing.map(|glyph| render_icon(glyph, None)))
            .when_some(self.tooltip, |this, text| this.tooltip(tooltip(crate::i18n::t(cx, text.as_ref()).to_owned().into())))
            .when_some(self.on_click.filter(|_| !self.disabled), |this, handler| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx)
                })
            })
    }
}

/// `switch.tsx`: an 18×30 track with a 14px sliding thumb.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    mixed: bool,
    disabled: bool,
    on_change: Option<ValueHandler<bool>>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Switch { id: id.into(), checked, mixed: false, disabled: false, on_change: None }
    }

    /// Centers and dims the thumb for an indeterminate value; activation sets every value on.
    pub fn mixed(mut self, mixed: bool) -> Self {
        self.mixed = mixed;
        self
    }

    fn selected(&self) -> bool {
        self.checked && !self.mixed
    }

    fn next_checked(&self) -> bool {
        !self.selected()
    }

    fn thumb_offset(&self) -> f32 {
        match (self.mixed, self.checked) {
            (true, _) => 6.,
            (false, true) => 12.,
            (false, false) => 0.,
        }
    }

    fn toggled(&self) -> gpui::Toggled {
        match (self.mixed, self.checked) {
            (true, _) => gpui::Toggled::Mixed,
            (false, true) => gpui::Toggled::True,
            (false, false) => gpui::Toggled::False,
        }
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
        let selected = self.selected();
        let toggled = self.toggled();
        let offset = transition((self.id.clone(), "switch-thumb"), self.thumb_offset(), Duration::from_millis(120), window, cx);
        let thumb = if dark && !selected { hsla(p.muted_foreground) } else { hsla(p.background) };
        let next = self.next_checked();
        let disabled = self.disabled;
        let handler = self.on_change.filter(|_| !disabled);
        let switch_state = if selected { SlotState::Selected } else { SlotState::Unchecked };
        let switch_bg_layer =
            theme_slot_state_background_layer(ThemeSlot::Switch, switch_state, r(9999.), cx);
        h_flex()
            .id(self.id)
            .relative()
            .when_some(focus, |this, focus| this.track_focus(&focus))
            .flex_none()
            .w(r(30.))
            .h(r(18.))
            .p(r(2.))
            .rounded_full()
            .bg(if selected { hsla(p.primary) } else { hsla(p.input) })
            .role(gpui::Role::Switch)
            .aria_toggled(toggled)
            .when(focused, |this| this.shadow(focus_ring(hsla(p.ring))))
            .theme_slot(ThemeSlot::Switch, cx)
            .theme_slot_state(ThemeSlot::Switch, switch_state, cx)
            .when(focused, |this| this.theme_slot_state(ThemeSlot::Switch, SlotState::FocusWithin, cx))
            .when(disabled, |this| this.opacity(0.64))
            .when(!disabled, |this| {
                this.cursor_pointer()
                    .when(slot_has_hover(ThemeSlot::Switch, cx), |this| {
                        this.hover(|style| refine_slot_hover(style, ThemeSlot::Switch, cx))
                    })
                    .when(slot_has_active(ThemeSlot::Switch, cx), |this| {
                        this.active(|style| refine_slot_active(style, ThemeSlot::Switch, cx))
                    })
            })
            .children(switch_bg_layer)
            .children(crate::theme::inspector_slot_probe(ThemeSlot::Switch, cx))
            .child(
                div()
                    .relative()
                    .size(r(14.))
                    .rounded_full()
                    .bg(thumb)
                    .ml(r(offset))
                    .when(self.mixed, |this| this.opacity(0.7))
                    .shadow(vec![BoxShadow {
                        color: gpui::black().opacity(0.05),
                        offset: point(px(0.), px(1.)),
                        blur_radius: px(3.),
                        spread_radius: px(0.),
                        inset: false,
                    }])
                    .theme_slot(ThemeSlot::SwitchThumb, cx)
                    .theme_slot_state(ThemeSlot::SwitchThumb, switch_state, cx),
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
    /// When set, the option cannot be selected and shows this tooltip (`useScopedModelDisabledReason`).
    pub disabled_reason: Option<SharedString>,
}

impl SelectOption {
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        SelectOption { value: value.into(), label: label.into(), description: None, group: None, icon: None, disabled_reason: None }
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

    pub fn disabled_reason(mut self, reason: Option<impl Into<SharedString>>) -> Self {
        self.disabled_reason = reason.map(Into::into);
        self
    }
}

#[derive(Default)]
struct SelectState {
    open: bool,
    highlighted: Option<usize>,
    closed_at: Option<Instant>,
}

#[derive(Default)]
struct OpenSelects(Vec<gpui::WeakEntity<SelectState>>);
impl gpui::Global for OpenSelects {}

fn note_select_open(state: &gpui::Entity<SelectState>, open: bool, cx: &mut App) {
    let id = state.entity_id();
    let weak = state.downgrade();
    let candidates = cx
        .try_global::<OpenSelects>()
        .map(|g| g.0.clone())
        .unwrap_or_default();
    let mut retained: Vec<_> = candidates
        .into_iter()
        .filter(|entry| entry.entity_id() != id && entry.upgrade().is_some_and(|e| e.read(cx).open))
        .collect();
    if open {
        retained.push(weak);
    }
    cx.set_global(OpenSelects(retained));
}

pub(super) fn dismiss_open_select(cx: &mut App) -> bool {
    let entries: Vec<_> = cx
        .try_global::<OpenSelects>()
        .map(|g| g.0.iter().filter_map(|w| w.upgrade()).collect())
        .unwrap_or_default();
    let mut closed = false;
    for entry in entries {
        if entry.read(cx).open {
            entry.update(cx, |state, cx| {
                state.open = false;
                cx.notify();
            });
            closed = true;
        }
    }
    if cx.has_global::<OpenSelects>() {
        cx.global_mut::<OpenSelects>().0.clear();
    }
    closed
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
    trigger_chevron_only: bool,
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
            trigger_chevron_only: false,
            disabled: false,
            on_change: None,
        }
    }

    pub fn trigger_chevron_only(mut self) -> Self {
        self.trigger_chevron_only = true;
        self
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
        note_select_open(&state, open, cx);
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
                let Some(option) = options.get(index) else { return };
                if option.disabled_reason.is_some() {
                    return;
                }
                state.update(cx, |state, cx| {
                    state.open = false;
                    cx.notify();
                });
                if let Some(handler) = on_change.as_ref() {
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
            .theme_slot(ThemeSlot::SelectTrigger, cx)
            .when(focused || open, |this| {
                this.theme_slot_state(ThemeSlot::SelectTrigger, SlotState::FocusWithin, cx)
            })
            .when(disabled, |v| v.opacity(DISABLED_OPACITY))
            .cursor_pointer()
            .when(!disabled && slot_has_hover(ThemeSlot::SelectTrigger, cx), |this| {
                this.hover(|style| refine_slot_hover(style, ThemeSlot::SelectTrigger, cx))
            })
            .when(!disabled && slot_has_active(ThemeSlot::SelectTrigger, cx), |this| {
                this.active(|style| refine_slot_active(style, ThemeSlot::SelectTrigger, cx))
            })
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
            .when(!self.trigger_chevron_only, |this| {
                this.children(trigger_content)
                    .children(trigger_icon.map(|glyph| icon(glyph, 14., hsla(p.icon_muted).opacity(0.8))))
                    .when(!trigger_label.is_empty(), |this| {
                        this.child(div().flex_1().min_w_0().truncate().child(crate::i18n::t(cx, trigger_label.as_ref()).to_owned()))
                    })
            })
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
                            .children(option.group.as_ref().map(|group| crate::i18n::t(cx, group.as_ref()).to_owned())),
                    );
                }
                let is_selected = selected_index == Some(index);
                let option_disabled = option.disabled_reason.is_some();
                let is_highlighted = !option_disabled && highlighted == Some(index);
                let pick = pick.clone();
                let hover_state = state.clone();
                let disabled_tip = option.disabled_reason.clone();
                list = list.child(
                    h_flex()
                        .id(("option", index))
                        .min_h(r(28.))
                        .px(r(8.))
                        .py(r(4.))
                        .gap(r(8.))
                        .rounded(r(4.))
                        .text_sm()
                        .when(!option_disabled, |this| this.cursor_pointer())
                        .when(option_disabled, |this| this.opacity(0.5))
                        .text_color(hsla(p.foreground))
                        .when(is_selected, |this| this.bg(hsla(p.foreground).opacity(0.08)))
                        .when(is_highlighted, |this| this.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                        .when_some(disabled_tip, |this, tip| this.tooltip(tooltip(crate::i18n::t(cx, tip.as_ref()).to_owned().into())))
                        .when(!option_disabled, |this| {
                            this.on_mouse_move(move |_, _, cx| {
                                hover_state.update(cx, |state, cx| {
                                    if state.highlighted != Some(index) {
                                        state.highlighted = Some(index);
                                        cx.notify();
                                    }
                                });
                            })
                            .on_click(move |_, window, cx| pick(index, window, cx))
                        })
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
                                        .child(div().truncate().child(crate::i18n::t(cx, option.label.as_ref()).to_owned())),
                                )
                                .children(option.description.as_ref().map(|description| {
                                    div().text_xs().line_height(r(16.)).text_color(hsla(p.muted_foreground)).child(crate::i18n::t(cx, description.as_ref()).to_owned())
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
                            .relative()
                            .flex()
                            .flex_col()
                            .occlude()
                            .mt(r(4.))
                            .min_w(r(self.popup_width))
                            .max_h(r(320.))
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
                            .theme_slot(ThemeSlot::PopoverPopup, cx)
                            .theme_slot(ThemeSlot::MenuPopup, cx)
                            .on_mouse_down_out(move |_, _, cx| {
                                close_state.update(cx, |state, cx| {
                                    state.open = false;
                                    state.closed_at = Some(Instant::now());
                                    cx.notify();
                                });
                            })
                            .children(
                                theme_slot_clipped_background_layer(ThemeSlot::PopoverPopup, r(8.), cx)
                                    .or_else(|| theme_slot_clipped_background_layer(ThemeSlot::MenuPopup, r(8.), cx)),
                            )
                            .child(
                                div()
                                    .id((self.id.clone(), "popup-scroll"))
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(list),
                            ),
                    ),
                )
                .with_priority(SELECT_POPUP_PRIORITY),
            )
        });

        div().relative().flex_none().child(trigger).children(popup)
    }
}

/// `SettingsSection` (grouped): a 16px semibold title above a bordered group whose rows divide.
pub fn section(title: impl Into<SharedString>, rows: Vec<AnyElement>, cx: &App) -> gpui::Div {
    let title: SharedString = title.into();
    let translated = crate::i18n::t(cx, title.as_ref()).to_owned();
    v_flex().gap(r(10.)).child(section_title(translated)).child(group(rows, cx))
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
                        .child(crate::i18n::t(cx, message)),
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
        .relative()
        .rounded(r(12.))
        .border_1()
        .border_color(hsla(p.border).opacity(0.6))
        .bg(hsla(p.card).opacity(0.4))
        .shadow(control_shadow())
        .theme_slot(ThemeSlot::SettingsGroup, cx)
        .children(theme_slot_clipped_background_layer(ThemeSlot::SettingsGroup, r(12.), cx))
        .children(crate::theme::inspector_slot_probe(ThemeSlot::SettingsGroup, cx))
        .children(rows.into_iter().enumerate().map(move |(index, row)| {
            div().relative().when(index > 0 && index < count, |this| this.border_t_1().border_color(divider)).child(row)
        }))
}

/// The settings sources in precedence order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SettingSource {
    /// A saved project override.
    Project,
    /// The environment's saved value.
    Environment,
    /// The built-in default.
    Default,
}

impl SettingSource {
    fn label(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::Environment => "Environment",
            Self::Default => "Default",
        }
    }
}

/// A formatted setting value and whether it supplies the effective setting.
pub struct SettingInheritanceLayer {
    source: SettingSource,
    value: Option<SharedString>,
    effective: bool,
}

impl SettingInheritanceLayer {
    /// Uses “Inherits” when a layer has no value of its own.
    pub fn new(source: SettingSource, value: Option<SharedString>, effective: bool) -> Self {
        Self { source, value, effective }
    }
}

/// The source chain and optional project override actions shown next to a settings row's title.
pub struct SettingInheritance {
    summary: SharedString,
    layers: Vec<SettingInheritanceLayer>,
    on_reset: Option<ClickHandler>,
    mixed: bool,
}

impl SettingInheritance {
    /// Builds the source indicator from a caller-resolved chain of layers.
    pub fn new(summary: impl Into<SharedString>, mut layers: Vec<SettingInheritanceLayer>) -> Self {
        layers.sort_by_key(|layer| layer.source);
        Self { summary: summary.into(), layers, on_reset: None, mixed: false }
    }

    /// Marks the indicator as mixed across the selected targets.
    pub fn mixed(mut self, mixed: bool) -> Self {
        self.mixed = mixed;
        self
    }

    /// Supplies a reset action for the displayed project setting.
    pub fn on_reset_overrides(mut self, reset: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_reset = Some(Rc::new(reset));
        self
    }

    fn render(self, id: ElementId, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let state = window.use_keyed_state((id.clone(), "open"), cx, |_, _| false);
        let open = *state.read(cx);
        let focus = focus_handle(&id, window, cx);
        let tint = if self.mixed {
            p.warning_foreground
        } else if self.layers.iter().any(|layer| layer.source == SettingSource::Project && layer.effective) {
            p.primary
        } else {
            p.muted_foreground
        };
        let trigger = Button::new(id.clone(), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
            .focus_handle(focus.clone())
            .icon(Icon::Layers)
            .icon_color(hsla(tint))
            .tooltip(self.summary)
            .on_click({
                let state = state.clone();
                move |_, _, cx| state.update(cx, |value, cx| {
                    *value = !open;
                    cx.notify();
                })
            });
        let mut wrapper = div().id((id.clone(), "wrapper")).relative().flex_none().child(trigger);
        if !open {
            return wrapper;
        }
        let close = Rc::new(move |cx: &mut App| state.update(cx, |open, cx| {
            if *open {
                *open = false;
                cx.notify();
            }
        }));
        let layers = self.layers.into_iter().map(|layer| {
            h_flex()
                .min_h(r(26.))
                .gap(r(12.))
                .px(r(8.))
                .rounded(r(6.))
                .text_color(hsla(p.muted_foreground))
                .when(layer.effective, |row| row.bg(hsla(p.accent)).text_color(hsla(p.foreground)).font_weight(FontWeight::MEDIUM))
                .child(div().flex_1().min_w_0().truncate().child(crate::i18n::t(cx, layer.source.label()).to_owned()))
                .child(div().max_w(r(128.)).truncate().child(layer.value.unwrap_or_else(|| crate::i18n::t(cx, "Inherits").into())))
                .child(div().size(r(14.)).flex_none().children(layer.effective.then(|| icon(Icon::Check, 14., hsla(p.primary)))))
        });
        let reset = self.on_reset.map(|reset| {
            let close = close.clone();
            let focus = focus.clone();
            h_flex().mt(r(6.)).pt(r(6.)).border_t_1().border_color(hsla(p.border)).px(r(8.))
                .child(InlineButton::new((id.clone(), "reset"), crate::i18n::t(cx, "Reset it"), InlineTone::Muted)
                    .on_click(move |event, window, cx| {
                        close(cx);
                        focus.focus(window, cx);
                        reset(event, window, cx);
                    }))
        });
        let popup = div()
            .id((id.clone(), "popup"))
            .relative()
            .occlude()
            .mt(r(4.))
            .w(r(280.))
            .max_h(r(288.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.dropdown_border))
            .bg(hsla(p.popover))
            .text_color(hsla(p.popover_foreground))
            .shadow_md()
            .theme_slot(ThemeSlot::PopoverPopup, cx)
            .on_mouse_down_out({
                let close = close.clone();
                move |_, _, cx| close(cx)
            })
            .children(theme_slot_clipped_background_layer(ThemeSlot::PopoverPopup, r(8.), cx))
            .child(div().id((id, "scroll")).relative().max_h(r(286.)).overflow_y_scroll().p(r(8.))
                .text_sm().children(layers).children(reset));
        wrapper = wrapper.key_context("SettingsInheritance").on_action(move |_: &CloseInheritance, window, cx| {
            close(cx);
            focus.focus(window, cx);
            cx.stop_propagation();
        });
        wrapper.child(
            div().absolute().bottom_0().right_0().child(
                gpui::deferred(
                    gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(popup),
                ).with_priority(2),
            ),
        )
    }
}

/// `SettingsRow`: title, optional reset and description on the left; the control on the
/// right; optional content below.
#[derive(IntoElement)]
pub struct Row {
    id: SharedString,
    title: SharedString,
    title_icon: Option<Icon>,
    title_tooltip: Option<SharedString>,
    inheritance: Option<SettingInheritance>,
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
            inheritance: None,
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

    /// Shows a source indicator and its inheritance popover next to the title.
    pub fn inheritance(mut self, inheritance: SettingInheritance) -> Self {
        self.inheritance = Some(inheritance);
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
        let reset_tooltip = self.reset_tooltip.unwrap_or_else(|| crate::i18n::t(cx, "Reset to default").into());
        let unavailable = self.unavailable.map(|msg| SharedString::from(crate::i18n::t(cx, msg.as_ref()).to_owned()));
        let reset = self.reset.filter(|_| unavailable.is_none()).map(|handler| {
            Button::new(ElementId::Name(format!("{}-reset", self.id).into()), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                .icon(Icon::Undo2)
                .tooltip(reset_tooltip)
                .on_click(move |event, window, cx| handler(event, window, cx))
        });
        let inheritance = self.inheritance.map(|source| {
            source.render((ElementId::Name(self.id.clone()), "inheritance").into(), window, cx).into_any_element()
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
                                    .child(div().min_w_0().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(crate::i18n::t(cx, self.title.as_ref()).to_owned()))
                                    .children(self.title_tooltip.map(|text| {
                                        Button::new(ElementId::Name(format!("{}-details", self.id).into()), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                                            .icon(Icon::Info)
                                            .tooltip(text)
                                    }))
                                    .children(inheritance)
                                    .child(div().size(r(20.)).flex().items_center().justify_center().children(reset)),
                            )
                            .children(self.description.map(|description| crate::i18n::t(cx, description.as_ref()).to_owned().into_any_element()).or(self.description_content).map(|description| {
                                div().max_w(r(672.)).text_xs().line_height(r(18.)).text_color(hsla(p.muted_foreground)).child(description)
                            }))
                            .children(self.status.map(|status| div().pt(r(2.)).text_xs().text_color(hsla(p.muted_foreground)).child(crate::i18n::t(cx, status.as_ref()).to_owned()))),
                    )
                    .children(self.control.map(|control| {
                        let id = self.id.clone();
                        h_flex()
                            .gap(r(8.))
                            .map(|this| if stacked { this.w_full().min_w_0() } else { this.flex_none().max_w(r(420.)).justify_end() })
                            .map(|this| match unavailable.clone() {
                                Some(message) => {
                                    let selector = format!("{id}-unavailable");
                                    let unavailable_id = ElementId::Name(selector.clone().into());
                                    let focus = focus_handle(&unavailable_id, window, cx);
                                    let ring = hsla(p.ring);
                                    this.child(
                                        div()
                                            .id(unavailable_id)
                                            .debug_selector(move || selector.clone())
                                            .relative()
                                            .rounded(r(6.))
                                            .track_focus(&focus)
                                            .tab_index(0)
                                            .role(gpui::Role::Group)
                                            .aria_label(message.clone())
                                            .focus_visible(move |s| {
                                                s.shadow(vec![BoxShadow {
                                                    color: ring,
                                                    offset: point(px(0.), px(0.)),
                                                    blur_radius: px(0.),
                                                    spread_radius: px(2.),
                                                    inset: true,
                                                }])
                                            })
                                            .child(div().opacity(0.5).child(control))
                                            .child(div().absolute().inset_0().occlude())
                                            .tooltip(tooltip(message)),
                                    )
                                }
                                None => this.child(control),
                            })
                    })),
            )
            .children(self.below.map(|below| {
                let id = self.id.clone();
                match unavailable {
                    Some(message) => {
                        let selector = format!("{id}-below-unavailable");
                        let unavailable_id = ElementId::Name(selector.clone().into());
                        let focus = focus_handle(&unavailable_id, window, cx);
                        let ring = hsla(p.ring);
                        div()
                            .id(unavailable_id)
                            .debug_selector(move || selector.clone())
                            .relative()
                            .rounded(r(6.))
                            .track_focus(&focus)
                            .tab_index(0)
                            .role(gpui::Role::Group)
                            .aria_label(message.clone())
                            .focus_visible(move |s| {
                                s.shadow(vec![BoxShadow {
                                    color: ring,
                                    offset: point(px(0.), px(0.)),
                                    blur_radius: px(0.),
                                    spread_radius: px(2.),
                                    inset: true,
                                }])
                            })
                            .child(div().opacity(0.5).child(below))
                            .child(div().absolute().inset_0().occlude())
                            .tooltip(tooltip(message))
                            .into_any_element()
                    }
                    None => below,
                }
            }))
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
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let focus = (!self.disabled).then(|| focus_handle(&self.id, window, cx));
        let ring = focus.as_ref().is_some_and(|focus| focus.is_focused(window) && window.last_input_was_keyboard());
        let (text, hover) = match self.tone {
            InlineTone::Muted => (hsla(p.muted_foreground), hsla(p.foreground)),
            InlineTone::Destructive => (hsla(p.destructive_foreground), hsla(p.destructive_foreground)),
        };
        div()
            .id(self.id)
            .role(gpui::Role::Button)
            .when_some(focus, |this, focus| this.track_focus(&focus))
            .when(ring, |this| this.rounded(r(2.)).shadow(focus_ring(hsla(p.ring))))
            .flex_none()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(text)
            .when(self.disabled, |this| this.opacity(DISABLED_OPACITY))
            .when(!self.disabled, |this| this.cursor_pointer().hover(move |this| this.text_color(hover).underline()))
            .child(crate::i18n::t(cx, self.label.as_ref()).to_owned())
            .when_some(self.tooltip, |this, text| this.tooltip(tooltip(crate::i18n::t(cx, text.as_ref()).to_owned().into())))
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
        self.render_inner(window, DIALOG_MODAL_PRIORITY, false, true, on_dismiss, cx)
    }

    pub fn render_modal(self, window: &Window, priority: usize, on_dismiss: impl Fn(&mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
        self.render_inner(window, priority, true, true, on_dismiss, cx)
    }

    pub fn render_modal_guarded(
        self,
        window: &Window,
        priority: usize,
        dismiss_on_backdrop: bool,
        on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        self.render_inner(window, priority, true, dismiss_on_backdrop, on_dismiss, cx)
    }

    fn render_inner(
        self,
        window: &Window,
        priority: usize,
        close: bool,
        dismiss_on_backdrop: bool,
        on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        let on_dismiss=std::rc::Rc::new(on_dismiss);
        let dismiss_button=on_dismiss.clone();
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let viewport = window.viewport_size();
        let has_custom_bg = slot_has_background(ThemeSlot::DialogPopup, cx);
        let backdrop_border = slot_border_color(ThemeSlot::DialogPopup, cx).unwrap_or_else(|| hsla(p.dialog_border));
        let dialog = v_flex()
            .id(self.id)
            .track_focus(&self.focus)
            .occlude()
            .relative()
            .overflow_hidden()
            .w_full()
            .max_w(r(self.max_width))
            .max_h(viewport.height - px(32.))
            .rounded(r(16.))
            .when(!has_custom_bg, |this| {
                this.child(crate::glass::backdrop(16., hsla(p.dialog_surface), backdrop_border, cx))
            })
            .when(has_custom_bg, |this| {
                this.bg(hsla(p.dialog_surface)).border_1().border_color(backdrop_border)
            })
            .children(theme_slot_state_background_layer(ThemeSlot::DialogPopup, SlotState::Base, r(16.), cx))
            .shadow(vec![BoxShadow {
                color: gpui::black().opacity(if dark { 0.9 } else { 0.65 }),
                offset: point(px(0.), px(24.)),
                blur_radius: px(64.),
                spread_radius: px(-24.),
                inset: false,
            }])
            .theme_slot(ThemeSlot::DialogPopup, cx)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .p(r(24.))
                    .gap(r(8.))
                    .child(div().text_xl().line_height(r(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child(crate::i18n::t(cx, self.title.as_ref()).to_owned()))
                    .when(!self.description.is_empty(), |this| this.child(div().text_sm().text_color(hsla(p.muted_foreground)).child(crate::i18n::t(cx, self.description.as_ref()).to_owned()))),
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
                    .when(dismiss_on_backdrop, |this| {
                        this.on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| on_dismiss(window, cx))
                    })
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

/// An editable number with decrement and increment steppers.
#[derive(IntoElement)]
pub struct NumberField {
    state: gpui::Entity<gpui_component::input::InputState>,
    disabled: bool,
}

impl NumberField {
    /// Uses the input state's value, step and bounds, and emits its normal input events.
    pub fn new(state: &gpui::Entity<gpui_component::input::InputState>) -> Self {
        Self { state: state.clone(), disabled: false }
    }

    /// Disables editing and both steppers.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for NumberField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        use gpui_component::{Disableable, Sizable, input::NumberInput};

        let background = theme_slot_clipped_background_layer(ThemeSlot::Input, r(8.), cx);
        let custom = background.is_some();
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .children(background)
            .children(crate::theme::inspector_slot_probe(ThemeSlot::Input, cx))
            .child(
                NumberInput::new(&self.state)
                    .appearance(!custom)
                    .small()
                    .disabled(self.disabled)
                    .theme_slot(ThemeSlot::Input, cx),
            )
    }
}

#[derive(IntoElement)]
pub struct PercentSlider {
    state: gpui::Entity<gpui_component::slider::SliderState>,
    value: u32,
}

impl PercentSlider {
    pub fn new(state: gpui::Entity<gpui_component::slider::SliderState>, value: u32) -> Self {
        Self { state, value }
    }
}

/// Shared track/fill/thumb slider chrome using theme tokens instead of `gpui-component`'s faint bar.
pub(super) fn styled_slider(
    state: &gpui::Entity<gpui_component::slider::SliderState>,
    cx: &App,
) -> AnyElement {
    use gpui_base::{Slider, SliderIndicator, SliderThumb, SliderTrack};

    let p = palette(cx);
    let position = gpui::relative(state.read(cx).percentage().end);
    Slider::new(state)
        .w_full()
        .child(
            SliderTrack::new(state).w_full().h(r(24.)).px(r(8.)).flex().items_center().child(
                SliderIndicator::new(state)
                    .relative()
                    .w_full()
                    .h(r(6.))
                    .rounded_full()
                    .border_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.muted))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(position)
                            .rounded_full()
                            .bg(hsla(p.primary)),
                    )
                    .child(
                        SliderThumb::new(state)
                            .absolute()
                            .top(r(-6.))
                            .left(position)
                            .ml(r(-8.))
                            .size(r(16.))
                            .rounded_full()
                            .border_2()
                            .border_color(hsla(p.primary))
                            .bg(hsla(p.background)),
                    ),
            ),
        )
        .into_any_element()
}

impl RenderOnce for PercentSlider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        if self.state.read(cx).value().start() != self.value as f32 {
            self.state.update(cx, |state, cx| state.set_value(self.value as f32, window, cx));
        }
        h_flex().w_full().min_w_0().gap(r(12.))
            .child(
                div().min_w(r(48.)).rounded(r(6.)).bg(hsla(p.muted)).px(r(8.)).py(r(4.))
                    .font_family(crate::theme::font_family(t3_core::client_settings::typography::FontRole::Code, cx))
                    .text_center().font_weight(FontWeight::MEDIUM).text_xs().text_color(hsla(p.foreground))
                    .child(format!("{}%", self.value)),
            )
            .child(div().flex_1().min_w_0().child(styled_slider(&self.state, cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_switch_has_an_indeterminate_thumb_and_activates_to_on() {
        for checked in [false, true] {
            let switch = Switch::new("switch", checked).mixed(true);
            assert_eq!(switch.toggled(), gpui::Toggled::Mixed);
            assert_eq!(switch.thumb_offset(), 6.);
            assert!(switch.next_checked());
        }
        let off = Switch::new("switch", false);
        assert_eq!(off.toggled(), gpui::Toggled::False);
        assert_eq!(off.thumb_offset(), 0.);
        let on = Switch::new("switch", true);
        assert_eq!(on.toggled(), gpui::Toggled::True);
        assert_eq!(on.thumb_offset(), 12.);
        assert!(on.selected());
    }

    #[test]
    fn select_popup_priority_outranks_dialog_modals() {
        const { assert!(SELECT_POPUP_PRIORITY > DIALOG_MODAL_PRIORITY); }
        const { assert!(SELECT_POPUP_PRIORITY > 4); }
    }
}
