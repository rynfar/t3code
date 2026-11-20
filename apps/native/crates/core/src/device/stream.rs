//! A device's live screen and input through the hub. Ports
//! `packages/client-runtime/src/device/stream.ts` and `duoControl.ts`:
//! - serve-sim's MJPEG feed (`/helper/<device>/stream.mjpeg`), AVCC H.264 stream
//!   (`/helper/<device>[/panel/<panelId>]/stream.avcc`, `AvccDemuxer` including tag `7` Simlock
//!   lease-end frames), and HID/Duo control socket (`/helper/ws`).
//! - serve-emu's Android H.264 WebSocket (`/ws?device=...&frame-meta=1`) with SEMU packet headers,
//!   Annex-B start-code parsing and Annex-B -> 4-byte AVCC conversion + SPS/PPS extraction, and
//!   JSON touch/key/text/button/reset-video control framing.
//! - `screenAspect`, `screenRotation`, `fitDeviceFrame`, and iPhone Duo hinge/posture controls.
//!
//! A lease that ended closes the input socket with `LEASE_ENDED_CLOSE` or emits AVCC tag `7`
//! with its reason; the stream stops there with `lease_ended` instead of reconnecting
//! (`pingdotgg/t3code#35994`).

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{HostKind, Platform, encode_component};

/// `RETRY_DELAY_MS`: before reconnecting video or input.
pub const RETRY_DELAY_MS: u64 = 1_000;
/// `FIRST_FRAME_TIMEOUT_MS`.
pub const FIRST_FRAME_TIMEOUT_MS: u64 = 15_000;
/// `createDuoControl` default timeout for one hinge/posture command reply.
pub const DUO_CONTROL_TIMEOUT_MS: u64 = 5_000;

/// expo-device-hub `serve-emu` packet magic (`"SEMU"`).
pub const SEMU_MAGIC: u32 = 0x5345_4d55;
pub const SEMU_HEADER_BYTES: usize = 16;
pub const SEMU_FLAG_KEY: u8 = 1;

/// expo-device-hub 0.15.3 `serve-emu` input.js `MAX_TEXT_BYTES` limits packets to 300 UTF-8 bytes.
pub const ANDROID_MAX_TEXT_BYTES: usize = 300;
/// The pinned hub queue depth is 128; the client caps burst text chunks at 64.
pub const ANDROID_MAX_TEXT_CHUNKS: usize = 64;

pub const CONNECTING_VIDEO: &str = "Connecting video…";
pub const NO_VIDEO: &str = "No video received from the device. Reconnect to try again.";
/// What the web says when the MJPEG image fails and no WebRTC fallback exists.
pub const STREAM_FAILED: &str = "Could not receive the device stream. Reconnect to try again.";
pub const INPUT_UNAVAILABLE: &str = "Simulator input is unavailable";

const MSG_TOUCH: u8 = 0x03;
const MSG_BUTTON: u8 = 0x04;
const MSG_KEY: u8 = 0x06;
const MSG_ORIENTATION: u8 = 0x07;
const MSG_DUO_CONTROL: u8 = 0x10;
const TAG_SCREEN_CONFIG: u8 = 0x82;
const TAG_INPUT_ADMITTED: u8 = 0x83;
const TAG_CONTROL_REPLY: u8 = 0x90;

const AVCC_TAG_DESCRIPTION: u8 = 1;
const AVCC_TAG_KEYFRAME: u8 = 2;
const AVCC_TAG_DELTA: u8 = 3;
const AVCC_TAG_SEED: u8 = 4;
/// Simlock's final frame on a lease that ended, carrying `{ "reason": ... }`.
const AVCC_TAG_END: u8 = 7;

/// How a device's screen reaches this client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// serve-sim's MJPEG feed with its HID socket.
    Mjpeg,
    /// serve-sim's AVCC H.264 stream with its HID/Duo socket (Simlock, or AVCC/Duo panels).
    Avcc,
    /// serve-emu's WebSocket H.264 (SEMU / Annex-B) feed and JSON control socket (Android).
    ServeEmu,
}

/// The transport for a device given whether hardware H.264 decoding is available on this platform.
pub fn transport_with_h264(platform: Platform, host: Option<HostKind>, h264_available: bool) -> Result<Transport, &'static str> {
    match (platform, host) {
        (_, Some(HostKind::Simlock)) => {
            if h264_available {
                Ok(Transport::Avcc)
            } else {
                Err("This app cannot decode this device's stream (H.264 decoding unavailable).")
            }
        }
        (Platform::Android, _) => {
            if h264_available {
                Ok(Transport::ServeEmu)
            } else {
                Err("This app cannot decode the Android stream (H.264 decoding unavailable).")
            }
        }
        (Platform::Ios, _) => Ok(Transport::Mjpeg),
    }
}

/// The transport for a device on the current target OS (`cfg!(target_os = "macos")` provides
/// native VideoToolbox H.264 decoding).
pub fn transport(platform: Platform, host: Option<HostKind>) -> Result<Transport, &'static str> {
    transport_with_h264(platform, host, cfg!(target_os = "macos"))
}

/// `DeviceStreamStatus`, with the detail the loading view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamStatus {
    Connecting,
    Streaming,
    Error(String),
}

