//! One device's screen and input through the server's device hub, porting
//! `createDeviceStreamClient` (`packages/client-runtime/src/device/stream.ts`):
//! - iOS (`serve-sim` MJPEG): MJPEG feed + HID/Duo socket (`/helper/ws`).
//! - Simlock / AVCC (`serve-sim` AVCC): framed AVCC H.264 stream (`/helper/<device>[/panel/<id>]/stream.avcc`)
//!   decoded via macOS VideoToolbox (`h264::H264Decoder`), honoring tag `7` lease-end JSON frames
//!   to stop immediately with the gateway's reason instead of reconnecting (`pingdotgg/t3code#35994`).
//! - Android (`serve-emu`): WebSocket H.264 stream (`/ws?device=...&frame-meta=1`) carrying SEMU
//!   Annex-B packets converted to 4-byte length-prefixed AVCC for VideoToolbox, plus JSON touch,
//!   key, text, and hardware-button control messages.
//!
//! `capture_screenshot` ports `client-runtime/src/device/screenshot.ts`, and `fetch_ax_tree`,
//! `subscribe_foreground`, `subscribe_event_log`, `read_android_fold`, and `set_android_fold`
//! port `apps/web/src/components/device/deviceHubApi.ts` and `deviceFold.ts`.

pub mod h264;

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use t3_core::device::hub::{
    AndroidFoldState, AxTree, EventLogEntry, FoldActionPosture, ForegroundInfo, SseParser, android_fold_path, append_event_log, ax_tree_path, event_log_path, foreground_path,
    parse_android_fold, parse_ax_tree, parse_event_log_message, parse_foreground_event,
};
use t3_core::device::stream::{
    AvccChunk, AvccConfig, AvccDemuxer, CloseAction, ControlReply, DUO_CONTROL_TIMEOUT_MS, DuoCommand, DuoControlQueue, DuoControlState, FIRST_FRAME_TIMEOUT_MS,
    HardwareButton, INPUT_UNAVAILABLE, IosMessage, KeyPhase, MjpegParser, NO_VIDEO, Orientation, RETRY_DELAY_MS, STREAM_FAILED, Screen, StreamStatus, TouchPhase,
    Transport, android_button, android_key, android_key_from_code, android_reset_video, android_screen_for_dimensions, android_text_chunks, android_touch, android_ws_path,
    annex_b_to_avcc, avcc_stream_path, classify_close, close_detail, decode_ios_message, duo_control_frame, is_video_session_message, lease_ended, parse_semu_packet,
    serve_sim_button, touch_for_platform, transport,
};
use t3_core::device::tools::{SCREENSHOT_FAILED, SCREENSHOT_INCOMPLETE, SCREENSHOT_TIMEOUT_MS, extract_png, screenshot_path, screenshot_serve_sim, screenshot_status_error};
use t3_core::device::{HostKind, HubAccess, Platform, encode_component};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::auth::{Credentials, issue_media_ticket};
use self::h264::H264Decoder;

const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Refused tickets in a row before the stream gives up.
const MAX_REFUSALS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceTarget {
    pub host_id: String,
    pub device_id: String,
}

/// Transport-selection options for opening a device stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamOptions {
    pub platform: Platform,
    pub host_kind: Option<HostKind>,
    /// Optional fixed Duo panel id (`1` = cover, `3` = inner) for `/helper/<device>/panel/<id>/stream.avcc`.
    pub panel_id: Option<u8>,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self { platform: Platform::Ios, host_kind: None, panel_id: None }
    }
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
    /// An Android `serve-emu` key event carrying DOM `key` and shortcut modifier state.
    AndroidKey { phase: KeyPhase, key: String, ctrl: bool, meta: bool },
    /// Plain text to type/paste into an Android `serve-emu` device.
    Text(String),
    Button(HardwareButton),
    /// The next orientation after the screen's current one.
    Rotate,
    /// Sets an explicit orientation (`setOrientation`).
    SetOrientation(Orientation),
    /// Sends an iPhone Duo hinge/posture command (`controlDuo`).
    Duo(DuoCommand),
}

/// One received frame from a device stream: either encoded JPEG (MJPEG / AVCC seed) or decoded
/// tight RGBA pixels from hardware H.264 decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Jpeg(Arc<[u8]>),
    Rgba { width: u32, height: u32, bytes: Arc<[u8]> },
}

