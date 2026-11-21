//! The diff surface. Ports `components/DiffPanel.tsx`: the scope (Changes, Uncommitted, a turn,
//! one tool call), the requests behind it, collapsed files per scope (`useDiffPanelSessionStore`),
//! and its loading, empty and error states. Patches are fetched and parsed on the network
//! runtime; the view paints only the rows on screen.

mod base_ref;
mod comments;
#[cfg(feature = "snapshot")]
mod snapshot;
mod header;
pub(crate) mod highlight;
pub(crate) mod lines;
mod source;
mod tree;
mod view;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    ClipboardItem, Context, Entity, IntoElement, ListAlignment, ListOffset, ListState, ParentElement, Pixels, Render,
    Styled, Subscription, Task, UniformListScrollHandle, Window, div, point, prelude::*, px,
};
use gpui_component::v_flex;
use t3_client::supervisor::ConnectionStatus;
use t3_core::composer::review::Side;
use t3_core::diff::{
    BranchCommits, DiffFileContents, DiffFoldOverride, DiffLine, DiffScope, DiffSelection,
    DiffSession, ExpandDirection, FILES_VIEWED_FLUSH_DELAY_MS, FileContextMenuAction,
    FileContextMenuCapabilities, FileReveal, FileRevealTarget, FileViewedState, FilesViewedTracker,
    InlineReviewThread, InlineReviewThreadsState, Layout, ParsedPatch, PreviewSourceKind,
    PullRequestDiffFolds, RevealAction, ScrollRestore, ScrollRestoreAction, TurnDiffSummary,
    branch_commits_request, decode_branch_commits, decode_diff_file_contents,
    diff_file_contents_request, expand_hunk_context, file_context_menu_error_title,
    file_context_menu_open_request, format_diff_path_for_clipboard, is_test_changed_file,
    line_number_for_side, project_commit_url, resolve_file_context_menu_absolute_path,
    set_files_viewed_payload, trailing_hidden_lines, turn_diff_summaries, viewed_range_indices,
    workspace_mutation_id,
};
use t3_core::right_panel::SurfaceKind;

use crate::app_state::{AppEvent, AppState, Phase};
use crate::theme::{Palette, hsla, palette};
use source::{Loaded, RequestKey};
pub use view::Parsed;
use view::View;

/// `LIVE_REFRESH_MIN_INTERVAL_MS`: showing the panel revalidates a git diff older than this.
const GIT_REFRESH_FLOOR: Duration = Duration::from_secs(10);
const CACHE_LIMIT: usize = 6;

pub struct DiffPanel {
    state: Entity<AppState>,
    thread_id: Option<String>,
    fingerprint: (usize, Option<String>),
    summaries: Arc<Vec<TurnDiffSummary>>,
    selection: DiffSelection,
    scope: DiffScope,
    request: Option<RequestKey>,
    patch_reads: Arc<tokio::sync::Semaphore>,
    patch_commands: Option<tokio::sync::mpsc::Sender<source::Command>>,
    #[cfg(feature = "snapshot")]
    fixture: bool,
    loaded: Option<(RequestKey, Loaded)>,
    cache: Vec<(RequestKey, Loaded)>,
    pending: bool,
    error: Option<String>,
    _fetch: Option<Fetch>,
    fetch_generation: u64,
    connection: Option<u64>,
    _grants: Option<Task<()>>,
    git_revision: u64,
    git_fetched_at: Option<Instant>,
    /// `deriveWorkspaceMutationId` as last seen: a change refetches git diffs mid-turn.
    workspace_mutation: Option<String>,
    /// Whether each cwd is a repository, learned from git previews (`isGitRepo`).
    repositories: HashMap<String, bool>,
    connected: bool,
    visible: bool,
    /// The persisted layout, file tree toggle and default folds, as last rendered.
    ui: (Layout, bool, bool),
    view: Option<View>,
    session: DiffSession,
    tree_collapsed: HashSet<String>,
    list: ListState,
    list_width: Option<Pixels>,
    tree_scroll: UniformListScrollHandle,
    wrap: bool,
    wrap_override: Option<bool>,
    measured_font: Option<(gpui::SharedString, Pixels)>,
    highlighted_lines: RefCell<highlight::DiffHighlightCache>,
    ignore_whitespace: bool,
    h_offset: Pixels,
    char_width: Option<Pixels>,
    top_file: Option<usize>,
    selected_file: Option<String>,
    file_reveal: FileReveal,
    scroll_restore: ScrollRestore,
    external_scroll_handler: Rc<RefCell<Option<ScrollHandler>>>,
    /// A patch from outside the thread (a pull request's Code tab) replaces the scopes.
    external: Option<External>,
    comments: comments::Comments,
    comments_config: comments::CommentsConfig,
    copied_path: Option<(String, u64)>,
    copied_generation: u64,
    _copied_reset: Option<Task<()>>,
    branch_commits: Option<BranchCommits>,
    branch_commits_error: Option<String>,
    branch_commits_loading: bool,
    branch_commits_key: Option<(String, String, Option<String>)>,
    branch_commits_generation: u64,
    _branch_commits: Option<Task<()>>,
    /// Per-file hydrated contents and loading/error status for hidden hunk context expansion.
    expansions: HashMap<String, FileExpansionState>,
    expansion_scope_key: Option<String>,
    /// Revision-tracked PR files-viewed state (`pingdotgg/t3code#35114`).
    viewed_target: Option<ViewedTarget>,
    viewed_tracker: FilesViewedTracker,
    /// GitHub-style PR diff fold tracker (`pingdotgg/t3code#37605`, `pingdotgg/t3code#39213`).
    pr_folds: Option<PullRequestDiffFolds>,
    /// File path explicitly revealed in detached/PR mode so hidden test files stay visible (`pingdotgg/t3code#39213`).
    revealed_path: Option<String>,
    /// On-demand loading status for oversized PR files withheld from the initial patch (`pingdotgg/t3code#37007`).
    withheld_files: HashMap<String, WithheldFileStatus>,
    withheld_readable: bool,
    /// Files whose loaded diff has server-truncated long lines (`…[N more characters]`), disabling hunk expansion.
    cut_line_files: HashSet<String>,
    on_load_withheld: Option<LoadWithheldCallback>,
    last_viewed_file_key: Option<String>,
    _viewed_flush: Option<Task<()>>,
    /// Inline review conversations and resolved-avatar expansion state (`pingdotgg/t3code#34644`).
    pub(super) review_threads: InlineReviewThreadsState,
    _subscription: Subscription,
}

type LoadWithheldCallback = Rc<dyn Fn(String, &mut gpui::App)>;

/// On-demand load status for a file withheld from an oversized PR diff (`pingdotgg/t3code#37007`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WithheldFileStatus {
    Idle,
    Loading,
    Error(String),
    Empty,
}

#[derive(Default)]
pub(super) struct FileExpansionState {
    pub contents: Option<DiffFileContents>,
    pub loading: bool,
    pub error: Option<String>,
    pub _task: Option<Task<()>>,
}

/// Host target for revision-tracked `pullRequests.setFilesViewed` (`pingdotgg/t3code#35114`).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewedTarget {
    pub project_id: String,
    pub number: u64,
    pub host: Option<String>,
    pub repository: Option<String>,
    /// Set when viewing a single commit diff, so `revision` is omitted from `setFilesViewed`.
    pub commit: Option<String>,
    pub enabled: bool,
}

type ScrollHandler = Box<dyn FnMut(&gpui::ListScrollEvent, &mut Window, &mut gpui::App)>;

struct Fetch {
    network: tokio::task::AbortHandle,
    _completion: Task<()>,
}

impl Drop for Fetch {
    fn drop(&mut self) {
        self.network.abort();
    }
}

/// What a detached panel shows: a key for its folds, and the patch or why there is none.
pub struct External {
    pub key: String,
    pub parsed: Option<Arc<Parsed>>,
    pub pending: bool,
    pub error: Option<String>,
}

