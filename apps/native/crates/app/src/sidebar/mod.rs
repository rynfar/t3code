//! Sidebar pane: header with thread search, the sectioned thread list, and the Undo notice
//! (`apps/web/src/components/Sidebar.tsx`, `sidebar/SidebarThreadHeader.tsx`,
//! `sidebar/SidebarThreadUndoNotice.tsx`). Rows have three heights (card, slim, shelf), so the
//! list is a variable-height `list` that splices only the rows that changed. The pane is
//! rendered `.cached()` and notifies itself only on shell, selection and notice changes.

pub mod controller;
mod project_filter;
mod row;
mod search;

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Context, Entity, FontWeight, IntoElement, ListAlignment, ListState, ParentElement, Pixels, Point, Render, Styled, Subscription,
    Task, Window, div, list, prelude::*, px,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use t3_core::keybindings::{ShortcutMatchContext, ShortcutModifierState, should_show_thread_jump_hints_for_modifiers};
use t3_core::sidebar::RowKey;
use t3_core::sidebar::time::{MINUTE_MS, Ms};

use crate::app_state::{AppEvent, AppState};
use crate::context_menu::{ContextMenu, ContextMenuEvent};
use crate::dialog::ConfirmDialog;
use crate::snooze_dialog::SnoozeDialog;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::workspace::TITLEBAR_HEIGHT;
use controller::now_ms;
use search::SearchState;

const JUMP_HINT_SHOW_DELAY_MS: u64 = 200;

