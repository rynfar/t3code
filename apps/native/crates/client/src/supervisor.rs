//! Keeps one environment connected: mints a ticket, opens a session, subscribes the shell and
//! every open thread, and on disconnect reconnects with backoff, resuming each stream from the
//! last sequence it saw. Each connection also fetches the device's grants
//! (`client-runtime/src/state/session.ts`). Frames are decoded here, off the UI thread.

use std::collections::HashMap;
use std::sync::Arc;
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
    /// The server speaks another orchestration protocol. Nothing retries until `retry_now`.
    Unsupported { reason: String, server_outdated: bool },
    NeedsPairing { reason: String },
}

#[derive(Debug)]
pub enum ClientEvent {
    Status(ConnectionStatus),
    Shell(ShellStreamItem),
    Thread { thread_id: String, item: Box<ThreadStreamItem> },
    ServerConfig(Box<ServerConfigStreamEvent>),
    StreamError { context: String, message: String },
}

enum Command {
    OpenThread(String),
    CloseThread(String),
    RefreshSessionState,
    RetryNow,
    Request { method: &'static str, payload: Value, reply: oneshot::Sender<Result<Value, RpcError>> },
    Stream { method: &'static str, payload: Value, reply: oneshot::Sender<Result<StreamHandle, RpcError>> },
}

#[derive(Clone)]
pub struct ClientHandle {
    commands: mpsc::UnboundedSender<Command>,
    credentials: Arc<Credentials>,
    session_state: watch::Receiver<Option<AuthSessionState>>,
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
        let queued = self.commands.send(Command::Request { method, payload, reply }).is_ok();
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
        let queued = self.commands.send(Command::Stream { method, payload, reply }).is_ok();
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
const THREAD_RETRY: Duration = Duration::from_secs(2);
/// `SESSION_STATE_RETRY_BASE_MS` and `SESSION_STATE_RETRY_MAX_MS`.
const SESSION_STATE_RETRY: Duration = Duration::from_secs(2);
const SESSION_STATE_RETRY_MAX: Duration = Duration::from_secs(60);

type Events = mpsc::Sender<ClientEvent>;

pub fn start(runtime: &tokio::runtime::Handle, credentials: Credentials) -> (ClientHandle, mpsc::Receiver<ClientEvent>) {
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (events, event_rx) = mpsc::channel(EVENT_QUEUE);
    let (grants, session_state) = watch::channel(None);
    let handle = ClientHandle { commands, credentials: Arc::new(credentials.clone()), session_state };
    runtime.spawn(supervise(credentials, command_rx, events, grants));
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
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: Events,
    grants: watch::Sender<Option<AuthSessionState>>,
) {
    let mut resume = Resume { shell: None, shell_options: None, threads: HashMap::new() };
    let mut attempt: u32 = 0;
    let mut last_failure = String::new();
    loop {
        // An attempt after a failure still reads as reconnecting, with the reason it failed.
        let _ = events.send(ClientEvent::Status(if attempt == 0 {
            ConnectionStatus::Connecting
        } else {
            ConnectionStatus::Reconnecting { reason: last_failure.clone(), attempt }
        })).await;
        let reason = match connect(&credentials).await {
            Ok(session) => {
                attempt = 0;
                let _ = events.send(ClientEvent::Status(ConnectionStatus::Connected)).await;
                let mut fetch = SessionStateFetch { credentials: &credentials, grants: &grants, task: None };
                fetch.start();
                let end = run_session(&session, &mut resume, &mut commands, &events, &mut fetch).await;
                fetch.stop();
                match end {
                    SessionEnd::Closed(reason) => reason,
                    SessionEnd::Dropped => return,
                }
            }
            Err(ConnectError::Unauthorized(reason)) => {
                let _ = events.send(ClientEvent::Status(ConnectionStatus::NeedsPairing { reason })).await;
                return;
            }
            Err(ConnectError::Unsupported { reason, server_outdated }) => {
                let _ = events.send(ClientEvent::Status(ConnectionStatus::Unsupported { reason: reason.clone(), server_outdated })).await;
                if !wait_draining(None, &mut commands, &mut resume).await {
                    return;
                }
                attempt += 1;
                last_failure = reason;
                continue;
            }
            Err(ConnectError::Transient(reason)) => reason,
        };
        attempt += 1;
        last_failure = reason.clone();
        let _ = events.send(ClientEvent::Status(ConnectionStatus::Reconnecting { reason, attempt })).await;
        let delay = Duration::from_millis((500u64 << attempt.min(5)).min(15_000));
        if !wait_draining(Some(delay), &mut commands, &mut resume).await {
            return;
        }
    }
}

enum ConnectError {
    Unauthorized(String),
    Unsupported { reason: String, server_outdated: bool },
    Transient(String),
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
async fn wait_draining(delay: Option<Duration>, commands: &mut mpsc::UnboundedReceiver<Command>, resume: &mut Resume) -> bool {
    let sleep = async {
        match delay {
            Some(delay) => tokio::time::sleep(delay).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return true,
            command = commands.recv() => match command {
                None => return false,
                Some(Command::RetryNow) => return true,
                Some(Command::OpenThread(id)) => { resume.threads.entry(id).or_insert(None); }
                Some(Command::CloseThread(id)) => { resume.threads.remove(&id); }
                Some(Command::RefreshSessionState) => {}
                Some(Command::Request { reply, .. }) => {
                    let _ = reply.send(Err(RpcError::Closed));
                }
                Some(Command::Stream { reply, .. }) => {
                    let _ = reply.send(Err(RpcError::Closed));
                }
            }
        }
    }
}

enum SessionEnd {
    Closed(String),
    Dropped,
}

type ThreadFrame = (String, Option<Result<Value, RpcError>>);

async fn run_session(
    session: &Session,
    resume: &mut Resume,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &Events,
    fetch: &mut SessionStateFetch<'_>,
) -> SessionEnd {
    let mut server_config = session.stream(method::SUBSCRIBE_SERVER_CONFIG, commands::subscribe_server_config()).ok();
    let options = initial_shell_options(&mut server_config, events).await;
    // Deltas never restore rows trimmed under other options (nor trim them), so the shell starts
    // over from an authoritative snapshot, as `shell.ts` does for every new session.
    if resume.shell_options.replace(options) != Some(options) {
        resume.shell = None;
    }
    let Ok(mut shell) = session.stream(method::SUBSCRIBE_SHELL, commands::subscribe_shell(resume.shell, options)) else {
        return SessionEnd::Closed("Could not subscribe to the shell.".into());
    };
    let (thread_tx, mut thread_rx) = mpsc::channel::<ThreadFrame>(256);
    let mut forwarders: HashMap<String, JoinHandle<()>> = HashMap::new();
    for (thread_id, sequence) in resume.threads.clone() {
        forwarders.insert(thread_id.clone(), forward_thread(session, &thread_id, sequence, thread_tx.clone()));
    }
    let end = loop {
        tokio::select! {
            value = shell.receiver.recv() => match value {
                Some(Ok(value)) => match serde_json::from_value::<ShellStreamItem>(value) {
                    Ok(item) => {
                        resume.shell = shell_sequence(&item).or(resume.shell);
                        let _ = events.send(ClientEvent::Shell(item)).await;
                    }
                    Err(error) => report(events, "shell", error.to_string()).await,
                },
                Some(Err(error)) => break SessionEnd::Closed(format!("Shell subscription failed: {error}")),
                None => break SessionEnd::Closed("Shell subscription ended.".into()),
            },
            value = next_config(&mut server_config) => match value {
                Some(Ok(value)) => match serde_json::from_value::<ServerConfigStreamEvent>(value) {
                    Ok(event) => { let _ = events.send(ClientEvent::ServerConfig(Box::new(event))).await; }
                    Err(error) => report(events, "server config", error.to_string()).await,
                },
                // The rest of the session works without it; the next session subscribes again.
                Some(Err(error)) => {
                    server_config = None;
                    report(events, "server config", error.to_string()).await;
                }
                None => server_config = None,
            },
            frame = thread_rx.recv() => {
                let Some((thread_id, value)) = frame else { continue };
                if !resume.threads.contains_key(&thread_id) { continue }
                match value {
                    Some(Ok(value)) => match decode_thread_stream_item(value) {
                        Ok(item) => {
                            if let Some(sequence) = thread_sequence(&item) {
                                resume.threads.insert(thread_id.clone(), Some(sequence));
                            }
                            let _ = events.send(ClientEvent::Thread { thread_id, item: Box::new(item) }).await;
                        }
                        Err(error) => report(events, &format!("thread {thread_id}"), error.to_string()).await,
                    },
                    Some(Err(RpcError::Closed)) | None => {}
                    Some(Err(error)) => report(events, &format!("thread {thread_id}"), error.to_string()).await,
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
                Some(Command::CloseThread(thread_id)) => {
                    resume.threads.remove(&thread_id);
                    if let Some(task) = forwarders.remove(&thread_id) { task.abort(); }
                }
                Some(Command::RefreshSessionState) => fetch.start(),
                Some(Command::RetryNow) => {}
                Some(Command::Request { method, payload, reply }) => {
                    let response = session.send_request(method, payload);
                    tokio::spawn(async move {
                        let _ = reply.send(response.await);
                    });
                }
                Some(Command::Stream { method, payload, reply }) => {
                    let _ = reply.send(session.stream(method, payload));
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
async fn initial_shell_options(stream: &mut Option<StreamHandle>, events: &Events) -> ShellOptions {
    let wait = async {
        loop {
            match next_config(stream).await {
                Some(Ok(value)) => match serde_json::from_value::<ServerConfigStreamEvent>(value) {
                    Ok(event) => {
                        let options = match &event {
                            ServerConfigStreamEvent::Snapshot { config } => {
                                Some(ShellOptions::for_server(config.shell_thread_families, config.shell_older_settled))
                            }
                            _ => None,
                        };
                        let _ = events.send(ClientEvent::ServerConfig(Box::new(event))).await;
                        if let Some(options) = options {
                            return options;
                        }
                    }
                    Err(error) => report(events, "server config", error.to_string()).await,
                },
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

/// The grants fetch of one connection. Failures retry with backoff while the connection lasts,
/// so a fetch that failed mid-reconnect cannot outlive the socket that recovered.
struct SessionStateFetch<'a> {
    credentials: &'a Credentials,
    grants: &'a watch::Sender<Option<AuthSessionState>>,
    task: Option<JoinHandle<()>>,
}

impl SessionStateFetch<'_> {
    fn start(&mut self) {
        if self.task.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let (credentials, grants) = (self.credentials.clone(), self.grants.clone());
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
                        return;
                    }
                    Err(error) => tracing::warn!(%error, "session state fetch failed"),
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
    }
}

async fn next_config(stream: &mut Option<StreamHandle>) -> Option<Result<Value, RpcError>> {
    match stream {
        Some(stream) => stream.receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// Keeps one thread subscribed for the life of the session. A stream that ends or fails on a
/// healthy connection resubscribes from the last sequence it delivered, as `threads.ts` retries.
fn forward_thread(session: &Session, thread_id: &str, sequence: Option<u64>, tx: mpsc::Sender<ThreadFrame>) -> JoinHandle<()> {
    let session = session.clone();
    let thread_id = thread_id.to_owned();
    tokio::spawn(async move {
        let mut sequence = sequence;
        loop {
            let Ok(mut stream) = session.stream(method::SUBSCRIBE_THREAD, commands::subscribe_thread(&thread_id, sequence)) else {
                return;
            };
            while let Some(value) = stream.receiver.recv().await {
                if let Ok(item) = &value {
                    sequence = item.get("snapshotSequence").or_else(|| item.get("sequence")).and_then(Value::as_u64).or(sequence);
                }
                let closed = matches!(value, Err(RpcError::Closed));
                if tx.send((thread_id.clone(), Some(value))).await.is_err() || closed {
                    return;
                }
            }
            tokio::time::sleep(THREAD_RETRY).await;
        }
    })
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

