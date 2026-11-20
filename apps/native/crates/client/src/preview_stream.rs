//! One viewed preview tab's socket, run as `createPreviewStreamClient` runs it: mint a `media`
//! ticket, connect to `/api/preview-stream/ws`, acknowledge every JPEG frame, decode the JSON
//! messages, track and withdraw offered file pickers on drop or stop (`pingdotgg/t3code#29655`),
//! send input only while this viewer controls the tab, and reconnect with backoff until the tab
//! is gone, the host needs setup, or the upgrade keeps being refused.
//!
//! Frames land in a `watch` channel, so a slow viewer only ever sees the newest one and nothing
//! queues behind it; the server already holds back all but one frame per acknowledgement.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use t3_core::pairing::websocket_base;
use t3_core::preview::stream::{
    ACK_MESSAGE, CloseAction, Control, FileChooserOffer, FileChooserTracker, HostSetup, MAX_UNAUTHORIZED_REFUSALS,
    SOCKS_REPLY_FAILURE, SOCKS_REPLY_SUCCEEDED, STREAM_PATH, Socks5Connect, StreamInput, StreamMessage,
    TICKET_REFRESH_MARGIN_MS, UploadFile, classify_close, download_url, encode_multipart_files, is_tunnel_endpoint,
    may_send, parse_socks5_connect, preview_tunnel_url, reconnect_delay_ms, refusal_delay_ms, socks5_reply,
    tunnel_endpoint, upload_url,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, watch};
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

/// Builds the environment's `/api/preview/tunnel` WebSocket URL (`pingdotgg/t3code#37948`).
pub fn tunnel_url(credentials: &Credentials, ticket: &str, port: Option<u16>) -> Option<String> {
    preview_tunnel_url(&credentials.http_base, ticket, port)
}

const TUNNEL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default reuse window for cached media tickets (server tickets live >= 60s; refresh 30s early).
const TUNNEL_TICKET_TTL: Duration = Duration::from_secs(60);

/// Local SOCKS5 listener forwarding loopback connections over an environment's
/// `/api/preview/tunnel` WebSocket (`apps/desktop/src/preview/LoopbackTunnel.ts`,
/// `apps/web/src/state/previewTunnel.ts`, `pingdotgg/t3code#37948`).
pub struct LoopbackTunnel {
    addr: SocketAddr,
    accept_task: tokio::task::JoinHandle<()>,
}

impl Drop for LoopbackTunnel {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

impl LoopbackTunnel {
    /// Starts a SOCKS5 loopback tunnel backed by `credentials`, minting and caching `media`
    /// tickets via `issue_media_ticket`.
    pub async fn start(credentials: Arc<Credentials>) -> std::io::Result<Self> {
        let cached: Arc<Mutex<Option<(String, Instant)>>> = Arc::new(Mutex::new(None));
        let resolve = Arc::new(move || {
            let credentials = credentials.clone();
            let cached = cached.clone();
            async move {
                let margin = Duration::from_millis(TICKET_REFRESH_MARGIN_MS);
                if let Some((url, expires_at)) = cached.lock().await.clone()
                    && expires_at.saturating_duration_since(Instant::now()) > margin
                {
                    return Some(url);
                }
                let ticket = issue_media_ticket(&credentials).await.ok()?;
                let url = tunnel_url(&credentials, &ticket, None)?;
                *cached.lock().await = Some((url.clone(), Instant::now() + TUNNEL_TICKET_TTL));
                Some(url)
            }
        });
        Self::start_with_resolver(resolve).await
    }

    /// Starts a SOCKS5 loopback tunnel using an explicit tunnel URL resolver (used by tests and
    /// callers that manage their own media ticket lifecycle).
    pub async fn start_with_resolver<F, Fut>(resolve_url: Arc<F>) -> std::io::Result<Self>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Option<String>> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let accept_task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let resolver = resolve_url.clone();
                tokio::spawn(async move {
                    let _ = handle_socks5_client(socket, resolver).await;
                });
            }
        });
        Ok(LoopbackTunnel { addr, accept_task })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

async fn read_socks5_request(socket: &mut TcpStream) -> std::io::Result<Option<Socks5Connect>> {
    let mut received = Vec::with_capacity(64);
    let mut buf = [0u8; 256];
    let mut greeted = false;
    loop {
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        received.extend_from_slice(&buf[..n]);
        if !greeted {
            if received.len() < 2 {
                continue;
            }
            let methods = usize::from(received[1]);
            if received.len() < 2 + methods {
                continue;
            }
            if received[0] != 5 || !received[2..2 + methods].contains(&0) {
                socket.write_all(&[5, 0xff]).await?;
                return Ok(None);
            }
            socket.write_all(&[5, 0]).await?;
            received.drain(..2 + methods);
            greeted = true;
        }
        if let Some(request) = parse_socks5_connect(&received) {
            return Ok(Some(request));
        }
        if received.len() > 1024 {
            return Ok(None);
        }
    }
}

