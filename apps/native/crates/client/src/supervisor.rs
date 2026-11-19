//! Keeps one environment connected: mints a ticket, opens a session, subscribes the shell and
//! every open thread, and on disconnect reconnects with backoff, resuming each stream from the
//! last sequence it saw. Each connection also fetches the device's grants
//! (`client-runtime/src/state/session.ts`). Frames are decoded here, off the UI thread.
//! A route that must be readied first, as an SSH tunnel is, passes a `Prepare` step that runs
//! before every attempt, as the web's connection resolver authorizes each one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use serde_json::Value;
use t3_core::auth::AuthSessionState;
use t3_core::commands::{self, ShellOptions, method};
use t3_core::compatibility::{ProtocolMismatch, mismatch_with, protocol_mismatch};
use t3_core::server_config::ServerConfigStreamEvent;
use t3_core::shell::ShellStreamItem;
use t3_core::history::HistoryPage;
use t3_core::thread::{ThreadStreamItem, decode_thread_stream_item};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::auth::{self, AuthError, Credentials};
use crate::session::{OpenError, RpcError, Session, StreamHandle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    /// Waiting to try again; `reason` is the last failure.
    Reconnecting { reason: String, attempt: u32 },
    /// Waiting for someone on the host to approve this T3 Connect device (`pingdotgg/t3code#36419`).
    AwaitingApproval { reason: String, attempt: u32 },
    /// The server speaks another orchestration protocol. Nothing retries until `retry_now`.
    Unsupported { reason: String, server_outdated: bool },
    NeedsPairing { reason: String },
    /// Preparing the route failed in a way retrying cannot fix, such as a cancelled SSH password
    /// prompt. Nothing retries until `retry_now`.
    Blocked { reason: String },
}

/// Extra fields read from raw `subscribeServerConfig` frames without modifying `t3-core`'s
/// `ServerConfig` schema: Connect device approval toggle (`#36419`), auth policy & direct
/// endpoints (`#35543` Tailscale Serve), socket thread pagination (`#33306`), and recent
/// host event-loop stall (`hostFreeze.ts`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerConfigExtras {
    pub require_connect_device_approval: Option<bool>,
    pub auth_policy: Option<String>,
    pub direct_endpoints: Vec<String>,
    pub thread_snapshot_pagination: bool,
    pub last_event_loop_stall: Option<crate::status::ServerEventLoopStall>,
    /// Accurate `subscribeServerConfig`-scoped clock sample `(server_time_ms, requested_at_ms, received_at_ms)` (`#36420`).
    pub clock_sample: Option<(Option<i64>, i64, i64)>,
}

/// Why preparing a route failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareError {
    /// Worth retrying with backoff.
    Transient(String),
    /// Waits for the user (`ConnectionBlockedError`).
    Blocked(String),
    /// Readied, but at this other address.
    Moved(String),
}

impl PrepareError {
    pub fn message(&self) -> &str {
        match self {
            PrepareError::Transient(message) | PrepareError::Blocked(message) => message,
            PrepareError::Moved(_) => "The connection moved to another local address.",
        }
    }
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// Readies the route before each connection attempt.
pub type Prepare = Arc<dyn Fn() -> futures::future::BoxFuture<'static, Result<(), PrepareError>> + Send + Sync>;

#[derive(Debug)]
pub enum ClientEvent {
    Status(ConnectionStatus),
    Shell(ShellStreamItem),
    Thread { thread_id: String, item: Box<ThreadStreamItem> },
    ServerConfig(Box<ServerConfigStreamEvent>),
    StreamError { context: String, message: String },
    /// The prepared route now listens at another address (an SSH tunnel had to change ports).
    /// The client stopped; the app starts one there.
    Moved { http_base: String },
}

enum Command {
    OpenThread(String),
    CloseThread(String),
    CloseAllThreads,
    RefreshSessionState,
    RetryNow,
    Request { method: &'static str, payload: Value, reply: oneshot::Sender<Result<Value, RpcError>>, settled: Option<crate::vcs::OnSettled> },
    Stream { method: &'static str, payload: Value, reply: oneshot::Sender<Result<StreamHandle, RpcError>>, settled: Option<crate::vcs::OnSettled> },
}

/// Keyed by environment `http_base` (just like `t3_core::clock` keys by environment id), so
/// `ClientHandle` keeps its exact struct layout while any caller can observe live extras.
fn extras_sender(http_base: &str) -> Arc<watch::Sender<ServerConfigExtras>> {
    static EXTRAS: OnceLock<RwLock<HashMap<String, Arc<watch::Sender<ServerConfigExtras>>>>> = OnceLock::new();
    let map = EXTRAS.get_or_init(|| RwLock::new(HashMap::new()));
    if let Ok(read) = map.read()
        && let Some(tx) = read.get(http_base)
    {
        return tx.clone();
    }
    let mut write = map.write().unwrap_or_else(|p| p.into_inner());
    write
        .entry(http_base.to_owned())
        .or_insert_with(|| Arc::new(watch::channel(ServerConfigExtras::default()).0))
        .clone()
}

/// Live extras decoded from `subscribeServerConfig` for `http_base`.
pub fn server_config_extras_for(http_base: &str) -> watch::Receiver<ServerConfigExtras> {
    extras_sender(http_base).subscribe()
}

#[derive(Clone)]
pub struct ClientHandle {
    pub(crate) refs: crate::vcs::RefState,
    commands: mpsc::UnboundedSender<Command>,
    credentials: Arc<Credentials>,
    session_state: watch::Receiver<Option<AuthSessionState>>,
    connection_generation: watch::Receiver<u64>,
}

impl ClientHandle {
    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// Live extras decoded from `subscribeServerConfig` snapshots and settings updates.
    pub fn server_config_extras(&self) -> watch::Receiver<ServerConfigExtras> {
        extras_sender(&self.credentials.http_base).subscribe()
    }

    /// Optimistically updates `requireConnectDeviceApproval` after `server.updateSettings` succeeds.
    pub fn set_require_connect_device_approval(&self, require: bool) {
        extras_sender(&self.credentials.http_base).send_modify(|extras| {
            extras.require_connect_device_approval = Some(require);
        });
    }
}

/// In-memory channels for app integration tests; no supervisor consumes their commands.
#[cfg(feature = "test-support")]
pub type FixtureRequest = (&'static str, Value, oneshot::Sender<Result<Value, RpcError>>);
#[cfg(feature = "test-support")]
pub type FixtureStreamRequest = (&'static str, oneshot::Sender<Result<StreamHandle, RpcError>>);
#[cfg(feature = "test-support")]
pub type FixtureRequests = (Vec<FixtureRequest>, Vec<FixtureStreamRequest>);

/// In-memory channels for app integration tests; no supervisor consumes their commands.
#[cfg(feature = "test-support")]
pub struct ClientFixture {
    pub grants: watch::Sender<Option<AuthSessionState>>,
    pub generation: watch::Sender<u64>,
    commands: mpsc::UnboundedReceiver<Command>,
}

#[cfg(feature = "test-support")]
impl ClientFixture {
    pub fn handle(session: AuthSessionState) -> (ClientHandle, Self) {
        Self::handle_with_http_base(session, "http://fixture.invalid".into())
    }

    pub fn handle_with_http_base(session: AuthSessionState, http_base: String) -> (ClientHandle, Self) {
        let (commands, received) = mpsc::unbounded_channel();
        let (grants, session_state) = watch::channel(Some(session));
        let (generation, connection_generation) = watch::channel(1);
        let handle = ClientHandle {
            refs: Default::default(),
            commands,
            credentials: Arc::new(Credentials {
                http_base,
                access_token: String::new(),
            }),
            session_state,
            connection_generation,
        };
        (
            handle,
            Self {
                grants,
                generation,
                commands: received,
            },
        )
    }

    pub async fn next_request(&mut self) -> (&'static str, Value, oneshot::Sender<Result<Value, RpcError>>) {
        loop {
            match self.commands.recv().await.expect("fixture request channel closed") {
                Command::Request { method, payload, reply, .. } => return (method, payload, reply),
                Command::OpenThread(_) | Command::CloseThread(_) | Command::CloseAllThreads | Command::Stream { .. } => {},
                _ => panic!("unexpected fixture command before request"),
            }
        }
    }

    pub fn take_commands(&mut self) -> Vec<&'static str> {
        let mut commands = Vec::new();
        while let Ok(command) = self.commands.try_recv() {
            commands.push(match command {
                Command::OpenThread(_) => "open-thread",
                Command::CloseThread(_) => "close-thread",
                Command::CloseAllThreads => "close-all-threads",
                Command::RefreshSessionState => "refresh-session",
                Command::RetryNow => "retry",
                Command::Request { method, .. } | Command::Stream { method, .. } => method,
            });
        }
        commands
    }

    pub fn take_requests(&mut self) -> FixtureRequests {
        let mut requests = Vec::new();
        let mut streams = Vec::new();
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::Request { method, payload, reply, .. } => requests.push((method, payload, reply)),
                Command::Stream { method, reply, .. } => streams.push((method, reply)),
                Command::OpenThread(_) | Command::CloseThread(_) | Command::CloseAllThreads | Command::RefreshSessionState | Command::RetryNow => {}
            }
        }
        (requests, streams)
    }
}

impl ClientHandle {
    /// The server this handle talks to; caches keyed by it never mix environments.
    pub fn environment(&self) -> &str {
        &self.credentials.http_base
    }

