//! One Effect RPC session over one WebSocket. The protocol never reopens its socket; the
//! supervisor makes a new session on disconnect.
//!
//! Flow control mirrors `RpcClient`: a chunk is acked only after the consumer took its values,
//! and the server waits for acks. Each stream has its own in-order delivery task, so the socket
//! reader never waits on a consumer and one slow stream cannot stall requests, other streams, or
//! heartbeats.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use t3_core::rpc::{ClientMessage, RequestId, RpcFailure, ServerMessage, decode_server_frame, encode_client_message};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// `SLOW_RPC_ACK_THRESHOLD_MS` (`apps/web/src/rpc/requestLatencyState.ts`).
pub const SLOW_RPC_ACK_THRESHOLD: Duration = Duration::from_secs(15);
/// `LONG_RUNNING_RPC_ACK_THRESHOLD_MS` (`apps/web/src/rpc/requestLatencyState.ts`).
pub const LONG_RUNNING_RPC_ACK_THRESHOLD: Duration = Duration::from_secs(120);
/// `MAX_TRACKED_RPC_ACK_REQUESTS` (`apps/web/src/rpc/requestLatencyState.ts`).
pub const MAX_TRACKED_RPC_ACK_REQUESTS: usize = 256;

/// One RPC request that has waited past its latency threshold without completing
/// (`SlowRpcAckRequest` in `apps/web/src/rpc/requestLatencyState.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowRpcAckRequest {
    pub request_id: String,
    pub started_at_ms: i64,
    pub tag: String,
    pub threshold_ms: u64,
}

/// `shouldTrackRpcAck` (`apps/web/src/rpc/requestLatencyState.ts`): excludes subscriptions,
/// background pull-request fetches, usage/activity reports, and user-paced forge sign-in waits.
pub fn should_track_rpc_ack(method: &str) -> bool {
    !method.contains("subscribe")
        && !method.starts_with("pullRequests.")
        && !matches!(
            method,
            "server.getUsageSummary" | "server.reportClientActivity" | "sourceControl.signInWait"
        )
}

/// `rpcAckThresholdMs` (`apps/web/src/rpc/requestLatencyState.ts`).
pub fn rpc_ack_threshold_for(method: &str, base_threshold: Duration) -> Duration {
    if matches!(
        method,
        "server.updateProvider"
            | "server.refreshProviders"
            | "server.updateServer"
            | "vcs.refreshStatus"
    ) {
        base_threshold.max(LONG_RUNNING_RPC_ACK_THRESHOLD)
    } else {
        base_threshold
    }
}

struct SlowRpcState {
    pending: VecDeque<(String, JoinHandle<()>)>,
    slow: Vec<SlowRpcAckRequest>,
}

/// Tracks pending RPC latency and publishes slow requests (`requestLatencyState.ts`).
pub struct SlowRpcTracker {
    base_threshold: Duration,
    state: Mutex<SlowRpcState>,
    tx: watch::Sender<Vec<SlowRpcAckRequest>>,
}

impl SlowRpcTracker {
    pub fn new(base_threshold: Duration) -> Arc<Self> {
        let (tx, _) = watch::channel(Vec::new());
        Arc::new(Self {
            base_threshold,
            state: Mutex::new(SlowRpcState {
                pending: VecDeque::new(),
                slow: Vec::new(),
            }),
            tx,
        })
    }

    pub fn subscribe(&self) -> watch::Receiver<Vec<SlowRpcAckRequest>> {
        self.tx.subscribe()
    }

    pub fn snapshot(&self) -> Vec<SlowRpcAckRequest> {
        self.tx.borrow().clone()
    }

