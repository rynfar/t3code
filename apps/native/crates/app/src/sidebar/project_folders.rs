//! Project folders in the default-off boring sidebar.

use gpui::{AnyElement, Context, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Styled, div, prelude::*};
use t3_core::sidebar::fleet::FolderDot;
use super::SidebarPane;
use crate::icons::Icon;
use crate::theme::{hsla, metrics, palette, rem};

impl SidebarPane {
    pub(super) fn render_project_folder(&self, key: &str, expanded: bool, hidden_count: usize, dot: Option<FolderDot>, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let model = &state.sidebar.fleet;
        let Some(group) = model.folder_groups.get(key) else { return div().into_any_element() };
        let first = model.rows.iter().find_map(|row| match row { t3_core::sidebar::fleet::ScopedSidebarRow::Folder { key, .. } => Some(key.as_str()), _ => None }) == Some(key);
        let visual = crate::project_icon::ProjectVisual::for_scoped_project(state, &group.representative);
        let p = palette(cx);
        let toggle = group.clone();
        let new_thread = group.clone();
        let context_group = group.clone();
        let color = dot.map(|dot| match dot { FolderDot::Attention => p.warning, FolderDot::Failed => p.error, FolderDot::Unread => p.success, FolderDot::Working => p.info });
        div().id(SharedString::from(format!("project-folder-{}", key))).group("project-folder")
            .mt(rem(if first { 0. } else { 8. })).h(rem(32.)).px(rem(metrics::SIDEBAR_ROW_CONTENT_INSET)).flex().items_center().gap(rem(10.)).rounded(rem(metrics::CONTROL_RADIUS)).cursor_pointer()
            .hover(|style| style.bg(hsla(p.sidebar_row_hover)))
            .child(visual.render(crate::project_icon::size(16., cx), hsla(p.sidebar_icon), cx))
            .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.sidebar_foreground).opacity(0.9)).child(group.label.clone()))
            .children(color.map(|color| div().size(rem(6.)).rounded_full().bg(hsla(color))))
            .when(!expanded && hidden_count > 0, |row| row.child(div().text_xs().text_color(hsla(p.sidebar_muted_foreground)).group_hover("project-folder", |style| style.invisible()).child(hidden_count.to_string())))
            .child(div().id(SharedString::from(format!("folder-new-{}", key))).invisible().group_hover("project-folder", |style| style.visible())
                .child(Icon::SquarePen.render(crate::project_icon::size(12., cx), hsla(p.sidebar_muted_foreground)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.state.update(cx, |state, cx| {
                        if let Some(member) = t3_core::sidebar::grouping::pick_project_target(&new_thread, None) {
                            state.navigate_scoped(member.project.owner.clone(), crate::app_state::ScopedRouteAction::Draft(member.project.project.id.clone()), cx);
                        }
                    });
                })))
            .child(if expanded { Icon::ChevronDown } else { Icon::ChevronRight }.render(crate::project_icon::size(12., cx), hsla(p.sidebar_muted_foreground)))
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