    pub fn open_thread(&self, thread_id: &str) {
        let _ = self.commands.send(Command::OpenThread(thread_id.to_owned()));
    }

    pub fn close_thread(&self, thread_id: &str) {
        let _ = self.commands.send(Command::CloseThread(thread_id.to_owned()));
    }

    pub fn close_all_threads(&self) { let _ = self.commands.send(Command::CloseAllThreads); }

    /// Advances only after a new WebSocket connects; update handoffs must outlive the old one.
    pub fn connection_generation(&self) -> watch::Receiver<u64> { self.connection_generation.clone() }

    /// This device's grants, `None` until the first fetch answers. The last answer stays while
    /// reconnecting; every connection fetches anew.
    pub fn session_state(&self) -> watch::Receiver<Option<AuthSessionState>> {
        self.session_state.clone()
    }

    /// Fetches the grants again on the live connection, unless a fetch is already running.
    pub fn refresh_session_state(&self) {
        let _ = self.commands.send(Command::RefreshSessionState);
    }

    /// Reconnects now instead of waiting out the backoff, or checks a blocked server again.
    pub fn retry_now(&self) {
        let _ = self.commands.send(Command::RetryNow);
    }

    /// Sends an orchestration command. Resolves with the committed sequence or the server's error.
    /// Queued before this returns, in order with `request` and `stream`.
    pub fn dispatch(&self, command: Value) -> impl Future<Output = Result<Value, RpcError>> + Send + 'static {
        self.request(method::DISPATCH_COMMAND, command)
    }

    /// Fetches and decodes one page of earlier history over HTTP. Run it on the network runtime.
    pub async fn history_page(&self, thread_id: &str, cursor: &str) -> Result<HistoryPage, String> {
        crate::history::fetch_history_page(&self.credentials, thread_id, cursor).await
    }

    pub async fn history_page_with(&self, thread_id: &str, query: crate::history::HistoryQuery) -> Result<HistoryPage, String> {
        crate::history::fetch_history_page_with(&self.credentials, thread_id, &query).await
    }

    /// Dispatches commands one after another, each only once the previous one committed, and
    /// stops at the first failure, returning its index. A send's prerequisites (declining
    /// approvals, persisting permissions) must land before its message, as the web awaits them.
    pub async fn dispatch_in_order(&self, commands: Vec<Value>) -> Result<(), (usize, RpcError)> {
        for (index, command) in commands.into_iter().enumerate() {
            self.dispatch(command).await.map_err(|error| (index, error))?;
        }
        Ok(())
    }

    /// A pull request's patch over HTTP (`POST /api/pull-requests/diff`). Run it on the network
    /// runtime.
    pub async fn pull_request_diff(&self, payload: Value) -> Result<Value, crate::pull_requests::HttpFailure> {
        crate::pull_requests::fetch_diff(&self.credentials, payload).await
    }

    /// An authenticated `GET` endpoint with this device's credential (`auth::get_access`). Run it
    /// on the network runtime.
    pub async fn get_access(&self, path: &str) -> Result<Value, String> {
        auth::get_access(&self.credentials, path).await
    }

