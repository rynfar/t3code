//! The timeline minimap (`apps/web/src/components/chat/TimelineMinimap.tsx`, wired as in
//! `MessagesTimeline.tsx`): a strip per user message beside the transcript, lit while its
//! message is on screen, with a fisheye and preview card under the pointer, click to jump, and
//! previous / next turn buttons. The model is `t3_core::timeline::minimap`.
//!
//! The rail reads what is on screen from the list's height tree in its own prepaint, after the
//! list has laid out, so lit strips paint in the frame they belong to. Geometry and finite
//! motion invalidate the minimap layer; hover and motion do not invalidate the cached rows.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Bounds, BoxShadow, Context, Entity, FocusHandle, FontWeight, ListOffset, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollWheelEvent, Subscription, WeakEntity, Window, canvas,
    div, fill, point, prelude::*, px, size,
};
use t3_core::timeline::minimap::{self, MinimapItem, MinimapPreview, RailPointer, RailPointerAction, RailPointerEvent};

use super::ThreadView;
use gpui_component::ActiveTheme;
use crate::icons::Icon;
use crate::theme::{hsla, palette, rem};

gpui::actions!(timeline_minimap, [MinimapTabNext, MinimapTabPrevious]);

pub fn init(cx: &mut gpui::App) {
    cx.bind_keys([
        gpui::KeyBinding::new("tab", MinimapTabNext, Some("TimelineMinimap")),
        gpui::KeyBinding::new("shift-tab", MinimapTabPrevious, Some("TimelineMinimap")),
    ]);
}

fn focus_handles(cx: &mut gpui::App) -> [FocusHandle; 3] {
    std::array::from_fn(|_| cx.focus_handle().tab_stop(true))
}

pub struct MinimapLayer {
    owner: WeakEntity<ThreadView>,
    _subscriptions: [Subscription; 2],
}

impl MinimapLayer {
    pub fn new(owner: &Entity<ThreadView>, rows: &Entity<super::rows::TimelineRows>, cx: &mut Context<Self>) -> Self {
        Self { owner: owner.downgrade(), _subscriptions: [
            cx.observe(owner, |_, _, cx| cx.notify()),
            cx.observe(rows, |_, _, cx| cx.notify()),
        ] }
    }
}

impl Render for MinimapLayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "snapshot")]
        crate::snapshot::record_render("minimap", cx);
        let repaint = cx.entity_id();
        let content = self.owner.update(cx, |owner, cx| {
            owner.minimap.repaint = Some(repaint);
            owner.render_minimap(window, cx)
        }).ok().flatten();
        div().relative().size_full().children(content)
    }
}

/// What the rail last measured after layout. Render draws from it; click handlers read it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Measured {
    range: Option<(usize, usize)>,
    current: Option<usize>,
    persistent: bool,
    hit_width: f32,
}

#[derive(Clone)]
struct ListLayout {
    list: gpui::ListState,
    header: usize,
    row_count: usize,
    rows: Rc<Vec<usize>>,
    lane_padding: f32,
    lane_max: Option<f32>,
}

impl ListLayout {
    fn measure(&self) -> Measured {
        let viewport = self.list.viewport_bounds();
        let width = f32::from(viewport.size.width);
        let lane = (width - self.lane_padding).max(0.);
        let lane = self.lane_max.map_or(lane, |max| lane.min(max));
        let scroll = -f32::from(self.list.scroll_px_offset_for_scrollbar().y);
        let range = minimap::visible_range(self.row_count, scroll, f32::from(viewport.size.height), |index| f32::from(self.list.offset_for_item(self.header + index)));
        Measured {
            range,
            current: minimap::current_index(range, self.rows.iter().copied()),
            persistent: minimap::has_persistent_gutter(width, lane),
            hit_width: minimap::hit_strip_width(width, lane),
        }
    }

    fn observe(self, content: AnyElement, measured: Rc<Cell<Measured>>, repaint: gpui::EntityId) -> AnyElement {
        // Descendants can resize without notifying either cached sibling's owner.
        let measure = canvas(move |_, window, _| {
            let next = self.measure();
            if measured.replace(next) != next { window.on_next_frame(move |_, cx| cx.notify(repaint)); }
        }, |_, _, _, _| {}).absolute().size_full();
        div().relative().size_full().child(content).child(measure).into_any_element()
    }
}