impl DiffPanel {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut panel = Self::construct(state, cx);
        #[cfg(feature = "snapshot")]
        if snapshot::install(&mut panel, cx) { return panel; }
        panel.sync(cx);
        panel
    }

    fn construct(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if *event == AppEvent::ReviewComments {
                if let Some(view) = &this.view { this.list.remeasure_items(0..view.row_count()); }
                cx.notify();
            }
            if matches!(event, AppEvent::Thread | AppEvent::Selection | AppEvent::RightPanel | AppEvent::Shell | AppEvent::Phase | AppEvent::SideThread) {
                this.sync(cx);
            }
        });
        let external_scroll_handler: Rc<RefCell<Option<ScrollHandler>>> = Rc::new(RefCell::new(None));
        let panel = DiffPanel {
            state,
            thread_id: None,
            fingerprint: (0, None),
            summaries: Arc::new(Vec::new()),
            selection: DiffSelection::default(),
            scope: DiffScope::Unstaged,
            request: None,
            patch_reads: Arc::new(tokio::sync::Semaphore::new(4)),
            patch_commands: None,
            #[cfg(feature = "snapshot")]
            fixture: false,
            loaded: None,
            cache: Vec::new(),
            pending: false,
            error: None,
            _fetch: None,
            fetch_generation: 0,
            connection: None,
            _grants: None,
            git_revision: 0,
            git_fetched_at: None,
            workspace_mutation: None,
            repositories: HashMap::new(),
            connected: false,
            visible: false,
            ui: (Layout::Stacked, false, true),
            view: None,
            session: DiffSession::default(),
            tree_collapsed: HashSet::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            list_width: None,
            tree_scroll: UniformListScrollHandle::new(),
            wrap: true,
            wrap_override: None,
            measured_font: None,
            highlighted_lines: RefCell::default(),
            ignore_whitespace: crate::settings::client_settings(cx).diff_ignore_whitespace(),
            h_offset: px(0.),
            char_width: None,
            top_file: None,
            selected_file: None,
            file_reveal: FileReveal::default(),
            scroll_restore: ScrollRestore::default(),
            external_scroll_handler: external_scroll_handler.clone(),
            external: None,
            comments: comments::Comments::default(),
            comments_config: comments::CommentsConfig::default(),
            copied_path: None,
            copied_generation: 0,
            _copied_reset: None,
            branch_commits: None,
            branch_commits_error: None,
            branch_commits_loading: false,
            branch_commits_key: None,
            branch_commits_generation: 0,
            _branch_commits: None,
            expansions: HashMap::new(),
            expansion_scope_key: None,
            viewed_target: None,
            viewed_tracker: FilesViewedTracker::default(),
            pr_folds: None,
            revealed_path: None,
            withheld_files: HashMap::new(),
            withheld_readable: false,
            cut_line_files: HashSet::new(),
            on_load_withheld: None,
            last_viewed_file_key: None,
            _viewed_flush: None,
            review_threads: InlineReviewThreadsState::default(),
            _subscription: subscription,
        };
        let entity = cx.entity().downgrade();
        panel.list.set_scroll_handler(move |event, window, cx| {
            if let Some(handler) = external_scroll_handler.borrow_mut().as_mut() {
                handler(event, window, cx);
            }
            let entity = entity.clone();
            cx.defer(move |cx| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |panel, cx| {
                        panel.on_list_scrolled();
                        panel.load_nearby(cx);
                    });
                }
            });
        });
        panel
    }

    /// A panel that shows `show_external`'s patch instead of the open thread's scopes.
    pub fn detached(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self::construct(state, cx);
        panel.external = Some(External { key: String::new(), parsed: None, pending: true, error: None });
        panel
    }

    /// The pull request Code tab uses the same initial fold preference as thread diffs.
    pub fn show_external(&mut self, external: External, cx: &mut Context<Self>) {
        if self.external.as_ref().map(|existing| existing.key.as_str()) != Some(external.key.as_str()) {
            self.revealed_path = None;
        }
        self.external = Some(external);
        self.rebuild(cx);
        cx.notify();
    }

    /// Reconciles with app state. Runs on every thread event, so it does nothing unless the
    /// scope, the request, the connection or the workspace actually changed.
    fn sync(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "snapshot")]
        if self.fixture { cx.notify(); return; }
        if self.external.is_some() {
            let ui = (crate::settings::client_settings(cx).diff_layout(), self.state.read(cx).panels.diff_file_tree_open, crate::settings::client_settings(cx).diff_files_collapsed());
            if ui != self.ui {
                self.ui = ui;
                self.rebuild(cx);
                cx.notify();
            }
            return;
        }
        let connection = self.state.read(cx).connection_id();
        let connection_changed = connection != self.connection;
        if connection_changed {
            self.connection = connection;
            self.cancel_fetch();
            self.request = None;
            self.loaded = None;
            self.cache.clear();
            self.repositories.clear();
            self.git_fetched_at = None;
            self.connected = false;
            self.branch_commits = None;
            self.branch_commits_error = None;
            self.branch_commits_loading = false;
            self.branch_commits_key = None;
            self.branch_commits_generation += 1;
            self._branch_commits = None;
            self.comments = comments::Comments::default();
            self.expansions.clear();
            self.expansion_scope_key = None;
            self.file_reveal.reset();
            self.revealed_path = None;
            self.scroll_restore = ScrollRestore::default();
            self._grants = None;
            if let Some(client) = self.state.read(cx).client_handle() {
                let mut grants = client.session_state();
                self._grants = Some(cx.spawn(async move |this, cx| {
                    while grants.changed().await.is_ok() {
                        grants.borrow_and_update();
                        if this.update(cx, |this, cx| {
                            this.state.update(cx, |state, cx| state.sync_side_threads(cx));
                            this.sync(cx);
                        }).is_err() { return; }
                    }
                }));
            }
        }
        let state = self.state.read(cx);
        let thread_id = state.selected_thread.clone();
        let thread_changed = thread_id != self.thread_id;
        if thread_changed {
            self.comments = comments::Comments::default();
            self.thread_id = thread_id.clone();
            self.fingerprint = (0, None);
            self.summaries = Arc::new(Vec::new());
            self.workspace_mutation = None;
            self.selected_file = None;
            self.file_reveal.reset();
            self.revealed_path = None;
            self.tree_collapsed.clear();
            self.branch_commits = None;
            self.branch_commits_error = None;
            self.branch_commits_loading = false;
            self.branch_commits_key = None;
            self.branch_commits_generation += 1;
            self._branch_commits = None;
        }
        let Some(thread_id) = thread_id else {
            self.cancel_fetch();
            if self.request.take().is_some() | self.view.take().is_some() {
                self.list.reset(0);
                cx.notify();
            }
            return;
        };
        let ignore_whitespace = self.session.ignore_whitespace(&thread_id, crate::settings::client_settings(cx).diff_ignore_whitespace());
        let whitespace_changed = self.ignore_whitespace != ignore_whitespace;
        self.ignore_whitespace = ignore_whitespace;
        // A failed request reruns once the connection settles (`runtime.ts` query re-execution).
        let connected = matches!(state.phase, Phase::Online(ConnectionStatus::Connected));
        let reconnected = connected && !self.connected;
        if reconnected {
            self.error = None;
        }
        self.connected = connected;
        let projection = state.thread.projection.as_ref().filter(|projection| projection.thread.id == thread_id);
        let fingerprint = projection
            .map(|projection| (projection.checkpoints.len(), projection.checkpoints.last().map(|checkpoint| checkpoint.id.clone())))
            .unwrap_or((0, None));
        let checkpoints_changed = fingerprint != self.fingerprint;
        if checkpoints_changed {
            self.summaries = Arc::new(projection.map(|projection| turn_diff_summaries(&projection.checkpoints)).unwrap_or_default());
            self.fingerprint = fingerprint;
        }
        let mutation = projection.and_then(|projection| {
            let latest_checkpoint = self.summaries.first().map(|summary| summary.completed_at.as_str());
            workspace_mutation_id(projection.visible_turn_items.iter().map(|row| row.item.as_ref()), latest_checkpoint)
        });
        let selection = state.panels.diff_selection(&thread_id);
        if matches!(selection, DiffSelection::Turn { .. }) && checkpoints_changed && !self.summaries.is_empty() {
            let summaries = self.summaries.clone();
            self.state.update(cx, |state, cx| state.reconcile_diff_selection(&summaries, cx));
        }
        let state = self.state.read(cx);
        let environment = state.environment().map(str::to_owned);
        let visible = state.panels.panels.active_surface(&thread_id).is_some_and(|surface| surface.kind == SurfaceKind::Diff);
        let scope = match DiffScope::resolve(&selection, &self.summaries) {
            DiffScope::ToolCall(target) => {
                let projection = state.thread_state(&target.thread_id).projection.as_ref().filter(|projection| projection.thread.id == target.thread_id);
                DiffScope::ToolCall(target.resolve(projection.map(|projection| projection.visible_turn_items.as_slice()).unwrap_or_default()))
            }
            scope => scope,
        };
        let cwd = state.thread_shell(&thread_id).and_then(|thread| {
            thread.worktree_path.clone().or_else(|| state.shell.project(&thread.project_id).map(|project| project.workspace_root.clone()))
        });
        let repository = cwd.as_ref().and_then(|cwd| self.repositories.get(cwd)).copied();
        // The agent may have changed files; showing the panel revalidates a stale git diff.
        let git_stale = self.git_fetched_at.is_some_and(|at| at.elapsed() >= GIT_REFRESH_FLOOR);
        if (mutation != self.workspace_mutation && self.git_fetched_at.is_some()) || (visible && !self.visible && git_stale) {
            self.git_revision += 1;
        }
        self.workspace_mutation = mutation;
        self.visible = visible;
        let can_read = state.can_read_threads();
        let source_exists = match &scope {
            DiffScope::ToolCall(target) => target.source_available(state.thread_shell(&target.thread_id), state.thread_state(&target.thread_id)),
            _ => true,
        };
        let branch_commits_key = environment.as_ref().zip(cwd.as_ref()).map(|(environment, cwd)| {
            (environment.clone(), cwd.clone(), state.panels.branch_base_refs.get(&thread_id).cloned())
        });
        let request = environment.filter(|_| can_read && source_exists).and_then(|environment| match &scope {
            DiffScope::Branch { base_ref } => cwd.clone().map(|cwd| RequestKey::Git {
                kind: t3_core::diff::PreviewSourceKind::BranchRange,
                environment,
                cwd,
                base_ref: base_ref.clone(),
                commit_sha: None,
                ignore_whitespace: self.ignore_whitespace,
                revision: self.git_revision,
            }),
            DiffScope::Unstaged => cwd.clone().map(|cwd| RequestKey::Git {
                kind: t3_core::diff::PreviewSourceKind::WorkingTree,
                environment,
                cwd,
                base_ref: None,
                commit_sha: None,
                ignore_whitespace: self.ignore_whitespace,
                revision: self.git_revision,
            }),
            DiffScope::Commit { sha, base_ref } => cwd.clone().map(|cwd| RequestKey::Git {
                kind: t3_core::diff::PreviewSourceKind::Commit,
                environment,
                cwd,
                base_ref: base_ref.clone(),
                commit_sha: Some(sha.clone()),
                ignore_whitespace: self.ignore_whitespace,
                revision: self.git_revision,
            }),
            // `useCheckpointDiff` is disabled outside a repository.
            DiffScope::Turn { summary_index, .. } if repository != Some(false) => Some(RequestKey::Turn {
                environment,
                thread_id: thread_id.clone(),
                turn_count: self.summaries[*summary_index].turn_count,
                ignore_whitespace: self.ignore_whitespace,
            }),
            DiffScope::Turn { .. } | DiffScope::NoTurns => None,
            DiffScope::ToolCall(target) => Some(RequestKey::ToolCall { environment, target: target.clone() }),
        });
        let ui = (crate::settings::client_settings(cx).diff_layout(), state.panels.diff_file_tree_open, crate::settings::client_settings(cx).diff_files_collapsed());
        let changed = connection_changed
            || thread_changed
            || request != self.request
            || scope != self.scope
            || selection != self.selection
            || checkpoints_changed
            || whitespace_changed
            || reconnected
            || ui != self.ui;
        self.ui = ui;
        self.selection = selection;
        self.scope = scope;
        if !can_read || !source_exists {
            self.loaded = None;
            self.cache.clear();
        }
        if thread_changed || !visible || !connected || request != self.request {
            self.cancel_fetch();
        }
        if reconnected {
            self.cache.clear();
            self.loaded = None;
        }
        if request != self.request {
            self.request = request.clone();
            self.error = None;
            if let Some(key) = request {
                match self.cache.iter().position(|(cached, _)| *cached == key) {
                    Some(index) => {
                        let entry = self.cache.remove(index);
                        self.cache.push(entry.clone());
                        self.loaded = Some(entry);
                        self.pending = false;
                        if visible && connected && matches!(key, RequestKey::Git { .. }) { self.fetch(key, cx); }
                    }
                    None if visible && connected => self.fetch(key, cx),
                    None => self.pending = false,
                }
            }
        } else if visible
            && connected
            && !self.pending
            && self.error.is_none()
            && let Some(key) = self.request.clone()
            && (source::needs_fetch(&key, self.loaded.as_ref().map(|(loaded, _)| loaded)) || (matches!(key, RequestKey::Git { .. }) && self.patch_commands.is_none()))
        {
            self.fetch(key, cx);
        }
        if !can_read {
            self.error = Some("This device does not have orchestration:read permission.".into());
        } else if !source_exists {
            self.error = Some("The source thread is no longer available.".into());
        }
        if branch_commits_key.as_ref() != self.branch_commits_key.as_ref() {
            self.branch_commits = None;
            self.branch_commits_error = None;
        }
        if visible && connected && can_read && repository != Some(false) && branch_commits_key.is_some() && self.branch_commits_key != branch_commits_key {
            self.refresh_branch_commits(cx);
        }
        if changed {
            self.rebuild(cx);
            cx.notify();
        }
    }

    /// Whether the current request's source has something to show, even nothing (a cwd that is
    /// not a repository), possibly from an older revision while the current one is fetched.
    fn has_loaded(&self) -> bool {
        match (&self.request, &self.loaded) {
            (Some(request), Some((key, _))) => key.same_source(request),
            _ => false,
        }
    }

    fn cancel_fetch(&mut self) {
        self.fetch_generation += 1;
        self._fetch = None;
        self.patch_commands = None;
        self.pending = false;
    }

    fn fetch(&mut self, key: RequestKey, cx: &mut Context<Self>) {
        if matches!(key, RequestKey::Git { .. }) {
            self.fetch_git(key, cx);
            return;
        }
        let Some(task) = source::fetch(self.state.read(cx), &key) else {
            self.error = Some("Not connected to the server.".to_owned());
            return;
        };
        self.pending = true;
        let network = task.abort_handle();
        let connection = self.connection;
        let generation = self.fetch_generation;
        let completion = cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.connection != connection || this.fetch_generation != generation || !this.visible || this.request.as_ref() != Some(&key) {
                    return;
                }
                this.pending = false;
                match result {
                    Ok(loaded) => {
                        if let (RequestKey::Git { cwd, .. }, Loaded::Git { repository, .. }) = (&key, &loaded) {
                            this.git_fetched_at = Some(Instant::now());
                            this.repositories.insert(cwd.clone(), *repository);
                        }
                        this.cache.retain(|(cached, _)| !cached.same_source(&key));
                        this.cache.push((key.clone(), loaded.clone()));
                        if this.cache.len() > CACHE_LIMIT {
                            this.cache.remove(0);
                        }
                        this.loaded = Some((key, loaded));
                    }
                    Err(error) => this.error = Some(error),
                }
                this.rebuild(cx);
                cx.notify();
            });
        });
        self._fetch = Some(Fetch { network, _completion: completion });
    }

    fn fetch_git(&mut self, key: RequestKey, cx: &mut Context<Self>) {
        let RequestKey::Git { environment, cwd, kind, base_ref, commit_sha, ignore_whitespace, .. } = &key else { return };
        let Some(client) = self.state.read(cx).client_handle() else { return };
        let scope = t3_core::diff::ReviewScope { environment: environment.clone(), connection: self.connection.unwrap_or_default(), cwd: cwd.clone(), kind: *kind, base_ref: base_ref.clone(), ignore_whitespace: *ignore_whitespace };
        let previous = self.loaded.as_ref().filter(|(old, _)| old.same_source(&key)).and_then(|(_, loaded)| match loaded {
            Loaded::Git { review, .. } if review.scope == scope && review.commit_sha == *commit_sha => Some((**review).clone()),
            _ => None,
        });
        let (send, commands) = tokio::sync::mpsc::channel(16);
        let (updates, mut receive) = tokio::sync::mpsc::channel(1);
        let task = self.state.read(cx).runtime().spawn(source::run(client, scope, commit_sha.clone(), previous, self.patch_reads.clone(), commands, updates));
        self.patch_commands = Some(send);
        self.pending = true;
        let network = task.abort_handle();
        let generation = self.fetch_generation;
        let connection = self.connection;
        let completion = cx.spawn(async move |this, cx| {
            while let Some(result) = receive.recv().await {
                if this.update(cx, |this, cx| {
                    if this.connection != connection || this.fetch_generation != generation || !this.visible || this.request.as_ref() != Some(&key) { return; }
                    match result {
                        Ok(loaded) => {
                            if let Loaded::Git { review, repository, .. } = &loaded {
                                this.pending = review.incomplete && this.error.is_none();
                                let preview_changed = this.loaded.as_ref().is_none_or(|(_, old)| match old {
                                    Loaded::Git { review: previous, .. } => previous.scope != review.scope || previous.source != review.source,
                                    _ => true,
                                });
                                if preview_changed { this.git_fetched_at = Some(Instant::now()); }
                                this.repositories.insert(review.scope.cwd.clone(), *repository);
                            }
                            this.cache.retain(|(cached, _)| !cached.same_source(&key));
                            this.cache.push((key.clone(), loaded.clone()));
                            if this.cache.len() > CACHE_LIMIT { this.cache.remove(0); }
                            this.loaded = Some((key.clone(), loaded));
                        }
                        Err(error) => { this.error = Some(error); this.pending = false; }
                    }
                    this.rebuild(cx);
                    cx.notify();
                }).is_err() { break; }
            }
        });
        self._fetch = Some(Fetch { network, _completion: completion });
    }

    fn review(&self) -> Option<&t3_core::diff::ReviewModel> {
        if self.external.is_some() || !self.has_loaded() { return None; }
        match &self.loaded.as_ref()?.1 { Loaded::Git { review, .. } => Some(review), _ => None }
    }

    fn load_nearby(&mut self, cx: &mut Context<Self>) {
        if !self.review().is_some_and(|r| r.lazy()) { return; }
        if view::seed_height_estimates(&self.list, self.list.viewport_bounds().size.width, &mut self.list_width) { cx.notify(); }
        let Some(review) = self.review() else { return };
        let Some(file) = review.files().iter().find(|f| review.state(f).is_none_or(|s| s.pending)) else { return };
        let Some(view) = &self.view else { return };
        let Some(index) = view.parsed.files().iter().position(|f| f.path() == file.path) else { return };
        let Some(header) = view.header_row(index) else { return };
        let top = self.list.logical_scroll_top();
        let viewport = self.list.viewport_bounds();
        let measured = self.list.bounds_for_item(header).map(|row| f32::from(row.top() - viewport.top()));
        let estimate = self.list.offset_for_item(header) - self.list.offset_for_item(top.item_ix) - top.offset_in_item;
        if t3_core::diff::review_boundary_near(header, top.item_ix, measured, estimate.into(), viewport.size.height.into()) {
            self.patch_command(source::Command::Next);
        }
    }

    fn patch_command(&self, command: source::Command) {
        if let Some(send) = &self.patch_commands { let _ = send.try_send(command); }
    }

    /// The patch for the current scope: the loaded one, or the last one of the same source
    /// while a refresh is in flight.
    fn current(&self) -> Option<Arc<Parsed>> {
        if let Some(external) = &self.external {
            return external.parsed.clone();
        }
        if !self.has_loaded() {
            return None;
        }
        match (&self.loaded.as_ref()?.1, &self.scope) {
            (Loaded::Git { parsed, .. }, DiffScope::Branch { .. } | DiffScope::Unstaged | DiffScope::Commit { .. }) => Some(parsed.clone()),
            (Loaded::Turn(parsed), DiffScope::Turn { .. }) => Some(parsed.clone()),
            (Loaded::ToolCall(call), DiffScope::ToolCall(_)) => call.parsed(self.ignore_whitespace),
            _ => None,
        }
    }

    /// The loaded tool call for the scope on screen.
    fn tool_call(&self) -> Option<Arc<source::ToolCall>> {
        match &self.loaded.as_ref().filter(|_| self.has_loaded())?.1 {
            Loaded::ToolCall(call) => Some(call.clone()),
            _ => None,
        }
    }

    /// `reviewSectionId`: folds are remembered per thread and scope.
    fn scope_key(&self) -> String {
        if let Some(external) = &self.external {
            return external.key.clone();
        }
        let section = match &self.scope {
            DiffScope::Branch { .. } => "branch".to_owned(),
            DiffScope::Unstaged => "unstaged".to_owned(),
            DiffScope::Commit { sha, .. } => format!("commit:{sha}"),
            DiffScope::Turn { summary_index, .. } => format!("turn:{}", self.summaries[*summary_index].run_id),
            DiffScope::NoTurns => "none".to_owned(),
            DiffScope::ToolCall(target) => format!("tool-call:{}:{}", target.thread_id, target.item_id),
        };
        format!("{}:{section}", self.thread_id.as_deref().unwrap_or_default())
    }

    fn sync_scope_for_rebuild(&mut self) -> String {
        let scope_key = self.scope_key();
        if self.comments.scope != scope_key {
            self.comments = comments::Comments::default();
            self.comments.scope = scope_key.clone();
        }
        if self.scroll_restore.current_scope() != Some(scope_key.as_str()) {
            self.file_reveal.reset();
            self.revealed_path = None;
        }
        if let DiffSelection::Turn { file_path: Some(path), reveal_request_id, .. } = &self.selection {
            self.file_reveal.request(&scope_key, *reveal_request_id, FileRevealTarget::file(path));
        }
        if self.expansion_scope_key.as_deref() != Some(scope_key.as_str()) {
            self.expansions.clear();
            self.expansion_scope_key = Some(scope_key.clone());
        }
        self.scroll_restore.sync_scope(&scope_key, self.file_reveal.has_pending());
        scope_key
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let scope_key = self.sync_scope_for_rebuild();
        let Some(parsed) = self.current() else {
            if self.view.take().is_some() {
                self.list.reset(0);
            }
            return;
        };

        let layout = crate::settings::client_settings(cx).diff_layout();
        let files_collapsed_default = crate::settings::client_settings(cx).diff_files_collapsed();
        let same = self.view.as_ref().is_some_and(|view| {
            Arc::ptr_eq(&view.parsed, &parsed) && view.scope_key == scope_key && view.layout == layout && view.files_collapsed_default == files_collapsed_default
        });
        if same {
            let hidden = self.hidden_files(parsed.files());
            if let Some(view) = self.view.as_mut()
                && view.set_hidden(hidden, &self.tree_collapsed)
            {
                self.list.reset_with_uniform_height(view.row_count(), px(lines::LINE_HEIGHT));
                cx.notify();
            }
            self.reveal_requested(cx);
            if self.scroll_restore.has_pending() {
                cx.notify();
            }
            return;
        }
        // A refresh of the scope on screen keeps the reader's place.
        let keep = self
            .view
            .as_ref()
            .filter(|view| view.scope_key == scope_key && view.layout == layout)
            .and_then(|view| {
                let top = self.list.logical_scroll_top();
                let file = view.row(top.item_ix)?.file();
                Some((view.key(file)?.to_owned(), top.item_ix - view.header_row(file)?, top.offset_in_item))
            });
        let review = self.review().is_some();
        let compatible = self.view.as_ref().is_some_and(|view| view.scope_key == scope_key && view.layout == layout && view.files_collapsed_default == files_collapsed_default);
        let changes = if compatible {
            self.view.as_mut().expect("compatible view").update(parsed, &self.session, &self.tree_collapsed, review)
        } else {
            self.view = Some(View::new(parsed, scope_key.clone(), layout, &self.session, files_collapsed_default, &self.tree_collapsed, review));
            None
        };
        let hidden = self.hidden_files(self.view.as_ref().expect("installed view").parsed.files());
        let hidden_changed = self.view.as_mut().expect("installed view").set_hidden(hidden, &self.tree_collapsed);
        let pr_folds_changed = self.apply_pr_folds_to_view();
        let view = self.view.as_ref().expect("installed view");
        if self.comments.editor.as_ref().is_some_and(|editor| !(0..view.parsed.files().len()).any(|i| view.key(i) == Some(editor.file_key.as_str()))) { self.comments.editor = None; }
        let count = view.row_count();
        match changes {
            Some(changes) if !hidden_changed && !pr_folds_changed => {
                view::apply_row_changes(&self.list, changes);
                let _ = self.list.clone().with_uniform_item_height(px(lines::LINE_HEIGHT));
            }
            _ => self.list.reset_with_uniform_height(count, px(lines::LINE_HEIGHT)),
        }
        let keep = keep.and_then(|(key, within, offset_in_item)| {
            let file = (0..view.parsed.files().len()).find(|&i| view.key(i) == Some(&key))?;
            let start = view.header_row(file)?;
            let end = view.header_row(file + 1).unwrap_or(count);
            Some(ListOffset { item_ix: start + within.min(end.saturating_sub(start + 1)), offset_in_item })
        });
        match keep {
            Some(top) => self.list.scroll_to(top),
            None => self.h_offset = px(0.),
        }
        self.top_file = view.row(0).map(|row| row.file());
        self.reveal_requested(cx);
    }

    /// Computes the GitHub-style fold state for every file in `self.view` from `self.pr_folds`
    /// and applies it without resetting the list (callers reset/splice as needed).
    fn apply_pr_folds_to_view(&mut self) -> bool {
        let viewed_enabled = self.viewed_enabled();
        let Some(pr_folds) = self.pr_folds.as_ref() else { return false };
        let viewed_tracker = &self.viewed_tracker;
        let Some(view) = self.view.as_mut() else { return false };
        let next: Vec<bool> = view
            .parsed
            .files()
            .iter()
            .enumerate()
            .map(|(i, file)| {
                let key = view.key(i).unwrap_or(file.path());
                let is_test = is_test_changed_file(file.path());
                let viewed = viewed_enabled && viewed_tracker.state_for(file.path()) == FileViewedState::Viewed;
                pr_folds.is_collapsed(key, is_test, viewed)
            })
            .collect();
        view.set_folds(next)
    }

    fn sync_pr_folds(&mut self, cx: &mut Context<Self>) {
        if self.apply_pr_folds_to_view()
            && let Some(view) = self.view.as_ref()
        {
            self.list.reset_with_uniform_height(view.row_count(), px(lines::LINE_HEIGHT));
            cx.notify();
        }
    }

    /// Folds or unfolds one file, splicing only its rows so the list keeps its place.
    fn set_folded(&mut self, file: usize, folded: bool, cx: &mut Context<Self>) {
        let Some(view) = self.view.as_mut() else { return };
        let Some(key) = view.key(file).map(str::to_owned) else { return };
        let is_test = view.parsed.files().get(file).is_some_and(|f| is_test_changed_file(f.path()));
        if let Some(pr_folds) = self.pr_folds.as_mut() {
            // Record explicit reader choice even if the file is already in that state, so
            // late-arriving Viewed marks cannot fold an explicitly opened/revealed file (`#37605`).
            pr_folds.set_choice(&key, is_test, Some(folded));
        }
        let change = view.set_folded(file, folded);
        self.session.set_file_fold(view.scope_key.clone(), key, folded, || view.folded_keys());
        if let Some((range, count)) = change {
            self.list.splice(range, count);
            cx.notify();
        }
    }

    /// Explicitly unfolds a file by path (used when an on-demand withheld diff finishes loading
    /// for the first time, `pingdotgg/t3code#37007`).
    pub fn unfold_path(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(file_ix) = self.view.as_ref().and_then(|view| {
            view.parsed.files().iter().position(|f| f.path() == path)
        }) else {
            return;
        };
        self.set_folded(file_ix, false, cx);
    }

    pub fn toggle_file(&mut self, file: usize, cx: &mut Context<Self>) {
        let unloaded = self.view.as_ref().and_then(|view| view.parsed.files().get(file)).is_some_and(|file| self.file_patch_state(file.path()).is_some_and(|state| state.files.is_none()));
        if let Some(path) = self.view.as_ref().and_then(|view| view.parsed.files().get(file)).map(|f| f.path().to_owned()) { self.patch_command(source::Command::File(path)); }
        let folded = self.is_collapsed(file);
        self.set_folded(file, if unloaded { false } else { !folded }, cx);
    }

    /// `toggleAllDiffFiles`: expands every file when all are collapsed, else collapses all.
    pub fn toggle_all_files(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.view.as_ref() else { return };
        let scope_key = view.scope_key.clone();
        let fold = !view.all_folded();
        if self.pr_folds.is_some() {
            let shown_test_keys: Option<Vec<String>> = self.session.hide_test_files.then(|| {
                view.parsed
                    .files()
                    .iter()
                    .enumerate()
                    .filter(|(i, f)| !view.is_hidden(*i) && is_test_changed_file(f.path()))
                    .filter_map(|(i, _)| view.key(i).map(str::to_owned))
                    .collect()
            });
            if let Some(pr_folds) = self.pr_folds.as_mut() {
                pr_folds.fold_all(
                    if fold { DiffFoldOverride::Folded } else { DiffFoldOverride::Expanded },
                    shown_test_keys.as_deref(),
                );
            }
            self.apply_pr_folds_to_view();
            if let Some(view) = self.view.as_ref() {
                self.session.remember_folds(scope_key.clone(), view.folded_keys());
                self.scroll_restore.discard(&scope_key);
                self.list.reset_with_uniform_height(view.row_count(), px(lines::LINE_HEIGHT));
            }
            cx.notify();
            return;
        }
        let Some(view) = self.view.as_mut() else { return };
        view.set_all_folded(fold);
        self.session.remember_folds(scope_key.clone(), view.folded_keys());
        self.scroll_restore.discard(&scope_key);
        self.list.reset_with_uniform_height(view.row_count(), px(lines::LINE_HEIGHT));
        cx.notify();
    }

    /// Runs `handler` after each scroll of the rows, for a detached panel that loads more as
    /// the reader nears the end.
    pub fn on_rows_scrolled(&self, handler: impl FnMut(&gpui::ListScrollEvent, &mut Window, &mut gpui::App) + 'static) {
        *self.external_scroll_handler.borrow_mut() = Some(Box::new(handler));
    }

    /// Whether the last row sits within `margin` below the viewport, as last laid out.
    pub fn near_end(&self, margin: Pixels) -> bool {
        let count = self.list.item_count();
        count > 0 && self.list.bounds_for_item(count - 1).is_some_and(|row| row.top() <= self.list.viewport_bounds().bottom() + margin)
    }

    pub fn is_external(&self) -> bool {
        self.external.is_some()
    }

    pub fn all_collapsed(&self) -> bool {
        self.view.as_ref().is_some_and(View::all_folded)
    }

    pub fn is_collapsed(&self, file: usize) -> bool {
        self.view.as_ref().is_some_and(|view| view.is_folded(file))
    }

    /// Reveals a file: unfolds it and scrolls its header to the top (`revealDiffFile`).
    pub fn reveal_file(&mut self, file: usize, cx: &mut Context<Self>) {
        let Some((path, scope_key)) = self.view.as_ref().and_then(|view| {
            view.parsed.files().get(file).map(|f| (f.path().to_owned(), view.scope_key.clone()))
        }) else {
            return;
        };
        self.file_reveal.clear_highlight();
        self.scroll_restore.discard(&scope_key);
        self.set_folded(file, false, cx);
        if let Some(index) = self.view.as_ref().and_then(|view| view.header_row(file)) {
            self.list.scroll_to(ListOffset { item_ix: index, offset_in_item: px(0.) });
            self.top_file = Some(file);
        }
        self.patch_command(source::Command::File(path.clone()));
        self.selected_file = Some(path);
        cx.notify();
    }

    /// Reveals a file path or line range (`useCodeViewFileReveal`), unfolding if collapsed and
    /// requesting lazy file patches if not loaded yet (host hook for `PR-*` in `#389`).
    #[allow(dead_code)]
    pub fn reveal_file_path(
        &mut self,
        request_id: u64,
        target: FileRevealTarget,
        cx: &mut Context<Self>,
    ) {
        let scope_key = self.scope_key();
        self.revealed_path = Some(target.path.clone());
        if let Some(files) = self.view.as_ref().map(|view| view.parsed.clone()) {
            let hidden = self.hidden_files(files.files());
            if let Some(view) = self.view.as_mut()
                && view.set_hidden(hidden, &self.tree_collapsed)
            {
                self.list.reset_with_uniform_height(view.row_count(), px(lines::LINE_HEIGHT));
            }
        }
        if self.file_reveal.request(&scope_key, request_id, target) {
            self.scroll_restore.discard(&scope_key);
            self.reveal_requested(cx);
            cx.notify();
        }
    }

    /// A turn's "files changed" link or `reveal_file_path` asked for a target: resolve once ready.
    fn reveal_requested(&mut self, cx: &mut Context<Self>) {
        if !self.file_reveal.has_pending() {
            return;
        }
        if let Some(pending_path) = self.file_reveal.pending_path().map(str::to_owned)
            && let Some(file_ix) = self.view.as_ref().and_then(|view| {
                view.parsed.files().iter().position(|f| t3_core::diff::file_matches_path(f, &pending_path))
            })
        {
            // Ensure the file is unfolded before resolving a line target inside its rows.
            self.set_folded(file_ix, false, cx);
            if self.file_patch_state(&pending_path).is_some_and(|state| state.files.is_none()) {
                self.patch_command(source::Command::File(pending_path.clone()));
            }
        }

        let action = {
            let Some(view) = self.view.as_ref() else { return };
            let patch_pending = !self.is_settled()
                || self
                    .file_reveal
                    .pending_path()
                    .and_then(|path| self.file_patch_state(path))
                    .is_some_and(|state| state.files.is_none() && (state.pending || state.error.is_none()));
            self.file_reveal.resolve_with(view.parsed.files(), patch_pending, |file_ix| {
                let base = view.header_row(file_ix).unwrap_or(0);
                let rows = view.file_rows_slice(file_ix).unwrap_or(&[]);
                (0, base, rows)
            })
        };

        match action {
            RevealAction::None => {}
            RevealAction::RequestPatch { path } => {
                self.patch_command(source::Command::File(path));
            }
            RevealAction::ScrollToFile { file, path } => {
                self.set_folded(file, false, cx);
                if let Some(index) = self.view.as_ref().and_then(|view| view.header_row(file)) {
                    self.list.scroll_to(ListOffset { item_ix: index, offset_in_item: px(0.) });
                    self.top_file = Some(file);
                }
                self.patch_command(source::Command::File(path.clone()));
                self.selected_file = Some(path);
                cx.notify();
            }
            RevealAction::ScrollToRow { file, path, row, .. } => {
                let viewport_h = f32::from(self.list.viewport_bounds().size.height);
                if viewport_h > 0.0 {
                    let row_top = f32::from(self.list.offset_for_item(row));
                    let center_lead = ((viewport_h - lines::LINE_HEIGHT) * 0.5).max(32.0);
                    let target_top = (row_top - center_lead).max(0.0);
                    self.list.set_offset_from_scrollbar(point(px(0.), px(-target_top)));
                } else {
                    let header = self.view.as_ref().and_then(|view| view.header_row(file)).unwrap_or(0);
                    let item_ix = row.saturating_sub(2).max(header);
                    self.list.scroll_to(ListOffset { item_ix, offset_in_item: px(0.) });
                }
                self.top_file = Some(file);
                self.selected_file = Some(path);
                cx.notify();
            }
        }
    }

    pub(super) fn cancel_navigation_on_interaction(&mut self) {
        self.file_reveal.cancel_on_user_interaction();
        self.scroll_restore.cancel_pending();
    }

    pub(super) fn is_revealed_line(&self, file: usize, line: &DiffLine, side: Option<Side>) -> bool {
        let Some(view) = &self.view else { return false };
        let Some(diff) = view.parsed.files().get(file) else { return false };
        let Some((start, end, target_side)) = self.file_reveal.highlight_for(diff.path()) else {
            return false;
        };
        let row_side = match (side, line.kind) {
            (Some(s), _) => Some(s),
            (None, t3_core::diff::LineKind::Deletion) => Some(Side::Deletions),
            (None, t3_core::diff::LineKind::Addition) => Some(Side::Additions),
            (None, t3_core::diff::LineKind::Context) => None,
        };
        if let (Some(req_side), Some(row_side)) = (target_side, row_side)
            && req_side != row_side
        {
            return false;
        }
        let Some(num) = line_number_for_side(line, target_side.or(row_side)) else {
            return false;
        };
        (start..=end).contains(&num)
    }

    fn is_settled(&self) -> bool {
        if let Some(external) = &self.external {
            return !external.pending;
        }
        if self.pending {
            return false;
        }
        if let Some(review) = self.review().filter(|review| review.lazy()) {
            return !review
                .files()
                .iter()
                .any(|file| review.state(file).is_none_or(|state| state.pending));
        }
        true
    }

    fn max_scroll_top_px(&self) -> Option<f32> {
        let viewport_h = f32::from(self.list.viewport_bounds().size.height);
        if viewport_h <= 0.0 {
            return None;
        }
        Some(f32::from(self.list.max_offset_for_scrollbar().y).max(0.0))
    }

    fn on_list_scrolled(&mut self) {
        if self.view.is_none() {
            return;
        }
        let scope_key = self.scope_key();
        let top = f32::from(-self.list.scroll_px_offset_for_scrollbar().y);
        self.scroll_restore.on_scroll(&scope_key, top);
    }

    pub(super) fn apply_pending_scroll_restore(&mut self, cx: &mut Context<Self>) {
        if self.file_reveal.has_pending() || !self.scroll_restore.has_pending() {
            return;
        }
        let settled = self.is_settled();
        let Some(max_top) = self.max_scroll_top_px() else {
            if !settled && self.review().is_some_and(|review| review.lazy()) {
                self.patch_command(source::Command::Next);
            }
            return;
        };
        let scope_key = self.scope_key();
        let need_more = match self.scroll_restore.apply(&scope_key, max_top, settled) {
            ScrollRestoreAction::None => false,
            ScrollRestoreAction::ScrollTo { offset_px, done } => {
                self.list.set_offset_from_scrollbar(point(px(0.), px(-offset_px)));
                cx.notify();
                !done
            }
            ScrollRestoreAction::WaitMore => true,
        };
        if need_more && !settled && self.review().is_some_and(|review| review.lazy()) {
            self.patch_command(source::Command::Next);
        }
    }

    pub fn select(&mut self, selection: DiffSelection, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.select_diff(selection, cx));
    }

    pub fn select_commit(&mut self, sha: &str, cx: &mut Context<Self>) {
        let remembered = self
            .thread_id
            .as_deref()
            .and_then(|id| self.state.read(cx).panels.branch_base_refs.get(id).cloned());
        let next = DiffSelection::commit(&self.selection, remembered.as_deref(), sha);
        self.select(next, cx);
    }

    pub fn commit_url(&self, sha: &str, cx: &gpui::App) -> Option<String> {
        let thread_id = self.thread_id.as_deref()?;
        let state = self.state.read(cx);
        let thread = state.thread_shell(thread_id)?;
        let project = state.shell.project(&thread.project_id)?;
        project_commit_url(project.repository_identity.as_ref(), sha)
    }

    pub fn refresh_branch_commits(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id.as_deref() else { return };
        let state = self.state.read(cx);
        if !state.can_read_threads() || !matches!(state.phase, Phase::Online(ConnectionStatus::Connected)) {
            return;
        }
        let Some(environment) = state.environment().map(str::to_owned) else { return };
        let Some(cwd) = state.thread_shell(thread_id).and_then(|thread| {
            thread.worktree_path.clone().or_else(|| state.shell.project(&thread.project_id).map(|project| project.workspace_root.clone()))
        }) else {
            return;
        };
        if self.repositories.get(&cwd) == Some(&false) {
            return;
        }
        let base_ref = state.panels.branch_base_refs.get(thread_id).cloned();
        let key = (environment, cwd.clone(), base_ref.clone());
        if self.branch_commits_loading && self.branch_commits_key.as_ref() == Some(&key) {
            return;
        }
        let Some(client) = state.client_handle() else { return };
        let runtime = state.runtime().clone();
        self.branch_commits_key = Some(key.clone());
        self.branch_commits_loading = true;
        self.branch_commits_error = None;
        self.branch_commits_generation += 1;
        let generation = self.branch_commits_generation;
        let (method, payload) = branch_commits_request(&cwd, base_ref.as_deref());
        let task = runtime.spawn(async move {
            let value = client.request(method, payload).await.map_err(|error| error.to_string())?;
            decode_branch_commits(&value)
        });
        self._branch_commits = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.branch_commits_generation != generation || this.branch_commits_key.as_ref() != Some(&key) {
                    return;
                }
                this.branch_commits_loading = false;
                match result {
                    Ok(commits) => {
                        this.branch_commits = Some(commits);
                        this.branch_commits_error = None;
                    }
                    Err(error) => this.branch_commits_error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.expansions.clear();
        self.git_revision += 1;
        self.error = None;
        self.sync(cx);
    }

    /// Configures on-demand withheld-file state and cut-line files (`pingdotgg/t3code#37007`).
    pub fn set_withheld_state(
        &mut self,
        withheld: HashMap<String, WithheldFileStatus>,
        readable: bool,
        cut_line_files: HashSet<String>,
        cx: &mut Context<Self>,
    ) {
        self.withheld_files = withheld;
        self.withheld_readable = readable;
        self.cut_line_files = cut_line_files;
        if let Some(view) = &self.view {
            self.list.remeasure_items(0..view.row_count());
        }
        cx.notify();
    }

    pub fn set_on_load_withheld(&mut self, callback: impl Fn(String, &mut gpui::App) + 'static) {
        self.on_load_withheld = Some(Rc::new(callback));
    }

    pub(super) fn withheld_status(&self, path: &str) -> Option<&WithheldFileStatus> {
        self.withheld_files.get(path)
    }

    pub(super) fn trigger_load_withheld(&mut self, path: String, cx: &mut Context<Self>) {
        if let Some(callback) = self.on_load_withheld.clone() {
            callback(path, cx);
        }
    }

    /// Whether hidden hunk context expansion is available for `file_ix` in the active scope.
    pub(super) fn can_expand_file(&self, file_ix: usize) -> bool {
        if self.external.is_some() {
            return false;
        }
        let Some(file) = self.view.as_ref().and_then(|view| view.parsed.files().get(file_ix)) else {
            return false;
        };
        if file.binary || file.hunks.is_empty() || self.cut_line_files.contains(file.path()) {
            return false;
        }
        matches!(self.scope, DiffScope::Branch { .. } | DiffScope::Unstaged | DiffScope::Commit { .. })
            && matches!(self.request, Some(RequestKey::Git { .. }))
    }

    pub(super) fn expansion_state(&self, file_ix: usize) -> Option<&FileExpansionState> {
        let key = self.view.as_ref()?.key(file_ix)?;
        self.expansions.get(key)
    }

    /// Expands hidden context lines at separator `hunk_ix` for `file_ix`, fetching
    /// `review.getDiffFileContents` on first expansion and validating `reproduces_patch`.
    pub fn expand_hunk(
        &mut self,
        file_ix: usize,
        hunk_ix: usize,
        direction: ExpandDirection,
        count: u32,
        cx: &mut Context<Self>,
    ) {
        if !self.can_expand_file(file_ix) {
            return;
        }
        let Some((file_key, file)) = self.view.as_ref().and_then(|view| {
            Some((view.key(file_ix)?.to_owned(), view.parsed.files().get(file_ix)?.clone()))
        }) else {
            return;
        };
        if let Some(contents) = self.expansions.get(&file_key).and_then(|state| state.contents.clone()) {
            self.apply_hunk_expansion(file_ix, &file_key, &file, &contents, hunk_ix, direction, count, cx);
            return;
        }
        if self.expansions.get(&file_key).is_some_and(|state| state.loading) {
            return;
        }
        let Some(RequestKey::Git { cwd, kind, base_ref, commit_sha, .. }) = self.request.clone() else {
            return;
        };
        let (resolved_base, resolved_head, source_kind) = match self.review().and_then(|r| r.source.as_ref()) {
            Some(source) => (source.base_ref.clone(), source.head_ref.clone(), source.kind),
            None => (
                base_ref.clone(),
                if kind == PreviewSourceKind::Commit { commit_sha.clone() } else { None },
                kind,
            ),
        };
        let (method, payload) = diff_file_contents_request(
            &cwd,
            source_kind,
            file.change,
            resolved_base.as_deref(),
            resolved_head.as_deref(),
            file.previous_path(),
            file.path(),
        );
        let Some(task) = self.state.read(cx).request(method, payload, |value| decode_diff_file_contents(&value)) else {
            return;
        };
        let scope_key = self.scope_key();
        let entry = self.expansions.entry(file_key.clone()).or_default();
        entry.loading = true;
        entry.error = None;
        let key_for_task = file_key.clone();
        entry._task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Failed to load file contents.".into()));
            let _ = this.update(cx, |panel, cx| {
                if panel.scope_key() != scope_key {
                    return;
                }
                let Some(current_ix) = panel
                    .view
                    .as_ref()
                    .and_then(|view| (0..view.parsed.files().len()).find(|&i| view.key(i) == Some(key_for_task.as_str())))
                else {
                    return;
                };
                let Some(current_file) = panel.view.as_ref().and_then(|view| view.parsed.files().get(current_ix).cloned()) else {
                    return;
                };
                match result {
                    Ok(contents) => {
                        if let Some(entry) = panel.expansions.get_mut(&key_for_task) {
                            entry.loading = false;
                            entry.error = None;
                            entry.contents = Some(contents.clone());
                        }
                        panel.apply_hunk_expansion(current_ix, &key_for_task, &current_file, &contents, hunk_ix, direction, count, cx);
                    }
                    Err(error) => {
                        if let Some(entry) = panel.expansions.get_mut(&key_for_task) {
                            entry.loading = false;
                            entry.error = Some(error.clone());
                        }
                        panel.state.update(cx, |state, cx| {
                            state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Error, "Unable to expand diff").description(error), cx);
                        });
                        cx.notify();
                    }
                }
            });
        }));
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_hunk_expansion(
        &mut self,
        file_ix: usize,
        file_key: &str,
        file: &t3_core::diff::FileDiff,
        contents: &DiffFileContents,
        hunk_ix: usize,
        direction: ExpandDirection,
        count: u32,
        cx: &mut Context<Self>,
    ) {
        match expand_hunk_context(file, contents, hunk_ix, direction, count.max(1)) {
            Ok(expanded) => {
                let trailing = trailing_hidden_lines(&expanded, contents);
                if let Some(view) = self.view.as_mut()
                    && let Some((range, new_count)) = view.replace_expanded_file(file_ix, expanded, trailing)
                {
                    self.list.splice(range, new_count);
                }
                if let Some(entry) = self.expansions.get_mut(file_key) {
                    entry.error = None;
                }
                cx.notify();
            }
            Err(error) => {
                if let Some(entry) = self.expansions.get_mut(file_key) {
                    entry.error = Some(error.clone());
                    entry.contents = None;
                }
                self.state.update(cx, |state, cx| {
                    state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Error, "Unable to expand diff").description(error), cx);
                });
                cx.notify();
            }
        }
    }

    /// Configures revision-tracked PR viewed state on this panel (`pingdotgg/t3code#35114`).
    /// Flushes any pending debounced Viewed presses for the previous target before switching scopes.
    #[allow(dead_code)]
    pub fn set_viewed_target(
        &mut self,
        target: Option<ViewedTarget>,
        states: HashMap<String, FileViewedState>,
        cx: &mut Context<Self>,
    ) {
        if self.viewed_target != target {
            if let Some(prev) = self.viewed_target.take()
                && prev.enabled
                && let Some((_, batch)) = self.viewed_tracker.take_batch()
            {
                let (method, payload) = set_files_viewed_payload(
                    &prev.project_id,
                    prev.number,
                    prev.host.as_deref(),
                    prev.repository.as_deref(),
                    batch,
                );
                let _ = self.state.read(cx).request(method, payload, |_| Ok(()));
            }
            self.viewed_target = target;
            self.viewed_tracker = FilesViewedTracker::default();
            self.pr_folds = self
                .viewed_target
                .as_ref()
                .filter(|t| t.enabled)
                .map(|_| PullRequestDiffFolds::default());
            self.last_viewed_file_key = None;
            self._viewed_flush = None;
        }
        self.viewed_tracker.set_server_states(states);
        self.sync_pr_folds(cx);
        cx.notify();
    }

    pub(super) fn viewed_enabled(&self) -> bool {
        self.viewed_target.as_ref().is_some_and(|target| target.enabled)
    }

    pub(super) fn file_viewed_state(&self, path: &str) -> FileViewedState {
        self.viewed_tracker.state_for(path)
    }

    /// Toggles Viewed for `file_ix` (or Shift-click range from the last toggled file), auto-folding
    /// viewed files and queuing a debounced revision-tracked `pullRequests.setFilesViewed` batch.
    pub fn toggle_file_viewed(&mut self, file_ix: usize, extend_range: bool, cx: &mut Context<Self>) {
        let Some(target) = self.viewed_target.as_ref().filter(|t| t.enabled).cloned() else {
            return;
        };
        let Some((clicked_key, next_viewed, file_keys)) = self.view.as_ref().and_then(|view| {
            let file = view.parsed.files().get(file_ix)?;
            let clicked_key = view.key(file_ix)?.to_owned();
            let next_viewed = !self.viewed_tracker.is_viewed(file.path());
            let keys: Vec<String> = (0..view.parsed.files().len())
                .filter_map(|i| view.key(i).map(str::to_owned))
                .collect();
            Some((clicked_key, next_viewed, keys))
        }) else {
            return;
        };
        let indices = viewed_range_indices(
            &file_keys,
            &clicked_key,
            extend_range.then_some(self.last_viewed_file_key.as_deref()).flatten(),
        );
        self.last_viewed_file_key = Some(clicked_key);
        let commit_scope = target.commit.is_some();
        for ix in indices {
            let Some((path, key, is_test, revision)) = self.view.as_ref().and_then(|view| {
                let file = view.parsed.files().get(ix)?;
                let key = view.key(ix)?.to_owned();
                let is_test = is_test_changed_file(file.path());
                // Commit diffs name that commit's blob, not the PR head's (`pingdotgg/t3code#35114`).
                let rev = (!commit_scope).then(|| file.new_object_id().map(str::to_owned)).flatten();
                Some((file.path().to_owned(), key, is_test, rev))
            }) else {
                continue;
            };
            self.viewed_tracker.set_viewed(&path, next_viewed, revision);
            if let Some(pr_folds) = self.pr_folds.as_mut() {
                pr_folds.on_viewed_toggled(&key, is_test, next_viewed);
                if let Some(view) = self.view.as_mut()
                    && let Some((range, count)) = view.set_folded(ix, next_viewed)
                {
                    self.list.splice(range, count);
                }
            } else {
                self.set_folded(ix, next_viewed, cx);
            }
        }
        self.schedule_viewed_flush(target, cx);
        cx.notify();
    }

    fn schedule_viewed_flush(&mut self, target: ViewedTarget, cx: &mut Context<Self>) {
        self._viewed_flush = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(FILES_VIEWED_FLUSH_DELAY_MS)).await;
            let request = this.update(cx, |panel, cx| {
                if panel.viewed_target.as_ref() != Some(&target) {
                    return None;
                }
                let (seq, batch) = panel.viewed_tracker.take_batch()?;
                let (method, payload) = set_files_viewed_payload(
                    &target.project_id,
                    target.number,
                    target.host.as_deref(),
                    target.repository.as_deref(),
                    batch,
                );
                Some((seq, panel.state.read(cx).rpc(method, payload)))
            });
            let Ok(Some((seq, rpc))) = request else { return };
            let ok = rpc.await.is_ok();
            let _ = this.update(cx, |panel, cx| {
                if panel.viewed_target.as_ref() == Some(&target) {
                    panel.viewed_tracker.finish_batch(seq, ok);
                    cx.notify();
                }
            });
        }));
    }

    /// Sets inline PR review conversations on this diff panel, folding resolved conversations into
    /// line-level author avatars (`pingdotgg/t3code#34644`).
    #[allow(dead_code)]
    pub fn set_review_threads(&mut self, threads: Vec<InlineReviewThread>, cx: &mut Context<Self>) {
        self.review_threads.set_threads(threads);
        if let Some(view) = &self.view {
            self.list.remeasure_items(0..view.row_count());
        }
        cx.notify();
    }

    pub fn toggle_resolved_review_threads(&mut self, file_ix: usize, side: Side, line: u32, cx: &mut Context<Self>) {
        let Some((file, layout, row_count)) = self.view.as_ref().and_then(|view| {
            Some((view.parsed.files().get(file_ix)?.clone(), view.layout, view.row_count()))
        }) else {
            return;
        };
        if self.review_threads.toggle_resolved(&file, layout, side, line) {
            self.list.remeasure_items(0..row_count);
            cx.notify();
        }
    }

    pub fn toggle_wrap(&mut self, cx: &mut Context<Self>) {
        self.wrap = !self.wrap;
        self.wrap_override = Some(self.wrap);
        self.h_offset = px(0.);
        self.list.remeasure();
        cx.notify();
    }

    pub fn toggle_whitespace(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id.as_deref() else { return };
        self.ignore_whitespace = self.session.toggle_whitespace(thread_id, crate::settings::client_settings(cx).diff_ignore_whitespace());
        self.sync(cx);
        // A tool call hides whitespace itself, so its request stays the same.
        self.rebuild(cx);
        cx.notify();
    }

    pub fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        crate::settings::store::update_client(&self.state, t3_core::client_settings::ClientSettingChange::DiffLayout(layout), cx);
    }

    pub fn toggle_tree(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let open = state.panels.diff_file_tree_open;
            state.set_diff_file_tree_open(!open, cx);
        });
    }

    pub fn toggle_directory(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.tree_collapsed.remove(path) {
            self.tree_collapsed.insert(path.to_owned());
        }
        if let Some(view) = self.view.as_mut() {
            view.rebuild_tree(&self.tree_collapsed);
        }
        cx.notify();
    }

    /// `isFileShown`: with "Hide test files" on, test files are left out, except the one another
    /// view asked to reveal (`pingdotgg/t3code#39213`).
    fn hidden_files(&self, files: &[t3_core::diff::FileDiff]) -> Vec<bool> {
        let keep = match &self.selection {
            DiffSelection::Turn { file_path: Some(path), .. } if self.external.is_none() => Some(path.as_str()),
            _ => self.revealed_path.as_deref(),
        };
        files.iter().map(|file| self.session.hide_test_files && keep != Some(file.path()) && is_test_changed_file(file.path())).collect()
    }

    /// `testFileCount`: the toggle shows only when the patch has test files.
    pub fn test_file_count(&self) -> usize {
        self.view.as_ref().map_or(0, |view| view.parsed.files().iter().filter(|file| t3_core::diff::is_test_changed_file(file.path())).count())
    }

    pub fn hides_test_files(&self) -> bool {
        self.session.hide_test_files
    }

    pub fn toggle_hide_test_files(&mut self, cx: &mut Context<Self>) {
        self.session.hide_test_files = !self.session.hide_test_files;
        let Some(files) = self.view.as_ref().map(|view| view.parsed.clone()) else { cx.notify(); return };
        let hidden = self.hidden_files(files.files());
        let Some(view) = self.view.as_mut() else { return };
        if view.set_hidden(hidden, &self.tree_collapsed) {
            self.scroll_restore.discard(&view.scope_key);
            self.list.reset_with_uniform_height(view.row_count(), px(lines::LINE_HEIGHT));
        }
        cx.notify();
    }

    pub fn copy_path(&mut self, file: usize, cx: &mut Context<Self>) {
        let Some(path) = self.view.as_ref().and_then(|view| view.parsed.files().get(file)).map(|file| file.path().to_owned()) else { return };
        let (workspace_root, _) = self.workspace_roots(cx);
        let formatted = format_diff_path_for_clipboard(&path, workspace_root.as_deref());
        cx.write_to_clipboard(ClipboardItem::new_string(formatted));
        self.copied_generation += 1;
        let generation = self.copied_generation;
        self.copied_path = Some((path, generation));
        self._copied_reset = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1000)).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied_generation == generation {
                    this.copied_path = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    pub(super) fn is_path_copied(&self, path: &str) -> bool {
        self.copied_path.as_ref().is_some_and(|(copied, _)| copied == path)
    }

    fn workspace_roots(&self, cx: &gpui::App) -> (Option<String>, Option<String>) {
        let Some(thread_id) = self.thread_id.as_deref() else { return (None, None) };
        let state = self.state.read(cx);
        let Some(thread) = state.thread_shell(thread_id) else { return (None, None) };
        let project = state.shell.project(&thread.project_id);
        let workspace_root = thread.worktree_path.clone().or_else(|| project.map(|project| project.workspace_root.clone()));
        // A worktree is its own repository root; tool-call diffs are already workspace-relative
        // (`pingdotgg/t3code#32684`), so skip repository root re-basing for them.
        let repository_root = if matches!(self.scope, DiffScope::ToolCall(_)) {
            None
        } else {
            match (&thread.worktree_path, project) {
                (None, Some(project)) => project.repository_identity.as_ref().and_then(|identity| identity.get("rootPath")).and_then(|root| root.as_str()).map(str::to_owned),
                _ => None,
            }
        };
        (workspace_root, repository_root)
    }

    /// `openDiffFilePrimaryAction`: a file title opens that file in a file tab, resolved against
    /// the thread's workspace. Paths outside it (another package of the repository, `..`, an
    /// absolute path elsewhere) do nothing.
    pub fn open_file(&mut self, file: usize, cx: &mut Context<Self>) {
        if self.external.is_some() { return; }
        let Some(path) = self.view.as_ref().and_then(|view| view.parsed.files().get(file)).map(|file| file.path().to_owned()) else { return };
        let (workspace_root, repository_root) = self.workspace_roots(cx);
        let Some(relative) = t3_core::diff::resolve_diff_path_for_workspace(&path, workspace_root.as_deref(), repository_root.as_deref()) else { return };
        self.state.update(cx, |state, cx| state.open_file(&relative, None, cx));
    }

    pub(super) fn file_context_menu(&self, file_ix: usize, cx: &gpui::App) -> Option<(String, FileContextMenuCapabilities)> {
        if self.external.is_some() { return None; }
        let path = self.view.as_ref()?.parsed.files().get(file_ix)?.path().to_owned();
        let (workspace_root, repository_root) = self.workspace_roots(cx);
        let absolute_path = resolve_file_context_menu_absolute_path(&path, workspace_root.as_deref(), repository_root.as_deref())?;
        let state = self.state.read(cx);
        let config = state.server_config.config.as_ref()?;
        let os = config.environment.platform.as_ref().map_or("unknown", |platform| platform.os.as_str());
        let capabilities = FileContextMenuCapabilities::from_server(
            &config.available_editors,
            os,
            config.shell_reveal_in_file_manager,
            config.shell_reveal_in_file_manager_kind.as_deref(),
        );
        (!capabilities.is_empty()).then_some((absolute_path, capabilities))
    }

    pub(super) fn activate_file_context_menu(&mut self, action: FileContextMenuAction, absolute_path: String, cx: &mut Context<Self>) {
        let (method, payload) = file_context_menu_open_request(&absolute_path, &action);
        let Some(request) = self.state.update(cx, |state, _| Some(state.rpc(method, payload))) else { return };
        let state = self.state.clone();
        cx.spawn(async move |_, cx| {
            if request.await.is_err() {
                let title = file_context_menu_error_title(&action);
                state.update(cx, |state, cx| {
                    state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Error, title).description(absolute_path), cx);
                });
            }
        })
        .detach();
    }

    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let message = |text: &str| centered_message(text, p);
        if let Some(external) = &self.external
            && self.view.is_none()
        {
            return match (&external.error, external.pending) {
                (Some(error), _) => message(error),
                (None, true) => lines::loading_state(cx).into_any_element(),
                (None, false) => message("No patch available for this selection."),
            };
        }
        if self.thread_id.is_none() && self.external.is_none() {
            return message("Select a thread to inspect turn diffs.");
        }
        if matches!(self.scope, DiffScope::ToolCall(_)) {
            return self.render_tool_call(window, cx);
        }
        if self.is_outside_repository(cx) {
            return message("Turn diffs are unavailable because this project is not a git repository.");
        }
        if self.scope == DiffScope::NoTurns {
            return message("No completed turns yet.");
        }
        let Some(parsed) = self.view.as_ref().map(|view| view.parsed.clone()) else {
            if self.pending || (self.request.is_some() && self.error.is_none() && !self.has_loaded()) {
                return lines::loading_state(cx).into_any_element();
            }
            return v_flex()
                .flex_1()
                .children(self.error.clone().map(|error| error_line(error, p)))
                .child(message("No patch available for this selection."))
                .into_any_element();
        };
        match &parsed.patch {
            ParsedPatch::Empty if self.review().is_some_and(|review| review.incomplete) => {
                v_flex().flex_1().child(notice(if self.error.is_some() { "Showing tracked changes. Untracked changes could not be loaded. Refresh to retry." } else { "Showing tracked changes. Checking untracked files..." }, p)).child(if self.error.is_some() { message("No patch available for this selection.") } else { lines::loading_state(cx).into_any_element() }).into_any_element()
            }
            ParsedPatch::Empty => v_flex().flex_1().children(self.error.clone().map(|error| error_line(error, p))).child(message("No net changes in this selection.")).into_any_element(),
            ParsedPatch::Raw { text, reason } => lines::raw_patch(text, reason, self.wrap, cx).into_any_element(),
            ParsedPatch::Files(_) => self.render_files(&parsed, window, cx),
        }
    }

    /// `!isGitRepo`: the thread's cwd answered a git preview with no sources.
    fn is_outside_repository(&self, cx: &gpui::App) -> bool {
        let Some(thread_id) = self.thread_id.as_deref() else { return false };
        let state = self.state.read(cx);
        let cwd = state.thread_shell(thread_id).and_then(|thread| {
            thread.worktree_path.as_deref().or_else(|| state.shell.project(&thread.project_id).map(|project| project.workspace_root.as_str()))
        });
        cwd.and_then(|cwd| self.repositories.get(cwd)) == Some(&false)
    }

    fn file_patch_state(&self, path: &str) -> Option<t3_core::diff::FilePatchState> {
        let review = self.review().filter(|review| review.lazy())?;
        let file = review.files().iter().find(|file| file.path == path)?;
        Some(review.state(file).cloned().unwrap_or_default())
    }

    fn render_files(&mut self, parsed: &Parsed, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let truncated = parsed.truncated.then(|| {
            let suffix = if parsed.totals.is_some() { " Totals include all changes." } else { "" };
            notice(format!("This preview exceeds the size limit. Changes shown are incomplete.{suffix}"), p)
        });
        let incomplete = self.review().is_some_and(|review| review.incomplete).then(|| notice(if self.error.is_some() { "Showing tracked changes. Untracked changes could not be loaded. Refresh to retry." } else { "Showing tracked changes. Checking untracked files..." }, p));
        let error = self.error.clone().filter(|_| incomplete.is_none()).map(|error| error_line(error, p));
        v_flex().flex_1().min_h_0().children(incomplete).children(error).children(truncated).child(self.files_body(window, cx)).into_any_element()
    }

    fn files_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let tree_open = self.state.read(cx).panels.diff_file_tree_open;
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(self.render_list(window, cx))
            .when(tree_open, |this| this.child(tree::render(self, cx)))
    }

    /// The tool-call scope (`toolCallSelection` in `DiffPanel.tsx`). It reads no repository, so
    /// it works outside one; the recorded content shows when the provider left no diff.
    fn render_tool_call(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let call = self.tool_call();
        // `getRenderablePatch` treats a blank patch as none.
        let parsed = self.view.as_ref().map(|view| view.parsed.clone()).filter(|parsed| parsed.patch != ParsedPatch::Empty);
        let truncated = call.as_ref().is_some_and(|call| call.diff.truncated).then(|| notice("This preview exceeds the size limit. Changes shown are incomplete.", p));
        let omitted = match (&call, parsed.as_deref().map(|parsed| &parsed.patch)) {
            (Some(call), Some(ParsedPatch::Files(files))) => Some((call.diff.recorded_files, call.diff.omitted_files(files.len()))),
            _ => None,
        };
        let omitted = omitted.filter(|(_, omitted)| *omitted > 0).map(|(recorded, omitted)| {
            notice(format!("This tool call changed {recorded} files. The recorded patch omits {omitted} of them."), p)
        });
        let error = self.error.clone().filter(|_| parsed.is_none()).map(|error| error_line(error, p));
        let body = match parsed.as_deref().map(|parsed| &parsed.patch) {
            Some(ParsedPatch::Files(_)) => self.files_body(window, cx).into_any_element(),
            Some(ParsedPatch::Raw { text, reason }) => lines::raw_patch(text, reason, self.wrap, cx).into_any_element(),
            Some(ParsedPatch::Empty) | None => match call.as_ref().filter(|call| !call.diff.content.is_empty()) {
                Some(call) => lines::recorded_content(&call.diff.content, self.wrap, cx).into_any_element(),
                None if self.pending || (self.error.is_none() && !self.has_loaded()) => lines::loading_state(cx).into_any_element(),
                // A blank patch parses to `Empty`; a missing one leaves no view.
                None if self.view.is_some() => centered_message("No net changes in this selection.", p),
                None => centered_message("No patch available for this selection.", p),
            },
        };
        v_flex().flex_1().min_h_0().children(truncated).children(omitted).children(error).child(body).into_any_element()
    }
}

