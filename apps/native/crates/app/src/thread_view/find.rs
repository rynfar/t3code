//! Find in thread (`cmd-f`): `ThreadFindBar.tsx`, `useThreadFind.ts` and the reveal half of
//! `useThreadFindNavigation.ts`. The server counts and orders matches across the whole thread;
//! the active match's row is revealed by opening the fold or group that hides it, or by paging
//! history in when it is not loaded yet, then scrolled into view and tinted.

use std::time::Duration;

use gpui::{AnyElement, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Styled, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::h_flex;
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use t3_core::timeline::{
    FindMatch, FindResult, FindStart, MAX_QUERY_CHARS, RowKind, SEARCH_THREAD, TimelineRow, bar_label, decode_result, fold_membership,
    search_request, step_locally,
};

use super::ThreadView;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::transcript::{ghost_icon_button, ghost_text_button};

const QUERY_DEBOUNCE: Duration = Duration::from_millis(100);
const LIVE_REFRESH: Duration = Duration::from_millis(300);
pub const BAR_WIDTH: f32 = 280.;

pub struct FindUi {
    input: Entity<InputState>,
    query: String,
    start: Option<FindStart>,
    offset: i64,
    result: Option<FindResult>,
    active: Option<(u64, FindMatch)>,
    loading: bool,
    failed: bool,
    /// A match in history that is not loaded yet; revealed once its page arrives.
    pending_reveal: Option<String>,
    request: Option<Task<()>>,
    refresh: Option<Task<()>>,
    _subscription: Subscription,
}

impl FindUi {
    pub fn active_entry(&self) -> Option<&str> {
        self.active.as_ref().map(|(_, found)| found.entry_id.as_str())
    }
}

/// The entry ids a row draws, as `orchestration.searchThread` names them.
pub fn row_holds(row: &TimelineRow, entry_id: &str) -> bool {
    match &row.kind {
        RowKind::Message { message, .. } => message.id == entry_id,
        RowKind::Work { entries, .. } | RowKind::WorkLive { entries, .. } | RowKind::WorkToggle { entries, .. } => {
            entries.iter().any(|entry| entry.id == entry_id)
        }
        RowKind::Event { item, subagents, .. } => item.id == entry_id || subagents.iter().any(|member| member.id == entry_id),
        RowKind::ProposedPlan(_) => row.id == entry_id,
        _ => false,
    }
}

fn row_entry(row: &TimelineRow) -> Option<&str> {
    match &row.kind {
        RowKind::Message { message, .. } => Some(&message.id),
        RowKind::Work { entries, .. } | RowKind::WorkLive { entries, .. } | RowKind::WorkToggle { entries, .. } => {
            entries.first().map(|entry| entry.id.as_str())
        }
        RowKind::Event { item, .. } => Some(&item.id),
        RowKind::ProposedPlan(_) => Some(&row.id),
        _ => None,
    }
}

impl ThreadView {
    /// Opens find, or refocuses it and restarts its search from the viewport.
    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread_id.is_none() {
            return;
        }
        if let Some(find) = &self.find {
            let input = find.input.clone();
            input.update(cx, |input, cx| input.focus(window, cx));
            if !find.query.trim().is_empty() {
                let start = self.viewport_start();
                if let Some(find) = self.find.as_mut() {
                    find.start = start;
                    find.offset = 0;
                }
                self.request_find(cx);
            }
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in thread"));
        let subscription = cx.subscribe_in(&input, window, |view, input, event: &InputEvent, _window, cx| match event {
            InputEvent::Change => {
                let query: String = input.read(cx).value().chars().take(MAX_QUERY_CHARS).collect();
                view.set_find_query(query, cx);
            }
            InputEvent::PressEnter { shift, .. } => view.step_find(if *shift { -1 } else { 1 }, cx),
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.find = Some(FindUi {
            input,
            query: String::new(),
            start: None,
            offset: 0,
            result: None,
            active: None,
            loading: false,
            failed: false,
            pending_reveal: None,
            request: None,
            refresh: None,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub fn close_find(&mut self, cx: &mut Context<Self>) {
        if self.find.take().is_some() {
            cx.notify();
        }
    }

    /// The first entry at or below the top of the viewport, so a search starts where the user is.
    fn viewport_start(&self) -> Option<FindStart> {
        let top = self.list.logical_scroll_top().item_ix.saturating_sub(self.header_count());
        self.rows.get(top..)?.iter().find_map(|row| row_entry(row)).map(|entry_id| FindStart { entry_id: entry_id.to_owned(), occurrence: 0 })
    }

    fn set_find_query(&mut self, query: String, cx: &mut Context<Self>) {
        let start = self.viewport_start();
        let Some(find) = self.find.as_mut() else { return };
        if find.query == query {
            return;
        }
        find.query = query;
        find.start = start;
        find.offset = 0;
        find.failed = false;
        if find.query.trim().is_empty() {
            find.request = None;
            find.result = None;
            find.active = None;
            find.loading = false;
            cx.notify();
            return;
        }
        find.loading = true;
        find.request = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(QUERY_DEBOUNCE).await;
            let _ = this.update(cx, |view, cx| view.request_find(cx));
        }));
        cx.notify();
    }

    fn request_find(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id.clone() else { return };
        let Some(find) = self.find.as_mut() else { return };
        if find.query.trim().is_empty() {
            return;
        }
        find.loading = true;
        let query = find.query.clone();
        let request = self.state.read(cx).rpc(SEARCH_THREAD, search_request(&thread_id, &query, find.start.as_ref(), find.offset));
        find.request = Some(cx.spawn(async move |this, cx| {
            let response = request.await;
            let _ = this.update(cx, |view, cx| {
                let Some(find) = view.find.as_mut().filter(|find| find.query == query) else { return };
                find.loading = false;
                match response.map(decode_result) {
                    Ok(Ok(result)) => {
                        find.failed = false;
                        find.active = result.r#match.clone().map(|found| (result.active_index, found));
                        find.result = Some(result);
                    }
                    _ => find.failed = true,
                }
                let target = find.active_entry().map(str::to_owned);
                if let Some(target) = target {
                    view.reveal_entry(&target, cx);
                }
                cx.notify();
            });
        }));
    }

    pub fn step_find(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else { return };
        let (Some(result), Some((index, current))) = (find.result.as_ref(), find.active.as_ref()) else { return };
        if find.loading || result.counting() || result.total_matches == 0 {
            return;
        }
        if let Some(next) = step_locally(result, *index, delta) {
            let target = next.1.entry_id.clone();
            find.active = Some(next);
            self.reveal_entry(&target, cx);
            cx.notify();
            return;
        }
        find.start = Some(FindStart { entry_id: current.entry_id.clone(), occurrence: current.occurrence });
        find.offset = delta;
        self.request_find(cx);
    }

    /// Streamed content can add or move matches: re-ask from the current match after a pause.
    pub(super) fn refresh_find_after_change(&mut self, cx: &mut Context<Self>) {
        let pending = self.find.as_ref().and_then(|find| find.pending_reveal.clone());
        if let Some(entry_id) = pending
            && self.rows.iter().any(|row| row_holds(row, &entry_id))
        {
            if let Some(find) = self.find.as_mut() {
                find.pending_reveal = None;
            }
            self.reveal_entry(&entry_id, cx);
        }
        let Some(find) = self.find.as_mut().filter(|find| !find.query.trim().is_empty() && find.refresh.is_none()) else { return };
        find.refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LIVE_REFRESH).await;
            let _ = this.update(cx, |view, cx| {
                let Some(find) = view.find.as_mut() else { return };
                find.refresh = None;
                if let Some((_, found)) = &find.active {
                    find.start = Some(FindStart { entry_id: found.entry_id.clone(), occurrence: found.occurrence });
                }
                find.offset = 0;
                view.request_find(cx);
            });
        }));
    }

    /// Opens whatever hides `entry_id` (turn fold, superseded attempt, tool group), scrolls its
    /// row into view, or pages history in when the entry is older than the loaded window.
    pub(super) fn reveal_entry(&mut self, entry_id: &str, cx: &mut Context<Self>) {
        for _ in 0..3 {
            if let Some(index) = self.rows.iter().position(|row| row_holds(row, entry_id)) {
                let row = self.rows[index].clone();
                if let RowKind::WorkToggle { group_id, .. } = &row.kind {
                    self.ui.expanded_groups.insert(group_id.clone());
                    self.rederive(cx);
                    continue;
                }
                if let RowKind::Work { entries, expanded_group: true, .. } = &row.kind
                    && let Some(member) = entries.iter().position(|entry| entry.id == entry_id)
                    && let Some(list) = self.group_lists.borrow().get(&row.id)
                {
                    list.scroll_to_reveal_item(member);
                }
                self.list.scroll_to_reveal_item(self.header_count() + index);
                cx.notify();
                return;
            }
            let (runs, attempts) = fold_membership(self.entries_cache.entries(), &self.activity);
            let opened = match (runs.get(entry_id), attempts.get(entry_id)) {
                (Some(run), _) => self.ui.expanded_runs.insert(run.clone()),
                (None, Some(attempt)) => self.ui.expanded_attempts.insert(attempt.clone()),
                (None, None) => false,
            };
            if !opened {
                break;
            }
            self.rederive(cx);
        }
        if self.history.as_ref().is_some_and(|history| history.has_more)
            && let Some(find) = self.find.as_mut()
        {
            find.pending_reveal = Some(entry_id.to_owned());
            self.load_earlier(cx);
        }
    }

    /// `ThreadFindBar`: field, count, Retry when the search failed, previous, next, close.
    pub(super) fn render_find_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let find = self.find.as_ref()?;
        let p = palette(cx);
        let active_index = find.active.as_ref().map_or(0, |(index, _)| *index);
        let label = bar_label(&find.query, find.result.as_ref(), active_index, find.loading && find.result.is_none(), find.failed);
        let no_results = !find.query.trim().is_empty() && !find.loading && !find.failed && find.result.as_ref().is_some_and(|r| r.total_matches == 0);
        let navigable = find.result.as_ref().is_some_and(|r| r.total_matches > 0 && !r.counting()) && !find.loading;
        let nav = |id: &'static str, icon: Icon, delta: i64, cx: &mut Context<Self>| {
            let button = ghost_icon_button(id, icon, cx.listener(move |view, _, _, cx| view.step_find(delta, cx)), cx);
            div().when(!navigable, |this| this.opacity(0.64)).child(button)
        };
        Some(
            h_flex()
                .id("thread-find-bar")
                .key_context("ThreadFind")
                .on_action(cx.listener(|view, _: &Escape, _, cx| view.close_find(cx)))
                .absolute()
                .top(px(12.))
                .right(px(12.))
                .w(px(BAR_WIDTH))
                .h(px(36.))
                .pl(px(10.))
                .pr(px(4.))
                .gap(px(2.))
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(p.dropdown_border))
                .bg(hsla(p.popover))
                .shadow_md()
                .child(crate::webview::occluder("thread-find-bar-cover", crate::webview::Layer::TranscriptChrome))
                .child(div().flex_1().min_w_0().text_size(px(14.)).child(Input::new(&find.input).appearance(false).bordered(false)))
                .child(
                    div()
                        .min_w(px(40.))
                        .flex_none()
                        .text_center()
                        .text_xs()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(hsla(if no_results { p.destructive } else { p.muted_foreground }))
                        .child(label),
                )
                .when(find.failed, |this| this.child(ghost_text_button("thread-find-retry", "Retry", None, cx.listener(|view, _, _, cx| view.request_find(cx)), cx)))
                .child(nav("thread-find-previous", Icon::ChevronUp, -1, cx))
                .child(nav("thread-find-next", Icon::ChevronDown, 1, cx))
                .child(ghost_icon_button("thread-find-close", Icon::X, cx.listener(|view, _, _, cx| view.close_find(cx)), cx))
                .into_any_element(),
        )
    }
}
