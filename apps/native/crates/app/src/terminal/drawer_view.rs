//! The drawer's look, from `ThreadTerminalDrawer.tsx` (drawer mode): a 6px resize strip on the
//! top edge, the active group's panes side by side or stacked, and either a floating action
//! group (one terminal) or the 144px session list (several). Metrics are design-system §5.
//! Without `terminal:operate` every action is disabled and rows lose their close button.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, DragMoveEvent, ElementId, EmptyView, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, ease_out_quint, prelude::*, px,
};
use gpui_component::button::Button;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable, Sizable, h_flex, v_flex};
use t3_core::terminal::{self, MAX_TERMINALS_PER_GROUP, SplitDirection, TerminalGroup, ThreadTerminalLayout};

use super::drawer::{DrawerAnimation, TerminalDrawer};
use super::with_shortcut;
use crate::icons::Icon;
use super::{CloseTerminal, NewTerminal, SplitTerminal, SplitTerminalVertical};
use crate::theme::{
    ThemeSlot, ThemeSlotExt as _, ThemeSlotInteractiveExt as _, hsla, palette,
    theme_slot_background_layer,
};

const RESIZE_STRIP: f32 = 6.;
const SIDEBAR_WIDTH: f32 = 144.;
const SIDEBAR_HEADER: f32 = 22.;
const ROW_HEIGHT: f32 = 24.;
const ACTION_ICON: f32 = 13.;

struct ResizeDrawer;

#[derive(Clone, Copy)]
enum Action {
    SplitHorizontal,
    SplitVertical,
    New,
    Close,
}

impl Action {
    const ALL: [Action; 4] = [Action::SplitHorizontal, Action::SplitVertical, Action::New, Action::Close];

    fn icon(self) -> Icon {
        match self {
            Action::SplitHorizontal => Icon::SquareSplitHorizontal,
            Action::SplitVertical => Icon::SquareSplitVertical,
            Action::New => Icon::Plus,
            Action::Close => Icon::Trash2,
        }
    }

    fn command(self) -> &'static str {
        match self {
            Action::SplitHorizontal => "terminal.split",
            Action::SplitVertical => "terminal.splitVertical",
            Action::New => "terminal.new",
            Action::Close => "terminal.close",
        }
    }

    fn label(self, at_limit: bool, cx: &App) -> String {
        let name = crate::i18n::t(cx, match self {
            Action::SplitHorizontal => "Split Terminal Horizontally",
            Action::SplitVertical => "Split Terminal Vertically",
            Action::New => "New Terminal",
            Action::Close => "Close Terminal",
        });
        if self.at_limit(at_limit) {
            return format!("{name} (max {MAX_TERMINALS_PER_GROUP} per group)");
        }
        with_shortcut(name, self.command(), cx)
    }

    fn at_limit(self, at_limit: bool) -> bool {
        at_limit && matches!(self, Action::SplitHorizontal | Action::SplitVertical)
    }
}

