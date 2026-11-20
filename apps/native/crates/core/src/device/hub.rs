//! Device hub auxiliary endpoints: accessibility tree overlay, live foreground app SSE,
//! event log SSE, and Android fold state/controls.
//! Ports `apps/web/src/components/device/deviceHubApi.ts`, `deviceHubAtoms.ts`, and
//! `deviceFold.ts`.

use serde_json::{Value, json};

use super::{Platform, encode_component};

pub const AX_ELEMENT_LIMIT: usize = 500;
pub const AX_POLL_INTERVAL_MS: u64 = 2_000;
pub const DEVICE_EVENT_LOG_LIMIT: usize = 100;

/// One normalized accessibility element (`DeviceAxElement`).
#[derive(Debug, Clone, PartialEq)]
pub struct AxElement {
    pub id: String,
    pub label: String,
    pub role: String,
    /// Normalized to the displayed screen: `0.0..=1.0` on both axes.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AxTree {
    pub elements: Vec<AxElement>,
    pub errors: Vec<String>,
}

/// Live foreground app info (`DeviceForegroundInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundInfo {
    pub id: String,
    pub label: Option<String>,
    pub pid: Option<u64>,
    pub is_react_native: Option<bool>,
}

/// One event log entry (`DeviceEventLogEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLogEntry {
    pub id: u64,
    pub timestamp: String,
    pub kind: String,
    pub summary: String,
}

impl EventLogEntry {
    /// `entry.timestamp.slice(11, 19)`: the `HH:MM:SS` portion of an ISO timestamp.
    pub fn short_time(&self) -> &str {
        if self.timestamp.len() >= 19 && self.timestamp.is_char_boundary(11) && self.timestamp.is_char_boundary(19) {
            &self.timestamp[11..19]
        } else {
            &self.timestamp
        }
    }
}

/// Hub path (relative to `/vendor/serve-sim` or `/vendor/serve-emu`) for the AX tree.
pub fn ax_tree_path(platform: Platform, device_id: &str) -> (bool, String) {
    match platform {
        Platform::Ios => (true, format!("/helper/{}/ax", encode_component(device_id))),
        Platform::Android => (false, format!("/api/accessibility?device={}", encode_component(device_id))),
    }
}

/// Hub path for iOS live foreground SSE (`/appstate?device=...`).
pub fn foreground_path(device_id: &str) -> String {
    format!("/appstate?device={}", encode_component(device_id))
}

/// Hub path for iOS event log SSE (`/api/event-log/events?device=...&limit=100`).
pub fn event_log_path(device_id: &str) -> String {
    format!("/api/event-log/events?device={}&limit={DEVICE_EVENT_LOG_LIMIT}", encode_component(device_id))
}

/// Hub path for Android fold GET/POST (`/api/fold?device=...`).
pub fn android_fold_path(device_id: &str) -> String {
    format!("/api/fold?device={}", encode_component(device_id))
}

fn number_or(value: Option<&Value>, fallback: f64) -> f64 {
    value.and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(fallback)
}

/// `fetchDeviceAxTree` parser for both iOS and Android payloads.
pub fn parse_ax_tree(platform: Platform, payload: &Value) -> AxTree {
    match platform {
        Platform::Ios => parse_ios_ax_tree(payload),
        Platform::Android => parse_android_ax_tree(payload),
    }
}

pub fn parse_ios_ax_tree(payload: &Value) -> AxTree {
    let Some(roots) = payload.as_array() else {
        let error = payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Unexpected accessibility payload.");
        return AxTree { elements: Vec::new(), errors: vec![error.to_owned()] };
    };
    let root_frame = roots.first().and_then(|first| first.get("frame"));
    let screen_width = number_or(root_frame.and_then(|f| f.get("width")), 1.0).max(1.0);
    let screen_height = number_or(root_frame.and_then(|f| f.get("height")), 1.0).max(1.0);
    let mut elements = Vec::new();
    fn visit(
        node: &Value,
        path: String,
        screen_width: f64,
        screen_height: f64,
        elements: &mut Vec<AxElement>,
    ) {
        if elements.len() >= AX_ELEMENT_LIMIT {
            return;
        }
        let Some(frame) = node.get("frame").filter(|v| v.is_object()) else {
            return;
        };
        let width = number_or(frame.get("width"), 0.0);
        let height = number_or(frame.get("height"), 0.0);
        let covers_screen = (width - screen_width).abs() < 0.5 && (height - screen_height).abs() < 0.5;
        if !covers_screen && width > 0.0 && height > 0.0 {
            elements.push(AxElement {
                id: node
                    .get("AXUniqueId")
                    .and_then(Value::as_str)
                    .map_or_else(|| path.clone(), str::to_owned),
                label: node.get("AXLabel").and_then(Value::as_str).unwrap_or("").to_owned(),
                role: node.get("type").and_then(Value::as_str).unwrap_or("").to_owned(),
                x: number_or(frame.get("x"), 0.0) / screen_width,
                y: number_or(frame.get("y"), 0.0) / screen_height,
                width: width / screen_width,
                height: height / screen_height,
            });
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for (index, child) in children.iter().enumerate() {
                visit(child, format!("{path}.{index}"), screen_width, screen_height, elements);
            }
        }
    }
    for (index, root) in roots.iter().enumerate() {
        visit(root, index.to_string(), screen_width, screen_height, &mut elements);
    }
    AxTree { elements, errors: Vec::new() }
}

