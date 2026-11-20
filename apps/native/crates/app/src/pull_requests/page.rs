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
use t3_client::supervisor::ConnectionStatus;
use t3_core::auth::ORCHESTRATION_READ_SCOPE;
use t3_core::issues::saved_views::{
    create_saved_view, decode_saved_views, delete_saved_view, rename_saved_view,
    update_saved_view_preferences,
};
use t3_core::pull_requests::assignment::{
    AssignableProject, AssignedProjectColumnRow, EnvironmentInfo, HostAvailability,
    project_assignment_rows_with_unreachable, pull_request_project_queries_with_unreachable,
    remember_hosts, reset_hosts,
};
use t3_core::pull_requests::cache::{LIST_CACHE_KEY, PersistedListCache, PersistedStatsCache, STATS_CACHE_KEY};
use t3_core::pull_requests::editing::patch_list_entry;
use t3_core::pull_requests::list::{Group, Shown, entry_key, merge_pull_request_lists, needs_stats, shown_groups};
use t3_core::pull_requests::query::parse_query;
use t3_core::pull_requests::saved_views::{
    PR_SAVED_VIEWS_KEY, PR_SAVED_VIEW_PLACEHOLDER, PrSavedView, apply_saved_view_patch,
};
use t3_core::pull_requests::wire::{Action, Activity, Detail, FilterOptions, ListResult, ListStats, MergeMethod, PrState, ProviderSummary, Stack, Summary, decode};
use t3_core::pull_requests::{
    ListPreferences, MAX_PAGE_SIZE, PAGE_SIZE, PREFERENCES_KEY, PrRef, SEARCH_DEBOUNCE_MS, action_done_label, action_failed_label, action_input, list_accepts, list_input, method,
    project_default_merge_method, stats_inputs,
};
use t3_core::sidebar::grouping::EnvironmentOwner;

use super::controls::{Button, Size, Variant};
use super::detail::{DetailView, has_held_detail_in_env, prefetch_into_held_in_env};
use super::mutations::{self, MergePicks, Mutations};
use super::preview::{LinkPreviewController, PreviewTarget};
use super::thread_panel::PendingSelection;
use super::{owner_for_pr, read, read_for_owner, settle};
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
/// Cooldown after a transient RPC failure on a connected fleet environment before retrying its
/// shared repositories (`pingdotgg/t3code#34088`). Disconnected environments remain excluded via
/// live `ConnectionStatus` until reconnect.
const UNREACHABLE_TTL_MS: i64 = 30_000;

struct PrFleetTarget {
    owner: Option<EnvironmentOwner>,
    connected: bool,
}

struct FleetInventory {
    projects: Vec<AssignableProject>,
    env_ids: Vec<String>,
    envs: Vec<EnvironmentInfo>,
    targets: HashMap<String, PrFleetTarget>,
    unreachable: HashSet<String>,
}

enum FetchCursors {
    Single(HashMap<String, String>),
    PerEnv {
        cursors: HashMap<String, HashMap<String, String>>,
        regrown: HashSet<String>,
    },
}

struct MergedFleetList {
    result: ListResult,
    entry_environments: HashMap<String, String>,
    next_cursors_by_env: HashMap<String, HashMap<String, String>>,
    truncated_envs: HashSet<String>,
}

/// Merges multi-environment `pullRequests.list` results using `merge_pull_request_lists` so
/// cross-server duplicates are collapsed by `entry.url` (`pingdotgg/t3code#37704`), while
/// retaining each entry's owning `environment_id` and per-environment pagination cursors.
fn merge_fleet_list_results(answers: &[(String, ListResult)]) -> MergedFleetList {
    let slices: Vec<(&str, &[t3_core::pull_requests::wire::ListEntry])> = answers
        .iter()
        .map(|(env_id, result)| (env_id.as_str(), result.entries.as_slice()))
        .collect();
    let merged_entries = merge_pull_request_lists(&slices);
    let mut entry_environments = HashMap::with_capacity(merged_entries.len());
    let mut entries = Vec::with_capacity(merged_entries.len());
    for item in merged_entries {
        entry_environments.insert(PrRef::of(&item.entry).key(), item.environment_id);
        entries.push(item.entry);
    }
    let mut viewers: HashMap<String, String> = HashMap::new();
    let mut providers: HashMap<String, ProviderSummary> = HashMap::new();
    let mut errors = Vec::new();
    let mut truncated = false;
    let mut next_cursors_by_env: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut truncated_envs: HashSet<String> = HashSet::new();
    for (env_id, answer) in answers {
        for (host, login) in &answer.viewers {
            viewers.entry(host.clone()).or_insert_with(|| login.clone());
        }
        for provider in &answer.providers {
            providers
                .entry(provider.host.clone())
                .and_modify(|held| {
                    held.project_count += provider.project_count;
                    held.searches_on_host = held.searches_on_host && provider.searches_on_host;
                    held.configured = held.configured || provider.configured;
                })
                .or_insert_with(|| provider.clone());
        }
        errors.extend(answer.errors.iter().cloned());
        if answer.truncated {
            truncated = true;
            truncated_envs.insert(env_id.clone());
        }
        if !answer.next_cursors.is_empty() {
            next_cursors_by_env.insert(env_id.clone(), answer.next_cursors.clone());
        }
    }
    MergedFleetList {
        result: ListResult {
            entries,
            viewers,
            providers: providers.into_values().collect(),
            errors,
            truncated,
            next_cursors: HashMap::new(),
        },
        entry_environments,
        next_cursors_by_env,
        truncated_envs,
    }
}

pub(super) struct SavedViewEditor {
    pub editing_id: Option<String>,
    pub input: Entity<InputState>,
    _subscription: Subscription,
}