/// The bar shown while frames arrive but input is down.
pub fn input_disconnected_label(detail: Option<&str>) -> String {
    match detail.filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("Input disconnected ({detail}), reconnecting…"),
        None => "Input disconnected, reconnecting…".to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Portrait,
    PortraitUpsideDown,
    LandscapeLeft,
    LandscapeRight,
}

impl Orientation {
    pub fn as_str(self) -> &'static str {
        match self {
            Orientation::Portrait => "portrait",
            Orientation::PortraitUpsideDown => "portrait_upside_down",
            Orientation::LandscapeLeft => "landscape_left",
            Orientation::LandscapeRight => "landscape_right",
        }
    }

    pub fn landscape(self) -> bool {
        matches!(self, Orientation::LandscapeLeft | Orientation::LandscapeRight)
    }

    /// `rotate`: the next orientation in `IOS_ORIENTATIONS`.
    pub fn next(self) -> Orientation {
        match self {
            Orientation::Portrait => Orientation::LandscapeLeft,
            Orientation::LandscapeLeft => Orientation::PortraitUpsideDown,
            Orientation::PortraitUpsideDown => Orientation::LandscapeRight,
            Orientation::LandscapeRight => Orientation::Portrait,
        }
    }
}

/// `DuoPose` (`duoControl.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DuoPose {
    Closed,
    Book,
    Open,
    Laptop,
    Tent,
}

/// `physical` value of `DuoCommand` (`duoControl.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DuoPhysical {
    FaceUp,
    FaceDown,
}

/// `DuoCommand` (`duoControl.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "control", content = "value", rename_all = "lowercase")]
pub enum DuoCommand {
    Angle(f64),
    Pose(DuoPose),
    Table(bool),
    Physical(DuoPhysical),
    Orientation(Orientation),
}

impl DuoCommand {
    pub fn is_valid(self) -> bool {
        match self {
            DuoCommand::Angle(angle) => angle.is_finite() && (0.0..=180.0).contains(&angle),
            _ => true,
        }
    }
}

/// `DuoControlState` (`duoControl.ts`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DuoControlState {
    pub pending: bool,
    pub requested: Option<DuoCommand>,
    pub error: Option<String>,
}

/// One serialized Duo request sent to serve-sim (`{ requestId, command }`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DuoControlRequest {
    pub request_id: u32,
    pub command: DuoCommand,
}

/// A `0x90` Duo control reply from serve-sim (`controlReplySchema`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlReply {
    pub request_id: u32,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// Pure single-in-flight queue porting `createDuoControl` (`duoControl.ts`).
#[derive(Debug, Clone)]
pub struct DuoControlQueue {
    next_id: u32,
    active: Option<DuoControlRequest>,
    queued: Option<DuoCommand>,
    error: Option<String>,
}

impl Default for DuoControlQueue {
    fn default() -> Self {
        Self { next_id: 1, active: None, queued: None, error: None }
    }
}

impl DuoControlQueue {
    pub fn state(&self) -> DuoControlState {
        DuoControlState {
            pending: self.active.is_some(),
            requested: self.queued.or_else(|| self.active.map(|req| req.command)),
            error: self.error.clone(),
        }
    }

    pub fn active_request_id(&self) -> Option<u32> {
        self.active.map(|req| req.request_id)
    }

    /// Queues `command`. Returns `Some(request)` when it should be transmitted immediately.
    pub fn enqueue(&mut self, command: DuoCommand) -> Option<DuoControlRequest> {
        if !command.is_valid() {
            return None;
        }
        self.queued = Some(command);
        self.error = None;
        if self.active.is_some() { None } else { self.drain() }
    }

    /// Marks the active request failed if sending it failed immediately.
    pub fn send_failed(&mut self) {
        self.clear(Some("Device is disconnected.".to_owned()));
    }

    /// Handles a reply from serve-sim; returns the next queued request to send, if any.
    pub fn receive(&mut self, reply: &ControlReply) -> Option<DuoControlRequest> {
        let active = self.active?;
        if reply.request_id != active.request_id {
            return None;
        }
        self.active = None;
        if !reply.ok {
            let error = reply
                .error
                .clone()
                .filter(|msg| !msg.is_empty())
                .unwrap_or_else(|| "Device control failed. Its position is unknown.".to_owned());
            self.clear(Some(error));
            return None;
        }
        self.error = None;
        self.drain()
    }

    /// Fires when the active request timed out without a reply.
    pub fn timeout(&mut self, request_id: u32) -> bool {
        if self.active.is_some_and(|req| req.request_id == request_id) {
            self.clear(Some("Device control timed out. Its position is unknown.".to_owned()));
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self, error: Option<String>) {
        self.active = None;
        self.queued = None;
        self.error = error;
    }

    fn drain(&mut self) -> Option<DuoControlRequest> {
        if self.active.is_some() {
            return None;
        }
        let command = self.queued.take()?;
        let request = DuoControlRequest { request_id: self.next_id, command };
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.active = Some(request);
        Some(request)
    }
}

/// `duoFoldState`'s fold bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuoFoldShape {
    Closed,
    Half,
    Open,
}

