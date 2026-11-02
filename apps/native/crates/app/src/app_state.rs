//! App-wide state: the connection, the shell, and the open thread. Network events arrive on a
//! channel from the client's runtime and are applied in batches, one notify per batch.

use std::future::Future;

use gpui::{Context, EventEmitter, Task};
use serde_json::Value;
use t3_client::auth::{self, Credentials};
use t3_client::session::RpcError;
use t3_client::store;
use t3_client::supervisor::{ClientEvent, ClientHandle, ConnectionStatus};
use t3_core::commands::{self, method};
use t3_core::composer::ClientSettings;
use t3_core::history::{HistoryStep, PageOutcome};
use t3_core::pairing::parse_pairing_link;
use t3_core::server_config::ServerConfigState;
use t3_core::shell::ShellState;
use t3_core::thread::ThreadState;
use t3_core::workflows::{self, StopTarget};

use crate::sidebar::controller::SidebarController;
use crate::toast::Toast;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// No saved credentials, or they stopped working.
    Unpaired { error: Option<String> },
    Pairing,
    Online(ConnectionStatus),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    Phase,
    Shell,
    Selection,
    Thread,
    ServerConfig,
    /// Only `thread.history_control()` changed: loading, error, or more history.
    History,
    /// Toasts and the Undo notice.
    Notice,
    /// Right panel tabs, visibility, width, or the diff selection changed.
    RightPanel,
}

pub struct AppState {
    pub phase: Phase,
    pub shell: ShellState,
    pub sidebar: SidebarController,
    pub toast: Option<Toast>,
    pub(crate) toast_timer: Option<Task<()>>,
    pub selected_thread: Option<String>,
    pub thread: ThreadState,
    pub last_error: Option<String>,
    pub server_config: ServerConfigState,
    pub client_settings: ClientSettings,
    /// The project of the new-thread draft, while the user composes a thread that does not exist.
    pub draft_project: Option<String>,
    pub panels: crate::right_panel::PanelState,
    restore_thread: Option<String>,
    /// Rows the shell leaves to on-demand reads: older settled pages and thread families.
    pub(crate) omitted: crate::omitted_rows::OmittedRows,
    /// A thread just launched from the draft, selected once the shell lists it.
    select_when_listed: Option<String>,
    client: Option<ClientHandle>,
    runtime: tokio::runtime::Handle,
    _pump: Option<Task<()>>,
}

impl EventEmitter<AppEvent> for AppState {}

const MAX_BATCH: usize = 512;

impl AppState {
    pub fn new(runtime: tokio::runtime::Handle, cx: &mut Context<Self>) -> Self {
        let mut state = AppState {
            phase: Phase::Unpaired { error: None },
            shell: ShellState::default(),
            sidebar: SidebarController::default(),
            toast: None,
            toast_timer: None,
            selected_thread: None,
            thread: ThreadState::default(),
            last_error: None,
            server_config: ServerConfigState::default(),
            client_settings: ClientSettings::from_json(&store::load_client_settings().unwrap_or_default()),
            draft_project: None,
            panels: crate::right_panel::PanelState::load(),
            restore_thread: store::load_last_thread(),
            omitted: Default::default(),
            select_when_listed: None,
            client: None,
            runtime,
            _pump: None,
        };
        if let Some(credentials) = store::load() {
            state.start_client(credentials, cx);
        }
        state
    }

    pub fn pair(&mut self, link: &str, cx: &mut Context<Self>) {
        let link = match parse_pairing_link(link, None) {
            Ok(link) => link,
            Err(error) => {
                self.set_phase(Phase::Unpaired { error: Some(error.to_string()) }, cx);
                return;
            }
        };
        self.set_phase(Phase::Pairing, cx);
        let redeem = self.runtime.spawn(async move { auth::redeem_pairing(&link).await });
        cx.spawn(async move |this, cx| {
            let result = redeem.await.map_err(|error| error.to_string()).and_then(|r| r.map_err(|e| e.to_string()));
            let _ = this.update(cx, |state, cx| match result {
                Ok(credentials) => {
                    if let Err(error) = store::save(&credentials) {
                        tracing::warn!(%error, "could not save credentials");
                    }
                    state.start_client(credentials, cx);
                }
                Err(error) => state.set_phase(Phase::Unpaired { error: Some(error) }, cx),
            });
        })
        .detach();
    }