pub struct PullRequestsPage {
    pub(super) state: Entity<AppState>,
    pub(super) prefs: ListPreferences,
    pub(super) search: Entity<InputState>,
    pub(super) typed: String,
    pub(super) sent: String,
    pub(super) saved_views: Vec<PrSavedView>,
    pub(super) selected_view_id: Option<String>,
    pub(super) saved_view_editor: Option<SavedViewEditor>,
    next_view_seq: u64,
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
    /// Connected fleet environments whose latest PR list read failed, mapped to retry deadline ms (`pingdotgg/t3code#34088`).
    unreachable_environments: HashMap<String, i64>,
    /// Owning fleet environment id for each merged list entry, keyed by `PrRef::key()`.
    entry_environments: HashMap<String, String>,
    /// Per-environment pagination cursors from the latest fleet list read (`PullRequestsPage.tsx:342`).
    next_cursors_by_env: HashMap<String, HashMap<String, String>>,
    /// Fleet environments whose latest page reported `truncated == true`.
    truncated_envs: HashSet<String>,
    /// Currently connected enabled fleet environment ids (for reconnect detection in `sync`).
    connected_fleet_envs: HashSet<String>,
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
    refreshes: Vec<Refreshes>,
    /// The refresh epoch each environment's held reads answer to.
    epochs: HashMap<String, u64>,
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
        let saved_views = decode_saved_views(store::load_ui_state(PR_SAVED_VIEWS_KEY));
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
            saved_views,
            selected_view_id: None,
            saved_view_editor: None,
            next_view_seq: 0,
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
            unreachable_environments: HashMap::new(),
            entry_environments: HashMap::new(),
            next_cursors_by_env: HashMap::new(),
            truncated_envs: HashSet::new(),
            connected_fleet_envs: HashSet::new(),
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
            refreshes: Vec::new(),
            epochs: HashMap::new(),
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

    /// Owning fleet environment id for a listed pull request, if tracked.
    pub(super) fn entry_environment(&self, reference: &PrRef) -> Option<&str> {
        self.entry_environments.get(&reference.key()).map(String::as_str)
    }

