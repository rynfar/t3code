//! Issues page (`apps/web/src/components/issue/IssuesPage.tsx`): multi-tracker virtualized list,
//! debounced search, filters (state, involvement, project, tracker/host, labels, assignee),
//! named saved views, per-tracker cursor continuation, retry-failed-only for environments and
//! candidate repositories, manual AZDO refresh (`refresh: true`), and row selection / quick actions.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight,
    IntoElement, ListAlignment, ListState, ParentElement, Render, Styled, Subscription, Task,
    Window, WindowControlArea, div, list, prelude::*, px,
};
use gpui_component::h_flex;
use gpui_component::input::{InputEvent, InputState};
use serde_json::Value;
use t3_client::store;
use t3_client::supervisor::ConnectionStatus;
use t3_core::issues::list::{
    EnvironmentIssueEntry, FilterProject, HeldIssuePage, HostOption, IssueCandidatesTracker,
    IssueListPreferences, IssueListReadTracker, IssueListRefreshMode, IssueTrackerGroup, PAGE_SIZE,
    PREFERENCES_KEY, ProjectDescriptor, SEARCH_DEBOUNCE_MS, continue_issue_page,
    filter_held_by_host, group_issues, hold_issue_pages, host_options_for, issue_cursors_for_host,
    issue_filter_projects, issue_filter_scopes,
};
use t3_core::issues::saved_views::{
    ISSUE_SAVED_VIEW_PLACEHOLDER, ISSUE_SAVED_VIEWS_KEY, SavedView, create_saved_view,
    decode_saved_views, delete_saved_view, issue_view_matches, rename_saved_view,
    resolve_selected_view, update_saved_view_preferences,
};
use t3_core::issues::wire::{
    IssueCandidates, IssueListResult, IssueRef, IssueTrackerSummary, decode, method,
};
use t3_core::right_panel::{ISSUES_PANEL_THREAD_KEY, IssueTarget};
use t3_core::sidebar::grouping::EnvironmentOwner;

use super::detail::IssueDetailPanel;
use super::preview::IssueLinkPreviewController;
use super::{
    HeldIssueKey, ReadFailure, hold_issue_seed, read_for_owner, settle,
};
use crate::app_state::{AppEvent, AppState};
use crate::i18n::t;
use crate::pull_requests::controls::{Button, Size, Variant};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

pub(super) const ROW_HEIGHT: f32 = 56.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Row {
    TrackerHeader(usize),
    TrackerNotice(usize),
    ProjectError(usize, usize),
    GroupEmpty(usize),
    Entry(usize, usize),
    LoadMore(usize, usize),
}

