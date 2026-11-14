//! The timeline minimap (`apps/web/src/components/chat/TimelineMinimap.tsx`, wired as in
//! `MessagesTimeline.tsx`): a strip per user message beside the transcript, lit while its
//! message is on screen, with a fisheye and preview card under the pointer, click to jump, and
//! previous / next turn buttons. The model is `t3_core::timeline::minimap`.
//!
//! The rail reads what is on screen from the list's height tree in its own prepaint, after the
//! list has laid out, so lit strips paint in the frame they belong to. Geometry and finite
//! motion invalidate the minimap layer; hover and motion do not invalidate the cached rows.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Bounds, BoxShadow, Context, Entity, FocusHandle, FontWeight, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollWheelEvent, Subscription, WeakEntity, Window,
    canvas, div, fill, point, prelude::*, px, size,
};
use t3_core::timeline::anchor::ScrollSource;
use t3_core::timeline::minimap::{self, MinimapItem, MinimapPreview, RailPointer, RailPointerAction, RailPointerEvent, RectF};

use super::ThreadView;
use gpui_component::ActiveTheme;
use crate::icons::Icon;
use crate::selectable_text::SelectionGroup;
use crate::theme::{hsla, palette, rem};

/// App-wide arbitration across `ThreadView` instances (e.g. main transcript vs side chat) so at
/// most one minimap navigation tooltip / hover preview stays active in the app at a time.
#[derive(Default)]
struct ActiveMinimapTooltipOwner(Option<WeakEntity<ThreadView>>);
impl gpui::Global for ActiveMinimapTooltipOwner {}

fn claim_minimap_tooltip_ownership(owner: &WeakEntity<ThreadView>, cx: &mut gpui::App) {
    let previous = cx.try_global::<ActiveMinimapTooltipOwner>().and_then(|g| g.0.clone());
    if let Some(prev) = previous
        && prev.entity_id() != owner.entity_id()
    {
        let _ = prev.update(cx, |view, cx| view.minimap_dismiss_external(cx));
    }
    if cx.has_global::<ActiveMinimapTooltipOwner>() {
        cx.global_mut::<ActiveMinimapTooltipOwner>().0 = Some(owner.clone());
    } else {
        cx.set_global(ActiveMinimapTooltipOwner(Some(owner.clone())));
    }
}

fn bounds_to_rect_f(bounds: Bounds<Pixels>) -> RectF {
    RectF {
        left: f32::from(bounds.left()),
        top: f32::from(bounds.top()),
        right: f32::from(bounds.right()),
        bottom: f32::from(bounds.bottom()),
    }
}

gpui::actions!(timeline_minimap, [MinimapTabNext, MinimapTabPrevious]);

pub fn init(cx: &mut gpui::App) {
    cx.bind_keys([
        gpui::KeyBinding::new("tab", MinimapTabNext, Some("TimelineMinimap")),
        gpui::KeyBinding::new("shift-tab", MinimapTabPrevious, Some("TimelineMinimap")),
    ]);
}

fn sync_navigation_focus(tips: &mut minimap::NavigationTooltips, index: usize, focus: &FocusHandle, window: &Window) -> bool {
    let pending = tips.deadline();
    tips.focus(index, focus.is_focused(window), window.last_input_was_keyboard());
    pending != tips.deadline()
}

fn navigation_reference_press(tips: &mut minimap::NavigationTooltips, index: usize, button: MouseButton, focus: &FocusHandle, window: &mut Window, cx: &mut gpui::App) {
    if button == MouseButton::Left {
        window.prevent_default();
        window.focus(focus, cx);
        sync_navigation_focus(tips, index, focus, window);
    }
    tips.press(index);
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
        let has_minimap = content.is_some();
        let owner = self.owner.clone();
        div().relative().size_full().children(content).when(has_minimap, |this| this.child(tooltip_dismiss_area(move |dismiss, _, cx| {
            let _ = owner.update(cx, |view, cx| view.minimap_dismiss_tooltip(dismiss, cx));
        })))
    }
}

#[derive(Clone, Copy)]
enum TooltipDismiss { Scroll, Press }