#[derive(Default)]
pub struct MinimapUi {
    items: Vec<MinimapItem>,
    row_indices: Rc<Vec<usize>>,
    /// The item under the pointer (`activeIndex`): it widens its strip and shows its preview.
    active: Option<usize>,
    preview: Option<(usize, MinimapPreview)>,
    strip_hovered: bool,
    preview_hovered: bool,
    nav_hovered: bool,
    nav_hovers: [bool; 2],
    nav_tooltip: Option<usize>,
    nav_tooltip_task: Option<gpui::Task<()>>,
    nav_bounds: [Rc<Cell<Bounds<Pixels>>>; 2],
    pointer: RailPointer,
    rail: Rc<Cell<Bounds<Pixels>>>,
    measured: Rc<Cell<Measured>>,
    focus: Option<FocusHandle>,
    previous_focus: Option<FocusHandle>,
    next_focus: Option<FocusHandle>,
    focus_subscriptions: Vec<Subscription>,
    preview_height: Rc<Cell<f32>>,
    repaint: Option<gpui::EntityId>,
    tooltips: [minimap::NavigationTooltip; 2],
}

impl MinimapUi {
    pub fn set_items(&mut self, items: Vec<MinimapItem>) {
        let same_text = |a: &MinimapItem, b: &MinimapItem| {
            Arc::ptr_eq(&a.user_text, &b.user_text)
                && match (&a.assistant_text, &b.assistant_text) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (a, b) => a.is_none() && b.is_none(),
                }
        };
        let keep = self.preview.as_ref().is_some_and(|(index, _)| self.items.get(*index).zip(items.get(*index)).is_some_and(|(a, b)| same_text(a, b)));
        if !keep {
            self.preview = None;
        }
        self.row_indices = Rc::new(items.iter().map(|item| item.row_index).collect());
        self.items = items;
    }

    fn preview(&mut self, index: usize) -> MinimapPreview {
        if let Some((cached, preview)) = &self.preview
            && *cached == index
        {
            return preview.clone();
        }
        let preview = minimap::resolve_minimap_preview(&self.items[index]);
        self.preview = Some((index, preview.clone()));
        preview
    }

    fn index_at(&self, y: Pixels) -> Option<usize> {
        let rail = self.rail.get();
        minimap::index_from_pointer(self.items.len(), f32::from(rail.top()), f32::from(rail.size.height), f32::from(y))
    }
}

impl ThreadView {
    fn minimap_list_layout(&self, window: &Window, cx: &gpui::App) -> ListLayout {
        ListLayout {
            list: self.list.clone(),
            header: self.header_count(),
            row_count: self.rows.len(),
            rows: self.minimap.row_indices.clone(),
            lane_padding: 2. * self.gutter() * f32::from(window.rem_size()) / 16.,
            lane_max: crate::theme::chat_max_width(cx).map(|width| f32::from(width.to_pixels(window.rem_size()))),
        }
    }

    pub(super) fn observe_minimap_layout(&self, transcript: AnyElement, window: &Window, cx: &gpui::App) -> AnyElement {
        if self.minimap.items.len() < minimap::MIN_ITEMS { return transcript; }
        self.minimap_list_layout(window, cx).observe(transcript, self.minimap.measured.clone(), self.minimap_layer.entity_id())
    }

    pub(super) fn minimap_escape(&mut self, key: &gpui::Keystroke, window: &Window, cx: &mut Context<Self>) -> bool {
        if key.key != "escape" { return false; }
        let mut dismissed = false;
        for (index, focus) in [&self.minimap.previous_focus, &self.minimap.next_focus].into_iter().enumerate() {
            let tooltip = &mut self.minimap.tooltips[index];
            if tooltip.visible(self.minimap.nav_hovers[index], focus.as_ref().is_some_and(|focus| focus.is_focused(window)), self.minimap.nav_tooltip == Some(index)) {
                tooltip.dismiss();
                dismissed = true;
            }
        }
        if dismissed {
            self.minimap.nav_tooltip = None;
            self.minimap.nav_tooltip_task = None;
            self.notify_minimap(cx);
            cx.stop_propagation();
        }
        dismissed
    }

    fn notify_minimap(&self, cx: &mut Context<Self>) {
        if let Some(repaint) = self.minimap.repaint { gpui::App::notify(cx, repaint); }
    }

