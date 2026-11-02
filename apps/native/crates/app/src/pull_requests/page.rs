//! The pull requests page (`PullRequestsPage.tsx`, `PullRequestsColumn.tsx`): one list read per
//! scope with a debounced host search, line counts read after the rows, grouped and sorted rows
//! in a virtualized list, and the selected pull request's detail beside it. While open it follows
//! `pullRequests.subscribeRefreshes`, re-reading the list and the open detail on each new epoch
//! (`refreshTrigger` in `client-runtime/src/state/pullRequests.ts`).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, ListAlignment, ListState, Modifiers,
    ParentElement, Render, Styled, Subscription, Task, Window, WindowControlArea, div, list, prelude::*, px, relative,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use t3_client::store;
use t3_core::pull_requests::list::{Group, Shown, entry_key, needs_stats, shown_groups};
use t3_core::pull_requests::query::parse_query;
use t3_core::pull_requests::wire::{Action, FilterOptions, ListResult, ListStats, MergeMethod, PrState, decode};
use t3_core::pull_requests::{
    ListPreferences, MAX_PAGE_SIZE, PAGE_SIZE, PREFERENCES_KEY, PrRef, SEARCH_DEBOUNCE_MS, action_done_label, action_failed_label, action_input, list_input, method, project_default_merge_method,
    stats_inputs,
};

use super::controls::{Button, Size, Variant};
use super::detail::DetailView;
use super::mutations::{self, MergePicks, Mutations};
use super::thread_panel::PendingSelection;
use super::{read, settle};
use crate::app_state::{AppEvent, AppState};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

pub(super) const ROW_HEIGHT: f32 = 56.;
/// A bound on remembered line counts.
const MAX_HELD_STATS: usize = 4000;

/// `pullRequestDiffStatKey`, in one environment.
fn stat_key(project_id: &str, number: u64) -> String {
    format!("{project_id} {number}")
}
const DETAIL_MIN_WIDTH: f32 = 360.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Row {
    Header(usize),
    Entry(usize),
    Footer,
}

#[derive(Default)]
pub(super) struct ListData {
    /// The scope and sent query this answer belongs to.
    pub key: Option<String>,
    pub result: Option<ListResult>,
    pub pending: bool,
    pub error: Option<String>,
    pub page_size: u32,
    pub loading_more: bool,
    /// Rows from the first page; later ones came from cursors and outlive a refresh.
    pub first_page: usize,
}

/// Follows the refresh signal on the network runtime; dropping it unsubscribes.
struct Refreshes {
    net: tokio::task::JoinHandle<()>,
    _ui: Task<()>,
}

impl Drop for Refreshes {
    fn drop(&mut self) {
        self.net.abort();
    }
}

const REFRESH_RETRY_MIN: Duration = Duration::from_secs(1);
const REFRESH_RETRY_MAX: Duration = Duration::from_secs(30);

