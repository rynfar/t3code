//! The `@`, `/` and `$` menus: `ComposerCommandMenu.tsx` (the attached list), the searches of
//! `useComposerMenuItems.ts` (`projects.searchEntries`, threads, provider skills and commands with
//! their per-cwd snapshot from `server.refreshProviders`), `onSelectComposerItem` and
//! `nudgeComposerMenuHighlight` from `ChatComposer.tsx`, and the menu keys of
//! `composerCommandKey.ts`. Rows, ranking and inserted text come from `t3_core::composer::menu`.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{AnyElement, App, Context, Focusable, FontWeight, IntoElement, ParentElement, ScrollHandle, SharedString, Styled, Task, Window, div, prelude::*, px, relative};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::composer::context as context_rules;
use t3_core::composer::menu::{
    self as rules, ComposerPullRequest, MenuAction, MenuItem, PathEntry, PathKind, SkillSource, SlashCommand, SlashMenuOptions, ThreadContext,
    Trigger, TriggerKind, TriggerState,
};
use t3_core::palette::PaletteThread;
use t3_core::providers as provider_rules;
use t3_core::pull_requests::{detail::StateKind, link::visible_links, method as pr_method, wire::ListResult};
use t3_core::server_config::{ServerProvider, WorkspaceSnapshot};
use t3_core::sidebar::time::format_iso;

use super::{Composer, Target};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::provider_icon::{BadgeSize, ProviderIcon, ProviderMark};
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

/// `(instance id, cwd)`.
type SnapshotKey = (String, String);
/// `(environment id, environment label, threads)`.
type SearchedEnvironmentThreads = (String, String, Vec<PaletteThread>);

/// A failed scan waits this long before the next try (`WORKSPACE_SNAPSHOT_RETRY_COOLDOWN_MS`).
const SNAPSHOT_RETRY_MS: i64 = 10_000;
/// `max-h-72`.
const LIST_MAX_HEIGHT: f32 = 288.;
/// How far the attached menu tucks under the card (`--chat-composer-attachment-overlap`).
const OVERLAP: f32 = 17.;

/// A key the open menu takes from the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuKey {
    Up,
    Down,
    Accept,
    Dismiss,
    Delete,
}

#[derive(Default)]
pub(super) struct Menu {
    triggers: TriggerState,
    /// The row the user moved to and the search it was listed for.
    highlighted: Option<(String, String)>,
    items: Vec<MenuItem>,
    loading: bool,
    /// The latest `projects.searchEntries` reply: cwd, query, entries.
    paths: Option<(String, String, Vec<PathEntry>)>,
    scope: Option<(String, String)>,
    path_search: Option<(String, String, Task<()>)>,
    /// Older settled & linked-machine `orchestration.searchThreads` hits for `@<query>` (`pingdotgg/t3code#37243`).
    searched_threads: Option<(String, Vec<SearchedEnvironmentThreads>)>,
    thread_search: Option<(String, Task<()>)>,
    /// The latest `pullRequests.list` reply: project_id, query, entries.
    prs: Option<(String, String, Vec<ComposerPullRequest>)>,
    pr_search: Option<(String, String, Task<()>)>,
    /// The latest `pullRequests.detail` fallback for an exact positive `#123` query.
    pr_detail: Option<(String, String, u64, Option<ComposerPullRequest>)>,
    pr_detail_search: Option<(String, String, u64, Task<()>)>,
    prs_unavailable: bool,
    prs_failed: bool,
    snapshots: HashMap<SnapshotKey, WorkspaceSnapshot>,
    /// When each snapshot was last asked for; one scan per TTL.
    requested: HashMap<SnapshotKey, i64>,
    scanning: HashSet<SnapshotKey>,
    /// The caret and length the menu last read, so cursor blinks cost a comparison.
    caret: Option<(usize, usize)>,
    scroll: ScrollHandle,
    /// Threads mentioned per target. A send carries those the prompt still links.
    threads: HashMap<Target, Vec<ThreadContext>>,
}

/// Converts a shell `ThreadShell` (e.g. from `orchestration.searchThreads` older settled rows)
/// into a `PaletteThread` for `@` thread mention ranking.
fn palette_thread_from_shell(environment_id: &str, thread: &t3_core::shell::ThreadShell) -> PaletteThread {
    let presented = t3_core::sidebar::thread::present_thread(thread, None);
    PaletteThread {
        environment_id: environment_id.to_owned(),
        id: thread.id.clone(),
        project_id: thread.project_id.clone(),
        title: thread.title.clone(),
        branch: thread.branch.clone(),
        archived_at: thread.archived_at.clone(),
        hidden: !presented.listed,
        created_at: thread.created_at.clone(),
        updated_at: thread.updated_at.clone(),
        latest_user_message_at: thread.latest_user_message_at.clone(),
        latest_user_authored_message_at: thread.latest_user_authored_message_at.clone(),
        ..Default::default()
    }
}

/// Converts a fleet `ScopedThread` into a `PaletteThread` for linked-machine `@` mentions.
fn palette_thread_from_scoped(environment_id: &str, scoped: &t3_core::sidebar::fleet::ScopedThread) -> PaletteThread {
    let thread = &scoped.thread;
    PaletteThread {
        environment_id: environment_id.to_owned(),
        id: thread.id.clone(),
        project_id: thread.project_id.clone(),
        title: thread.title.clone(),
        branch: thread.branch.clone(),
        archived_at: thread.archived.then(|| thread.updated_at.map(format_iso).unwrap_or_default()),
        hidden: !thread.listed,
        created_at: scoped.created_at.clone(),
        updated_at: thread.updated_at.map(format_iso).unwrap_or_default(),
        latest_user_message_at: thread.latest_user_message_at.map(format_iso),
        latest_user_authored_message_at: thread.latest_user_authored_message_at.map(|at| at.map(format_iso)),
        opens: thread.opens.clone(),
        ..Default::default()
    }
}

fn environment_label_for_owner(state: &AppState, owner: &t3_core::sidebar::grouping::EnvironmentOwner) -> String {
    state
        .environments
        .environments
        .iter()
        .find(|e| {
            e.id == owner.saved_id
                || owner
                    .canonical_id
                    .as_deref()
                    .is_some_and(|cid| e.environment_id.as_deref() == Some(cid))
        })
        .map(|e| e.label.clone())
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| owner.saved_id.clone())
}

