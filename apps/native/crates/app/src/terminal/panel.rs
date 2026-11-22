//! Right-panel terminal tab view (`mode="panel"` in `ThreadTerminalDrawer.tsx` +
//! `PersistentThreadTerminalPanel` in `PersistentThreadTerminals.tsx`): renders one
//! `SurfaceKind::Terminal` right-panel tab with horizontal/vertical splits up to
//! `MAX_TERMINALS_PER_GROUP`, floating actions when unsplit, a 144px session list when split,
//! close confirmation, and pending focus consumption.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, Focusable, Hsla, InteractiveElement, IntoElement, ParentElement,
    PromptLevel, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};
use t3_core::composer::context::TerminalSelection;
use t3_core::files::metadata::MetadataCache;
use t3_core::right_panel::SurfaceKind;
use t3_core::terminal::{self, MAX_TERMINALS_PER_GROUP, SplitDirection};

use super::drawer::{
    active_drawer, allocate_panel_terminal_id, can_operate_terminals, close_terminal_session, consume_terminal_focus,
    launch_panel_terminal, link_metadata_cache, record_terminal_focus, request_terminal_focus, terminal_label,
    terminal_location,
};
use super::pane::{Excerpt, PaneEvent, TerminalPane};
use super::session::Target;
use super::{CloseTerminal, NewTerminal, SplitTerminal, SplitTerminalVertical, with_shortcut};
use crate::app_state::{AppEvent, AppState, Phase};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const SIDEBAR_WIDTH: f32 = 144.;
const SIDEBAR_HEADER: f32 = 22.;
const ROW_HEIGHT: f32 = 24.;
const ACTION_ICON: f32 = 13.;

#[derive(Clone, Copy)]
enum PanelAction {
    SplitHorizontal,
    SplitVertical,
    New,
    Close,
}

impl PanelAction {
    const ALL: [PanelAction; 4] = [
        PanelAction::SplitHorizontal,
        PanelAction::SplitVertical,
        PanelAction::New,
        PanelAction::Close,
    ];

    fn icon(self) -> Icon {
        match self {
            PanelAction::SplitHorizontal => Icon::SquareSplitHorizontal,
            PanelAction::SplitVertical => Icon::SquareSplitVertical,
            PanelAction::New => Icon::Plus,
            PanelAction::Close => Icon::Trash2,
        }
    }

    fn command(self) -> &'static str {
        match self {
            PanelAction::SplitHorizontal => "terminal.split",
            PanelAction::SplitVertical => "terminal.splitVertical",
            PanelAction::New => "terminal.new",
            PanelAction::Close => "terminal.close",
        }
    }

    fn label(self, at_limit: bool, cx: &App) -> String {
        let name = match self {
            PanelAction::SplitHorizontal => "Split Terminal Horizontally",
            PanelAction::SplitVertical => "Split Terminal Vertically",
            PanelAction::New => "New Terminal",
            PanelAction::Close => "Close Terminal",
        };
        if self.at_limit(at_limit) {
            let translated = crate::i18n::t(cx, name);
            return format!("{translated} (max {MAX_TERMINALS_PER_GROUP} per group)");
        }
        with_shortcut(name, self.command(), cx)
    }

    fn at_limit(self, at_limit: bool) -> bool {
        at_limit && matches!(self, PanelAction::SplitHorizontal | PanelAction::SplitVertical)
    }
}

type TerminalGroupState = (Vec<String>, String, SplitDirection);

pub struct PanelTerminalView {
    app: Entity<AppState>,
    thread_id: String,
    surface_id: String,
    panes: HashMap<String, Entity<TerminalPane>>,
    visible: bool,
    last_group: Option<TerminalGroupState>,
    connection: Option<u64>,
    _subscriptions: Vec<Subscription>,
}

