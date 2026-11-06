//! The sidebar's project filter (`SidebarProjectScope` in `Sidebar.tsx`): a popover listing
//! project groups, pinned first; checking groups scopes the thread list to them, All projects
//! clears it. Right-click opens the project menu; hover shows the pin toggle where supported, and
//! each row ends in a Project settings button.

use gpui::{AnyElement, Context, FontWeight, IntoElement, MouseButton, ParentElement, Pixels, Point, SharedString, Styled, anchored, deferred, div, prelude::*, px};
use t3_core::sidebar::ENVIRONMENT_ID;
use t3_core::sidebar::grouping::{ProjectGroup, build_project_groups};

use super::SidebarPane;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

impl SidebarPane {
    fn project_groups(&self, cx: &Context<Self>) -> Vec<ProjectGroup> {
        let state = self.state.read(cx);
        let Some(snapshot) = &state.shell.snapshot else { return Vec::new() };
        let groups = build_project_groups(ENVIRONMENT_ID, &snapshot.projects, &state.sidebar.settings.grouping);
        let open = groups.into_iter().filter(|group| !group.archived);
        let (pinned, rest): (Vec<_>, Vec<_>) = open.partition(|group| group.pinned);
        pinned.into_iter().chain(rest).collect()
    }

    pub(super) fn toggle_project_filter(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.project_filter = match self.project_filter {
            Some(_) => None,
            None => Some(position),
        };
        cx.notify();
    }

    fn toggle_scope(&mut self, group: &ProjectGroup, cx: &mut Context<Self>) {
        let groups = self.project_groups(cx);
        self.state.update(cx, |state, cx| {
            let mut scope = state.sidebar.project_filter_scope.clone();
            scope.toggle(group, &groups, state.shell.synchronized && state.server_config.config.is_some());
            state.set_project_scope(scope, cx);
        });
    }

