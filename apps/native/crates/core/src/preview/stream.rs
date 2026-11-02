//! The preview stream: one WebSocket per viewed tab at `/api/preview-stream/ws` carrying JPEG
//! frames (each acknowledged) and JSON messages one way, input the other. Ports
//! `packages/client-runtime/src/preview/serverBrowserStream.ts` (messages, input, control,
//! modifiers, close codes, retry backoff, host setup) and the viewer arithmetic of
//! `apps/web/src/browser/ServerBrowserSurface.tsx` (`pagePoint`, `placeAgentCursor`, the frame cap
//! and the key events it sends).

use serde::Serialize;
use serde_json::Value;

pub const STREAM_PATH: &str = "/api/preview-stream/ws";
pub const ACK_MESSAGE: &str = r#"{"type":"ack"}"#;
/// `TAB_GONE_CODE`: the tab closed; stop.
pub const GONE_CLOSE_CODE: u16 = 4404;
/// `PREVIEW_STREAM_HOST_SETUP_CLOSE_CODE`: the host cannot start its browser.
pub const HOST_SETUP_CLOSE_CODE: u16 = 4503;
/// `MAX_UNAUTHORIZED_REFUSALS`: refused upgrades before the viewer offers Reconnect.
pub const MAX_UNAUTHORIZED_REFUSALS: u32 = 3;
/// `RESIZE_THROTTLE_MS`.
pub const RESIZE_THROTTLE_MS: u64 = 150;
/// `WHEEL_LINE_PX`: line-mode wheel deltas in page pixels.
pub const WHEEL_LINE_PX: f64 = 16.;
/// `CURSOR_ACTIVE_MS`: the agent cursor stays bright this long after each move.
pub const AGENT_CURSOR_ACTIVE_MS: u64 = 700;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Controller {
    Agent,
    You,
    AnotherViewer,
    Unclaimed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub kind: String,
    pub message: String,
    pub default_value: String,
}

/// `PreviewStreamControl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    pub can_operate: bool,
    pub controller: Controller,
    pub generation: u64,
    pub dialog: Option<Dialog>,
}

impl Control {
    pub fn is_yours(&self) -> bool {
        self.controller == Controller::You
    }
}

/// `previewStreamControlLabel`.
pub fn control_label(control: Option<&Control>) -> &'static str {
    match control {
        None => "Connecting...",
        Some(control) if !control.can_operate => "Read-only",
        Some(control) => match control.controller {
            Controller::You => "You have control",
            Controller::Agent => "Agent has control",
            Controller::AnotherViewer => "Another viewer has control",
            Controller::Unclaimed => "Watching",
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pointer {
    pub click: bool,
    pub x: f64,
    pub y: f64,
    pub sequence: u64,
}

/// A text message from the server. Anything malformed or new is `Unknown` and ignored.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamMessage {
    Viewport(Viewport),
    Control(Control),
    Pointer(Pointer),
    Popup { tab_id: String },
    Clipboard { text: String },
    Download { id: String, file_name: String, size_bytes: f64 },
    FileChooser { id: String, multiple: bool, accept: String },
    FileChooserClosed { id: String },
    Unknown,
}

fn dialog_of(value: Option<&Value>) -> Option<Option<Dialog>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(dialog) => Some(Some(Dialog {
            kind: dialog.get("type")?.as_str()?.to_owned(),
            message: dialog.get("message")?.as_str()?.to_owned(),
            default_value: dialog.get("defaultValue")?.as_str()?.to_owned(),
        })),
    }
}

impl StreamMessage {
    pub fn decode(text: &str) -> StreamMessage {
        let Ok(message) = serde_json::from_str::<Value>(text) else { return StreamMessage::Unknown };
        Self::from_value(&message).unwrap_or(StreamMessage::Unknown)
    }