    pub fn forget_connection(&mut self, cx: &mut Context<Self>) {
        store::forget();
        self.client = None;
        self._pump = None;
        self.shell = ShellState::default();
        self.sidebar = SidebarController::default();
        self.omitted = Default::default();
        self.selected_thread = None;
        self.thread = ThreadState::default();
        self.server_config = ServerConfigState::default();
        self.draft_project = None;
        self.set_phase(Phase::Unpaired { error: None }, cx);
        cx.emit(AppEvent::Shell);
        cx.emit(AppEvent::Selection);
    }

    fn start_client(&mut self, credentials: Credentials, cx: &mut Context<Self>) {
        let (client, mut events) = t3_client::supervisor::start(&self.runtime, credentials);
        self.client = Some(client);
        self.set_phase(Phase::Online(ConnectionStatus::Connecting), cx);
        self._pump = Some(cx.spawn(async move |this, cx| {
            while let Some(first) = events.recv().await {
                let mut batch = vec![first];
                while batch.len() < MAX_BATCH {
                    match events.try_recv() {
                        Ok(event) => batch.push(event),
                        Err(_) => break,
                    }
                }
                if this.update(cx, |state, cx| state.apply_batch(batch, cx)).is_err() {
                    return;
                }
            }
        }));
    }

    fn apply_batch(&mut self, batch: Vec<ClientEvent>, cx: &mut Context<Self>) {
        let (mut shell_changed, mut sidebar_changed, mut thread_changed, mut config_changed) = (false, false, false, false);
        for event in batch {
            match event {
                ClientEvent::Status(ConnectionStatus::NeedsPairing { reason }) => {
                    store::forget();
                    self.client = None;
                    self.set_phase(Phase::Unpaired { error: Some(reason) }, cx);
                }
                ClientEvent::Status(status) => self.set_phase(Phase::Online(status), cx),
                ClientEvent::Shell(item) => {
                    let quiet = crate::sidebar::controller::is_streaming_tick(&self.shell, &item);
                    if self.shell.apply(item) {
                        shell_changed = true;
                        sidebar_changed |= !quiet;
                    }
                }
                ClientEvent::Thread { thread_id, item } => {
                    if self.selected_thread.as_deref() == Some(thread_id.as_str()) {
                        thread_changed |= self.thread.apply(*item);
                    }
                }
                ClientEvent::ServerConfig(event) => config_changed |= self.server_config.apply(*event),
                ClientEvent::StreamError { context, message } => {
                    self.last_error = Some(format!("Could not read {context}: {message}"));
                }
            }
        }
        if shell_changed {
            self.sync_omitted_rows(cx);
            if sidebar_changed {
                self.rebuild_sidebar(cx);
            }
            cx.emit(AppEvent::Shell);
            if let Some(id) = self.select_when_listed.take_if(|id| self.shell.thread(id).is_some()) {
                self.select_thread(&id, cx);
            } else if self.selected_thread.is_none()
                && self.draft_project.is_none()
                && let Some(id) = self.restore_thread.take_if(|id| self.shell.thread(id).is_some())
            {
                self.select_thread(&id, cx);
            } else if self.selected_thread.is_none()
                && self.draft_project.is_none()
                && self.shell.synchronized
                && let Some(id) = self.restore_thread.take()
            {
                // A shell that omits older settled or idle threads can still hold it: ask its family.
                self.read_family(&id, true, cx);
            }
        }
        if thread_changed && self.thread.deleted {
            // The web treats a deleted open thread as missing and goes home.
            self.close_selected_thread(cx);
        } else if thread_changed {
            cx.emit(AppEvent::Thread);
        }
        if config_changed {
            self.sync_server_config(cx);
            cx.emit(AppEvent::ServerConfig);
        }
    }