impl Frame {
    pub fn len(&self) -> usize {
        match self {
            Frame::Jpeg(bytes) => bytes.len(),
            Frame::Rgba { bytes, .. } => bytes.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Dropping its `input` stops the stream.
pub struct DeviceStream {
    pub input: InputSender,
    pub frames: watch::Receiver<Option<Frame>>,
    pub events: mpsc::UnboundedReceiver<DeviceStreamEvent>,
    pub duo: watch::Receiver<DuoControlState>,
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
    open_with_options(runtime, credentials, target, StreamOptions::default())
}

pub fn open_with_options(runtime: &tokio::runtime::Handle, credentials: Arc<Credentials>, target: DeviceTarget, options: StreamOptions) -> DeviceStream {
    let (frames_tx, frames) = watch::channel(None);
    let (events_tx, events) = mpsc::unbounded_channel();
    let (duo_tx, duo) = watch::channel(DuoControlState::default());
    let (input, input_rx) = mpsc::unbounded_channel();
    runtime.spawn(run(credentials, target, options, frames_tx, events_tx, duo_tx, input_rx));
    DeviceStream { input: InputSender(input), frames, events, duo }
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

async fn run(
    credentials: Arc<Credentials>,
    target: DeviceTarget,
    options: StreamOptions,
    frames: watch::Sender<Option<Frame>>,
    events: Events,
    duo: watch::Sender<DuoControlState>,
    mut input: mpsc::UnboundedReceiver<DeviceInput>,
) {
    let selected = if options.panel_id.is_some() {
        if cfg!(target_os = "macos") {
            Ok(Transport::Avcc)
        } else {
            Err("This app cannot decode this device's stream (H.264 decoding unavailable).")
        }
    } else {
        transport(options.platform, options.host_kind)
    };
    let selected = match selected {
        Ok(selected) => selected,
        Err(detail) => return fail(&events, detail),
    };
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
        let ended = match selected {
            Transport::Mjpeg | Transport::Avcc => lifecycle_serve_sim(&access, &target.device_id, options, selected, &frames, &events, &duo, &mut input).await,
            Transport::ServeEmu => lifecycle_serve_emu(&access, &target.device_id, &frames, &events, &mut input).await,
        };
        match ended {
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

/// One ticket's serve-sim video (MJPEG or AVCC) and HID/Duo socket.
#[allow(clippy::too_many_arguments)]
async fn lifecycle_serve_sim(
    access: &HubAccess,
    device: &str,
    options: StreamOptions,
    selected: Transport,
    frames: &watch::Sender<Option<Frame>>,
    events: &Events,
    duo: &watch::Sender<DuoControlState>,
    input: &mut mpsc::UnboundedReceiver<DeviceInput>,
) -> Ended {
    let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Connecting));
    let (outcome_tx, mut outcome) = mpsc::unbounded_channel();
    let serve_sim_only = options.host_kind == Some(HostKind::Simlock);
    let (capture_tx, capture) = watch::channel(serve_sim_only);
    let (socket_tx, socket_rx) = mpsc::unbounded_channel();
    let streamed = Arc::new(AtomicBool::new(false));
    let _video = Abort(match selected {
        Transport::Mjpeg => tokio::spawn(video_mjpeg(
            access.clone(),
            device.to_owned(),
            frames.clone(),
            events.clone(),
            outcome_tx.clone(),
            capture_tx,
            streamed.clone(),
        )),
        Transport::Avcc => tokio::spawn(video_avcc(
            access.clone(),
            device.to_owned(),
            options.panel_id,
            frames.clone(),
            events.clone(),
            outcome_tx.clone(),
            capture_tx,
            streamed.clone(),
        )),
        Transport::ServeEmu => unreachable!(),
    });
    let _socket = Abort(tokio::spawn(input_socket(
        access.clone(),
        device.to_owned(),
        options.platform,
        serve_sim_only,
        events.clone(),
        duo.clone(),
        outcome_tx,
        capture,
        socket_rx,
    )));
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
async fn video_mjpeg(
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
    let mut last: Option<Arc<[u8]>> = None;
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
                let bytes = Arc::<[u8]>::from(frame);
                last = Some(bytes.clone());
                frames.send_replace(Some(Frame::Jpeg(bytes)));
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

/// Reads serve-sim's framed AVCC H.264 feed (`/helper/<device>[/panel/<id>]/stream.avcc`),
/// decoding frames via VideoToolbox and halting reconnects on Simlock tag `7` lease-end frames.
#[allow(clippy::too_many_arguments)]
async fn video_avcc(
    access: HubAccess,
    device: String,
    panel_id: Option<u8>,
    frames: watch::Sender<Option<Frame>>,
    events: Events,
    outcome: mpsc::UnboundedSender<Outcome>,
    capture: watch::Sender<bool>,
    streamed: Arc<AtomicBool>,
) {
    let url = access.http_url(true, &avcc_stream_path(&device, panel_id));
    let Ok(client) = reqwest::Client::builder().connect_timeout(OPEN_TIMEOUT).build() else {
        let _ = outcome.send(Outcome::Failed(STREAM_FAILED.to_owned()));
        return;
    };
    let decoder = Arc::new(Mutex::new(H264Decoder::new()));
    let first_frame = || Instant::now() + Duration::from_millis(FIRST_FRAME_TIMEOUT_MS);
    let mut deadline = first_frame();
    let mut streaming = false;
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
            Ok(response) if panel_id.is_some() && matches!(response.status().as_u16(), 400 | 404 | 405 | 410) => {
                let _ = outcome.send(Outcome::Failed("This Device Hub does not provide fixed Duo display feeds.".to_owned()));
                return;
            }
            Ok(response) if response.status().is_success() => response,
            other => {
                if let Ok(response) = other {
                    tracing::debug!(status = %response.status(), "avcc device stream refused");
                }
                let _ = outcome.send(Outcome::Failed(STREAM_FAILED.to_owned()));
                return;
            }
        };
        capture.send_replace(true);
        let mut demuxer = AvccDemuxer::new();
        let mut awaiting_keyframe = true;
        let mut decoded_video_on_feed = false;
        let mut lease_end_reason: Option<String> = None;
        loop {
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk,
                () = tokio::time::sleep_until(deadline), if !streaming => return overdue(&outcome),
            };
            let Ok(Some(bytes)) = chunk else { break };
            for item in demuxer.push(&bytes) {
                match item {
                    AvccChunk::Seed(jpeg) => {
                        if !decoded_video_on_feed {
                            frames.send_replace(Some(Frame::Jpeg(Arc::from(jpeg))));
                            if !streaming {
                                streaming = true;
                                streamed.store(true, Ordering::Relaxed);
                                let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Streaming));
                            }
                        }
                    }
                    AvccChunk::Description(avcc) => {
                        awaiting_keyframe = true;
                        let dec = decoder.clone();
                        let res = tokio::task::spawn_blocking(move || dec.lock().unwrap().configure_avcc(&avcc)).await;
                        match res {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
                                tracing::debug!(%error, "avcc decoder configure failed");
                                let _ = outcome.send(Outcome::Failed(error));
                                return;
                            }
                            Err(_) => return,
                        }
                    }
                    AvccChunk::Keyframe(au) => {
                        awaiting_keyframe = false;
                        let dec = decoder.clone();
                        let decoded = tokio::task::spawn_blocking(move || dec.lock().unwrap().decode_avcc(&au)).await;
                        match decoded {
                            Ok(Ok(Some(rgba))) => {
                                decoded_video_on_feed = true;
                                frames.send_replace(Some(Frame::Rgba { width: rgba.width, height: rgba.height, bytes: rgba.bytes }));
                                if !streaming {
                                    streaming = true;
                                    streamed.store(true, Ordering::Relaxed);
                                    let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Streaming));
                                }
                            }
                            Ok(Ok(None)) => {}
                            Ok(Err(error)) => {
                                tracing::debug!(%error, "avcc keyframe decode failed; waiting for next keyframe");
                                awaiting_keyframe = true;
                            }
                            Err(_) => return,
                        }
                    }
                    AvccChunk::Delta(au) => {
                        if awaiting_keyframe {
                            continue;
                        }
                        let dec = decoder.clone();
                        let decoded = tokio::task::spawn_blocking(move || dec.lock().unwrap().decode_avcc(&au)).await;
                        match decoded {
                            Ok(Ok(Some(rgba))) => {
                                decoded_video_on_feed = true;
                                frames.send_replace(Some(Frame::Rgba { width: rgba.width, height: rgba.height, bytes: rgba.bytes }));
                                if !streaming {
                                    streaming = true;
                                    streamed.store(true, Ordering::Relaxed);
                                    let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Streaming));
                                }
                            }
                            Ok(Ok(None)) => {}
                            Ok(Err(error)) => {
                                tracing::debug!(%error, "avcc delta decode failed; waiting for next keyframe");
                                awaiting_keyframe = true;
                            }
                            Err(_) => return,
                        }
                    }
                    AvccChunk::End { reason } => {
                        lease_end_reason = Some(reason);
                        break;
                    }
                }
            }
            if lease_end_reason.is_some() {
                break;
            }
        }
        // Tag `7` lease-end frame: stop immediately with the gateway's reason; never reconnect.
        if let Some(reason) = lease_end_reason {
            let _ = outcome.send(Outcome::Failed(lease_ended(&reason)));
            return;
        }
        {
            decoder.lock().unwrap().teardown();
        }
        if streaming {
            streaming = false;
            deadline = first_frame();
            let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Connecting));
        }
        tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
    }
}

