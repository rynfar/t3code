//! One terminal's connection to the server: attaches to its PTY (or observes it read-only when
//! this device lacks `terminal:operate`), parses output into the emulator on the network
//! runtime, and signals the pane, which repaints at most once per frame. Ports
//! `createTerminalSession` in `client-runtime/src/state/terminal.ts` and the attach/observe
//! choice of `apps/web/src/state/terminalSessions.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use t3_client::session::RpcError;
use t3_client::supervisor::ClientHandle;
use t3_core::terminal::emulator::{Emulator, FairMutex, ThemeColors};
use t3_core::terminal::{
    self, AttachEvent, BufferState, BufferStatus, Grid, LaunchLocation, Lifecycle, LifecycleCommand, LifecycleEvent,
    OutputOp, method,
};
use tokio::sync::mpsc;


const AUTHORIZATION_ERROR: &str = "EnvironmentAuthorizationError";

pub struct SessionCore {
    pub emulator: Emulator,
    pub buffer: BufferState,
    lifecycle: Lifecycle,
    /// The grid the pane fits; the PTY hears it once it settles.
    pub measured: Option<Grid>,
    pub read_only: bool,
    shown_error: Option<String>,
    announced: BufferStatus,
}

pub type Shared = Arc<FairMutex<SessionCore>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Changed,
    /// The shell ended while attached; the drawer closes the terminal.
    Exited,
}

pub struct Target {
    pub thread_id: String,
    pub terminal_id: String,
    pub location: LaunchLocation,
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct Session {
    pub target: Arc<Target>,
    pub shared: Shared,
    client: ClientHandle,
    runtime: tokio::runtime::Handle,
    _driver: AbortOnDrop,
}

/// Wakes the pane without flooding it: one pending `Changed` at a time.
#[derive(Clone)]
struct Notifier {
    signals: mpsc::UnboundedSender<Signal>,
    pending: Arc<AtomicBool>,
}

impl Notifier {
    fn changed(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            let _ = self.signals.send(Signal::Changed);
        }
    }

    fn send(&self, signal: Signal) {
        let _ = self.signals.send(signal);
    }
}

pub struct Signals {
    pub receiver: mpsc::UnboundedReceiver<Signal>,
    pub pending: Arc<AtomicBool>,
}

impl Session {
    pub fn start(
        target: Target,
        client: ClientHandle,
        runtime: tokio::runtime::Handle,
        theme: ThemeColors,
    ) -> (Session, Signals) {
        let shared: Shared = Arc::new(FairMutex::new(SessionCore {
            emulator: Emulator::new(Grid::new(80, 24), theme),
            buffer: BufferState::default(),
            lifecycle: Lifecycle::default(),
            measured: None,
            read_only: false,
            shown_error: None,
            announced: BufferStatus::Closed,
        }));
        let (signals, receiver) = mpsc::unbounded_channel();
        let pending = Arc::new(AtomicBool::new(false));
        let target = Arc::new(target);
        let notifier = Notifier { signals, pending: pending.clone() };
        let driver = Driver { target: target.clone(), client: client.clone(), shared: shared.clone(), notifier };
        let handle = runtime.spawn(driver.run());
        let session = Session { target, shared, client, runtime, _driver: AbortOnDrop(handle) };
        (session, Signals { receiver, pending })
    }

    /// Sends input in order. Resolves with the server's error, if any.
    pub fn write(&self, data: &str) -> Vec<tokio::task::JoinHandle<Result<Value, RpcError>>> {
        if data.is_empty() || self.shared.lock().read_only {
            return Vec::new();
        }
        let Target { thread_id, terminal_id, .. } = &*self.target;
        terminal::paste_chunks(data)
            .into_iter()
            .map(|chunk| self.runtime.spawn(self.client.request(method::WRITE, terminal::write_input(thread_id, terminal_id, chunk))))
            .collect()
    }

    /// Tells the PTY the settled grid, if this device operates a live shell.
    pub fn send_resize(&self) {
        let (grid, live) = {
            let core = self.shared.lock();
            (core.measured, !core.read_only && core.buffer.version > 0 && core.buffer.status == BufferStatus::Running)
        };
        if let (Some(grid), true) = (grid, live) {
            let Target { thread_id, terminal_id, .. } = &*self.target;
            self.runtime.spawn(self.client.request(method::RESIZE, terminal::resize_input(thread_id, terminal_id, grid)));
        }
    }
}

struct Driver {
    target: Arc<Target>,
    client: ClientHandle,
    shared: Shared,
    notifier: Notifier,
}

enum End {
    /// The connection dropped; resubscribe once it is back.
    Disconnected,
    /// The server finished the stream: the terminal is gone.
    Completed,
    Failed(t3_core::rpc::RpcFailure),
}

