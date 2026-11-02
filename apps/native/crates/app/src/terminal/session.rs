//! One terminal's connection to the server: attaches to its PTY while this device holds
//! `terminal:operate`, observes it read-only with `terminal:read`, and switches as the grants
//! change. Output is parsed into the emulator on the network runtime in bounded slices, and a
//! publisher copies the emulator into a front buffer at most once per frame, so the renderer
//! never waits on parsing. Ports `createTerminalSession` in `client-runtime/src/state/terminal.ts`
//! and the attach/observe choice of `apps/web/src/state/terminalSessions.ts`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;
use t3_client::session::RpcError;
use t3_client::supervisor::ClientHandle;
use t3_core::terminal::emulator::{Emulator, FairMutex, Snapshot, ThemeColors};
use t3_core::terminal::{
    self, AttachEvent, BufferState, BufferStatus, Grid, LaunchLocation, LifecycleCommand, OutputOp, SessionFollow, SessionMode,
    TerminalAccess, method,
};
use tokio::sync::{Notify, mpsc};

const AUTHORIZATION_ERROR: &str = "EnvironmentAuthorizationError";
/// Output parsed per hold of the emulator lock, as alacritty bounds its reader.
const MAX_LOCKED_PARSE: usize = u16::MAX as usize;
/// The publisher copies the emulator for the renderer at most once per frame.
const FRAME: Duration = Duration::from_millis(16);

pub struct SessionCore {
    pub emulator: Emulator,
    pub buffer: BufferState,
    follow: SessionFollow,
    /// The grid the pane fits; the PTY hears it once it settles.
    pub measured: Option<Grid>,
    /// Input goes nowhere unless attached.
    pub read_only: bool,
    shown_error: Option<String>,
    announced: BufferStatus,
}

impl SessionCore {
    /// The emulator's grid: the pane's fit, capped by the grid the server shares.
    fn fit(&self) -> Grid {
        match self.measured {
            Some(measured) => Grid::fit_within(self.buffer.grid, measured),
            None => self.buffer.grid.unwrap_or_else(|| self.emulator.grid()),
        }
    }

    /// Fits the pane's new grid and cell size.
    pub fn lay_out(&mut self, measured: Grid, cell: (f32, f32)) {
        self.measured = Some(measured);
        let fit = self.fit();
        self.emulator.resize(fit);
        self.emulator.set_cell_size(cell.0, cell.1);
    }
}

pub type Shared = Arc<FairMutex<SessionCore>>;

/// One published copy of the emulator.
#[derive(Default)]
pub struct Frame {
    pub generation: u64,
    pub snapshot: Snapshot,
}

/// The front buffer: the newest frame, swapped out by the renderer.
#[derive(Default)]
pub struct Published {
    next: u64,
    frame: Frame,
}

pub type Front = Arc<Mutex<Published>>;

/// Copies the emulator into the front buffer. The caller holds the emulator lock, so frames are
/// published in the order the emulator changed; the front lock is held only for the swap.
fn publish(core: &SessionCore, front: &Front, back: &mut Frame) {
    core.emulator.snapshot(&mut back.snapshot);
    let mut published = front.lock().unwrap_or_else(PoisonError::into_inner);
    published.next += 1;
    back.generation = published.next;
    std::mem::swap(&mut published.frame, back);
}