    /// An access endpoint `POST` with this device's credential (`auth::post_access`). Run it on the
    /// network runtime.
    pub async fn post_access(&self, path: &str, body: Option<Value>) -> Result<Value, String> {
        auth::post_access(&self.credentials, path, body).await
    }

    /// Downloads a signed asset (`assets.createUrl`) over HTTP, at most `max_bytes`. Run it on
    /// the network runtime.
    pub async fn asset_bytes(&self, relative_url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        crate::asset::fetch_asset(&self.credentials.http_base, relative_url, max_bytes).await
    }

    /// Opens the stream of one viewed preview tab on `runtime`. Dropping it closes the socket.
    pub fn preview_stream(&self, runtime: &tokio::runtime::Handle, target: crate::preview_stream::StreamTarget) -> crate::preview_stream::PreviewStream {
        crate::preview_stream::open(runtime, self.credentials.clone(), target)
    }

    /// Opens one device's screen and input through the device hub on `runtime`. Dropping its
    /// input handle stops it.
    pub fn device_stream(&self, runtime: &tokio::runtime::Handle, target: crate::device_stream::DeviceTarget) -> crate::device_stream::DeviceStream {
        crate::device_stream::open(runtime, self.credentials.clone(), target)
    }

    /// One unary RPC on the live session, such as `orchestration.getTurnDiff`. Fails with
    /// `Closed` while disconnected rather than waiting for a reconnect. The request is queued
    /// before this returns, so calls made in order reach the server in order (terminal input
    /// depends on it).
    pub fn request(&self, method: &'static str, payload: Value) -> impl Future<Output = Result<Value, RpcError>> + Send + 'static {
        let (reply, rx) = oneshot::channel();
        let queued = self.commands.send(Command::Request { method, payload, reply, settled: self.refs.on_settled(method) }).is_ok();
        async move {
            if !queued {
                return Err(RpcError::Closed);
            }
            rx.await.unwrap_or(Err(RpcError::Closed))
        }
    }

    /// A streaming RPC on the live session. It ends with `RpcError::Closed` when the connection
    /// drops, and callers resubscribe. Dropping the handle interrupts it on the server.
    pub fn stream(&self, method: &'static str, payload: Value) -> impl Future<Output = Result<StreamHandle, RpcError>> + Send + 'static {
        let (reply, rx) = oneshot::channel();
        let queued = self.commands.send(Command::Stream { method, payload, reply, settled: self.refs.on_settled(method) }).is_ok();
        async move {
            if !queued {
                return Err(RpcError::Closed);
            }
            rx.await.unwrap_or(Err(RpcError::Closed))
        }
    }
}

/// Bounded so a stalled UI pushes back through acks to the server instead of buffering forever.
const EVENT_QUEUE: usize = 1024;
#[cfg(not(test))]
const THREAD_RETRY_BASE: Duration = Duration::from_millis(250);
#[cfg(test)]
const THREAD_RETRY_BASE: Duration = Duration::from_millis(20);
const THREAD_RETRY_MAX: Duration = Duration::from_secs(30);
/// `SESSION_STATE_RETRY_BASE_MS` and `SESSION_STATE_RETRY_MAX_MS`.
const SESSION_STATE_RETRY: Duration = Duration::from_secs(2);
const SESSION_STATE_RETRY_MAX: Duration = Duration::from_secs(60);

type Events = mpsc::Sender<ClientEvent>;

pub fn start(runtime: &tokio::runtime::Handle, credentials: Credentials) -> (ClientHandle, mpsc::Receiver<ClientEvent>) {
    start_prepared(runtime, credentials, None)
}

/// `start`, running `prepare` before every connection attempt.
pub fn start_prepared(runtime: &tokio::runtime::Handle, credentials: Credentials, prepare: Option<Prepare>) -> (ClientHandle, mpsc::Receiver<ClientEvent>) {
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (events, event_rx) = mpsc::channel(EVENT_QUEUE);
    let (grants, session_state) = watch::channel(None);
    let (generations, connection_generation) = watch::channel(0);
    let config_extras_tx = extras_sender(&credentials.http_base);
    let handle = ClientHandle {
        refs: Default::default(),
        commands,
        credentials: Arc::new(credentials.clone()),
        session_state,
        connection_generation,
    };
    runtime.spawn(async move {
        tokio::select! {
            _ = events.closed() => {},
            _ = supervise(credentials, prepare, command_rx, events.clone(), grants, generations, config_extras_tx) => {},
        }
    });
    (handle, event_rx)
}

struct Resume {
    shell: Option<u64>,
    /// The options the shell that reached `shell` was trimmed with.
    shell_options: Option<ShellOptions>,
    threads: HashMap<String, Option<u64>>,
}

async fn supervise(
    credentials: Credentials,
    prepare: Option<Prepare>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: Events,
    grants: watch::Sender<Option<AuthSessionState>>,
    generations: watch::Sender<u64>,
    config_extras: Arc<watch::Sender<ServerConfigExtras>>,
) {
    let mut resume = Resume { shell: None, shell_options: None, threads: HashMap::new() };
    let mut attempt: u32 = 0;
    let mut last_failure = String::new();
    let mut awaiting_approval = false;
    loop {
        // An attempt after a failure still reads as reconnecting, with the reason it failed.
        let _ = events.send(ClientEvent::Status(if attempt == 0 {
            ConnectionStatus::Connecting
        } else if awaiting_approval {
            ConnectionStatus::AwaitingApproval { reason: last_failure.clone(), attempt }
        } else {
            ConnectionStatus::Reconnecting { reason: last_failure.clone(), attempt }
        })).await;
        let prepared = match &prepare {
            Some(prepare) => match prepare_draining(prepare, &mut commands, &mut resume).await {
                PrepareOutcome::Done(prepared) => prepared,
                PrepareOutcome::RetryNow => {
                    attempt = 0;
                    awaiting_approval = false;
                    continue;
                }
                PrepareOutcome::Dropped => return,
            },
            None => Ok(()),
        };
        let outcome = match prepared {
            Ok(()) => match connect_draining(&credentials, &mut commands, &mut resume).await {
                ConnectOutcome::Done(result) => result,
                ConnectOutcome::RetryNow => {
                    attempt = 0;
                    awaiting_approval = false;
                    continue;
                }
                ConnectOutcome::Dropped => return,
            },
            Err(PrepareError::Moved(http_base)) => {
                let _ = events.send(ClientEvent::Moved { http_base }).await;
                return;
            }
            Err(PrepareError::Transient(reason)) => Err(ConnectError::Transient(reason)),
            Err(PrepareError::Blocked(reason)) => Err(ConnectError::Blocked(reason)),
        };
        let (reason, is_approval) = match outcome {
            Ok(session) => {
                generations.send_modify(|generation| *generation += 1);
                attempt = 0;
                let _ = events.send(ClientEvent::Status(ConnectionStatus::Connected)).await;
                let mut fetch = SessionStateFetch::new(&credentials, &grants);
                fetch.start();
                let end = run_session(&session, &mut resume, &mut commands, &events, &mut fetch, &config_extras).await;
                fetch.stop();
                match end {
                    SessionEnd::Closed(reason) => (reason, false),
                    SessionEnd::Dropped => return,
                }
            }
            Err(ConnectError::Unauthorized(reason)) => {
                let _ = events.send(ClientEvent::Status(ConnectionStatus::NeedsPairing { reason })).await;
                return;
            }
            Err(ConnectError::ApprovalPending(reason)) => (reason, true),
            Err(ConnectError::Unsupported { reason, server_outdated }) => {
                let _ = events.send(ClientEvent::Status(ConnectionStatus::Unsupported { reason: reason.clone(), server_outdated })).await;
                match wait_draining(None, &mut commands, &mut resume).await {
                    WaitOutcome::Elapsed | WaitOutcome::RetryNow => {
                        attempt += 1;
                        last_failure = reason;
                        awaiting_approval = false;
                        continue;
                    }
                    WaitOutcome::Dropped => return,
                }
            }
            Err(ConnectError::Blocked(reason)) => {
                let _ = events.send(ClientEvent::Status(ConnectionStatus::Blocked { reason: reason.clone() })).await;
                match wait_draining(None, &mut commands, &mut resume).await {
                    WaitOutcome::Elapsed | WaitOutcome::RetryNow => {
                        attempt += 1;
                        last_failure = reason;
                        awaiting_approval = false;
                        continue;
                    }
                    WaitOutcome::Dropped => return,
                }
            }
            Err(ConnectError::Transient(reason)) => (reason, false),
        };
        attempt += 1;
        last_failure = reason.clone();
        awaiting_approval = is_approval;
        let _ = events.send(ClientEvent::Status(if is_approval {
            ConnectionStatus::AwaitingApproval { reason, attempt }
        } else {
            ConnectionStatus::Reconnecting { reason, attempt }
        })).await;
        let delay = Duration::from_millis((500u64 << attempt.min(5)).min(15_000));
        match wait_draining(Some(delay), &mut commands, &mut resume).await {
            WaitOutcome::Elapsed => {}
            WaitOutcome::RetryNow => {
                // `pingdotgg/t3code#24932`: reset backoff on explicit retry while keeping the
                // reconnecting status reason visible on the next attempt.
                attempt = 1;
            }
            WaitOutcome::Dropped => return,
        }
    }
}

enum ConnectError {
    Unauthorized(String),
    ApprovalPending(String),
    Unsupported { reason: String, server_outdated: bool },
    Blocked(String),
    Transient(String),
}

enum PrepareOutcome {
    Done(Result<(), PrepareError>),
    RetryNow,
    Dropped,
}

enum ConnectOutcome {
    Done(Result<Session, ConnectError>),
    RetryNow,
    Dropped,
}

enum WaitOutcome {
    Elapsed,
    RetryNow,
    Dropped,
}

/// Runs `prepare` while keeping track of threads opened or closed and failing dispatches.
/// A `RetryNow` interrupts a stalled preparation so a dead route never holds reconnect
/// (`pingdotgg/t3code#34623`, `pingdotgg/t3code#24932`).
async fn prepare_draining(prepare: &Prepare, commands: &mut mpsc::UnboundedReceiver<Command>, resume: &mut Resume) -> PrepareOutcome {
    let preparation = prepare();
    tokio::pin!(preparation);
    loop {
        tokio::select! {
            prepared = &mut preparation => return PrepareOutcome::Done(prepared),
            command = commands.recv() => match command {
                None => return PrepareOutcome::Dropped,
                Some(Command::RetryNow) => return PrepareOutcome::RetryNow,
                Some(command) => drain_command(command, resume),
            }
        }
    }
}

/// Runs `connect` while listening for handle drop or `RetryNow` so a stalled ticket or dead route
/// never blocks a user-requested retry (`pingdotgg/t3code#34623`, `pingdotgg/t3code#24932`).
async fn connect_draining(credentials: &Credentials, commands: &mut mpsc::UnboundedReceiver<Command>, resume: &mut Resume) -> ConnectOutcome {
    let attempt = connect(credentials);
    tokio::pin!(attempt);
    loop {
        tokio::select! {
            result = &mut attempt => return ConnectOutcome::Done(result),
            command = commands.recv() => match command {
                None => return ConnectOutcome::Dropped,
                Some(Command::RetryNow) => return ConnectOutcome::RetryNow,
                Some(command) => drain_command(command, resume),
            }
        }
    }
}

/// A command that arrives while there is no session.
fn drain_command(command: Command, resume: &mut Resume) {
    match command {
        Command::OpenThread(id) => {
            resume.threads.entry(id).or_insert(None);
        }
        Command::CloseAllThreads => { resume.threads.clear(); }
        Command::CloseThread(id) => {
            resume.threads.remove(&id);
        }
        Command::RefreshSessionState | Command::RetryNow => {}
        Command::Request { reply, settled, .. } => {
            drop(settled);
            let _ = reply.send(Err(RpcError::Closed));
        }
        Command::Stream { reply, settled, .. } => {
            drop(settled);
            let _ = reply.send(Err(RpcError::Closed));
        }
    }
}

async fn connect(credentials: &Credentials) -> Result<Session, ConnectError> {
    // `orchestrationProtocolCompatibilityError`: a server on another protocol is blocked, not
    // retried. One that can't describe itself is left to the socket to accept or refuse.
    if let Ok(descriptor) = auth::fetch_descriptor(&credentials.http_base).await
        && let Some(mismatch) = protocol_mismatch(&descriptor)
    {
        let server_outdated = mismatch == ProtocolMismatch::ServerOutdated;
        return Err(ConnectError::Unsupported { reason: mismatch.message(&descriptor.label), server_outdated });
    }
    let ticket = auth::issue_rpc_ticket(credentials).await.map_err(|error| match error {
        AuthError::Unauthorized | AuthError::Rejected(_) => ConnectError::Unauthorized(error.to_string()),
        AuthError::ConnectApprovalPending { message, .. } => ConnectError::ApprovalPending(message),
        other => ConnectError::Transient(other.to_string()),
    })?;
    Session::connect(&auth::socket_url(credentials, &ticket)).await.map_err(|error| match error {
        OpenError::Unauthorized => ConnectError::Unauthorized(error.to_string()),
        // Only a server on a newer protocol answers 426 to ours, unless it says otherwise.
        OpenError::Incompatible { server_protocol } => {
            let mismatch = server_protocol.and_then(mismatch_with).unwrap_or(ProtocolMismatch::ClientOutdated);
            ConnectError::Unsupported { reason: mismatch.message(""), server_outdated: mismatch == ProtocolMismatch::ServerOutdated }
        }
        OpenError::Failed(reason) => ConnectError::Transient(reason),
    })
}

/// While waiting to reconnect (until `retry_now` when `delay` is `None`), keep track of threads
/// opened or closed and fail dispatches.
async fn wait_draining(delay: Option<Duration>, commands: &mut mpsc::UnboundedReceiver<Command>, resume: &mut Resume) -> WaitOutcome {
    let sleep = async {
        match delay {
            Some(delay) => tokio::time::sleep(delay).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return WaitOutcome::Elapsed,
            command = commands.recv() => match command {
                None => return WaitOutcome::Dropped,
                Some(Command::RetryNow) => return WaitOutcome::RetryNow,
                Some(command) => drain_command(command, resume),
            }
        }
    }
}

enum SessionEnd {
    Closed(String),
    Dropped,
}

enum ThreadDelivery {
    Item(Box<ThreadStreamItem>),
    DecodeError(String),
    RpcError(RpcError),
}

type ThreadFrame = (String, ThreadDelivery);

fn wall_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Opts into `providerStatusDeltas` (`pingdotgg/t3code#35072`) alongside `commands::subscribe_server_config`.
fn subscribe_server_config_payload() -> Value {
    let mut payload = commands::subscribe_server_config();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("providerStatusDeltas".to_owned(), Value::Bool(true));
    }
    payload
}

/// Tracks the last seen raw provider JSON list per session so `providerStatusDeltas` events
/// (`pingdotgg/t3code#35072`) can reconstruct omitted `models`, `slashCommands`, and `skills`
/// before decoding `ServerConfigStreamEvent::ProviderStatuses`.
#[derive(Default)]
struct ServerConfigPreprocessor {
    held_providers: HashMap<String, Value>,
    clock_recorded: bool,
}

impl ServerConfigPreprocessor {
    /// Inspects raw `subscribeServerConfig` JSON, updates `config_extras`, records host clock on
    /// the first snapshot (`pingdotgg/t3code#36420`), and expands `providerStatusDeltas` into
    /// `providerStatuses` (`pingdotgg/t3code#35072`).
    fn preprocess(
        &mut self,
        mut value: Value,
        config_requested_at_ms: i64,
        config_extras: &watch::Sender<ServerConfigExtras>,
    ) -> Value {
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "snapshot" => {
                if let Some(config) = value.get("config") {
                    if let Some(providers) = config.get("providers").and_then(Value::as_array) {
                        self.record_providers(providers);
                    }
                    let mut clock_sample = config_extras.borrow().clock_sample;
                    if !self.clock_recorded {
                        let received_at_ms = wall_now_ms();
                        let server_time_ms = config
                            .get("serverTime")
                            .and_then(Value::as_str)
                            .and_then(t3_core::sidebar::time::parse_iso);
                        if let Some(env_id) = config
                            .pointer("/environment/environmentId")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                        {
                            t3_core::clock::record_server_clock(env_id, server_time_ms, config_requested_at_ms, received_at_ms);
                        }
                        clock_sample = Some((server_time_ms, config_requested_at_ms, received_at_ms));
                        self.clock_recorded = true;
                    }
                    let mut extras = extract_server_config_extras(config);
                    extras.clock_sample = clock_sample;
                    config_extras.send_replace(extras);
                }
            }
            "settingsUpdated" => {
                if let Some(require) = value.pointer("/payload/settings/requireConnectDeviceApproval").and_then(Value::as_bool) {
                    config_extras.send_modify(|extras| extras.require_connect_device_approval = Some(require));
                }
            }
            "providerStatuses" => {
                if let Some(providers) = value.pointer("/payload/providers").and_then(Value::as_array) {
                    self.record_providers(providers);
                }
            }
            "providerStatusDeltas" => {
                if let Some(deltas) = value.pointer("/payload/providers").and_then(Value::as_array) {
                    let rebuilt = apply_provider_status_deltas(&self.held_providers, deltas);
                    self.record_providers(&rebuilt);
                    if let Some(obj) = value.as_object_mut() {
                        obj.insert("type".to_owned(), Value::String("providerStatuses".to_owned()));
                        obj.insert("payload".to_owned(), serde_json::json!({ "providers": rebuilt }));
                    }
                }
            }
            _ => {}
        }
        value
    }