/// Result of `duoFoldState` (`duoControl.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuoFoldState {
    pub fold: DuoFoldShape,
    pub stand: bool,
    pub phone_vertical: bool,
    pub settled: bool,
}

/// `duoScreenSettled` (`duoControl.ts`).
pub fn duo_screen_settled(screen: &Screen) -> bool {
    let angle = screen.hinge_angle.unwrap_or(if screen.screen_id == Some(1) { 0.0 } else { 180.0 });
    (screen.screen_id == Some(1)) == (angle == 0.0)
}

/// `duoFoldState` (`duoControl.ts`).
pub fn duo_fold_state(screen: &Screen) -> DuoFoldState {
    let angle = screen.hinge_angle.unwrap_or(if screen.screen_id == Some(1) { 0.0 } else { 180.0 });
    let landscape = screen.orientation.landscape();
    let fold = if angle == 0.0 {
        DuoFoldShape::Closed
    } else if angle == 180.0 {
        DuoFoldShape::Open
    } else {
        DuoFoldShape::Half
    };
    let stand = matches!(screen.hinge_pose, Some(DuoPose::Laptop | DuoPose::Tent));
    let phone_vertical = if screen.screen_id == Some(1) { !landscape } else { landscape };
    DuoFoldState { fold, stand, phone_vertical, settled: duo_screen_settled(screen) }
}

/// `duoHoldOrientation` (`duoControl.ts`).
pub fn duo_hold_orientation(vertical: bool, screen_id: Option<u32>) -> Orientation {
    if screen_id == Some(1) {
        if vertical { Orientation::Portrait } else { Orientation::LandscapeLeft }
    } else if vertical {
        Orientation::LandscapeLeft
    } else {
        Orientation::PortraitUpsideDown
    }
}

/// `DeviceScreenSize`: serve-sim's screen config (or Android's decoded frame size), in raw
/// framebuffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Screen {
    pub width: f64,
    pub height: f64,
    pub orientation: Orientation,
    #[serde(default)]
    pub input_unavailable: bool,
    #[serde(default)]
    pub screen_id: Option<u32>,
    #[serde(default)]
    pub supports_hinge_angle: bool,
    #[serde(default)]
    pub supports_physical_orientation: bool,
    #[serde(default)]
    pub hinge_angle: Option<f64>,
    #[serde(default)]
    pub hinge_pose: Option<DuoPose>,
    #[serde(default)]
    pub table_mode: Option<bool>,
    #[serde(default)]
    pub table_mode_available: Option<bool>,
}

impl Screen {
    pub fn new(width: f64, height: f64, orientation: Orientation) -> Self {
        Self {
            width,
            height,
            orientation,
            input_unavailable: false,
            screen_id: None,
            supports_hinge_angle: false,
            supports_physical_orientation: false,
            hinge_angle: None,
            hinge_pose: None,
            table_mode: None,
            table_mode_available: None,
        }
    }
}

/// Synthesizes an Android `Screen` from decoded video frame dimensions (`stream.ts:586-589`).
pub fn android_screen_for_dimensions(width: u32, height: u32) -> Screen {
    let orientation = if width > height { Orientation::LandscapeLeft } else { Orientation::Portrait };
    Screen::new(f64::from(width), f64::from(height), orientation)
}

/// What serve-sim's input socket sent.
#[derive(Debug, Clone, PartialEq)]
pub enum IosMessage {
    /// The helper accepts this viewer's input.
    InputAdmitted,
    Screen(Screen),
    /// A Duo control reply (`tag 0x90`).
    ControlReply(ControlReply),
    /// Anything malformed or unrecognized.
    Other,
}

/// Decodes a binary message from serve-sim's input socket. Malformed payloads are ignored, as
/// `decodeScreenConfig` / `decodeControlReply` drop them.
pub fn decode_ios_message(bytes: &[u8]) -> IosMessage {
    match bytes.split_first() {
        Some((&TAG_INPUT_ADMITTED, _)) => IosMessage::InputAdmitted,
        Some((&TAG_SCREEN_CONFIG, payload)) => match serde_json::from_slice::<Screen>(payload) {
            Ok(screen)
                if screen.width.is_finite()
                    && screen.height.is_finite()
                    && screen.width > 0.
                    && screen.height > 0.
                    && screen.hinge_angle.is_none_or(|angle| angle.is_finite() && (0.0..=180.0).contains(&angle)) =>
            {
                IosMessage::Screen(screen)
            }
            _ => IosMessage::Other,
        },
        Some((&TAG_CONTROL_REPLY, payload)) => match serde_json::from_slice::<ControlReply>(payload) {
            Ok(reply) => IosMessage::ControlReply(reply),
            Err(_) => IosMessage::Other,
        },
        _ => IosMessage::Other,
    }
}

fn tagged(tag: u8, payload: serde_json::Value) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(serde_json::to_vec(&payload).expect("device input serializes"));
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPhase {
    Begin,
    Move,
    End,
}

impl TouchPhase {
    fn as_str(self) -> &'static str {
        match self {
            TouchPhase::Begin => "begin",
            TouchPhase::Move => "move",
            TouchPhase::End => "end",
        }
    }

    fn android_action(self) -> &'static str {
        match self {
            TouchPhase::Begin => "down",
            TouchPhase::Move => "move",
            TouchPhase::End => "up",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    Down,
    Up,
}

