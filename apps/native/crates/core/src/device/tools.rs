//! A device's settings and tools, run on the machine that hosts it. Wire types port
//! `packages/contracts/src/device.ts` (`DeviceSettings`, `DeviceForegroundApp`, `DeviceDetail`,
//! `DeviceDetailInput` and `DeviceActionInput`). `Controls` ports
//! `apps/web/src/components/device/useDeviceControls.ts` with the read lane of
//! `apps/web/src/state/device.ts` (`deviceDetailAtom`, `deviceDetailReadAtom`). The rail's rules
//! port `DeviceControlsRail.tsx` and `DeviceWorkspace.tsx` (`runsHostCommands`, the appearance
//! switch, the screenshot button), the option tables and the location and permission forms port
//! `DeviceToolsPanel.tsx`, and the screenshot capture ports `client-runtime/src/device/screenshot.ts`
//! with `packages/shared/src/png.ts` (`extractPng`).

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use super::stream::Orientation;
use super::{HostKind, Platform, encode_component};

/// `DeviceAppearance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    pub fn as_str(self) -> &'static str {
        match self {
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

/// `DeviceTextSize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextSize {
    Small,
    Default,
    Large,
    ExtraLarge,
}

impl TextSize {
    pub const ALL: [TextSize; 4] = [TextSize::Small, TextSize::Default, TextSize::Large, TextSize::ExtraLarge];

    pub fn as_str(self) -> &'static str {
        match self {
            TextSize::Small => "small",
            TextSize::Default => "default",
            TextSize::Large => "large",
            TextSize::ExtraLarge => "extra-large",
        }
    }

    /// `TEXT_SIZES` labels.
    pub fn label(self) -> &'static str {
        match self {
            TextSize::Small => "Small",
            TextSize::Default => "Default",
            TextSize::Large => "Large",
            TextSize::ExtraLarge => "Extra large",
        }
    }

    pub fn parse(value: &str) -> Option<TextSize> {
        TextSize::ALL.into_iter().find(|size| size.as_str() == value)
    }
}

/// `DeviceColorFilter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorFilter {
    None,
    Grayscale,
    RedGreen,
    GreenRed,
    BlueYellow,
}

impl ColorFilter {
    pub const ALL: [ColorFilter; 5] = [ColorFilter::None, ColorFilter::Grayscale, ColorFilter::RedGreen, ColorFilter::GreenRed, ColorFilter::BlueYellow];

    pub fn as_str(self) -> &'static str {
        match self {
            ColorFilter::None => "none",
            ColorFilter::Grayscale => "grayscale",
            ColorFilter::RedGreen => "red-green",
            ColorFilter::GreenRed => "green-red",
            ColorFilter::BlueYellow => "blue-yellow",
        }
    }

    /// `COLOR_FILTERS` labels.
    pub fn label(self) -> &'static str {
        match self {
            ColorFilter::None => "None",
            ColorFilter::Grayscale => "Grayscale",
            ColorFilter::RedGreen => "Red / green (protanopia)",
            ColorFilter::GreenRed => "Green / red (deuteranopia)",
            ColorFilter::BlueYellow => "Blue / yellow (tritanopia)",
        }
    }

    pub fn parse(value: &str) -> Option<ColorFilter> {
        ColorFilter::ALL.into_iter().find(|filter| filter.as_str() == value)
    }
}

/// The Liquid Glass look an iOS 26 simulator offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LiquidGlass {
    Clear,
    Tinted,
}

impl LiquidGlass {
    pub fn as_str(self) -> &'static str {
        match self {
            LiquidGlass::Clear => "clear",
            LiquidGlass::Tinted => "tinted",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LiquidGlass::Clear => "Clear",
            LiquidGlass::Tinted => "Tinted",
        }
    }
}

/// `ORIENTATIONS` in the tools panel: what an Android emulator can be rotated to.
pub const ORIENTATIONS: [(Orientation, &str); 4] = [
    (Orientation::Portrait, "Portrait"),
    (Orientation::LandscapeLeft, "Landscape left"),
    (Orientation::PortraitUpsideDown, "Upside down"),
    (Orientation::LandscapeRight, "Landscape right"),
];