async fn handle_socks5_client<F, Fut>(mut client: TcpStream, resolve_url: Arc<F>) -> std::io::Result<()>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Option<String>> + Send + 'static,
{
    let Some(request) = read_socks5_request(&mut client).await? else {
        return Ok(());
    };
    let port = match request {
        Socks5Connect::Refusal(code) => {
            let _ = client.write_all(&socks5_reply(code)).await;
            return Ok(());
        }
        Socks5Connect::Port(port) => port,
    };
    let Some(base_tunnel_url) = resolve_url().await else {
        let _ = client.write_all(&socks5_reply(SOCKS_REPLY_FAILURE)).await;
        return Ok(());
    };
    if is_tunnel_endpoint(&base_tunnel_url, port) {
        let Some((host, endpoint_port)) = tunnel_endpoint(&base_tunnel_url) else {
            let _ = client.write_all(&socks5_reply(SOCKS_REPLY_FAILURE)).await;
            return Ok(());
        };
        let Ok(Ok(mut upstream)) =
            tokio::time::timeout(TUNNEL_CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), endpoint_port))).await
        else {
            let _ = client.write_all(&socks5_reply(SOCKS_REPLY_FAILURE)).await;
            return Ok(());
        };
        client.write_all(&socks5_reply(SOCKS_REPLY_SUCCEEDED)).await?;
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        return Ok(());
    }

    let Ok(mut ws_url) = url::Url::parse(&base_tunnel_url) else {
        let _ = client.write_all(&socks5_reply(SOCKS_REPLY_FAILURE)).await;
        return Ok(());
    };
    ws_url.query_pairs_mut().append_pair("port", &port.to_string());
    let config = WebSocketConfig { max_message_size: Some(MAX_MESSAGE_BYTES), max_frame_size: Some(MAX_MESSAGE_BYTES), ..Default::default() };
    let Ok(Ok((ws, _))) = tokio::time::timeout(
        TUNNEL_CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_with_config(ws_url.as_str(), Some(config), false),
    )
    .await
    else {
        let _ = client.write_all(&socks5_reply(SOCKS_REPLY_FAILURE)).await;
        return Ok(());
    };
    client.write_all(&socks5_reply(SOCKS_REPLY_SUCCEEDED)).await?;
    let (mut tcp_read, mut tcp_write) = client.into_split();
    let (mut ws_sink, mut ws_stream) = ws.split();
    let to_ws = async {
        let mut buf = vec![0u8; 16_384];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    let _ = ws_sink.close().await;
                    break;
                }
                Ok(n) => {
                    if ws_sink.send(Message::Binary(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            }
        }
    };
    let from_ws = async {
        while let Some(Ok(msg)) = ws_stream.next().await {
            match msg {
                Message::Binary(bytes) => {
                    if tcp_write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Message::Text(text) => {
                    if tcp_write.write_all(text.as_bytes()).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = tcp_write.shutdown().await;
    };
    tokio::select! {
        () = to_ws => {}
        () = from_ws => {}
    }
    Ok(())
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
            stream_url(&credentials, &target, "abc"),
            "wss://host.example:3773/api/preview-stream/ws?threadId=thread+1&tabId=tab&maxWidth=1080&maxHeight=1&wsTicket=abc"
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

    #[tokio::test]
    async fn loopback_tunnel_forwards_socks5_loopback_over_ws_and_refuses_non_loopback() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Mock T3 server handling POST /api/auth/ws-ticket and WS /api/preview/tunnel.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();
        let (seen_uri_tx, seen_uri_rx) = tokio::sync::oneshot::channel::<String>();
        let server_task = tokio::spawn(async move {
            let mut seen_uri_tx = Some(seen_uri_tx);
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut peek_buf = vec![0u8; 1024];
                let n = stream.peek(&mut peek_buf).await.unwrap();
                let head = String::from_utf8_lossy(&peek_buf[..n]);
                if head.starts_with("POST /api/auth/websocket-ticket/media") {
                    let mut req_buf = vec![0u8; n];
                    stream.read_exact(&mut req_buf).await.unwrap();
                    let body = r#"{"ticket":"tk-tunnel","expiresAt":"2099-01-01T00:00:00.000Z"}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(resp.as_bytes()).await.unwrap();
                } else {
                    let first_line = head.lines().next().unwrap_or("").to_owned();
                    if let Some(tx) = seen_uri_tx.take() {
                        let _ = tx.send(first_line);
                    }
                    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    while let Some(Ok(msg)) = ws.next().await {
                        if let Message::Binary(data) = msg {
                            let mut echoed = b"echo:".to_vec();
                            echoed.extend_from_slice(&data);
                            ws.send(Message::Binary(echoed)).await.unwrap();
                            break;
                        }
                    }
                    let _ = ws.close(None).await;
                }
            }
        });

        let creds = Arc::new(Credentials { http_base: format!("http://{server_addr}"), access_token: "tok".into() });
        assert_eq!(
            tunnel_url(&creds, "tk-tunnel", Some(5173)).as_deref(),
            Some(format!("ws://{server_addr}/api/preview/tunnel?wsTicket=tk-tunnel&port=5173").as_str())
        );
        let tunnel = LoopbackTunnel::start(creds).await.unwrap();
        let proxy_addr = tunnel.addr();

        // 1. Non-loopback SOCKS5 CONNECT is refused with SOCKS_REPLY_NOT_ALLOWED.
        let mut refused = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
        refused.write_all(&[5, 1, 0]).await.unwrap();
        let mut greeting = [0u8; 2];
        refused.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 0]);
        refused.write_all(&[5, 1, 0, 1, 10, 0, 0, 1, 0, 80]).await.unwrap();
        let mut reply = [0u8; 10];
        refused.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], t3_core::preview::stream::SOCKS_REPLY_NOT_ALLOWED);

        // 2. Loopback SOCKS5 CONNECT dials /api/preview/tunnel?wsTicket=...&port=5173 and relays bytes.
        let mut client = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        client.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 0]);
        client.write_all(&[5, 1, 0, 1, 127, 0, 0, 1, 0x14, 0x35]).await.unwrap();
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], t3_core::preview::stream::SOCKS_REPLY_SUCCEEDED);
        client.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 10];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"echo:hello");

        let uri_line = seen_uri_rx.await.unwrap();
        assert!(uri_line.contains("/api/preview/tunnel?wsTicket=tk-tunnel&port=5173"), "unexpected uri: {uri_line}");
        drop(client);
        drop(tunnel);
        server_task.await.unwrap();
    }
}