    fn record_providers(&mut self, providers: &[Value]) {
        self.held_providers.clear();
        for provider in providers {
            if let Some(id) = provider.get("instanceId").and_then(Value::as_str) {
                self.held_providers.insert(id.to_owned(), provider.clone());
            }
        }
    }
}

/// `applyServerProviderDeltas` (`packages/contracts/src/server.ts`, `pingdotgg/t3code#35072`):
/// merges each delta with the previously held `models`, `slashCommands`, and `skills` when omitted.
fn apply_provider_status_deltas(held: &HashMap<String, Value>, deltas: &[Value]) -> Vec<Value> {
    deltas
        .iter()
        .filter_map(|delta| {
            let mut next = delta.as_object()?.clone();
            let id = next.get("instanceId")?.as_str()?;
            let base = held.get(id).and_then(Value::as_object);
            for field in ["models", "slashCommands", "skills"] {
                if !next.contains_key(field) {
                    let inherited = base.and_then(|base| base.get(field)).cloned()?;
                    next.insert(field.to_owned(), inherited);
                }
            }
            Some(Value::Object(next))
        })
        .collect()
}

fn extract_server_config_extras(config: &Value) -> ServerConfigExtras {
    let require_connect_device_approval = config
        .pointer("/settings/requireConnectDeviceApproval")
        .and_then(Value::as_bool);
    let auth_policy = config.pointer("/auth/policy").and_then(Value::as_str).map(str::to_owned);
    let direct_endpoints = config
        .get("directEndpoints")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("url").and_then(Value::as_str).or_else(|| item.as_str()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let thread_snapshot_pagination = config.get("threadSnapshotPagination").and_then(Value::as_bool).unwrap_or(false);
    let last_event_loop_stall = config
        .get("lastEventLoopStall")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    ServerConfigExtras {
        require_connect_device_approval,
        auth_policy,
        direct_endpoints,
        thread_snapshot_pagination,
        last_event_loop_stall,
        clock_sample: None,
    }
}

async fn run_session(
    session: &Session,
    resume: &mut Resume,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &Events,
    fetch: &mut SessionStateFetch<'_>,
    config_extras: &watch::Sender<ServerConfigExtras>,
) -> SessionEnd {
    let config_requested_at_ms = wall_now_ms();
    let mut preprocessor = ServerConfigPreprocessor::default();
    let mut server_config = session.stream(method::SUBSCRIBE_SERVER_CONFIG, subscribe_server_config_payload()).ok();
    let options = initial_shell_options(&mut server_config, events, &mut preprocessor, config_requested_at_ms, config_extras).await;
    // Deltas never restore rows trimmed under other options (nor trim them), so the shell starts
    // over from an authoritative snapshot, as `shell.ts` does for every new session.
    if resume.shell_options.replace(options) != Some(options) {
        resume.shell = None;
    }
    let Ok(mut shell) = session.stream(method::SUBSCRIBE_SHELL, commands::subscribe_shell(resume.shell, options)) else {
        return SessionEnd::Closed("Could not subscribe to the shell.".into());
    };
    let (thread_tx, mut thread_rx) = mpsc::channel::<ThreadFrame>(256);
    let mut forwarders: HashMap<String, ThreadForwarder> = HashMap::new();
    for (thread_id, sequence) in resume.threads.clone() {
        forwarders.insert(thread_id.clone(), forward_thread(session, &thread_id, sequence, thread_tx.clone()));
    }
    let end = loop {
        tokio::select! {
            value = shell.receiver.recv() => match value {
                Some(Ok(value)) => {
                    // `snapshot-required`: server cannot replay from `resume.shell`; clear cursor
                    // and resubscribe for a fresh snapshot on the same live socket.
                    if value.get("kind").and_then(Value::as_str) == Some("snapshot-required") {
                        resume.shell = None;
                        match session.stream(method::SUBSCRIBE_SHELL, commands::subscribe_shell(None, options)) {
                            Ok(fresh) => {
                                shell = fresh;
                                continue;
                            }
                            Err(error) => break SessionEnd::Closed(format!("Shell reload failed: {error}")),
                        }
                    }
                    match serde_json::from_value::<ShellStreamItem>(value) {
                        Ok(item) => {
                            resume.shell = shell_sequence(&item).or(resume.shell);
                            let _ = events.send(ClientEvent::Shell(item)).await;
                        }
                        Err(error) => report(events, "shell", error.to_string()).await,
                    }
                }
                Some(Err(error)) => break SessionEnd::Closed(format!("Shell subscription failed: {error}")),
                None => break SessionEnd::Closed("Shell subscription ended.".into()),
            },
            value = next_config(&mut server_config) => match value {
                Some(Ok(value)) => {
                    let value = preprocessor.preprocess(value, config_requested_at_ms, config_extras);
                    match serde_json::from_value::<ServerConfigStreamEvent>(value) {
                        Ok(event) => { let _ = events.send(ClientEvent::ServerConfig(Box::new(event.prepare()))).await; }
                        Err(error) => report(events, "server config", error.to_string()).await,
                    }
                }
                // The rest of the session works without it; the next session subscribes again.
                Some(Err(error)) => {
                    server_config = None;
                    report(events, "server config", error.to_string()).await;
                }
                None => server_config = None,
            },
            frame = thread_rx.recv() => {
                let Some((thread_id, delivery)) = frame else { continue };
                if !resume.threads.contains_key(&thread_id) { continue }
                match delivery {
                    ThreadDelivery::Item(item) => {
                        if let Some(sequence) = thread_sequence(&item) {
                            resume.threads.insert(thread_id.clone(), Some(sequence));
                        }
                        let _ = events.send(ClientEvent::Thread { thread_id, item }).await;
                    }
                    ThreadDelivery::DecodeError(message) => {
                        report(events, &format!("thread {thread_id}"), message).await;
                    }
                    ThreadDelivery::RpcError(RpcError::Closed) => {}
                    ThreadDelivery::RpcError(error) => report(events, &format!("thread {thread_id}"), error.to_string()).await,
                }
            }
            command = commands.recv() => match command {
                None => break SessionEnd::Dropped,
                Some(Command::OpenThread(thread_id)) => {
                    if !forwarders.contains_key(&thread_id) {
                        let sequence = *resume.threads.entry(thread_id.clone()).or_insert(None);
                        forwarders.insert(thread_id.clone(), forward_thread(session, &thread_id, sequence, thread_tx.clone()));
                    }
                }
                Some(Command::CloseAllThreads) => {
                    resume.threads.clear();
                    for (_, task) in forwarders.drain() { task.abort(); }
                }
                Some(Command::CloseThread(thread_id)) => {
                    resume.threads.remove(&thread_id);
                    if let Some(task) = forwarders.remove(&thread_id) { task.abort(); }
                }
                Some(Command::RefreshSessionState) => fetch.start(),
                Some(Command::RetryNow) => {}
                Some(Command::Request { method, payload, mut reply, settled }) => {
                    if !reply.is_closed() {
                        let response = session.send_request(method, payload);
                        tokio::spawn(async move {
                            tokio::select! {
                                _ = reply.closed() => {},
                                result = response => { drop(settled); let _ = reply.send(result); }
                            }
                        });
                    }
                }
                Some(Command::Stream { method, payload, reply, settled }) => {
                    let stream = session.stream(method, payload).map(|mut stream| {
                        stream.settled = settled;
                        stream
                    });
                    let _ = reply.send(stream);
                }
            },
            reason = session.closed() => break SessionEnd::Closed(reason),
        }
    };
    for (_, task) in forwarders {
        task.abort();
    }
    end
}

/// How long the shell waits for the config that decides its options before asking for every row.
const CONFIG_WAIT: Duration = Duration::from_secs(10);

/// `shell.ts` builds the shell input from `session.initialConfig`: the first config snapshot,
/// forwarded like any other. Without one the shell asks for every row, as a config failure does.
async fn initial_shell_options(
    stream: &mut Option<StreamHandle>,
    events: &Events,
    preprocessor: &mut ServerConfigPreprocessor,
    config_requested_at_ms: i64,
    config_extras: &watch::Sender<ServerConfigExtras>,
) -> ShellOptions {
    let wait = async {
        loop {
            match next_config(stream).await {
                Some(Ok(value)) => {
                    let value = preprocessor.preprocess(value, config_requested_at_ms, config_extras);
                    match serde_json::from_value::<ServerConfigStreamEvent>(value) {
                        Ok(event) => {
                            let options = match &event {
                                ServerConfigStreamEvent::Snapshot { config } => {
                                    Some(ShellOptions::for_server(config.shell_thread_families, config.shell_older_settled))
                                }
                                _ => None,
                            };
                            let _ = events.send(ClientEvent::ServerConfig(Box::new(event.prepare()))).await;
                            if let Some(options) = options {
                                return options;
                            }
                        }
                        Err(error) => report(events, "server config", error.to_string()).await,
                    }
                }
                Some(Err(error)) => {
                    report(events, "server config", error.to_string()).await;
                    return ShellOptions::default();
                }
                None => return ShellOptions::default(),
            }
        }
    };
    tokio::time::timeout(CONFIG_WAIT, wait).await.unwrap_or_default()
}

#[derive(Clone, Copy)]
enum FetchGate {
    Idle,
    Running { dirty: bool },
}

/// The grants fetch of one connection. Failures retry with backoff while the connection lasts,
/// so a fetch that failed mid-reconnect cannot outlive the socket that recovered.
struct SessionStateFetch<'a> {
    credentials: &'a Credentials,
    grants: &'a watch::Sender<Option<AuthSessionState>>,
    task: Option<JoinHandle<()>>,
    gate: Arc<Mutex<FetchGate>>,
}

impl Drop for SessionStateFetch<'_> {
    fn drop(&mut self) { self.stop(); }
}

