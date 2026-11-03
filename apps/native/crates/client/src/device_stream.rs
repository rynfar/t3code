//! One device's screen and input through the server's device hub, run as
//! `createDeviceStreamClient` runs an iOS device with `preferMjpeg`: mint a `media` ticket, read
//! serve-sim's MJPEG feed, and, once that feed has started the helper's capture, keep its HID
//! socket open for input. Frames land in a `watch` channel, so a slow viewer only sees the newest
//! JPEG and a repeated still is never sent twice; status, screen config and input state go out
//! as events. A refused ticket mints new access, a few times in a row at most. A refused or
//! silent feed stops with the error the view offers to reconnect from, as the web's MJPEG image
//! does when no WebRTC fallback exists.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use t3_core::device::stream::{
    CloseAction, FIRST_FRAME_TIMEOUT_MS, HardwareButton, INPUT_UNAVAILABLE, IosMessage, KeyPhase, MjpegParser, NO_VIDEO, Orientation, RETRY_DELAY_MS,
    STREAM_FAILED, Screen, StreamStatus, TouchPhase, classify_close, close_detail, decode_ios_message,
};
use t3_core::device::{HubAccess, encode_component};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::auth::{Credentials, issue_media_ticket};

const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Refused tickets in a row before the stream gives up.
const MAX_REFUSALS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceTarget {
    pub host_id: String,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeviceStreamEvent {
    Status(StreamStatus),
    Screen(Screen),
    /// Whether input reaches the device, and why not.
    Input { connected: bool, detail: Option<String> },
}

/// What the viewer sends. Touches are 0..1 in the displayed frame.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceInput {
    Touch { phase: TouchPhase, x: f64, y: f64 },
    /// A DOM `KeyboardEvent.code`.
    Key { phase: KeyPhase, code: String },
    Button(HardwareButton),
    /// The next orientation after the screen's current one.
    Rotate,
}

pub type Frame = Arc<[u8]>;

/// Dropping its `input` stops the stream.
pub struct DeviceStream {
    pub input: InputSender,
    pub frames: watch::Receiver<Option<Frame>>,
    pub events: mpsc::UnboundedReceiver<DeviceStreamEvent>,
}

#[derive(Clone)]
pub struct InputSender(mpsc::UnboundedSender<DeviceInput>);

impl InputSender {
    /// Queues input; it is dropped unless the device admitted this viewer's input.
    pub fn send(&self, input: DeviceInput) {
        let _ = self.0.send(input);
    }
}

pub fn open(runtime: &tokio::runtime::Handle, credentials: Arc<Credentials>, target: DeviceTarget) -> DeviceStream {
    let (frames_tx, frames) = watch::channel(None);
    let (events_tx, events) = mpsc::unbounded_channel();
    let (input, input_rx) = mpsc::unbounded_channel();
    runtime.spawn(run(credentials, target, frames_tx, events_tx, input_rx));
    DeviceStream { input: InputSender(input), frames, events }
}

type Events = mpsc::UnboundedSender<DeviceStreamEvent>;

/// Aborts a task with its owner.
struct Abort(tokio::task::JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum Outcome {
    Unauthorized,
    Failed(String),
}

enum Ended {
    /// The viewer dropped its handle.
    Stopped,
    Unauthorized { streamed: bool },
    Failed(String),
}

fn fail(events: &Events, detail: &str) {
    let _ = events.send(DeviceStreamEvent::Input { connected: false, detail: Some(detail.to_owned()) });
    let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Error(detail.to_owned())));
}

async fn until_closed(input: &mut mpsc::UnboundedReceiver<DeviceInput>) {
    while input.recv().await.is_some() {}
}

/// Waits `ms` unless the viewer goes away first; input meanwhile has nowhere to go.
async fn pause(ms: u64, input: &mut mpsc::UnboundedReceiver<DeviceInput>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(Duration::from_millis(ms)) => true,
        () = until_closed(input) => false,
    }
}

