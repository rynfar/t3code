//! The sidebar's project filter (`SidebarProjectScope` in `Sidebar.tsx`,
//! `ProjectComboboxRow.tsx`, `ProjectEnvironmentBadge.tsx`, `ProjectPinButton.tsx`): a searchable
//! combobox popover listing `"All projects"` and logical project groups, pinned first with row
//! order frozen while open (`useOrderWhileOpen`). Plain click selects a single project and closes;
//! Shift-click toggles multi-select and keeps the popover open.

use gpui::{
    AnyElement, App, BoxShadow, Context, Focusable, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, Pixels, Point, SharedString, Styled, Window, anchored, deferred, div, point,
    prelude::*, px,
};
use gpui_component::input::{Escape, Input, MoveDown, MoveUp};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Sizable};
use t3_core::sidebar::ENVIRONMENT_ID;
use t3_core::sidebar::grouping::{
    GroupConsumer, ProjectEnvironmentBadge, ProjectGroup, ProjectScope, ScopedProjectGroup,
    build_project_groups, filter_project_scope_items, order_while_open,
    pickable_scoped_project_groups, project_groups_span_environments,
    resolve_project_environment_badge,
};

use super::SidebarPane;
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::theme::{Palette, hsla, palette};

#[derive(Clone)]
enum ScopeEntry<G> {
    All,
    Group(G),
}

impl<G> ScopeEntry<G> {
    fn value<'a>(&'a self, key: impl Fn(&'a G) -> &'a str) -> &'a str {
        match self {
            ScopeEntry::All => "all",
            ScopeEntry::Group(group) => key(group),
        }
    }

    fn label<'a>(&'a self, label: impl Fn(&'a G) -> &'a str) -> &'a str {
        match self {
            ScopeEntry::All => "All projects",
            ScopeEntry::Group(group) => label(group),
        }
    }
}

fn primary_environment_id(state: &AppState) -> Option<&str> {
    state.local_saved_environment().map(|environment| environment.id.as_str()).or_else(|| {
        let mut local = state.environments.environments.iter().filter(|environment| environment.local);
        let first = local.next()?;
        local.next().is_none().then_some(first.id.as_str())
    })
}

fn environment_machine_icon(state: &AppState, saved_id: &str) -> Icon {
    let config = state
        .scoped_environment(saved_id)
        .and_then(|entry| entry.config.config.as_ref())
        .or_else(|| {
            (state.environments.active.as_deref() == Some(saved_id))
                .then_some(state.server_config.config.as_ref())
                .flatten()
        });
    let machine = config
        .and_then(|cfg| {
            cfg.settings
                .environment_icon
                .as_deref()
                .or_else(|| cfg.environment.platform.as_ref().map(|platform| platform.machine.as_str()))
        })
        .unwrap_or("server");
    match machine {
        "laptop" => Icon::Laptop,
        "desktop" => Icon::Monitor,
        "cloud" => Icon::Cloud,
        "linux" => Icon::Linux,
        "mac-mini" => Icon::MacMini,
        "mac-studio" => Icon::MacStudio,
        _ => Icon::Server,
    }
}

fn scope_row_icon(icon: AnyElement, selected: bool, p: &Palette) -> AnyElement {
    div()
        .relative()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(icon)
        .when(selected, |this| {
            this.child(
                div()
                    .absolute()
                    .right(px(-4.))
                    .bottom(px(-4.))
                    .size(px(12.))
                    .rounded_full()
                    .bg(hsla(p.primary))
                    .shadow(vec![BoxShadow {
                        color: hsla(p.popover),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(1.5),
                        inset: false,
                    }])
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::Check.render(px(8.), hsla(p.primary_foreground))),
            )
        })
        .into_any_element()
}

fn trigger_favicons(mut icons: Vec<AnyElement>, p: &Palette) -> AnyElement {
    if icons.len() == 1 {
        return icons.remove(0);
    }
    let second = icons.remove(1);
    let first = icons.remove(0);
    div()
        .flex()
        .flex_none()
        .items_center()
        .child(div().flex().items_center().child(first))
        .child(
            div()
                .ml(px(-4.))
                .flex()
                .items_center()
                .rounded(px(3.5))
                .bg(hsla(p.sidebar))
                .shadow(vec![BoxShadow {
                    color: hsla(p.sidebar),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(2.),
                    inset: false,
                }])
                .child(second),
        )
        .into_any_element()
}