pub struct PullRequestsPage {
    pub(super) state: Entity<AppState>,
    pub(super) prefs: ListPreferences,
    pub(super) search: Entity<InputState>,
    pub(super) typed: String,
    pub(super) sent: String,
    pub(super) data: ListData,
    pub(super) groups: Vec<Group>,
    pub(super) rows: Vec<Row>,
    pub(super) list: ListState,
    pub(super) selected: Option<PrRef>,
    detail: Option<Entity<DetailView>>,
    pub(super) shift: bool,
    pub(super) options: FilterOptions,
    /// Line counts by `pullRequestDiffStatKey`, kept across reads (GitHub listings omit them).
    stats: HashMap<String, (u64, u64)>,
    requested: HashSet<String>,
    focus: FocusHandle,
    connected: bool,
    open: bool,
    refreshes: Option<Refreshes>,
    /// The refresh epoch the held reads answer to.
    epoch: Option<u64>,
    _debounce: Option<Task<()>>,
    _list: Option<Task<()>>,
    _options: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for PullRequestsPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl PullRequestsPage {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prefs: ListPreferences = store::load_ui_state(PREFERENCES_KEY).and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default();
        let typed = prefs.q.clone().unwrap_or_default();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search pull requests, or label:bug").default_value(typed.clone()));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => this.typed_changed(input.read(cx).value().to_string(), cx),
                InputEvent::PressEnter { .. } => this.submit_query(cx),
                _ => {}
            }),
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
                if matches!(event, AppEvent::Phase | AppEvent::Selection) {
                    this.sync(cx);
                }
            }),
            cx.observe_global::<Mutations>(|_, cx| cx.notify()),
        ];
        let mut page = PullRequestsPage {
            state,
            prefs,
            search,
            sent: typed.clone(),
            typed,
            data: ListData { page_size: PAGE_SIZE, ..Default::default() },
            groups: Vec::new(),
            rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(800.)),
            selected: None,
            detail: None,
            shift: false,
            options: FilterOptions::default(),
            stats: HashMap::new(),
            requested: HashSet::new(),
            focus: cx.focus_handle(),
            connected: false,
            open: false,
            refreshes: None,
            epoch: None,
            _debounce: None,
            _list: None,
            _options: None,
            _subscriptions: subscriptions,
        };
        page.sync(cx);
        page
    }

    /// Reads when the page is shown and connected, again on reopening and on reconnecting, and
    /// follows the refresh signal only while both hold.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let connected = state.client_handle().is_some() && matches!(state.phase, crate::app_state::Phase::Online(t3_client::supervisor::ConnectionStatus::Connected));
        let open = state.pull_requests_open;
        let returned = connected && open && (!self.connected || !self.open);
        if open && let Some(reference) = cx.try_global::<PendingSelection>().and_then(|pending| pending.0.clone()) {
            cx.set_global(PendingSelection(None));
            self.select(reference, cx);
        }
        self.connected = connected;
        self.open = open;
        if !(open && connected) {
            self.refreshes = None;
            self.epoch = None;
        } else if self.refreshes.is_none() {
            self.follow_refreshes(cx);
        }
        if open && connected && (returned || self.data.key.is_none()) {
            self.refetch(cx);
            self.fetch_options(cx);
            if returned && let Some(detail) = &self.detail {
                detail.update(cx, |detail, cx| detail.refresh(cx));
            }
        }
        cx.notify();
    }

    fn follow_refreshes(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let net = state.runtime().spawn(async move {
            let mut delay = REFRESH_RETRY_MIN;
            loop {
                match client.stream(method::SUBSCRIBE_REFRESHES, serde_json::json!({})).await {
                    Ok(mut stream) => {
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(value) => {
                                    delay = REFRESH_RETRY_MIN;
                                    if let Some(epoch) = value.as_u64()
                                        && tx.send(epoch).is_err()
                                    {
                                        return;
                                    }
                                }
                                Err(t3_client::session::RpcError::Failure(failure)) if failure.tag.as_deref() == Some("EnvironmentAuthorizationError") => return,
                                Err(_) => break,
                            }
                        }
                    }
                    Err(t3_client::session::RpcError::Failure(failure)) if failure.tag.as_deref() == Some("EnvironmentAuthorizationError") => return,
                    Err(_) => {}
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(REFRESH_RETRY_MAX);
            }
        });
        let ui = cx.spawn(async move |this, cx| {
            while let Some(mut epoch) = rx.recv().await {
                while let Ok(later) = rx.try_recv() {
                    epoch = later;
                }
                if this.update(cx, |this, cx| this.refreshed(epoch, cx)).is_err() {
                    return;
                }
            }
        });
        self.refreshes = Some(Refreshes { net, _ui: ui });
    }

    /// A new epoch means the host's reads moved on (a turn ended, or any client wrote). The first
    /// one heard only marks where the reads made on opening stand.
    fn refreshed(&mut self, epoch: u64, cx: &mut Context<Self>) {
        let moved = self.epoch.is_some_and(|held| held != epoch);
        self.epoch = Some(epoch);
        if !moved {
            return;
        }
        self.requested.clear();
        self.fetch(None, true, cx);
        if let Some(detail) = &self.detail {
            detail.update(cx, |detail, cx| detail.refresh(cx));
        }
    }

    fn scope_key(&self) -> String {
        let scope = ListPreferences { sort: Default::default(), q: None, merge_method: None, ..self.prefs.clone() };
        format!("{}|{}", serde_json::to_string(&scope).unwrap_or_default(), self.sent)
    }

    pub(super) fn save_prefs(&mut self) {
        store::save_ui_state(PREFERENCES_KEY, &self.prefs);
    }

    /// A scope change: persist, re-read from the first page.
    pub(super) fn update_prefs(&mut self, change: impl FnOnce(&mut ListPreferences), cx: &mut Context<Self>) {
        let before = self.scope_key();
        change(&mut self.prefs);
        self.save_prefs();
        if self.scope_key() != before {
            self.data.page_size = PAGE_SIZE;
            self.refetch(cx);
        } else {
            self.rebuild(cx);
        }
        cx.notify();
    }

    fn typed_changed(&mut self, value: String, cx: &mut Context<Self>) {
        if value == self.typed {
            return;
        }
        self.typed = value.clone();
        self.prefs.q = (!value.is_empty()).then(|| value.chars().take(200).collect());
        self.save_prefs();
        self.rebuild(cx);
        self._debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(SEARCH_DEBOUNCE_MS)).await;
            let _ = this.update(cx, |this, cx| this.send_query(value.trim().to_owned(), cx));
        }));
        cx.notify();
    }

    fn submit_query(&mut self, cx: &mut Context<Self>) {
        self._debounce = None;
        let query = self.typed.trim().to_owned();
        self.send_query(query, cx);
    }

    fn send_query(&mut self, query: String, cx: &mut Context<Self>) {
        if query != self.sent {
            self.sent = query;
            self.data.page_size = PAGE_SIZE;
            self.refetch(cx);
        }
    }

    pub(super) fn clear_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.set_value("", window, cx));
        self.typed_changed(String::new(), cx);
        self.submit_query(cx);
    }

    pub(super) fn refetch(&mut self, cx: &mut Context<Self>) {
        self.fetch(None, false, cx);
    }

    /// `refreshing` re-reads the first page under rows that later cursors added.
    fn fetch(&mut self, cursors: Option<HashMap<String, String>>, refreshing: bool, cx: &mut Context<Self>) {
        let appending = cursors.is_some();
        let key = self.scope_key();
        let input = list_input(&self.prefs, &self.sent, self.data.page_size, cursors.as_ref());
        let task = read(self.state.read(cx), method::LIST, input, decode::<ListResult>);
        self.data.pending = true;
        self.data.loading_more = appending;
        self._list = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| this.apply_list(key, appending, refreshing, result, cx));
        }));
        cx.notify();
    }

    fn apply_list(&mut self, key: String, appending: bool, refreshing: bool, result: Result<ListResult, String>, cx: &mut Context<Self>) {
        if key != self.scope_key() {
            return;
        }
        self.data.pending = false;
        self.data.loading_more = false;
        match result {
            Ok(mut answer) => {
                let first_page = answer.entries.len();
                if appending && let Some(previous) = self.data.result.take() {
                    let seen: HashSet<String> = previous.entries.iter().map(entry_key).collect();
                    let fresh: Vec<_> = answer.entries.drain(..).filter(|entry| !seen.contains(&entry_key(entry))).collect();
                    answer.entries = previous.entries.into_iter().chain(fresh).collect();
                } else if refreshing
                    && let Some(previous) = self.data.result.take().filter(|previous| previous.entries.len() > self.data.first_page && self.data.key.as_deref() == Some(key.as_str()))
                {
                    let seen: HashSet<String> = answer.entries.iter().map(entry_key).collect();
                    let later = previous.entries.into_iter().skip(self.data.first_page).filter(|entry| !seen.contains(&entry_key(entry)));
                    answer.entries.extend(later);
                    answer.next_cursors = previous.next_cursors;
                    answer.truncated = previous.truncated;
                }
                if !appending {
                    self.data.first_page = first_page;
                }
                for entry in answer.entries.iter_mut().filter(|entry| needs_stats(entry)) {
                    if let Some((additions, deletions)) = self.stats.get(&stat_key(&entry.project_id, entry.number)) {
                        entry.additions = *additions;
                        entry.deletions = *deletions;
                    }
                }
                self.data.error = None;
                self.data.key = Some(key);
                self.data.result = Some(answer);
                self.fetch_stats(cx);
            }
            Err(error) => self.data.error = Some(error),
        }
        self.rebuild(cx);
        cx.notify();
    }

    /// `pullRequests.listStats` for rows the listing left unmeasured, once per row per epoch, in
    /// batches the contract takes.
    fn fetch_stats(&mut self, cx: &mut Context<Self>) {
        let Some(result) = &self.data.result else { return };
        let wanted: Vec<_> = result.entries.iter().filter(|entry| needs_stats(entry) && !self.requested.contains(&stat_key(&entry.project_id, entry.number))).collect();
        let batches = stats_inputs(wanted.iter().copied());
        if batches.is_empty() {
            return;
        }
        if self.stats.len() > MAX_HELD_STATS {
            self.stats.clear();
            self.requested.clear();
        }
        self.requested.extend(wanted.iter().map(|entry| stat_key(&entry.project_id, entry.number)));
        for input in batches {
            self.fetch_stats_batch(input, cx);
        }
    }

    fn fetch_stats_batch(&mut self, input: serde_json::Value, cx: &mut Context<Self>) {
        let task = read(self.state.read(cx), method::LIST_STATS, input, decode::<ListStats>);
        cx.spawn(async move |this, cx| {
            let Ok(stats) = settle(task).await else { return };
            let _ = this.update(cx, |this, cx| {
                for stat in stats.stats {
                    this.stats.insert(stat_key(&stat.project_id, stat.number), (stat.additions, stat.deletions));
                }
                let Some(result) = this.data.result.as_mut() else { return };
                for entry in result.entries.iter_mut().filter(|entry| needs_stats(entry)) {
                    if let Some((additions, deletions)) = this.stats.get(&stat_key(&entry.project_id, entry.number)) {
                        entry.additions = *additions;
                        entry.deletions = *deletions;
                    }
                }
                this.rebuild(cx);
            });
        })
        .detach();
    }

    /// The host's labels and authors for the Filters menu, beside those the loaded rows carry.
    fn fetch_options(&mut self, cx: &mut Context<Self>) {
        let mut scope = serde_json::Map::new();
        if let Some(project) = &self.prefs.project_id {
            scope.insert("projectId".into(), project.clone().into());
        }
        let state = self.state.read(cx);
        let input = |kind: &str| {
            let mut input = scope.clone();
            input.insert("kind".into(), kind.into());
            serde_json::Value::Object(input)
        };
        let labels = read(state, method::FILTER_OPTIONS, input("labels"), decode::<FilterOptions>);
        let authors = read(state, method::FILTER_OPTIONS, input("authors"), decode::<FilterOptions>);
        self._options = Some(cx.spawn(async move |this, cx| {
            let (labels, authors) = (settle(labels).await.unwrap_or_default(), settle(authors).await.unwrap_or_default());
            let _ = this.update(cx, |this, _| this.options = FilterOptions { labels: labels.labels, authors: authors.authors, truncated: false });
        }));
    }

    pub(super) fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(result) = &self.data.result else { return };
        if !result.next_cursors.is_empty() {
            let cursors = result.next_cursors.clone();
            self.fetch(Some(cursors), false, cx);
        } else if self.data.page_size < MAX_PAGE_SIZE {
            self.data.page_size = (self.data.page_size + PAGE_SIZE).min(MAX_PAGE_SIZE);
            self.data.loading_more = true;
            self.fetch(None, false, cx);
        }
    }

    pub(super) fn can_load_more(&self) -> bool {
        self.data.result.as_ref().is_some_and(|result| result.truncated && (!result.next_cursors.is_empty() || self.data.page_size < MAX_PAGE_SIZE))
    }

    /// "Check again": the host is asked afresh (`pullRequests.invalidate`), then the list re-read.
    pub(super) fn refresh_from_host(&mut self, cx: &mut Context<Self>) {
        let task = read(self.state.read(cx), method::INVALIDATE, t3_core::pull_requests::invalidate_input(None), |_| Ok(()));
        self.data.pending = true;
        self.stats.clear();
        self.requested.clear();
        cx.notify();
        self._list = Some(cx.spawn(async move |this, cx| {
            let _ = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.refetch(cx);
                if let Some(detail) = &this.detail {
                    detail.update(cx, |detail, cx| detail.refresh(cx));
                }
            });
        }));
    }

    /// Regroups and resorts the held rows; the list keeps its place.
    pub(super) fn rebuild(&mut self, cx: &mut Context<Self>) {
        let parsed = parse_query(&self.typed);
        let filters = self.prefs.menu_filters().overlay(&parsed.filters);
        let answered = self.data.key.as_deref() == Some(self.scope_key().as_str()) && self.typed.trim() == self.sent;
        self.groups = match &self.data.result {
            Some(result) => shown_groups(&Shown {
                entries: &result.entries,
                viewers: &result.viewers,
                providers: &result.providers,
                involvement: self.prefs.involvement,
                filters: &filters,
                text: &parsed.text,
                answered,
                sort: self.prefs.sort,
                state: self.prefs.state,
            }),
            None => Vec::new(),
        };
        let mut rows = Vec::new();
        for (index, group) in self.groups.iter().enumerate() {
            if !group.label.is_empty() {
                rows.push(Row::Header(index));
            }
            rows.extend(group.entries.iter().map(|entry| Row::Entry(*entry)));
        }
        if !rows.is_empty() && self.data.result.as_ref().is_some_and(|result| result.truncated) {
            rows.push(Row::Footer);
        }
        if rows != self.rows {
            let top = self.list.logical_scroll_top();
            self.list.reset(rows.len());
            if top.item_ix < rows.len() {
                self.list.scroll_to(top);
            }
            self.rows = rows;
        }
        cx.notify();
    }

    pub(super) fn shown_count(&self) -> usize {
        self.groups.iter().map(|group| group.entries.len()).sum()
    }

    pub(super) fn select(&mut self, reference: PrRef, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&reference) {
            return;
        }
        let entry = self.data.result.as_ref().and_then(|result| result.entries.iter().find(|entry| PrRef::of(entry) == reference)).cloned();
        let state = self.state.clone();
        let page = cx.entity().downgrade();
        self.detail = Some(cx.new(|cx| DetailView::new(state, page, reference.clone(), entry, cx)));
        self.selected = Some(reference);
        cx.notify();
    }

    pub(super) fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.detail = None;
        cx.notify();
    }

    /// A finished action: the row shows its new state, and leaves a listing of another state,
    /// until the next host read.
    pub(super) fn acted(&mut self, reference: &PrRef, action: Action, cx: &mut Context<Self>) {
        let Some(result) = self.data.result.as_mut() else { return };
        if let Some(entry) = result.entries.iter_mut().find(|entry| PrRef::of(entry) == *reference) {
            match action {
                Action::Close => entry.state = PrState::Closed,
                Action::Reopen => entry.state = PrState::Open,
                Action::Ready => entry.is_draft = false,
                Action::Draft => entry.is_draft = true,
                Action::Merge => entry.state = PrState::Merged,
                _ => {}
            }
        }
        self.rebuild(cx);
    }

    pub(super) fn remember_merge_method(&mut self, method: MergeMethod) {
        if self.prefs.merge_method != Some(method) {
            self.prefs.merge_method = Some(method);
            self.save_prefs();
        }
    }

    /// The project's and the last-picked merge methods, read when a merge is asked for.
    pub(super) fn merge_picks(&self, project_id: &str, current: Option<MergeMethod>, cx: &App) -> MergePicks {
        let project_default = self.state.read(cx).server_config.config.as_ref().and_then(|config| project_default_merge_method(&config.settings, project_id));
        MergePicks { current, project_default, last: self.prefs.merge_method }
    }

    /// A list row's quick action (`PullRequestSpeedActions`): runs immediately, no confirmation.
    /// A merge is only requested, since some hosts accept one before it lands, so the row waits
    /// for the host to say it merged.
    pub(super) fn quick_action(&mut self, index: usize, action: Action, cx: &mut Context<Self>) {
        let Some(entry) = self.data.result.as_ref().and_then(|result| result.entries.get(index)).cloned() else { return };
        let reference = PrRef::of(&entry);
        if !mutations::begin(&reference, action, cx) {
            return;
        }
        let term = t3_core::pull_requests::detail::change_request_term(&entry.provider);
        let task = if action == Action::Merge {
            let picks = self.merge_picks(&entry.project_id, None, cx);
            mutations::merge(self.state.read(cx), reference.clone(), picks, cx)
        } else {
            mutations::run_action(self.state.read(cx), action_input(&reference, action, None), cx)
        };
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                mutations::finish(&reference, cx);
                let toast = match &result {
                    Ok(()) => Toast::new(ToastKind::Success, action_done_label(action, term)),
                    Err(error) => Toast::new(ToastKind::Error, action_failed_label(action, term)).description(error.clone()),
                };
                if result.is_ok() && action != Action::Merge {
                    this.acted(&reference, action, cx);
                }
                this.state.update(cx, |state, cx| state.show_toast(toast, cx));
            });
        })
        .detach();
    }

    /// Shift alone shows the rows' quick actions (`PullRequestSpeedModeProvider`).
    pub fn set_modifiers(&mut self, modifiers: Modifiers, cx: &mut Context<Self>) {
        let speed = modifiers == Modifiers::shift() && self.state.read(cx).pull_requests_open;
        if speed != self.shift {
            self.shift = speed;
            cx.notify();
        }
    }

    /// `pullRequest.copyNumber` while a pull request is open here.
    pub fn copy_selected_number(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(reference) = self.selected.clone().filter(|_| self.state.read(cx).pull_requests_open) else { return false };
        self.copy(format!("#{}", reference.number), "Number", cx);
        true
    }

    pub(super) fn copy(&mut self, text: String, what: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        let toast = Toast::new(ToastKind::Success, format!("{what} copied")).description(text);
        self.state.update(cx, |state, cx| state.show_toast(toast, cx));
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let collapsed = self.state.read(cx).sidebar.collapsed;
        h_flex()
            .window_control_area(WindowControlArea::Drag)
            .h(px(crate::workspace::TITLEBAR_HEIGHT))
            .flex_none()
            .pl(px(if collapsed { 130. } else { 20. }))
            .pr(px(12.))
            .gap(px(12.))
            .border_b_1()
            .border_color(hsla(p.border))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Pull requests"))
    }

    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let shown = self.shown_count();
        let first_load = self.data.result.is_none();
        if first_load && self.data.error.is_none() {
            return super::row::ghost("Loading pull requests…", cx).into_any_element();
        }
        if let Some(error) = self.data.error.clone().filter(|_| shown == 0) {
            let retry = cx.listener(|this, _, _, cx| this.refetch(cx));
            return super::row::unavailable(error, self.data.pending, retry, cx).into_any_element();
        }
        if shown == 0 {
            return super::row::empty_state(self, window, cx).into_any_element();
        }
        list(self.list.clone(), cx.processor(|this, index: usize, window, cx| super::row::render(this, index, window, cx)))
            .flex_1()
            .into_any_element()
    }
}