/// `DeviceHardwareButton`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareButton {
    Home,
    Back,
    Recents,
    AppSwitcher,
    Power,
}

/// `rawPoint`: serve-sim streams the raw portrait framebuffer, so a rotated screen's input is
/// remapped into it. `x` and `y` are 0..1 in the displayed frame.
pub fn raw_point(screen: Option<&Screen>, x: f64, y: f64) -> (f64, f64) {
    let Some(screen) = screen.filter(|screen| screen.width <= screen.height) else { return (x, y) };
    match screen.orientation {
        Orientation::LandscapeLeft => (y, 1. - x),
        Orientation::LandscapeRight => (1. - y, x),
        Orientation::PortraitUpsideDown => (1. - x, 1. - y),
        Orientation::Portrait => (x, y),
    }
}

/// `sendTouch` for serve-sim (remapping portrait framebuffers for iOS; Simlock Android does not remap).
pub fn touch_for_platform(platform: Platform, phase: TouchPhase, x: f64, y: f64, screen: Option<&Screen>) -> Vec<u8> {
    let (x, y) = if platform == Platform::Ios { raw_point(screen, x, y) } else { (x, y) };
    tagged(MSG_TOUCH, json!({ "type": phase.as_str(), "x": x, "y": y }))
}

/// `sendTouch` for serve-sim iOS.
pub fn touch(phase: TouchPhase, x: f64, y: f64, screen: Option<&Screen>) -> Vec<u8> {
    touch_for_platform(Platform::Ios, phase, x, y, screen)
}

/// `pressButton` for serve-sim.
pub fn button(button: HardwareButton) -> Vec<u8> {
    let name = match button {
        HardwareButton::Home => "home",
        HardwareButton::Back => "back",
        HardwareButton::Recents => "recents",
        HardwareButton::AppSwitcher => "app_switcher",
        HardwareButton::Power => "lock",
    };
    tagged(MSG_BUTTON, json!({ "button": name }))
}

/// `pressButton` for serve-sim with platform filtering (Simlock serves both iOS and Android).
pub fn serve_sim_button(platform: Platform, btn: HardwareButton) -> Option<Vec<u8>> {
    let name = match btn {
        HardwareButton::Home => "home",
        HardwareButton::AppSwitcher | HardwareButton::Recents => "app_switcher",
        HardwareButton::Power => "lock",
        HardwareButton::Back if platform == Platform::Android => "back",
        HardwareButton::Back => return None,
    };
    Some(tagged(MSG_BUTTON, json!({ "button": name })))
}

/// `sendKey` for serve-sim: a DOM `code` as its HID usage. `None` for keys HID cannot name.
pub fn key(phase: KeyPhase, code: &str) -> Option<Vec<u8>> {
    let usage = hid_usage(code)?;
    let phase = if phase == KeyPhase::Down { "down" } else { "up" };
    Some(tagged(MSG_KEY, json!({ "type": phase, "usage": usage })))
}

/// `setOrientation` for serve-sim.
pub fn orientation(orientation: Orientation) -> Vec<u8> {
    tagged(MSG_ORIENTATION, json!({ "orientation": orientation.as_str() }))
}

/// Encodes one Duo control request for serve-sim's HID socket (`stream.ts:472-479`).
pub fn duo_control_frame(request_id: u32, command: DuoCommand) -> Vec<u8> {
    match command {
        DuoCommand::Orientation(value) => orientation(value),
        _ => tagged(MSG_DUO_CONTROL, json!({ "requestId": request_id, "command": command })),
    }
}

/// Hub path for serve-sim's AVCC feed, optionally targeting a fixed Duo panel (`1` = cover, `3` = inner).
pub fn avcc_stream_path(device_id: &str, panel_id: Option<u8>) -> String {
    let device = encode_component(device_id);
    match panel_id {
        Some(panel) => format!("/helper/{device}/panel/{panel}/stream.avcc"),
        None => format!("/helper/{device}/stream.avcc"),
    }
}

/// `avcCodecString` (`stream.ts:200-204`).
pub fn avc_codec_string(bytes: &[u8]) -> String {
    if bytes.len() < 4 {
        return "avc1.42E01E".to_owned();
    }
    format!("avc1.{:02x}{:02x}{:02x}", bytes[1], bytes[2], bytes[3])
}

/// Decodes a Simlock tag-`7` lease-end JSON payload (`{ "reason": "..." }`), falling back to `"ended"`.
pub fn decode_lease_end(payload: &[u8]) -> String {
    #[derive(Deserialize)]
    struct LeaseEnd {
        reason: String,
    }
    serde_json::from_slice::<LeaseEnd>(payload)
        .ok()
        .map(|end| end.reason)
        .filter(|reason| !reason.is_empty())
        .unwrap_or_else(|| "ended".to_owned())
}