pub fn parse_android_ax_tree(payload: &Value) -> AxTree {
    let Some(raw_nodes) = payload.get("nodes").and_then(Value::as_array) else {
        let error = payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Unexpected accessibility payload.");
        return AxTree { elements: Vec::new(), errors: vec![error.to_owned()] };
    };
    let nodes: Vec<&Value> = raw_nodes
        .iter()
        .filter(|node| node.is_object() && node.get("bounds").is_some_and(Value::is_object))
        .collect();
    let root = nodes.first().and_then(|node| node.get("bounds"));
    let screen_width = number_or(root.and_then(|b| b.get("right")), 1.0).max(1.0);
    let screen_height = number_or(root.and_then(|b| b.get("bottom")), 1.0).max(1.0);
    let mut elements = Vec::new();
    for node in nodes.into_iter().skip(1) {
        if elements.len() >= AX_ELEMENT_LIMIT {
            break;
        }
        let Some(bounds) = node.get("bounds") else { continue };
        let left = number_or(bounds.get("left"), 0.0);
        let top = number_or(bounds.get("top"), 0.0);
        let width = (number_or(bounds.get("right"), left) - left) / screen_width;
        let height = (number_or(bounds.get("bottom"), top) - top) / screen_height;
        if width >= 0.95 && height >= 0.9 {
            continue;
        }
        let text = node.get("text").and_then(Value::as_str).unwrap_or("");
        let desc = node.get("contentDescription").and_then(Value::as_str).unwrap_or("");
        let label = if !text.is_empty() { text } else { desc };
        let clickable = node.get("clickable").and_then(Value::as_bool) == Some(true);
        if label.is_empty() && !clickable {
            continue;
        }
        let class_name = node.get("className").and_then(Value::as_str).unwrap_or("");
        let role = class_name.rsplit('.').next().unwrap_or("").to_owned();
        let id = match node.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        elements.push(AxElement {
            id,
            label: label.to_owned(),
            role,
            x: left / screen_width,
            y: top / screen_height,
            width,
            height,
        });
    }
    AxTree { elements, errors: Vec::new() }
}

/// `subscribeDeviceForeground` event payload parser.
/// Returns `None` when the event is malformed/ignored, `Some(None)` when no app is in foreground,
/// or `Some(Some(info))` when a foreground bundle ID was reported.
pub fn parse_foreground_event(data: &Value) -> Option<Option<ForegroundInfo>> {
    let obj = data.as_object()?;
    match obj.get("bundleId") {
        Some(Value::Null) => Some(None),
        Some(Value::String(id)) if id.is_empty() => Some(None),
        Some(Value::String(id)) => Some(Some(ForegroundInfo {
            id: id.clone(),
            label: obj.get("label").and_then(Value::as_str).map(str::to_owned),
            pid: obj.get("pid").and_then(Value::as_u64),
            is_react_native: obj.get("isReactNative").and_then(Value::as_bool),
        })),
        _ => None,
    }
}