fn encode_serve_sim_input(
    input: DeviceInput,
    platform: Platform,
    serve_sim_only: bool,
    screen: Option<&Screen>,
    duo_queue: &mut DuoControlQueue,
    rotation_cursor: &mut Option<Orientation>,
    pending_orientation: &mut Option<u32>,
) -> Option<Vec<u8>> {
    use t3_core::device::stream as wire;
    match input {
        DeviceInput::Touch { phase, x, y } => Some(touch_for_platform(platform, phase, x, y, screen)),
        DeviceInput::Key { phase, code } => wire::key(phase, &code),
        DeviceInput::AndroidKey { .. } | DeviceInput::Text(_) => None,
        DeviceInput::Button(btn) => {
            if serve_sim_only {
                serve_sim_button(platform, btn)
            } else {
                Some(wire::button(btn))
            }
        }
        DeviceInput::SetOrientation(orient) => (platform == Platform::Ios).then(|| wire::orientation(orient)),
        DeviceInput::Rotate => {
            if platform != Platform::Ios {
                return None;
            }
            let supports_hinge = screen.is_some_and(|s| s.supports_hinge_angle);
            let current = if supports_hinge {
                rotation_cursor.or_else(|| screen.map(|s| s.orientation)).unwrap_or(Orientation::Portrait)
            } else {
                screen.map_or(Orientation::Portrait, |s| s.orientation)
            };
            let next = current.next();
            if supports_hinge {
                *rotation_cursor = Some(next);
                let req = duo_queue.enqueue(DuoCommand::Orientation(next))?;
                *pending_orientation = Some(req.request_id);
                Some(duo_control_frame(req.request_id, req.command))
            } else {
                Some(wire::orientation(next))
            }
        }
        DeviceInput::Duo(command) => {
            let screen = screen.filter(|s| s.supports_hinge_angle)?;
            if matches!(command, DuoCommand::Physical(_)) && !screen.supports_physical_orientation {
                return None;
            }
            let req = duo_queue.enqueue(command)?;
            if let DuoCommand::Orientation(value) = req.command {
                *rotation_cursor = Some(value);
                *pending_orientation = Some(req.request_id);
            } else {
                *pending_orientation = None;
            }
            Some(duo_control_frame(req.request_id, req.command))
        }
    }
}

/// serve-sim's HID & Duo control socket (`/helper/ws`).
#[allow(clippy::too_many_arguments)]
async fn input_socket(
    access: HubAccess,
    device: String,
    platform: Platform,
    serve_sim_only: bool,
    events: Events,
    duo: watch::Sender<DuoControlState>,
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
        let mut duo_queue = DuoControlQueue::default();
        let mut rotation_cursor: Option<Orientation> = None;
        let mut pending_orientation: Option<u32> = None;
        let mut duo_deadline: Option<(u32, Instant)> = None;
        let (code, reason) = loop {
            let duo_timer = async {
                match duo_deadline {
                    Some((id, when)) => {
                        tokio::time::sleep_until(when).await;
                        id
                    }
                    None => std::future::pending::<u32>().await,
                }
            };
            tokio::select! {
                timed_out_id = duo_timer => {
                    duo_deadline = None;
                    if duo_queue.timeout(timed_out_id) {
                        pending_orientation = None;
                        duo.send_replace(duo_queue.state());
                    }
                }
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
                            let previous = screen;
                            screen = Some(next);
                            if next.hinge_pose.is_some() && next.hinge_pose != previous.and_then(|s| s.hinge_pose) {
                                rotation_cursor = Some(if next.hinge_pose == Some(t3_core::device::stream::DuoPose::Laptop) {
                                    Orientation::LandscapeLeft
                                } else {
                                    Orientation::Portrait
                                });
                            } else if previous.is_none_or(|s| s.orientation != next.orientation) {
                                rotation_cursor = Some(next.orientation);
                            }
                            let _ = events.send(DeviceStreamEvent::Screen(next));
                            if let Some(request_id) = pending_orientation.take() {
                                let reply = ControlReply { request_id, ok: true, error: None };
                                if let Some(next_req) = duo_queue.receive(&reply) {
                                    if let DuoCommand::Orientation(value) = next_req.command {
                                        rotation_cursor = Some(value);
                                        pending_orientation = Some(next_req.request_id);
                                    }
                                    duo_deadline = Some((next_req.request_id, Instant::now() + Duration::from_millis(DUO_CONTROL_TIMEOUT_MS)));
                                    let frame = duo_control_frame(next_req.request_id, next_req.command);
                                    if sink.send(Message::Binary(frame)).await.is_err() {
                                        break (Some(1006), String::new());
                                    }
                                } else {
                                    duo_deadline = None;
                                }
                                duo.send_replace(duo_queue.state());
                            }
                        }
                        IosMessage::ControlReply(reply) => {
                            if let Some(next_req) = duo_queue.receive(&reply) {
                                if let DuoCommand::Orientation(value) = next_req.command {
                                    rotation_cursor = Some(value);
                                    pending_orientation = Some(next_req.request_id);
                                } else {
                                    pending_orientation = None;
                                }
                                duo_deadline = Some((next_req.request_id, Instant::now() + Duration::from_millis(DUO_CONTROL_TIMEOUT_MS)));
                                let frame = duo_control_frame(next_req.request_id, next_req.command);
                                if sink.send(Message::Binary(frame)).await.is_err() {
                                    break (Some(1006), String::new());
                                }
                            } else if duo_queue.active_request_id().is_none() {
                                duo_deadline = None;
                                pending_orientation = None;
                            }
                            duo.send_replace(duo_queue.state());
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
                        if admitted && !unavailable {
                            let prev_active = duo_queue.active_request_id();
                            let frame = encode_serve_sim_input(
                                next,
                                platform,
                                serve_sim_only,
                                screen.as_ref(),
                                &mut duo_queue,
                                &mut rotation_cursor,
                                &mut pending_orientation,
                            );
                            if duo_queue.active_request_id() != prev_active {
                                duo_deadline = duo_queue.active_request_id().map(|id| (id, Instant::now() + Duration::from_millis(DUO_CONTROL_TIMEOUT_MS)));
                            }
                            duo.send_replace(duo_queue.state());
                            if let Some(frame) = frame && sink.send(Message::Binary(frame)).await.is_err() {
                                duo_queue.send_failed();
                                duo.send_replace(duo_queue.state());
                                break (Some(1006), String::new());
                            }
                        }
                    }
                },
            }
        };
        duo_queue.clear(None);
        duo.send_replace(duo_queue.state());
        match classify_close(code) {
            CloseAction::Unauthorized => {
                let _ = outcome.send(Outcome::Unauthorized);
                return;
            }
            // Reconnecting would only be refused again: stop with the gateway's reason.
            CloseAction::LeaseEnded => {
                let _ = outcome.send(Outcome::Failed(lease_ended(&reason)));
                return;
            }
            CloseAction::Retry => {}
        }
        let _ = events.send(DeviceStreamEvent::Input { connected: false, detail: Some(close_detail(code, &reason)) });
        if !pause(RETRY_DELAY_MS, &mut input).await {
            return;
        }
    }
}