impl Render for PullRequestsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let error_with_rows = self.data.error.clone().filter(|_| self.shown_count() > 0);
        let detail = self.detail.clone();
        let detail_width = (f32::from(window.viewport_size().width) / 2.).max(DETAIL_MIN_WIDTH);
        h_flex()
            .id("pull-requests-page")
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .bg(hsla(p.background))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_header(cx))
                    .child(super::toolbar::render(self, window, cx))
                    .children(error_with_rows.map(|error| {
                        let retry = cx.listener(|this, _, _, cx| this.refetch(cx));
                        h_flex()
                            .flex_none()
                            .mx(px(20.))
                            .mb(px(8.))
                            .px(px(12.))
                            .py(px(8.))
                            .gap(px(12.))
                            .justify_between()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(hsla(p.warning).opacity(0.3))
                            .bg(hsla(p.warning_surface))
                            .text_xs()
                            .child(format!("{error} Showing the last pull requests loaded."))
                            .child(Button::new("pr-list-retry", Variant::Outline, Size::Xs).label("Retry").on_click(retry))
                    }))
                    .child(div().flex_1().min_h_0().flex().flex_col().px(px(8.)).child(self.render_body(window, cx))),
            )
            .children(detail.map(|detail| {
                div()
                    .flex_none()
                    .h_full()
                    .w(px(detail_width))
                    .max_w(relative(0.6))
                    .border_l_1()
                    .border_color(hsla(p.border))
                    .child(detail)
            }))
    }
}