impl PanelTerminalView {
    pub fn new(
        app: Entity<AppState>,
        thread_id: String,
        surface_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![cx.subscribe_in(&app, window, |this, _, event: &AppEvent, window, cx| {
            if matches!(event, AppEvent::Phase | AppEvent::RightPanel | AppEvent::Selection | AppEvent::Shell) {
                this.sync(window, cx);
            }
        })];
        // Follow TerminalDrawer notifications so async `terminal:operate` grant gains and server
        // metadata label updates immediately re-sync panes and consume pending focus requests.
        if let Some(drawer) = active_drawer(cx) {
            subscriptions.push(cx.observe_in(&drawer, window, |this, _, window, cx| {
                this.sync(window, cx);
            }));
        }
        let mut view = PanelTerminalView {
            app,
            thread_id,
            surface_id,
            panes: HashMap::new(),
            visible: false,
            last_group: None,
            connection: None,
            _subscriptions: subscriptions,
        };
        view.sync(window, cx);
        view
    }

    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            self.sync(window, cx);
        }
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn pane_count_for_test(&self) -> usize {
        self.panes.len()
    }

    fn surface_state(&self, cx: &App) -> Option<TerminalGroupState> {
        let app = self.app.read(cx);
        let panel = app.panels.panels.panel(&self.thread_id);
        let surface = panel.surfaces.iter().find(|s| s.id == self.surface_id)?;
        match &surface.kind {
            SurfaceKind::Terminal { terminal_ids, active_terminal_id, split_direction, .. } => Some((
                terminal_ids.clone(),
                active_terminal_id.clone(),
                split_direction.unwrap_or(SplitDirection::Horizontal),
            )),
            _ => None,
        }
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = {
            let state = self.app.read(cx);
            if matches!(state.phase, Phase::Online(_)) { state.connection_id() } else { None }
        };
        if current != self.connection {
            self.panes.clear();
            self.last_group = None;
            self.connection = current;
        }
        if let Some(group) = self.surface_state(cx) {
            self.last_group = Some(group);
        } else if !self.visible {
            self.last_group = None;
        }
        let Some((terminal_ids, _, _)) = self.last_group.clone() else {
            self.panes.clear();
            return;
        };
        self.panes.retain(|id, _| terminal_ids.contains(id));
        for terminal_id in &terminal_ids {
            self.ensure_pane(terminal_id, window, cx);
        }
        let can_operate = can_operate_terminals(cx);
        let mut focus_target = None;
        for (terminal_id, pane) in &self.panes {
            let visible = self.visible && terminal_ids.contains(terminal_id);
            pane.update(cx, |pane, cx| pane.set_visible(visible, cx));
            if visible && consume_terminal_focus(&self.thread_id, terminal_id, true, can_operate, cx) {
                focus_target = Some(pane.clone());
            }
        }
        if let Some(pane) = focus_target {
            window.focus(&pane.focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn ensure_pane(&mut self, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TerminalPane>> {
        if let Some(pane) = self.panes.get(terminal_id) {
            return Some(pane.clone());
        }
        let location = terminal_location(self.app.read(cx), &self.thread_id, Some(terminal_id), cx)?;
        let client = self.app.read(cx).client_handle()?;
        let runtime = self.app.read(cx).runtime().clone();
        let target = Target { thread_id: self.thread_id.clone(), terminal_id: terminal_id.to_owned(), location };
        let app = self.app.clone();
        let link_metadata = link_metadata_cache(cx)
            .unwrap_or_else(|| Rc::new(RefCell::new((Instant::now(), MetadataCache::default()))));
        let pane = cx.new(|cx| {
            let mut pane = TerminalPane::new(target, client, runtime, window, cx);
            pane.set_link_context(app, link_metadata);
            pane
        });
        let terminal = terminal_id.to_owned();
        cx.subscribe_in(&pane, window, move |this, _, event: &PaneEvent, window, cx| match event {
            PaneEvent::Exited => this.close_terminal_confirmed(&terminal, window, cx),
            PaneEvent::Activated => {
                record_terminal_focus(&this.thread_id, &terminal, cx);
                this.activate_terminal(&terminal, window, cx);
            }
            PaneEvent::AddToChat(excerpt) => this.add_to_chat(&terminal, excerpt.clone(), cx),
        })
        .detach();
        self.panes.insert(terminal_id.to_owned(), pane.clone());
        Some(pane)
    }

    fn add_to_chat(&self, terminal_id: &str, excerpt: Excerpt, cx: &mut Context<Self>) {
        let selection = TerminalSelection {
            terminal_id: terminal_id.to_owned(),
            terminal_label: terminal_label(&self.thread_id, terminal_id, cx),
            line_start: excerpt.line_start,
            line_end: excerpt.line_end,
            text: excerpt.text,
        };
        let thread_id = self.thread_id.clone();
        self.app.update(cx, |state, cx| state.add_terminal_context(&thread_id, selection, cx));
    }

    fn activate_terminal(&mut self, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (thread_id, surface_id) = (self.thread_id.clone(), self.surface_id.clone());
        let tid = terminal_id.to_owned();
        self.app.update(cx, |state, cx| state.activate_panel_terminal(&thread_id, &surface_id, &tid, cx));
        request_terminal_focus(&self.thread_id, terminal_id, cx);
        self.sync(window, cx);
    }

    fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !can_operate_terminals(cx) {
            return;
        }
        let terminal_id = allocate_panel_terminal_id(self.app.read(cx), &self.thread_id, cx);
        if launch_panel_terminal(self.app.read(cx), &self.thread_id, &terminal_id, cx).is_none() {
            return;
        }
        let thread_id = self.thread_id.clone();
        self.app.update(cx, |state, cx| state.open_panel_terminal(&thread_id, &terminal_id, cx));
        request_terminal_focus(&self.thread_id, &terminal_id, cx);
        self.sync(window, cx);
    }

    fn split(&mut self, direction: SplitDirection, window: &mut Window, cx: &mut Context<Self>) {
        if !can_operate_terminals(cx) {
            return;
        }
        let Some((terminal_ids, _, _)) = self.surface_state(cx) else { return };
        if terminal_ids.len() >= MAX_TERMINALS_PER_GROUP {
            return;
        }
        let terminal_id = allocate_panel_terminal_id(self.app.read(cx), &self.thread_id, cx);
        if launch_panel_terminal(self.app.read(cx), &self.thread_id, &terminal_id, cx).is_none() {
            return;
        }
        let (thread_id, surface_id) = (self.thread_id.clone(), self.surface_id.clone());
        self.app.update(cx, |state, cx| {
            state.split_panel_terminal(&thread_id, &surface_id, &terminal_id, direction, cx);
        });
        request_terminal_focus(&self.thread_id, &terminal_id, cx);
        self.sync(window, cx);
    }

    fn request_close(&mut self, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !can_operate_terminals(cx) {
            return;
        }
        if !crate::settings::store::try_client(cx).is_none_or(|doc| doc.confirm_terminal_close()) {
            self.close_terminal_confirmed(terminal_id, window, cx);
            return;
        }
        let label = terminal_label(&self.thread_id, terminal_id, cx);
        let (message, detail) = terminal::close_confirmation(&[label]);
        let close_label = crate::i18n::t(cx, "Close");
        let cancel_label = crate::i18n::t(cx, "Cancel");
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &[close_label, cancel_label], cx);
        let terminal_id = terminal_id.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| this.close_terminal_confirmed(&terminal_id, window, cx));
            }
        })
        .detach();
    }

    fn close_terminal_confirmed(&mut self, terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        close_terminal_session(self.app.read(cx), &self.thread_id, terminal_id);
        self.panes.remove(terminal_id);
        let (thread_id, surface_id) = (self.thread_id.clone(), self.surface_id.clone());
        let tid = terminal_id.to_owned();
        self.app.update(cx, |state, cx| state.close_panel_terminal(&thread_id, &surface_id, &tid, cx));
        if let Some((_, active_terminal_id, _)) = self.surface_state(cx) {
            request_terminal_focus(&self.thread_id, &active_terminal_id, cx);
        }
        self.sync(window, cx);
        cx.notify();
    }

    fn run(&mut self, action: PanelAction, active_terminal_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            PanelAction::SplitHorizontal => self.split(SplitDirection::Horizontal, window, cx),
            PanelAction::SplitVertical => self.split(SplitDirection::Vertical, window, cx),
            PanelAction::New => self.new_terminal(window, cx),
            PanelAction::Close => self.request_close(active_terminal_id, window, cx),
        }
    }

    fn action_button(
        &self,
        action: PanelAction,
        active_terminal_id: String,
        at_limit: bool,
        hover: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let p = palette(cx);
        let label = SharedString::from(action.label(at_limit, cx));
        let disabled = !can_operate_terminals(cx) || action.at_limit(at_limit);
        div()
            .id(SharedString::from(format!("panel-terminal-action-{}-{}", self.surface_id, action as u8)))
            .h_full()
            .flex()
            .items_center()
            .px_1()
            .when(disabled, |this| this.opacity(0.64))
            .when(!disabled, |this| this.cursor_pointer().hover(move |style| style.bg(hover)))
            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                if !disabled {
                    this.run(action, &active_terminal_id, window, cx);
                }
            }))
            .child(action.icon().render(px(ACTION_ICON), hsla(p.foreground).opacity(0.9)))
    }

    fn render_floating_actions(&self, active_terminal_id: &str, at_limit: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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
        for (index, action) in PanelAction::ALL.into_iter().enumerate() {
            if index > 0 {
                group = group.child(div().h(px(16.)).w(px(1.)).bg(hsla(p.border).opacity(0.8)));
            }
            group = group.child(div().py_1().child(self.action_button(action, active_terminal_id.to_owned(), at_limit, hover, cx)));
        }
        div().absolute().top_2().right_2().child(group)
    }

    fn render_sidebar(
        &self,
        terminal_ids: &[String],
        active_terminal_id: &str,
        direction: SplitDirection,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let p = palette(cx);
        let line = hsla(p.border).opacity(0.7);
        let at_limit = terminal_ids.len() >= MAX_TERMINALS_PER_GROUP;
        let can_operate = can_operate_terminals(cx);
        let mut header = h_flex().h_full().items_stretch();
        for (index, action) in PanelAction::ALL.into_iter().enumerate() {
            header = header.child(
                div()
                    .h_full()
                    .when(index > 0, |this| this.border_l_1().border_color(line))
                    .child(self.action_button(action, active_terminal_id.to_owned(), at_limit, hsla(p.accent).opacity(0.7), cx)),
            );
        }
        let mut list = v_flex()
            .id(SharedString::from(format!("panel-terminal-list-{}", self.surface_id)))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_1()
            .py_1()
            .gap(px(2.));
        let (group_icon, label) = match direction {
            SplitDirection::Horizontal => (Icon::SquareSplitHorizontal, "Side by side"),
            SplitDirection::Vertical => (Icon::SquareSplitVertical, "Stacked"),
        };
        let count = crate::i18n::format_integer(cx, terminal_ids.len() as i64);
        let label = crate::i18n::t(cx, label);
        list = list.child(
            h_flex().flex_none().h(px(22.)).gap_1().px(px(6.)).text_size(px(11.))
                .text_color(hsla(p.muted_foreground))
                .child(group_icon.render(px(12.), hsla(p.muted_foreground)))
                .child(format!("{label} {count}")),
        );
        for terminal_id in terminal_ids {
            let active = terminal_id == active_terminal_id;
            let label = terminal_label(&self.thread_id, terminal_id, cx);
            let text = if active { hsla(p.foreground) } else { hsla(p.muted_foreground) };
            let group_name = SharedString::from(format!("panel-term-row-{}-{terminal_id}", self.surface_id));
            let (close_target, activate_target) = (terminal_id.clone(), terminal_id.clone());
            let close_label = SharedString::from(if active {
                with_shortcut(&format!("Close {label}"), "terminal.close", cx)
            } else {
                format!("Close {label}")
            });
            let icon = div().size(px(16.)).flex_none().flex().items_center().justify_center();
            let close = if !can_operate {
                icon.child(Icon::SquareTerminal.render(px(12.), text)).into_any_element()
            } else {
                icon
                    .id(SharedString::from(format!("panel-term-close-{}-{terminal_id}", self.surface_id)))
                    .relative()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hsla(p.muted)))
                    .tooltip(move |window, cx| Tooltip::new(close_label.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| this.request_close(&close_target, window, cx)))
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
            list = list.child(
                div()
                    .id(SharedString::from(format!("panel-term-tab-{}-{terminal_id}", self.surface_id)))
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
                            .id(SharedString::from(format!("panel-term-label-{}-{terminal_id}", self.surface_id)))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| this.activate_terminal(&activate_target, window, cx)))
                            .child(label),
                    ),
            );
        }
        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .min_w(px(SIDEBAR_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(line)
            .bg(hsla(p.muted).opacity(0.1))
            .child(h_flex().h(px(SIDEBAR_HEADER)).flex_none().justify_end().border_b_1().border_color(line).child(header))
            .child(list)
    }

    fn render_panes(
        &mut self,
        terminal_ids: &[String],
        active_terminal_id: &str,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let vertical = direction == SplitDirection::Vertical;
        let mut panes = if vertical { v_flex() } else { h_flex() }.size_full().min_w_0().min_h_0().overflow_hidden();
        for (index, terminal_id) in terminal_ids.iter().enumerate() {
            let Some(pane) = self.ensure_pane(terminal_id, window, cx) else { continue };
            let line = if terminal_id == active_terminal_id { hsla(p.border) } else { hsla(p.border).opacity(0.7) };
            panes = panes.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
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

impl Render for PanelTerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let Some((terminal_ids, active_terminal_id, direction)) = self.last_group.clone() else {
            return div().size_full().bg(hsla(p.terminal_background)).into_any_element();
        };
        let sidebar = terminal_ids.len() > 1;
        let at_limit = terminal_ids.len() >= MAX_TERMINALS_PER_GROUP;
        let floating = (!sidebar).then(|| self.render_floating_actions(&active_terminal_id, at_limit, cx));
        let active_for_close = active_terminal_id.clone();
        h_flex()
            .id(SharedString::from(format!("panel-terminal-{}", self.surface_id)))
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(hsla(p.terminal_background))
            .on_action(cx.listener(|this, _: &NewTerminal, window, cx| this.new_terminal(window, cx)))
            .on_action(cx.listener(|this, _: &SplitTerminal, window, cx| this.split(SplitDirection::Horizontal, window, cx)))
            .on_action(cx.listener(|this, _: &SplitTerminalVertical, window, cx| this.split(SplitDirection::Vertical, window, cx)))
            .on_action(cx.listener(move |this, _: &CloseTerminal, window, cx| this.request_close(&active_for_close, window, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_panes(&terminal_ids, &active_terminal_id, direction, window, cx)),
            )
            .when(sidebar, |this| {
                this.gap(px(6.)).child(self.render_sidebar(&terminal_ids, &active_terminal_id, direction, cx))
            })
            .children(floating)
            .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use serde_json::json;

    struct Host {
        panel: Entity<PanelTerminalView>,
        _drawer: Entity<super::super::TerminalDrawer>,
    }
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(self.panel.clone())
        }
    }

    #[gpui::test]
    async fn closing_split_panel_terminal_focuses_surviving_pane(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) =
            cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "orchestration:operate", "terminal:operate"]
                }))
                .unwrap(),
            ))
            .unwrap();
        let surface_id = state.update(cx, |state, cx| {
            state.open_panel_terminal("menu-thread", "term-1", cx);
            let surface_id = state.panels.panels.active_surface("menu-thread").unwrap().id.clone();
            state.split_panel_terminal("menu-thread", &surface_id, "term-2", SplitDirection::Horizontal, cx);
            surface_id
        });
        let window = cx.add_window(|window, cx| {
            // Keep ActiveDrawer alive so terminal focus queue & operate permissions are active.
            let drawer = cx.new(|cx| super::super::TerminalDrawer::new(state.clone(), window, cx));
            let panel = cx.new(|cx| {
                let mut view = PanelTerminalView::new(state.clone(), "menu-thread".into(), surface_id.clone(), window, cx);
                view.set_visible(true, window, cx);
                view
            });
            Host { panel, _drawer: drawer }
        });
        cx.run_until_parked();

        window
            .update(cx, |host, window, cx| {
                host.panel.update(cx, |panel, cx| {
                    // Focus term-2 first, then close term-2; surviving term-1 must receive window focus.
                    panel.activate_terminal("term-2", window, cx);
                    assert!(panel.panes.get("term-2").unwrap().focus_handle(cx).is_focused(window));
                    panel.close_terminal_confirmed("term-2", window, cx);
                    assert!(!panel.panes.contains_key("term-2"));
                    assert!(
                        panel.panes.get("term-1").unwrap().focus_handle(cx).is_focused(window),
                        "closing split panel terminal must focus surviving active terminal pane"
                    );
                });
            })
            .unwrap();

        state.update(cx, |state, _| state.stop_pump());
        runtime.shutdown_background();
    }
}
