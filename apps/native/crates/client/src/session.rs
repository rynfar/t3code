//! One Effect RPC session over one WebSocket. The protocol never reopens its socket; the
//! supervisor makes a new session on disconnect.
//!
//! Flow control mirrors `RpcClient`: a chunk is acked only after the consumer took its values,
//! and the server waits for acks. Each stream has its own in-order delivery task, so the socket
//! reader never waits on a consumer and one slow stream cannot stall requests, other streams, or
//! heartbeats.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use t3_core::rpc::{ClientMessage, RequestId, RpcFailure, ServerMessage, decode_server_frame, encode_client_message};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Thread subscriptions ask for bounded snapshots, but older servers and other streams can still
/// send whole projections.
const MAX_MESSAGE_BYTES: usize = 256 << 20;

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
}

pub struct StreamHandle {
    pub receiver: mpsc::Receiver<Result<Value, RpcError>>,
    id: RequestId,
    outbound: mpsc::UnboundedSender<ClientMessage>,
    pending: PendingMap,
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

impl Session {
    pub async fn connect(url: &str) -> Result<Session, String> {
        Self::connect_with(url, TIMING).await
    }

    async fn connect_with(url: &str, timing: Timing) -> Result<Session, String> {
        let config = WebSocketConfig {
            max_message_size: Some(MAX_MESSAGE_BYTES),
            max_frame_size: Some(MAX_MESSAGE_BYTES),
            ..Default::default()
        };
        let connect = tokio_tungstenite::connect_async_with_config(url, Some(config), false);
        let (socket, _) = tokio::time::timeout(OPEN_TIMEOUT, connect)
            .await
            .map_err(|_| "Timed out opening the WebSocket.".to_owned())?
            .map_err(describe_ws_error)?;
        let (mut sink, mut stream) = socket.split();
        let (outbound, mut outbound_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let pending: PendingMap = Arc::new(Mutex::new(Some(HashMap::new())));
        let (closed_tx, closed) = watch::channel(None);

        // Acks go through a weak handle so the task does not keep its own session alive: when
        // every `Session` and `StreamHandle` is gone, `outbound_rx` closes and the socket shuts.
        let task_pending = pending.clone();
        tokio::spawn(async move {
            let mut last_frame = Instant::now();
            let mut ping = tokio::time::interval(timing.ping_interval);
            let reason = loop {
                tokio::select! {
                    frame = stream.next() => {
                        let text = match frame {
                            None => break "Disconnected.".into(),
                            Some(Ok(Message::Text(text))) => text,
                            Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                            Some(Ok(Message::Close(_))) => break "Server closed the connection.".into(),
                            Some(Ok(_)) => { last_frame = Instant::now(); continue }
                            Some(Err(error)) => break describe_ws_error(error).into(),
                        };
                        last_frame = Instant::now();
                        match decode_server_frame(&text) {
                            Ok(messages) => {
                                if let Some(end) = messages.into_iter().find_map(|m| route(m, &task_pending)) {
                                    break end;
                                }
                            }
                            Err(error) => tracing::warn!(%error, "undecodable server frame"),
                        }
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

        Ok(Session { outbound, pending, next_id: Arc::new(AtomicU64::new(1)), closed, stream_queue: timing.stream_queue })
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
        let (tx, rx) = oneshot::channel();
        self.start(tag, payload, Pending::Unary(tx))?;
        rx.await.unwrap_or(Err(RpcError::Closed))
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
        Ok(StreamHandle { receiver, id, outbound: self.outbound.clone(), pending: self.pending.clone() })
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

fn describe_ws_error(error: tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Http(response) => match response.status().as_u16() {
            401 | 403 => "This device is no longer authorized. Pair again.".to_owned(),
            status => format!("The server refused the WebSocket ({status})."),
        },
        Error::Io(error) => format!("Could not connect: {error}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests;
