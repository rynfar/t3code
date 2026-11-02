//! Sidebar pane: header with thread search, the sectioned thread list, and the Undo notice
//! (`apps/web/src/components/Sidebar.tsx`, `sidebar/SidebarThreadHeader.tsx`,
//! `sidebar/SidebarThreadUndoNotice.tsx`). Rows have three heights (card, slim, shelf), so the
//! list is a variable-height `list` that splices only the rows that changed. The pane is
//! rendered `.cached()` and notifies itself only on shell, selection and notice changes.

mod bulk;
pub mod controller;
mod drag;
mod favicons;
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

use t3_core::settings::search::Section;

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
    drag: Option<drag::PaneDrag>,
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
                AppEvent::SidebarSelection => {
                    this.model_seen = this.state.read(cx).sidebar.model.clone();
                    this.sync_rows(cx);
                    cx.notify();
                }
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
                ContextMenuEvent::CustomSnooze(thread_ids) => snooze.update(cx, |snooze, cx| snooze.open(thread_ids.clone(), window, cx)),
                ContextMenuEvent::BulkDelete(thread_ids) => {
                    if !crate::settings::client_settings(cx).confirm_thread_delete() {
                        return this.state.update(cx, |state, cx| state.delete_threads(thread_ids.clone(), cx));
                    }
                    let (heading, description) = t3_core::sidebar::selection::bulk_delete_confirmation(thread_ids.len());
                    let ids = thread_ids.clone();
                    dialog.update(cx, |dialog, cx| dialog.confirm(heading, description, "Delete", move |state, cx| state.delete_threads(ids, cx), window, cx));
                }
                ContextMenuEvent::Delete(thread_id) => {
                    if !crate::settings::client_settings(cx).confirm_thread_delete() {
                        return this.state.update(cx, |state, cx| state.delete_thread(thread_id, cx));
                    }
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
            drag: None,
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

    /// A selected row opens the bulk menu (`handleThreadContextMenu`).
    pub fn open_menu(&mut self, thread_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).sidebar.selection.contains(thread_id) {
            self.menu.update(cx, |menu, cx| menu.open_for_selection(position, window, cx));
        } else {
            self.menu.update(cx, |menu, cx| menu.open_for_thread(thread_id, position, window, cx));
        }
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

    /// `SidebarChromeHeader`: the brand (a link home) and the history buttons, in the 52px drag
    /// strip after the window controls and the sidebar toggle.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let state = self.state.clone();
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
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .id("sidebar-brand")
                            .h(px(28.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap(px(4.))
                                    .child(div().text_color(hsla(p.foreground)).font_weight(FontWeight::SEMIBOLD).child("T3"))
                                    .child(div().text_color(hsla(p.muted_foreground)).child("Code")),
                            )
                            .on_click(move |_, _, cx| state.update(cx, |state, cx| state.go_home(cx))),
                    )
                    .child(div().flex().items_center().child(self.render_history_button(true, cx)).child(self.render_history_button(false, cx))),
            )
    }

    /// `SidebarHeaderIconButton`: a 28px `SidebarMenuButton` with a 16px icon and a tooltip above.
    /// The tooltip text is read when it opens, so a rebound shortcut shows without a re-render.
    fn header_button(
        id: &'static str,
        content: impl IntoElement,
        tooltip: impl Fn(&App) -> String + 'static,
        enabled: bool,
        cx: &App,
    ) -> gpui::Stateful<gpui::Div> {
        let p = palette(cx);
        div()
            .id(id)
            .group(id)
            .size(px(28.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .when(!enabled, |this| this.opacity(0.64))
            .when(enabled, |this| this.cursor_pointer().hover(|this| this.bg(hsla(p.sidebar_row_hover))))
            .child(content)
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip(cx)).build(window, cx))
    }

    /// A sidebar menu button's 16px icon: `sidebar_icon`, `sidebar_foreground` while its button
    /// (`group`) is hovered. GPUI SVGs do not inherit the text color, so it is set here.
    fn button_icon(glyph: Icon, group: &'static str, cx: &App) -> gpui::Svg {
        let p = palette(cx);
        glyph.render(px(16.), hsla(p.sidebar_icon)).group_hover(group, |this| this.text_color(hsla(p.sidebar_foreground)))
    }

    /// `HistoryNavigationButton`: ghost `icon-sm` with a 14px arrow, disabled at either end.
    fn render_history_button(&self, back: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let enabled = self.state.read(cx).can_navigate(back);
        let command = if back { "navigation.back" } else { "navigation.forward" };
        let label = format!("{}{}", if back { "Go back" } else { "Go forward" }, crate::keymap::label(command, cx).map_or_else(String::new, |key| format!(" ({key})")));
        div()
            .id(if back { "history-back" } else { "history-forward" })
            .size(px(28.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .when(!enabled, |this| this.opacity(0.64))
            .when(enabled, |this| this.cursor_pointer().hover(|this| this.bg(hsla(p.sidebar_accent))))
            .child((if back { Icon::ArrowLeft } else { Icon::ArrowRight }).render(px(14.), hsla(p.sidebar_icon)))
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
            .when(enabled, |this| this.on_click(cx.listener(move |this, _, _, cx| this.state.update(cx, |state, cx| state.navigate_history(back, cx)))))
    }

    /// `SidebarThreadHeader`: the search field, then Filter and Add project once there are
    /// projects, then New thread (disabled without a project), inset 8 like a `SidebarGroup`.
    fn render_search(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let searching = self.search.is_active();
        let has_projects = self.state.read(cx).shell.snapshot.as_ref().is_some_and(|snapshot| !snapshot.projects.is_empty());
        div().p(px(8.)).flex().items_center().gap(px(4.)).child(
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
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .when(has_projects, |this| {
                    this.child(self.render_project_filter_button(cx)).child(
                        Self::header_button("add-project", Self::button_icon(Icon::FolderPlus, "add-project", cx), |_| "Add project".to_owned(), true, cx)
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::palette::OpenAddProject), cx)),
                    )
                })
                .child(
                    Self::header_button("new-thread-sidebar", Self::button_icon(Icon::SquarePen, "new-thread-sidebar", cx), new_thread_label, has_projects, cx)
                        .when(has_projects, |this| this.on_click(|_, window, cx| window.dispatch_action(Box::new(crate::new_thread::NewThread), cx))),
                ),
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

    /// `SidebarChromeFooter`: a row of 32px utility buttons (Settings, Pull requests, Usage), or
    /// Back while the pull requests page is open, then the Undo notice in the space left.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let on_pull_requests = self.state.read(cx).pull_requests_open;
        let utility = |id: &'static str, glyph: Icon, label: &'static str| {
            div()
                .id(id)
                .group(id)
                .size(px(32.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|this| this.bg(hsla(p.sidebar_row_hover)))
                .child(Self::button_icon(glyph, id, cx))
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label).build(window, cx))
        };
        let state = self.state.clone();
        let mut row = div().flex().items_center().gap(px(4.));
        if on_pull_requests {
            row = row.child(
                div()
                    .id("sidebar-back")
                    .flex_1()
                    .min_w_0()
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.sidebar_muted_foreground).opacity(0.8))
                    .hover(|this| this.bg(hsla(p.sidebar_row_hover)).text_color(hsla(p.sidebar_foreground)))
                    .child(Icon::ArrowLeft.sized(px(16.)).text_color(hsla(p.sidebar_icon)))
                    .child("Back")
                    .on_click(move |_, _, cx| state.update(cx, |state, cx| state.set_pull_requests_open(false, cx))),
            );
        } else {
            row = row
                .child(utility("sidebar-settings", Icon::Settings, "Settings").on_click(|_, _, cx| crate::settings::open(Section::General, cx)))
                .child(
                    utility("sidebar-pull-requests", Icon::GitPullRequestArrow, "Pull requests")
                        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.set_pull_requests_open(true, cx))),
                )
                .child(
                    utility("sidebar-usage", Icon::ChartNoAxesColumn, "Usage")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::usage::OpenUsage), cx)),
                );
        }
        div().flex_none().px(px(8.)).pt(px(4.)).pb(px(8.)).child(row.children(self.render_undo_notice(cx)))
    }

    /// `SidebarThreadUndoNotice`: "<summary>, ⌘Z to undo" in the footer row's free space.
    fn render_undo_notice(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let notice = self.state.read(cx).sidebar.undo.notice()?;
        let p = palette(cx);
        let label = crate::keymap::label("thread.undo", cx).map_or_else(|| "Undo".to_owned(), |shortcut| format!("{shortcut} to undo"));
        Some(
            div()
                .flex_1()
                .min_w_0()
                .h(px(24.))
                .overflow_hidden()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(11.))
                .text_color(hsla(p.sidebar_muted_foreground))
                .child(div().flex_none().child(format!("{},", notice.summary())))
                .child(
                    div()
                        .id("sidebar-undo")
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .cursor_pointer()
                        .font_weight(FontWeight::MEDIUM)
                        .hover(|this| this.underline())
                        .child(div().truncate().child(label))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |state, cx| state.undo_latest(cx));
                        })),
                ),
        )
    }
}

fn new_thread_label(cx: &App) -> String {
    crate::keymap::label("chat.new", cx).map_or_else(|| "New thread".to_owned(), |key| format!("New thread ({key})"))
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
            div()
                .id("sidebar-thread-list")
                .flex_1()
                .flex()
                .flex_col()
                .child(list(self.list.clone(), cx.processor(|this, index: usize, window, cx| this.render_row(index, window, cx))).flex_1())
                .on_drop(cx.listener(|this, _: &drag::DraggedThread, _, cx| this.finish_drag(true, cx)))
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(p.sidebar))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| this.finish_drag(false, cx)))
            .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| this.finish_drag(false, cx)))
            .child(self.render_header(cx))
            .child(self.render_search(cx))
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .child(self.render_footer(cx))
            .children(self.render_project_filter(cx))
    }
}