/// Collects cached local + linked-machine threads and environment labels for `@` mentions (`pingdotgg/t3code#37243`).
fn collect_environment_threads(
    state: &AppState,
    active_env_id: &str,
    searched: Option<&[SearchedEnvironmentThreads]>,
) -> (Vec<PaletteThread>, HashMap<String, String>) {
    let mut threads = palette_threads(state);
    for t in &mut threads {
        t.environment_id = active_env_id.to_owned();
    }
    let mut labels = HashMap::new();
    if let Some(label) = state
        .server_config
        .config
        .as_ref()
        .map(|c| c.environment.label.clone())
        .filter(|l| !l.trim().is_empty())
    {
        labels.insert(active_env_id.to_owned(), label);
    }

    let mut seen: HashSet<(String, String)> = threads
        .iter()
        .map(|t| (t.environment_id.clone(), t.id.clone()))
        .collect();

    for entry in &state.fleet.environments {
        let env_id = entry
            .summary
            .owner
            .canonical_id
            .as_deref()
            .unwrap_or(&entry.summary.owner.saved_id)
            .to_owned();
        let label = environment_label_for_owner(state, &entry.summary.owner);
        labels.entry(env_id.clone()).or_insert(label);
        if env_id == active_env_id {
            continue;
        }
        for t in &entry.summary.threads {
            if !t.thread.deleted && seen.insert((env_id.clone(), t.thread.id.clone())) {
                threads.push(palette_thread_from_scoped(&env_id, t));
            }
        }
    }

    if let Some(extra) = searched {
        for (env_id, label, hits) in extra {
            if !label.trim().is_empty() {
                labels.entry(env_id.clone()).or_insert_with(|| label.clone());
            }
            for hit in hits {
                if seen.insert((env_id.clone(), hit.id.clone())) {
                    threads.push(hit.clone());
                }
            }
        }
    }
    (threads, labels)
}

/// The sidebar's threads in the shape thread search ranks.
fn palette_threads(state: &AppState) -> Vec<PaletteThread> {
    state
        .sidebar
        .model
        .threads
        .iter()
        .filter(|thread| !thread.deleted)
        .map(|thread| PaletteThread {
            environment_id: t3_core::sidebar::ENVIRONMENT_ID.to_owned(),
            id: thread.id.clone(),
            project_id: thread.project_id.clone(),
            title: thread.title.clone(),
            branch: thread.branch.clone(),
            archived_at: thread.archived.then(|| thread.updated_at.map(format_iso).unwrap_or_default()),
            hidden: !thread.listed,
            created_at: thread.created_at.map(format_iso).unwrap_or_default(),
            updated_at: thread.updated_at.map(format_iso).unwrap_or_default(),
            latest_user_message_at: thread.latest_user_message_at.map(format_iso),
            latest_user_authored_message_at: thread.latest_user_authored_message_at.map(|at| at.map(format_iso)),
            opens: thread.opens.clone(),
            ..Default::default()
        })
        .collect()
}

impl Composer {
    pub(super) fn menu_open(&self) -> bool {
        self.menu.triggers.trigger().is_some()
    }

    /// The same checkout as draft launch (`ChatView.tsx` gitCwd).
    pub(super) fn menu_cwd(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let root = &state.shell.project(self.snapshot.project_id.as_deref()?)?.workspace_root;
        if let Some(id) = self.thread_id() {
            return Some(state.shell.thread(&id).and_then(|thread| thread.worktree_path.clone()).unwrap_or_else(|| root.clone()));
        }
        Some(t3_core::workspace::draft_cwd(self.selected_workspace().as_ref(), state.environments.active.as_deref().unwrap_or_default(), root).to_owned())
    }

    pub(super) fn workspace_snapshot_for<'a>(&'a self, provider: &'a ServerProvider, cwd: Option<&str>) -> Option<&'a WorkspaceSnapshot> {
        let fetched = cwd.and_then(|cwd| self.menu.snapshots.get(&(provider.instance_id.clone(), cwd.to_owned())));
        rules::workspace_snapshot(provider, fetched, cwd)
    }

    pub(super) fn workspace_snapshots_for_cwd(&self, cwd: Option<&str>) -> Vec<(String, WorkspaceSnapshot)> {
        let Some(cwd) = cwd else { return Vec::new() };
        self.menu
            .snapshots
            .iter()
            .filter(|((_, snap_cwd), _)| snap_cwd == cwd)
            .map(|((instance_id, _), snapshot)| (instance_id.clone(), snapshot.clone()))
            .collect()
    }

    pub(super) fn workspace_scanning_for_cwd(&self, cwd: Option<&str>) -> Vec<String> {
        let Some(cwd) = cwd else { return Vec::new() };
        self.menu
            .scanning
            .iter()
            .filter(|(_, scan_cwd)| scan_cwd == cwd)
            .map(|(instance_id, _)| instance_id.clone())
            .collect()
    }

    pub(super) fn ensure_workspace_snapshot(&mut self, instance_id: &str, cx: &mut Context<Self>) {
        let Some(cwd) = self.menu_cwd(cx) else { return };
        let snapshot = self
            .state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .and_then(|config| config.provider(instance_id))
            .and_then(|provider| self.workspace_snapshot_for(provider, Some(&cwd)));
        if !rules::snapshot_is_current(snapshot, now_ms()) {
            self.refresh_snapshot((instance_id.to_owned(), cwd), now_ms(), cx);
            if self.picker_open {
                let input = self.picker_input(cx);
                self.picker.update(cx, |picker, cx| picker.update_input(input, false, cx));
            }
        }
    }

    fn sync_menu_scope(&mut self, cx: &App) -> bool {
        let scope = self.state.read(cx).environments.active.clone().zip(self.menu_cwd(cx));
        if self.menu.scope == scope {return false;}
        if self.menu.scope.as_ref().map(|(environment, _)| environment) != scope.as_ref().map(|(environment, _)| environment) {
            self.menu.snapshots.clear();
            self.menu.requested.clear();
            self.menu.scanning.clear();
        }
        self.menu.scope = scope;
        self.menu.paths = None;
        self.menu.path_search = None;
        self.menu.searched_threads = None;
        self.menu.thread_search = None;
        self.menu.prs = None;
        self.menu.pr_search = None;
        self.menu.pr_detail = None;
        self.menu.pr_detail_search = None;
        self.menu.prs_unavailable = false;
        self.menu.prs_failed = false;
        self.menu.items.clear();
        self.menu.highlighted = None;
        true
    }

    pub(super) fn environment_id(&self, cx: &App) -> String {
        let config = self.state.read(cx).server_config.config.as_ref();
        config.map(|config| config.environment.environment_id.clone()).unwrap_or_else(|| t3_core::sidebar::ENVIRONMENT_ID.to_owned())
    }