/// One ticket's Android `serve-emu` WebSocket carrying SEMU Annex-B H.264 video and JSON input.
async fn lifecycle_serve_emu(
    access: &HubAccess,
    device: &str,
    frames: &watch::Sender<Option<Frame>>,
    events: &Events,
    input: &mut mpsc::UnboundedReceiver<DeviceInput>,
) -> Ended {
    let url = access.ws_url(false, &android_ws_path(device));
    let decoder = Arc::new(Mutex::new(H264Decoder::new()));
    let first_frame = || Instant::now() + Duration::from_millis(FIRST_FRAME_TIMEOUT_MS);
    let mut streamed = false;
    loop {
        let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Connecting));
        let mut deadline = first_frame();
        let mut streaming = false;
        let connect = tokio::time::timeout(OPEN_TIMEOUT, tokio_tungstenite::connect_async(url.as_str()));
        let socket = tokio::select! {
            socket = connect => socket,
            () = until_closed(input) => return Ended::Stopped,
            () = tokio::time::sleep_until(deadline) => return Ended::Failed(NO_VIDEO.to_owned()),
        };
        let socket = match socket {
            Ok(Ok((socket, _))) => socket,
            Ok(Err(WsError::Http(response))) if matches!(response.status().as_u16(), 401 | 403) => {
                return Ended::Unauthorized { streamed };
            }
            _ => {
                let _ = events.send(DeviceStreamEvent::Input { connected: false, detail: Some(close_detail(Some(1006), "")) });
                if !pause(RETRY_DELAY_MS, input).await {
                    return Ended::Stopped;
                }
                continue;
            }
        };
        let _ = events.send(DeviceStreamEvent::Input { connected: true, detail: None });
        let (mut sink, mut stream) = socket.split();
        let mut cached_sps: Vec<Vec<u8>> = Vec::new();
        let mut cached_pps: Vec<Vec<u8>> = Vec::new();
        let mut awaiting_keyframe = true;
        let mut keyframe_requested = false;
        let mut last_size: Option<(u32, u32)> = None;

        let (code, reason) = loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline), if !streaming => {
                    let _ = sink.close().await;
                    decoder.lock().unwrap().teardown();
                    return Ended::Failed(NO_VIDEO.to_owned());
                }
                message = stream.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        if is_video_session_message(&text) {
                            decoder.lock().unwrap().teardown();
                            cached_sps.clear();
                            cached_pps.clear();
                            awaiting_keyframe = true;
                            keyframe_requested = false;
                            streaming = false;
                            deadline = first_frame();
                            let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Connecting));
                            if sink.send(Message::Text(android_reset_video())).await.is_err() {
                                break (Some(1006), String::new());
                            }
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        let packet = parse_semu_packet(&bytes);
                        let au = annex_b_to_avcc(packet.data);
                        let is_key = packet.is_key.unwrap_or(au.is_key);
                        if !au.sps.is_empty() {
                            cached_sps = au.sps;
                        }
                        if !au.pps.is_empty() {
                            cached_pps = au.pps;
                        }
                        if !cached_sps.is_empty() && !cached_pps.is_empty() {
                            let cfg = AvccConfig { nal_length_size: 4, sps: cached_sps.clone(), pps: cached_pps.clone() };
                            let dec = decoder.clone();
                            let configured = tokio::task::spawn_blocking(move || dec.lock().unwrap().configure(&cfg)).await;
                            if let Ok(Err(error)) = configured {
                                tracing::debug!(%error, "serve-emu H.264 configure failed");
                                return Ended::Failed(error);
                            }
                        }
                        if !decoder.lock().unwrap().is_configured() {
                            if !is_key && !keyframe_requested {
                                keyframe_requested = true;
                                if sink.send(Message::Text(android_reset_video())).await.is_err() {
                                    break (Some(1006), String::new());
                                }
                            }
                            continue;
                        }
                        if au.avcc_data.is_empty() {
                            continue;
                        }
                        if awaiting_keyframe {
                            if !is_key {
                                if !keyframe_requested {
                                    keyframe_requested = true;
                                    if sink.send(Message::Text(android_reset_video())).await.is_err() {
                                        break (Some(1006), String::new());
                                    }
                                }
                                continue;
                            }
                            awaiting_keyframe = false;
                            keyframe_requested = false;
                        }
                        let dec = decoder.clone();
                        let avcc_data = au.avcc_data;
                        let decoded = tokio::task::spawn_blocking(move || dec.lock().unwrap().decode_avcc(&avcc_data)).await;
                        match decoded {
                            Ok(Ok(Some(rgba))) => {
                                if last_size != Some((rgba.width, rgba.height)) {
                                    last_size = Some((rgba.width, rgba.height));
                                    let _ = events.send(DeviceStreamEvent::Screen(android_screen_for_dimensions(rgba.width, rgba.height)));
                                }
                                frames.send_replace(Some(Frame::Rgba { width: rgba.width, height: rgba.height, bytes: rgba.bytes }));
                                if !streaming {
                                    streaming = true;
                                    streamed = true;
                                    let _ = events.send(DeviceStreamEvent::Status(StreamStatus::Streaming));
                                }
                            }
                            Ok(Ok(None)) => {}
                            Ok(Err(error)) => {
                                tracing::debug!(%error, "serve-emu frame decode failed; requesting keyframe");
                                awaiting_keyframe = true;
                                if sink.send(Message::Text(android_reset_video())).await.is_err() {
                                    break (Some(1006), String::new());
                                }
                            }
                            Err(_) => return Ended::Stopped,
                        }
                    }
                    Some(Ok(Message::Close(frame))) => break frame.map_or((None, String::new()), |frame| (Some(u16::from(frame.code)), frame.reason.into_owned())),
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break (Some(1006), String::new()),
                },
                next = input.recv() => match next {
                    None => {
                        let _ = sink.close().await;
                        decoder.lock().unwrap().teardown();
                        return Ended::Stopped;
                    }
                    Some(next) => {
                        let payloads: Vec<String> = match next {
                            DeviceInput::Touch { phase, x, y } => vec![android_touch(phase, x, y)],
                            DeviceInput::Button(btn) => vec![android_button(btn)],
                            DeviceInput::AndroidKey { phase, key, ctrl, meta } => android_key(phase, &key, ctrl, meta).into_iter().collect(),
                            DeviceInput::Key { phase, code } => android_key_from_code(phase, &code).into_iter().collect(),
                            DeviceInput::Text(text) => android_text_chunks(&text),
                            DeviceInput::Rotate | DeviceInput::SetOrientation(_) | DeviceInput::Duo(_) => Vec::new(),
                        };
                        for text in payloads {
                            if sink.send(Message::Text(text)).await.is_err() {
                                break;
                            }
                        }
                    }
                },
            }
        };
        decoder.lock().unwrap().teardown();
        match classify_close(code) {
            CloseAction::Unauthorized => return Ended::Unauthorized { streamed },
            CloseAction::LeaseEnded => return Ended::Failed(lease_ended(&reason)),
            CloseAction::Retry => {}
        }
        let _ = events.send(DeviceStreamEvent::Input { connected: false, detail: Some(close_detail(code, &reason)) });
        if !pause(RETRY_DELAY_MS, input).await {
            return Ended::Stopped;
        }
    }
}