impl<'a> SessionStateFetch<'a> {
    fn new(credentials: &'a Credentials, grants: &'a watch::Sender<Option<AuthSessionState>>) -> Self {
        Self { credentials, grants, task: None, gate: Arc::new(Mutex::new(FetchGate::Idle)) }
    }

    fn start(&mut self) {
        {
            let mut gate = self.gate.lock().expect("fetch gate");
            if let FetchGate::Running { dirty } = &mut *gate {
                *dirty = true;
                return;
            }
            *gate = FetchGate::Running { dirty: false };
        }
        let (credentials, grants, gate) = (self.credentials.clone(), self.grants.clone(), self.gate.clone());
        self.task = Some(tokio::spawn(async move {
            let mut delay = SESSION_STATE_RETRY;
            loop {
                match auth::fetch_session_state(&credentials).await {
                    Ok(state) => {
                        grants.send_if_modified(|current| {
                            let changed = current.as_ref() != Some(&state);
                            *current = Some(state);
                            changed
                        });
                        let mut guard = gate.lock().expect("fetch gate");
                        if matches!(*guard, FetchGate::Running { dirty: true }) {
                            *guard = FetchGate::Running { dirty: false };
                            delay = SESSION_STATE_RETRY;
                            continue;
                        }
                        *guard = FetchGate::Idle;
                        return;
                    }
                    Err(error) => tracing::warn!(%error, "session state fetch failed"),
                }
                let dirty = {
                    let mut guard = gate.lock().expect("fetch gate");
                    if matches!(*guard, FetchGate::Running { dirty: true }) {
                        *guard = FetchGate::Running { dirty: false };
                        true
                    } else {
                        false
                    }
                };
                if dirty {
                    delay = SESSION_STATE_RETRY;
                    continue;
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(SESSION_STATE_RETRY_MAX);
            }
        }));
    }

    fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.gate = Arc::new(Mutex::new(FetchGate::Idle));
    }
}