    /// The filter trigger: the list-filter icon, or the scoped projects' favicons (two at most,
    /// overlapping), labelled with their names.
    pub(super) fn render_project_filter_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let state = self.state.read(cx);
        let scoped: Vec<ProjectGroup> = self.project_groups(cx).into_iter().filter(|group| state.sidebar.project_filter_scope.is_selected(group)).collect();
        let fleet_groups = state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx);
        let fleet_keys = state.scoped_project_scope(&fleet_groups);
        let fleet_selected = fleet_groups.iter().filter(|group| fleet_keys.contains(&group.key)).collect::<Vec<_>>();
        let label = if !fleet_selected.is_empty() { format!("Filter threads by project: {}", fleet_selected.iter().map(|group| group.label.as_str()).collect::<Vec<_>>().join(", ")) }
        else if scoped.is_empty() {
            "Filter threads by project".to_owned()
        } else {
            format!("Filter threads by project: {}", scoped.iter().map(|group| group.label.as_str()).collect::<Vec<_>>().join(", "))
        };
        let size = crate::project_icon::size(if scoped.len() > 1 { 14. } else { 16. }, cx);
        let favicon = |group: &ProjectGroup| crate::project_icon::render(state, &group.representative_id, size, hsla(p.sidebar_icon), cx);
        let content = if !state.fleet.environments.is_empty() && !fleet_selected.is_empty() {
            div().flex().flex_none().children(fleet_selected.iter().take(2).enumerate().map(|(index,group)| div().when(index>0,|style| style.ml(px(-6.))).child(crate::project_icon::ProjectVisual::for_scoped_project(state,&group.representative).render(size,hsla(p.sidebar_icon),cx)))).into_any_element()
        } else if !state.fleet.environments.is_empty() || scoped.is_empty() {
            Self::button_icon(Icon::ListFilter, "project-filter", cx).into_any_element()
        } else {
            div().flex().flex_none().children(scoped.iter().take(2).enumerate().map(|(index, group)| div().when(index > 0, |this| this.ml(px(-6.))).child(favicon(group)))).into_any_element()
        };
        Self::header_button("project-filter", content, move |_| label.clone(), true, cx)
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| this.toggle_project_filter(event.position(), cx)))
    }

    pub(super) fn render_project_filter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let position = self.project_filter?;
        if !self.state.read(cx).fleet.environments.is_empty() { return Some(self.render_scoped_project_filter(position, cx)); }
        let p = palette(cx);
        let groups = self.project_groups(cx);
        let state = self.state.read(cx);
        let scope = state.sidebar.project_scope.clone();
        let can_pin = state.can_pin_projects();
        let row = |id: SharedString, checked: bool| {
            div()
                .id(id)
                .group("project-row")
                .min_h(px(28.))
                .px(px(8.))
                .py(px(4.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(4.))
                .text_sm()
                .text_color(hsla(p.popover_foreground))
                .cursor_pointer()
                .hover(|this| this.bg(hsla(p.accent)))
                .child(div().w(px(16.)).flex_none().children(checked.then(|| Icon::Check.render(px(14.), hsla(p.foreground)))))
        };
        let mut rows: Vec<AnyElement> = vec![
            row("project-filter-all".into(), scope.is_none())
                .child(Icon::Folder.render(crate::project_icon::size(16., cx), hsla(p.icon_muted)))
                .child("All projects")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |state, cx| state.set_project_scope(Default::default(), cx));
                    this.project_filter = None;
                    cx.notify();
                }))
                .into_any_element(),
            div().h(px(1.)).my(px(4.)).mx(px(-4.)).bg(hsla(p.border)).into_any_element(),
        ];
        for group in groups {
            let checked = state.sidebar.project_filter_scope.is_selected(&group);
            let (toggle_group, menu_project, pin_group) = (group.clone(), group.representative_id.clone(), group.clone());
            let settings_project = group.representative_id.clone();
            rows.push(
                row(SharedString::from(format!("project-filter-{}", group.key)), checked)
                    .child(crate::project_icon::render(state, &group.representative_id, crate::project_icon::size(16., cx), hsla(p.icon_muted), cx))
                    .child(div().flex_1().min_w_0().truncate().child(group.label.clone()))
                    .when(can_pin || group.pinned, |this| {
                        this.child(
                            div()
                                .id(SharedString::from(format!("project-pin-{}", group.key)))
                                .flex_none()
                                .when(!group.pinned, |this| this.invisible().group_hover("project-row", |this| this.visible()))
                                .child(Icon::Pin.render(px(14.), hsla(p.muted_foreground)))
                                .when(can_pin, |this| {
                                    this.on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        let pinned = !pin_group.pinned;
                                        this.state.update(cx, |state, cx| {
                                            for id in &pin_group.member_project_ids {
                                                state.set_project_pinned(id, pinned, cx);
                                            }
                                        });
                                    }))
                                }),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("project-settings-{}", group.key)))
                            .flex_none()
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .hover(|this| this.bg(hsla(p.accent)))
                            .child(Icon::Settings.render(px(14.), hsla(p.muted_foreground)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.project_filter = None;
                                crate::settings::open_project(&settings_project, cx);
                                cx.notify();
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_scope(&toggle_group, cx)))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.menu.update(cx, |menu, cx| menu.open_for_project(&menu_project, event.position, window, cx));
                    }))
                    .into_any_element(),
            );
        }
        Some(
            deferred(
                div()
                    .id("project-filter-backdrop")
                    .absolute()
                    .inset_0()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.project_filter = None;
                        cx.notify();
                    }))
                    .child(
                        anchored().position(position + gpui::point(px(-8.), px(16.))).snap_to_window_with_margin(px(8.)).child(
                            div()
                                .id("project-filter-popup")
                                .occlude()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .relative()
                                .w(px(240.))
                                .max_h(px(360.))
                                .overflow_y_scroll()
                                .p(px(5.))
                                .rounded(px(8.))
                                .child(crate::glass::backdrop(8., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
                                .child(div().px(px(8.)).py(px(6.)).text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Filter by project"))
                                .children(rows),
                        ),
                    ),
            )
            .with_priority(130)
            .into_any_element(),
        )
    }
}

