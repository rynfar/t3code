//! Sidebar thread search (`searchSidebarThreads` in `Sidebar.logic.ts`, `useThreadSearch` in
//! `apps/web/src/state/queries.ts`, and `client-runtime/src/state/threadSearch.ts`): title, branch
//! and pull request matches ranked locally, then threads listed only because their messages match,
//! ranked across environments while keeping already-shown rows in place.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, App, BoxShadow, Context, Entity, FontWeight, HighlightStyle, IntoElement, ListAlignment, ListState,
    ParentElement, SharedString, Styled, StyledText, Task, Window, div, list, point, prelude::*, px,
};
use t3_core::families::FamilyRead;
use t3_core::pull_requests::thread_badge::{PrGlyph, PrTone};
use t3_core::search::{
    MessageRankOptions, SearchRange, ServerThreadSearchMatch, SettledThreadSearch, ShownOrder,
    ThreadSearchOptions, ThreadSearchSubject, decode_search_matches, find_for_message_match,
    find_search_token_ranges, fold_search_query, matching_search_pr_badge, merge_local_open,
    next_shown_order, rank_message_matches, rank_search_items, score_thread_search, search_pull_requests,
    visible_thread_search,
};
use t3_core::shell::ThreadShell;
use t3_core::sidebar::Section;
use t3_core::sidebar::fleet::ScopedSidebarRow;
use t3_core::sidebar::grouping::{EnvironmentOwner, GroupConsumer, ProjectRef};
use t3_core::sidebar::older_settled::{SEARCH_THREADS, search_request, searches_messages, truncate_search_query};
use t3_core::sidebar::sort::{ThreadSortOrder, thread_sort_timestamp};
use t3_core::sidebar::tag_filter::ThreadKey;
use t3_core::sidebar::thread::SidebarThread;
use t3_core::sidebar::time::compact_relative_label;
use t3_core::sidebar::SidebarRow;

use super::SidebarPane;
use super::row::AppearanceExt;
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::project_icon::ProjectVisual;
use crate::theme::{hsla, palette};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
const RESULT_HEIGHT: f32 = 36.;

#[derive(Clone, PartialEq, Eq, Hash)]
enum SearchOwner {
    Local { generation: u64 },
    Scoped(EnvironmentOwner),
}

impl SearchOwner {
    fn environment_id(&self) -> &str {
        match self {
            Self::Local { .. } => "",
            Self::Scoped(owner) => &owner.saved_id,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct OwnerSpec {
    owner: SearchOwner,
    before: Option<String>,
    project_ids: Option<Vec<String>>,
}

struct OwnerFlight {
    id: u64,
    spec: OwnerSpec,
    query: String,
    loading: bool,
    matches: Vec<ServerThreadSearchMatch>,
    rows: Option<FamilyRead>,
}

#[derive(Clone)]
struct SettledSnapshot {
    search: SettledThreadSearch<ServerThreadSearchMatch>,
    mounts: Vec<(SearchOwner, FamilyRead)>,
    owners: HashSet<SearchOwner>,
}

pub struct SearchState {
    query: String,
    normalized_query: String,
    debounced_query: String,
    settled_query: String,
    debouncing: bool,
    pending: bool,
    next_flight_id: u64,
    flights: Vec<OwnerFlight>,
    previous: Option<SettledSnapshot>,
    shown_order: Option<ShownOrder<ThreadKey>>,
    results: Vec<ThreadKey>,
    content_matches: Vec<(ThreadKey, ServerThreadSearchMatch)>,
    highlighted: usize,
    list: ListState,
    debounce: Option<Task<()>>,
    requests: Vec<Task<()>>,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            normalized_query: String::new(),
            debounced_query: String::new(),
            settled_query: String::new(),
            debouncing: false,
            pending: false,
            next_flight_id: 0,
            flights: Vec::new(),
            previous: None,
            shown_order: None,
            results: Vec::new(),
            content_matches: Vec::new(),
            highlighted: 0,
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            debounce: None,
            requests: Vec::new(),
        }
    }
}

impl SearchState {
    pub fn is_active(&self) -> bool {
        !self.normalized_query.is_empty()
    }