/// The rail's Android rotate menu.
pub const RAIL_ORIENTATIONS: [(Orientation, &str); 2] = [(Orientation::Portrait, "Portrait"), (Orientation::LandscapeLeft, "Landscape")];

pub fn parse_orientation(value: &str) -> Option<Orientation> {
    ORIENTATIONS.into_iter().map(|(orientation, _)| orientation).find(|orientation| orientation.as_str() == value)
}

/// A `setToggle` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    ReduceMotion,
    IncreaseContrast,
    ReduceTransparency,
    ShowBorders,
    VoiceOver,
    NetworkEnabled,
}

impl Toggle {
    pub fn as_str(self) -> &'static str {
        match self {
            Toggle::ReduceMotion => "reduceMotion",
            Toggle::IncreaseContrast => "increaseContrast",
            Toggle::ReduceTransparency => "reduceTransparency",
            Toggle::ShowBorders => "showBorders",
            Toggle::VoiceOver => "voiceOver",
            Toggle::NetworkEnabled => "networkEnabled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Toggle::ReduceMotion => "Reduce Motion",
            Toggle::IncreaseContrast => "Increase Contrast",
            Toggle::ReduceTransparency => "Reduce Transparency",
            Toggle::ShowBorders => "Show Borders",
            Toggle::VoiceOver => "VoiceOver",
            Toggle::NetworkEnabled => "Network",
        }
    }

    /// The switches the tools panel shows for a platform, in its order.
    pub fn for_platform(platform: Platform) -> &'static [Toggle] {
        match platform {
            Platform::Ios => &[Toggle::ReduceMotion, Toggle::IncreaseContrast, Toggle::ReduceTransparency, Toggle::ShowBorders, Toggle::VoiceOver],
            Platform::Android => &[Toggle::ReduceMotion, Toggle::NetworkEnabled],
        }
    }

    /// What the host last confirmed; `None` disables the switch.
    pub fn value(self, settings: &Settings) -> Option<bool> {
        match self {
            Toggle::ReduceMotion => settings.reduce_motion,
            Toggle::IncreaseContrast => settings.increase_contrast,
            Toggle::ReduceTransparency => settings.reduce_transparency,
            Toggle::ShowBorders => settings.show_borders,
            Toggle::VoiceOver => settings.voice_over,
            Toggle::NetworkEnabled => settings.network_enabled,
        }
    }
}

/// `DevicePermission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Camera,
    Microphone,
    Photos,
    Contacts,
    Calendar,
    Reminders,
    Location,
    Notifications,
    Motion,
    MediaLibrary,
    FaceId,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Camera => "camera",
            Permission::Microphone => "microphone",
            Permission::Photos => "photos",
            Permission::Contacts => "contacts",
            Permission::Calendar => "calendar",
            Permission::Reminders => "reminders",
            Permission::Location => "location",
            Permission::Notifications => "notifications",
            Permission::Motion => "motion",
            Permission::MediaLibrary => "media-library",
            Permission::FaceId => "faceid",
        }
    }

    /// `IOS_PERMISSIONS` or `ANDROID_PERMISSIONS`, with their labels.
    pub fn for_platform(platform: Platform) -> &'static [(Permission, &'static str)] {
        match platform {
            Platform::Ios => &[
                (Permission::Camera, "Camera"),
                (Permission::Microphone, "Microphone"),
                (Permission::Photos, "Photos"),
                (Permission::Contacts, "Contacts"),
                (Permission::Calendar, "Calendar"),
                (Permission::Reminders, "Reminders"),
                (Permission::Location, "Location"),
                (Permission::Notifications, "Notifications"),
                (Permission::Motion, "Motion"),
                (Permission::MediaLibrary, "Media library"),
                (Permission::FaceId, "Face ID"),
            ],
            Platform::Android => &[
                (Permission::Camera, "Camera"),
                (Permission::Microphone, "Microphone"),
                (Permission::Photos, "Photos"),
                (Permission::Contacts, "Contacts"),
                (Permission::Calendar, "Calendar"),
                (Permission::Location, "Location"),
                (Permission::Notifications, "Notifications"),
                (Permission::Motion, "Physical activity"),
            ],
        }
    }

    pub fn parse(value: &str) -> Option<Permission> {
        Permission::for_platform(Platform::Ios).iter().map(|(permission, _)| *permission).find(|permission| permission.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Grant,
    Revoke,
    /// iOS only (`canReset`).
    Reset,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Grant => "grant",
            Decision::Revoke => "revoke",
            Decision::Reset => "reset",
        }
    }
}

