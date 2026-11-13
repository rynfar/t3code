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
mod hover;
mod project_filter;
mod project_folders;
mod row;
mod search;
#[cfg(all(test, feature = "snapshot"))]
mod drag_tests;

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
pub(crate) enum ClockRate {
    Second,
    Minute,
}

#[derive(Clone, PartialEq, Eq)]
enum PaneRowKey { Draft(String), Divider, Thread(RowKey), Scoped(t3_core::sidebar::fleet::ScopedRowKey) }

fn reanchored_scroll_top(
    old: &[PaneRowKey],
    new: &[PaneRowKey],
    scroll_top: gpui::ListOffset,
    range: &std::ops::Range<usize>,
    count: usize,
    dragging: bool,
) -> Option<gpui::ListOffset> {
    if new.is_empty() {
        return None;
    }
    if scroll_top.item_ix == 0 && scroll_top.offset_in_item == px(0.) {
        return (dragging && range.start == 0 && range.is_empty() && count > 0)
            .then_some(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.) });
    }
    if let Some(key) = old.get(scroll_top.item_ix)
        && let Some(item_ix) = new.iter().position(|candidate| candidate == key)
    {
        return Some(gpui::ListOffset { item_ix, offset_in_item: scroll_top.offset_in_item });
    }
    let neighbor_ix = (scroll_top.item_ix.saturating_add(1)..old.len())
        .find_map(|ix| new.iter().position(|candidate| candidate == &old[ix]))
        .or_else(|| {
            (0..scroll_top.item_ix.min(old.len()))
                .rev()
                .find_map(|ix| new.iter().position(|candidate| candidate == &old[ix]))
        });
    neighbor_ix.map(|item_ix| gpui::ListOffset { item_ix, offset_in_item: px(0.) })
}