    pub fn is_pending(&self) -> bool {
        self.pending
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub fn settled_query(&self) -> &str {
        &self.settled_query
    }

    pub fn highlighted_key(&self) -> Option<&ThreadKey> {
        self.results.get(self.highlighted)
    }

    pub fn move_highlight(&mut self, step: isize) {
        let count = self.results.len() as isize;
        if count > 0 {
            self.highlighted = (self.highlighted as isize + step).rem_euclid(count) as usize;
            self.list.scroll_to_reveal_item(self.highlighted);
        }
    }

    pub fn set_query(&mut self, query: String, state: &Entity<AppState>, cx: &mut Context<SidebarPane>) {
        if query == self.query {
            return;
        }
        let was_active = self.is_active();
        self.query = query;
        let next_normalized = truncate_search_query(&self.query);
        if next_normalized != self.normalized_query {
            self.normalized_query = next_normalized.clone();
            self.highlighted = 0;
            self.debounce = None;
            self.requests.clear();
            self.flights.clear();
            if next_normalized.is_empty() {
                self.debounced_query.clear();
                self.settled_query.clear();
                self.debouncing = false;
                self.pending = false;
                self.previous = None;
                self.shown_order = None;
            } else {
                self.debouncing = true;
                let state = state.clone();
                self.debounce = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.search.normalized_query != next_normalized {
                            return;
                        }
                        this.search.debouncing = false;
                        this.search.debounced_query = next_normalized;
                        this.search.debounce = None;
                        this.search.refresh(&state, cx);
                        cx.notify();
                    });
                }));
            }
        }
        self.refresh(state, cx);
        if was_active && !self.is_active() {
            state.update(cx, |state, cx| {
                state.rebuild_sidebar(cx);
                cx.emit(crate::app_state::AppEvent::Shell);
            });
        }
    }

    /// Re-syncs server search requests for the current scope/owners and re-ranks against the model.
    pub fn refresh(&mut self, state: &Entity<AppState>, cx: &mut Context<SidebarPane>) {
        if !self.is_active() {
            self.flights.clear();
            self.requests.clear();
            self.previous = None;
            self.shown_order = None;
            self.settled_query.clear();
            self.pending = false;
            self.results.clear();
            self.list.reset(0);
            self.content_matches.clear();
            state.update(cx, |state, cx| state.sync_search_mounts(None, &[], cx));
            return;
        }
        let desired = desired_specs(state.read(cx), cx);
        let admitted: HashSet<&SearchOwner> = desired.iter().map(|spec| &spec.owner).collect();
        if let Some(prev) = &mut self.previous {
            prev.mounts.retain(|(owner, _)| admitted.contains(owner));
            prev.owners.retain(|owner| admitted.contains(owner));
            let live_envs: HashSet<&str> = prev.owners.iter().map(SearchOwner::environment_id).collect();
            prev.search.matches.retain(|hit| live_envs.contains(hit.environment_id.as_str()));
            if prev.owners.is_empty() {
                self.previous = None;
            }
        }
        let messages = searches_messages(&self.normalized_query);
        let any_older = desired.iter().any(|spec| spec.before.is_some());
        let can_search = !desired.is_empty()
            && (messages || (any_older && self.normalized_query.encode_utf16().count() >= 2));

        if !can_search {
            self.flights.clear();
            self.requests.clear();
        } else if !self.debouncing && self.debounced_query == self.normalized_query {
            self.sync_flights(&desired, state, cx);
        }
        self.reconcile(can_search, state, cx);
    }

    fn sync_flights(
        &mut self,
        desired: &[OwnerSpec],
        state: &Entity<AppState>,
        cx: &mut Context<SidebarPane>,
    ) {
        let active_query = self.debounced_query.clone();
        self.flights.retain(|flight| {
            flight.query == active_query
                && desired.iter().any(|spec| {
                    *spec == flight.spec
                        && search_request(&active_query, spec.before.as_deref(), spec.project_ids.as_deref()).is_some()
                })
        });
        if !self.flights.iter().any(|flight| flight.loading) {
            self.requests.clear();
        }
        let runtime = state.read(cx).runtime().clone();
        for spec in desired {
            let Some(payload) = search_request(&active_query, spec.before.as_deref(), spec.project_ids.as_deref()) else {
                continue;
            };
            if self.flights.iter().any(|flight| flight.spec == *spec && flight.query == active_query) {
                continue;
            }
            self.next_flight_id += 1;
            let flight_id = self.next_flight_id;
            self.flights.push(OwnerFlight {
                id: flight_id,
                spec: spec.clone(),
                query: active_query.clone(),
                loading: true,
                matches: Vec::new(),
                rows: None,
            });
            match &spec.owner {
                SearchOwner::Local { generation } => {
                    let Some(client) = state.read(cx).client_handle() else {
                        self.finish_flight(flight_id, None);
                        continue;
                    };
                    let generation = *generation;
                    let state = state.clone();
                    let runtime = runtime.clone();
                    self.requests.push(cx.spawn(async move |this, cx| {
                        let response = runtime
                            .spawn(async move { client.request(SEARCH_THREADS, payload).await })
                            .await
                            .ok()
                            .and_then(Result::ok);
                        let _ = this.update(cx, |this, cx| {
                            let valid = state.read(cx).fleet.environments.is_empty()
                                && state.read(cx).shell.generation == generation;
                            if this.search.finish_flight(flight_id, valid.then_some(response).flatten()) {
                                this.search.refresh(&state, cx);
                                cx.notify();
                            }
                        });
                    }));
                }
                SearchOwner::Scoped(owner) => {
                    let owner = owner.clone();
                    let resolve = state.read(cx).connections.destination(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE);
                    let state = state.clone();
                    let runtime = runtime.clone();
                    self.requests.push(cx.spawn(async move |this, cx| {
                        let response = match resolve.await {
                            Ok(target) => runtime
                                .spawn(async move { target.client.request(SEARCH_THREADS, payload).await })
                                .await
                                .ok()
                                .and_then(Result::ok),
                            Err(_) => None,
                        };
                        let _ = this.update(cx, |this, cx| {
                            let valid = state.read(cx).scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE);
                            if this.search.finish_flight(flight_id, valid.then_some(response).flatten()) {
                                this.search.refresh(&state, cx);
                                cx.notify();
                            }
                        });
                    }));
                }
            }
        }
    }

    fn finish_flight(&mut self, flight_id: u64, value: Option<serde_json::Value>) -> bool {
        let Some(flight) = self.flights.iter_mut().find(|flight| flight.id == flight_id) else {
            return false;
        };
        flight.loading = false;
        if let Some(value) = value {
            flight.matches = decode_search_matches(&value, flight.spec.owner.environment_id());
            flight.rows = serde_json::from_value::<FamilyRead>(value)
                .ok()
                .filter(|read| !read.threads.is_empty());
        } else {
            flight.matches.clear();
            flight.rows = None;
        }
        true
    }

    fn reconcile(&mut self, can_search: bool, state: &Entity<AppState>, cx: &mut Context<SidebarPane>) {
        let ready = if can_search && !self.debouncing && self.debounced_query == self.normalized_query {
            let any_loading = self.flights.iter().any(|flight| flight.loading);
            let any_data = self.flights.iter().any(|flight| !flight.matches.is_empty() || flight.rows.is_some());
            (!any_loading || any_data).then(|| SettledSnapshot {
                search: SettledThreadSearch {
                    query: self.debounced_query.clone(),
                    matches: self.flights.iter().flat_map(|flight| flight.matches.iter().cloned()).collect(),
                },
                mounts: self
                    .flights
                    .iter()
                    .filter_map(|flight| Some((flight.spec.owner.clone(), flight.rows.clone()?)))
                    .collect(),
                owners: self.flights.iter().map(|flight| flight.spec.owner.clone()).collect(),
            })
        } else {
            None
        };
        if let Some(snapshot) = &ready {
            self.previous = Some(snapshot.clone());
        }
        let visible = if can_search {
            visible_thread_search(
                &self.normalized_query,
                ready.as_ref().map(|snapshot| &snapshot.search),
                self.previous.as_ref().map(|snapshot| &snapshot.search),
            )
        } else {
            SettledThreadSearch { query: String::new(), matches: Vec::new() }
        };
        let active_mounts: &[(SearchOwner, FamilyRead)] = if visible.query.is_empty() {
            &[]
        } else if let Some(snapshot) = &ready {
            &snapshot.mounts
        } else if let Some(prev) = &self.previous {
            &prev.mounts
        } else {
            &[]
        };
        let local_mount = active_mounts.iter().find_map(|(owner, read)| match owner {
            SearchOwner::Local { generation } => Some((*generation, read.clone())),
            SearchOwner::Scoped(_) => None,
        });
        let scoped_mounts: Vec<(EnvironmentOwner, FamilyRead)> = active_mounts
            .iter()
            .filter_map(|(owner, read)| match owner {
                SearchOwner::Local { .. } => None,
                SearchOwner::Scoped(owner) => Some((owner.clone(), read.clone())),
            })
            .collect();
        state.update(cx, |state, cx| state.sync_search_mounts(local_mount, &scoped_mounts, cx));

        let previous_highlight = self.highlighted_key().cloned();
        let app = state.read(cx);
        let now = crate::sidebar::controller::now_ms();
        let folded = fold_search_query(&self.query);
        let subjects = build_subjects(app);
        let title_results: Vec<ThreadKey> = if folded.is_empty() {
            Vec::new()
        } else {
            rank_search_items(&subjects, |(_, subject), typos| {
                score_thread_search(&folded, subject, now, ThreadSearchOptions { typos, picks: false })
            })
            .into_iter()
            .map(|ranked| ranked.item.0.clone())
            .collect()
        };
        let title_set: HashSet<&ThreadKey> = title_results.iter().collect();

        let subject_by_key: HashMap<&ThreadKey, &ThreadSearchSubject> =
            subjects.iter().map(|(key, subject)| (key, subject)).collect();
        let ranked_hits = rank_message_matches(
            &visible.matches,
            |hit| subject_by_key.get(&ThreadKey::new(&hit.environment_id, &hit.thread_id)).copied(),
            &folded,
            now,
            MessageRankOptions { picks: false, without_title_matches: false },
        );
        let mut best_by_key: HashMap<ThreadKey, &ServerThreadSearchMatch> = HashMap::new();
        let mut ranked_keys = Vec::new();
        for hit in ranked_hits {
            let key = ThreadKey::new(&hit.environment_id, &hit.thread_id);
            if let std::collections::hash_map::Entry::Vacant(vacant) = best_by_key.entry(key.clone()) {
                vacant.insert(hit);
                ranked_keys.push(key);
            }
        }
        let order = next_shown_order(self.shown_order.as_ref(), &visible.query, &ranked_keys, usize::MAX);
        let mut results = title_results.clone();
        let mut content_matches = Vec::new();
        if !folded.is_empty() {
            for key in &order.keys {
                if subject_by_key.contains_key(key)
                    && !title_set.contains(key)
                    && let Some(hit) = best_by_key.get(key)
                {
                    results.push(key.clone());
                    content_matches.push((key.clone(), (*hit).clone()));
                }
            }
        }
        self.shown_order = Some(order);
        self.settled_query = visible.query;
        self.pending = can_search && (self.debouncing || self.flights.iter().any(|flight| flight.loading));
        self.content_matches = content_matches;
        self.highlighted = previous_highlight
            .and_then(|key| results.iter().position(|candidate| *candidate == key))
            .unwrap_or(0);
        if self.results != results {
            self.list.reset(results.len());
        }
        self.results = results;
        if !self.results.is_empty() {
            self.list.scroll_to_reveal_item(self.highlighted);
        }
    }

    pub fn content_match(&self, thread_id: &ThreadKey) -> Option<&ServerThreadSearchMatch> {
        self.content_matches
            .iter()
            .find(|(id, _)| id == thread_id)
            .map(|(_, hit)| hit)
    }

    #[cfg(all(test, feature = "snapshot"))]
    fn snippet(&self, thread_id: &ThreadKey) -> Option<&str> {
        self.content_match(thread_id)
            .map(|hit| hit.snippet.as_str())
            .filter(|s| !s.is_empty())
    }
}

fn desired_specs(app: &AppState, cx: &App) -> Vec<OwnerSpec> {
    if !app.fleet.environments.is_empty() {
        let owners: Vec<_> = app
            .fleet
            .environments
            .iter()
            .filter(|entry| app.scoped_ready(&entry.summary.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE))
            .map(|entry| entry.summary.owner.clone())
            .collect();
        if owners.is_empty() {
            return Vec::new();
        }
        let groups = app.project_groups_for(GroupConsumer::Sidebar, cx);
        let ready = app
            .fleet
            .environments
            .iter()
            .all(|entry| entry.summary.synchronized && entry.config.config.is_some());
        let project_ids = app.project_preferences.scoped_members(&groups, ready).map(|members| {
            let mut ids: Vec<String> = members.into_iter().map(|reference| reference.project_id).collect();
            ids.sort();
            ids
        });
        return owners
            .into_iter()
            .map(|owner| OwnerSpec {
                before: app
                    .sidebar
                    .owner_omitted
                    .get(&owner)
                    .and_then(|omitted| omitted.older())
                    .map(|older| older.before),
                owner: SearchOwner::Scoped(owner),
                project_ids: project_ids.clone(),
            })
            .collect();
    }
    if app.client_handle().is_none() {
        return Vec::new();
    }
    let project_ids = app.sidebar.project_scope.as_ref().map(|scope| {
        let mut ids: Vec<String> = scope.iter().cloned().collect();
        ids.sort();
        ids
    });
    vec![OwnerSpec {
        owner: SearchOwner::Local { generation: app.shell.generation },
        before: app.omitted.older().map(|older| older.before),
        project_ids,
    }]
}

fn scoped_thread_shell<'a>(app: &'a AppState, owner: &EnvironmentOwner, id: &str) -> Option<&'a ThreadShell> {
    if app.environments.active.as_deref() == Some(owner.saved_id.as_str())
        && app.shell.generation == owner.shell_generation
        && let Some(shell) = app.thread_shell(id)
    {
        return Some(shell);
    }
    app.sidebar
        .owner_omitted
        .get(owner)
        .and_then(|omitted| omitted.mounts.mounted_thread(owner.shell_generation, id))
}

