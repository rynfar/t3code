//! Transcript view: `t3_core::timeline` turns the projection into rows, a bottom-aligned
//! `list()` renders them so streaming keeps the tail pinned. Rows are shared and keep their
//! identity across updates, so only rows whose pointer changed are spliced and remeasured.
//! Item 0 is the "Load earlier turns" control while there is history to load.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

mod find;
mod limit_recovery;
mod rewind;
mod status;

pub use limit_recovery::LimitRecoveryBanner;

use gpui::{Context, Entity, IntoElement, ListAlignment, ListState, ParentElement, Render, Styled, Subscription, Window, actions, div, list, prelude::*, px};
use t3_core::history::HistoryControl;
use t3_core::timeline::{
    EntriesCache, GET_TURN_ITEM, RowKind, open_command_items, RowsCache, RowsInput, ThreadActivity, TimelineRow, WorkLogEntry, decode_detail, derive_activity,
    derive_rows, detail_request, detail_revision, needs_detail_fetch, fork_support, rows_equal, turn_diffs_by_message, update_entries,
};
use t3_core::diff::TurnDiffSummary;

use crate::app_state::{AppEvent, AppState};
use crate::theme::{hsla, palette};
use crate::transcript::markdown::MarkdownStore;
use crate::transcript::{Clock, CommandOutputSlot, Detail, RowEnv, TimerKind, TranscriptUi, WorkingTimer, history_control, render_group_member, render_row};

actions!(thread_view, [FindInThread]);

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
    /// Open command rows' output subscriptions, by entry; a row that closes drops its own.
    command_outputs: HashMap<String, CommandOutputSlot>,
    /// `commandOutputStreaming`, as of the last sync.
    streams_output: bool,
    group_lists: RefCell<HashMap<String, ListState>>,
    working_timer: Option<Entity<WorkingTimer>>,
    find: Option<find::FindUi>,
    footer: status::ThreadFooter,
    turn_diffs: HashMap<String, Arc<TurnDiffSummary>>,
    fork_support: HashMap<String, bool>,
    forking: std::collections::HashSet<String>,
    /// What `turn_diffs` was derived from: the message count and the checkpoints.
    turn_diff_source: Option<(usize, Vec<t3_core::thread::Entity>)>,
    banners: status::Banners,
    /// A failure from an action taken here, shown in the error banner like the web's local error.
    local_error: Option<String>,
    clock: Clock,
    window_height: f32,
    rewind: rewind::RewindUi,
    media: Entity<crate::media::MediaCache>,
    viewer: Entity<crate::media::MediaViewer>,
    /// A side chat's own thread, shown in the right panel; `None` follows the selection.
    fixed: Option<String>,
    _subscription: Subscription,
}