    fn from_value(message: &Value) -> Option<StreamMessage> {
        let string = |name: &str| message.get(name).and_then(Value::as_str).map(str::to_owned);
        let number = |name: &str| message.get(name).and_then(Value::as_f64);
        let boolean = |name: &str| message.get(name).and_then(Value::as_bool);
        Some(match message.get("type")?.as_str()? {
            "viewport" => StreamMessage::Viewport(Viewport { width: number("width")?, height: number("height")? }),
            "control" => {
                let controller = match message.get("controller")?.as_str()? {
                    "agent" => Controller::Agent,
                    "you" => Controller::You,
                    "another-viewer" => Controller::AnotherViewer,
                    "unclaimed" => Controller::Unclaimed,
                    _ => return None,
                };
                StreamMessage::Control(Control {
                    can_operate: boolean("canOperate")?,
                    controller,
                    generation: number("generation")? as u64,
                    dialog: dialog_of(message.get("dialog"))?,
                })
            }
            "pointer" => {
                let click = match message.get("phase")?.as_str()? {
                    "move" => false,
                    "click" => true,
                    _ => return None,
                };
                StreamMessage::Pointer(Pointer { click, x: number("x")?, y: number("y")?, sequence: number("sequence")? as u64 })
            }
            "popup" => StreamMessage::Popup { tab_id: string("tabId")? },
            "clipboard" => StreamMessage::Clipboard { text: string("text")? },
            "download" => StreamMessage::Download { id: string("id")?, file_name: string("fileName")?, size_bytes: number("sizeBytes")? },
            "fileChooser" => StreamMessage::FileChooser { id: string("id")?, multiple: boolean("multiple")?, accept: string("accept")? },
            "fileChooserClosed" => StreamMessage::FileChooserClosed { id: string("id")? },
            _ => StreamMessage::Unknown,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseAction {
    Move,
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    None,
    Left,
    Middle,
    Right,
}

impl MouseButton {
    /// The `buttons` bit DOM pointer events use: left 1, right 2, middle 4.
    pub fn bit(self) -> u8 {
        match self {
            MouseButton::None => 0,
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyAction {
    Down,
    Up,
}

/// `PreviewStreamInput`. Coordinates are page CSS px.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum StreamInput {
    TakeControl,
    ReleaseControl,
    #[serde(rename_all = "camelCase")]
    Dialog {
        accept: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_text: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Mouse { action: MouseAction, x: f64, y: f64, button: MouseButton, buttons: u8, click_count: u32, modifiers: u8 },
    #[serde(rename_all = "camelCase")]
    Wheel { x: f64, y: f64, delta_x: f64, delta_y: f64, modifiers: u8 },
    #[serde(rename_all = "camelCase")]
    Key {
        action: KeyAction,
        key: String,
        code: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        key_code: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        modifiers: u8,
    },
    Text { text: String },
    Resize { width: u32, height: u32 },
    Navigate { url: String },
    History { delta: i8 },
    #[serde(rename_all = "camelCase")]
    Reload {
        #[serde(skip_serializing_if = "Option::is_none")]
        ignore_cache: Option<bool>,
    },
}

impl StreamInput {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("stream input serializes")
    }
}

/// The client's gate in `send`: only an operating viewer sends, and only the controller sends
/// anything but a request for control.
pub fn may_send(control: Option<&Control>, input: &StreamInput) -> bool {
    control.is_some_and(|control| control.can_operate && (matches!(input, StreamInput::TakeControl) || control.is_yours()))
}

/// `previewStreamModifiers`: alt 1, ctrl 2, meta 4, shift 8.
pub fn modifiers(alt: bool, ctrl: bool, meta: bool, shift: bool) -> u8 {
    u8::from(alt) | u8::from(ctrl) << 1 | u8::from(meta) << 2 | u8::from(shift) << 3
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSetup {
    /// `sandbox` or `libraries`.
    pub need: String,
    pub command: String,
}

impl HostSetup {
    /// `decodeHostSetup`: the close reason's JSON, or the sandbox default.
    pub fn decode(reason: &str) -> HostSetup {
        serde_json::from_str::<Value>(reason)
            .ok()
            .and_then(|value| {
                let need = value.get("need")?.as_str().filter(|need| *need == "sandbox" || *need == "libraries")?.to_owned();
                Some(HostSetup { need, command: value.get("command")?.as_str()?.to_owned() })
            })
            .unwrap_or_else(|| HostSetup { need: "sandbox".to_owned(), command: "sudo npx t3 browser setup".to_owned() })
    }

    /// `previewStreamHostSetupMessage`.
    pub fn message(&self) -> &'static str {
        if self.need == "sandbox" {
            "This server's host blocks the sandbox its browser runs in. Run this once on the host, then try again:"
        } else {
            "This server's host is missing libraries its browser needs. Run this once on the host, then try again:"
        }
    }
}

/// What a closed socket means for the next connection (the `close` listener).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseAction {
    /// The tab is gone; stop for good.
    Gone,
    HostSetup(HostSetup),
    /// The upgrade was refused; mint new access before trying again.
    Unauthorized,
    Retry,
}

/// A refused upgrade never opens; the browser reports it as 1006.
pub fn classify_close(code: Option<u16>, reason: &str, opened: bool) -> CloseAction {
    match code {
        Some(GONE_CLOSE_CODE) => CloseAction::Gone,
        Some(HOST_SETUP_CLOSE_CODE) => CloseAction::HostSetup(HostSetup::decode(reason)),
        Some(1008 | 4401) => CloseAction::Unauthorized,
        Some(1006) | None if !opened => CloseAction::Unauthorized,
        _ => CloseAction::Retry,
    }
}

/// `Math.min(500 * 2 ** failures, 10_000)`.
pub fn reconnect_delay_ms(failures: u32) -> u64 {
    500u64.saturating_mul(1u64 << failures.min(20)).min(10_000)
}

/// Delay before minting new access after the `refusals`-th refused upgrade.
pub fn refusal_delay_ms(refusals: u32) -> u64 {
    if refusals <= 1 { 0 } else { 1_000u64.saturating_mul(1u64 << (refusals - 1).min(20)) }
}

/// Where a frame draws inside the viewer: scaled to fit (`object-contain`) and centered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// `object-contain` of a `frame` (device px) in a `width`×`height` box (logical px).
pub fn fit(box_size: (f64, f64), frame: (u32, u32)) -> Option<Fit> {
    let (frame_width, frame_height) = (f64::from(frame.0), f64::from(frame.1));
    if !(frame_width > 0. && frame_height > 0. && box_size.0 > 0. && box_size.1 > 0.) {
        return None;
    }
    let scale = (box_size.0 / frame_width).min(box_size.1 / frame_height);
    let (width, height) = (frame_width * scale, frame_height * scale);
    Some(Fit { left: (box_size.0 - width) / 2., top: (box_size.1 - height) / 2., width, height })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePoint {
    pub x: f64,
    pub y: f64,
    /// Page px per viewer px, for touch-style deltas.
    pub scale: f64,
}

/// `pagePoint`: a viewer position (relative to the viewer's top-left) in page CSS px. Outside
/// the drawn frame it is `None` unless `clamp`, which pins drags that leave the frame.
pub fn page_point(box_size: (f64, f64), frame: (u32, u32), viewport: Viewport, at: (f64, f64), clamp: bool) -> Option<PagePoint> {
    let fit = fit(box_size, frame)?;
    let scale = viewport.width / fit.width;
    let x = (at.0 - fit.left) * scale;
    let y = (at.1 - fit.top) * (viewport.height / fit.height);
    let inside = x >= 0. && y >= 0. && x <= viewport.width && y <= viewport.height;
    if !inside && !clamp {
        return None;
    }
    Some(PagePoint { x: x.clamp(0., viewport.width), y: y.clamp(0., viewport.height), scale })
}

/// `placeAgentCursor`: where the agent's pointer draws in the viewer.
pub fn cursor_position(box_size: (f64, f64), frame: (u32, u32), viewport: Viewport, pointer: &Pointer) -> Option<(f64, f64)> {
    let fit = fit(box_size, frame)?;
    if !(viewport.width > 0. && viewport.height > 0.) {
        return None;
    }
    Some((fit.left + pointer.x * fit.width / viewport.width, fit.top + pointer.y * fit.height / viewport.height))
}

/// `growCap`: the largest frame the server may send, in device px. It only grows, because a
/// new cap reconnects the stream; `None` keeps the current one.
pub fn grow_cap(size: (f64, f64), scale_factor: f64, current: Option<(u32, u32)>) -> Option<(u32, u32)> {
    let width = (size.0 * scale_factor).round().max(1.) as u32;
    let height = (size.1 * scale_factor).round().max(1.) as u32;
    match current {
        Some((current_width, current_height)) if current_width >= width && current_height >= height => None,
        Some((current_width, current_height)) => Some((width.max(current_width), height.max(current_height))),
        None => Some((width, height)),
    }
}

/// A DOM key event's identity, as `handleKey` sends it from a keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomKey {
    pub key: String,
    pub code: String,
    pub key_code: u32,
    /// Only for printable keys without Ctrl or Meta; Enter carries "\r" so forms submit.
    pub text: Option<String>,
}

fn named(key: &str) -> Option<(&'static str, &'static str, u32)> {
    Some(match key {
        "enter" => ("Enter", "Enter", 13),
        "tab" => ("Tab", "Tab", 9),
        "backspace" => ("Backspace", "Backspace", 8),
        "delete" => ("Delete", "Delete", 46),
        "escape" => ("Escape", "Escape", 27),
        "left" => ("ArrowLeft", "ArrowLeft", 37),
        "up" => ("ArrowUp", "ArrowUp", 38),
        "right" => ("ArrowRight", "ArrowRight", 39),
        "down" => ("ArrowDown", "ArrowDown", 40),
        "home" => ("Home", "Home", 36),
        "end" => ("End", "End", 35),
        "pageup" => ("PageUp", "PageUp", 33),
        "pagedown" => ("PageDown", "PageDown", 34),
        "insert" => ("Insert", "Insert", 45),
        _ => return None,
    })
}

fn punctuation(key: char) -> Option<(&'static str, u32)> {
    Some(match key {
        '-' => ("Minus", 189),
        '=' => ("Equal", 187),
        '[' => ("BracketLeft", 219),
        ']' => ("BracketRight", 221),
        '\\' => ("Backslash", 220),
        ';' => ("Semicolon", 186),
        '\'' => ("Quote", 222),
        ',' => ("Comma", 188),
        '.' => ("Period", 190),
        '/' => ("Slash", 191),
        '`' => ("Backquote", 192),
        _ => return None,
    })
}

/// Maps a GPUI keystroke (`key` is the unmodified key name, `key_char` what it types) to the DOM
/// key event the server replays. `None` for modifier-only and unmapped keys.
pub fn dom_key(key: &str, key_char: Option<&str>, shortcut: bool) -> Option<DomKey> {
    if let Some((name, code, key_code)) = named(key) {
        let text = (key == "enter" && !shortcut).then(|| "\r".to_owned());
        return Some(DomKey { key: name.to_owned(), code: code.to_owned(), key_code, text });
    }
    if let Some(number) = key.strip_prefix('f').and_then(|rest| rest.parse::<u32>().ok()).filter(|n| (1..=24).contains(n)) {
        let name = format!("F{number}");
        return Some(DomKey { key: name.clone(), code: name, key_code: 111 + number, text: None });
    }
    let typed = key_char.filter(|typed| typed.chars().count() == 1);
    if key == "space" {
        let key = typed.unwrap_or(" ").to_owned();
        return Some(DomKey { text: (!shortcut).then(|| key.clone()), key, code: "Space".to_owned(), key_code: 32 });
    }
    let mut chars = key.chars();
    let (Some(base), None) = (chars.next(), chars.next()) else { return None };
    let (code, key_code) = match base {
        'a'..='z' => (format!("Key{}", base.to_ascii_uppercase()), u32::from(base.to_ascii_uppercase())),
        '0'..='9' => (format!("Digit{base}"), u32::from(base)),
        other => {
            let (code, key_code) = punctuation(other)?;
            (code.to_owned(), key_code)
        }
    };
    let dom = typed.map_or_else(|| base.to_string(), str::to_owned);
    Some(DomKey { text: (!shortcut).then(|| dom.clone()), key: dom, code, key_code })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_messages_as_the_web_client_reads_them() {
        assert_eq!(StreamMessage::decode(r#"{"type":"viewport","width":800,"height":600}"#), StreamMessage::Viewport(Viewport { width: 800., height: 600. }));
        let control = StreamMessage::decode(r#"{"type":"control","canOperate":true,"controller":"agent","generation":3,"dialog":{"type":"prompt","message":"Name?","defaultValue":"x"}}"#);
        assert_eq!(
            control,
            StreamMessage::Control(Control {
                can_operate: true,
                controller: Controller::Agent,
                generation: 3,
                dialog: Some(Dialog { kind: "prompt".into(), message: "Name?".into(), default_value: "x".into() }),
            })
        );
        assert!(matches!(StreamMessage::decode(r#"{"type":"control","canOperate":true,"controller":"you","generation":1,"dialog":null}"#), StreamMessage::Control(Control { dialog: None, .. })));
        // A bad field drops the message instead of guessing.
        assert_eq!(StreamMessage::decode(r#"{"type":"control","canOperate":true,"controller":"robot","generation":1,"dialog":null}"#), StreamMessage::Unknown);
        assert_eq!(StreamMessage::decode(r#"{"type":"pointer","phase":"click","x":4,"y":5,"sequence":9}"#), StreamMessage::Pointer(Pointer { click: true, x: 4., y: 5., sequence: 9 }));
        assert_eq!(StreamMessage::decode(r#"{"type":"popup","tabId":"t2"}"#), StreamMessage::Popup { tab_id: "t2".into() });
        assert_eq!(StreamMessage::decode(r#"{"type":"somethingNew"}"#), StreamMessage::Unknown);
        assert_eq!(StreamMessage::decode("not json"), StreamMessage::Unknown);
    }

    #[test]
    fn encodes_input_in_the_wire_shape() {
        assert_eq!(StreamInput::TakeControl.encode(), r#"{"type":"takeControl"}"#);
        assert_eq!(
            StreamInput::Mouse { action: MouseAction::Down, x: 1.5, y: 2., button: MouseButton::Left, buttons: 1, click_count: 2, modifiers: 8 }.encode(),
            r#"{"type":"mouse","action":"down","x":1.5,"y":2.0,"button":"left","buttons":1,"clickCount":2,"modifiers":8}"#
        );
        assert_eq!(
            StreamInput::Key { action: KeyAction::Up, key: "a".into(), code: "KeyA".into(), key_code: Some(65), text: None, modifiers: 0 }.encode(),
            r#"{"type":"key","action":"up","key":"a","code":"KeyA","keyCode":65,"modifiers":0}"#
        );
        assert_eq!(StreamInput::Wheel { x: 0., y: 0., delta_x: 0., delta_y: 3., modifiers: 0 }.encode(), r#"{"type":"wheel","x":0.0,"y":0.0,"deltaX":0.0,"deltaY":3.0,"modifiers":0}"#);
        assert_eq!(StreamInput::Reload { ignore_cache: None }.encode(), r#"{"type":"reload"}"#);
        assert_eq!(StreamInput::Dialog { accept: true, prompt_text: Some("x".into()) }.encode(), r#"{"type":"dialog","accept":true,"promptText":"x"}"#);
        assert_eq!(StreamInput::History { delta: -1 }.encode(), r#"{"type":"history","delta":-1}"#);
    }

    #[test]
    fn only_the_controller_sends_input() {
        let control = |controller, can_operate| Control { can_operate, controller, generation: 0, dialog: None };
        let navigate = StreamInput::Navigate { url: "https://example.com/".into() };
        assert!(!may_send(None, &StreamInput::TakeControl));
        assert!(may_send(Some(&control(Controller::Agent, true)), &StreamInput::TakeControl));
        assert!(!may_send(Some(&control(Controller::Agent, true)), &navigate));
        assert!(may_send(Some(&control(Controller::You, true)), &navigate));
        assert!(!may_send(Some(&control(Controller::You, false)), &navigate));
        assert_eq!(control_label(None), "Connecting...");
        assert_eq!(control_label(Some(&control(Controller::You, false))), "Read-only");
        assert_eq!(control_label(Some(&control(Controller::Unclaimed, true))), "Watching");
    }

    #[test]
    fn close_codes_decide_the_next_step() {
        assert_eq!(classify_close(Some(4404), "tab closed", true), CloseAction::Gone);
        assert_eq!(classify_close(Some(4401), "", true), CloseAction::Unauthorized);
        assert_eq!(classify_close(None, "", false), CloseAction::Unauthorized);
        assert_eq!(classify_close(Some(1006), "", true), CloseAction::Retry);
        assert_eq!(classify_close(Some(1012), "reconnect", true), CloseAction::Retry);
        let CloseAction::HostSetup(setup) = classify_close(Some(4503), r#"{"need":"libraries","command":"npx t3 browser setup"}"#, false) else { panic!() };
        assert_eq!(setup.command, "npx t3 browser setup");
        assert!(setup.message().contains("missing libraries"));
        assert_eq!(HostSetup::decode("garbage").command, "sudo npx t3 browser setup");
        assert_eq!((reconnect_delay_ms(0), reconnect_delay_ms(2), reconnect_delay_ms(9)), (500, 2_000, 10_000));
        assert_eq!((refusal_delay_ms(1), refusal_delay_ms(2), refusal_delay_ms(3)), (0, 2_000, 4_000));
    }

    #[test]
    fn viewer_points_map_into_page_pixels() {
        // A 2x frame of a 400x300 page drawn letterboxed in a 500x300 box: 400 wide, 50 each side.
        let viewport = Viewport { width: 400., height: 300. };
        let frame = (800, 600);
        let point = page_point((500., 300.), frame, viewport, (250., 150.), false).unwrap();
        assert_eq!((point.x, point.y, point.scale), (200., 150., 1.));
        assert_eq!(page_point((500., 300.), frame, viewport, (20., 150.), false), None);
        let clamped = page_point((500., 300.), frame, viewport, (20., 400.), true).unwrap();
        assert_eq!((clamped.x, clamped.y), (0., 300.));
        // Half-size viewer: one viewer px is two page px.
        let small = page_point((200., 150.), frame, viewport, (100., 75.), false).unwrap();
        assert_eq!((small.x, small.y, small.scale), (200., 150., 2.));
        let cursor = cursor_position((500., 300.), frame, viewport, &Pointer { click: false, x: 400., y: 0., sequence: 1 }).unwrap();
        assert_eq!(cursor, (450., 0.));
        assert_eq!(page_point((500., 300.), (0, 0), viewport, (1., 1.), false), None);
    }

    #[test]
    fn the_frame_cap_only_grows() {
        assert_eq!(grow_cap((400., 300.), 2., None), Some((800, 600)));
        assert_eq!(grow_cap((300., 200.), 2., Some((800, 600))), None);
        assert_eq!(grow_cap((500., 200.), 2., Some((800, 600))), Some((1000, 600)));
    }

    #[test]
    fn keystrokes_become_dom_key_events() {
        let key = |key: &str, typed: Option<&str>, shortcut| dom_key(key, typed, shortcut).map(|dom| (dom.key, dom.code, dom.key_code, dom.text));
        assert_eq!(key("a", Some("a"), false), Some(("a".into(), "KeyA".into(), 65, Some("a".into()))));
        assert_eq!(key("a", Some("A"), false), Some(("A".into(), "KeyA".into(), 65, Some("A".into()))));
        assert_eq!(key("c", None, true), Some(("c".into(), "KeyC".into(), 67, None)));
        assert_eq!(key("1", Some("!"), false), Some(("!".into(), "Digit1".into(), 49, Some("!".into()))));
        assert_eq!(key("/", Some("?"), false), Some(("?".into(), "Slash".into(), 191, Some("?".into()))));
        assert_eq!(key("enter", None, false), Some(("Enter".into(), "Enter".into(), 13, Some("\r".into()))));
        assert_eq!(key("enter", None, true), Some(("Enter".into(), "Enter".into(), 13, None)));
        assert_eq!(key("left", None, false), Some(("ArrowLeft".into(), "ArrowLeft".into(), 37, None)));
        assert_eq!(key("space", Some(" "), false), Some((" ".into(), "Space".into(), 32, Some(" ".into()))));
        assert_eq!(key("f5", None, false), Some(("F5".into(), "F5".into(), 116, None)));
        assert_eq!(key("shift", None, false), None);
        assert_eq!(modifiers(true, false, true, true), 13);
    }
}
