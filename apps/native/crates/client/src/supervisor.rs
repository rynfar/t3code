//! Keeps one environment connected: mints a ticket, opens a session, subscribes the shell and
//! every open thread, and on disconnect reconnects with backoff, resuming each stream from the
//! last sequence it saw. Each connection also fetches the device's grants
//! (`client-runtime/src/state/session.ts`). Frames are decoded here, off the UI thread.
//! A route that must be readied first, as an SSH tunnel is, passes a `Prepare` step that runs
//! before every attempt, as the web's connection resolver authorizes each one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

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
use crate::routes::{
    BETTER_ROUTE_CHECK_INTERVAL, BETTER_ROUTE_COOLDOWN, CandidateRoute, ROUTE_CHECK_HEAD_START,
    ROUTE_CHECK_TIMEOUT, ReportedEndpoint, RouteAttemptError, RouteCheck, RouteConnectResult,
    SILENT_ROUTE_BACKOFF_MAX, connect_over_routes, merge_learned_routes, merge_refused_routes,
    normalize_http_base,
};
use crate::session::{OpenError, RpcError, Session, StreamHandle};
use crate::wakeups::{
    self, ConnectionWakeup, probe_timeout_for, reaches_connection_supervisor, resets_retry_backoff,
    should_resubscribe_after_wakeup,
};

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
/// `ServerConfig` schema: Connect device approval toggle (`pingdotgg/t3code#36419`), auth policy & direct
/// endpoints (`pingdotgg/t3code#35543` Tailscale Serve), socket thread pagination (`pingdotgg/t3code#33306`), and recent
/// host event-loop stall (`hostFreeze.ts`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerConfigExtras {
    pub require_connect_device_approval: Option<bool>,
    pub auth_policy: Option<String>,
    pub direct_endpoints: Vec<String>,
    pub direct_endpoint_entries: Vec<ReportedEndpoint>,
    pub thread_snapshot_pagination: bool,
    pub secret_references: bool,
    pub last_event_loop_stall: Option<crate::status::ServerEventLoopStall>,
    /// Accurate `subscribeServerConfig`-scoped clock sample `(server_time_ms, requested_at_ms, received_at_ms)` (`pingdotgg/t3code#36420`).
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
    OpenThread { thread_id: String, sequence: Option<u64> },
    CloseThread(String),
    CloseAllThreads,
    RefreshSessionState,
    RetryNow,
    Wakeup(ConnectionWakeup),
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

/// Test helper: publishes synthetic `ServerConfigExtras` for `http_base`.
#[cfg(feature = "test-support")]
pub fn set_server_config_extras_for(http_base: &str, extras: ServerConfigExtras) {
    extras_sender(http_base).send_replace(extras);
}

type RefusedRoutesSender = Arc<watch::Sender<Vec<String>>>;

/// Saved route IDs whose host refused this device's sign-in while another route connected
/// (`refusedRouteIds` in `supervisor.ts`, `pingdotgg/t3code#33199`), keyed by environment `http_base`.
fn refused_routes_sender(http_base: &str) -> RefusedRoutesSender {
    static REFUSED: OnceLock<RwLock<HashMap<String, RefusedRoutesSender>>> = OnceLock::new();
    let map = REFUSED.get_or_init(|| RwLock::new(HashMap::new()));
    let key = normalize_http_base(http_base);
    if let Ok(read) = map.read()
        && let Some(tx) = read.get(&key)
    {
        return tx.clone();
    }
    let mut write = map.write().unwrap_or_else(|p| p.into_inner());
    write
        .entry(key)
        .or_insert_with(|| Arc::new(watch::channel(Vec::new()).0))
        .clone()
}

/// Live route IDs whose host refused this device's sign-in (`pingdotgg/t3code#33199`).
pub fn refused_routes_for(http_base: &str) -> watch::Receiver<Vec<String>> {
    refused_routes_sender(http_base).subscribe()
}

/// Test helper: publishes synthetic refused route IDs for `http_base`.
/// Test/runtime helper mirroring [`set_server_config_extras_for`].
pub fn set_refused_routes_for(http_base: &str, route_ids: Vec<String>) {
    refused_routes_sender(http_base).send_replace(route_ids);
}

#[derive(Clone)]
pub struct ClientHandle {
    pub(crate) refs: crate::vcs::RefState,
    commands: mpsc::UnboundedSender<Command>,
    credentials: Arc<Credentials>,
    active_credentials: watch::Receiver<Option<Credentials>>,
    session_state: watch::Receiver<Option<AuthSessionState>>,
    connection_generation: watch::Receiver<u64>,
}

impl ClientHandle {
    pub fn credentials(&self) -> Credentials {
        self.active_credentials.borrow().clone().unwrap_or_else(|| (*self.credentials).clone())
    }

    /// The saved primary address, regardless of which route the supervisor dialed.
    pub fn primary_http_base(&self) -> &str {
        &self.credentials.http_base
    }

    /// Live extras decoded from `subscribeServerConfig` snapshots and settings updates.
    pub fn server_config_extras(&self) -> watch::Receiver<ServerConfigExtras> {
        extras_sender(&self.credentials.http_base).subscribe()
    }