fn tooltip_dismiss_area(dismiss: impl Fn(TooltipDismiss, &mut Window, &mut gpui::App) + Clone + 'static) -> impl IntoElement {
    canvas(|_, _, _| {}, move |_, _, window, _| {
        let on_press = dismiss.clone();
        window.on_mouse_event(move |_: &gpui::MouseDownEvent, phase, window, cx| {
            if phase.capture() { on_press(TooltipDismiss::Press, window, cx); }
        });
    }).absolute().inset_0().size_full()
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

    #[cfg(test)]
    fn observe(self, content: AnyElement, measured: Rc<Cell<Measured>>, repaint: gpui::EntityId, scroll: Rc<Cell<Option<Pixels>>>, on_scroll: impl Fn(&mut gpui::App) + Clone + 'static) -> AnyElement {
        self.observe_with_groups(content, measured, repaint, scroll, None, Rc::default(), on_scroll, |_| {})
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_with_groups(
        self,
        content: AnyElement,
        measured: Rc<Cell<Measured>>,
        repaint: gpui::EntityId,
        scroll: Rc<Cell<Option<Pixels>>>,
        group_lists: Option<Rc<RefCell<HashMap<String, gpui::ListState>>>>,
        group_scrolls: Rc<RefCell<Vec<(String, Pixels)>>>,
        on_scroll: impl Fn(&mut gpui::App) + Clone + 'static,
        on_list_scroll: impl Fn(&mut gpui::App) + Clone + 'static,
    ) -> AnyElement {
        // Descendants (including nested work-group lists) can resize or scroll without notifying
        // the minimap layer's cached owner.
        let measure = canvas(move |_, window, _| {
            let next = self.measure();
            if measured.replace(next) != next { window.on_next_frame(move |_, cx| cx.notify(repaint)); }
            let position = self.list.scroll_px_offset_for_scrollbar().y;
            let main_scrolled = scroll.replace(Some(position)).is_some_and(|previous| previous != position);
            let mut descendant_scrolled = false;
            if let Some(groups) = &group_lists {
                let mut current: Vec<(String, Pixels)> = groups
                    .borrow()
                    .iter()
                    .map(|(id, list)| (id.clone(), list.scroll_px_offset_for_scrollbar().y))
                    .collect();
                current.sort_by(|a, b| a.0.cmp(&b.0));
                let mut prev = group_scrolls.borrow_mut();
                if !prev.is_empty() {
                    descendant_scrolled = current.iter().any(|(id, pos)| {
                        prev.binary_search_by(|(pid, _)| pid.cmp(id)).ok().is_some_and(|idx| prev[idx].1 != *pos)
                    });
                }
                *prev = current;
            }
            if main_scrolled {
                let on_list_scroll = on_list_scroll.clone();
                window.on_next_frame(move |_, cx| on_list_scroll(cx));
            }
            if main_scrolled || descendant_scrolled {
                let on_scroll = on_scroll.clone();
                window.on_next_frame(move |_, cx| on_scroll(cx));
            }
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
    last_preview: Option<(usize, MinimapPreview)>,
    preview: Option<(usize, MinimapPreview)>,
    preview_selection: Option<SelectionGroup>,
    preview_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    rail_pointer: Option<(f32, f32)>,
    strip_hovered: bool,
    preview_hovered: bool,
    nav_hovered: bool,
    nav_hovers: [bool; 2],
    nav_tooltip_bounds: [Rc<Cell<Option<Bounds<Pixels>>>>; 2],
    tooltip_epoch: Option<Instant>,
    tooltip_scroll: Rc<Cell<Option<Pixels>>>,
    group_scrolls: Rc<RefCell<Vec<(String, Pixels)>>>,
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
    tooltips: minimap::NavigationTooltips,
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
        if self.items.len() < minimap::MIN_ITEMS {
            self.nav_tooltip_task = None;
            self.tooltips = minimap::NavigationTooltips::default();
        }
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
        let owner = self.minimap_layer.read(cx).owner.clone();
        let scroll_owner = owner.clone();
        let has_minimap = self.minimap.items.len() >= minimap::MIN_ITEMS;
        self.minimap_list_layout(window, cx).observe_with_groups(
            transcript,
            self.minimap.measured.clone(),
            self.minimap_layer.entity_id(),
            self.minimap.tooltip_scroll.clone(),
            Some(self.group_lists.clone()),
            self.minimap.group_scrolls.clone(),
            move |cx| {
                if has_minimap {
                    let _ = owner.update(cx, |view, cx| view.minimap_dismiss_tooltip(TooltipDismiss::Scroll, cx));
                }
            },
            move |cx| {
                let _ = scroll_owner.update(cx, |view, cx| view.on_observed_list_scroll(cx));
            },
        )
    }

    pub(super) fn minimap_dismiss_external(&mut self, cx: &mut Context<Self>) {
        let changed = self.minimap.tooltips.dismiss_all(true) || self.minimap.active.is_some();
        self.minimap.nav_tooltip_task = None;
        self.minimap.active = None;
        self.minimap.strip_hovered = false;
        self.minimap.preview_hovered = false;
        self.minimap.nav_hovered = false;
        self.minimap.nav_hovers = [false; 2];
        if changed {
            self.notify_minimap(cx);
        }
    }

    pub(super) fn minimap_escape(&mut self, key: &gpui::Keystroke, _window: &Window, cx: &mut Context<Self>) -> bool {
        if key.key != "escape" { return false; }
        let dismissed = self.minimap.tooltips.escape();
        self.minimap.nav_tooltip_task = None;
        if dismissed {
            self.notify_minimap(cx);
            cx.stop_propagation();
        }
        dismissed
    }

    fn minimap_tooltip_now(&mut self, cx: &gpui::App) -> Duration {
        let now = cx.background_executor().now();
        self.minimap.tooltips.set_reduced_motion(cx.reduce_motion());
        now.saturating_duration_since(*self.minimap.tooltip_epoch.get_or_insert(now))
    }

    fn minimap_tooltip_changed(&mut self, cx: &mut Context<Self>) {
        self.minimap.nav_tooltip_task = None;
        if self.minimap.tooltips.visible().is_some() || self.minimap.tooltips.deadline().is_some() {
            claim_minimap_tooltip_ownership(&cx.entity().downgrade(), cx);
        }
        if let Some(deadline) = self.minimap.tooltips.deadline() {
            let delay = deadline.saturating_sub(self.minimap_tooltip_now(cx));
            self.minimap.nav_tooltip_task = Some(cx.spawn(async move |view, cx| {
                cx.background_executor().timer(delay).await;
                let _ = view.update(cx, |view, cx| {
                    let now = view.minimap_tooltip_now(cx);
                    view.minimap.tooltips.advance(now);
                    if view.minimap.tooltips.visible().is_some() {
                        claim_minimap_tooltip_ownership(&cx.entity().downgrade(), cx);
                    }
                    view.notify_minimap(cx);
                });
            }));
        }
        self.notify_minimap(cx);
    }

    fn minimap_dismiss_tooltip(&mut self, dismiss: TooltipDismiss, cx: &mut Context<Self>) {
        let before = (self.minimap.tooltips.visible(), self.minimap.tooltips.deadline());
        match dismiss {
            TooltipDismiss::Scroll => self.minimap.tooltips.scroll(),
            TooltipDismiss::Press => self.minimap.tooltips.outside_press(),
        }
        if before != (self.minimap.tooltips.visible(), self.minimap.tooltips.deadline()) { self.minimap_tooltip_changed(cx); }
    }

    fn notify_minimap(&self, cx: &mut Context<Self>) {
        if let Some(repaint) = self.minimap.repaint { gpui::App::notify(cx, repaint); }
    }

    /// `onSelect`: smooth-scrolls the message to just below the top and leaves follow-to-bottom.
    fn jump_to_minimap_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.disclosure.cancel();
        let Some(row_index) = self.minimap.items.get(index).map(|item| item.row_index) else { return };
        let target = self.header_count() + row_index;
        self.smooth_jump_to_item(target, minimap::JUMP_VIEW_OFFSET, ScrollSource::Minimap, window, cx);
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

    /// A leave is settled after the event, once the rail, preview, and safe-polygon corridor report.
    fn minimap_hover_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.minimap.active.is_some() || self.minimap.strip_hovered || self.minimap.preview_hovered {
            claim_minimap_tooltip_ownership(&cx.entity().downgrade(), cx);
        }
        self.notify_minimap(cx);
        let cursor = window.mouse_position();
        cx.defer_in(window, move |view, _window, cx| {
            let ui = &mut view.minimap;
            if !ui.strip_hovered && !ui.preview_hovered && ui.active.is_some() {
                let in_corridor = ui.rail_pointer.zip(ui.preview_bounds.get()).is_some_and(|(origin, bounds)| {
                    let pt = (f32::from(cursor.x), f32::from(cursor.y));
                    minimap::point_in_safe_polygon(origin, bounds_to_rect_f(bounds), pt, 6.)
                });
                if in_corridor {
                    return;
                }
                ui.active = None;
                ui.rail_pointer = None;
                view.notify_minimap(cx);
            }
        });
    }

    /// `forwardWheelToTimeline`: the rail and its preview sit over the transcript, so a wheel
    /// there scrolls the transcript, never a scroller underneath (an open command's output).
    fn forward_minimap_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if !self.forward_wheel_with_follow(event, cx) { return; }
        self.minimap_dismiss_tooltip(TooltipDismiss::Scroll, cx);
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
            self.minimap.nav_tooltip_task = None;
            let now = self.minimap_tooltip_now(cx);
            for index in 0..2 { self.minimap.tooltips.hover(index, false, now); }
        }
        let rail_height = minimap::rail_height(count, f32::from(window.viewport_size().height) - minimap::MAX_HEIGHT_INSET * rem_px);
        let active = self.minimap.active.filter(|index| *index < count);
        let preview = active.map(|index| (index, self.minimap.preview(index)));
        let preview_open = preview.is_some();
        if let Some((index, ref data)) = preview {
            if self.minimap.last_preview.as_ref().is_some_and(|(prev_ix, _)| *prev_ix != index)
                && let Some(group) = &self.minimap.preview_selection
            {
                group.clear(cx);
            }
            self.minimap.last_preview = Some((index, data.clone()));
            if self.minimap.preview_selection.is_none() {
                self.minimap.preview_selection = Some(SelectionGroup::new(cx));
            }
        }
        let preview_progress = transition_to(
            "minimap-preview-motion",
            vec![if preview_open { 1. } else { 0. }],
            false,
            self.minimap.repaint.unwrap(),
            window,
            cx,
        )[0];
        if !preview_open && preview_progress == 0. {
            self.minimap.last_preview = None;
            self.minimap.preview_bounds.set(None);
            if let Some(group) = &self.minimap.preview_selection {
                group.clear(cx);
            }
        }
        let rendered_preview = preview.or_else(|| (preview_progress > 0.).then(|| self.minimap.last_preview.clone()).flatten());
        let width = if preview_open { minimap::EXPANDED_HIT_STRIP_WIDTH * rem_px } else { measured.hit_width };
        let focused = focus.contains_focused(window, cx)
            || self.minimap.previous_focus.as_ref().is_some_and(|focus| focus.is_focused(window))
            || self.minimap.next_focus.as_ref().is_some_and(|focus| focus.is_focused(window));
        let revealed = self.minimap.strip_hovered || self.minimap.preview_hovered || self.minimap.nav_hovered || active.is_some() || focused;

        let rail = self.render_minimap_rail(active, revealed, rem_px, window, cx);
        let ring = hsla(palette(cx).ring).opacity(0.7);
        let preview_selection = self.minimap.preview_selection.clone();
        let preview_bounds = self.minimap.preview_bounds.clone();
        let jump = div()
            .id("timeline-minimap")
            .absolute()
            .inset_0()
            .child(rail)
            .track_focus(&focus)
            .key_context("TimelineMinimap")
            .role(gpui::Role::Button)
            .aria_label(format!("Jump to message: {}", rendered_preview.as_ref().and_then(|(_, preview)| preview.user_text.as_deref()).unwrap_or(minimap::USER_MESSAGE)))
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
            .children(rendered_preview.map(|(index, preview)| {
                render_preview(
                    index,
                    count,
                    rail_height,
                    preview,
                    preview_progress,
                    preview_selection.unwrap_or_else(|| SelectionGroup::new(cx)),
                    self.minimap.preview_height.clone(),
                    preview_bounds,
                    self.minimap.repaint.unwrap(),
                    cx,
                )
            }))
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
                        let pt = (f32::from(event.position.x), f32::from(event.position.y));
                        let in_safe = ui.active.is_some()
                            && index != ui.active
                            && ui.rail_pointer.zip(ui.preview_bounds.get()).is_some_and(|(origin, bounds)| {
                                minimap::point_in_safe_polygon(origin, bounds_to_rect_f(bounds), pt, 6.)
                            });
                        if !in_safe {
                            ui.rail_pointer = Some(pt);
                            if action == RailPointerAction::Track && index != ui.active {
                                ui.active = index;
                                view.notify_minimap(cx);
                            }
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
        for (index, focus) in [self.minimap.previous_focus.as_ref().unwrap(), self.minimap.next_focus.as_ref().unwrap()].into_iter().enumerate() {
            if sync_navigation_focus(&mut self.minimap.tooltips, index, focus, window) { self.minimap.nav_tooltip_task = None; }
        }
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
        let mouse_focus = focus.clone();
        let click_focus = focus.clone();
        let focused = focus.is_focused(window);
        let hovered = self.minimap.nav_hovers[usize::from(!previous)];
        let opacity = transition(if previous { "minimap-previous-fade" } else { "minimap-next-fade" }, vec![if focused || hovered { 1. } else { 0. }], self.minimap.repaint.unwrap(), window, cx)[0];
        let nav_index = usize::from(!previous);
        let tooltip_visible = self.minimap.tooltips.visible() == Some(nav_index);
        let instant = self.minimap.tooltips.instant(nav_index);
        let tooltip_progress = transition_to(if previous { "minimap-previous-tooltip" } else { "minimap-next-tooltip" }, vec![if tooltip_visible { 1. } else { 0. }], instant, self.minimap.repaint.unwrap(), window, cx)[0];
        let bounds = self.minimap.nav_bounds[nav_index].clone();
        let tooltip_bounds = bounds.clone();
        let popup_bounds = self.minimap.nav_tooltip_bounds[nav_index].clone();
        if tooltip_progress == 0. {
            popup_bounds.set(None);
        }
        let tooltip = (tooltip_progress > 0.).then(|| gpui::deferred(
            gpui_base::Positioner::side(tooltip_bounds.get())
                .placement(if previous { gpui_base::Placement::Top } else { gpui_base::Placement::Bottom })
                .offset(px(4.))
                .margin(px(5.))
                .child(navigation_tooltip(label, previous, tooltip_progress, popup_bounds, cx))
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
            .on_any_mouse_down(cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                if event.button == MouseButton::Left { window.prevent_default(); }
                if interactive && enabled {
                    navigation_reference_press(&mut view.minimap.tooltips, nav_index, event.button, &mouse_focus, window, cx);
                    view.minimap_tooltip_changed(cx);
                }
            }))
            .aria_label(label)
            .focus_visible(move |style| style.shadow(focus_ring(hsla(p.ring))))
            .on_hover(cx.listener(move |view, hovered: &bool, window, cx| {
                if !interactive { return; }
                let now = view.minimap_tooltip_now(cx);
                let position = window.mouse_position();
                if !*hovered
                    && view.minimap.tooltips.visible() == Some(nav_index)
                    && let Some(popup) = view.minimap.nav_tooltip_bounds[nav_index].get()
                {
                    let origin = view.minimap.nav_bounds[nav_index].get().center();
                    let in_corridor = minimap::point_in_safe_polygon(
                        (f32::from(origin.x), f32::from(origin.y)),
                        bounds_to_rect_f(popup),
                        (f32::from(position.x), f32::from(position.y)),
                        4.,
                    );
                    if in_corridor {
                        return;
                    }
                }
                view.minimap.nav_hovers[usize::from(!previous)] = *hovered;
                view.minimap.nav_hovered = view.minimap.nav_hovers.iter().any(|hover| *hover);
                view.minimap.tooltips.hover(nav_index, *hovered, now);
                // GPUI emits hover entry from this MouseMove, after on_mouse_move.
                if *hovered {
                    view.minimap.tooltips.pointer_move(nav_index, f32::from(position.x), f32::from(position.y), now);
                }
                view.minimap_tooltip_changed(cx);
            }))
            .on_mouse_move(cx.listener(move |view, event: &MouseMoveEvent, _, cx| {
                if !interactive { return; }
                let deadline = view.minimap.tooltips.deadline();
                let now = view.minimap_tooltip_now(cx);
                view.minimap.tooltips.pointer_move(nav_index, f32::from(event.position.x), f32::from(event.position.y), now);
                if view.minimap.tooltips.deadline() != deadline { view.minimap_tooltip_changed(cx); }
            }))
            .when(enabled, |this| this.on_click(cx.listener(move |view, event: &gpui::ClickEvent, window, cx| {
                if interactive || event.is_keyboard() {
                    sync_navigation_focus(&mut view.minimap.tooltips, nav_index, &click_focus, window);
                    view.minimap.tooltips.press(nav_index);
                    view.minimap_tooltip_changed(cx);
                    view.step_minimap(delta, window, cx);
                }
            })))
            .child(button)
            .child(measure)
            .children(tooltip)
            .into_any_element()
    }
}

fn navigation_tooltip(label: &'static str, previous: bool, progress: f32, bounds_cell: Rc<Cell<Option<Bounds<Pixels>>>>, cx: &gpui::App) -> impl IntoElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let slide = (1. - progress) * if previous { 4. } else { -4. };
    let scale_inset = px((1. - progress) * 2.);
    let measure = canvas(move |bounds, _, _| bounds_cell.set(Some(bounds)), |_, _, _, _| {}).absolute().size_full();
    div()
        .mt(px(slide))
        .p(scale_inset)
        .child(
            div().id("minimap-navigation-tooltip").role(gpui::Role::Tooltip).relative()
                .px(rem(8.)).py(rem(4.)).rounded(rem(6.)).border_1().border_color(hsla(p.border))
                .bg(hsla(p.popover)).text_color(hsla(p.popover_foreground)).text_size(rem(12.)).line_height(rem(16.5))
                .shadow(vec![
                    BoxShadow { color: gpui::black().opacity(0.05), offset: point(px(0.), px(4.)), blur_radius: px(6.), spread_radius: px(-1.), inset: false },
                    BoxShadow { color: gpui::black().opacity(0.05), offset: point(px(0.), px(2.)), blur_radius: px(4.), spread_radius: px(-2.), inset: false },
                    BoxShadow { color: if dark { gpui::white().opacity(0.06) } else { gpui::black().opacity(0.04) }, offset: point(px(0.), px(if dark { -1. } else { 1. })), blur_radius: px(0.), spread_radius: px(0.), inset: true },
                ])
                .opacity(progress)
                .child(measure)
                .child(label),
        )
}

/// The hover card: the message on one line and its turn's answer on up to three, centred on
/// its strip (or hanging from the first and last), with slide/scale enter-exit transitions and
/// cross-paragraph text selection.
#[allow(clippy::too_many_arguments)]
fn render_preview(
    index: usize,
    count: usize,
    rail_height: f32,
    preview: MinimapPreview,
    progress: f32,
    selection_group: SelectionGroup,
    measured_height: Rc<Cell<f32>>,
    measured_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    view: gpui::EntityId,
    cx: &mut Context<ThreadView>,
) -> AnyElement {
    let p = palette(cx);
    let height = measured_height.get();
    let measure = canvas(move |bounds, window, _| {
        measured_bounds.set(Some(bounds));
        let next = f32::from(bounds.size.height);
        if measured_height.replace(next) != next { window.on_next_frame(move |_, cx| cx.notify(view)); }
    }, |_, _, _, _| {}).absolute().size_full();
    let backdrop_inset = px((1. - progress) * 3.);
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
        .child(div().absolute().inset(backdrop_inset).child(crate::glass::backdrop(12., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx)))
        .child(crate::webview::occluder("timeline-minimap-preview-cover", crate::webview::Layer::TranscriptChrome))
        .child(measure)
        .child(
            div()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.popover_foreground))
                .child(
                    crate::selectable_text::SelectableText::new(("minimap-preview-user", index), preview.user_text.unwrap_or_else(|| minimap::USER_MESSAGE.to_owned()))
                        .selection_group(selection_group.clone(), 0)
                        .tab_stop(false)
                        .tail_ellipsis(),
                ),
        )
        .children(preview.assistant_text.map(|text| {
            div()
                .mt(rem(4.))
                .line_clamp(3)
                .text_ellipsis()
                .text_color(hsla(p.muted_foreground))
                .child(
                    crate::selectable_text::SelectableText::new(("minimap-preview-assistant", index), text)
                        .selection_group(selection_group, 1)
                        .tab_stop(false)
                        .tail_ellipsis(),
                )
        }));
    div()
        .absolute()
        .left(rem(32.))
        .ml(px(-4. * (1. - progress)))
        .top(px(rail_height * minimap::top_percent(index, count) / 100. + minimap::preview_offset(index, count, height)))
        .w(rem(320.))
        .opacity(progress)
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
    transition_to(id, target, false, view, window, cx)
}

