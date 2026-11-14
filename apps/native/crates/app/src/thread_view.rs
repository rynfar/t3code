//! Transcript view: `t3_core::timeline` turns the projection into rows, a bottom-aligned
//! `list()` renders them so streaming keeps the tail pinned. Rows are shared and keep their
//! identity across updates, so only rows whose pointer changed are spliced and remeasured.
//! Item 0 is the "Load earlier turns" control while there is history to load. A change keeps the
//! first row in view where it was (`t3_core::timeline::anchor`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

mod citations;
mod find;
mod limit_recovery;
mod minimap;
pub use minimap::init;
mod rewind;
mod rows;
mod status;

pub use limit_recovery::LimitRecoveryBanner;

use gpui::{Bounds, Context, Entity, IntoElement, ListAlignment, ListOffset, ListState, ParentElement, Render, Styled, Subscription, Window, actions, canvas, div, list, point, prelude::*, px, size};
use t3_client::history::HistoryQuery;
use t3_core::history::{HistoryControl, HistoryStep, PageOutcome, WorkPageOutcome};
use t3_core::timeline::anchor::{self, Anchor, ScrollTop, VisibleRows};
use t3_core::timeline::{
    EntriesCache, GET_TURN_ITEM, RowKind, open_command_items, RowsCache, RowsInput, ThreadActivity, TimelineRow, WorkLogEntry, decode_detail, derive_activity,
    derive_rows, detail_request, detail_revision, entry_fetches_detail, fork_support, rows_equal, turn_diffs_by_message, update_entries,
};
use t3_core::diff::TurnDiffSummary;

use crate::app_state::{AppEvent, AppState};
use crate::theme::{hsla, palette};
use crate::transcript::markdown::MarkdownStore;
use crate::transcript::{Clock, CommandOutputSlot, Detail, FoldWorkState, RowEnv, SetupEnv, TimerKind, TranscriptUi, WorkingTimer, history_control, render_group_member, render_row};
use crate::worktree_setup::{SetupEvent, WorktreeSetupStore};

actions!(thread_view, [FindInThread]);

const DETAIL_CACHE_LIMIT: usize = 64;

