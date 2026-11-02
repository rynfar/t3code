//! Transcript view: `t3_core::timeline` turns the projection into rows, a bottom-aligned
//! `list()` renders them so streaming keeps the tail pinned. Rows are shared and keep their
//! identity across updates, so only rows whose pointer changed are spliced and remeasured.
//! Item 0 is the "Load earlier turns" control while there is history to load.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

mod find;

use gpui::{Context, Entity, IntoElement, ListAlignment, ListState, ParentElement, Render, Styled, Subscription, Window, actions, div, list, prelude::*, px};
use t3_core::history::HistoryControl;
use t3_core::timeline::{
    EntriesCache, GET_TURN_ITEM, RowKind, RowsCache, RowsInput, ThreadActivity, TimelineRow, WorkLogEntry, decode_detail, derive_activity,
    derive_rows, detail_request, detail_revision, needs_detail_fetch, rows_equal, update_entries,
};

use crate::app_state::{AppEvent, AppState};
use crate::theme::{hsla, palette};
use crate::transcript::markdown::MarkdownStore;
use crate::transcript::{Clock, Detail, RowEnv, TranscriptUi, WorkingTimer, history_control, render_group_member, render_row};

actions!(thread_view, [FindInThread]);

pub const CONTENT_MAX_WIDTH: f32 = 736.;
const DETAIL_CACHE_LIMIT: usize = 64;

pub struct ThreadView {
    state: Entity<AppState>,
    list: ListState,
    rows: Vec<Arc<TimelineRow>>,
    entries_cache: EntriesCache,
    rows_cache: RowsCache,
    activity: ThreadActivity,
    ui: TranscriptUi,
    markdown: MarkdownStore,
    thread_id: Option<String>,
    workspace_root: Option<String>,
    history: Option<HistoryControl>,
    details: HashMap<String, Detail>,
    group_lists: RefCell<HashMap<String, ListState>>,
    working_timer: Option<Entity<WorkingTimer>>,
    find: Option<find::FindUi>,
    clock: Clock,
    window_height: f32,
    _subscription: Subscription,
}

