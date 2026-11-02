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

const PING_INTERVAL: Duration = Duration::from_secs(5);
const PING_TIMEOUT: Duration = Duration::from_secs(15);
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
const STREAM_QUEUE: usize = 256;
/// Full thread snapshots can be large until bounded snapshots land (ledger TV-HISTORY).
const MAX_MESSAGE_BYTES: usize = 256 << 20;

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
}

pub struct StreamHandle {
    pub receiver: mpsc::Receiver<Result<Value, RpcError>>,
    id: RequestId,
    outbound: mpsc::UnboundedSender<ClientMessage>,
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        let _ = self.outbound.send(ClientMessage::Interrupt { request_id: self.id.clone() });
    }
}

impl Session {
    pub async fn connect(url: &str) -> Result<Session, String> {
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
            let mut ping = tokio::time::interval(PING_INTERVAL);
            let reason = loop {
                tokio::select! {
                    frame = stream.next() => {
                        let text = match frame {
                            None => break "Disconnected.".to_owned(),
                            Some(Ok(Message::Text(text))) => text,
                            Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                            Some(Ok(Message::Close(_))) => break "Server closed the connection.".to_owned(),
                            Some(Ok(_)) => { last_frame = Instant::now(); continue }
                            Some(Err(error)) => break describe_ws_error(error),
                        };
                        last_frame = Instant::now();
                        match decode_server_frame(&text) {
                            Ok(messages) => {
                                if let Some(reason) = messages.into_iter().find_map(|m| route(m, &task_pending)) {
                                    break reason;
                                }
                            }
                            Err(error) => tracing::warn!(%error, "undecodable server frame"),
                        }
                    }
                    outgoing = outbound_rx.recv() => {
                        let Some(message) = outgoing else { break "Session dropped.".to_owned() };
                        if let Err(error) = sink.send(Message::Text(encode_client_message(&message))).await {
                            break describe_ws_error(error);
                        }
                    }
                    _ = ping.tick() => {
                        if last_frame.elapsed() > PING_TIMEOUT {
                            break format!("The server stopped responding for {}s.", PING_TIMEOUT.as_secs());
                        }
                        if sink.send(Message::Text(encode_client_message(&ClientMessage::Ping))).await.is_err() {
                            break "Disconnected.".to_owned();
                        }
                    }
                }
            };
            let _ = sink.close().await;
            let entries = lock(&task_pending).take().unwrap_or_default();
            for (_, entry) in entries {
                match entry {
                    Pending::Unary(tx) => {
                        let _ = tx.send(Err(RpcError::Closed));
                    }
                    Pending::Stream(tx) => {
                        let _ = tx.send(Delivery::Fail(RpcError::Closed));
                    }
                }
            }
            let _ = closed_tx.send(Some(reason));
        });

        Ok(Session { outbound, pending, next_id: Arc::new(AtomicU64::new(1)), closed })
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
        let (consumer, receiver) = mpsc::channel(STREAM_QUEUE);
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
        Ok(StreamHandle { receiver, id, outbound: self.outbound.clone() })
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

/// Routes one server message. Returns a reason when the whole connection must close.
fn route(message: ServerMessage, pending: &PendingMap) -> Option<String> {
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
        // Both are connection-wide in `RpcClient`: every pending call fails.
        ServerMessage::Defect { defect } => Some(format!("Server error: {defect}")),
        ServerMessage::ClientProtocolError { error } => Some(format!("Protocol error: {error}")),
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