impl ThreadView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_for(state, None, window, cx)
    }

    /// The transcript of `thread_id` whatever is selected, for a side chat tab.
    pub fn embedded(state: Entity<AppState>, thread_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new_for(state, Some(thread_id), window, cx);
        view.sync(cx);
        view
    }

    fn new_for(state: Entity<AppState>, fixed: Option<String>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
            AppEvent::Thread | AppEvent::Selection | AppEvent::SideThread => this.sync(cx),
            AppEvent::History => this.sync_history(cx),
            AppEvent::Shell => this.sync_status(cx),
            AppEvent::ServerConfig => {
                this.sync_status(cx);
                this.sync_command_outputs(cx);
            }
            AppEvent::Phase => this.sync_command_outputs(cx),
            _ => {}
        });
        let media = crate::media::cache(&state, cx);
        let viewer = cx.new(crate::media::MediaViewer::new);
        ThreadView {
            state,
            list: ListState::new(0, ListAlignment::Bottom, px(600.)),
            rows: Vec::new(),
            entries_cache: EntriesCache::default(),
            rows_cache: RowsCache::default(),
            activity: ThreadActivity::default(),
            ui: TranscriptUi::default(),
            markdown: MarkdownStore::new(cx.entity_id()),
            thread_id: None,
            workspace_root: None,
            history: None,
            details: HashMap::new(),
            command_outputs: HashMap::new(),
            streams_output: false,
            group_lists: RefCell::new(HashMap::new()),
            working_timer: None,
            find: None,
            footer: status::ThreadFooter::default(),
            turn_diffs: HashMap::new(),
            fork_support: HashMap::new(),
            forking: std::collections::HashSet::new(),
            turn_diff_source: None,
            banners: status::Banners::default(),
            local_error: None,
            clock: Clock::now(),
            window_height: 0.,
            rewind: rewind::RewindUi::default(),
            media,
            viewer,
            fixed,
            _subscription: subscription,
        }
    }

    fn header_count(&self) -> usize {
        usize::from(self.history.is_some())
    }

    pub(crate) fn thread_state<'a>(&self, state: &'a AppState) -> &'a t3_core::thread::ThreadState {
        match &self.fixed {
            Some(id) => state.thread_state(id),
            None => &state.thread,
        }
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let selected = self.fixed.clone().or_else(|| state.selected_thread.clone());
        if selected == self.thread_id && self.thread_state(state).is_merging_history() {
            return;
        }
        if selected != self.thread_id {
            self.thread_id = selected;
            self.ui = TranscriptUi::default();
            self.markdown.clear();
            self.entries_cache = EntriesCache::default();
            self.rows_cache = RowsCache::default();
            self.details.clear();
            self.command_outputs.clear();
            self.group_lists.borrow_mut().clear();
            self.rows.clear();
            self.history = None;
            self.find = None;
            self.reset_status();
            self.turn_diffs.clear();
            self.turn_diff_source = None;
            self.reset_rewind();
            self.list.reset(0);
        }
        let state = self.state.read(cx);
        match self.thread_state(state).projection.as_ref() {
            Some(projection) => {
                update_entries(&mut self.entries_cache, &projection.visible_turn_items, &projection.attempts, &projection.nodes, &projection.plans);
                self.activity = derive_activity(projection);
                self.fork_support = fork_support(projection);
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
        self.sync_revert_counts(cx);
        self.check_rewind(cx);
        self.sync_turn_diffs(cx);
        self.sync_status(cx);
        self.refresh_open_details(cx);
        self.refresh_find_after_change(cx);
    }

    /// The control above the first row: shown, hidden, or relabelled without touching rows.
    fn sync_history(&mut self, cx: &mut Context<Self>) {
        let next = self.thread_state(self.state.read(cx)).history_control();
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
        match self.fixed.clone() {
            Some(id) => self.state.update(cx, |state, cx| state.load_older_side_history(&id, cx)),
            None => self.state.update(cx, |state, cx| state.load_older_history(cx)),
        }
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
        // Folding or opening a group or turn changes which open commands are drawn.
        self.sync_command_outputs(cx);
        cx.notify();
    }

    /// One timer view per working stretch; nothing else in the transcript ticks.
    fn sync_timer(&mut self, rows: &[Arc<TimelineRow>], cx: &mut Context<Self>) {
        let working = rows.iter().find_map(|row| match (&row.kind, &row.created_at) {
            (RowKind::Working { compacting }, Some(started)) => Some((started.clone(), *compacting)),
            _ => None,
        });
        let working = working.map(|(started, compacting)| (started, if compacting { TimerKind::Compacting } else { TimerKind::Working }));
        match working {
            Some((started, kind)) if self.working_timer.as_ref().is_some_and(|timer| timer.read(cx).matches(&started, kind)) => {}
            Some((started, kind)) => self.working_timer = Some(cx.new(|cx| WorkingTimer::new(started, kind, cx))),
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
        if streams_output(&item) && self.streams_command_output(cx) {
            return;
        }
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

    /// Opening one entry changes only its own row's height, and fetches withheld output or
    /// subscribes to a command's; closing it lets the subscription go.
    pub fn toggle_entry(&mut self, key: &str, cx: &mut Context<Self>) {
        if toggle(&mut self.ui.expanded_entries, key)
            && let Some(entry) = self.find_entry(key)
        {
            self.fetch_detail(&entry, cx);
        }
        self.sync_command_outputs(cx);
        self.remeasure_entry(key);
        cx.notify();
    }

    /// `commandOutputStreaming`: the server streams command output, live and final.
    fn streams_command_output(&self, cx: &Context<Self>) -> bool {
        let config = self.state.read(cx).server_config.config.as_ref();
        config.is_some_and(|config| config.environment.capabilities.command_output_streaming == Some(true))
    }

    /// Keeps one output subscription per open command row, as the web mounts
    /// `CommandOutputPanel` only while the row is open: closing the row, folding its group, or
    /// switching thread or connection drops it. The server ends each one once its command settles.
    fn sync_command_outputs(&mut self, cx: &mut Context<Self>) {
        let connection = self.state.read(cx).connection_id();
        let streams = self.streams_command_output(cx);
        if streams != self.streams_output {
            self.streams_output = streams;
            self.remeasure_where(|row| matches!(row.kind, RowKind::Work { .. }));
            cx.notify();
        }
        let wanted: HashMap<String, Arc<t3_core::turn_item::TurnItem>> =
            if streams && connection.is_some() { open_command_items(&self.rows, &self.ui.expanded_entries).into_iter().collect() } else { HashMap::new() };
        let target = |item: &t3_core::turn_item::TurnItem| (item.thread_id.clone(), item.id.clone(), connection);
        self.command_outputs.retain(|id, slot| wanted.get(id).is_some_and(|item| slot.target == target(item)));
        let missing: Vec<(String, Arc<t3_core::turn_item::TurnItem>)> = wanted.into_iter().filter(|(id, _)| !self.command_outputs.contains_key(id)).collect();
        if missing.is_empty() {
            return;
        }
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let runtime = state.runtime().clone();
        for (id, item) in missing {
            let stream = t3_client::command_output::subscribe(&runtime, client.clone(), &item.thread_id, &item.id);
            let mut status = stream.status.clone();
            let key = id.clone();
            let pump = cx.spawn(async move |this, cx| {
                while status.changed().await.is_ok() {
                    let next = status.borrow_and_update().clone();
                    let updated = this.update(cx, |view, cx| {
                        let Some(slot) = view.command_outputs.get_mut(&key) else { return };
                        if slot.update(next) {
                            view.remeasure_entry(&key);
                        }
                        cx.notify();
                    });
                    if updated.is_err() {
                        return;
                    }
                }
            });
            self.command_outputs.insert(id, CommandOutputSlot::new(target(&item), stream, pump));
        }
    }

    /// Remeasures the row that draws `key`, and only the open group list holding it.
    fn remeasure_entry(&self, key: &str) {
        let offset = self.header_count();
        let lists = self.group_lists.borrow();
        for (index, row) in self.rows.iter().enumerate() {
            if row.id.ends_with(key) || matches!(&row.kind, RowKind::Work { entries, .. } if entries.iter().any(|e| e.id == key)) {
                self.list.remeasure_items(offset + index..offset + index + 1);
                if let Some(list) = lists.get(&row.id) {
                    list.remeasure();
                }
            }
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

    pub fn open_media(&mut self, items: Vec<crate::media::ViewerItem>, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.viewer.update(cx, |viewer, cx| viewer.open(items, index, window, cx));
    }

    pub fn open_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.open_related_thread(thread_id, cx));
    }

    /// Re-derives the changed-files cards when the checkpoints or messages change, remeasuring
    /// only the assistant rows whose card appeared, changed or went.
    fn sync_turn_diffs(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(projection) = self.thread_state(state).projection.as_ref() else { return };
        if self.turn_diff_source.as_ref().is_some_and(|(messages, checkpoints)| *messages == projection.messages.len() && *checkpoints == projection.checkpoints) {
            return;
        }
        let next = turn_diffs_by_message(projection);
        self.turn_diff_source = Some((projection.messages.len(), projection.checkpoints.clone()));
        let changed: std::collections::HashSet<String> = next
            .iter()
            .filter(|(id, summary)| self.turn_diffs.get(*id).is_none_or(|old| old != *summary))
            .map(|(id, _)| id.clone())
            .chain(self.turn_diffs.keys().filter(|id| !next.contains_key(*id)).cloned())
            .collect();
        self.turn_diffs = next;
        if !changed.is_empty() {
            self.remeasure_where(|row| matches!(&row.kind, RowKind::Message { message, .. } if changed.contains(&message.id)));
            cx.notify();
        }
    }

    /// `onForkFromRun`: forks into `<title> fork`, then opens it once the shell lists it. A failure
    /// shows in the error banner.
    pub fn fork_from_run(&mut self, source_thread_id: &str, run_id: &str, cx: &mut Context<Self>) {
        if self.forking.contains(run_id) {
            return;
        }
        let state = self.state.read(cx);
        let Some(title) = self.thread_id.as_deref().and_then(|id| state.thread_shell(id)).map(|thread| thread.title.clone()) else { return };
        let target = crate::app_state::uuid_v4();
        let command = t3_core::timeline::fork_from_run(&crate::app_state::uuid_v4(), source_thread_id, &target, run_id, &title);
        let request = state.rpc(t3_core::commands::method::DISPATCH_COMMAND, command);
        let run_id = run_id.to_owned();
        self.forking.insert(run_id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |view, cx| {
                view.forking.remove(&run_id);
                match result {
                    Ok(_) => view.state.update(cx, |state, cx| state.select_when_listed(&target, cx)),
                    Err(error) => {
                        let message = error.to_string();
                        view.show_local_error(if message.trim().is_empty() { t3_core::timeline::FORK_FAILED.to_owned() } else { message }, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn open_turn_diff(&mut self, run_id: &str, file_path: Option<&str>, cx: &mut Context<Self>) {
        // The diff panel shows the open thread's checkpoints, not a side chat's.
        if self.fixed.is_some() {
            return;
        }
        self.state.update(cx, |state, cx| state.open_turn_diff(run_id, file_path, cx));
    }

    /// A file change's "Open diff".
    pub fn open_tool_call_diff(&mut self, target: t3_core::diff::ToolCallTarget, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.open_tool_call_diff(target, cx));
    }

    /// Expand or Collapse all folders; a folder's own toggle resets with it, as on the web.
    pub fn toggle_changed_files_all(&mut self, run_id: &str, message_id: &str, cx: &mut Context<Self>) {
        toggle(&mut self.ui.changed_files_open, run_id);
        self.ui.changed_file_dirs.remove(run_id);
        self.remeasure_where(|row| matches!(&row.kind, RowKind::Message { message, .. } if message.id == message_id));
        cx.notify();
    }

    pub fn toggle_changed_dir(&mut self, run_id: &str, path: &str, all_expanded: bool, message_id: &str, cx: &mut Context<Self>) {
        let dirs = self.ui.changed_file_dirs.entry(run_id.to_owned()).or_default();
        let open = dirs.get(path).copied().unwrap_or(all_expanded);
        dirs.insert(path.to_owned(), !open);
        self.remeasure_where(|row| matches!(&row.kind, RowKind::Message { message, .. } if message.id == message_id));
        cx.notify();
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
            turn_diffs: &self.turn_diffs,
            fork_support: &self.fork_support,
            forking: &self.forking,
            revert_counts: &self.rewind.counts,
            rewind_disabled: self.rewind.busy() || self.activity.is_working,
            tool_diffs: self.fixed.is_none(),
            command_outputs: self.streams_output.then_some(&self.command_outputs),
        };
        render(&env, &mut self.markdown)
    }

    /// The transcript's side padding: the narrow-window gutter inside the right panel.
    fn gutter(&self) -> f32 {
        if self.fixed.is_some() { 12. } else { 48. }
    }

    pub fn render_group_member(&mut self, entry: &Arc<WorkLogEntry>, cx: &mut Context<Self>) -> gpui::AnyElement {
        self.with_env(|env, markdown| render_group_member(entry, env, markdown, cx))
    }
}

fn streams_output(item: &t3_core::turn_item::TurnItem) -> bool {
    matches!(item.kind, t3_core::turn_item::TurnItemKind::CommandExecution { .. })
}

/// Returns whether `key` is now in the set.
fn toggle(set: &mut std::collections::HashSet<String>, key: &str) -> bool {
    if set.remove(key) {
        return false;
    }
    set.insert(key.to_owned());
    true
}

fn lane(content: gpui::AnyElement, first: bool, max_width: Option<gpui::Rems>, gutter: f32) -> gpui::AnyElement {
    div()
        .w_full()
        .flex()
        .justify_center()
        .px(crate::theme::rem(gutter))
        .when(first, |this| this.pt(crate::theme::rem(16.)))
        .child(div().w_full().min_w_0().when_some(max_width, |this, width| this.max_w(width)).child(content))
        .into_any_element()
}

impl Render for ThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let thread = self.thread_state(self.state.read(cx));
        let loading = thread.projection.is_none() && !thread.is_merging_history();
        let muted = hsla(palette(cx).muted_foreground);
        if loading {
            return div().size_full().flex().items_center().justify_center().text_sm().text_color(muted).child("Loading thread…").into_any_element();
        }
        if self.rows.is_empty() && self.history.is_none() && !self.footer_shown() {
            let banners = self.render_banners(cx);
            return div()
                .relative()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child("Send a message to start the conversation.")
                .children(banners)
                .into_any_element();
        }
        self.clock = Clock::now();
        self.window_height = f32::from(window.viewport_size().height);
        self.markdown.begin_frame();
        self.media.update(cx, |media, cx| media.begin_frame(window, cx));
        let scope = crate::media::MediaScope {
            owner: cx.entity_id(),
            thread_id: self.thread_id.clone(),
            workspace_root: self.workspace_root.clone(),
            viewer: self.viewer.downgrade(),
        };
        crate::media::set_scope(scope, cx);
        let find_bar = self.render_find_bar(cx);
        let banners = self.render_banners(cx);
        let transcript = list(
            self.list.clone(),
            cx.processor(|this, ix: usize, _window, cx| {
                let offset = this.header_count();
                if ix < offset {
                    let content = match &this.history {
                        Some(control) => history_control(control.has_more, control.loading, control.error.as_deref(), cx),
                        None => div().into_any_element(),
                    };
                    return lane(content, true, crate::theme::chat_max_width(cx), this.gutter());
                }
                let Some(row) = this.rows.get(ix - offset).cloned() else {
                    return lane(this.render_footer(cx), ix == 0, crate::theme::chat_max_width(cx), this.gutter());
                };
                let content = this.with_env(|env, markdown| render_row(&row, env, markdown, cx));
                lane(content, ix == 0, crate::theme::chat_max_width(cx), this.gutter())
            }),
        )
        .size_full();
        div().relative().size_full().child(transcript).children(banners).children(find_bar).child(self.viewer.clone()).children(self.render_rewind_confirm(window, cx)).into_any_element()
    }
}
