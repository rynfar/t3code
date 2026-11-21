//! Source control for the open thread: its live status (`subscribeVcsStatus`), the running Git
//! action and its progress, the branch picker's refs, and the thread terminals project scripts
//! run in. Ports the state behind `GitActionsControl.tsx`, `BranchToolbarBranchSelector.tsx`
//! and `useChatTerminalActions.ts` (`client-runtime/src/state/vcs.ts`, `vcsAction.ts`,
//! `vcsStatusRevalidation.ts`). Wire work and decoding run on the network runtime; this entity
//! only applies results. Operations capture the thread they started on, so a completion after
//! the selection moved still updates that thread.

mod actions;
mod branch_picker;
mod dialogs;

use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Subscription, Task};
use serde::de::DeserializeOwned;
use serde_json::Value;
use t3_client::session::RpcError;
use t3_client::supervisor::ClientHandle;
use t3_core::auth::AuthSessionState;
use t3_core::git_actions::{self, ActionProgress, Operation, ProgressEvent, RunRequest, RunResult, StackedAction, StackedActionRun, Terminology, terminology};
use t3_core::keybindings::decode_resolved_keybindings;
use t3_core::project_scripts::{self, ProjectScript, TerminalMetadataEvent, Terminals};
use t3_core::settings::project_actions::{self as project_action_rules, EditorRequest, FileScript, Member, ProjectFile, ScriptsEdit};
use t3_core::vcs::{self, PullResult, RefNameResult, VcsRef, VcsStatus, VcsStatusEvent, apply_status_event, method};
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
    pub project_id: String,
    pub is_draft: bool,
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
    pub refs: vcs::refs::RefListing,
    refs_feed: Option<Task<()>>,
    pub terminals: Terminals,
    /// This device's grants, `None` until the server answers (which grants nothing).
    grants: Option<AuthSessionState>,
    status_feed: Option<Feed>,
    terminal_feed: Option<Feed>,
    grants_task: Option<Task<()>>,
    /// The connection the feeds and the target's reads belong to.
    connection: Option<u64>,
    refs_task: Option<Task<()>>,
    success_timer: Option<Task<()>>,
    tick: Option<Task<()>>,
    wake: Option<Task<()>>,
    /// The typed message from the last failed `Commit` in `(cwd, message)`, restored on reopen.
    failed_commit_message: Option<(String, String)>,
    /// The open project's `t3.json`, keyed by `project_cwd` (`From t3.json` quick-imports).
    project_file: Option<(String, ProjectFile)>,
    project_file_task: Option<Task<()>>,
    importing_script: bool,
    _subscription: Subscription,
}

impl EventEmitter<GitEvent> for GitStore {}