impl ThreadView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
            AppEvent::Thread | AppEvent::Selection => this.sync(cx),
            AppEvent::History => this.sync_history(cx),
            _ => {}
        });
        ThreadView {
            state,
            list: ListState::new(0, ListAlignment::Bottom, px(600.)),
            rows: Vec::new(),
            entries_cache: EntriesCache::default(),
            rows_cache: RowsCache::default(),
            activity: ThreadActivity::default(),
            ui: TranscriptUi::default(),
            markdown: MarkdownStore::default(),
            thread_id: None,
            workspace_root: None,
            history: None,
            details: HashMap::new(),
            group_lists: RefCell::new(HashMap::new()),
            working_timer: None,
            find: None,
            clock: Clock::now(),
            window_height: 0.,
            _subscription: subscription,
        }
    }

    fn header_count(&self) -> usize {
        usize::from(self.history.is_some())
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let selected = state.selected_thread.clone();
        if selected == self.thread_id && state.thread.is_merging_history() {
            return;
        }
        if selected != self.thread_id {
            self.thread_id = selected;
            self.ui = TranscriptUi::default();
            self.markdown.clear();
            self.entries_cache = EntriesCache::default();
            self.rows_cache = RowsCache::default();
            self.details.clear();
            self.group_lists.borrow_mut().clear();
            self.rows.clear();
            self.history = None;
            self.find = None;
            self.list.reset(0);
        }
        let state = self.state.read(cx);
        match state.thread.projection.as_ref() {
            Some(projection) => {
                update_entries(&mut self.entries_cache, &projection.visible_turn_items, &projection.attempts, &projection.nodes, &projection.plans);
                self.activity = derive_activity(projection);
                let thread = &projection.thread;
                self.workspace_root =
                    thread.worktree_path.clone().or_else(|| state.shell.project(&thread.project_id).map(|p| p.workspace_root.clone()));
            }
            None => {
                self.entries_cache = EntriesCache::default();
                self.activity = ThreadActivity::default();
            }
        }
        self.sync_history(cx);
        self.rederive(cx);
        self.refresh_open_details(cx);
        self.refresh_find_after_change(cx);
    }

    /// The control above the first row: shown, hidden, or relabelled without touching rows.
    fn sync_history(&mut self, cx: &mut Context<Self>) {
        let next = self.state.read(cx).thread.history_control();
        if next == self.history {
            return;
        }
        let (before, after) = (usize::from(self.history.is_some()), usize::from(next.is_some()));
        self.history = next;
        if before == after {
            self.list.remeasure_items(0..after);
        } else {
            self.list.splice(0..before, after);
        }
        cx.notify();
    }

    pub fn load_earlier(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.load_older_history(cx));
    }

    fn rederive(&mut self, cx: &mut Context<Self>) {
        let input = RowsInput {
            entries: self.entries_cache.entries(),
            activity: &self.activity,
            expanded_run_ids: &self.ui.expanded_runs,
            expanded_attempt_ids: &self.ui.expanded_attempts,
            expanded_work_group_ids: &self.ui.expanded_groups,
        };
        let next = derive_rows(&input, &mut self.rows_cache);
        self.sync_timer(&next, cx);
        let same = |a: &Arc<TimelineRow>, b: &Arc<TimelineRow>| Arc::ptr_eq(a, b) || rows_equal(a, b);
        let prefix = self.rows.iter().zip(&next).take_while(|(a, b)| same(a, b)).count();
        if prefix == self.rows.len() && prefix == next.len() {
            return;
        }
        let max_suffix = self.rows.len().min(next.len()) - prefix;
        let suffix = self.rows.iter().rev().zip(next.iter().rev()).take(max_suffix).take_while(|(a, b)| same(a, b)).count();
        let offset = self.header_count();
        let old_range = offset + prefix..offset + self.rows.len() - suffix;
        let new_count = next.len() - suffix - prefix;
        self.rows = next;
        self.list.splice(old_range, new_count);
        let open: std::collections::HashSet<&str> =
            self.rows.iter().filter(|row| matches!(row.kind, RowKind::Work { expanded_group: true, .. })).map(|row| row.id.as_str()).collect();
        self.group_lists.borrow_mut().retain(|id, _| open.contains(id.as_str()));
        cx.notify();
    }

    /// One timer view per working stretch; nothing else in the transcript ticks.
    fn sync_timer(&mut self, rows: &[Arc<TimelineRow>], cx: &mut Context<Self>) {
        let working = rows.iter().find_map(|row| match (&row.kind, &row.created_at) {
            (RowKind::Working { compacting }, Some(started)) => Some((started.clone(), *compacting)),
            _ => None,
        });
        match working {
            Some((started, compacting)) if self.working_timer.as_ref().is_some_and(|timer| timer.read(cx).matches(&started, compacting)) => {}
            Some((started, compacting)) => self.working_timer = Some(cx.new(|cx| WorkingTimer::new(started, compacting, cx))),
            None => self.working_timer = None,
        }
    }

    fn find_entry(&self, id: &str) -> Option<Arc<WorkLogEntry>> {
        self.rows.iter().find_map(|row| match &row.kind {
            RowKind::Work { entries, .. } | RowKind::WorkLive { entries, .. } => entries.iter().find(|entry| entry.id == id).cloned(),
            _ => None,
        })
    }

    /// Fetches withheld output for an open row: once while its item runs and again when it
    /// settles (`turnItemDetailRevision`).
    fn fetch_detail(&mut self, entry: &WorkLogEntry, cx: &mut Context<Self>) {
        let Some(item) = entry.item.clone().filter(|item| needs_detail_fetch(item)) else { return };
        let revision = detail_revision(&item);
        if self.details.get(&item.id).is_some_and(|detail| detail.revision() == revision) {
            return;
        }
        if self.details.len() >= DETAIL_CACHE_LIMIT {
            let open = &self.ui.expanded_entries;
            self.details.retain(|id, _| open.contains(id));
        }
        self.details.insert(item.id.clone(), Detail::Loading { revision: revision.clone() });
        let request = self.state.read(cx).rpc(GET_TURN_ITEM, detail_request(&item));
        let (id, entry_id) = (item.id.clone(), entry.id.clone());
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |view, cx| {
                if view.details.get(&id).is_none_or(|detail| detail.revision() != revision) {
                    return;
                }
                let detail = match result.map(decode_detail) {
                    Ok(Ok(Some(item))) => Detail::Loaded { revision, item: Arc::new(item) },
                    Ok(Ok(None)) => Detail::Missing { revision },
                    Ok(Err(error)) => Detail::Failed { revision, message: error.to_string() },
                    Err(error) => Detail::Failed { revision, message: error.to_string() },
                };
                view.details.insert(id, detail);
                view.remeasure_entry(&entry_id);
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_open_details(&mut self, cx: &mut Context<Self>) {
        let open: Vec<Arc<WorkLogEntry>> = self.ui.expanded_entries.iter().filter_map(|id| self.find_entry(id)).collect();
        for entry in open {
            self.fetch_detail(&entry, cx);
        }
    }

    pub fn toggle_run(&mut self, run_id: &str, cx: &mut Context<Self>) {
        toggle(&mut self.ui.expanded_runs, run_id);
        self.rederive(cx);
    }

    pub fn toggle_attempt(&mut self, attempt_id: &str, cx: &mut Context<Self>) {
        toggle(&mut self.ui.expanded_attempts, attempt_id);
        self.rederive(cx);
    }

    pub fn toggle_group(&mut self, group_id: &str, _row_id: &str, cx: &mut Context<Self>) {
        toggle(&mut self.ui.expanded_groups, group_id);
        self.rederive(cx);
    }

    /// Opening one entry changes only its own row's height, and fetches withheld output.
    pub fn toggle_entry(&mut self, key: &str, cx: &mut Context<Self>) {
        if toggle(&mut self.ui.expanded_entries, key)
            && let Some(entry) = self.find_entry(key)
        {
            self.fetch_detail(&entry, cx);
        }
        self.remeasure_entry(key);
        cx.notify();
    }

    fn remeasure_entry(&self, key: &str) {
        self.remeasure_where(|row| {
            row.id.ends_with(key) || matches!(&row.kind, RowKind::Work { entries, .. } if entries.iter().any(|e| e.id == key))
        });
        for list in self.group_lists.borrow().values() {
            list.remeasure();
        }
    }

    pub fn toggle_message(&mut self, row_id: &str, cx: &mut Context<Self>) {
        toggle(&mut self.ui.expanded_messages, row_id);
        self.remeasure_where(|row| row.id == row_id);
        cx.notify();
    }

    pub fn toggle_plan(&mut self, row_id: &str, cx: &mut Context<Self>) {
        toggle(&mut self.ui.expanded_plans, row_id);
        self.remeasure_where(|row| row.id == row_id);
        cx.notify();
    }

    pub fn open_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.select_thread(thread_id, cx));
    }

    fn remeasure_where(&self, matches: impl Fn(&TimelineRow) -> bool) {
        let offset = self.header_count();
        for (index, row) in self.rows.iter().enumerate() {
            if matches(row) {
                self.list.remeasure_items(offset + index..offset + index + 1);
            }
        }
    }

    /// The pieces a row draws from, borrowed apart from the markdown states it may create.
    fn with_env<R>(&mut self, render: impl FnOnce(&RowEnv, &mut MarkdownStore) -> R) -> R {
        let env = RowEnv {
            clock: self.clock,
            workspace_root: self.workspace_root.as_deref(),
            ui: &self.ui,
            pending_approvals: &self.activity.pending_approval_ids,
            window_height: self.window_height,
            details: &self.details,
            group_lists: &self.group_lists,
            working_timer: self.working_timer.as_ref(),
            find_target: self.find.as_ref().and_then(|find| find.active_entry()),
        };
        render(&env, &mut self.markdown)
    }

    pub fn render_group_member(&mut self, entry: &Arc<WorkLogEntry>, cx: &mut Context<Self>) -> gpui::AnyElement {
        self.with_env(|env, markdown| render_group_member(entry, env, markdown, cx))
    }
}