/// Whether an IME composition is open in `input`; Escape and arrows then belong to the IME.
pub(crate) fn is_composing(input: &Entity<InputState>, window: &mut Window, cx: &mut App) -> bool {
    input.update(cx, |input, cx| gpui::EntityInputHandler::marked_text_range(input, window, cx).is_some())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClockRate {
    Second,
    Minute,
}

pub struct SidebarPane {
    state: Entity<AppState>,
    menu: Entity<ContextMenu>,
    list: ListState,
    row_keys: Vec<RowKey>,
    search_input: Entity<InputState>,
    search: SearchState,
    rename: Option<(String, Entity<InputState>)>,
    /// Where the project filter popover opens, while it is open.
    project_filter: Option<gpui::Point<gpui::Pixels>>,
    jump_hints: bool,
    jump_hint_timer: Option<Task<()>>,
    now: Ms,
    model_seen: Arc<t3_core::sidebar::SidebarModel>,
    /// Whether the last frame drew a ticking working duration; only then does the clock run per second.
    live_rendered: bool,
    clock: Option<(ClockRate, Task<()>)>,
    _subscriptions: Vec<Subscription>,
}

impl SidebarPane {
    pub fn new(
        state: Entity<AppState>,
        menu: Entity<ContextMenu>,
        dialog: Entity<ConfirmDialog>,
        snooze: Entity<SnoozeDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subscriptions = vec![
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
                AppEvent::Shell | AppEvent::Selection => {
                    let model = this.state.read(cx).sidebar.model.clone();
                    // Streaming ticks reach the shell but leave the model alone; nothing to redraw.
                    if matches!(event, AppEvent::Shell) && Arc::ptr_eq(&model, &this.model_seen) {
                        return;
                    }
                    this.model_seen = model;
                    this.sync_rows(cx);
                    this.search.refresh(&this.state, cx);
                    cx.notify();
                }
                AppEvent::Notice => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&search_input, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let query = input.read(cx).value().to_string();
                    this.search.set_query(query, &this.state, cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.open_search_result(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&menu, window, move |this, _, event: &ContextMenuEvent, window, cx| match event {
                ContextMenuEvent::Rename(thread_id) => this.start_rename(thread_id, window, cx),
                ContextMenuEvent::CustomSnooze(thread_id) => snooze.update(cx, |snooze, cx| snooze.open(thread_id, window, cx)),
                ContextMenuEvent::Delete(thread_id) => {
                    let Some(title) = this.state.read(cx).sidebar.model.thread(thread_id).map(|t| t.title.clone()) else { return };
                    let (heading, description) = t3_core::thread_actions::delete_confirmation(&title);
                    let id = thread_id.clone();
                    dialog.update(cx, |dialog, cx| dialog.confirm(heading, description, "Delete", move |state, cx| state.delete_thread(&id, cx), window, cx));
                }
            }),
        ];
        let mut pane = SidebarPane {
            state,
            menu,
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            row_keys: Vec::new(),
            search_input,
            search: SearchState::default(),
            rename: None,
            project_filter: None,
            jump_hints: false,
            jump_hint_timer: None,
            now: now_ms(),
            model_seen: Default::default(),
            live_rendered: false,
            clock: None,
            _subscriptions: subscriptions,
        };
        pane.sync_rows(cx);
        pane
    }

    /// Splices the list so scroll position and measured heights survive streaming updates.
    fn sync_rows(&mut self, cx: &mut Context<Self>) {
        let keys = self.state.read(cx).sidebar.model.row_keys();
        if let Some((range, count)) = t3_core::sidebar::splice_range(&self.row_keys, &keys) {
            self.list.splice(range, count);
        }
        self.row_keys = keys;
        self.schedule_clock(cx);
    }

    /// Relative times tick each minute; working durations each second, but only while one was
    /// drawn in the last frame and the sidebar shows. GPUI redraws a notified view's ancestors
    /// either way, so the visibility gate, not a per-label view, is what bounds the work.
    fn schedule_clock(&mut self, cx: &mut Context<Self>) {
        let rate = if self.live_rendered { ClockRate::Second } else { ClockRate::Minute };
        self.schedule_clock_at(rate, cx);
    }

    fn schedule_clock_at(&mut self, rate: ClockRate, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        if state.sidebar.collapsed || state.sidebar.model.rows.is_empty() {
            self.clock = None;
            return;
        }
        let now = now_ms();
        let period = if rate == ClockRate::Second { 1_000 } else { MINUTE_MS };
        let wait = period - now.rem_euclid(period);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(wait as u64 + 5)).await;
            let _ = this.update(cx, |this, cx| {
                this.now = now_ms();
                this.schedule_clock(cx);
                cx.notify();
            });
        });
        self.clock = Some((rate, task));
    }

    /// Called while drawing a live duration row: switches a minute clock to seconds.
    fn note_live_row(&mut self, cx: &mut Context<Self>) {
        self.live_rendered = true;
        if !matches!(self.clock, Some((ClockRate::Second, _))) {
            self.schedule_clock_at(ClockRate::Second, cx);
        }
    }

    /// Jump hints appear once the jump modifier has been held for a moment, so a quick shortcut
    /// never flashes them (`useThreadJumpHintsVisible`), and hide when it is released.
    pub fn set_modifiers(&mut self, modifiers: gpui::Modifiers, cx: &mut Context<Self>) {
        let mac = cfg!(target_os = "macos");
        let state = ShortcutModifierState { meta_key: modifiers.platform, ctrl_key: modifiers.control, shift_key: modifiers.shift, alt_key: modifiers.alt };
        let rules = cx.try_global::<crate::keymap::Keymap>().map(|keymap| keymap.rules.clone()).unwrap_or_default();
        let show = should_show_thread_jump_hints_for_modifiers(&state, &rules, &ShortcutMatchContext::default(), mac);
        if !show {
            self.jump_hint_timer = None;
            if std::mem::take(&mut self.jump_hints) {
                cx.notify();
            }
            return;
        }
        if self.jump_hints || self.jump_hint_timer.is_some() {
            return;
        }
        self.jump_hint_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(JUMP_HINT_SHOW_DELAY_MS)).await;
            let _ = this.update(cx, |this, cx| {
                this.jump_hint_timer = None;
                this.jump_hints = true;
                cx.notify();
            });
        }));
    }

    fn start_rename(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.state.read(cx).sidebar.model.thread(thread_id).map(|t| t.title.clone()) else { return };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let id = thread_id.to_owned();
        let subscription = cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                let title = input.read(cx).value().to_string();
                if this.rename.as_ref().is_some_and(|(renaming, _)| *renaming == id) {
                    this.rename = None;
                    this.state.update(cx, |state, cx| state.rename_thread(&id, &title, cx));
                    cx.notify();
                }
            }
            _ => {}
        });
        self._subscriptions.push(subscription);
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.rename = Some((thread_id.to_owned(), input));
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            cx.notify();
        }
    }

    pub fn open_menu(&mut self, thread_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.menu.update(cx, |menu, cx| menu.open_for_thread(thread_id, position, window, cx));
    }

    pub fn open_snooze_menu(&mut self, thread_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.menu.update(cx, |menu, cx| menu.open_snooze(thread_id, position, window, cx));
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.search.set_query(String::new(), &self.state, cx);
        cx.notify();
    }

    fn open_search_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.search.highlighted_id().map(str::to_owned) else { return };
        self.clear_search(window, cx);
        self.state.update(cx, |state, cx| state.select_thread(&thread_id, cx));
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .pl(px(130.))
            .pr(px(12.))
            .gap(px(8.))
            .window_control_area(gpui::WindowControlArea::Drag)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(4.))
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(div().text_color(hsla(p.sidebar_foreground)).font_weight(FontWeight::SEMIBOLD).child("T3"))
                    .child(div().text_color(hsla(p.muted_foreground)).child("Code")),
            )
    }

    fn render_search(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let searching = self.search.is_active();
        div().px(px(8.)).pb(px(4.)).flex().items_center().gap(px(4.)).child(
            div()
                .id("sidebar-search")
                .flex_1()
                .min_w_0()
                .h(px(32.))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .rounded(px(6.))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.sidebar_muted_foreground))
                .hover(|this| this.bg(hsla(p.sidebar_row_hover)))
                .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                    if !is_composing(&this.search_input, window, cx) {
                        this.search.move_highlight(1);
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                    if !is_composing(&this.search_input, window, cx) {
                        this.search.move_highlight(-1);
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                    if !is_composing(&this.search_input, window, cx) {
                        this.clear_search(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(Icon::Search.sized(px(16.)).text_color(hsla(p.sidebar_icon)))
                .child(div().flex_1().min_w_0().child(Input::new(&self.search_input).appearance(false).bordered(false)))
                .when(searching, |this| {
                    this.child(
                        div()
                            .id("sidebar-search-clear")
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|this| this.bg(hsla(p.sidebar_row_hover)))
                            .child(Icon::X.sized(px(12.)).text_color(hsla(p.sidebar_muted_foreground)))
                            .on_click(cx.listener(|this, _, window, cx| this.clear_search(window, cx))),
                    )
                }),
        )
        .child(self.render_project_filter_button(cx))
        .child(
            div()
                .id("add-project")
                .size(px(28.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|this| this.bg(hsla(p.sidebar_row_hover)))
                .child(Icon::FolderPlus.render(px(16.), hsla(p.sidebar_icon)))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::palette::OpenAddProject), cx)),
        )
    }

    /// The empty list copy, naming the filtered projects when a filter applies.
    fn empty_message(&self, synchronized: bool, cx: &Context<Self>) -> String {
        let state = self.state.read(cx);
        if !synchronized {
            return "Loading…".to_owned();
        }
        if state.shell.snapshot.as_ref().is_some_and(|snapshot| snapshot.projects.is_empty()) {
            return "No projects yet".to_owned();
        }
        let Some(scope) = &state.sidebar.project_scope else { return "No threads yet".to_owned() };
        let names: std::collections::BTreeSet<&String> = scope.iter().filter_map(|id| state.sidebar.model.project_names.get(id)).collect();
        match names.iter().next() {
            Some(name) if names.len() == 1 => format!("No threads in {name} yet"),
            _ => "No threads in these projects yet".to_owned(),
        }
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let notice = self.state.read(cx).sidebar.undo.notice()?;
        let p = palette(cx);
        let label = crate::keymap::label("thread.undo", cx).map_or_else(|| "Undo".to_owned(), |shortcut| format!("{shortcut} to undo"));
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(24.))
                .mx(px(8.))
                .mb(px(8.))
                .px(px(4.))
                .text_size(px(11.))
                .text_color(hsla(p.sidebar_muted_foreground))
                .child(format!("{},", notice.summary()))
                .child(
                    div()
                        .id("sidebar-undo")
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .cursor_pointer()
                        .hover(|this| this.text_color(hsla(p.sidebar_foreground)))
                        .child(Icon::Undo2.sized(px(12.)))
                        .child(label)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |state, cx| state.undo_latest(cx));
                        })),
                ),
        )
    }
}

impl Render for SidebarPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.state.read(cx).sidebar.model.clone();
        let synchronized = self.state.read(cx).shell.synchronized;
        self.live_rendered = false;
        let p = palette(cx);
        let body = if self.search.is_active() {
            self.render_search_results(cx).into_any_element()
        } else if model.rows.is_empty() {
            div()
                .px(px(8.))
                .py(px(24.))
                .flex()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground).opacity(0.6))
                .child(self.empty_message(synchronized, cx))
                .into_any_element()
        } else {
            list(self.list.clone(), cx.processor(|this, index: usize, window, cx| this.render_row(index, window, cx))).flex_1().into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(p.sidebar))
            .child(self.render_header(cx))
            .child(self.render_search(cx))
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .children(self.render_footer(cx))
            .children(self.render_project_filter(cx))
    }
}