/// One demuxed frame from serve-sim's AVCC HTTP stream (`AvccChunk`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvccChunk {
    /// Tag `1`: raw `avcC` (`AVCDecoderConfigurationRecord`).
    Description(Vec<u8>),
    /// Tag `2`: length-prefixed AVCC IDR keyframe access unit.
    Keyframe(Vec<u8>),
    /// Tag `3`: length-prefixed AVCC non-IDR delta access unit.
    Delta(Vec<u8>),
    /// Tag `4`: JPEG still seed frame emitted before H.264 decodes.
    Seed(Vec<u8>),
    /// Tag `7`: Simlock lease-end frame carrying `{ "reason": ... }`.
    End { reason: String },
}

/// Incremental demuxer for serve-sim's framed AVCC stream (`AvccDemuxer` in `stream.ts:272-311`):
/// 4-byte big-endian frame length followed by 1-byte tag (`1`, `2`, `3`, `4`, `7`) and payload.
#[derive(Debug, Default)]
pub struct AvccDemuxer {
    buffer: Vec<u8>,
}

impl AvccDemuxer {
    pub fn new() -> Self {
        Self { buffer: Vec::with_capacity(64 * 1024) }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<AvccChunk> {
        self.buffer.extend_from_slice(bytes);
        let mut chunks = Vec::new();
        let mut offset = 0usize;
        while self.buffer.len().saturating_sub(offset) >= 4 {
            let frame_len = u32::from_be_bytes([
                self.buffer[offset],
                self.buffer[offset + 1],
                self.buffer[offset + 2],
                self.buffer[offset + 3],
            ]) as usize;
            if self.buffer.len().saturating_sub(offset + 4) < frame_len {
                break;
            }
            if frame_len >= 1 {
                let tag = self.buffer[offset + 4];
                let payload = &self.buffer[offset + 5..offset + 4 + frame_len];
                let chunk = match tag {
                    AVCC_TAG_DESCRIPTION => Some(AvccChunk::Description(payload.to_vec())),
                    AVCC_TAG_KEYFRAME => Some(AvccChunk::Keyframe(payload.to_vec())),
                    AVCC_TAG_DELTA => Some(AvccChunk::Delta(payload.to_vec())),
                    AVCC_TAG_SEED => Some(AvccChunk::Seed(payload.to_vec())),
                    AVCC_TAG_END => Some(AvccChunk::End { reason: decode_lease_end(payload) }),
                    _ => None,
                };
                if let Some(chunk) = chunk {
                    chunks.push(chunk);
                }
            }
            offset += 4 + frame_len;
        }
        if offset > 0 {
            self.buffer.drain(..offset);
        }
        chunks
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

/// Parsed ISO/IEC 14496-15 `AVCDecoderConfigurationRecord` (`avcC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvccConfig {
    /// NAL length prefix size in bytes (`1`, `2`, or `4`).
    pub nal_length_size: u8,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
}

/// Parses a raw `avcC` (`AVCDecoderConfigurationRecord`) payload into its NAL length size, SPS,
/// and PPS parameter sets.
pub fn parse_avcc_config(bytes: &[u8]) -> Option<AvccConfig> {
    if bytes.len() < 7 || bytes[0] != 1 {
        return None;
    }
    let nal_length_size = (bytes[4] & 0x03) + 1;
    if !matches!(nal_length_size, 1 | 2 | 4) {
        return None;
    }
    let num_sps = (bytes[5] & 0x1f) as usize;
    let mut pos = 6usize;
    let mut sps = Vec::with_capacity(num_sps);
    for _ in 0..num_sps {
        if pos + 2 > bytes.len() {
            return None;
        }
        let len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        pos += 2;
        if len == 0 || pos + len > bytes.len() {
            return None;
        }
        sps.push(bytes[pos..pos + len].to_vec());
        pos += len;
    }
    if pos >= bytes.len() {
        return None;
    }
    let num_pps = bytes[pos] as usize;
    pos += 1;
    let mut pps = Vec::with_capacity(num_pps);
    for _ in 0..num_pps {
        if pos + 2 > bytes.len() {
            return None;
        }
        let len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        pos += 2;
        if len == 0 || pos + len > bytes.len() {
            return None;
        }
        pps.push(bytes[pos..pos + len].to_vec());
        pos += len;
    }
    if sps.is_empty() || pps.is_empty() {
        return None;
    }
    Some(AvccConfig { nal_length_size, sps, pps })
}

/// Builds a minimal ISO/IEC 14496-15 `avcC` record (4-byte NAL lengths) from one SPS and one PPS.
pub fn build_avcc_config(sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    if sps.len() < 4 || pps.is_empty() || sps.len() > usize::from(u16::MAX) || pps.len() > usize::from(u16::MAX) {
        return None;
    }
    let mut out = Vec::with_capacity(11 + sps.len() + pps.len());
    out.push(1); // configurationVersion
    out.push(sps[1]); // AVCProfileIndication
    out.push(sps[2]); // profile_compatibility
    out.push(sps[3]); // AVCLevelIndication
    out.push(0xff); // 6 reserved 1-bits + lengthSizeMinusOne = 3 (4 bytes)
    out.push(0xe1); // 3 reserved 1-bits + numOfSequenceParameterSets = 1
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1); // numOfPictureParameterSets = 1
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    Some(out)
}

/// Decoded header + payload of an Android `serve-emu` WebSocket binary packet (`parseSemuPacket`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemuPacket<'a> {
    pub data: &'a [u8],
    pub is_key: Option<bool>,
    pub timestamp_us: Option<u64>,
}

