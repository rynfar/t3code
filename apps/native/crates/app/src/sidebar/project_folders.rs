//! Project folders in the default-off boring sidebar.

use std::{cell::Cell, rc::Rc};
use gpui::{AnyElement, Bounds, Context, ElementId, FontWeight, IntoElement, MouseButton, ParentElement, Pixels, SharedString, Styled, Window, canvas, div, prelude::*};
use t3_core::sidebar::fleet::FolderDot;
use super::SidebarPane;
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
        let toggle = group.clone();
        let new_thread = group.clone();
        let context_group = group.clone();
        let keyboard_group = group.clone();
        let label = group.label.clone();
        let id = ElementId::Name(SharedString::from(format!("project-folder-{key}")));
        let new_id = ElementId::Name(SharedString::from(format!("folder-new-{key}")));
        let focus = crate::settings::controls::focus_handle(&id, window, cx);
        let keyboard_focus = focus.clone();
        let new_focus = crate::settings::controls::focus_handle(&new_id, window, cx);
        let bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let measured = bounds.clone();
        let color = dot.map(|dot| match dot { FolderDot::Attention => p.warning, FolderDot::Failed => p.error, FolderDot::Unread => p.success, FolderDot::Working => p.info });
        div().id(id).group("project-folder").relative().track_focus(&focus)
            .role(gpui::Role::Button).aria_label(label.clone()).aria_expanded(expanded)
            .focus_visible(|style| style.shadow(vec![gpui::BoxShadow { color:hsla(p.ring), offset:gpui::point(gpui::px(0.),gpui::px(0.)), blur_radius:gpui::px(0.), spread_radius:gpui::px(2.), inset:false }]))
            .mt(rem(if first { 0. } else { 8. })).h(rem(32.)).px(rem(metrics::SIDEBAR_ROW_CONTENT_INSET)).flex().items_center().gap(rem(10.)).rounded(rem(metrics::CONTROL_RADIUS)).cursor_pointer()
            .hover(|style| style.bg(hsla(p.sidebar_row_hover)))
            .child(visual.render(crate::project_icon::size(16., cx), hsla(p.sidebar_icon), cx))
            .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.sidebar_foreground).opacity(0.9)).child(label))
            .children(color.map(|color| div().size(rem(6.)).rounded_full().bg(hsla(color))))
            .when(!expanded && hidden_count > 0, |row| row.child(div().text_xs().text_color(hsla(p.sidebar_muted_foreground)).group_hover("project-folder", |style| style.invisible()).child(hidden_count.to_string())))
            .child(div().id(new_id).track_focus(&new_focus).role(gpui::Role::Button).aria_label("New thread")
                .opacity(0.).group_hover("project-folder", |style| style.opacity(1.))
                .focus_visible(|style| style.opacity(1.).shadow(vec![gpui::BoxShadow { color:hsla(p.ring), offset:gpui::point(gpui::px(0.),gpui::px(0.)), blur_radius:gpui::px(0.), spread_radius:gpui::px(2.), inset:false }]))
                .child(Icon::SquarePen.render(crate::project_icon::size(12., cx), hsla(p.sidebar_muted_foreground)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.state.update(cx, |state, cx| state.run_scoped_project_action(&new_thread, t3_core::thread_actions::ProjectAction::NewThread, cx));
                })))
            .child(if expanded { Icon::ChevronDown } else { Icon::ChevronRight }.render(crate::project_icon::size(12., cx), hsla(p.sidebar_muted_foreground)))
            .child(canvas(move |bounds, _, _| measured.set(Some(bounds)), |_, _, _, _| {}).absolute().inset_0())
            .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                let key = &event.keystroke;
                let context_key = (key.key == "menu" && !key.modifiers.modified())
                    || (key.key == "f10" && key.modifiers.shift && !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform && !key.modifiers.function);
                if context_key && keyboard_focus.is_focused(window) && let Some(bounds) = bounds.get() {
                    window.prevent_default();
                    cx.stop_propagation();
                    this.menu.update(cx, |menu, cx| menu.open_scoped_project(keyboard_group.clone(), bounds.bottom_left(), window, cx));
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.state.update(cx, |state, cx| {
                    state.project_preferences.set_expanded(&toggle, !expanded);
                    state.save_project_preferences(cx);
                    state.rebuild_sidebar(cx);
                    cx.emit(crate::app_state::AppEvent::Shell);
                });
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                window.focus(&focus, cx);
                this.menu.update(cx, |menu, cx| menu.open_scoped_project(context_group.clone(), event.position, window, cx));
            })).into_any_element()
    }
}
