//! Dragging rows and the thread hover card (design-system §7.2, `sidebar/SidebarThreadRow.tsx`,
//! `sidebar/SidebarListMarkers.tsx`, `ThreadHoverCard.tsx`). The dragged row stays in place,
//! dimmed; a raised copy follows the pointer with the drop badge, and a line marks the gap the
//! row will land in. Drop resolution and planning live in `t3_core::sidebar::drag`.

use gpui::{AnyElement, App, Context, DragMoveEvent, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, prelude::*, px};
use t3_core::sidebar::Section;
use t3_core::sidebar::drag::{DropVerb, Marker, resolve_drop_verb};

use super::SidebarPane;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const PREVIEW_WIDTH: f32 = 240.;
const BOUNDARY_HEIGHT: f32 = 24.;
const PLACEHOLDER_HEIGHT: f32 = 36.;

/// The drag payload of a sidebar thread row.
pub struct DraggedThread {
    pub id: String,
    pub key: Option<t3_core::sidebar::tag_filter::ThreadKey>,
}

/// The pointer's gap while dragging: the row it is over, below its midpoint when `after`.
#[derive(Default)]
pub(super) struct PaneDrag {
    pub at: Option<(usize, bool)>,
    /// The section a drop at `at` lands in, when it would change anything.
    pub target: Option<Section>,
    pub preview: Option<Entity<ThreadDragPreview>>,
}

pub struct ThreadDragPreview {
    title: SharedString,
    project: Option<SharedString>,
    verb: Option<DropVerb>,
    visual: crate::project_icon::ProjectVisual,
}

impl ThreadDragPreview {
    pub fn new(title: String, project: Option<String>, visual: crate::project_icon::ProjectVisual) -> Self {
        ThreadDragPreview { title: title.into(), project: project.map(Into::into), verb: None, visual }
    }
}

fn verb_icon(verb: DropVerb) -> Icon {
    match verb {
        DropVerb::Pin => Icon::Pin,
        DropVerb::Unpin => Icon::PinOff,
        DropVerb::Settle => Icon::CircleCheck,
        DropVerb::Unsettle => Icon::Undo2,
        DropVerb::Wake => Icon::AlarmClockOff,
    }
}

impl Render for ThreadDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let badge = self.verb.map(|verb| {
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
        });
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
    }
}

impl SidebarPane {
    /// The drag constructor ran: the list shows its drop markers.
    pub(super) fn begin_drag(&mut self, thread_id: &str, key: Option<&t3_core::sidebar::tag_filter::ThreadKey>, preview: Entity<ThreadDragPreview>, cx: &mut Context<Self>) {
        self.drag = Some(PaneDrag { preview: Some(preview), ..Default::default() });
        self.state.update(cx, |state, cx| match key { Some(key) => state.start_scoped_thread_drag(key, cx), None => state.start_thread_drag(thread_id, cx) });
    }

    /// Tracks the gap under the pointer; listeners on every rendered row see each move.
    pub(super) fn drag_moved(&mut self, row: usize, event: &DragMoveEvent<DraggedThread>, cx: &mut Context<Self>) {
        if !event.bounds.contains(&event.event.position) {
            return;
        }
        let after = event.event.position.y > event.bounds.center().y;
        let Some(drag) = self.drag.as_mut().filter(|drag| drag.at != Some((row, after))) else { return };
        drag.at = Some((row, after));
        let state = self.state.read(cx);
        let from = state.sidebar.fleet_drag.as_ref().map(|(_, from)| *from).or_else(|| state.sidebar.drag.as_ref().map(|(_, from)| *from));
        let target = if state.sidebar.fleet_drag.is_some() { state.scoped_thread_drop_target(row + usize::from(after)).map(|(target, _)| target.section) }
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
        cx.notify();
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
            Marker::PinnedHeader => ("Pinned", Section::Pinned),
            Marker::PinnedDivider | Marker::ActivePlaceholder => ("Active", Section::Active),
            _ => ("Settled", Section::Settled),
        };
        let is_target = self.drop_target() == Some(section);
        match marker {
            Marker::ActivePlaceholder | Marker::SettledPlaceholder => div()
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
                .into_any_element(),
            _ => div()
                .h(px(BOUNDARY_HEIGHT))
                .px(px(18.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(if is_target { hsla(p.primary) } else { hsla(p.sidebar_foreground).opacity(0.8) }).child(label))
                .child(div().flex_1().h(px(1.)).bg(if is_target { hsla(p.primary).opacity(0.5) } else { hsla(p.sidebar_foreground).opacity(0.25) }))
                .into_any_element(),
        }
    }
}

/// `ThreadHoverCard` in the glass tooltip: the title, then project, branch, model and any error.
pub struct ThreadHoverCard {
    pub title: SharedString,
    pub project: Option<SharedString>,
    pub favicon: crate::project_icon::ProjectVisual,
    pub branch: Option<SharedString>,
    pub model: Option<SharedString>,
    /// The error line and whether it is a usage limit.
    pub error: Option<bool>,
}

impl Render for ThreadHoverCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let line = |icon: Icon, text: SharedString| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_w_0()
                .child(icon.sized(px(12.)).flex_none().text_color(hsla(p.muted_foreground)))
                .child(div().min_w_0().truncate().text_color(hsla(p.foreground).opacity(0.75)).child(text))
        };
        let error = self.error.map(|usage_limit| {
            let color = if usage_limit { hsla(p.warning) } else { hsla(p.error) };
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_color(color)
                .child(Icon::CircleAlert.sized(px(12.)).flex_none())
                .child(div().truncate().child(if usage_limit { "Usage limit reached" } else { "Error occurred" }))
        });
        div()
            .relative()
            .max_w(px(320.))
            .rounded(px(6.))
            .shadow_lg()
            .child(crate::glass::backdrop(6., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
            .child(
                div()
                    .relative()
                    .px(px(12.))
                    .py(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().min_w_0().truncate().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(self.title.clone()))
                    .child(
                        div()
                            .pl(px(2.))
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .children(self.project.clone().map(|project| {
                                let icon = self.favicon.render(crate::project_icon::size(12., cx), hsla(p.icon_muted), cx);
                                div().flex().items_center().gap(px(8.)).min_w_0().child(icon).child(div().min_w_0().truncate().text_color(hsla(p.foreground).opacity(0.75)).child(project))
                            }))
                            .children(self.branch.clone().map(|branch| line(Icon::GitBranch, branch)))
                            .children(self.model.clone().map(|model| line(Icon::Bot, model)))
                            .children(error),
                    ),
            )
    }
}