/// `LOCATION_PRESETS`: label, latitude, longitude.
pub const LOCATION_PRESETS: [(&str, f64, f64); 5] = [
    ("San Francisco", 37.7749, -122.4194),
    ("New York", 40.7128, -74.006),
    ("London", 51.5074, -0.1278),
    ("Stockholm", 59.3293, 18.0686),
    ("Tokyo", 35.6762, 139.6503),
];

/// One bad value reads as unknown, so a newer host's settings never fail the whole detail.
fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned>(deserializer: D) -> Result<Option<T>, D::Error> {
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// `DeviceSettings`: what the host could read. A setting it could not read stays `None`, and
/// its control stays disabled.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default, deserialize_with = "lenient")]
    pub appearance: Option<Appearance>,
    #[serde(default, deserialize_with = "lenient")]
    pub text_size: Option<TextSize>,
    #[serde(default, deserialize_with = "lenient")]
    pub reduce_motion: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    pub increase_contrast: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    pub reduce_transparency: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    pub show_borders: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    pub voice_over: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    pub liquid_glass: Option<LiquidGlass>,
    #[serde(default, deserialize_with = "lenient")]
    pub color_filter: Option<ColorFilter>,
    #[serde(default, deserialize_with = "lenient")]
    pub network_enabled: Option<bool>,
}

/// `DeviceForegroundApp`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ForegroundApp {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// `DeviceDetail`: `device.detail`'s answer, and every `device.action`'s.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    pub host_id: String,
    pub device_id: String,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default, deserialize_with = "lenient")]
    pub foreground_app: Option<ForegroundApp>,
}

impl Detail {
    pub fn decode(value: Value) -> Result<Detail, String> {
        serde_json::from_value(value).map_err(|error| format!("Invalid device detail: {error}"))
    }
}

/// A `DeviceActionInput` without its target.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    SetAppearance(Appearance),
    SetTextSize(TextSize),
    SetToggle(Toggle, bool),
    SetLiquidGlass(LiquidGlass),
    SetColorFilter(ColorFilter),
    SetOrientation(Orientation),
    SetLocation { latitude: f64, longitude: f64 },
    ClearLocation,
    SetPermission { app_id: String, permission: Permission, decision: Decision },
    OpenUrl(String),
    LaunchApp(String),
    TerminateApp(String),
    /// iOS only: an alert text, as the tools panel sends it.
    SendPush { app_id: String, payload: String },
}

/// `device.detail` for one device.
pub fn detail_payload(host_id: &str, device_id: &str) -> Value {
    json!({ "hostId": host_id, "deviceId": device_id })
}