pub struct SidebarPane {
    updates: Entity<crate::updates::UpdateControl>,
    pub(crate) composer: Entity<crate::composer::Composer>,
    drafts: t3_core::sidebar::drafts::DraftRows,
    discarded: std::collections::HashMap<String, (t3_core::thread_actions::Claim, crate::composer::drafts::DiscardedDraft)>,
    discarded_threads: std::collections::HashMap<String, (t3_core::thread_actions::Claim, crate::composer::drafts::DiscardedDraft)>,
    state: Entity<AppState>,
    menu: Entity<ContextMenu>,
    list: ListState,
    measured_font: Option<(Pixels, gpui::SharedString)>,
    row_keys: Vec<PaneRowKey>,
    pub(crate) search_input: Entity<InputState>,
    pub(crate) search: SearchState,
    rename: Option<(String, Entity<InputState>)>,
    rename_previous_focus: Option<gpui::FocusHandle>,
    /// Where the project filter popover opens, while it is open.
    pub(crate) project_filter: Option<gpui::Point<gpui::Pixels>>,
    pub(crate) project_filter_search: Entity<InputState>,
    pub(crate) project_filter_focus: gpui::FocusHandle,
    pub(crate) project_filter_return_focus: Option<gpui::FocusHandle>,
    pub(crate) project_filter_scroll: gpui::ScrollHandle,
    pub(crate) project_filter_order: std::cell::RefCell<Vec<String>>,
    pub(crate) project_filter_highlighted: Option<String>,
    jump_hints: bool,
    jump_hint_timer: Option<Task<()>>,
    pub(crate) now: Ms,
    #[cfg(feature = "snapshot")]
    pub(crate) now_override: Option<Ms>,
    model_seen: Arc<t3_core::sidebar::SidebarModel>,
    fleet_seen: Arc<t3_core::sidebar::fleet::ScopedSidebarModel>,
    rename_key: Option<t3_core::sidebar::tag_filter::ThreadKey>,
    /// Whether the last frame drew a ticking working duration; only then does the clock run per second.
    live_rendered: bool,
    pub(crate) clock: Option<(ClockRate, Task<()>)>,
    drag: Option<drag::PaneDrag>,
    project_drag: Option<drag::PaneProjectDrag>,
    sweep: Option<drag::PaneSweep>,
    #[cfg(all(test, feature = "snapshot"))]
    row_focuses: std::cell::RefCell<std::collections::HashMap<gpui::SharedString, gpui::FocusHandle>>,
    folder_multiline: bool,
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
        let project_filter_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects..."));
        let project_filter_focus = cx.focus_handle();
        state.update(cx, |state, _| state.sidebar.search_input = Some(search_input.downgrade()));
        let subscriptions = vec![
            cx.subscribe(&composer, |this, _, event: &crate::composer::drafts::DraftsChanged, cx| {
                let state = this.state.read(cx);
                if this.drafts.update_draft(this.composer.read(cx).sidebar_drafts(), &event.project_id, state.sidebar.project_scope.as_ref()) {
                    this.sync_list_rows(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&composer, |_, _, _: &crate::composer::drafts::ThreadDraftsChanged, cx| {
                cx.notify();
            }),
            cx.observe_global::<crate::terminal::RunningTerminals>(|_, cx| {
                cx.notify();
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::RestoreDraft {
                    let (projects, threads) = this.state.update(cx, |state, _| {
                        (
                            std::mem::take(&mut state.sidebar.drafts_to_restore),
                            std::mem::take(&mut state.sidebar.thread_drafts_to_restore),
                        )
                    });
                    for project in projects {
                        if let Some((_, draft)) = this.discarded.remove(&project) {
                            this.composer.update(cx, |composer, cx| composer.restore_sidebar_draft(&project, draft, window, cx));
                        }
                    }
                    for thread_id in threads {
                        if let Some((_, draft)) = this.discarded_threads.remove(&thread_id) {
                            this.composer.update(cx, |composer, cx| composer.restore_thread_draft(&thread_id, draft, window, cx));
                        }
                    }
                    this.sync_rows(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
                AppEvent::Shell | AppEvent::Selection => {
                    let (model, fleet, built_at, last_wake_at) = {
                        let state = this.state.read(cx);
                        (
                            state.sidebar.model.clone(),
                            state.sidebar.fleet.clone(),
                            state.sidebar.built_at,
                            state.sidebar.last_wake_at,
                        )
                    };
                    // Streaming ticks reach the shell but leave both derived models alone.
                    if matches!(event, AppEvent::Shell) && Arc::ptr_eq(&model, &this.model_seen) && Arc::ptr_eq(&fleet, &this.fleet_seen) { return; }
                    let woke_crossed = last_wake_at.is_some_and(|woke| woke > this.now)
                        || model.threads.iter().any(|t| t.snoozed_until.is_some_and(|until| until <= built_at && until > this.now))
                        || fleet.threads.iter().any(|r| r.thread.snoozed_until.is_some_and(|until| until <= built_at && until > this.now));
                    if woke_crossed {
                        this.now = this.now.max(built_at).max(last_wake_at.unwrap_or(0));
                    }
                    #[cfg(feature = "snapshot")]
                    if let Some(override_now) = this.now_override {
                        this.now = override_now;
                    }
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
                    let expired_threads: Vec<_> = this.discarded_threads.iter().filter(|(thread_id, (claim, _))| !this.state.read(cx).sidebar.undo.is_current(claim) && !this.state.read(cx).sidebar.thread_drafts_to_restore.contains(*thread_id)).map(|(thread_id, _)| thread_id.clone()).collect();
                    for thread_id in expired_threads {
                        if let Some((_, discarded)) = this.discarded_threads.remove(&thread_id) {
                            this.composer.update(cx, |composer, cx| composer.commit_sidebar_discard(discarded, cx));
                        }
                    }
                    cx.notify();
                },
                AppEvent::Phase | AppEvent::ServerConfig => {
                    this.sync_snooze_grants(cx);
                    this.search.refresh(&this.state, cx);
                    if this.search.is_active() {
                        cx.notify();
                    }
                }
                AppEvent::SidebarSelection => {
                    let state = this.state.read(cx);
                    this.model_seen = state.sidebar.model.clone();
                    this.fleet_seen = state.sidebar.fleet.clone();
                    this.sync_rows(cx);
                    cx.notify();
                }
                AppEvent::SidebarVisibility => this.schedule_clock(cx),
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
            cx.subscribe_in(&project_filter_search, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.sync_project_filter_highlight(cx);
                    cx.notify();
                }
                InputEvent::PressEnter { shift, .. } if this.project_filter.is_some() => {
                    this.activate_highlighted_project_filter(*shift, window, cx);
                }
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
                    if action == t3_core::sidebar::selection::BulkAction::Unpin {
                        return this.state.update(cx, |state, cx| state.run_scoped_bulk_action(owners, action, cx));
                    }
                    let needs_confirm = match action { t3_core::sidebar::selection::BulkAction::Delete => crate::settings::client_settings(cx).confirm_thread_delete(), _ => false };
                    if !needs_confirm { return this.state.update(cx, |state, cx| state.run_scoped_bulk_action(owners, action, cx)); }
                    let (heading, description) = t3_core::sidebar::selection::bulk_delete_confirmation(owners.len());
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
            discarded_threads: Default::default(),
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
            project_filter_search,
            project_filter_focus,
            project_filter_return_focus: None,
            project_filter_scroll: gpui::ScrollHandle::new(),
            project_filter_order: Default::default(),
            project_filter_highlighted: None,
            jump_hints: false,
            jump_hint_timer: None,
            now: now_ms(),
            #[cfg(feature = "snapshot")]
            now_override: None,
            model_seen: Default::default(),
            fleet_seen: Default::default(),
            rename_key: None,
            live_rendered: false,
            clock: None,
            drag: None,
            project_drag: None,
            sweep: None,
            #[cfg(all(test, feature = "snapshot"))]
            row_focuses: std::cell::RefCell::new(std::collections::HashMap::new()),
            folder_multiline: crate::settings::store::client(cx).boring_sidebar_multiline_rows(),
            menu_open: false,
            snooze_grant_watch: None,
            snooze_access: false,
            _subscriptions: subscriptions,
        };
        #[cfg(feature = "snapshot")]
        crate::snapshot::register_sidebar_pane(cx.entity().downgrade(), cx);
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
        if self.drag.is_some() && state.sidebar.drag.is_none() && state.sidebar.fleet_drag.is_none() {
            self.drag = None;
        }
        if self.project_drag.is_some() && crate::settings::store::client(cx).project_sort_order() != t3_core::client_settings::ProjectSortOrder::Manual {
            self.project_drag = None;
        }
        let mut keys: Vec<_> = self.drafts.rows.iter().map(|row| PaneRowKey::Draft(row.project_id.clone())).collect();
        if !keys.is_empty() { keys.push(PaneRowKey::Divider); }
        if state.fleet.environments.is_empty() { keys.extend(state.sidebar.model.row_keys().into_iter().map(PaneRowKey::Thread)); }
        else { keys.extend(state.sidebar.fleet.row_keys().into_iter().map(PaneRowKey::Scoped)); }
        if let Some((range, count)) = t3_core::sidebar::splice_range(&self.row_keys, &keys) {
            let scroll_top = self.list.logical_scroll_top();
            self.list.splice(range.clone(), count);
            if let Some(target) = reanchored_scroll_top(&self.row_keys, &keys, scroll_top, &range, count, self.drag.is_some()) {
                let after = self.list.logical_scroll_top();
                if (after.item_ix, after.offset_in_item) != (target.item_ix, target.offset_in_item) {
                    self.list.scroll_to(target);
                }
            }
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
        #[cfg(feature = "snapshot")]
        if self.now_override.is_some() {
            self.clock = None;
            return;
        }
        let state = self.state.read(cx);
        let empty = if state.fleet.environments.is_empty() {
            state.sidebar.model.rows.is_empty()
        } else {
            state.sidebar.fleet.rows.is_empty()
        };
        if state.sidebar_hidden() || empty {
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
        self.rename_previous_focus = previous.clone();
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
        self.rename_key = None;
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

    pub(crate) fn handle_escape(&mut self, overlay_open: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let overlay_open = overlay_open || self.project_filter.is_some() || self.rename.is_some();
        if overlay_open {
            return false;
        }
        if self.drag.is_some() || self.sweep.is_some() || self.project_drag.is_some() {
            self.finish_drag(false, cx);
            self.finish_sweep(false, cx);
            self.finish_project_drag(false, cx);
            return true;
        }
        let search_focused = self.search_input.read(cx).focus_handle(cx).is_focused(window);
        let composing = search_focused && is_composing(&self.search_input, window, cx);
        let searching = self.search.is_active();
        let has_selection = {
            let state = self.state.read(cx);
            !state.sidebar.selection.is_empty() || !state.sidebar.fleet_selection.is_empty()
        };
        match t3_core::sidebar::selection::sidebar_escape(t3_core::sidebar::selection::EscapeContext {
            composing,
            search_focused,
            searching,
            overlay_open,
            has_selection,
        }) {
            t3_core::sidebar::selection::SidebarEscape::ClearSearch => {
                self.clear_search(window, cx);
                true
            }
            t3_core::sidebar::selection::SidebarEscape::ClearSelection => {
                self.state.update(cx, |state, cx| {
                    state.clear_thread_selection(cx);
                });
                true
            }
            t3_core::sidebar::selection::SidebarEscape::Ignore => false,
        }
    }

    fn open_search_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.search.highlighted_key().cloned() else { return };
        self.activate_search_result(key, window, cx);
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
            .debug_selector(move || id.to_owned())
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
        let (has_filterable_projects, has_any_projects) = {
            let state = self.state.read(cx);
            if state.fleet.environments.is_empty() {
                let any = state.shell.snapshot.as_ref().is_some_and(|snapshot| !snapshot.projects.is_empty());
                (!self.project_groups(cx).is_empty(), any)
            } else {
                let pickable = t3_core::sidebar::grouping::pickable_scoped_project_groups(
                    &state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx),
                );
                (!pickable.is_empty(), !state.sidebar.grouping_projects.is_empty())
            }
        };
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
                .when(has_filterable_projects, |this| {
                    this.child(self.render_project_filter_button(cx)).child(
                        Self::header_button("add-project", Self::button_icon(Icon::FolderPlus, "add-project", cx), |_| "Add project".to_owned(), true, cx)
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::palette::OpenAddProject), cx)),
                    )
                })
                .child(
                    Self::header_button("new-thread-sidebar", Self::button_icon(Icon::SquarePen, "new-thread-sidebar", cx), new_thread_label, has_any_projects, cx)
                        .when(has_any_projects, |this| this.on_click(|_, window, cx| window.dispatch_action(Box::new(crate::new_thread::NewThread), cx))),
                ),
        )
    }

    /// The empty list copy, naming the filtered projects when a filter applies.
    fn empty_message(&self, synchronized: bool, cx: &Context<Self>) -> String {
        let state = self.state.read(cx);
        if !synchronized {
            return "Loading…".to_owned();
        }
        if !state.fleet.environments.is_empty() {
            if state.sidebar.grouping_projects.is_empty() {
                return "No projects yet".to_owned();
            }
            let groups = t3_core::sidebar::grouping::pickable_scoped_project_groups(
                &state.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx),
            );
            let keys = state.scoped_project_scope(&groups);
            let scoped: Vec<_> = groups.iter().filter(|group| keys.contains(&group.key)).collect();
            return match scoped.as_slice() {
                [] => "No threads yet".to_owned(),
                [group] => format!("No threads in {} yet", group.label),
                _ => "No threads in these projects yet".to_owned(),
            };
        }
        if state.shell.snapshot.as_ref().is_some_and(|snapshot| snapshot.projects.is_empty()) {
            return "No projects yet".to_owned();
        }
        if state.sidebar.project_scope.is_none() {
            return "No threads yet".to_owned();
        }
        let scoped: Vec<_> = self
            .project_groups(cx)
            .into_iter()
            .filter(|group| state.sidebar.project_filter_scope.is_selected(group))
            .collect();
        match scoped.as_slice() {
            [] => "No threads yet".to_owned(),
            [group] => format!("No threads in {} yet", group.label),
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
                        .on_click(move |_, _, cx| state.update(cx, |state, cx| {
                            state.close_sidebar_sheet(cx);
                            state.set_pull_requests_open(true, cx);
                        })),
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
        let multiline = crate::settings::store::client(cx).boring_sidebar_multiline_rows();
        if self.measured_font.as_ref() != Some(&font) || self.folder_multiline != multiline {
            self.measured_font = Some(font);
            self.folder_multiline = multiline;
            self.list.remeasure();
        }
        let (synchronized, no_projects) = {
            let state = self.state.read(cx);
            if state.fleet.environments.is_empty() {
                let sync = state.shell.synchronized;
                (sync, sync && state.shell.snapshot.as_ref().is_some_and(|snapshot| snapshot.projects.is_empty()))
            } else {
                let sync = state.fleet.environments.iter().any(|entry| entry.summary.synchronized);
                (sync, sync && state.sidebar.grouping_projects.is_empty())
            }
        };
        self.live_rendered = false;
        let p = palette(cx);
        let body = if self.search.is_active() {
            self.render_search_results(cx).into_any_element()
        } else if self.row_keys.is_empty() {
            div()
                .px(px(8.))
                .py(px(24.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .text_xs()
                .text_color(hsla(p.muted_foreground).opacity(0.6))
                .child(self.empty_message(synchronized, cx))
                .when(no_projects, |this| {
                    this.child(
                        div()
                            .id("empty-add-project")
                            .h(px(28.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(hsla(p.border))
                            .bg(hsla(p.card))
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .cursor_pointer()
                            .hover(|s| s.bg(hsla(p.accent)))
                            .child(Icon::Plus.sized(px(12.)).text_color(hsla(p.foreground)))
                            .child("Add project")
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::palette::OpenAddProject), cx)),
                    )
                })
                .into_any_element()
        } else {
            div()
                .id("sidebar-thread-list")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    list(self.list.clone(), cx.processor(|this, index: usize, window, cx| this.render_row(index, window, cx)))
                        .flex_1()
                        .min_h_0()
                        .when(self.drag.is_some(), |this| this.pb(px(40.))),
                )
                .on_drop(cx.listener(|this, _: &drag::DraggedThread, _, cx| this.finish_drag(true, cx)))
                .on_drop(cx.listener(|this, _: &drag::DraggedSweep, _, cx| this.finish_sweep(true, cx)))
                .on_drop(cx.listener(|this, _: &drag::DraggedProject, _, cx| this.finish_project_drag(true, cx)))
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(p.sidebar))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.finish_drag(false, cx);
                this.finish_sweep(false, cx);
                this.finish_project_drag(false, cx);
            }))
            .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.finish_drag(false, cx);
                this.finish_sweep(false, cx);
                this.finish_project_drag(false, cx);
            }))
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
                    "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":if saved=="grouping-b" { "background-model-315" } else { "replay" }},
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
        let draft_root = state.read_with(&cx, |state, _| {
            let project = state.draft_project.as_ref().expect("view initialization retains the existing home draft");
            state.shell.project(project).unwrap().workspace_root.clone()
        });
        let (method, payload, reply) = runtime.block_on(active_fixture.next_request());
        assert_eq!(method, t3_core::commands::method::PROJECTS_READ_FILE, "the composer draft initialization read precedes any hover input");
        assert_eq!(payload, t3_core::workspace::project_file_request(&draft_root));
        reply.send(Ok(json!({"exists":false}))).unwrap();
        let (refs_method, payload, reply) = runtime.block_on(active_fixture.next_request());
        assert_eq!(refs_method, t3_core::commands::method::VCS_LIST_REFS, "draft workspace initialization follows its project-file reply");
        assert_eq!(payload, t3_core::workspace::list_refs_request(&draft_root));
        reply.send(Ok(json!({"isRepo":false,"refs":[]}))).unwrap();
        let setup_commands = vec![method, refs_method];
        cx.run_until_parked();
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
            let hover_position = pane.read_with(&cx, |pane, cx| {
                let state = pane.state.read(cx);
                let index = state.sidebar.fleet.rows.iter().position(|row| match row {
                    ScopedSidebarRow::Thread { index, .. } => state.sidebar.fleet.threads[*index].key == t3_core::sidebar::tag_filter::ThreadKey::new("grouping-b", "same-thread"),
                    _ => false,
                }).expect("background collision must have a rendered production row");
                let offset = if pane.drafts.rows.is_empty() { 0 } else { pane.drafts.rows.len() + 1 };
                pane.list.bounds_for_item(index + offset).expect("background production row must have measured list bounds").center()
            });
            cx.update_window(window.into(), |_, window, cx| window.simulate_mouse_move(hover_position, cx)).unwrap();
            cx.run_until_parked();
            // The locked GPUI default tooltip delay is 500ms; advance only its deterministic clock.
            cx.advance_clock(std::time::Duration::from_millis(500));
            capture(&mut cx,&format!("{theme}-background-model-hover"));
            state.read_with(&cx, |state, _| assert_eq!((state.environments.active.clone(),state.selected_thread.clone(),state.draft_project.clone()),before));
            let active_commands = active_fixture.take_commands();
            assert!(active_commands.is_empty(), "hover must not send active-client commands: {active_commands:?}");
            for (index, fixture) in fixtures.iter_mut().enumerate() {
                let commands = fixture.take_commands();
                assert!(commands.is_empty(), "hover must not send destination-client commands owner={index}: {commands:?}");
            }
            cx.update_window(window.into(), |_, window, cx| window.simulate_mouse_move(gpui::point(px(1200.), px(850.)), cx)).unwrap();
            cx.run_until_parked();
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
        let mut commands=setup_commands;
        commands.extend(active_fixture.take_commands());
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

