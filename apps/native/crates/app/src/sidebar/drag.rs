//! Dragging rows (design-system §7.2, `sidebar/SidebarThreadRow.tsx`,
//! `sidebar/SidebarListMarkers.tsx`). The dragged row stays in place,
//! dimmed; a raised copy follows the pointer with the drop badge, and a line marks the gap the
//! row will land in. Drop resolution and planning live in `t3_core::sidebar::drag`.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use gpui::{AnyElement, App, Context, DragMoveEvent, Entity, FontWeight, IntoElement, MouseButton, MouseMoveEvent, ParentElement, Pixels, Point, Render, SharedString, Styled, Window, div, prelude::*, px};
use t3_core::sidebar::Section;
use t3_core::sidebar::drag::{DropVerb, Marker, SweepAction, resolve_drop_verb, sweep_keys};
use t3_core::sidebar::selection::BulkAction;
use t3_core::sidebar::tag_filter::ThreadKey;
use t3_core::thread_actions::ThreadAction;

use super::SidebarPane;
use crate::icons::Icon;
use crate::theme::{Palette, hsla, palette};

const PREVIEW_WIDTH: f32 = 240.;
const BOUNDARY_HEIGHT: f32 = 24.;
const PLACEHOLDER_HEIGHT: f32 = 36.;
pub(super) const SIDEBAR_DRAG_DISTANCE: f32 = 6.;

fn pointer_distance(a: Point<Pixels>, b: Point<Pixels>) -> f32 {
    let delta = a - b;
    f32::from(delta.x).hypot(f32::from(delta.y))
}

/// The drag payload of a sidebar thread row.
pub struct DraggedThread {
    pub id: String,
    pub key: Option<t3_core::sidebar::tag_filter::ThreadKey>,
    pub activated: Arc<AtomicBool>,
}

impl DraggedThread {
    pub(super) fn new(id: String, key: Option<ThreadKey>) -> Self {
        Self { id, key, activated: Arc::new(AtomicBool::new(false)) }
    }

    pub(crate) fn is_activated(&self) -> bool {
        self.activated.load(Ordering::Relaxed)
    }
}

/// The pointer's gap while dragging: the row it is over, below its midpoint when `after`.
#[derive(Default)]
pub(super) struct PaneDrag {
    pub at: Option<(usize, bool)>,
    /// The section a drop at `at` lands in, when it would change anything.
    pub target: Option<Section>,
    pub preview: Option<Entity<ThreadDragPreview>>,
    pub pointer_drag: bool,
}

pub(super) struct PendingThreadPress {
    pub id: String,
    pub key: Option<ThreadKey>,
    pub row_id: SharedString,
    pub down_pos: Point<Pixels>,
    pub click_count: usize,
    pub reorderable: bool,
    pub search_result: bool,
    pub left_held: bool,
    pub drag_attached: bool,
    pub activated: Arc<AtomicBool>,
    pub preview: Option<Entity<ThreadDragPreview>>,
}

pub struct ThreadDragPreview {
    title: SharedString,
    project: Option<SharedString>,
    verb: Option<DropVerb>,
    visual: crate::project_icon::ProjectVisual,
    visible: bool,
}

impl ThreadDragPreview {
    pub fn new(title: String, project: Option<String>, visual: crate::project_icon::ProjectVisual) -> Self {
        ThreadDragPreview { title: title.into(), project: project.map(Into::into), verb: None, visual, visible: true }
    }

    pub fn new_hidden(title: String, project: Option<String>, visual: crate::project_icon::ProjectVisual) -> Self {
        ThreadDragPreview { title: title.into(), project: project.map(Into::into), verb: None, visual, visible: false }
    }

    pub(super) fn show(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            self.visible = true;
            cx.notify();
        }
    }
}

pub(super) fn verb_icon(verb: DropVerb) -> Icon {
    match verb {
        DropVerb::Pin => Icon::Pin,
        DropVerb::Unpin => Icon::PinOff,
        DropVerb::Settle => Icon::CircleCheck,
        DropVerb::Unsettle => Icon::Undo2,
        DropVerb::Wake => Icon::AlarmClockOff,
    }
}