    /// Reads when the page is shown and at least one environment is connected (active or fleet),
    /// again on reopening and on reconnecting, and follows refresh signals while open.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let active_connected = state.client_handle().is_some()
            && matches!(state.phase, crate::app_state::Phase::Online(ConnectionStatus::Connected));
        let live_fleet_envs: HashSet<String> = state
            .fleet
            .environments
            .iter()
            .filter(|env| {
                env.status == ConnectionStatus::Connected
                    && !state
                        .environments
                        .environments
                        .iter()
                        .any(|saved| saved.id == env.summary.owner.saved_id && saved.disabled)
            })
            .map(|env| env.summary.owner.saved_id.clone())
            .collect();
        let connected = if !state.fleet.environments.is_empty() {
            !live_fleet_envs.is_empty()
        } else {
            active_connected
        };
        let open = state.pull_requests_open;
        let fleet_changed = live_fleet_envs != self.connected_fleet_envs;
        // Any environment that just transitioned to Connected clears any prior transient failure mark.
        for env_id in &live_fleet_envs {
            if !self.connected_fleet_envs.contains(env_id) {
                self.unreachable_environments.remove(env_id);
            }
        }
        self.connected_fleet_envs = live_fleet_envs;
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
            self.refreshes.clear();
            self.epochs.clear();
            self.cancel_intent();
            self.preview.clear();
        } else if self.refreshes.is_empty() || fleet_changed {
            self.follow_refreshes(cx);
        }
        if open && self.data.result.is_none() {
            self.hydrate_from_cache(cx);
        }
        if open && connected && (returned || fleet_changed || self.data.key.is_none()) {
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
        let env_id = self.entry_environment(&reference);
        if self.selected.as_ref() == Some(&reference) || has_held_detail_in_env(self.state.read(cx), env_id, &reference, cx) {
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
        let env_id = self.entry_environment(&reference).map(str::to_owned);
        let app = self.state.read(cx);
        let owner = owner_for_pr(app, env_id.as_deref(), &reference.project_id);
        let detail_task = read_for_owner(app, owner.as_ref(), method::DETAIL, payload, decode::<Detail>);
        let activity_task = read_for_owner(app, owner.as_ref(), method::ACTIVITY, reference.payload(), decode::<Activity>);
        self._intent_fetch = Some(cx.spawn(async move |this, cx| {
            let (detail, activity) = (settle(detail_task).await.ok(), settle(activity_task).await.ok());
            let _ = this.update(cx, |page, cx| {
                if page.is_listed(&reference) {
                    let state = page.state.clone();
                    prefetch_into_held_in_env(&state, env_id.as_deref(), &reference, detail.as_ref(), activity.as_ref(), cx);
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
        let env_id = self.entry_environment(&reference);
        let app = self.state.read(cx);
        let owner = owner_for_pr(app, env_id, &reference.project_id);
        let task = read_for_owner(app, owner.as_ref(), method::STACK, reference.payload(), decode::<Option<Stack>>);
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
        self.refreshes.clear();
        let state = self.state.read(cx);
        if !state.fleet.environments.is_empty() {
            let owners: Vec<(String, EnvironmentOwner)> = state
                .fleet
                .environments
                .iter()
                .filter(|env| {
                    env.status == ConnectionStatus::Connected
                        && !state
                            .environments
                            .environments
                            .iter()
                            .any(|saved| saved.id == env.summary.owner.saved_id && saved.disabled)
                })
                .map(|env| (env.summary.owner.saved_id.clone(), env.summary.owner.clone()))
                .collect();
            for (env_id, owner) in owners {
                let dest_fut = state.connections.destination(&owner, ORCHESTRATION_READ_SCOPE);
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
                let net = state.runtime().spawn(async move {
                    let Ok(dest) = dest_fut.await else { return };
                    let client = dest.client;
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
                        if this.update(cx, |this, cx| this.refreshed(&env_id, epoch, cx)).is_err() {
                            return;
                        }
                    }
                });
                self.refreshes.push(Refreshes { net, _ui: ui });
            }
            return;
        }
        let Some(client) = state.client_handle() else { return };
        let env_id = self.environment_key(cx).unwrap_or_else(|| "local".into());
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
                if this.update(cx, |this, cx| this.refreshed(&env_id, epoch, cx)).is_err() {
                    return;
                }
            }
        });
        self.refreshes.push(Refreshes { net, _ui: ui });
    }

    /// A new epoch means the host's reads moved on (a turn ended, or any client wrote). The first
    /// one heard for an environment only marks where the reads made on opening stand.
    fn refreshed(&mut self, env_id: &str, epoch: u64, cx: &mut Context<Self>) {
        let moved = self.epochs.insert(env_id.to_owned(), epoch).is_some_and(|held| held != epoch);
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

    pub(super) fn retry_fetch(&mut self, cx: &mut Context<Self>) {
        self.unreachable_environments.clear();
        self.refetch(cx);
    }

    /// Computes the per-environment `pullRequests.list` requests for fleet mode: scopes each
    /// environment to its assigned `projectIds`, passes only that environment's own pagination
    /// cursors (`PullRequestsPage.tsx:366-388`), and skips environments with no cursor/regrowth
    /// during cursor continuation.
    fn plan_fleet_requests(
        &self,
        inventory: &FleetInventory,
        cursors: Option<&FetchCursors>,
    ) -> Vec<(String, Option<EnvironmentOwner>, serde_json::Value)> {
        let slices = pull_request_project_queries_with_unreachable(
            &inventory.projects,
            &inventory.env_ids,
            Some(&self.host_availability),
            Some(&inventory.unreachable),
        );
        let mut planned = Vec::new();
        for slice in slices {
            let Some(target) = inventory.targets.get(&slice.environment_id).filter(|t| t.connected) else {
                continue;
            };
            if let Some(scoped) = self.prefs.project_id.as_deref() {
                let owns_scoped = match &slice.project_ids {
                    Some(ids) => ids.iter().any(|id| id == scoped),
                    None => inventory
                        .projects
                        .iter()
                        .any(|p| p.environment_id == slice.environment_id && p.id == scoped),
                };
                if !owns_scoped {
                    continue;
                }
            }
            let env_cursors = match cursors {
                None => None,
                Some(FetchCursors::Single(map)) => Some(map),
                Some(FetchCursors::PerEnv { cursors, regrown }) => {
                    let next = cursors.get(&slice.environment_id).filter(|map| !map.is_empty());
                    if next.is_none() && !regrown.contains(&slice.environment_id) {
                        continue;
                    }
                    next
                }
            };
            let mut input = list_input(&self.prefs, &self.sent, self.data.page_size, env_cursors);
            if self.prefs.project_id.is_none()
                && let Some(project_ids) = &slice.project_ids
                && let Some(obj) = input.as_object_mut()
            {
                obj.insert("projectIds".into(), serde_json::json!(project_ids));
            }
            planned.push((slice.environment_id, target.owner.clone(), input));
        }
        planned
    }

    #[cfg(test)]
    pub(crate) fn planned_fleet_requests(
        &self,
        per_env_cursors: Option<HashMap<String, HashMap<String, String>>>,
        cx: &App,
    ) -> Vec<(String, serde_json::Value)> {
        let inventory = self.fleet_inventory(cx);
        let fetch_cursors = per_env_cursors.map(|cursors| FetchCursors::PerEnv {
            regrown: self
                .truncated_envs
                .iter()
                .filter(|env_id| !cursors.contains_key(*env_id))
                .cloned()
                .collect(),
            cursors,
        });
        self.plan_fleet_requests(&inventory, fetch_cursors.as_ref())
            .into_iter()
            .map(|(env_id, _, input)| (env_id, input))
            .collect()
    }

    /// `refreshing` re-reads the first page under rows that later cursors added. Queries across
    /// all reachable fleet environments using `pull_request_project_queries_with_unreachable`
    /// (`pingdotgg/t3code#34088`) and deduplicates cross-server results by URL (`pingdotgg/t3code#37704`).
    fn fetch(&mut self, cursors: Option<FetchCursors>, refreshing: bool, cx: &mut Context<Self>) {
        let appending = cursors.is_some();
        let key = self.scope_key();
        let fleet_mode = !self.state.read(cx).fleet.environments.is_empty();
        if fleet_mode {
            let inventory = self.fleet_inventory(cx);
            let planned = self.plan_fleet_requests(&inventory, cursors.as_ref());
            if planned.is_empty() {
                if inventory.projects.is_empty() {
                    self.apply_list(key, appending, refreshing, Ok(ListResult::default()), cx);
                } else if appending {
                    self.data.pending = false;
                    self.data.loading_more = false;
                    cx.notify();
                } else {
                    let message = if self.prefs.project_id.is_some() {
                        "The environment for this project is currently offline.".to_owned()
                    } else {
                        super::NOT_CONNECTED.to_owned()
                    };
                    self.apply_list(key, appending, refreshing, Err(message), cx);
                }
                return;
            }
            let state = self.state.read(cx);
            let dispatched: Vec<_> = planned
                .into_iter()
                .map(|(env_id, owner, input)| {
                    let task = read_for_owner(state, owner.as_ref(), method::LIST, input, decode::<ListResult>);
                    (env_id, task)
                })
                .collect();
            self.data.pending = true;
            self.data.loading_more = appending;
            self._list = Some(cx.spawn(async move |this, cx| {
                let mut settled = Vec::with_capacity(dispatched.len());
                for (env_id, task) in dispatched {
                    settled.push((env_id, settle(task).await));
                }
                let _ = this.update(cx, |this, cx| {
                    this.apply_fleet_lists(key, appending, refreshing, settled, cx);
                });
            }));
            cx.notify();
            return;
        }
        let single_cursors = match &cursors {
            Some(FetchCursors::Single(map)) => Some(map),
            _ => None,
        };
        let state = self.state.read(cx);
        let input = list_input(&self.prefs, &self.sent, self.data.page_size, single_cursors);
        let task = read(state, method::LIST, input, decode::<ListResult>);
        self.data.pending = true;
        self.data.loading_more = appending;
        self._list = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| this.apply_list(key, appending, refreshing, result, cx));
        }));
        cx.notify();
    }

    /// Merges multi-environment `pullRequests.list` responses, deduplicating PRs across servers
    /// by URL (`pingdotgg/t3code#37704`), preserving owning `environment_id` + per-environment
    /// cursors, and tracking unreachable environments (`pingdotgg/t3code#34088`).
    pub(crate) fn apply_fleet_lists(
        &mut self,
        key: String,
        appending: bool,
        refreshing: bool,
        answers: Vec<(String, Result<ListResult, String>)>,
        cx: &mut Context<Self>,
    ) {
        if key != self.scope_key() {
            return;
        }
        let now = now_ms();
        let mut ok_answers: Vec<(String, ListResult)> = Vec::new();
        let mut first_error: Option<String> = None;
        for (env_id, outcome) in answers {
            match outcome {
                Ok(result) => {
                    self.unreachable_environments.remove(&env_id);
                    self.host_availability = remember_hosts(&self.host_availability, &[(env_id.as_str(), &result.providers, now)]);
                    ok_answers.push((env_id, result));
                }
                Err(error) => {
                    self.unreachable_environments.insert(env_id, now + UNREACHABLE_TTL_MS);
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if ok_answers.is_empty() {
            self.apply_list(key, appending, refreshing, Err(first_error.unwrap_or_else(|| super::NOT_CONNECTED.to_owned())), cx);
            return;
        }
        let mut merged = merge_fleet_list_results(&ok_answers);
        let keeping_later_pages = refreshing
            && self
                .data
                .result
                .as_ref()
                .is_some_and(|previous| previous.entries.len() > self.data.first_page && self.data.key.as_deref() == Some(key.as_str()));
        if !appending && !keeping_later_pages {
            self.entry_environments = merged.entry_environments;
            self.next_cursors_by_env = merged.next_cursors_by_env;
            self.truncated_envs = merged.truncated_envs;
        } else if appending {
            for (pr_key, env_id) in merged.entry_environments {
                self.entry_environments.entry(pr_key).or_insert(env_id);
            }
            for (env_id, _) in &ok_answers {
                if let Some(next) = merged.next_cursors_by_env.remove(env_id) {
                    self.next_cursors_by_env.insert(env_id.clone(), next);
                } else {
                    self.next_cursors_by_env.remove(env_id);
                }
                if merged.truncated_envs.contains(env_id) {
                    self.truncated_envs.insert(env_id.clone());
                } else {
                    self.truncated_envs.remove(env_id);
                }
            }
            merged.result.truncated = !self.next_cursors_by_env.is_empty() || !self.truncated_envs.is_empty();
        } else {
            for (pr_key, env_id) in merged.entry_environments {
                self.entry_environments.insert(pr_key, env_id);
            }
        }
        self.apply_list(key, appending, refreshing, Ok(merged.result), cx);
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

    /// `pullRequests.listStats` for rows the listing left unmeasured, grouped by each row's owning
    /// fleet environment so secondary-server PR stats are read from their owning host.
    fn fetch_stats(&mut self, cx: &mut Context<Self>) {
        let Some(result) = &self.data.result else { return };
        let restat = std::mem::take(&mut self.restat);
        let mut by_env: HashMap<Option<String>, Vec<&t3_core::pull_requests::wire::ListEntry>> = HashMap::new();
        let mut total_wanted = 0usize;
        for entry in &result.entries {
            let key = stat_key(&entry.project_id, entry.number);
            if (needs_stats(entry) || (restat && self.stats.contains_key(&key))) && !self.requested.contains(&key) {
                let env_id = self.entry_environments.get(&PrRef::of(entry).key()).cloned();
                by_env.entry(env_id).or_default().push(entry);
                total_wanted += 1;
            }
        }
        if total_wanted == 0 {
            return;
        }
        if self.stats.len() > MAX_HELD_STATS {
            self.stats.clear();
            self.requested.clear();
        }
        let mut dispatches: Vec<(Option<String>, Option<EnvironmentOwner>, serde_json::Value)> = Vec::new();
        let state = self.state.read(cx);
        for (env_id, entries) in by_env {
            let Some(first) = entries.first() else { continue };
            let owner = owner_for_pr(state, env_id.as_deref(), &first.project_id);
            let batches = stats_inputs(entries.iter().copied());
            for entry in &entries {
                self.requested.insert(stat_key(&entry.project_id, entry.number));
            }
            for input in batches {
                dispatches.push((env_id.clone(), owner.clone(), input));
            }
        }
        for (env_id, owner, input) in dispatches {
            self.fetch_stats_batch(env_id, owner, input, cx);
        }
    }

    fn fetch_stats_batch(
        &mut self,
        env_id: Option<String>,
        owner: Option<EnvironmentOwner>,
        input: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let task = read_for_owner(self.state.read(cx), owner.as_ref(), method::LIST_STATS, input, decode::<ListStats>);
        cx.spawn(async move |this, cx| {
            let Ok(stats) = settle(task).await else { return };
            let _ = this.update(cx, |this, cx| {
                if let Some(env) = env_id.or_else(|| this.environment_key(cx)) {
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

    /// The host's labels and authors for the Filters menu, fanned out across connected fleet
    /// environments (or the single environment owning `prefs.project_id`).
    fn fetch_options(&mut self, cx: &mut Context<Self>) {
        let mut scope = serde_json::Map::new();
        if let Some(project) = &self.prefs.project_id {
            scope.insert("projectId".into(), project.clone().into());
        }
        let input = |kind: &str| {
            let mut input = scope.clone();
            input.insert("kind".into(), kind.into());
            serde_json::Value::Object(input)
        };
        let fleet_mode = !self.state.read(cx).fleet.environments.is_empty();
        if fleet_mode {
            let inventory = self.fleet_inventory(cx);
            let state = self.state.read(cx);
            let mut tasks = Vec::new();
            for env_id in &inventory.env_ids {
                let Some(target) = inventory.targets.get(env_id).filter(|t| t.connected) else {
                    continue;
                };
                if let Some(scoped) = self.prefs.project_id.as_deref()
                    && !inventory.projects.iter().any(|p| p.environment_id == *env_id && p.id == scoped)
                {
                    continue;
                }
                let labels = read_for_owner(state, target.owner.as_ref(), method::FILTER_OPTIONS, input("labels"), decode::<FilterOptions>);
                let authors = read_for_owner(state, target.owner.as_ref(), method::FILTER_OPTIONS, input("authors"), decode::<FilterOptions>);
                tasks.push((labels, authors));
            }
            self._options = Some(cx.spawn(async move |this, cx| {
                let mut labels = Vec::new();
                let mut authors = Vec::new();
                for (l_task, a_task) in tasks {
                    for label in settle(l_task).await.unwrap_or_default().labels {
                        if !labels.iter().any(|known: &t3_core::pull_requests::wire::Label| known.name.eq_ignore_ascii_case(&label.name)) {
                            labels.push(label);
                        }
                    }
                    for actor in settle(a_task).await.unwrap_or_default().authors {
                        if !authors.iter().any(|known: &t3_core::pull_requests::wire::Actor| known.login.eq_ignore_ascii_case(&actor.login)) {
                            authors.push(actor);
                        }
                    }
                }
                let _ = this.update(cx, |this, _| this.options = FilterOptions { labels, authors, truncated: false });
            }));
            return;
        }
        let state = self.state.read(cx);
        let labels = read(state, method::FILTER_OPTIONS, input("labels"), decode::<FilterOptions>);
        let authors = read(state, method::FILTER_OPTIONS, input("authors"), decode::<FilterOptions>);
        self._options = Some(cx.spawn(async move |this, cx| {
            let (labels, authors) = (settle(labels).await.unwrap_or_default(), settle(authors).await.unwrap_or_default());
            let _ = this.update(cx, |this, _| this.options = FilterOptions { labels: labels.labels, authors: authors.authors, truncated: false });
        }));
    }

    pub(super) fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(result) = &self.data.result else { return };
        if !self.next_cursors_by_env.is_empty() {
            let cursors = self.next_cursors_by_env.clone();
            let regrown: HashSet<String> = self
                .truncated_envs
                .iter()
                .filter(|env_id| !cursors.contains_key(*env_id))
                .cloned()
                .collect();
            if !regrown.is_empty() && self.data.page_size < MAX_PAGE_SIZE {
                self.data.page_size = (self.data.page_size + PAGE_SIZE).min(MAX_PAGE_SIZE);
            }
            self.fetch(Some(FetchCursors::PerEnv { cursors, regrown }), false, cx);
        } else if !result.next_cursors.is_empty() {
            let cursors = result.next_cursors.clone();
            self.fetch(Some(FetchCursors::Single(cursors)), false, cx);
        } else if self.data.page_size < MAX_PAGE_SIZE {
            self.data.page_size = (self.data.page_size + PAGE_SIZE).min(MAX_PAGE_SIZE);
            self.data.loading_more = true;
            self.fetch(None, false, cx);
        }
    }

    pub(super) fn can_load_more(&self) -> bool {
        self.data.result.as_ref().is_some_and(|result| {
            result.truncated
                && (!self.next_cursors_by_env.is_empty()
                    || !result.next_cursors.is_empty()
                    || self.data.page_size < MAX_PAGE_SIZE)
        })
    }

    /// "Check again": every connected host is asked afresh (`pullRequests.invalidate`), caches and
    /// host availability reset, then the list re-read.
    pub(super) fn refresh_from_host(&mut self, cx: &mut Context<Self>) {
        let fleet_mode = !self.state.read(cx).fleet.environments.is_empty();
        let invalidate_tasks: Vec<_> = if fleet_mode {
            let inventory = self.fleet_inventory(cx);
            let state = self.state.read(cx);
            inventory
                .targets
                .values()
                .filter(|target| target.connected)
                .map(|target| {
                    read_for_owner(
                        state,
                        target.owner.as_ref(),
                        method::INVALIDATE,
                        t3_core::pull_requests::invalidate_input(None),
                        |_| Ok(()),
                    )
                })
                .collect()
        } else {
            vec![read(
                self.state.read(cx),
                method::INVALIDATE,
                t3_core::pull_requests::invalidate_input(None),
                |_| Ok(()),
            )]
        };
        self.data.pending = true;
        self.stats.clear();
        self.requested.clear();
        self.row_stacks.clear();
        self.unreachable_environments.clear();
        self.next_cursors_by_env.clear();
        self.truncated_envs.clear();
        self.host_availability = reset_hosts(&self.host_availability);
        if let Some(env) = self.environment_key(cx) {
            self.list_cache.clear_environment(&env);
            self.stats_cache.clear_environment(&env);
            store::save_ui_state(LIST_CACHE_KEY, &self.list_cache);
            store::save_ui_state(STATS_CACHE_KEY, &self.stats_cache);
        }
        cx.notify();
        self._list = Some(cx.spawn(async move |this, cx| {
            for task in invalidate_tasks {
                let _ = settle(task).await;
            }
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
        let env_id = self.entry_environment(&reference).map(str::to_owned);
        let state = self.state.clone();
        let page = cx.entity().downgrade();
        self.detail = Some(cx.new(|cx| DetailView::new_in_environment(state, page, env_id, reference.clone(), entry, cx)));
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

    // --- Saved PR filter views (`pingdotgg/t3code#29387`) ---

    fn save_views(&self) {
        store::save_ui_state(PR_SAVED_VIEWS_KEY, &self.saved_views);
    }

    pub(super) fn current_saved_preferences(&self) -> ListPreferences {
        let mut current = self.prefs.clone();
        let trimmed = self.typed.trim();
        current.q = (!trimmed.is_empty()).then(|| trimmed.to_owned());
        current.normalized()
    }

    pub(super) fn apply_saved_view(
        &mut self,
        view_id: String,
        preferences: ListPreferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_view_id = Some(view_id);
        let next = apply_saved_view_patch(&self.prefs, &preferences);
        let q = next.q.clone().unwrap_or_default();
        self.typed = q.clone();
        self.sent = q.trim().to_owned();
        self._debounce = None;
        self.search.update(cx, |input, cx| input.set_value(q, window, cx));
        self.update_prefs(move |prefs| *prefs = next, cx);
    }

    pub(super) fn open_saved_view_editor(
        &mut self,
        editing_id: Option<String>,
        initial_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(PR_SAVED_VIEW_PLACEHOLDER)
                .default_value(initial_name)
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => this.submit_saved_view_editor(cx),
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.saved_view_editor = Some(SavedViewEditor {
            editing_id,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(super) fn close_saved_view_editor(&mut self, cx: &mut Context<Self>) {
        if self.saved_view_editor.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn submit_saved_view_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.saved_view_editor.as_ref() else {
            return;
        };
        let name = editor.input.read(cx).value().to_string();
        let editing_id = editor.editing_id.clone();
        match editing_id {
            Some(id) => match rename_saved_view(&mut self.saved_views, &id, &name) {
                Ok(true) => {
                    self.save_views();
                    self.selected_view_id = Some(id);
                    self.saved_view_editor = None;
                    cx.notify();
                }
                Ok(false) => {
                    if let Some(ed) = self.saved_view_editor.as_mut() {
                        ed.editing_id = None;
                    }
                    let toast = Toast::new(ToastKind::Error, crate::i18n::t(cx, "This saved view was removed"))
                        .description(crate::i18n::t(cx, "Your name has been kept. Save it as a new view or cancel."));
                    self.state.update(cx, |state, cx| state.show_toast(toast, cx));
                    cx.notify();
                }
                Err(_) => {}
            },
            None => {
                let id = format!("pr-view-{}-{}", now_ms(), self.next_view_seq);
                self.next_view_seq += 1;
                let normalized = self.current_saved_preferences();
                if let Ok(saved_id) = create_saved_view(&mut self.saved_views, id, &name, normalized) {
                    self.save_views();
                    self.selected_view_id = Some(saved_id);
                    self.saved_view_editor = None;
                    cx.notify();
                }
            }
        }
    }

    pub(super) fn update_active_saved_view(&mut self, view_id: &str, cx: &mut Context<Self>) {
        let normalized = self.current_saved_preferences();
        if update_saved_view_preferences(&mut self.saved_views, view_id, normalized) {
            self.save_views();
            cx.notify();
        }
    }

    pub(super) fn delete_active_saved_view(&mut self, view_id: &str, cx: &mut Context<Self>) {
        if delete_saved_view(&mut self.saved_views, view_id) {
            self.save_views();
            if self.selected_view_id.as_deref() == Some(view_id) {
                self.selected_view_id = None;
            }
            cx.notify();
        }
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
        let state = self.state.read(cx);
        let fleet_default = state
            .fleet
            .environments
            .iter()
            .find(|env| env.summary.projects.iter().any(|scoped| scoped.project.id == project_id))
            .and_then(|env| env.config.config.as_ref())
            .and_then(|config| project_default_merge_method(&config.settings, project_id));
        let project_default = fleet_default.or_else(|| {
            state
                .server_config
                .config
                .as_ref()
                .and_then(|config| project_default_merge_method(&config.settings, project_id))
        });
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
        let env_id = self.entry_environment(&reference).map(str::to_owned);
        let task = if action == Action::Merge {
            let picks = self.merge_picks(&entry.project_id, None, cx);
            let app = self.state.read(cx);
            let owner = owner_for_pr(app, env_id.as_deref(), &entry.project_id);
            mutations::merge(app, owner.as_ref(), reference.clone(), picks, cx)
        } else {
            let app = self.state.read(cx);
            let owner = owner_for_pr(app, env_id.as_deref(), &entry.project_id);
            mutations::run_action(app, owner.as_ref(), action_input(&reference, action, None), cx)
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

    fn fleet_inventory(&self, cx: &App) -> FleetInventory {
        let state = self.state.read(cx);
        let active_id = self.environment_key(cx);
        let now = now_ms();
        let mut unreachable: HashSet<String> = self
            .unreachable_environments
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(env_id, _)| env_id.clone())
            .collect();
        if !state.fleet.environments.is_empty() {
            let mut env_ids: Vec<String> = Vec::new();
            let mut envs: Vec<EnvironmentInfo> = Vec::new();
            let mut targets: HashMap<String, PrFleetTarget> = HashMap::new();
            let mut projects: Vec<AssignableProject> = Vec::new();
            let mut ordered: Vec<_> = state
                .fleet
                .environments
                .iter()
                .filter(|env| {
                    !state
                        .environments
                        .environments
                        .iter()
                        .any(|saved| saved.id == env.summary.owner.saved_id && saved.disabled)
                })
                .collect();
            // Active environment sorts first so it is preferred when sharing repositories.
            ordered.sort_by_key(|env| {
                let is_active = active_id.as_deref().is_some_and(|id| {
                    env.summary.owner.saved_id == id || env.summary.owner.canonical_id.as_deref() == Some(id)
                });
                !is_active
            });
            for env in ordered {
                let env_id = env.summary.owner.saved_id.clone();
                let label = env
                    .config
                    .config
                    .as_ref()
                    .map(|c| c.environment.label.clone())
                    .filter(|l| !l.trim().is_empty())
                    .or_else(|| {
                        state
                            .environments
                            .environments
                            .iter()
                            .find(|saved| saved.id == env_id)
                            .map(|saved| saved.label.clone())
                    })
                    .unwrap_or_else(|| env_id.clone());
                let connected = env.status == ConnectionStatus::Connected;
                if !connected {
                    unreachable.insert(env_id.clone());
                }
                env_ids.push(env_id.clone());
                envs.push(EnvironmentInfo { environment_id: env_id.clone(), label: label.clone() });
                targets.insert(
                    env_id.clone(),
                    PrFleetTarget {
                        owner: Some(env.summary.owner.clone()),
                        connected,
                    },
                );
                for scoped in &env.summary.projects {
                    if scoped.project.archived_at.is_some() {
                        continue;
                    }
                    let repo_key = scoped
                        .project
                        .repository_identity
                        .as_ref()
                        .and_then(|id| id.get("canonicalKey").and_then(serde_json::Value::as_str))
                        .map(str::to_owned);
                    let host = repo_key.as_deref().and_then(|key| key.split('/').next()).map(str::to_owned);
                    projects.push(AssignableProject {
                        id: scoped.project.id.clone(),
                        environment_id: env_id.clone(),
                        title: scoped.project.title.clone(),
                        workspace_root: scoped.project.workspace_root.clone(),
                        repository_key: repo_key,
                        host,
                    });
                }
            }
            return FleetInventory { projects, env_ids, envs, targets, unreachable };
        }

        let env_id = active_id.unwrap_or_else(|| "local".into());
        let env_label = state
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.label.clone())
            .or_else(|| state.active_environment().map(|e| e.label.clone()))
            .unwrap_or_else(|| "Local".to_owned());
        let connected = state.client_handle().is_some()
            && matches!(state.phase, crate::app_state::Phase::Online(ConnectionStatus::Connected));
        let env_ids = vec![env_id.clone()];
        let envs = vec![EnvironmentInfo { environment_id: env_id.clone(), label: env_label }];
        let mut targets = HashMap::new();
        targets.insert(env_id.clone(), PrFleetTarget { owner: None, connected });
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
        FleetInventory { projects, env_ids, envs, targets, unreachable }
    }

    /// Computes the project-assignment rows and `"All projects"` total across all fleet environments,
    /// yielding shared repositories away from unreachable or unconfigured environments
    /// (`pingdotgg/t3code#34088`).
    pub(crate) fn assignment_rows(&self, cx: &App) -> (Vec<AssignedProjectColumnRow>, usize) {
        let FleetInventory { projects, env_ids, envs, unreachable, .. } = self.fleet_inventory(cx);
        let rows = project_assignment_rows_with_unreachable(
            &projects,
            &env_ids,
            &envs,
            Some(&self.host_availability),
            &self.project_counts,
            &self.project_unavailable,
            Some(&unreachable),
        );
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
            let retry = cx.listener(|this, _, _, cx| this.retry_fetch(cx));
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
        let saved_view_dialog = self
            .saved_view_editor
            .as_ref()
            .map(|editor| super::toolbar::render_saved_view_dialog(self, editor, cx));
        h_flex()
            .id("pull-requests-page")
            .relative()
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
                                        let retry = cx.listener(|this, _, _, cx| this.retry_fetch(cx));
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
            .children(saved_view_dialog)
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
            assert!(!has_held_detail_in_env(state.read(cx), None, &reference, cx));
            prefetch_into_held_in_env(&state, None, &reference, Some(&detail), None, cx);
            assert!(has_held_detail_in_env(state.read(cx), None, &reference, cx));

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

    #[gpui::test]
    fn saved_pr_views_create_apply_modify_rename_delete_preserving_merge_method_and_selection(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });
        let (page, vcx) = cx.add_window_view(|window, cx| PullRequestsPage::new(state.clone(), window, cx));
        let selected_ref = PrRef {
            project_id: "p1".into(),
            host: Some("github.com".into()),
            repository: "acme/app".into(),
            number: 99,
        };
        page.update_in(vcx, |page, window, cx| {
            page.saved_views.clear();
            page.prefs.merge_method = Some(MergeMethod::Squash);
            page.prefs.state = t3_core::pull_requests::list::ListState::Closed;
            page.typed = "label:bug".into();
            page.selected = Some(selected_ref.clone());

            // Create a saved view from current preferences.
            page.open_saved_view_editor(None, "Closed bugs".into(), window, cx);
            page.submit_saved_view_editor(cx);
            assert_eq!(page.saved_views.len(), 1);
            assert_eq!(page.saved_views[0].name, "Closed bugs");
            assert_eq!(page.saved_views[0].preferences.merge_method, None);
            let id = page.saved_views[0].id.clone();

            // Mutate preferences, then re-apply saved view: merge_method and selected PR survive.
            page.prefs.state = t3_core::pull_requests::list::ListState::Open;
            page.prefs.merge_method = Some(MergeMethod::Rebase);
            let saved_prefs = page.saved_views[0].preferences.clone();
            page.apply_saved_view(id.clone(), saved_prefs, window, cx);
            assert_eq!(page.prefs.state, t3_core::pull_requests::list::ListState::Closed);
            assert_eq!(page.prefs.merge_method, Some(MergeMethod::Rebase));
            assert_eq!(page.selected.as_ref(), Some(&selected_ref));
            assert_eq!(page.typed, "label:bug");

            // Update, rename, and delete.
            page.typed = "label:urgent".into();
            page.update_active_saved_view(&id, cx);
            assert_eq!(page.saved_views[0].preferences.q.as_deref(), Some("label:urgent"));

            page.open_saved_view_editor(Some(id.clone()), "Urgent closed".into(), window, cx);
            page.submit_saved_view_editor(cx);
            assert_eq!(page.saved_views[0].name, "Urgent closed");

            page.delete_active_saved_view(&id, cx);
            assert!(page.saved_views.is_empty());
            assert!(page.selected_view_id.is_none());
        });
    }

    #[gpui::test]
    fn fleet_multi_environment_assignment_failover_and_cross_server_url_dedupe(cx: &mut gpui::TestAppContext) {
        use std::sync::Arc;

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            state.update(cx, |state, _| {
                let make_env = |saved: &str, canonical: &str, label: &str, status: ConnectionStatus, projects: serde_json::Value| {
                    let mut config = t3_core::server_config::ServerConfigState::default();
                    config.apply(
                        serde_json::from_value(json!({
                            "type": "snapshot",
                            "config": {
                                "environment": { "environmentId": canonical, "label": label },
                                "cwd": "/tmp",
                                "providers": []
                            }
                        }))
                        .unwrap(),
                    );
                    let mut shell = t3_core::shell::ShellState::default();
                    shell.synchronized = true;
                    shell.snapshot = Some(
                        serde_json::from_value(json!({
                            "snapshotSequence": 1,
                            "projects": projects,
                            "threads": [],
                            "updatedAt": "2026-01-02T00:00:00Z"
                        }))
                        .unwrap(),
                    );
                    let owner = EnvironmentOwner {
                        saved_id: saved.into(),
                        canonical_id: Some(canonical.into()),
                        generation: 1,
                        connection_generation: 1,
                        shell_generation: 1,
                    };
                    Arc::new(t3_client::environments::EnvironmentView {
                        summary: Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(owner, &shell, &config)),
                        status,
                        config,
                        grants: None,
                        settings_revision: 0,
                        keybindings_revision: 0,
                    })
                };

                let project_json = |id: &str, title: &str, repo: &str| {
                    json!({
                        "id": id,
                        "title": title,
                        "workspaceRoot": format!("/tmp/{id}"),
                        "repositoryIdentity": {
                            "canonicalKey": format!("github.com/{repo}"),
                            "provider": "github",
                            "displayName": repo
                        },
                        "defaultModelSelection": null,
                        "scripts": [],
                        "createdAt": "2026-01-01T00:00:00Z",
                        "updatedAt": "2026-01-01T00:00:00Z"
                    })
                };

                state.environments.active = Some("saved-local".into());
                state.fleet = Arc::new(t3_client::environments::FleetView {
                    revision: 1,
                    environments: vec![
                        make_env(
                            "saved-local",
                            "canon-local",
                            "MacBook",
                            ConnectionStatus::Reconnecting { reason: "offline".into(), attempt: 1 },
                            json!([
                                project_json("p-shared-local", "Shared", "acme/shared"),
                                project_json("p-only-local", "OnlyLocal", "acme/only-local")
                            ]),
                        ),
                        make_env(
                            "saved-remote",
                            "canon-remote",
                            "Linux Box",
                            ConnectionStatus::Connected,
                            json!([project_json("p-shared-remote", "Shared", "acme/shared")]),
                        ),
                    ],
                });
            });
            state
        });

        let (page, cx) = cx.add_window_view(|window, cx| PullRequestsPage::new(state.clone(), window, cx));
        let make_pr = |project_id: &str, number: u64, title: &str, url: &str| -> serde_json::Value {
            json!({
                "provider": "github", "host": "github.com", "projectId": project_id, "projectTitle": "Shared",
                "repository": "acme/shared", "number": number, "title": title,
                "url": url, "author": { "login": "alice" },
                "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 3, "deletions": 1,
                "viewerReviewRequested": false, "headBranch": "feat", "baseBranch": "main",
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z"
            })
        };

        page.update(cx, |page, cx| {
            // 0. Fleet-aware `sync`: even though active `saved-local` is offline (`Reconnecting`),
            // `saved-remote` is `Connected`, so the page treats the fleet as connected.
            assert!(page.connected, "fleet page must stay connected while a secondary fleet env is Connected");

            // 1. Disconnected active env yields shared repo to connected remote env (`#34088`),
            // while keeping its exclusive repository assigned to itself.
            let (rows, _) = page.assignment_rows(cx);
            let shared = rows.iter().find(|r| r.project_title == "Shared").expect("shared row");
            assert_eq!(shared.environment_id, "saved-remote");
            assert_eq!(shared.project_id, "p-shared-remote");
            let local_only = rows.iter().find(|r| r.project_title == "OnlyLocal").expect("local-only row");
            assert_eq!(local_only.environment_id, "saved-local");

            // 2. Cross-server URL deduplication (`#37704`): identical PR URL from two servers
            // collapses to a single row while preserving the preferred server's copy AND tracking
            // each entry's owning `environment_id` and per-environment `next_cursors`.
            let key = page.scope_key();
            let local_list: ListResult = decode(json!({
                "entries": [make_pr("p-shared-local", 42, "From MacBook", "https://github.com/acme/shared/pull/42")],
                "providers": [], "errors": [], "truncated": true,
                "nextCursors": { "github": "cursor-local-1" }
            }))
            .unwrap();
            let remote_list: ListResult = decode(json!({
                "entries": [
                    make_pr("p-shared-remote", 42, "From Linux", "https://github.com/acme/shared/pull/42"),
                    make_pr("p-shared-remote", 43, "Remote Only", "https://github.com/acme/shared/pull/43")
                ],
                "providers": [], "errors": [], "truncated": true,
                "nextCursors": { "github": "cursor-remote-9" }
            }))
            .unwrap();
            page.apply_fleet_lists(
                key.clone(),
                false,
                false,
                vec![("saved-local".into(), Ok(local_list)), ("saved-remote".into(), Ok(remote_list.clone()))],
                cx,
            );
            let entries = &page.data.result.as_ref().unwrap().entries;
            assert_eq!(entries.len(), 2);
            let pr42 = entries.iter().find(|e| e.number == 42).unwrap();
            let pr43 = entries.iter().find(|e| e.number == 43).unwrap();
            assert_eq!(pr42.title, "From MacBook");
            assert_eq!(page.entry_environment(&PrRef::of(pr42)), Some("saved-local"));
            assert_eq!(page.entry_environment(&PrRef::of(pr43)), Some("saved-remote"));

            // Selecting a PR owned by secondary env `saved-remote` binds `DetailView` to `saved-remote`
            // and `owner_for_pr` resolves `saved-remote`'s `EnvironmentOwner`.
            let pr43_ref = PrRef::of(pr43);
            page.select(pr43_ref.clone(), cx);
            let detail_env = page.detail.as_ref().unwrap().read(cx).target_environment().map(str::to_owned);
            assert_eq!(detail_env.as_deref(), Some("saved-remote"));
            let resolved_owner = owner_for_pr(page.state.read(cx), detail_env.as_deref(), &pr43_ref.project_id).unwrap();
            assert_eq!(resolved_owner.saved_id, "saved-remote");

            // Per-environment cursors stay isolated (neither overwrites the other's `"github"` cursor)
            // and `planned_fleet_requests` sends ONLY `saved-remote`'s own cursor to `saved-remote`.
            assert_eq!(
                page.next_cursors_by_env.get("saved-local").and_then(|c| c.get("github")).map(String::as_str),
                Some("cursor-local-1")
            );
            assert_eq!(
                page.next_cursors_by_env.get("saved-remote").and_then(|c| c.get("github")).map(String::as_str),
                Some("cursor-remote-9")
            );
            let planned_next = page.planned_fleet_requests(Some(page.next_cursors_by_env.clone()), cx);
            assert_eq!(planned_next.len(), 1);
            assert_eq!(planned_next[0].0, "saved-remote");
            assert_eq!(planned_next[0].1["cursors"]["github"], json!("cursor-remote-9"));

            // 3. Partial fleet failure marks the failing env unreachable while still painting the
            // healthy env's rows (`#34088`), and `retry_fetch` clears transient unreachable marks.
            page.apply_fleet_lists(
                key,
                false,
                false,
                vec![
                    ("saved-local".into(), Err("environment offline".into())),
                    ("saved-remote".into(), Ok(remote_list)),
                ],
                cx,
            );
            assert!(page.unreachable_environments.contains_key("saved-local"));
            assert!(!page.unreachable_environments.contains_key("saved-remote"));
            assert_eq!(page.data.result.as_ref().unwrap().entries.len(), 2);
            assert!(page.data.error.is_none());

            // 4. Zero-slice fallback: scoping to `p-only-local` (owned only by disconnected `saved-local`)
            // dispatches zero fleet slices and surfaces an offline error instead of querying another server.
            page.prefs.project_id = Some("p-only-local".into());
            assert!(page.planned_fleet_requests(None, cx).is_empty());
            page.retry_fetch(cx);
            assert!(!page.unreachable_environments.contains_key("saved-local"), "retry_fetch clears transient failure marks");
            assert_eq!(
                page.data.error.as_deref(),
                Some("The environment for this project is currently offline.")
            );
        });
    }
}
