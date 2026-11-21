//! Project folders in the default-off boring sidebar.

use gpui::{AnyElement, Context, ElementId, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, div, prelude::*};
use t3_core::sidebar::fleet::FolderDot;
use super::SidebarPane;
use crate::i18n::{t, t_fmt};
use crate::icons::Icon;
use crate::theme::{hsla, metrics, palette, rem};

impl SidebarPane {
    pub(super) fn render_project_folder(&self, key: &str, expanded: bool, hidden_count: usize, dot: Option<FolderDot>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let model = &state.sidebar.fleet;
        let Some(group) = model.folder_groups.get(key) else { return div().into_any_element() };
        let first = model.rows.iter().find_map(|row| match row { t3_core::sidebar::fleet::ScopedSidebarRow::Folder { key, .. } => Some(key.as_str()), _ => None }) == Some(key);
        let visual = crate::project_icon::ProjectVisual::for_scoped_project(state, &group.representative);
        let p = palette(cx);
        let environment_badge = t3_core::sidebar::grouping::resolve_project_environment_badge(
            group,
            crate::new_thread::primary_environment_id(state),
        ).map(|badge| {
            let icon = crate::context_chip::environment_machine_icon(state, &badge.environment_id);
            let id = SharedString::from(format!("project-environment-{key}"));
            div().id(id)
                .flex_none()
                .child(icon.render(crate::project_icon::size(12., cx), hsla(p.sidebar_icon)))
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(badge.description.clone()).build(window, cx))
        });
        let toggle = group.clone();
        let new_thread = group.clone();
        let context_group = group.clone();
        let keyboard_group = group.clone();
        let is_scratch = group.is_scratch;
        let drag_key = group.key.clone();
        let move_key = group.key.clone();
        let label = if is_scratch { t(cx, "No project").to_owned() } else { group.label.clone() };
        let id = ElementId::Name(SharedString::from(format!("project-folder-{key}")));
        let new_id = ElementId::Name(SharedString::from(format!("folder-new-{key}")));
        let focus = self.tree.focus(&super::tree::TreeRow::Folder(key.to_owned()))
            .unwrap_or_else(|| crate::settings::controls::focus_handle(&id, window, cx));
        let keyboard_focus = focus.clone();
        let new_focus = crate::settings::controls::focus_handle(&new_id, window, cx);
        let (bounds, measure) = super::measure_trigger();
        let folder_group = SharedString::from(format!("project-folder-group-{key}"));
        let new_thread_aria = t_fmt(cx, "New thread in {project}", &[("project", label.as_str().into())]);
        let new_thread_tooltip = new_thread_aria.clone();
        let color = dot.map(|dot| match dot { FolderDot::Attention => p.warning, FolderDot::Failed => p.error, FolderDot::Unread => p.success, FolderDot::Working => p.info });
        let manual_sort = crate::settings::store::client(cx).project_sort_order() == t3_core::client_settings::ProjectSortOrder::Manual;
        let manual_reorder = manual_sort && !is_scratch;
        let dragging_this = self.project_drag.as_ref().is_some_and(|drag| drag.group_key == drag_key);
        let drop_line = self.project_drop_line(&drag_key, cx);
        let drag_label = label.clone();
        let drag_visual = visual.clone();
        div()
            .px(gpui::px(8.))
            .pb(gpui::px(1.))
            .child(
                div().id(id).group(folder_group.clone()).relative().track_focus(&focus)
                    .role(gpui::Role::Button).aria_label(label.clone()).aria_expanded(expanded)
                    .focus_visible(|style| style.shadow(vec![gpui::BoxShadow { color:hsla(p.ring), offset:gpui::point(gpui::px(0.),gpui::px(0.)), blur_radius:gpui::px(0.), spread_radius:gpui::px(2.), inset:false }]))
                    .mt(rem(if first { 0. } else { 8. })).h(rem(32.)).px(rem(metrics::SIDEBAR_ROW_CONTENT_INSET)).flex().items_center().gap(rem(10.)).rounded(rem(metrics::CONTROL_RADIUS)).cursor_pointer()
                    .when(dragging_this, |row| row.opacity(0.4))
                    .hover(|style| style.bg(hsla(p.sidebar_row_hover)))
                    .when(manual_reorder, |row| {
                        let pane = cx.entity().downgrade();
                        let drag_key = drag_key.clone();
                        let drag_label = drag_label.clone();
                        let drag_visual = drag_visual.clone();
                        row.on_drag(super::drag::DraggedProject { group_key: drag_key.clone() }, move |_, _, _, cx| {
                            let preview = cx.new(|_| super::drag::ProjectDragPreview::new(drag_label.clone(), drag_visual.clone()));
                            let _ = pane.update(cx, |this, cx| {
                                this.begin_project_drag_with_preview(drag_key.clone(), preview.clone(), cx);
                            });
                            preview
                        })
                    })
                    .when(manual_sort, |row| {
                        let move_key = move_key.clone();
                        row.drag_over::<super::drag::DraggedProject>(|style, _, _, _| style)
                            .on_drag_move(cx.listener(move |this, event: &gpui::DragMoveEvent<super::drag::DraggedProject>, _, cx| {
                                this.project_drag_moved(move_key.clone(), event, cx);
                            }))
                            .on_drop(cx.listener(|this, _: &super::drag::DraggedProject, _, cx| {
                                this.finish_project_drag(true, cx);
                            }))
                    })
                    .children(drop_line)
                    .child(visual.render(crate::project_icon::size(16., cx), hsla(p.sidebar_icon), cx))
                    .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.sidebar_foreground).opacity(0.9)).child(label))
                    .children(environment_badge)
                    .children(color.map(|color| div().size(rem(6.)).rounded_full().bg(hsla(color))))
                    .when(!expanded && hidden_count > 0, |row| row.child(div().text_xs().text_color(hsla(p.sidebar_muted_foreground)).group_hover(folder_group.clone(), |style| style.invisible()).child(hidden_count.to_string())))
                    .child(div().id(new_id).track_focus(&new_focus).role(gpui::Role::Button).aria_label(new_thread_aria)
                        .absolute().top_0().bottom_0().right(rem(24.)).my_auto().size(rem(24.)).flex().items_center().justify_center().rounded(rem(6.))
                        .opacity(0.).group_hover(folder_group, |style| style.opacity(1.))
                        .hover(|style| style.bg(hsla(p.sidebar_row_hover)))
                        .focus_visible(|style| style.opacity(1.).shadow(vec![gpui::BoxShadow { color:hsla(p.ring), offset:gpui::point(gpui::px(0.),gpui::px(0.)), blur_radius:gpui::px(0.), spread_radius:gpui::px(2.), inset:false }]))
                        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(new_thread_tooltip.clone()).build(window, cx))
                        .child(Icon::SquarePen.render(crate::project_icon::size(14., cx), hsla(p.sidebar_muted_foreground)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.state.update(cx, |state, cx| state.run_scoped_project_action(&new_thread, t3_core::thread_actions::ProjectAction::NewThread, cx));
                        })))
                    .child(if expanded { Icon::ChevronDown } else { Icon::ChevronRight }.render(crate::project_icon::size(12., cx), hsla(p.sidebar_muted_foreground)))
                    .child(measure)
                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                        if keyboard_focus.is_focused(window) && super::tree::context_menu_key(&event.keystroke) {
                            window.prevent_default();
                            cx.stop_propagation();
                            if !event.is_held && let Some(bounds) = bounds.get() {
                                this.menu.update(cx, |menu, cx| menu.open_scoped_project(keyboard_group.clone(), bounds.bottom_left(), window, cx));
                            }
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_project_folder(&toggle, !expanded, cx);
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        window.focus(&focus, cx);
                        this.menu.update(cx, |menu, cx| menu.open_scoped_project(context_group.clone(), event.position, window, cx));
                    })),
            )
            .into_any_element()
    }
}