fn pin_tooltip_label(pinned: bool, label: &str, cx: &App) -> String {
    let action = if pinned { "Unpin" } else { "Pin" };
    match crate::keymap::label("thread.pin", cx) {
        Some(shortcut) => format!("{action} {label} ({shortcut})"),
        None => format!("{action} {label}"),
    }
}

impl SidebarPane {
    pub(super) fn project_groups(&self, cx: &App) -> Vec<ProjectGroup> {
        let state = self.state.read(cx);
        let Some(snapshot) = &state.shell.snapshot else { return Vec::new() };
        let groups = build_project_groups(ENVIRONMENT_ID, &snapshot.projects, &state.sidebar.settings.grouping);
        let open = groups.into_iter().filter(|group| !group.archived);
        let (pinned, rest): (Vec<_>, Vec<_>) = open.partition(|group| group.pinned);
        pinned.into_iter().chain(rest).collect()
    }

    pub(crate) fn close_project_filter(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.close_project_filter_with_focus(window, false, cx);
    }

    pub(crate) fn close_project_filter_with_focus(
        &mut self,
        window: Option<&mut Window>,
        restore_focus: bool,
        cx: &mut Context<Self>,
    ) {
        self.project_filter = None;
        self.project_filter_order.borrow_mut().clear();
        self.project_filter_highlighted = None;
        let return_focus = self.project_filter_return_focus.take();
        if let Some(window) = window {
            if !self.project_filter_search.read(cx).value().is_empty() {
                self.project_filter_search.update(cx, |input, cx| input.set_value("", window, cx));
            }
            if restore_focus && let Some(focus) = return_focus {
                window.focus(&focus, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn sync_project_filter_highlight(&mut self, cx: &App) {
        if self.project_filter.is_none() {
            self.project_filter_highlighted = None;
            return;
        }
        self.project_filter_highlighted = self.visible_filter_values(cx).into_iter().next();
        self.project_filter_scroll.scroll_to_item(0);
    }

    pub(super) fn toggle_project_filter(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_filter.is_some() {
            self.close_project_filter(Some(window), cx);
            return;
        }
        self.project_filter_order.borrow_mut().clear();
        self.project_filter_return_focus = window.focused(cx);
        self.project_filter = Some(position);
        self.project_filter_search.update(cx, |input, cx| {
            if !input.value().is_empty() {
                input.set_value("", window, cx);
            }
            input.focus(window, cx);
        });
        self.sync_project_filter_highlight(cx);
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

    fn select_single_scope(&mut self, group: &ProjectGroup, window: &mut Window, cx: &mut Context<Self>) {
        let scope = ProjectScope::single(group);
        self.state.update(cx, |state, cx| state.set_project_scope(scope, cx));
        self.close_project_filter(Some(window), cx);
    }

    fn toggle_scoped_scope(&mut self, key: &str, cx: &mut Context<Self>) {
        let key = key.to_owned();
        self.state.update(cx, |state, cx| {
            let groups = state.project_groups_for(GroupConsumer::Sidebar, cx);
            let mut keys = state.scoped_project_scope(&groups);
            if let Some(index) = keys.iter().position(|existing| existing == &key) {
                keys.remove(index);
            } else {
                keys.push(key);
            }
            state.set_scoped_project_scope(keys, cx);
        });
    }

    fn select_single_scoped_scope(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let keys = vec![key.to_owned()];
        self.state.update(cx, |state, cx| state.set_scoped_project_scope(keys, cx));
        self.close_project_filter(Some(window), cx);
    }

    fn can_pin_scoped_group(state: &AppState, group: &ScopedProjectGroup) -> bool {
        !group.registrations.is_empty()
            && group.registrations.iter().all(|project| {
                state.scoped_ready(&project.owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)
                    && state
                        .scoped_environment(&project.owner.saved_id)
                        .and_then(|entry| entry.config.config.as_ref())
                        .is_some_and(|config| config.environment.capabilities.project_organization == Some(true))
            })
    }

    fn visible_filter_values(&self, cx: &App) -> Vec<String> {
        let query = self.project_filter_search.read(cx).value().to_string();
        let state = self.state.read(cx);
        if !state.fleet.environments.is_empty() {
            let raw = pickable_scoped_project_groups(&state.project_groups_for(GroupConsumer::Sidebar, cx));
            let ordered = order_while_open(&mut self.project_filter_order.borrow_mut(), raw, |g| g.key.as_str());
            let items: Vec<_> = std::iter::once(ScopeEntry::All).chain(ordered.into_iter().map(ScopeEntry::Group)).collect();
            filter_project_scope_items(items, &query, |e| e.value(|g| g.key.as_str()), |e| e.label(|g| g.label.as_str()))
                .into_iter()
                .map(|e| e.value(|g| g.key.as_str()).to_owned())
                .collect()
        } else {
            let ordered = order_while_open(&mut self.project_filter_order.borrow_mut(), self.project_groups(cx), |g| g.key.as_str());
            let items: Vec<_> = std::iter::once(ScopeEntry::All).chain(ordered.into_iter().map(ScopeEntry::Group)).collect();
            filter_project_scope_items(items, &query, |e| e.value(|g| g.key.as_str()), |e| e.label(|g| g.label.as_str()))
                .into_iter()
                .map(|e| e.value(|g| g.key.as_str()).to_owned())
                .collect()
        }
    }

    pub(super) fn move_project_filter_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let values = self.visible_filter_values(cx);
        if values.is_empty() {
            self.project_filter_highlighted = None;
            cx.notify();
            return;
        }
        let next = match self.project_filter_highlighted.as_deref().and_then(|cur| values.iter().position(|v| v == cur)) {
            Some(idx) => (idx as isize + step).rem_euclid(values.len() as isize) as usize,
            None if step >= 0 => 0,
            None => values.len() - 1,
        };
        self.project_filter_highlighted = Some(values[next].clone());
        self.project_filter_scroll.scroll_to_item(next);
        cx.notify();
    }

    pub(super) fn activate_highlighted_project_filter(&mut self, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        let values = self.visible_filter_values(cx);
        let Some(target) = self
            .project_filter_highlighted
            .clone()
            .filter(|cur| values.contains(cur))
            .or_else(|| values.first().cloned())
        else {
            return;
        };
        let fleet = !self.state.read(cx).fleet.environments.is_empty();
        if target == "all" {
            if fleet {
                self.state.update(cx, |state, cx| state.set_scoped_project_scope(Vec::new(), cx));
            } else {
                self.state.update(cx, |state, cx| state.set_project_scope(Default::default(), cx));
            }
            self.close_project_filter(Some(window), cx);
            return;
        }
        if fleet {
            if shift {
                self.toggle_scoped_scope(&target, cx);
            } else {
                self.select_single_scoped_scope(&target, window, cx);
            }
        } else if let Some(group) = self.project_groups(cx).into_iter().find(|g| g.key == target) {
            if shift {
                self.toggle_scope(&group, cx);
            } else {
                self.select_single_scope(&group, window, cx);
            }
        }
    }

    fn toggle_highlighted_project_pin(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(target) = self.project_filter_highlighted.clone().filter(|cur| cur != "all") else {
            return false;
        };
        let fleet = !self.state.read(cx).fleet.environments.is_empty();
        if fleet {
            let group = {
                let state = self.state.read(cx);
                let groups = pickable_scoped_project_groups(&state.project_groups_for(GroupConsumer::Sidebar, cx));
                groups.into_iter().find(|g| g.key == target && Self::can_pin_scoped_group(state, g))
            };
            let Some(group) = group else { return false };
            self.state.update(cx, |state, cx| state.pin_scoped_project_group(&group, !group.pinned, cx));
            cx.notify();
            true
        } else {
            if !self.state.read(cx).can_pin_projects() {
                return false;
            }
            let Some(group) = self.project_groups(cx).into_iter().find(|g| g.key == target) else {
                return false;
            };
            let pinned = !group.pinned;
            self.state.update(cx, |state, cx| {
                for id in &group.member_project_ids {
                    state.set_project_pinned(id, pinned, cx);
                }
            });
            cx.notify();
            true
        }
    }

    fn open_highlighted_project_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(target) = self.project_filter_highlighted.clone().filter(|cur| cur != "all") else {
            return false;
        };
        let fleet = !self.state.read(cx).fleet.environments.is_empty();
        if fleet {
            let group = {
                let state = self.state.read(cx);
                let groups = pickable_scoped_project_groups(&state.project_groups_for(GroupConsumer::Sidebar, cx));
                groups.into_iter().find(|g| g.key == target)
            };
            let Some(group) = group else { return false };
            self.close_project_filter(Some(window), cx);
            self.state.update(cx, |state, cx| {
                state.run_scoped_project_action(&group, t3_core::thread_actions::ProjectAction::Settings, cx);
            });
            true
        } else {
            let Some(group) = self.project_groups(cx).into_iter().find(|g| g.key == target) else {
                return false;
            };
            self.close_project_filter(Some(window), cx);
            crate::settings::open_project(&group.representative_id, cx);
            true
        }
    }

    /// The filter trigger: the list-filter icon, or the scoped projects' favicons (two at most,
    /// overlapping with a `ring-2 ring-sidebar` cutout on the second), labelled with their names.
    pub(super) fn render_project_filter_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let state = self.state.read(cx);
        if !state.fleet.environments.is_empty() {
            let fleet_groups = pickable_scoped_project_groups(&state.project_groups_for(GroupConsumer::Sidebar, cx));
            let fleet_keys = state.scoped_project_scope(&fleet_groups);
            let selected: Vec<_> = fleet_groups.iter().filter(|group| fleet_keys.contains(&group.key)).collect();
            let label = if selected.is_empty() {
                "Filter threads by project".to_owned()
            } else {
                format!(
                    "Filter threads by project: {}",
                    selected.iter().map(|group| group.label.as_str()).collect::<Vec<_>>().join(", ")
                )
            };
            let content = if selected.is_empty() {
                Self::button_icon(Icon::ListFilter, "project-filter", cx).into_any_element()
            } else {
                let size = crate::project_icon::size(if selected.len() > 1 { 14. } else { 16. }, cx);
                let icons = selected
                    .iter()
                    .take(2)
                    .map(|group| {
                        crate::project_icon::ProjectVisual::for_scoped_project(state, &group.representative)
                            .render(size, hsla(p.sidebar_icon), cx)
                            .into_any_element()
                    })
                    .collect();
                trigger_favicons(icons, &p)
            };
            return Self::header_button("project-filter", content, move |_| label.clone(), true, cx).on_click(
                cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                    this.toggle_project_filter(event.position(), window, cx)
                }),
            );
        }

        let scoped: Vec<ProjectGroup> = self
            .project_groups(cx)
            .into_iter()
            .filter(|group| state.sidebar.project_filter_scope.is_selected(group))
            .collect();
        let label = if scoped.is_empty() {
            "Filter threads by project".to_owned()
        } else {
            format!(
                "Filter threads by project: {}",
                scoped.iter().map(|group| group.label.as_str()).collect::<Vec<_>>().join(", ")
            )
        };
        let content = if scoped.is_empty() {
            Self::button_icon(Icon::ListFilter, "project-filter", cx).into_any_element()
        } else {
            let size = crate::project_icon::size(if scoped.len() > 1 { 14. } else { 16. }, cx);
            let icons = scoped
                .iter()
                .take(2)
                .map(|group| {
                    crate::project_icon::render(state, &group.representative_id, size, hsla(p.sidebar_icon), cx)
                        .into_any_element()
                })
                .collect();
            trigger_favicons(icons, &p)
        };
        Self::header_button("project-filter", content, move |_| label.clone(), true, cx).on_click(cx.listener(
            |this, event: &gpui::ClickEvent, window, cx| this.toggle_project_filter(event.position(), window, cx),
        ))
    }