pub(super) fn drop_verb_badge(verb: DropVerb, p: Palette) -> gpui::Div {
    div()
        .flex_none()
        .h(px(20.))
        .px(px(6.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(4.))
        .border_1()
        .border_color(hsla(p.primary).opacity(0.4))
        .bg(hsla(p.primary).opacity(0.1))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.primary))
        .child(verb_icon(verb).sized(px(12.)))
        .child(verb.badge().1)
}

impl Render for ThreadDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().into_any_element();
        }
        let p = palette(cx);
        let badge = self.verb.map(|verb| drop_verb_badge(verb, p));
        div()
            .w(px(PREVIEW_WIDTH))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(6.))
            .bg(hsla(p.sidebar))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(self.visual.render(crate::project_icon::size(16., cx), hsla(p.sidebar_icon), cx))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(hsla(p.sidebar_secondary_label)).children(self.project.clone()))
                    .children(badge),
            )
            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(self.title.clone()))
            .into_any_element()
    }
}

impl SidebarPane {
    pub(super) fn record_thread_press(&mut self, press: PendingThreadPress) {
        self.pending_project_press = None;
        self.pending_thread_press = Some(press);
    }

    pub(super) fn record_project_press(&mut self, press: PendingProjectPress) {
        self.pending_thread_press = None;
        self.pending_project_press = Some(press);
    }