    /// Re-reads the token under the caret after an edit (`force`) or a caret move.
    pub(super) fn sync_menu(&mut self, force: bool, cx: &mut Context<Self>) {
        let scope_changed = self.sync_menu_scope(cx);
        let caret = {
            let input = self.input.read(cx);
            (input.cursor(), input.text().len())
        };
        if !force && !scope_changed && self.menu.caret == Some(caret) {
            return;
        }
        self.menu.caret = Some(caret);
        // A question's custom answer is literal text: no menus (`isLiteralPendingAnswer`).
        let candidate = match self.answering.is_some() || self.snapshot.target.is_none() {
            true => None,
            false => rules::detect_trigger(&self.prompt(cx), caret.0),
        };
        if self.menu.triggers.set(candidate) || scope_changed {
            self.refresh_menu(cx);
            cx.notify();
        }
    }

    /// Rebuilds the rows for the active trigger, starting the searches it needs.
    pub(super) fn refresh_menu(&mut self, cx: &mut Context<Self>) {
        self.sync_menu_scope(cx);
        let Some(trigger) = self.menu.triggers.trigger().cloned() else {
            self.menu.items.clear();
            self.menu.loading = false;
            self.menu.path_search = None;
            self.menu.thread_search = None;
            self.menu.pr_search = None;
            self.menu.pr_detail_search = None;
            return;
        };
        let cwd = self.menu_cwd(cx);
        let (items, loading) = match trigger.kind {
            TriggerKind::Path => self.path_menu(&trigger, cwd, cx),
            TriggerKind::SlashCommand | TriggerKind::Skill => self.skill_menu(&trigger, cwd, cx),
            TriggerKind::PullRequest => self.pull_request_menu(&trigger, cx),
        };
        self.menu.items = items;
        self.menu.loading = loading;
    }

    fn pull_request_menu(&mut self, trigger: &Trigger, cx: &mut Context<Self>) -> (Vec<MenuItem>, bool) {
        let Some(project_id) = self.snapshot.project_id.clone() else {
            self.menu.pr_search = None;
            self.menu.pr_detail_search = None;
            self.menu.prs_unavailable = true;
            self.menu.prs_failed = false;
            return (Vec::new(), false);
        };
        let repository = {
            let state = self.state.read(cx);
            state
                .server_config
                .config
                .as_ref()
                .and_then(|_| state.shell.project(&project_id))
                .and_then(|project| rules::project_repository(project.repository_identity.as_ref()))
        };
        let Some(repository) = repository else {
            self.menu.pr_search = None;
            self.menu.pr_detail_search = None;
            self.menu.prs_unavailable = true;
            self.menu.prs_failed = false;
            return (Vec::new(), false);
        };
        self.menu.prs_unavailable = false;
        let linked: Vec<ComposerPullRequest> = self
            .thread_id()
            .and_then(|id| {
                let state = self.state.read(cx);
                let thread = state.shell.thread(&id)?;
                Some(
                    visible_links(&thread.pull_requests)
                        .into_iter()
                        .filter_map(|link| ComposerPullRequest::from_thread_link(&project_id, link))
                        .collect(),
                )
            })
            .unwrap_or_default();
        let query = trigger.query.trim().to_owned();
        let exact_number = (!query.is_empty() && query.chars().all(|c| c.is_ascii_digit()))
            .then(|| query.parse::<u64>().ok().filter(|&n| n > 0))
            .flatten();
        // `useComposerMenuItems.ts`: digits-only queries share the `""` list query and add
        // `pullRequests.detail` when the exact PR is missing from the recent 99 entries.
        let list_query = if exact_number.is_some() { String::new() } else { query.clone() };
        let (cached, list_exact) = match &self.menu.prs {
            Some((pid, cached_query, entries)) if *pid == project_id => (entries.clone(), *cached_query == list_query),
            _ => (Vec::new(), false),
        };
        if !list_exact && !matches!(&self.menu.pr_search, Some((pid, q, _)) if *pid == project_id && *q == list_query) {
            self.search_pull_requests(project_id.clone(), list_query, cx);
        }
        let mut combined = Vec::new();
        let mut waiting_detail = false;
        if let Some(number) = exact_number {
            let in_cached = list_exact
                && cached.iter().any(|entry| {
                    entry.project_id == project_id
                        && entry.repository.trim().eq_ignore_ascii_case(&repository)
                        && entry.number == number
                });
            if !in_cached {
                let detail_ready = match &self.menu.pr_detail {
                    Some((pid, repo, n, entry))
                        if *pid == project_id && repo.eq_ignore_ascii_case(&repository) && *n == number =>
                    {
                        if let Some(pr) = entry {
                            combined.push(pr.clone());
                        }
                        true
                    }
                    _ => false,
                };
                if !detail_ready {
                    waiting_detail = true;
                    let in_flight = matches!(
                        &self.menu.pr_detail_search,
                        Some((pid, repo, n, _)) if *pid == project_id && repo.eq_ignore_ascii_case(&repository) && *n == number
                    );
                    if !in_flight {
                        self.fetch_pull_request_detail(project_id.clone(), repository.clone(), number, cx);
                    }
                }
            }
        } else {
            self.menu.pr_detail_search = None;
        }
        combined.extend(cached);
        let items = rules::pull_request_items(&combined, &linked, &project_id, &repository, &query);
        (items, !list_exact || waiting_detail)
    }

    fn search_pull_requests(&mut self, project_id: String, query: String, cx: &mut Context<Self>) {
        let (searched_project, searched) = (project_id.clone(), query.clone());
        let scope = self.menu.scope.clone();
        let debounce_ms = if self.menu.prs.is_none() && searched.is_empty() { 0 } else { rules::PULL_REQUEST_SEARCH_DEBOUNCE_MS };
        let task = cx.spawn(async move |this, cx| {
            if debounce_ms > 0 {
                cx.background_executor().timer(Duration::from_millis(debounce_ms)).await;
            }
            let request = this.update(cx, |this, cx| {
                if this.state.read(cx).environments.active.clone().zip(this.menu_cwd(cx)) != scope {
                    return None;
                }
                let payload = rules::pull_request_list_request(&searched_project, &searched);
                this.state.read(cx).request(pr_method::LIST, payload, |reply| {
                    let result: ListResult = serde_json::from_value(reply).map_err(|e| e.to_string())?;
                    Ok(result)
                })
            });
            let Ok(Some(request)) = request else { return };
            let result = request.await.ok().and_then(Result::ok);
            let _ = this.update(cx, |this, cx| {
                if this.menu.scope != scope
                    || this.state.read(cx).environments.active.clone().zip(this.menu_cwd(cx)) != scope
                    || !matches!(&this.menu.pr_search, Some((p, q, _)) if *p == searched_project && *q == searched)
                {
                    return;
                }
                this.menu.pr_search = None;
                match result {
                    Some(list) => {
                        let entries: Vec<ComposerPullRequest> = list.entries.iter().map(ComposerPullRequest::from).collect();
                        this.menu.prs_failed = list.errors.iter().any(|error| error.project_id == searched_project);
                        this.menu.prs = Some((searched_project, searched, entries));
                    }
                    None => {
                        this.menu.prs_failed = true;
                        this.menu.prs = Some((searched_project, searched, Vec::new()));
                    }
                }
                this.refresh_menu(cx);
                cx.notify();
            });
        });
        self.menu.pr_search = Some((project_id, query, task));
    }