fn subject_for_thread(
    thread: &SidebarThread,
    local_opened_at: Option<i64>,
    shell: Option<&ThreadShell>,
) -> ThreadSearchSubject {
    let activity_at = match thread.latest_user_authored_message_at {
        None => thread_sort_timestamp(thread, ThreadSortOrder::UpdatedAt),
        Some(authored) => authored.or(thread.created_at),
    };
    ThreadSearchSubject {
        id: Some(thread.id.clone()),
        title: thread.title.clone(),
        branch: thread.branch.clone(),
        pull_requests: search_pull_requests(shell.map_or(thread.pull_requests.as_slice(), |row| &row.pull_requests)),
        activity_at,
        opens: merge_local_open(thread.opens.as_ref(), local_opened_at),
        ..Default::default()
    }
}

fn build_subjects(app: &AppState) -> Vec<(ThreadKey, ThreadSearchSubject)> {
    if app.fleet.environments.is_empty() {
        let saved = app.environments.active();
        let canonical = app
            .server_config
            .config
            .as_ref()
            .map(|config| config.environment.environment_id.as_str())
            .filter(|id| !id.is_empty())
            .or_else(|| saved.and_then(|entry| entry.environment_id.as_deref()));
        let saved_id = saved.map(|entry| entry.id.as_str());
        return app
            .sidebar
            .model
            .threads
            .iter()
            .map(|thread| {
                let opened = app.open_history.opened_at(saved_id, canonical, &thread.id);
                (
                    ThreadKey::new("", &thread.id),
                    subject_for_thread(thread, opened, app.thread_shell(&thread.id)),
                )
            })
            .collect();
    }
    app.sidebar
        .fleet
        .threads
        .iter()
        .map(|row| {
            let opened = app.open_history.opened_at(
                Some(&row.owner.saved_id),
                row.owner.canonical_id.as_deref(),
                &row.thread.id,
            );
            let shell = scoped_thread_shell(app, &row.owner, &row.thread.id);
            (row.key.clone(), subject_for_thread(&row.thread, opened, shell))
        })
        .collect()
}

fn char_ranges_to_bytes(text: &str, ranges: &[SearchRange]) -> Vec<std::ops::Range<usize>> {
    if ranges.is_empty() {
        return Vec::new();
    }
    let mut offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    offsets.push(text.len());
    let mut bytes = Vec::with_capacity(ranges.len());
    let mut prev_end = 0;
    for range in ranges {
        let Some(&start) = offsets.get(range.start) else { continue };
        let Some(&end) = offsets.get(range.end) else { continue };
        let clamped_start = start.max(prev_end);
        if clamped_start < end {
            bytes.push(clamped_start..end);
            prev_end = end;
        }
    }
    bytes
}

fn is_search_thread_settled(app: &AppState, id: &ThreadKey, thread: &SidebarThread, scoped: bool) -> bool {
    if scoped {
        return app
            .sidebar
            .fleet
            .rows
            .iter()
            .find_map(|row| match row {
                ScopedSidebarRow::Thread { index, section, .. }
                    if app.sidebar.fleet.threads.get(*index).is_some_and(|item| item.key == *id) =>
                {
                    Some(*section == Section::Settled)
                }
                _ => None,
            })
            .unwrap_or_else(|| thread.is_settled());
    }
    app.sidebar
        .model
        .rows
        .iter()
        .find_map(|row| match row {
            SidebarRow::Thread { index, section }
                if app.sidebar.model.threads.get(*index).is_some_and(|item| item.id == id.thread_id) =>
            {
                Some(*section == Section::Settled)
            }
            _ => None,
        })
        .unwrap_or_else(|| thread.is_settled())
}