    pub fn track(self: &Arc<Self>, request_id: String, method: &str, tag: String) -> Option<SlowRpcGuard> {
        if !should_track_rpc_ack(method) {
            return None;
        }
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let threshold = rpc_ack_threshold_for(method, self.base_threshold);
        let started_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let entry = SlowRpcAckRequest {
            request_id: request_id.clone(),
            started_at_ms,
            tag,
            threshold_ms: threshold.as_millis().min(u64::MAX as u128) as u64,
        };
        let tracker = Arc::clone(self);
        let timer_id = request_id.clone();
        let timer = handle.spawn(async move {
            tokio::time::sleep(threshold).await;
            tracker.promote_slow(timer_id, entry);
        });
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pos) = state.pending.iter().position(|(id, _)| id == &request_id)
                && let Some((_, prev)) = state.pending.remove(pos)
            {
                prev.abort();
            }
            while state.pending.len() >= MAX_TRACKED_RPC_ACK_REQUESTS {
                if let Some((_, oldest)) = state.pending.pop_front() {
                    oldest.abort();
                }
            }
            state.pending.push_back((request_id.clone(), timer));
        }
        Some(SlowRpcGuard {
            tracker: Arc::clone(self),
            request_id,
        })
    }

    fn promote_slow(&self, request_id: String, entry: SlowRpcAckRequest) {
        let next = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let Some(pos) = state.pending.iter().position(|(id, _)| id == &request_id) else {
                return;
            };
            state.pending.remove(pos);
            state.slow.push(entry);
            if state.slow.len() > MAX_TRACKED_RPC_ACK_REQUESTS {
                let overflow = state.slow.len() - MAX_TRACKED_RPC_ACK_REQUESTS;
                state.slow.drain(..overflow);
            }
            state.slow.clone()
        };
        self.tx.send_replace(next);
    }

    pub fn acknowledge(&self, request_id: &str) {
        let updated = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pos) = state.pending.iter().position(|(id, _)| id == request_id)
                && let Some((_, timer)) = state.pending.remove(pos)
            {
                timer.abort();
            }
            let before = state.slow.len();
            state.slow.retain(|req| req.request_id != request_id);
            (state.slow.len() != before).then(|| state.slow.clone())
        };
        if let Some(next) = updated {
            self.tx.send_replace(next);
        }
    }
}

/// Drops the tracked request from pending/slow state when the RPC completes or is cancelled.
pub struct SlowRpcGuard {
    tracker: Arc<SlowRpcTracker>,
    request_id: String,
}

impl Drop for SlowRpcGuard {
    fn drop(&mut self) {
        self.tracker.acknowledge(&self.request_id);
    }
}

fn global_slow_rpc_tracker() -> &'static Arc<SlowRpcTracker> {
    static TRACKER: OnceLock<Arc<SlowRpcTracker>> = OnceLock::new();
    TRACKER.get_or_init(|| SlowRpcTracker::new(SLOW_RPC_ACK_THRESHOLD))
}

fn next_tracked_rpc_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Subscribes to the live set of slow pending RPC requests across all sessions
/// (`subscribeSlowRpcAckRequests` in `apps/web/src/rpc/requestLatencyState.ts`).
pub fn subscribe_slow_rpc_requests() -> watch::Receiver<Vec<SlowRpcAckRequest>> {
    global_slow_rpc_tracker().subscribe()
}

/// Test helper: publishes a synthetic slow-RPC snapshot to `subscribe_slow_rpc_requests`.
#[cfg(feature = "test-support")]
pub fn publish_slow_rpc_requests_for_test(requests: Vec<SlowRpcAckRequest>) {
    let tracker = global_slow_rpc_tracker();
    {
        let mut state = tracker.state.lock().unwrap_or_else(|p| p.into_inner());
        state.slow = requests.clone();
    }
    tracker.tx.send_replace(requests);
}

const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Thread subscriptions ask for bounded snapshots, but older servers and other streams can still
/// send whole projections.
const MAX_MESSAGE_BYTES: usize = 256 << 20;
/// `PART_PREFIX` (`packages/shared/src/socketFrameParts.ts`, `pingdotgg/t3code#23758`):
/// no JSON message starts with ASCII Record Separator; it prefixes every part of a split frame
/// except the last.
pub(crate) const FRAME_PART_PREFIX: char = '\u{001e}';

/// Reassembles application-level socket frame parts (`joinSocketFrameParts` in
/// `packages/shared/src/socketFrameParts.ts`). Each incoming part updates socket liveness so a
/// large `ServerConfig` on a slow link never triggers the 15-second idle timeout.
#[derive(Default)]
pub(crate) struct FramePartJoiner {
    pending: String,
}