pub struct ThreadView {
    pub(crate) state: Entity<AppState>,
    list: ListState,
    rows: Vec<Arc<TimelineRow>>,
    entries_cache: EntriesCache,
    rows_cache: RowsCache,
    activity: ThreadActivity,
    pub(crate) ui: TranscriptUi,
    markdown: MarkdownStore,
    thread_id: Option<String>,
    workspace_root: Option<String>,
    workspace_mutation_id: Option<String>,
    history: Option<HistoryControl>,
    details: HashMap<String, Detail>,
    /// Open command rows' output subscriptions, by entry; a row that closes drops its own.
    command_outputs: HashMap<String, CommandOutputSlot>,
    /// `commandOutputStreaming`, as of the last sync.
    streams_output: bool,
    group_lists: RefCell<HashMap<String, ListState>>,
    /// A row whose top must sit this far below the viewport's top; its next prepaint asks the
    /// list to scroll up into the rows above it, which only then have heights.
    anchor_gap: Rc<RefCell<Option<(String, f32)>>>,
    disclosure: anchor::DisclosureFollow,
    working_timer: Option<Entity<WorkingTimer>>,
    find: Option<find::FindUi>,
    citation_ui: citations::CitationUi,
    minimap: minimap::MinimapUi,
    minimap_layer: Entity<minimap::MinimapLayer>,
    timeline_rows: Entity<rows::TimelineRows>,
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
    measured_typography: TranscriptTypography,
    rewind: rewind::RewindUi,
    media: Entity<crate::media::MediaCache>,
    viewer: Entity<crate::media::MediaViewer>,
    /// A side chat's own thread, shown in the right panel; `None` follows the selection.
    fixed: Option<String>,
    /// The open thread's worktree setup; a side chat has none.
    setup: Option<Entity<WorktreeSetupStore>>,
    setup_env: SetupEnv,
    _subscriptions: Vec<Subscription>,
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
        let subscription = cx.subscribe_in(&state, _window, |this, _, event: &AppEvent, window, cx| match event {
            AppEvent::Thread | AppEvent::Selection | AppEvent::SideThread => {
                this.sync(cx);
                this.consume_pending_find(window, cx);
            }
            AppEvent::ThreadFind => {
                this.sync(cx);
                this.consume_pending_find(window, cx);
            }
            AppEvent::History => {
                this.sync_history(cx);
                this.sync_work_folds(cx);
                this.advance_citation_source(cx);
            }
            AppEvent::Shell => {
                this.sync_status(cx);
                let state=this.state.read(cx);
                let mut changed=false;
                for (id,previous) in this.ui.thread_chip_titles.borrow_mut().iter_mut() {
                    let title=state.thread_shell(id).map(|t|t.title.clone()).unwrap_or_default();
                    if *previous!=title {*previous=title;changed=true;}
                }
                if changed {cx.notify();}
            },
            AppEvent::SidebarSelection if !this.ui.thread_chip_titles.borrow().is_empty() => cx.notify(),
            AppEvent::ServerConfig => {
                this.sync_status(cx);
                this.sync_command_outputs(cx);
            }
            AppEvent::Phase => {
                this.watch_banner_grants(cx);
                this.sync_command_outputs(cx);
            },
            AppEvent::Rewind => {
                this.check_rewind(cx);
                cx.notify();
            }
            AppEvent::CitationSource => this.open_citation_source(cx),
            _ => {}
        });
        let media = crate::media::cache(&state, cx);
        crate::transcript::html_render::init(&state, cx);
        crate::transcript::mcp_app::init(&state, cx);
        let viewer = cx.new(crate::media::MediaViewer::new);
        let setup = fixed.is_none().then(|| crate::worktree_setup::store(&state, cx));
        let owner = cx.entity().downgrade();
        let citation_window = _window.window_handle().window_id();
        let citation_keys = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle().window_id() != citation_window { return; }
            let _ = owner.update(cx, |this, cx| {
                if !this.minimap_escape(&event.keystroke, window, cx) { this.citation_key(&event.keystroke, window, cx); }
            });
        });
        let minimap_owner = cx.entity();
        let timeline_rows = cx.new(|cx| rows::TimelineRows::new(&minimap_owner, cx));
        let minimap_layer = cx.new(|cx| minimap::MinimapLayer::new(&minimap_owner, &timeline_rows, cx));
        let mut subscriptions = vec![subscription, citation_keys];
        if let Some(setup) = &setup {
            subscriptions.push(cx.subscribe(setup, |this, _, _: &SetupEvent, cx| this.sync_setup(cx)));
        }
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
            workspace_mutation_id: None,
            history: None,
            details: HashMap::new(),
            command_outputs: HashMap::new(),
            streams_output: false,
            group_lists: RefCell::new(HashMap::new()),
            anchor_gap: Rc::default(),
            disclosure: anchor::DisclosureFollow::default(),
            working_timer: None,
            find: None,
            citation_ui: citations::CitationUi::new(cx),
            minimap: minimap::MinimapUi::default(),
            minimap_layer,
            timeline_rows,
            footer: status::ThreadFooter::default(),
            turn_diffs: HashMap::new(),
            fork_support: HashMap::new(),
            forking: std::collections::HashSet::new(),
            turn_diff_source: None,
            banners: status::Banners::default(),
            local_error: None,
            clock: Clock::now(),
            window_height: 0.,
            measured_typography: TranscriptTypography::of(_window, cx),
            rewind: rewind::RewindUi::default(),
            media,
            viewer,
            fixed,
            setup,
            setup_env: SetupEnv::default(),
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn html_page_origin(&self, cx: &gpui::App) -> Option<t3_core::html_render::routing::PageOrigin> {
        let state = self.state.read(cx);
        t3_core::html_render::routing::PageOrigin::for_view(state.environment(), state.selected_thread.as_deref(), self.fixed.as_deref())
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
            self.disclosure.cancel();
            self.dismiss_citation_selection(cx);
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
            self.minimap = minimap::MinimapUi::default();
            self.reset_status();
            self.turn_diffs.clear();
            self.turn_diff_source = None;
            self.reset_rewind();
            self.anchor_gap.borrow_mut().take();
            self.list.reset(0);
            self.setup_env = SetupEnv::default();
            self.read_setup(cx);
        }
        let mutation = self.thread_state(self.state.read(cx)).projection.as_ref().and_then(t3_core::media::workspace_mutation_id);
        if self.workspace_mutation_id != mutation {
            self.workspace_mutation_id = mutation;
            cx.notify();
        }
        let state = self.state.read(cx);
        match self.thread_state(state).projection.as_ref() {
            Some(projection) => {
                #[cfg(feature = "snapshot")]
                let fixture = crate::transcript::html_render::fixture::with_page(&projection.visible_turn_items);
                #[cfg(feature = "snapshot")]
                let mcp_fixture = crate::transcript::mcp_app::fixture::with_page(&fixture);
                #[cfg(feature = "snapshot")]
                let items: &[t3_core::thread::ProjectedTurnItem] = &mcp_fixture;
                #[cfg(not(feature = "snapshot"))]
                let items: &[t3_core::thread::ProjectedTurnItem] = &projection.visible_turn_items;
                update_entries(&mut self.entries_cache, items, &projection.attempts, &projection.nodes, &projection.plans);
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
        let paused = self.is_follow_paused(cx);
        let follow = (!paused && self.disclosure.needs_follow_check()).then(|| disclosure_end_distance(&self.list));
        let visible = self.visible_rows(cx);
        self.sync_history(cx);
        let changed = self.rederive_rows(cx);
        let follow = !paused && changed && follow.is_some_and(|(distance, viewport)| self.disclosure.follow_change(distance, viewport));
        if follow { self.list.scroll_to_end(); } else { self.keep_place(visible); }
        if changed { self.disclosure.changed(); }
        let pending_work = self.sync_work_folds(cx);
        self.load_deferred_work(pending_work, cx);
        self.sync_revert_counts(cx);
        self.check_rewind(cx);
        self.sync_turn_diffs(cx);
        self.sync_status(cx);
        self.refresh_open_details(cx);
        self.refresh_find_after_change(cx);
        self.advance_citation_source(cx);
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
        self.disclosure.cancel();
        match self.fixed.clone() {
            Some(id) => self.state.update(cx, |state, cx| state.load_older_side_history(&id, cx)),
            None => self.state.update(cx, |state, cx| state.load_older_history(cx)),
        }
    }

    /// Loads earlier history targeting `through_entry_id`, trying the lightweight `view=conversation`
    /// slice first and filling in the full `deferWork=1` page (`threads.ts` `loadEarlier`).
    pub(super) fn load_earlier_through(&mut self, entry_id: String, cx: &mut Context<Self>) {
        self.disclosure.cancel();
        let state = self.state.clone();
        let fixed = self.fixed.clone();
        let Some((thread_id, client, runtime, merge_lock, initial_cursor)) = state.update(cx, |state, cx| {
            let thread_id = fixed.clone().or_else(|| state.selected_thread.clone())?;
            let client = state.client_handle()?;
            let runtime = state.runtime().clone();
            let merge_lock = state.history_merge_lock.clone();
            let thread = thread_state_for_mut(state, fixed.is_some(), &thread_id)?;
            if thread.projection.as_ref().is_some_and(|projection| projection.has_entry(&entry_id)) {
                return None;
            }
            let cursor = thread.begin_history_load()?;
            emit_thread_updates(fixed.is_some(), false, cx);
            Some((thread_id, client, runtime, merge_lock, cursor))
        }) else {
            return;
        };

        cx.spawn(async move |_, cx| {
            let mut cursor = initial_cursor;
            for step in 0..8 {
                if step > 0 {
                    let Some(next_cursor) = state.update(cx, |state, cx| {
                        let thread = thread_state_for_mut(state, fixed.is_some(), &thread_id)?;
                        if thread.projection.as_ref().is_some_and(|projection| projection.has_entry(&entry_id)) {
                            return None;
                        }
                        if thread.history.history_cursor.as_deref() == Some(cursor.as_str()) {
                            thread.history.error = Some("History did not advance.".into());
                            emit_thread_updates(fixed.is_some(), false, cx);
                            return None;
                        }
                        let next = thread.begin_history_load()?;
                        emit_thread_updates(fixed.is_some(), false, cx);
                        Some(next)
                    }) else {
                        return;
                    };
                    cursor = next_cursor;
                }
                let mut found = false;
                for complete in [false, true] {
                    let query = if complete {
                        HistoryQuery {
                            cursor: cursor.clone(),
                            through_entry_id: Some(entry_id.clone()),
                            view: None,
                            defer_work: true,
                        }
                    } else {
                        HistoryQuery::through_entry(&cursor, &entry_id)
                    };
                    let (fetch_client, fetch_thread) = (client.clone(), thread_id.clone());
                    let result = runtime
                        .spawn(async move { fetch_client.history_page_with(&fetch_thread, query).await })
                        .await
                        .unwrap_or_else(|_| Err("Could not load earlier activity.".into()));
                    let _merge_guard = merge_lock.lock().await;
                    let merge = state.update(cx, |state, cx| {
                        let thread = thread_state_for_mut(state, fixed.is_some(), &thread_id)?;
                        let outcome = match result {
                            Ok(page) => thread.begin_history_merge_with(&cursor, page, complete, false),
                            Err(message) => PageOutcome::Step(thread.fail_history_load(&cursor, message)),
                        };
                        match outcome {
                            PageOutcome::Merge(merge) => {
                                emit_thread_updates(fixed.is_some(), false, cx);
                                Some(merge)
                            }
                            PageOutcome::Step(step) => {
                                if step != HistoryStep::Stale {
                                    emit_thread_updates(fixed.is_some(), false, cx);
                                }
                                None
                            }
                        }
                    });
                    let Some(merge) = merge else { return };
                    let merged = cx.background_executor().spawn(async move { merge.run() }).await;
                    let keep_going = state.update(cx, |state, cx| {
                        let Some(thread) = thread_state_for_mut(state, fixed.is_some(), &thread_id) else {
                            return false;
                        };
                        let step = thread.finish_history_merge(merged);
                        if step == HistoryStep::Stale {
                            return false;
                        }
                        found = thread
                            .projection
                            .as_ref()
                            .is_some_and(|projection| projection.has_entry(&entry_id));
                        emit_thread_updates(fixed.is_some(), true, cx);
                        !found || !complete
                    });
                    drop(_merge_guard);
                    if !keep_going {
                        return;
                    }
                    if found {
                        // Target already visible from `view=conversation`; finish metadata pass unless complete ran.
                        continue;
                    }
                }
            }
        })
        .detach();
    }

    pub(super) fn load_deferred_work(&self, anchors: Vec<String>, cx: &mut Context<Self>) {
        if anchors.is_empty() {
            return;
        }
        let state = self.state.clone();
        let fixed = self.fixed.clone();
        let is_side = fixed.is_some();
        let Some((thread_id, client, runtime, merge_lock, targets)) = state.update(cx, |state, cx| {
            let thread_id = fixed.clone().or_else(|| state.selected_thread.clone())?;
            let client = state.client_handle()?;
            let runtime = state.runtime().clone();
            let merge_lock = state.history_merge_lock.clone();
            let thread = thread_state_for_mut(state, is_side, &thread_id)?;
            let mut targets = Vec::new();
            for anchor in anchors {
                if let Some(target) = thread.begin_work_load(&anchor) {
                    targets.push(target);
                }
            }
            if targets.is_empty() {
                return None;
            }
            emit_thread_updates(is_side, false, cx);
            Some((thread_id, client, runtime, merge_lock, targets))
        }) else {
            return;
        };

        for target in targets {
            let (state, client, runtime, merge_lock, thread_id) =
                (state.clone(), client.clone(), runtime.clone(), merge_lock.clone(), thread_id.clone());
            cx.spawn(async move |_, cx| {
                let anchor_id = target.anchor_id;
                let mut cursor = target.cursor;
                loop {
                    let query = HistoryQuery::work(&cursor);
                    let (fetch_client, fetch_thread) = (client.clone(), thread_id.clone());
                    let result = runtime
                        .spawn(async move { fetch_client.history_page_with(&fetch_thread, query).await })
                        .await
                        .unwrap_or_else(|_| Err("Could not load deferred work.".into()));
                    let _merge_guard = merge_lock.lock().await;
                    let Some(merge) = state.update(cx, |state, cx| {
                        let thread = thread_state_for_mut(state, is_side, &thread_id)?;
                        let outcome = match result {
                            Ok(page) => thread.begin_work_merge(&anchor_id, &cursor, page),
                            Err(message) => {
                                WorkPageOutcome::Step(thread.fail_work_load(&anchor_id, &cursor, message))
                            }
                        };
                        match outcome {
                            WorkPageOutcome::Ready(merge) => {
                                emit_thread_updates(is_side, false, cx);
                                Some(merge)
                            }
                            WorkPageOutcome::Step(step) => {
                                if step != HistoryStep::Stale {
                                    emit_thread_updates(is_side, false, cx);
                                }
                                None
                            }
                        }
                    }) else {
                        return;
                    };
                    let merged = cx.background_executor().spawn(async move { merge.run() }).await;
                    let Some(next_cursor) = state.update(cx, |state, cx| {
                        let thread = thread_state_for_mut(state, is_side, &thread_id)?;
                        let step = thread.finish_work_merge(merged);
                        if step == HistoryStep::Stale {
                            return None;
                        }
                        emit_thread_updates(is_side, true, cx);
                        let HistoryStep::Continue(_) = step else { return None };
                        let next = thread.begin_work_load(&anchor_id)?;
                        emit_thread_updates(is_side, false, cx);
                        Some(next.cursor)
                    }) else {
                        return;
                    };
                    drop(_merge_guard);
                    if next_cursor == cursor {
                        return;
                    }
                    cursor = next_cursor;
                }
            })
            .detach();
        }
    }

    pub(super) fn load_work_for_run(&self, run_id: &str, cx: &mut Context<Self>) {
        let (runs, _) = t3_core::timeline::fold_membership(self.entries_cache.entries(), &self.activity);
        let entry_ids: HashSet<&str> = runs
            .iter()
            .filter_map(|(id, run)| (run == run_id).then_some(id.as_str()))
            .collect();
        let anchors = self
            .thread_state(self.state.read(cx))
            .projection
            .as_ref()
            .map(|projection| deferred_anchors_for_entries(projection, &entry_ids))
            .unwrap_or_default();
        self.load_deferred_work(anchors, cx);
    }

    pub(super) fn load_work_for_attempt(&self, attempt_id: &str, cx: &mut Context<Self>) {
        let (_, attempts) = t3_core::timeline::fold_membership(self.entries_cache.entries(), &self.activity);
        let entry_ids: HashSet<&str> = attempts
            .iter()
            .filter_map(|(id, attempt)| (attempt == attempt_id).then_some(id.as_str()))
            .collect();
        let anchors = self
            .thread_state(self.state.read(cx))
            .projection
            .as_ref()
            .map(|projection| deferred_anchors_for_entries(projection, &entry_ids))
            .unwrap_or_default();
        self.load_deferred_work(anchors, cx);
    }

    fn sync_work_folds(&mut self, cx: &mut Context<Self>) -> Vec<String> {
        if self.ui.expanded_runs.is_empty() && self.ui.expanded_attempts.is_empty() {
            if !self.ui.work_folds.is_empty() {
                self.ui.work_folds.clear();
                self.remeasure_where(|row| matches!(row.kind, RowKind::TurnFold { .. } | RowKind::AttemptFold { .. }));
                cx.notify();
            }
            return Vec::new();
        }
        let mut next = HashMap::new();
        let mut pending = Vec::new();
        {
            let state = self.state.read(cx);
            let thread = self.thread_state(state);
            if let Some(projection) = thread.projection.as_ref() {
                let (runs, attempts) = t3_core::timeline::fold_membership(self.entries_cache.entries(), &self.activity);
                let mut seen_pending = HashSet::new();
                let mut collect = |key: String, map: &HashMap<String, String>, id: &str| {
                    let entry_ids: HashSet<&str> = map
                        .iter()
                        .filter_map(|(entry_id, owner)| (owner == id).then_some(entry_id.as_str()))
                        .collect();
                    let anchors = deferred_anchors_for_entries(projection, &entry_ids);
                    let loading = anchors.iter().any(|anchor| thread.history.is_loading_work(anchor));
                    let error = !loading && anchors.iter().any(|anchor| thread.history.work_error(anchor).is_some());
                    for anchor in anchors {
                        if !thread.history.is_loading_work(&anchor)
                            && thread.history.work_error(&anchor).is_none()
                            && seen_pending.insert(anchor.clone())
                        {
                            pending.push(anchor);
                        }
                    }
                    if loading || error {
                        next.insert(key, FoldWorkState { loading, error });
                    }
                };
                for run_id in &self.ui.expanded_runs {
                    collect(format!("run:{run_id}"), &runs, run_id);
                }
                for attempt_id in &self.ui.expanded_attempts {
                    collect(format!("attempt:{attempt_id}"), &attempts, attempt_id);
                }
            }
        }
        if next != self.ui.work_folds {
            let changed: HashSet<String> = self
                .ui
                .work_folds
                .keys()
                .chain(next.keys())
                .filter(|key| self.ui.work_folds.get(*key) != next.get(*key))
                .cloned()
                .collect();
            self.ui.work_folds = next;
            self.remeasure_where(|row| match &row.kind {
                RowKind::TurnFold { run_id, .. } => changed.contains(&format!("run:{run_id}")),
                RowKind::AttemptFold { attempt_id, .. } => changed.contains(&format!("attempt:{attempt_id}")),
                _ => false,
            });
            cx.notify();
        }
        pending
    }

    pub(super) fn load_work_for_group(&self, group_id: &str, cx: &mut Context<Self>) {
        let entry_ids: HashSet<&str> = self
            .rows
            .iter()
            .filter_map(|row| match &row.kind {
                RowKind::WorkToggle { group_id: id, entries, .. } if id == group_id => Some(entries),
                _ => None,
            })
            .flat_map(|entries| entries.iter().map(|entry| entry.id.as_str()))
            .collect();
        let anchors = self
            .thread_state(self.state.read(cx))
            .projection
            .as_ref()
            .map(|projection| deferred_anchors_for_entries(projection, &entry_ids))
            .unwrap_or_default();
        self.load_deferred_work(anchors, cx);
    }

    fn rederive(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible_rows(cx);
        self.rederive_rows(cx);
        self.keep_place(visible);
    }

    fn is_follow_paused(&self, cx: &gpui::App) -> bool {
        crate::transcript::html_render::is_fullscreen_open(cx)
            || crate::transcript::mcp_app::is_fullscreen_open(cx)
            || self.viewer.read(cx).is_open()
    }

    /// The rows in view and where they sit, before a change. A gap not yet scrolled into still
    /// counts: that row sits below the viewport's top.
    fn visible_rows(&self, cx: &gpui::App) -> Option<VisibleRows> {
        self.visible_rows_for(self.disclosure.anchor(), self.is_follow_paused(cx))
    }

    fn visible_rows_for(&self, disclosure: Option<&str>, pin_at_tail: bool) -> Option<VisibleRows> {
        let top = self.list.logical_scroll_top();
        let row = top.item_ix.checked_sub(self.header_count()).and_then(|index| self.rows.get(index));
        let pending = self.anchor_gap.borrow().as_ref().filter(|(id, _)| row.is_some_and(|row| row.id == *id)).map_or(0., |(_, gap)| *gap);
        let top = ScrollTop { item: top.item_ix, offset: f32::from(top.offset_in_item) - pending };
        let viewport = f32::from(self.list.viewport_bounds().size.height);
        if pin_at_tail {
            anchor::capture_pinned(&self.rows, |row| row.id.as_str(), self.header_count(), top, viewport, |ix| f32::from(self.list.offset_for_item(ix)), disclosure)
        } else {
            anchor::capture(&self.rows, |row| row.id.as_str(), self.header_count(), top, viewport, |ix| f32::from(self.list.offset_for_item(ix)), disclosure)
        }
    }

    /// Puts the first row that was in view back where it was, as the web list's
    /// `maintainVisibleContentPosition` does; the list's own splice keeps only rows below the change.
    fn keep_place(&mut self, visible: Option<VisibleRows>) {
        let Some(visible) = visible else { return };
        let current = self.list.logical_scroll_top();
        let current = ScrollTop { item: current.item_ix, offset: f32::from(current.offset_in_item) };
        match anchor::restore(&visible, &self.rows, |row| row.id.as_str(), self.header_count(), current) {
            Some(Anchor::At(top)) => {
                self.anchor_gap.borrow_mut().take();
                let top = anchor::include_preceding_rows(top, |ix| f32::from(self.list.offset_for_item(ix + 1) - self.list.offset_for_item(ix)));
                self.list.scroll_to(ListOffset { item_ix: top.item, offset_in_item: px(top.offset) });
            }
            Some(Anchor::Below { item, gap }) => {
                self.list.scroll_to(ListOffset { item_ix: item, offset_in_item: px(0.) });
                let row = self.rows.get(item - self.header_count()).map(|row| row.id.clone());
                *self.anchor_gap.borrow_mut() = row.map(|row| (row, gap));
            }
            None => {}
        }
    }

    fn rederive_rows(&mut self, cx: &mut Context<Self>) -> bool {
        let input = RowsInput {
            entries: self.entries_cache.entries(),
            activity: &self.activity,
            expanded_run_ids: &self.ui.expanded_runs,
            expanded_attempt_ids: &self.ui.expanded_attempts,
            expanded_work_group_ids: &self.ui.expanded_groups,
            worktree_setup: self.setup_env.view.visible.as_ref(),
        };
        let next = derive_rows(&input, &mut self.rows_cache);
        self.sync_timer(&next, cx);
        let same = |a: &Arc<TimelineRow>, b: &Arc<TimelineRow>| Arc::ptr_eq(a, b) || rows_equal(a, b);
        let prefix = self.rows.iter().zip(&next).take_while(|(a, b)| same(a, b)).count();
        if prefix == self.rows.len() && prefix == next.len() {
            return false;
        }
        let max_suffix = self.rows.len().min(next.len()) - prefix;
        let suffix = self.rows.iter().rev().zip(next.iter().rev()).take(max_suffix).take_while(|(a, b)| same(a, b)).count();
        let offset = self.header_count();
        let old_range = offset + prefix..offset + self.rows.len() - suffix;
        let new_count = next.len() - suffix - prefix;
        self.rows = next;
        self.list.splice(old_range, new_count);
        self.minimap.set_items(t3_core::timeline::minimap::derive_minimap_items(&self.rows));
        let open: std::collections::HashSet<&str> =
            self.rows.iter().filter(|row| matches!(row.kind, RowKind::Work { expanded_group: true, .. })).map(|row| row.id.as_str()).collect();
        self.group_lists.borrow_mut().retain(|id, _| open.contains(id.as_str()));
        // Folding or opening a group or turn changes which open commands are drawn.
        self.sync_command_outputs(cx);
        cx.notify();
        true
    }

    /// One timer view per working stretch, none while the row says the worktree is being set
    /// up; besides it only a running setup stage ticks (`sync_setup`).
    fn sync_timer(&mut self, rows: &[Arc<TimelineRow>], cx: &mut Context<Self>) {
        let preparing = self.setup_env.view.preparing;
        let working = rows.iter().filter(|_| !preparing).find_map(|row| match (&row.kind, &row.created_at) {
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
        let streams = self.streams_command_output(cx);
        let Some(item) = entry.item.clone().filter(|_| entry_fetches_detail(entry, streams)) else { return };
        let revision = detail_revision(&item);
        if self.details.get(&item.id).is_some_and(|detail| detail.revision() == revision) {
            return;
        }
        if self.details.len() >= DETAIL_CACHE_LIMIT {
            let open = &self.ui.expanded_entries;
            self.details.retain(|id, _| open.contains(id));
        }
        // Decoded on the network runtime: a large output never parses on the UI thread.
        let decode = |value| decode_detail(value).map_err(|error| error.to_string());
        let Some(request) = self.state.read(cx).request(GET_TURN_ITEM, detail_request(&item), decode) else { return };
        self.details.insert(item.id.clone(), Detail::Loading { revision: revision.clone() });
        let (id, entry_id) = (item.id.clone(), entry.id.clone());
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |view, cx| {
                if view.details.get(&id).is_none_or(|detail| detail.revision() != revision) {
                    return;
                }
                let detail = match result {
                    Ok(Ok(Some(item))) => Detail::Loaded { revision, item: Arc::new(item) },
                    Ok(Ok(None)) => Detail::Missing { revision },
                    Ok(Err(message)) => Detail::Failed { revision, message },
                    Err(error) => Detail::Failed { revision, message: error.to_string() },
                };
                view.details.insert(id, detail);
                view.remeasure_entry(&entry_id, cx);
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

    fn begin_disclosure(&mut self, row_id: &str) {
        let (distance, viewport) = disclosure_end_distance(&self.list);
        self.disclosure.toggle(row_id, anchor::at_end(distance, viewport));
    }

    fn follow_disclosure_change(&mut self) -> bool {
        if !self.disclosure.needs_follow_check() { return false; }
        let (distance, viewport) = disclosure_end_distance(&self.list);
        self.disclosure.follow_change(distance, viewport)
    }

    pub fn toggle_run(&mut self, run_id: &str, cx: &mut Context<Self>) {
        self.begin_disclosure(&format!("turn-fold:{run_id}"));
        let visible = self.visible_rows(cx);
        if self.ui.expanded_runs.contains(run_id) && self.ui.work_fold_for_run(run_id).error {
            self.load_work_for_run(run_id, cx);
            self.sync_work_folds(cx);
            return;
        }
        let expanded = toggle(&mut self.ui.expanded_runs, run_id);
        self.rederive_rows(cx);
        self.keep_place(visible);
        if expanded {
            self.load_work_for_run(run_id, cx);
        }
        self.sync_work_folds(cx);
    }

    pub fn toggle_attempt(&mut self, attempt_id: &str, cx: &mut Context<Self>) {
        self.begin_disclosure(&format!("attempt-fold:{attempt_id}"));
        let visible = self.visible_rows(cx);
        if self.ui.expanded_attempts.contains(attempt_id) && self.ui.work_fold_for_attempt(attempt_id).error {
            self.load_work_for_attempt(attempt_id, cx);
            self.sync_work_folds(cx);
            return;
        }
        let expanded = toggle(&mut self.ui.expanded_attempts, attempt_id);
        self.rederive_rows(cx);
        self.keep_place(visible);
        if expanded {
            self.load_work_for_attempt(attempt_id, cx);
        }
        self.sync_work_folds(cx);
    }

    pub fn toggle_group(&mut self, group_id: &str, row_id: &str, cx: &mut Context<Self>) {
        self.begin_disclosure(row_id);
        let visible = self.visible_rows(cx);
        let expanded = toggle(&mut self.ui.expanded_groups, group_id);
        if expanded {
            self.load_work_for_group(group_id, cx);
        }
        self.rederive_rows(cx);
        self.keep_place(visible);
    }

    /// Opening one entry changes only its own row's height, and fetches withheld output or
    /// subscribes to a command's; closing it lets the subscription go.
    pub fn toggle_entry(&mut self, key: &str, cx: &mut Context<Self>) {
        let row = self.rows.iter().find(|row| matches!(&row.kind, RowKind::Work { entries, .. } if entries.iter().any(|e| e.id == key)))
            .or_else(|| self.rows.iter().find(|row| row.id.ends_with(key))).map(|row| row.id.clone());
        if let Some(row) = row { self.begin_disclosure(&row); }
        if toggle(&mut self.ui.expanded_entries, key) {
            let anchors = self
                .thread_state(self.state.read(cx))
                .projection
                .as_ref()
                .map(|projection| deferred_anchors_for_entries(projection, &HashSet::from([key])))
                .unwrap_or_default();
            self.load_deferred_work(anchors, cx);
            if let Some(entry) = self.find_entry(key) {
                self.fetch_detail(&entry, cx);
            }
        }
        self.sync_command_outputs(cx);
        self.remeasure_entry(key, cx);
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
                            view.remeasure_entry(&key, cx);
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
    fn remeasure_entry(&mut self, key: &str, cx: &gpui::App) {
        let paused = self.is_follow_paused(cx);
        let follow = !paused && self.follow_disclosure_change();
        let visible = if paused || self.disclosure.anchor().is_some() { self.visible_rows(cx) } else { None };
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
        drop(lists);
        if follow { self.list.scroll_to_end(); } else { self.keep_place(visible); }
        self.disclosure.changed();
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
            let visible = self.is_follow_paused(cx).then(|| self.visible_rows(cx)).flatten();
            self.remeasure_where(|row| matches!(&row.kind, RowKind::Message { message, .. } if changed.contains(&message.id)));
            self.keep_place(visible);
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
            state: &self.state,
            thread_id: self.thread_id.as_deref(),
            embedded: self.fixed.is_some(),
            clock: self.clock,
            workspace_root: self.workspace_root.as_deref(),
            ui: &self.ui,
            pending_approvals: &self.activity.pending_approval_ids,
            window_height: self.window_height,
            details: &self.details,
            group_lists: &self.group_lists,
            working_timer: self.working_timer.as_ref(),
            find_target: self.find.as_ref().and_then(|find| find.active_entry()).or(self.citation_ui.highlighted.as_deref()),
            turn_diffs: &self.turn_diffs,
            fork_support: &self.fork_support,
            forking: &self.forking,
            revert_counts: &self.rewind.counts,
            rewind_disabled: self.rewind.busy || self.activity.is_working,
            command_outputs: self.streams_output.then_some(&self.command_outputs),
            setup: &self.setup_env,
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

impl ThreadView {
    /// Takes the store's view of the open thread's setup, if it follows this one. Returns whether
    /// it changed.
    fn read_setup(&mut self, cx: &mut Context<Self>) -> bool {
        let next = match &self.setup {
            Some(store) => store.read(cx).view_for(self.thread_id.as_deref()).cloned().unwrap_or_default(),
            None => return false,
        };
        if next == self.setup_env.view {
            return false;
        }
        if next.background.is_none() {
            self.setup_env.popover_open = false;
        }
        self.setup_env.view = next;
        // Only a running stage ticks, and only while setup runs: a settled card never repaints.
        let starts = self.setup_env.live_starts();
        self.setup_env.timers.retain(|started_at, _| starts.contains(started_at));
        for started_at in starts {
            if let std::collections::hash_map::Entry::Vacant(slot) = self.setup_env.timers.entry(started_at) {
                let started_at = slot.key().clone();
                slot.insert(cx.new(|cx| WorkingTimer::new(started_at, TimerKind::SetupStage, cx)));
            }
        }
        true
    }

    fn sync_setup(&mut self, cx: &mut Context<Self>) {
        if self.read_setup(cx) {
            self.rederive(cx);
            // The working row's label and chip follow it without the row changing.
            cx.notify();
        }
    }

    pub fn toggle_setup_details(&mut self, cx: &mut Context<Self>) {
        self.setup_env.details_open = !self.setup_env.details_open;
        self.remeasure_where(|row| matches!(row.kind, RowKind::WorktreeSetup { .. }));
        cx.notify();
    }

    pub fn toggle_setup_popover(&mut self, cx: &mut Context<Self>) {
        // The click that dismissed the popover also lands on its trigger.
        let just_dismissed = self.setup_env.popover_dismissed.take().is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(250));
        if !just_dismissed {
            self.setup_env.popover_open = !self.setup_env.popover_open;
        }
        cx.notify();
    }

    pub fn dismiss_setup_popover(&mut self, cx: &mut Context<Self>) {
        if self.setup_env.popover_open {
            self.setup_env.popover_open = false;
            self.setup_env.popover_dismissed = Some(std::time::Instant::now());
            cx.notify();
        }
    }

    pub fn cancel_setup(&mut self, cx: &mut Context<Self>) {
        if let Some(store) = &self.setup {
            store.update(cx, |store, cx| store.cancel(cx));
        }
    }

    pub fn work_locally(&mut self, cx: &mut Context<Self>) {
        if let Some(store) = &self.setup {
            store.update(cx, |store, cx| store.work_locally(cx));
        }
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
        #[cfg(feature = "snapshot")]
        crate::snapshot::record_render("thread-shell", cx);
        remeasure_for_typography(TranscriptTypography::of(window, cx), &mut self.measured_typography, &self.list, &self.group_lists.borrow());
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
        let find_bar = self.render_find_bar(cx);
        let banners = self.render_banners(cx);
        let minimap = self.minimap_layer.clone().cached(gpui::StyleRefinement::default().absolute().inset_0());
        if let Some(generation) = self.disclosure.generation() {
            // Match the web's two-frame disclosure settle; newer layouts invalidate this ticket.
            cx.on_next_frame(window, move |_, window, cx| {
                cx.on_next_frame(window, move |view, _, cx| {
                    if !view.is_follow_paused(cx) && settle_disclosure(&view.list, &mut view.disclosure, generation) { cx.notify(); }
                });
            });
        }
        // TextView selection registrations must paint each drawn frame; scene reuse makes them stale.
        let transcript = self.timeline_rows.clone();
        div().id("citation-viewport")
            .track_focus(&self.citation_ui.selection_focus)
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| this.dismiss_citation_selection(cx)))
            .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                this.disclosure.cancel();
                this.dismiss_citation_selection(cx);
            }))
            .relative().size_full().child(transcript).child(minimap).children(banners).children(find_bar).children(self.render_citation_toolbar(window, cx)).child(self.viewer.clone()).children(self.render_rewind_confirm(window, cx)).into_any_element()
    }
}

fn disclosure_end_distance(list: &ListState) -> (f32, f32) {
    let viewport = f32::from(list.viewport_bounds().size.height);
    let top = list.logical_scroll_top();
    let distance = if top.item_ix >= list.item_count() { 0. } else {
        f32::from(list.offset_for_item(list.item_count()) - list.offset_for_item(top.item_ix) - top.offset_in_item) - viewport
    };
    (distance, viewport)
}

fn settle_disclosure(list: &ListState, follow: &mut anchor::DisclosureFollow, generation: u64) -> bool {
    let (distance, viewport) = disclosure_end_distance(list);
    if !follow.settle(generation, distance, viewport) { return false; }
    list.scroll_to_end();
    true
}

#[derive(Clone, PartialEq)]
struct TranscriptTypography {
    rem_size: gpui::Pixels,
    interface_family: gpui::SharedString,
    code_family: gpui::SharedString,
    code_size: gpui::Pixels,
    word_wrap: bool,
}

impl TranscriptTypography {
    fn of(window: &Window, cx: &gpui::App) -> Self {
        use t3_core::client_settings::typography::FontRole;
        Self {
            rem_size: window.rem_size(),
            interface_family: crate::theme::font_family(FontRole::Interface, cx),
            code_family: crate::theme::font_family(FontRole::Code, cx),
            code_size: px(crate::theme::font_size(FontRole::Code, cx)),
            word_wrap: crate::theme::preferences(cx).typography.word_wrap,
        }
    }
}

fn remeasure_for_typography(key: TranscriptTypography, previous: &mut TranscriptTypography, list: &ListState, groups: &HashMap<String, ListState>) {
    if *previous == key { return; }
    *previous = key;
    list.remeasure();
    for group in groups.values() { group.remeasure(); }
}

fn thread_state_for_mut<'a>(
    state: &'a mut AppState,
    is_side: bool,
    thread_id: &str,
) -> Option<&'a mut t3_core::thread::ThreadState> {
    if is_side {
        if !state.has_side_thread(thread_id) {
            return None;
        }
        state.thread_state_mut(thread_id)
    } else if state.selected_thread.as_deref() == Some(thread_id) {
        Some(&mut state.thread)
    } else {
        None
    }
}