/// Parses an optional 16-byte `SEMU` v1 packet header (`stream.ts:206-224`).
pub fn parse_semu_packet(raw: &[u8]) -> SemuPacket<'_> {
    if raw.len() > SEMU_HEADER_BYTES {
        let magic = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
        if magic == SEMU_MAGIC && raw[4] == 1 {
            let pts = u64::from_be_bytes([raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15]]);
            return SemuPacket {
                data: &raw[SEMU_HEADER_BYTES..],
                is_key: Some((raw[5] & SEMU_FLAG_KEY) != 0),
                timestamp_us: Some(pts),
            };
        }
    }
    SemuPacket { data: raw, is_key: None, timestamp_us: None }
}

/// Returns true when a `serve-emu` text message announces a new video session (`stream.ts:226-233`).
pub fn is_video_session_message(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| value.get("type").and_then(serde_json::Value::as_str).map(|ty| ty == "video-session"))
        .unwrap_or(false)
}

/// Parsed Annex-B access unit converted to 4-byte length-prefixed AVCC for VideoToolbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnnexBAccessUnit {
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    pub is_key: bool,
    /// 4-byte big-endian length-prefixed AVCC NAL units (excluding SPS/PPS parameter sets).
    pub avcc_data: Vec<u8>,
}

/// Finds the next Annex-B start code (`00 00 00 01` or `00 00 01`) at or after `from`, returning
/// `(start_offset, code_len)`.
fn find_start_code(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 2 < buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 {
            if i + 3 < buf.len() && buf[i + 2] == 0 && buf[i + 3] == 1 {
                return Some((i, 4));
            }
            if buf[i + 2] == 1 {
                return Some((i, 3));
            }
        }
        i += 1;
    }
    None
}

/// Splits an H.264 Annex-B byte stream (`00 00 01` / `00 00 00 01`) into SPS/PPS parameter sets,
/// keyframe flag (NAL type 5), and a 4-byte length-prefixed AVCC access unit payload suitable for
/// `CMSampleBuffer` / VideoToolbox.
pub fn annex_b_to_avcc(buf: &[u8]) -> AnnexBAccessUnit {
    let mut out = AnnexBAccessUnit::default();
    let Some((mut sc_pos, mut sc_len)) = find_start_code(buf, 0) else {
        return out;
    };
    loop {
        let nal_start = sc_pos + sc_len;
        let next = find_start_code(buf, nal_start);
        let nal_end = next.map_or(buf.len(), |(pos, _)| pos);
        if nal_start < nal_end {
            let nal = &buf[nal_start..nal_end];
            let nal_type = nal[0] & 0x1f;
            match nal_type {
                7 => out.sps.push(nal.to_vec()),
                8 => out.pps.push(nal.to_vec()),
                _ => {
                    if nal_type == 5 {
                        out.is_key = true;
                    }
                    if let Ok(len) = u32::try_from(nal.len()) {
                        out.avcc_data.extend_from_slice(&len.to_be_bytes());
                        out.avcc_data.extend_from_slice(nal);
                    }
                }
            }
        }
        match next {
            Some((pos, len)) => {
                sc_pos = pos;
                sc_len = len;
            }
            None => break,
        }
    }
    out
}

/// WebSocket path for Android `serve-emu` video + input (`/ws?device=<id>&frame-meta=1`).
pub fn android_ws_path(device_id: &str) -> String {
    format!("/ws?device={}&frame-meta=1", encode_component(device_id))
}

/// `requestKeyframe` JSON message for Android `serve-emu`.
pub fn android_reset_video() -> String {
    json!({ "type": "reset-video", "ack": false }).to_string()
}

/// `sendTouch` JSON message for Android `serve-emu`.
pub fn android_touch(phase: TouchPhase, x: f64, y: f64) -> String {
    json!({ "type": "touch", "action": phase.android_action(), "x": x, "y": y }).to_string()
}

/// `pressButton` JSON message for Android `serve-emu`.
pub fn android_button(button: HardwareButton) -> String {
    let kind = match button {
        HardwareButton::Home => "home",
        HardwareButton::Back => "back",
        HardwareButton::Recents | HardwareButton::AppSwitcher => "recents",
        HardwareButton::Power => "power",
    };
    json!({ "type": kind }).to_string()
}

/// `ANDROID_KEYCODE_BY_KEY` (`stream.ts:389-402`).
pub fn android_keycode(key: &str) -> Option<u32> {
    Some(match key {
        "ArrowUp" => 19,
        "ArrowDown" => 20,
        "ArrowLeft" => 21,
        "ArrowRight" => 22,
        "Tab" => 61,
        "Enter" => 66,
        "Backspace" => 67,
        "Delete" => 112,
        "Home" => 122,
        "End" => 123,
        "PageUp" => 92,
        "PageDown" => 93,
        _ => return None,
    })
}