/// One PNG of the device's screen (`captureDeviceScreenshot`), or the message to show.
pub async fn capture_screenshot(credentials: Arc<Credentials>, target: DeviceTarget, platform: Platform) -> Result<Vec<u8>, String> {
    let ticket = issue_media_ticket(&credentials).await.map_err(|error| {
        tracing::debug!(%error, "device hub ticket failed");
        SCREENSHOT_FAILED.to_owned()
    })?;
    let access = HubAccess::new(&credentials.http_base, &ticket, &target.host_id).ok_or(SCREENSHOT_FAILED)?;
    let url = access.http_url(screenshot_serve_sim(platform), &screenshot_path(&target.device_id));
    let client = reqwest::Client::builder().timeout(Duration::from_millis(SCREENSHOT_TIMEOUT_MS)).build().map_err(|_| SCREENSHOT_FAILED)?;
    let response = client.post(&url).send().await.map_err(|error| {
        tracing::debug!(%error, "device screenshot failed");
        SCREENSHOT_FAILED.to_owned()
    })?;
    if !response.status().is_success() {
        return Err(screenshot_status_error(response.status().as_u16()));
    }
    let bytes = response.bytes().await.map_err(|_| SCREENSHOT_FAILED)?;
    extract_png(&bytes).map(<[u8]>::to_vec).ok_or_else(|| SCREENSHOT_INCOMPLETE.to_owned())
}

async fn hub_access(credentials: &Credentials, host_id: &str) -> Result<HubAccess, String> {
    let ticket = issue_media_ticket(credentials).await.map_err(|error| {
        tracing::debug!(%error, "device hub ticket failed");
        "Could not reach the device hub.".to_owned()
    })?;
    HubAccess::new(&credentials.http_base, &ticket, host_id).ok_or_else(|| "Could not reach the device hub.".to_owned())
}

/// Fetches one accessibility tree snapshot (`fetchDeviceAxTree` in `deviceHubApi.ts`).
pub async fn fetch_ax_tree(credentials: Arc<Credentials>, target: DeviceTarget, platform: Platform) -> Result<AxTree, String> {
    let access = hub_access(&credentials, &target.host_id).await?;
    let (serve_sim, path) = ax_tree_path(platform, &target.device_id);
    let url = access.http_url(serve_sim, &path);
    let client = reqwest::Client::builder().timeout(OPEN_TIMEOUT).build().map_err(|_| "Could not read the device accessibility tree.".to_owned())?;
    let response = client.get(&url).send().await.map_err(|_| "Could not read the device accessibility tree.".to_owned())?;
    if !response.status().is_success() {
        return Err(format!("{} {}", response.status().as_u16(), response.status().canonical_reason().unwrap_or("")).trim().to_owned());
    }
    let payload: serde_json::Value = response.json().await.map_err(|_| "Unexpected accessibility payload.".to_owned())?;
    Ok(parse_ax_tree(platform, &payload))
}

/// Subscribes to iOS live foreground app SSE (`/appstate`, `subscribeDeviceForeground`).
/// Stops cleanly when the returned receiver is dropped.
pub fn subscribe_foreground(
    runtime: &tokio::runtime::Handle,
    credentials: Arc<Credentials>,
    target: DeviceTarget,
) -> watch::Receiver<Option<Option<ForegroundInfo>>> {
    let (tx, rx) = watch::channel(None);
    runtime.spawn(async move {
        let Ok(access) = hub_access(&credentials, &target.host_id).await else { return };
        let url = access.http_url(true, &foreground_path(&target.device_id));
        let Ok(client) = reqwest::Client::builder().connect_timeout(OPEN_TIMEOUT).build() else { return };
        let Ok(mut response) = client.get(&url).send().await else { return };
        if !response.status().is_success() {
            return;
        }
        let mut parser = SseParser::default();
        loop {
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk,
                () = tx.closed() => return,
            };
            let Ok(Some(bytes)) = chunk else { break };
            for data in parser.push(&bytes) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data)
                    && let Some(app) = parse_foreground_event(&value)
                {
                    let _ = tx.send(Some(app));
                }
            }
        }
    });
    rx
}

/// Subscribes to iOS event log SSE (`/api/event-log/events`, `subscribeDeviceEventLog`).
/// Stops cleanly when the returned receiver is dropped.
pub fn subscribe_event_log(
    runtime: &tokio::runtime::Handle,
    credentials: Arc<Credentials>,
    target: DeviceTarget,
) -> watch::Receiver<Vec<EventLogEntry>> {
    let (tx, rx) = watch::channel(Vec::new());
    runtime.spawn(async move {
        let Ok(access) = hub_access(&credentials, &target.host_id).await else { return };
        let url = access.http_url(true, &event_log_path(&target.device_id));
        let Ok(client) = reqwest::Client::builder().connect_timeout(OPEN_TIMEOUT).build() else { return };
        let Ok(mut response) = client.get(&url).send().await else { return };
        if !response.status().is_success() {
            return;
        }
        let mut parser = SseParser::default();
        let mut entries = Vec::new();
        loop {
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk,
                () = tx.closed() => return,
            };
            let Ok(Some(bytes)) = chunk else { break };
            for data in parser.push(&bytes) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data)
                    && let Some((incoming, reset)) = parse_event_log_message(&value)
                {
                    entries = append_event_log(&entries, &incoming, reset);
                    let _ = tx.send(entries.clone());
                }
            }
        }
    });
    rx
}

/// Reads Android foldable posture (`readAndroidFold` in `deviceFold.ts`).
pub async fn read_android_fold(credentials: Arc<Credentials>, target: DeviceTarget) -> Result<AndroidFoldState, String> {
    android_fold_request(credentials, target, None).await
}

