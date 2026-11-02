//! App-wide state: the connection, the shell, and the open thread. Network events arrive on a
//! channel from the client's runtime and are applied in batches, one notify per batch.

use std::sync::Arc;

use gpui::{Context, EventEmitter, Task};
use serde_json::Value;
use t3_client::auth::{self, Credentials};
use t3_client::session::RpcError;
use t3_client::store;
use t3_client::supervisor::{ClientEvent, ClientHandle, ConnectionStatus};
use t3_core::commands::{self, DispatchMode};
use t3_core::pairing::parse_pairing_link;
use t3_core::shell::ShellState;
use t3_core::sidebar::{SidebarRow, sidebar_rows};
use t3_core::thread::ThreadState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// No saved credentials, or they stopped working.
    Unpaired { error: Option<String> },
    Pairing,
    Online(ConnectionStatus),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    PhaseChanged,
    ShellChanged,
    SelectionChanged,
    ThreadChanged,
}

pub struct AppState {
    pub phase: Phase,
    pub shell: ShellState,
    pub sidebar_rows: Arc<Vec<SidebarRow>>,
    pub selected_thread: Option<String>,
    pub thread: ThreadState,
    pub last_error: Option<String>,
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
            sidebar_rows: Arc::new(Vec::new()),
            selected_thread: None,
            thread: ThreadState::default(),
            last_error: None,
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
        self.sidebar_rows = Arc::new(Vec::new());
        self.selected_thread = None;
        self.thread = ThreadState::default();
        self.set_phase(Phase::Unpaired { error: None }, cx);
        cx.emit(AppEvent::ShellChanged);
        cx.emit(AppEvent::SelectionChanged);
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
        let (mut shell_changed, mut thread_changed) = (false, false);
        for event in batch {
            match event {
                ClientEvent::Status(ConnectionStatus::NeedsPairing { reason }) => {
                    store::forget();
                    self.client = None;
                    self.set_phase(Phase::Unpaired { error: Some(reason) }, cx);
                }
                ClientEvent::Status(status) => self.set_phase(Phase::Online(status), cx),
                ClientEvent::Shell(item) => shell_changed |= self.shell.apply(item),
                ClientEvent::Thread { thread_id, item } => {
                    if self.selected_thread.as_deref() == Some(thread_id.as_str()) {
                        thread_changed |= self.thread.apply(*item);
                    }
                }
                ClientEvent::StreamError { context, message } => {
                    self.last_error = Some(format!("Could not read {context}: {message}"));
                }
            }
        }
        if shell_changed {
            self.sidebar_rows = Arc::new(self.shell.snapshot.as_ref().map(sidebar_rows).unwrap_or_default());
            cx.emit(AppEvent::ShellChanged);
        }
        if thread_changed {
            cx.emit(AppEvent::ThreadChanged);
        }
    }

    fn set_phase(&mut self, phase: Phase, cx: &mut Context<Self>) {
        if self.phase != phase {
            self.phase = phase;
            cx.emit(AppEvent::PhaseChanged);
        }
    }

    pub fn select_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.selected_thread.as_deref() == Some(thread_id) {
            return;
        }
        if let (Some(client), Some(previous)) = (&self.client, &self.selected_thread) {
            client.close_thread(previous);
        }
        self.selected_thread = Some(thread_id.to_owned());
        self.thread = ThreadState::default();
        if let Some(client) = &self.client {
            client.open_thread(thread_id);
        }
        cx.emit(AppEvent::SelectionChanged);
        cx.emit(AppEvent::ThreadChanged);
    }

    pub fn is_selected_thread_running(&self) -> bool {
        self.selected_thread
            .as_deref()
            .and_then(|id| self.shell.thread(id))
            .is_some_and(|thread| thread.is_live())
    }

    pub fn send_message(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(thread_id) = self.selected_thread.clone() else { return };
        let mode = match self.shell.thread(&thread_id).and_then(|t| t.active_run_id.clone()) {
            Some(_) => DispatchMode::QueueAfterActive,
            None => DispatchMode::StartImmediately,
        };
        let command_id = uuid_v4();
        self.dispatch(commands::send_message(&command_id, &thread_id, &text, &mode), cx);
    }

    pub fn stop_selected_thread(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.selected_thread.clone() else { return };
        self.dispatch(commands::stop_thread(&uuid_v4(), &thread_id), cx);
    }

    fn dispatch(&mut self, command: Value, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let task = self.runtime.spawn(async move { client.dispatch(command).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or(Err(RpcError::Closed));
            if let Err(error) = result {
                let _ = this.update(cx, |state, cx| {
                    state.last_error = Some(error.to_string());
                    cx.emit(AppEvent::ThreadChanged);
                });
            }
        })
        .detach();
    }
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}