/// `sendKey` JSON message for Android `serve-emu` (`stream.ts:1437-1443`).
pub fn android_key(phase: KeyPhase, key: &str, ctrl: bool, meta: bool) -> Option<String> {
    if phase != KeyPhase::Down {
        return None;
    }
    if key == "Escape" {
        return Some(json!({ "type": "back" }).to_string());
    }
    if let Some(keycode) = android_keycode(key) {
        return Some(json!({ "type": "key", "keycode": keycode }).to_string());
    }
    if key.chars().count() == 1 && !ctrl && !meta {
        return Some(json!({ "type": "text", "text": key }).to_string());
    }
    None
}

/// Fallback mapping from DOM `KeyboardEvent.code` to Android `serve-emu` key messages.
pub fn android_key_from_code(phase: KeyPhase, code: &str) -> Option<String> {
    let key = match code {
        "Escape" | "Enter" | "Backspace" | "Tab" | "Delete" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "Home" | "End" | "PageUp" | "PageDown" => code,
        "Space" => " ",
        _ => return None,
    };
    android_key(phase, key, false, false)
}

/// `sendText` for Android `serve-emu` (`stream.ts:1445-1462`): chunks UTF-8 text into packets of at
/// most `ANDROID_MAX_TEXT_BYTES` (300) bytes and at most `ANDROID_MAX_TEXT_CHUNKS` (64) chunks.
pub fn android_text_chunks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chunk = String::new();
    for ch in text.chars() {
        let ch_bytes = ch.len_utf8();
        if chunk.len() + ch_bytes > ANDROID_MAX_TEXT_BYTES && !chunk.is_empty() {
            out.push(json!({ "type": "text", "text": chunk }).to_string());
            chunk.clear();
            if out.len() >= ANDROID_MAX_TEXT_CHUNKS {
                return out;
            }
        }
        chunk.push(ch);
    }
    if !chunk.is_empty() && out.len() < ANDROID_MAX_TEXT_CHUNKS {
        out.push(json!({ "type": "text", "text": chunk }).to_string());
    }
    out
}

/// `hidUsageForCode`: the USB HID keyboard usage of a DOM `KeyboardEvent.code`.
pub fn hid_usage(code: &str) -> Option<u8> {
    if let Some(letter) = code.strip_prefix("Key").filter(|rest| rest.len() == 1).and_then(|rest| rest.bytes().next()).filter(u8::is_ascii_uppercase) {
        return Some(0x04 + (letter - b'A'));
    }
    if let Some(digit) = code.strip_prefix("Digit").filter(|rest| rest.len() == 1).and_then(|rest| rest.bytes().next()).filter(u8::is_ascii_digit) {
        return Some(if digit == b'0' { 0x27 } else { 0x1e + (digit - b'1') });
    }
    Some(match code {
        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2a,
        "Tab" => 0x2b,
        "Space" => 0x2c,
        "Minus" => 0x2d,
        "Equal" => 0x2e,
        "BracketLeft" => 0x2f,
        "BracketRight" => 0x30,
        "Backslash" => 0x31,
        "Semicolon" => 0x33,
        "Quote" => 0x34,
        "Backquote" => 0x35,
        "Comma" => 0x36,
        "Period" => 0x37,
        "Slash" => 0x38,
        "Delete" => 0x4c,
        "ArrowRight" => 0x4f,
        "ArrowLeft" => 0x50,
        "ArrowDown" => 0x51,
        "ArrowUp" => 0x52,
        "NumpadDivide" => 0x54,
        "NumpadMultiply" => 0x55,
        "NumpadSubtract" => 0x56,
        "NumpadAdd" => 0x57,
        "NumpadEnter" => 0x58,
        "Numpad1" => 0x59,
        "Numpad2" => 0x5a,
        "Numpad3" => 0x5b,
        "Numpad4" => 0x5c,
        "Numpad5" => 0x5d,
        "Numpad6" => 0x5e,
        "Numpad7" => 0x5f,
        "Numpad8" => 0x60,
        "Numpad9" => 0x61,
        "Numpad0" => 0x62,
        "NumpadDecimal" => 0x63,
        "NumpadEqual" => 0x67,
        "ControlLeft" => 0xe0,
        "ShiftLeft" => 0xe1,
        "AltLeft" => 0xe2,
        "MetaLeft" => 0xe3,
        "ControlRight" => 0xe4,
        "ShiftRight" => 0xe5,
        "AltRight" => 0xe6,
        "MetaRight" => 0xe7,
        _ => return None,
    })
}

/// The close code a Simlock gateway sends when the device's lease ended.
pub const LEASE_ENDED_CLOSE: u16 = 4001;

/// What a closed input socket means: a refused ticket mints new access, an ended lease stops
/// for good (retrying would only be refused again), anything else retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    Unauthorized,
    LeaseEnded,
    Retry,
}

pub fn classify_close(code: Option<u16>) -> CloseAction {
    match code {
        Some(1008 | 4401) => CloseAction::Unauthorized,
        Some(LEASE_ENDED_CLOSE) => CloseAction::LeaseEnded,
        _ => CloseAction::Retry,
    }
}

/// `leaseEnded`: the error a stream stops with, keeping the gateway's reason.
pub fn lease_ended(reason: &str) -> String {
    let reason = if reason.is_empty() { "ended" } else { reason };
    format!("Device lease ended: {reason}")
}