impl TerminalDrawer {
    fn run(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::SplitHorizontal => self.split(SplitDirection::Horizontal, window, cx),
            Action::SplitVertical => self.split(SplitDirection::Vertical, window, cx),
            Action::New => self.new_terminal(window, cx),
            Action::Close => self.close_active(window, cx),
        }
    }

    fn action_button(&self, action: Action, at_limit: bool, hover: Hsla, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let label = SharedString::from(action.label(at_limit, cx));
        let disabled = !self.can_operate() || action.at_limit(at_limit);
        div()
            .id(SharedString::from(format!("terminal-action-{}", action as u8)))
            .h_full()
            .flex()
            .items_center()
            .px_1()
            .when(disabled, |this| this.opacity(0.64))
            .when(!disabled, |this| this.cursor_pointer().hover(move |style| style.bg(hover)))
            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            .on_click(cx.listener(move |drawer, _, window, cx| {
                if !disabled {
                    drawer.run(action, window, cx);
                }
            }))
            .child(action.icon().render(px(ACTION_ICON), hsla(p.foreground).opacity(0.9)))
    }

    fn render_floating_actions(&self, at_limit: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let hover = hsla(p.accent);
        let mut group = h_flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(6.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.8))
            .bg(hsla(p.background))
            .shadow_xs();
        for (index, action) in Action::ALL.into_iter().enumerate() {
            if index > 0 {
                group = group.child(div().h(px(16.)).w(px(1.)).bg(hsla(p.border).opacity(0.8)));
            }
            group = group.child(div().py_1().child(self.action_button(action, at_limit, hover, cx)));
        }
        div().absolute().top_2().right_2().child(group)
    }

    fn render_sidebar(&self, thread_id: &str, layout: &ThreadTerminalLayout, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let line = hsla(p.border).opacity(0.7);
        let at_limit = layout.has_reached_split_limit();
        let mut header = h_flex().h_full().items_stretch();
        for (index, action) in Action::ALL.into_iter().enumerate() {
            header = header.child(
                div()
                    .h_full()
                    .when(index > 0, |this| this.border_l_1().border_color(line))
                    .child(self.action_button(action, at_limit, hsla(p.accent).opacity(0.7), cx)),
            );
        }
        let show_headers = layout.shows_group_headers();
        let mut list = v_flex().id("terminal-session-list").flex_1().min_h_0().overflow_y_scroll().px_1().py_1();
        for group in &layout.terminal_groups {
            list = list.child(self.render_group(thread_id, group, layout, show_headers, cx));
        }
        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .min_w(px(SIDEBAR_WIDTH))
            .h_full()
            .border_1()
            .border_color(line)
            .bg(hsla(p.muted).opacity(0.1))
            .child(h_flex().h(px(SIDEBAR_HEADER)).flex_none().justify_end().border_b_1().border_color(line).child(header))
            .child(list)
    }

    fn render_group(
        &self,
        thread_id: &str,
        group: &TerminalGroup,
        layout: &ThreadTerminalLayout,
        show_header: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let p = palette(cx);
        let active_in_group = group.terminal_ids.contains(&layout.active_terminal_id);
        let group_target = if active_in_group { layout.active_terminal_id.clone() } else { group.terminal_ids[0].clone() };
        let split = group.terminal_ids.len() > 1;
        let (group_icon, group_label) = match (split, group.split_direction) {
            (false, _) => (Icon::Square, "Single"),
            (true, SplitDirection::Vertical) => (Icon::SquareSplitVertical, "Stacked"),
            (true, SplitDirection::Horizontal) => (Icon::SquareSplitHorizontal, "Side by side"),
        };
        let header_color = if active_in_group { hsla(p.foreground) } else { hsla(p.muted_foreground) };
        let header = div()
            .id(SharedString::from(format!("terminal-group-{}", group.id)))
            .h(px(22.))
            .w_full()
            .flex()
            .items_center()
            .gap_1()
            .rounded(px(4.))
            .px(px(6.))
            .text_size(px(11.))
            .text_color(header_color)
            .cursor_pointer()
            .when(active_in_group, |this| this.bg(hsla(p.accent).opacity(0.5)))
            .when(!active_in_group, |this| {
                this.hover(|style| style.bg(hsla(p.accent).opacity(0.4)).text_color(hsla(p.foreground)))
            })
            .on_click(cx.listener(move |drawer, _, window, cx| drawer.activate(&group_target, window, cx)))
            .child(group_icon.render(px(12.), header_color))
            .child(div().flex_1().min_w_0().truncate().child(group_label))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(hsla(p.muted_foreground).opacity(0.7))
                    .child(group.terminal_ids.len().to_string()),
            );
        let mut rows = v_flex().gap(px(2.));
        for terminal_id in &group.terminal_ids {
            rows = rows.child(self.render_row(thread_id, terminal_id, terminal_id == &layout.active_terminal_id, cx));
        }
        v_flex().pb(px(2.)).when(show_header, |this| this.child(header)).child(rows)
    }

    fn render_row(&self, thread_id: &str, terminal_id: &str, active: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let label = self.label(thread_id, terminal_id);
        let close_label = SharedString::from(if active { with_shortcut(&format!("Close {label}"), "terminal.close", cx) } else { format!("Close {label}") });
        let text = if active { hsla(p.foreground) } else { hsla(p.muted_foreground) };
        let group_name = SharedString::from(format!("terminal-row-{terminal_id}"));
        let (close_target, activate_target) = (terminal_id.to_owned(), terminal_id.to_owned());
        let icon = div().size(px(16.)).flex_none().flex().items_center().justify_center();
        let close = if !self.can_operate() {
            icon.child(Icon::SquareTerminal.render(px(12.), text)).into_any_element()
        } else {
            icon
                .id(SharedString::from(format!("terminal-close-{terminal_id}")))
                .relative()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|style| style.bg(hsla(p.muted)))
                .tooltip(move |window, cx| Tooltip::new(close_label.clone()).build(window, cx))
                .on_click(cx.listener(move |drawer, _, window, cx| drawer.request_close(&close_target, window, cx)))
                .child(div().group_hover(group_name.clone(), |style| style.invisible()).child(Icon::SquareTerminal.render(px(12.), text)))
                .child(
                    div()
                        .absolute()
                        .invisible()
                        .group_hover(group_name.clone(), |style| style.visible())
                        .child(Icon::X.render(px(12.), text)),
                )
                .into_any_element()
        };
        div()
            .id(SharedString::from(format!("terminal-tab-{terminal_id}")))
            .group(group_name)
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .gap(px(2.))
            .rounded(px(6.))
            .pr_2()
            .pl(px(6.))
            .text_xs()
            .text_color(text)
            .when(active, |this| this.bg(hsla(p.accent)))
            .when(!active, |this| this.hover(|style| style.bg(hsla(p.accent).opacity(0.6)).text_color(hsla(p.foreground))))
            .child(close)
            .child(
                div()
                    .id(SharedString::from(format!("terminal-label-{terminal_id}")))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .on_click(cx.listener(move |drawer, _, window, cx| drawer.activate(&activate_target, window, cx)))
                    .child(label),
            )
    }

    fn render_panes(&mut self, thread_id: &str, layout: &ThreadTerminalLayout, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let vertical = layout.split_direction() == SplitDirection::Vertical;
        let mut panes = if vertical { v_flex() } else { h_flex() }.size_full().min_w_0().overflow_hidden();
        for (index, terminal_id) in layout.visible_terminal_ids().into_iter().enumerate() {
            let Some(pane) = self.pane(thread_id, &terminal_id, window, cx) else { continue };
            let line = if terminal_id == layout.active_terminal_id { hsla(p.border) } else { hsla(p.border).opacity(0.7) };
            panes = panes.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    // A row centers its children, so the pane takes the row's height explicitly.
                    .h_full()
                    .border_color(line)
                    .when(index > 0 && vertical, |this| this.border_t_1())
                    .when(index > 0 && !vertical, |this| this.border_l_1())
                    .child(pane),
            );
        }
        panes.into_any_element()
    }
}

