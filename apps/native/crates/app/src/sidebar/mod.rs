//! Sidebar pane: header with thread search, the sectioned thread list, and the Undo notice
//! (`apps/web/src/components/Sidebar.tsx`, `sidebar/SidebarThreadHeader.tsx`,
//! `sidebar/SidebarThreadUndoNotice.tsx`). Rows have three heights (card, slim, shelf), so the
//! list is a variable-height `list` that splices only the rows that changed. The pane is
//! rendered `.cached()` and notifies itself when visible sidebar state changes.

mod bulk;
mod drafts;
pub mod controller;
mod drag;
mod favicons;
mod project_filter;
mod project_folders;
mod row;
mod search;

use gpui::Focusable;
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

#[derive(PartialEq, Eq)]
enum PaneRowKey { Draft(String), Divider, Thread(RowKey), Scoped(t3_core::sidebar::fleet::ScopedRowKey) }

pub struct SidebarPane {
    updates: Entity<crate::updates::UpdateControl>,
    composer: Entity<crate::composer::Composer>,
    drafts: t3_core::sidebar::drafts::DraftRows,
    discarded: std::collections::HashMap<String, (t3_core::thread_actions::Claim, crate::composer::drafts::DiscardedDraft)>,
    state: Entity<AppState>,
    menu: Entity<ContextMenu>,
    list: ListState,
    measured_font: Option<(Pixels, gpui::SharedString)>,
    row_keys: Vec<PaneRowKey>,
    search_input: Entity<InputState>,
    search: SearchState,
    rename: Option<(String, Entity<InputState>)>,
    rename_previous_focus: Option<gpui::FocusHandle>,
    /// Where the project filter popover opens, while it is open.
    project_filter: Option<gpui::Point<gpui::Pixels>>,
    jump_hints: bool,
    jump_hint_timer: Option<Task<()>>,
    now: Ms,
    model_seen: Arc<t3_core::sidebar::SidebarModel>,
    fleet_seen: Arc<t3_core::sidebar::fleet::ScopedSidebarModel>,
    rename_key: Option<t3_core::sidebar::tag_filter::ThreadKey>,
    /// Whether the last frame drew a ticking working duration; only then does the clock run per second.
    live_rendered: bool,
    clock: Option<(ClockRate, Task<()>)>,
    drag: Option<drag::PaneDrag>,
    /// A thread or project menu is open; rows show no hover card over it.
    menu_open: bool,
    snooze_grant_watch: Option<((u64, u64), Task<()>)>,
    snooze_access: bool,
    _subscriptions: Vec<Subscription>,
}