async fn run(credentials: Arc<Credentials>, target: DeviceTarget, frames: watch::Sender<Option<Frame>>, events: Events, mut input: mpsc::UnboundedReceiver<DeviceInput>) {
    let mut refusals = 0u32;
    loop {
        let ticket = tokio::select! {
            ticket = issue_media_ticket(&credentials) => ticket,
            () = until_closed(&mut input) => return,
        };
        let access = match ticket.map(|ticket| HubAccess::new(&credentials.http_base, &ticket, &target.host_id)) {
            Ok(Some(access)) => access,
            other => {
                if let Err(error) = other {
                    tracing::debug!(%error, "device hub ticket failed");
                }
                return fail(&events, STREAM_FAILED);
            }
        };
        match lifecycle(&access, &target.device_id, &frames, &events, &mut input).await {
            Ended::Stopped => return,
            Ended::Failed(detail) => return fail(&events, &detail),
            Ended::Unauthorized { streamed } => {
                refusals = if streamed { 1 } else { refusals + 1 };
                if refusals >= MAX_REFUSALS {
                    return fail(&events, STREAM_FAILED);
                }
                let _ = events.send(DeviceStreamEvent::Input { connected: false, detail: None });
            }
        }
    }
}

/// One ticket's video and input, until the viewer leaves or either needs new access or fails.
async fn lifecycle(access: &HubAccess, device: &str, frames: &watch::Sender<Option<Frame>>, events: &Events, input: &mut mpsc::UnboundedReceiver<DeviceInput>) -> Ended {
    let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Connecting));
    let (outcome_tx, mut outcome) = mpsc::unbounded_channel();
    let (capture_tx, capture) = watch::channel(false);
    let (socket_tx, socket_rx) = mpsc::unbounded_channel();
    let streamed = Arc::new(AtomicBool::new(false));
    let _video = Abort(tokio::spawn(video(access.clone(), device.to_owned(), frames.clone(), events.clone(), outcome_tx.clone(), capture_tx, streamed.clone())));
    let _socket = Abort(tokio::spawn(input_socket(access.clone(), device.to_owned(), events.clone(), outcome_tx, capture, socket_rx)));
    loop {
        tokio::select! {
            next = input.recv() => match next {
                None => return Ended::Stopped,
                Some(next) => {
                    let _ = socket_tx.send(next);
                }
            },
            next = outcome.recv() => return match next {
                Some(Outcome::Unauthorized) => Ended::Unauthorized { streamed: streamed.load(Ordering::Relaxed) },
                Some(Outcome::Failed(detail)) => Ended::Failed(detail),
                None => Ended::Failed(STREAM_FAILED.to_owned()),
            },
        }
    }
}