pub(super) struct SavedViewEditor {
    pub(super) editing_id: Option<String>,
    pub(super) input: Entity<InputState>,
    _subscription: Subscription,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvironmentTarget {
    /// Saved environment ID (consistent with `EnvironmentOwner.saved_id` and `ProjectRef`).
    env_id: String,
    /// Canonical server `environmentId`, when known, so saved/URL preferences matching either
    /// saved or canonical ID resolve cleanly.
    canonical_id: Option<String>,
    /// `Some` when backed by `state.fleet.environments`, `None` for single-client fallback.
    owner: Option<EnvironmentOwner>,
}

impl EnvironmentTarget {
    fn matches_id(&self, id: &str) -> bool {
        self.env_id == id || self.canonical_id.as_deref() == Some(id)
    }
}

pub struct IssuesPage {
    pub(super) state: Entity<AppState>,
    pub(super) prefs: IssueListPreferences,
    pub(super) search: Entity<InputState>,
    pub(super) typed: String,
    pub(super) sent: String,
    pub(super) read_tracker: IssueListReadTracker,
    pub(super) held_pages: BTreeMap<String, HeldIssuePage>,
    pub(super) groups: Vec<IssueTrackerGroup>,
    pub(super) rows: Vec<Row>,
    pub(super) list: ListState,
    pub(super) selected: Option<IssueRef>,
    pub(super) selected_entry: Option<EnvironmentIssueEntry>,
    detail: Option<Entity<IssueDetailPanel>>,
    pub(super) preview: IssueLinkPreviewController,
    pub(super) loading_more: Option<String>,
    loading_more_gen: u64,
    pub(super) unsupported: bool,
    pub(super) candidates_tracker: IssueCandidatesTracker,
    pub(super) saved_views: Vec<SavedView<IssueListPreferences>>,
    pub(super) selected_view_id: Option<String>,
    saved_view_editor: Option<SavedViewEditor>,
    next_view_seq: u64,
    focus: FocusHandle,
    connected_targets: Vec<String>,
    open: bool,
    _debounce: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for IssuesPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl IssuesPage {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prefs: IssueListPreferences = store::load_ui_state(PREFERENCES_KEY)
            .and_then(|value| serde_json::from_value::<IssueListPreferences>(value).ok())
            .map(|p| p.normalized())
            .unwrap_or_default();
        let saved_views: Vec<SavedView<IssueListPreferences>> =
            decode_saved_views(store::load_ui_state(ISSUE_SAVED_VIEWS_KEY));
        let typed = prefs.q.clone().unwrap_or_default();
        let placeholder = t(cx, "Search issues").to_owned();
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(typed.clone())
        });
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => this.typed_changed(input.read(cx).value().to_string(), cx),
                InputEvent::PressEnter { .. } => this.submit_query(cx),
                _ => {}
            }),
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
                if matches!(
                    event,
                    AppEvent::Phase | AppEvent::Selection | AppEvent::Shell | AppEvent::ServerConfig
                ) {
                    this.sync(cx);
                } else if matches!(event, AppEvent::RightPanel) {
                    this.sync_detail_from_panel(cx);
                }
            }),
        ];
        let selected_view_id =
            resolve_selected_view(&saved_views, None, &prefs, issue_view_matches)
                .map(|view| view.id.clone());
        let mut page = Self {
            state,
            prefs,
            search,
            sent: typed.trim().to_owned(),
            typed,
            read_tracker: IssueListReadTracker::default(),
            held_pages: BTreeMap::new(),
            groups: Vec::new(),
            rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(800.)),
            selected: None,
            selected_entry: None,
            detail: None,
            preview: IssueLinkPreviewController::default(),
            loading_more: None,
            loading_more_gen: 0,
            unsupported: false,
            candidates_tracker: IssueCandidatesTracker::default(),
            saved_views,
            selected_view_id,
            saved_view_editor: None,
            next_view_seq: 1,
            focus: cx.focus_handle(),
            connected_targets: Vec::new(),
            open: false,
            _debounce: None,
            _subscriptions: subscriptions,
        };
        page.sync(cx);
        page
    }

    fn is_environment_disabled(state: &AppState, saved_id: &str) -> bool {
        state
            .environments
            .environments
            .iter()
            .any(|record| record.id == saved_id && record.disabled)
    }

    /// All connected, non-disabled environment targets across the fleet, falling back to the
    /// active single-client environment when `state.fleet.environments` is empty.
    fn all_environment_targets(&self, cx: &App) -> Vec<EnvironmentTarget> {
        let state = self.state.read(cx);
        if !state.fleet.environments.is_empty() {
            let mut targets = Vec::new();
            for env in &state.fleet.environments {
                let saved_id = &env.summary.owner.saved_id;
                if Self::is_environment_disabled(state, saved_id) {
                    continue;
                }
                if env.status != ConnectionStatus::Connected {
                    continue;
                }
                let canonical_id = env
                    .summary
                    .owner
                    .canonical_id
                    .clone()
                    .or_else(|| {
                        env.config
                            .config
                            .as_ref()
                            .map(|c| c.environment.environment_id.clone())
                            .filter(|id| !id.is_empty())
                    });
                targets.push(EnvironmentTarget {
                    env_id: saved_id.clone(),
                    canonical_id,
                    owner: Some(env.summary.owner.clone()),
                });
            }
            return targets;
        }
        let active_connected = state.client_handle().is_some()
            && matches!(
                state.phase,
                crate::app_state::Phase::Online(ConnectionStatus::Connected)
            );
        let fallback_id = state
            .environments
            .active
            .as_deref()
            .or_else(|| state.environment())
            .map(str::to_owned);
        let Some(env_id) = fallback_id.filter(|id| !Self::is_environment_disabled(state, id))
        else {
            return Vec::new();
        };
        if !active_connected {
            return Vec::new();
        }
        let canonical_id = state
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.environment_id.clone())
            .filter(|id| !id.is_empty());
        vec![EnvironmentTarget {
            env_id,
            canonical_id,
            owner: None,
        }]
    }

    fn environment_target_for(&self, env_id: &str, cx: &App) -> Option<EnvironmentTarget> {
        self.all_environment_targets(cx)
            .into_iter()
            .find(|target| target.matches_id(env_id))
    }

    fn environment_labels(&self, cx: &App) -> HashMap<String, String> {
        let state = self.state.read(cx);
        let mut map = HashMap::new();
        for env in &state.environments.environments {
            map.insert(env.id.clone(), env.label.clone());
            if let Some(canonical) = &env.environment_id {
                map.insert(canonical.clone(), env.label.clone());
            }
        }
        for env in &state.fleet.environments {
            let label = env
                .config
                .config
                .as_ref()
                .map(|c| c.environment.label.clone())
                .filter(|l| !l.trim().is_empty())
                .or_else(|| map.get(&env.summary.owner.saved_id).cloned())
                .unwrap_or_else(|| env.summary.owner.saved_id.clone());
            map.insert(env.summary.owner.saved_id.clone(), label.clone());
            if let Some(canonical) = &env.summary.owner.canonical_id {
                map.insert(canonical.clone(), label.clone());
            }
            if let Some(config) = &env.config.config
                && !config.environment.environment_id.is_empty()
            {
                map.insert(config.environment.environment_id.clone(), label);
            }
        }
        if let Some(config) = &state.server_config.config
            && !config.environment.environment_id.is_empty()
        {
            map.insert(
                config.environment.environment_id.clone(),
                config.environment.label.clone(),
            );
            if let Some(active) = &state.environments.active {
                map.entry(active.clone())
                    .or_insert_with(|| config.environment.label.clone());
            }
        }
        map
    }

    pub(super) fn environment_label_for_row(&self, env_id: &str, cx: &App) -> Option<String> {
        if self.all_environment_targets(cx).len() <= 1 && self.held_pages.len() <= 1 {
            return None;
        }
        self.environment_labels(cx).get(env_id).cloned()
    }

    fn project_descriptors(&self, cx: &App) -> Vec<ProjectDescriptor> {
        let state = self.state.read(cx);
        if !state.fleet.environments.is_empty() {
            let mut out = Vec::new();
            for env in &state.fleet.environments {
                let saved_id = &env.summary.owner.saved_id;
                if Self::is_environment_disabled(state, saved_id) {
                    continue;
                }
                for scoped in &env.summary.projects {
                    if scoped.project.archived_at.is_some() {
                        continue;
                    }
                    out.push(ProjectDescriptor::from_repository_identity(
                        saved_id.clone(),
                        scoped.project.id.clone(),
                        scoped.project.title.clone(),
                        scoped.is_scratch,
                        scoped.project.repository_identity.as_ref(),
                    ));
                }
            }
            return out;
        }
        let env_id = state
            .environments
            .active
            .as_deref()
            .or_else(|| state.environment())
            .unwrap_or("local")
            .to_owned();
        let Some(snapshot) = &state.shell.snapshot else {
            return Vec::new();
        };
        snapshot
            .projects
            .iter()
            .filter(|project| project.archived_at.is_none())
            .map(|project| {
                ProjectDescriptor::from_repository_identity(
                    env_id.clone(),
                    project.id.clone(),
                    project.title.clone(),
                    state.is_scratch_project(&project.id),
                    project.repository_identity.as_ref(),
                )
            })
            .collect()
    }

    pub(super) fn filter_projects(&self, cx: &App) -> Vec<FilterProject> {
        let descriptors = self.project_descriptors(cx);
        let labels = self.environment_labels(cx);
        issue_filter_projects(&descriptors, &labels)
    }

    pub(super) fn host_options(&self) -> Vec<HostOption> {
        let trackers: Vec<IssueTrackerSummary> = self
            .held_pages
            .values()
            .flat_map(|page| page.answer.trackers.iter().cloned())
            .collect();
        host_options_for(&trackers, self.prefs.host.as_deref())
    }

    pub(super) fn is_pending(&self) -> bool {
        self.read_tracker.is_pending()
    }

    fn active_targets(&self, cx: &App) -> Vec<EnvironmentTarget> {
        let targets = self.all_environment_targets(cx);
        if let Some(scoped) = self.prefs.scoped_project() {
            targets
                .into_iter()
                .filter(|target| target.matches_id(&scoped.environment_id))
                .collect()
        } else {
            targets
        }
    }

    pub(super) fn canonical_scoped_project(
        &self,
        cx: &App,
    ) -> Option<t3_core::issues::list::ScopedProject> {
        let mut scoped = self.prefs.scoped_project()?;
        if let Some(target) = self.environment_target_for(&scoped.environment_id, cx) {
            scoped.environment_id = target.env_id;
        }
        Some(scoped)
    }

    fn search_scope_key(&self) -> String {
        let normalized = self.prefs.normalized();
        format!(
            "{}|{}",
            serde_json::to_string(&normalized).unwrap_or_default(),
            self.sent
        )
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let open = state.issues_open && !state.pull_requests_open;
        let targets = self.active_targets(cx);
        let target_ids: Vec<String> = targets.iter().map(|t| t.env_id.clone()).collect();
        let targets_changed = target_ids != self.connected_targets;
        let has_targets = !target_ids.is_empty();
        let reopened = open && !self.open;
        let left = self.open && !open;

        self.open = open;
        self.connected_targets = target_ids;

        if left {
            // Closed searches do not accumulate in memory once the page closes (`pingdotgg/t3code#38493`).
            self._debounce = None;
            self.read_tracker.prune_closed_searches();
            self.close_saved_view_editor(cx);
        }

        let should_fetch = (has_targets
            && (((reopened || targets_changed) && self.read_tracker.should_live_refresh())
                || (self.held_pages.is_empty() && !self.is_pending())))
            || (!has_targets && targets_changed);
        if open && should_fetch {
            self.fetch_all(IssueListRefreshMode::Background, cx);
        } else if open {
            self.rebuild(cx);
        }
        cx.notify();
    }

    fn save_prefs(&self) {
        store::save_ui_state(PREFERENCES_KEY, &self.prefs.normalized());
    }

    fn save_views(&self) {
        store::save_ui_state(ISSUE_SAVED_VIEWS_KEY, &self.saved_views);
    }

    pub(super) fn update_prefs(
        &mut self,
        change: impl FnOnce(&mut IssueListPreferences),
        cx: &mut Context<Self>,
    ) {
        let before = self.search_scope_key();
        change(&mut self.prefs);
        self.prefs = self.prefs.normalized();
        self.save_prefs();
        if self.search_scope_key() != before {
            self._debounce = None;
            self.sent = self.typed.trim().to_owned();
            self.candidates_tracker.clear();
            self.fetch_all(IssueListRefreshMode::Background, cx);
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
        let trimmed = value.trim();
        self.prefs.q = (!trimmed.is_empty()).then(|| trimmed.chars().take(200).collect());
        self.save_prefs();
        self.rebuild(cx);
        let next_query = trimmed.to_owned();
        self._debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SEARCH_DEBOUNCE_MS))
                .await;
            let _ = this.update(cx, |this, cx| this.send_query(next_query, cx));
        }));
        cx.notify();
    }

    fn submit_query(&mut self, cx: &mut Context<Self>) {
        self._debounce = None;
        let query = self.typed.trim().to_owned();
        self.send_query(query, cx);
    }

    fn send_query(&mut self, query: String, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        if query != self.sent {
            self.sent = query;
            self.fetch_all(IssueListRefreshMode::Background, cx);
        }
    }

    pub(super) fn clear_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.typed_changed(String::new(), cx);
        self.submit_query(cx);
    }

    /// Explicit refresh: passes `refresh: true` so Azure DevOps re-reads each project's
    /// finished states and tracker metadata (`pingdotgg/t3code#38986`).
    pub(super) fn manual_refresh(&mut self, cx: &mut Context<Self>) {
        self.candidates_tracker.clear();
        self.fetch_all(IssueListRefreshMode::Manual, cx);
    }

    /// Retries ONLY environments whose read failed, never restarting already-loaded or
    /// still-in-flight environments (`pingdotgg/t3code#38654`).
    pub(super) fn retry_failed(&mut self, cx: &mut Context<Self>) {
        let failed = self.read_tracker.failed_environments();
        if failed.is_empty() {
            if self.held_pages.is_empty() && !self.is_pending() {
                self.fetch_all(IssueListRefreshMode::Background, cx);
            }
            return;
        }
        let search_key = self.search_scope_key();
        let active = self.active_targets(cx);
        for env_id in failed {
            let target = active
                .iter()
                .find(|t| t.matches_id(&env_id))
                .cloned()
                .unwrap_or(EnvironmentTarget {
                    env_id,
                    canonical_id: None,
                    owner: None,
                });
            self.fetch_environment(
                target,
                search_key.clone(),
                IssueListRefreshMode::Background,
                cx,
            );
        }
    }

    fn fetch_all(&mut self, mode: IssueListRefreshMode, cx: &mut Context<Self>) {
        let targets = self.active_targets(cx);
        let target_ids: Vec<String> = targets.iter().map(|t| t.env_id.clone()).collect();
        let search_key = self.search_scope_key();
        self.loading_more = None;
        self.loading_more_gen += 1;
        self.read_tracker
            .set_search_key(search_key.clone(), &target_ids);
        self.unsupported = false;
        if targets.is_empty() {
            self.held_pages.clear();
            self.rebuild(cx);
            return;
        }
        for target in targets {
            self.fetch_environment(target, search_key.clone(), mode, cx);
        }
    }

    fn fetch_environment(
        &mut self,
        target: EnvironmentTarget,
        search_key: String,
        mode: IssueListRefreshMode,
        cx: &mut Context<Self>,
    ) {
        let env_id = target.env_id;
        let (generation, send_refresh) = self.read_tracker.begin_read(&env_id, mode);
        let input = self
            .prefs
            .to_list_input(PAGE_SIZE, &self.sent, None, send_refresh);
        let payload =
            serde_json::to_value(&input).unwrap_or_else(|_| Value::Object(Default::default()));
        let task = read_for_owner(
            self.state.read(cx),
            target.owner.as_ref(),
            method::LIST,
            payload,
            decode::<IssueListResult>,
        );
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |page, cx| {
                page.apply_environment_result(env_id, search_key, generation, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn apply_environment_result(
        &mut self,
        env_id: String,
        search_key: String,
        generation: u64,
        result: Result<IssueListResult, ReadFailure>,
        cx: &mut Context<Self>,
    ) {
        if search_key != self.search_scope_key() {
            return;
        }
        match result {
            Ok(answer) => {
                self.unsupported = false;
                if self.read_tracker.finish_read(&env_id, generation, Ok(answer)) {
                    let answers = self.read_tracker.answers();
                    self.held_pages = hold_issue_pages(&answers, &self.held_pages);
                    self.rebuild(cx);
                }
            }
            Err(failure) => {
                if failure.unsupported {
                    self.unsupported = true;
                }
                if self
                    .read_tracker
                    .finish_read(&env_id, generation, Err(failure.message))
                {
                    let answers = self.read_tracker.answers();
                    self.held_pages = hold_issue_pages(&answers, &self.held_pages);
                    self.rebuild(cx);
                }
            }
        }
        cx.notify();
    }

    /// Loads the next cursor page for one tracker host on one environment (`loadMore`).
    pub(super) fn load_more(
        &mut self,
        env_id: String,
        host: String,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = self.held_pages.get(&env_id).cloned() else {
            return;
        };
        let cursors = issue_cursors_for_host(&page.next_cursors, &host);
        if cursors.is_empty() || self.loading_more.is_some() {
            return;
        }
        let search_key = self.search_scope_key();
        self.loading_more_gen += 1;
        let load_gen = self.loading_more_gen;
        self.loading_more = Some(format!("{env_id} {host}"));
        let owner = self
            .environment_target_for(&env_id, cx)
            .and_then(|target| target.owner);
        let input = self
            .prefs
            .to_list_input(PAGE_SIZE, &self.sent, Some(cursors.clone()), false);
        let payload =
            serde_json::to_value(&input).unwrap_or_else(|_| Value::Object(Default::default()));
        let task = read_for_owner(
            self.state.read(cx),
            owner.as_ref(),
            method::LIST,
            payload,
            decode::<IssueListResult>,
        );
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                if this.loading_more_gen != load_gen {
                    return;
                }
                this.loading_more = None;
                if search_key != this.search_scope_key() {
                    return;
                }
                if let Some(current) = this.held_pages.get(&env_id).cloned() {
                    let next = match result {
                        Ok(ref answer) => Ok(answer),
                        Err(failure) => Err(failure.message),
                    };
                    let updated = continue_issue_page(&current, next, &cursors);
                    this.held_pages.insert(env_id, updated);
                    this.rebuild(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Fetches `issues.candidates` lazily when the Filters menu opens.
    pub(super) fn ensure_candidates(&mut self, cx: &mut Context<Self>) {
        let projects = self.project_descriptors(cx);
        let shown = filter_held_by_host(&self.held_pages, self.prefs.host.as_deref());
        let scoped = self.canonical_scoped_project(cx);
        let scopes = issue_filter_scopes(&shown, &projects, scoped.as_ref());
        let to_fetch = self.candidates_tracker.sync_targets(scopes);
        for target in to_fetch {
            self.fetch_candidate_target(target, cx);
        }
    }

    /// Retries ONLY failed candidate scopes without reloading answered or in-flight scopes
    /// (`pingdotgg/t3code#38423`).
    pub(super) fn retry_failed_candidates(&mut self, cx: &mut Context<Self>) {
        let failed = self.candidates_tracker.failed_targets();
        for target in failed {
            self.fetch_candidate_target(target, cx);
        }
    }

    fn fetch_candidate_target(
        &mut self,
        target: t3_core::issues::list::IssueCandidatesTarget,
        cx: &mut Context<Self>,
    ) {
        let target_key = target.key();
        let generation = self.candidates_tracker.begin_read(&target);
        let owner = self
            .environment_target_for(&target.environment_id, cx)
            .and_then(|env| env.owner);
        let payload = target.input.payload();
        let task = read_for_owner(
            self.state.read(cx),
            owner.as_ref(),
            method::CANDIDATES,
            payload,
            decode::<IssueCandidates>,
        );
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |page, cx| {
                let outcome = result.map_err(|failure| failure.message);
                if page
                    .candidates_tracker
                    .finish_read(&target_key, generation, outcome)
                {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn rebuild(&mut self, cx: &mut Context<Self>) {
        let projects = self.project_descriptors(cx);
        let shown = filter_held_by_host(&self.held_pages, self.prefs.host.as_deref());
        self.groups = group_issues(&shown, &self.sent, &projects, self.prefs.effective_sort());

        let mut rows = Vec::new();
        for (group_idx, group) in self.groups.iter().enumerate() {
            rows.push(Row::TrackerHeader(group_idx));
            if !group.tracker.configured {
                rows.push(Row::TrackerNotice(group_idx));
            }
            for err_idx in 0..group.errors.len() {
                rows.push(Row::ProjectError(group_idx, err_idx));
            }
            if group.tracker.configured && group.rows.is_empty() && group.errors.is_empty() {
                rows.push(Row::GroupEmpty(group_idx));
            }
            for row_idx in 0..group.rows.len() {
                rows.push(Row::Entry(group_idx, row_idx));
            }
            for env_idx in 0..group.more.len() {
                rows.push(Row::LoadMore(group_idx, env_idx));
            }
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

    fn open_detail_pane(
        &mut self,
        env_id: Option<String>,
        reference: IssueRef,
        entry: Option<EnvironmentIssueEntry>,
        cx: &mut Context<Self>,
    ) {
        let seed = entry.as_ref().map(|e| e.entry.clone());
        if let Some(detail) = &self.detail {
            detail.update(cx, |panel, cx| {
                panel.retarget(env_id, reference.clone(), seed, cx);
            });
        } else {
            let page_weak = cx.entity().downgrade();
            let close_weak = page_weak.clone();
            let on_close = std::rc::Rc::new(move |cx: &mut App| {
                let _ = close_weak.update(cx, |page, cx| page.clear_selection(cx));
            });
            let on_mutated = std::rc::Rc::new(move |cx: &mut App| {
                let _ = page_weak.update(cx, |page, cx| {
                    page.fetch_all(IssueListRefreshMode::Background, cx);
                });
            });
            let state = self.state.clone();
            let ref_clone = reference.clone();
            self.detail = Some(cx.new(|cx| {
                IssueDetailPanel::new(
                    state,
                    env_id,
                    ref_clone,
                    None,
                    seed,
                    Some(on_close),
                    None,
                    Some(on_mutated),
                    cx,
                )
            }));
        }
        self.selected = Some(reference);
        self.selected_entry = entry;
        cx.notify();
    }

    fn sync_detail_from_panel(&mut self, cx: &mut Context<Self>) {
        let active_target = {
            let state = self.state.read(cx);
            if !state.issues_open {
                return;
            }
            let panel = state.panels.panels.panel(ISSUES_PANEL_THREAD_KEY);
            if !panel.is_open {
                return;
            }
            panel.active().and_then(|surface| match &surface.kind {
                t3_core::right_panel::SurfaceKind::Issue(target) => Some(target.as_ref().clone()),
                _ => None,
            })
        };
        let Some(target) = active_target else {
            return;
        };
        let reference = target.issue_ref();
        let env_id = target.environment_id.clone();
        if self.selected.as_ref() == Some(&reference)
            && self
                .detail
                .as_ref()
                .is_some_and(|panel| panel.read(cx).matches_target(env_id.as_deref(), &reference))
        {
            return;
        }
        let matched = self
            .groups
            .iter()
            .flat_map(|g| g.rows.iter())
            .find(|row| {
                IssueRef::of(&row.entry) == reference
                    && (env_id.is_none() || env_id.as_deref() == Some(row.environment_id.as_str()))
            })
            .cloned();
        self.open_detail_pane(env_id, reference, matched, cx);
    }

    pub(super) fn select(
        &mut self,
        reference: IssueRef,
        entry: EnvironmentIssueEntry,
        cx: &mut Context<Self>,
    ) {
        let env_id = Some(entry.environment_id.clone());
        if let Some(key) = HeldIssueKey::of(self.state.read(cx), env_id.as_deref(), &reference) {
            hold_issue_seed(key, &entry.entry, cx);
        }
        let target = IssueTarget::from_ref(
            env_id.clone(),
            &reference,
            Some(entry.entry.display_key.clone()),
            Some(entry.entry.url.clone()),
        );
        self.open_detail_pane(env_id, reference, Some(entry), cx);
        self.state.update(cx, |state, cx| {
            state.open_issue_surface_for_key(ISSUES_PANEL_THREAD_KEY, target, cx);
        });
    }

    pub(super) fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.selected_entry = None;
        self.detail = None;
        self.state.update(cx, |state, cx| {
            if state.panels.panels.hide(ISSUES_PANEL_THREAD_KEY) {
                state.save_panels(cx);
            }
        });
        cx.notify();
    }

    pub fn copy_selected_key(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self
            .selected_entry
            .as_ref()
            .filter(|_| self.state.read(cx).issues_open)
            .cloned()
        else {
            return false;
        };
        self.copy_text(entry.entry.display_key, "Issue key", cx);
        true
    }

    pub(super) fn copy_text(&mut self, text: String, what: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        let toast = Toast::new(ToastKind::Success, format!("{what} copied")).description(text);
        self.state
            .update(cx, |state, cx| state.show_toast(toast, cx));
    }

    // --- Saved views ---

    pub(super) fn apply_saved_view(
        &mut self,
        view_id: String,
        preferences: IssueListPreferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_view_id = Some(view_id);
        let q = preferences.q.clone().unwrap_or_default();
        self.typed = q.clone();
        self.sent = q.trim().to_owned();
        self._debounce = None;
        self.search
            .update(cx, |input, cx| input.set_value(q, window, cx));
        self.update_prefs(move |prefs| *prefs = preferences, cx);
    }

    pub(super) fn open_saved_view_editor(
        &mut self,
        editing_id: Option<String>,
        initial_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placeholder = t(cx, ISSUE_SAVED_VIEW_PLACEHOLDER).to_owned();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(initial_name)
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
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
                    let toast = Toast::new(ToastKind::Error, t(cx, "This saved view was removed"))
                        .description(t(
                            cx,
                            "Your name has been kept. Save it as a new view or cancel.",
                        ));
                    self.state
                        .update(cx, |state, cx| state.show_toast(toast, cx));
                    cx.notify();
                }
                Err(_) => {}
            },
            None => {
                let id = format!(
                    "issue-view-{}-{}",
                    crate::sidebar::controller::now_ms(),
                    self.next_view_seq
                );
                self.next_view_seq += 1;
                if let Ok(saved_id) = create_saved_view(
                    &mut self.saved_views,
                    id,
                    &name,
                    self.prefs.normalized(),
                ) {
                    self.save_views();
                    self.selected_view_id = Some(saved_id);
                    self.saved_view_editor = None;
                    cx.notify();
                }
            }
        }
    }

    pub(super) fn update_active_saved_view(&mut self, view_id: &str, cx: &mut Context<Self>) {
        if update_saved_view_preferences(&mut self.saved_views, view_id, self.prefs.normalized()) {
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

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let collapsed = crate::workspace::sidebar_inline_width(
            self.state.read(cx),
            f32::from(window.viewport_size().width),
        ) == 0.;
        h_flex()
            .window_control_area(WindowControlArea::Drag)
            .h(px(crate::workspace::TITLEBAR_HEIGHT))
            .flex_none()
            .pl(px(if collapsed { 130. } else { 20. }))
            .pr(px(12.))
            .gap(px(12.))
            .border_b_1()
            .border_color(hsla(p.border))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .child(t(cx, "Issues").to_owned()),
            )
    }

    fn render_body(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.unsupported {
            return super::row::unsupported_state(cx).into_any_element();
        }
        let none_answered = self.held_pages.is_empty();
        let pending = self.is_pending();
        let first_error = self.read_tracker.first_error().map(str::to_owned);

        if none_answered && pending {
            return super::row::ghost(t(cx, "Loading issues…"), cx).into_any_element();
        }
        if let Some(error) = first_error.filter(|_| none_answered) {
            let retry = cx.listener(|this, _, _, cx| this.retry_failed(cx));
            return super::row::unavailable(error, pending, retry, cx).into_any_element();
        }
        if self.groups.is_empty() {
            return super::row::empty_no_trackers(cx).into_any_element();
        }
        list(
            self.list.clone(),
            cx.processor(|this, index: usize, window, cx| super::row::render(this, index, window, cx)),
        )
        .flex_1()
        .into_any_element()
    }
}

impl Render for IssuesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let partial_error = self
            .read_tracker
            .first_error()
            .filter(|_| !self.held_pages.is_empty())
            .map(str::to_owned);
        let detail = self.detail.clone();
        let preview_card = self.preview.render_card(cx);
        let dialog = self
            .saved_view_editor
            .as_ref()
            .map(|editor| super::toolbar::render_saved_view_dialog(self, editor, cx));

        let list_column = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(super::toolbar::render(self, window, cx))
            .children(partial_error.map(|error| {
                let retry = cx.listener(|this, _, _, cx| this.retry_failed(cx));
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
                    .text_color(hsla(p.foreground))
                    .child(format!("{error} {}", t(cx, "Showing the issues that loaded.")))
                    .child(
                        Button::new("issues-partial-retry", Variant::Outline, Size::Xs)
                            .label(t(cx, "Retry").to_owned())
                            .on_click(retry),
                    )
            }))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .px(px(8.))
                    .child(self.render_body(window, cx)),
            );

        div()
            .id("issues-page")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(hsla(p.background))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    if this.saved_view_editor.is_some() {
                        this.close_saved_view_editor(cx);
                        cx.stop_propagation();
                    } else if this.detail.is_some() {
                        this.clear_selection(cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .child(self.render_header(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .child(list_column)
                    .children(detail.map(|detail| {
                        div()
                            .id("issues-detail-pane")
                            .flex_none()
                            .w(px(480.))
                            .min_w(px(360.))
                            .h_full()
                            .min_h_0()
                            .border_l_1()
                            .border_color(hsla(p.border))
                            .child(detail)
                    })),
            )
            .children(preview_card.map(|card| {
                gpui::deferred(
                    div()
                        .absolute()
                        .bottom(px(16.))
                        .right(px(16.))
                        .child(card),
                )
                .with_priority(40)
            }))
            .children(dialog)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use serde_json::json;
    use t3_core::issues::wire::{IssueInvolvement, IssueListResult, IssueListState, decode};

    use super::*;

    fn test_runtime() -> tokio::runtime::Handle {
        static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RT.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test tokio runtime")
        })
        .handle()
        .clone()
    }

    #[gpui::test]
    fn issues_page_renders_groups_filters_saved_views_and_selection(cx: &mut gpui::TestAppContext) {
        let handle = test_runtime();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let state = cx.new(|cx| AppState::new(handle.clone(), cx));
            state.update(cx, |state, _| {
                state.environments.active = Some("env-a".into());
                state.issues_open = true;
            });
            state
        });
        let (page, cx) = cx.add_window_view(|window, cx| IssuesPage::new(state.clone(), window, cx));

        let result: IssueListResult = decode(json!({
            "viewers": { "github.com": "alice" },
            "trackers": [{
                "tracker": "github",
                "host": "github.com",
                "projectCount": 1,
                "capabilities": {
                    "create": true, "edit": true, "comment": true, "labels": true,
                    "assignees": true, "reactions": true, "templates": true,
                    "linkedPullRequests": true, "events": true, "search": true
                },
                "configured": true,
                "detail": null
            }],
            "entries": [{
                "tracker": "github",
                "host": "github.com",
                "scope": "acme/app",
                "key": "42",
                "displayKey": "#42",
                "title": "Fix issues page layout",
                "url": "https://github.com/acme/app/issues/42",
                "author": { "login": "alice" },
                "state": { "category": "open", "name": "Open" },
                "createdAt": "2026-01-01T00:00:00Z",
                "updatedAt": "2026-01-02T00:00:00Z",
                "closedAt": null,
                "assignees": [{ "login": "bob" }],
                "labels": [{ "name": "bug", "color": "d73a4a" }],
                "commentCount": 3,
                "projectId": "p1",
                "projectTitle": "App"
            }],
            "errors": [],
            "nextCursors": {}
        }))
        .unwrap();

        page.update(cx, |page, cx| {
            let search_key = page.search_scope_key();
            page.read_tracker
                .set_search_key(search_key.clone(), &["env-a".to_owned()]);
            let (generation, _) = page
                .read_tracker
                .begin_read("env-a", IssueListRefreshMode::Background);
            page.apply_environment_result("env-a".into(), search_key, generation, Ok(result), cx);

            assert_eq!(page.groups.len(), 1);
            assert_eq!(page.groups[0].rows.len(), 1);
            assert_eq!(page.groups[0].rows[0].entry.display_key, "#42");
            assert_eq!(page.rows.len(), 2); // TrackerHeader + Entry

            // Select entry and verify copy_selected_key.
            let row = page.groups[0].rows[0].clone();
            let reference = IssueRef::of(&row.entry);
            page.select(reference.clone(), row, cx);
            assert_eq!(page.selected, Some(reference));
            assert!(page.copy_selected_key(cx));
        });

        // Saved view creation, modification detection, rename, and deletion.
        page.update_in(cx, |page, window, cx| {
            page.prefs.who = IssueInvolvement::Assigned;
            page.prefs.status = IssueListState::Open;
            page.open_saved_view_editor(None, "Assigned Open".into(), window, cx);
            page.submit_saved_view_editor(cx);
            assert_eq!(page.saved_views.len(), 1);
            assert_eq!(page.saved_views[0].name, "Assigned Open");

            let view_id = page.saved_views[0].id.clone();
            page.open_saved_view_editor(Some(view_id.clone()), "My Queue".into(), window, cx);
            page.submit_saved_view_editor(cx);
            assert_eq!(page.saved_views[0].name, "My Queue");

            page.delete_active_saved_view(&view_id, cx);
            assert!(page.saved_views.is_empty());
        });

        // Verify mutual exclusivity with pull_requests_open.
        state.update(cx, |state, cx| {
            state.pull_requests_open = true;
            state.issues_open = false;
            state.set_issues_open(true, cx);
            assert!(state.issues_open);
            assert!(!state.pull_requests_open);
        });
    }

    #[gpui::test]
    fn fleet_multi_environment_targets_prioritized_pickers_and_cleanup(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::sync::Arc;

        let handle = test_runtime();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let state = cx.new(|cx| AppState::new(handle.clone(), cx));
            state.update(cx, |state, _| {
                let make_env = |saved: &str, canonical: &str, label: &str, project_id: &str, title: &str| {
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
                            "projects": [{
                                "id": project_id,
                                "title": title,
                                "workspaceRoot": format!("/tmp/{project_id}"),
                                "repositoryIdentity": {
                                    "canonicalKey": format!("dev.azure.com/acme/{title}/_git/repo"),
                                    "provider": "azure-devops",
                                    "displayName": format!("acme/{title}/_git/repo")
                                },
                                "defaultModelSelection": null,
                                "scripts": [],
                                "createdAt": "2026-01-01T00:00:00Z",
                                "updatedAt": "2026-01-01T00:00:00Z"
                            }],
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
                        summary: Arc::new(
                            t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                                owner, &shell, &config,
                            ),
                        ),
                        status: ConnectionStatus::Connected,
                        config,
                        grants: None,
                        settings_revision: 0,
                        keybindings_revision: 0,
                    })
                };

                state.environments.active = Some("saved-local".into());
                state.environments.environments = vec![
                    t3_client::store::SavedEnvironment {
                        id: "saved-disabled".into(),
                        environment_id: Some("canon-disabled".into()),
                        label: "Disabled".into(),
                        http_base: "http://localhost:9003".into(),
                        disabled: true,
                        access_token: None,
                        server_version: None,
                        local: false,
                        ssh: None,
                    },
                ];
                state.fleet = Arc::new(t3_client::environments::FleetView {
                    revision: 1,
                    environments: vec![
                        make_env("saved-local", "canon-local", "MacBook", "p-local", "Core"),
                        make_env("saved-remote", "canon-remote", "Linux Box", "p-remote", "Core"),
                        make_env("saved-disabled", "canon-disabled", "Disabled", "p-off", "Off"),
                    ],
                });
                state.issues_open = true;
            });
            state
        });
        let (page, cx) = cx.add_window_view(|window, cx| IssuesPage::new(state.clone(), window, cx));

        page.update(cx, |page, cx| {
            // 1. Multi-env targets include both connected enabled envs, skip disabled env.
            let targets = page.active_targets(cx);
            assert_eq!(targets.len(), 2);
            assert_eq!(targets[0].env_id, "saved-local");
            assert_eq!(targets[1].env_id, "saved-remote");
            assert_eq!(
                page.environment_label_for_row("saved-remote", cx).as_deref(),
                Some("Linux Box")
            );

            // Secondary-env project appears in filter_projects with disambiguated title, and
            // Azure DevOps issueScope is extracted properly from RepositoryIdentity.
            let projects = page.filter_projects(cx);
            assert_eq!(projects.len(), 2);
            assert!(projects.iter().any(|p| p.project_id == "p-remote" && p.title == "Core · Linux Box"));
            let descriptors = page.project_descriptors(cx);
            let remote_desc = descriptors.iter().find(|d| d.project_id == "p-remote").unwrap();
            assert_eq!(remote_desc.host.as_deref(), Some("dev.azure.com"));
            assert_eq!(remote_desc.issue_scope.as_deref(), Some("acme/Core"));

            // Selecting a secondary-env project by canonical ID still matches saved-remote!
            page.prefs.issue_server = Some("canon-remote".into());
            page.prefs.issue_project = Some("p-remote".into());
            let scoped_targets = page.active_targets(cx);
            assert_eq!(scoped_targets.len(), 1);
            assert_eq!(scoped_targets[0].env_id, "saved-remote");
            assert_eq!(
                page.canonical_scoped_project(cx).map(|s| s.environment_id),
                Some("saved-remote".into())
            );

            // 2. Active labels/assignee lead before 30-item truncation.
            let many_labels: Vec<String> = (0..35).map(|i| format!("a-label-{i:02}")).collect();
            let prioritized_labels = super::super::toolbar::prioritize_labels(
                &["z-active".to_owned()],
                many_labels,
                30,
            );
            assert_eq!(prioritized_labels.len(), 30);
            assert_eq!(prioritized_labels[0], "z-active");

            let many_people: Vec<String> = (0..35).map(|i| format!("a-user-{i:02}")).collect();
            let prioritized_people = super::super::toolbar::prioritize_assignees(
                Some("z-assignee"),
                many_people,
                30,
            );
            assert_eq!(prioritized_people.len(), 30);
            assert_eq!(prioritized_people[0], "z-assignee");

            // 3. Debounce cancels on update_prefs and send_query is a no-op when closed.
            page.typed_changed("pending".into(), cx);
            assert!(page._debounce.is_some());
            page.loading_more = Some("saved-remote dev.azure.com".into());
            page.update_prefs(|p| p.status = IssueListState::Closed, cx);
            assert!(page._debounce.is_none());
            assert!(page.loading_more.is_none(), "fetch_all must clear stale loading_more");

            page.open = false;
            page.sent = "old".into();
            page.send_query("after-close".into(), cx);
            assert_eq!(page.sent, "old", "send_query must not fetch after closing page");
        });

        // 4. Opening an issue via ISSUES_PANEL_THREAD_KEY while IssuesPage is active
        // (e.g. clicking an inline issue link in an issue body/comment) visibly navigates
        // IssuesPage's split detail pane.
        let linked_target = IssueTarget {
            environment_id: Some("saved-remote".into()),
            project_id: "p-remote".into(),
            tracker: t3_core::issues::wire::IssueTrackerKind::AzureDevOps,
            host: Some("dev.azure.com".into()),
            scope: "acme/Core".into(),
            key: "77".into(),
            display_key: Some("#77".into()),
            url: Some("https://dev.azure.com/acme/Core/_workitems/edit/77".into()),
        };
        let expected_ref = linked_target.issue_ref();
        state.update(cx, |app, cx| {
            app.issues_open = true;
            app.open_issue_surface_for_key(ISSUES_PANEL_THREAD_KEY, linked_target, cx);
        });
        cx.run_until_parked();
        page.read_with(cx, |page, cx| {
            assert_eq!(page.selected.as_ref(), Some(&expected_ref));
            let detail = page.detail.as_ref().expect("detail pane opened from panel event");
            assert!(detail.read(cx).matches_target(Some("saved-remote"), &expected_ref));
        });
    }
}
