//! The thread context menu (design-system §6.3), anchored at the pointer. Items come from
//! `t3_core::thread_actions::build_menu`; actions the app can run go to `AppState`, the rest
//! (rename, delete) are emitted for the sidebar and workspace.

use gpui::{
    AnyElement, App, BoxShadow, Context, MouseButton, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent,
    ParentElement, Pixels, Point, Render, SharedString, Styled, Window, anchored, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use t3_core::sidebar::settled::{self, SnoozePreset};
use t3_core::thread_actions::{Available, MenuItem, ProjectAction, Supports, ThreadAction, ThreadActionMenuState, build_menu, build_project_menu};

use crate::app_state::AppState;
use crate::icons::Icon;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

const MENU_WIDTH: f32 = 220.;
const ITEM_HEIGHT: f32 = 28.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    Thread(ThreadAction),
    Project(ProjectAction),
}

pub enum ContextMenuEvent {
    Rename(String),
    Delete(String),
    CustomSnooze(String),
}

struct OpenMenu {
    /// The thread or project the menu acts on.
    target_id: String,
    items: Vec<MenuItem<MenuAction>>,
    position: Point<Pixels>,
    highlighted: Option<usize>,
    submenu: Option<usize>,
    sub_highlighted: Option<usize>,
    presets: Vec<SnoozePreset>,
}

pub struct ContextMenu {
    state: Entity<AppState>,
    open: Option<OpenMenu>,
    focus: FocusHandle,
}

impl EventEmitter<ContextMenuEvent> for ContextMenu {}
impl EventEmitter<DismissEvent> for ContextMenu {}

impl Focusable for ContextMenu {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ContextMenu {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        ContextMenu { state, open: None, focus: cx.focus_handle() }
    }