fn transition_to(id: &'static str, target: Vec<f32>, instant: bool, view: gpui::EntityId, window: &mut Window, cx: &mut gpui::App) -> Vec<f32> {
    let now = cx.background_executor().now();
    let state = window.use_keyed_state(id, cx, |_, _| Motion { from: target.clone(), target: target.clone(), start: now, pending: false });
    let reduced = cx.reduce_motion();
    let (value, schedule) = state.update(cx, |state, _| {
        if state.target.len() != target.len() || reduced || instant {
            state.from = target.clone();
            state.target = target.clone();
        }
        let progress = (now.saturating_duration_since(state.start).as_secs_f32() / minimap::MOTION_DURATION.as_secs_f32()).min(1.);
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
    fn navigation_focus_is_sampled_without_relying_on_focus_event_delivery(cx: &mut gpui::TestAppContext) {
        struct Nav { focus: FocusHandle, tips: minimap::NavigationTooltips }
        impl Render for Nav {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                sync_navigation_focus(&mut self.tips, 0, &self.focus, window);
                let owner = cx.entity().downgrade();
                div().relative().size_full()
                    .child(div().id("navigation-focus").track_focus(&self.focus).size_full()
                        .on_any_mouse_down(cx.listener(|nav, event: &gpui::MouseDownEvent, window, cx| {
                            navigation_reference_press(&mut nav.tips, 0, event.button, &nav.focus, window, cx);
                        })))
                    .child(tooltip_dismiss_area(move |_, _, cx| {
                        owner.update(cx, |nav, _| nav.tips.outside_press()).unwrap();
                    }))
            }
        }
        let window = cx.open_window(size(px(200.), px(120.)), |_, cx| Nav { focus: cx.focus_handle(), tips: minimap::NavigationTooltips::default() });
        let nav = window.root(cx).unwrap();
        let focus = nav.read_with(cx, |nav, _| nav.focus.clone());
        let draw = |cx: &mut gpui::TestAppContext| { cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear()).unwrap(); };
        draw(cx);
        cx.update_window(window.into(), |_, window, cx| window.focus(&focus, cx)).unwrap();
        draw(cx);
        nav.read_with(cx, |nav, _| assert_eq!(nav.tips.visible(), None, "programmatic pointer-modality focus is not focus-visible"));
        cx.update_window(window.into(), |_, window, _| window.blur()).unwrap();
        draw(cx);
        cx.simulate_keystrokes(window.into(), "a");
        cx.update_window(window.into(), |_, window, cx| window.focus(&focus, cx)).unwrap();
        draw(cx);
        nav.read_with(cx, |nav, _| assert_eq!(nav.tips.visible(), Some(0)));
        nav.update(cx, |nav, _| { nav.tips.escape(); });
        draw(cx);
        nav.read_with(cx, |nav, _| assert_eq!(nav.tips.visible(), None, "sampling unchanged focus must preserve Escape"));
        cx.update_window(window.into(), |_, window, _| window.blur()).unwrap();
        draw(cx);
        cx.update_window(window.into(), |_, window, cx| window.focus(&focus, cx)).unwrap();
        draw(cx);
        nav.read_with(cx, |nav, _| assert_eq!(nav.tips.visible(), Some(0)));
        cx.update_window(window.into(), |_, window, _| window.blur()).unwrap();
        draw(cx);
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            visual.update(|window, cx| { window.blur(); window.draw(cx).clear(); });
            nav.update(visual, |nav, _| {
                nav.tips = minimap::NavigationTooltips::default();
                nav.tips.hover(0, true, Duration::ZERO);
                nav.tips.pointer_move(0, 50., 50., Duration::ZERO);
                nav.tips.advance(Duration::from_millis(600));
                assert!(!nav.tips.instant(0));
            });
            visual.simulate_mouse_move(point(px(50.), px(50.)), None, Default::default());
            visual.simulate_mouse_down(point(px(50.), px(50.)), button, Default::default());
            visual.update(|window, cx| window.draw(cx).clear());
            nav.read_with(visual, |nav, _| assert_eq!(nav.tips.visible(), None, "a held reference press cannot reopen through newly acquired focus"));
            nav.read_with(visual, |nav, _| assert!(nav.tips.instant(0), "reference press overrides outside-press capture, button={button:?}"));
            visual.simulate_mouse_up(point(px(50.), px(50.)), button, Default::default());
        }
    }

    #[gpui::test]
    fn tooltip_press_capture_survives_consuming_children_but_wheel_without_scrolling_does_not_dismiss(cx: &mut gpui::TestAppContext) {
        struct Area { tips: minimap::NavigationTooltips }
        impl Render for Area {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let owner = cx.entity().downgrade();
                div().relative().w(px(200.)).h(px(120.))
                    .child(div().id("consuming-preview").size_full()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()))
                    .child(tooltip_dismiss_area(move |dismiss, _, cx| {
                        owner.update(cx, |area, _| match dismiss {
                            TooltipDismiss::Scroll => area.tips.scroll(),
                            TooltipDismiss::Press => area.tips.outside_press(),
                        }).unwrap();
                    }))
            }
        }
        let window = cx.open_window(size(px(200.), px(120.)), |_, _| Area { tips: minimap::NavigationTooltips::default() });
        let area = window.root(cx).unwrap();
        let cx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        for pending in [true, false] {
            area.update(cx, |area, _| {
                area.tips = minimap::NavigationTooltips::default();
                area.tips.hover(0, true, Duration::ZERO);
                area.tips.pointer_move(0, 10., 20., Duration::ZERO);
                if !pending { area.tips.advance(Duration::from_millis(600)); }
            });
            cx.update(|window, cx| window.draw(cx).clear());
            cx.simulate_event(gpui::ScrollWheelEvent {
                position: point(px(50.), px(50.)), delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-40.))),
                modifiers: Default::default(), touch_phase: gpui::TouchPhase::Moved,
            });
            area.update(cx, |area, _| {
                area.tips.advance(Duration::from_secs(2));
                assert_eq!(area.tips.visible(), Some(0), "wheel intent without offset change must not dismiss");
                assert_eq!(area.tips.deadline(), None);
                area.tips.focus(0, true, true);
            });
            cx.simulate_event(gpui::ScrollWheelEvent {
                position: point(px(50.), px(50.)), delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-40.))),
                modifiers: Default::default(), touch_phase: gpui::TouchPhase::Moved,
            });
            area.read_with(cx, |area, _| assert_eq!(area.tips.visible(), Some(0)));
            cx.simulate_mouse_down(point(px(50.), px(50.)), MouseButton::Left, Default::default());
            cx.simulate_mouse_up(point(px(50.), px(50.)), MouseButton::Left, Default::default());
            area.read_with(cx, |area, _| assert_eq!(area.tips.visible(), None));
        }
    }

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
        struct Rows { row: Entity<Row>, layout: ListLayout, measured: Rc<Cell<Measured>>, scroll: Rc<Cell<Option<Pixels>>>, scroll_changes: Rc<Cell<usize>>, rail: gpui::EntityId }
        impl Render for Rows {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let row = self.row.clone();
                let content = gpui::list(self.layout.list.clone(), move |index, _, _| {
                    if index == 1 { row.clone().into_any_element() } else { div().h(px(40.)).into_any_element() }
                }).size_full().into_any_element();
                let changes = self.scroll_changes.clone();
                self.layout.clone().observe(content, self.measured.clone(), self.rail, self.scroll.clone(), move |_| changes.set(changes.get() + 1))
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
        list.scroll_to(gpui::ListOffset { item_ix: 1, offset_in_item: px(20.) });
        let layout = ListLayout { list: list.clone(), header: 0, row_count: 6, rows: Rc::new(vec![0, 2, 4]), lane_padding: 0., lane_max: None };
        let displayed = Rc::new(Cell::new(Measured::default()));
        let scroll_changes = Rc::new(Cell::new(0));
        let window = cx.open_window(size(px(200.), px(120.)), |_, cx| {
            let row = cx.new(|_| Row { height: 40. });
            let rail = cx.new(|_| Rail { layout: layout.clone(), displayed: displayed.clone() });
            let rows = cx.new(|_| Rows { row, layout: layout.clone(), measured: Rc::default(), scroll: Rc::default(), scroll_changes: scroll_changes.clone(), rail: rail.entity_id() });
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
        assert_eq!(scroll_changes.get(), 0, "initial measurement is not scrolling");
        list.scroll_by(px(40.));
        rows.update(cx, |_, cx| cx.notify());
        draw(cx);
        next_frame(cx);
        draw(cx);
        assert_eq!(scroll_changes.get(), 1, "actual list scrolling invokes dismissal once");
        assert_eq!(next_frame(cx), 0, "settled scroll offsets schedule no work");
        list.scroll_to(gpui::ListOffset { item_ix: 1, offset_in_item: px(20.) });
        rows.update(cx, |_, cx| cx.notify());
        draw(cx);
        next_frame(cx);
        draw(cx);
        owner_notifications.set(0);
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

    #[gpui::test]
    fn descendant_group_list_scroll_dismisses_tooltip_and_app_arbitration_closes_prior_owner(cx: &mut gpui::TestAppContext) {
        struct ScrollerOwner {
            main_list: gpui::ListState,
            group_lists: Rc<RefCell<HashMap<String, gpui::ListState>>>,
            group_scrolls: Rc<RefCell<Vec<(String, Pixels)>>>,
            dismiss_count: Rc<Cell<usize>>,
        }
        impl Render for ScrollerOwner {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let layout = ListLayout {
                    list: self.main_list.clone(),
                    header: 0,
                    row_count: 1,
                    rows: Rc::new(vec![0]),
                    lane_padding: 0.,
                    lane_max: None,
                };
                let count = self.dismiss_count.clone();
                let child = self.group_lists.borrow().get("g1").cloned().unwrap();
                let content = div()
                    .size_full()
                    .child(gpui::list(child, |_, _, _| div().h(px(40.)).into_any_element()).h(px(120.)))
                    .into_any_element();
                layout.observe_with_groups(
                    content,
                    Rc::default(),
                    gpui::EntityId::from(1u64),
                    Rc::default(),
                    Some(self.group_lists.clone()),
                    self.group_scrolls.clone(),
                    move |_| count.set(count.get() + 1),
                    |_| {},
                )
            }
        }
        let main_list = gpui::ListState::new(1, gpui::ListAlignment::Top, px(200.));
        let child_list = gpui::ListState::new(10, gpui::ListAlignment::Top, px(200.));
        let group_lists = Rc::new(RefCell::new(HashMap::from([("g1".to_owned(), child_list.clone())])));
        let dismiss_count = Rc::new(Cell::new(0));
        let window = cx.open_window(size(px(200.), px(120.)), |_, _| ScrollerOwner {
            main_list,
            group_lists,
            group_scrolls: Rc::default(),
            dismiss_count: dismiss_count.clone(),
        });
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            window.draw(cx).clear();
            window.simulate_next_frame(cx);
        });
        assert_eq!(dismiss_count.get(), 0);

        child_list.scroll_by(px(32.));
        visual.update(|window, cx| {
            window.draw(cx).clear();
            window.simulate_next_frame(cx);
        });
        assert_eq!(dismiss_count.get(), 1, "nested group_list scroll must trigger tooltip scroll dismissal");
    }
}