impl Render for TerminalDrawer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((thread_id, layout)) = self.layout(cx) else {
            self.last_open_state = None;
            self.drawer_animation = None;
            self._close_animation_task = None;
            return div().into_any_element();
        };
        let has_project = self.has_project(cx);
        let is_open = layout.terminal_open && has_project;
        let animation_duration_ms = crate::settings::store::try_client(cx)
            .map_or(t3_core::client_settings::DEFAULT_PANEL_ANIMATION_DURATION_MS, |doc| doc.panel_animation_duration_ms());
        let same_thread_opened = matches!(
            self.last_open_state.as_ref(),
            Some((prev_thread, false)) if prev_thread == &thread_id
        ) && is_open;
        let same_thread_closed = matches!(
            self.last_open_state.as_ref(),
            Some((prev_thread, true)) if prev_thread == &thread_id
        ) && !is_open;
        let thread_switched = self.last_open_state.as_ref().is_some_and(|(prev, _)| prev != &thread_id);
        self.last_open_state = Some((thread_id.clone(), is_open));
        if animation_duration_ms == 0 || thread_switched {
            self.drawer_animation = None;
            self._close_animation_task = None;
        }
        let p = palette(cx);
        if !is_open {
            if same_thread_closed && animation_duration_ms > 0 {
                let from_height = self.last_rendered_height.max(1.);
                self.animation_seq = self.animation_seq.wrapping_add(1);
                let seq = self.animation_seq;
                self.drawer_animation = Some(DrawerAnimation {
                    seq,
                    duration_ms: animation_duration_ms,
                    opening: false,
                    from_height,
                });
                let wait = Duration::from_millis(u64::from(animation_duration_ms) + 16);
                self._close_animation_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    let _ = this.update(cx, |drawer, cx| {
                        if drawer.drawer_animation.is_some_and(|a| !a.opening && a.seq == seq) {
                            drawer.drawer_animation = None;
                            drawer._close_animation_task = None;
                            cx.notify();
                        }
                    });
                }));
            }
            if let Some(anim) = self.drawer_animation.filter(|a| !a.opening) {
                return div()
                    .id("terminal-drawer-closing")
                    .relative()
                    .w_full()
                    .flex_none()
                    .overflow_hidden()
                    .bg(hsla(p.background))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.8))
                    .theme_slot(ThemeSlot::ThreadTerminalDrawer, cx)
                    .children(theme_slot_background_layer(ThemeSlot::ThreadTerminalDrawer, cx))
                    .with_animation(
                        ElementId::Name(format!("terminal-drawer-close-{}", anim.seq).into()),
                        Animation::new(Duration::from_millis(u64::from(anim.duration_ms))).with_easing(ease_out_quint()),
                        move |el, delta| el.h(px(anim.from_height * (1. - delta))),
                    )
                    .into_any_element();
            }
            return div().into_any_element();
        }
        let viewport = f32::from(window.viewport_size().height);
        let height = terminal::clamp_drawer_height(layout.terminal_height, viewport);
        self.last_rendered_height = height;
        if same_thread_opened && animation_duration_ms > 0 {
            self._close_animation_task = None;
            self.animation_seq = self.animation_seq.wrapping_add(1);
            self.drawer_animation = Some(DrawerAnimation {
                seq: self.animation_seq,
                duration_ms: animation_duration_ms,
                opening: true,
                from_height: 0.,
            });
        } else if self.drawer_animation.is_some_and(|a| !a.opening) {
            self.drawer_animation = None;
            self._close_animation_task = None;
        }
        let strip = div()
            .id("terminal-resize")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(RESIZE_STRIP))
            .cursor_row_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |drawer, event: &MouseDownEvent, _, _| drawer.begin_resize(f32::from(event.position.y), height)),
            )
            .on_drag(ResizeDrawer, |_, _, _, cx| cx.new(|_| EmptyView));
        let body = if layout.terminal_ids.is_empty() {
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_3()
                .px_4()
                .py_6()
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child("No terminal sessions for this thread yet.")
                .child(
                    Button::new("terminal-empty-new")
                        .label(with_shortcut("New Terminal", "terminal.new", cx))
                        .outline()
                        .xsmall()
                        .disabled(!self.can_operate())
                        .on_click(cx.listener(|drawer, _, window, cx| drawer.new_terminal(window, cx))),
                )
                .into_any_element()
        } else {
            let sidebar = layout.terminal_ids.len() > 1;
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .bg(hsla(p.terminal_background))
                .when(sidebar, |this| this.gap(px(6.)))
                .child(div().flex_1().min_w_0().h_full().child(self.render_panes(&thread_id, &layout, window, cx)))
                .when(sidebar, |this| this.child(self.render_sidebar(&thread_id, &layout, cx)))
                .into_any_element()
        };
        let floating = (layout.terminal_ids.len() == 1).then(|| self.render_floating_actions(layout.has_reached_split_limit(), cx));
        let opening_anim = self.drawer_animation.filter(|a| a.opening);
        let drawer_el = v_flex()
            .id("terminal-drawer")
            .relative()
            .w_full()
            .h(px(height))
            .when(opening_anim.is_none(), |this| this.min_h(px(terminal::MIN_DRAWER_HEIGHT)))
            .flex_shrink(1.)
            .min_w_0()
            .overflow_hidden()
            .bg(hsla(p.background))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.8))
            .theme_slot_interactive(ThemeSlot::ThreadTerminalDrawer, cx)
            .on_action(cx.listener(|drawer, _: &NewTerminal, window, cx| drawer.new_terminal(window, cx)))
            .on_action(cx.listener(|drawer, _: &SplitTerminal, window, cx| drawer.split(SplitDirection::Horizontal, window, cx)))
            .on_action(cx.listener(|drawer, _: &SplitTerminalVertical, window, cx| drawer.split(SplitDirection::Vertical, window, cx)))
            .on_action(cx.listener(|drawer, _: &CloseTerminal, window, cx| drawer.close_active(window, cx)))
            .on_drag_move(cx.listener(move |drawer, event: &DragMoveEvent<ResizeDrawer>, _, cx| {
                drawer.resize_to(f32::from(event.event.position.y), viewport, cx)
            }))
            .children(theme_slot_background_layer(ThemeSlot::ThreadTerminalDrawer, cx))
            .child(body)
            .children(floating)
            .child(strip);
        match opening_anim {
            Some(anim) => drawer_el
                .with_animation(
                    ElementId::Name(format!("terminal-drawer-open-{}", anim.seq).into()),
                    Animation::new(Duration::from_millis(u64::from(anim.duration_ms))).with_easing(ease_out_quint()),
                    move |el, delta| el.h(px(height * delta)),
                )
                .into_any_element(),
            None => drawer_el.into_any_element(),
        }
    }
}