impl Driver {
    async fn run(self) {
        let Target { thread_id, terminal_id, location } = &*self.target;
        let (mut replied, mut observing, mut attempt) = (false, false, 0u32);
        loop {
            let payload = if observing {
                terminal::observe_input(thread_id, terminal_id)
            } else {
                let grid = self.shared.lock().measured;
                terminal::attach_input(thread_id, terminal_id, location, grid, replied)
            };
            let method = if observing { method::OBSERVE } else { method::ATTACH };
            let end = match self.client.stream(method, payload).await {
                Err(RpcError::Closed) => End::Disconnected,
                Err(RpcError::Failure(failure)) => End::Failed(failure),
                Ok(mut stream) => loop {
                    let deadline = self.shared.lock().emulator.sync_deadline();
                    let next = match deadline {
                        Some(deadline) => match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), stream.receiver.recv()).await {
                            Ok(next) => next,
                            Err(_) => {
                                let replies = self.shared.lock().emulator.flush_sync();
                                self.reply(&replies, observing);
                                self.notifier.changed();
                                continue;
                            }
                        },
                        None => stream.receiver.recv().await,
                    };
                    match next {
                        Some(Ok(value)) => {
                            replied = true;
                            attempt = 0;
                            match serde_json::from_value::<AttachEvent>(value) {
                                Ok(event) => self.apply(event, observing),
                                Err(error) => tracing::warn!(%error, "undecodable terminal event"),
                            }
                        }
                        Some(Err(RpcError::Failure(failure))) => break End::Failed(failure),
                        Some(Err(RpcError::Closed)) => break End::Disconnected,
                        None => break End::Completed,
                    }
                },
            };
            {
                let mut core = self.shared.lock();
                core.lifecycle.transition(LifecycleEvent::Detached { can_operate: !observing });
            }
            match end {
                End::Disconnected => {}
                End::Completed => return,
                End::Failed(failure) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) && !observing => {
                    observing = true;
                    let mut core = self.shared.lock();
                    core.read_only = true;
                    core.lifecycle.transition(LifecycleEvent::Observed);
                    drop(core);
                    self.notifier.changed();
                    continue;
                }
                End::Failed(failure) => {
                    tracing::warn!(terminal = %terminal_id, message = %failure.message, "terminal stream failed");
                    self.show_error(failure.message);
                    return;
                }
            }
            attempt += 1;
            tokio::time::sleep(Duration::from_millis((125u64 << attempt.min(4)).min(2_000))).await;
        }
    }

    fn apply(&self, event: AttachEvent, observing: bool) {
        let fresh = matches!(event, AttachEvent::Snapshot { .. } | AttachEvent::Restarted { .. });
        let mut core = self.shared.lock();
        let op = core.buffer.apply(event);
        let local = match core.measured {
            Some(measured) => Grid::fit_within(core.buffer.grid, measured),
            None => core.buffer.grid.unwrap_or_else(|| core.emulator.grid()),
        };
        core.emulator.resize(local);
        let replies = match op {
            OutputOp::Reset(history) => {
                core.emulator.reset(&history);
                core.shown_error = None;
                String::new()
            }
            OutputOp::Append(data) => core.emulator.feed(data.as_bytes()),
            OutputOp::None => String::new(),
        };
        if let Some(error) = core.buffer.error.clone().filter(|error| core.shown_error.as_ref() != Some(error)) {
            core.emulator.print_local(&terminal::system_message(&error));
            core.shown_error = Some(error);
        }
        let (status, version) = (core.buffer.status, core.buffer.version);
        let ended = matches!(status, BufferStatus::Exited | BufferStatus::Closed);
        let command = core.lifecycle.transition(LifecycleEvent::Status { status, version });
        if version > 0 && ended && core.announced != status && command != Some(LifecycleCommand::Reopen) {
            let note = if status == BufferStatus::Closed { "Terminal closed" } else { "Process exited" };
            core.emulator.print_local(&terminal::system_message(note));
        }
        if version > 0 {
            core.announced = status;
        }
        let resize = fresh && status == BufferStatus::Running && !observing;
        let measured = core.measured;
        drop(core);

        self.reply(&replies, observing);
        let Target { thread_id, terminal_id, location } = &*self.target;
        if let (true, Some(grid)) = (resize, measured) {
            tokio::spawn(self.client.request(method::RESIZE, terminal::resize_input(thread_id, terminal_id, grid)));
        }
        match command {
            Some(LifecycleCommand::Exit) if !observing => self.notifier.send(Signal::Exited),
            Some(LifecycleCommand::Reopen) if !observing => {
                let open = self.client.request(method::OPEN, terminal::open_input(thread_id, terminal_id, location, measured));
                let shared = self.shared.clone();
                tokio::spawn(async move {
                    if open.await.is_err() {
                        shared.lock().lifecycle.transition(LifecycleEvent::ReopenFailed);
                    }
                });
            }
            _ => {}
        }
        self.notifier.changed();
    }

    fn reply(&self, replies: &str, observing: bool) {
        if replies.is_empty() || observing {
            return;
        }
        let Target { thread_id, terminal_id, .. } = &*self.target;
        tokio::spawn(self.client.request(method::WRITE, terminal::write_input(thread_id, terminal_id, replies)));
    }

    fn show_error(&self, message: String) {
        let mut core = self.shared.lock();
        if core.shown_error.as_ref() == Some(&message) {
            return;
        }
        core.emulator.print_local(&terminal::system_message(&message));
        core.shown_error = Some(message);
        drop(core);
        self.notifier.changed();
    }
}
