//! One viewed preview tab's socket, run as `createPreviewStreamClient` runs it: mint a `media`
//! ticket, connect to `/api/preview-stream/ws`, acknowledge every JPEG frame, decode the JSON
//! messages, send input only while this viewer controls the tab, and reconnect with backoff
//! until the tab is gone, the host needs setup, or the upgrade keeps being refused.
//!
//! Frames land in a `watch` channel, so a slow viewer only ever sees the newest one and nothing
//! queues behind it; the server already holds back all but one frame per acknowledgement.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use t3_core::pairing::websocket_base;
use t3_core::preview::stream::{
    ACK_MESSAGE, CloseAction, Control, HostSetup, MAX_UNAUTHORIZED_REFUSALS, STREAM_PATH, StreamInput, StreamMessage, classify_close, may_send,
    reconnect_delay_ms, refusal_delay_ms,
};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::auth::{AuthError, Credentials, issue_media_ticket};

const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// The server disconnects a viewer whose socket buffers more than 8 MiB.
const MAX_MESSAGE_BYTES: usize = 16 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTarget {
    pub thread_id: String,
    pub tab_id: String,
    /// The viewer's backing size in device px; the server never sends larger frames.
    pub max_width: u32,
    pub max_height: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Connected(bool),
    Message(StreamMessage),
    /// The tab closed; the stream stopped.
    Gone,
    HostSetup(HostSetup),
    /// Refused `MAX_UNAUTHORIZED_REFUSALS` times in a row; the stream stopped.
    Refused,
}

pub type Frame = Arc<[u8]>;

/// Dropping its `input` closes the socket.
pub struct PreviewStream {
    pub input: StreamSender,
    pub frames: watch::Receiver<Option<Frame>>,
    pub events: mpsc::UnboundedReceiver<StreamEvent>,
}

#[derive(Clone)]
pub struct StreamSender(mpsc::UnboundedSender<StreamInput>);

impl StreamSender {
    /// Queues input; the stream drops it unless this viewer may send it when it goes out.
    pub fn send(&self, input: StreamInput) {
        let _ = self.0.send(input);
    }
}

pub fn open(runtime: &tokio::runtime::Handle, credentials: Arc<Credentials>, target: StreamTarget) -> PreviewStream {
    let (frames_tx, frames) = watch::channel(None);
    let (events_tx, events) = mpsc::unbounded_channel();
    let (input, input_rx) = mpsc::unbounded_channel();
    runtime.spawn(run(credentials, target, frames_tx, events_tx, input_rx));
    PreviewStream { input: StreamSender(input), frames, events }
}

pub fn stream_url(credentials: &Credentials, target: &StreamTarget, ticket: &str) -> String {
    let base = websocket_base(&credentials.http_base);
    let mut url = url::Url::parse(&format!("{}{STREAM_PATH}", base.trim_end_matches("/ws"))).expect("valid preview stream url");
    url.query_pairs_mut()
        .append_pair("threadId", &target.thread_id)
        .append_pair("tabId", &target.tab_id)
        .append_pair("maxWidth", &target.max_width.max(1).to_string())
        .append_pair("maxHeight", &target.max_height.max(1).to_string())
        .append_pair("wsTicket", ticket);
    url.to_string()
}

enum Ended {
    Closed(CloseAction),
    /// The viewer dropped its handle.
    Stopped,
}

/// Waits `ms` unless the viewer goes away first.
async fn pause(ms: u64, input: &mut mpsc::UnboundedReceiver<StreamInput>) -> bool {
    let sleep = tokio::time::sleep(Duration::from_millis(ms));
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            () = &mut sleep => return true,
            next = input.recv() => if next.is_none() { return false },
        }
    }
}

/// Input before a connection opens has no controller to reach; it is dropped.
async fn until_closed(input: &mut mpsc::UnboundedReceiver<StreamInput>) {
    while input.recv().await.is_some() {}
}