    pub(super) fn observe_pointer_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_thread_press.is_none() && self.pending_project_press.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            if let Some(press) = self.pending_thread_press.take()
                && press.drag_attached
            {
                cx.stop_active_drag(window);
                if press.activated.load(Ordering::Relaxed) {
                    self.finish_drag(false, cx);
                }
            }
            if let Some(press) = self.pending_project_press.take()
                && press.drag_attached
            {
                cx.stop_active_drag(window);
                if press.activated.load(Ordering::Relaxed) {
                    self.finish_project_drag(false, cx);
                }
            }
            return;
        }
        if let Some(press) = self.pending_thread_press.as_mut() {
            press.left_held = true;
        }
        if let Some(press) = self.pending_project_press.as_mut() {
            press.left_held = true;
        }
    }

    /// The drag constructor ran: the list shows its drop markers.
    pub(super) fn begin_drag(&mut self, thread_id: &str, key: Option<&t3_core::sidebar::tag_filter::ThreadKey>, preview: Entity<ThreadDragPreview>, cx: &mut Context<Self>) {
        // Set pane drag before controller updates emit AppEvent::Shell so sync_list_rows sees an active drag for #293 scroll preservation.
        self.hover.dismiss(cx);
        self.drag = Some(PaneDrag { preview: Some(preview), ..Default::default() });
        self.state.update(cx, |state, cx| match key { Some(key) => state.start_scoped_thread_drag(key, cx), None => state.start_thread_drag(thread_id, cx) });
        let started = {
            let state = self.state.read(cx);
            state.sidebar.drag.is_some() || state.sidebar.fleet_drag.is_some()
        };
        if !started {
            self.drag = None;
        }
        cx.notify();
    }

    pub(super) fn begin_pointer_drag(&mut self, thread_id: &str, key: Option<&ThreadKey>, preview: Entity<ThreadDragPreview>, cx: &mut Context<Self>) {
        self.begin_drag(thread_id, key, preview, cx);
        if let Some(drag) = self.drag.as_mut() {
            drag.pointer_drag = true;
        }
    }

    pub(super) fn attach_thread_drag(
        &mut self,
        dragged: &DraggedThread,
        title: String,
        project: Option<String>,
        visual: crate::project_icon::ProjectVisual,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ThreadDragPreview> {
        let Some(press) = self
            .pending_thread_press
            .as_mut()
            .filter(|press| press.id == dragged.id && press.key == dragged.key && press.left_held)
        else {
            window.defer(cx, |window, cx| {
                cx.stop_active_drag(window);
            });
            return cx.new(|_| ThreadDragPreview::new_hidden(title, project, visual));
        };
        let down_pos = press.down_pos;
        let reorderable = press.reorderable;
        let search_result = press.search_result;
        press.drag_attached = true;
        press.activated = dragged.activated.clone();
        let crossed = pointer_distance(window.mouse_position(), down_pos) > SIDEBAR_DRAG_DISTANCE;
        let preview = cx.new(|_| {
            if crossed {
                ThreadDragPreview::new(title, project, visual)
            } else {
                ThreadDragPreview::new_hidden(title, project, visual)
            }
        });
        if let Some(press) = self.pending_thread_press.as_mut() {
            press.preview = Some(preview.clone());
        }
        if crossed {
            dragged.activated.store(true, Ordering::Relaxed);
            self.hover.dismiss(cx);
            if reorderable && !search_result {
                self.begin_pointer_drag(&dragged.id, dragged.key.as_ref(), preview.clone(), cx);
            }
        }
        preview
    }

    pub(super) fn activate_thread_drag_if_ready(
        &mut self,
        event: &DragMoveEvent<DraggedThread>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.event.pressed_button != Some(MouseButton::Left) {
            self.pending_thread_press = None;
            cx.stop_active_drag(window);
            self.finish_drag(false, cx);
            return;
        }
        if event.drag(cx).is_activated() {
            return;
        }
        let Some(press) = self.pending_thread_press.as_mut() else {
            cx.stop_active_drag(window);
            self.finish_drag(false, cx);
            return;
        };
        if pointer_distance(event.event.position, press.down_pos) <= SIDEBAR_DRAG_DISTANCE {
            return;
        }
        press.activated.store(true, Ordering::Relaxed);
        event.drag(cx).activated.store(true, Ordering::Relaxed);
        let id = press.id.clone();
        let key = press.key.clone();
        let reorderable = press.reorderable;
        let search_result = press.search_result;
        let preview = press.preview.clone();
        if let Some(preview) = preview.as_ref() {
            preview.update(cx, |preview, cx| preview.show(cx));
        }
        self.hover.dismiss(cx);
        if reorderable && !search_result
            && let Some(preview) = preview
        {
            self.begin_pointer_drag(&id, key.as_ref(), preview, cx);
        }
    }

    pub(super) fn finish_thread_drop(&mut self, drop: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Leave `cx.active_drag` untouched on Capture `mouse_up_out` so Composer's Bubble `on_drop` can still receive activated drags.
        let pending = self.pending_thread_press.take();
        if let Some(press) = pending
            && !press.activated.load(Ordering::Relaxed)
        {
            self.finish_drag(false, cx);
            if drop {
                let pos = window.mouse_position();
                if pointer_distance(pos, press.down_pos) <= SIDEBAR_DRAG_DISTANCE
                    && self.hover.row_bounds_for(&press.row_id).is_none_or(|b| b.contains(&pos))
                {
                    if press.search_result {
                        let key = press.key.unwrap_or_else(|| ThreadKey::new("", &press.id));
                        self.hover.dismiss(cx);
                        self.activate_search_result(key, window, cx);
                    } else {
                        self.activate_thread_row_click(
                            &press.id,
                            press.key.as_ref(),
                            window.modifiers(),
                            press.click_count,
                            window,
                            cx,
                        );
                    }
                }
            }
            return;
        }
        self.finish_drag(drop, cx);
    }

    fn settled_placeholder_row_index(&self) -> Option<usize> {
        self.row_keys.iter().position(|key| matches!(
            key,
            super::PaneRowKey::Thread(t3_core::sidebar::RowKey::Marker(Marker::SettledPlaceholder))
                | super::PaneRowKey::Scoped(t3_core::sidebar::fleet::ScopedRowKey::Marker(Marker::SettledPlaceholder))
        ))
    }

    fn update_thread_drag_position(&mut self, row: usize, after: bool, cx: &mut Context<Self>) {
        let previous_target = self.drag.as_ref().and_then(|drag| drag.target);
        let Some(drag) = self.drag.as_mut().filter(|drag| drag.at != Some((row, after))) else { return };
        drag.at = Some((row, after));
        let state = self.state.read(cx);
        let from = state.sidebar.fleet_drag.as_ref().map(|(_, from)| *from).or_else(|| state.sidebar.drag.as_ref().map(|(_, from)| *from));
        let target = if state.sidebar.fleet_drag.is_some() { state.scoped_thread_drop_target(row + usize::from(after), cx).map(|(target, _)| target.section) }
            else { state.sidebar.model.drop_slot(row, after).and_then(|slot| state.thread_drop_target(slot)).map(|(target, _)| target.section) };
        drag.target = target;
        let verb = from.and_then(|from| resolve_drop_verb(from, target));
        if let Some(preview) = drag.preview.clone() {
            preview.update(cx, |preview, cx| {
                if preview.verb != verb {
                    preview.verb = verb;
                    cx.notify();
                }
            });
        }
        if target == Some(Section::Settled) && previous_target != Some(Section::Settled)
            && let Some(idx) = self.settled_placeholder_row_index()
        {
            self.list.scroll_to_reveal_item(idx);
        }
        cx.notify();
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn set_drag_position_for_test(&mut self, row: usize, after: bool, cx: &mut Context<Self>) {
        self.update_thread_drag_position(row, after, cx);
    }

    /// Tracks the gap under the pointer; listeners on every rendered row see each move.
    pub(super) fn drag_moved(&mut self, row: usize, event: &DragMoveEvent<DraggedThread>, cx: &mut Context<Self>) {
        if !event.drag(cx).is_activated() || !event.bounds.contains(&event.event.position) {
            return;
        }
        let after = event.event.position.y > event.bounds.center().y;
        self.update_thread_drag_position(row, after, cx);
    }

    pub(super) fn finish_drag(&mut self, drop: bool, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        let scoped = self.state.read(cx).sidebar.fleet_drag.is_some();
        let slot = drag.target.and(drag.at).and_then(|(row, after)| if scoped { Some(row + usize::from(after)) } else { self.state.read(cx).sidebar.model.drop_slot(row, after) });
        self.state.update(cx, |state, cx| match slot {
            Some(slot) if drop && scoped => state.drop_scoped_thread(slot, cx),
            Some(slot) if drop => state.drop_thread(slot, cx),
            _ => state.end_thread_drag(cx),
        });
        cx.notify();
    }

    /// The insertion line for row `index`: above it, or below it when the pointer is past its
    /// midpoint.
    pub(super) fn drop_line(&self, index: usize, cx: &App) -> Option<AnyElement> {
        let drag = self.drag.as_ref()?;
        drag.target?;
        let after = drag.at.filter(|(row, _)| *row == index).map(|(_, after)| after)?;
        let p = palette(cx);
        Some(div().absolute().left(px(12.)).right(px(12.)).h(px(2.)).rounded(px(1.)).bg(hsla(p.primary)).when(after, |this| this.bottom(px(-1.))).when(!after, |this| this.top(px(-1.))).into_any_element())
    }

    pub(super) fn drop_target(&self) -> Option<Section> {
        self.drag.as_ref().and_then(|drag| drag.target)
    }

    /// Drop markers: the Pinned and Active boundary labels and the empty-section hints.
    pub(super) fn render_marker(&self, marker: Marker, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let (label, section) = match marker {
            Marker::PinnedHeader => (crate::i18n::t(cx, "Pinned"), Section::Pinned),
            Marker::PinnedDivider | Marker::ActivePlaceholder => (crate::i18n::t(cx, "Active"), Section::Active),
            _ => (crate::i18n::t(cx, "Settled"), Section::Settled),
        };
        let is_target = self.drop_target() == Some(section);
        match marker {
            Marker::ActivePlaceholder | Marker::SettledPlaceholder => {
                let marker_id = match marker {
                    Marker::ActivePlaceholder => "sidebar-drag-marker-active-placeholder",
                    _ => "sidebar-drag-marker-settled-placeholder",
                };
                div()
                    .id(marker_id)
                    .debug_selector(move || marker_id.to_owned())
                    .px(px(10.))
                .pb(px(1.))
                .child(
                    div()
                        .h(px(PLACEHOLDER_HEIGHT))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .border_1()
                        .border_dashed()
                        .text_xs()
                        .when(is_target, |this| this.border_color(hsla(p.primary).opacity(0.4)).bg(hsla(p.primary).opacity(0.05)).text_color(hsla(p.primary)))
                        .when(!is_target, |this| this.border_color(hsla(p.sidebar_foreground).opacity(0.25)).text_color(hsla(p.sidebar_foreground).opacity(0.8)))
                        .child(label),
                )
                .into_any_element()
            }
            _ => {
                let marker_id = match marker {
                    Marker::PinnedHeader => "sidebar-drag-marker-pinned-header",
                    Marker::PinnedDivider => "sidebar-drag-marker-pinned-divider",
                    Marker::WorkingHeader => "sidebar-drag-marker-working-header",
                    Marker::SnoozedHeader => "sidebar-drag-marker-snoozed-header",
                    _ => "sidebar-drag-marker-settled-header",
                };
                div()
                    .id(marker_id)
                    .debug_selector(move || marker_id.to_owned())
                    .h(px(BOUNDARY_HEIGHT))
                .px(px(18.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(if is_target { hsla(p.primary) } else { hsla(p.sidebar_foreground).opacity(0.8) }).child(label))
                    .child(div().flex_1().h(px(1.)).bg(if is_target { hsla(p.primary).opacity(0.5) } else { hsla(p.sidebar_foreground).opacity(0.25) }))
                    .into_any_element()
            }
        }
    }
}

/// The drag payload when manually reordering project folders in the sidebar.
pub struct DraggedProject {
    pub group_key: String,
    pub activated: Arc<AtomicBool>,
}

impl DraggedProject {
    pub(super) fn new(group_key: String) -> Self {
        Self { group_key, activated: Arc::new(AtomicBool::new(false)) }
    }

    pub(super) fn is_activated(&self) -> bool {
        self.activated.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
pub(super) struct PaneProjectDrag {
    pub group_key: String,
    pub at: Option<(String, bool)>,
    #[allow(dead_code)]
    pub preview: Option<Entity<ProjectDragPreview>>,
    pub pointer_drag: bool,
}

pub(super) struct PendingProjectPress {
    pub group: t3_core::sidebar::grouping::ScopedProjectGroup,
    pub expanded: bool,
    pub down_pos: Point<Pixels>,
    pub bounds: super::TriggerBounds,
    pub left_held: bool,
    pub drag_attached: bool,
    pub activated: Arc<AtomicBool>,
    pub preview: Option<Entity<ProjectDragPreview>>,
}

pub struct ProjectDragPreview {
    label: SharedString,
    visual: crate::project_icon::ProjectVisual,
    visible: bool,
}

impl ProjectDragPreview {
    pub fn new(label: String, visual: crate::project_icon::ProjectVisual) -> Self {
        ProjectDragPreview { label: label.into(), visual, visible: true }
    }

    pub fn new_hidden(label: String, visual: crate::project_icon::ProjectVisual) -> Self {
        ProjectDragPreview { label: label.into(), visual, visible: false }
    }

    pub(super) fn show(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            self.visible = true;
            cx.notify();
        }
    }
}

impl Render for ProjectDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().into_any_element();
        }
        let p = palette(cx);
        div()
            .w(px(PREVIEW_WIDTH))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(6.))
            .bg(hsla(p.sidebar))
            .shadow_lg()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(self.visual.render(crate::project_icon::size(16., cx), hsla(p.sidebar_icon), cx))
            .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(self.label.clone()))
            .into_any_element()
    }
}

