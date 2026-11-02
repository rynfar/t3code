//! Transcript view: `t3_core::timeline` turns the projection into rows, a bottom-aligned
//! `list()` renders them so streaming keeps the tail pinned. Updates splice only the rows that
//! changed (prefix/suffix match by row identity), so a streaming token remeasures one row.

use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Entity, IntoElement, ListAlignment, ListState, ParentElement, Render, Styled, Subscription, Task, Window, div, list, prelude::*, px};
use t3_core::timeline::{EntriesCache, RowsCache, RowsInput, ThreadActivity, TimelineEntry, TimelineRow, derive_activity, derive_entries, derive_rows, rows_equal};

use crate::app_state::{AppEvent, AppState};
use crate::theme::{hsla, palette};
use crate::transcript::markdown::MarkdownStore;
use crate::transcript::{Clock, RowEnv, TranscriptUi, render_row};

pub const CONTENT_MAX_WIDTH: f32 = 736.;
/// Rows from the top at which scrolling up loads the page of history before the window.
const EARLIER_HISTORY_ROWS: usize = 3;

pub struct ThreadView {
    state: Entity<AppState>,
    list: ListState,
    rows: Vec<TimelineRow>,
    entries: Vec<Arc<TimelineEntry>>,
    entries_cache: EntriesCache,
    rows_cache: RowsCache,
    activity: ThreadActivity,
    ui: TranscriptUi,
    markdown: MarkdownStore,
    thread_id: Option<String>,
    workspace_root: Option<String>,
    ticker: Option<Task<()>>,
    _subscription: Subscription,
}

impl ThreadView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Thread | AppEvent::Selection) {
                this.sync(cx);
            }
        });
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        // Prepended rows arrive as `splice(0..0, n)` from `sync_rows`, which keeps the scroll anchored.
        let weak = state.downgrade();
        list.set_scroll_handler(move |event, _, cx| {
            if event.visible_range.start <= EARLIER_HISTORY_ROWS {
                let _ = weak.update(cx, |state, cx| state.load_older_history(cx));
            }
        });
        ThreadView {
            state,
            list,
            rows: Vec::new(),
            entries: Vec::new(),
            entries_cache: EntriesCache::default(),
            rows_cache: RowsCache::default(),
            activity: ThreadActivity::default(),
            ui: TranscriptUi::default(),
            markdown: MarkdownStore::default(),
            thread_id: None,
            workspace_root: None,
            ticker: None,
            _subscription: subscription,
        }
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let selected = state.selected_thread.clone();
        if selected != self.thread_id {
            self.thread_id = selected;
            self.ui = TranscriptUi::default();
            self.markdown.clear();
            self.entries_cache = EntriesCache::default();
            self.rows_cache = RowsCache::default();
            self.rows.clear();
            self.list.reset(0);
        }
        let state = self.state.read(cx);
        match state.thread.projection.as_ref() {
            Some(projection) => {
                self.entries = derive_entries(
                    &mut self.entries_cache,
                    &projection.visible_turn_items,
                    &projection.attempts,
                    &projection.nodes,
                    &projection.plans,
                );
                self.activity = derive_activity(projection);
                let thread = &projection.thread;
                self.workspace_root = thread
                    .worktree_path
                    .clone()
                    .or_else(|| state.shell.project(&thread.project_id).map(|p| p.workspace_root.clone()));
            }
            None => {
                self.entries.clear();
                self.activity = ThreadActivity::default();
            }
        }
        self.update_ticker(cx);
        self.rederive(cx);
    }

    fn rederive(&mut self, cx: &mut Context<Self>) {
        let input = RowsInput {
            entries: &self.entries,
            activity: &self.activity,
            expanded_run_ids: &self.ui.expanded_runs,
            expanded_attempt_ids: &self.ui.expanded_attempts,
            expanded_work_group_ids: &self.ui.expanded_groups,
        };
        let next = derive_rows(&input, &mut self.rows_cache);
        let prefix = self.rows.iter().zip(&next).take_while(|(a, b)| rows_equal(a, b)).count();
        if prefix == self.rows.len() && prefix == next.len() {
            return;
        }
        let max_suffix = self.rows.len().min(next.len()) - prefix;
        let suffix = self.rows.iter().rev().zip(next.iter().rev()).take(max_suffix).take_while(|(a, b)| rows_equal(a, b)).count();
        let old_range = prefix..self.rows.len() - suffix;
        let new_count = next.len() - suffix - prefix;
        self.rows = next;
        self.list.splice(old_range, new_count);
        cx.notify();
    }

    /// The working row counts seconds; nothing else in the transcript repaints on a timer.
    fn update_ticker(&mut self, cx: &mut Context<Self>) {
        if !self.activity.is_working {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
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

    /// Opening one entry changes only its own row's height.
    pub fn toggle_entry(&mut self, key: &str, cx: &mut Context<Self>) {
        toggle(&mut self.ui.expanded_entries, key);
        self.remeasure_where(|row| {
            row.id.ends_with(key)
                || matches!(&row.kind, t3_core::timeline::RowKind::Work { entries, .. } if entries.iter().any(|e| e.id == key))
        });
        cx.notify();
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
        for (index, row) in self.rows.iter().enumerate() {
            if matches(row) {
                self.list.remeasure_items(index..index + 1);
            }
        }
    }
}

fn toggle(set: &mut std::collections::HashSet<String>, key: &str) {
    if !set.remove(key) {
        set.insert(key.to_owned());
    }
}

impl Render for ThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.state.read(cx).thread.projection.is_none();
        let muted = hsla(palette(cx).muted_foreground);
        if loading {
            return div().size_full().flex().items_center().justify_center().text_sm().text_color(muted).child("Loading thread…").into_any_element();
        }
        if self.rows.is_empty() {
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
        let clock = Clock::now();
        let window_height = f32::from(window.viewport_size().height);
        list(
            self.list.clone(),
            cx.processor(move |this, ix: usize, _window, cx| {
                let Some(row) = this.rows.get(ix) else { return div().into_any_element() };
                let env = RowEnv {
                    clock,
                    workspace_root: this.workspace_root.as_deref(),
                    ui: &this.ui,
                    pending_approvals: &this.activity.pending_approval_ids,
                    window_height,
                };
                let content = render_row(row, &env, &mut this.markdown, cx);
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .px(px(48.))
                    .when(ix == 0, |this| this.pt(px(16.)))
                    .child(div().w_full().min_w_0().max_w(px(CONTENT_MAX_WIDTH)).child(content))
                    .into_any_element()
            }),
        )
        .size_full()
        .into_any_element()
    }
}