impl SidebarPane {
    pub fn new(
        state: Entity<AppState>,
        composer: Entity<crate::composer::Composer>,
        menu: Entity<ContextMenu>,
        dialog: Entity<ConfirmDialog>,
        snooze: Entity<SnoozeDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        state.update(cx, |state, _| state.sidebar.search_input = Some(search_input.downgrade()));
        let subscriptions = vec![
            cx.subscribe(&composer, |this, _, event: &crate::composer::drafts::DraftsChanged, cx| {
                let state = this.state.read(cx);
                if this.drafts.update_draft(this.composer.read(cx).sidebar_drafts(), &event.project_id, state.sidebar.project_scope.as_ref()) {
                    this.sync_list_rows(cx);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::RestoreDraft {
                    let projects = this.state.update(cx, |state, _| std::mem::take(&mut state.sidebar.drafts_to_restore));
                    for project in projects {
                        if let Some((_, draft)) = this.discarded.remove(&project) {
                            this.composer.update(cx, |composer, cx| composer.restore_sidebar_draft(&project, draft, window, cx));
                        }
                    }
                    this.sync_rows(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
                AppEvent::Shell | AppEvent::Selection => {
                    let state = this.state.read(cx);
                    let model = state.sidebar.model.clone();
                    let fleet = state.sidebar.fleet.clone();
                    // Streaming ticks reach the shell but leave both derived models alone.
                    if matches!(event, AppEvent::Shell) && Arc::ptr_eq(&model, &this.model_seen) && Arc::ptr_eq(&fleet, &this.fleet_seen) { return; }
                    this.model_seen = model;
                    this.fleet_seen = fleet;
                    this.sync_rows(cx);
                    this.search.refresh(&this.state, cx);
                    cx.notify();
                }
                AppEvent::Notice => {
                    let expired: Vec<_> = this.discarded.iter().filter(|(project, (claim, _))| !this.state.read(cx).sidebar.undo.is_current(claim) && !this.state.read(cx).sidebar.drafts_to_restore.contains(*project)).map(|(project, _)| project.clone()).collect();
                    for project in expired {
                        if let Some((_, discarded)) = this.discarded.remove(&project) {
                            this.composer.update(cx, |composer, cx| composer.commit_sidebar_discard(discarded, cx));
                        }
                    }
                    cx.notify();
                },
                AppEvent::Phase | AppEvent::ServerConfig => this.sync_snooze_grants(cx),
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
            // The web's thread menu is the native one, which takes the pointer from the rows.
            cx.observe(&menu, |this, menu, cx| {
                let open = menu.read(cx).is_open();
                if open != this.menu_open {
                    this.menu_open = open;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&menu, window, move |this, _, event: &ContextMenuEvent, window, cx| match event {
                ContextMenuEvent::DiscardDraft(project) => this.discard_draft(project, window, cx),
                ContextMenuEvent::Rename(owner, focus) => { if owner.allows(this.state.read(cx), &t3_core::thread_actions::ThreadAction::Rename) {
                    if let Some(key) = owner.key() { this.start_scoped_rename(&key, window, cx); }
                    else { this.start_rename_from_menu(&owner.thread, focus.clone(), window, cx); }
                } },
                ContextMenuEvent::CustomSnooze(thread_ids) => snooze.update(cx, |snooze, cx| snooze.open(thread_ids.clone(), window, cx)),
                ContextMenuEvent::ScopedConfirm(owner, action) => {
                    let owner = owner.clone(); let action = action.clone();
                    if !owner.allows(this.state.read(cx),&action) { return; }
                    let Some(key) = owner.key() else { return };
                    let title = this.state.read(cx).scoped_thread(&key).map(|row| row.thread.title.clone()).unwrap_or_default();
                    let (confirm,heading,description,label) = if matches!(action,t3_core::thread_actions::ThreadAction::Unpin) {
                        let (heading,description) = t3_core::thread_actions::unpin_confirmation(&title);
                        (crate::settings::client_settings(cx).confirm_thread_unpin(),heading,description,"Unpin")
                    } else {
                        let heading = t3_core::thread_actions::archive_confirmation(&title);
                        (crate::settings::client_settings(cx).confirm_thread_archive(),heading,"You can reopen this thread later.","Archive")
                    };
                    if confirm { dialog.update(cx,|dialog,cx| dialog.confirm(heading,description,label,move |state,cx| {
                        if owner.allows(state,&action) { state.run_scoped_thread_action(&key,action,&[],cx); }
                    },window,cx)); }
                    else { this.state.update(cx,|state,cx| state.run_scoped_thread_action(&key,action,&[],cx)); }
                }
                ContextMenuEvent::ScopedBulk(owners, action) => {
                    let owners = owners.clone(); let action = action.clone();
                    let needs_confirm = match action { t3_core::sidebar::selection::BulkAction::Delete => crate::settings::client_settings(cx).confirm_thread_delete(), t3_core::sidebar::selection::BulkAction::Unpin => crate::settings::client_settings(cx).confirm_thread_unpin(), _ => false };
                    if !needs_confirm { return this.state.update(cx, |state, cx| state.run_scoped_bulk_action(owners, action, cx)); }
                    let (heading, description) = if matches!(action, t3_core::sidebar::selection::BulkAction::Unpin) { (format!("Unpin {} threads?", owners.len()), "This will move the threads out of your pinned section.") } else { t3_core::sidebar::selection::bulk_delete_confirmation(owners.len()) };
                    dialog.update(cx, |dialog, cx| dialog.confirm(heading, description, "Confirm", move |state, cx| state.run_scoped_bulk_action(owners, action, cx), window, cx));
                }
                ContextMenuEvent::BulkDelete(thread_ids) => {
                    if !crate::settings::client_settings(cx).confirm_thread_delete() {
                        return this.state.update(cx, |state, cx| state.delete_threads(thread_ids.clone(), cx));
                    }
                    let (heading, description) = t3_core::sidebar::selection::bulk_delete_confirmation(thread_ids.len());
                    let ids = thread_ids.clone();
                    dialog.update(cx, |dialog, cx| dialog.confirm(heading, description, "Delete", move |state, cx| state.delete_threads(ids, cx), window, cx));
                }
                ContextMenuEvent::Delete(owner, focus) => {
                    if !owner.allows(this.state.read(cx), &t3_core::thread_actions::ThreadAction::Delete) { return; }
                    let thread_id = &owner.thread;
                    if let Some(key) = owner.key() {
                        let title = this.state.read(cx).scoped_thread(&key).map(|row| row.thread.title.clone()).unwrap_or_default();
                        let (heading, description) = t3_core::thread_actions::delete_confirmation(&title);
                        let owner = owner.clone();
                        if crate::settings::client_settings(cx).confirm_thread_delete() {
                            dialog.update(cx, |dialog, cx| dialog.confirm(heading, description, "Delete", move |state, cx| {
                                if owner.allows(state, &t3_core::thread_actions::ThreadAction::Delete) { state.run_scoped_thread_action(&key, t3_core::thread_actions::ThreadAction::Delete, &[], cx); }
                            }, window, cx));
                        } else { this.state.update(cx, |state, cx| state.run_scoped_thread_action(&key, t3_core::thread_actions::ThreadAction::Delete, &[], cx)); }
                        return;
                    }
                    if !crate::settings::client_settings(cx).confirm_thread_delete() {
                        return this.state.update(cx, |state, cx| state.delete_thread(thread_id, cx));
                    }
                    let Some(title) = this.state.read(cx).sidebar.model.thread(thread_id).map(|t| t.title.clone()) else { return };
                    let (heading, description) = t3_core::thread_actions::delete_confirmation(&title);
                    let owner = owner.clone();
                    let previous_focus = focus.clone();
                    dialog.update(cx, |dialog, cx| dialog.confirm(heading, description, "Delete", move |state, cx| { if owner.allows(state, &t3_core::thread_actions::ThreadAction::Delete) { state.delete_thread(&owner.thread, cx); } }, window, cx));
                    dialog.update(cx, |dialog, _| dialog.set_previous_focus(previous_focus));
                }
            }),
        ];
        let updates = cx.new(|cx| crate::updates::UpdateControl::new(state.clone(), cx));
        let mut pane = SidebarPane {
            updates,
            composer,
            drafts: Default::default(),
            discarded: Default::default(),
            state,
            menu,
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            measured_font: None,
            row_keys: Vec::new(),
            search_input,
            search: SearchState::default(),
            rename: None,
            rename_previous_focus: None,
            project_filter: None,
            jump_hints: false,
            jump_hint_timer: None,
            now: now_ms(),
            model_seen: Default::default(),
            fleet_seen: Default::default(),
            rename_key: None,
            live_rendered: false,
            clock: None,
            drag: None,
            menu_open: false,
            snooze_grant_watch: None,
            snooze_access: false,
            _subscriptions: subscriptions,
        };
        pane.sync_rows(cx);
        pane.sync_snooze_grants(cx);
        pane
    }

    fn sync_snooze_grants(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let owner = snooze_grant_owner(state);
        let allowed = snooze_access(state);
        let owner_changed = self.snooze_grant_watch.as_ref().map(|(owner, _)| *owner) != owner;
        if owner_changed {
            self.snooze_grant_watch = None;
            if let (Some(owner), Some(client)) = (owner, self.state.read(cx).client_handle()) {
                let mut grants = client.session_state();
                self.snooze_grant_watch = Some((owner, cx.spawn(async move |this, cx| {
                    while grants.changed().await.is_ok() {
                        let current = this.update(cx, |this, cx| {
                            if snooze_grant_owner(this.state.read(cx)) != Some(owner) { return false; }
                            this.sync_snooze_grants(cx);
                            true
                        }).unwrap_or(false);
                        if !current { break; }
                    }
                })));
            }
        }
        if owner_changed || self.snooze_access != allowed {
            self.snooze_access = allowed;
            cx.notify();
        }
    }

    /// Splices the list so scroll position and measured heights survive streaming updates.
    fn sync_rows(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        self.drafts.update(self.composer.read(cx).sidebar_drafts(), state.draft_project.as_deref(), state.sidebar.project_scope.as_ref());
        self.sync_list_rows(cx);
    }

    fn sync_list_rows(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let mut keys: Vec<_> = self.drafts.rows.iter().map(|row| PaneRowKey::Draft(row.project_id.clone())).collect();
        if !keys.is_empty() { keys.push(PaneRowKey::Divider); }
        if state.fleet.environments.is_empty() { keys.extend(state.sidebar.model.row_keys().into_iter().map(PaneRowKey::Thread)); }
        else { keys.extend(state.sidebar.fleet.row_keys().into_iter().map(PaneRowKey::Scoped)); }
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

    fn start_scoped_rename(&mut self, key: &t3_core::sidebar::tag_filter::ThreadKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(owner) = crate::context_menu::ThreadMenuOwner::capture_scoped(self.state.read(cx), key) else { return; };
        if !owner.allows(self.state.read(cx), &t3_core::thread_actions::ThreadAction::Rename) { return; }
        let Some(title) = self.state.read(cx).scoped_thread(key).map(|row| row.thread.title.clone()) else { return; };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let input_key = key.clone();
        let previous = window.focused(cx);
        let subscription = cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) && this.rename_key.as_ref() == Some(&input_key) {
                let title = input.read(cx).value().to_string();
                this.rename = None;
                this.rename_key = None;
                this.state.update(cx, |state, cx| {
                    if owner.allows(state, &t3_core::thread_actions::ThreadAction::Rename) {
                        state.run_scoped_command(owner.clone(), t3_core::thread_actions::rename(&controller::uuid(), &owner.thread, title.trim()), t3_core::auth::ORCHESTRATION_OPERATE_SCOPE, "Failed to rename thread", cx);
                    }
                });
                if input.read(cx).focus_handle(cx).is_focused(window) && let Some(focus) = &previous { window.focus(focus, cx); }
                cx.notify();
            }
        });
        self.rename_key = Some(key.clone());
        self.rename = Some((key.thread_id.clone(), input.clone()));
        self._subscriptions.push(subscription);
        input.update(cx, |input, cx| { input.focus(window, cx); input.select_all(window, cx); });
        cx.notify();
    }

    fn start_rename(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.start_rename_from_menu(thread_id, None, window, cx);
    }

    fn start_rename_from_menu(&mut self, thread_id: &str, previous_focus: Option<gpui::FocusHandle>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.state.read(cx).sidebar.model.thread(thread_id).map(|t| t.title.clone()) else { return };
        let Some(owner) = crate::context_menu::ThreadMenuOwner::capture(self.state.read(cx), thread_id) else { return };
        let previous_focus = previous_focus.or_else(|| window.focused(cx));
        self.rename_previous_focus = previous_focus.clone();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let id = thread_id.to_owned();
        let subscription = cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                let title = input.read(cx).value().to_string();
                if this.rename.as_ref().is_some_and(|(renaming, _)| *renaming == id) {
                    this.rename = None;
                    this.state.update(cx, |state, cx| { if owner.allows(state, &t3_core::thread_actions::ThreadAction::Rename) { state.rename_thread(&id, &title, cx); } });
                    if input.read(cx).focus_handle(cx).is_focused(window) && let Some(focus) = &previous_focus { window.focus(focus, cx); }
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
        if let Some((_, input)) = self.rename.take() {
            let input_focus = input.read(cx).focus_handle(cx);
            let previous = self.rename_previous_focus.take();
            cx.defer(move |cx| {
                if let Some(window) = cx.active_window() { let _ = window.update(cx, |_, window, cx| {
                    if input_focus.is_focused(window) && let Some(previous) = previous { window.focus(&previous, cx); }
                }); }
            });
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
        let Some(key) = self.search.highlighted_key().cloned() else { return };
        self.clear_search(window, cx);
        self.state.update(cx, |state, cx| {
            if key.environment_id.is_empty() { state.select_thread(&key.thread_id, cx); }
            else if let Some(owner) = state.scoped_environment(&key.environment_id).map(|entry| entry.summary.owner.clone()) { state.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Thread(key.thread_id), cx); }
        });
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
        let state = self.state.read(cx);
        let notice = state.sidebar.undo.notice()?;
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

fn snooze_grant_owner(state: &AppState) -> Option<(u64, u64)> {
    Some((state.connection_id()?, *state.client_handle()?.connection_generation().borrow()))
}

fn snooze_access(state: &AppState) -> bool {
    state.sidebar.capabilities.config_loaded
        && state.server_config.config.is_some()
        && state.sidebar.capabilities.can_operate
        && state.client_handle().is_some_and(|client| client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)))
}

fn new_thread_label(cx: &App) -> String {
    crate::keymap::label("chat.new", cx).map_or_else(|| "New thread".to_owned(), |key| format!("New thread ({key})"))
}

impl Render for SidebarPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font = (window.rem_size(), crate::theme::font_family(t3_core::client_settings::typography::FontRole::Interface, cx));
        if self.measured_font.as_ref() != Some(&font) {
            self.measured_font = Some(font);
            self.list.remeasure();
        }
        let synchronized = self.state.read(cx).shell.synchronized;
        self.live_rendered = false;
        let p = palette(cx);
        let body = if self.search.is_active() {
            self.render_search_results(cx).into_any_element()
        } else if self.row_keys.is_empty() {
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
            .child(self.updates.clone())
            .child(self.render_footer(cx))
            .children(self.render_project_filter(cx))
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod snooze_grant_tests {
    use super::*;
    use gpui::{AppContext, MouseButton, StyleRefinement};
    use std::{cell::Cell, rc::Rc};
    use crate::context_menu::project_filter_tests::{install_state, session};
    use t3_client::supervisor::{ClientFixture, ConnectionStatus};

    struct CachedSidebar {
        pane: Entity<SidebarPane>,
        menu: Entity<ContextMenu>,
    }

    impl Render for CachedSidebar {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(400.)).h_full()
                .child(self.pane.clone().cached(StyleRefinement::default().size_full()))
                .child(self.menu.clone())
        }
    }

    #[gpui::test]
    fn cached_snooze_grants_notify_without_pointer_or_clock_changes(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            let (state, fixture) = install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            state.update(cx, |s, cx| {
                s.shell.snapshot.as_mut().unwrap().threads.retain(|t| t.id == "menu-thread");
                s.rebuild_sidebar(cx);
                s.draft_project = None;
                s.pull_requests_open = true;
                assert!(s.selected_thread.is_none());
                assert!(t3_core::sidebar::settled::can_snooze(s.sidebar.model.thread("menu-thread").unwrap(), now_ms()));
            });
            (state, fixture)
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu.clone(), confirm, snooze, window, cx));
            CachedSidebar { pane, menu }
        });
        let pane = root.read_with(cx, |root, _| root.pane.clone());
        cx.run_until_parked();
        cx.update(|window, cx| { window.draw(cx).clear(); });
        let target = gpui::point(px(306.), px(116.));
        cx.simulate_mouse_move(target, None::<MouseButton>, Default::default());
        cx.update(|window, cx| { window.draw(cx).clear(); });
        let now = pane.read_with(cx, |pane, _| pane.now);
        assert!(!pane.read_with(cx, |pane, _| pane.live_rendered));
        let notifications = Rc::new(Cell::new(0));
        let observed = notifications.clone();
        let _subscription = cx.update(|_, cx| cx.observe(&pane, move |_, _| observed.set(observed.get() + 1)));
        cx.run_until_parked();
        let before = notifications.get();
        fixture.grants.send(Some(session(false))).unwrap();
        cx.run_until_parked();
        assert!(notifications.get() > before, "grant-only revoke must notify the cached production pane");
        cx.update(|window, cx| { assert_eq!(window.mouse_position(), target); window.draw(cx).clear(); });
        assert_eq!(pane.read_with(cx, |pane, _| pane.now), now);
        assert!(!pane.read_with(cx, |pane, cx| snooze_access(pane.state.read(cx))));

        let before = notifications.get();
        fixture.grants.send(Some(session(true))).unwrap();
        cx.run_until_parked();
        assert!(notifications.get() > before, "grant-only regrant must notify the cached production pane");
        cx.update(|window, cx| { assert_eq!(window.mouse_position(), target); window.draw(cx).clear(); });
        assert_eq!(pane.read_with(cx, |pane, _| pane.now), now);

        assert!(pane.read_with(cx, |pane, cx| snooze_access(pane.state.read(cx))));
        let before = notifications.get();
        let mut unrelated = session(true);
        unrelated.permissions.as_mut().unwrap().push("orchestration:read".into());
        fixture.grants.send(Some(unrelated)).unwrap();
        cx.run_until_parked();
        assert_eq!(notifications.get(), before, "unrelated grant changes do not notify the pane");

        // Queue an old-owner delivery before replacement; it must neither notify nor stay subscribed.
        fixture.grants.send(Some(session(false))).unwrap();
        let (client, mut replacement) = ClientFixture::handle(session(false));
        replacement.grants.send(None).unwrap();
        state.update(cx, |s, cx| s.install_active_environment(client.clone(), s.shell.clone(), s.server_config.clone(), ConnectionStatus::Connected, cx));
        drop(client);
        cx.run_until_parked();
        assert_eq!(fixture.grants.receiver_count(), 0, "old owner watchers are released");
        assert_eq!(pane.read_with(cx, |pane, _| pane.snooze_grant_watch.as_ref().map(|(owner, _)| *owner)), state.read_with(cx, |state, _| snooze_grant_owner(state)));
        cx.update(|window, cx| { window.draw(cx).clear(); });
        let before = notifications.get();
        fixture.grants.send_replace(Some(session(true)));
        cx.run_until_parked();
        assert_eq!(notifications.get(), before, "old channel cannot invalidate the new owner");

        replacement.grants.send(Some(session(true))).unwrap();
        cx.run_until_parked();
        assert!(notifications.get() > before, "initial current-owner grant must invalidate its cached pane");
        cx.update(|window, cx| { assert_eq!(window.mouse_position(), target); window.draw(cx).clear(); });
        assert_eq!(pane.read_with(cx, |pane, _| pane.now), now);
        assert!(pane.read_with(cx, |pane, cx| snooze_access(pane.state.read(cx))));
        let receivers = replacement.grants.receiver_count();
        replacement.generation.send(2).unwrap();
        state.update(cx, |_, cx| cx.emit(AppEvent::Phase));
        cx.run_until_parked();
        assert_eq!(replacement.grants.receiver_count(), receivers, "reconnect replaces the owned watch rather than accumulating receivers");

        for operate in [false, true] {
            let before = notifications.get();
            replacement.grants.send(Some(session(operate))).unwrap();
            cx.run_until_parked();
            assert!(notifications.get() > before, "reconnected current-owner grant still invalidates the pane");
            cx.update(|window, _| assert_eq!(window.mouse_position(), target));
            assert_eq!(pane.read_with(cx, |pane, _| pane.now), now);
        }

        for field in ["loaded", "capability", "config"] {
            let config = state.read_with(cx, |state, _| state.server_config.config.clone());
            for allowed in [false, true] {
                let before = notifications.get();
                state.update(cx, |s, cx| {
                    match field {
                        "loaded" => s.sidebar.capabilities.config_loaded = allowed,
                        "capability" => s.sidebar.capabilities.can_operate = allowed,
                        _ => s.server_config.config = allowed.then(|| config.clone().unwrap()),
                    }
                    cx.emit(AppEvent::ServerConfig);
                });
                cx.run_until_parked();
                assert!(notifications.get() > before, "{field} change invalidates the pane");
                assert_eq!(pane.read_with(cx, |pane, cx| snooze_access(pane.state.read(cx))), allowed);
                cx.update(|window, cx| { assert_eq!(window.mouse_position(), target); window.draw(cx).clear(); });
            }
        }
        assert_eq!(pane.read_with(cx, |pane, _| pane.now), now);
        let receivers = replacement.grants.receiver_count();
        pane.update(cx, |pane, _| pane.snooze_grant_watch = None);
        cx.run_until_parked();
        assert_eq!(replacement.grants.receiver_count(), receivers - 1, "dropping the owned task releases exactly its grant receiver");
        assert!(!fixture.take_commands().contains(&"orchestration.dispatchCommand"));
        assert!(!replacement.take_commands().contains(&"orchestration.dispatchCommand"));
        state.update(cx, |state, _| state.stop_pump());
        cx.cx.update(|cx| { cx.shutdown(); cx.clear_globals(); });
        drop(fixture); drop(replacement); drop(runtime); cx.run_until_parked();
        println!("production cached-pane notification contract: revoke/regrant/initial grant, stale-owner release, reconnect and config; stationary pointer, unchanged clock; retained ebd row hit proof is separate");
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod grouping_frames {
    use super::*;
    use gpui::{AppContext, HeadlessAppContext, Render, size};
    use serde_json::json;
    use std::sync::Arc;
    use t3_client::supervisor::{ClientFixture, ConnectionStatus};
    use t3_core::sidebar::fleet::ScopedSidebarRow;
    use t3_core::sidebar::grouping::GroupConsumer;

    struct GroupingFrame {
        pane: Entity<SidebarPane>,
        menu: Entity<crate::context_menu::ContextMenu>,
        dialog: Entity<crate::dialog::ConfirmDialog>,
        snooze: Entity<crate::snooze_dialog::SnoozeDialog>,
        palette: Entity<crate::palette::CommandPalette>,
    }
    impl Render for GroupingFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().flex().bg(hsla(palette(cx).background))
                .child(div().w(px(320.)).h_full().child(self.pane.clone()))
                .child(div().flex_1().h_full().border_l_1().border_color(hsla(palette(cx).border)))
                .child(self.menu.clone()).child(self.dialog.clone()).child(self.snooze.clone())
                .child(self.palette.clone()).children(crate::settings::overlay(window, cx))
        }
    }

    #[test]
    fn production_grouping_frames() {
        let Some(out) = std::env::var_os("T3_GROUPING_CAPTURE") else { return };
        let out = std::path::PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut cx = HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(), Arc::new(crate::icons::Assets),
            gpui_platform::current_headless_renderer,
        );
        cx.allow_parking();
        let (state, mut active_fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });
        t3_client::store::save_ui_state(t3_core::composer::drafts::STORE_NAME, &json!({"version":1,"drafts":{}}));
        runtime.block_on(runtime.spawn_blocking(|| t3_client::store::commit_client_settings(serde_json::Map::new())))
            .expect("fixture writer barrier task").expect("empty fixture drafts committed");
        let before = state.read_with(&cx, |state, _| (state.environments.active.clone(), state.selected_thread.clone(), state.draft_project.clone()));
        let (ready, loaded) = tokio::sync::oneshot::channel();
        let mut ready = Some(ready);
        let _loaded = state.update(&mut cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Shell && state.fleet.environments.len() == 2 {
                if let Some(ready) = ready.take() { let _ = ready.send(()); }
            }
        }));
        let mut fixtures = Vec::new();
        let mut fixture_events = Vec::new();
        for saved in ["grouping-a", "grouping-b"] {
            let (client, fixture) = ClientFixture::handle(serde_json::from_value(json!({
                "authenticated":true,"permissions":["orchestration:read","orchestration:operate","settings:write"]
            })).unwrap());
            let mut shell = t3_core::shell::ShellState::default();
            shell.apply(serde_json::from_value(json!({"kind":"snapshot","snapshot":{
                "snapshotSequence":1,"projects":[{
                    "id":"same-project","title":"Shared repository","workspaceRoot":format!("/fixture/{saved}"),
                    "createdAt":"2026-11-01T00:00:00Z","updatedAt":"2026-11-01T00:00:00Z",
                    "projectIcon":{"kind":"monogram","text":"SR","color":"blue"},
                    "repositoryIdentity":{"canonicalKey":"shared-repository"}
                }],"threads":[{
                    "id":"same-thread","projectId":"same-project","title":format!("{saved} thread"),
                    "createdAt":"2026-11-01T00:00:00Z","updatedAt":"2026-11-01T00:00:00Z",
                    "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
                    "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":"same-thread"},
                    "status":if saved=="grouping-a" { "running" } else { "idle" },
                    "latestRunId":if saved=="grouping-a" { Some("fixture-run") } else { None },
                    "activeRunId":if saved=="grouping-a" { Some("fixture-run") } else { None },
                    "activityRunStartedAt":if saved=="grouping-a" { Some("2026-11-01T00:00:00Z") } else { None },
                    "hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0
                }]
            }})).unwrap());
            shell.synchronized = true;
            let mut config = t3_core::server_config::ServerConfigState::default();
            config.apply(serde_json::from_value(json!({"type":"snapshot","config":{
                "environment":{"environmentId":saved,"label":saved,"capabilities":{"projectOrganization":true}},
                "cwd":format!("/fixture/{saved}"),"settings":{}
            }})).unwrap());
            let install = state.update(&mut cx, |state, _| state.connections.install_fixture(saved, client, shell, config, ConnectionStatus::Connected));
            fixture_events.push(runtime.block_on(install));
            fixtures.push(fixture);
        }
        let executor = cx.update(|cx| cx.foreground_executor().clone());
        executor.block_test(loaded).unwrap();
        state.update(&mut cx, |state, cx| {
            assert_eq!(state.fleet.environments.len(), 2);
            state.project_preferences = Default::default();
            state.sidebar.settings.working_shelf_enabled = true;
            state.sidebar.shelves.working_expanded = true;
            state.rebuild_sidebar(cx);
            let groups = state.project_groups_for(GroupConsumer::Sidebar, cx);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].registrations.len(), 2);
            assert_eq!(state.sidebar.fleet.threads.len(), 2);
            assert_ne!(state.sidebar.fleet.threads[0].key, state.sidebar.fleet.threads[1].key);
            assert!(state.sidebar.fleet.rows.iter().any(|row| matches!(row, ScopedSidebarRow::Shelf { shelf:t3_core::sidebar::Shelf::Working, .. })));
        });
        let mut handles = None;
        let mut outside = None;
        let window = cx.open_window(size(px(1280.), px(900.)), |window, cx| {
            outside = Some(cx.focus_handle());
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let dialog = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer.clone(), menu.clone(), dialog.clone(), snooze.clone(), window, cx));
            let command_palette = cx.new(|cx| crate::palette::CommandPalette::new(state.clone(), composer, window, cx));
            handles = Some((pane.clone(), command_palette.clone(), menu.clone()));
            cx.new(|_| GroupingFrame { pane, menu, dialog, snooze, palette:command_palette })
        }).unwrap();
        let (pane, command_palette, menu) = handles.unwrap();
        let outside = outside.unwrap();
        let click = |cx:&mut HeadlessAppContext, x:f32, y:f32| {
            cx.update_window(window.into(), |_, window, cx| {
                let position = gpui::point(px(x), px(y));
                window.simulate_mouse_move(position, cx);
                window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent { button:gpui::MouseButton::Left, position, modifiers:Default::default(), click_count:1, first_mouse:false }), cx);
                window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent { button:gpui::MouseButton::Left, position, modifiers:Default::default(), click_count:1 }), cx);
            }).unwrap();
        };
        let press_key = |cx:&mut HeadlessAppContext, key:&str| {
            let keystroke = gpui::Keystroke::parse(key).unwrap();
            cx.update_window(window.into(), |_, window, cx| {
                window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent { keystroke:keystroke.clone(), is_held:false, prefer_character_input:false }), cx);
                window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
            }).unwrap();
        };
        let capture = |cx:&mut HeadlessAppContext, name:&str| {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| { window.refresh(); window.draw(cx).clear(); }).unwrap();
            cx.capture_screenshot(window.into()).unwrap().save(out.join(format!("{name}.png"))).unwrap();
        };
        cx.update(|cx| crate::settings::store::install_committed_client(&state, serde_json::from_value(json!({"boringSidebarEnabled":true})).unwrap(), cx));
        state.update(&mut cx, |state, cx| {
            state.sidebar.settings.working_shelf_enabled = true;
            state.rebuild_sidebar(cx);
            assert!(state.sidebar.fleet.rows.iter().any(|row| matches!(row,ScopedSidebarRow::Folder { .. })));
            assert!(!state.sidebar.fleet.rows.iter().any(|row| matches!(row,ScopedSidebarRow::Shelf { shelf:t3_core::sidebar::Shelf::Working, .. })));
            let groups=state.project_groups_for(GroupConsumer::Sidebar,cx);
            state.project_preferences.scope_keys=vec![groups[0].key.clone()];
            state.rebuild_sidebar(cx);
            assert_eq!(state.project_preferences.resolved_scope(&groups,true),vec![groups[0].key.clone()]);
            assert_eq!(state.sidebar.fleet.threads.len(),2);
        });
        for dark in [false,true] {
            let theme=if dark { "dark" } else { "light" };
            cx.update(|cx| {
                let mut preferences=crate::theme::preferences(cx).clone();
                let mode=if dark { t3_core::client_settings::AppearanceMode::Dark } else { t3_core::client_settings::AppearanceMode::Light };
                preferences.theme=t3_core::client_settings::ThemePreference::Default(mode); preferences.mode=mode;
                crate::theme::set_preferences(preferences,cx);
            });
            capture(&mut cx,&format!("{theme}-folders"));
            let folder_position = pane.read_with(&cx, |pane, cx| {
                let state = pane.state.read(cx);
                let index = state.sidebar.fleet.rows.iter().position(|row| matches!(row, ScopedSidebarRow::Folder { .. })).unwrap();
                let offset = if pane.drafts.rows.is_empty() { 0 } else { pane.drafts.rows.len() + 1 };
                pane.list.bounds_for_item(index + offset).expect("rendered production folder must have measured list bounds").center()
            });
            std::fs::write(out.join(format!("{theme}-folder-hit.json")), serde_json::to_vec_pretty(&json!({"x":f32::from(folder_position.x),"y":f32::from(folder_position.y),"source":"production ListState::bounds_for_item center"})).unwrap()).unwrap();
            cx.update_window(window.into(), |_, window, cx| window.focus(&outside, cx)).unwrap();
            click(&mut cx, f32::from(folder_position.x), f32::from(folder_position.y));
            cx.run_until_parked();
            let expanded_before_menu = state.read_with(&cx, |state, cx| state.project_preferences.expanded(&state.project_groups_for(GroupConsumer::Sidebar, cx)[0]));
            let folder_focus = cx.update_window(window.into(), |_, window, cx| window.focused(cx)).unwrap();
            for key in ["shift-f10", "menu"] {
                press_key(&mut cx, key);
                cx.run_until_parked();
                assert!(menu.read_with(&cx, |menu, _| menu.is_open()), "focused production folder must open its context menu from {key}");
                assert!(folder_focus.is_some());
                state.read_with(&cx, |state, cx| {
                    assert_eq!(state.project_preferences.expanded(&state.project_groups_for(GroupConsumer::Sidebar, cx)[0]), expanded_before_menu, "context-menu key must not toggle the folder");
                    assert_eq!((state.environments.active.clone(),state.selected_thread.clone(),state.draft_project.clone()),before);
                });
                capture(&mut cx,&format!("{theme}-folder-context-{key}"));
                press_key(&mut cx, "escape");
                cx.run_until_parked();
                assert!(!menu.read_with(&cx, |menu, _| menu.is_open()));
                cx.update_window(window.into(), |_, window, cx| assert_eq!(window.focused(cx), folder_focus, "Escape restores the folder's real focus")).unwrap();
            }
            cx.update_window(window.into(), |_, window, cx| {
                window.simulate_mouse_move(folder_position, cx);
                window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent { button:gpui::MouseButton::Right, position:folder_position, modifiers:Default::default(), click_count:1, first_mouse:false }), cx);
                window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent { button:gpui::MouseButton::Right, position:folder_position, modifiers:Default::default(), click_count:1 }), cx);
            }).unwrap();
            cx.run_until_parked();
            assert!(menu.read_with(&cx, |menu, _| menu.is_open()), "right-click on the measured production folder opens the same menu");
            capture(&mut cx,&format!("{theme}-folder-context-right-click"));
            let backdrop = cx.update_window(window.into(), |_, window, cx| {
                let sidebar = pane.read(cx).list.viewport_bounds();
                let viewport = window.viewport_size();
                gpui::point((sidebar.right() + viewport.width) / 2., viewport.height / 2.)
            }).unwrap();
            click(&mut cx, f32::from(backdrop.x), f32::from(backdrop.y));
            cx.run_until_parked();
            assert!(!menu.read_with(&cx, |menu, _| menu.is_open()), "actual content backdrop click cancels the project menu");
            state.read_with(&cx, |state, cx| {
                assert_eq!(state.project_preferences.expanded(&state.project_groups_for(GroupConsumer::Sidebar, cx)[0]), expanded_before_menu);
                assert_eq!((state.environments.active.clone(),state.selected_thread.clone(),state.draft_project.clone()),before);
            });
            cx.update_window(window.into(), |_, window, cx| assert_eq!(window.focused(cx), folder_focus, "backdrop cancellation restores folder focus")).unwrap();
            for key in ["enter", "space"] {
                press_key(&mut cx, key);
                cx.run_until_parked();
                state.read_with(&cx, |state, cx| assert_eq!(state.project_preferences.expanded(&state.project_groups_for(GroupConsumer::Sidebar, cx)[0]), !expanded_before_menu, "{key} toggles the focused folder exactly once"));
                press_key(&mut cx, key);
                cx.run_until_parked();
                state.read_with(&cx, |state, cx| assert_eq!(state.project_preferences.expanded(&state.project_groups_for(GroupConsumer::Sidebar, cx)[0]), expanded_before_menu, "{key} reopens the same folder exactly once"));
            }
            click(&mut cx, f32::from(folder_position.x), f32::from(folder_position.y));
            cx.run_until_parked();
            capture(&mut cx,&format!("{theme}-folders-keyboard-restored"));
            click(&mut cx, 150., 125.);
            state.read_with(&cx, |state, cx| {
                let group = &state.project_groups_for(GroupConsumer::Sidebar, cx)[0];
                assert!(!state.project_preferences.expanded(group), "observed folder row click collapses the logical group");
                assert!(!state.sidebar.fleet.rows.iter().any(|row| matches!(row, ScopedSidebarRow::Thread { .. })));
            });
            capture(&mut cx,&format!("{theme}-folders-collapsed"));
            click(&mut cx, 150., 125.);
            state.read_with(&cx, |state, cx| assert!(state.project_preferences.expanded(&state.project_groups_for(GroupConsumer::Sidebar, cx)[0])));
            pane.update(&mut cx,|pane,cx| { pane.project_filter=Some(gpui::point(px(260.),px(60.))); cx.notify(); });
            capture(&mut cx,&format!("{theme}-project-filter"));
            pane.update(&mut cx,|pane,cx| { pane.project_filter=None; cx.notify(); });
            cx.update_window(window.into(),|_,window,cx| {
                command_palette.update(cx,|palette,cx| palette.toggle(window,cx));
                assert!(command_palette.read(cx).focus_handle(cx).is_focused(window));
                // Focused Input requires a real macOS NSView; this captures the production palette unfocused.
                window.focus(&outside, cx);
            }).unwrap();
            capture(&mut cx,&format!("{theme}-command-palette"));
            cx.update_window(window.into(),|_,window,cx| command_palette.update(cx,|palette,cx| palette.toggle(window,cx))).unwrap();
            cx.update(|cx| crate::settings::open(t3_core::settings::search::Section::Projects,cx));
            capture(&mut cx,&format!("{theme}-project-settings-list"));
            click(&mut cx, 500., 230.);
            capture(&mut cx,&format!("{theme}-project-settings-detail"));
            cx.update_window(window.into(),|_,window,cx| window.dispatch_action(Box::new(crate::settings::CloseSettings),cx)).unwrap();
            cx.run_until_parked();
        }
        state.read_with(&cx, |state,_| assert_eq!((state.environments.active.clone(),state.selected_thread.clone(),state.draft_project.clone()),before));
        let restore = state.read_with(&cx, |state, _| (state.client_handle().unwrap(),state.shell.clone(),state.server_config.clone()));
        let target = state.read_with(&cx, |state, cx| state.scoped_project_action_target(&state.project_groups_for(GroupConsumer::Sidebar,cx)[0],cx).unwrap());
        let expanded_before_draft = state.read_with(&cx, |state,cx| state.project_preferences.expanded(&state.project_groups_for(GroupConsumer::Sidebar,cx)[0]));
        let header_position = pane.read_with(&cx, |pane,cx| {
            let index=pane.state.read(cx).sidebar.fleet.rows.iter().position(|row| matches!(row,ScopedSidebarRow::Folder { .. })).unwrap();
            let offset=if pane.drafts.rows.is_empty() { 0 } else { pane.drafts.rows.len()+1 };
            pane.list.bounds_for_item(index+offset).unwrap().center()
        });
        click(&mut cx,f32::from(header_position.x),f32::from(header_position.y));
        cx.run_until_parked();
        capture(&mut cx,"folder-child-before-tab");
        let header_focus=cx.update_window(window.into(),|_,window,cx| window.focused(cx)).unwrap().unwrap();
        cx.update_window(window.into(),|_,window,cx| window.focus_next(cx)).unwrap();
        cx.run_until_parked();
        capture(&mut cx,"folder-child-tab-focused");
        let child_focus=cx.update_window(window.into(),|_,window,cx| window.focused(cx)).unwrap().unwrap();
        assert_ne!(child_focus,header_focus,"Tab moves from the real header to its New thread affordance");
        cx.update_window(window.into(),|_,window,cx| window.focus_prev(cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(),|_,window,cx| assert_eq!(window.focused(cx),Some(header_focus))).unwrap();
        cx.update_window(window.into(),|_,window,cx| window.focus_next(cx)).unwrap();
        cx.run_until_parked();
        capture(&mut cx,"folder-child-activation-ready");
        cx.update_window(window.into(),|_,window,cx| assert_eq!(window.focused(cx),Some(child_focus))).unwrap();
        press_key(&mut cx,"enter");
        cx.run_until_parked();
        let barrier=state.read_with(&cx,|state,_| state.connections.destination(&target.project.owner,t3_core::auth::ORCHESTRATION_READ_SCOPE));
        executor.block_test(runtime.spawn(barrier)).unwrap().unwrap();
        cx.run_until_parked();
        state.read_with(&cx,|state,cx| {
            assert_eq!(state.environments.active.as_deref(),Some(target.project.owner.saved_id.as_str()));
            assert_eq!(state.draft_project.as_deref(),Some(target.project.project.id.as_str()));
            assert_eq!(state.project_preferences.expanded(&state.project_groups_for(GroupConsumer::Sidebar,cx)[0]),!expanded_before_draft,"New thread activation must not also toggle the header");
        });
        state.update(&mut cx,|state,cx| {
            state.environments.active=before.0.clone();
            state.install_active_environment(restore.0,restore.1,restore.2,ConnectionStatus::Connected,cx);
            state.selected_thread=before.1.clone(); state.draft_project=before.2.clone();
            let group=state.project_groups_for(GroupConsumer::Sidebar,cx)[0].clone();
            state.project_preferences.set_expanded(&group,expanded_before_draft);
            state.rebuild_sidebar(cx);
            cx.emit(crate::app_state::AppEvent::Selection);
        });
        cx.run_until_parked();
        state.read_with(&cx,|state,_| assert_eq!((state.environments.active.clone(),state.selected_thread.clone(),state.draft_project.clone()),before));
        let forbidden = |method:&str| matches!(method,
            t3_core::commands::method::DISPATCH_COMMAND | t3_core::commands::method::LAUNCH_THREAD | t3_core::projects::PROJECTS_MUTATE
            | t3_core::commands::method::SERVER_UPDATE_SETTINGS | t3_core::settings::keybindings::SERVER_UPSERT_KEYBINDING
            | t3_core::settings::keybindings::SERVER_REMOVE_KEYBINDING | "guestShare.create"
            | "projects.createFromThread" | "projects.createNew"
        );
        let mut commands=active_fixture.take_commands();
        for fixture in &mut fixtures { commands.extend(fixture.take_commands()); }
        assert!(!commands.iter().any(|method| forbidden(method)),"frames must not send writes or launch providers");
        std::fs::write(out.join("grouping-frames.json"),serde_json::to_vec_pretty(&json!({
            "themes":["light","dark"],"owners":["grouping-a","grouping-b"],"rawProjectId":"same-project","rawThreadId":"same-thread",
            "logicalGroups":1,"registrations":2,"folderWorkingShelf":false,"routeUnchanged":true,"rpcWrites":0,
            "settingsFrame":"projects-list-and-observed-row-click-detail","folderCollapseClick":true,"paletteFrame":"unfocused; native NSView input not verified","commandsObserved":commands
        })).unwrap()).unwrap();
        state.update(&mut cx,|state,_| state.stop_pump());
        drop(fixture_events);
    }
}