/// Moves a newer published frame into the renderer's own.
pub fn take_newer(front: &Front, frame: &mut Frame) {
    let mut published = front.lock().unwrap_or_else(PoisonError::into_inner);
    if published.frame.generation > frame.generation {
        std::mem::swap(&mut published.frame, frame);
    }
}

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
    pub front: Front,
    scratch: RefCell<Frame>,
    client: ClientHandle,
    runtime: tokio::runtime::Handle,
    _driver: AbortOnDrop,
    _publisher: AbortOnDrop,
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
            follow: SessionFollow::default(),
            measured: None,
            read_only: true,
            shown_error: None,
            announced: BufferStatus::Closed,
        }));
        let front = Front::default();
        let (signals, receiver) = mpsc::unbounded_channel();
        let pending = Arc::new(AtomicBool::new(false));
        let target = Arc::new(target);
        let notifier = Notifier { signals, pending: pending.clone() };
        let dirty = Arc::new(Notify::new());
        let publisher = runtime.spawn(publish_frames(shared.clone(), front.clone(), dirty.clone(), notifier.clone()));
        let driver = Driver { target: target.clone(), client: client.clone(), shared: shared.clone(), notifier, dirty };
        let handle = runtime.spawn(driver.run());
        let session = Session {
            target,
            shared,
            front,
            scratch: RefCell::default(),
            client,
            runtime,
            _driver: AbortOnDrop(handle),
            _publisher: AbortOnDrop(publisher),
        };
        (session, Signals { receiver, pending })
    }

    /// Changes the emulator from the UI and publishes the result for the next paint.
    pub fn update<R>(&self, change: impl FnOnce(&mut SessionCore) -> R) -> R {
        let mut core = self.shared.lock();
        let result = change(&mut core);
        publish(&core, &self.front, &mut self.scratch.borrow_mut());
        result
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

/// Publishes the emulator whenever it changed, at most once per frame.
async fn publish_frames(shared: Shared, front: Front, dirty: Arc<Notify>, notifier: Notifier) {
    let mut back = Frame::default();
    loop {
        dirty.notified().await;
        publish(&shared.lock(), &front, &mut back);
        notifier.changed();
        tokio::time::sleep(FRAME).await;
    }
}

struct Driver {
    target: Arc<Target>,
    client: ClientHandle,
    shared: Shared,
    notifier: Notifier,
    dirty: Arc<Notify>,
}

enum End {
    /// The connection dropped; resubscribe the same way once it is back.
    Disconnected,
    /// The grants changed what this session may follow.
    Switched,
    /// The server finished the stream: the terminal is gone.
    Completed,
    Failed(t3_core::rpc::RpcFailure),
}

impl Driver {
    async fn run(self) {
        let Target { thread_id, terminal_id, location } = &*self.target;
        let mut grants = self.client.session_state();
        // Revalidate on mount, as the web's session atom does.
        self.client.refresh_session_state();
        let (mut replied, mut attempt) = (false, 0u32);
        loop {
            let access = TerminalAccess::of(grants.borrow_and_update().as_ref());
            let (mode, previous) = {
                let mut core = self.shared.lock();
                let previous = core.follow.mode();
                let mode = core.follow.follow(access);
                core.read_only = mode != Some(SessionMode::Attached);
                (mode, previous)
            };
            let Some(mode) = mode else {
                if grants.changed().await.is_err() {
                    return;
                }
                continue;
            };
            if mode == SessionMode::Attached && previous != Some(SessionMode::Attached) {
                replied = false;
            }
            let observing = mode == SessionMode::Observing;
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
                    let flush = async {
                        match deadline {
                            Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
                            None => std::future::pending().await,
                        }
                    };
                    let next = tokio::select! {
                        next = stream.receiver.recv() => next,
                        () = flush => {
                            let replies = self.shared.lock().emulator.flush_sync();
                            self.reply(&replies, observing);
                            self.dirty.notify_one();
                            continue;
                        }
                        changed = grants.changed() => match changed {
                            Ok(()) if TerminalAccess::of(grants.borrow().as_ref()).mode() != Some(mode) => break End::Switched,
                            Ok(()) => continue,
                            Err(_) => return,
                        },
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
            match end {
                End::Disconnected => {}
                End::Switched => continue,
                End::Completed => return,
                End::Failed(failure) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => {
                    // The grants were stale: follow what a fresh fetch allows.
                    self.show_error(failure.message);
                    self.client.refresh_session_state();
                    if grants.changed().await.is_err() {
                        return;
                    }
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
        let op = {
            let mut core = self.shared.lock();
            let op = core.buffer.apply(event);
            let fit = core.fit();
            core.emulator.resize(fit);
            op
        };
        let replies = match op {
            OutputOp::Reset(history) => {
                self.replay(&history);
                String::new()
            }
            OutputOp::Append(data) => self.feed(data.as_bytes()),
            OutputOp::None => String::new(),
        };
        let mut core = self.shared.lock();
        if let Some(error) = core.buffer.error.clone().filter(|error| core.shown_error.as_ref() != Some(error)) {
            core.emulator.print_local(&terminal::system_message(&error));
            core.shown_error = Some(error);
        }
        let (status, version) = (core.buffer.status, core.buffer.version);
        let ended = matches!(status, BufferStatus::Exited | BufferStatus::Closed);
        let command = core.follow.status(status, version);
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
        self.dirty.notify_one();

        self.reply(&replies, observing);
        let Target { thread_id, terminal_id, location } = &*self.target;
        if let (true, Some(grid)) = (resize, measured) {
            tokio::spawn(self.client.request(method::RESIZE, terminal::resize_input(thread_id, terminal_id, grid)));
        }
        match command {
            Some(LifecycleCommand::Exit) => self.notifier.send(Signal::Exited),
            Some(LifecycleCommand::Reopen) => {
                let open = self.client.request(method::OPEN, terminal::open_input(thread_id, terminal_id, location, measured));
                let shared = self.shared.clone();
                tokio::spawn(async move {
                    if open.await.is_err() {
                        shared.lock().follow.reopen_failed();
                    }
                });
            }
            None => {}
        }
    }

    /// Live output, parsed in slices so the UI can take the lock between them.
    fn feed(&self, bytes: &[u8]) -> String {
        let mut replies = String::new();
        for slice in bytes.chunks(MAX_LOCKED_PARSE) {
            replies.push_str(&self.shared.lock().emulator.feed(slice));
        }
        replies
    }

    /// History replays into a fresh emulator off the lock, then replaces the old one.
    fn replay(&self, history: &str) {
        let mut next = self.shared.lock().emulator.blank();
        next.replay(history);
        let mut core = self.shared.lock();
        next.adopt_view(&core.emulator);
        core.emulator = next;
        core.shown_error = None;
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
        self.dirty.notify_one();
    }
}