/// Sets Android foldable posture (`setAndroidFold` in `deviceFold.ts`).
pub async fn set_android_fold(credentials: Arc<Credentials>, target: DeviceTarget, posture: FoldActionPosture) -> Result<AndroidFoldState, String> {
    android_fold_request(credentials, target, Some(posture)).await
}

async fn android_fold_request(
    credentials: Arc<Credentials>,
    target: DeviceTarget,
    posture: Option<FoldActionPosture>,
) -> Result<AndroidFoldState, String> {
    let access = hub_access(&credentials, &target.host_id).await?;
    let url = access.http_url(false, &android_fold_path(&target.device_id));
    let client = reqwest::Client::builder().timeout(OPEN_TIMEOUT).build().map_err(|_| "Fold command failed.".to_owned())?;
    let req = match posture {
        Some(posture) => client.post(&url).json(&posture.body()),
        None => client.get(&url),
    };
    let response = req.send().await.map_err(|_| "Fold command failed.".to_owned())?;
    let status = response.status();
    if !status.is_success() {
        let error = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("error").and_then(serde_json::Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| format!("Fold command failed ({}).", status.as_u16()));
        return Err(error);
    }
    let payload: serde_json::Value = response.json().await.map_err(|_| "Unexpected Android fold response.".to_owned())?;
    parse_android_fold(&payload)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use t3_core::device::stream::{SEMU_FLAG_KEY, SEMU_MAGIC, build_avcc_config};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
    use tokio_tungstenite::tungstenite::protocol::Role;

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    /// Minimal 32x16 solid-red H.264 Baseline Annex-B bitstream (SPS + PPS + IDR).
    const RED_32X16_ANNEX_B: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x0a, 0xd9, 0x0b, 0xb0, 0x11, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x32, 0x0f, 0x12, 0x26, 0x48,
        0x00, 0x00, 0x00, 0x01, 0x68, 0xcb, 0x83, 0xcb, 0x20,
        0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x0a, 0xf1, 0x18, 0xa0, 0x00, 0x24, 0x03, 0x1c, 0x00, 0x04, 0x3a, 0xe3, 0x80, 0x00, 0x97, 0xc4, 0x9e,
    ];

    /// A fake server with a device hub: a media ticket, serve-sim's MJPEG/AVCC feeds, HID socket,
    /// and serve-emu's Android WebSocket.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Live,
        /// The feed answers 401, as an expired ticket would.
        RefuseFeed,
        /// The input socket closes with 4001, as a Simlock gateway does when the lease ended.
        LeaseEnded,
        /// The AVCC feed delivers H.264 then ends with Simlock tag `7` lease-end JSON.
        AvccLeaseEnd,
        /// The HID socket advertises Duo support and replies to `0x10` Duo commands.
        Duo,
    }

    struct FakeHub {
        addr: std::net::SocketAddr,
        /// Binary messages received by the HID socket.
        received: mpsc::UnboundedReceiver<Vec<u8>>,
        /// Text messages received by the Android serve-emu socket.
        android_received: mpsc::UnboundedReceiver<String>,
        /// Every request line, in order.
        requests: Arc<Mutex<Vec<String>>>,
    }

    fn part(body: &[u8]) -> Vec<u8> {
        [format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(), body, b"\r\n"].concat()
    }

    fn avcc_frame(tag: u8, payload: &[u8]) -> Vec<u8> {
        let len = (1 + payload.len()) as u32;
        [len.to_be_bytes().as_slice(), &[tag], payload].concat()
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
        async fn start(mode: Mode) -> FakeHub {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (sent, received) = mpsc::unbounded_channel();
            let (android_sent, android_received) = mpsc::unbounded_channel();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = requests.clone();
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let (sent, android_sent, log) = (sent.clone(), android_sent.clone(), log.clone());
                    tokio::spawn(async move {
                        let Some(head) = read_head(&mut stream).await else { return };
                        log.lock().unwrap().push(head.lines().next().unwrap_or_default().to_owned());
                        if head.starts_with("POST /api/auth/websocket-ticket/media") {
                            let body = r#"{"ticket":"tick"}"#;
                            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                        } else if head.contains("/stream.mjpeg?") {
                            if mode == Mode::RefuseFeed {
                                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
                                return;
                            }
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: multipart/x-mixed-replace; boundary=frame\r\nconnection: close\r\n\r\n").await;
                            let _ = stream.write_all(&[part(b"first"), part(b"first")].concat()).await;
                            let _ = stream.write_all(&part(b"second")).await;
                            // Hold the feed open, as a still screen does.
                            let mut rest = Vec::new();
                            let _ = stream.read_to_end(&mut rest).await;
                        } else if head.contains("/stream.avcc?") {
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\nconnection: close\r\n\r\n").await;
                            let au = annex_b_to_avcc(RED_32X16_ANNEX_B);
                            let avcc_cfg = build_avcc_config(&au.sps[0], &au.pps[0]).unwrap();
                            let _ = stream.write_all(&avcc_frame(1, &avcc_cfg)).await;
                            let _ = stream.write_all(&avcc_frame(2, &au.avcc_data)).await;
                            if mode == Mode::AvccLeaseEnd {
                                let _ = stream.write_all(&avcc_frame(7, br#"{"reason":"quota reached"}"#)).await;
                                return;
                            }
                            let mut rest = Vec::new();
                            let _ = stream.read_to_end(&mut rest).await;
                        } else if head.starts_with("POST /api/device-hub/vendor/") && head.contains("/api/screenshot?") {
                            if head.contains("device=broken") {
                                let _ = stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
                                return;
                            }
                            let body = [b"WARNING: two displays\n".as_slice(), &tiny_png(), b"\n"].concat();
                            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes()).await;
                            let _ = stream.write_all(&body).await;
                        } else if head.contains("/ax?") || head.contains("/api/accessibility?") {
                            let body = if head.contains("serve-sim") {
                                r#"[{"type":"Application","frame":{"x":0,"y":0,"width":390,"height":844},"children":[{"type":"Button","AXUniqueId":"ok","AXLabel":"OK","frame":{"x":39,"y":422,"width":195,"height":42.2}}]}]"#
                            } else {
                                r#"{"nodes":[{"id":0,"bounds":{"left":0,"top":0,"right":1080,"bottom":2400}},{"id":1,"text":"Go","className":"android.widget.Button","clickable":true,"bounds":{"left":108,"top":240,"right":540,"bottom":480}}]}"#
                            };
                            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                        } else if head.contains("/appstate?") {
                            let body = "data: {\"bundleId\":\"com.acme.live\",\"pid\":99}\n\n";
                            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                        } else if head.contains("/api/event-log/events?") {
                            let body = "data: {\"events\":[{\"id\":1,\"timestamp\":\"2026-11-19T12:00:00.000Z\",\"kind\":\"tap\",\"summary\":\"Tapped OK\"}]}\n\ndata: {\"event\":{\"id\":2,\"timestamp\":\"2026-11-19T12:00:01.000Z\",\"kind\":\"nav\",\"summary\":\"Opened Home\"}}\n\n";
                            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                        } else if head.contains("/api/fold?") {
                            let body = if head.starts_with("POST ") {
                                r#"{"ok":true,"fold":{"supported":true,"posture":"closed","hingeAngle":0}}"#
                            } else {
                                r#"{"ok":true,"fold":{"supported":true,"posture":"opened","hingeAngle":180}}"#
                            };
                            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                        } else if head.contains("/vendor/serve-emu/ws?") {
                            let key = head.lines().find_map(|line| line.strip_prefix("Sec-WebSocket-Key: ").or_else(|| line.strip_prefix("sec-websocket-key: "))).unwrap_or_default().trim().to_owned();
                            let accept = derive_accept_key(key.as_bytes());
                            let _ = stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await;
                            let mut socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
                            let mut semu = Vec::new();
                            semu.extend_from_slice(&SEMU_MAGIC.to_be_bytes());
                            semu.push(1);
                            semu.push(SEMU_FLAG_KEY);
                            semu.extend_from_slice(&[0, 0]);
                            semu.extend_from_slice(&0u64.to_be_bytes());
                            semu.extend_from_slice(RED_32X16_ANNEX_B);
                            let _ = socket.send(Message::Binary(semu)).await;
                            while let Some(Ok(message)) = socket.next().await {
                                if let Message::Text(text) = message {
                                    let _ = android_sent.send(text);
                                }
                            }
                        } else if head.contains("/helper/ws?") {
                            let key = head.lines().find_map(|line| line.strip_prefix("Sec-WebSocket-Key: ").or_else(|| line.strip_prefix("sec-websocket-key: "))).unwrap_or_default().trim().to_owned();
                            let accept = derive_accept_key(key.as_bytes());
                            let _ = stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await;
                            let mut socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
                            let config = if mode == Mode::Duo {
                                [&[0x82u8][..], br#"{"width":1170,"height":2532,"orientation":"portrait","screenId":3,"supportsHingeAngle":true,"hingeAngle":180,"hingePose":"open"}"#].concat()
                            } else {
                                [&[0x82u8][..], br#"{"width":1170,"height":2532,"orientation":"landscape_left"}"#].concat()
                            };
                            let _ = socket.send(Message::Binary(config)).await;
                            let _ = socket.send(Message::Binary(vec![0x83])).await;
                            if mode == Mode::LeaseEnded {
                                let frame = tokio_tungstenite::tungstenite::protocol::CloseFrame { code: 4001.into(), reason: "lease expired".into() };
                                let _ = socket.close(Some(frame)).await;
                                return;
                            }
                            while let Some(Ok(message)) = socket.next().await {
                                if let Message::Binary(bytes) = message {
                                    if mode == Mode::Duo
                                        && bytes.first() == Some(&0x10)
                                        && let Ok(req) = serde_json::from_slice::<serde_json::Value>(&bytes[1..])
                                        && let Some(id) = req.get("requestId").and_then(serde_json::Value::as_u64)
                                    {
                                        let reply = [&[0x90u8][..], format!(r#"{{"requestId":{id},"ok":true}}"#).as_bytes()].concat();
                                        let _ = socket.send(Message::Binary(reply)).await;
                                    }
                                    let _ = sent.send(bytes);
                                }
                            }
                        }
                    });
                }
            });
            FakeHub { addr, received, android_received, requests }
        }

        fn credentials(&self) -> Arc<Credentials> {
            Arc::new(Credentials { http_base: format!("http://{}", self.addr), access_token: "token".into() })
        }
    }

    fn tiny_png() -> Vec<u8> {
        [&[137u8, 80, 78, 71, 13, 10, 26, 10][..], &[0, 0, 0, 0], b"IEND", &[0, 0, 0, 0]].concat()
    }

    async fn next_event(stream: &mut DeviceStream) -> DeviceStreamEvent {
        tokio::time::timeout(WAIT, stream.events.recv()).await.expect("an event arrives").expect("the stream runs")
    }

    #[tokio::test]
    async fn streams_newest_frames_and_sends_input_once_admitted() {
        let mut hub = FakeHub::start(Mode::Live).await;
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
                if let Some(Frame::Jpeg(bytes)) = stream.frames.borrow_and_update().clone()
                    && &bytes[..] == b"second"
                {
                    return bytes;
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

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn simlock_avcc_decodes_h264_and_stops_on_tag_7_lease_end() {
        let hub = FakeHub::start(Mode::AvccLeaseEnd).await;
        let target = DeviceTarget { host_id: "lab".into(), device_id: "SIM 1".into() };
        let options = StreamOptions { platform: Platform::Ios, host_kind: Some(HostKind::Simlock), panel_id: None };
        let mut stream = open_with_options(&tokio::runtime::Handle::current(), hub.credentials(), target, options);
        let mut saw_rgba = false;
        let detail = loop {
            if let Some(Frame::Rgba { width, height, .. }) = stream.frames.borrow_and_update().clone() {
                assert_eq!((width, height), (32, 16));
                saw_rgba = true;
            }
            if let DeviceStreamEvent::Status(StreamStatus::Error(detail)) = next_event(&mut stream).await {
                break detail;
            }
        };
        if !saw_rgba
            && let Some(Frame::Rgba { width, height, .. }) = stream.frames.borrow().clone()
        {
            assert_eq!((width, height), (32, 16));
            saw_rgba = true;
        }
        assert!(saw_rgba, "decoded RGBA frame should arrive before lease-end");
        assert_eq!(detail, "Device lease ended: quota reached");
        let avcc_requests = hub.requests.lock().unwrap().iter().filter(|line| line.contains("/stream.avcc?")).count();
        assert_eq!(avcc_requests, 1, "tag 7 lease-end halts reconnects");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn android_serve_emu_decodes_semu_annex_b_and_forwards_json_input() {
        let mut hub = FakeHub::start(Mode::Live).await;
        let target = DeviceTarget { host_id: "local".into(), device_id: "emu 1".into() };
        let options = StreamOptions { platform: Platform::Android, host_kind: Some(HostKind::Local), panel_id: None };
        let mut stream = open_with_options(&tokio::runtime::Handle::current(), hub.credentials(), target, options);
        let (mut streaming, mut screen) = (false, None);
        while !(streaming && screen.is_some()) {
            match next_event(&mut stream).await {
                DeviceStreamEvent::Status(StreamStatus::Streaming) => streaming = true,
                DeviceStreamEvent::Screen(next) => screen = Some(next),
                DeviceStreamEvent::Status(StreamStatus::Error(detail)) => panic!("android stream failed: {detail}"),
                _ => {}
            }
        }
        let screen = screen.unwrap();
        assert_eq!((screen.width, screen.height, screen.orientation), (32.0, 16.0, Orientation::LandscapeLeft));
        match stream.frames.borrow().clone() {
            Some(Frame::Rgba { width, height, bytes }) => {
                assert_eq!((width, height, bytes.len()), (32, 16, 32 * 16 * 4));
            }
            other => panic!("expected Rgba frame, got {other:?}"),
        }
        stream.input.send(DeviceInput::Touch { phase: TouchPhase::Begin, x: 0.5, y: 0.25 });
        stream.input.send(DeviceInput::Text("hello".into()));
        stream.input.send(DeviceInput::Button(HardwareButton::Back));
        let t = tokio::time::timeout(WAIT, hub.android_received.recv()).await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&t).unwrap(), serde_json::json!({ "type": "touch", "action": "down", "x": 0.5, "y": 0.25 }));
        let txt = tokio::time::timeout(WAIT, hub.android_received.recv()).await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&txt).unwrap(), serde_json::json!({ "type": "text", "text": "hello" }));
        let btn = tokio::time::timeout(WAIT, hub.android_received.recv()).await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&btn).unwrap(), serde_json::json!({ "type": "back" }));
    }

    #[tokio::test]
    async fn duo_command_round_trips_and_clears_pending_state() {
        let mut hub = FakeHub::start(Mode::Duo).await;
        let target = DeviceTarget { host_id: "local".into(), device_id: "DUO".into() };
        let mut stream = open(&tokio::runtime::Handle::current(), hub.credentials(), target);
        let (mut screen, mut connected) = (false, false);
        while !(screen && connected) {
            match next_event(&mut stream).await {
                DeviceStreamEvent::Screen(s) if s.supports_hinge_angle => screen = true,
                DeviceStreamEvent::Input { connected: true, .. } => connected = true,
                DeviceStreamEvent::Status(StreamStatus::Error(detail)) => panic!("duo stream failed: {detail}"),
                _ => {}
            }
        }
        stream.input.send(DeviceInput::Duo(DuoCommand::Angle(90.0)));
        let sent = tokio::time::timeout(WAIT, hub.received.recv()).await.unwrap().unwrap();
        assert_eq!(sent[0], 0x10);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&sent[1..]).unwrap(),
            serde_json::json!({ "requestId": 1, "command": { "control": "angle", "value": 90.0 } })
        );
        tokio::time::timeout(WAIT, async {
            loop {
                let state = stream.duo.borrow_and_update().clone();
                if !state.pending && state.error.is_none() && state.requested.is_none() {
                    return;
                }
                stream.duo.changed().await.unwrap();
            }
        })
        .await
        .expect("duo state settles after reply");
    }

    #[tokio::test]
    async fn a_refused_feed_mints_new_access_then_gives_up() {
        let hub = FakeHub::start(Mode::RefuseFeed).await;
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

    #[tokio::test]
    async fn an_ended_lease_stops_the_stream_with_its_reason_instead_of_reconnecting() {
        let hub = FakeHub::start(Mode::LeaseEnded).await;
        let target = DeviceTarget { host_id: "lab".into(), device_id: "sim".into() };
        let mut stream = open(&tokio::runtime::Handle::current(), hub.credentials(), target);
        let detail = loop {
            if let DeviceStreamEvent::Status(StreamStatus::Error(detail)) = next_event(&mut stream).await {
                break detail;
            }
        };
        assert_eq!(detail, "Device lease ended: lease expired");
        let sockets = hub.requests.lock().unwrap().iter().filter(|line| line.contains("/helper/ws?")).count();
        assert_eq!(sockets, 1, "the input socket is not retried");
    }

    #[tokio::test]
    async fn a_screenshot_is_the_png_the_hub_returns() {
        let hub = FakeHub::start(Mode::Live).await;
        let png = capture_screenshot(hub.credentials(), DeviceTarget { host_id: "local".into(), device_id: "SIM 1".into() }, Platform::Ios).await;
        assert_eq!(png, Ok(tiny_png()));
        let android = capture_screenshot(hub.credentials(), DeviceTarget { host_id: "mac".into(), device_id: "emu".into() }, Platform::Android).await;
        assert!(android.is_ok());
        let failed = capture_screenshot(hub.credentials(), DeviceTarget { host_id: "local".into(), device_id: "broken".into() }, Platform::Ios).await;
        assert_eq!(failed, Err("Screenshot capture failed (500). Try again.".to_owned()));
        let requests = hub.requests.lock().unwrap().clone();
        assert!(requests.iter().any(|line| line.starts_with("POST /api/device-hub/vendor/serve-sim/api/screenshot?device=SIM%201&wsTicket=tick&hostId=local ")), "{requests:?}");
        assert!(requests.iter().any(|line| line.starts_with("POST /api/device-hub/vendor/serve-emu/api/screenshot?device=emu&wsTicket=tick&hostId=mac ")), "{requests:?}");
    }

    #[tokio::test]
    async fn hub_endpoints_fetch_ax_tree_stream_foreground_event_log_and_fold() {
        let hub = FakeHub::start(Mode::Live).await;
        let target = DeviceTarget { host_id: "local".into(), device_id: "SIM 1".into() };

        let ios_ax = fetch_ax_tree(hub.credentials(), target.clone(), Platform::Ios).await.unwrap();
        assert_eq!(ios_ax.elements.len(), 1);
        assert_eq!(ios_ax.elements[0].label, "OK");

        let android_ax = fetch_ax_tree(hub.credentials(), DeviceTarget { host_id: "local".into(), device_id: "emu".into() }, Platform::Android).await.unwrap();
        assert_eq!(android_ax.elements.len(), 1);
        assert_eq!(android_ax.elements[0].label, "Go");

        let mut fg_rx = subscribe_foreground(&tokio::runtime::Handle::current(), hub.credentials(), target.clone());
        tokio::time::timeout(WAIT, async {
            while fg_rx.borrow_and_update().is_none() {
                fg_rx.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let fg = fg_rx.borrow().clone().unwrap().unwrap();
        assert_eq!(fg.id, "com.acme.live");
        assert_eq!(fg.pid, Some(99));

        let mut log_rx = subscribe_event_log(&tokio::runtime::Handle::current(), hub.credentials(), target.clone());
        tokio::time::timeout(WAIT, async {
            while log_rx.borrow_and_update().len() < 2 {
                log_rx.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let entries = log_rx.borrow().clone();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "Tapped OK");
        assert_eq!(entries[1].summary, "Opened Home");

        let fold = read_android_fold(hub.credentials(), DeviceTarget { host_id: "local".into(), device_id: "emu".into() }).await.unwrap();
        assert!(fold.supported);
        assert_eq!(fold.angle(), 180.0);
        let closed = set_android_fold(hub.credentials(), DeviceTarget { host_id: "local".into(), device_id: "emu".into() }, FoldActionPosture::Closed).await.unwrap();
        assert_eq!(closed.angle(), 0.0);
    }
}
