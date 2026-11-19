//! The pull requests page (`PullRequestsPage.tsx`, `PullRequestsColumn.tsx`): one list read per
//! scope with a debounced host search, persisted cold-start list and diff-stat caches
//! (`pullRequestStatsPersistence.ts`), hover-intent detail prefetch (`usePullRequestIntentPrefetch.ts`),
//! link hover preview cards (`PullRequestLinkPreview.tsx`), stacked project-assignment column
//! (`pullRequestProjectAssignment.logic.ts`), grouped and sorted rows in a virtualized list, and
//! the selected pull request's detail beside it. While open it follows
//! `pullRequests.subscribeRefreshes`, re-reading the list and the open detail on each new epoch
//! (`refreshTrigger` in `client-runtime/src/state/pullRequests.ts`).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, ListAlignment, ListState, Modifiers,
    ParentElement, Render, SharedString, Styled, Subscription, Task, Window, WindowControlArea, div, list, prelude::*, px, relative,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use t3_client::store;
use t3_core::pull_requests::assignment::{
    AssignableProject, AssignedProjectColumnRow, EnvironmentInfo, HostAvailability, project_assignment_rows, remember_hosts, reset_hosts,
};
use t3_core::pull_requests::cache::{LIST_CACHE_KEY, PersistedListCache, PersistedStatsCache, STATS_CACHE_KEY};
use t3_core::pull_requests::editing::patch_list_entry;
use t3_core::pull_requests::list::{Group, Shown, entry_key, needs_stats, shown_groups};
use t3_core::pull_requests::query::parse_query;
use t3_core::pull_requests::wire::{Action, Activity, Detail, FilterOptions, ListResult, ListStats, MergeMethod, PrState, Stack, Summary, decode};
use t3_core::pull_requests::{
    ListPreferences, MAX_PAGE_SIZE, PAGE_SIZE, PREFERENCES_KEY, PrRef, SEARCH_DEBOUNCE_MS, action_done_label, action_failed_label, action_input, list_accepts, list_input, method,
    project_default_merge_method, stats_inputs,
};

use super::controls::{Button, Size, Variant};
use super::detail::{DetailView, has_held_detail, prefetch_into_held};
use super::mutations::{self, MergePicks, Mutations};
use super::preview::{LinkPreviewController, PreviewTarget};
use super::thread_panel::PendingSelection;
use super::{read, settle};
use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

