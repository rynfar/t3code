//! A device's live screen and input through the hub. Ports the iOS half of
//! `packages/client-runtime/src/device/stream.ts` as the client runs it with `preferMjpeg`: the
//! picture is serve-sim's MJPEG feed (`/helper/<device>/stream.mjpeg`), input is serve-sim's
//! HID socket (`/helper/ws`) with its tagged JSON messages (touch, button, key, orientation; the
//! screen config and input-admitted replies), `rawPoint`, the rotation cycle and the HID usage
//! table. The browser's `<img>` parses the multipart feed itself; native reads it with
//! `MjpegParser`, following the framing `apps/server/src/device/mjpegStillFrame.ts` documents.
//! Also `screenAspect` and `screenRotation` from `DeviceStreamView.tsx` and `fitDeviceFrame`
//! from `deviceFrameLayout.ts`.
//!
//! Android (serve-emu) and Simlock hosts stream only H.264 (or WebRTC), which this client cannot
//! decode yet; `transport` says so in the web's words for a browser without WebCodecs.

use serde::Deserialize;
use serde_json::json;

use super::{HostKind, Platform};

/// `RETRY_DELAY_MS`: before reconnecting video or input.
pub const RETRY_DELAY_MS: u64 = 1_000;
/// `FIRST_FRAME_TIMEOUT_MS`.
pub const FIRST_FRAME_TIMEOUT_MS: u64 = 15_000;

pub const CONNECTING_VIDEO: &str = "Connecting video…";
pub const NO_VIDEO: &str = "No video received from the device. Reconnect to try again.";
/// What the web says when the MJPEG image fails and no WebRTC fallback exists.
pub const STREAM_FAILED: &str = "Could not receive the device stream. Reconnect to try again.";
pub const INPUT_UNAVAILABLE: &str = "Simulator input is unavailable";

const MSG_TOUCH: u8 = 0x03;
const MSG_BUTTON: u8 = 0x04;
const MSG_KEY: u8 = 0x06;
const MSG_ORIENTATION: u8 = 0x07;
const TAG_SCREEN_CONFIG: u8 = 0x82;
const TAG_INPUT_ADMITTED: u8 = 0x83;

/// How a device's screen reaches this client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// serve-sim's MJPEG feed with its HID socket.
    Mjpeg,
}

/// The transport for a device, or why there is none (`start`'s WebCodecs failures).
pub fn transport(platform: Platform, host: Option<HostKind>) -> Result<Transport, &'static str> {
    match (platform, host) {
        (_, Some(HostKind::Simlock)) => Err("This app cannot decode this device's stream (H.264 decoding unavailable)."),
        (Platform::Android, _) => Err("This app cannot decode the Android stream (H.264 decoding unavailable)."),
        (Platform::Ios, _) => Ok(Transport::Mjpeg),
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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

    fn landscape(self) -> bool {
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

/// `DeviceScreenSize`: serve-sim's screen config, in raw framebuffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Screen {
    pub width: f64,
    pub height: f64,
    pub orientation: Orientation,
    #[serde(default)]
    pub input_unavailable: bool,
}

/// What serve-sim's input socket sent.
#[derive(Debug, Clone, PartialEq)]
pub enum IosMessage {
    /// The helper accepts this viewer's input.
    InputAdmitted,
    Screen(Screen),
    /// A Duo control reply, or anything this client does not read.
    Other,
}

/// Decodes a binary message from the input socket. A malformed screen config is ignored, as
/// `decodeScreenConfig` drops it.
pub fn decode_ios_message(bytes: &[u8]) -> IosMessage {
    match bytes.split_first() {
        Some((&TAG_INPUT_ADMITTED, _)) => IosMessage::InputAdmitted,
        Some((&TAG_SCREEN_CONFIG, payload)) => match serde_json::from_slice::<Screen>(payload) {
            Ok(screen) if screen.width.is_finite() && screen.height.is_finite() && screen.width > 0. && screen.height > 0. => IosMessage::Screen(screen),
            _ => IosMessage::Other,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    Down,
    Up,
}

/// `DeviceHardwareButton`, as far as an iOS rail sends them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareButton {
    Home,
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

/// `sendTouch` for serve-sim.
pub fn touch(phase: TouchPhase, x: f64, y: f64, screen: Option<&Screen>) -> Vec<u8> {
    let (x, y) = raw_point(screen, x, y);
    tagged(MSG_TOUCH, json!({ "type": phase.as_str(), "x": x, "y": y }))
}

/// `pressButton` for serve-sim.
pub fn button(button: HardwareButton) -> Vec<u8> {
    let name = match button {
        HardwareButton::Home => "home",
        HardwareButton::AppSwitcher => "app_switcher",
        HardwareButton::Power => "lock",
    };
    tagged(MSG_BUTTON, json!({ "button": name }))
}

/// `sendKey` for serve-sim: a DOM `code` as its HID usage. `None` for keys HID cannot name.
pub fn key(phase: KeyPhase, code: &str) -> Option<Vec<u8>> {
    let usage = hid_usage(code)?;
    let phase = if phase == KeyPhase::Down { "down" } else { "up" };
    Some(tagged(MSG_KEY, json!({ "type": phase, "usage": usage })))
}

/// `setOrientation`.
pub fn orientation(orientation: Orientation) -> Vec<u8> {
    tagged(MSG_ORIENTATION, json!({ "orientation": orientation.as_str() }))
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

/// What a closed input socket means: a refused ticket mints new access, anything else retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    Unauthorized,
    Retry,
}

pub fn classify_close(code: Option<u16>) -> CloseAction {
    match code {
        Some(1008 | 4401) => CloseAction::Unauthorized,
        _ => CloseAction::Retry,
    }
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
