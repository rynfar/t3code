//! Source control for the open thread: its live status (`subscribeVcsStatus`), the running Git
//! action and its progress, the branch picker's refs, and the thread terminals project scripts
//! run in. Ports the state behind `GitActionsControl.tsx`, `BranchToolbarBranchSelector.tsx`
//! and `useChatTerminalActions.ts` (`client-runtime/src/state/vcs.ts`, `vcsAction.ts`).
//! Wire work and decoding run on the network runtime; this entity only applies results.

mod actions;
mod branch_picker;
mod dialogs;

use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Subscription, Task};
use serde::de::DeserializeOwned;
use serde_json::Value;
use t3_client::session::RpcError;
use t3_client::supervisor::ClientHandle;
use t3_core::git_actions::{self, ActionProgress, Operation, ProgressEvent, RunRequest, RunResult, StackedAction, terminology};
use t3_core::project_scripts::{self, ProjectScript, TerminalMetadataEvent, Terminals};
use t3_core::vcs::{self, ListRefsResult, PullResult, RefNameResult, VcsRef, VcsStatus, VcsStatusEvent, apply_status_event, method};
use tokio::sync::mpsc;

pub use actions::{changes_row, git_actions_row};
pub use branch_picker::BranchPicker;
pub use dialogs::GitDialogs;

use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::sidebar::controller::now_ms;
use crate::toast::{Toast, ToastKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitEvent {
    Changed,
}

/// The open thread's checkout: where Git commands run and the thread they update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub thread_id: String,
    pub cwd: String,
    pub project_cwd: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
}

/// The inline result that replaces the Git action button for ten seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSuccess {
    pub title: String,
    pub description: Option<String>,
}

/// One loaded listing of the branch picker: every page fetched for `query`.
#[derive(Debug, Clone, Default)]
pub struct Refs {
    pub query: String,
    pub refs: Vec<VcsRef>,
    pub next_cursor: Option<u64>,
    pub total: u64,
    pub loading: bool,
    pub loaded: bool,
    pub error: Option<String>,
}

/// A network task and the UI task that applies what it sends; dropping it stops both.
struct Feed {
    net: tokio::task::JoinHandle<()>,
    _ui: Task<()>,
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.net.abort();
    }
}

