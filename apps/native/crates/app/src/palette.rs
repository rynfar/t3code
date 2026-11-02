//! The command palette (`apps/web/src/components/CommandPalette.tsx`, design-system §5 and
//! §6.8): 576 wide, at most 420 tall, radius 16, 10vh from the top. Actions, recent threads,
//! and on a query, threads and projects ranked by `t3_core::palette`.

use std::collections::HashMap;

use gpui::{
    App, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, Render, ScrollHandle, SharedString, Styled,
    Subscription, Window, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use t3_core::palette::{
    CommandPaletteGroup, CommandPaletteItem, CommandPaletteProject, CommandPaletteRow, CommandPaletteSearch, PaletteThread, build_command_palette_rows,
    build_project_action_items, build_root_groups, build_thread_action_items, filter_command_palette_groups, recent_thread_items,
};
use t3_core::sidebar::time::{compact_relative_label, format_iso, parse_iso};

use crate::app_state::AppState;
use crate::keymap::{
    self, CopyThreadReference, JumpToThread1, JumpToThread2, JumpToThread3, JumpToThread4, JumpToThread5, JumpToThread6, JumpToThread7, JumpToThread8,
    JumpToThread9, PinThread,
};
use crate::sidebar::is_composing;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

const ENVIRONMENT: &str = t3_core::sidebar::ENVIRONMENT_ID;

pub struct CommandPalette {
    state: Entity<AppState>,
    input: Entity<InputState>,
    open: bool,
    query: String,
    root: Vec<CommandPaletteGroup>,
    threads: Vec<CommandPaletteItem>,
    projects: Vec<CommandPaletteItem>,
    groups: Vec<CommandPaletteGroup>,
    highlighted: Option<String>,
    focus_return: Option<FocusHandle>,
    scroll: ScrollHandle,
    _subscription: Subscription,
}

impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

/// The palette's own commands: what this client can run today, with web titles.
fn action_items(state: &AppState) -> Vec<CommandPaletteItem> {
    let action = |value: &str, title: &str, terms: &[&str], command: Option<&str>| CommandPaletteItem {
        value: value.to_owned(),
        title: title.to_owned(),
        search_terms: std::iter::once(title).chain(terms.iter().copied()).map(str::to_owned).collect(),
        shortcut_command: command.map(str::to_owned),
        ..Default::default()
    };
    let mut items = vec![action("action:toggle-sidebar", "Toggle sidebar", &["hide", "show", "panel"], Some("sidebar.toggle"))];
    if let Some(thread) = state.selected_thread.as_deref().and_then(|id| state.sidebar.model.thread(id)) {
        let pin = if thread.pinned { "Unpin thread" } else { "Pin thread" };
        let settle = if thread.is_settled() { "Un-settle thread" } else { "Settle thread" };
        items.push(action("action:pin", pin, &["pin"], Some("thread.pin")));
        items.push(action("action:settle", settle, &["archive", "done"], Some("thread.settle")));
        items.push(action("action:copy-thread", "Copy thread reference", &["link", "copy"], Some("thread.copyReference")));
    }
    items.push(action("action:previous-thread", "Previous thread", &["navigate"], Some("thread.previous")));
    items.push(action("action:next-thread", "Next thread", &["navigate"], Some("thread.next")));
    items
}

impl CommandPalette {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search commands, projects, and threads..."));
        let subscription = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                this.query = input.read(cx).value().to_string();
                this.refilter(cx);
            }
            InputEvent::PressEnter { .. } => this.activate_highlighted(window, cx),
            _ => {}
        });
        CommandPalette {
            state,
            input,
            open: false,
            query: String::new(),
            root: Vec::new(),
            threads: Vec::new(),
            projects: Vec::new(),
            groups: Vec::new(),
            highlighted: None,
            focus_return: None,
            scroll: ScrollHandle::new(),
            _subscription: subscription,
        }
    }

    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            return self.close(window, cx);
        }
        self.rebuild(cx);
        self.open = true;
        self.focus_return = window.focused(cx);
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.query.clear();
        self.refilter(cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        if let Some(focus) = self.focus_return.take() {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let model = state.sidebar.model.clone();
        let titles: HashMap<String, String> = model.project_names.iter().map(|(id, name)| (format!("{ENVIRONMENT}:{id}"), name.clone())).collect();
        let threads: Vec<PaletteThread> = model
            .threads
            .iter()
            .map(|thread| PaletteThread {
                environment_id: ENVIRONMENT.to_owned(),
                id: thread.id.clone(),
                project_id: thread.project_id.clone(),
                title: thread.title.clone(),
                branch: thread.branch.clone(),
                created_at: thread.created_at.map(format_iso).unwrap_or_default(),
                updated_at: thread.updated_at.map(format_iso).unwrap_or_default(),
                latest_user_message_at: thread.latest_user_message_at.map(format_iso),
                latest_user_authored_message_at: thread.latest_user_authored_message_at.map(|at| at.map(format_iso)),
                opens: thread.opens.clone(),
                ..Default::default()
            })
            .collect();
        let active = state.selected_thread.as_deref().map(|id| (ENVIRONMENT, id));
        let items = build_thread_action_items(&threads, active, &titles, |_| None);
        let active_value = active.map(|(env, id)| format!("thread:{env}:{id}"));
        let recent = recent_thread_items(&items, active_value.as_deref());
        let projects: Vec<CommandPaletteProject> = state
            .shell
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .projects
                    .iter()
                    .filter(|project| project.archived_at.is_none())
                    .map(|project| CommandPaletteProject {
                        environment_id: ENVIRONMENT.to_owned(),
                        id: project.id.clone(),
                        title: project.title.clone(),
                        display_name: model.project_names.get(&project.id).cloned().unwrap_or_else(|| project.title.clone()),
                        workspace_root: project.workspace_root.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.projects = build_project_action_items(&projects, "project", |_| Vec::new(), None);
        self.root = build_root_groups(action_items(state), recent);
        self.threads = items;
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.groups = filter_command_palette_groups(&CommandPaletteSearch {
            active_groups: &self.root,
            query: &self.query,
            is_in_submenu: false,
            all_options: None,
            project_search_items: &self.projects,
            settings_search_items: &[],
            thread_search_items: &self.threads,
            now: now_ms(),
        });
        let rows = build_command_palette_rows(&self.groups);
        self.highlighted = rows.item_values.first().map(|value| (*value).to_owned());
        cx.notify();
    }

    fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let rows = build_command_palette_rows(&self.groups);
        let count = rows.item_values.len() as isize;
        if count == 0 {
            return;
        }
        let current = self.highlighted.as_deref().and_then(|value| rows.item_values.iter().position(|v| *v == value)).map_or(-1, |p| p as isize);
        let next = if current < 0 { if step > 0 { 0 } else { count - 1 } } else { (current + step).rem_euclid(count) };
        self.highlighted = rows.item_values.get(next as usize).map(|value| (*value).to_owned());
        if let Some(row) = rows.row_index_by_item_index.get(next as usize) {
            self.scroll.scroll_to_item(*row);
        }
        cx.notify();
    }

    /// A jump key runs the row that carries its shortcut; otherwise the palette swallows it.
    fn jump(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(command) = t3_core::keybindings::thread_jump_command_for_index(index) else { return };
        let value = self.groups.iter().flat_map(|group| &group.items).find(|item| item.shortcut_command.as_deref() == Some(command)).map(|item| item.value.clone());
        if let Some(value) = value {
            self.activate(&value, window, cx);
        }
    }

    /// The pin key toggles the highlighted thread row's pin; anything else pins the open thread.
    fn pin_highlighted(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.highlighted.as_deref().and_then(|value| value.strip_prefix("thread:")).map(|rest| rest.split_once(':').map_or(rest, |(_, id)| id).to_owned()) else {
            return cx.propagate();
        };
        self.state.update(cx, |state, cx| match state.sidebar.model.thread(&thread_id).map(|thread| thread.pinned) {
            Some(true) => state.unpin_thread(&thread_id, cx),
            Some(false) => state.pin_thread(&thread_id, None, cx),
            None => {}
        });
    }

    fn activate_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(value) = self.highlighted.clone() {
            self.activate(&value, window, cx);
        }
    }

    fn activate(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close(window, cx);
        if let Some(command) = value.strip_prefix("action:") {
            let action: Box<dyn gpui::Action> = match command {
                "toggle-sidebar" => keymap::action_for("sidebar.toggle"),
                "pin" => keymap::action_for("thread.pin"),
                "settle" => keymap::action_for("thread.settle"),
                "copy-thread" => keymap::action_for("thread.copyReference"),
                "previous-thread" => keymap::action_for("thread.previous"),
                "next-thread" => keymap::action_for("thread.next"),
                _ => return,
            };
            window.dispatch_action(action, cx);
        } else if let Some(rest) = value.strip_prefix("thread:") {
            let thread_id = rest.split_once(':').map_or(rest, |(_, id)| id).to_owned();
            self.state.update(cx, |state, cx| state.select_thread(&thread_id, cx));
        } else if let Some(rest) = value.strip_prefix("project:") {
            let project_id = rest.split_once(':').map_or(rest, |(_, id)| id).to_owned();
            // No new-thread flow yet: a project opens its latest thread.
            let latest = self.state.read(cx).sidebar.model.threads.iter().filter(|t| t.project_id == project_id).max_by_key(|t| t.latest_user_message_at.or(t.updated_at)).map(|t| t.id.clone());
            if let Some(thread_id) = latest {
                self.state.update(cx, |state, cx| state.select_thread(&thread_id, cx));
            }
        }
    }

    fn render_row(&self, row: &CommandPaletteRow, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        match row {
            CommandPaletteRow::Label { label, first, .. } => div()
                .px(px(8.))
                .pt(px(if *first { 4. } else { 12. }))
                .pb(px(4.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.muted_foreground))
                .child(label.to_string())
                .into_any_element(),
            CommandPaletteRow::Item { item, .. } => {
                let highlighted = self.highlighted.as_deref() == Some(item.value.as_str());
                let value = item.value.clone();
                let hover_value = item.value.clone();
                let trailing = item
                    .shortcut_command
                    .as_deref()
                    .and_then(|command| keymap::label(command, cx))
                    .or_else(|| item.timestamp.as_deref().and_then(parse_iso).map(|at| compact_relative_label(at, now_ms())));
                div()
                    .id(SharedString::from(format!("palette-{}", item.value)))
                    .min_h(px(28.))
                    .px(px(8.))
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(4.))
                    .text_sm()
                    .cursor_pointer()
                    .when(highlighted, |this| this.bg(hsla(p.foreground).opacity(0.09)))
                    .child(div().flex_none().max_w(px(320.)).truncate().text_color(hsla(p.foreground)).child(item.title.clone()))
                    .children(item.description.clone().map(|text| div().flex_1().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(text)))
                    .child(div().flex_1())
                    .children(trailing.map(|text| div().flex_none().text_xs().text_color(hsla(p.muted_foreground)).child(text)))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.highlighted.as_deref() != Some(hover_value.as_str()) {
                            this.highlighted = Some(hover_value.clone());
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(&value, window, cx)))
                    .into_any_element()
            }
        }
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let p = palette(cx);
        let groups = std::mem::take(&mut self.groups);
        let rows = build_command_palette_rows(&groups);
        let rendered: Vec<_> = rows.rows.iter().map(|row| self.render_row(row, cx)).collect();
        let empty = rows.item_values.is_empty();
        drop(rows);
        self.groups = groups;
        let top = window.viewport_size().height * 0.1;
        deferred(
            div()
                .id("palette-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .justify_center()
                .pt(top)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close(window, cx)))
                .child(
                    div()
                        .id("command-palette")
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                            if !is_composing(&this.input, window, cx) {
                                this.move_highlight(1, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                            if !is_composing(&this.input, window, cx) {
                                this.move_highlight(-1, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                            if !is_composing(&this.input, window, cx) {
                                this.close(window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        // Inside the palette, jump keys and the pin key act on its rows (`CommandPalette.tsx`).
                        .on_action(cx.listener(|this, _: &JumpToThread1, window, cx| this.jump(0, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread2, window, cx| this.jump(1, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread3, window, cx| this.jump(2, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread4, window, cx| this.jump(3, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread5, window, cx| this.jump(4, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread6, window, cx| this.jump(5, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread7, window, cx| this.jump(6, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread8, window, cx| this.jump(7, window, cx)))
                        .on_action(cx.listener(|this, _: &JumpToThread9, window, cx| this.jump(8, window, cx)))
                        .on_action(cx.listener(|this, _: &PinThread, _, cx| this.pin_highlighted(cx)))
                        .on_action(cx.listener(|this, _: &CopyThreadReference, window, cx| {
                            this.close(window, cx);
                            cx.propagate();
                        }))
                        .w(px(576.))
                        .max_h(px(420.))
                        .flex()
                        .flex_col()
                        .relative()
                        .rounded(px(16.))
                        .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                        .shadow(vec![BoxShadow { color: gpui_hsla(0., 0., 0., 0.65), offset: point(px(0.), px(24.)), blur_radius: px(64.), spread_radius: px(-24.), inset: false }])
                        .child(
                            div()
                                .px(px(8.))
                                .py(px(6.))
                                .border_b_1()
                                .border_color(hsla(p.border))
                                .child(div().h(px(34.)).flex().items_center().child(Input::new(&self.input).appearance(false).bordered(false))),
                        )
                        .child(
                            div()
                                .id("palette-results")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                                .p(px(8.))
                                .when(empty, |this| this.child(div().py(px(24.)).flex().justify_center().text_sm().text_color(hsla(p.muted_foreground)).child("No results")))
                                .children(rendered),
                        ),
                ),
        )
        .with_priority(60)
        .into_any_element()
    }
}