/// Reads the MJPEG feed, reconnecting when it ends, until the first frame is overdue.
async fn video(
    access: HubAccess,
    device: String,
    frames: watch::Sender<Option<Frame>>,
    events: Events,
    outcome: mpsc::UnboundedSender<Outcome>,
    capture: watch::Sender<bool>,
    streamed: Arc<AtomicBool>,
) {
    let url = access.http_url(true, &format!("/helper/{}/stream.mjpeg", encode_component(&device)));
    let Ok(client) = reqwest::Client::builder().connect_timeout(OPEN_TIMEOUT).build() else {
        let _ = outcome.send(Outcome::Failed(STREAM_FAILED.to_owned()));
        return;
    };
    let first_frame = || Instant::now() + Duration::from_millis(FIRST_FRAME_TIMEOUT_MS);
    let mut deadline = first_frame();
    let mut streaming = false;
    let mut last: Option<Frame> = None;
    let overdue = |outcome: &mpsc::UnboundedSender<Outcome>| {
        let _ = outcome.send(Outcome::Failed(NO_VIDEO.to_owned()));
    };
    loop {
        let response = tokio::select! {
            response = client.get(&url).send() => response,
            () = tokio::time::sleep_until(deadline), if !streaming => return overdue(&outcome),
        };
        let mut response = match response {
            Ok(response) if matches!(response.status().as_u16(), 401 | 403) => {
                let _ = outcome.send(Outcome::Unauthorized);
                return;
            }
            Ok(response) if response.status().is_success() => response,
            other => {
                if let Ok(response) = other {
                    tracing::debug!(status = %response.status(), "device stream refused");
                }
                let _ = outcome.send(Outcome::Failed(STREAM_FAILED.to_owned()));
                return;
            }
        };
        capture.send_replace(true);
        let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).map(str::to_owned);
        let mut parser = MjpegParser::new(content_type.as_deref());
        loop {
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk,
                () = tokio::time::sleep_until(deadline), if !streaming => return overdue(&outcome),
            };
            let Ok(Some(bytes)) = chunk else { break };
            let Some(frame) = parser.push(&bytes) else { continue };
            if last.as_deref() != Some(frame.as_slice()) {
                let frame = Frame::from(frame);
                last = Some(frame.clone());
                frames.send_replace(Some(frame));
            }
            if !streaming {
                streaming = true;
                streamed.store(true, Ordering::Relaxed);
                let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Streaming));
            }
        }
        // The feed ended: connect again, as the AVCC reader does.
        if streaming {
            streaming = false;
            deadline = first_frame();
            let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Connecting));
        }
        tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
    }
}

fn encode(input: DeviceInput, screen: Option<&Screen>) -> Option<Vec<u8>> {
    use t3_core::device::stream as wire;
    match input {
        DeviceInput::Touch { phase, x, y } => Some(wire::touch(phase, x, y, screen)),
        DeviceInput::Key { phase, code } => wire::key(phase, &code),
        DeviceInput::Button(button) => Some(wire::button(button)),
        DeviceInput::Rotate => Some(wire::orientation(screen.map_or(Orientation::Portrait, |screen| screen.orientation).next())),
    }
}