    fn fetch_pull_request_detail(&mut self, project_id: String, repository: String, number: u64, cx: &mut Context<Self>) {
        let (target_project, target_repo) = (project_id.clone(), repository.clone());
        let scope = self.menu.scope.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(rules::PULL_REQUEST_SEARCH_DEBOUNCE_MS)).await;
            let request = this.update(cx, |this, cx| {
                if this.state.read(cx).environments.active.clone().zip(this.menu_cwd(cx)) != scope {
                    return None;
                }
                let payload = rules::pull_request_detail_request(&target_project, &target_repo, number);
                this.state.read(cx).request(pr_method::DETAIL, payload, |reply| Ok(rules::decode_pull_request_detail(&reply)))
            });
            let Ok(Some(request)) = request else { return };
            let entry = request.await.ok().and_then(Result::ok).flatten();
            let _ = this.update(cx, |this, cx| {
                if this.menu.scope != scope
                    || this.state.read(cx).environments.active.clone().zip(this.menu_cwd(cx)) != scope
                    || !matches!(
                        &this.menu.pr_detail_search,
                        Some((p, r, n, _)) if *p == target_project && r.eq_ignore_ascii_case(&target_repo) && *n == number
                    )
                {
                    return;
                }
                this.menu.pr_detail_search = None;
                this.menu.pr_detail = Some((target_project, target_repo, number, entry));
                this.refresh_menu(cx);
                cx.notify();
            });
        });
        self.menu.pr_detail_search = Some((project_id, repository, number, task));
    }

    /// Threads first (active + linked-machine + older settled hits), then environments, models,
    /// and workspace entries of the latest search for this cwd.
    fn path_menu(&mut self, trigger: &Trigger, cwd: Option<String>, cx: &mut Context<Self>) -> (Vec<MenuItem>, bool) {
        let query = trigger.query.trim().to_owned();
        let environment_id = self.environment_id(cx);
        let mut items = if query.is_empty() {
            self.menu.thread_search = None;
            Vec::new()
        } else {
            let has_searched = self
                .menu
                .searched_threads
                .as_ref()
                .is_some_and(|(q, _)| *q == query);
            if !has_searched
                && query.encode_utf16().count() >= 2
                && !matches!(&self.menu.thread_search, Some((q, _)) if *q == query)
            {
                self.search_threads_for_menu(query.clone(), cx);
            }
            let searched = self
                .menu
                .searched_threads
                .as_ref()
                .filter(|(q, _)| *q == query)
                .map(|(_, hits)| hits.as_slice());
            let (threads, labels) = collect_environment_threads(self.state.read(cx), &environment_id, searched);
            rules::thread_items_across_environments(
                &threads,
                &environment_id,
                self.thread_id().as_deref(),
                &labels,
                &trigger.query,
                now_ms(),
            )
        };
        items.extend(t3_core::composer::records::environment_items(
            self.state.read(cx).environments.environments.iter().filter_map(|e| Some((e.environment_id.as_deref()?, e.label.as_str()))),
            &environment_id, &query,
        ));
        if let Some(config) = self.state.read(cx).server_config.config.as_ref() {
            let entries = provider_rules::entries(config, self.target_project());
            let snapshots: Vec<(String, WorkspaceSnapshot)> = entries
                .iter()
                .filter_map(|entry| {
                    self.workspace_snapshot_for(entry.provider, cwd.as_deref())
                        .map(|snapshot| (entry.instance_id().to_owned(), snapshot.clone()))
                })
                .collect();
            items.extend(rules::model_items(&entries, &config.settings, &snapshots, &trigger.query));
        }
        let Some(cwd) = cwd.filter(|_| !query.is_empty()) else {
            self.menu.path_search = None;
            return (items, false);
        };
        let mut current = false;
        if let Some((searched_cwd, searched, entries)) = &self.menu.paths
            && *searched_cwd == cwd
        {
            current = *searched == query;
            items.extend(rules::path_items(entries));
        }
        if !current && !matches!(&self.menu.path_search, Some((c, q, _)) if *c == cwd && *q == query) {
            self.search_paths(cwd, query, cx);
        }
        (items, !current)
    }

    /// Queries `orchestration.searchThreads` across active and linked/fleet machines so older
    /// settled threads surface in the `@` picker (`pingdotgg/t3code#37243`).
    fn search_threads_for_menu(&mut self, query: String, cx: &mut Context<Self>) {
        let searched = query.clone();
        let scope = self.menu.scope.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(rules::PATH_SEARCH_DEBOUNCE_MS))
                .await;
            let plan = this.update(cx, |this, cx| {
                if this.menu.scope != scope {
                    return None;
                }
                let state = this.state.read(cx);
                let active_env_id = this.environment_id(cx);
                let active_label = state
                    .server_config
                    .config
                    .as_ref()
                    .map(|c| c.environment.label.clone())
                    .unwrap_or_default();
                let runtime = state.runtime().clone();
                let mut active_req = None;
                if let Some(before) = state
                    .shell
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.older_settled.as_ref())
                    .map(|c| c.before.clone())
                    && let Some(client) = state.client_handle()
                {
                    let payload = serde_json::json!({
                        "query": t3_core::sidebar::older_settled::truncate_search_query(&searched),
                        "olderSettledBefore": before,
                        "titlesOnly": true,
                    });
                    active_req = Some((active_env_id.clone(), active_label, client, payload));
                }
                let mut linked_reqs = Vec::new();
                for entry in &state.fleet.environments {
                    let env_id = entry
                        .summary
                        .owner
                        .canonical_id
                        .as_deref()
                        .unwrap_or(&entry.summary.owner.saved_id)
                        .to_owned();
                    if env_id == active_env_id {
                        continue;
                    }
                    let Some(before) = entry.summary.older_settled.as_ref().map(|c| c.before.clone()) else {
                        continue;
                    };
                    if !state.scoped_ready(&entry.summary.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) {
                        continue;
                    }
                    let resolve = state
                        .connections
                        .destination(&entry.summary.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE);
                    let payload = serde_json::json!({
                        "query": t3_core::sidebar::older_settled::truncate_search_query(&searched),
                        "olderSettledBefore": before,
                        "titlesOnly": true,
                    });
                    let label = environment_label_for_owner(state, &entry.summary.owner);
                    linked_reqs.push((env_id, label, resolve, payload));
                }
                Some((runtime, active_req, linked_reqs))
            });
            let Ok(Some((runtime, active_req, linked_reqs))) = plan else {
                return;
            };
            let mut results: Vec<SearchedEnvironmentThreads> = Vec::new();
            if let Some((env_id, label, client, payload)) = active_req {
                let value = runtime
                    .spawn(async move {
                        client
                            .request(t3_core::sidebar::older_settled::SEARCH_THREADS, payload)
                            .await
                    })
                    .await
                    .ok()
                    .and_then(Result::ok);
                if let Some(read) = value.and_then(|v| serde_json::from_value::<t3_core::families::FamilyRead>(v).ok()) {
                    let threads = read
                        .threads
                        .iter()
                        .map(|t| palette_thread_from_shell(&env_id, t))
                        .collect();
                    results.push((env_id, label, threads));
                }
            }
            for (env_id, label, resolve, payload) in linked_reqs {
                let value = match resolve.await {
                    Ok(target) => runtime
                        .spawn(async move {
                            target
                                .client
                                .request(t3_core::sidebar::older_settled::SEARCH_THREADS, payload)
                                .await
                        })
                        .await
                        .ok()
                        .and_then(Result::ok),
                    Err(_) => None,
                };
                if let Some(read) = value.and_then(|v| serde_json::from_value::<t3_core::families::FamilyRead>(v).ok()) {
                    let threads = read
                        .threads
                        .iter()
                        .map(|t| palette_thread_from_shell(&env_id, t))
                        .collect();
                    results.push((env_id, label, threads));
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.menu.scope != scope
                    || !matches!(&this.menu.thread_search, Some((q, _)) if *q == searched)
                {
                    return;
                }
                this.menu.thread_search = None;
                this.menu.searched_threads = Some((searched, results));
                this.refresh_menu(cx);
                cx.notify();
            });
        });
        self.menu.thread_search = Some((query, task));
    }

    fn search_paths(&mut self, cwd: String, query: String, cx: &mut Context<Self>) {
        let (searched_cwd, searched) = (cwd.clone(), query.clone());
        let scope = self.menu.scope.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(rules::PATH_SEARCH_DEBOUNCE_MS)).await;
            let request = this.update(cx, |this, cx| {
                if this.state.read(cx).environments.active.clone().zip(this.menu_cwd(cx)) != scope {return None;}
                let payload = rules::search_entries_request(&searched_cwd, &searched);
                this.state.read(cx).request(rules::SEARCH_ENTRIES, payload, |reply| Ok(rules::decode_entries(&reply)))
            });
            let Ok(Some(request)) = request else { return };
            let entries = request.await.ok().and_then(Result::ok).unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.menu.scope != scope || this.state.read(cx).environments.active.clone().zip(this.menu_cwd(cx)) != scope || !matches!(&this.menu.path_search, Some((c, q, _)) if *c == searched_cwd && *q == searched) {return;}
                this.menu.path_search = None;
                this.menu.paths = Some((searched_cwd, searched, entries));
                this.refresh_menu(cx);
                cx.notify();
            });
        });
        self.menu.path_search = Some((cwd, query, task));
    }

    /// The `/` and `$` rows from the selected provider, with its snapshot for this cwd.
    fn skill_menu(&mut self, trigger: &Trigger, cwd: Option<String>, cx: &mut Context<Self>) -> (Vec<MenuItem>, bool) {
        let resolved = self.resolve(cx);
        let state = self.state.read(cx);
        let show_skills = state.client_settings.show_skills_in_slash_menu;
        let provider = resolved.model.as_ref().and_then(|model| state.server_config.config.as_ref()?.provider(&model.instance_id)).cloned();
        let Some(provider) = provider else { return (Vec::new(), false) };
        let key = cwd.as_ref().map(|cwd| (provider.instance_id.clone(), cwd.clone()));
        let fetched = key.as_ref().and_then(|key| self.menu.snapshots.get(key));
        let snapshot = rules::workspace_snapshot(&provider, fetched, cwd.as_deref());
        let (skills, commands) = rules::skills_and_commands(&provider, snapshot);
        let now = now_ms();
        let current = rules::snapshot_is_current(snapshot, now);
        let complete = snapshot.is_some_and(|snapshot| snapshot.slash_commands_pending != Some(true));
        let items = match trigger.kind {
            TriggerKind::SlashCommand => {
                let options = SlashMenuOptions {
                    fresh_session: self.fresh_session_command_available(cx),
                    side_chat: self.fixed.is_none() && state.side_chats_offered(),
                    side_chat_disabled: self.side_chat_disabled_reason(cx),
                    compact: self.compact_available(trigger, cx),
                    show_skills,
                };
                rules::slash_items(trigger, &provider.driver, &skills, &commands, options)
            }
            _ => rules::skill_items(trigger, &provider.driver, &skills),
        };
        let Some(key) = key else { return (items, false) };
        if !current {
            self.refresh_snapshot(key.clone(), now, cx);
        }
        (items, !complete && self.menu.scanning.contains(&key))
    }

    /// `useProviderWorkspaceSnapshotRefresh`: one scan per TTL, a short cooldown after a failure.
    fn refresh_snapshot(&mut self, key: SnapshotKey, now: i64, cx: &mut Context<Self>) {
        if self.menu.scanning.contains(&key)
            || self.menu.requested.get(&key).is_some_and(|at| now - at < rules::WORKSPACE_SNAPSHOT_TTL_MS)
        {
            return;
        }
        self.menu.requested.insert(key.clone(), now);
        self.menu.scanning.insert(key.clone());
        let environment = self.state.read(cx).environments.active.clone();
        let request = self.state.read(cx).rpc(rules::REFRESH_PROVIDERS, rules::refresh_providers_request(&key.0, &key.1));
        cx.spawn(async move |this, cx| {
            let reply = request.await;
            let _ = this.update(cx, |this, cx| {
                if this.state.read(cx).environments.active != environment {return;}
                this.menu.scanning.remove(&key);
                match reply.ok().and_then(|reply| rules::refreshed_snapshot(&reply, &key.0, &key.1)) {
                    Some(snapshot) => {
                        this.menu.snapshots.insert(key, snapshot);
                    }
                    None => {
                        this.menu.requested.insert(key, now_ms() - rules::WORKSPACE_SNAPSHOT_TTL_MS + SNAPSHOT_RETRY_MS);
                    }
                }
                if this.menu_open() {
                    this.refresh_menu(cx);
                    cx.notify();
                }
                if this.picker_open {
                    let input = this.picker_input(cx);
                    this.picker.update(cx, |picker, cx| picker.update_input(input, true, cx));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// `compactSlashCommandAvailable`: `/compact` alone in the prompt of an idle server thread
    /// that has something to compact.
    fn compact_available(&self, trigger: &Trigger, cx: &App) -> bool {
        let Some(Target::Thread(thread_id)) = &self.snapshot.target else { return false };
        let snapshot = &self.snapshot;
        if snapshot.running
            || !snapshot.loaded
            || !snapshot.pending.approvals.is_empty()
            || !snapshot.pending.user_inputs.is_empty()
            || !self.staged().is_empty()
        {
            return false;
        }
        let text = self.prompt(cx);
        let blank = |range: Option<&str>| range.is_some_and(|text| text.trim().is_empty());
        if !blank(text.get(..trigger.range_start)) || !blank(text.get(trigger.range_end..)) {
            return false;
        }
        let state = self.state.read(cx);
        state.thread_state(thread_id).projection.as_ref().filter(|projection| projection.thread.id == *thread_id).is_some_and(rules::has_compactable_conversation)
    }

    fn active_menu_item(&self) -> Option<&MenuItem> {
        let key = self.menu.triggers.trigger().map(rules::search_key);
        let (id, highlight_key) = match &self.menu.highlighted {
            Some((id, highlight_key)) => (Some(id.as_str()), Some(highlight_key.as_str())),
            None => (None, None),
        };
        rules::active_item(&self.menu.items, id, highlight_key, key.as_deref())
    }

    fn highlight(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(key) = self.menu.triggers.trigger().map(rules::search_key) else { return };
        if self.menu.highlighted.as_ref().is_some_and(|(current, k)| *current == id && *k == key) {
            return;
        }
        self.menu.highlighted = Some((id, key));
        cx.notify();
    }

    /// The menu's share of `composerCommandKey`. Returns whether the key was taken.
    pub(super) fn menu_key(&mut self, key: MenuKey, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // Only the prompt's own keys: the model picker's search field has its own Up and Down.
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        if !self.menu_open()
            || !focused
            || self.input.update(cx, |input, cx| gpui::EntityInputHandler::marked_text_range(input, window, cx).is_some())
        {
            return false;
        }
        match key {
            MenuKey::Up | MenuKey::Down => {
                let current = self.active_menu_item().map(|item| item.id.clone());
                let Some(next) = rules::nudge(&self.menu.items, current.as_deref(), key == MenuKey::Down) else { return false };
                if let Some(index) = self.menu.items.iter().position(|item| item.id == next) {
                    self.menu.scroll.scroll_to_item(index);
                }
                self.highlight(next, cx);
                true
            }
            MenuKey::Accept => {
                let Some(item) = self.active_menu_item().cloned() else { return false };
                self.select_menu_item(item, window, cx);
                true
            }
            MenuKey::Dismiss => {
                let trigger = self.current_trigger(cx);
                self.menu.triggers.dismiss(trigger);
                self.refresh_menu(cx);
                cx.notify();
                true
            }
            MenuKey::Delete => false,
        }
    }

    /// The trigger under the caret now, unless Escape dismissed it.
    fn current_trigger(&self, cx: &App) -> Option<Trigger> {
        let cursor = self.input.read(cx).cursor();
        self.menu.triggers.resolve(rules::detect_trigger(&self.prompt(cx), cursor))
    }

    /// Replaces `start..end` the way typing would, so undo and IME state stay intact.
    fn replace_prompt_range(&mut self, start: usize, end: usize, replacement: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_selected_range(start..end, cx);
            input.replace(replacement.to_owned(), window, cx);
        });
    }

    /// Replaces the trigger token, swallowing one space after it when the text ends in one.
    fn replace_trigger(&mut self, text: &str, trigger: &Trigger, replacement: &str, window: &mut Window, cx: &mut Context<Self>) {
        let end = rules::extend_range_for_trailing_space(text, trigger.range_end, replacement);
        self.replace_prompt_range(trigger.range_start, end, replacement, window, cx);
    }

    /// `onSelectComposerItem`.
    pub(super) fn select_menu_item(&mut self, item: MenuItem, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.prompt(cx);
        let Some(trigger) = self.current_trigger(cx) else { return };
        match item.action {
            MenuAction::Path { path, .. } => {
                let cwd = self.menu_cwd(cx);
                let current = matches!(&self.menu.paths, Some((c, q, _)) if Some(c) == cwd.as_ref() && *q == trigger.query.trim());
                if trigger.kind != TriggerKind::Path || !current {
                    return;
                }
                self.replace_trigger(&text, &trigger, &rules::file_mention_text(&path), window, cx);
            }
            MenuAction::Slash(SlashCommand::Model) => {
                self.replace_prompt_range(trigger.range_start, trigger.range_end, "", window, cx);
                if !self.picker_open {
                    self.toggle_picker(window, cx);
                }
            }
            MenuAction::Slash(SlashCommand::Clear) => {
                self.replace_prompt_range(trigger.range_start, trigger.range_end, "", window, cx);
                self.arm_fresh_session(cx);
            }
            MenuAction::Slash(command @ (SlashCommand::Side | SlashCommand::Btw)) => {
                if self.side_chat_disabled_reason(cx).is_some() {
                    return;
                }
                self.replace_prompt_range(trigger.range_start, trigger.range_end, &format!("/{} ", command.name()), window, cx);
            }
            MenuAction::ProviderCommand { name } => {
                if name == t3_core::usage::limits::UsageLimitsReport::COMMAND_NAME && self.can_intercept_usage_limits(&text, cx) {
                    self.replace_prompt_range(trigger.range_start, trigger.range_end, "", window, cx);
                    self.open_usage_limits(cx);
                    self.menu.highlighted = None;
                    cx.notify();
                    return;
                }
                self.replace_trigger(&text, &trigger, &format!("/{name} "), window, cx);
            }
            MenuAction::Skill { name, .. } => self.replace_trigger(&text, &trigger, &format!("${name} "), window, cx),
            MenuAction::Environment {environment_id,label} => {
                let record=t3_core::composer::records::environment_record(&environment_id,&label);
                self.replace_trigger(&text,&trigger,&format!("{} ",t3_core::composer::records::reference(&record).unwrap_or_default()),window,cx);
                if let Some(target)=self.snapshot.target.clone(){self.seed_records(&target,vec![record]);}
                self.draft_changed(cx);
            }
            MenuAction::Model { instance_id, model, .. } => {
                if trigger.kind != TriggerKind::Path {
                    return;
                }
                self.replace_trigger(&text, &trigger, &rules::model_mention_text(&instance_id, &model), window, cx);
            }
            MenuAction::Thread { thread_id, title } => {
                if trigger.kind != TriggerKind::Path {
                    return;
                }
                let context = ThreadContext::new(&self.environment_id(cx), &thread_id, &title);
                self.replace_trigger(&text, &trigger, &format!("{} ", context.reference()), window, cx);
                if let Some(target) = self.snapshot.target.clone() {
                    let threads = self.menu.threads.entry(target).or_default();
                    threads.retain(|existing| existing.context_id != context.context_id);
                    threads.push(context);
                }
                self.draft_changed(cx);
            }
            MenuAction::LinkedThread { environment_id, thread_id, title } => {
                if trigger.kind != TriggerKind::Path {
                    return;
                }
                let context = ThreadContext::new(&environment_id, &thread_id, &title);
                self.replace_trigger(&text, &trigger, &format!("{} ", context.reference()), window, cx);
                if let Some(target) = self.snapshot.target.clone() {
                    let threads = self.menu.threads.entry(target).or_default();
                    threads.retain(|existing| existing.context_id != context.context_id);
                    threads.push(context);
                }
                self.draft_changed(cx);
            }
            MenuAction::PullRequest(pr) => {
                if trigger.kind != TriggerKind::PullRequest {
                    return;
                }
                let record = rules::build_pull_request_reference_record(&pr);
                let replacement = format!("{} ", t3_core::composer::records::reference(&record).unwrap_or_default());
                self.replace_trigger(&text, &trigger, &replacement, window, cx);
                if let Some(target) = self.snapshot.target.clone() {
                    self.seed_records(&target, vec![record]);
                }
                self.draft_changed(cx);
            }
        }
        self.menu.highlighted = None;
        // A click on a row leaves the caret where typing continues; `/model` hands focus to the picker.
        if !self.picker_open {
            self.focus(window, cx);
        }
        cx.notify();
    }

    /// Inserts a file mention at the end of the prompt, after a space when the prompt does not
    /// end in one (`insertComposerTextAtEnd` with `ensureLeadingBoundary`). For the file tree and
    /// go-to-file.
    pub fn insert_mention(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.snapshot.target.is_none() || self.answering.is_some() {
            return;
        }
        let text = self.prompt(cx);
        let boundary = if text.is_empty() || text.ends_with(char::is_whitespace) { "" } else { " " };
        let end = text.len();
        self.replace_prompt_range(end, end, &format!("{boundary}{}", rules::file_mention_text(path)), window, cx);
        self.focus(window, cx);
    }

    /// Appends a Codex artifact template prompt to the active composer (`insertArtifactTemplate`).
    pub fn insert_artifact_template(
        &mut self,
        template: &t3_core::timeline::CodexArtifactTemplate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.snapshot.target.is_none() || self.sending || self.answering.is_some() {
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    crate::toast::Toast::new(crate::toast::ToastKind::Error, "Unable to add to chat")
                        .description("The composer is busy; try again once it is ready."),
                    cx,
                );
            });
            return;
        }
        let text = self.prompt(cx);
        if let Some(prompt) = t3_core::timeline::codex_artifact_template_prompt_to_append(&text, template) {
            let boundary = if text.is_empty() || text.ends_with(char::is_whitespace) { "" } else { " " };
            let end = text.len();
            self.replace_prompt_range(end, end, &format!("{boundary}{prompt}"), window, cx);
        }
        self.focus(window, cx);
    }

    /// The `context` a send to the current target carries: the terminal excerpts and threads
    /// its text still links, then its attachments (preserving SnapShot `source` metadata and long
    /// preview-annotation comments up to `COMPOSER_CONTEXT_PREVIEW_COMMENT_MAX_CHARS`).
    pub(super) fn message_context(&self, text: &str, attachments: &[t3_core::composer::attachments::Attachment]) -> Option<serde_json::Value> {
        let threads = self.snapshot.target.as_ref().and_then(|target| self.menu.threads.get(target)).map(Vec::as_slice).unwrap_or_default();
        let base = t3_core::composer::attachments::message_context_with_reviews(text, threads, self.current_terminals(), self.current_reviews(), attachments);
        let mut records = base.and_then(|value| value["records"].as_array().cloned()).unwrap_or_default();
        // Preserve SnapShot `source` metadata on staged image records (`pingdotgg/t3code#37771`).
        for staged in self.staged() {
            if let Some(source) = staged.snapshot_source.as_ref() {
                let cid = staged.attachment.context_id();
                if let Some(rec) = records.iter_mut().find(|r| t3_core::composer::records::field(r, "contextId") == cid)
                    && let Some(obj) = rec.as_object_mut()
                {
                    obj.insert("source".to_owned(), source.clone());
                }
            }
        }
        if let Some(target) = &self.snapshot.target {
            for r in self.records_for(target) {
                records.push(t3_core::composer::records::clamp_preview_annotation_record((**r).clone()));
            }
            if let Some(reply) = self.reply_to_for(target) {
                records.push(reply.clone());
            }
        }
        t3_core::composer::records::message_context(text, &records)
    }

    pub(super) fn has_context_records(&self, text: &str) -> bool {
        self.linked_threads(text) > 0 || self.chip_references(text).iter().any(|reference| {
            self.current_terminals().iter().any(|terminal| terminal.context_id() == reference.id)
                || self.current_reviews().iter().any(|review| review.context_id() == reference.id)
        })
    }

    /// How many mentioned threads or stored context records make a message sendable on their own.
    pub(super) fn linked_threads(&self, text: &str) -> usize {
        let Some(target) = self.snapshot.target.as_ref() else { return 0 };
        let linked = self.chip_references(text);
        self.mentions(target).iter().filter(|thread| linked.iter().any(|r| r.id == thread.context_id)).count()
            + self.records_for(target).iter().filter(|record| linked.iter().any(|r| r.id == t3_core::composer::records::field(record, "contextId"))).count()
    }

    pub(super) fn mentions(&self, target: &Target) -> &[ThreadContext] {
        self.menu.threads.get(target).map(Vec::as_slice).unwrap_or_default()
    }

    pub(super) fn seed_mentions(&mut self, target: &Target, threads: Vec<ThreadContext>) {
        if !threads.is_empty() {
            self.menu.threads.insert(target.clone(), threads);
        }
    }

    /// Forgets a target's mentioned threads and terminal excerpts once a stash took them.
    pub(super) fn clear_mentions(&mut self, target: &Target) {
        self.menu.threads.remove(target);
        self.clear_terminals(target);
        self.context.reviews.remove(target);
        self.context.records.remove(target);
    }

    /// The threads a send's `prompt` links; the rest go with the prompt's history.
    pub(super) fn take_threads(&mut self, target: &Target, prompt: &str) -> Vec<ThreadContext> {
        self.menu.threads.get_mut(target).map(|held| context_rules::take_for_send(held, prompt)).unwrap_or_default()
    }

    pub(super) fn restore_threads(&mut self, target: &Target, sent: Vec<ThreadContext>) {
        context_rules::restore_unsent(self.menu.threads.entry(target.clone()).or_default(), sent);
    }

    /// Threads `prompt` no longer links go once its undo history does.
    pub(super) fn reset_thread_history(&mut self, target: &Target, prompt: &str) {
        if let Some(held) = self.menu.threads.get_mut(target) {
            context_rules::reset_history(held, prompt);
        }
    }

    pub(super) fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let trigger = self.menu.triggers.trigger()?;
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let outline = hsla(p.composer_outline);
        let body = if self.menu.items.is_empty() {
            div()
                .px(px(20.))
                .pt(px(14.))
                .pb(px(28.))
                .text_xs()
                .text_color(hsla(p.secondary_label))
                .child(
                    if trigger.kind == TriggerKind::PullRequest && !self.menu.loading {
                        rules::pull_request_empty_text(self.menu.prs_unavailable, self.menu.prs_failed, &trigger.query)
                    } else {
                        rules::empty_text(trigger.kind, self.menu.loading).to_owned()
                    },
                )
                .into_any_element()
        } else {
            let active = self.active_menu_item().map(|item| item.id.clone());
            div()
                .id("composer-menu-list")
                .max_h(px(LIST_MAX_HEIGHT))
                .overflow_y_scroll()
                .track_scroll(&self.menu.scroll)
                .p(px(8.))
                .children(self.menu.items.iter().enumerate().map(|(index, item)| {
                    self.render_menu_row(index, item, trigger.kind, active.as_deref() == Some(item.id.as_str()), cx)
                }))
                .into_any_element()
        };
        let surface = v_flex()
            .id("composer-menu")
            .occlude()
            .mb(px(-OVERLAP))
            .rounded_t(px(16.))
            .border_1()
            .border_color(outline)
            .bg(hsla(if dark { p.surface_raised } else { p.card }))
            .pb(px(OVERLAP))
            .child(body);
        let cover = crate::webview::occluder("composer-menu-cover", crate::webview::Layer::TranscriptChrome);
        Some(div().absolute().left(px(22.)).right(px(22.)).bottom_full().child(surface).child(cover).into_any_element())
    }

    fn render_menu_row(&self, index: usize, item: &MenuItem, kind: TriggerKind, active: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let muted = hsla(p.secondary_label);
        let icon: Option<AnyElement> = match &item.action {
            MenuAction::Path { kind: PathKind::Directory, .. } => Some(Icon::Folder.render(px(16.), hsla(p.muted_foreground)).into_any_element()),
            MenuAction::Path { .. } => Some(Icon::File.render(px(16.), hsla(p.muted_foreground)).into_any_element()),
            MenuAction::Environment { .. } => Some(Icon::Server.render(px(16.), muted).into_any_element()),
            MenuAction::Thread { .. } | MenuAction::LinkedThread { .. } => Some(Icon::MessagesSquare.render(px(16.), muted).into_any_element()),
            MenuAction::Model { instance_id, .. } => self.state.read(cx).server_config.config.as_ref().and_then(|config| {
                let entries = provider_rules::entries(config, self.target_project());
                let entry = entries.iter().find(|entry| entry.instance_id() == instance_id)?;
                Some(ProviderIcon::new(ProviderMark::of(entry, &entries)).size(px(16.)).badge(BadgeSize::Compact, hsla(p.popover)).into_any_element())
            }),
            MenuAction::PullRequest(pr) => {
                let (glyph, color) = crate::pull_requests::controls::state_glyph(StateKind::of(pr.state, pr.is_draft), cx);
                Some(glyph.render(px(16.), color).into_any_element())
            }
            _ => None,
        };
        let label: AnyElement = match (&item.action, kind) {
            (MenuAction::Skill { .. }, TriggerKind::SlashCommand) => h_flex()
                .child(div().text_color(muted).child("/skill:"))
                .child(SharedString::from(item.label.trim_start_matches("/skill:").to_owned()))
                .into_any_element(),
            _ => SharedString::from(item.label.clone()).into_any_element(),
        };
        let badge = match &item.action {
            MenuAction::Skill { source, .. } => Some(skill_badge(*source, kind == TriggerKind::Skill, cx)),
            _ => None,
        };
        let id = item.id.clone();
        let picked = item.clone();
        h_flex()
            .id(("composer-menu-item", index))
            .min_h(px(28.))
            .px(px(8.))
            .py(px(6.))
            .gap(px(8.))
            .rounded(px(8.))
            .text_sm()
            .cursor_pointer()
            .when(active, |this| this.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
            .on_mouse_move(cx.listener(move |this, _, _, cx| this.highlight(id.clone(), cx)))
            .on_click(cx.listener(move |this, _, window, cx| this.select_menu_item(picked.clone(), window, cx)))
            .children(icon)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.))
                    .child(div().flex_shrink_0().max_w(relative(0.45)).min_w_0().truncate().text_xs().font_weight(FontWeight::MEDIUM).child(label))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(SharedString::from(item.description.clone())))
                    .children(badge),
            )
            .into_any_element()
    }
}

/// `SkillSourceBadge`: the secondary badge naming where a skill comes from.
fn skill_badge(source: SkillSource, suffix: bool, cx: &App) -> AnyElement {
    let p = palette(cx);
    let icon = match source {
        SkillSource::App => Icon::Blocks,
        SkillSource::Repo | SkillSource::Project => Icon::Folder,
        SkillSource::Personal => Icon::UserRound,
        SkillSource::System => Icon::Settings,
        SkillSource::Other => Icon::Package,
    };
    let color = hsla(p.secondary_foreground);
    h_flex()
        .flex_shrink_0()
        .h(px(18.))
        .min_w(px(18.))
        .px(px(3.))
        .gap(px(4.))
        .rounded(px(4.))
        .bg(hsla(p.secondary))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(icon.render(px(12.), color.opacity(0.8)))
        .child(if suffix { format!("{} Skill", source.label()) } else { source.label().to_owned() })
        .into_any_element()
}