    fn render_popup_shell(
        &self,
        backdrop_id: &'static str,
        popup_id: &'static str,
        position: Point<Pixels>,
        rows: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let body = if rows.is_empty() {
            div()
                .py(px(18.))
                .px(px(8.))
                .flex()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child("No matching projects.")
                .into_any_element()
        } else {
            div()
                .id("project-filter-list")
                .max_h(px(320.))
                .overflow_y_scroll()
                .track_scroll(&self.project_filter_scroll)
                .p(px(4.))
                .flex()
                .flex_col()
                .gap(px(1.))
                .children(rows)
                .into_any_element()
        };
        deferred(
            div()
                .id(backdrop_id)
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_project_filter(Some(window), cx)),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _, window, cx| this.close_project_filter(Some(window), cx)),
                )
                .child(
                    anchored()
                        .position(position + point(px(-8.), px(16.)))
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .id(popup_id)
                                .track_focus(&self.project_filter_focus)
                                .occlude()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                                .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                                    if !super::is_composing(&this.project_filter_search, window, cx) {
                                        this.move_project_filter_highlight(1, cx);
                                        cx.stop_propagation();
                                    }
                                }))
                                .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                                    if !super::is_composing(&this.project_filter_search, window, cx) {
                                        this.move_project_filter_highlight(-1, cx);
                                        cx.stop_propagation();
                                    }
                                }))
                                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                                    if !super::is_composing(&this.project_filter_search, window, cx) {
                                        this.close_project_filter_with_focus(Some(window), true, cx);
                                        cx.stop_propagation();
                                    }
                                }))
                                .on_action(cx.listener(|this, _: &crate::keymap::PinThread, _, cx| {
                                    if this.project_filter.is_some() {
                                        this.toggle_highlighted_project_pin(cx);
                                        cx.stop_propagation();
                                    }
                                }))
                                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                    if super::is_composing(&this.project_filter_search, window, cx) {
                                        return;
                                    }
                                    let key = &event.keystroke;
                                    let context_key = (key.key == "menu" && !key.modifiers.modified())
                                        || (key.key == "f10"
                                            && key.modifiers.shift
                                            && !key.modifiers.control
                                            && !key.modifiers.alt
                                            && !key.modifiers.platform
                                            && !key.modifiers.function);
                                    if context_key {
                                        if this.open_highlighted_project_settings(window, cx) {
                                            window.prevent_default();
                                            cx.stop_propagation();
                                        }
                                        return;
                                    }
                                    let search_focused = this.project_filter_search.read(cx).focus_handle(cx).is_focused(window);
                                    if !search_focused
                                        && !key.modifiers.control
                                        && !key.modifiers.alt
                                        && !key.modifiers.platform
                                        && !key.modifiers.function
                                    {
                                        match key.key.as_str() {
                                            "down" => {
                                                window.prevent_default();
                                                cx.stop_propagation();
                                                this.move_project_filter_highlight(1, cx);
                                            }
                                            "up" => {
                                                window.prevent_default();
                                                cx.stop_propagation();
                                                this.move_project_filter_highlight(-1, cx);
                                            }
                                            "escape" => {
                                                window.prevent_default();
                                                cx.stop_propagation();
                                                this.close_project_filter_with_focus(Some(window), true, cx);
                                            }
                                            "enter" => {
                                                window.prevent_default();
                                                cx.stop_propagation();
                                                this.activate_highlighted_project_filter(key.modifiers.shift, window, cx);
                                            }
                                            _ => {}
                                        }
                                    }
                                }))
                                .relative()
                                .w(px(288.))
                                .max_h(px(380.))
                                .rounded(px(8.))
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .text_sm()
                                .text_color(hsla(p.popover_foreground))
                                .shadow(crate::composer::controls::floating_shadow(cx.theme().is_dark()))
                                .child(crate::glass::backdrop(8., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
                                .child(
                                    div()
                                        .id("project-filter-search")
                                        .h(px(36.))
                                        .px(px(10.))
                                        .flex()
                                        .flex_none()
                                        .items_center()
                                        .gap(px(8.))
                                        .border_b_1()
                                        .border_color(hsla(p.border).opacity(0.7))
                                        .child(Icon::Search.sized(px(15.)).text_color(hsla(p.muted_foreground).opacity(0.6)))
                                        .child(
                                            div().flex_1().min_w_0().child(
                                                Input::new(&self.project_filter_search)
                                                    .appearance(false)
                                                    .bordered(false)
                                                    .xsmall()
                                                    .px_0(),
                                            ),
                                        ),
                                )
                                .child(body),
                        ),
                ),
        )
        .with_priority(130)
        .into_any_element()
    }

    pub(super) fn render_project_filter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let position = self.project_filter?;
        if !self.state.read(cx).fleet.environments.is_empty() {
            return Some(self.render_scoped_project_filter(position, cx));
        }
        let p = palette(cx);
        let query = self.project_filter_search.read(cx).value().to_string();
        let ordered = order_while_open(&mut self.project_filter_order.borrow_mut(), self.project_groups(cx), |g| g.key.as_str());
        let entries: Vec<_> = std::iter::once(ScopeEntry::All).chain(ordered.into_iter().map(ScopeEntry::Group)).collect();
        let filtered = filter_project_scope_items(
            entries,
            &query,
            |entry| entry.value(|g| g.key.as_str()),
            |entry| entry.label(|g| g.label.as_str()),
        );
        let state = self.state.read(cx);
        let all_selected = state.sidebar.project_scope.is_none();
        let can_pin = state.can_pin_projects();
        let mut rows = Vec::with_capacity(filtered.len());

        for entry in filtered {
            match entry {
                ScopeEntry::All => {
                    let highlighted = self.project_filter_highlighted.as_deref() == Some("all");
                    let folder = Icon::Folder.render(crate::project_icon::size(16., cx), hsla(p.icon_muted)).into_any_element();
                    rows.push(
                        div()
                            .id("project-filter-all")
                            .debug_selector(|| "project-filter-all".to_owned())
                            .min_h(px(30.))
                            .px(px(8.))
                            .py(px(3.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .when(highlighted, |s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .hover(|s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .child(scope_row_icon(folder, all_selected, &p))
                            .child(div().min_w_0().truncate().child("All projects"))
                            .child(div().ml_auto().size(px(24.)).flex_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.state.update(cx, |state, cx| state.set_project_scope(Default::default(), cx));
                                this.close_project_filter(Some(window), cx);
                            }))
                            .into_any_element(),
                    );
                }
                ScopeEntry::Group(group) => {
                    let checked = state.sidebar.project_filter_scope.is_selected(&group);
                    let highlighted = self.project_filter_highlighted.as_deref() == Some(group.key.as_str());
                    let row_id_str = format!("project-filter-{}", group.key);
                    let row_group = SharedString::from(format!("project-scope-row-{}", group.key));
                    let visual = crate::project_icon::render(
                        state,
                        &group.representative_id,
                        crate::project_icon::size(16., cx),
                        hsla(p.icon_muted),
                        cx,
                    )
                    .into_any_element();
                    let settings_project = group.representative_id.clone();
                    let settings_label = format!("Project settings for {}", group.label);
                    let pin_group_data = group.clone();
                    let pin_label = group.label.clone();
                    let is_pinned = group.pinned;
                    let pin_btn_group = SharedString::from(format!("project-pin-btn-{}", group.key));
                    let toggle_group = group.clone();
                    let menu_project = group.representative_id.clone();

                    let actions = div()
                        .ml_auto()
                        .flex()
                        .flex_none()
                        .items_center()
                        .child(
                            div()
                                .id(SharedString::from(format!("project-settings-{}", group.key)))
                                .size(px(24.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .invisible()
                                .when(highlighted, |s| s.visible())
                                .group_hover(row_group.clone(), |s| s.visible())
                                .hover(|s| s.bg(hsla(p.accent)))
                                .tooltip(move |window, cx| Tooltip::new(settings_label.clone()).build(window, cx))
                                .child(Icon::Settings.render(px(14.), hsla(p.muted_foreground)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_project_filter(Some(window), cx);
                                    crate::settings::open_project(&settings_project, cx);
                                })),
                        )
                        .child(if can_pin {
                            div()
                                .id(SharedString::from(format!("project-pin-{}", group.key)))
                                .group(pin_btn_group.clone())
                                .relative()
                                .size(px(24.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .when(!is_pinned, |s| {
                                    s.invisible()
                                        .when(highlighted, |s| s.visible())
                                        .group_hover(row_group.clone(), |s| s.visible())
                                })
                                .hover(|s| s.bg(hsla(p.accent)))
                                .tooltip(move |window, cx| Tooltip::new(pin_tooltip_label(is_pinned, &pin_label, cx)).build(window, cx))
                                .child(
                                    Icon::Pin
                                        .render(px(14.), hsla(p.muted_foreground))
                                        .when(is_pinned, |icon| icon.group_hover(pin_btn_group.clone(), |s| s.invisible())),
                                )
                                .when(is_pinned, |btn| {
                                    btn.child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .invisible()
                                            .group_hover(pin_btn_group, |s| s.visible())
                                            .child(Icon::PinOff.render(px(14.), hsla(p.foreground))),
                                    )
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let next_pinned = !pin_group_data.pinned;
                                    this.state.update(cx, |state, cx| {
                                        for id in &pin_group_data.member_project_ids {
                                            state.set_project_pinned(id, next_pinned, cx);
                                        }
                                    });
                                }))
                                .into_any_element()
                        } else {
                            div().size(px(24.)).flex_none().into_any_element()
                        });

                    let debug_id = row_id_str.clone();
                    rows.push(
                        div()
                            .id(SharedString::from(row_id_str))
                            .debug_selector(move || debug_id.clone())
                            .group(row_group)
                            .min_h(px(30.))
                            .px(px(8.))
                            .py(px(3.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .when(highlighted, |s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .hover(|s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .child(scope_row_icon(visual, checked, &p))
                            .child(div().min_w_0().truncate().child(group.label.clone()))
                            .child(actions)
                            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                                if event.modifiers().shift {
                                    this.toggle_scope(&toggle_group, cx);
                                } else {
                                    this.select_single_scope(&toggle_group, window, cx);
                                }
                            }))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.close_project_filter(Some(window), cx);
                                    this.menu.update(cx, |menu, cx| menu.open_for_project(&menu_project, event.position, window, cx));
                                }),
                            )
                            .into_any_element(),
                    );
                }
            }
        }

        Some(self.render_popup_shell("project-filter-backdrop", "project-filter-popup", position, rows, cx))
    }

    fn render_scoped_project_filter(&self, position: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let query = self.project_filter_search.read(cx).value().to_string();
        let (all_groups, selected, span_environments) = {
            let state = self.state.read(cx);
            let raw = state.project_groups_for(GroupConsumer::Sidebar, cx);
            let selected = state.scoped_project_scope(&raw);
            let pickable = pickable_scoped_project_groups(&raw);
            let span = project_groups_span_environments(&pickable);
            (pickable, selected, span)
        };
        let ordered = order_while_open(&mut self.project_filter_order.borrow_mut(), all_groups, |g| g.key.as_str());
        let entries: Vec<_> = std::iter::once(ScopeEntry::All).chain(ordered.into_iter().map(ScopeEntry::Group)).collect();
        let filtered = filter_project_scope_items(
            entries,
            &query,
            |entry| entry.value(|g| g.key.as_str()),
            |entry| entry.label(|g| g.label.as_str()),
        );
        let state = self.state.read(cx);
        let primary = primary_environment_id(state);
        let all_selected = selected.is_empty();
        let mut rows = Vec::with_capacity(filtered.len());

        for entry in filtered {
            match entry {
                ScopeEntry::All => {
                    let highlighted = self.project_filter_highlighted.as_deref() == Some("all");
                    let folder = Icon::Folder.render(crate::project_icon::size(16., cx), hsla(p.icon_muted)).into_any_element();
                    rows.push(
                        div()
                            .id("scoped-project-all")
                            .debug_selector(|| "scoped-project-all".to_owned())
                            .min_h(px(30.))
                            .px(px(8.))
                            .py(px(3.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .when(highlighted, |s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .hover(|s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .child(scope_row_icon(folder, all_selected, &p))
                            .child(div().min_w_0().truncate().child("All projects"))
                            .child(div().ml_auto().size(px(24.)).flex_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.state.update(cx, |state, cx| state.set_scoped_project_scope(Vec::new(), cx));
                                this.close_project_filter(Some(window), cx);
                            }))
                            .into_any_element(),
                    );
                }
                ScopeEntry::Group(group) => {
                    let checked = selected.contains(&group.key);
                    let highlighted = self.project_filter_highlighted.as_deref() == Some(group.key.as_str());
                    let can_pin = Self::can_pin_scoped_group(state, &group);
                    let badge: Option<(Icon, ProjectEnvironmentBadge)> = span_environments
                        .then(|| resolve_project_environment_badge(&group, primary))
                        .flatten()
                        .map(|badge| (environment_machine_icon(state, &badge.environment_id), badge));
                    let visual = crate::project_icon::ProjectVisual::for_scoped_project(state, &group.representative)
                        .render(crate::project_icon::size(16., cx), hsla(p.icon_muted), cx)
                        .into_any_element();
                    let row_id_str = format!("scoped-project-filter-{}", group.key);
                    let row_group = SharedString::from(format!("scoped-project-row-{}", group.key));
                    let pin_btn_group = SharedString::from(format!("scoped-project-pin-btn-{}", group.key));
                    let settings_group = group.clone();
                    let settings_label = format!("Project settings for {}", group.label);
                    let pin_group_data = group.clone();
                    let pin_label = group.label.clone();
                    let is_pinned = group.pinned;
                    let toggle_key = group.key.clone();
                    let menu_group = group.clone();

                    let env_badge_el = badge.map(|(machine_icon, info)| {
                        let desc = info.description;
                        div()
                            .id(SharedString::from(format!("scoped-project-env-{}", group.key)))
                            .flex_none()
                            .flex()
                            .items_center()
                            .tooltip(move |window, cx| Tooltip::new(desc.clone()).build(window, cx))
                            .child(machine_icon.render(px(14.), hsla(p.muted_foreground)))
                            .into_any_element()
                    });

                    let actions = div()
                        .ml_auto()
                        .flex()
                        .flex_none()
                        .items_center()
                        .child(
                            div()
                                .id(SharedString::from(format!("scoped-project-settings-{}", group.key)))
                                .size(px(24.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .invisible()
                                .when(highlighted, |s| s.visible())
                                .group_hover(row_group.clone(), |s| s.visible())
                                .hover(|s| s.bg(hsla(p.accent)))
                                .tooltip(move |window, cx| Tooltip::new(settings_label.clone()).build(window, cx))
                                .child(Icon::Settings.render(px(14.), hsla(p.muted_foreground)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_project_filter(Some(window), cx);
                                    this.state.update(cx, |state, cx| {
                                        state.run_scoped_project_action(
                                            &settings_group,
                                            t3_core::thread_actions::ProjectAction::Settings,
                                            cx,
                                        )
                                    });
                                })),
                        )
                        .child(if can_pin {
                            div()
                                .id(SharedString::from(format!("scoped-project-pin-{}", group.key)))
                                .group(pin_btn_group.clone())
                                .relative()
                                .size(px(24.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .when(!is_pinned, |s| {
                                    s.invisible()
                                        .when(highlighted, |s| s.visible())
                                        .group_hover(row_group.clone(), |s| s.visible())
                                })
                                .hover(|s| s.bg(hsla(p.accent)))
                                .tooltip(move |window, cx| Tooltip::new(pin_tooltip_label(is_pinned, &pin_label, cx)).build(window, cx))
                                .child(
                                    Icon::Pin
                                        .render(px(14.), hsla(p.muted_foreground))
                                        .when(is_pinned, |icon| icon.group_hover(pin_btn_group.clone(), |s| s.invisible())),
                                )
                                .when(is_pinned, |btn| {
                                    btn.child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .invisible()
                                            .group_hover(pin_btn_group, |s| s.visible())
                                            .child(Icon::PinOff.render(px(14.), hsla(p.foreground))),
                                    )
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.state.update(cx, |state, cx| {
                                        state.pin_scoped_project_group(&pin_group_data, !pin_group_data.pinned, cx)
                                    });
                                }))
                                .into_any_element()
                        } else {
                            div().size(px(24.)).flex_none().into_any_element()
                        });

                    let debug_id = row_id_str.clone();
                    rows.push(
                        div()
                            .id(SharedString::from(row_id_str))
                            .debug_selector(move || debug_id.clone())
                            .group(row_group)
                            .min_h(px(30.))
                            .px(px(8.))
                            .py(px(3.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .when(highlighted, |s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .hover(|s| s.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .child(scope_row_icon(visual, checked, &p))
                            .child(div().min_w_0().truncate().child(group.label.clone()))
                            .children(env_badge_el)
                            .child(actions)
                            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                                if event.modifiers().shift {
                                    this.toggle_scoped_scope(&toggle_key, cx);
                                } else {
                                    this.select_single_scoped_scope(&toggle_key, window, cx);
                                }
                            }))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.close_project_filter(Some(window), cx);
                                    this.menu.update(cx, |context, cx| {
                                        context.open_scoped_project(menu_group.clone(), event.position, window, cx)
                                    });
                                }),
                            )
                            .into_any_element(),
                    );
                }
            }
        }

        self.render_popup_shell("scoped-project-filter-backdrop", "scoped-project-filter-popup", position, rows, cx)
    }
}