fn to_event_log_entry(raw: &Value) -> Option<EventLogEntry> {
    let obj = raw.as_object()?;
    let id = obj.get("id")?.as_u64()?;
    let timestamp = obj.get("timestamp").and_then(Value::as_str).unwrap_or("").to_owned();
    let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("").to_owned();
    let summary = obj
        .get("summary")
        .and_then(Value::as_str)
        .or_else(|| obj.get("msg").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned();
    Some(EventLogEntry { id, timestamp, kind, summary })
}

/// `subscribeDeviceEventLog` message parser: returns `(entries, reset)`.
pub fn parse_event_log_message(data: &Value) -> Option<(Vec<EventLogEntry>, bool)> {
    let obj = data.as_object()?;
    if let Some(events) = obj.get("events").and_then(Value::as_array) {
        let entries = events.iter().filter_map(to_event_log_entry).collect();
        return Some((entries, true));
    }
    let entry = to_event_log_entry(obj.get("event")?)?;
    Some((vec![entry], false))
}

/// `appendDeviceEventLog`: replaces on `reset`, else appends, keeping at most `DEVICE_EVENT_LOG_LIMIT`.
pub fn append_event_log(
    current: &[EventLogEntry],
    incoming: &[EventLogEntry],
    reset: bool,
) -> Vec<EventLogEntry> {
    let mut merged = if reset {
        incoming.to_vec()
    } else {
        let mut combined = Vec::with_capacity(current.len() + incoming.len());
        combined.extend_from_slice(current);
        combined.extend_from_slice(incoming);
        combined
    };
    if merged.len() > DEVICE_EVENT_LOG_LIMIT {
        let drop = merged.len() - DEVICE_EVENT_LOG_LIMIT;
        merged.drain(..drop);
    }
    merged
}

/// Incremental Server-Sent Events (`text/event-stream`) parser extracting `data:` payloads.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut events = Vec::new();
        loop {
            let split = self
                .buffer
                .find("\n\n")
                .map(|i| (i, 2))
                .into_iter()
                .chain(self.buffer.find("\r\n\r\n").map(|i| (i, 4)))
                .min_by_key(|(i, _)| *i);
            let Some((pos, sep_len)) = split else { break };
            let block = self.buffer[..pos].to_owned();
            self.buffer.drain(..pos + sep_len);
            let mut data_lines = Vec::new();
            for line in block.lines() {
                let line = line.trim_end_matches('\r');
                if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
                }
            }
            if !data_lines.is_empty() {
                events.push(data_lines.join("\n"));
            }
        }
        // Cap unbounded buffer on malformed streams.
        if self.buffer.len() > 256 * 1024 {
            self.buffer.clear();
        }
        events
    }
}

/// `AndroidFoldPosture` (`deviceFold.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldActionPosture {
    Closed,
    Opened,
}

impl FoldActionPosture {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Opened => "opened",
        }
    }

    pub fn body(self) -> Value {
        json!({ "posture": self.as_str() })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldPosture {
    Closed,
    HalfOpened,
    Opened,
    Flipped,
    Tent,
}

impl FoldPosture {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "closed" => Some(Self::Closed),
            "half_opened" => Some(Self::HalfOpened),
            "opened" => Some(Self::Opened),
            "flipped" => Some(Self::Flipped),
            "tent" => Some(Self::Tent),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Closed => "Closed",
            Self::HalfOpened => "Half-opened",
            Self::Opened => "Opened",
            Self::Flipped => "Flipped",
            Self::Tent => "Tent",
        }
    }
}

/// `AndroidFoldState` (`deviceFold.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct AndroidFoldState {
    pub supported: bool,
    pub posture: Option<FoldPosture>,
    pub hinge_angle: Option<f64>,
}

impl AndroidFoldState {
    /// `foldAngle` (`deviceHubAtoms.ts`).
    pub fn angle(&self) -> f64 {
        self.hinge_angle.unwrap_or(if self.posture == Some(FoldPosture::Closed) { 0.0 } else { 180.0 })
    }
}

/// `parseFold` (`deviceFold.ts`).
pub fn parse_android_fold(payload: &Value) -> Result<AndroidFoldState, String> {
    let err = || "Unexpected Android fold response.".to_owned();
    let obj = payload.as_object().ok_or_else(err)?;
    if obj.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(err());
    }
    let fold = obj.get("fold").and_then(Value::as_object).ok_or_else(err)?;
    let supported = fold.get("supported").and_then(Value::as_bool).ok_or_else(err)?;
    let posture = match fold.get("posture") {
        Some(Value::Null) | None => None,
        Some(Value::String(s)) => Some(FoldPosture::parse(s).ok_or_else(err)?),
        _ => return Err(err()),
    };
    let hinge_angle = match fold.get("hingeAngle") {
        Some(Value::Null) | None => None,
        Some(Value::Number(n)) => {
            let v = n.as_f64().filter(|v| v.is_finite()).ok_or_else(err)?;
            Some(v)
        }
        _ => return Err(err()),
    };
    Ok(AndroidFoldState { supported, posture, hinge_angle })
}