    /// `onSelect`: scrolls the message to just below the top and leaves follow-to-bottom.
    fn jump_to_minimap_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.disclosure.cancel();
        let Some(row_index) = self.minimap.items.get(index).map(|item| item.row_index) else { return };
        let list = self.list.clone();
        let target = self.header_count() + row_index;
        let (item_ix, offset, remaining) =
            minimap::jump_anchor(target, minimap::JUMP_VIEW_OFFSET, |ix| f32::from(list.offset_for_item(ix + 1) - list.offset_for_item(ix)));
        list.scroll_to(ListOffset { item_ix, offset_in_item: px(offset) });
        if remaining > 0. {
            // The rows above are measured once the list lays out at the anchor.
            let view = cx.entity_id();
            window.on_next_frame(move |_, cx| {
                let top = list.logical_scroll_top();
                if top.item_ix == item_ix && top.offset_in_item == px(0.) {
                    list.scroll_by(px(-remaining));
                    cx.notify(view);
                }
            });
        }
        cx.notify();
    }

    fn minimap_tab(&mut self, part: minimap::FocusPart, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.minimap.measured.get().current.filter(|i| *i < self.minimap.items.len());
        let previous = current.is_some_and(|i| i > 0);
        let next = current.is_some_and(|i| i + 1 < self.minimap.items.len());
        if let Some(target) = minimap::tab_target(part, backwards, previous, next) {
            let focus = match target {
                minimap::FocusPart::Previous => self.minimap.previous_focus.as_ref(),
                minimap::FocusPart::Rail => self.minimap.focus.as_ref(),
                minimap::FocusPart::Next => self.minimap.next_focus.as_ref(),
            };
            if let Some(focus) = focus { window.focus(focus, cx); }
            window.prevent_default();
            cx.stop_propagation();
            self.notify_minimap(cx);
            return;
        }
        if backwards { window.focus_prev(cx); } else { window.focus_next(cx); }
        window.prevent_default();
        cx.stop_propagation();
    }

    /// Previous or next turn from the one being read.
    fn step_minimap(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.minimap.measured.get().current.and_then(|current| current.checked_add_signed(delta));
        if let Some(target) = target.filter(|target| *target < self.minimap.items.len()) {
            self.jump_to_minimap_item(target, window, cx);
        }
    }

    /// A leave is settled after the event, once the rail and the preview both reported.
    fn minimap_hover_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.notify_minimap(cx);
        cx.defer_in(window, |view, _, cx| {
            let ui = &mut view.minimap;
            if !ui.strip_hovered && !ui.preview_hovered && ui.active.is_some() {
                ui.active = None;
                view.notify_minimap(cx);
            }
        });
    }

    /// `forwardWheelToTimeline`: the rail and its preview sit over the transcript, so a wheel
    /// there scrolls the transcript, never a scroller underneath (an open command's output).
    fn forward_minimap_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if !forward_wheel(&self.list, &mut self.disclosure, event) { return; }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_minimap(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = self.minimap.items.len();
        if count < minimap::MIN_ITEMS {
            return None;
        }
        if self.minimap.focus.is_none() {
            let [previous, focus, next] = focus_handles(cx);
            self.minimap.focus_subscriptions = vec![
                cx.on_focus(&focus, window, |view, _, cx| {
                    let ui = &mut view.minimap;
                    ui.active = minimap::focus_index(ui.active, ui.measured.get().current, ui.items.len());
                    view.notify_minimap(cx);
                }),
                cx.on_focus_out(&focus, window, |view, _, _, cx| {
                    view.minimap.active = None;
                    view.notify_minimap(cx);
                }),
            ];
            self.minimap.focus = Some(focus);
            self.minimap.previous_focus = Some(previous);
            self.minimap.next_focus = Some(next);
        }
        let focus = self.minimap.focus.as_ref().unwrap().clone();
        #[cfg(feature = "snapshot")]
        {
            crate::snapshot::register_focus("timeline-minimap", focus.clone(), cx);
            crate::snapshot::register_focus("timeline-minimap-previous", self.minimap.previous_focus.as_ref().unwrap().clone(), cx);
            crate::snapshot::register_focus("timeline-minimap-next", self.minimap.next_focus.as_ref().unwrap().clone(), cx);
        }
        let rem_px = f32::from(window.rem_size()) / 16.;
        let measured = self.minimap.measured.get();
        let interactive = measured.hit_width > 0.;
        let navigation = interactive && minimap::navigation_interactive(measured.hit_width);
        // A rail that stops taking the pointer can no longer report a leave.
        if !interactive && !focus.contains_focused(window, cx) {
            (self.minimap.active, self.minimap.strip_hovered, self.minimap.preview_hovered) = (None, false, false);
            self.minimap.pointer = RailPointer::default();
        }
        if !navigation {
            self.minimap.nav_hovered = false;
            self.minimap.nav_hovers = [false; 2];
            self.minimap.nav_tooltip = None;
            self.minimap.nav_tooltip_task = None;
        }
        let rail_height = minimap::rail_height(count, f32::from(window.viewport_size().height) - minimap::MAX_HEIGHT_INSET * rem_px);
        let active = self.minimap.active.filter(|index| *index < count);
        let preview = active.map(|index| (index, self.minimap.preview(index)));
        let width = if preview.is_some() { minimap::EXPANDED_HIT_STRIP_WIDTH * rem_px } else { measured.hit_width };
        let focused = focus.contains_focused(window, cx)
            || self.minimap.previous_focus.as_ref().is_some_and(|focus| focus.is_focused(window))
            || self.minimap.next_focus.as_ref().is_some_and(|focus| focus.is_focused(window));
        let revealed = self.minimap.strip_hovered || self.minimap.preview_hovered || self.minimap.nav_hovered || active.is_some() || focused;

        let rail = self.render_minimap_rail(active, revealed, rem_px, window, cx);
        let ring = hsla(palette(cx).ring).opacity(0.7);
        let jump = div()
            .id("timeline-minimap")
            .absolute()
            .inset_0()
            .child(rail)
            .track_focus(&focus)
            .key_context("TimelineMinimap")
            .role(gpui::Role::Button)
            .aria_label(format!("Jump to message: {}", preview.as_ref().and_then(|(_, preview)| preview.user_text.as_deref()).unwrap_or(minimap::USER_MESSAGE)))
            .focus_visible(move |style| style.shadow(focus_ring(ring)))
            .on_action(cx.listener(|view, _: &MinimapTabNext, window, cx| {
                view.minimap_tab(minimap::FocusPart::Rail, false, window, cx);
            }))
            .on_action(cx.listener(|view, _: &MinimapTabPrevious, window, cx| {
                view.minimap_tab(minimap::FocusPart::Rail, true, window, cx);
            }))
            .on_key_down(cx.listener(|view, event: &gpui::KeyDownEvent, window, cx| {
                if !view.minimap.focus.as_ref().is_some_and(|focus| focus.is_focused(window)) { return; }
                let key = match event.keystroke.key.as_str() {
                    "down" => "ArrowDown", "up" => "ArrowUp", "home" => "Home", "end" => "End", "enter" => "Enter", "space" => " ", key => key,
                };
                match minimap::keyboard_action(view.minimap.active, view.minimap.items.len(), key) {
                    minimap::KeyboardAction::Ignore => return,
                    minimap::KeyboardAction::Preview(index) => view.minimap.active = Some(index),
                    minimap::KeyboardAction::Jump(index) => view.jump_to_minimap_item(index, window, cx),
                }
                window.prevent_default();
                cx.stop_propagation();
                view.notify_minimap(cx);
            }))
            .children(preview.map(|(index, preview)| render_preview(index, count, rail_height, preview, self.minimap.preview_height.clone(), self.minimap.repaint.unwrap(), cx)))
            .when(interactive, |this| {
                this.cursor_pointer()
                    .occlude()
                    .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, _, cx| view.forward_minimap_wheel(event, cx)))
                    .on_hover(cx.listener(|view, hovered: &bool, window, cx| {
                        if view.minimap.strip_hovered == *hovered { return; }
                        view.minimap.strip_hovered = *hovered;
                        view.minimap_hover_changed(window, cx);
                    }))
                    .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _, cx| {
                        let ui = &mut view.minimap;
                        let action = ui.pointer.handle(RailPointerEvent::Move { on_rail: true, pressed: event.pressed_button.is_some() });
                        let index = ui.index_at(event.position.y);
                        if action == RailPointerAction::Track && index != ui.active {
                            ui.active = index;
                            view.notify_minimap(cx);
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, cx.listener(|view, _, window, _| {
                        window.prevent_default();
                        view.minimap.pointer.handle(RailPointerEvent::Down { on_rail: true });
                    }))
                    .on_mouse_down_out(cx.listener(|view, _, _, _| {
                        view.minimap.pointer.handle(RailPointerEvent::Down { on_rail: false });
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|view, event: &MouseUpEvent, window, cx| {
                            if view.minimap.pointer.handle(RailPointerEvent::Up { on_rail: true }) == RailPointerAction::Jump
                                && let Some(index) = view.minimap.index_at(event.position.y)
                            {
                                view.jump_to_minimap_item(index, window, cx);
                                view.minimap.active = None;
                                window.blur();
                            }
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|view, _, _, _| {
                            view.minimap.pointer.handle(RailPointerEvent::Up { on_rail: false });
                        }),
                    )
            });
        let current = measured.current.filter(|current| *current < count);
        let previous = current.and_then(|current| current.checked_sub(1));
        let next = current.map(|current| current + 1).filter(|next| *next < count);
        let previous = self.render_minimap_nav(-1, previous.is_some(), navigation, rail_height, window, cx);
        let next = self.render_minimap_nav(1, next.is_some(), navigation, rail_height, window, cx);
        let strip = div().relative().flex_none().ml(rem(12.)).w(px(width)).h(px(rail_height)).child(previous).child(jump).child(next);
        Some(div().absolute().top_0().bottom_0().left_0().w(rem(72.)).flex().flex_col().justify_center().child(strip).into_any_element())
    }

    /// The strips, painted from the list's layout of this frame.
    fn render_minimap_rail(&self, active: Option<usize>, revealed: bool, rem_px: f32, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let layout = self.minimap_list_layout(window, cx);
        let rows = self.minimap.row_indices.clone();
        let rail_cell = self.minimap.rail.clone();
        let measured_cell = self.minimap.measured.clone();
        let rendered = measured_cell.get();
        let view = self.minimap.repaint.unwrap();
        let p = palette(cx);
        let (line, strip, strip_active, lit) =
            (hsla(p.border).opacity(0.15), hsla(p.muted_foreground).opacity(0.25), hsla(p.muted_foreground).opacity(0.6), hsla(p.foreground).opacity(0.55));
        canvas(
            move |bounds, window, _| {
                rail_cell.set(bounds);
                let next = layout.measure();
                measured_cell.set(next);
                if next != rendered {
                    window.on_next_frame(move |_, cx| cx.notify(view));
                }
                (next.range, next.persistent, rows)
            },
            move |bounds, (range, persistent, rows), window, cx| {
                let opacity = transition("minimap-gutter-opacity", vec![if persistent || revealed { 1. } else { 0. }], view, window, cx)[0];
                if opacity == 0. { return; }
                let count = rows.len();
                let mut targets = Vec::with_capacity(count * 2);
                for (index, row) in rows.iter().enumerate() {
                    targets.push(strip_scale(active, index));
                    targets.push(if minimap::is_row_visible(*row, range) { 1. } else { 0. });
                }
                let values = transition("minimap-rail-motion", targets, view, window, cx);
                let (left, top, height) = (bounds.left(), bounds.top(), bounds.size.height);
                window.paint_quad(fill(Bounds::new(point(left + px(12. * rem_px), top), size(px(1.), height)), line.opacity(opacity)));
                let thickness = px(2. * rem_px);
                for index in 0..rows.len() {
                    let distance = active.map(|active| active.abs_diff(index));
                    let scale = values[index * 2];
                    let y = top + height * (minimap::top_percent(index, count) / 100.) - thickness / 2.;
                    let bounds = Bounds::new(point(left, y), size(px(24. * rem_px * scale), thickness));
                    let radius = thickness / 2.;
                    window.paint_quad(fill(bounds, (if distance == Some(0) { strip_active } else { strip }).opacity(opacity)).corner_radii(radius));
                    window.paint_quad(fill(bounds, lit.opacity(opacity * values[1 + index * 2])).corner_radii(radius));
                }
            },
        )
        .size_full()
    }

    /// `TimelineMinimapNavigationButton`: an `icon-micro` ghost-muted button above or below the
    /// rail, shown only while hovered.
    fn render_minimap_nav(&mut self, delta: isize, enabled: bool, interactive: bool, rail_height: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let previous = delta < 0;
        let label = if previous { minimap::PREVIOUS_TURN } else { minimap::NEXT_TURN };
        let icon = if previous { Icon::ChevronUp } else { Icon::ChevronDown };
        let focus = if previous { self.minimap.previous_focus.as_ref().unwrap() } else { self.minimap.next_focus.as_ref().unwrap() }.clone().tab_stop(enabled);
        let focused = focus.is_focused(window);
        let hovered = self.minimap.nav_hovers[usize::from(!previous)];
        let opacity = transition(if previous { "minimap-previous-fade" } else { "minimap-next-fade" }, vec![if focused || hovered { 1. } else { 0. }], self.minimap.repaint.unwrap(), window, cx)[0];
        let nav_index = usize::from(!previous);
        let tooltip_visible = self.minimap.tooltips[nav_index].visible(hovered, focused, self.minimap.nav_tooltip == Some(nav_index));
        let tooltip_opacity = transition(if previous { "minimap-previous-tooltip" } else { "minimap-next-tooltip" }, vec![if tooltip_visible { 1. } else { 0. }], self.minimap.repaint.unwrap(), window, cx)[0];
        let bounds = self.minimap.nav_bounds[nav_index].clone();
        let tooltip_bounds = bounds.clone();
        let tooltip = (tooltip_opacity > 0.).then(|| gpui::deferred(
            gpui_base::Positioner::side(tooltip_bounds.get())
                .placement(if previous { gpui_base::Placement::Top } else { gpui_base::Placement::Bottom })
                .offset(px(4.))
                .margin(px(5.))
                .child(navigation_tooltip(label, tooltip_opacity, cx))
        ).with_priority(140));
        let measure = canvas(move |bounds_now, _, _| bounds.set(bounds_now), |_, _, _, _| {}).absolute().size_full();
        let button = div()
            .size(rem(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(rem(4.))
            .child(gpui::svg().path(icon.path()).size(rem(16.)).flex_none().text_color(hsla(p.foreground).opacity(0.9)))
            .map(|this| if enabled { this.cursor_pointer().hover(|style| style.bg(hsla(p.accent))) } else { this.opacity(0.64) });
        div()
            .id(if previous { "timeline-minimap-previous" } else { "timeline-minimap-next" })
            .absolute()
            .left(rem(-6.))
            .map(|this| if previous { this.bottom(px(rail_height + 2.)) } else { this.top(px(rail_height + 2.)) })
            .opacity(opacity)
            .when(enabled, |this| this.track_focus(&focus))
            .role(gpui::Role::Button)
            .on_action(cx.listener(move |view, _: &MinimapTabNext, window, cx| {
                view.minimap_tab(if previous { minimap::FocusPart::Previous } else { minimap::FocusPart::Next }, false, window, cx);
            }))
            .on_action(cx.listener(move |view, _: &MinimapTabPrevious, window, cx| {
                view.minimap_tab(if previous { minimap::FocusPart::Previous } else { minimap::FocusPart::Next }, true, window, cx);
            }))
            .key_context("TimelineMinimap")
            .on_mouse_down(MouseButton::Left, move |_, window, _| {
                if !interactive { window.prevent_default(); }
            })
            .aria_label(label)
            .focus_visible(move |style| style.shadow(focus_ring(hsla(p.ring))))
            .on_hover(cx.listener(move |view, hovered: &bool, _, cx| {
                if !interactive { return; }
                view.minimap.nav_hovers[usize::from(!previous)] = *hovered;
                view.minimap.nav_hovered = view.minimap.nav_hovers.iter().any(|hover| *hover);
                view.minimap.nav_tooltip = None;
                view.minimap.nav_tooltip_task = None;
                if *hovered {
                    view.minimap.nav_tooltip_task = Some(cx.spawn(async move |view, cx| {
                        cx.background_executor().timer(Duration::from_millis(600)).await;
                        let _ = view.update(cx, |view, cx| {
                            if view.minimap.nav_hovers[nav_index] { view.minimap.nav_tooltip = Some(nav_index); view.notify_minimap(cx); }
                        });
                    }));
                }
                view.notify_minimap(cx);
            }))
            .when(enabled, |this| this.on_click(cx.listener(move |view, event: &gpui::ClickEvent, window, cx| {
                if interactive || event.is_keyboard() { view.step_minimap(delta, window, cx); }
            })))
            .child(button)
            .child(measure)
            .children(tooltip)
            .into_any_element()
    }
}

fn navigation_tooltip(label: &'static str, opacity: f32, cx: &gpui::App) -> impl IntoElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    div().id("minimap-navigation-tooltip").role(gpui::Role::Tooltip)
        .px(rem(8.)).py(rem(4.)).rounded(rem(6.)).border_1().border_color(hsla(p.border))
        .bg(hsla(p.popover)).text_color(hsla(p.popover_foreground)).text_size(rem(12.)).line_height(rem(16.5))
        .shadow(vec![
            BoxShadow { color: gpui::black().opacity(0.05), offset: point(px(0.), px(4.)), blur_radius: px(6.), spread_radius: px(-1.), inset: false },
            BoxShadow { color: gpui::black().opacity(0.05), offset: point(px(0.), px(2.)), blur_radius: px(4.), spread_radius: px(-2.), inset: false },
            BoxShadow { color: if dark { gpui::white().opacity(0.06) } else { gpui::black().opacity(0.04) }, offset: point(px(0.), px(if dark { -1. } else { 1. })), blur_radius: px(0.), spread_radius: px(0.), inset: true },
        ])
        .opacity(opacity).child(label)
}

/// The hover card: the message on one line and its turn's answer on up to three, centred on
/// its strip (or hanging from the first and last).
fn render_preview(index: usize, count: usize, rail_height: f32, preview: MinimapPreview, measured_height: Rc<Cell<f32>>, view: gpui::EntityId, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let height = measured_height.get();
    let measure = canvas(move |bounds, window, _| {
        let next = f32::from(bounds.size.height);
        if measured_height.replace(next) != next { window.on_next_frame(move |_, cx| cx.notify(view)); }
    }, |_, _, _, _| {}).absolute().size_full();
    let card = div()
        .id("timeline-minimap-preview")
        .relative()
        .flex_none()
        .w_full()
        .p(rem(12.))
        .rounded(px(12.))
        .text_size(rem(14.))
        .line_height(rem(20.))
        .shadow(vec![
            BoxShadow { color: gpui::black().opacity(0.25), offset: point(px(0.), px(20.)), blur_radius: px(25.), spread_radius: px(-5.), inset: false },
            BoxShadow { color: gpui::black().opacity(0.25), offset: point(px(0.), px(8.)), blur_radius: px(10.), spread_radius: px(-6.), inset: false },
        ])
        .occlude()
        .on_mouse_move(|_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, _, cx| view.forward_minimap_wheel(event, cx)))
        .on_hover(cx.listener(|view, hovered: &bool, window, cx| {
            if view.minimap.preview_hovered == *hovered { return; }
            view.minimap.preview_hovered = *hovered;
            view.minimap_hover_changed(window, cx);
        }))
        .child(crate::glass::backdrop(12., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
        .child(crate::webview::occluder("timeline-minimap-preview-cover", crate::webview::Layer::TranscriptChrome))
        .child(measure)
        .child(
            div()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.popover_foreground))
                .child(crate::selectable_text::SelectableText::new(("minimap-preview-user", index), preview.user_text.unwrap_or_else(|| minimap::USER_MESSAGE.to_owned())).tab_stop(false).tail_ellipsis()),
        )
        .children(preview.assistant_text.map(|text| div().mt(rem(4.)).line_clamp(3).text_ellipsis().text_color(hsla(p.muted_foreground)).child(crate::selectable_text::SelectableText::new(("minimap-preview-assistant", index), text).tab_stop(false).tail_ellipsis())));
    div()
        .absolute()
        .left(rem(32.))
        .top(px(rail_height * minimap::top_percent(index, count) / 100. + minimap::preview_offset(index, count, height)))
        .w(rem(320.))
        .child(card)
        .into_any_element()
}

fn focus_ring(color: gpui::Hsla) -> Vec<BoxShadow> {
    vec![BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(2.), inset: true }]
}

