//! The timeline minimap (`apps/web/src/components/chat/TimelineMinimap.tsx`, wired as in
//! `MessagesTimeline.tsx`): a strip per user message beside the transcript, lit while its
//! message is on screen, with a fisheye and preview card under the pointer, click to jump, and
//! previous / next turn buttons. The model is `t3_core::timeline::minimap`.
//!
//! The rail reads what is on screen from the list's height tree in its own prepaint, after the
//! list has laid out, so lit strips paint in the frame they belong to. Only a change of current
//! turn or gutter asks for one more render, as the web's `setState` does; nothing repaints
//! while the transcript is still.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, Bounds, BoxShadow, Context, FontWeight, ListOffset, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent, Window, canvas,
    div, fill, point, prelude::*, px, size,
};
use t3_core::timeline::minimap::{self, MinimapItem, MinimapPreview, RailPointer, RailPointerAction, RailPointerEvent};

use super::ThreadView;
use crate::icons::Icon;
use crate::theme::{hsla, palette, rem};

/// What the rail last measured after layout. Render draws from it; click handlers read it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Measured {
    current: Option<usize>,
    persistent: bool,
    hit_width: f32,
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
    pointer: RailPointer,
    rail: Rc<Cell<Bounds<Pixels>>>,
    measured: Rc<Cell<Measured>>,
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
        self.minimap.active = None;
        cx.notify();
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
        cx.notify();
        cx.defer_in(window, |view, _, cx| {
            let ui = &mut view.minimap;
            if !ui.strip_hovered && !ui.preview_hovered && ui.active.is_some() {
                ui.active = None;
                cx.notify();
            }
        });
    }

    /// `forwardWheelToTimeline`: the rail and its preview sit over the transcript, so a wheel
    /// there scrolls the transcript, never a scroller underneath (an open command's output).
    fn forward_minimap_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        // The list's own line height for line-based wheels.
        let delta = event.delta.pixel_delta(px(20.)).y;
        if event.modifiers.control || delta == px(0.) {
            return;
        }
        // As the list's own wheel does: clamped, and back to following the end at the bottom.
        let top = self.list.scroll_px_offset_for_scrollbar();
        self.list.set_offset_from_scrollbar(point(px(0.), top.y + delta));
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_minimap(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = self.minimap.items.len();
        if count < minimap::MIN_ITEMS {
            return None;
        }
        let rem_px = f32::from(window.rem_size()) / 16.;
        let measured = self.minimap.measured.get();
        let interactive = measured.hit_width > 0.;
        let navigation = interactive && minimap::navigation_interactive(measured.hit_width);
        // A rail that stops taking the pointer can no longer report a leave.
        if !interactive {
            (self.minimap.active, self.minimap.strip_hovered, self.minimap.preview_hovered) = (None, false, false);
            self.minimap.pointer = RailPointer::default();
        }
        if !navigation {
            self.minimap.nav_hovered = false;
        }
        let rail_height = minimap::rail_height(count, f32::from(window.viewport_size().height) - minimap::MAX_HEIGHT_INSET * rem_px);
        let active = self.minimap.active.filter(|index| *index < count);
        let preview = active.map(|index| (index, self.minimap.preview(index)));
        let width = if preview.is_some() { minimap::EXPANDED_HIT_STRIP_WIDTH * rem_px } else { measured.hit_width };
        let revealed = self.minimap.strip_hovered || self.minimap.preview_hovered || self.minimap.nav_hovered || active.is_some();

        let rail = self.render_minimap_rail(active, revealed, rem_px, window, cx);
        let jump = div()
            .id("timeline-minimap")
            .absolute()
            .inset_0()
            .child(rail)
            .children(preview.map(|(index, preview)| render_preview(index, count, rail_height, preview, cx)))
            .when(interactive, |this| {
                this.cursor_pointer()
                    .occlude()
                    .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, _, cx| view.forward_minimap_wheel(event, cx)))
                    .on_hover(cx.listener(|view, hovered: &bool, window, cx| {
                        view.minimap.strip_hovered = *hovered;
                        view.minimap_hover_changed(window, cx);
                    }))
                    .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _, cx| {
                        let ui = &mut view.minimap;
                        let action = ui.pointer.handle(RailPointerEvent::Move { on_rail: true, pressed: event.pressed_button.is_some() });
                        let index = ui.index_at(event.position.y);
                        if action == RailPointerAction::Track && index != ui.active {
                            ui.active = index;
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, cx.listener(|view, _, _, _| {
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
        let previous = navigation.then(|| self.render_minimap_nav(-1, previous.is_some(), rail_height, cx));
        let next = navigation.then(|| self.render_minimap_nav(1, next.is_some(), rail_height, cx));
        let strip = div().relative().flex_none().ml(rem(12.)).w(px(width)).h(px(rail_height)).children(previous).child(jump).children(next);
        Some(div().absolute().top_0().bottom_0().left_0().w(rem(72.)).flex().flex_col().justify_center().child(strip).into_any_element())
    }

    /// The strips, painted from the list's layout of this frame.
    fn render_minimap_rail(&self, active: Option<usize>, revealed: bool, rem_px: f32, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let list = self.list.clone();
        let header = self.header_count();
        let row_count = self.rows.len();
        let rows = self.minimap.row_indices.clone();
        let rail_cell = self.minimap.rail.clone();
        let measured_cell = self.minimap.measured.clone();
        let rendered = measured_cell.get();
        let lane_padding = 2. * self.gutter() * rem_px;
        let lane_max = crate::theme::chat_max_width(cx).map(|width| f32::from(width.to_pixels(window.rem_size())));
        let view = cx.entity_id();
        let p = palette(cx);
        let (line, strip, strip_active, lit) =
            (hsla(p.border).opacity(0.15), hsla(p.muted_foreground).opacity(0.25), hsla(p.muted_foreground).opacity(0.6), hsla(p.foreground).opacity(0.55));
        canvas(
            move |bounds, window, _| {
                rail_cell.set(bounds);
                let viewport = list.viewport_bounds();
                let width = f32::from(viewport.size.width);
                let lane = (width - lane_padding).max(0.);
                let lane = lane_max.map_or(lane, |max| lane.min(max));
                let scroll = -f32::from(list.scroll_px_offset_for_scrollbar().y);
                let range = minimap::visible_range(row_count, scroll, f32::from(viewport.size.height), |index| f32::from(list.offset_for_item(header + index)));
                let next = Measured {
                    current: minimap::current_index(range, rows.iter().copied()),
                    persistent: minimap::has_persistent_gutter(width, lane),
                    hit_width: minimap::hit_strip_width(width, lane),
                };
                measured_cell.set(next);
                if next != rendered {
                    window.on_next_frame(move |_, cx| cx.notify(view));
                }
                (range, next.persistent, rows)
            },
            move |bounds, (range, persistent, rows), window, _| {
                if !persistent && !revealed {
                    return;
                }
                let count = rows.len();
                let (left, top, height) = (bounds.left(), bounds.top(), bounds.size.height);
                window.paint_quad(fill(Bounds::new(point(left + px(12. * rem_px), top), size(px(1.), height)), line));
                let thickness = px(2. * rem_px);
                for (index, row) in rows.iter().enumerate() {
                    let distance = active.map(|active| active.abs_diff(index));
                    let scale = match distance {
                        Some(0) => 1.,
                        Some(1) => 0.667,
                        Some(2) => 0.417,
                        _ => 0.333,
                    };
                    let y = top + height * (minimap::top_percent(index, count) / 100.) - thickness / 2.;
                    let bounds = Bounds::new(point(left, y), size(px(24. * rem_px * scale), thickness));
                    let radius = thickness / 2.;
                    window.paint_quad(fill(bounds, if distance == Some(0) { strip_active } else { strip }).corner_radii(radius));
                    if minimap::is_row_visible(*row, range) {
                        window.paint_quad(fill(bounds, lit).corner_radii(radius));
                    }
                }
            },
        )
        .size_full()
    }

    /// `TimelineMinimapNavigationButton`: an `icon-micro` ghost-muted button above or below the
    /// rail, shown only while hovered.
    fn render_minimap_nav(&self, delta: isize, enabled: bool, rail_height: f32, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let previous = delta < 0;
        let label = if previous { minimap::PREVIOUS_TURN } else { minimap::NEXT_TURN };
        let icon = if previous { Icon::ChevronUp } else { Icon::ChevronDown };
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
            .opacity(0.)
            .hover(|style| style.opacity(1.))
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label).build(window, cx))
            .on_hover(cx.listener(|view, hovered: &bool, _, cx| {
                view.minimap.nav_hovered = *hovered;
                cx.notify();
            }))
            .when(enabled, |this| this.on_click(cx.listener(move |view, _, window, cx| view.step_minimap(delta, window, cx))))
            .child(button)
            .into_any_element()
    }
}

/// The hover card: the message on one line and its turn's answer on up to three, centred on
/// its strip (or hanging from the first and last).
fn render_preview(index: usize, count: usize, rail_height: f32, preview: MinimapPreview, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
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
        .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, _, cx| view.forward_minimap_wheel(event, cx)))
        .on_hover(cx.listener(|view, hovered: &bool, window, cx| {
            view.minimap.preview_hovered = *hovered;
            view.minimap_hover_changed(window, cx);
        }))
        .child(crate::glass::backdrop(12., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
        .child(crate::webview::occluder("timeline-minimap-preview-cover", crate::webview::Layer::TranscriptChrome))
        .child(
            div()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.popover_foreground))
                .child(preview.user_text.unwrap_or_else(|| minimap::USER_MESSAGE.to_owned())),
        )
        .children(preview.assistant_text.map(|text| div().mt(rem(4.)).line_clamp(3).text_ellipsis().text_color(hsla(p.muted_foreground)).child(text)));
    div()
        .absolute()
        .left(rem(32.))
        .top(px(rail_height * minimap::top_percent(index, count) / 100.))
        .w(rem(320.))
        .h(px(0.))
        .flex()
        .flex_col()
        .map(|this| match index {
            0 => this.justify_start(),
            _ if index + 1 == count => this.justify_end(),
            _ => this.justify_center(),
        })
        .child(card)
        .into_any_element()
}