/// `device.action` for one device.
pub fn action_payload(host_id: &str, device_id: &str, action: &Action) -> Value {
    let mut body = Map::new();
    body.insert("hostId".into(), host_id.into());
    body.insert("deviceId".into(), device_id.into());
    let mut set = |key: &str, value: Value| {
        body.insert(key.to_owned(), value);
    };
    let kind = match action {
        Action::SetAppearance(value) => {
            set("value", value.as_str().into());
            "setAppearance"
        }
        Action::SetTextSize(value) => {
            set("value", value.as_str().into());
            "setTextSize"
        }
        Action::SetToggle(setting, value) => {
            set("setting", setting.as_str().into());
            set("value", (*value).into());
            "setToggle"
        }
        Action::SetLiquidGlass(value) => {
            set("value", value.as_str().into());
            "setLiquidGlass"
        }
        Action::SetColorFilter(value) => {
            set("value", value.as_str().into());
            "setColorFilter"
        }
        Action::SetOrientation(value) => {
            set("value", value.as_str().into());
            "setOrientation"
        }
        Action::SetLocation { latitude, longitude } => {
            set("latitude", json!(latitude));
            set("longitude", json!(longitude));
            "setLocation"
        }
        Action::ClearLocation => "clearLocation",
        Action::SetPermission { app_id, permission, decision } => {
            set("appId", app_id.as_str().into());
            set("permission", permission.as_str().into());
            set("decision", decision.as_str().into());
            "setPermission"
        }
        Action::OpenUrl(url) => {
            set("url", url.as_str().into());
            "openUrl"
        }
        Action::LaunchApp(app_id) => {
            set("appId", app_id.as_str().into());
            "launchApp"
        }
        Action::TerminateApp(app_id) => {
            set("appId", app_id.as_str().into());
            "terminateApp"
        }
        Action::SendPush { app_id, payload } => {
            set("appId", app_id.as_str().into());
            set("payload", payload.as_str().into());
            "sendPush"
        }
    };
    body.insert("type".into(), kind.into());
    Value::Object(body)
}

/// `runsHostCommands`: settings and tools run commands on the device's own machine, which EAS and
/// Simlock hosts do not expose.
pub fn runs_host_commands(kind: Option<HostKind>) -> bool {
    matches!(kind, Some(HostKind::Local | HostKind::Ssh))
}

/// Simlock hosts offer no screenshot (the rail hides the button).
pub fn offers_screenshot(kind: Option<HostKind>) -> bool {
    kind != Some(HostKind::Simlock)
}

/// The rail's appearance switch: what it turns the device to, and its label.
pub fn next_appearance(settings: Option<&Settings>) -> Appearance {
    if settings.and_then(|settings| settings.appearance) == Some(Appearance::Dark) { Appearance::Light } else { Appearance::Dark }
}

pub fn appearance_label(next: Appearance) -> String {
    format!("Switch device to {} mode", next.as_str())
}

/// A device's detail and the action running on it (`useDeviceControls`). Reads and actions share
/// one lane: a read answers only while no action started after it was sent, so a slow read can
/// never undo the settings an action already confirmed. One action runs at a time; hiding the
/// device does not cancel it, so a reopened panel cannot race it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Controls {
    detail: Option<Detail>,
    error: Option<String>,
    pending: bool,
    /// Counts actions started, so a read can tell whether one overtook it.
    actions: u64,
}

/// Names one read, so its answer can tell whether it still applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadTicket(u64);

impl Controls {
    pub fn detail(&self) -> Option<&Detail> {
        self.detail.as_ref()
    }