pub(super) const ROW_HEIGHT: f32 = 56.;
/// A bound on remembered line counts.
const MAX_HELD_STATS: usize = 4000;
/// `INTENT_DELAY_MS` (`usePullRequestIntentPrefetch.ts`).
pub(super) const INTENT_PREFETCH_DELAY: Duration = Duration::from_millis(150);
const PROJECT_COLUMN_WIDTH: f32 = 200.;

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
pub(super) struct Refreshes {
    pub(super) net: tokio::task::JoinHandle<()>,
    pub(super) _ui: Task<()>,
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
    /// Persisted cold-start caches (`pullRequestStatsPersistence.ts` + list scope cache).
    list_cache: PersistedListCache,
    stats_cache: PersistedStatsCache,
    /// Line counts by `pullRequestDiffStatKey`, kept across reads (GitHub listings omit them).
    stats: HashMap<String, (u64, u64)>,
    requested: HashSet<String>,
    /// A refresh epoch passed, so counts `listStats` measured are read again.
    restat: bool,
    /// Host availability and stacked project-assignment column state (`pullRequestProjectAssignment.logic.ts`).
    host_availability: HostAvailability,
    projects_column_open: bool,
    /// Per-project counts preserved across project-scoped reads, keyed by `(env, unfiltered_scope_key)`.
    project_counts_scope: Option<(String, String)>,
    project_counts: HashMap<String, usize>,
    project_unavailable: HashSet<String>,
    all_projects_total: Option<usize>,
    /// Lazily loaded stack info per row key for `PullRequestStackPopover`.
    pub(super) row_stacks: HashMap<String, Result<Option<Stack>, String>>,
    row_stack_loading: HashSet<String>,
    /// Hover-intent prefetch (`usePullRequestIntentPrefetch.ts`).
    intent_target: Option<PrRef>,
    _intent_timer: Option<Task<()>>,
    _intent_fetch: Option<Task<()>>,
    /// Link hover preview card (`PullRequestLinkPreview.tsx`).
    pub(super) preview: LinkPreviewController,
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
        let list_cache = PersistedListCache::from_value(store::load_ui_state(LIST_CACHE_KEY));
        let stats_cache = PersistedStatsCache::from_value(store::load_ui_state(STATS_CACHE_KEY));
        let typed = prefs.q.clone().unwrap_or_default();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search pull requests, or label:bug").default_value(typed.clone()));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => this.typed_changed(input.read(cx).value().to_string(), cx),
                InputEvent::PressEnter { .. } => this.submit_query(cx),
                _ => {}
            }),
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
                if matches!(event, AppEvent::Phase | AppEvent::Selection | AppEvent::Shell) {
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
            list_cache,
            stats_cache,
            stats: HashMap::new(),
            requested: HashSet::new(),
            restat: false,
            host_availability: HashMap::new(),
            projects_column_open: true,
            project_counts_scope: None,
            project_counts: HashMap::new(),
            project_unavailable: HashSet::new(),
            all_projects_total: None,
            row_stacks: HashMap::new(),
            row_stack_loading: HashSet::new(),
            intent_target: None,
            _intent_timer: None,
            _intent_fetch: None,
            preview: LinkPreviewController::default(),
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
        page.hydrate_from_cache(cx);
        page.sync(cx);
        page
    }

    fn environment_key(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        state.environment().or(state.environments.active.as_deref()).map(str::to_owned)
    }

    /// Hydrates cold-start list rows and diff stats from the persisted cache so the page paints
    /// immediately before the network read finishes (`pullRequestStatsPersistence.ts`).
    fn hydrate_from_cache(&mut self, cx: &mut Context<Self>) {
        let Some(env) = self.environment_key(cx) else { return };
        let key = self.scope_key();
        let unfiltered_key = self.unfiltered_scope_key();
        if self.data.key.as_deref() == Some(key.as_str()) && self.data.result.is_some() {
            return;
        }
        let Some(mut cached) = self.list_cache.get(&env, &key).cloned() else { return };
        self.stats_cache.hydrate_list(&env, &mut cached);
        for entry in &cached.entries {
            if !needs_stats(entry) {
                self.stats.insert(stat_key(&entry.project_id, entry.number), (entry.additions, entry.deletions));
            }
        }
        let project_filter = self.prefs.project_id.clone();
        self.record_project_counts(Some(&env), &unfiltered_key, project_filter.as_deref(), &cached);
        self.data.first_page = cached.entries.len();
        self.data.key = Some(key);
        self.data.result = Some(cached);
        self.rebuild(cx);
    }

    /// Updates per-project counts and `"All projects"` total without collapsing sibling project
    /// counts when the list is filtered to a single `project_id`.
    fn record_project_counts(&mut self, env: Option<&str>, unfiltered_key: &str, project_filter: Option<&str>, result: &ListResult) {
        let scope_id = (env.unwrap_or("").to_owned(), unfiltered_key.to_owned());
        if self.project_counts_scope.as_ref() != Some(&scope_id) {
            self.project_counts.clear();
            self.project_unavailable.clear();
            self.all_projects_total = None;
            self.project_counts_scope = Some(scope_id);
            if project_filter.is_some()
                && let Some(unfiltered) = env.and_then(|e| self.list_cache.get(e, unfiltered_key))
            {
                for entry in &unfiltered.entries {
                    *self.project_counts.entry(entry.project_id.clone()).or_default() += 1;
                }
                for error in &unfiltered.errors {
                    self.project_unavailable.insert(error.project_id.clone());
                }
                self.all_projects_total = Some(unfiltered.entries.len());
            }
        }
        match project_filter {
            None => {
                self.project_counts.clear();
                self.project_unavailable.clear();
                for entry in &result.entries {
                    *self.project_counts.entry(entry.project_id.clone()).or_default() += 1;
                }
                for error in &result.errors {
                    self.project_unavailable.insert(error.project_id.clone());
                }
                self.all_projects_total = Some(result.entries.len());
            }
            Some(pid) => {
                let count = result.entries.iter().filter(|entry| entry.project_id == pid).count();
                self.project_counts.insert(pid.to_owned(), count);
                self.project_unavailable.remove(pid);
                if result.errors.iter().any(|error| error.project_id == pid) {
                    self.project_unavailable.insert(pid.to_owned());
                }
                self.all_projects_total = Some(self.project_counts.values().sum());
            }
        }
    }

    /// Reads when the page is shown and connected, again on reopening and on reconnecting, and
    /// follows the refresh signal only while both hold.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let connected = state.client_handle().is_some() && matches!(state.phase, crate::app_state::Phase::Online(t3_client::supervisor::ConnectionStatus::Connected));
        let open = state.pull_requests_open;
        let returned = connected && open && (!self.connected || !self.open);
        if open && let Some((reference, url)) = cx.try_global::<PendingSelection>().and_then(|pending| pending.0.clone()) {
            cx.set_global(PendingSelection(None));
            self.select(reference, cx);
            if let (Some(url), Some(detail)) = (url, &self.detail) {
                detail.update(cx, |detail, cx| detail.reveal(&url, cx));
            }
        }
        self.connected = connected;
        self.open = open;
        if !(open && connected) {
            self.refreshes = None;
            self.epoch = None;
            self.cancel_intent();
            self.preview.clear();
        } else if self.refreshes.is_none() {
            self.follow_refreshes(cx);
        }
        if open && self.data.result.is_none() {
            self.hydrate_from_cache(cx);
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

    /// Hover-intent prefetch (`usePullRequestIntentPrefetch.ts`): after 150ms resting on a listed
    /// row that is not already selected or held in `DetailView`'s `Held` cache, prefetches
    /// `pullRequests.detail` (`allowStale: false`) + `pullRequests.activity` into `Held`.
    pub(super) fn hover_row(&mut self, target: Option<(PrRef, PreviewTarget)>, cx: &mut Context<Self>) {
        let Some((reference, preview_target)) = target else {
            self.cancel_intent();
            self.preview.hover(&self.state.clone(), None, |page: &mut Self| &mut page.preview, cx);
            return;
        };
        let state_entity = self.state.clone();
        self.preview.hover(&state_entity, Some(preview_target), |page: &mut Self| &mut page.preview, cx);
        if self.selected.as_ref() == Some(&reference) || has_held_detail(self.state.read(cx), &reference, cx) {
            self.cancel_intent();
            return;
        }
        if self.intent_target.as_ref() == Some(&reference) {
            return;
        }
        self.cancel_intent();
        self.intent_target = Some(reference.clone());
        self._intent_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(INTENT_PREFETCH_DELAY).await;
            let _ = this.update(cx, |page, cx| page.run_intent_prefetch(reference, cx));
        }));
    }

    fn cancel_intent(&mut self) {
        self.intent_target = None;
        self._intent_timer = None;
        self._intent_fetch = None;
    }

    fn is_listed(&self, reference: &PrRef) -> bool {
        self.data.result.as_ref().is_some_and(|result| result.entries.iter().any(|entry| PrRef::of(entry) == *reference))
    }

    fn run_intent_prefetch(&mut self, reference: PrRef, cx: &mut Context<Self>) {
        if self.intent_target.as_ref() != Some(&reference) || !self.is_listed(&reference) {
            return;
        }
        let mut payload = reference.payload();
        payload["allowStale"] = serde_json::json!(false);
        let detail_task = read(self.state.read(cx), method::DETAIL, payload, decode::<Detail>);
        let activity_task = read(self.state.read(cx), method::ACTIVITY, reference.payload(), decode::<Activity>);
        self._intent_fetch = Some(cx.spawn(async move |this, cx| {
            let (detail, activity) = (settle(detail_task).await.ok(), settle(activity_task).await.ok());
            let _ = this.update(cx, |page, cx| {
                if page.is_listed(&reference) {
                    let state = page.state.clone();
                    prefetch_into_held(&state, &reference, detail.as_ref(), activity.as_ref(), cx);
                }
            });
        }));
    }

    /// Lazily loads `pullRequests.stack` for a stacked row's `PullRequestStackPopover`.
    pub(super) fn ensure_row_stack(&mut self, reference: PrRef, force: bool, cx: &mut Context<Self>) {
        let key = reference.key();
        if !force && (self.row_stacks.contains_key(&key) || self.row_stack_loading.contains(&key)) {
            return;
        }
        self.row_stack_loading.insert(key.clone());
        let task = read(self.state.read(cx), method::STACK, reference.payload(), decode::<Option<Stack>>);
        cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |page, cx| {
                page.row_stack_loading.remove(&key);
                page.row_stacks.insert(key, res);
                cx.notify();
            });
        })
        .detach();
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
        self.restat = true;
        self.fetch(None, true, cx);
        if let Some(detail) = &self.detail {
            detail.update(cx, |detail, cx| detail.refresh(cx));
        }
    }

    fn scope_key_for(&self, project_id: Option<&str>) -> String {
        let mut scope = ListPreferences { sort: Default::default(), q: None, merge_method: None, ..self.prefs.clone() };
        scope.project_id = project_id.map(str::to_owned);
        format!("{}|{}", serde_json::to_string(&scope).unwrap_or_default(), self.sent)
    }

    fn scope_key(&self) -> String {
        self.scope_key_for(self.prefs.project_id.as_deref())
    }

    fn unfiltered_scope_key(&self) -> String {
        self.scope_key_for(None)
    }

    pub(super) fn save_prefs(&mut self) {
        store::save_ui_state(PREFERENCES_KEY, &self.prefs);
    }

    /// A scope change: persist, hydrate from cache if present, re-read from the first page.
    pub(super) fn update_prefs(&mut self, change: impl FnOnce(&mut ListPreferences), cx: &mut Context<Self>) {
        let before = self.scope_key();
        change(&mut self.prefs);
        self.save_prefs();
        if self.scope_key() != before {
            self.data.page_size = PAGE_SIZE;
            self.hydrate_from_cache(cx);
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
            self.hydrate_from_cache(cx);
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
                let env_key = self.environment_key(cx);
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
                if let Some(env) = &env_key {
                    self.stats_cache.hydrate_list(env, &mut answer);
                    self.host_availability = remember_hosts(&self.host_availability, &[(env.as_str(), &answer.providers, now_ms())]);
                }
                for entry in answer.entries.iter_mut().filter(|entry| needs_stats(entry)) {
                    if let Some((additions, deletions)) = self.stats.get(&stat_key(&entry.project_id, entry.number)) {
                        entry.additions = *additions;
                        entry.deletions = *deletions;
                    }
                }
                if let Some(env) = &env_key {
                    self.list_cache.put(env, &key, answer.clone(), now_ms());
                    store::save_ui_state(LIST_CACHE_KEY, &self.list_cache);
                }
                let unfiltered_key = self.unfiltered_scope_key();
                let project_filter = self.prefs.project_id.clone();
                self.record_project_counts(env_key.as_deref(), &unfiltered_key, project_filter.as_deref(), &answer);
                self.data.error = None;
                self.data.key = Some(key);
                self.data.result = Some(answer);
                if self.intent_target.as_ref().is_some_and(|target| !self.is_listed(target)) {
                    self.cancel_intent();
                }
                self.fetch_stats(cx);
                #[cfg(feature = "snapshot")]
                self.select_for_snapshot(cx);
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
        let restat = std::mem::take(&mut self.restat);
        let wanted: Vec<_> = result
            .entries
            .iter()
            .filter(|entry| {
                let key = stat_key(&entry.project_id, entry.number);
                (needs_stats(entry) || (restat && self.stats.contains_key(&key))) && !self.requested.contains(&key)
            })
            .collect();
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
                if let Some(env) = this.environment_key(cx) {
                    this.stats_cache.record(&env, &stats, now_ms());
                    store::save_ui_state(STATS_CACHE_KEY, &this.stats_cache);
                }
                let mut answered = HashSet::new();
                for stat in stats.stats {
                    let key = stat_key(&stat.project_id, stat.number);
                    this.stats.insert(key.clone(), (stat.additions, stat.deletions));
                    answered.insert(key);
                }
                let Some(result) = this.data.result.as_mut() else { return };
                for entry in result.entries.iter_mut() {
                    let key = stat_key(&entry.project_id, entry.number);
                    if let Some((additions, deletions)) = this.stats.get(&key).filter(|_| answered.contains(&key)) {
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

    /// "Check again": the host is asked afresh (`pullRequests.invalidate`), caches and host
    /// availability reset, then the list re-read.
    pub(super) fn refresh_from_host(&mut self, cx: &mut Context<Self>) {
        let task = read(self.state.read(cx), method::INVALIDATE, t3_core::pull_requests::invalidate_input(None), |_| Ok(()));
        self.data.pending = true;
        self.stats.clear();
        self.requested.clear();
        self.row_stacks.clear();
        self.host_availability = reset_hosts(&self.host_availability);
        if let Some(env) = self.environment_key(cx) {
            self.list_cache.clear_environment(&env);
            self.stats_cache.clear_environment(&env);
            store::save_ui_state(LIST_CACHE_KEY, &self.list_cache);
            store::save_ui_state(STATS_CACHE_KEY, &self.stats_cache);
        }
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
        self.cancel_intent();
        self.preview.clear();
        let entry = self.data.result.as_ref().and_then(|result| result.entries.iter().find(|entry| PrRef::of(entry) == reference)).cloned();
        let state = self.state.clone();
        let page = cx.entity().downgrade();
        self.detail = Some(cx.new(|cx| DetailView::new(state, page, reference.clone(), entry, cx)));
        self.selected = Some(reference);
        cx.notify();
    }

    #[cfg(feature = "snapshot")]
    fn select_for_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some((number, code)) = cx.try_global::<super::SnapshotSelection>().map(|wanted| (wanted.number, wanted.code)) else { return };
        let Some(reference) = self.data.result.as_ref().and_then(|result| result.entries.iter().find(|entry| entry.number == number)).map(PrRef::of) else { return };
        cx.remove_global::<super::SnapshotSelection>();
        self.select(reference, cx);
        if code && let Some(detail) = &self.detail {
            detail.update(cx, |detail, cx| detail.set_tab(super::detail::Tab::Code, cx));
        }
    }

    pub(super) fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.detail = None;
        cx.notify();
    }

    /// A finished action. With the host's reply the row shows what the host said, and leaves a
    /// listing that no longer takes it (`patchPullRequestListEntries`); without one, what the
    /// action implies, until the next host read.
    pub(super) fn acted(&mut self, reference: &PrRef, action: Action, reply: Option<&Summary>, cx: &mut Context<Self>) {
        let Some(result) = self.data.result.as_mut() else { return };
        let Some(index) = result.entries.iter().position(|entry| PrRef::of(entry) == *reference) else { return };
        let entry = &mut result.entries[index];
        match reply {
            Some(reply) => {
                let (prefs, sent) = (&self.prefs, self.sent.as_str());
                if !patch_list_entry(entry, reply, |state, is_draft| list_accepts(prefs, sent, state, is_draft)) {
                    result.entries.remove(index);
                }
            }
            None => match action {
                Action::Close => entry.state = PrState::Closed,
                Action::Reopen => entry.state = PrState::Open,
                Action::Ready => entry.is_draft = false,
                Action::Draft => entry.is_draft = true,
                Action::Merge => entry.state = PrState::Merged,
                _ => {}
            },
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
                    Ok(_) => Toast::new(ToastKind::Success, action_done_label(action, term)),
                    Err(error) => Toast::new(ToastKind::Error, action_failed_label(action, term)).description(error.clone()),
                };
                match &result {
                    Ok(Some(reply)) => {
                        this.acted(&reference, action, Some(reply), cx);
                        if this.selected.as_ref() == Some(&reference)
                            && let Some(detail) = this.detail.clone()
                        {
                            detail.update(cx, |detail, cx| {
                                if detail.apply_reply(Some(reply), cx) {
                                    detail.refresh(cx);
                                }
                            });
                        }
                    }
                    Ok(None) if action != Action::Merge => {
                        this.acted(&reference, action, None, cx);
                        if this.selected.as_ref() == Some(&reference)
                            && let Some(detail) = this.detail.clone()
                        {
                            detail.update(cx, |detail, cx| detail.refresh(cx));
                        }
                    }
                    _ => {}
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

    /// Computes the project-assignment rows and `"All projects"` total for the active environment
    /// (matching the server `PullRequestsPage::fetch` queries, so foreign fleet projects are never
    /// shown with bogus `0` counts or misrouted on click).
    pub(crate) fn assignment_rows(&self, cx: &App) -> (Vec<AssignedProjectColumnRow>, usize) {
        let state = self.state.read(cx);
        let env_id = self.environment_key(cx).unwrap_or_else(|| "local".into());
        let env_label = state
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.label.clone())
            .or_else(|| state.active_environment().map(|e| e.label.clone()))
            .unwrap_or_else(|| "Local".to_owned());
        let env_ids = vec![env_id.clone()];
        let envs = vec![EnvironmentInfo { environment_id: env_id.clone(), label: env_label }];
        let mut projects: Vec<AssignableProject> = Vec::new();
        if let Some(snapshot) = &state.shell.snapshot {
            for project in snapshot.projects.iter().filter(|p| p.archived_at.is_none()) {
                let repo_key = project
                    .repository_identity
                    .as_ref()
                    .and_then(|id| id.get("canonicalKey").and_then(serde_json::Value::as_str))
                    .map(str::to_owned);
                let host = repo_key.as_deref().and_then(|key| key.split('/').next()).map(str::to_owned);
                projects.push(AssignableProject {
                    id: project.id.clone(),
                    environment_id: env_id.clone(),
                    title: project.title.clone(),
                    workspace_root: project.workspace_root.clone(),
                    repository_key: repo_key,
                    host,
                });
            }
        }
        let rows = project_assignment_rows(&projects, &env_ids, &envs, Some(&self.host_availability), &self.project_counts, &self.project_unavailable);
        let total = self.all_projects_total.unwrap_or_else(|| self.data.result.as_ref().map_or(0, |r| r.entries.len()));
        (rows, total)
    }

    /// Stacked project-assignment column (`pullRequestProjectAssignment.logic.ts`): deduplicates
    /// shared repositories on the active environment and lets the user scope the list to any project
    /// while preserving sibling project counts.
    fn render_project_assignment_column(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.projects_column_open {
            return None;
        }
        let (rows, total) = self.assignment_rows(cx);
        if rows.is_empty() {
            return None;
        }
        let p = palette(cx);
        let active_project = self.prefs.project_id.clone();
        Some(
            v_flex()
                .id("pr-project-assignment-column")
                .flex_none()
                .w(px(PROJECT_COLUMN_WIDTH))
                .h_full()
                .border_r_1()
                .border_color(hsla(p.border))
                .p(px(8.))
                .gap(px(2.))
                .overflow_y_scroll()
                .child(div().px(px(8.)).py(px(6.)).text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Projects"))
                .child(
                    h_flex()
                        .id("pr-assign-all")
                        .gap(px(6.))
                        .px(px(8.))
                        .py(px(6.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_xs()
                        .when(active_project.is_none(), |this| this.bg(hsla(p.accent)).font_weight(FontWeight::MEDIUM))
                        .when(active_project.is_some(), |this| this.hover(|s| s.bg(hsla(p.accent).opacity(0.6))))
                        .on_click(cx.listener(|this, _, _, cx| this.update_prefs(|p| p.project_id = None, cx)))
                        .child(div().flex_1().truncate().text_color(hsla(p.foreground)).child("All projects"))
                        .child(div().text_color(hsla(p.muted_foreground)).child(total.to_string())),
                )
                .children(rows.into_iter().enumerate().map(|(ix, row)| {
                    let selected = active_project.as_deref() == Some(row.project_id.as_str());
                    let pid = row.project_id.clone();
                    let blocked = row.unavailable;
                    v_flex()
                        .id(SharedString::from(format!("pr-assign-proj-{ix}")))
                        .px(px(8.))
                        .py(px(6.))
                        .rounded(px(6.))
                        .when(!blocked, |this| this.cursor_pointer())
                        .when(selected, |this| this.bg(hsla(p.accent)))
                        .when(!selected && !blocked, |this| this.hover(|s| s.bg(hsla(p.accent).opacity(0.6))))
                        .when(blocked, |this| this.opacity(0.55))
                        .when(!blocked, |this| {
                            this.on_click(cx.listener(move |this, _, _, cx| {
                                let pid = pid.clone();
                                this.update_prefs(move |prefs| prefs.project_id = Some(pid), cx);
                            }))
                        })
                        .child(
                            h_flex()
                                .gap(px(6.))
                                .min_w_0()
                                .text_xs()
                                .child(div().flex_1().min_w_0().truncate().font_weight(if selected { FontWeight::MEDIUM } else { FontWeight::NORMAL }).text_color(hsla(p.foreground)).child(row.project_title))
                                .child(div().flex_none().text_color(hsla(p.muted_foreground)).child(if blocked { "—".to_owned() } else { row.pr_count.to_string() })),
                        )
                        .when(blocked, |this| {
                            this.child(div().truncate().text_size(px(10.)).text_color(hsla(p.muted_foreground)).child(format!("{} · Unavailable", row.environment_label)))
                        })
                }))
                .into_any_element(),
        )
    }

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let collapsed = crate::workspace::sidebar_inline_width(self.state.read(cx), f32::from(window.viewport_size().width)) == 0.;
        h_flex()
            .window_control_area(WindowControlArea::Drag)
            .h(px(crate::workspace::TITLEBAR_HEIGHT))
            .flex_none()
            .pl(px(if collapsed { 130. } else { 20. }))
            .pr(px(12.))
            .gap(px(12.))
            .border_b_1()
            .border_color(hsla(p.border))
            .child(div().flex_1().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Pull requests"))
            .child(
                Button::new("pr-toggle-projects-col", Variant::GhostMuted, Size::Xs)
                    .icon(Icon::PanelLeft)
                    .tooltip(if self.projects_column_open { "Hide project assignment column" } else { "Show project assignment column" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.projects_column_open = !this.projects_column_open;
                        cx.notify();
                    })),
            )
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
        let projects_col = self.render_project_assignment_column(cx);
        let preview_card = self.preview.render_card(cx);
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
                    .relative()
                    .child(self.render_header(window, cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .children(projects_col)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
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
                            ),
                    )
                    .children(preview_card.map(|card| div().absolute().right(px(16.)).bottom(px(16.)).child(card))),
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use t3_core::pull_requests::wire::{Detail, ListEntry, decode};

    use super::*;

    #[gpui::test]
    fn hover_prefetch_seeds_held_and_preview_controller_renders_fallback_card(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let entry: ListEntry = decode(json!({
            "provider": "github", "host": "github.com", "projectId": "p1", "projectTitle": "Repo",
            "repository": "acme/app", "number": 7, "title": "Improve cold start cache",
            "url": "https://github.com/acme/app/pull/7", "author": { "login": "alice" },
            "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 10, "deletions": 2, "viewerReviewRequested": false, "headBranch": "feat/cache", "baseBranch": "main",
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z"
        }))
        .unwrap();
        let detail: Detail = decode(json!({
            "provider": "github",
            "capabilities": { "actions": [], "mergeMethods": ["merge"], "edit": { "changeRequest": false, "comment": false }, "review": { "resolve": false }, "labels": false },
            "viewerPermissions": { "actions": [], "comment": false, "resolve": false, "labels": false },
            "projectId": "p1", "projectTitle": "Repo", "workspaceRoot": "/tmp/repo", "repository": "acme/app", "number": 7,
            "title": "Improve cold start cache", "body": "", "url": "https://github.com/acme/app/pull/7", "author": { "login": "alice" },
            "viewer": "alice", "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 10, "deletions": 2, "changedFiles": 1,
            "headBranch": "feat/cache", "baseBranch": "main", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "observedAt": 100, "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
            "mergeCapabilities": { "merge": true, "squash": false, "rebase": false }
        }))
        .unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            state.update(cx, |state, _| state.environments.active = Some("env-a".into()));

            let reference = PrRef::of(&entry);
            assert!(!has_held_detail(state.read(cx), &reference, cx));
            prefetch_into_held(&state, &reference, Some(&detail), None, cx);
            assert!(has_held_detail(state.read(cx), &reference, cx));

            // LinkPreviewController renders immediately from fallback summary and clears cleanly.
            struct Host {
                preview: LinkPreviewController,
            }
            let host = cx.new(|_| Host { preview: LinkPreviewController::default() });
            host.update(cx, |host, cx| {
                assert!(!host.preview.is_open());
                host.preview.open_now(&state, PreviewTarget::from_entry(&entry), |h| &mut h.preview, cx);
                assert!(host.preview.is_open());
                assert_eq!(host.preview.active_url(), Some("https://github.com/acme/app/pull/7"));
                assert!(host.preview.render_card(cx).is_some());
                host.preview.clear();
                assert!(!host.preview.is_open());
                assert!(host.preview.render_card(cx).is_none());
            });
        });
    }

    #[gpui::test]
    fn project_assignment_preserves_sibling_counts_and_excludes_foreign_fleet_projects(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            state.update(cx, |state, _| {
                state.environments.active = Some("env-a".into());
                state.shell.snapshot = Some(serde_json::from_value(json!({
                    "snapshotSequence": 1,
                    "projects": [
                        {
                            "id": "p1", "title": "Alpha", "workspaceRoot": "/tmp/alpha",
                            "defaultModelSelection": null, "scripts": [],
                            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"
                        },
                        {
                            "id": "p2", "title": "Beta", "workspaceRoot": "/tmp/beta",
                            "defaultModelSelection": null, "scripts": [],
                            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"
                        }
                    ],
                    "threads": [],
                    "updatedAt": "2026-01-02T00:00:00Z"
                })).unwrap());
            });
            state
        });
        let (page, cx) = cx.add_window_view(|window, cx| PullRequestsPage::new(state.clone(), window, cx));
        let entry = |project_id: &str, project_title: &str, number: u64| -> serde_json::Value {
            json!({
                "provider": "github", "host": "github.com", "projectId": project_id, "projectTitle": project_title,
                "repository": format!("acme/{project_id}"), "number": number, "title": format!("PR #{number}"),
                "url": format!("https://github.com/acme/{project_id}/pull/{number}"), "author": { "login": "alice" },
                "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 5, "deletions": 1,
                "viewerReviewRequested": false, "headBranch": "feat", "baseBranch": "main",
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z"
            })
        };
        let unfiltered: ListResult = decode(json!({
            "entries": [entry("p1", "Alpha", 1), entry("p1", "Alpha", 2), entry("p2", "Beta", 3)],
            "providers": [],
            "errors": [],
            "hasMore": false
        }))
        .unwrap();
        let filtered_p1: ListResult = decode(json!({
            "entries": [entry("p1", "Alpha", 1), entry("p1", "Alpha", 2)],
            "providers": [],
            "errors": [],
            "hasMore": false
        }))
        .unwrap();

        page.update(cx, |page, cx| {
            let key = page.scope_key();
            page.apply_list(key, false, false, Ok(unfiltered), cx);
            let (rows, total) = page.assignment_rows(cx);
            assert_eq!(total, 3);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows.iter().find(|r| r.project_id == "p1").map(|r| r.pr_count), Some(2));
            assert_eq!(rows.iter().find(|r| r.project_id == "p2").map(|r| r.pr_count), Some(1));

            // Selecting p1 and receiving a project-scoped response must keep sibling p2's count (1)
            // and the "All projects" total (3) intact rather than collapsing them to 0 / 2.
            page.prefs.project_id = Some("p1".into());
            let filtered_key = page.scope_key();
            page.apply_list(filtered_key, false, false, Ok(filtered_p1), cx);
            let (rows_after, total_after) = page.assignment_rows(cx);
            assert_eq!(total_after, 3);
            assert_eq!(rows_after.iter().find(|r| r.project_id == "p1").map(|r| r.pr_count), Some(2));
            assert_eq!(rows_after.iter().find(|r| r.project_id == "p2").map(|r| r.pr_count), Some(1));
        });
    }
}