pub(super) fn forward_wheel(list: &gpui::ListState, disclosure: &mut t3_core::timeline::anchor::DisclosureFollow, event: &ScrollWheelEvent) -> bool {
    // The list's own line height for line-based wheels.
    let delta = event.delta.pixel_delta(px(20.)).y;
    if event.modifiers.control || delta == px(0.) {
        return false;
    }
    disclosure.cancel();
    // As the list's own wheel does: clamped, and back to following the end at the bottom.
    let top = list.scroll_px_offset_for_scrollbar();
    list.set_offset_from_scrollbar(point(px(0.), top.y + delta));
    true
}

fn strip_scale(active: Option<usize>, index: usize) -> f32 {
    match active.map(|active| active.abs_diff(index)) {
        Some(0) => 1., Some(1) => 0.667, Some(2) => 0.417, _ => 0.333,
    }
}

struct Motion {
    from: Vec<f32>,
    target: Vec<f32>,
    start: Instant,
    pending: bool,
}

// A finite 150ms CSS transition, sampled at most 30 times a second; idle schedules nothing.
fn transition(id: &'static str, target: Vec<f32>, view: gpui::EntityId, window: &mut Window, cx: &mut gpui::App) -> Vec<f32> {
    let now = cx.background_executor().now();
    let state = window.use_keyed_state(id, cx, |_, _| Motion { from: target.clone(), target: target.clone(), start: now, pending: false });
    let reduced = cx.reduce_motion();
    let (value, schedule) = state.update(cx, |state, _| {
        if state.target.len() != target.len() || reduced {
            state.from = target.clone();
            state.target = target.clone();
        }
        let progress = (now.saturating_duration_since(state.start).as_secs_f32() / 0.15).min(1.);
        let eased = gpui_base::animation::cubic_bezier(0.4, 0., 0.2, 1.)(progress);
        let value: Vec<_> = if progress == 1. { state.target.clone() } else { state.from.iter().zip(&state.target).map(|(from, to)| from + (to - from) * eased).collect() };
        if state.target != target {
            state.from = value.clone();
            state.target = target;
            state.start = now;
        }
        let running = value != state.target;
        let schedule = running && !state.pending;
        state.pending |= schedule;
        (value, schedule)
    });
    if schedule {
        let weak = state.downgrade();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(34)).await;
            cx.update(|cx| {
                if weak.update(cx, |state, _| state.pending = false).is_ok() { cx.notify(view); }
            });
        }).detach();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn descendant_height_changes_refresh_a_cached_minimap_without_owner_notifications(cx: &mut gpui::TestAppContext) {
        struct Row { height: f32 }
        impl Render for Row {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement { div().h(px(self.height)) }
        }
        struct Rail { layout: ListLayout, displayed: Rc<Cell<Measured>> }
        impl Render for Rail {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let layout = self.layout.clone();
                let displayed = self.displayed.clone();
                canvas(move |_, _, _| displayed.set(layout.measure()), |_, _, _, _| {}).size_full()
            }
        }
        struct Rows { row: Entity<Row>, layout: ListLayout, measured: Rc<Cell<Measured>>, rail: gpui::EntityId }
        impl Render for Rows {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let row = self.row.clone();
                let content = gpui::list(self.layout.list.clone(), move |index, _, _| {
                    if index == 1 { row.clone().into_any_element() } else { div().h(px(40.)).into_any_element() }
                }).size_full().into_any_element();
                self.layout.clone().observe(content, self.measured.clone(), self.rail)
            }
        }
        struct Owner { rows: Entity<Rows>, rail: Entity<Rail> }
        impl Render for Owner {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().relative().size_full()
                    .child(self.rows.clone().cached(gpui::StyleRefinement::default().size_full()))
                    .child(self.rail.clone().cached(gpui::StyleRefinement::default().absolute().inset_0()))
            }
        }
        let list = gpui::ListState::new(6, gpui::ListAlignment::Top, px(600.));
        list.scroll_to(ListOffset { item_ix: 1, offset_in_item: px(20.) });
        let layout = ListLayout { list: list.clone(), header: 0, row_count: 6, rows: Rc::new(vec![0, 2, 4]), lane_padding: 0., lane_max: None };
        let displayed = Rc::new(Cell::new(Measured::default()));
        let window = cx.open_window(size(px(200.), px(120.)), |_, cx| {
            let row = cx.new(|_| Row { height: 40. });
            let rail = cx.new(|_| Rail { layout: layout.clone(), displayed: displayed.clone() });
            let rows = cx.new(|_| Rows { row, layout: layout.clone(), measured: Rc::default(), rail: rail.entity_id() });
            Owner { rows, rail }
        });
        let owner = window.root(cx).unwrap();
        let (rows, row) = cx.read(|cx| {
            let rows = owner.read(cx).rows.clone();
            (rows.clone(), rows.read(cx).row.clone())
        });
        let owner_notifications = Rc::new(Cell::new(0));
        let _owner_subscription = cx.update(|cx| {
            let notifications = owner_notifications.clone();
            cx.observe(&owner, move |_, _| notifications.set(notifications.get() + 1))
        });
        let _rows_subscription = cx.update(|cx| {
            let notifications = owner_notifications.clone();
            cx.observe(&rows, move |_, _| notifications.set(notifications.get() + 1))
        });
        let draw = |cx: &mut gpui::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear()).unwrap();
        };
        let next_frame = |cx: &mut gpui::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| window.simulate_next_frame(cx)).unwrap()
        };
        draw(cx);
        next_frame(cx);
        draw(cx);
        assert_eq!(displayed.get().range, Some((1, 4)));
        assert_eq!(displayed.get().current, Some(1));
        for (height, range, current) in [(200., (1, 1), 0), (40., (1, 4), 1)] {
            row.update(cx, |row, cx| { row.height = height; cx.notify(); });
            draw(cx);
            assert_eq!(list.bounds_for_item(1).unwrap().size.height, px(height));
            next_frame(cx);
            draw(cx);
            assert_eq!(displayed.get().range, Some(range), "the cached visible strips must follow descendant height {height}");
            assert_eq!(displayed.get().current, Some(current), "Previous/Next must use the remeasured current item");
            assert_eq!(owner_notifications.get(), 0, "only the descendant and minimap may notify");
            assert_eq!(next_frame(cx), 0, "unchanged measurements must not request another frame");
        }
    }

    #[gpui::test]
    fn tab_enters_minimap_from_an_adjacent_control_and_skips_disabled_endpoints(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        for (previous, next) in [(true, true), (false, true), (true, false), (false, false)] {
            let (before, stops, after) = cx.update(|_, cx| (cx.focus_handle().tab_stop(true), focus_handles(cx), cx.focus_handle().tab_stop(true)));
            let [previous_focus, rail_focus, next_focus] = &stops;
            cx.draw(point(px(0.), px(0.)), size(px(200.), px(120.)), |_, _| {
                div().child(div().id("before-minimap").track_focus(&before))
                    .when(previous, |el| el.child(div().id("previous").track_focus(previous_focus)))
                    .child(div().id("rail").track_focus(rail_focus))
                    .when(next, |el| el.child(div().id("next").track_focus(next_focus)))
                    .child(div().id("after-minimap").track_focus(&after))
            });
            cx.update(|window, cx| {
                window.focus(&before, cx);
                let expected = stops.iter().enumerate().filter(|(index, _)| match index { 0 => previous, 2 => next, _ => true }).map(|(_, focus)| focus).chain(std::iter::once(&after));
                for focus in expected {
                    window.focus_next(cx);
                    assert!(focus.is_focused(window), "forward Tab: previous={previous}, next={next}");
                }
                window.focus(&after, cx);
                for focus in stops.iter().enumerate().rev().filter(|(index, _)| match index { 0 => previous, 2 => next, _ => true }).map(|(_, focus)| focus).chain(std::iter::once(&before)) {
                    window.focus_prev(cx);
                    assert!(focus.is_focused(window), "reverse Tab: previous={previous}, next={next}");
                }
            });
        }
    }
}