const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// `vcsStatusRevalidation`'s `wakeDebounceMs`.
const WAKE_DEBOUNCE: Duration = Duration::from_millis(250);

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
            refs: Default::default(),
            refs_feed: None,
            terminals: Terminals::default(),
            grants: None,
            status_feed: None,
            terminal_feed: None,
            grants_task: None,
            connection: None,
            refs_task: None,
            success_timer: None,
            tick: None,
            wake: None,
            failed_commit_message: None,
            project_file: None,
            project_file_task: None,
            importing_script: false,
            _subscription: subscription,
        };
        store.sync_target(cx);
        store
    }

    fn client(&self, cx: &App) -> Option<(ClientHandle, tokio::runtime::Handle)> {
        let state = self.state.read(cx);
        Some((state.client_handle()?, state.runtime().clone()))
    }

    /// `gitCwd`: the thread's worktree, else the thread or draft project root.
    fn resolve_target(state: &AppState) -> Option<Target> {
        if let Some(thread) = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id)) {
            let project = state.shell.project(&thread.project_id)?;
            return Some(Target {
                thread_id: thread.id.clone(),
                project_id: thread.project_id.clone(),
                is_draft: false,
                cwd: thread.worktree_path.clone().unwrap_or_else(|| project.workspace_root.clone()),
                project_cwd: project.workspace_root.clone(),
                worktree_path: thread.worktree_path.clone(),
                branch: thread.branch.clone(),
            });
        }
        let project_id = state.draft_project.as_deref()?;
        let project = state.shell.project(project_id)?;
        if state.is_scratch_project(project_id) || project.workspace_root.is_empty() {
            return None;
        }
        Some(Target {
            thread_id: t3_core::composer::drafts::new_thread_key(project_id),
            project_id: project_id.to_owned(),
            is_draft: true,
            cwd: project.workspace_root.clone(),
            project_cwd: project.workspace_root.clone(),
            worktree_path: None,
            branch: None,
        })
    }

    fn sync_target(&mut self, cx: &mut Context<Self>) {
        let next = Self::resolve_target(self.state.read(cx));
        let current = self.state.read(cx).connection_id();
        let connected = current.is_some();
        if current != self.connection {
            // Another environment: nothing read from the last one applies, and its feeds hold
            // its client open.
            self.connection = current;
            self.refs_feed = None;
            self.refs_task = None;
            self.refs.reset();
            self.terminal_feed = None;
            self.status_feed = None;
            self.grants_task = None;
            self.grants = None;
            self.terminals = Terminals::default();
            self.target = None;
            self.status = None;
            self.status_error = None;
            self.failed_commit_message = None;
            self.project_file = None;
            self.project_file_task = None;
            self.importing_script = false;
            cx.emit(GitEvent::Changed);
        }
        if connected {
            if self.refs_feed.is_none() {
                self.watch_refs(cx);
            }
            if self.terminal_feed.is_none() {
                self.start_terminal_feed(cx);
            }
            if self.grants_task.is_none() {
                self.watch_grants(cx);
            }
        }
        if next == self.target {
            if connected && self.status_feed.is_none() && self.target.is_some() {
                self.start_status_feed(cx);
            }
            if connected {
                self.ensure_project_file(cx);
            }
            return;
        }
        let cwd_changed = next.as_ref().map(|t| &t.cwd) != self.target.as_ref().map(|t| &t.cwd);
        let project_cwd_changed = next.as_ref().map(|t| &t.project_cwd) != self.target.as_ref().map(|t| &t.project_cwd);
        self.target = next;
        if project_cwd_changed {
            self.project_file = None;
            self.project_file_task = None;
            if connected {
                self.ensure_project_file(cx);
            }
        }
        if cwd_changed {
            self.status = None;
            self.status_error = None;
            self.success = None;
            self.optimistic_branch = None;
            self.failed_commit_message = None;
            self.refs.reset();
            self.refs_task = None;
            self.start_status_feed(cx);
        }
        cx.emit(GitEvent::Changed);
    }

    fn watch_refs(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.state.read(cx).client_handle() else {
            return;
        };
        let mut revisions = client.refs_revision();
        let mut sessions = client.connection_generation();
        self.refs_feed = Some(cx.spawn(async move |this, cx| {
            loop {
                let changed = tokio::select! {
                    changed = revisions.changed() => changed,
                    changed = sessions.changed() => changed,
                };
                if changed.is_err() {
                    return;
                }
                if this
                    .update(cx, |store, cx| {
                        if store.refs.loaded || store.refs.loading {
                            let query = store.refs.query.clone();
                            store.load_refs(&query, false, cx);
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    /// Follows this device's grants; the last answer holds while reconnecting.
    fn watch_grants(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.state.read(cx).client_handle() else { return };
        let mut grants = client.session_state();
        self.grants = grants.borrow_and_update().clone();
        self.grants_task = Some(cx.spawn(async move |this, cx| {
            while grants.changed().await.is_ok() {
                let next = grants.borrow_and_update().clone();
                let updated = this.update(cx, |store, cx| {
                    if store.grants != next {
                        store.grants = next;
                        cx.emit(GitEvent::Changed);
                    }
                });
                if updated.is_err() {
                    return;
                }
            }
        }));
    }

    /// Whether this device may switch or create the thread's ref (`canChangeThreadBranch`).
    pub fn can_change_branch(&self) -> bool {
        vcs::can_change_thread_branch(self.grants.as_ref())
    }

    /// Whether this device may record a branch on the thread (`canUpdateThreadBranch`).
    pub fn can_operate_thread(&self) -> bool {
        vcs::can_operate_thread(self.grants.as_ref())
    }

    /// The project's checkout root (`workingTree.rootPath` of the project's status), which the
    /// open status names only while the thread works in the project folder itself.
    pub fn project_checkout_root(&self) -> Option<&str> {
        self.target.as_ref().filter(|target| target.worktree_path.is_none() && target.cwd == target.project_cwd)?;
        self.status.as_ref()?.local.working_tree.root_path.as_deref()
    }

    /// Whether this device may mutate git/source control (`AuthSourceControlWriteScope`).
    pub fn can_write_source_control(&self) -> bool {
        self.grants.as_ref().is_some_and(|grants| grants.grants(t3_core::auth::SOURCE_CONTROL_WRITE_SCOPE))
    }

    /// Whether this device may switch or create a feature branch for the current target
    /// (`canChangeThreadBranch` in `GitActionsControl.tsx`).
    pub fn can_change_target_branch(&self) -> bool {
        self.can_write_source_control() && (self.target.as_ref().is_some_and(|target| target.is_draft) || self.can_operate_thread())
    }

    /// Returns and consumes a failed commit's typed message for the active checkout (`pingdotgg/t3code#35781`).
    pub fn take_failed_commit_message(&mut self) -> Option<String> {
        let cwd = self.target.as_ref().map(|target| target.cwd.as_str())?;
        self.failed_commit_message.take().and_then(|(saved_cwd, msg)| (saved_cwd == cwd).then_some(msg))
    }

    pub fn clear_failed_commit_message(&mut self) {
        self.failed_commit_message = None;
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
        if self.is_running() || !self.can_write_source_control() || (feature_branch && !self.can_change_target_branch()) {
            return;
        }
        let (Some(origin), Some((client, runtime))) = (self.target.clone(), self.client(cx)) else { return };
        let action_id = uuid_v4();
        let request = RunRequest {
            commit_message: commit_message.as_deref(),
            feature_branch,
            file_paths: file_paths.as_deref(),
            thread_id: (!origin.is_draft).then_some(origin.thread_id.as_str()),
            project_id: origin.is_draft.then_some(origin.project_id.as_str()),
        };
        let payload = git_actions::run_stacked_action(&action_id, &origin.cwd, action, &request);
        let terms = terminology(self.status.as_ref().and_then(VcsStatus::provider_kind));
        self.begin(Operation::RunChangeRequest, &action_id, cx);
        let (tx, mut rx) = mpsc::unbounded_channel::<ProgressEvent>();
        let mut run = StackedActionRun::new(&action_id, action);
        let net = runtime.spawn(async move {
            // Read to the end: dropping the stream interrupts the server's PR link and refresh.
            let mut stream = client.stream(method::GIT_RUN_STACKED_ACTION, payload).await.map_err(|error| error.to_string())?;
            let mut stream_error = None;
            while let Some(item) = stream.receiver.recv().await {
                match item {
                    Ok(value) => {
                        let event = serde_json::from_value::<ProgressEvent>(value).unwrap_or(ProgressEvent::Unknown);
                        run.observe(&event);
                        let _ = tx.send(event);
                    }
                    Err(error) => {
                        stream_error = Some(error.to_string());
                        break;
                    }
                }
            }
            run.settle(stream_error)
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
            let _ = this.update(cx, |store, cx| store.finish_stacked_action(&origin, terms, action, commit_message, result, cx));
        })
        .detach();
    }

    /// Whether the card still shows the checkout `origin` ran in.
    fn shows(&self, origin: &Target) -> bool {
        self.target.as_ref().is_some_and(|target| target.cwd == origin.cwd)
    }

    fn finish_stacked_action(&mut self, origin: &Target, terms: Terminology, action: StackedAction, commit_message: Option<String>, result: Result<RunResult, String>, cx: &mut Context<Self>) {
        match result {
            Ok(result) => {
                if self.failed_commit_message.as_ref().is_some_and(|(cwd, _)| *cwd == origin.cwd) {
                    self.failed_commit_message = None;
                }
                // `persistThreadBranchSync` for the thread the action ran for.
                if !origin.is_draft
                    && let Some(branch) = result.created_branch()
                    && origin.branch.as_deref() != Some(branch)
                    && self.can_operate_thread()
                {
                    let command = vcs::update_thread_branch_metadata(&uuid_v4(), &origin.thread_id, Some(branch), origin.branch.as_deref());
                    self.state.update(cx, |state, cx| state.dispatch_then(command, None, cx, |_, _, _| {}));
                }
                let (title, description) = result.summary(terms);
                let success = self.shows(origin).then_some(InlineSuccess { title, description });
                self.finish(success, cx);
            }
            Err(message) => {
                if action == StackedAction::Commit
                    && let Some(typed) = commit_message.filter(|text| !text.trim().is_empty())
                {
                    self.failed_commit_message = Some((origin.cwd.clone(), typed));
                }
                self.finish(None, cx);
                self.toast(Toast::new(ToastKind::Error, "Action failed").description(message), cx);
            }
        }
    }

    /// The quick action's Pull.
    pub fn pull(&mut self, cx: &mut Context<Self>) {
        if self.is_running() || !self.can_write_source_control() {
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
        if self.initializing || !self.can_write_source_control() {
            return;
        }
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

    /// Coming back to the window rechecks the checkout, where edits made in another app or
    /// terminal may be waiting (`vcsStatusRevalidation`).
    pub fn revalidate_on_return(&mut self, cx: &mut Context<Self>) {
        if self.target.is_none() {
            return;
        }
        self.wake = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WAKE_DEBOUNCE).await;
            let _ = this.update(cx, |store, cx| store.refresh_status(false, cx));
        }));
    }

    /// `ServerSettings.offeredSourceControlProviders`, when config has loaded (`GT-ACTIONS`).
    pub fn offered_source_control_providers(&self, cx: &App) -> Option<std::collections::BTreeMap<String, bool>> {
        self.state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .map(|config| config.settings.source_control.offered_source_control_providers.clone())
    }

    /// Opens a changed file from the commit dialog in the preferred editor
    /// (`openChangedFileInEditor` in `GitActionsControl.tsx`).
    pub fn open_changed_file(&self, file_path: &str, cx: &mut Context<Self>) {
        let Some(root) = self.status.as_ref().and_then(|s| s.local.working_tree.root_path.as_deref()) else {
            self.toast(Toast::new(ToastKind::Error, "Editor opening is unavailable."), cx);
            return;
        };
        let Some(resolved) = git_actions::resolve_commit_dialog_file_path(file_path, root) else {
            self.toast(Toast::new(ToastKind::Error, "Editor opening is unavailable."), cx);
            return;
        };
        let app = self.state.read(cx);
        let available = app.active_open_in_config().map(|c| c.available_editors.clone()).unwrap_or_default();
        let preferred = crate::thread_details::details(cx)
            .and_then(|d| d.read(cx).preferred_editor(&available))
            .or_else(|| vcs::preferred_editor(None, &available));
        let Some(editor) = preferred else {
            self.toast(Toast::new(ToastKind::Error, "Editor opening is unavailable."), cx);
            return;
        };
        let mut payload = vcs::open_in_editor(&resolved, editor);
        payload["projectRoot"] = serde_json::json!(root);
        let Some(task) = app.request(method::SHELL_OPEN_IN_EDITOR, payload, Ok) else { return };
        let state = self.state.clone();
        cx.spawn(async move |_, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Unable to open file.".into()));
            if let Err(error) = result {
                state.update(cx, |state, cx| {
                    state.show_toast(Toast::new(ToastKind::Error, "Unable to open file").description(error), cx);
                });
            }
        })
        .detach();
    }

    /// Loads `server.discoverSourceControl` for `PublishRepositoryDialog`.
    pub fn discover_publish_providers(&self, cx: &App) -> Option<tokio::task::JoinHandle<Result<Vec<git_actions::PublishProviderDiscovery>, String>>> {
        self.state.read(cx).request(
            git_actions::method::SOURCE_CONTROL_DISCOVERY,
            serde_json::json!({}),
            |value| Ok(git_actions::decode_publish_discovery(&value)),
        )
    }

    /// Publishes the active repository (`sourceControl.publishRepository`).
    pub fn publish_repository(
        &self,
        provider: &str,
        repository: &str,
        visibility: git_actions::PublishVisibility,
        remote_name: &str,
        protocol: git_actions::CloneProtocol,
        cx: &App,
    ) -> Option<tokio::task::JoinHandle<Result<git_actions::PublishRepositoryResult, String>>> {
        let target = self.target.as_ref()?;
        let payload = git_actions::publish_repository_input(&target.cwd, provider, repository, visibility, remote_name, protocol);
        self.state
            .read(cx)
            .request(git_actions::method::SOURCE_CONTROL_PUBLISH_REPOSITORY, payload, decode::<git_actions::PublishRepositoryResult>)
    }

    /// Resolves a pull/merge request reference (`git.resolvePullRequest`).
    pub fn resolve_pull_request(&self, reference: &str, cx: &App) -> Option<tokio::task::JoinHandle<Result<vcs::ResolvePullRequestResult, String>>> {
        let target = self.target.as_ref()?;
        self.state
            .read(cx)
            .request(method::GIT_RESOLVE_PULL_REQUEST, vcs::resolve_pull_request(&target.project_cwd, reference), decode::<vcs::ResolvePullRequestResult>)
    }

    /// Prepares a local or worktree checkout for a pull/merge request (`git.preparePullRequestThread`).
    /// Drafts only allow `Local` checkout here because attaching a new worktree to a draft lives
    /// under `PR-DETAIL`'s `usePullRequestDraftThread` flow (`composer/`).
    pub fn prepare_pull_request_thread(
        &self,
        reference: &str,
        mode: vcs::PullRequestCheckoutMode,
        cx: &App,
    ) -> Option<(Target, tokio::task::JoinHandle<Result<vcs::PreparePullRequestThreadResult, String>>)> {
        let target = self.target.clone()?;
        if target.is_draft && mode == vcs::PullRequestCheckoutMode::Worktree {
            return None;
        }
        let thread_id = (!target.is_draft).then_some(target.thread_id.as_str());
        let payload = vcs::prepare_pull_request_thread(&target.project_cwd, reference, mode, thread_id);
        let task = self
            .state
            .read(cx)
            .request(method::GIT_PREPARE_PULL_REQUEST_THREAD, payload, decode::<vcs::PreparePullRequestThreadResult>)?;
        Some((target, task))
    }

    /// Applies the prepared PR checkout to the active thread/checkout (`handlePreparedPullRequestThread`).
    pub fn apply_prepared_pull_request(&mut self, origin: &Target, result: &vcs::PreparePullRequestThreadResult, cx: &mut Context<Self>) {
        if origin.is_draft && result.worktree_path.is_some() {
            return;
        }
        if !origin.is_draft {
            self.set_thread_branch(origin, Some(&result.branch), result.worktree_path.as_deref(), cx);
        }
        if self.shows(origin) {
            self.optimistic_branch = Some(Some(result.branch.clone()));
            self.refresh_status(false, cx);
            let query = self.refs.query.clone();
            self.load_refs(&query, false, cx);
        }
        let terms = terminology(self.status.as_ref().and_then(VcsStatus::provider_kind));
        if !result.is_on_pull_request_head {
            let (title, description) = vcs::off_head_checkout_toast(terms.singular);
            self.toast(Toast::new(ToastKind::Info, title).description(description), cx);
        } else {
            self.toast(
                Toast::new(ToastKind::Success, format!("Checked out {} #{}", terms.short_label, result.pull_request.number))
                    .description(result.branch.clone()),
                cx,
            );
        }
        cx.emit(GitEvent::Changed);
    }

    /// Opens the URL in the system browser (used by `PublishRepositoryDialog`'s summary step).
    pub fn open_external_url(&self, url: &str, cx: &mut App) {
        cx.open_url(url);
    }

    /// Loads the first page for `query`, or the next page of the current listing.
    /// Seeds unfiltered initial opens from the persisted offline ref snapshot while live refs load.
    pub fn load_refs(&mut self, query: &str, next_page: bool, cx: &mut Context<Self>) {
        let (Some(target), Some((client, runtime))) = (self.target.clone(), self.client(cx)) else {
            return;
        };
        let env_key = self.state.read(cx).environment().map(str::to_owned);
        let query = vcs::sanitize_new_ref_name(query);
        if !next_page
            && query.is_empty()
            && let Some(env_key) = env_key.as_deref()
            && let Some(snapshot) = client.load_cached_refs(env_key, &target.cwd)
        {
            self.refs.seed_offline_snapshot(&query, snapshot);
        }
        let Some(request) = self.refs.begin(&query, next_page) else {
            return;
        };
        let cwd = target.cwd.clone();
        let cursors = request.cursors.clone();
        let connection = self.connection;
        let revisions = client.refs_revision();
        let revision = *revisions.borrow();
        let sessions = client.connection_generation();
        let generation = *sessions.borrow();
        let task = runtime.spawn(async move {
            let mut pages = Vec::with_capacity(cursors.len());
            for cursor in cursors {
                let payload = vcs::list_refs(&cwd, Some(&query), cursor, vcs::REF_PAGE_LIMIT);
                let answer = match env_key.as_deref() {
                    Some(key) => client.list_refs_cached(key, payload).await?,
                    None => client.list_refs(payload).await?,
                };
                match answer {
                    Some(page) => pages.push(page),
                    None => return Ok(None),
                }
            }
            Ok(Some(pages))
        });
        cx.emit(GitEvent::Changed);
        self.refs_task = Some(cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Failed to load refs.".into()));
            let _ = this.update(cx, |store, cx| {
                if store.connection != connection || !store.shows(&target) || *revisions.borrow() != revision || *sessions.borrow() != generation {
                    return;
                }
                let result = match result {
                    Ok(Some(page)) => Ok(page),
                    Ok(None) => return,
                    Err(error) => Err(error),
                };
                if store.refs.finish(&request, result) {
                    cx.emit(GitEvent::Changed);
                }
            });
        }));
    }

    /// `selectBranch`: reuse the worktree a ref is checked out in, else check it out here.
    pub fn select_ref(&mut self, item: &VcsRef, cx: &mut Context<Self>) {
        let Some(origin) = self.target.clone() else { return };
        if self.branch_pending || (!origin.is_draft && !self.can_operate_thread()) {
            return;
        }
        let selection = vcs::branch_selection_target(&origin.project_cwd, self.project_checkout_root(), origin.worktree_path.as_deref(), item);
        if !origin.is_draft && vcs::records_existing_worktree(&selection, self.can_change_branch()) {
            self.set_thread_branch(&origin, Some(&item.name), selection.next_worktree_path.as_deref(), cx);
            return;
        }
        if !self.allow_branch_change(cx) {
            return;
        }
        let local_name = if item.is_remote() { vcs::local_branch_name_from_remote_ref(&item.name).to_owned() } else { item.name.clone() };
        let payload = vcs::switch_ref(&selection.checkout_cwd, &item.name);
        let remote = item.is_remote();
        self.branch_operation(origin, (method::VCS_SWITCH_REF, payload), local_name, "Failed to switch ref.", cx, move |result, fallback| {
            let name = if remote { result.ref_name.unwrap_or(fallback) } else { fallback };
            (name, selection.next_worktree_path)
        });
    }

    /// `createRef`: create the typed ref and switch the checkout to it.
    pub fn create_ref(&mut self, raw: &str, cx: &mut Context<Self>) {
        let name = vcs::sanitize_new_ref_name(raw);
        let Some(origin) = self.target.clone() else { return };
        if name.is_empty() || self.branch_pending || !self.can_change_branch() {
            return;
        }
        let payload = vcs::create_ref(&origin.cwd, &name);
        let worktree = origin.worktree_path.clone();
        self.branch_operation(origin, (method::VCS_CREATE_REF, payload), name, "Failed to create and switch ref.", cx, move |result, fallback| {
            (result.ref_name.unwrap_or(fallback), worktree)
        });
    }

    /// `runBranchAction`'s guard: a switch writes to source control and to the thread.
    fn allow_branch_change(&self, cx: &mut Context<Self>) -> bool {
        if self.can_change_branch() {
            return true;
        }
        self.toast(Toast::new(ToastKind::Error, "Action unavailable").description("This connection cannot change the thread's branch."), cx);
        false
    }

    fn branch_operation(
        &mut self,
        origin: Target,
        (rpc, payload): (&'static str, Value),
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
                // The optimistic label and listings belong to the checkout on screen, which may
                // no longer be the one this switched.
                let shown = store.shows(&origin);
                match result {
                    Ok(result) => {
                        let (branch, worktree) = resolve(result, optimistic);
                        if shown {
                            store.optimistic_branch = Some(Some(branch.clone()));
                        }
                        store.set_thread_branch(&origin, Some(&branch), worktree.as_deref(), cx);
                    }
                    Err(message) => {
                        if shown {
                            store.optimistic_branch = None;
                        }
                        store.toast(Toast::new(ToastKind::Error, failure).description(message), cx);
                    }
                }
                if shown {
                    store.refresh_status(true, cx);
                    let query = store.refs.query.clone();
                    store.load_refs(&query, false, cx);
                }
                cx.emit(GitEvent::Changed);
            });
        })
        .detach();
    }

    /// `setThreadBranch` for a server thread: record the branch and worktree it now follows.
    fn set_thread_branch(&mut self, origin: &Target, branch: Option<&str>, worktree_path: Option<&str>, cx: &mut Context<Self>) {
        if origin.is_draft || !self.can_operate_thread() {
            return;
        }
        let command = vcs::update_thread_branch(&uuid_v4(), &origin.thread_id, branch, worktree_path);
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
            let label = vcs::editor_label(editor, cfg!(target_os = "macos"));
            let state = self.state.clone();
            cx.spawn(async move |_, cx| {
                match task.await {
                    Ok(Ok(_)) => { state.update(cx, |state, cx| state.announce(format!("Opened in {label}"), None, cx)); }
                    Ok(Err(error)) => tracing::warn!(%error, "open in editor failed"),
                    Err(error) => tracing::warn!(%error, "open in editor task failed"),
                }
            }).detach();
        }
    }

    pub fn script_running(&self, script_id: &str) -> bool {
        self.target.as_ref().is_some_and(|target| self.terminals.script_running(&target.thread_id, script_id))
    }

    /// Whether this device may run project scripts in thread terminals (`AuthTerminalOperateScope`).
    pub fn can_run_scripts(&self) -> bool {
        self.grants.as_ref().is_some_and(|grants| grants.grants(t3_core::auth::TERMINAL_OPERATE_SCOPE))
    }

    /// Whether this device may edit server settings / project actions (`AuthSettingsWriteScope`).
    pub fn can_write_settings(&self) -> bool {
        self.grants.as_ref().is_some_and(|grants| grants.grants("settings:write"))
    }

    /// Scripts declared by the open project's `t3.json`, once read.
    pub fn file_scripts(&self) -> &[FileScript] {
        let Some(target) = self.target.as_ref() else { return &[] };
        self.project_file
            .as_ref()
            .filter(|(cwd, _)| *cwd == target.project_cwd)
            .map(|(_, file)| file.scripts())
            .unwrap_or(&[])
    }

    /// Reads the open project's `t3.json` once per `(connection, project_cwd)` (`useT3ProjectFileState`).
    fn ensure_project_file(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.as_ref() else { return };
        if target.project_cwd.is_empty() {
            return;
        }
        if self.project_file.as_ref().is_some_and(|(cwd, _)| *cwd == target.project_cwd) || self.project_file_task.is_some() {
            return;
        }
        let cwd = target.project_cwd.clone();
        let connection = self.connection;
        let payload = t3_core::workspace::project_file_request(&cwd);
        let Some(task) = self.state.read(cx).request(t3_core::files::READ_FILE, payload, |value| Ok(project_action_rules::project_file_from_read(&value))) else {
            return;
        };
        self.project_file_task = Some(cx.spawn(async move |this, cx| {
            let file = task.await.ok().and_then(Result::ok).unwrap_or(ProjectFile::Missing);
            let _ = this.update(cx, |store, cx| {
                store.project_file_task = None;
                if store.connection != connection || store.target.as_ref().is_none_or(|t| t.project_cwd != cwd) {
                    return;
                }
                store.project_file = Some((cwd, file));
                cx.emit(GitEvent::Changed);
            });
        }));
    }

    /// Quick-imports a script from `t3.json` straight from the thread details card
    /// (`importFileScript` in `ProjectScriptsControl.tsx`), falling back to the settings editor
    /// prefilled with the error if saving fails.
    pub fn import_file_script(&mut self, project_id: &str, file_script: FileScript, cx: &mut Context<Self>) {
        if self.importing_script || !self.can_write_settings() {
            return;
        }
        let Some((client, runtime)) = self.client(cx) else { return };
        let app = self.state.read(cx);
        let Some(config) = app.server_config.config.as_ref() else { return };
        let Some(project) = app.shell.project(project_id) else { return };
        let to_member = |project: &t3_core::shell::ProjectShell| Member {
            id: project.id.clone(),
            scripts: project.scripts.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect(),
        };
        let projects: Vec<Member> = app
            .shell
            .snapshot
            .as_ref()
            .map(|s| s.projects.as_slice())
            .unwrap_or_default()
            .iter()
            .map(to_member)
            .collect();
        let member = to_member(project);
        let members = [member];
        let mut input = file_script.import_input();
        if !self.can_write_settings() {
            input.keybinding = None;
        }
        let id = project_action_rules::submit_id(&config.settings, &projects, &members, None, &input.name, now_ms().max(0) as u64);
        let keybindings = decode_resolved_keybindings(&config.keybindings);
        let edit = ScriptsEdit::Save { id, input: input.clone(), is_new: true };
        let writes = project_action_rules::plan_writes(&config.settings, &keybindings, &projects, &members, &edit);
        let project_id = project_id.to_owned();
        let connection = self.connection;
        self.importing_script = true;
        cx.emit(GitEvent::Changed);
        let task = runtime.spawn(async move {
            for write in writes {
                client.request(write.method(), write.payload().clone()).await?;
            }
            Ok::<(), RpcError>(())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(|_| RpcError::Closed).and_then(|res| res);
            let _ = this.update(cx, |store, cx| {
                store.importing_script = false;
                if let Err(error) = result
                    && store.connection == connection
                {
                    let message = error.to_string();
                    let request = EditorRequest { script_id: None, initial: input, error: Some(message.clone()) };
                    open_script_editor_request(&project_id, request, cx);
                    store.toast(Toast::new(ToastKind::Error, "Failed to import action.").description(message), cx);
                }
                cx.emit(GitEvent::Changed);
            });
        })
        .detach();
    }

    /// `runProjectScript`: in the script's idle thread terminal, or a fresh one beside it. A
    /// `stopOnSettle` script, or a device that cannot read terminals, always opens a new one.
    /// Opens and focuses the script's terminal drawer, and launches `previewUrl` when
    /// `autoOpenPreview` is enabled (`useChatTerminalActions.ts`).
    pub fn run_script(&mut self, script: &ProjectScript, cx: &mut Context<Self>) {
        if !self.can_run_scripts() {
            return;
        }
        let (Some(target), Some((client, runtime))) = (self.target.clone(), self.client(cx)) else { return };
        let suffix: String = uuid_v4().chars().take(8).collect();
        let can_reuse = self.grants.as_ref().is_some_and(|grants| grants.grants(t3_core::auth::TERMINAL_READ_SCOPE));
        let launch = self.terminals.launch_for(&target.thread_id, script, can_reuse, &suffix);
        let open = project_scripts::open_script_terminal(&target.thread_id, &launch, &target.project_cwd, target.worktree_path.as_deref(), script.stops_on_settle());
        let write = project_scripts::write_terminal(&target.thread_id, &launch.terminal_id, &project_scripts::terminal_input(&script.command));
        let name = script.name.clone();
        let auto_preview_url = (script.auto_open_preview == Some(true))
            .then(|| script.preview_url.as_deref().map(str::trim).filter(|url| !url.is_empty()).map(str::to_owned))
            .flatten();
        let drawer_thread = target.thread_id.clone();
        let drawer_terminal = launch.terminal_id.clone();
        cx.defer(move |cx| {
            crate::terminal::open_script_terminal(drawer_thread, drawer_terminal, cx);
        });
        let connection = self.connection;
        let preview_thread = target.thread_id.clone();
        let task = runtime.spawn(async move {
            client.request(method::TERMINAL_OPEN, open).await?;
            client.request(method::TERMINAL_WRITE, write).await
        });
        cx.spawn(async move |this, cx| {
            match task.await.map_err(|_| RpcError::Closed).and_then(|res| res) {
                Ok(_) => {
                    if let Some(raw_url) = auto_preview_url {
                        let _ = this.update(cx, |store, cx| {
                            if store.connection != connection {
                                return;
                            }
                            let env_http_base = store.state.read(cx).active_environment().map(|env| env.http_base.clone());
                            let url = t3_core::thread_details::resolve_discovered_server_url(&raw_url, env_http_base.as_deref());
                            if let Some(previews) = crate::preview::previews(cx)
                                && previews.read(cx).availability() == crate::preview::Availability::Available
                            {
                                previews.update(cx, |previews, cx| previews.open_session(preview_thread, Some(url), cx));
                            }
                        });
                    }
                }
                Err(error) => {
                    let _ = this.update(cx, |store, cx| {
                        store.toast(Toast::new(ToastKind::Error, format!("Failed to run script \"{name}\".")).description(error.to_string()), cx);
                    });
                }
            }
        })
        .detach();
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// The thread card's Add or Edit action (`ProjectScriptsControl`'s editor request), waiting for
/// Settings → Projects to show the project; its action editor takes it from there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptEditorRequest {
    pub project_id: String,
    /// `None` adds an action.
    pub script_id: Option<String>,
    /// Prefilled editor form (e.g. after a failed `From t3.json` quick-import).
    pub request: Option<EditorRequest>,
}