    /// Live route IDs whose host refused this device's sign-in (`pingdotgg/t3code#33199`).
    pub fn refused_route_ids(&self) -> watch::Receiver<Vec<String>> {
        refused_routes_sender(&self.credentials.http_base).subscribe()
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
        let (_, active_credentials) = watch::channel(None);
        let handle = ClientHandle {
            refs: Default::default(),
            commands,
            credentials: Arc::new(Credentials {
                http_base,
                access_token: String::new(),
            }),
            active_credentials,
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
                Command::OpenThread { .. } | Command::CloseThread(_) | Command::CloseAllThreads | Command::Stream { .. } => {},
                _ => panic!("unexpected fixture command before request"),
            }
        }
    }

    pub async fn next_stream(&mut self) -> (&'static str, oneshot::Sender<Result<StreamHandle, RpcError>>) {
        loop {
            match self.commands.recv().await.expect("fixture stream channel closed") {
                Command::Stream { method, reply, .. } => return (method, reply),
                Command::Request { reply, .. } => {
                    let _ = reply.send(Ok(serde_json::json!({})));
                }
                Command::OpenThread { .. }
                | Command::CloseThread(_)
                | Command::CloseAllThreads
                | Command::RefreshSessionState
                | Command::RetryNow
                | Command::Wakeup(_) => {}
            }
        }
    }

    pub fn take_commands(&mut self) -> Vec<&'static str> {
        let mut commands = Vec::new();
        while let Ok(command) = self.commands.try_recv() {
            commands.push(match command {
                Command::OpenThread { .. } => "open-thread",
                Command::CloseThread(_) => "close-thread",
                Command::CloseAllThreads => "close-all-threads",
                Command::RefreshSessionState => "refresh-session",
                Command::RetryNow => "retry",
                Command::Wakeup(_) => "wakeup",
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
                Command::OpenThread { .. } | Command::CloseThread(_) | Command::CloseAllThreads | Command::RefreshSessionState | Command::RetryNow | Command::Wakeup(_) => {}
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
        self.open_thread_from(thread_id, None);
    }

    pub fn open_thread_from(&self, thread_id: &str, sequence: Option<u64>) {
        let _ = self.commands.send(Command::OpenThread {
            thread_id: thread_id.to_owned(),
            sequence,
        });
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

    /// Delivers an OS/application wakeup directly to this supervisor (`wakeups.ts`).
    pub fn notify_wakeup(&self, reason: ConnectionWakeup) {
        let _ = self.commands.send(Command::Wakeup(reason));
    }

    /// Sends an orchestration command. Resolves with the committed sequence or the server's error.
    /// Queued before this returns, in order with `request` and `stream`.
    pub fn dispatch(&self, command: Value) -> impl Future<Output = Result<Value, RpcError>> + Send + 'static {
        self.request(method::DISPATCH_COMMAND, command)
    }

    /// Fetches and decodes one page of earlier history over HTTP. Run it on the network runtime.
    pub async fn history_page(&self, thread_id: &str, cursor: &str) -> Result<HistoryPage, String> {
        crate::history::fetch_history_page(&self.credentials(), thread_id, cursor).await
    }

    pub async fn history_page_with(&self, thread_id: &str, query: crate::history::HistoryQuery) -> Result<HistoryPage, String> {
        crate::history::fetch_history_page_with(&self.credentials(), thread_id, &query).await
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
        crate::pull_requests::fetch_diff(&self.credentials(), payload).await
    }

    /// An authenticated `GET` endpoint with this device's credential (`auth::get_access`). Run it
    /// on the network runtime.
    pub async fn get_access(&self, path: &str) -> Result<Value, String> {
        auth::get_access(&self.credentials(), path).await
    }

    /// An access endpoint `POST` with this device's credential (`auth::post_access`). Run it on the
    /// network runtime.
    pub async fn post_access(&self, path: &str, body: Option<Value>) -> Result<Value, String> {
        auth::post_access(&self.credentials(), path, body).await
    }

    /// Downloads a signed asset (`assets.createUrl`) over HTTP, at most `max_bytes`. Run it on
    /// the network runtime.
    pub async fn asset_bytes(&self, relative_url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        crate::asset::fetch_asset(&self.credentials().http_base, relative_url, max_bytes).await
    }

    /// Opens the stream of one viewed preview tab on `runtime`. Dropping it closes the socket.
    pub fn preview_stream(&self, runtime: &tokio::runtime::Handle, target: crate::preview_stream::StreamTarget) -> crate::preview_stream::PreviewStream {
        crate::preview_stream::open(runtime, Arc::new(self.credentials()), target)
    }

    pub fn preview_stream_read_only(&self, runtime: &tokio::runtime::Handle, target: crate::preview_stream::StreamTarget) -> crate::preview_stream::PreviewStream {
        crate::preview_stream::open_read_only(runtime, Arc::new(self.credentials()), target)
    }

    /// Opens one device's screen and input through the device hub on `runtime`. Dropping its
    /// input handle stops it.
    pub fn device_stream(&self, runtime: &tokio::runtime::Handle, target: crate::device_stream::DeviceTarget) -> crate::device_stream::DeviceStream {
        crate::device_stream::open(runtime, Arc::new(self.credentials()), target)
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
    start_with_routes(runtime, credentials, Vec::new(), None, prepare, None)
}

/// `start_prepared` with alternate candidate routes and expected `environmentId` for multi-route
/// candidate fallback (`packages/client-runtime/src/connection/{routes,resolver,driver}.ts`).
pub fn start_with_routes(
    runtime: &tokio::runtime::Handle,
    credentials: Credentials,
    alternate_routes: Vec<CandidateRoute>,
    expected_environment_id: Option<String>,
    prepare: Option<Prepare>,
    relay: Option<Arc<crate::connect::Cloud>>,
) -> (ClientHandle, mpsc::Receiver<ClientEvent>) {
    wakeups::ensure_clock_jump_watcher(runtime);
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (events, event_rx) = mpsc::channel(EVENT_QUEUE);
    let (grants, session_state) = watch::channel(None);
    let (generations, connection_generation) = watch::channel(0);
    let (active_credentials_tx, active_credentials) = watch::channel(None);
    let config_extras_tx = extras_sender(&credentials.http_base);
    let refused_routes_tx = refused_routes_sender(&credentials.http_base);
    let handle = ClientHandle {
        refs: Default::default(),
        commands,
        credentials: Arc::new(credentials.clone()),
        active_credentials,
        session_state,
        connection_generation,
    };
    runtime.spawn(async move {
        tokio::select! {
            _ = events.closed() => {},
            _ = supervise(
                credentials,
                alternate_routes,
                expected_environment_id,
                prepare,
                relay,
                command_rx,
                events.clone(),
                grants,
                generations,
                active_credentials_tx,
                config_extras_tx,
                refused_routes_tx,
            ) => {},
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

async fn next_wakeup(rx: &mut tokio::sync::broadcast::Receiver<ConnectionWakeup>) -> ConnectionWakeup {
    loop {
        match rx.recv().await {
            Ok(reason) if reaches_connection_supervisor(reason) => return reason,
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => std::future::pending().await,
        }
    }
}

fn initial_candidate_routes(
    credentials: &Credentials,
    alternate_routes: Vec<CandidateRoute>,
    ssh: bool,
) -> Vec<CandidateRoute> {
    if credentials.access_token.is_empty()
        && alternate_routes.iter().any(|route| route.relay_access_token().is_some())
    {
        return alternate_routes.into_iter().filter(|route| route.relay_access_token().is_some()).collect();
    }
    let primary = CandidateRoute::primary(&credentials.http_base, ssh);
    let mut routes = vec![primary.clone()];
    for candidate in alternate_routes {
        if candidate.relay_access_token().is_some() || (normalize_http_base(&candidate.http_base) != primary.http_base
            && !routes.iter().any(|existing| normalize_http_base(&existing.http_base) == normalize_http_base(&candidate.http_base))
        ) {
            routes.push(candidate);
        }
    }
    routes
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    credentials: Credentials,
    alternate_routes: Vec<CandidateRoute>,
    mut expected_environment_id: Option<String>,
    prepare: Option<Prepare>,
    relay: Option<Arc<crate::connect::Cloud>>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: Events,
    grants: watch::Sender<Option<AuthSessionState>>,
    generations: watch::Sender<u64>,
    active_credentials_tx: watch::Sender<Option<Credentials>>,
    config_extras: Arc<watch::Sender<ServerConfigExtras>>,
    refused_routes_tx: Arc<watch::Sender<Vec<String>>>,
) {
    let mut routes = initial_candidate_routes(&credentials, alternate_routes, prepare.is_some());
    let mut refused_routes: Vec<String> = Vec::new();
    let mut preferred_route_id: Option<String> = None;
    let mut last_route_switch: Option<Instant> = None;
    let mut wakeup_rx = wakeups::subscribe();
    let mut resume = Resume { shell: None, shell_options: None, threads: HashMap::new() };
    let mut attempt: u32 = 0;
    let mut last_failure = String::new();
    let mut awaiting_approval = false;
    let route_progress = crate::status::RouteProgressPublisher::new(&credentials.http_base);
    loop {
        route_progress.publish(None, None);
        // An attempt after a failure still reads as reconnecting, with the reason it failed.
        let _ = events.send(ClientEvent::Status(if attempt == 0 {
            ConnectionStatus::Connecting
        } else if awaiting_approval {
            ConnectionStatus::AwaitingApproval { reason: last_failure.clone(), attempt }
        } else {
            ConnectionStatus::Reconnecting { reason: last_failure.clone(), attempt }
        })).await;
        let prepared = match &prepare {
            Some(prepare) => match prepare_draining(prepare, &mut commands, &mut wakeup_rx, &mut resume).await {
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
        let mut ordered_routes = prioritize_preferred_route(&routes, preferred_route_id.as_deref());
        preferred_route_id = None;
        let prepared = prepared.or_else(|error| {
            if matches!(error, PrepareError::Transient(_) | PrepareError::Blocked(_))
                && ordered_routes.iter().any(|route| route.relay_access_token().is_some())
            {
                ordered_routes.retain(|route| route.relay_access_token().is_some());
                Ok(())
            } else {
                Err(error)
            }
        });
        let outcome = match prepared {
            Ok(()) => match connect_draining(&credentials, &ordered_routes, expected_environment_id.as_deref(), relay.as_deref(), &mut commands, &mut wakeup_rx, &mut resume).await {
                ConnectOutcome::Done(result) => *result,
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
            Ok(RouteConnectResult { route: active_route, value: (session, active_credentials), refused_route_ids }) => {
                refused_routes = merge_refused_routes(&refused_routes, &refused_route_ids, |id| {
                    id != active_route.id && routes.iter().any(|route| route.id == id)
                });
                refused_routes_tx.send_replace(refused_routes.clone());
                active_credentials_tx.send_replace(Some(active_credentials.clone()));
                generations.send_modify(|generation| *generation += 1);
                attempt = 0;
                route_progress.publish(Some(&active_route.id), None);
                let _ = events.send(ClientEvent::Status(ConnectionStatus::Connected)).await;
                let mut fetch = SessionStateFetch::new(&active_credentials, &grants);
                fetch.start();
                let end = run_session(
                    &session,
                    &active_credentials,
                    &active_route,
                    &credentials.access_token,
                    prepare.as_ref(),
                    &mut routes,
                    &mut refused_routes,
                    &refused_routes_tx,
                    &mut expected_environment_id,
                    last_route_switch,
                    &mut resume,
                    &mut commands,
                    &mut wakeup_rx,
                    &events,
                    &mut fetch,
                    &config_extras,
                )
                .await;
                fetch.stop();
                active_credentials_tx.send_replace(None);
                match end {
                    SessionEnd::Closed(reason) => (reason, false),
                    SessionEnd::ReconnectImmediately => {
                        attempt = 0;
                        awaiting_approval = false;
                        continue;
                    }
                    SessionEnd::SwitchRoute(route_id) => {
                        preferred_route_id = Some(route_id);
                        last_route_switch = Some(Instant::now());
                        attempt = 0;
                        awaiting_approval = false;
                        continue;
                    }
                    SessionEnd::Dropped => return,
                }
            }
            Err(ConnectError::Unauthorized { reason, .. }) => {
                refused_routes_tx.send_replace(Vec::new());
                let _ = events.send(ClientEvent::Status(ConnectionStatus::NeedsPairing { reason })).await;
                return;
            }
            Err(ConnectError::ApprovalPending(reason)) => (reason, true),
            Err(ConnectError::Unsupported { reason, server_outdated }) => {
                refused_routes_tx.send_replace(Vec::new());
                let _ = events.send(ClientEvent::Status(ConnectionStatus::Unsupported { reason: reason.clone(), server_outdated })).await;
                match wait_draining(None, &mut commands, &mut wakeup_rx, &mut resume).await {
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
                refused_routes_tx.send_replace(Vec::new());
                let _ = events.send(ClientEvent::Status(ConnectionStatus::Blocked { reason: reason.clone() })).await;
                match wait_draining(None, &mut commands, &mut wakeup_rx, &mut resume).await {
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
        refused_routes_tx.send_replace(Vec::new());
        attempt += 1;
        last_failure = reason.clone();
        awaiting_approval = is_approval;
        let delay = Duration::from_millis((500u64 << attempt.min(5)).min(15_000));
        route_progress.publish(None, Some(delay));
        let _ = events.send(ClientEvent::Status(if is_approval {
            ConnectionStatus::AwaitingApproval { reason, attempt }
        } else {
            ConnectionStatus::Reconnecting { reason, attempt }
        })).await;
        match wait_draining(Some(delay), &mut commands, &mut wakeup_rx, &mut resume).await {
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
    Unauthorized { reason: String, clock_skew: bool },
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
    Done(Box<Result<RouteConnectResult<(Session, Credentials)>, ConnectError>>),
    RetryNow,
    Dropped,
}

enum WaitOutcome {
    Elapsed,
    RetryNow,
    Dropped,
}

fn prioritize_preferred_route(routes: &[CandidateRoute], preferred_route_id: Option<&str>) -> Vec<CandidateRoute> {
    let Some(preferred_id) = preferred_route_id else {
        return routes.to_vec();
    };
    let mut ordered = Vec::with_capacity(routes.len());
    if let Some(preferred) = routes.iter().find(|route| route.id == preferred_id) {
        ordered.push(preferred.clone());
    }
    for route in routes {
        if route.id != preferred_id {
            ordered.push(route.clone());
        }
    }
    ordered
}

/// Runs `prepare` while keeping track of threads opened or closed and failing dispatches.
/// A `RetryNow` or backoff-resetting OS wake interrupts a stalled preparation so a dead route
/// never holds reconnect (`pingdotgg/t3code#34623`, `pingdotgg/t3code#24932`, `wakeups.ts`).
async fn prepare_draining(
    prepare: &Prepare,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    wakeup_rx: &mut tokio::sync::broadcast::Receiver<ConnectionWakeup>,
    resume: &mut Resume,
) -> PrepareOutcome {
    let preparation = prepare();
    tokio::pin!(preparation);
    loop {
        tokio::select! {
            prepared = &mut preparation => return PrepareOutcome::Done(prepared),
            reason = next_wakeup(wakeup_rx) => {
                if resets_retry_backoff(reason) {
                    return PrepareOutcome::RetryNow;
                }
            }
            command = commands.recv() => match command {
                None => return PrepareOutcome::Dropped,
                Some(Command::RetryNow) => return PrepareOutcome::RetryNow,
                Some(Command::Wakeup(reason)) if resets_retry_backoff(reason) => return PrepareOutcome::RetryNow,
                Some(command) => drain_command(command, resume),
            }
        }
    }
}

/// Runs multi-route `connect` while listening for handle drop, `RetryNow`, or backoff-resetting
/// OS wake so a stalled ticket or dead route never blocks recovery (`pingdotgg/t3code#34623`,
/// `pingdotgg/t3code#24932`, `wakeups.ts`).
async fn connect_draining(
    credentials: &Credentials,
    routes: &[CandidateRoute],
    expected_environment_id: Option<&str>,
    relay: Option<&crate::connect::Cloud>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    wakeup_rx: &mut tokio::sync::broadcast::Receiver<ConnectionWakeup>,
    resume: &mut Resume,
) -> ConnectOutcome {
    let attempt = connect_routes(credentials, routes, expected_environment_id, relay);
    tokio::pin!(attempt);
    loop {
        tokio::select! {
            result = &mut attempt => return ConnectOutcome::Done(Box::new(result)),
            reason = next_wakeup(wakeup_rx) => {
                if resets_retry_backoff(reason) {
                    return ConnectOutcome::RetryNow;
                }
            }
            command = commands.recv() => match command {
                None => return ConnectOutcome::Dropped,
                Some(Command::RetryNow) => return ConnectOutcome::RetryNow,
                Some(Command::Wakeup(reason)) if resets_retry_backoff(reason) => return ConnectOutcome::RetryNow,
                Some(command) => drain_command(command, resume),
            }
        }
    }
}

/// A command that arrives while there is no session.
fn drain_command(command: Command, resume: &mut Resume) {
    match command {
        Command::OpenThread { thread_id, sequence } => {
            resume.threads.entry(thread_id).or_insert(sequence);
        }
        Command::CloseAllThreads => { resume.threads.clear(); }
        Command::CloseThread(id) => {
            resume.threads.remove(&id);
        }
        Command::RefreshSessionState | Command::RetryNow | Command::Wakeup(_) => {}
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

async fn connect_routes(
    credentials: &Credentials,
    routes: &[CandidateRoute],
    expected_environment_id: Option<&str>,
    relay: Option<&crate::connect::Cloud>,
) -> Result<RouteConnectResult<(Session, Credentials)>, ConnectError> {
    let expected = expected_environment_id.map(str::to_owned);
    let token = credentials.access_token.clone();
    connect_over_routes(
        routes,
        ROUTE_CHECK_HEAD_START,
        move |route| {
            let expected = expected.clone();
            async move {
                if route.relay_access_token().is_some() {
                    return RouteCheck::Answered;
                }
                match auth::fetch_descriptor_timeout(&route.http_base, ROUTE_CHECK_TIMEOUT).await {
                    Ok(descriptor) => {
                        if expected.as_deref().is_none_or(|wanted| descriptor.environment_id == wanted) {
                            RouteCheck::Answered
                        } else {
                            RouteCheck::Silent
                        }
                    }
                    Err(_) => RouteCheck::Silent,
                }
            }
        },
        move |route| {
            let token = token.clone();
            async move {
                let (route_creds, relay_socket) = if let Some(seed) = route.relay_access_token() {
                    let (relay, environment_id) = relay.zip(expected_environment_id).ok_or_else(|| {
                        RouteAttemptError::Transient(ConnectError::Transient("Missing relay configuration.".into()))
                    })?;
                    let response = relay
                        .connect_environment(environment_id)
                        .await
                        .map_err(|error| RouteAttemptError::Transient(ConnectError::Transient(error)))?;
                    let socket = url::Url::parse(&response.endpoint.ws_base_url)
                        .ok()
                        .filter(|url| matches!(url.scheme(), "ws" | "wss"))
                        .ok_or_else(|| RouteAttemptError::Transient(ConnectError::Transient("Invalid relay WebSocket URL.".into())))?;
                    let expires_at = response.expires_at_epoch_ms()
                        .ok_or_else(|| RouteAttemptError::Transient(ConnectError::Transient("Invalid relay credential expiry.".into())))?;
                    // A direct pairing can use the same origin as the tunnel and retain its own DPoP entry.
                    crate::dpop::put_direct_credential(&route.id, crate::dpop::DpopConnectionCredential {
                        access_token: response.credential,
                        expires_at_epoch_ms: expires_at,
                        seed_token: Some(seed.to_owned()),
                        previous_access_token: None,
                        environment_id: Some(response.environment_id),
                    });
                    (Credentials {
                        http_base: response.endpoint.http_base_url,
                        access_token: seed.to_owned(),
                    }, Some(socket))
                } else {
                    (Credentials { http_base: route.http_base.clone(), access_token: token }, None)
                };
                match connect(&route_creds, expected_environment_id, relay_socket).await {
                    Ok(session) => Ok((session, route_creds)),
                    Err(err @ ConnectError::Unsupported { .. }) => Err(RouteAttemptError::Unsupported(err)),
                    Err(ConnectError::Unauthorized { reason, .. }) if route.relay_access_token().is_some() => {
                        Err(RouteAttemptError::Transient(ConnectError::Transient(reason)))
                    }
                    Err(err @ ConnectError::Unauthorized { clock_skew: false, .. }) => {
                        Err(RouteAttemptError::Refused(err))
                    }
                    Err(err @ (ConnectError::Unauthorized { clock_skew: true, .. } | ConnectError::Blocked(_))) => {
                        Err(RouteAttemptError::Blocked(err))
                    }
                    Err(err @ (ConnectError::Transient(_) | ConnectError::ApprovalPending(_))) => {
                        Err(RouteAttemptError::Transient(err))
                    }
                }
            }
        },
    )
    .await
    .map_err(|err| err.unwrap_or_else(|| ConnectError::Transient("Environment did not answer on any saved route.".to_owned())))
}

async fn connect(
    credentials: &Credentials,
    expected_environment_id: Option<&str>,
    relay_socket: Option<url::Url>,
) -> Result<Session, ConnectError> {
    // `orchestrationProtocolCompatibilityError`: a server on another protocol is blocked, not
    // retried. One that can't describe itself is left to the socket to accept or refuse.
    if let Ok(descriptor) = auth::fetch_descriptor(&credentials.http_base).await {
        if let Some(expected) = expected_environment_id
            && !descriptor.environment_id.is_empty()
            && descriptor.environment_id != expected
        {
            return Err(ConnectError::Transient(format!(
                "Another environment ({}) answered at {}.",
                descriptor.label, credentials.http_base
            )));
        }
        if let Some(mismatch) = protocol_mismatch(&descriptor) {
            let server_outdated = mismatch == ProtocolMismatch::ServerOutdated;
            return Err(ConnectError::Unsupported { reason: mismatch.message(&descriptor.label), server_outdated });
        }
    }
    let ticket = auth::issue_rpc_ticket(credentials).await.map_err(|error| match error {
        AuthError::Unauthorized | AuthError::Rejected(_) => ConnectError::Unauthorized {
            reason: error.to_string(),
            clock_skew: false,
        },
        AuthError::AuthRefused { clock_skew, .. } => ConnectError::Unauthorized {
            reason: error.to_string(),
            clock_skew,
        },
        AuthError::ConnectApprovalPending { message, .. } => ConnectError::ApprovalPending(message),
        other => ConnectError::Transient(other.to_string()),
    })?;
    let socket = match relay_socket {
        Some(url) => auth::relay_socket_url(credentials, &ticket, url),
        None => auth::socket_url(credentials, &ticket),
    };
    Session::connect(&socket).await.map_err(|error| match error {
        OpenError::Unauthorized => ConnectError::Unauthorized {
            reason: error.to_string(),
            clock_skew: false,
        },
        // Only a server on a newer protocol answers 426 to ours, unless it says otherwise.
        OpenError::Incompatible { server_protocol } => {
            let mismatch = server_protocol.and_then(mismatch_with).unwrap_or(ProtocolMismatch::ClientOutdated);
            ConnectError::Unsupported { reason: mismatch.message(""), server_outdated: mismatch == ProtocolMismatch::ServerOutdated }
        }
        OpenError::Failed(reason) => ConnectError::Transient(reason),
    })
}

/// While waiting to reconnect (until `retry_now` or a backoff-resetting wakeup when `delay` is `None`),
/// keep track of threads opened or closed and fail dispatches.
async fn wait_draining(
    delay: Option<Duration>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    wakeup_rx: &mut tokio::sync::broadcast::Receiver<ConnectionWakeup>,
    resume: &mut Resume,
) -> WaitOutcome {
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
            reason = next_wakeup(wakeup_rx) => {
                if resets_retry_backoff(reason) {
                    return WaitOutcome::RetryNow;
                }
            }
            command = commands.recv() => match command {
                None => return WaitOutcome::Dropped,
                Some(Command::RetryNow) => return WaitOutcome::RetryNow,
                Some(Command::Wakeup(reason)) => {
                    if reaches_connection_supervisor(reason) && resets_retry_backoff(reason) {
                        return WaitOutcome::RetryNow;
                    }
                }
                Some(command) => drain_command(command, resume),
            }
        }
    }
}

enum SessionEnd {
    Closed(String),
    ReconnectImmediately,
    SwitchRoute(String),
    Dropped,
}

enum WakeProbeResult {
    Alive(ConnectionWakeup),
    ReplaceImmediately,
    Failed(String),
}

struct RunningWakeProbe {
    reason: ConnectionWakeup,
    task: JoinHandle<()>,
    rx: oneshot::Receiver<WakeProbeResult>,
}

impl RunningWakeProbe {
    fn abort(self) {
        self.task.abort();
    }
}

/// Runs a wakeup liveness check off the `run_session` dispatch loop so live RPCs and streams are
/// never stalled while probing a half-open socket (`supervisor.ts:788-899`).
fn spawn_wake_probe(
    session: Session,
    http_base: String,
    reason: ConnectionWakeup,
    nearby: bool,
) -> Option<RunningWakeProbe> {
    let timeout = probe_timeout_for(reason, nearby)?;
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let tolerates_miss = reason == ConnectionWakeup::ApplicationActiveProbe;
        let long_resume = reason == ConnectionWakeup::ApplicationActiveLong;
        let outcome = if tolerates_miss {
            // Foreground probe (`supervisor.ts:789-792, 857-863`): a busy host can miss one probe
            // while still healthy, so retry once before declaring the session dead.
            if session.probe(timeout).await || session.probe(timeout).await {
                WakeProbeResult::Alive(reason)
            } else {
                WakeProbeResult::Failed("Connection did not respond to a connection health check.".into())
            }
        } else {
            // Non-foreground wake (`ApplicationActive`, `ApplicationActiveLong`, `NetworkChanged`):
            // race the WebSocket probe against an HTTP host check (`awaitSilentSocket`). If HTTP
            // answers while the socket stays silent, replace the socket immediately.
            let ws_probe = session.probe(timeout);
            let http_check = async {
                auth::fetch_descriptor_timeout(&http_base, timeout).await.is_ok()
            };
            tokio::pin!(ws_probe);
            tokio::pin!(http_check);
            let mut http_done = false;
            loop {
                tokio::select! {
                    alive = &mut ws_probe => {
                        break if alive {
                            WakeProbeResult::Alive(reason)
                        } else if long_resume {
                            WakeProbeResult::ReplaceImmediately
                        } else {
                            WakeProbeResult::Failed("Connection did not respond after system wake.".into())
                        };
                    }
                    reachable = &mut http_check, if !http_done => {
                        http_done = true;
                        if reachable {
                            // Host answered HTTP; give the WebSocket a brief grace window to see a
                            // frame before treating it as a silent socket to replace immediately.
                            let grace = Duration::from_millis(250).min(timeout);
                            if session.probe(grace).await {
                                break WakeProbeResult::Alive(reason);
                            }
                            break WakeProbeResult::ReplaceImmediately;
                        }
                    }
                }
            }
        };
        let _ = tx.send(outcome);
    });
    Some(RunningWakeProbe { reason, task, rx })
}

async fn next_wake_probe(probe: &mut Option<RunningWakeProbe>) -> WakeProbeResult {
    match probe.as_mut() {
        Some(running) => match (&mut running.rx).await {
            Ok(result) => {
                *probe = None;
                result
            }
            Err(_) => {
                *probe = None;
                std::future::pending().await
            }
        },
        None => std::future::pending().await,
    }
}

struct BetterRouteProbeResult {
    ready: Option<String>,
    refused: Vec<String>,
}

struct RunningBetterRouteProbe {
    task: JoinHandle<()>,
    rx: oneshot::Receiver<BetterRouteProbeResult>,
}

impl RunningBetterRouteProbe {
    fn abort(self) {
        self.task.abort();
    }
}

fn better_ranked_routes(routes: &[CandidateRoute], active_route: &CandidateRoute) -> Vec<CandidateRoute> {
    let active_rank = active_route.kind().rank();
    let mut better: Vec<CandidateRoute> = routes
        .iter()
        .filter(|candidate| candidate.id != active_route.id && candidate.kind().rank() < active_rank)
        .cloned()
        .collect();
    better.sort_by_key(|candidate| candidate.kind().rank());
    better
}

/// Preflights better-ranked routes: checks the descriptor and tests authorization so a route whose
/// host refuses this device's sign-in is surfaced in `refusedRouteIds` (`pingdotgg/t3code#33199`).
fn spawn_better_route_probe(
    candidates: Vec<CandidateRoute>,
    expected_environment_id: Option<String>,
    access_token: String,
    prepare: Option<Prepare>,
) -> RunningBetterRouteProbe {
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut refused = Vec::new();
        for candidate in candidates {
            if candidate.ssh && let Some(prepare) = &prepare && prepare().await.is_err() {
                continue;
            }
            if let Ok(descriptor) = auth::fetch_descriptor_timeout(&candidate.http_base, ROUTE_CHECK_TIMEOUT).await
                && expected_environment_id
                    .as_deref()
                    .is_none_or(|wanted| descriptor.environment_id.is_empty() || descriptor.environment_id == wanted)
            {
                let route_creds = Credentials {
                    http_base: candidate.http_base.clone(),
                    access_token: access_token.clone(),
                };
                match auth::issue_rpc_ticket(&route_creds).await {
                    Ok(_) => {
                        let _ = tx.send(BetterRouteProbeResult {
                            ready: Some(candidate.id),
                            refused,
                        });
                        return;
                    }
                    Err(error) if error.is_host_credential_refusal() => {
                        refused.push(candidate.id);
                    }
                    Err(_) => {}
                }
            }
        }
        let _ = tx.send(BetterRouteProbeResult { ready: None, refused });
    });
    RunningBetterRouteProbe { task, rx }
}

async fn next_better_route_probe(probe: &mut Option<RunningBetterRouteProbe>) -> BetterRouteProbeResult {
    match probe.as_mut() {
        Some(running) => match (&mut running.rx).await {
            Ok(result) => {
                *probe = None;
                result
            }
            Err(_) => {
                *probe = None;
                std::future::pending().await
            }
        },
        None => std::future::pending().await,
    }
}

/// Keeps a direct DPoP session's access token fresh while its socket is open
/// (`renewDpopBeforeExpiry` in `packages/client-runtime/src/authorization/service.ts`,
/// `pingdotgg/t3code#37990`).
fn spawn_direct_dpop_renewal(credentials: Credentials) -> Option<JoinHandle<()>> {
    let (_, initial) = crate::dpop::find_direct_credential(&credentials.http_base, &credentials.access_token)?;
    Some(tokio::spawn(async move {
        let mut expires_at = initial.expires_at_epoch_ms;
        loop {
            let now = crate::dpop::now_epoch_ms();
            let floor_ms = if now < expires_at {
                5_000
            } else {
                crate::dpop::DPOP_ACCESS_TOKEN_REFRESH_SKEW_MS
            };
            let wait_ms = expires_at
                .saturating_sub(crate::dpop::DPOP_ACCESS_TOKEN_REFRESH_SKEW_MS)
                .saturating_sub(now)
                .max(floor_ms);
            tokio::time::sleep(Duration::from_millis(wait_ms as u64)).await;
            match auth::renew_direct_dpop_token(&credentials, None).await {
                Ok(renewed) => {
                    expires_at = expires_at.max(renewed.expires_at_epoch_ms);
                }
                Err(error) if error.is_host_credential_refusal() => {
                    tracing::warn!(http_base = %credentials.http_base, %error, "direct DPoP renewal refused; stopping background renewal");
                    break;
                }
                Err(error) => {
                    tracing::debug!(http_base = %credentials.http_base, %error, "direct DPoP renewal transient failure; will retry");
                }
            }
        }
    }))
}

fn next_better_route_deadline(
    active_route: &CandidateRoute,
    routes: &[CandidateRoute],
    last_route_switch: Option<Instant>,
    delay: Duration,
) -> Option<tokio::time::Instant> {
    if better_ranked_routes(routes, active_route).is_empty() {
        return None;
    }
    let cooldown_remaining = last_route_switch
        .map(|switched| BETTER_ROUTE_COOLDOWN.saturating_sub(switched.elapsed()))
        .unwrap_or(Duration::ZERO);
    Some(tokio::time::Instant::now() + delay.max(cooldown_remaining))
}

async fn sleep_until_opt(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
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
                if next.get(field).is_none_or(Value::is_null) {
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
    let direct_endpoint_entries: Vec<ReportedEndpoint> = config
        .get("directEndpoints")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let url = item
                        .get("httpBaseUrl")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("url").and_then(Value::as_str))
                        .or_else(|| item.as_str())?;
                    let kind = item.get("kind").and_then(Value::as_str).map(str::to_owned);
                    Some(ReportedEndpoint { http_base_url: url.to_owned(), kind })
                })
                .collect()
        })
        .unwrap_or_default();
    let direct_endpoints = direct_endpoint_entries
        .iter()
        .map(|entry| entry.http_base_url.clone())
        .collect();
    let thread_snapshot_pagination = config.get("threadSnapshotPagination").and_then(Value::as_bool).unwrap_or(false);
    let secret_references = config.get("secretReferences").and_then(Value::as_bool).unwrap_or(false);
    let last_event_loop_stall = config
        .get("lastEventLoopStall")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    ServerConfigExtras {
        require_connect_device_approval,
        auth_policy,
        direct_endpoints,
        direct_endpoint_entries,
        thread_snapshot_pagination,
        secret_references,
        last_event_loop_stall,
        clock_sample: None,
    }
}

fn absorb_learned_routes(
    active_route: &CandidateRoute,
    routes: &mut Vec<CandidateRoute>,
    expected_environment_id: &mut Option<String>,
    config_extras: &watch::Sender<ServerConfigExtras>,
    event: &ServerConfigStreamEvent,
) {
    if let ServerConfigStreamEvent::Snapshot { config } = event {
        let env_id = config.environment.environment_id.trim();
        if !env_id.is_empty() {
            *expected_environment_id = Some(env_id.to_owned());
        }
        let reported = config_extras.borrow().direct_endpoint_entries.clone();
        if active_route.relay_access_token().is_none() && !reported.is_empty() {
            let env_key = expected_environment_id.as_deref().unwrap_or("env");
            if let Some(updated) = merge_learned_routes(env_key, routes, &active_route.id, &reported, true) {
                *routes = updated;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    session: &Session,
    active_credentials: &Credentials,
    active_route: &CandidateRoute,
    primary_access_token: &str,
    prepare: Option<&Prepare>,
    routes: &mut Vec<CandidateRoute>,
    refused_routes: &mut Vec<String>,
    refused_routes_tx: &watch::Sender<Vec<String>>,
    expected_environment_id: &mut Option<String>,
    last_route_switch: Option<Instant>,
    resume: &mut Resume,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    wakeup_rx: &mut tokio::sync::broadcast::Receiver<ConnectionWakeup>,
    events: &Events,
    fetch: &mut SessionStateFetch<'_>,
    config_extras: &watch::Sender<ServerConfigExtras>,
) -> SessionEnd {
    let dpop_renewal_task = spawn_direct_dpop_renewal(active_credentials.clone());
    let config_requested_at_ms = wall_now_ms();
    let mut preprocessor = ServerConfigPreprocessor::default();
    let mut environment_label: Option<String> = None;
    let mut server_config = session.stream(method::SUBSCRIBE_SERVER_CONFIG, subscribe_server_config_payload()).ok();
    let options = initial_shell_options(
        &mut server_config,
        active_route,
        routes,
        expected_environment_id,
        &mut environment_label,
        events,
        &mut preprocessor,
        config_requested_at_ms,
        config_extras,
    )
    .await;
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
    let nearby = active_route.kind().is_nearby();
    let mut wake_probe: Option<RunningWakeProbe> = None;
    let mut better_route_backoff = BETTER_ROUTE_CHECK_INTERVAL;
    let mut better_route_probe: Option<RunningBetterRouteProbe> = None;
    let mut better_route_deadline =
        next_better_route_deadline(active_route, routes, last_route_switch, better_route_backoff);

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
                        Ok(event) => {
                            if let ServerConfigStreamEvent::Snapshot { config } = &event {
                                let label = config.environment.label.trim();
                                if !label.is_empty() {
                                    environment_label = Some(label.to_owned());
                                }
                            }
                            absorb_learned_routes(active_route, routes, expected_environment_id, config_extras, &event);
                            if better_route_probe.is_none() && better_route_deadline.is_none() {
                                better_route_deadline = next_better_route_deadline(
                                    active_route,
                                    routes,
                                    last_route_switch,
                                    better_route_backoff,
                                );
                            }
                            let _ = events.send(ClientEvent::ServerConfig(Box::new(event.prepare()))).await;
                        }
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
            probe_result = next_wake_probe(&mut wake_probe) => match probe_result {
                WakeProbeResult::Alive(reason) => {
                    if should_resubscribe_after_wakeup(reason) {
                        fetch.start();
                        match session.stream(method::SUBSCRIBE_SHELL, commands::subscribe_shell(resume.shell, options)) {
                            Ok(fresh) => shell = fresh,
                            Err(error) => break SessionEnd::Closed(format!("Post-wake shell resubscribe failed: {error}")),
                        }
                        if server_config.is_none() {
                            server_config = session
                                .stream(method::SUBSCRIBE_SERVER_CONFIG, subscribe_server_config_payload())
                                .ok();
                        }
                    }
                }
                WakeProbeResult::ReplaceImmediately => break SessionEnd::ReconnectImmediately,
                WakeProbeResult::Failed(reason) => break SessionEnd::Closed(reason),
            },
            _ = sleep_until_opt(better_route_deadline), if better_route_probe.is_none() => {
                better_route_deadline = None;
                let better = better_ranked_routes(routes, active_route);
                if !better.is_empty() {
                    better_route_probe = Some(spawn_better_route_probe(
                        better,
                        expected_environment_id.clone(),
                        primary_access_token.to_owned(),
                        prepare.cloned(),
                    ));
                }
            }
            better_result = next_better_route_probe(&mut better_route_probe) => {
                if !better_result.refused.is_empty() {
                    let merged = merge_refused_routes(refused_routes, &better_result.refused, |_| true);
                    if merged != *refused_routes {
                        *refused_routes = merged;
                        refused_routes_tx.send_replace(refused_routes.clone());
                    }
                }
                if let Some(better_id) = better_result.ready {
                    break SessionEnd::SwitchRoute(better_id);
                }
                better_route_backoff = (better_route_backoff * 2).min(SILENT_ROUTE_BACKOFF_MAX);
                better_route_deadline = next_better_route_deadline(
                    active_route,
                    routes,
                    last_route_switch,
                    better_route_backoff,
                );
            }
            reason = next_wakeup(wakeup_rx) => {
                let supersede = wake_probe
                    .as_ref()
                    .is_none_or(|running| running.reason == ConnectionWakeup::ApplicationActiveProbe && reason != ConnectionWakeup::ApplicationActiveProbe);
                if supersede
                    && let Some(next_probe) = spawn_wake_probe(session.clone(), active_route.http_base.clone(), reason, nearby)
                    && let Some(prev) = wake_probe.replace(next_probe)
                {
                    prev.abort();
                }
                if better_route_probe.is_none() && !better_ranked_routes(routes, active_route).is_empty() {
                    better_route_backoff = BETTER_ROUTE_CHECK_INTERVAL;
                    better_route_deadline = next_better_route_deadline(
                        active_route,
                        routes,
                        last_route_switch,
                        Duration::ZERO,
                    );
                }
            }
            command = commands.recv() => match command {
                None => break SessionEnd::Dropped,
                Some(Command::OpenThread { thread_id, sequence }) => {
                    if !forwarders.contains_key(&thread_id) {
                        let sequence = *resume.threads.entry(thread_id.clone()).or_insert(sequence);
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
                Some(Command::Wakeup(reason)) => {
                    if reaches_connection_supervisor(reason) {
                        let supersede = wake_probe
                            .as_ref()
                            .is_none_or(|running| running.reason == ConnectionWakeup::ApplicationActiveProbe && reason != ConnectionWakeup::ApplicationActiveProbe);
                        if supersede
                            && let Some(next_probe) = spawn_wake_probe(session.clone(), active_route.http_base.clone(), reason, nearby)
                            && let Some(prev) = wake_probe.replace(next_probe)
                        {
                            prev.abort();
                        }
                        if better_route_probe.is_none() && !better_ranked_routes(routes, active_route).is_empty() {
                            better_route_backoff = BETTER_ROUTE_CHECK_INTERVAL;
                            better_route_deadline = next_better_route_deadline(
                                active_route,
                                routes,
                                last_route_switch,
                                Duration::ZERO,
                            );
                        }
                    }
                }
                Some(Command::Request { method, payload, mut reply, settled }) => {
                    if !reply.is_closed() {
                        let response = session.send_request_labeled(method, payload, environment_label.as_deref());
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
    if let Some(task) = dpop_renewal_task {
        task.abort();
    }
    if let Some(probe) = wake_probe {
        probe.abort();
    }
    if let Some(probe) = better_route_probe {
        probe.abort();
    }
    for (_, task) in forwarders {
        task.abort();
    }
    end
}

/// How long the shell waits for the config that decides its options before asking for every row.
const CONFIG_WAIT: Duration = Duration::from_secs(10);

/// `shell.ts` builds the shell input from `session.initialConfig`: the first config snapshot,
/// forwarded like any other. Without one the shell asks for every row, as a config failure does.
#[allow(clippy::too_many_arguments)]
async fn initial_shell_options(
    stream: &mut Option<StreamHandle>,
    active_route: &CandidateRoute,
    routes: &mut Vec<CandidateRoute>,
    expected_environment_id: &mut Option<String>,
    environment_label: &mut Option<String>,
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
                            if let ServerConfigStreamEvent::Snapshot { config } = &event {
                                let label = config.environment.label.trim();
                                if !label.is_empty() {
                                    *environment_label = Some(label.to_owned());
                                }
                            }
                            absorb_learned_routes(active_route, routes, expected_environment_id, config_extras, &event);
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
        let mut wakeup_rx = wakeups::subscribe();
        let mut resume = Resume { shell: None, shell_options: None, threads: HashMap::new() };
        let prepare: Prepare = Arc::new(|| Box::pin(std::future::pending()));
        tx.send(Command::RetryNow).unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_millis(200),
            prepare_draining(&prepare, &mut rx, &mut wakeup_rx, &mut resume),
        )
        .await
        .expect("retry_now must interrupt stalled prepare immediately");
        assert!(matches!(outcome, PrepareOutcome::RetryNow));
    }

    #[tokio::test]
    async fn supervisor_negotiates_secret_references_falls_back_across_routes_and_wakes() {
        let mut live = crate::test_server::FakeServer::start().await;
        live.set_descriptor(Some(json!({
            "environmentId": "env-multi",
            "label": "Studio",
            "serverVersion": "0.1.9",
            "orchestrationProtocolVersion": 2
        })));
        let dead_creds = Credentials { http_base: "http://127.0.0.1:1".into(), access_token: "token".into() };
        let fallback = CandidateRoute::saved(live.credentials().http_base);
        let (client, mut events) = start_with_routes(
            &tokio::runtime::Handle::current(),
            dead_creds,
            vec![fallback],
            Some("env-multi".into()),
            None,
            None,
        );
        let mut conn = tokio::time::timeout(Duration::from_secs(10), live.connection())
            .await
            .expect("fallback route connects");
        assert!(
            conn.path.contains("clientCapabilities=settingsSecretReferences"),
            "socket URL must advertise settingsSecretReferences: {}",
            conn.path
        );
        let config_req = conn.request(method::SUBSCRIBE_SERVER_CONFIG).await;
        conn.chunk(
            &config_req,
            json!([{
                "type": "snapshot",
                "config": {
                    "environment": { "environmentId": "env-multi", "label": "Studio", "serverVersion": "0.1.9" },
                    "secretReferences": true
                }
            }]),
        )
        .await;
        let _shell = conn.request(method::SUBSCRIBE_SHELL).await;
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("event")
                .expect("open");
            if matches!(event, ClientEvent::ServerConfig(_)) {
                break;
            }
        }
        assert!(client.server_config_extras().borrow().secret_references);
        assert_eq!(client.credentials().http_base, live.credentials().http_base);
        assert_eq!(client.primary_http_base(), "http://127.0.0.1:1");

        // Sending a long-resume wakeup while the server ignores Ping closes the stale session and reconnects.
        client.notify_wakeup(ConnectionWakeup::ApplicationActiveLong);
        let mut second = tokio::time::timeout(Duration::from_secs(10), live.connection())
            .await
            .expect("wakeup probe failure triggers reconnect");
        assert!(second.path.contains("clientCapabilities=settingsSecretReferences"));
        let _ = second.request(method::SUBSCRIBE_SERVER_CONFIG).await;
    }

    #[tokio::test]
    async fn wake_probe_never_blocks_live_rpcs_resubscribes_after_wake_and_probes_better_routes() {
        let mut server = crate::test_server::FakeServer::start().await;
        server.set_descriptor(Some(json!({
            "environmentId": "env-wake",
            "label": "Studio",
            "serverVersion": "0.1.9",
            "orchestrationProtocolVersion": 2
        })));
        let (client, mut events) = start(&tokio::runtime::Handle::current(), server.credentials());
        let mut conn = tokio::time::timeout(Duration::from_secs(5), server.connection())
            .await
            .expect("connects");
        let config_req = conn.request(method::SUBSCRIBE_SERVER_CONFIG).await;
        conn.chunk(
            &config_req,
            json!([{
                "type": "snapshot",
                "config": { "environment": { "environmentId": "env-wake", "label": "Studio", "serverVersion": "0.1.9" } }
            }]),
        )
        .await;
        let _initial_shell = conn.request(method::SUBSCRIBE_SHELL).await;
        while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
            if matches!(ev, ClientEvent::ServerConfig(_)) {
                break;
            }
        }

        // 1. Trigger a foreground probe (`ApplicationActiveProbe` = 3s nearby) and immediately
        // dispatch an RPC. Because the probe runs off the `select!` loop, the RPC reaches the
        // server right away instead of blocking for 3s, and its reply marks the socket alive.
        client.notify_wakeup(ConnectionWakeup::ApplicationActiveProbe);
        let rpc_client = client.clone();
        let rpc_task = tokio::spawn(async move { rpc_client.request("server.ping", json!({})).await });
        let req = tokio::time::timeout(Duration::from_millis(800), conn.request("server.ping"))
            .await
            .expect("RPC must not be stalled by in-flight wake probe");
        conn.send(json!({
            "_tag": "Exit",
            "requestId": req["id"],
            "exit": { "_tag": "Success", "value": { "ok": true } }
        }))
        .await;
        let reply = tokio::time::timeout(Duration::from_secs(2), rpc_task)
            .await
            .expect("rpc joins")
            .unwrap()
            .expect("rpc succeeds");
        assert_eq!(reply["ok"], true);

        // 2. An `ApplicationActive` wakeup that observes live traffic triggers post-wake
        // `SUBSCRIBE_SHELL` resubscribe (`should_resubscribe_after_wakeup`).
        client.notify_wakeup(ConnectionWakeup::ApplicationActive);
        let rpc_client = client.clone();
        let rpc_task = tokio::spawn(async move { rpc_client.request("server.ping", json!({})).await });
        let req = tokio::time::timeout(Duration::from_millis(800), conn.request("server.ping"))
            .await
            .expect("RPC during ApplicationActive probe");
        conn.send(json!({
            "_tag": "Exit",
            "requestId": req["id"],
            "exit": { "_tag": "Success", "value": { "ok": true } }
        }))
        .await;
        let _ = rpc_task.await.unwrap().expect("rpc succeeds");
        let _resubscribed_shell = tokio::time::timeout(Duration::from_secs(2), conn.request(method::SUBSCRIBE_SHELL))
            .await
            .expect("surviving ApplicationActive wake must resubscribe shell");

        // 3. `prepare_draining` interrupts on `NetworkChanged` / `ApplicationActive` (not just Long).
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut wakeup_rx = wakeups::subscribe();
        let mut resume = Resume { shell: None, shell_options: None, threads: HashMap::new() };
        let stalled: Prepare = Arc::new(|| Box::pin(std::future::pending()));
        tx.send(Command::Wakeup(ConnectionWakeup::NetworkChanged)).unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            prepare_draining(&stalled, &mut rx, &mut wakeup_rx, &mut resume),
        )
        .await
        .expect("NetworkChanged interrupts stalled preparation");
        assert!(matches!(outcome, PrepareOutcome::RetryNow));

        // 4. `better_ranked_routes` + `spawn_better_route_probe` discovers a higher-ranked
        // candidate answering with the expected `environmentId`, and reports routes whose host
        // refuses this device's sign-in (`pingdotgg/t3code#33199`).
        let wan_active = CandidateRoute::saved("https://remote.example.com:3773");
        let local_better = CandidateRoute::saved(server.credentials().http_base);
        let better = better_ranked_routes(&[wan_active.clone(), local_better.clone()], &wan_active);
        assert_eq!(better.len(), 1);
        assert_eq!(better[0].id, local_better.id);
        let mut probe = Some(spawn_better_route_probe(
            better.clone(),
            Some("env-wake".into()),
            server.credentials().access_token,
            None,
        ));
        let winner = tokio::time::timeout(Duration::from_secs(3), next_better_route_probe(&mut probe))
            .await
            .expect("better-route probe completes");
        assert_eq!(winner.ready.as_deref(), Some(local_better.id.as_str()));
        assert!(winner.refused.is_empty());

        // When a better route's host answers its descriptor but refuses our credential (401),
        // preflight surfaces it in `refused` (`pingdotgg/t3code#33199`).
        let refusing = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refusing_base = format!("http://{}", refusing.local_addr().unwrap());
        let refusing_task = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            for _ in 0..2 {
                let Ok((mut sock, _)) = refusing.accept().await else { break };
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                let (status, body) = if head.starts_with("GET /.well-known/t3/environment ") {
                    (200, r#"{"environmentId":"env-wake","label":"Refusing","serverVersion":"0.1.9","orchestrationProtocolVersion":2}"#)
                } else {
                    (401, r#"{"code":"auth_invalid","reason":"invalid_credential","traceId":"trace-refused"}"#)
                };
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        let refusing_route = CandidateRoute::saved(refusing_base);
        let mut refused_probe = Some(spawn_better_route_probe(
            vec![refusing_route.clone()],
            Some("env-wake".into()),
            "expired-token".into(),
            None,
        ));
        let refused_result = tokio::time::timeout(Duration::from_secs(3), next_better_route_probe(&mut refused_probe))
            .await
            .expect("refused better-route probe completes");
        refusing_task.abort();
        assert_eq!(refused_result.ready, None);
        assert_eq!(refused_result.refused, vec![refusing_route.id]);
    }

    #[test]
    fn provider_status_deltas_inherit_omitted_and_null_lists_and_drop_unrebuildable() {
        let mut held = HashMap::new();
        held.insert(
            "codex".to_owned(),
            json!({
                "instanceId": "codex",
                "driver": "codex",
                "models": [{"slug": "gpt-5.4", "name": "GPT-5.4"}],
                "slashCommands": [{"name": "review"}],
                "skills": [{"name": "rust", "enabled": true}]
            }),
        );
        let deltas = vec![
            json!({
                "instanceId": "codex",
                "driver": "codex",
                "status": "warning",
                "models": null
            }),
            json!({
                "instanceId": "brand-new-missing-lists",
                "driver": "codex",
                "status": "ready"
            }),
        ];
        let rebuilt = apply_provider_status_deltas(&held, &deltas);
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(rebuilt[0]["status"], "warning");
        assert_eq!(rebuilt[0]["models"][0]["slug"], "gpt-5.4");
        assert_eq!(rebuilt[0]["slashCommands"][0]["name"], "review");
        assert_eq!(rebuilt[0]["skills"][0]["name"], "rust");
    }

    #[test]
    fn open_thread_from_seeds_resume_sequence_and_close_clears_it() {
        let mut resume = Resume {
            shell: None,
            shell_options: None,
            threads: HashMap::new(),
        };
        drain_command(
            Command::OpenThread { thread_id: "thr-1".to_owned(), sequence: Some(42) },
            &mut resume,
        );
        assert_eq!(resume.threads.get("thr-1"), Some(&Some(42)));
        drain_command(Command::CloseThread("thr-1".to_owned()), &mut resume);
        assert!(!resume.threads.contains_key("thr-1"));

        let (commands, mut received) = mpsc::unbounded_channel();
        let (_, session_state) = watch::channel(None);
        let (_, connection_generation) = watch::channel(1);
        let (_, active_credentials) = watch::channel(None);
        let handle = ClientHandle {
            refs: Default::default(),
            commands,
            credentials: Arc::new(Credentials {
                http_base: "http://127.0.0.1:1".into(),
                access_token: String::new(),
            }),
            active_credentials,
            session_state,
            connection_generation,
        };
        handle.open_thread_from("thr-2", Some(99));
        let cmd = received.try_recv().expect("queued command");
        match cmd {
            Command::OpenThread { thread_id, sequence } => {
                assert_eq!(thread_id, "thr-2");
                assert_eq!(sequence, Some(99));
            }
            _ => panic!("expected Command::OpenThread"),
        }
    }
}