/// Returns whether `key` is now in the set.
fn toggle(set: &mut std::collections::HashSet<String>, key: &str) -> bool {
    if set.remove(key) {
        return false;
    }
    set.insert(key.to_owned());
    true
}

fn lane(content: gpui::AnyElement, first: bool) -> gpui::AnyElement {
    div()
        .w_full()
        .flex()
        .justify_center()
        .px(px(48.))
        .when(first, |this| this.pt(px(16.)))
        .child(div().w_full().min_w_0().max_w(px(CONTENT_MAX_WIDTH)).child(content))
        .into_any_element()
}

impl Render for ThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let thread = &self.state.read(cx).thread;
        let loading = thread.projection.is_none() && !thread.is_merging_history();
        let muted = hsla(palette(cx).muted_foreground);
        if loading {
            return div().size_full().flex().items_center().justify_center().text_sm().text_color(muted).child("Loading thread…").into_any_element();
        }
        if self.rows.is_empty() && self.history.is_none() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child("Send a message to start the conversation.")
                .into_any_element();
        }
        self.clock = Clock::now();
        self.window_height = f32::from(window.viewport_size().height);
        self.markdown.begin_frame();
        let find_bar = self.render_find_bar(cx);
        let transcript = list(
            self.list.clone(),
            cx.processor(|this, ix: usize, _window, cx| {
                let offset = this.header_count();
                if ix < offset {
                    let content = match &this.history {
                        Some(control) => history_control(control.has_more, control.loading, control.error.as_deref(), cx),
                        None => div().into_any_element(),
                    };
                    return lane(content, true);
                }
                let Some(row) = this.rows.get(ix - offset).cloned() else { return div().into_any_element() };
                let content = this.with_env(|env, markdown| render_row(&row, env, markdown, cx));
                lane(content, ix == 0)
            }),
        )
        .size_full();
        div().relative().size_full().child(transcript).children(find_bar).into_any_element()
    }
}