impl FramePartJoiner {
    /// Consumes one WebSocket text frame. Returns `Ok(None)` when holding an intermediate part,
    /// `Ok(Some(whole))` when a complete JSON frame is ready to decode, or `Err` if the assembled
    /// message exceeds `MAX_MESSAGE_BYTES`.
    pub(crate) fn push(&mut self, frame: String) -> Result<Option<String>, &'static str> {
        if let Some(piece) = frame.strip_prefix(FRAME_PART_PREFIX) {
            if self.pending.len().saturating_add(piece.len()) > MAX_MESSAGE_BYTES {
                self.pending.clear();
                return Err("Server frame exceeded maximum message size.");
            }
            self.pending.push_str(piece);
            return Ok(None);
        }
        if self.pending.is_empty() {
            return Ok(Some(frame));
        }
        if self.pending.len().saturating_add(frame.len()) > MAX_MESSAGE_BYTES {
            self.pending.clear();
            return Err("Server frame exceeded maximum message size.");
        }
        let mut whole = std::mem::take(&mut self.pending);
        whole.push_str(&frame);
        Ok(Some(whole))
    }
}

/// `client-runtime/src/rpc/protocol.ts`: pings every 5 seconds, and 15 seconds without any frame
/// drops the socket. The stream queue bounds what a slow consumer buffers before acks stop.
#[derive(Debug, Clone, Copy)]
struct Timing {
    ping_interval: Duration,
    ping_timeout: Duration,
    stream_queue: usize,
}

const TIMING: Timing = Timing {
    ping_interval: Duration::from_secs(5),
    ping_timeout: Duration::from_secs(15),
    stream_queue: 256,
};