impl SidebarPane {
    pub(super) fn begin_project_drag_with_preview(
        &mut self,
        group_key: String,
        preview: Entity<ProjectDragPreview>,
        cx: &mut Context<Self>,
    ) {
        self.hover.dismiss(cx);
        self.project_drag = Some(PaneProjectDrag { group_key, at: None, preview: Some(preview), pointer_drag: false });
        cx.notify();
    }

    pub(super) fn begin_pointer_project_drag_with_preview(
        &mut self,
        group_key: String,
        preview: Entity<ProjectDragPreview>,
        cx: &mut Context<Self>,
    ) {
        self.begin_project_drag_with_preview(group_key, preview, cx);
        if let Some(drag) = self.project_drag.as_mut() {
            drag.pointer_drag = true;
        }
    }

    pub(super) fn attach_project_drag(
        &mut self,
        dragged: &DraggedProject,
        label: String,
        visual: crate::project_icon::ProjectVisual,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ProjectDragPreview> {
        let Some(press) = self
            .pending_project_press
            .as_mut()
            .filter(|press| press.group.key == dragged.group_key && press.left_held)
        else {
            window.defer(cx, |window, cx| {
                cx.stop_active_drag(window);
            });
            return cx.new(|_| ProjectDragPreview::new_hidden(label, visual));
        };
        let down_pos = press.down_pos;
        press.drag_attached = true;
        press.activated = dragged.activated.clone();
        let crossed = pointer_distance(window.mouse_position(), down_pos) > SIDEBAR_DRAG_DISTANCE;
        let preview = cx.new(|_| {
            if crossed {
                ProjectDragPreview::new(label, visual)
            } else {
                ProjectDragPreview::new_hidden(label, visual)
            }
        });
        if let Some(press) = self.pending_project_press.as_mut() {
            press.preview = Some(preview.clone());
        }
        if crossed {
            dragged.activated.store(true, Ordering::Relaxed);
            self.begin_pointer_project_drag_with_preview(dragged.group_key.clone(), preview.clone(), cx);
        }
        preview
    }

    pub(super) fn activate_project_drag_if_ready(
        &mut self,
        event: &DragMoveEvent<DraggedProject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.event.pressed_button != Some(MouseButton::Left) {
            self.pending_project_press = None;
            cx.stop_active_drag(window);
            self.finish_project_drag(false, cx);
            return;
        }
        if event.drag(cx).is_activated() {
            return;
        }
        let Some(press) = self.pending_project_press.as_mut() else {
            cx.stop_active_drag(window);
            self.finish_project_drag(false, cx);
            return;
        };
        if pointer_distance(event.event.position, press.down_pos) <= SIDEBAR_DRAG_DISTANCE {
            return;
        }
        press.activated.store(true, Ordering::Relaxed);
        event.drag(cx).activated.store(true, Ordering::Relaxed);
        let group_key = press.group.key.clone();
        let preview = press.preview.clone();
        if let Some(preview) = preview.as_ref() {
            preview.update(cx, |preview, cx| preview.show(cx));
        }
        if let Some(preview) = preview {
            self.begin_pointer_project_drag_with_preview(group_key, preview, cx);
        }
    }

    pub(super) fn finish_project_drop(&mut self, drop: bool, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self.pending_project_press.take();
        if let Some(press) = pending
            && !press.activated.load(Ordering::Relaxed)
        {
            self.finish_project_drag(false, cx);
            if drop {
                let pos = window.mouse_position();
                if pointer_distance(pos, press.down_pos) <= SIDEBAR_DRAG_DISTANCE
                    && press.bounds.get().is_none_or(|b| b.contains(&pos))
                {
                    self.toggle_project_folder(&press.group, !press.expanded, cx);
                }
            }
            return;
        }
        self.finish_project_drag(drop, cx);
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn begin_project_drag(
        &mut self,
        group_key: String,
        label: String,
        visual: crate::project_icon::ProjectVisual,
        cx: &mut Context<Self>,
    ) -> Entity<ProjectDragPreview> {
        let preview = cx.new(|_| ProjectDragPreview::new(label, visual));
        self.begin_project_drag_with_preview(group_key, preview.clone(), cx);
        preview
    }

    pub(super) fn project_drag_moved(&mut self, target_key: String, event: &DragMoveEvent<DraggedProject>, cx: &mut Context<Self>) {
        if !event.drag(cx).is_activated() || !event.bounds.contains(&event.event.position) {
            return;
        }
        let after = event.event.position.y > event.bounds.center().y;
        let Some(drag) = self.project_drag.as_ref() else { return };
        if drag.group_key != event.drag(cx).group_key {
            return;
        }
        let next = {
            if crate::settings::store::client(cx).project_sort_order() != t3_core::client_settings::ProjectSortOrder::Manual {
                None
            } else {
                let state = self.state.read(cx);
                let groups = state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx);
                t3_core::sidebar::grouping::resolve_project_drop_target(&groups, &drag.group_key, &target_key, after)
            }
        };
        let Some(drag) = self.project_drag.as_mut() else { return };
        if drag.at != next {
            drag.at = next;
            cx.notify();
        }
    }

    pub(super) fn finish_project_drag(&mut self, drop: bool, cx: &mut Context<Self>) {
        let Some(drag) = self.project_drag.take() else { return };
        if drop && let Some((target_key, after)) = drag.at {
            self.state.update(cx, |state, cx| {
                state.reorder_scoped_project(&drag.group_key, &target_key, after, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn project_drop_line(&self, group_key: &str, cx: &App) -> Option<AnyElement> {
        let (_, after) = self.project_drag.as_ref()?.at.as_ref().filter(|(target, _)| target == group_key)?;
        let p = palette(cx);
        Some(
            div()
                .absolute()
                .left(px(8.))
                .right(px(8.))
                .h(px(2.))
                .rounded(px(1.))
                .bg(hsla(p.primary))
                .when(*after, |this| this.bottom(px(-1.)))
                .when(!*after, |this| this.top(px(-1.)))
                .into_any_element(),
        )
    }
}

/// The drag payload when pressing and sweeping across Settle / Unsettle / Unsnooze actions.
pub struct DraggedSweep {
    pub id: String,
    pub key: Option<ThreadKey>,
    pub action: SweepAction,
    pub section: Section,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum PaneSweepKey {
    Local(String),
    Scoped(ThreadKey),
}

pub(super) struct PaneSweep {
    pub action: SweepAction,
    pub section: Section,
    pub origin: PaneSweepKey,
    pub keys: Vec<PaneSweepKey>,
}

impl SidebarPane {
    pub(super) fn begin_sweep(&mut self, dragged: &DraggedSweep, cx: &mut Context<Self>) {
        self.hover.dismiss(cx);
        let origin = dragged.key.clone().map_or_else(|| PaneSweepKey::Local(dragged.id.clone()), PaneSweepKey::Scoped);
        self.sweep = Some(PaneSweep {
            action: dragged.action,
            section: dragged.section,
            origin: origin.clone(),
            keys: vec![origin],
        });
        cx.notify();
    }

    pub(super) fn sweep_moved(&mut self, row: usize, event: &DragMoveEvent<DraggedSweep>, cx: &mut Context<Self>) {
        if !event.bounds.contains(&event.event.position) {
            return;
        }
        let Some(sweep) = self.sweep.as_mut() else { return };
        let state = self.state.read(cx);
        let keys = if !state.fleet.environments.is_empty() {
            let Some(t3_core::sidebar::fleet::ScopedSidebarRow::Thread { index, .. }) = state.sidebar.fleet.rows.get(row) else {
                return;
            };
            let target = PaneSweepKey::Scoped(state.sidebar.fleet.threads[*index].key.clone());
            let entries: Vec<_> = state
                .sidebar
                .fleet
                .rows
                .iter()
                .filter_map(|row| match row {
                    t3_core::sidebar::fleet::ScopedSidebarRow::Thread { index, section, .. } => {
                        let thread = &state.sidebar.fleet.threads[*index];
                        let eligible = *section == sweep.section
                            && state.fleet.environment(&thread.key.environment_id).is_some_and(|env| env.config.config.is_some());
                        Some((PaneSweepKey::Scoped(thread.key.clone()), eligible))
                    }
                    _ => None,
                })
                .collect();
            let ordered: Vec<_> = entries.iter().map(|(key, _)| key.clone()).collect();
            sweep_keys(&ordered, &sweep.origin, &target, |candidate| {
                entries.iter().any(|(key, eligible)| *eligible && key == candidate)
            })
        } else {
            let Some(t3_core::sidebar::SidebarRow::Thread { index, .. }) = state.sidebar.model.rows.get(row) else {
                return;
            };
            let target = PaneSweepKey::Local(state.sidebar.model.threads[*index].id.clone());
            let config_loaded = state.server_config.config.is_some();
            let entries: Vec<_> = state
                .sidebar
                .model
                .rows
                .iter()
                .filter_map(|row| match row {
                    t3_core::sidebar::SidebarRow::Thread { index, section } => {
                        let id = state.sidebar.model.threads[*index].id.clone();
                        Some((PaneSweepKey::Local(id), config_loaded && *section == sweep.section))
                    }
                    _ => None,
                })
                .collect();
            let ordered: Vec<_> = entries.iter().map(|(key, _)| key.clone()).collect();
            sweep_keys(&ordered, &sweep.origin, &target, |candidate| {
                entries.iter().any(|(key, eligible)| *eligible && key == candidate)
            })
        };
        if sweep.keys != keys {
            sweep.keys = keys;
            cx.notify();
        }
    }

    pub(super) fn finish_sweep(&mut self, drop: bool, cx: &mut Context<Self>) {
        let Some(sweep) = self.sweep.take() else { return };
        if drop {
            self.state.update(cx, |state, cx| {
                if !state.fleet.environments.is_empty() {
                    let keys: Vec<_> = sweep
                        .keys
                        .iter()
                        .filter_map(|key| match key {
                            PaneSweepKey::Scoped(key) => Some(key.clone()),
                            PaneSweepKey::Local(_) => None,
                        })
                        .filter(|key| {
                            state.sidebar.fleet.section_of(key) == Some(sweep.section)
                                && state.fleet.environment(&key.environment_id).is_some_and(|env| env.config.config.is_some())
                        })
                        .collect();
                    match sweep.action {
                        SweepAction::Settle => {
                            let owners: Vec<_> = keys.iter().filter_map(|key| crate::context_menu::ThreadMenuOwner::capture_scoped(state, key)).collect();
                            state.run_scoped_bulk(owners, BulkAction::Settle, None, false, cx).detach();
                        }
                        SweepAction::Unsettle => {
                            for key in &keys {
                                state.run_scoped_thread_action(key, ThreadAction::Unsettle, &[], cx);
                            }
                        }
                        SweepAction::Unsnooze => {
                            for key in &keys {
                                state.run_scoped_thread_action(key, ThreadAction::Unsnooze, &[], cx);
                            }
                        }
                    }
                } else if state.server_config.config.is_some() {
                    let ids: Vec<_> = sweep
                        .keys
                        .iter()
                        .filter_map(|key| match key {
                            PaneSweepKey::Local(id) => Some(id.clone()),
                            PaneSweepKey::Scoped(_) => None,
                        })
                        .filter(|id| state.sidebar.model.section_of(id) == Some(sweep.section))
                        .collect();
                    for id in ids {
                        match sweep.action {
                            SweepAction::Settle => state.settle_thread(&id, cx),
                            SweepAction::Unsettle => state.unsettle_thread(&id, cx),
                            SweepAction::Unsnooze => state.unsnooze_thread(&id, cx),
                        }
                    }
                }
            });
        }
        cx.notify();
    }

    pub(super) fn sweep_action_for(&self, key: Option<&ThreadKey>, id: &str) -> Option<SweepAction> {
        let sweep = self.sweep.as_ref()?;
        let target = key.cloned().map_or_else(|| PaneSweepKey::Local(id.to_owned()), PaneSweepKey::Scoped);
        sweep.keys.contains(&target).then_some(sweep.action)
    }
}