fn emit_thread_updates(is_side: bool, projection_changed: bool, cx: &mut Context<AppState>) {
    if is_side {
        cx.emit(AppEvent::SideThread);
    } else {
        if projection_changed {
            cx.emit(AppEvent::Thread);
        }
        cx.emit(AppEvent::History);
    }
}

fn deferred_anchors_for_entries(
    projection: &t3_core::thread::ThreadProjection,
    entry_ids: &HashSet<&str>,
) -> Vec<String> {
    if entry_ids.is_empty() {
        return Vec::new();
    }
    projection
        .visible_turn_items
        .iter()
        .filter(|row| {
            row.item.deferred_work.is_some()
                && (entry_ids.contains(row.source_item_id.as_str()) || entry_ids.contains(row.item.id.as_str()))
        })
        .map(|row| row.source_item_id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn settled_disclosures_resume_following_appends_and_growth(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        struct TestList { list: ListState, heights: Vec<f32>, target: usize, header: Rc<std::cell::Cell<gpui::Pixels>> }
        impl Render for TestList {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let heights = self.heights.clone();
                let target = self.target;
                let header = self.header.clone();
                list(self.list.clone(), move |ix, _, _| {
                    let header = header.clone();
                    div().h(px(heights[ix])).relative().when(ix == target, |row| {
                        row.child(canvas(move |bounds, _, _| header.set(bounds.top()), |_, _, _, _| {}).absolute().size_full())
                    }).into_any_element()
                }).size_full()
            }
        }
        for count in [2, 5] {
            for (extra, collapse, wheel) in [(20., false, false), (100., false, false), (100., true, false), (100., false, true)] {
                let list = ListState::new(count, ListAlignment::Bottom, px(600.));
                let target = if count == 2 { 0 } else { 2 };
                let header = Rc::new(std::cell::Cell::new(px(0.)));
                let view = cx.update(|_, cx| cx.new(|_| TestList { list: list.clone(), heights: vec![40.; count], target, header: header.clone() }));
                let draw = |cx: &mut gpui::VisualTestContext| {
                    cx.draw(point(px(0.), px(0.)), size(px(200.), px(120.)), |_, _| view.clone().into_any_element());
                };
                draw(cx);
                let mut follow = anchor::DisclosureFollow::default();
                follow.toggle("group", true);
                cx.update(|_, cx| view.update(cx, |view, cx| { view.heights[target] += extra; cx.notify(); }));
                list.remeasure_items(target..target + 1);
                // The disclosure kept its old position, leaving the tail `extra` pixels below.
                list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(count as f32 * 40. - 120.) });
                draw(cx);
                let generation = follow.generation().unwrap();
                let before_settle = header.get();
                let rearmed = settle_disclosure(&list, &mut follow, generation);
                draw(cx);
                assert_eq!(header.get(), before_settle, "settling alone must not move the toggled row: count={count}, extra={extra}");
                if wheel {
                    let event = gpui::ScrollWheelEvent { delta: gpui::ScrollDelta::Pixels(point(px(0.), px(20.))), ..Default::default() };
                    assert!(minimap::forward_wheel(&list, &mut follow, &event));
                    draw(cx);
                }
                if collapse {
                    follow.toggle("group", false);
                    cx.update(|_, cx| view.update(cx, |view, cx| { view.heights[target] -= extra; cx.notify(); }));
                    list.remeasure_items(target..target + 1);
                    draw(cx);
                    let generation = follow.generation().unwrap();
                    settle_disclosure(&list, &mut follow, generation);
                } else if !rearmed {
                    let (distance, viewport) = disclosure_end_distance(&list);
                    if follow.follow_change(distance, viewport) { list.scroll_to_end(); }
                }
                cx.update(|_, cx| view.update(cx, |view, cx| { view.heights.push(40.); cx.notify(); }));
                list.splice(count..count, 1);
                draw(cx);
                if wheel {
                    assert!(list.logical_scroll_top().item_ix < count + 1, "minimap wheel navigation must survive an append: count={count}");
                } else {
                    assert_eq!(list.logical_scroll_top().item_ix, count + 1, "appends follow after settling: count={count}, extra={extra}, collapse={collapse}");
                }
                cx.update(|_, cx| view.update(cx, |view, cx| { *view.heights.last_mut().unwrap() += 20.; cx.notify(); }));
                list.remeasure_items(count..count + 1);
                draw(cx);
                if wheel {
                    assert!(list.logical_scroll_top().item_ix < count + 1, "minimap wheel navigation must survive growth: count={count}");
                } else {
                    assert_eq!(list.logical_scroll_top().item_ix, count + 1, "growth keeps following too: count={count}, extra={extra}, collapse={collapse}");
                }
            }
        }
    }

    #[gpui::test]
    fn paused_follow_pins_tail_while_media_viewer_is_open(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        struct TestList { list: ListState, rows: Vec<(String, f32)>, anchor_row: usize, top: Rc<std::cell::Cell<gpui::Pixels>> }
        impl Render for TestList {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let rows = self.rows.clone();
                let anchor_row = self.anchor_row;
                let top_cell = self.top.clone();
                list(self.list.clone(), move |ix, _, _| {
                    let top_cell = top_cell.clone();
                    div().h(px(rows[ix].1)).relative().when(ix == anchor_row, |row| {
                        row.child(canvas(move |bounds, _, _| top_cell.set(bounds.top()), |_, _, _, _| {}).absolute().size_full())
                    }).into_any_element()
                }).size_full()
            }
        }
        let rows: Vec<(String, f32)> = (0..5).map(|i| (format!("r{i}"), 40.)).collect();
        let list = ListState::new(rows.len(), ListAlignment::Bottom, px(600.));
        let top_cell = Rc::new(std::cell::Cell::new(px(0.)));
        let view = cx.update(|_, cx| cx.new(|_| TestList { list: list.clone(), rows, anchor_row: 3, top: top_cell.clone() }));
        let viewer = cx.update(|_, cx| cx.new(crate::media::MediaViewer::new));
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.draw(point(px(0.), px(0.)), size(px(200.), px(120.)), |_, _| view.clone().into_any_element());
        };
        draw(cx);
        // Before opening the viewer, appends at tail naturally follow to the new end.
        cx.update(|_, cx| view.update(cx, |v, cx| { v.rows.push(("r5".into(), 40.)); cx.notify(); }));
        list.splice(5..5, 1);
        draw(cx);
        assert_eq!(list.logical_scroll_top().item_ix, 6);
        // Opening MediaViewer pauses live follow and captures/restores the tail viewport instead.
        cx.update(|window, cx| {
            assert!(!crate::transcript::html_render::is_fullscreen_open(cx));
            viewer.update(cx, |v, cx| {
                v.open(vec![crate::media::ViewerItem::image(t3_core::media::MediaSource::Direct("data:image/png;base64,iVBORw0KGgo=".into()), "img")], 0, window, cx);
            });
            assert!(viewer.read(cx).is_open());
        });
        let before_pause_y = top_cell.get();
        let captured = cx.update(|_, cx| {
            let paused = crate::transcript::html_render::is_fullscreen_open(cx) || viewer.read(cx).is_open();
            assert!(paused);
            let st = list.logical_scroll_top();
            let st = ScrollTop { item: st.item_ix, offset: f32::from(st.offset_in_item) };
            let viewport = f32::from(list.viewport_bounds().size.height);
            view.read(cx).rows.split_last().and(Some(())).and_then(|_| {
                anchor::capture_pinned(&view.read(cx).rows, |(id, _)| id.as_str(), 0, st, viewport, |ix| f32::from(list.offset_for_item(ix)), None)
            })
        }).expect("pinned tail captures visible rows");
        let mut follow = anchor::DisclosureFollow::default();
        follow.toggle("r3", true);
        let generation = follow.generation().unwrap();
        cx.update(|_, cx| view.update(cx, |v, cx| { v.rows.push(("r6".into(), 40.)); cx.notify(); }));
        list.splice(6..6, 1);
        let paused = cx.update(|_, cx| crate::transcript::html_render::is_fullscreen_open(cx) || viewer.read(cx).is_open());
        assert!(!(!paused && settle_disclosure(&list, &mut follow, generation)), "disclosure settle must defer while paused");
        assert_eq!(follow.generation(), Some(generation));
        let st = list.logical_scroll_top();
        let current = ScrollTop { item: st.item_ix, offset: f32::from(st.offset_in_item) };
        if let Some(anchor::Anchor::At(target)) = cx.update(|_, cx| anchor::restore(&captured, &view.read(cx).rows, |(id, _)| id.as_str(), 0, current)) {
            list.scroll_to(ListOffset { item_ix: target.item, offset_in_item: px(target.offset) });
        }
        draw(cx);
        assert_eq!(top_cell.get(), before_pause_y, "visible rows stay pinned while MediaViewer is open");
        assert!(list.logical_scroll_top().item_ix < 7, "new tail row stays below the fold while paused");
        // Closing MediaViewer clears the pause so tail appends follow again once back at end.
        cx.update(|window, cx| {
            viewer.update(cx, |v, cx| v.close(window, cx));
            assert!(!viewer.read(cx).is_open());
        });
        list.scroll_to_end();
        draw(cx);
        assert!(settle_disclosure(&list, &mut follow, generation));
        cx.update(|_, cx| view.update(cx, |v, cx| { v.rows.push(("r7".into(), 40.)); cx.notify(); }));
        list.splice(7..7, 1);
        draw(cx);
        assert_eq!(list.logical_scroll_top().item_ix, 8, "live follow resumes after MediaViewer closes");
    }

    #[gpui::test]
    fn font_resize_invalidates_offscreen_transcript_and_group_measurements(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let main_list = ListState::new(100, ListAlignment::Top, px(0.));
        let groups = HashMap::from([("group".to_owned(), ListState::new(100, ListAlignment::Top, px(0.)))]);
        struct TestLists { main: ListState, group: ListState, height: f32 }
        impl Render for TestLists {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let height = self.height;
                div().flex().size_full()
                    .child(list(self.main.clone(), move |_, _, _| div().h(px(height)).into_any_element()).w(px(100.)).h_full())
                    .child(list(self.group.clone(), move |_, _, _| div().h(px(height)).into_any_element()).w(px(100.)).h_full())
            }
        }
        let view = cx.update(|_, cx| cx.new(|_| TestLists { main: main_list.clone(), group: groups["group"].clone(), height: 24. }));
        let draw = |viewport, cx: &mut gpui::VisualTestContext| {
            cx.draw(point(px(0.), px(0.)), size(px(200.), px(viewport)), |_, _| view.clone().into_any_element());
        };
        draw(2400., cx);
        draw(100., cx);
        let baseline = TranscriptTypography { rem_size: px(16.), interface_family: "System".into(), code_family: "Menlo".into(), code_size: px(13.), word_wrap: true };
        let mut previous = baseline.clone();
        remeasure_for_typography(baseline.clone(), &mut previous, &main_list, &groups);
        assert_eq!(main_list.bounds_for_item(99).unwrap().size.height, px(24.));
        assert_eq!(groups["group"].bounds_for_item(99).unwrap().size.height, px(24.));
        remeasure_for_typography(TranscriptTypography { rem_size: px(20.), ..baseline.clone() }, &mut previous, &main_list, &groups);
        assert!(main_list.bounds_for_item(99).is_none(), "offscreen main rows must be measured again");
        assert!(groups["group"].bounds_for_item(99).is_none(), "offscreen group members must be measured again");
        assert_eq!(previous.rem_size, px(20.));
        cx.update(|_, cx| view.update(cx, |view, cx| { view.height = 30.; cx.notify(); }));
        draw(100., cx);
        remeasure_for_typography(TranscriptTypography { rem_size: px(20.), ..baseline.clone() }, &mut previous, &main_list, &groups);
        assert_eq!(main_list.bounds_for_item(0).unwrap().size.height, px(30.));
        remeasure_for_typography(baseline.clone(), &mut previous, &main_list, &groups);
        assert!(main_list.bounds_for_item(0).is_none());
        assert!(groups["group"].bounds_for_item(0).is_none());
        for key in [
            TranscriptTypography { code_size: px(18.), ..baseline.clone() },
            TranscriptTypography { code_family: "Courier New".into(), ..baseline.clone() },
            TranscriptTypography { interface_family: "Arial".into(), ..baseline.clone() },
            TranscriptTypography { word_wrap: false, ..baseline.clone() },
        ] {
            remeasure_for_typography(baseline.clone(), &mut previous, &main_list, &groups);
            draw(4000., cx);
            draw(100., cx);
            assert!(main_list.bounds_for_item(99).is_some());
            remeasure_for_typography(key.clone(), &mut previous, &main_list, &groups);
            assert!(main_list.bounds_for_item(99).is_none(), "typography changes invalidate offscreen rows");
            assert!(groups["group"].bounds_for_item(99).is_none(), "typography changes invalidate offscreen group members");
            draw(100., cx);
            remeasure_for_typography(key, &mut previous, &main_list, &groups);
            assert!(main_list.bounds_for_item(0).is_some(), "unchanged typography keeps cached measurements");
            assert!(groups["group"].bounds_for_item(0).is_some(), "unchanged typography keeps group measurements");
        }
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn thread_view_loads_through_entry_and_deferred_work_on_expand(cx: &mut gpui::TestAppContext) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let requests_log = requests.clone();

        let _server = runtime.spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let requests_log = requests_log.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_owned();
                    requests_log.lock().unwrap().push(path.clone());
                    let body = if path.contains("view=work") {
                        serde_json::json!({
                            "snapshotSequence": 1,
                            "items": [
                                {
                                    "position": 0,
                                    "visibility": "local",
                                    "sourceThreadId": "t1",
                                    "sourceItemId": "w-loaded",
                                    "item": {
                                        "id": "w-loaded",
                                        "threadId": "t1",
                                        "runId": "r1",
                                        "nodeId": null,
                                        "parentItemId": null,
                                        "ordinal": 2,
                                        "type": "command_execution",
                                        "status": "completed",
                                        "title": null,
                                        "startedAt": null,
                                        "completedAt": null,
                                        "updatedAt": "2026-04-01T00:00:02.000Z",
                                        "input": "echo hi",
                                        "output": ""
                                    }
                                }
                            ],
                            "nextCursor": null,
                            "hasMoreHistory": false,
                            "hasUserTurn": false
                        })
                    } else if path.contains("view=conversation") {
                        serde_json::json!({
                            "snapshotSequence": 1,
                            "items": [
                                {
                                    "position": 0,
                                    "visibility": "local",
                                    "sourceThreadId": "t1",
                                    "sourceItemId": "msg-old",
                                    "item": {
                                        "id": "msg-old",
                                        "threadId": "t1",
                                        "runId": "r0",
                                        "nodeId": null,
                                        "parentItemId": null,
                                        "ordinal": 0,
                                        "type": "user_message",
                                        "status": "completed",
                                        "title": null,
                                        "startedAt": null,
                                        "completedAt": null,
                                        "updatedAt": "2026-04-01T00:00:00.000Z",
                                        "messageId": "msg-old",
                                        "text": "Earlier prompt",
                                        "inputIntent": "message"
                                    }
                                }
                            ],
                            "nextCursor": null,
                            "hasMoreHistory": false,
                            "hasUserTurn": true
                        })
                    } else {
                        serde_json::json!({
                            "snapshotSequence": 1,
                            "items": [],
                            "nextCursor": null,
                            "hasMoreHistory": false,
                            "hasUserTurn": false
                        })
                    };
                    let json = serde_json::to_string(&body).unwrap();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        json.len(),
                        json
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });

        let (state, _old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, _fixture) = t3_client::supervisor::ClientFixture::handle_with_http_base(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": ["orchestration:read", "orchestration:operate"]
            })).unwrap(),
            format!("http://127.0.0.1:{port}"),
        );
        let shell = state.read_with(cx, |state, _| state.shell.clone());
        state.update(cx, |state, cx| {
            let mut shell = shell;
            shell.snapshot.as_mut().unwrap().threads = vec![serde_json::from_value(serde_json::json!({
                "id": "t1", "projectId": "checkout-a", "title": "T1",
                "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                "lastVisitedAt": null,
                "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"},
                "status": "idle", "hasActionableProposedPlan": false, "itemCount": 2, "visibleItemCount": 2
            })).unwrap()];
            state.install_active_environment(client, shell, state.server_config.clone(), t3_client::supervisor::ConnectionStatus::Connected, cx);
            state.select_thread("t1", cx);
            let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
                "kind": "snapshot",
                "snapshotSequence": 1,
                "hasMoreHistory": true,
                "historyCursor": "c1",
                "projection": {
                    "thread": {
                        "id": "t1", "projectId": "checkout-a", "title": "T1",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"}
                    },
                    "runs": [{
                        "id": "r1", "ordinal": 1, "providerInstanceId": "replay",
                        "modelSelection": {"instanceId": "replay", "model": "replay"},
                        "userMessageId": "m0", "rootNodeId": null, "status": "completed",
                        "requestedAt": "2026-04-01T00:00:01.000Z", "startedAt": null,
                        "completedAt": "2026-04-01T00:00:02.000Z"
                    }],
                    "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                    "providerThreads": [], "providerTurns": [], "runtimeRequests": [],
                    "messages": [], "plans": [], "turnItems": [], "checkpointScopes": [],
                    "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                    "visibleTurnItems": [
                        {
                            "position": 0,
                            "visibility": "local",
                            "sourceThreadId": "t1",
                            "sourceItemId": "m0",
                            "item": {
                                "id": "m0",
                                "threadId": "t1",
                                "runId": null,
                                "nodeId": null,
                                "parentItemId": null,
                                "ordinal": 1,
                                "type": "user_message",
                                "status": "completed",
                                "title": null,
                                "startedAt": null,
                                "completedAt": null,
                                "updatedAt": "2026-04-01T00:00:01.000Z",
                                "messageId": "m0",
                                "text": "Run check",
                                "inputIntent": "message"
                            }
                        },
                        {
                            "position": 1,
                            "visibility": "local",
                            "sourceThreadId": "t1",
                            "sourceItemId": "w-anchor",
                            "item": {
                                "id": "w-anchor",
                                "threadId": "t1",
                                "runId": "r1",
                                "nodeId": null,
                                "parentItemId": null,
                                "ordinal": 2,
                                "type": "command_execution",
                                "status": "completed",
                                "title": null,
                                "startedAt": null,
                                "completedAt": null,
                                "updatedAt": "2026-04-01T00:00:01.000Z",
                                "input": "ls",
                                "output": "",
                                "deferredWork": { "cursor": "work-c1", "itemCount": 1, "lastOrdinal": 3 }
                            }
                        },
                        {
                            "position": 2,
                            "visibility": "local",
                            "sourceThreadId": "t1",
                            "sourceItemId": "a1",
                            "item": {
                                "id": "a1",
                                "threadId": "t1",
                                "runId": "r1",
                                "nodeId": null,
                                "parentItemId": null,
                                "ordinal": 4,
                                "type": "assistant_message",
                                "status": "completed",
                                "title": null,
                                "startedAt": null,
                                "completedAt": null,
                                "updatedAt": "2026-04-01T00:00:04.000Z",
                                "messageId": "a1",
                                "text": "Done.",
                                "streaming": false
                            }
                        }
                    ],
                    "updatedAt": "2026-04-01T00:00:05.000Z"
                }
            }"#).unwrap();
            state.thread.apply(snapshot);
            cx.emit(AppEvent::Thread);
            cx.emit(AppEvent::History);
        });

        let (sent, merged) = tokio::sync::oneshot::channel();
        let mut sent = Some(sent);
        let _sub = state.update(cx, |_, cx| {
            cx.subscribe(&state, move |state, _, event, _| {
                if matches!(event, AppEvent::Thread | AppEvent::History)
                    && state.thread.projection.as_ref().is_some_and(|p| p.has_entry("w-loaded") && p.has_entry("msg-old"))
                {
                    if let Some(sent) = sent.take() {
                        let _ = sent.send(());
                    }
                }
            })
        });

        let (work_sent, work_done) = tokio::sync::oneshot::channel();
        let mut work_sent = Some(work_sent);
        let _work_sub = state.update(cx, |_, cx| {
            cx.subscribe(&state, move |state, _, event, _| {
                if matches!(event, AppEvent::Thread | AppEvent::History)
                    && state.thread.projection.as_ref().is_some_and(|p| p.has_entry("w-loaded"))
                {
                    if let Some(sent) = work_sent.take() {
                        let _ = sent.send(());
                    }
                }
            })
        });
        let (view, vcx) = cx.add_window_view(|window, cx| {
            let mut view = ThreadView::new(state.clone(), window, cx);
            view.sync(cx);
            view
        });
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.toggle_run("r1", cx);
            });
        });
        work_done.await.unwrap();
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.load_earlier_through("msg-old".into(), cx);
            });
        });

        merged.await.unwrap();
        state.read_with(cx, |state, _| {
            let p = state.thread.projection.as_ref().unwrap();
            assert!(p.has_entry("w-loaded"), "toggle_run must load deferred work anchor");
            assert!(p.has_entry("msg-old"), "load_earlier_through must load target entry");
        });
        let seen = requests.lock().unwrap().clone();
        assert!(seen.iter().any(|p| p.contains("cursor=work-c1") && p.contains("view=work")), "{seen:?}");
        assert!(seen.iter().any(|p| p.contains("throughEntryId=msg-old") && p.contains("view=conversation")), "{seen:?}");
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn load_earlier_through_clears_loading_after_eight_pages(cx: &mut gpui::TestAppContext) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let complete_pages = Arc::new(AtomicUsize::new(0));
        let pages_counter = complete_pages.clone();
        let (eight_tx, eight_rx) = tokio::sync::oneshot::channel::<()>();
        let eight_tx = Arc::new(std::sync::Mutex::new(Some(eight_tx)));

        let _server = runtime.spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let pages_counter = pages_counter.clone();
                let eight_tx = eight_tx.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_owned();
                    let next_cursor = if path.contains("view=conversation") {
                        None
                    } else {
                        let step = pages_counter.fetch_add(1, Ordering::SeqCst) + 1;
                        if step == 8 {
                            if let Some(tx) = eight_tx.lock().unwrap().take() {
                                let _ = tx.send(());
                            }
                        }
                        Some(format!("c{}", step + 1))
                    };
                    let body = serde_json::json!({
                        "snapshotSequence": 1,
                        "items": [],
                        "nextCursor": next_cursor,
                        "hasMoreHistory": true,
                        "hasUserTurn": false
                    });
                    let json = serde_json::to_string(&body).unwrap();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        json.len(),
                        json
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });

        let (state, _old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, _fixture) = t3_client::supervisor::ClientFixture::handle_with_http_base(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": ["orchestration:read", "orchestration:operate"]
            })).unwrap(),
            format!("http://127.0.0.1:{port}"),
        );
        let shell = state.read_with(cx, |state, _| state.shell.clone());
        state.update(cx, |state, cx| {
            let mut shell = shell;
            shell.snapshot.as_mut().unwrap().threads = vec![serde_json::from_value(serde_json::json!({
                "id": "t1", "projectId": "checkout-a", "title": "T1",
                "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                "lastVisitedAt": null,
                "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"},
                "status": "idle", "hasActionableProposedPlan": false, "itemCount": 1, "visibleItemCount": 1
            })).unwrap()];
            state.install_active_environment(client, shell, state.server_config.clone(), t3_client::supervisor::ConnectionStatus::Connected, cx);
            state.select_thread("t1", cx);
            let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
                "kind": "snapshot",
                "snapshotSequence": 1,
                "hasMoreHistory": true,
                "historyCursor": "c1",
                "projection": {
                    "thread": {
                        "id": "t1", "projectId": "checkout-a", "title": "T1",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"}
                    },
                    "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                    "providerThreads": [], "providerTurns": [], "runtimeRequests": [],
                    "messages": [], "plans": [], "turnItems": [], "checkpointScopes": [],
                    "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                    "visibleTurnItems": [{
                        "position": 0, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": "m0",
                        "item": {
                            "id": "m0", "threadId": "t1", "runId": null, "nodeId": null, "parentItemId": null,
                            "ordinal": 10, "type": "user_message", "status": "completed", "title": null,
                            "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:01.000Z",
                            "messageId": "m0", "text": "Prompt", "inputIntent": "message"
                        }
                    }],
                    "updatedAt": "2026-04-01T00:00:05.000Z"
                }
            }"#).unwrap();
            state.thread.apply(snapshot);
            cx.emit(AppEvent::Thread);
            cx.emit(AppEvent::History);
        });

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let done_tx = Arc::new(std::sync::Mutex::new(Some(done_tx)));
        let _sub = state.update(cx, |_, cx| {
            let done_tx = done_tx.clone();
            cx.subscribe(&state, move |state, _, event, _| {
                if matches!(event, AppEvent::History | AppEvent::Thread)
                    && state.thread.history.history_cursor.as_deref() == Some("c9")
                    && !state.thread.history.loading
                {
                    if let Some(tx) = done_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
            })
        });

        let (view, vcx) = cx.add_window_view(|window, cx| ThreadView::new(state.clone(), window, cx));
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.load_earlier_through("missing-entry".into(), cx);
            });
        });

        eight_rx.await.unwrap();
        done_rx.await.unwrap();
        assert_eq!(complete_pages.load(Ordering::SeqCst), 8);
        state.read_with(cx, |state, _| {
            assert!(!state.thread.history.loading, "loading must clear after 8-page cap");
            assert_eq!(state.thread.history.history_cursor.as_deref(), Some("c9"));
        });
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn toggle_run_loads_multiple_deferred_work_anchors_concurrently(cx: &mut gpui::TestAppContext) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let _server = runtime.spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_owned();
                    let (id, ordinal) = if path.contains("cursor=work-c2") {
                        ("w2-loaded", 5)
                    } else {
                        ("w1-loaded", 3)
                    };
                    let body = serde_json::json!({
                        "snapshotSequence": 1,
                        "items": [{
                            "position": 0,
                            "visibility": "local",
                            "sourceThreadId": "t1",
                            "sourceItemId": id,
                            "item": {
                                "id": id,
                                "threadId": "t1",
                                "runId": "r1",
                                "nodeId": null,
                                "parentItemId": null,
                                "ordinal": ordinal,
                                "type": "command_execution",
                                "status": "completed",
                                "title": null,
                                "startedAt": null,
                                "completedAt": null,
                                "updatedAt": "2026-04-01T00:00:02.000Z",
                                "input": id,
                                "output": ""
                            }
                        }],
                        "nextCursor": null,
                        "hasMoreHistory": false,
                        "hasUserTurn": false
                    });
                    let json = serde_json::to_string(&body).unwrap();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        json.len(),
                        json
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });

        let (state, _old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, _fixture) = t3_client::supervisor::ClientFixture::handle_with_http_base(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": ["orchestration:read", "orchestration:operate"]
            })).unwrap(),
            format!("http://127.0.0.1:{port}"),
        );
        let shell = state.read_with(cx, |state, _| state.shell.clone());
        state.update(cx, |state, cx| {
            let mut shell = shell;
            shell.snapshot.as_mut().unwrap().threads = vec![serde_json::from_value(serde_json::json!({
                "id": "t1", "projectId": "checkout-a", "title": "T1",
                "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                "lastVisitedAt": null,
                "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"},
                "status": "idle", "hasActionableProposedPlan": false, "itemCount": 3, "visibleItemCount": 3
            })).unwrap()];
            state.install_active_environment(client, shell, state.server_config.clone(), t3_client::supervisor::ConnectionStatus::Connected, cx);
            state.select_thread("t1", cx);
            let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
                "kind": "snapshot",
                "snapshotSequence": 1,
                "hasMoreHistory": false,
                "historyCursor": null,
                "projection": {
                    "thread": {
                        "id": "t1", "projectId": "checkout-a", "title": "T1",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"}
                    },
                    "runs": [{
                        "id": "r1", "ordinal": 1, "providerInstanceId": "replay",
                        "modelSelection": {"instanceId": "replay", "model": "replay"},
                        "userMessageId": "m0", "rootNodeId": null, "status": "completed",
                        "requestedAt": "2026-04-01T00:00:01.000Z", "startedAt": null,
                        "completedAt": "2026-04-01T00:00:02.000Z"
                    }],
                    "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                    "providerThreads": [], "providerTurns": [], "runtimeRequests": [],
                    "messages": [], "plans": [], "turnItems": [], "checkpointScopes": [],
                    "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                    "visibleTurnItems": [
                        {
                            "position": 0, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": "m0",
                            "item": {
                                "id": "m0", "threadId": "t1", "runId": null, "nodeId": null, "parentItemId": null,
                                "ordinal": 1, "type": "user_message", "status": "completed", "title": null,
                                "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:01.000Z",
                                "messageId": "m0", "text": "Run check", "inputIntent": "message"
                            }
                        },
                        {
                            "position": 1, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": "w-anchor-1",
                            "item": {
                                "id": "w-anchor-1", "threadId": "t1", "runId": "r1", "nodeId": null, "parentItemId": null,
                                "ordinal": 2, "type": "command_execution", "status": "completed", "title": null,
                                "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:01.000Z",
                                "input": "ls", "output": "",
                                "deferredWork": { "cursor": "work-c1", "itemCount": 1, "lastOrdinal": 3 }
                            }
                        },
                        {
                            "position": 2, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": "w-anchor-2",
                            "item": {
                                "id": "w-anchor-2", "threadId": "t1", "runId": "r1", "nodeId": null, "parentItemId": null,
                                "ordinal": 4, "type": "command_execution", "status": "completed", "title": null,
                                "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:01.000Z",
                                "input": "pwd", "output": "",
                                "deferredWork": { "cursor": "work-c2", "itemCount": 1, "lastOrdinal": 5 }
                            }
                        }
                    ],
                    "updatedAt": "2026-04-01T00:00:05.000Z"
                }
            }"#).unwrap();
            state.thread.apply(snapshot);
            cx.emit(AppEvent::Thread);
            cx.emit(AppEvent::History);
        });

        let (sent, done) = tokio::sync::oneshot::channel();
        let sent = Arc::new(std::sync::Mutex::new(Some(sent)));
        let _sub = state.update(cx, |_, cx| {
            let sent = sent.clone();
            cx.subscribe(&state, move |state, _, event, _| {
                if matches!(event, AppEvent::Thread | AppEvent::History)
                    && state.thread.projection.as_ref().is_some_and(|p| p.has_entry("w1-loaded") && p.has_entry("w2-loaded"))
                {
                    if let Some(tx) = sent.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
            })
        });

        let (view, vcx) = cx.add_window_view(|window, cx| {
            let mut view = ThreadView::new(state.clone(), window, cx);
            view.sync(cx);
            view
        });
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.toggle_run("r1", cx);
            });
        });
        done.await.unwrap();
        state.read_with(cx, |state, _| {
            let p = state.thread.projection.as_ref().unwrap();
            assert!(p.has_entry("w1-loaded"));
            assert!(p.has_entry("w2-loaded"));
            assert!(!state.thread.history.is_loading_work("w-anchor-1"));
            assert!(!state.thread.history.is_loading_work("w-anchor-2"));
        });
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    async fn fold_work_loading_error_and_retry_keeps_fold_expanded(cx: &mut gpui::TestAppContext) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let (req1_seen_tx, req1_seen_rx) = tokio::sync::oneshot::channel();
        let (req1_go_tx, req1_go_rx) = tokio::sync::oneshot::channel();
        let (req2_seen_tx, req2_seen_rx) = tokio::sync::oneshot::channel();
        let (req2_go_tx, req2_go_rx) = tokio::sync::oneshot::channel();
        let mut req1_seen_tx = Some(req1_seen_tx);
        let mut req1_go_rx = Some(req1_go_rx);
        let mut req2_seen_tx = Some(req2_seen_tx);
        let mut req2_go_rx = Some(req2_go_rx);

        let _server = runtime.spawn(async move {
            for attempt in 0..2 {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                if attempt == 0 {
                    if let Some(tx) = req1_seen_tx.take() { let _ = tx.send(()); }
                    if let Some(rx) = req1_go_rx.take() { let _ = rx.await; }
                    let resp = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nfail";
                    let _ = stream.write_all(resp.as_bytes()).await;
                } else {
                    if let Some(tx) = req2_seen_tx.take() { let _ = tx.send(()); }
                    if let Some(rx) = req2_go_rx.take() { let _ = rx.await; }
                    let body = serde_json::json!({
                        "snapshotSequence": 1,
                        "items": [{
                            "position": 0, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": "w-loaded",
                            "item": {
                                "id": "w-loaded", "threadId": "t1", "runId": "r1", "nodeId": null, "parentItemId": null,
                                "ordinal": 3, "type": "command_execution", "status": "completed", "title": null,
                                "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:02.000Z",
                                "input": "echo ok", "output": ""
                            }
                        }],
                        "nextCursor": null, "hasMoreHistory": false, "hasUserTurn": false
                    });
                    let json = serde_json::to_string(&body).unwrap();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        json.len(),
                        json
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                }
            }
        });

        let (state, _old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, _fixture) = t3_client::supervisor::ClientFixture::handle_with_http_base(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": ["orchestration:read", "orchestration:operate"]
            })).unwrap(),
            format!("http://127.0.0.1:{port}"),
        );
        let shell = state.read_with(cx, |state, _| state.shell.clone());
        state.update(cx, |state, cx| {
            let mut shell = shell;
            shell.snapshot.as_mut().unwrap().threads = vec![serde_json::from_value(serde_json::json!({
                "id": "t1", "projectId": "checkout-a", "title": "T1",
                "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                "lastVisitedAt": null,
                "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"},
                "status": "idle", "hasActionableProposedPlan": false, "itemCount": 2, "visibleItemCount": 2
            })).unwrap()];
            state.install_active_environment(client, shell, state.server_config.clone(), t3_client::supervisor::ConnectionStatus::Connected, cx);
            state.select_thread("t1", cx);
            let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
                "kind": "snapshot", "snapshotSequence": 1, "hasMoreHistory": false, "historyCursor": null,
                "projection": {
                    "thread": {
                        "id": "t1", "projectId": "checkout-a", "title": "T1",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"}
                    },
                    "runs": [{
                        "id": "r1", "ordinal": 1, "providerInstanceId": "replay",
                        "modelSelection": {"instanceId": "replay", "model": "replay"},
                        "userMessageId": "m0", "rootNodeId": null, "status": "completed",
                        "requestedAt": "2026-04-01T00:00:01.000Z", "startedAt": null,
                        "completedAt": "2026-04-01T00:00:02.000Z"
                    }],
                    "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                    "providerThreads": [], "providerTurns": [], "runtimeRequests": [],
                    "messages": [], "plans": [], "turnItems": [], "checkpointScopes": [],
                    "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                    "visibleTurnItems": [
                        {
                            "position": 0, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": "m0",
                            "item": {
                                "id": "m0", "threadId": "t1", "runId": null, "nodeId": null, "parentItemId": null,
                                "ordinal": 1, "type": "user_message", "status": "completed", "title": null,
                                "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:01.000Z",
                                "messageId": "m0", "text": "Run check", "inputIntent": "message"
                            }
                        },
                        {
                            "position": 1, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": "w-anchor-1",
                            "item": {
                                "id": "w-anchor-1", "threadId": "t1", "runId": "r1", "nodeId": null, "parentItemId": null,
                                "ordinal": 2, "type": "command_execution", "status": "completed", "title": null,
                                "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:01.000Z",
                                "input": "ls", "output": "",
                                "deferredWork": { "cursor": "work-c1", "itemCount": 1, "lastOrdinal": 3 }
                            }
                        }
                    ],
                    "updatedAt": "2026-04-01T00:00:05.000Z"
                }
            }"#).unwrap();
            state.thread.apply(snapshot);
            cx.emit(AppEvent::Thread);
            cx.emit(AppEvent::History);
        });

        let (err_tx, err_rx) = tokio::sync::oneshot::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let err_tx = Arc::new(std::sync::Mutex::new(Some(err_tx)));
        let done_tx = Arc::new(std::sync::Mutex::new(Some(done_tx)));
        let _sub = state.update(cx, |_, cx| {
            let (err_tx, done_tx) = (err_tx.clone(), done_tx.clone());
            cx.subscribe(&state, move |state, _, event, _| {
                if matches!(event, AppEvent::History) && state.thread.history.work_error("w-anchor-1").is_some() {
                    if let Some(tx) = err_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
                if matches!(event, AppEvent::Thread | AppEvent::History)
                    && state.thread.projection.as_ref().is_some_and(|p| p.has_entry("w-loaded"))
                {
                    if let Some(tx) = done_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
            })
        });

        let (view, vcx) = cx.add_window_view(|window, cx| {
            let mut view = ThreadView::new(state.clone(), window, cx);
            view.sync(cx);
            view
        });
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.toggle_run("r1", cx);
            });
        });
        req1_seen_rx.await.unwrap();
        view.read_with(vcx, |view, _| {
            assert!(view.ui.expanded_runs.contains("r1"));
            assert_eq!(view.ui.work_fold_for_run("r1"), FoldWorkState { loading: true, error: false });
        });

        let _ = req1_go_tx.send(());
        err_rx.await.unwrap();
        vcx.run_until_parked();
        view.read_with(vcx, |view, _| {
            assert!(view.ui.expanded_runs.contains("r1"));
            assert_eq!(view.ui.work_fold_for_run("r1"), FoldWorkState { loading: false, error: true });
        });

        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.toggle_run("r1", cx);
            });
        });
        req2_seen_rx.await.unwrap();
        view.read_with(vcx, |view, _| {
            assert!(view.ui.expanded_runs.contains("r1"));
            assert_eq!(view.ui.work_fold_for_run("r1"), FoldWorkState { loading: true, error: false });
        });

        let _ = req2_go_tx.send(());
        done_rx.await.unwrap();
        vcx.run_until_parked();
        view.read_with(vcx, |view, _| {
            assert!(view.ui.expanded_runs.contains("r1"));
            assert_eq!(view.ui.work_fold_for_run("r1"), FoldWorkState::default());
        });
    }
}