    fn close_selected_thread(&mut self, cx: &mut Context<Self>) {
        if let (Some(client), Some(thread_id)) = (&self.client, self.selected_thread.take()) {
            client.close_thread(&thread_id);
        }
        self.release_families(None);
        self.thread = ThreadState::default();
        cx.emit(AppEvent::Selection);
        cx.emit(AppEvent::Thread);
    }

    fn set_phase(&mut self, phase: Phase, cx: &mut Context<Self>) {
        if self.phase != phase {
            self.phase = phase;
            cx.emit(AppEvent::Phase);
            // Connection state places threads on the Working shelf.
            if self.shell.snapshot.is_some() {
                self.rebuild_sidebar(cx);
                cx.emit(AppEvent::Shell);
            }
        }
    }

    pub fn select_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.selected_thread.as_deref() == Some(thread_id) {
            return;
        }
        self.draft_project = None;
        if let (Some(client), Some(previous)) = (&self.client, &self.selected_thread) {
            client.close_thread(previous);
        }
        self.selected_thread = Some(thread_id.to_owned());
        self.thread = ThreadState::default();
        self.restore_thread = None;
        store::save_last_thread(thread_id);
        if let Some(client) = &self.client {
            client.open_thread(thread_id);
        }
        // An idle subagent or older settled thread opens from its family when the shell omits it.
        self.release_families(Some(thread_id));
        if self.thread_shell(thread_id).is_none() {
            self.read_family(thread_id, false, cx);
        }
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Selection);
        cx.emit(AppEvent::Thread);
    }

    /// Loads the page of history before the open thread's window, and keeps paging while pages
    /// hold no user turn (`threads.ts` `loadEarlier`). The transcript's "Load earlier turns"
    /// control and its retry call this. A no-op while loading or at the start.
    pub fn load_older_history(&mut self, cx: &mut Context<Self>) {
        let (Some(thread_id), Some(client)) = (self.selected_thread.clone(), self.client.clone()) else { return };
        let Some(cursor) = self.thread.begin_history_load() else { return };
        cx.emit(AppEvent::History);
        self.fetch_history_page(thread_id, cursor, client, cx);
    }

    /// Fetches and decodes on the network runtime, merges on the background executor, and
    /// touches the UI thread only to check the projection out and swap the result back.
    fn fetch_history_page(&mut self, thread_id: String, cursor: String, client: ClientHandle, cx: &mut Context<Self>) {
        let task = {
            let (client, thread_id, cursor) = (client.clone(), thread_id.clone(), cursor.clone());
            self.runtime.spawn(async move { client.history_page(&thread_id, &cursor).await })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Could not load earlier activity.".into()));
            let started = this.update(cx, |state, cx| {
                if state.selected_thread.as_deref() != Some(thread_id.as_str()) {
                    return None;
                }
                let outcome = match result {
                    Ok(page) => state.thread.begin_history_merge(&cursor, page),
                    Err(message) => PageOutcome::Step(state.thread.fail_history_load(&cursor, message)),
                };
                match outcome {
                    PageOutcome::Merge(merge) => {
                        cx.emit(AppEvent::History);
                        Some(merge)
                    }
                    PageOutcome::Step(step) => {
                        if step != HistoryStep::Stale {
                            cx.emit(AppEvent::History);
                        }
                        None
                    }
                }
            });
            let Ok(Some(merge)) = started else { return };
            let merged = cx.background_executor().spawn(async move { merge.run() }).await;
            let _ = this.update(cx, |state, cx| {
                if state.selected_thread.as_deref() != Some(thread_id.as_str()) {
                    return;
                }
                let step = state.thread.finish_history_merge(merged);
                if step == HistoryStep::Stale {
                    return;
                }
                cx.emit(AppEvent::Thread);
                cx.emit(AppEvent::History);
                if let HistoryStep::Continue(next) = step {
                    state.fetch_history_page(thread_id, next, client, cx);
                }
            });
        })
        .detach();
    }

    /// Opens the new-thread draft for `project_id`, leaving the open thread.
    pub fn start_draft(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if self.draft_project.as_deref() == Some(project_id) && self.selected_thread.is_none() {
            return;
        }
        if let (Some(client), Some(previous)) = (&self.client, self.selected_thread.take()) {
            client.close_thread(&previous);
        }
        self.thread = ThreadState::default();
        self.restore_thread = None;
        self.draft_project = Some(project_id.to_owned());
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Selection);
        cx.emit(AppEvent::Thread);
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if let (Some(client), Some(previous)) = (&self.client, self.selected_thread.take()) {
            client.close_thread(&previous);
        }
        self.thread = ThreadState::default();
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Selection);
        cx.emit(AppEvent::Thread);
    }

    /// Selects a thread the draft launched as soon as the shell lists it.
    pub fn select_when_listed(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.shell.thread(thread_id).is_some() {
            self.select_thread(thread_id, cx);
        } else {
            self.select_when_listed = Some(thread_id.to_owned());
        }
    }

    /// Any RPC on the network runtime. The future resolves wherever it is awaited.
    /// Nothing is sent until the future is first polled, so futures built together still run in
    /// the order they are awaited.
    pub fn rpc(&self, method: &'static str, payload: Value) -> impl Future<Output = Result<Value, RpcError>> + use<> {
        let (client, runtime) = (self.client.clone(), self.runtime.clone());
        async move {
            let Some(client) = client else { return Err(RpcError::Closed) };
            runtime.spawn(async move { client.request(method, payload).await }).await.unwrap_or(Err(RpcError::Closed))
        }
    }

    /// Dispatches commands in order, each after the previous one committed, stopping at the
    /// first failure with its index.
    pub fn dispatch_in_order(&self, commands: Vec<Value>) -> impl Future<Output = Result<(), (usize, RpcError)>> + use<> {
        let (client, runtime) = (self.client.clone(), self.runtime.clone());
        async move {
            let Some(client) = client else { return Err((0, RpcError::Closed)) };
            runtime.spawn(async move { client.dispatch_in_order(commands).await }).await.unwrap_or(Err((0, RpcError::Closed)))
        }
    }

    pub(crate) fn client_handle(&self) -> Option<ClientHandle> {
        self.client.clone()
    }

    pub(crate) fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }

    pub fn is_selected_thread_running(&self) -> bool {
        self.selected_thread
            .as_deref()
            .and_then(|id| self.thread_shell(id))
            .is_some_and(|thread| thread.is_live())
    }

    /// `interruptThreadTurn`: `thread.stop` where the server has it, else interrupt the live run
    /// or the settled run that left background work running.
    pub fn stop_selected_thread(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.selected_thread.clone() else { return };
        let thread_stop = self.server_config.config.as_ref().is_none_or(|c| c.environment.capabilities.thread_stop == Some(true));
        let projection = self.thread.projection.as_ref().filter(|projection| projection.thread.id == thread_id);
        let command = match workflows::stop_target(projection, thread_stop) {
            StopTarget::Thread => commands::stop_thread(&uuid_v4(), &thread_id),
            StopTarget::Run(run_id) => commands::interrupt_run(&uuid_v4(), &thread_id, &run_id),
            StopTarget::Nothing => return,
        };
        self.dispatch(command, cx);
    }

    /// One unary RPC, run and decoded on the network runtime so large replies never parse on the
    /// UI thread. `None` while there is no connection.
    pub fn request<T: Send + 'static>(
        &self,
        method: &'static str,
        payload: Value,
        decode: impl FnOnce(Value) -> Result<T, String> + Send + 'static,
    ) -> Option<tokio::task::JoinHandle<Result<T, String>>> {
        let client = self.client.clone()?;
        Some(self.runtime.spawn(async move { decode(client.request(method, payload).await.map_err(|error| error.to_string())?) }))
    }

    /// The connected server's address, `None` while unpaired.
    pub fn environment(&self) -> Option<&str> {
        self.client.as_ref().map(ClientHandle::environment)
    }

    fn dispatch(&mut self, command: Value, cx: &mut Context<Self>) {
        let task = self.rpc(method::DISPATCH_COMMAND, command);
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |state, cx| {
                    state.last_error = Some(error.to_string());
                    cx.emit(AppEvent::Thread);
                });
            }
        })
        .detach();
    }
}

pub fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}
