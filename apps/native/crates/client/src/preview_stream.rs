//! One viewed preview tab's socket, run as `createPreviewStreamClient` runs it: mint a `media`
//! ticket, connect to `/api/preview-stream/ws`, acknowledge every JPEG frame, decode the JSON
//! messages, track and withdraw offered file pickers on drop or stop (`pingdotgg/t3code#29655`),
//! send input only while this viewer controls the tab, and reconnect with backoff until the tab
//! is gone, the host needs setup, or the upgrade keeps being refused.
//!
//! Frames land in a `watch` channel, so a slow viewer only ever sees the newest one and nothing
//! queues behind it; the server already holds back all but one frame per acknowledgement.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use t3_core::pairing::websocket_base;
use t3_core::preview::stream::{
    ACK_MESSAGE, CloseAction, Control, FileChooserOffer, FileChooserTracker, HostSetup, MAX_UNAUTHORIZED_REFUSALS, STREAM_PATH, StreamInput,
    StreamMessage, UploadFile, classify_close, download_url, encode_multipart_files, may_send, reconnect_delay_ms, refusal_delay_ms, upload_url,
};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::auth::{AuthError, Credentials, issue_media_ticket};

const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// The server disconnects a viewer whose socket buffers more than 8 MiB.
const MAX_MESSAGE_BYTES: usize = 16 << 20;
const MAX_DOWNLOAD_BYTES: usize = 100 << 20;

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
    /// Current file picker offered by the page, or `None` when closed or withdrawn on drop/stop
    /// (`pingdotgg/t3code#29655`).
    FileChooser(Option<FileChooserOffer>),
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
    open_with_mode(runtime, credentials, target, true)
}

pub(crate) fn open_read_only(runtime: &tokio::runtime::Handle, credentials: Arc<Credentials>, target: StreamTarget) -> PreviewStream {
    open_with_mode(runtime, credentials, target, false)
}

fn open_with_mode(runtime: &tokio::runtime::Handle, credentials: Arc<Credentials>, target: StreamTarget, interactive: bool) -> PreviewStream {
    let (frames_tx, frames) = watch::channel(None);
    let (events_tx, events) = mpsc::unbounded_channel();
    let (input, input_rx) = mpsc::unbounded_channel();
    runtime.spawn(run(credentials, target, interactive, frames_tx, events_tx, input_rx));
    PreviewStream { input: StreamSender(input), frames, events }
}

fn stream_url(credentials: &Credentials, target: &StreamTarget, ticket: &str, interactive: bool) -> String {
    let base = websocket_base(&credentials.http_base);
    let mut url = url::Url::parse(&format!("{}{STREAM_PATH}", base.trim_end_matches("/ws"))).expect("valid preview stream url");
    url.query_pairs_mut()
        .append_pair("threadId", &target.thread_id)
        .append_pair("tabId", &target.tab_id)
        .append_pair("maxWidth", &target.max_width.max(1).to_string())
        .append_pair("maxHeight", &target.max_height.max(1).to_string())
        .append_pair("wsTicket", ticket);
    if !interactive {
        url.query_pairs_mut().append_pair("interactive", "false");
    }
    url.to_string()
}

/// Uploads files to answer an offered preview file picker (`uploadPreviewStreamFiles`).
/// Passing an empty slice cancels the page's picker.
pub async fn upload_file_chooser(
    credentials: &Credentials,
    thread_id: &str,
    tab_id: &str,
    chooser_id: &str,
    files: &[UploadFile],
) -> Result<(), String> {
    let ticket = issue_media_ticket(credentials).await.map_err(|err| err.to_string())?;
    let url = upload_url(&credentials.http_base, thread_id, tab_id, chooser_id, Some(&ticket));
    let boundary = format!("t3-preview-{}", uuid::Uuid::new_v4().simple());
    let body = encode_multipart_files(&boundary, files);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|err| err.to_string())?;
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let response = crate::auth::send_authed_request(credentials, "POST", &url, || {
        client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, &content_type)
            .body(body.clone())
    })
    .await
    .map_err(|err| match err {
        crate::auth::AuthedHttpError::Auth(e) => e.to_string(),
        crate::auth::AuthedHttpError::Http(e) => e.to_string(),
    })?;
    if !response.status().is_success() {
        let message = response.text().await.unwrap_or_default();
        return Err(if message.trim().is_empty() { "The upload was refused.".to_owned() } else { message });
    }
    Ok(())
}

