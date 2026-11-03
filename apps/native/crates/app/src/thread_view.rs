//! Transcript view: `t3_core::timeline` turns the projection into rows, a bottom-aligned
//! `list()` renders them so streaming keeps the tail pinned. Rows are shared and keep their
//! identity across updates, so only rows whose pointer changed are spliced and remeasured.
//! Item 0 is the "Load earlier turns" control while there is history to load. A change keeps the
//! first row in view where it was (`t3_core::timeline::anchor`).

use std::cell::RefCell;
use std::collections::HashMap;
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
use t3_core::history::HistoryControl;
use t3_core::timeline::anchor::{self, Anchor, ScrollTop, VisibleRows};
use t3_core::timeline::{
    EntriesCache, GET_TURN_ITEM, RowKind, open_command_items, RowsCache, RowsInput, ThreadActivity, TimelineRow, WorkLogEntry, decode_detail, derive_activity,
    derive_rows, detail_request, detail_revision, entry_fetches_detail, fork_support, rows_equal, turn_diffs_by_message, update_entries,
};
use t3_core::diff::TurnDiffSummary;

use crate::app_state::{AppEvent, AppState};
use crate::theme::{hsla, palette};
use crate::transcript::markdown::MarkdownStore;
use crate::transcript::{Clock, CommandOutputSlot, Detail, RowEnv, SetupEnv, TimerKind, TranscriptUi, WorkingTimer, history_control, render_group_member, render_row};
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
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
            AppEvent::Thread | AppEvent::Selection | AppEvent::SideThread => this.sync(cx),
            AppEvent::History => {
                this.sync_history(cx);
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
            AppEvent::Phase => this.sync_command_outputs(cx),
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
        let follow = self.disclosure.needs_follow_check().then(|| disclosure_end_distance(&self.list));
        let visible = self.visible_rows();
        self.sync_history(cx);
        let changed = self.rederive_rows(cx);
        let follow = changed && follow.is_some_and(|(distance, viewport)| self.disclosure.follow_change(distance, viewport));
        if follow { self.list.scroll_to_end(); } else { self.keep_place(visible); }
        if changed { self.disclosure.changed(); }
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

    fn rederive(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible_rows();
        self.rederive_rows(cx);
        self.keep_place(visible);
    }

    /// The rows in view and where they sit, before a change. A gap not yet scrolled into still
    /// counts: that row sits below the viewport's top.
    fn visible_rows(&self) -> Option<VisibleRows> {
        self.visible_rows_for(self.disclosure.anchor())
    }

    fn visible_rows_for(&self, disclosure: Option<&str>) -> Option<VisibleRows> {
        let top = self.list.logical_scroll_top();
        let row = top.item_ix.checked_sub(self.header_count()).and_then(|index| self.rows.get(index));
        let pending = self.anchor_gap.borrow().as_ref().filter(|(id, _)| row.is_some_and(|row| row.id == *id)).map_or(0., |(_, gap)| *gap);
        let top = ScrollTop { item: top.item_ix, offset: f32::from(top.offset_in_item) - pending };
        let viewport = f32::from(self.list.viewport_bounds().size.height);
        anchor::capture(&self.rows, |row| row.id.as_str(), self.header_count(), top, viewport, |ix| f32::from(self.list.offset_for_item(ix)), disclosure)
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
        let visible = self.visible_rows();
        toggle(&mut self.ui.expanded_runs, run_id);
        self.rederive_rows(cx);
        self.keep_place(visible);
    }

    pub fn toggle_attempt(&mut self, attempt_id: &str, cx: &mut Context<Self>) {
        self.begin_disclosure(&format!("attempt-fold:{attempt_id}"));
        let visible = self.visible_rows();
        toggle(&mut self.ui.expanded_attempts, attempt_id);
        self.rederive_rows(cx);
        self.keep_place(visible);
    }

    pub fn toggle_group(&mut self, group_id: &str, row_id: &str, cx: &mut Context<Self>) {
        self.begin_disclosure(row_id);
        let visible = self.visible_rows();
        toggle(&mut self.ui.expanded_groups, group_id);
        self.rederive_rows(cx);
        self.keep_place(visible);
    }

    /// Opening one entry changes only its own row's height, and fetches withheld output or
    /// subscribes to a command's; closing it lets the subscription go.
    pub fn toggle_entry(&mut self, key: &str, cx: &mut Context<Self>) {
        let row = self.rows.iter().find(|row| matches!(&row.kind, RowKind::Work { entries, .. } if entries.iter().any(|e| e.id == key)))
            .or_else(|| self.rows.iter().find(|row| row.id.ends_with(key))).map(|row| row.id.clone());
        if let Some(row) = row { self.begin_disclosure(&row); }
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
    fn remeasure_entry(&mut self, key: &str) {
        let follow = self.follow_disclosure_change();
        let visible = self.disclosure.anchor().and_then(|_| self.visible_rows());
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
            state: &self.state,
            thread_id: self.thread_id.as_deref(),
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
            rewind_disabled: self.rewind.busy() || self.activity.is_working,
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
                    if settle_disclosure(&view.list, &mut view.disclosure, generation) { cx.notify(); }
                });
            });
        }
        let transcript = self.timeline_rows.clone().cached(gpui::StyleRefinement::default().size_full());
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
}