pub struct GitStore {
    state: Entity<AppState>,
    pub target: Option<Target>,
    pub status: Option<VcsStatus>,
    pub status_error: Option<String>,
    pub progress: Option<ActionProgress>,
    pub success: Option<InlineSuccess>,
    /// The branch shown while a switch or create is in flight (`useOptimistic`).
    pub optimistic_branch: Option<Option<String>>,
    pub branch_pending: bool,
    pub initializing: bool,
    pub refs: Refs,
    pub terminals: Terminals,
    status_feed: Option<Feed>,
    terminal_feed: Option<Feed>,
    refs_task: Option<Task<()>>,
    success_timer: Option<Task<()>>,
    tick: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<GitEvent> for GitStore {}

const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Keeps one streaming RPC open, decoding each chunk on the network runtime and resubscribing
/// with backoff when it ends (a reconnect ends every stream).
fn follow<T: DeserializeOwned + Send + 'static>(client: ClientHandle, runtime: &tokio::runtime::Handle, rpc: &'static str, payload: Value) -> (tokio::task::JoinHandle<()>, mpsc::UnboundedReceiver<Result<T, String>>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let net = runtime.spawn(async move {
        let mut delay = RETRY_MIN;
        loop {
            if let Ok(mut stream) = client.stream(rpc, payload.clone()).await {
                while let Some(item) = stream.receiver.recv().await {
                    let decoded = match item {
                        Ok(value) => serde_json::from_value::<T>(value).map_err(|error| error.to_string()),
                        // The session went away; the resubscribe below picks up on the next one.
                        Err(RpcError::Closed) => break,
                        Err(error) => Err(error.to_string()),
                    };
                    if decoded.is_ok() {
                        delay = RETRY_MIN;
                    }
                    if tx.send(decoded).is_err() {
                        return;
                    }
                }
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(RETRY_MAX);
        }
    });
    (net, rx)
}

impl GitStore {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Shell | AppEvent::Selection | AppEvent::Phase) {
                this.sync_target(cx);
            }
        });
        let mut store = GitStore {
            state,
            target: None,
            status: None,
            status_error: None,
            progress: None,
            success: None,
            optimistic_branch: None,
            branch_pending: false,
            initializing: false,
            refs: Refs::default(),
            terminals: Terminals::default(),
            status_feed: None,
            terminal_feed: None,
            refs_task: None,
            success_timer: None,
            tick: None,
            _subscription: subscription,
        };
        store.sync_target(cx);
        store
    }

    fn client(&self, cx: &App) -> Option<(ClientHandle, tokio::runtime::Handle)> {
        let state = self.state.read(cx);
        Some((state.client_handle()?, state.runtime().clone()))
    }

    /// `gitCwd`: the thread's worktree, else its project root.
    fn resolve_target(state: &AppState) -> Option<Target> {
        let thread = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id))?;
        let project = state.shell.project(&thread.project_id)?;
        Some(Target {
            thread_id: thread.id.clone(),
            cwd: thread.worktree_path.clone().unwrap_or_else(|| project.workspace_root.clone()),
            project_cwd: project.workspace_root.clone(),
            worktree_path: thread.worktree_path.clone(),
            branch: thread.branch.clone(),
        })
    }

    fn sync_target(&mut self, cx: &mut Context<Self>) {
        let next = Self::resolve_target(self.state.read(cx));
        let connected = self.state.read(cx).client_handle().is_some();
        if !connected {
            self.terminal_feed = None;
            self.status_feed = None;
        } else if self.terminal_feed.is_none() {
            self.start_terminal_feed(cx);
        }
        if next == self.target {
            if connected && self.status_feed.is_none() && self.target.is_some() {
                self.start_status_feed(cx);
            }
            return;
        }
        let cwd_changed = next.as_ref().map(|t| &t.cwd) != self.target.as_ref().map(|t| &t.cwd);
        self.target = next;
        if cwd_changed {
            self.status = None;
            self.status_error = None;
            self.success = None;
            self.optimistic_branch = None;
            self.refs = Refs::default();
            self.refs_task = None;
            self.start_status_feed(cx);
        }
        cx.emit(GitEvent::Changed);
    }

    fn start_status_feed(&mut self, cx: &mut Context<Self>) {
        self.status_feed = None;
        let (Some(target), Some((client, runtime))) = (self.target.clone(), self.client(cx)) else { return };
        let (net, mut rx) = follow::<VcsStatusEvent>(client, &runtime, method::SUBSCRIBE_VCS_STATUS, vcs::status_subscription(&target.cwd));
        let ui = cx.spawn(async move |this, cx| {
            while let Some(update) = rx.recv().await {
                let applied = this.update(cx, |store, cx| {
                    match update {
                        Ok(event) => {
                            store.status = apply_status_event(store.status.take(), event);
                            store.status_error = None;
                        }
                        Err(error) => store.status_error = Some(error),
                    }
                    cx.emit(GitEvent::Changed);
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        self.status_feed = Some(Feed { net, _ui: ui });
    }

    fn start_terminal_feed(&mut self, cx: &mut Context<Self>) {
        let Some((client, runtime)) = self.client(cx) else { return };
        let (net, mut rx) = follow::<TerminalMetadataEvent>(client, &runtime, method::SUBSCRIBE_TERMINAL_METADATA, serde_json::json!({}));
        let ui = cx.spawn(async move |this, cx| {
            while let Some(update) = rx.recv().await {
                let Ok(event) = update else { continue };
                if this.update(cx, |store, cx| {
                    store.terminals.apply(event);
                    cx.emit(GitEvent::Changed);
                })
                .is_err()
                {
                    return;
                }
            }
        });
        self.terminal_feed = Some(Feed { net, _ui: ui });
    }

    pub fn is_running(&self) -> bool {
        self.progress.is_some()
    }

    /// The branch the picker's trigger shows: the optimistic one while a switch runs, else the
    /// checkout's, else the thread's (`resolveBranchToolbarValue`).
    pub fn active_branch(&self) -> Option<String> {
        if let Some(branch) = &self.optimistic_branch {
            return branch.clone();
        }
        let current = self.status.as_ref().and_then(|status| status.local.ref_name.as_deref());
        vcs::branch_toolbar_value(false, self.target.as_ref().and_then(|t| t.branch.as_deref()), current)
    }

    fn toast(&self, toast: Toast, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.show_toast(toast, cx));
    }

    fn begin(&mut self, operation: Operation, action_id: &str, cx: &mut Context<Self>) {
        self.success = None;
        self.success_timer = None;
        self.progress = Some(ActionProgress::begin(operation, action_id, now_ms()));
        // The elapsed label ticks once a second, and only while an action runs.
        self.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.emit(GitEvent::Changed)).is_err() {
                    return;
                }
            }
        }));
        cx.emit(GitEvent::Changed);
    }

    fn finish(&mut self, success: Option<InlineSuccess>, cx: &mut Context<Self>) {
        self.progress = None;
        self.tick = None;
        if let Some(success) = success {
            self.success = Some(success);
            self.success_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(git_actions::SUCCESS_VISIBLE_MS)).await;
                let _ = this.update(cx, |store, cx| {
                    store.success = None;
                    cx.emit(GitEvent::Changed);
                });
            }));
        }
        cx.emit(GitEvent::Changed);
    }

    /// `runGitActionWithToast` after any default-branch confirmation.
    pub fn run_stacked_action(&mut self, action: StackedAction, commit_message: Option<String>, file_paths: Option<Vec<String>>, feature_branch: bool, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        let (Some(target), Some((client, runtime))) = (self.target.clone(), self.client(cx)) else { return };
        let action_id = uuid_v4();
        let request = RunRequest { commit_message: commit_message.as_deref(), feature_branch, file_paths: file_paths.as_deref(), thread_id: Some(&target.thread_id) };
        let payload = git_actions::run_stacked_action(&action_id, &target.cwd, action, &request);
        self.begin(Operation::RunChangeRequest, &action_id, cx);
        let (tx, mut rx) = mpsc::unbounded_channel::<ProgressEvent>();
        let net = runtime.spawn(async move {
            let mut stream = client.stream(method::GIT_RUN_STACKED_ACTION, payload).await.map_err(|error| error.to_string())?;
            while let Some(item) = stream.receiver.recv().await {
                let event = serde_json::from_value::<ProgressEvent>(item.map_err(|error| error.to_string())?).unwrap_or(ProgressEvent::Unknown);
                match &event {
                    ProgressEvent::ActionFinished { result, .. } => return Ok(result.clone()),
                    ProgressEvent::ActionFailed { phase, .. } => return Err(git_actions::remote_failure_message(action, phase.as_deref())),
                    _ => {
                        let _ = tx.send(event);
                    }
                }
            }
            Err(git_actions::missing_terminal_message(action))
        });
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let _ = this.update(cx, |store, cx| {
                    if let Some(progress) = store.progress.as_mut() {
                        progress.apply(&event, now_ms());
                        cx.emit(GitEvent::Changed);
                    }
                });
            }
            let result: Result<RunResult, String> = net.await.unwrap_or_else(|_| Err("Source control action failed.".into()));
            let _ = this.update(cx, |store, cx| store.finish_stacked_action(result, cx));
        })
        .detach();
    }

    fn finish_stacked_action(&mut self, result: Result<RunResult, String>, cx: &mut Context<Self>) {
        match result {
            Ok(result) => {
                if let (Some(branch), Some(target)) = (result.created_branch(), self.target.as_ref())
                    && target.branch.as_deref() != Some(branch)
                {
                    let command = vcs::update_thread_branch_metadata(&uuid_v4(), &target.thread_id, Some(branch), target.branch.as_deref());
                    self.state.update(cx, |state, cx| state.dispatch_then(command, None, cx, |_, _, _| {}));
                }
                let terms = terminology(self.status.as_ref().and_then(VcsStatus::provider_kind));
                let (title, description) = result.summary(terms);
                self.finish(Some(InlineSuccess { title, description }), cx);
            }
            Err(message) => {
                self.finish(None, cx);
                self.toast(Toast::new(ToastKind::Error, "Action failed").description(message), cx);
            }
        }
    }

    /// The quick action's Pull.
    pub fn pull(&mut self, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        let Some(target) = self.target.clone() else { return };
        let Some(task) = self.state.read(cx).request(method::VCS_PULL, vcs::pull(&target.cwd), decode::<PullResult>) else { return };
        self.begin(Operation::Pull, &uuid_v4(), cx);
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Pull failed.".into()));
            let _ = this.update(cx, |store, cx| match result {
                Ok(pull) => {
                    let (title, description) = pull.summary();
                    store.finish(Some(InlineSuccess { title, description: Some(description) }), cx);
                }
                Err(message) => {
                    store.finish(None, cx);
                    store.toast(Toast::new(ToastKind::Error, "Pull failed").description(message), cx);
                }
            });
        })
        .detach();
    }

    /// "Initialize Git" for a project folder that is not a repository yet.
    pub fn init_repo(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.as_ref() else { return };
        let Some(task) = self.state.read(cx).request(method::VCS_INIT, vcs::init(&target.cwd), Ok) else { return };
        self.initializing = true;
        cx.emit(GitEvent::Changed);
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Git initialization failed.".into()));
            let _ = this.update(cx, |store, cx| {
                store.initializing = false;
                if let Err(message) = result {
                    store.toast(Toast::new(ToastKind::Error, "Git initialization failed").description(message), cx);
                }
                cx.emit(GitEvent::Changed);
            });
        })
        .detach();
    }

    /// Opening the Git menu or the branch picker asks the server to recheck the checkout.
    pub fn refresh_status(&mut self, local_only: bool, cx: &mut Context<Self>) {
        let Some(target) = self.target.as_ref() else { return };
        if let Some(task) = self.state.read(cx).request(method::VCS_REFRESH_STATUS, vcs::refresh_status(&target.cwd, local_only), Ok) {
            cx.background_executor().spawn(async move { drop(task.await) }).detach();
        }
    }

    /// Loads the first page for `query`, or the next page of the current listing.
    pub fn load_refs(&mut self, query: &str, next_page: bool, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return };
        let query = vcs::sanitize_new_ref_name(query);
        let cursor = if next_page && self.refs.query == query { self.refs.next_cursor } else { None };
        if next_page && cursor.is_none() {
            return;
        }
        if !next_page {
            self.refs = Refs { query: query.clone(), ..Refs::default() };
        }
        self.refs.loading = true;
        let payload = vcs::list_refs(&target.cwd, Some(&query), cursor, vcs::REF_PAGE_LIMIT);
        let Some(task) = self.state.read(cx).request(method::VCS_LIST_REFS, payload, decode::<ListRefsResult>) else { return };
        cx.emit(GitEvent::Changed);
        self.refs_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Failed to load refs.".into()));
            let _ = this.update(cx, |store, cx| {
                if store.refs.query != query {
                    return;
                }
                store.refs.loading = false;
                store.refs.loaded = true;
                match result {
                    Ok(page) => {
                        for item in page.refs {
                            match store.refs.refs.iter_mut().find(|existing| existing.name == item.name) {
                                Some(existing) => *existing = item,
                                None => store.refs.refs.push(item),
                            }
                        }
                        store.refs.next_cursor = page.next_cursor;
                        store.refs.total = store.refs.total.max(page.total_count);
                        store.refs.error = None;
                    }
                    Err(error) => store.refs.error = Some(error),
                }
                cx.emit(GitEvent::Changed);
            });
        }));
    }

    /// `selectBranch`: reuse the worktree a ref is checked out in, else check it out here.
    pub fn select_ref(&mut self, item: &VcsRef, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return };
        if self.branch_pending {
            return;
        }
        let selection = vcs::branch_selection_target(&target.project_cwd, target.worktree_path.as_deref(), item);
        if selection.reuse_existing_worktree {
            self.set_thread_branch(Some(&item.name), selection.next_worktree_path.as_deref(), cx);
            return;
        }
        let local_name = if item.is_remote() { vcs::local_branch_name_from_remote_ref(&item.name).to_owned() } else { item.name.clone() };
        let payload = vcs::switch_ref(&selection.checkout_cwd, &item.name);
        let remote = item.is_remote();
        self.branch_operation(payload, method::VCS_SWITCH_REF, local_name, "Failed to switch ref.", cx, move |result, fallback| {
            let name = if remote { result.ref_name.unwrap_or(fallback) } else { fallback };
            (name, selection.next_worktree_path)
        });
    }

    /// `createRef`: create the typed ref and switch the checkout to it.
    pub fn create_ref(&mut self, raw: &str, cx: &mut Context<Self>) {
        let name = vcs::sanitize_new_ref_name(raw);
        let Some(target) = self.target.clone() else { return };
        if name.is_empty() || self.branch_pending {
            return;
        }
        let payload = vcs::create_ref(&target.cwd, &name);
        let worktree = target.worktree_path.clone();
        self.branch_operation(payload, method::VCS_CREATE_REF, name, "Failed to create and switch ref.", cx, move |result, fallback| {
            (result.ref_name.unwrap_or(fallback), worktree)
        });
    }

    fn branch_operation(
        &mut self,
        payload: Value,
        rpc: &'static str,
        optimistic: String,
        failure: &'static str,
        cx: &mut Context<Self>,
        resolve: impl FnOnce(RefNameResult, String) -> (String, Option<String>) + 'static,
    ) {
        let Some(task) = self.state.read(cx).request(rpc, payload, decode::<RefNameResult>) else { return };
        self.branch_pending = true;
        self.optimistic_branch = Some(Some(optimistic.clone()));
        cx.emit(GitEvent::Changed);
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err(failure.into()));
            let _ = this.update(cx, |store, cx| {
                store.branch_pending = false;
                match result {
                    Ok(result) => {
                        let (branch, worktree) = resolve(result, optimistic);
                        store.optimistic_branch = Some(Some(branch.clone()));
                        store.set_thread_branch(Some(&branch), worktree.as_deref(), cx);
                    }
                    Err(message) => {
                        store.optimistic_branch = None;
                        store.toast(Toast::new(ToastKind::Error, failure).description(message), cx);
                    }
                }
                store.refresh_status(true, cx);
                let query = store.refs.query.clone();
                store.load_refs(&query, false, cx);
                cx.emit(GitEvent::Changed);
            });
        })
        .detach();
    }

    /// `setThreadBranch` for a server thread: record the branch and worktree it now follows.
    fn set_thread_branch(&mut self, branch: Option<&str>, worktree_path: Option<&str>, cx: &mut Context<Self>) {
        let Some(target) = self.target.as_ref() else { return };
        let command = vcs::update_thread_branch(&uuid_v4(), &target.thread_id, branch, worktree_path);
        self.state.update(cx, |state, cx| state.dispatch_then(command, Some("Failed to update the thread's branch"), cx, |_, _, _| {}));
    }

    /// The shell catches up with a branch switch on its own; the optimistic label yields to it.
    pub fn settle_optimistic_branch(&mut self) {
        let checkout = self.status.as_ref().and_then(|status| status.local.ref_name.clone());
        if self.optimistic_branch.as_ref().is_some_and(|branch| *branch == checkout) && !self.branch_pending {
            self.optimistic_branch = None;
        }
    }

    pub fn open_in_editor(&self, editor: &str, cx: &mut Context<Self>) {
        let Some(target) = self.target.as_ref() else { return };
        if let Some(task) = self.state.read(cx).request(method::SHELL_OPEN_IN_EDITOR, vcs::open_in_editor(&target.cwd, editor), Ok) {
            cx.background_executor()
                .spawn(async move {
                    if let Ok(Err(error)) = task.await {
                        tracing::warn!(%error, "open in editor failed");
                    }
                })
                .detach();
        }
    }

    pub fn script_running(&self, script_id: &str) -> bool {
        self.target.as_ref().is_some_and(|target| self.terminals.script_running(&target.thread_id, script_id))
    }

    /// `runProjectScript`: in the script's idle thread terminal, or a fresh one beside it.
    pub fn run_script(&mut self, script: &ProjectScript, cx: &mut Context<Self>) {
        let (Some(target), Some((client, runtime))) = (self.target.clone(), self.client(cx)) else { return };
        let suffix: String = uuid_v4().chars().take(8).collect();
        let terminal_id = self.terminals.terminal_for_run(&target.thread_id, &script.id, &suffix);
        let open = project_scripts::open_terminal(&target.thread_id, &terminal_id, &target.project_cwd, target.worktree_path.as_deref());
        let write = project_scripts::write_terminal(&target.thread_id, &terminal_id, &project_scripts::terminal_input(&script.command));
        let name = script.name.clone();
        let task = runtime.spawn(async move {
            client.request(method::TERMINAL_OPEN, open).await?;
            client.request(method::TERMINAL_WRITE, write).await
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Err(error)) | Err(error) = task.await.map_err(|_| RpcError::Closed) {
                let _ = this.update(cx, |store, cx| {
                    store.toast(Toast::new(ToastKind::Error, format!("Failed to run script \"{name}\".")).description(error.to_string()), cx);
                });
            }
        })
        .detach();
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

pub fn init(state: Entity<AppState>, cx: &mut App) -> Entity<GitStore> {
    cx.new(|cx| GitStore::new(state, cx))
}