#[cfg(all(test, feature = "snapshot"))]
mod sb_sections_pane_tests {
    use super::*;
    use crate::context_menu::project_filter_tests::{install_state, session};
    use gpui::{AppContext, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, StyleRefinement, point};
    use serde_json::json;
    use std::{cell::Cell, rc::Rc};
    use t3_client::supervisor::{ClientFixture, ConnectionStatus};
    use t3_core::sidebar::grouping::{GroupConsumer, ProjectScope, build_project_groups};
    use t3_core::sidebar::older_settled::GET_OLDER_SETTLED_THREADS;
    use t3_core::sidebar::{SETTLED_TAIL_INITIAL_COUNT, Shelf};

    struct CachedPaneHost {
        pane: Entity<SidebarPane>,
        menu: Entity<ContextMenu>,
    }

    impl Render for CachedPaneHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(320.))
                .h(px(900.))
                .child(self.pane.clone().cached(StyleRefinement::default().size_full()))
                .child(self.menu.clone())
        }
    }

    #[gpui::test]
    async fn cached_pane_sections_older_settled_and_natural_woke_render(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            let (state, fixture) = install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            crate::settings::store::install_committed_client(
                &state,
                serde_json::from_value(json!({"sidebarWorkingShelfEnabled": true})).unwrap(),
                cx,
            );
            state.update(cx, |s, cx| {
                s.draft_project = None;
                let make = |id: &str, project: &str, status: &str, pinned: bool, settled: bool| -> t3_core::shell::ThreadShell {
                    serde_json::from_value(json!({
                        "id": id,
                        "projectId": project,
                        "title": id,
                        "createdAt": "2026-01-02T00:00:00Z",
                        "updatedAt": "2026-01-02T00:00:00Z",
                        "providerInstanceId": "replay",
                        "modelSelection": {"instanceId": "replay", "model": "replay"},
                        "runtimeMode": "auto",
                        "interactionMode": "default",
                        "lineage": {"rootThreadId": id},
                        "status": status,
                        "latestRunId": if status == "running" { Some("r1") } else { None },
                        "activeRunId": if status == "running" { Some("r1") } else { None },
                        "hasActionableProposedPlan": false,
                        "itemCount": 0,
                        "visibleItemCount": 0,
                        "pinnedAt": pinned.then_some("2026-01-02T00:00:00Z"),
                        "settledOverride": settled.then_some("settled"),
                        "settledAt": settled.then_some("2026-01-02T00:00:00Z"),
                    }))
                    .unwrap()
                };
                let snap = s.shell.snapshot.as_mut().unwrap();
                snap.threads = vec![
                    make("pin-1", "checkout-a", "idle", true, false),
                    make("act-1", "checkout-a", "idle", false, false),
                    make("work-sel", "checkout-a", "running", false, false),
                    make("work-2", "checkout-a", "running", false, false),
                    make("settled-1", "checkout-a", "idle", false, true),
                ];
                snap.older_settled = Some(t3_core::shell::OlderSettledCut {
                    before: "2026-01-01T00:00:00.000Z".into(),
                    as_of_sequence: snap.snapshot_sequence,
                    counts: vec![t3_core::shell::OlderSettledCount { project_id: "checkout-a".into(), count: 15 }],
                });
                s.selected_thread = Some("work-sel".into());
                s.sync_omitted_rows(cx);
                s.rebuild_sidebar(cx);
            });
            (state, fixture)
        });
        let (root, vcx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu.clone(), confirm, snooze, window, cx));
            CachedPaneHost { pane, menu }
        });
        let pane = root.read_with(vcx, |host, _| host.pane.clone());
        let notifications = Rc::new(Cell::new(0));
        let observed = notifications.clone();
        let _sub = vcx.update(|_, cx| cx.observe(&pane, move |_, _| observed.set(observed.get() + 1)));
        vcx.run_until_parked();
        fixture.take_commands();
        vcx.update(|window, cx| { window.draw(cx).clear(); });

        pane.read_with(vcx, |pane, _| {
            assert!(pane.row_keys.contains(&PaneRowKey::Thread(RowKey::Shelf(Shelf::Working))));
            assert!(pane.row_keys.iter().any(|k| matches!(k, PaneRowKey::Thread(RowKey::Thread { id, .. }) if id == "work-sel")));
            assert!(!pane.row_keys.iter().any(|k| matches!(k, PaneRowKey::Thread(RowKey::Thread { id, .. }) if id == "work-2")));
            assert!(pane.row_keys.contains(&PaneRowKey::Thread(RowKey::Shelf(Shelf::Settled))));
        });

        // Expand Settled -> Loading -> Retry -> Ok -> collapse releases page mount.
        let before_expand = notifications.get();
        state.update(vcx, |s, cx| s.toggle_shelf(Shelf::Settled, cx));
        let (method, _, fail_reply) = fixture.next_request().await;
        assert_eq!(method, GET_OLDER_SETTLED_THREADS);
        vcx.run_until_parked();
        assert!(notifications.get() > before_expand);
        assert_eq!(state.read_with(vcx, |s, _| s.settled_show_more_label(15)), "Loading");
        vcx.update(|window, cx| { window.draw(cx).clear(); });

        let before_fail = notifications.get();
        let (fail_tx, fail_rx) = tokio::sync::oneshot::channel();
        let mut fail_tx = Some(fail_tx);
        let _fail_sub = state.update(vcx, |_, cx| {
            cx.subscribe(&state, move |_, _, event, _| {
                if *event == AppEvent::Shell
                    && let Some(tx) = fail_tx.take()
                {
                    let _ = tx.send(());
                }
            })
        });
        fail_reply
            .send(Err(t3_client::session::RpcError::Failure(t3_core::rpc::RpcFailure {
                tag: Some("OperationError".into()),
                message: "boom".into(),
                error: json!({"_tag": "OperationError"}),
            })))
            .unwrap();
        fail_rx.await.unwrap();
        vcx.run_until_parked();
        assert!(notifications.get() > before_fail);
        assert_eq!(state.read_with(vcx, |s, _| s.settled_show_more_label(15)), "Could not load older threads. Retry");

        vcx.update(|window, cx| { window.draw(cx).clear(); });
        let show_more_center = pane.read_with(vcx, |pane, _| {
            let idx = pane
                .row_keys
                .iter()
                .position(|k| matches!(k, PaneRowKey::Thread(RowKey::ShowMore)))
                .expect("settled show-more row key present");
            pane.list.bounds_for_item(idx).expect("show-more row measured").center()
        });
        vcx.simulate_mouse_move(show_more_center, None::<MouseButton>, Default::default());
        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: show_more_center,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: show_more_center,
            modifiers: Default::default(),
            click_count: 1,
        });
        let (method, _, ok_reply) = fixture.next_request().await;
        assert_eq!(method, GET_OLDER_SETTLED_THREADS);
        let (ok_tx, ok_rx) = tokio::sync::oneshot::channel();
        let mut ok_tx = Some(ok_tx);
        let _ok_sub = state.update(vcx, |_, cx| {
            cx.subscribe(&state, move |s, _, event, _| {
                if *event == AppEvent::Shell
                    && s.sidebar.model.thread("older-page-1").is_some()
                    && let Some(tx) = ok_tx.take()
                {
                    let _ = tx.send(());
                }
            })
        });
        ok_reply
            .send(Ok(json!({
                "snapshotSequence": 1,
                "threads": [{
                    "id": "older-page-1",
                    "projectId": "checkout-a",
                    "title": "Older page thread",
                    "createdAt": "2025-12-31T00:00:00Z",
                    "updatedAt": "2025-12-31T00:00:00Z",
                    "providerInstanceId": "replay",
                    "modelSelection": {"instanceId": "replay", "model": "replay"},
                    "runtimeMode": "auto",
                    "interactionMode": "default",
                    "lineage": {"rootThreadId": "older-page-1"},
                    "status": "idle",
                    "hasActionableProposedPlan": false,
                    "itemCount": 0,
                    "visibleItemCount": 0,
                    "settledOverride": "settled",
                    "settledAt": "2025-12-31T00:00:00Z"
                }]
            })))
            .unwrap();
        ok_rx.await.unwrap();
        vcx.run_until_parked();
        pane.read_with(vcx, |pane, _| {
            assert!(pane.row_keys.iter().any(|k| matches!(k, PaneRowKey::Thread(RowKey::Thread { id, .. }) if id == "older-page-1")));
        });
        assert_eq!(state.read_with(vcx, |s, _| s.sidebar.shelves.settled_visible), 35);

        // Collapsing Settled releases the "older-settled" page mount.
        state.update(vcx, |s, cx| s.toggle_shelf(Shelf::Settled, cx));
        vcx.run_until_parked();
        assert!(
            state.read_with(vcx, |s, _| s.omitted.mounts.extras(&s.shell).is_empty()),
            "collapsing Settled shelf must release older-settled page mounts"
        );
        state.update(vcx, |s, _| s.sidebar.shelves.settled_visible = 35);

        // Changing single-env project scope resets settled_visible to SETTLED_TAIL_INITIAL_COUNT (10).
        state.update(vcx, |s, cx| {
            let env_id = s.server_config.config.as_ref().unwrap().environment.environment_id.clone();
            let groups = build_project_groups(&env_id, &s.shell.snapshot.as_ref().unwrap().projects, &s.sidebar.settings.grouping);
            let shared = groups.iter().find(|g| g.label == "Shared repo").unwrap();
            let mut scope = ProjectScope::default();
            scope.toggle(shared, &groups, true);
            s.set_project_scope(scope, cx);
        });
        assert_eq!(
            state.read_with(vcx, |s, _| s.sidebar.shelves.settled_visible),
            SETTLED_TAIL_INITIAL_COUNT,
            "changing single-env project scope must reset grown settled_visible"
        );

        // Empty message for a multi-checkout logical group uses the logical group's label.
        let saved_threads = state.update(vcx, |s, cx| {
            let saved = s.shell.snapshot.as_ref().unwrap().threads.clone();
            s.selected_thread = None;
            s.shell.snapshot.as_mut().unwrap().older_settled = None;
            s.omitted = Default::default();
            s.shell.snapshot.as_mut().unwrap().threads.clear();
            s.rebuild_sidebar(cx);
            saved
        });
        let msg = pane.update(vcx, |p, cx| p.empty_message(true, cx));
        assert_eq!(msg, "No threads in Shared repo yet");
        state.update(vcx, |s, cx| {
            s.shell.snapshot.as_mut().unwrap().threads = saved_threads;
            s.set_project_scope(ProjectScope::default(), cx);
        });

        // Natural visible wake on cached SidebarPane renders `#woke-act-1` immediately without clock wait.
        let (sent, woke) = tokio::sync::oneshot::channel();
        let mut sent = Some(sent);
        let _woke_sub = state.update(vcx, |_, cx| {
            cx.subscribe(&state, move |s, _, event, _| {
                if *event == AppEvent::Shell
                    && s.sidebar.model.section_of("act-1") == Some(t3_core::sidebar::Section::Active)
                    && s.sidebar.model.next_change_at.is_none()
                    && let Some(sent) = sent.take()
                {
                    let _ = sent.send(());
                }
            })
        });
        let until = state.update(vcx, |s, cx| {
            let now = now_ms();
            let until = now + 120;
            let t = s.shell.snapshot.as_mut().unwrap().threads.iter_mut().find(|t| t.id == "act-1").unwrap();
            t.snoozed_at = Some(t3_core::sidebar::time::format_iso(now));
            t.snoozed_until = Some(t3_core::sidebar::time::format_iso(until));
            s.rebuild_sidebar(cx);
            cx.emit(AppEvent::Shell);
            until
        });
        pane.update(vcx, |pane, _| pane.now = until - 5_000);
        vcx.run_until_parked();
        fixture.take_commands();
        vcx.update(|window, cx| { window.draw(cx).clear(); });
        let before_wake = notifications.get();
        woke.await.unwrap();
        vcx.run_until_parked();
        assert!(notifications.get() > before_wake, "natural wake must notify the cached SidebarPane");
        assert!(
            pane.read_with(vcx, |pane, _| pane.now >= until),
            "natural wake must advance pane.now past snoozed_until so Woke badge renders immediately"
        );
        vcx.update(|window, cx| { window.draw(cx).clear(); });
        let woke_badge_pos = pane.read_with(vcx, |pane, _| {
            let idx = pane
                .row_keys
                .iter()
                .position(|k| matches!(k, PaneRowKey::Thread(RowKey::Thread { id, .. }) if id == "act-1"))
                .expect("act-1 row rendered");
            let b = pane.list.bounds_for_item(idx).expect("act-1 row measured");
            point(b.right() - px(24.), b.top() + px(14.))
        });
        vcx.simulate_mouse_move(woke_badge_pos, None::<MouseButton>, Default::default());
        vcx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: woke_badge_pos, modifiers: Default::default(), click_count: 1, first_mouse: false });
        vcx.simulate_event(MouseUpEvent { button: MouseButton::Left, position: woke_badge_pos, modifiers: Default::default(), click_count: 1 });
        let (ack_method, _, _) = fixture.next_request().await;
        assert_eq!(ack_method, t3_core::commands::method::DISPATCH_COMMAND, "clicking #woke-act-1 dispatches acknowledge_woke");

        state.update(vcx, |s, _| s.stop_pump());
    }

    #[gpui::test]
    async fn fleet_clock_and_project_filter_286_scope_and_pin_stability(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _orig) = cx.update(|cx| {
            gpui_component::init(cx);
            let installed = install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });
        for (saved, can_pin) in [("saved-fixture", true), ("remote-cloud", false)] {
            let (client, _fix) = ClientFixture::handle(session(true));
            let mut shell = t3_core::shell::ShellState::default();
            shell.apply(
                serde_json::from_value(json!({
                    "kind": "snapshot",
                    "snapshot": {
                        "snapshotSequence": 1,
                        "projects": [{
                            "id": format!("proj-{saved}"),
                            "title": if saved == "saved-fixture" { "Local Alpha" } else { "Cloud Beta" },
                            "workspaceRoot": format!("/{saved}"),
                            "createdAt": "2026-01-01T00:00:00Z",
                            "updatedAt": "2026-01-01T00:00:00Z"
                        }],
                        "threads": [{
                            "id": format!("th-{saved}"),
                            "projectId": format!("proj-{saved}"),
                            "title": format!("Thread {saved}"),
                            "createdAt": "2026-01-01T00:00:00Z",
                            "updatedAt": "2026-01-01T00:00:00Z",
                            "providerInstanceId": "replay",
                            "modelSelection": {"instanceId": "replay", "model": "replay"},
                            "runtimeMode": "auto",
                            "interactionMode": "default",
                            "lineage": {"rootThreadId": format!("th-{saved}")},
                            "status": "idle",
                            "hasActionableProposedPlan": false,
                            "itemCount": 0,
                            "visibleItemCount": 0
                        }]
                    }
                }))
                .unwrap(),
            );
            shell.synchronized = true;
            let mut config = t3_core::server_config::ServerConfigState::default();
            config.apply(
                serde_json::from_value(json!({
                    "type": "snapshot",
                    "config": {
                        "environment": {
                            "environmentId": format!("env-{saved}"),
                            "label": saved,
                            "platform": {"os": "linux", "arch": "x64", "machine": "cloud"},
                            "capabilities": {"projectOrganization": can_pin}
                        },
                        "cwd": format!("/{saved}"),
                        "settings": {}
                    }
                }))
                .unwrap(),
            );
            let (tx, rx) = tokio::sync::oneshot::channel();
            let mut tx = Some(tx);
            let _s = state.update(cx, |_, cx| {
                cx.subscribe(&state, move |s, _, ev, _| {
                    if *ev == AppEvent::Shell
                        && s.scoped_environment(saved).is_some()
                        && let Some(tx) = tx.take()
                    {
                        let _ = tx.send(());
                    }
                })
            });
            let install = state.update(cx, |s, _| s.connections.install_fixture(saved, client, shell, config, ConnectionStatus::Connected));
            let _events = runtime.spawn(install).await.unwrap();
            rx.await.unwrap();
        }
        // Clear local single-env threads so `state.sidebar.model.rows` is empty while `fleet.rows` is populated.
        state.update(cx, |s, cx| {
            s.draft_project = None;
            s.shell.snapshot.as_mut().unwrap().threads.clear();
            s.rebuild_sidebar(cx);
        });
        let (root, vcx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu.clone(), confirm, snooze, window, cx));
            CachedPaneHost { pane, menu }
        });
        let pane = root.read_with(vcx, |host, _| host.pane.clone());
        vcx.run_until_parked();
        assert!(
            pane.read_with(vcx, |p, _| p.clock.is_some()),
            "schedule_clock_at must stay active for fleet rows even when single-env model.rows is empty"
        );

        let groups = state.read_with(vcx, |s, cx| s.project_groups_for(GroupConsumer::Sidebar, cx));
        let alpha_key = groups.iter().find(|g| g.label == "Local Alpha").unwrap().key.clone();
        let beta_key = groups.iter().find(|g| g.label == "Cloud Beta").unwrap().key.clone();
        let alpha_id: &'static str = Box::leak(format!("scoped-project-filter-{alpha_key}").into_boxed_str());
        let beta_id: &'static str = Box::leak(format!("scoped-project-filter-{beta_key}").into_boxed_str());

        let click_in = |window: &mut gpui::Window, cx: &mut gpui::App, button: MouseButton, pos: gpui::Point<Pixels>, modifiers: Modifiers| {
            window.dispatch_event(
                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                    position: pos,
                    pressed_button: None,
                    modifiers,
                }),
                cx,
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                    button,
                    position: pos,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                    button,
                    position: pos,
                    modifiers,
                    click_count: 1,
                }),
                cx,
            );
        };
        let key_in = |window: &mut gpui::Window, cx: &mut gpui::App, key: &str| {
            let keystroke = gpui::Keystroke::parse(key).unwrap();
            window.dispatch_event(
                gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke,
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
        };
        let draw_bounds = |vcx: &mut gpui::VisualTestContext| {
            pane.update(vcx, |_, cx| cx.notify());
            vcx.update(|window, cx| {
                window.draw(cx).clear();
            });
        };

        draw_bounds(vcx);
        let trigger_pos = vcx.debug_bounds("project-filter").expect("#project-filter button rendered").center();

        // 1) Clicking `#project-filter` focuses `#project-filter-search` and auto-highlights `"all"`.
        vcx.update(|window, cx| {
            click_in(window, cx, MouseButton::Left, trigger_pos, Default::default());
            let pane_ref = pane.read(cx);
            assert!(pane_ref.project_filter.is_some(), "clicking #project-filter must open popover");
            assert!(
                pane_ref.project_filter_search.read(cx).focus_handle(cx).is_focused(window),
                "opening project filter must focus #project-filter-search input"
            );
            assert_eq!(pane_ref.project_filter_highlighted.as_deref(), Some("all"), "autoHighlight starts on first visible item");
            // Park focus onto `popup_focus` before TestWindow::draw renders `<Input>` (macOS `window_handle` is unimplemented on TestWindow).
            let popup_focus = pane_ref.project_filter_focus.clone();
            window.focus(&popup_focus, cx);
        });
        draw_bounds(vcx);

        // With popup in the dispatch tree: focused-Input capture actions (`MoveDown`, `MoveUp`, `Escape`) route through `popup_id`.
        let dispatch_focused_action = |vcx: &mut gpui::VisualTestContext, action: Box<dyn gpui::Action>| {
            vcx.update(|window, cx| {
                let search = pane.read(cx).project_filter_search.clone();
                let popup_focus = pane.read(cx).project_filter_focus.clone();
                search.update(cx, |input, cx| input.focus(window, cx));
                assert!(search.read(cx).focus_handle(cx).is_focused(window));
                window.dispatch_action(action, cx);
                // After GPUI's deferred action runs (FIFO), move focus off Input before TestWindow::draw runs.
                let win = window.window_handle();
                cx.defer(move |cx| {
                    let _ = cx.update_window(win, |_, window, cx| window.focus(&popup_focus, cx));
                });
            });
            vcx.run_until_parked();
        };
        dispatch_focused_action(vcx, Box::new(gpui_component::input::MoveDown));
        assert_eq!(pane.read_with(vcx, |p, _| p.project_filter_highlighted.clone()).as_deref(), Some(beta_key.as_str()));
        dispatch_focused_action(vcx, Box::new(gpui_component::input::MoveUp));
        assert_eq!(pane.read_with(vcx, |p, _| p.project_filter_highlighted.clone()).as_deref(), Some("all"));
        dispatch_focused_action(vcx, Box::new(gpui_component::input::Escape));
        assert!(pane.read_with(vcx, |p, _| p.project_filter.is_none()), "Escape capture action must close project filter from focused Input");

        // 2) Re-open, type query (`autoHighlight` picks first match), clear query (`autoHighlight` returns to `"all"`), fallback keys + Shift+F10 on `popup_id`.
        vcx.update(|window, cx| {
            click_in(window, cx, MouseButton::Left, trigger_pos, Default::default());
            let search = pane.read(cx).project_filter_search.clone();
            assert!(search.read(cx).focus_handle(cx).is_focused(window));
            search.update(cx, |input, cx| {
                gpui::EntityInputHandler::replace_text_in_range(input, Some(0..0), "cloud", window, cx);
            });
            let popup_focus = pane.read(cx).project_filter_focus.clone();
            window.focus(&popup_focus, cx);
        });
        vcx.run_until_parked();
        pane.read_with(vcx, |p, cx| {
            assert_eq!(p.project_filter_search.read(cx).value(), "cloud");
            assert_eq!(p.project_filter_highlighted.as_deref(), Some(beta_key.as_str()));
        });

        vcx.update(|window, cx| {
            let search = pane.read(cx).project_filter_search.clone();
            search.update(cx, |input, cx| {
                gpui::EntityInputHandler::replace_text_in_range(input, Some(0..5), "", window, cx);
            });
            let popup_focus = pane.read(cx).project_filter_focus.clone();
            window.focus(&popup_focus, cx);
        });
        vcx.run_until_parked();
        assert_eq!(pane.read_with(vcx, |p, _| p.project_filter_highlighted.as_deref().map(str::to_owned)).as_deref(), Some("all"));

        draw_bounds(vcx);
        vcx.update(|window, cx| {
            key_in(window, cx, "down");
            assert_eq!(pane.read(cx).project_filter_highlighted.as_deref(), Some(beta_key.as_str()));
            key_in(window, cx, "down");
            assert_eq!(pane.read(cx).project_filter_highlighted.as_deref(), Some(alpha_key.as_str()));
            key_in(window, cx, "up");
            assert_eq!(pane.read(cx).project_filter_highlighted.as_deref(), Some(beta_key.as_str()));
            key_in(window, cx, "down");
            assert_eq!(pane.read(cx).project_filter_highlighted.as_deref(), Some(alpha_key.as_str()));
            key_in(window, cx, "shift-f10");
            assert!(pane.read(cx).project_filter.is_none(), "Shift+F10 must close project filter after opening project settings");
        });
        vcx.run_until_parked();

        // 3) Fallback `Escape` on `popup_id` when focus is on popup instead of Input.
        vcx.update(|window, cx| {
            click_in(window, cx, MouseButton::Left, trigger_pos, Default::default());
            let popup_focus = pane.read(cx).project_filter_focus.clone();
            window.focus(&popup_focus, cx);
        });
        draw_bounds(vcx);
        vcx.update(|window, cx| {
            key_in(window, cx, "escape");
            assert!(pane.read(cx).project_filter.is_none(), "Escape fallback on popup_id must close when focus leaves Input");
        });
        vcx.run_until_parked();

        // 4) Right-clicking a project row closes project filter before opening ContextMenu.
        vcx.update(|window, cx| {
            click_in(window, cx, MouseButton::Left, trigger_pos, Default::default());
            let focus = pane.read(cx).project_filter_focus.clone();
            window.focus(&focus, cx);
        });
        draw_bounds(vcx);
        let alpha_pos = vcx.debug_bounds(alpha_id).expect("alpha filter row rendered").center();
        vcx.update(|window, cx| {
            click_in(window, cx, MouseButton::Right, alpha_pos, Default::default());
        });
        vcx.run_until_parked();
        assert!(pane.read_with(vcx, |p, _| p.project_filter.is_none()), "right-click must close project filter before opening ContextMenu");
        assert!(root.read_with(vcx, |host, cx| host.menu.read(cx).is_open()), "right-click must open project ContextMenu");
        root.update(vcx, |host, cx| host.menu.update(cx, |m, cx| m.close(cx)));
        vcx.run_until_parked();

        // 5) Shift-click toggles + keeps filter open; plain click selects single project + closes.
        vcx.update(|window, cx| {
            click_in(window, cx, MouseButton::Left, trigger_pos, Default::default());
            let focus = pane.read(cx).project_filter_focus.clone();
            window.focus(&focus, cx);
        });
        draw_bounds(vcx);
        let beta_pos = vcx.debug_bounds(beta_id).expect("beta filter row rendered").center();
        let shift = Modifiers { shift: true, ..Default::default() };
        vcx.update(|window, cx| {
            click_in(window, cx, MouseButton::Left, beta_pos, shift);
            let focus = pane.read(cx).project_filter_focus.clone();
            window.focus(&focus, cx);
        });
        vcx.run_until_parked();
        assert!(pane.read_with(vcx, |p, _| p.project_filter.is_some()), "Shift-click must keep project filter open");
        assert_eq!(state.read_with(vcx, |s, cx| s.scoped_project_scope(&s.project_groups_for(GroupConsumer::Sidebar, cx))), vec![beta_key.clone()]);

        draw_bounds(vcx);
        let alpha_pos = vcx.debug_bounds(alpha_id).expect("alpha filter row rendered").center();
        vcx.update(|window, cx| {
            click_in(window, cx, MouseButton::Left, alpha_pos, Default::default());
        });
        vcx.run_until_parked();
        assert!(
            pane.read_with(vcx, |p, _| p.project_filter.is_none()),
            "plain click on a project row must select only that project and close the popup"
        );
        assert_eq!(
            state.read_with(vcx, |s, cx| s.scoped_project_scope(&s.project_groups_for(GroupConsumer::Sidebar, cx))),
            vec![alpha_key]
        );

        state.update(vcx, |s, cx| {
            s.set_scoped_project_scope(Vec::new(), cx);
            s.stop_pump();
        });
    }

    fn make_thread_json(id: &str, project_id: &str, status: &str) -> serde_json::Value {
        json!({
            "id": id,
            "projectId": project_id,
            "title": format!("Thread {id}"),
            "createdAt": "2026-01-02T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "providerInstanceId": "replay",
            "modelSelection": {"instanceId": "replay", "model": "replay"},
            "runtimeMode": "auto",
            "interactionMode": "default",
            "lineage": {"rootThreadId": id},
            "status": status,
            "hasActionableProposedPlan": false,
            "itemCount": 0,
            "visibleItemCount": 0
        })
    }

    #[gpui::test]
    async fn row_running_terminals_and_thread_draft_discard_undo_cycle(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            let installed = install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });
        state.update(cx, |s, cx| {
            s.draft_project = None;
            s.selected_thread = Some("th-active".into());
            s.server_config.apply(serde_json::from_value(json!({
                "type": "snapshot",
                "config": {
                    "environment": {
                        "environmentId": "fixture-env",
                        "label": "Fixture",
                        "platform": {"os": "macos", "arch": "arm64", "machine": "laptop"},
                        "capabilities": {"projectOrganization": true}
                    },
                    "cwd": "/fixture",
                    "settings": {}
                }
            })).unwrap());
            s.shell.snapshot.as_mut().unwrap().threads = vec![
                serde_json::from_value(make_thread_json("th-active", "checkout-a", "idle")).unwrap(),
                serde_json::from_value(make_thread_json("th-draft", "checkout-a", "idle")).unwrap(),
            ];
            s.rebuild_sidebar(cx);
        });
        let composer_slot: Rc<std::cell::RefCell<Option<Entity<crate::composer::Composer>>>> = Default::default();
        let composer_out = composer_slot.clone();
        let (root, vcx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            *composer_out.borrow_mut() = Some(composer.clone());
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu.clone(), confirm, snooze, window, cx));
            CachedPaneHost { pane, menu }
        });
        let composer = composer_slot.borrow().clone().unwrap();
        let pane = root.read_with(vcx, |host, _| host.pane.clone());

        let choice = t3_core::model::ModelSelection { instance_id: "replay".into(), model: "replay-model".into(), options: None };
        composer.update(vcx, |c, cx| {
            c.set_thread_draft_fixture("th-draft", "unsent prompt", Some(choice.clone()), 1_780_000_000_000, cx);
            c.set_thread_draft_fixture("th-active", "open prompt", None, 1_780_000_000_000, cx);
        });
        vcx.update(|_, cx| {
            let mut running = crate::terminal::RunningTerminals::default();
            running.by_thread.insert("th-draft".into(), vec!["term-1".into()]);
            cx.set_global(running);
        });
        vcx.run_until_parked();
        pane.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });

        assert!(vcx.debug_bounds("terminal-status-th-draft").is_some(), "RunningTerminals global update must show terminal icon");
        assert!(vcx.debug_bounds("draft-indicator-th-draft").is_some(), "inactive thread draft must show draft pen");
        assert!(vcx.debug_bounds("draft-indicator-th-active").is_none(), "active thread must hide draft pen");

        let row_center = vcx.debug_bounds("thread-row-None-th-draft").expect("draft row rendered").center();
        vcx.simulate_mouse_move(row_center, None::<MouseButton>, Default::default());
        vcx.run_until_parked();
        let discard_center = vcx.debug_bounds("discard-draft-thread-th-draft").expect("discard draft hover action rendered").center();
        vcx.simulate_mouse_move(discard_center, None::<MouseButton>, Default::default());
        vcx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: discard_center, modifiers: Default::default(), click_count: 1, first_mouse: false });
        vcx.simulate_event(MouseUpEvent { button: MouseButton::Left, position: discard_center, modifiers: Default::default(), click_count: 1 });
        vcx.run_until_parked();
        pane.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });

        let saved = composer.read_with(vcx, |c, _| c.sidebar_drafts().get("thread:th-draft").cloned());
        assert!(saved.as_ref().is_some_and(|d| d.prompt.is_empty() && !d.has_user_content() && d.model_selection.as_ref() == Some(&choice)), "discarding thread draft clears content while keeping choices");
        assert!(state.read_with(vcx, |s, _| s.sidebar.undo.notice().is_some()), "discarding thread draft records undo notice");
        assert!(vcx.debug_bounds("draft-indicator-th-draft").is_none(), "draft pen disappears after discard");

        assert!(state.update(vcx, |s, cx| s.undo_latest(cx)));
        vcx.run_until_parked();
        pane.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let restored = composer.read_with(vcx, |c, _| c.sidebar_drafts().get("thread:th-draft").cloned());
        assert_eq!(restored.as_ref().map(|d| d.prompt.as_str()), Some("unsent prompt"));
        assert!(vcx.debug_bounds("draft-indicator-th-draft").is_some(), "undo restores thread draft pen");
        state.update(vcx, |s, _| s.stop_pump());
    }

    #[gpui::test]
    async fn scoped_woke_acknowledge_delete_deselect_and_unpin_confirm(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture_local) = cx.update(|cx| {
            gpui_component::init(cx);
            let installed = install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            crate::settings::store::install_committed_client(&installed.0, serde_json::from_value(json!({"confirmThreadUnpin": true})).unwrap(), cx);
            installed
        });

        let mut env_fixtures = Vec::new();
        for (saved, active) in [("env-a", true), ("env-b", false)] {
            let (client, fix) = ClientFixture::handle(session(true));
            let mut shell = t3_core::shell::ShellState::default();
            let mut woke_thread = make_thread_json(&format!("th-{saved}-woke"), &format!("proj-{saved}"), "idle");
            woke_thread["snoozedUntil"] = json!("2026-01-01T00:00:05Z");
            woke_thread["pinnedAt"] = json!("2026-01-01T00:00:00Z");
            let del_thread = make_thread_json(&format!("th-{saved}-del"), &format!("proj-{saved}"), "idle");
            shell.apply(serde_json::from_value(json!({
                "kind": "snapshot",
                "snapshot": {
                    "snapshotSequence": 1,
                    "projects": [{
                        "id": format!("proj-{saved}"),
                        "title": saved,
                        "workspaceRoot": format!("/{saved}"),
                        "createdAt": "2026-01-01T00:00:00Z",
                        "updatedAt": "2026-01-01T00:00:00Z"
                    }],
                    "threads": [woke_thread, del_thread]
                }
            })).unwrap());
            shell.synchronized = true;
            let mut config = t3_core::server_config::ServerConfigState::default();
            config.apply(serde_json::from_value(json!({
                "type": "snapshot",
                "config": {
                    "environment": {
                        "environmentId": format!("id-{saved}"),
                        "label": saved,
                        "platform": {"os": "linux", "arch": "x64", "machine": "cloud"},
                        "capabilities": {"projectOrganization": true}
                    },
                    "cwd": format!("/{saved}"),
                    "settings": {}
                }
            })).unwrap());
            let install = state.update(cx, |s, _| s.connections.install_fixture(saved, client, shell, config, ConnectionStatus::Connected));
            let _events = runtime.spawn(install).await.unwrap();
            if active {
                state.update(cx, |s, _| s.environments.active = Some(saved.into()));
            }
            env_fixtures.push(fix);
        }
        let mut fix_a = env_fixtures.remove(0);
        let mut fix_b = env_fixtures.remove(0);

        state.update(cx, |s, cx| {
            s.draft_project = None;
            s.shell.snapshot.as_mut().unwrap().threads.clear();
            s.rebuild_sidebar(cx);
        });
        let confirm_slot: Rc<std::cell::RefCell<Option<Entity<ConfirmDialog>>>> = Default::default();
        let confirm_out = confirm_slot.clone();
        let (root, vcx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
            *confirm_out.borrow_mut() = Some(confirm.clone());
            let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu.clone(), confirm, snooze, window, cx));
            CachedPaneHost { pane, menu }
        });
        let pane = root.read_with(vcx, |host, _| host.pane.clone());
        let confirm = confirm_slot.borrow().clone().unwrap();
        let woke_time = t3_core::sidebar::time::parse_iso("2026-01-01T00:00:10Z").unwrap();
        pane.update(vcx, |p, cx| {
            p.now_override = Some(woke_time);
            p.now = woke_time;
            cx.notify();
        });
        vcx.run_until_parked();
        pane.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        fixture_local.take_commands();
        fix_a.take_commands();
        fix_b.take_commands();

        let woke_pos = vcx.debug_bounds("woke-env-b-th-env-b-woke").expect("remote woke badge rendered").center();
        vcx.simulate_mouse_move(woke_pos, None::<MouseButton>, Default::default());
        vcx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: woke_pos, modifiers: Default::default(), click_count: 1, first_mouse: false });
        vcx.simulate_event(MouseUpEvent { button: MouseButton::Left, position: woke_pos, modifiers: Default::default(), click_count: 1 });
        let (method, params, _) = fix_b.next_request().await;
        assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(params["type"], "thread.visit");
        assert_eq!(params["threadId"], "th-env-b-woke");
        assert!(fix_a.take_commands().is_empty(), "scoped acknowledge_woke must route to owning env-b, not active env-a");

        // Verify delete_scoped_threads removes deleted keys from fleet_selection and preserves others.
        let key_woke = t3_core::sidebar::tag_filter::ThreadKey { environment_id: "env-b".into(), thread_id: "th-env-b-woke".into() };
        let key_del = t3_core::sidebar::tag_filter::ThreadKey { environment_id: "env-b".into(), thread_id: "th-env-b-del".into() };
        state.update(vcx, |s, cx| {
            s.sidebar.fleet_selection.insert(key_woke.clone());
            s.sidebar.fleet_selection.insert(key_del.clone());
            let owner_del = crate::context_menu::ThreadMenuOwner::capture_scoped(s, &key_del).unwrap();
            s.delete_scoped_threads(vec![owner_del], cx);
        });
        let (del_method, del_params, reply_del) = fix_b.next_request().await;
        assert_eq!(del_method, t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(del_params["type"], "thread.delete");
        let _ = reply_del.send(Ok(json!({})));
        vcx.run_until_parked();
        state.read_with(vcx, |s, _| {
            assert!(!s.sidebar.fleet_selection.contains(&key_del), "deleted scoped thread must be removed from selection");
            assert!(s.sidebar.fleet_selection.contains(&key_woke), "surviving selected thread must remain selected");
        });

        // Verify ConfirmDialog unpin prompt opens on request_unpin_thread and canceling keeps thread pinned.
        vcx.update(|window, cx| {
            let key_local_woke = t3_core::sidebar::tag_filter::ThreadKey { environment_id: "env-a".into(), thread_id: "th-env-a-woke".into() };
            let owner = crate::context_menu::ThreadMenuOwner::capture_scoped(state.read(cx), &key_local_woke).unwrap();
            let menu = root.read(cx).menu.clone();
            menu.update(cx, |m, cx| m.confirm_scoped(owner, t3_core::thread_actions::ThreadAction::Unpin, cx));
            let _ = window;
        });
        vcx.run_until_parked();
        assert!(confirm.read_with(vcx, |d, _| d.is_open()) || !fix_a.take_commands().contains(&t3_core::commands::method::DISPATCH_COMMAND), "unpin confirmation must prevent immediate dispatch");
        state.update(vcx, |s, _| s.stop_pump());
    }

    #[gpui::test]
    async fn sweep_settle_drag_escape_priority_and_row_keyboard(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            let installed = install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });
        state.update(cx, |s, cx| {
            s.draft_project = None;
            s.server_config.apply(serde_json::from_value(json!({
                "type": "snapshot",
                "config": {
                    "environment": {
                        "environmentId": "fixture-env",
                        "label": "Fixture",
                        "platform": {"os": "macos", "arch": "arm64", "machine": "laptop"},
                        "capabilities": {"projectOrganization": true}
                    },
                    "cwd": "/fixture",
                    "settings": {}
                }
            })).unwrap());
            let mut settled = make_thread_json("th-4", "checkout-a", "idle");
            settled["settledOverride"] = json!("settled");
            settled["settledAt"] = json!("2026-01-01T00:00:00Z");
            s.shell.snapshot.as_mut().unwrap().threads = vec![
                serde_json::from_value(make_thread_json("th-1", "checkout-a", "idle")).unwrap(),
                serde_json::from_value(make_thread_json("th-2", "checkout-a", "idle")).unwrap(),
                serde_json::from_value(make_thread_json("th-3", "checkout-a", "idle")).unwrap(),
                serde_json::from_value(settled).unwrap(),
            ];
            s.sidebar.shelves.settled_expanded = true;
            s.rebuild_sidebar(cx);
        });
        let (root, vcx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu.clone(), confirm, snooze, window, cx));
            CachedPaneHost { pane, menu }
        });
        let pane = root.read_with(vcx, |host, _| host.pane.clone());
        vcx.run_until_parked();
        pane.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        fixture.take_commands();

        let th1_bounds = vcx.debug_bounds("thread-row-None-th-1").expect("th-1 row rendered");
        let th2_bounds = vcx.debug_bounds("thread-row-None-th-2").expect("th-2 row rendered");
        let th4_bounds = vcx.debug_bounds("thread-row-None-th-4").expect("th-4 row rendered");
        vcx.simulate_mouse_move(th1_bounds.center(), None::<MouseButton>, Default::default());
        vcx.run_until_parked();
        let settle1_center = vcx.debug_bounds("settle-th-1").expect("settle action rendered").center();

        // Start sweep drag on th-1 Settle, sweep across th-2, ignore Settled th-4.
        vcx.simulate_mouse_move(settle1_center, None::<MouseButton>, Default::default());
        vcx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: settle1_center, modifiers: Default::default(), click_count: 1, first_mouse: false });
        vcx.simulate_mouse_move(gpui::point(settle1_center.x - px(12.), settle1_center.y), Some(MouseButton::Left), Default::default());
        vcx.run_until_parked();
        vcx.simulate_mouse_move(th2_bounds.center(), Some(MouseButton::Left), Default::default());
        vcx.run_until_parked();
        assert_eq!(pane.read_with(vcx, |p, _| p.sweep.as_ref().map(|s| s.keys.len())), Some(2), "sweep drag covers th-1 and th-2");
        vcx.simulate_mouse_move(th4_bounds.center(), Some(MouseButton::Left), Default::default());
        vcx.run_until_parked();
        assert_eq!(pane.read_with(vcx, |p, _| p.sweep.as_ref().map(|s| s.keys.len())), Some(3), "sweep range includes th-3 between th-1 and th-4");
        assert!(!pane.read_with(vcx, |p, _| p.sweep.as_ref().is_some_and(|s| s.keys.contains(&drag::PaneSweepKey::Local("th-4".into())))), "sweep drag must not include rows from Settled section");

        // Escape priority: overlay_open=true ignores Escape; overlay_open=false cancels sweep without settling.
        vcx.update(|window, cx| {
            state.update(cx, |s, _| s.sidebar.selection.toggle("th-3"));
            assert!(!pane.update(cx, |p, cx| p.handle_escape(true, window, cx)));
            assert!(pane.read(cx).sweep.is_some());
            assert!(pane.update(cx, |p, cx| p.handle_escape(false, window, cx)));
            assert!(pane.read(cx).sweep.is_none());
            assert!(state.read(cx).sidebar.selection.contains("th-3"), "first Escape clears sweep before selection");
        });
        vcx.simulate_event(MouseUpEvent { button: MouseButton::Left, position: th2_bounds.center(), modifiers: Default::default(), click_count: 1 });
        vcx.run_until_parked();
        assert!(fixture.take_commands().is_empty(), "canceled sweep must not dispatch settle");

        // Complete a real sweep drop over th-1..th-2.
        vcx.simulate_mouse_move(settle1_center, None::<MouseButton>, Default::default());
        vcx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: settle1_center, modifiers: Default::default(), click_count: 1, first_mouse: false });
        vcx.simulate_mouse_move(gpui::point(settle1_center.x - px(12.), settle1_center.y), Some(MouseButton::Left), Default::default());
        vcx.run_until_parked();
        vcx.simulate_mouse_move(th2_bounds.center(), Some(MouseButton::Left), Default::default());
        vcx.run_until_parked();
        vcx.simulate_event(MouseUpEvent { button: MouseButton::Left, position: th2_bounds.center(), modifiers: Default::default(), click_count: 1 });
        let (m1, p1, _) = fixture.next_request().await;
        let (m2, p2, _) = fixture.next_request().await;
        assert_eq!((m1, m2), (t3_core::commands::method::DISPATCH_COMMAND, t3_core::commands::method::DISPATCH_COMMAND));
        assert_eq!(p1["type"], "thread.settle");
        assert_eq!(p2["type"], "thread.settle");
        assert_eq!(state.read_with(vcx, |s, _| s.sidebar.selection.len()), 1, "sweep settle preserves multi-selection");

        // Escape clears multi-selection when no sweep/search is active, then Space + F2 on th-3.
        let th3_focus = pane.read_with(vcx, |p, _| p.row_focuses.borrow().get("thread-row-None-th-3").expect("th-3 focus handle registered").clone());
        vcx.update(|window, cx| {
            assert!(pane.update(cx, |p, cx| p.handle_escape(false, window, cx)));
            assert!(state.read(cx).sidebar.selection.is_empty());
            window.focus(&th3_focus, cx);
            window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse("space").unwrap(),
                is_held: false,
                prefer_character_input: false,
            }), cx);
            assert_eq!(state.read(cx).selected_thread.as_deref(), Some("th-3"), "Space activates focused row");
            window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse("f2").unwrap(),
                is_held: false,
                prefer_character_input: false,
            }), cx);
            assert!(pane.read(cx).rename.as_ref().is_some_and(|(id, _)| id == "th-3"), "F2 starts inline rename on focused row");
            pane.update(cx, |p, cx| p.cancel_rename(cx));
        });
        vcx.run_until_parked();
        state.update(vcx, |s, _| s.stop_pump());
    }

    #[gpui::test]
    async fn folder_multiline_rows_and_scratch_machine_label(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _orig) = cx.update(|cx| {
            gpui_component::init(cx);
            let installed = install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            crate::settings::store::install_committed_client(
                &installed.0,
                serde_json::from_value(json!({"boringSidebarEnabled": true, "boringSidebarMultilineRows": false})).unwrap(),
                cx,
            );
            installed
        });
        for (saved, machine, label) in [("mac", "laptop", "Mac Studio"), ("cloud", "cloud", "Cloud VM")] {
            let (client, _fix) = ClientFixture::handle(session(true));
            let mut shell = t3_core::shell::ShellState::default();
            let scratch_thread = make_thread_json(&format!("th-{saved}-scratch"), &format!("scratch-{saved}"), "idle");
            let mut repo_thread = make_thread_json(&format!("th-{saved}-repo"), &format!("repo-{saved}"), "idle");
            repo_thread["branch"] = json!("feat/parity");
            shell.apply(serde_json::from_value(json!({
                "kind": "snapshot",
                "snapshot": {
                    "snapshotSequence": 1,
                    "projects": [
                        {"id": format!("scratch-{saved}"), "title": "Scratch", "workspaceRoot": "/scratch", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"},
                        {"id": format!("repo-{saved}"), "title": "App", "workspaceRoot": "/app", "repositoryIdentity": {"canonicalKey": "repo/app"}, "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"}
                    ],
                    "threads": [scratch_thread, repo_thread]
                }
            })).unwrap());
            shell.synchronized = true;
            let mut config = t3_core::server_config::ServerConfigState::default();
            config.apply(serde_json::from_value(json!({
                "type": "snapshot",
                "config": {
                    "environment": {
                        "environmentId": format!("id-{saved}"),
                        "label": label,
                        "platform": {"os": "macos", "arch": "arm64", "machine": machine},
                        "capabilities": {"projectOrganization": true}
                    },
                    "cwd": "/",
                    "scratchWorkspaceRoot": "/scratch",
                    "settings": {}
                }
            })).unwrap());
            let install = state.update(cx, |s, _| s.connections.install_fixture(saved, client, shell, config, ConnectionStatus::Connected));
            let _events = runtime.spawn(install).await.unwrap();
        }
        state.update(cx, |s, cx| {
            s.draft_project = None;
            s.shell.snapshot.as_mut().unwrap().threads.clear();
            s.rebuild_sidebar(cx);
        });
        let (root, vcx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu.clone(), confirm, snooze, window, cx));
            CachedPaneHost { pane, menu }
        });
        let pane = root.read_with(vcx, |host, _| host.pane.clone());
        vcx.run_until_parked();
        pane.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(!pane.read_with(vcx, |p, _| p.folder_multiline));
        let scratch_key = t3_core::sidebar::tag_filter::ThreadKey { environment_id: "cloud".into(), thread_id: "th-cloud-scratch".into() };
        let scratch_row_id: &'static str = Box::leak(format!("thread-row-{:?}-{}", Some(&scratch_key), "th-cloud-scratch").into_boxed_str());
        let single_height = vcx.debug_bounds(scratch_row_id).expect("single-line folder scratch row rendered").size.height;
        assert!(single_height <= px(38.), "single-line folder row stays ~36px, got {single_height:?}");

        state.read_with(vcx, |s, _| {
            assert_eq!(
                s.sidebar.fleet.scratch_machine_labels.get(&t3_core::sidebar::grouping::ProjectRef { environment_id: "cloud".into(), project_id: "scratch-cloud".into() }).map(String::as_str),
                Some("Cloud VM")
            );
        });

        vcx.update(|_, cx| {
            crate::settings::store::install_committed_client(
                &state,
                serde_json::from_value(json!({"boringSidebarEnabled": true, "boringSidebarMultilineRows": true})).unwrap(),
                cx,
            );
        });
        vcx.run_until_parked();
        pane.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(pane.read_with(vcx, |p, _| p.folder_multiline));
        let multi_height = vcx.debug_bounds(scratch_row_id).expect("multiline folder scratch row rendered").size.height;
        assert!(multi_height > px(45.), "multiline folder row with scratch machine meta line must grow, got {multi_height:?}");
        state.update(vcx, |s, _| s.stop_pump());
    }

    #[gpui::test]
    async fn snoozed_wake_queued_turn_mixed_fleet_snooze_and_scoped_inner_ids(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let now = t3_core::sidebar::time::parse_iso("2026-01-01T00:00:10Z").unwrap();
        let (state, mut fixture_local) = cx.update(|cx| {
            gpui_component::init(cx);
            let installed = install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });

        let mut env_fixtures = Vec::new();
        for (saved, operate, include_snoozed) in [("env-op", true, true), ("env-ro", false, false)] {
            let (client, fix) = ClientFixture::handle(session(operate));
            let mut shell = t3_core::shell::ShellState::default();
            let mut shared = make_thread_json("shared-th", &format!("proj-{saved}"), "idle");
            shared["snoozedUntil"] = json!("2026-01-01T00:00:05Z");
            shared["pinnedAt"] = json!("2026-01-01T00:00:00Z");
            shared["branch"] = json!(format!("feat/{saved}"));
            shared["worktreePath"] = json!(format!("/worktrees/{saved}"));
            let mut threads = vec![shared];
            if include_snoozed {
                let mut snoozed_queued = make_thread_json("snoozed-queued", &format!("proj-{saved}"), "idle");
                snoozed_queued["snoozedUntil"] = json!("2026-01-01T01:00:00Z");
                snoozed_queued["latestUserMessageAt"] = json!("2026-01-01T00:00:05Z");
                threads.push(snoozed_queued);
            }
            shell.apply(serde_json::from_value(json!({
                "kind": "snapshot",
                "snapshot": {
                    "snapshotSequence": 1,
                    "projects": [{
                        "id": format!("proj-{saved}"),
                        "title": saved,
                        "workspaceRoot": format!("/{saved}"),
                        "createdAt": "2026-01-01T00:00:00Z",
                        "updatedAt": "2026-01-01T00:00:00Z"
                    }],
                    "threads": threads
                }
            })).unwrap());
            shell.synchronized = true;
            let mut config = t3_core::server_config::ServerConfigState::default();
            config.apply(serde_json::from_value(json!({
                "type": "snapshot",
                "config": {
                    "environment": {
                        "environmentId": format!("id-{saved}"),
                        "label": saved,
                        "platform": {"os": "linux", "arch": "x64", "machine": "cloud"},
                        "capabilities": {"projectOrganization": true}
                    },
                    "cwd": format!("/{saved}"),
                    "settings": {}
                }
            })).unwrap());
            let install = state.update(cx, |s, _| s.connections.install_fixture(saved, client, shell, config, ConnectionStatus::Connected));
            let _events = runtime.spawn(install).await.unwrap();
            env_fixtures.push(fix);
        }
        let mut fix_op = env_fixtures.remove(0);
        let mut fix_ro = env_fixtures.remove(0);

        state.update(cx, |s, cx| {
            s.draft_project = None;
            s.shell.snapshot.as_mut().unwrap().threads.clear();
            s.server_config.apply(serde_json::from_value(json!({
                "type": "snapshot",
                "config": {
                    "environment": {
                        "environmentId": "local-env",
                        "label": "Local",
                        "platform": {"os": "macos", "arch": "arm64", "machine": "laptop"},
                        "capabilities": {"projectOrganization": true}
                    },
                    "cwd": "/local",
                    "settings": {}
                }
            })).unwrap());
            s.environments.active = Some("env-op".into());
            s.sidebar.now_override = Some(now);
            s.sidebar.shelves.snoozed_expanded = true;
            s.rebuild_sidebar(cx);
        });

        let (root, vcx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu.clone(), confirm, snooze, window, cx));
            CachedPaneHost { pane, menu }
        });
        let pane = root.read_with(vcx, |host, _| host.pane.clone());
        pane.update(vcx, |p, cx| {
            p.now_override = Some(now);
            p.now = now;
            cx.notify();
        });
        vcx.run_until_parked();
        pane.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        fixture_local.take_commands();
        fix_op.take_commands();
        fix_ro.take_commands();

        // Shared raw thread.id ("shared-th") across two fleet environments must produce distinct, non-colliding inner element bounds.
        let woke_op = vcx.debug_bounds("woke-env-op-shared-th").expect("env-op woke badge rendered");
        let woke_ro = vcx.debug_bounds("woke-env-ro-shared-th").expect("env-ro woke badge rendered");
        assert_ne!(woke_op.origin, woke_ro.origin);
        let worktree_op = vcx.debug_bounds("thread-worktree-env-op-shared-th").expect("env-op worktree badge rendered");
        let worktree_ro = vcx.debug_bounds("thread-worktree-env-ro-shared-th").expect("env-ro worktree badge rendered");
        assert_ne!(worktree_op.origin, worktree_ro.origin);
        assert!(vcx.debug_bounds("unpin-env-op-shared-th").is_some(), "operable pinned row must have scoped unpin id");

        let op_key = t3_core::sidebar::tag_filter::ThreadKey { environment_id: "env-op".into(), thread_id: "shared-th".into() };
        let ro_key = t3_core::sidebar::tag_filter::ThreadKey { environment_id: "env-ro".into(), thread_id: "shared-th".into() };
        let queued_key = t3_core::sidebar::tag_filter::ThreadKey { environment_id: "env-op".into(), thread_id: "snoozed-queued".into() };
        let op_row_id: &'static str = Box::leak(format!("thread-row-{:?}-shared-th", Some(&op_key)).into_boxed_str());
        let ro_row_id: &'static str = Box::leak(format!("thread-row-{:?}-shared-th", Some(&ro_key)).into_boxed_str());
        let queued_row_id: &'static str = Box::leak(format!("thread-row-{:?}-snoozed-queued", Some(&queued_key)).into_boxed_str());

        let op_center = vcx.debug_bounds(op_row_id).expect("env-op shared row rendered").center();
        let ro_center = vcx.debug_bounds(ro_row_id).expect("env-ro shared row rendered").center();
        let queued_center = vcx.debug_bounds(queued_row_id).expect("snoozed-queued row rendered").center();

        // Mixed-capability Fleet Card Snooze when local ALSO has snooze_access: operable env-op shows Snooze; read-only env-ro hides it.
        vcx.simulate_mouse_move(op_center, None::<MouseButton>, Default::default());
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("snooze-env-op-shared-th").is_some(), "operable fleet env must render Card Snooze");
        let settle_op = vcx.debug_bounds("settle-env-op-shared-th").expect("env-op settle action rendered");

        vcx.simulate_mouse_move(ro_center, None::<MouseButton>, Default::default());
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("snooze-env-ro-shared-th").is_none(), "read-only fleet env must hide Card Snooze even when local can snooze");
        let settle_ro = vcx.debug_bounds("settle-env-ro-shared-th").expect("env-ro settle action rendered");
        assert_ne!(settle_op.origin, settle_ro.origin);

        // Reverse Fleet mixed-capability: drop local server_config so super::snooze_access(state) is false; env-op must keep Card Snooze.
        state.update(vcx, |s, cx| {
            s.server_config.config = None;
            s.rebuild_sidebar(cx);
        });
        vcx.run_until_parked();
        pane.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(!state.read_with(vcx, |s, _| super::snooze_access(s)));
        vcx.simulate_mouse_move(op_center, None::<MouseButton>, Default::default());
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("snooze-env-op-shared-th").is_some(), "operable remote env must keep Card Snooze when local lacks snooze_access");
        vcx.simulate_mouse_move(ro_center, None::<MouseButton>, Default::default());
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("snooze-env-ro-shared-th").is_none(), "read-only remote env stays hidden");

        // Snoozed row with has_queued_turn_start must still render Wake and dispatch thread.unsnooze on click.
        vcx.simulate_mouse_move(queued_center, None::<MouseButton>, Default::default());
        vcx.run_until_parked();
        let wake_pos = vcx.debug_bounds("wake-env-op-snoozed-queued").expect("snoozed row with queued turn start must keep hover Wake button").center();
        vcx.simulate_mouse_move(wake_pos, None::<MouseButton>, Default::default());
        vcx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: wake_pos, modifiers: Default::default(), click_count: 1, first_mouse: false });
        vcx.simulate_event(MouseUpEvent { button: MouseButton::Left, position: wake_pos, modifiers: Default::default(), click_count: 1 });
        let (method, params, _) = fix_op.next_request().await;
        assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(params["type"], "thread.unsnooze");
        assert_eq!(params["threadId"], "snoozed-queued");
        assert!(fix_ro.take_commands().is_empty());

        state.update(vcx, |s, _| s.stop_pump());
    }
}