/// Why a WebSocket did not open.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    #[error("This device is no longer authorized. Pair again.")]
    Unauthorized,
    /// HTTP 426 (`orchestration_protocol_incompatible` in `apps/server/src/ws.ts`): the server
    /// speaks another orchestration protocol, the one it names when it does.
    #[error("The server speaks another orchestration protocol.")]
    Incompatible { server_protocol: Option<u32> },
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RpcError {
    #[error(transparent)]
    Failure(#[from] RpcFailure),
    #[error("connection closed")]
    Closed,
}

enum Pending {
    Unary(oneshot::Sender<Result<Value, RpcError>>),
    Stream(mpsc::UnboundedSender<Delivery>),
}

enum Delivery {
    Chunk(Vec<Value>),
    Fail(RpcError),
}

/// `None` once the session closed: later requests fail immediately instead of waiting forever.
type PendingMap = Arc<Mutex<Option<HashMap<RequestId, Pending>>>>;

fn lock(pending: &PendingMap) -> MutexGuard<'_, Option<HashMap<RequestId, Pending>>> {
    pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
pub struct Session {
    outbound: mpsc::UnboundedSender<ClientMessage>,
    pending: PendingMap,
    next_id: Arc<AtomicU64>,
    closed: watch::Receiver<Option<String>>,
    stream_queue: usize,
    inbound_frames: Arc<AtomicU64>,
    frame_notify: Arc<tokio::sync::Notify>,
}

pub struct StreamHandle {
    pub(crate) settled: Option<crate::vcs::OnSettled>,
    pub receiver: mpsc::Receiver<Result<Value, RpcError>>,
    id: RequestId,
    outbound: mpsc::UnboundedSender<ClientMessage>,
    pending: PendingMap,
}

impl StreamHandle {
    /// Connection-wide Defect/ClientProtocolError failures clear the session's pending map.
    pub(crate) fn connection_closed(&self) -> bool { lock(&self.pending).is_none() }

    #[cfg(feature = "test-support")]
    pub fn fixture() -> (Self, mpsc::Sender<Result<Value, RpcError>>) {
        let (tx, receiver) = mpsc::channel(8);
        let (outbound, _) = mpsc::unbounded_channel();
        let handle = StreamHandle {
            settled: None,
            receiver,
            id: RequestId::Str("fixture-stream".into()),
            outbound,
            pending: Arc::new(Mutex::new(Some(HashMap::new()))),
        };
        (handle, tx)
    }
}

impl Drop for StreamHandle {
    /// Later chunks for this id are dropped unread instead of buffering until the server exits.
    fn drop(&mut self) {
        if let Some(map) = lock(&self.pending).as_mut() {
            map.remove(&self.id);
        }
        let _ = self.outbound.send(ClientMessage::Interrupt { request_id: self.id.clone() });
    }
}

struct PendingRequest {
    id: RequestId,
    outbound: mpsc::UnboundedSender<ClientMessage>,
    pending: PendingMap,
}
impl Drop for PendingRequest {
    fn drop(&mut self) {
        let removed = lock(&self.pending).as_mut().and_then(|map| map.remove(&self.id)).is_some();
        if removed { let _ = self.outbound.send(ClientMessage::Interrupt { request_id: self.id.clone() }); }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeClockOutcome {
    pub alive: bool,
    pub clock_sample: Option<(i64, i64, i64)>,
}

fn epoch_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

impl Session {
    pub async fn connect(url: &str) -> Result<Session, OpenError> {
        Self::connect_with(url, TIMING).await
    }

    async fn connect_with(url: &str, timing: Timing) -> Result<Session, OpenError> {
        let config = WebSocketConfig {
            max_message_size: Some(MAX_MESSAGE_BYTES),
            max_frame_size: Some(MAX_MESSAGE_BYTES),
            ..Default::default()
        };
        let connect = tokio_tungstenite::connect_async_with_config(url, Some(config), false);
        let (socket, _) = tokio::time::timeout(OPEN_TIMEOUT, connect)
            .await
            .map_err(|_| OpenError::Failed("Timed out opening the WebSocket.".to_owned()))?
            .map_err(open_error)?;
        let (mut sink, mut stream) = socket.split();
        let (outbound, mut outbound_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let pending: PendingMap = Arc::new(Mutex::new(Some(HashMap::new())));
        let (closed_tx, closed) = watch::channel(None);
        let inbound_frames = Arc::new(AtomicU64::new(0));
        let frame_notify = Arc::new(tokio::sync::Notify::new());

        // Acks go through a weak handle so the task does not keep its own session alive: when
        // every `Session` and `StreamHandle` is gone, `outbound_rx` closes and the socket shuts.
        let task_pending = pending.clone();
        let task_frames = inbound_frames.clone();
        let task_notify = frame_notify.clone();
        tokio::spawn(async move {
            let mut last_frame = Instant::now();
            let mut ping = tokio::time::interval(timing.ping_interval);
            let mut frame_parts = FramePartJoiner::default();
            let reason = loop {
                tokio::select! {
                    frame = stream.next() => {
                        let text = match frame {
                            None => break "Disconnected.".into(),
                            Some(Ok(Message::Text(text))) => text,
                            Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                            Some(Ok(Message::Close(_))) => break "Server closed the connection.".into(),
                            Some(Ok(_)) => {
                                last_frame = Instant::now();
                                task_frames.fetch_add(1, Ordering::Release);
                                task_notify.notify_waiters();
                                continue;
                            }
                            Some(Err(error)) => break describe_ws_error(error).into(),
                        };
                        last_frame = Instant::now();
                        let text = match frame_parts.push(text) {
                            Ok(Some(whole)) => whole,
                            Ok(None) => {
                                task_frames.fetch_add(1, Ordering::Release);
                                task_notify.notify_waiters();
                                continue;
                            }
                            Err(error) => break error.into(),
                        };
                        let end = match decode_server_frame(&text) {
                            Ok(messages) => messages.into_iter().find_map(|m| route(m, &task_pending)),
                            Err(error) => {
                                tracing::warn!(%error, "undecodable server frame");
                                None
                            }
                        };
                        if let Some(end) = end {
                            break end;
                        }
                        task_frames.fetch_add(1, Ordering::Release);
                        task_notify.notify_waiters();
                    }
                    outgoing = outbound_rx.recv() => {
                        let Some(message) = outgoing else { break "Session dropped.".into() };
                        if let Err(error) = sink.send(Message::Text(encode_client_message(&message))).await {
                            break describe_ws_error(error).into();
                        }
                    }
                    _ = ping.tick() => {
                        if last_frame.elapsed() > timing.ping_timeout {
                            break format!("The server stopped responding for {}s.", timing.ping_timeout.as_secs()).into();
                        }
                        if sink.send(Message::Text(encode_client_message(&ClientMessage::Ping))).await.is_err() {
                            break "Disconnected.".into();
                        }
                    }
                }
            };
            let _ = sink.close().await;
            let entries = lock(&task_pending).take().unwrap_or_default();
            for (_, entry) in entries {
                let error = reason.failure.clone().map_or(RpcError::Closed, RpcError::Failure);
                match entry {
                    Pending::Unary(tx) => {
                        let _ = tx.send(Err(error));
                    }
                    Pending::Stream(tx) => {
                        let _ = tx.send(Delivery::Fail(error));
                    }
                }
            }
            let _ = closed_tx.send(Some(reason.message));
        });

        Ok(Session {
            outbound,
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
            closed,
            stream_queue: timing.stream_queue,
            inbound_frames,
            frame_notify,
        })
    }

    /// Count of inbound WebSocket frames received so far (`RpcSession.inboundFrames`).
    pub fn inbound_frames(&self) -> u64 {
        self.inbound_frames.load(Ordering::Acquire)
    }

    /// Sends a heartbeat ping and waits up to `timeout` for any inbound frame or `Pong`.
    /// Returns `true` if the socket is alive, `false` if it timed out or closed.
    pub async fn probe(&self, timeout: Duration) -> bool {
        let before = self.inbound_frames.load(Ordering::Acquire);
        if self.outbound.send(ClientMessage::Ping).is_err() {
            return false;
        }
        let wait = async {
            loop {
                let notified = self.frame_notify.notified();
                if self.inbound_frames.load(Ordering::Acquire) > before {
                    return true;
                }
                tokio::select! {
                    _ = notified => {
                        if self.inbound_frames.load(Ordering::Acquire) > before {
                            return true;
                        }
                    }
                    _ = self.closed() => return false,
                }
            }
        };
        tokio::time::timeout(timeout, wait).await.unwrap_or(false)
    }

    /// Probes liveness and refreshes host clock skew when `server.probe` answers with `serverTime`
    /// (`pingdotgg/t3code#41037`).
    pub async fn probe_with_clock(
        &self,
        timeout: Duration,
        environment_id: Option<String>,
    ) -> ProbeClockOutcome {
        let before = self.inbound_frames.load(Ordering::Acquire);
        let started = std::time::Instant::now();
        let requested_at_ms = epoch_now_ms();
        let rpc = self.send_request(t3_core::commands::method::SERVER_PROBE, serde_json::json!({}));
        tokio::pin!(rpc);

        let wait_inbound = async {
            loop {
                let notified = self.frame_notify.notified();
                if self.inbound_frames.load(Ordering::Acquire) > before {
                    return true;
                }
                tokio::select! {
                    _ = notified => {
                        if self.inbound_frames.load(Ordering::Acquire) > before {
                            return true;
                        }
                    }
                    _ = self.closed() => return false,
                }
            }
        };

        let result = tokio::time::timeout(timeout, async {
            tokio::select! {
                biased;
                reply = &mut rpc => Ok(reply),
                alive = wait_inbound => Err(alive),
            }
        })
        .await;

        match result {
            Ok(Ok(Ok(reply))) => {
                let received_at_ms = epoch_now_ms();
                let server_time_ms = reply
                    .get("serverTime")
                    .and_then(Value::as_str)
                    .and_then(t3_core::sidebar::time::parse_iso);
                if let Some(server_time_ms) = server_time_ms
                    && let Some(env_id) = environment_id.as_deref().filter(|id| !id.is_empty())
                {
                    t3_core::clock::record_server_clock(
                        env_id,
                        Some(server_time_ms),
                        requested_at_ms,
                        received_at_ms,
                    );
                }
                ProbeClockOutcome {
                    alive: true,
                    clock_sample: server_time_ms.map(|st| (st, requested_at_ms, received_at_ms)),
                }
            }
            Ok(Ok(Err(RpcError::Failure(_)))) => {
                // Older servers without `server.probe` reject the RPC; fall back to WebSocket ping.
                let remaining = timeout
                    .saturating_sub(started.elapsed())
                    .max(Duration::from_millis(50));
                ProbeClockOutcome {
                    alive: self.probe(remaining).await,
                    clock_sample: None,
                }
            }
            Ok(Ok(Err(RpcError::Closed))) | Err(_) => ProbeClockOutcome {
                alive: false,
                clock_sample: None,
            },
            Ok(Err(alive)) => ProbeClockOutcome {
                alive,
                clock_sample: None,
            },
        }
    }

    fn start(&self, tag: &str, payload: Value, entry: Pending) -> Result<RequestId, RpcError> {
        let id: RequestId = self.next_id.fetch_add(1, Ordering::Relaxed).into();
        let mut pending = lock(&self.pending);
        let map = pending.as_mut().ok_or(RpcError::Closed)?;
        map.insert(id.clone(), entry);
        let request = ClientMessage::Request { id: id.clone(), tag: tag.to_owned(), payload, headers: Vec::new() };
        if self.outbound.send(request).is_err() {
            map.remove(&id);
            return Err(RpcError::Closed);
        }
        Ok(id)
    }

    pub async fn request(&self, tag: &str, payload: Value) -> Result<Value, RpcError> {
        self.send_request(tag, payload).await
    }

    /// Sends the request now and returns its reply, so requests keep their call order.
    pub fn send_request(&self, tag: &str, payload: Value) -> impl Future<Output = Result<Value, RpcError>> + Send + 'static {
        self.send_request_labeled(tag, payload, None)
    }

    /// Sends a unary request and tracks its pending latency for `SlowRpcRequestToastCoordinator`.
    pub(crate) fn send_request_labeled(
        &self,
        method: &str,
        payload: Value,
        environment_label: Option<&str>,
    ) -> impl Future<Output = Result<Value, RpcError>> + Send + 'static {
        let (tx, rx) = oneshot::channel();
        let started = self
            .start(method, payload, Pending::Unary(tx))
            .map(|id| PendingRequest { id, outbound: self.outbound.clone(), pending: self.pending.clone() });
        let slow_guard = if started.is_ok() {
            let display_tag = match environment_label.map(str::trim).filter(|s| !s.is_empty()) {
                Some(label) => format!("{method} \u{00b7} {label}"),
                None => method.to_owned(),
            };
            let tracked_id = format!("rpc:{}", next_tracked_rpc_id());
            global_slow_rpc_tracker().track(tracked_id, method, display_tag)
        } else {
            None
        };
        async move {
            let _slow_guard = slow_guard;
            let _pending = started?;
            rx.await.unwrap_or(Err(RpcError::Closed))
        }
    }

    pub fn stream(&self, tag: &str, payload: Value) -> Result<StreamHandle, RpcError> {
        let (consumer, receiver) = mpsc::channel(self.stream_queue);
        let (inbox, mut deliveries) = mpsc::unbounded_channel::<Delivery>();
        let id = self.start(tag, payload, Pending::Stream(inbox))?;
        // Weak, so a live stream task never keeps a dropped session's socket open.
        let acks = self.outbound.downgrade();
        let ack_id = id.clone();
        tokio::spawn(async move {
            while let Some(delivery) = deliveries.recv().await {
                match delivery {
                    Delivery::Chunk(values) => {
                        for value in values {
                            if consumer.send(Ok(value)).await.is_err() {
                                return;
                            }
                        }
                        if let Some(outbound) = acks.upgrade() {
                            let _ = outbound.send(ClientMessage::Ack { request_id: ack_id.clone() });
                        }
                    }
                    Delivery::Fail(error) => {
                        let _ = consumer.send(Err(error)).await;
                        return;
                    }
                }
            }
        });
        Ok(StreamHandle { settled: None, receiver, id, outbound: self.outbound.clone(), pending: self.pending.clone() })
    }

    /// Resolves with the close reason once the socket is gone.
    pub async fn closed(&self) -> String {
        let mut closed = self.closed.clone();
        loop {
            if let Some(reason) = closed.borrow().clone() {
                return reason;
            }
            if closed.changed().await.is_err() {
                return "Disconnected.".to_owned();
            }
        }
    }
}

/// Why the socket closed, and the error pending calls fail with (`Closed` when `None`).
struct SessionEnd {
    message: String,
    failure: Option<RpcFailure>,
}

impl From<String> for SessionEnd {
    fn from(message: String) -> Self {
        SessionEnd { message, failure: None }
    }
}

impl From<&str> for SessionEnd {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

fn connection_failure(prefix: &str, error: Value) -> SessionEnd {
    let detail = error.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| error.to_string());
    let message = format!("{prefix}: {detail}");
    SessionEnd { failure: Some(RpcFailure { tag: None, message: message.clone(), error }), message }
}

/// Routes one server message. Returns how the connection ends when it must close.
fn route(message: ServerMessage, pending: &PendingMap) -> Option<SessionEnd> {
    match message {
        ServerMessage::Chunk { request_id, values } => {
            if let Some(Pending::Stream(inbox)) = lock(pending).as_ref().and_then(|map| map.get(&request_id)) {
                let _ = inbox.send(Delivery::Chunk(values));
            }
            None
        }
        ServerMessage::Exit { request_id, exit } => {
            // Removing a stream's entry drops its inbox: the delivery task drains what it has, then
            // the consumer sees the end of the stream.
            let entry = lock(pending).as_mut().and_then(|map| map.remove(&request_id));
            match (entry, exit.into_result()) {
                (Some(Pending::Unary(tx)), result) => {
                    let _ = tx.send(result.map_err(RpcError::from));
                }
                (Some(Pending::Stream(inbox)), Err(failure)) => {
                    let _ = inbox.send(Delivery::Fail(RpcError::Failure(failure)));
                }
                _ => {}
            }
            None
        }
        // Both are connection-wide in `RpcClient`: every pending call fails with the error. The
        // socket closes too, so the supervisor resubscribes on a fresh session.
        ServerMessage::Defect { defect } => Some(connection_failure("Server error", defect)),
        ServerMessage::ClientProtocolError { error } => Some(connection_failure("Protocol error", error)),
        ServerMessage::Pong | ServerMessage::Unknown => None,
    }
}

fn open_error(error: tokio_tungstenite::tungstenite::Error) -> OpenError {
    let tokio_tungstenite::tungstenite::Error::Http(response) = &error else { return OpenError::Failed(describe_ws_error(error)) };
    match response.status().as_u16() {
        401 | 403 => OpenError::Unauthorized,
        426 => {
            let body = response.body().as_deref().and_then(|body| serde_json::from_slice::<Value>(body).ok());
            let server_protocol = body.as_ref().and_then(|body| body.get("orchestrationProtocolVersion")?.as_u64()?.try_into().ok());
            OpenError::Incompatible { server_protocol }
        }
        status => OpenError::Failed(format!("The server refused the WebSocket ({status}).")),
    }
}

fn describe_ws_error(error: tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Io(error) => format!("Could not connect: {error}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod frame_parts_tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn frame_part_joiner_reassembles_split_frames_and_passes_whole_frames_through() {
        let mut joiner = FramePartJoiner::default();
        assert_eq!(joiner.push(r#"{"_tag":"Pong"}"#.into()).unwrap().as_deref(), Some(r#"{"_tag":"Pong"}"#));
        assert_eq!(joiner.push(format!("{FRAME_PART_PREFIX}{{\"_tag\":")).unwrap(), None);
        assert_eq!(joiner.push(format!("{FRAME_PART_PREFIX}\"Exit\",")).unwrap(), None);
        assert_eq!(
            joiner.push(r#""requestId":"1"}"#.into()).unwrap().as_deref(),
            Some(r#"{"_tag":"Exit","requestId":"1"}"#)
        );
    }

    #[tokio::test]
    async fn slow_link_multipart_frame_keeps_session_alive_past_ping_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["_tag"] == "Request" {
                    let id = value["id"].clone();
                    let full = serde_json::json!([{
                        "_tag": "Exit",
                        "requestId": id,
                        "exit": { "_tag": "Success", "value": { "big": "x".repeat(256) } }
                    }])
                    .to_string();
                    let chunk_size = full.len() / 4;
                    let pieces = [
                        format!("{FRAME_PART_PREFIX}{}", &full[..chunk_size]),
                        format!("{FRAME_PART_PREFIX}{}", &full[chunk_size..chunk_size * 2]),
                        format!("{FRAME_PART_PREFIX}{}", &full[chunk_size * 2..chunk_size * 3]),
                        full[chunk_size * 3..].to_owned(),
                    ];
                    // Total delivery (~160ms) exceeds ping_timeout (100ms), while each part arrives
                    // within the timeout (40ms < 100ms), proving slow-link liveness (`#23758`).
                    for piece in pieces {
                        tokio::time::sleep(Duration::from_millis(40)).await;
                        ws.send(Message::Text(piece)).await.unwrap();
                    }
                    // Keep the socket open until the client finishes and drops the session.
                    let _ = ws.next().await;
                    break;
                }
            }
        });
        let session = Session::connect_with(
            &format!("ws://{addr}/ws?frameParts=1"),
            Timing {
                ping_interval: Duration::from_millis(30),
                ping_timeout: Duration::from_millis(100),
                stream_queue: 16,
            },
        )
        .await
        .unwrap();
        let reply = session.request("server.getConfig", serde_json::json!({})).await.unwrap();
        assert_eq!(reply["big"].as_str().map(str::len), Some(256));
        server.await.unwrap();
    }

    #[test]
    fn slow_rpc_filtering_and_thresholds_match_web() {
        assert!(!should_track_rpc_ack("orchestration.subscribeShell"));
        assert!(!should_track_rpc_ack("subscribeServerConfig"));
        assert!(!should_track_rpc_ack("pullRequests.list"));
        assert!(!should_track_rpc_ack("pullRequests.linkedThreads"));
        assert!(!should_track_rpc_ack("server.getUsageSummary"));
        assert!(!should_track_rpc_ack("server.reportClientActivity"));
        assert!(!should_track_rpc_ack("sourceControl.signInWait"));
        assert!(should_track_rpc_ack("orchestration.dispatchCommand"));
        assert!(should_track_rpc_ack("projects.mutate"));
        assert!(should_track_rpc_ack("vcs.refreshStatus"));

        assert_eq!(rpc_ack_threshold_for("projects.mutate", SLOW_RPC_ACK_THRESHOLD), SLOW_RPC_ACK_THRESHOLD);
        assert_eq!(rpc_ack_threshold_for("server.updateProvider", SLOW_RPC_ACK_THRESHOLD), LONG_RUNNING_RPC_ACK_THRESHOLD);
        assert_eq!(rpc_ack_threshold_for("server.refreshProviders", SLOW_RPC_ACK_THRESHOLD), LONG_RUNNING_RPC_ACK_THRESHOLD);
        assert_eq!(rpc_ack_threshold_for("server.updateServer", SLOW_RPC_ACK_THRESHOLD), LONG_RUNNING_RPC_ACK_THRESHOLD);
        assert_eq!(rpc_ack_threshold_for("vcs.refreshStatus", SLOW_RPC_ACK_THRESHOLD), LONG_RUNNING_RPC_ACK_THRESHOLD);
    }

    #[tokio::test]
    async fn slow_rpc_tracker_promotes_past_threshold_and_dismisses_on_drop() {
        let tracker = SlowRpcTracker::new(Duration::from_millis(25));
        let mut rx = tracker.subscribe();
        assert!(rx.borrow().is_empty());

        // Untracked method returns None.
        assert!(tracker.track("u1".into(), "pullRequests.list", "pullRequests.list".into()).is_none());

        // Fast request dropped before threshold never becomes slow.
        let fast = tracker.track("r-fast".into(), "projects.mutate", "projects.mutate · Mac".into()).unwrap();
        drop(fast);

        // Slow request stays pending past threshold and publishes.
        let slow = tracker.track("r-slow".into(), "orchestration.dispatchCommand", "orchestration.dispatchCommand · Mac".into()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), rx.changed()).await.unwrap().unwrap();
        let list = rx.borrow().clone();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].request_id, "r-slow");
        assert_eq!(list[0].tag, "orchestration.dispatchCommand · Mac");
        assert_eq!(list[0].threshold_ms, 25);

        // Dropping the guard acknowledges and clears the slow request.
        drop(slow);
        tokio::time::timeout(Duration::from_secs(2), rx.changed()).await.unwrap().unwrap();
        assert!(rx.borrow().is_empty());
    }
}