#[derive(Default)]
pub struct PendingScriptEditor(pub Option<ScriptEditorRequest>);

impl gpui::Global for PendingScriptEditor {}

/// Opens the action editor on `project_id`'s actions: Add, or Edit on `script_id`.
pub fn open_script_editor(project_id: &str, script_id: Option<&str>, cx: &mut App) {
    cx.set_global(PendingScriptEditor(Some(ScriptEditorRequest { project_id: project_id.to_owned(), script_id: script_id.map(str::to_owned), request: None })));
    crate::settings::open_project(project_id, cx);
}

/// Opens the action editor prefilled with `request` (e.g. a failed quick-import).
pub fn open_script_editor_request(project_id: &str, request: EditorRequest, cx: &mut App) {
    cx.set_global(PendingScriptEditor(Some(ScriptEditorRequest { project_id: project_id.to_owned(), script_id: None, request: Some(request) })));
    crate::settings::open_project(project_id, cx);
}

pub fn init(state: Entity<AppState>, cx: &mut App) -> Entity<GitStore> {
    cx.new(|cx| GitStore::new(state, cx))
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;

    #[gpui::test]
    fn draft_pr_checkout_blocks_worktree_mode_and_ignores_worktree_results(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        let store = cx.update(|cx| cx.new(|cx| GitStore::new(state.clone(), cx)));
        let draft_target = Target {
            thread_id: "draft:proj-1".into(),
            project_id: "proj-1".into(),
            is_draft: true,
            cwd: "/repo".into(),
            project_cwd: "/repo".into(),
            worktree_path: None,
            branch: Some("main".into()),
        };

        store.update(cx, |store, cx| {
            store.target = Some(draft_target.clone());
            assert!(
                store.prepare_pull_request_thread("#42", vcs::PullRequestCheckoutMode::Worktree, cx).is_none(),
                "Worktree PR checkout must be refused on drafts"
            );
            assert!(
                store.prepare_pull_request_thread("#42", vcs::PullRequestCheckoutMode::Local, cx).is_some(),
                "Local PR checkout remains allowed on drafts"
            );

            // Defensive guard: a worktree result on a draft must never corrupt optimistic_branch.
            let orphan_result = vcs::PreparePullRequestThreadResult {
                pull_request: vcs::ResolvedPullRequest {
                    number: 42,
                    title: "Test PR".into(),
                    url: "https://github.com/org/repo/pull/42".into(),
                    base_branch: "main".into(),
                    head_branch: "feat/pr-42".into(),
                    state: "open".into(),
                },
                branch: "feat/pr-42".into(),
                worktree_path: Some("/repo-wt/pr-42".into()),
                is_on_pull_request_head: true,
            };
            store.apply_prepared_pull_request(&draft_target, &orphan_result, cx);
            assert_eq!(store.optimistic_branch, None);

            // Local result on a draft updates optimistic_branch cleanly.
            let local_result = vcs::PreparePullRequestThreadResult {
                worktree_path: None,
                ..orphan_result
            };
            store.apply_prepared_pull_request(&draft_target, &local_result, cx);
            assert_eq!(store.optimistic_branch, Some(Some("feat/pr-42".into())));
        });
    }
}
