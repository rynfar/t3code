//! Project folders in the default-off boring sidebar.

use gpui::{AnyElement, Context, IntoElement, MouseButton, ParentElement, SharedString, Styled, div, prelude::*, px};
use t3_core::sidebar::fleet::FolderDot;
use t3_core::sidebar::grouping::GroupConsumer;
use super::SidebarPane;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

impl SidebarPane {
    pub(super) fn render_project_folder(&self, key: &str, expanded: bool, hidden_count: usize, dot: Option<FolderDot>, cx: &mut Context<Self>) -> AnyElement {
        let groups = self.state.read(cx).project_groups_for(GroupConsumer::Sidebar, cx);
        let Some(group) = groups.into_iter().find(|group| group.key == key) else { return div().into_any_element() };
        let p = palette(cx);
        let toggle = group.clone();
        let new_thread = group.clone();
        let context_group = group.clone();
        let color = dot.map(|dot| match dot { FolderDot::Attention => p.warning_foreground, FolderDot::Failed => p.destructive, FolderDot::Unread => p.success, FolderDot::Working => p.info_foreground });
        div().id(SharedString::from(format!("project-folder-{}", key))).group("project-folder")
            .mt(px(8.)).h(px(32.)).px(px(12.)).flex().items_center().gap(px(10.)).rounded(px(6.)).cursor_pointer()
            .hover(|style| style.bg(hsla(p.sidebar_row_hover)))
            .child(Icon::Folder.render(px(16.), hsla(p.sidebar_icon)))
            .child(div().flex_1().min_w_0().truncate().text_sm().text_color(hsla(p.sidebar_foreground)).child(group.label.clone()))
            .children(color.map(|color| div().size(px(6.)).rounded_full().bg(hsla(color))))
            .when(!expanded && hidden_count > 0, |row| row.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(hidden_count.to_string())))
            .child(div().id(SharedString::from(format!("folder-new-{}", key))).invisible().group_hover("project-folder", |style| style.visible())
                .child(Icon::SquarePen.render(px(12.), hsla(p.muted_foreground)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.state.update(cx, |state, cx| {
                        if let Some(member) = t3_core::sidebar::grouping::pick_project_target(&new_thread, None) {
                            state.navigate_scoped(member.project.owner.clone(), crate::app_state::ScopedRouteAction::Draft(member.project.project.id.clone()), cx);
                        }
                    });
                })))
            .child(if expanded { Icon::ChevronDown } else { Icon::ChevronRight }.render(px(12.), hsla(p.muted_foreground)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.update(cx, |state, cx| {
                    state.project_preferences.set_expanded(&toggle, !expanded);
                    state.save_project_preferences(cx);
                    state.rebuild_sidebar(cx);
                    cx.emit(crate::app_state::AppEvent::Shell);
                });
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.menu.update(cx, |menu, cx| menu.open_scoped_project(context_group.clone(), event.position, window, cx));
            })).into_any_element()
    }
}