/// The detail an input close reports: its reason, else its code.
pub fn close_detail(code: Option<u16>, reason: &str) -> String {
    match (reason.is_empty(), code) {
        (false, _) => reason.to_owned(),
        (true, Some(code)) if code != 1006 => format!("closed {code}"),
        _ => "input socket refused".to_owned(),
    }
}

/// `screenAspect`: width over height as displayed, a phone's before the first screen config.
pub fn screen_aspect(platform: Platform, screen: Option<&Screen>) -> f64 {
    let Some(screen) = screen else { return if platform == Platform::Ios { 9. / 19.5 } else { 9. / 20. } };
    let (long, short) = (screen.width.max(screen.height), screen.width.min(screen.height));
    if screen.orientation.landscape() { long / short } else { short / long }
}

/// `screenRotation`: degrees to turn serve-sim's raw portrait framebuffer for display.
pub fn screen_rotation(platform: Platform, screen: Option<&Screen>) -> i32 {
    let Some(screen) = screen.filter(|screen| platform == Platform::Ios && screen.width <= screen.height) else { return 0 };
    match screen.orientation {
        Orientation::LandscapeLeft => 90,
        Orientation::LandscapeRight => -90,
        Orientation::PortraitUpsideDown => 180,
        Orientation::Portrait => 0,
    }
}

/// `fitDeviceFrame`: the screen's box at `aspect`, filling the height unless that overflows the
/// width left of `right_inset`.
pub fn fit_frame(aspect: f64, width: f64, height: f64, right_inset: f64) -> (f64, f64) {
    let available = (width - right_inset).max(0.);
    if available == 0. || height <= 0. {
        return (0., 0.);
    }
    let by_height = height * aspect;
    if by_height <= available { (by_height, height) } else { (available, available / aspect) }
}

const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Simulator frames are far smaller; a larger part is broken framing, not something to keep.
const MAX_PART_BYTES: usize = 16 * 1024 * 1024;
const HEADER_END: &[u8] = b"\r\n\r\n";

/// Reads `multipart/x-mixed-replace` as the hub writes it: `--<boundary>`, headers with a
/// `Content-Length`, a blank line, the JPEG, CRLF. A part without a length runs to the next
/// boundary. Bytes it cannot frame are dropped until the next boundary.
#[derive(Debug)]
pub struct MjpegParser {
    buffer: Vec<u8>,
    /// `--<boundary>`.
    delimiter: Vec<u8>,
    /// The current part's body: where it starts and, when the headers said, how long it is.
    body: Option<(usize, Option<usize>)>,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

impl MjpegParser {
    /// `content_type` is the response's; its `boundary` parameter names the delimiter.
    pub fn new(content_type: Option<&str>) -> MjpegParser {
        let boundary = content_type
            .and_then(|value| value.split(';').find_map(|part| part.trim().strip_prefix("boundary=")))
            .map(|boundary| boundary.trim_matches('"'))
            .filter(|boundary| !boundary.is_empty())
            .unwrap_or("frame");
        let boundary = boundary.strip_prefix("--").unwrap_or(boundary);
        MjpegParser { buffer: Vec::new(), delimiter: format!("--{boundary}").into_bytes(), body: None }
    }

    /// Feeds bytes; returns the newest part that completed, if any.
    pub fn push(&mut self, chunk: &[u8]) -> Option<Vec<u8>> {
        self.buffer.extend_from_slice(chunk);
        let mut newest = None;
        loop {
            let Some((start, length)) = self.body else {
                if !self.read_headers() {
                    break;
                }
                continue;
            };
            let end = match length {
                Some(length) if self.buffer.len() >= start + length => start + length,
                Some(_) => break,
                None => match find(&self.buffer[start..], &[b"\r\n".as_slice(), &self.delimiter].concat()) {
                    Some(offset) => start + offset,
                    None => {
                        if self.buffer.len() - start > MAX_PART_BYTES {
                            self.resync();
                        }
                        break;
                    }
                },
            };
            newest = Some(self.buffer[start..end].to_vec());
            self.buffer.drain(..end);
            self.body = None;
        }
        newest
    }

    /// Finds the next part's headers; false while they have not all arrived.
    fn read_headers(&mut self) -> bool {
        let Some(at) = find(&self.buffer, &self.delimiter) else {
            // Keep a tail that could still begin a delimiter.
            let keep = self.delimiter.len().min(self.buffer.len());
            self.buffer.drain(..self.buffer.len() - keep);
            return false;
        };
        self.buffer.drain(..at);
        let Some(end) = find(&self.buffer[..self.buffer.len().min(MAX_HEADER_BYTES + HEADER_END.len())], HEADER_END) else {
            if self.buffer.len() > MAX_HEADER_BYTES {
                self.resync();
            }
            return false;
        };
        let headers = String::from_utf8_lossy(&self.buffer[..end]);
        let length = headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
        });
        if length.is_some_and(|length| length > MAX_PART_BYTES) {
            self.resync();
            return true;
        }
        self.body = Some((end + HEADER_END.len(), length));
        true
    }

    /// Drops the current part and looks for the next delimiter past it.
    fn resync(&mut self) {
        self.body = None;
        let skip = self.delimiter.len().min(self.buffer.len());
        self.buffer.drain(..skip);
    }
}