async fn run(
    credentials: Arc<Credentials>,
    target: StreamTarget,
    frames: watch::Sender<Option<Frame>>,
    events: mpsc::UnboundedSender<StreamEvent>,
    mut input: mpsc::UnboundedReceiver<StreamInput>,
) {
    let mut failures = 0u32;
    let mut refusals = 0u32;
    let config = WebSocketConfig { max_message_size: Some(MAX_MESSAGE_BYTES), max_frame_size: Some(MAX_MESSAGE_BYTES), ..Default::default() };
    loop {
        if input.is_closed() {
            return;
        }
        let refused = |refusals: &mut u32| {
            *refusals += 1;
            (*refusals < MAX_UNAUTHORIZED_REFUSALS).then(|| refusal_delay_ms(*refusals))
        };
        let ticket = match issue_media_ticket(&credentials).await {
            Ok(ticket) => ticket,
            Err(AuthError::Unauthorized) => match refused(&mut refusals) {
                Some(delay) if pause(delay, &mut input).await => continue,
                Some(_) => return,
                None => {
                    let _ = events.send(StreamEvent::Refused);
                    return;
                }
            },
            Err(error) => {
                tracing::debug!(%error, "preview stream ticket failed");
                if pause(reconnect_delay_ms(failures), &mut input).await {
                    failures += 1;
                    continue;
                }
                return;
            }
        };
        let url = stream_url(&credentials, &target, &ticket);
        let connect = tokio_tungstenite::connect_async_with_config(url, Some(config), false);
        let socket = tokio::select! {
            result = tokio::time::timeout(OPEN_TIMEOUT, connect) => result,
            () = until_closed(&mut input) => return,
        };
        let socket = match socket {
            Ok(Ok((socket, _))) => socket,
            Ok(Err(WsError::Http(response))) if matches!(response.status().as_u16(), 401 | 403) => match refused(&mut refusals) {
                Some(delay) if pause(delay, &mut input).await => continue,
                Some(_) => return,
                None => {
                    let _ = events.send(StreamEvent::Refused);
                    return;
                }
            },
            other => {
                if let Ok(Err(error)) = other {
                    tracing::debug!(%error, "preview stream connect failed");
                }
                if pause(reconnect_delay_ms(failures), &mut input).await {
                    failures += 1;
                    continue;
                }
                return;
            }
        };
        let _ = events.send(StreamEvent::Connected(true));
        let ended = pump(socket, &frames, &events, &mut input, &mut failures, &mut refusals).await;
        let _ = events.send(StreamEvent::Connected(false));
        let action = match ended {
            Ended::Stopped => return,
            Ended::Closed(action) => action,
        };
        match action {
            CloseAction::Gone => {
                let _ = events.send(StreamEvent::Gone);
                return;
            }
            CloseAction::HostSetup(setup) => {
                let _ = events.send(StreamEvent::HostSetup(setup));
                return;
            }
            CloseAction::Unauthorized => match refused(&mut refusals) {
                Some(delay) if pause(delay, &mut input).await => {}
                Some(_) => return,
                None => {
                    let _ = events.send(StreamEvent::Refused);
                    return;
                }
            },
            CloseAction::Retry => {
                if !pause(reconnect_delay_ms(failures), &mut input).await {
                    return;
                }
                failures += 1;
            }
        }
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn pump(
    socket: Socket,
    frames: &watch::Sender<Option<Frame>>,
    events: &mpsc::UnboundedSender<StreamEvent>,
    input: &mut mpsc::UnboundedReceiver<StreamInput>,
    failures: &mut u32,
    refusals: &mut u32,
) -> Ended {
    let (mut sink, mut stream) = socket.split();
    let mut control: Option<Control> = None;
    loop {
        tokio::select! {
            message = stream.next() => match message {
                Some(Ok(Message::Binary(bytes))) => {
                    *failures = 0;
                    *refusals = 0;
                    if sink.send(Message::Text(ACK_MESSAGE.to_owned())).await.is_err() {
                        return Ended::Closed(CloseAction::Retry);
                    }
                    frames.send_replace(Some(Frame::from(bytes)));
                }
                Some(Ok(Message::Text(text))) => {
                    let message = StreamMessage::decode(&text);
                    match &message {
                        StreamMessage::Unknown => continue,
                        StreamMessage::Viewport(_) => *failures = 0,
                        StreamMessage::Control(next) => control = Some(next.clone()),
                        _ => {}
                    }
                    if events.send(StreamEvent::Message(message)).is_err() {
                        return Ended::Stopped;
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    let (code, reason) = frame.map_or((None, String::new()), |frame| (Some(u16::from(frame.code)), frame.reason.into_owned()));
                    return Ended::Closed(classify_close(code, &reason, true));
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return Ended::Closed(CloseAction::Retry),
            },
            next = input.recv() => match next {
                None => {
                    let _ = sink.close().await;
                    return Ended::Stopped;
                }
                Some(next) => {
                    if may_send(control.as_ref(), &next) && sink.send(Message::Text(next.encode())).await.is_err() {
                        return Ended::Closed(CloseAction::Retry);
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_url_carries_the_target_and_ticket() {
        let credentials = Credentials { http_base: "https://host.example:3773".into(), access_token: "t".into() };
        let target = StreamTarget { thread_id: "thread 1".into(), tab_id: "tab".into(), max_width: 1080, max_height: 0 };
        assert_eq!(
            stream_url(&credentials, &target, "abc"),
            "wss://host.example:3773/api/preview-stream/ws?threadId=thread+1&tabId=tab&maxWidth=1080&maxHeight=1&wsTicket=abc"
        );
    }
}