impl SidebarPane {
    fn render_scoped_project_filter(&self, position: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let groups = state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx);
        let selected = state.scoped_project_scope(&groups);
        let p = palette(cx);
        let mut rows = vec![div().id("scoped-project-all").px(px(8.)).py(px(6.)).cursor_pointer().child("All projects")
            .on_click(cx.listener(|this, _, _, cx| { this.state.update(cx, |state, cx| {
                state.set_scoped_project_scope(Vec::new(), cx);
            }); })).into_any_element()];
        for group in groups.into_iter().filter(|group| !group.archived) {
            let toggle = group.clone(); let settings = group.clone(); let pin = group.clone(); let menu = group.clone();
            let checked = selected.contains(&group.key);
            let can_pin = group.registrations.iter().all(|project| state.scoped_ready(&project.owner,t3_core::auth::ORCHESTRATION_OPERATE_SCOPE) && state.scoped_environment(&project.owner.saved_id).and_then(|entry| entry.config.config.as_ref()).is_some_and(|config| config.environment.capabilities.project_organization==Some(true)));
            rows.push(div().id(SharedString::from(format!("scoped-project-filter-{}", group.key))).flex().items_center().gap(px(8.)).px(px(8.)).py(px(6.)).rounded(px(4.)).cursor_pointer().hover(|style| style.bg(hsla(p.accent)))
                .child(div().w(px(16.)).children(checked.then(|| Icon::Check.render(px(14.), hsla(p.foreground)))))
                .child(div().flex_1().min_w_0().truncate().child(group.label.clone()))
                .when(group.members.iter().any(|member| Some(member.project.owner.saved_id.as_str()) != state.environments.active.as_deref()), |row| row.child(Icon::Monitor.render(px(12.), hsla(p.muted_foreground))))
                .child(div().id(SharedString::from(format!("scoped-project-settings-{}", group.key))).child(Icon::Settings.render(px(14.), hsla(p.muted_foreground)))
                    .on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.state.update(cx, |state, cx| state.run_scoped_project_action(&settings, t3_core::thread_actions::ProjectAction::Settings, cx)); })))
                .when(can_pin,|row| row.child(div().id(SharedString::from(format!("scoped-project-pin-{}", group.key))).child(if pin.pinned { Icon::PinOff } else { Icon::Pin }.render(px(14.), hsla(p.muted_foreground)))
                    .on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.state.update(cx, |state, cx| state.pin_scoped_project_group(&pin, !pin.pinned, cx)); }))))
                .on_click(cx.listener(move |this, _, _, cx| { this.state.update(cx, |state, cx| {
                    let groups = state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx);
                    let mut keys = state.scoped_project_scope(&groups);
                    if let Some(index) = keys.iter().position(|key| key == &toggle.key) { keys.remove(index); } else { keys.push(toggle.key.clone()); }
                    state.set_scoped_project_scope(keys, cx);
                }); }))
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| { cx.stop_propagation(); this.menu.update(cx, |context, cx| context.open_scoped_project(menu.clone(), event.position, window, cx)); })).into_any_element());
        }
        deferred(div().id("scoped-project-filter-backdrop").absolute().inset_0().occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| { this.project_filter = None; cx.notify(); }))
            .child(anchored().position(position + gpui::point(px(-8.), px(16.))).snap_to_window_with_margin(px(8.)).child(
                div().id("scoped-project-filter-popup").occlude().w(px(280.)).max_h(px(360.)).overflow_y_scroll().p(px(5.)).rounded(px(8.)).text_sm().text_color(hsla(p.popover_foreground))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(crate::glass::backdrop(8., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx)).children(rows))))
            .with_priority(130).into_any_element()
    }
}
