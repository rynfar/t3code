//! One Effect RPC session over one WebSocket. The protocol never reopens its socket; the
//! supervisor makes a new session on disconnect. Streams ack every chunk (the server waits for
//! acks before sending more), and a silent socket is dropped after `PING_TIMEOUT`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use t3_core::rpc::{ClientMessage, RequestId, RpcFailure, ServerMessage, decode_server_frame, encode_client_message};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

const PING_INTERVAL: Duration = Duration::from_secs(5);
const PING_TIMEOUT: Duration = Duration::from_secs(15);
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
const STREAM_QUEUE: usize = 256;

#[derive(Debug, Clone, thiserror::Error)]
pub enum RpcError {
    #[error(transparent)]
    Failure(#[from] RpcFailure),
    #[error("connection closed")]
    Closed,
}

enum Pending {
    Unary(oneshot::Sender<Result<Value, RpcError>>),
    Stream(mpsc::Sender<Result<Value, RpcError>>),
}

#[derive(Clone)]
pub struct Session {
    outbound: mpsc::UnboundedSender<ClientMessage>,
    pending: PendingMap,
    next_id: Arc<AtomicU64>,
    closed: watch::Receiver<Option<String>>,
}

type PendingMap = Arc<Mutex<HashMap<RequestId, Pending>>>;

fn lock(pending: &Mutex<HashMap<RequestId, Pending>>) -> MutexGuard<'_, HashMap<RequestId, Pending>> {
    pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let (socket, _) = tokio::time::timeout(OPEN_TIMEOUT, tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| "Timed out opening the WebSocket.".to_owned())?
            .map_err(describe_ws_error)?;
        let (mut sink, mut stream) = socket.split();
        let (outbound, mut outbound_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (closed_tx, closed) = watch::channel(None);

        let reader_pending = pending.clone();
        let reader_outbound = outbound.clone();
        tokio::spawn(async move {
            let mut last_frame = Instant::now();
            let mut ping = tokio::time::interval(PING_INTERVAL);
            let reason = loop {
                tokio::select! {
                    frame = stream.next() => {
                        let Some(frame) = frame else { break "Disconnected.".to_owned() };
                        let text = match frame {
                            Ok(Message::Text(text)) => text,
                            Ok(Message::Binary(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
                            Ok(Message::Close(_)) => break "Server closed the connection.".to_owned(),
                            Ok(_) => { last_frame = Instant::now(); continue }
                            Err(error) => break describe_ws_error(error),
                        };
                        last_frame = Instant::now();
                        match decode_server_frame(&text) {
                            Ok(messages) => {
                                for message in messages {
                                    route(message, &reader_pending, &reader_outbound).await;
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
            let entries: Vec<Pending> = lock(&reader_pending).drain().map(|(_, entry)| entry).collect();
            for entry in entries {
                match entry {
                    Pending::Unary(tx) => {
                        let _ = tx.send(Err(RpcError::Closed));
                    }
                    Pending::Stream(tx) => {
                        let _ = tx.try_send(Err(RpcError::Closed));
                    }
                }
            }
            let _ = closed_tx.send(Some(reason));
        });

        Ok(Session { outbound, pending, next_id: Arc::new(AtomicU64::new(1)), closed })
    }

    fn allocate(&self) -> RequestId {
        self.next_id.fetch_add(1, Ordering::Relaxed).into()
    }

    fn send_request(&self, id: RequestId, tag: &str, payload: Value) -> Result<(), RpcError> {
        self.outbound
            .send(ClientMessage::Request { id, tag: tag.to_owned(), payload, headers: Vec::new() })
            .map_err(|_| RpcError::Closed)
    }

    pub async fn request(&self, tag: &str, payload: Value) -> Result<Value, RpcError> {
        let id = self.allocate();
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id.clone(), Pending::Unary(tx));
        self.send_request(id, tag, payload)?;
        rx.await.unwrap_or(Err(RpcError::Closed))
    }

    pub fn stream(&self, tag: &str, payload: Value) -> Result<StreamHandle, RpcError> {
        let id = self.allocate();
        let (tx, receiver) = mpsc::channel(STREAM_QUEUE);
        lock(&self.pending).insert(id.clone(), Pending::Stream(tx));
        self.send_request(id.clone(), tag, payload)?;
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

async fn route(
    message: ServerMessage,
    pending: &Mutex<HashMap<RequestId, Pending>>,
    outbound: &mpsc::UnboundedSender<ClientMessage>,
) {
    match message {
        ServerMessage::Chunk { request_id, values } => {
            let sender = match lock(pending).get(&request_id) {
                Some(Pending::Stream(tx)) => tx.clone(),
                _ => return,
            };
            for value in values {
                // Back-pressure: a slow consumer slows the socket reader instead of growing memory.
                if sender.send(Ok(value)).await.is_err() {
                    return;
                }
            }
            let _ = outbound.send(ClientMessage::Ack { request_id });
        }
        ServerMessage::Exit { request_id, exit } => {
            let entry = lock(pending).remove(&request_id);
            match (entry, exit.into_result()) {
                (Some(Pending::Unary(tx)), result) => {
                    let _ = tx.send(result.map_err(RpcError::from));
                }
                (Some(Pending::Stream(tx)), Err(failure)) => {
                    let _ = tx.send(Err(RpcError::Failure(failure))).await;
                }
                _ => {}
            }
        }
        ServerMessage::Defect { defect } => tracing::warn!(%defect, "server defect"),
        ServerMessage::ClientProtocolError { error } => tracing::warn!(%error, "client protocol error"),
        ServerMessage::Pong | ServerMessage::Unknown => {}
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