/// Fetches a preview download artifact from `/api/preview-stream/download`.
pub async fn download_file(
    credentials: &Credentials,
    thread_id: &str,
    tab_id: &str,
    download_id: &str,
) -> Result<Vec<u8>, String> {
    let ticket = issue_media_ticket(credentials).await.map_err(|err| err.to_string())?;
    let url = download_url(&credentials.http_base, thread_id, tab_id, download_id, Some(&ticket));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|err| err.to_string())?;
    let response = crate::auth::send_authed_request(credentials, "GET", &url, || client.get(&url))
        .await
        .map_err(|err| match err {
            crate::auth::AuthedHttpError::Auth(e) => e.to_string(),
            crate::auth::AuthedHttpError::Http(e) => e.to_string(),
        })?;
    let status = response.status();
    if !status.is_success() {
        let message = response.text().await.unwrap_or_default();
        return Err(if message.trim().is_empty() {
            format!("Download failed ({}).", status.as_u16())
        } else {
            message
        });
    }
    if response.content_length().is_some_and(|len| len as usize > MAX_DOWNLOAD_BYTES) {
        return Err("Downloaded file exceeds the 100 MiB limit.".to_owned());
    }
    let bytes = response.bytes().await.map_err(|err| err.to_string())?;
    if bytes.len() > MAX_DOWNLOAD_BYTES {
        return Err("Downloaded file exceeds the 100 MiB limit.".to_owned());
    }
    Ok(bytes.to_vec())
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
    interactive: bool,
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
        let url = stream_url(&credentials, &target, &ticket, interactive);
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