    pub fn open_for_thread(&mut self, thread_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(thread) = state.sidebar.model.thread(thread_id) else { return };
        let now = now_ms();
        let presets = state.snooze_presets();
        let caps = state.sidebar.capabilities;
        let loaded = caps.config_loaded;
        let supports = Supports { settlement: loaded, auto_settle_opt_out: caps.auto_settle_opt_out, snooze: loaded, pinning: loaded, title_regeneration: loaded };
        let items = build_menu(&ThreadActionMenuState {
            can_operate: caps.can_operate,
            can_find: false,
            branch: thread.branch.as_deref(),
            project_filter: None,
            is_pinned: thread.pinned,
            is_settled: thread.is_settled(),
            auto_settle_enabled: !thread.auto_settle_disabled,
            is_snoozed: settled::effective_snoozed(thread, now),
            can_snooze_now: settled::can_snooze(thread, now),
            is_regenerating_title: thread.regenerating_title,
            is_running: !thread.can_archive(),
            supports,
            snooze_presets: &presets,
            available: Available { new_thread_on_branch: false, custom_snooze: true, project_settings: false },
        });
        let items = items.into_iter().map(|item| item.map(&MenuAction::Thread)).collect();
        self.open = Some(OpenMenu { target_id: thread_id.to_owned(), items, position, highlighted: None, submenu: None, sub_highlighted: None, presets });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The project menu, from a project row in the project filter.
    pub fn open_for_project(&mut self, project_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(project) = state.shell.project(project_id) else { return };
        let items = build_project_menu(project.pinned_at.is_some(), state.can_pin_projects()).into_iter().map(|item| item.map(&MenuAction::Project)).collect();
        self.open = Some(OpenMenu { target_id: project_id.to_owned(), items, position, highlighted: None, submenu: None, sub_highlighted: None, presets: Vec::new() });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The row's Snooze button: just the presets (`SnoozeMenuButton`).
    pub fn open_snooze(&mut self, thread_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let presets = self.state.read(cx).snooze_presets();
        let items = presets
            .iter()
            .map(|preset| MenuItem {
                action: MenuAction::Thread(ThreadAction::SnoozePreset(preset.id.clone())),
                label: format!("{} ({})", preset.label, preset.when_label),
                icon: None,
                disabled: false,
                destructive: false,
                separator_before: false,
                checked: None,
                children: Vec::new(),
            })
            .collect();
        self.open = Some(OpenMenu { target_id: thread_id.to_owned(), items, position, highlighted: None, submenu: None, sub_highlighted: None, presets });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            cx.emit(DismissEvent);
            cx.notify();
        }
    }

    fn activate(&mut self, action: MenuAction, cx: &mut Context<Self>) {
        let Some(open) = self.open.take() else { return };
        cx.emit(DismissEvent);
        cx.notify();
        match action {
            MenuAction::Thread(ThreadAction::Rename) => cx.emit(ContextMenuEvent::Rename(open.target_id)),
            MenuAction::Thread(ThreadAction::Delete) => cx.emit(ContextMenuEvent::Delete(open.target_id)),
            MenuAction::Thread(ThreadAction::SnoozeCustom) => cx.emit(ContextMenuEvent::CustomSnooze(open.target_id)),
            MenuAction::Thread(action) => self.state.update(cx, |state, cx| state.run_thread_action(&open.target_id, action, &open.presets, cx)),
            MenuAction::Project(action) => self.state.update(cx, |state, cx| match action {
                ProjectAction::NewThread => state.start_draft(&open.target_id, cx),
                ProjectAction::CopyPath => state.copy_project_path(&open.target_id, cx),
                ProjectAction::Pin => state.set_project_pinned(&open.target_id, true, cx),
                ProjectAction::Unpin => state.set_project_pinned(&open.target_id, false, cx),
            }),
        }
    }

    fn enabled(items: &[MenuItem<MenuAction>]) -> Vec<usize> {
        items.iter().enumerate().filter(|(_, item)| !item.disabled).map(|(index, _)| index).collect()
    }

    fn step(current: Option<usize>, enabled: &[usize], down: bool) -> Option<usize> {
        let position = current.and_then(|current| enabled.iter().position(|index| *index == current));
        let next = match (position, down) {
            (None, true) => 0,
            (None, false) => enabled.len().checked_sub(1)?,
            (Some(p), true) => (p + 1) % enabled.len(),
            (Some(p), false) => (p + enabled.len() - 1) % enabled.len(),
        };
        enabled.get(next).copied()
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(open) = self.open.as_mut() else { return };
        let key = event.keystroke.key.as_str();
        let in_submenu = open.submenu.is_some();
        match key {
            "escape" if in_submenu => open.submenu = None,
            "escape" => return self.close(cx),
            "down" | "up" => {
                if let Some(parent) = open.submenu {
                    let enabled = Self::enabled(&open.items[parent].children);
                    open.sub_highlighted = Self::step(open.sub_highlighted, &enabled, key == "down");
                } else {
                    let enabled = Self::enabled(&open.items);
                    open.highlighted = Self::step(open.highlighted, &enabled, key == "down");
                }
            }
            "right" => {
                if let Some(index) = open.highlighted.filter(|index| !open.items[*index].children.is_empty()) {
                    open.submenu = Some(index);
                    open.sub_highlighted = Self::enabled(&open.items[index].children).first().copied();
                }
            }
            "left" => open.submenu = None,
            "enter" | "space" => {
                let action = match (open.submenu, open.sub_highlighted, open.highlighted) {
                    (Some(parent), Some(child), _) => Some(open.items[parent].children[child].action.clone()),
                    (None, _, Some(index)) if open.items[index].children.is_empty() => Some(open.items[index].action.clone()),
                    (None, _, Some(index)) => {
                        open.submenu = Some(index);
                        open.sub_highlighted = Self::enabled(&open.items[index].children).first().copied();
                        None
                    }
                    _ => None,
                };
                if let Some(action) = action {
                    return self.activate(action, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn render_item(&self, item: &MenuItem<MenuAction>, index: usize, highlighted: bool, submenu_parent: Option<usize>, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let text = if item.destructive { p.destructive_foreground } else if highlighted { p.accent_foreground } else { p.popover_foreground };
        let icon_color = if item.destructive { text } else { p.muted_foreground };
        let has_children = !item.children.is_empty();
        let action = item.action.clone();
        let disabled = item.disabled;
        let id = SharedString::from(format!("menu-item-{}-{index}", submenu_parent.map_or(-1, |p| p as i64)));
        div()
            .id(id)
            .flex()
            .items_center()
            .min_h(px(ITEM_HEIGHT))
            .px(px(8.))
            .py(px(4.))
            .gap(px(8.))
            .rounded(px(4.))
            .text_sm()
            .text_color(hsla(text))
            .when(highlighted, |this| this.bg(hsla(p.accent)))
            .when(disabled, |this| this.opacity(0.64))
            .when(item.checked.is_some(), |this| {
                this.child(div().w(px(16.)).flex_none().children(item.checked.filter(|c| *c).map(|_| Icon::Check.sized(px(14.)).text_color(hsla(text)))))
            })
            .children(item.icon.and_then(Icon::from_name).map(|icon| div().opacity(0.8).child(icon.sized(px(16.)).text_color(hsla(icon_color)))))
            .child(div().flex_1().min_w_0().truncate().child(item.label.clone()))
            .when(has_children, |this| this.child(Icon::ChevronRight.sized(px(14.)).text_color(hsla(p.muted_foreground))))
            .when(!disabled, |this| {
                this.on_mouse_move(cx.listener(move |this, _, _, cx| {
                    let Some(open) = this.open.as_mut() else { return };
                    match submenu_parent {
                        Some(_) if open.sub_highlighted != Some(index) => open.sub_highlighted = Some(index),
                        None if open.highlighted != Some(index) || (has_children && open.submenu != Some(index)) => {
                            open.highlighted = Some(index);
                            open.submenu = has_children.then_some(index);
                            open.sub_highlighted = None;
                        }
                        _ => return,
                    }
                    cx.notify();
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !has_children {
                        this.activate(action.clone(), cx);
                    }
                }))
            })
            .into_any_element()
    }

    fn panel(cx: &App) -> gpui::Div {
        let p = palette(cx);
        div()
            .relative()
            .w(px(MENU_WIDTH))
            .p(px(5.))
            .rounded(px(8.))
            .child(crate::glass::backdrop(8., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
            .shadow(vec![BoxShadow {
                color: gpui_hsla(0., 0., 0., 0.55),
                offset: point(px(0.), px(16.)),
                blur_radius: px(40.),
                spread_radius: px(-18.),
                inset: false,
            }])
    }

    fn render_list(&self, items: &[MenuItem<MenuAction>], highlighted: Option<usize>, parent: Option<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = palette(cx);
        let mut children = Vec::new();
        for (index, item) in items.iter().enumerate() {
            if item.separator_before && index > 0 {
                children.push(div().h(px(1.)).my(px(4.)).mx(px(-4.)).bg(hsla(p.border)).into_any_element());
            }
            children.push(self.render_item(item, index, highlighted == Some(index), parent, cx));
        }
        children
    }
}

/// The y offset of item `index` inside a panel, counting separators.
fn item_offset(items: &[MenuItem<MenuAction>], index: usize) -> f32 {
    let separators = items.iter().take(index + 1).enumerate().filter(|(i, item)| item.separator_before && *i > 0).count();
    4. + index as f32 * ITEM_HEIGHT + separators as f32 * 9.
}

impl Render for ContextMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(open) = self.open.as_ref() else { return div().into_any_element() };
        let main = self.render_list(&open.items, open.highlighted, None, cx);
        let submenu = open.submenu.map(|parent| {
            let items = &open.items[parent].children;
            let list = self.render_list(items, open.sub_highlighted, Some(parent), cx);
            Self::panel(cx).id("thread-context-submenu").occlude().absolute().left(px(MENU_WIDTH - 4.)).top(px(item_offset(&open.items, parent) - 5.)).children(list)
        });
        let position = open.position;
        div()
            .id("thread-context-menu-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close(cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, _, _, cx| this.close(cx)))
            .child(
                deferred(
                    anchored().position(position).snap_to_window_with_margin(px(8.)).child(
                        Self::panel(cx)
                            .id("thread-context-menu")
                            .relative()
                            .occlude()
                            .track_focus(&self.focus)
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .font_weight(FontWeight::NORMAL)
                            .children(main)
                            .children(submenu),
                    ),
                )
                .with_priority(130),
            )
            .into_any_element()
    }
}