async fn next_config(stream: &mut Option<StreamHandle>) -> Option<Result<Value, RpcError>> {
    match stream {
        Some(stream) => stream.receiver.recv().await,
        None => std::future::pending().await,
    }
}

struct ThreadForwarder(JoinHandle<()>);
impl ThreadForwarder { fn abort(&self) { self.0.abort(); } }
impl Drop for ThreadForwarder { fn drop(&mut self) { self.abort(); } }

fn next_thread_retry(base: Duration, attempt: u32) -> Duration {
    let factor = 1u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX);
    base.saturating_mul(factor).min(THREAD_RETRY_MAX)
}

fn is_thread_auth_failure(error: &RpcError) -> bool {
    matches!(
        error,
        RpcError::Failure(failure) if failure.tag.as_deref() == Some("EnvironmentAuthorizationError")
    )
}

/// Keeps one thread subscribed for the life of the session. A stream that ends or fails on a
/// healthy connection resubscribes with exponential backoff from the last delivered sequence,
/// stopping on `EnvironmentAuthorizationError` or socket close (`client.ts` / `threads.ts`).
fn forward_thread(session: &Session, thread_id: &str, sequence: Option<u64>, tx: mpsc::Sender<ThreadFrame>) -> ThreadForwarder {
    let session = session.clone();
    let thread_id = thread_id.to_owned();
    ThreadForwarder(tokio::spawn(async move {
        let mut sequence = sequence;
        let mut retry_attempt = 0u32;
        loop {
            let mut stream = match session.stream(method::SUBSCRIBE_THREAD, commands::subscribe_thread(&thread_id, sequence)) {
                Ok(stream) => stream,
                Err(RpcError::Closed) => return,
                Err(error) => {
                    let stop = is_thread_auth_failure(&error);
                    if tx.send((thread_id.clone(), ThreadDelivery::RpcError(error))).await.is_err() || stop {
                        return;
                    }
                    tokio::time::sleep(next_thread_retry(THREAD_RETRY_BASE, retry_attempt)).await;
                    retry_attempt = retry_attempt.saturating_add(1);
                    continue;
                }
            };
            while let Some(value) = stream.receiver.recv().await {
                match value {
                    Ok(raw) => match decode_thread_stream_item(raw) {
                        Ok(item) => {
                            retry_attempt = 0;
                            sequence = thread_sequence(&item).or(sequence);
                            if tx.send((thread_id.clone(), ThreadDelivery::Item(Box::new(item)))).await.is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            if tx.send((thread_id.clone(), ThreadDelivery::DecodeError(error.to_string()))).await.is_err() {
                                return;
                            }
                            break;
                        }
                    },
                    Err(RpcError::Closed) => return,
                    Err(error) => {
                        let stop = is_thread_auth_failure(&error);
                        if tx.send((thread_id.clone(), ThreadDelivery::RpcError(error))).await.is_err() || stop {
                            return;
                        }
                        break;
                    }
                }
            }
            tokio::time::sleep(next_thread_retry(THREAD_RETRY_BASE, retry_attempt)).await;
            retry_attempt = retry_attempt.saturating_add(1);
        }
    }))
}