/// serve-sim's HID socket. Its helper accepts input and reports the screen only once capture
/// runs, which the MJPEG feed starts, so it connects after the feed answers.
async fn input_socket(
    access: HubAccess,
    device: String,
    events: Events,
    outcome: mpsc::UnboundedSender<Outcome>,
    mut capture: watch::Receiver<bool>,
    mut input: mpsc::UnboundedReceiver<DeviceInput>,
) {
    loop {
        if *capture.borrow_and_update() {
            break;
        }
        tokio::select! {
            changed = capture.changed() => if changed.is_err() { return },
            next = input.recv() => if next.is_none() { return },
        }
    }
    let url = access.ws_url(true, &format!("/helper/ws?device={}", encode_component(&device)));
    let mut screen: Option<Screen> = None;
    loop {
        let connect = tokio::time::timeout(OPEN_TIMEOUT, tokio_tungstenite::connect_async(url.as_str()));
        let socket = tokio::select! {
            socket = connect => socket,
            () = until_closed(&mut input) => return,
        };
        let socket = match socket {
            Ok(Ok((socket, _))) => socket,
            Ok(Err(WsError::Http(response))) if matches!(response.status().as_u16(), 401 | 403) => {
                let _ = outcome.send(Outcome::Unauthorized);
                return;
            }
            _ => {
                let _ = events.send(DeviceStreamEvent::Input { connected: false, detail: Some(close_detail(Some(1006), "")) });
                if !pause(RETRY_DELAY_MS, &mut input).await {
                    return;
                }
                continue;
            }
        };
        let (mut sink, mut stream) = socket.split();
        let (mut admitted, mut unavailable) = (false, false);
        let (code, reason) = loop {
            tokio::select! {
                message = stream.next() => match message {
                    Some(Ok(Message::Binary(bytes))) => match decode_ios_message(&bytes) {
                        IosMessage::InputAdmitted => {
                            admitted = true;
                            let _ = events.send(DeviceStreamEvent::Input { connected: !unavailable, detail: None });
                        }
                        IosMessage::Screen(next) => {
                            unavailable = next.input_unavailable;
                            let detail = unavailable.then(|| INPUT_UNAVAILABLE.to_owned());
                            let _ = events.send(DeviceStreamEvent::Input { connected: admitted && !unavailable, detail });
                            screen = Some(next);
                            let _ = events.send(DeviceStreamEvent::Screen(next));
                        }
                        IosMessage::Other => {}
                    },
                    Some(Ok(Message::Close(frame))) => break frame.map_or((None, String::new()), |frame| (Some(u16::from(frame.code)), frame.reason.into_owned())),
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break (Some(1006), String::new()),
                },
                next = input.recv() => match next {
                    None => {
                        let _ = sink.close().await;
                        return;
                    }
                    Some(next) => {
                        let frame = (admitted && !unavailable).then(|| encode(next, screen.as_ref())).flatten();
                        if let Some(frame) = frame && sink.send(Message::Binary(frame)).await.is_err() {
                            break (Some(1006), String::new());
                        }
                    }
                },
            }
        };
        if classify_close(code) == CloseAction::Unauthorized {
            let _ = outcome.send(Outcome::Unauthorized);
            return;
        }
        let _ = events.send(DeviceStreamEvent::Input { connected: false, detail: Some(close_detail(code, &reason)) });
        if !pause(RETRY_DELAY_MS, &mut input).await {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
    use tokio_tungstenite::tungstenite::protocol::Role;

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    /// A fake server with a device hub: a media ticket, serve-sim's MJPEG feed and HID socket.
    struct FakeHub {
        addr: std::net::SocketAddr,
        /// What the HID socket received.
        received: mpsc::UnboundedReceiver<Vec<u8>>,
        /// Every request line, in order.
        requests: Arc<Mutex<Vec<String>>>,
    }

    fn part(body: &[u8]) -> Vec<u8> {
        [format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(), body, b"\r\n"].concat()
    }

    async fn read_head(stream: &mut TcpStream) -> Option<String> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.ok()?;
            head.push(byte[0]);
        }
        String::from_utf8(head).ok()
    }

    impl FakeHub {
        /// `refuse_feed`: the feed answers 401, as an expired ticket would.
        async fn start(refuse_feed: bool) -> FakeHub {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (sent, received) = mpsc::unbounded_channel();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = requests.clone();
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let (sent, log) = (sent.clone(), log.clone());
                    tokio::spawn(async move {
                        let Some(head) = read_head(&mut stream).await else { return };
                        log.lock().unwrap().push(head.lines().next().unwrap_or_default().to_owned());
                        if head.starts_with("POST /api/auth/websocket-ticket/media") {
                            let body = r#"{"ticket":"tick"}"#;
                            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                        } else if head.contains("/stream.mjpeg?") {
                            if refuse_feed {
                                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
                                return;
                            }
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: multipart/x-mixed-replace; boundary=frame\r\nconnection: close\r\n\r\n").await;
                            let _ = stream.write_all(&[part(b"first"), part(b"first")].concat()).await;
                            let _ = stream.write_all(&part(b"second")).await;
                            // Hold the feed open, as a still screen does.
                            let mut rest = Vec::new();
                            let _ = stream.read_to_end(&mut rest).await;
                        } else if head.contains("/helper/ws?") {
                            let key = head.lines().find_map(|line| line.strip_prefix("Sec-WebSocket-Key: ").or_else(|| line.strip_prefix("sec-websocket-key: "))).unwrap_or_default().trim().to_owned();
                            let accept = derive_accept_key(key.as_bytes());
                            let _ = stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await;
                            let mut socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
                            let config = [&[0x82u8][..], br#"{"width":1170,"height":2532,"orientation":"landscape_left"}"#].concat();
                            let _ = socket.send(Message::Binary(config)).await;
                            let _ = socket.send(Message::Binary(vec![0x83])).await;
                            while let Some(Ok(message)) = socket.next().await {
                                if let Message::Binary(bytes) = message {
                                    let _ = sent.send(bytes);
                                }
                            }
                        }
                    });
                }
            });
            FakeHub { addr, received, requests }
        }

        fn credentials(&self) -> Arc<Credentials> {
            Arc::new(Credentials { http_base: format!("http://{}", self.addr), access_token: "token".into() })
        }
    }

    async fn next_event(stream: &mut DeviceStream) -> DeviceStreamEvent {
        tokio::time::timeout(WAIT, stream.events.recv()).await.expect("an event arrives").expect("the stream runs")
    }

    #[tokio::test]
    async fn streams_newest_frames_and_sends_input_once_admitted() {
        let mut hub = FakeHub::start(false).await;
        let target = DeviceTarget { host_id: "local".into(), device_id: "SIM 1".into() };
        let mut stream = open(&tokio::runtime::Handle::current(), hub.credentials(), target);
        // Input before the helper admits this viewer goes nowhere.
        stream.input.send(DeviceInput::Button(HardwareButton::Home));
        let (mut streaming, mut screen, mut connected) = (false, None, false);
        while !(streaming && screen.is_some() && connected) {
            match next_event(&mut stream).await {
                DeviceStreamEvent::Status(StreamStatus::Streaming) => streaming = true,
                DeviceStreamEvent::Screen(next) => screen = Some(next),
                DeviceStreamEvent::Input { connected: true, .. } => connected = true,
                DeviceStreamEvent::Status(StreamStatus::Error(detail)) => panic!("stream failed: {detail}"),
                _ => {}
            }
        }
        let frame = tokio::time::timeout(WAIT, async {
            loop {
                if let Some(frame) = stream.frames.borrow_and_update().clone()
                    && &frame[..] == b"second"
                {
                    return frame;
                }
                stream.frames.changed().await.expect("frames keep coming");
            }
        })
        .await
        .expect("the newest frame arrives");
        assert_eq!(&frame[..], b"second");
        // The screen is landscape on a portrait framebuffer: touches are remapped into it.
        stream.input.send(DeviceInput::Touch { phase: TouchPhase::Begin, x: 0.25, y: 0.5 });
        stream.input.send(DeviceInput::Rotate);
        let touch = tokio::time::timeout(WAIT, hub.received.recv()).await.unwrap().unwrap();
        assert_eq!(touch, [&[0x03][..], br#"{"type":"begin","x":0.5,"y":0.75}"#].concat());
        let rotate = tokio::time::timeout(WAIT, hub.received.recv()).await.unwrap().unwrap();
        assert_eq!(rotate, [&[0x07][..], br#"{"orientation":"portrait_upside_down"}"#].concat());
        let requests = hub.requests.lock().unwrap().clone();
        assert!(requests.iter().any(|line| line.starts_with("GET /api/device-hub/vendor/serve-sim/helper/SIM%201/stream.mjpeg?wsTicket=tick&hostId=local ")), "{requests:?}");
        assert!(requests.iter().any(|line| line.starts_with("GET /api/device-hub/vendor/serve-sim/helper/ws?device=SIM%201&wsTicket=tick&hostId=local ")), "{requests:?}");
    }

    #[tokio::test]
    async fn a_refused_feed_mints_new_access_then_gives_up() {
        let hub = FakeHub::start(true).await;
        let target = DeviceTarget { host_id: "local".into(), device_id: "sim".into() };
        let mut stream = open(&tokio::runtime::Handle::current(), hub.credentials(), target);
        let detail = loop {
            if let DeviceStreamEvent::Status(StreamStatus::Error(detail)) = next_event(&mut stream).await {
                break detail;
            }
        };
        assert_eq!(detail, STREAM_FAILED);
        let tickets = hub.requests.lock().unwrap().iter().filter(|line| line.starts_with("POST /api/auth/websocket-ticket/media")).count();
        assert_eq!(tickets, MAX_REFUSALS as usize);
    }
}