fn centered_message(text: &str, p: Palette) -> gpui::AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(px(20.))
        .text_center()
        .text_xs()
        .text_color(hsla(p.muted_foreground).opacity(0.7))
        .child(text.to_owned())
        .into_any_element()
}

/// The strip above the files that says what the patch leaves out.
fn notice(text: impl Into<gpui::SharedString>, p: Palette) -> gpui::Div {
    div()
        .flex_none()
        .border_b_1()
        .border_color(hsla(p.border).opacity(0.7))
        .bg(hsla(p.muted).opacity(0.4))
        .px(px(12.))
        .py(px(6.))
        .text_size(px(11.))
        .text_color(hsla(p.muted_foreground))
        .child(text.into())
}

fn error_line(error: String, p: Palette) -> gpui::Div {
    div().px(px(12.)).mb(px(8.)).text_size(px(11.)).text_color(hsla(p.error).opacity(0.8)).child(error)
}


impl Render for DiffPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wrap = self.wrap_override.unwrap_or(crate::theme::preferences(cx).typography.word_wrap);
        if self.wrap != wrap {
            self.wrap = wrap;
            self.h_offset = px(0.);
            self.list.remeasure();
        }
        let p = palette(cx);
        v_flex()
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .child(header::render(self, window, cx))
            .child(self.render_body(window, cx))
    }
}

#[cfg(test)]
mod tests;