fn shell_sequence(item: &ShellStreamItem) -> Option<u64> {
    match item {
        ShellStreamItem::Snapshot { snapshot, .. } => Some(snapshot.snapshot_sequence),
        ShellStreamItem::ProjectUpdated { sequence, .. }
        | ShellStreamItem::ProjectRemoved { sequence, .. }
        | ShellStreamItem::ThreadUpdated { sequence, .. }
        | ShellStreamItem::ThreadRemoved { sequence, .. } => Some(*sequence),
        ShellStreamItem::Synchronized | ShellStreamItem::Unknown => None,
    }
}

fn thread_sequence(item: &ThreadStreamItem) -> Option<u64> {
    match item {
        ThreadStreamItem::Snapshot { snapshot_sequence, .. } => Some(*snapshot_sequence),
        ThreadStreamItem::Event { sequence, .. } => Some(*sequence),
        ThreadStreamItem::Synchronized | ThreadStreamItem::Unknown => None,
    }
}

async fn report(events: &Events, context: &str, message: String) {
    tracing::warn!(context, %message, "stream failed");
    let _ = events.send(ClientEvent::StreamError { context: context.to_owned(), message }).await;
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod parity_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_status_deltas_reconstruct_omitted_lists_and_extract_extras() {
        let (extras_tx, extras_rx) = watch::channel(ServerConfigExtras::default());
        let mut pre = ServerConfigPreprocessor::default();
        let snapshot = json!({
            "type": "snapshot",
            "config": {
                "environment": { "environmentId": "env-delta", "label": "Studio", "serverVersion": "0.1.9" },
                "auth": { "policy": "loopback-browser" },
                "directEndpoints": [{ "url": "https://studio.ts.net:3773" }],
                "threadSnapshotPagination": true,
                "lastEventLoopStall": { "durationMs": 6200, "loadAverage": 12.0, "cpuCount": 8 },
                "settings": { "requireConnectDeviceApproval": true },
                "providers": [{
                    "instanceId": "codex",
                    "driver": "codex",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "models": [{ "slug": "gpt-5", "name": "GPT-5" }],
                    "slashCommands": [{ "name": "help" }],
                    "skills": []
                }]
            }
        });
        let processed = pre.preprocess(snapshot, 1000, &extras_tx);
        let parsed: ServerConfigStreamEvent = serde_json::from_value(processed).unwrap();
        assert!(matches!(parsed, ServerConfigStreamEvent::Snapshot { .. }));
        let extras = extras_rx.borrow().clone();
        assert_eq!(extras.require_connect_device_approval, Some(true));
        assert_eq!(extras.auth_policy.as_deref(), Some("loopback-browser"));
        assert_eq!(extras.direct_endpoints, vec!["https://studio.ts.net:3773"]);
        assert!(extras.thread_snapshot_pagination);
        assert!(extras.last_event_loop_stall.is_some());
        assert!(extras.clock_sample.is_some());

        // Delta omits models, slashCommands, skills -> reconstructed from prior snapshot!
        let delta = json!({
            "type": "providerStatusDeltas",
            "payload": {
                "providers": [{
                    "instanceId": "codex",
                    "driver": "codex",
                    "enabled": true,
                    "installed": true,
                    "status": "warning"
                }]
            }
        });
        let rewritten = pre.preprocess(delta, 1000, &extras_tx);
        assert_eq!(rewritten["type"], "providerStatuses");
        let event: ServerConfigStreamEvent = serde_json::from_value(rewritten).unwrap();
        match event {
            ServerConfigStreamEvent::ProviderStatuses { payload } => {
                assert_eq!(payload.providers.len(), 1);
                assert_eq!(payload.providers[0].status, "warning");
                assert_eq!(payload.providers[0].models.len(), 1);
                assert_eq!(payload.providers[0].models[0].slug, "gpt-5");
                assert_eq!(payload.providers[0].slash_commands.len(), 1);
            }
            other => panic!("expected ProviderStatuses, got {other:?}"),
        }

        // settingsUpdated toggles requireConnectDeviceApproval live.
        let updated = json!({
            "type": "settingsUpdated",
            "payload": { "settings": { "requireConnectDeviceApproval": false } }
        });
        pre.preprocess(updated, 1000, &extras_tx);
        assert_eq!(extras_rx.borrow().require_connect_device_approval, Some(false));
    }

    #[tokio::test]
    async fn retry_now_interrupts_stalled_preparation_immediately() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut resume = Resume { shell: None, shell_options: None, threads: HashMap::new() };
        let prepare: Prepare = Arc::new(|| Box::pin(std::future::pending()));
        tx.send(Command::RetryNow).unwrap();
        let outcome = tokio::time::timeout(Duration::from_millis(200), prepare_draining(&prepare, &mut rx, &mut resume))
            .await
            .expect("retry_now must interrupt stalled prepare immediately");
        assert!(matches!(outcome, PrepareOutcome::RetryNow));
    }
}