    pub fn settings(&self) -> Option<&Settings> {
        self.detail.as_ref().map(|detail| &detail.settings)
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn pending(&self) -> bool {
        self.pending
    }

    pub fn foreground_app(&self) -> Option<&ForegroundApp> {
        self.detail.as_ref()?.foreground_app.as_ref()
    }

    /// `disabled`: while an action runs, before the first detail, or while hidden.
    pub fn disabled(&self, visible: bool) -> bool {
        self.pending || self.detail.is_none() || !visible
    }

    /// `deviceDetailReadAtom` mounting: a read is about to be sent.
    pub fn start_read(&self) -> ReadTicket {
        ReadTicket(self.actions)
    }

    /// The read's answer; whether anything shown changed.
    pub fn read(&mut self, ticket: ReadTicket, result: Result<Detail, String>) -> bool {
        if ticket.0 != self.actions {
            return false;
        }
        let (detail, error) = match result {
            Ok(detail) => (Some(detail), None),
            Err(error) => (None, Some(error)),
        };
        let changed = self.detail != detail || self.error != error;
        self.detail = detail;
        self.error = error;
        changed
    }

    /// `act`: whether to send the action now. Nothing runs before the first detail, while hidden,
    /// or while another action runs.
    pub fn begin(&mut self, visible: bool) -> bool {
        if self.pending || self.detail.is_none() || !visible {
            return false;
        }
        self.pending = true;
        self.actions += 1;
        self.error = None;
        true
    }

    /// The action's answer: the detail it confirmed, or why it failed (the last detail stays).
    /// Whether it succeeded, so a form clears only on success.
    pub fn finish(&mut self, result: Result<Detail, String>) -> bool {
        self.pending = false;
        match result {
            Ok(detail) => {
                self.detail = Some(detail);
                self.error = None;
                true
            }
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }
}

/// `LocationSection`'s fields: both set and within range.
pub fn parse_location(latitude: &str, longitude: &str) -> Option<(f64, f64)> {
    let (latitude, longitude) = (latitude.trim(), longitude.trim());
    if latitude.is_empty() || longitude.is_empty() {
        return None;
    }
    let (latitude, longitude) = (latitude.parse::<f64>().ok()?, longitude.parse::<f64>().ok()?);
    (latitude.abs() <= 90. && longitude.abs() <= 180.).then_some((latitude, longitude))
}

/// `PermissionsSection`'s app: the typed one, else the foreground app.
pub fn resolve_app_id<'a>(typed: &'a str, foreground: Option<&'a str>) -> Option<&'a str> {
    let typed = typed.trim();
    if typed.is_empty() { foreground.filter(|id| !id.is_empty()) } else { Some(typed) }
}

/// `SubmitRow`: the trimmed value, when there is one.
pub fn submit_value(value: &str) -> Option<&str> {
    Some(value.trim()).filter(|value| !value.is_empty())
}

/// The hub path that captures a PNG (`captureDeviceScreenshot`), on serve-sim for iOS and
/// serve-emu for Android.
pub fn screenshot_path(device_id: &str) -> String {
    format!("/api/screenshot?device={}", encode_component(device_id))
}

/// `SCREENSHOT_TIMEOUT`: `AbortSignal.timeout(20_000)`.
pub const SCREENSHOT_TIMEOUT_MS: u64 = 20_000;
pub const SCREENSHOT_INCOMPLETE: &str = "The device returned an incomplete screenshot. Try again.";
pub const SCREENSHOT_FAILED: &str = "Screenshot capture failed. Try again.";

/// `DeviceScreenshotError.message`.
pub fn screenshot_status_error(status: u16) -> String {
    format!("Screenshot capture failed ({status}). Try again.")
}

/// The saved file's name: the device name with anything but letters, digits and dashes
/// replaced, then the capture time in milliseconds.
pub fn screenshot_file_name(device_name: &str, millis: u128) -> String {
    let name: String = device_name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect();
    format!("{name}-{millis}.png")
}

const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

/// `extractPng`: the first complete PNG in `bytes`. `adb exec-out screencap -p` can print
/// warnings, such as a foldable's multiple displays, around the image.
pub fn extract_png(bytes: &[u8]) -> Option<&[u8]> {
    let start = bytes.windows(PNG_SIGNATURE.len()).position(|window| window == PNG_SIGNATURE)?;
    let mut chunk = start + PNG_SIGNATURE.len();
    while chunk + 12 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[chunk..chunk + 4].try_into().ok()?) as usize;
        let end = chunk.checked_add(12)?.checked_add(length)?;
        if end > bytes.len() {
            return None;
        }
        if &bytes[chunk + 4..chunk + 8] == b"IEND" {
            return Some(&bytes[start..end]);
        }
        chunk = end;
    }
    None
}

/// The vendor the screenshot goes through: serve-sim for iOS.
pub fn screenshot_serve_sim(platform: Platform) -> bool {
    platform == Platform::Ios
}