impl SidebarPane {
    pub(super) fn render_search_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        if self.search.results.is_empty() {
            let label = if self.search.is_pending() { "Searching thread messages…" } else { "No threads found" };
            return div()
                .flex()
                .justify_center()
                .px(px(8.))
                .py(px(24.))
                .text_xs()
                .text_color(hsla(p.muted_foreground).opacity(0.6))
                .child(label)
                .into_any_element();
        }
        list(
            self.search.list.clone(),
            cx.processor(|this, index: usize, _, cx| this.render_search_result(index, cx)),
        )
        .flex_1()
        .into_any_element()
    }

    fn render_search_result(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(id) = self.search.results.get(index).cloned() else { return div().into_any_element() };
        let state = self.state.read(cx);
        let scoped = state.sidebar.fleet.thread(&id);
        let Some(thread) = scoped
            .map(|row| &row.thread)
            .or_else(|| state.sidebar.model.thread(&id.thread_id).filter(|_| id.environment_id.is_empty()))
        else {
            return div().h(px(RESULT_HEIGHT)).into_any_element();
        };
        let active = if scoped.is_some() {
            state.active_thread_key(&id.thread_id).as_ref() == Some(&id)
                && state.selected_thread.as_deref() == Some(id.thread_id.as_str())
        } else {
            state.selected_thread.as_deref() == Some(id.thread_id.as_str())
        };
        let highlighted = index == self.search.highlighted;
        let settled = is_search_thread_settled(state, &id, thread, scoped.is_some());
        #[cfg(feature = "snapshot")]
        let now = state.sidebar.now_override.unwrap_or(self.now);
        #[cfg(not(feature = "snapshot"))]
        let now = self.now;
        let timestamp = t3_core::sidebar::thread::activity_timestamp(thread).map(|at| compact_relative_label(at, now));
        let pr_links = scoped
            .and_then(|row| scoped_thread_shell(state, &row.owner, &row.thread.id))
            .or_else(|| if id.environment_id.is_empty() { state.thread_shell(&id.thread_id) } else { None })
            .map_or(thread.pull_requests.as_slice(), |shell| shell.pull_requests.as_slice());
        let pr_badge = matching_search_pr_badge(pr_links, &self.search.normalized_query)
            .or_else(|| matching_search_pr_badge(pr_links, &self.search.settled_query))
            .map(|badge| {
                let icon = match badge.glyph {
                    PrGlyph::PullRequest => Icon::GitPullRequestArrow,
                    PrGlyph::Draft => Icon::GitPullRequestDraft,
                    PrGlyph::Closed => Icon::GitPullRequestClosed,
                    PrGlyph::Merged => Icon::GitMerge,
                    PrGlyph::Stack => Icon::Layers,
                };
                let tone = match badge.tone {
                    PrTone::Open => p.pill_completed,
                    PrTone::Draft | PrTone::Muted => p.muted_foreground,
                    PrTone::Closed => p.pill_failed,
                    PrTone::Merged => p.pill_plan,
                };
                let hover_color = hsla(tone);
                let base_color = if settled { hsla(p.secondary_label) } else { hover_color };
                (icon, base_color, hover_color, badge.has_snapshot, badge.text)
            });

        let visual = if scoped.is_some() {
            ProjectVisual::for_scoped_project(
                state,
                &ProjectRef { environment_id: id.environment_id.clone(), project_id: thread.project_id.clone() },
            )
        } else {
            ProjectVisual::for_project(state, &thread.project_id)
        };
        let favicon_size = crate::project_icon::size(16., cx);
        let favicon_ink = hsla(p.sidebar_icon);
        let favicon = if settled && !active && !highlighted {
            visual.render_receding(favicon_size, favicon_ink, cx)
        } else {
            visual.render(favicon_size, favicon_ink, cx)
        };

        let excerpt = self.search.content_match(&id).filter(|hit| !hit.snippet.is_empty()).map(|hit| {
            let prefix = match hit.source.as_deref() {
                Some("user") => Some(("You:", hsla(p.info_foreground).opacity(0.85))),
                Some(_) => Some(("Agent:", hsla(p.success_foreground).opacity(0.85))),
                None => None,
            };
            let ranges = char_ranges_to_bytes(&hit.snippet, &find_search_token_ranges(&hit.snippet, &self.search.settled_query));
            let bold = HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), color: Some(hsla(p.foreground)), ..Default::default() };
            let text = StyledText::new(SharedString::from(hit.snippet.clone())).with_highlights(ranges.into_iter().map(move |range| (range, bold)));
            (prefix, text)
        });

        let has_excerpt = excerpt.is_some();
        let light = !cx.window_appearance_is_dark();
        let ring = BoxShadow {
            color: hsla(p.sidebar_border).opacity(0.7),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.),
            inset: true,
        };
        let title: SharedString = thread.title.clone().into();
        let drop_id = id.clone();
        let title_color = if active || highlighted || !settled {
            hsla(p.foreground)
        } else {
            hsla(p.secondary_label).opacity(0.7)
        };
        let row_id = SharedString::from(format!("search-result-{:?}", id));
        let debug_row_id = row_id.clone();
        div()
            .px(px(8.))
            .py(px(1.))
            .child(
                div()
                    .id(row_id)
                    .debug_selector(move || debug_row_id.to_string())
                    .group("sidebar-row")
                    .w_full()
                    .min_h(px(RESULT_HEIGHT))
                    .flex()
                    .when(has_excerpt, |this| this.items_start().py(px(5.)))
                    .when(!has_excerpt, |this| this.items_center())
                    .gap(px(8.))
                    .px(px(10.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(active || highlighted, |this| {
                        this.bg(hsla(p.sidebar_row_active)).when(active && light, |this| this.shadow(vec![ring]))
                    })
                    .when(!active && !highlighted, |this| this.hover(|style| style.bg(hsla(p.sidebar_row_hover))))
                    .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| {
                        style.bg(hsla(p.primary).opacity(0.08))
                    })
                    .on_drop(cx.listener(move |this, paths: &gpui::ExternalPaths, window, cx| {
                        let key = drop_id.clone();
                        let files = paths.paths().to_vec();
                        this.state.update(cx, |state, cx| {
                            state.drop_files_on_sidebar_thread(key, files, cx);
                        });
                        this.clear_search(window, cx);
                    }))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.search.highlighted != index {
                            this.search.highlighted = index;
                            cx.notify();
                        }
                    }))
                    .child(div().flex_none().when(has_excerpt, |this| this.pt(px(2.))).child(favicon))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .font_weight(if settled { FontWeight::NORMAL } else { FontWeight::MEDIUM })
                                            .text_color(title_color)
                                            .when(settled && !active && !highlighted, |this| {
                                                this.group_hover("sidebar-row", move |style| style.text_color(hsla(p.foreground)))
                                            })
                                            .when(thread.regenerating_title, |this| this.opacity(0.55))
                                            .child(title),
                                    )
                                    .children(pr_badge.map(|(icon, base_color, hover_color, has_snapshot, text)| {
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap(px(3.))
                                            .text_xs()
                                            .text_color(base_color)
                                            .when(settled && has_snapshot, |this| {
                                                this.group_hover("sidebar-row", move |style| style.text_color(hover_color))
                                            })
                                            .child(icon.sized(px(12.)).text_color(base_color).when(
                                                settled && has_snapshot,
                                                |this| {
                                                    this.group_hover("sidebar-row", move |style| style.text_color(hover_color))
                                                },
                                            ))
                                            .child(text)
                                    }))
                                    .children(timestamp.map(|label| {
                                        div().flex_none().text_xs().text_color(hsla(p.sidebar_secondary_label)).child(label)
                                    })),
                            )
                            .children(excerpt.map(|(prefix, text)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(hsla(p.sidebar_secondary_label))
                                    .children(prefix.map(|(label, color)| div().flex_none().font_weight(FontWeight::MEDIUM).text_color(color).child(label)))
                                    .child(div().flex_1().min_w_0().truncate().child(text))
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_search_result(id.clone(), window, cx);
                    })),
            )
            .into_any_element()
    }

    pub(super) fn activate_search_result(&mut self, key: ThreadKey, window: &mut Window, cx: &mut Context<Self>) {
        let query = if self.search.settled_query.is_empty() {
            self.search.normalized_query.as_str()
        } else {
            self.search.settled_query.as_str()
        };
        let pending_find = self
            .search
            .content_match(&key)
            .map(|hit| find_for_message_match(&hit.snippet, hit.message_id.as_deref(), query));
        self.state.update(cx, |state, cx| {
            if key.environment_id.is_empty() {
                state.click_sidebar_thread(&key.thread_id, gpui::Modifiers::default(), cx);
            } else {
                if !state.sidebar.fleet_selection.is_empty() {
                    state.sidebar.fleet_selection.clear();
                    cx.emit(crate::app_state::AppEvent::SidebarSelection);
                }
                state.sidebar.fleet_anchor = Some(key.clone());
                if let Some(owner) = state.scoped_environment(&key.environment_id).map(|entry| entry.summary.owner.clone()) {
                    state.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Thread(key.thread_id.clone()), cx);
                }
            }
            if let Some(find) = pending_find {
                state.queue_thread_find(key, find, cx);
            }
        });
        self.clear_search(window, cx);
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::AppContext;
    use serde_json::json;
    use t3_client::supervisor::{ClientFixture, ConnectionStatus};
    use t3_core::sidebar::grouping::GroupConsumer;

    #[gpui::test]
    async fn cut_search_requests_the_chosen_project_scope(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, active_fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });
        let (client, mut fixture) = ClientFixture::handle(serde_json::from_value(json!({
            "authenticated":true,"permissions":["orchestration:read"]
        })).unwrap());
        let (mut shell, mut config) = state.read_with(cx, |state, _| (state.shell.clone(),state.server_config.clone()));
        let before = t3_core::sidebar::time::format_iso(100_000);
        let snapshot = shell.snapshot.as_mut().unwrap();
        snapshot.snapshot_sequence = 1;
        snapshot.older_settled = Some(serde_json::from_value(json!({
            "before":before,"asOfSequence":1,"counts":[
                {"projectId":"checkout-a","count":2},
                {"projectId":"checkout-b","count":3},
                {"projectId":"other","count":4}
            ]
        })).unwrap());
        config.config.as_mut().unwrap().environment.environment_id = "search-scope-server".into();
        state.update(cx, |state, _| state.selected_thread = None);
        let (sent, loaded) = tokio::sync::oneshot::channel();
        let mut sent = Some(sent);
        let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("search-scope-fixture").is_some() {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        let install = state.update(cx, |state, _| state.connections.install_fixture(
            "search-scope-fixture",client,shell,config,ConnectionStatus::Connected,
        ));
        let events = runtime.spawn(install).await.unwrap();
        loaded.await.unwrap();
        let (owner, scope_a) = state.read_with(cx, |state, cx| {
            let groups = state.project_groups_for(GroupConsumer::Sidebar,cx);
            let group = groups.iter().find(|group| group.member_refs.iter().any(|reference|
                reference.environment_id == "search-scope-fixture" && reference.project_id == "checkout-b"
            )).unwrap();
            (state.scoped_environment("search-scope-fixture").unwrap().summary.owner.clone(),group.key.clone())
        });
        state.update(cx, |state, cx| state.set_scoped_project_scope(vec![scope_a],cx));
        let project_ids = state.read_with(cx, |state, cx| {
            let groups = state.project_groups_for(GroupConsumer::Sidebar,cx);
            let members = state.project_preferences.scoped_members(&groups,state.fleet.environments.iter().all(|entry| entry.summary.synchronized)).unwrap();
            let mut ids = members.into_iter().map(|reference| reference.project_id).collect::<Vec<_>>();
            ids.sort();
            assert!(!ids.is_empty());
            assert!(ids.iter().any(|id| id == "checkout-b"));
            assert!(ids.iter().all(|id| id != "other"));
            assert_eq!(state.fleet.environments.len(),1);
            assert!(state.scoped_ready(&owner,t3_core::auth::ORCHESTRATION_READ_SCOPE));
            assert_eq!(state.sidebar.owner_omitted.get(&owner).unwrap().older().unwrap().before,before);
            assert!(!state.sidebar.shelves.settled_expanded);
            ids
        });
        let (pane, cx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(),cx));
            let confirm = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(),cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(),window,cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(),window,cx));
            SidebarPane::new(state.clone(),composer,menu,confirm,snooze,window,cx)
        });
        pane.update(cx, |pane, cx| pane.search.set_query("deploy".into(),&pane.state,cx));
        cx.run_until_parked();
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        let (method,payload,reply) = fixture.next_request().await;
        assert_eq!(method,SEARCH_THREADS);
        assert_eq!(payload["query"],"deploy");
        assert_eq!(payload["olderSettledBefore"],before);
        assert_eq!(payload["projectIds"],json!(project_ids),"cut-present search must forward the chosen physical project ids");
        let tasks = pane.update(cx, |pane, _| std::mem::take(&mut pane.search.requests));
        assert_eq!(tasks.len(),1);
        reply.send(Ok(json!({
            "matches":[{"threadId":"menu-thread","snippet":"deployed"}],
            "snapshotSequence":1,"threads":[]
        }))).unwrap();
        for task in tasks { task.await; }
        pane.read_with(cx, |pane, _| {
            let key = ThreadKey::new("search-scope-fixture", "menu-thread");
            assert_eq!(pane.search.snippet(&key), Some("deployed"));
            assert_eq!(pane.search.content_matches.len(), 1);
            assert_eq!(pane.search.query,"deploy");
        });
        state.read_with(cx, |state, _| {
            assert_eq!(state.scoped_environment("search-scope-fixture").unwrap().summary.owner,owner);
            assert!(state.selected_thread.is_none());
        });
        state.update(cx, |state, _| state.stop_pump());
        drop(events);
        cx.cx.update(|cx| { cx.shutdown(); cx.clear_globals(); });
        drop(active_fixture);
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn same_query_scope_and_grant_changes_resync_requests_and_unmount_search_rows(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, active_fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });
        let (client, mut fixture) = ClientFixture::handle(serde_json::from_value(json!({
            "authenticated": true, "permissions": ["orchestration:read"]
        })).unwrap());
        let (mut shell, mut config) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
        let before = t3_core::sidebar::time::format_iso(100_000);
        let sample = shell.snapshot.as_ref().unwrap().threads[0].clone();
        let older_row = |id: &str, project_id: &str, title: &str| {
            let mut row = sample.clone();
            row.id = id.into();
            row.project_id = project_id.into();
            row.title = title.into();
            row.lineage.root_thread_id = id.into();
            row.settled_override = Some("settled".into());
            row.settled_at = Some(t3_core::sidebar::time::format_iso(50_000));
            row
        };
        let snapshot = shell.snapshot.as_mut().unwrap();
        snapshot.snapshot_sequence = 1;
        snapshot.threads.clear();
        snapshot.older_settled = Some(serde_json::from_value(json!({
            "before": before, "asOfSequence": 1, "counts": [
                {"projectId": "checkout-a", "count": 2},
                {"projectId": "checkout-b", "count": 3},
                {"projectId": "other", "count": 4}
            ]
        })).unwrap());
        config.config.as_mut().unwrap().environment.environment_id = "search-lifetime-server".into();
        state.update(cx, |state, _| state.selected_thread = None);
        let (sent, loaded) = tokio::sync::oneshot::channel();
        let mut sent = Some(sent);
        let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("search-lifetime-fixture").is_some() {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        let install = state.update(cx, |state, _| state.connections.install_fixture(
            "search-lifetime-fixture", client, shell, config, ConnectionStatus::Connected,
        ));
        let events = runtime.spawn(install).await.unwrap();
        loaded.await.unwrap();
        let (owner, scope_shared, scope_other) = state.read_with(cx, |state, cx| {
            let groups = state.project_groups_for(GroupConsumer::Sidebar, cx);
            let shared = groups.iter().find(|group| group.member_refs.iter().any(|r| r.project_id == "checkout-b")).unwrap().key.clone();
            let other = groups.iter().find(|group| group.member_refs.iter().any(|r| r.project_id == "other")).unwrap().key.clone();
            (state.scoped_environment("search-lifetime-fixture").unwrap().summary.owner.clone(), shared, other)
        });
        state.update(cx, |state, cx| state.set_scoped_project_scope(vec![scope_shared], cx));
        let (pane, cx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            SidebarPane::new(state.clone(), composer, menu, confirm, snooze, window, cx)
        });

        pane.update(cx, |pane, cx| pane.search.set_query("deploy".into(), &pane.state, cx));
        cx.run_until_parked();
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        let (_, first_payload, first_reply) = fixture.next_request().await;
        assert_eq!(first_payload["projectIds"], json!(["checkout-a", "checkout-b"]));
        let first_tasks = pane.update(cx, |pane, _| std::mem::take(&mut pane.search.requests));
        assert_eq!(first_tasks.len(), 1);
        first_reply.send(Ok(json!({
            "matches": [{"threadId": "older-b", "snippet": "deploy shared"}],
            "snapshotSequence": 1,
            "threads": [older_row("older-b", "checkout-b", "Unrelated shared")]
        }))).unwrap();
        for task in first_tasks { task.await; }
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.search.results, vec![ThreadKey::new("search-lifetime-fixture", "older-b")]);
        });

        // Changing project scope while the same query remains active must re-request with the new scope.
        state.update(cx, |state, cx| state.set_scoped_project_scope(vec![scope_other], cx));
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.search.requests.len(), 1, "same-query scope change must issue a refreshed searchThreads request");
        });
        let second_req = fixture.next_request().await;
        assert_eq!(second_req.1["query"], "deploy");
        assert_eq!(second_req.1["projectIds"], json!(["other"]));
        let second_tasks = pane.update(cx, |pane, _| std::mem::take(&mut pane.search.requests));
        second_req.2.send(Ok(json!({
            "matches": [{"threadId": "older-other", "snippet": "deploy other"}],
            "snapshotSequence": 1,
            "threads": [older_row("older-other", "other", "Unrelated other")]
        }))).unwrap();
        for task in second_tasks { task.await; }
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.search.results, vec![ThreadKey::new("search-lifetime-fixture", "older-other")]);
        });

        // Phase-only transitions (e.g. non-active environment status changes) must refresh active search without Shell.
        state.update(cx, |state, cx| {
            let mut env = (*state.fleet.environments[0]).clone();
            env.status = ConnectionStatus::Reconnecting { reason: "lost".into(), attempt: 1 };
            std::sync::Arc::make_mut(&mut state.fleet).environments[0] = std::sync::Arc::new(env);
            cx.emit(crate::app_state::AppEvent::Phase);
        });
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| assert!(pane.search.results.is_empty(), "Phase offline must clear stale search results"));
        state.update(cx, |state, cx| {
            let mut env = (*state.fleet.environments[0]).clone();
            env.status = ConnectionStatus::Connected;
            std::sync::Arc::make_mut(&mut state.fleet).environments[0] = std::sync::Arc::new(env);
            cx.emit(crate::app_state::AppEvent::Phase);
        });
        cx.run_until_parked();
        let phase_req = fixture.next_request().await;
        assert_eq!(phase_req.0, SEARCH_THREADS);
        let phase_tasks = pane.update(cx, |pane, _| std::mem::take(&mut pane.search.requests));
        phase_req.2.send(Ok(json!({
            "matches": [{"threadId": "older-other", "snippet": "deploy other"}],
            "snapshotSequence": 1,
            "threads": [older_row("older-other", "other", "Unrelated other")]
        }))).unwrap();
        for task in phase_tasks { task.await; }

        // Revoking orchestration:read while search is active must unmount SEARCH_KEY and clear stale hits.
        let (grant_tx, grant_rx) = tokio::sync::oneshot::channel();
        let mut grant_tx = Some(grant_tx);
        let owner_for_sub = owner.clone();
        let _grant_sub = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Shell && !state.scoped_ready(&owner_for_sub, t3_core::auth::ORCHESTRATION_READ_SCOPE) {
                if let Some(tx) = grant_tx.take() { let _ = tx.send(()); }
            }
        }));
        fixture.grants.send(Some(serde_json::from_value(json!({"authenticated": true, "permissions": []})).unwrap())).unwrap();
        grant_rx.await.unwrap();
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| assert!(pane.search.results.is_empty(), "revoked read grant must clear stale search results"));
        state.read_with(cx, |state, _| {
            assert!(state.sidebar.owner_omitted.get(&owner).is_none_or(|o| o.mounts.generation_of("search").is_none()));
        });

        state.update(cx, |state, _| state.stop_pump());
        drop(events);
        cx.cx.update(|cx| { cx.shutdown(); cx.clear_globals(); });
        drop(active_fixture);
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn cross_environment_matches_rank_by_quality_hold_shown_order_and_retain_related_queries(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, active_fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });
        let now = crate::sidebar::controller::now_ms();
        let now_iso = t3_core::sidebar::time::format_iso(now);
        let day_ms = 86_400_000;
        let (base_shell, base_config) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
        let sample = base_shell.snapshot.as_ref().unwrap().threads[0].clone();
        let make_row = |id: &str, title: &str, created_at: &str, authored: Option<Option<&str>>| {
            let mut row = sample.clone();
            row.id = id.into();
            row.project_id = "checkout-b".into();
            row.title = title.into();
            row.lineage.root_thread_id = id.into();
            row.created_at = created_at.into();
            row.updated_at = created_at.into();
            row.latest_user_authored_message_at = authored.map(|inner| inner.map(str::to_owned));
            row
        };

        let two_days_ago = t3_core::sidebar::time::format_iso(now - 2 * day_ms);
        let five_days_ago = t3_core::sidebar::time::format_iso(now - 5 * day_ms);
        let mut fixtures = Vec::new();
        let mut senders = Vec::new();
        for (env_id, rows) in [
            (
                "env-a",
                vec![
                    make_row("a-older-authored", "Deploy gateway", &two_days_ago, Some(Some(&two_days_ago))),
                    make_row("a-created", "Deploy gateway", &now_iso, Some(None)),
                    make_row("a-unopened", "Worker queue", &two_days_ago, Some(Some(&two_days_ago))),
                    make_row("a-opened", "Worker queue", &five_days_ago, Some(Some(&five_days_ago))),
                    make_row("a-prefix", "Alpha task", &now_iso, Some(None)),
                ],
            ),
            (
                "env-b",
                vec![
                    make_row("b-mid", "Gamma task", &now_iso, Some(None)),
                    make_row("b-whole", "Beta task", &now_iso, Some(None)),
                ],
            ),
        ] {
            let (client, fixture) = ClientFixture::handle(serde_json::from_value(json!({
                "authenticated": true, "permissions": ["orchestration:read"]
            })).unwrap());
            let mut shell = base_shell.clone();
            let snapshot = shell.snapshot.as_mut().unwrap();
            snapshot.snapshot_sequence = 1;
            snapshot.threads = rows;
            let mut config = base_config.clone();
            config.config.as_mut().unwrap().environment.environment_id = format!("server-{env_id}");
            let (sent, loaded) = tokio::sync::oneshot::channel();
            let mut sent = Some(sent);
            let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
                if *event == crate::app_state::AppEvent::Shell && state.scoped_environment(env_id).is_some() {
                    if let Some(sent) = sent.take() { let _ = sent.send(()); }
                }
            }));
            let install = state.update(cx, |state, _| state.connections.install_fixture(
                env_id, client, shell, config, ConnectionStatus::Connected,
            ));
            senders.push(runtime.spawn(install).await.unwrap());
            loaded.await.unwrap();
            fixtures.push(fixture);
        }
        let mut fix_a = fixtures.remove(0);
        let mut fix_b = fixtures.remove(0);

        state.update(cx, |state, cx| {
            state.selected_thread = None;
            state.set_scoped_project_scope(Vec::new(), cx);
            let env = state.open_history.environment(Some("env-a"), Some("server-env-a")).unwrap();
            state.open_history.claim(&env, "a-opened", now);
        });

        let (pane, cx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            SidebarPane::new(state.clone(), composer, menu, confirm, snooze, window, cx)
        });

        // 1. Subject parity: Some(None) falls back to createdAt; local_opened_at boosts; project title is excluded.
        pane.update(cx, |pane, cx| pane.search.set_query("deploy gateway".into(), &pane.state, cx));
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.search.results, vec![
                ThreadKey::new("env-a", "a-created"),
                ThreadKey::new("env-a", "a-older-authored"),
            ]);
        });
        pane.update(cx, |pane, cx| pane.search.set_query("worker queue".into(), &pane.state, cx));
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.search.results, vec![
                ThreadKey::new("env-a", "a-opened"),
                ThreadKey::new("env-a", "a-unopened"),
            ]);
        });
        pane.update(cx, |pane, cx| pane.search.set_query("shared repo".into(), &pane.state, cx));
        pane.read_with(cx, |pane, _| {
            assert!(pane.search.results.is_empty(), "sidebar search must not match every thread by project title");
        });

        // 2. Cross-env server ranking + nextShownOrder + visibleThreadSearch retention.
        pane.update(cx, |pane, cx| pane.search.set_query("needle".into(), &pane.state, cx));
        cx.run_until_parked();
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        let (_, req_a, reply_a) = fix_a.next_request().await;
        let (_, req_b, reply_b) = fix_b.next_request().await;
        assert_eq!(req_a["query"], "needle");
        assert_eq!(req_b["query"], "needle");
        let mut tasks = pane.update(cx, |pane, _| std::mem::take(&mut pane.search.requests));
        assert_eq!(tasks.len(), 2);
        let task_a = tasks.remove(0);
        let task_b = tasks.remove(0);

        reply_a.send(Ok(json!({
            "matches": [{"threadId": "a-prefix", "snippet": "needles...", "quality": 0.8, "hits": 1}]
        }))).unwrap();
        task_a.await;
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.search.results, vec![ThreadKey::new("env-a", "a-prefix")]);
        });

        reply_b.send(Ok(json!({
            "matches": [
                {"threadId": "b-mid", "snippet": "aneedlez", "quality": 0.3, "hits": 1},
                {"threadId": "b-whole", "snippet": "exact needle", "quality": 1.25, "hits": 5}
            ]
        }))).unwrap();
        task_b.await;
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.search.results, vec![
                ThreadKey::new("env-a", "a-prefix"),
                ThreadKey::new("env-b", "b-whole"),
                ThreadKey::new("env-b", "b-mid"),
            ]);
        });

        // Refining to a related query keeps previous matches during debounce; switching to an unrelated query clears.
        pane.update(cx, |pane, cx| pane.search.set_query("needles".into(), &pane.state, cx));
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.search.results, vec![
                ThreadKey::new("env-a", "a-prefix"),
                ThreadKey::new("env-b", "b-whole"),
                ThreadKey::new("env-b", "b-mid"),
            ]);
        });
        pane.update(cx, |pane, cx| pane.search.set_query("completely-unrelated".into(), &pane.state, cx));
        pane.read_with(cx, |pane, _| {
            assert!(pane.search.results.is_empty(), "unrelated query must clear retained matches immediately");
        });

        state.update(cx, |state, _| state.stop_pump());
        drop(senders);
        cx.cx.update(|cx| { cx.shutdown(); cx.clear_globals(); });
        drop(active_fixture);
        drop(fix_a);
        drop(fix_b);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn active_older_settled_search_hit_selects_thread_and_excludes_unlisted_matches(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, orig_fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });
        let (client, mut fixture) = ClientFixture::handle(serde_json::from_value(json!({
            "authenticated": true, "permissions": ["orchestration:read"]
        })).unwrap());
        let (mut shell, mut config) = state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
        let before = t3_core::sidebar::time::format_iso(100_000);
        let sample = shell.snapshot.as_ref().unwrap().threads[0].clone();
        let mut older_hit = sample.clone();
        older_hit.id = "older-active".into();
        older_hit.project_id = "checkout-b".into();
        older_hit.title = "Unrelated settled row".into();
        older_hit.lineage.root_thread_id = "older-active".into();
        older_hit.settled_override = Some("settled".into());
        older_hit.settled_at = Some(t3_core::sidebar::time::format_iso(50_000));
        shell.snapshot.as_mut().unwrap().older_settled = Some(serde_json::from_value(json!({
            "before": before, "asOfSequence": 1, "counts": [{"projectId": "checkout-b", "count": 2}]
        })).unwrap());
        config.config.as_mut().unwrap().shell_thread_families = true;
        config.config.as_mut().unwrap().settings.hide_tagged_threads = true;
        config.config.as_mut().unwrap().settings.hidden_thread_tags = vec!["automation".into()];
        config.config.as_mut().unwrap().environment.environment_id = "fixture-env".into();

        state.update(cx, |state, cx| {
            state.set_scoped_project_scope(Vec::new(), cx);
            state.install_active_environment(client.clone(), shell.clone(), config.clone(), ConnectionStatus::Connected, cx);
            state.select_thread("menu-thread", cx);
        });
        fixture.take_commands();
        let (sent, loaded) = tokio::sync::oneshot::channel();
        let mut sent = Some(sent);
        let _loaded = state.update(cx, |_, cx| cx.subscribe(&state, move |state, _, event, _| {
            if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("saved-fixture").is_some() {
                if let Some(sent) = sent.take() { let _ = sent.send(()); }
            }
        }));
        let install = state.update(cx, |state, _| state.connections.install_fixture(
            "saved-fixture", client, shell, config, ConnectionStatus::Connected,
        ));
        let events = runtime.spawn(install).await.unwrap();
        loaded.await.unwrap();
        let scope_shared = state.read_with(cx, |state, cx| {
            state
                .project_groups_for(GroupConsumer::Sidebar, cx)
                .into_iter()
                .find(|group| group.member_refs.iter().any(|r| r.project_id == "checkout-b"))
                .unwrap()
                .key
        });
        state.update(cx, |state, cx| state.set_scoped_project_scope(vec![scope_shared], cx));

        let (pane, cx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            SidebarPane::new(state.clone(), composer, menu, confirm, snooze, window, cx)
        });

        pane.update(cx, |pane, cx| pane.search.set_query("deploy".into(), &pane.state, cx));
        cx.run_until_parked();
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        cx.run_until_parked();
        let (_, _, reply) = fixture.next_request().await;
        let tasks = pane.update(cx, |pane, _| std::mem::take(&mut pane.search.requests));
        reply.send(Ok(json!({
            "matches": [
                {"threadId": "older-active", "snippet": "deploy log", "quality": 0.8, "messageId": "msg-42"},
                {"threadId": "other-thread", "snippet": "out-of-scope deploy", "quality": 1.0},
                {"threadId": "deleted-ghost", "snippet": "missing deploy", "quality": 1.2}
            ],
            "snapshotSequence": 1,
            "threads": [older_hit]
        }))).unwrap();
        for task in tasks { task.await; }

        let active_key = ThreadKey::new("saved-fixture", "older-active");
        pane.read_with(cx, |pane, _| {
            assert_eq!(
                pane.search.results,
                vec![active_key.clone()],
                "server matches absent from admitted sidebar threads must be excluded",
            );
            assert_eq!(
                pane.search.content_match(&active_key).and_then(|hit| hit.message_id.as_deref()),
                Some("msg-42"),
            );
        });
        assert_eq!(state.read_with(cx, |state, _| state.selected_thread.clone()), Some("menu-thread".into()));

        pane.update_in(cx, |pane, window, cx| pane.open_search_result(window, cx));
        cx.run_until_parked();

        state.read_with(cx, |state, _| {
            assert_eq!(
                state.selected_thread.as_deref(),
                Some("older-active"),
                "activating an older-settled hit on the active environment must switch selection",
            );
            assert!(state.omitted.mounts.generation_of("search").is_none());
        });

        state.update(cx, |state, _| state.stop_pump());
        drop(events);
        cx.cx.update(|cx| { cx.shutdown(); cx.clear_globals(); });
        drop(orig_fixture);
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn non_active_remote_pr_links_match_and_search_clears_selection_anchors_and_hands_off_find_in_thread(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, active_fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });

        let (mut active_shell, mut active_config) =
            state.read_with(cx, |state, _| (state.shell.clone(), state.server_config.clone()));
        active_config.config.as_mut().unwrap().environment.environment_id = "env-local".into();
        let sample = active_shell.snapshot.as_ref().unwrap().threads[0].clone();
        let mut target_thread = sample.clone();
        target_thread.id = "match-thread".into();
        target_thread.title = "Unrelated thread".into();
        target_thread.lineage.root_thread_id = "match-thread".into();
        active_shell.snapshot.as_mut().unwrap().threads.push(target_thread);

        let (active_client, mut active_env_fixture) = ClientFixture::handle(
            serde_json::from_value(json!({
                "authenticated": true,
                "permissions": ["orchestration:read"]
            }))
            .unwrap(),
        );

        let mut remote_shell = active_shell.clone();
        active_shell.snapshot.as_mut().unwrap().threads[0].title = "Cafe menu".into();
        let mut remote_pr_thread = sample.clone();
        remote_pr_thread.id = "remote-pr-hit".into();
        remote_pr_thread.title = "Background sync".into();
        remote_pr_thread.lineage.root_thread_id = "remote-pr-hit".into();
        remote_pr_thread.pull_requests = serde_json::from_value(json!([{
            "host": "github.com",
            "repository": "ping/t3",
            "number": 22889,
            "url": "https://github.com/ping/t3/pull/22889",
            "source": "manual",
            "linkedAt": "2026-10-24T00:00:00Z",
            "snapshot": {
                "state": "open",
                "title": "Smaller MCP payloads",
                "headBranch": "b",
                "baseBranch": "main",
                "isDraft": false
            }
        }]))
        .unwrap();
        remote_shell.snapshot.as_mut().unwrap().threads = vec![remote_pr_thread];
        let mut remote_config = active_config.clone();
        remote_config.config.as_mut().unwrap().environment.environment_id = "env-remote".into();
        let (remote_client, mut remote_fixture) = ClientFixture::handle(
            serde_json::from_value(json!({
                "authenticated": true,
                "permissions": ["orchestration:read"]
            }))
            .unwrap(),
        );

        let (sent, loaded) = tokio::sync::oneshot::channel();
        let mut sent = Some(sent);
        let _loaded = state.update(cx, |_, cx| {
            cx.subscribe(&state, move |state, _, event, _| {
                if *event == crate::app_state::AppEvent::Shell
                    && state.scoped_environment("saved-local").is_some()
                    && state.scoped_environment("saved-remote").is_some()
                {
                    if let Some(sent) = sent.take() {
                        let _ = sent.send(());
                    }
                }
            })
        });
        let local_install = state.update(cx, |state, _| {
            state.connections.install_fixture(
                "saved-local",
                active_client.clone(),
                active_shell.clone(),
                active_config.clone(),
                ConnectionStatus::Connected,
            )
        });
        let remote_install = state.update(cx, |state, _| {
            state.connections.install_fixture(
                "saved-remote",
                remote_client,
                remote_shell,
                remote_config,
                ConnectionStatus::Connected,
            )
        });
        let local_events = runtime.spawn(local_install).await.unwrap();
        let remote_events = runtime.spawn(remote_install).await.unwrap();
        loaded.await.unwrap();
        state.update(cx, |state, cx| {
            state.install_active_environment(
                active_client,
                active_shell,
                active_config,
                ConnectionStatus::Connected,
                cx,
            );
            state.environments.active = Some("saved-local".into());
            state.select_thread("menu-thread", cx);
        });
        active_env_fixture.take_commands();

        struct SearchAndThread {
            pane: Entity<SidebarPane>,
            thread: Entity<crate::thread_view::ThreadView>,
        }
        impl gpui::Render for SearchAndThread {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .flex()
                    .child(div().w(px(320.)).h_full().child(self.pane.clone()))
                    .child(div().flex_1().h_full().child(self.thread.clone()))
            }
        }

        let (root, cx) = cx.add_window_view(|window, cx| {
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu, confirm, snooze, window, cx));
            let thread = cx.new(|cx| crate::thread_view::ThreadView::new(state.clone(), window, cx));
            SearchAndThread { pane, thread }
        });
        let (pane, thread_view) = root.read_with(cx, |r, _| (r.pane.clone(), r.thread.clone()));

        // 1. Non-active remote summary PR link matches #22889 even though active env is saved-local.
        pane.update(cx, |pane, cx| pane.search.set_query("#22889".into(), &pane.state, cx));
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert!(
                pane.search.results.contains(&ThreadKey::new("saved-remote", "remote-pr-hit")),
                "non-active remote summary PR links must match in search results"
            );
        });

        // 2. Pending state tracks debounce/flight and settled_query lags until search settles.
        pane.update(cx, |pane, cx| pane.search.set_query("cafe".into(), &pane.state, cx));
        pane.read_with(cx, |pane, _| {
            assert!(pane.search.is_pending(), "debouncing search must report pending");
        });
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        cx.run_until_parked();
        let (method, _, reply) = active_env_fixture.next_request().await;
        assert_eq!(method, SEARCH_THREADS);
        let (remote_method, _, remote_reply) = remote_fixture.next_request().await;
        assert_eq!(remote_method, SEARCH_THREADS);
        let tasks = pane.update(cx, |pane, _| std::mem::take(&mut pane.search.requests));
        reply
            .send(Ok(json!({
                "matches": [{
                    "threadId": "match-thread",
                    "source": "user",
                    "snippet": "Cafeteria serves Café",
                    "messageId": "msg-42"
                }],
                "snapshotSequence": 1,
                "threads": []
            })))
            .unwrap();
        remote_reply.send(Ok(json!({ "matches": [], "snapshotSequence": 1, "threads": [] }))).unwrap();
        for task in tasks {
            task.await;
        }
        pane.read_with(cx, |pane, _| {
            assert!(!pane.search.is_pending());
            assert_eq!(pane.search.settled_query(), "cafe");
        });

        // Raw typing ahead of debounce keeps settled_query at "cafe".
        pane.update(cx, |pane, cx| {
            pane.search.set_query("cafe open".into(), &pane.state, cx);
            cx.notify();
        });
        pane.read_with(cx, |pane, _| {
            assert!(pane.search.is_pending());
            assert_eq!(pane.search.settled_query(), "cafe");
        });

        // Multi-selection is active prior to activating the search result.
        let match_key = ThreadKey::new("saved-local", "match-thread");
        state.update(cx, |state, _| {
            state.sidebar.fleet_selection.insert(ThreadKey::new("saved-local", "menu-thread"));
            state.sidebar.fleet_selection.insert(match_key.clone());
        });

        // Pointer hover updates highlighted search row so PressEnter opens the hovered result.
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let match_index = pane
            .read_with(cx, |pane, _| pane.search.results.iter().position(|key| key == &match_key))
            .expect("match-thread must remain visible in settled cafe results");
        let row_center = pane
            .read_with(cx, |pane, _| pane.search.list.bounds_for_item(match_index))
            .expect("hovered search result row must have measured bounds")
            .center();
        cx.simulate_mouse_move(row_center, None::<gpui::MouseButton>, Default::default());
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.search.highlighted_key(), Some(&match_key), "pointer hover must sync search highlight");
        });

        // PressEnter (open_search_result) activates hovered hit: clears selection, anchors target, clears search, and hands off find.
        pane.update_in(cx, |pane, window, cx| pane.open_search_result(window, cx));
        cx.run_until_parked();

        state.read_with(cx, |state, _| {
            assert!(state.sidebar.fleet_selection.is_empty(), "activating result must clear multi-select");
            assert_eq!(state.sidebar.fleet_anchor.as_ref(), Some(&match_key));
            assert_eq!(state.selected_thread.as_deref(), Some("match-thread"));
        });
        pane.read_with(cx, |pane, _| assert!(!pane.search.is_active()));

        let (find_payload, find_reply) = loop {
            let (method, payload, reply) = active_env_fixture.next_request().await;
            if method == t3_core::timeline::SEARCH_THREAD {
                break (payload, reply);
            }
            let _ = reply.send(Ok(json!({})));
        };
        assert_eq!(find_payload["threadId"], "match-thread");
        assert_eq!(find_payload["query"], "Café", "find handoff must use settled query + snippet casing");
        assert_eq!(find_payload["start"], json!({ "entryId": "msg-42", "occurrence": 0 }));
        let _ = find_reply.send(Ok(json!({ "totalMatches": 1, "activeIndex": 0, "navigation": [] })));
        cx.run_until_parked();
        thread_view.read_with(cx, |view, _| {
            assert_eq!(view.find_query_for_test(), Some("Café"));
        });

        // Switching to a different thread closes find.
        state.update(cx, |state, cx| state.select_thread("menu-thread", cx));
        cx.run_until_parked();
        thread_view.read_with(cx, |view, _| {
            assert_eq!(view.find_query_for_test(), None);
        });

        // Starting a draft clears any queued pending_thread_find.
        let draft_project = state.read_with(cx, |state, _| state.sidebar.grouping_projects[0].project.id.clone());
        state.update(cx, |state, cx| {
            state.queue_thread_find(
                match_key.clone(),
                t3_core::timeline::PendingThreadFind { query: "Café".into(), start: None },
                cx,
            );
            state.start_draft(&draft_project, cx);
        });
        state.update(cx, |state, _| {
            assert!(state.take_pending_thread_find("match-thread").is_none(), "start_draft must clear pending_thread_find");
        });

        state.update(cx, |state, _| state.stop_pump());
        drop(local_events);
        drop(remote_events);
        cx.cx.update(|cx| { cx.shutdown(); cx.clear_globals(); });
        drop(active_fixture);
        drop(active_env_fixture);
        drop(remote_fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[test]
    fn search_capture_frames() {
        let Some(out) = std::env::var_os("T3_SEARCH_CAPTURE") else { return };
        let out = std::path::PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut cx = gpui::HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(),
            std::sync::Arc::new(crate::icons::Assets),
            gpui_platform::current_headless_renderer,
        );
        cx.allow_parking();
        let (state, active_fixture) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(installed.0.clone(), cx);
            crate::updates::init(cx);
            installed
        });

        let (mut local_shell, mut local_config) =
            state.read_with(&cx, |state, _| (state.shell.clone(), state.server_config.clone()));
        local_config.config.as_mut().unwrap().environment.environment_id = "env-local".into();
        let sample = local_shell.snapshot.as_ref().unwrap().threads[0].clone();
        let mut active_title_hit = sample.clone();
        active_title_hit.id = "active-title".into();
        active_title_hit.title = "Cafe espresso layout".into();
        active_title_hit.lineage.root_thread_id = "active-title".into();
        active_title_hit.latest_user_message_at = Some("2026-11-01T10:00:00Z".into());
        active_title_hit.pull_requests = serde_json::from_value(json!([{
            "host": "github.com",
            "repository": "ping/t3",
            "number": 22889,
            "url": "https://github.com/ping/t3/pull/22889",
            "source": "manual",
            "linkedAt": "2026-10-24T00:00:00Z",
            "snapshot": {
                "state": "open",
                "title": "Cafe PR badge check",
                "headBranch": "feat/cafe",
                "baseBranch": "main",
                "isDraft": false
            }
        }]))
        .unwrap();

        let mut regen_hit = sample.clone();
        regen_hit.id = "regen-hit".into();
        regen_hit.title = "Cafe draft regeneration...".into();
        regen_hit.title_regeneration = Some(json!({ "status": "running" }));
        regen_hit.lineage.root_thread_id = "regen-hit".into();
        regen_hit.latest_user_message_at = Some("2026-11-01T09:00:00Z".into());
        regen_hit.pull_requests = serde_json::from_value(json!([
            {
                "host": "github.com",
                "repository": "ping/t3",
                "number": 22890,
                "url": "https://github.com/ping/t3/pull/22890",
                "source": "manual",
                "linkedAt": "2026-10-23T00:00:00Z",
                "snapshot": {
                    "state": "merged",
                    "title": "Cafe stack base",
                    "headBranch": "feat/cafe-base",
                    "baseBranch": "main",
                    "isDraft": false
                }
            },
            {
                "host": "github.com",
                "repository": "ping/t3",
                "number": 22891,
                "url": "https://github.com/ping/t3/pull/22891",
                "source": "manual",
                "linkedAt": "2026-10-24T00:00:00Z",
                "snapshot": {
                    "state": "open",
                    "title": "Cafe stack top",
                    "headBranch": "feat/cafe-top",
                    "baseBranch": "feat/cafe-base",
                    "isDraft": false
                }
            }
        ]))
        .unwrap();

        let mut settled_hit = sample.clone();
        settled_hit.id = "settled-hit".into();
        settled_hit.title = "Settled cafe thread".into();
        settled_hit.settled_override = Some("settled".into());
        settled_hit.last_visited_at = Some(Some("2026-11-01T08:00:00Z".into()));
        settled_hit.updated_at = "2026-10-15T08:00:00Z".into();
        settled_hit.latest_user_message_at = Some("2026-10-15T08:00:00Z".into());
        settled_hit.lineage.root_thread_id = "settled-hit".into();
        settled_hit.pull_requests = serde_json::from_value(json!([{
            "host": "github.com",
            "repository": "ping/t3",
            "number": 22888,
            "url": "https://github.com/ping/t3/pull/22888",
            "source": "manual",
            "linkedAt": "2026-10-15T00:00:00Z",
            "snapshot": {
                "state": "merged",
                "title": "Cafe settled PR badge",
                "headBranch": "fix/settled",
                "baseBranch": "main",
                "isDraft": false
            }
        }]))
        .unwrap();

        let mut user_msg_hit = sample.clone();
        user_msg_hit.id = "user-msg-hit".into();
        user_msg_hit.title = "Investigate search ranking".into();
        user_msg_hit.lineage.root_thread_id = "user-msg-hit".into();
        user_msg_hit.latest_user_message_at = Some("2026-11-01T11:30:00Z".into());

        let mut agent_msg_hit = sample.clone();
        agent_msg_hit.id = "agent-msg-hit".into();
        agent_msg_hit.title = "Refactor excerpt truncation".into();
        agent_msg_hit.last_visited_at = Some(Some("2026-11-01T06:00:00Z".into()));
        agent_msg_hit.updated_at = "2026-10-30T06:00:00Z".into();
        agent_msg_hit.latest_user_message_at = Some("2026-10-30T06:00:00Z".into());
        agent_msg_hit.lineage.root_thread_id = "agent-msg-hit".into();

        local_shell.snapshot.as_mut().unwrap().threads =
            vec![active_title_hit, regen_hit, settled_hit, user_msg_hit, agent_msg_hit];
        let (local_client, mut local_fixture) = ClientFixture::handle(
            serde_json::from_value(json!({ "authenticated": true, "permissions": ["orchestration:read"] })).unwrap(),
        );
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let mut ready_tx = Some(ready_tx);
        let _sub = state.update(&mut cx, |_, cx| {
            cx.subscribe(&state, move |state, _, event, _| {
                if *event == crate::app_state::AppEvent::Shell && state.scoped_environment("saved-local").is_some() {
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(());
                    }
                }
            })
        });
        let install = state.update(&mut cx, |state, _| {
            state.connections.install_fixture(
                "saved-local",
                local_client.clone(),
                local_shell.clone(),
                local_config.clone(),
                ConnectionStatus::Connected,
            )
        });
        let _events = runtime.block_on(install);
        cx.update(|cx| cx.foreground_executor().clone()).block_test(ready_rx).unwrap();
        state.update(&mut cx, |state, cx| {
            state.install_active_environment(local_client, local_shell, local_config, ConnectionStatus::Connected, cx);
            state.environments.active = Some("saved-local".into());
            state.sidebar.now_override = Some(1_793_534_400_000);
            state.select_thread("active-title", cx);
        });
        local_fixture.take_commands();

        struct CaptureFrame {
            pane: Entity<SidebarPane>,
        }
        impl gpui::Render for CaptureFrame {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .flex()
                    .bg(hsla(palette(cx).background))
                    .child(div().w(px(320.)).h_full().child(self.pane.clone()))
                    .child(div().flex_1().h_full().border_l_1().border_color(hsla(palette(cx).border)))
            }
        }
        let mut pane_handle = None;
        let window = cx
            .open_window(gpui::size(px(640.), px(560.)), |window, cx| {
                let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
                let dialog = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
                let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
                let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
                let pane = cx.new(|cx| SidebarPane::new(state.clone(), composer, menu, dialog, snooze, window, cx));
                pane_handle = Some(pane.clone());
                cx.new(|_| CaptureFrame { pane })
            })
            .unwrap();
        let pane = pane_handle.unwrap();
        let capture = |cx: &mut gpui::HeadlessAppContext, name: &str| {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear();
            })
            .unwrap();
            cx.capture_screenshot(window.into()).unwrap().save(out.join(format!("{name}.png"))).unwrap();
        };

        let set_search = |cx: &mut gpui::HeadlessAppContext, q: &str| {
            let owned = q.to_owned();
            cx.update_window(window.into(), |_, window, cx| {
                pane.update(cx, |pane, cx| {
                    pane.search_input.update(cx, |input, cx| input.set_value(owned.clone(), window, cx));
                    pane.search.set_query(owned, &pane.state, cx);
                    cx.notify();
                });
            })
            .unwrap();
            cx.run_until_parked();
        };

        let set_dark = |cx: &mut gpui::HeadlessAppContext, dark: bool| {
            cx.update(|cx| {
                let mut preferences = crate::theme::preferences(cx).clone();
                let mode = if dark {
                    t3_core::client_settings::AppearanceMode::Dark
                } else {
                    t3_core::client_settings::AppearanceMode::Light
                };
                preferences.theme = t3_core::client_settings::ThemePreference::Default(mode);
                preferences.mode = mode;
                crate::theme::set_preferences(preferences, cx);
            });
        };
        set_dark(&mut cx, true);

        // 1. Empty state ("No threads found").
        set_search(&mut cx, "zzzzzz");
        cx.advance_clock(SEARCH_DEBOUNCE);
        cx.run_until_parked();
        let (_, _, reply) = runtime.block_on(local_fixture.next_request());
        let tasks = pane.update(&mut cx, |pane, _| std::mem::take(&mut pane.search.requests));
        reply.send(Ok(json!({ "matches": [], "snapshotSequence": 1, "threads": [] }))).unwrap();
        let exec = cx.update(|cx| cx.foreground_executor().clone());
        for task in tasks {
            exec.block_test(task);
        }
        capture(&mut cx, "dark-empty");

        // 2. Pending state ("Searching thread messages…").
        set_search(&mut cx, "qqq");
        capture(&mut cx, "dark-pending");

        // 3. Settled mixed results (title + PR badge, regenerating title, settled receding, You:/Agent: excerpts).
        set_search(&mut cx, "cafe");
        cx.advance_clock(SEARCH_DEBOUNCE);
        cx.run_until_parked();
        let (_, _, reply) = runtime.block_on(local_fixture.next_request());
        let tasks = pane.update(&mut cx, |pane, _| std::mem::take(&mut pane.search.requests));
        reply
            .send(Ok(json!({
                "matches": [
                    { "threadId": "user-msg-hit", "source": "user", "snippet": "Cafeteria serves Café espresso blends", "quality": 1.1, "messageId": "m-u" },
                    { "threadId": "agent-msg-hit", "source": "assistant", "snippet": "Updated Café sidebar excerpt layout for long strings", "quality": 0.9, "messageId": "m-a" }
                ],
                "snapshotSequence": 1,
                "threads": []
            })))
            .unwrap();
        for task in tasks {
            exec.block_test(task);
        }
        pane.update(&mut cx, |pane, cx| {
            pane.search.move_highlight(1);
            cx.notify();
        });
        for dark in [true, false] {
            set_dark(&mut cx, dark);
            capture(&mut cx, if dark { "dark-results" } else { "light-results" });
        }

        state.update(&mut cx, |state, _| state.stop_pump());
        drop(_events);
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(active_fixture);
        drop(local_fixture);
        drop(runtime);
        cx.run_until_parked();
    }
}