async fn pump<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
    frames: &watch::Sender<Option<Frame>>,
    events: &mpsc::UnboundedSender<StreamEvent>,
    input: &mut mpsc::UnboundedReceiver<StreamInput>,
    failures: &mut u32,
    refusals: &mut u32,
) -> Ended
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut sink, mut stream) = socket.split();
    let mut control: Option<Control> = None;
    let mut file_chooser = FileChooserTracker::default();
    let withdraw = |tracker: &mut FileChooserTracker| {
        if tracker.withdraw() {
            let _ = events.send(StreamEvent::FileChooser(None));
        }
    };
    loop {
        tokio::select! {
            message = stream.next() => match message {
                Some(Ok(Message::Binary(bytes))) => {
                    *failures = 0;
                    *refusals = 0;
                    if sink.send(Message::Text(ACK_MESSAGE.to_owned())).await.is_err() {
                        withdraw(&mut file_chooser);
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
                    if let Some(next_chooser) = file_chooser.on_message(&message)
                        && events.send(StreamEvent::FileChooser(next_chooser)).is_err()
                    {
                        return Ended::Stopped;
                    }
                    if events.send(StreamEvent::Message(message)).is_err() {
                        return Ended::Stopped;
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    let (code, reason) = frame.map_or((None, String::new()), |frame| (Some(u16::from(frame.code)), frame.reason.into_owned()));
                    withdraw(&mut file_chooser);
                    return Ended::Closed(classify_close(code, &reason, true));
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => {
                    withdraw(&mut file_chooser);
                    return Ended::Closed(CloseAction::Retry);
                }
            },
            next = input.recv() => match next {
                None => {
                    withdraw(&mut file_chooser);
                    let _ = sink.close().await;
                    return Ended::Stopped;
                }
                Some(next) => {
                    if may_send(control.as_ref(), &next) && sink.send(Message::Text(next.encode())).await.is_err() {
                        withdraw(&mut file_chooser);
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
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    #[test]
    fn stream_url_carries_the_target_and_ticket() {
        let credentials = Credentials { http_base: "https://host.example:3773".into(), access_token: "t".into() };
        let target = StreamTarget { thread_id: "thread 1".into(), tab_id: "tab".into(), max_width: 1080, max_height: 0 };
        assert_eq!(
            stream_url(&credentials, &target, "abc", true),
            "wss://host.example:3773/api/preview-stream/ws?threadId=thread+1&tabId=tab&maxWidth=1080&maxHeight=1&wsTicket=abc"
        );
    }

    #[test]
    fn read_only_stream_url_disables_interaction_and_keeps_the_media_ticket() {
        let credentials = Credentials { http_base: "http://127.0.0.1:3773".into(), access_token: "t".into() };
        let target = StreamTarget { thread_id: "thread+1".into(), tab_id: "tab/1".into(), max_width: 0, max_height: 720 };
        let url = stream_url(&credentials, &target, "ticket&1", false);
        assert_eq!(
            url,
            "ws://127.0.0.1:3773/api/preview-stream/ws?threadId=thread%2B1&tabId=tab%2F1&maxWidth=1&maxHeight=720&wsTicket=ticket%261&interactive=false"
        );
    }

    async fn connect_ws_pair() -> (
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap()
        });
        let client_tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (client_ws, _) = tokio_tungstenite::client_async(format!("ws://{addr}/ws"), client_tcp).await.unwrap();
        let server_ws = server_task.await.unwrap();
        (client_ws, server_ws)
    }

    /// Regression test for `#222` (`pingdotgg/t3code#29655`): a dropped or disposed stream
    /// withdraws its stale file picker, and reconnecting accepts the server's new chooser id.
    #[tokio::test]
    async fn dropped_and_stopped_streams_withdraw_stale_file_picker_and_reconnect_accepts_new_id() {
        // 1. Socket drop (close 1012) withdraws chooser c1.
        let (client_ws, mut server_ws) = connect_ws_pair().await;
        let (frames_tx, _frames_rx) = watch::channel(None);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let (_input_tx, mut input_rx) = mpsc::unbounded_channel();
        let pump_task = tokio::spawn(async move {
            let (mut failures, mut refusals) = (0, 0);
            pump(client_ws, &frames_tx, &events_tx, &mut input_rx, &mut failures, &mut refusals).await
        });

        server_ws
            .send(Message::Text(r#"{"type":"fileChooser","id":"c1","multiple":false,"accept":".csv"}"#.into()))
            .await
            .unwrap();
        // Mismatched close id must be ignored.
        server_ws
            .send(Message::Text(r#"{"type":"fileChooserClosed","id":"stale"}"#.into()))
            .await
            .unwrap();
        server_ws
            .send(Message::Close(Some(CloseFrame { code: CloseCode::Restart, reason: "reconnect".into() })))
            .await
            .unwrap();

        let ended = pump_task.await.unwrap();
        assert!(matches!(ended, Ended::Closed(CloseAction::Retry)));

        let mut chooser_events = Vec::new();
        while let Ok(event) = events_rx.try_recv() {
            if let StreamEvent::FileChooser(offer) = event {
                chooser_events.push(offer);
            }
        }
        assert_eq!(
            chooser_events,
            vec![
                Some(FileChooserOffer { id: "c1".into(), multiple: false, accept: ".csv".into() }),
                None,
            ]
        );

        // 2. Reconnect offers new chooser id c2, and dropping/stopping the stream withdraws it.
        let (client_ws2, mut server_ws2) = connect_ws_pair().await;
        let (frames_tx2, _frames_rx2) = watch::channel(None);
        let (events_tx2, mut events_rx2) = mpsc::unbounded_channel();
        let (input_tx2, mut input_rx2) = mpsc::unbounded_channel();
        let pump_task2 = tokio::spawn(async move {
            let (mut failures, mut refusals) = (0, 0);
            pump(client_ws2, &frames_tx2, &events_tx2, &mut input_rx2, &mut failures, &mut refusals).await
        });

        server_ws2
            .send(Message::Text(r#"{"type":"fileChooser","id":"c2","multiple":true,"accept":""}"#.into()))
            .await
            .unwrap();
        // Wait until c2 has been observed, then drop the input sender (disposing the stream).
        let first = loop {
            match events_rx2.recv().await.unwrap() {
                StreamEvent::FileChooser(offer) => break offer,
                _ => continue,
            }
        };
        assert_eq!(first, Some(FileChooserOffer { id: "c2".into(), multiple: true, accept: "".into() }));
        drop(input_tx2);

        let ended2 = pump_task2.await.unwrap();
        assert!(matches!(ended2, Ended::Stopped));

        let mut remaining = Vec::new();
        while let Ok(event) = events_rx2.try_recv() {
            if let StreamEvent::FileChooser(offer) = event {
                remaining.push(offer);
            }
        }
        assert_eq!(remaining, vec![None]);
    }
}
