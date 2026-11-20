//! SnapShots settings, setup wizard, shortcut parsing/formatting, conflict detection,
//! and capture target-resolution logic.
//!
//! Ports:
//! - `packages/contracts/src/settings.ts` (`SnapShotShortcut`, `SnapShotModifier`, `SnapShotSound`)
//! - `packages/contracts/src/chatAttachment.ts` (`SnapShotSource`, `SNAP_SHOT_ACCESSIBLE_TEXT_MAX_CHARS`)
//! - `apps/web/src/lib/snapShotShortcut.ts`
//! - `apps/web/src/lib/snapShotSource.ts`
//! - `apps/web/src/components/settings/SnapShotSettings.logic.ts`
//! - `apps/web/src/components/settings/SnapShotSetupDialog.logic.ts`
//! - `apps/web/src/components/settings/CaptureShortcutConfig.tsx`
//! - `apps/web/src/components/desktop/SnapShotCoordinator.tsx`
//! - `apps/desktop/src/snapShot/snapShot.ts` (`snapShotShortcutSystemConflict`, `snapShotShortcutRegistrationFailureMessage`)
//! - `apps/desktop/src/snapShot/MacSnapShot.ts` (`PNG_SIGNATURE`, max capture dimensions)

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::composer::attachments::{MAX_ATTACHMENTS, MAX_IMAGE_BYTES};
use crate::keybindings::{
    KeybindingShortcut, ResolvedKeybindingRule, format_shortcut_key_label, format_shortcut_label,
    parse_keybinding_shortcut, shortcut_conflict_key,
};
use crate::settings::keybindings::shortcut_to_keybinding_input;

/// Maximum capture width in pixels (`DesktopSnapShot.ts`: `MAX_CAPTURE_WIDTH`).
pub const MAX_CAPTURE_WIDTH: u32 = 2_560;
/// Maximum capture height in pixels (`DesktopSnapShot.ts`: `MAX_CAPTURE_HEIGHT`).
pub const MAX_CAPTURE_HEIGHT: u32 = 1_600;
/// `PROVIDER_SEND_TURN_MAX_ATTACHMENTS`.
pub const SNAP_SHOT_MAX_ATTACHMENTS: usize = MAX_ATTACHMENTS;
/// `PROVIDER_SEND_TURN_MAX_IMAGE_BYTES` (10 MiB).
pub const SNAP_SHOT_MAX_IMAGE_BYTES: u64 = MAX_IMAGE_BYTES;
/// `SNAP_SHOT_ACCESSIBLE_TEXT_MAX_CHARS`.
pub const SNAP_SHOT_ACCESSIBLE_TEXT_MAX_CHARS: usize = 32_000;
/// `PNG_SIGNATURE` (`MacSnapShot.ts`).
pub const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

pub const MAC_SCREEN_CAPTURE_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
pub const MAC_ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

pub const MAC_SCREEN_CAPTURE_PERMISSION_MESSAGE: &str =
    "Allow Screen Recording in System Settings, then restart T3 Code.";
pub const MAC_ACCESSIBILITY_PERMISSION_MESSAGE: &str =
    "Allow Accessibility in System Settings, then restart T3 Code.";
pub const MAC_BOTH_PERMISSIONS_MESSAGE: &str =
    "Allow Accessibility and Screen Recording in System Settings, then restart T3 Code.";

/// `SnapShotModifier` (`packages/contracts/src/settings.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnapShotModifier {
    Shift,
    Meta,
    Control,
    Alt,
}

impl SnapShotModifier {
    pub const ALL: &'static [SnapShotModifier] = &[
        SnapShotModifier::Shift,
        SnapShotModifier::Meta,
        SnapShotModifier::Control,
        SnapShotModifier::Alt,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SnapShotModifier::Shift => "shift",
            SnapShotModifier::Meta => "meta",
            SnapShotModifier::Control => "control",
            SnapShotModifier::Alt => "alt",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "shift" => Some(SnapShotModifier::Shift),
            "meta" => Some(SnapShotModifier::Meta),
            "control" => Some(SnapShotModifier::Control),
            "alt" => Some(SnapShotModifier::Alt),
            _ => None,
        }
    }

    /// `MAC_MODIFIER_PAIR_DEVICE_MASKS` (`MacModifierPairShortcutProcess.ts`):
    /// `(left_device_mask, right_device_mask)` in IOKit/CGEventFlags lower 16 bits.
    pub fn mac_device_masks(self) -> (u64, u64) {
        match self {
            SnapShotModifier::Shift => (0x2, 0x4),
            SnapShotModifier::Control => (0x1, 0x2000),
            SnapShotModifier::Alt => (0x20, 0x40),
            SnapShotModifier::Meta => (0x8, 0x10),
        }
    }

    /// `snapShotModifierPairLabel` (`packages/contracts/src/settings.ts`).
    pub fn pair_label(self, apple: bool) -> String {
        let label = match (self, apple) {
            (SnapShotModifier::Shift, _) => "Shift",
            (SnapShotModifier::Meta, true) => "Command",
            (SnapShotModifier::Meta, false) => "Super",
            (SnapShotModifier::Control, true) => "Control",
            (SnapShotModifier::Control, false) => "Ctrl",
            (SnapShotModifier::Alt, true) => "Option",
            (SnapShotModifier::Alt, false) => "Alt",
        };
        format!("{label} + {label}")
    }

    /// `modifierKeyLabel` (`apps/web/src/lib/snapShotShortcut.ts`).
    pub fn keycap_label(self, mac: bool, windows: bool) -> &'static str {
        if self == SnapShotModifier::Shift {
            return "\u{21e7}";
        }
        if mac {
            return match self {
                SnapShotModifier::Meta => "\u{2318}",
                SnapShotModifier::Control => "\u{2303}",
                SnapShotModifier::Alt => "\u{2325}",
                SnapShotModifier::Shift => "\u{21e7}",
            };
        }
        match self {
            SnapShotModifier::Meta => {
                if windows {
                    "\u{229e}"
                } else {
                    "Super"
                }
            }
            SnapShotModifier::Control => "Ctrl",
            SnapShotModifier::Alt => "Alt",
            SnapShotModifier::Shift => "\u{21e7}",
        }
    }
}

/// `SnapShotShortcut` (`packages/contracts/src/settings.ts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum SnapShotShortcut {
    #[default]
    BothShiftKeys,
    ModifierPair(SnapShotModifier),
    KeyChord(KeybindingShortcut),
}

impl SnapShotShortcut {
    pub fn is_modifier_pair(&self) -> bool {
        matches!(self, SnapShotShortcut::BothShiftKeys | SnapShotShortcut::ModifierPair(_))
    }

    pub fn modifier_pair(&self) -> Option<SnapShotModifier> {
        match self {
            SnapShotShortcut::BothShiftKeys => Some(SnapShotModifier::Shift),
            SnapShotShortcut::ModifierPair(modifier) => Some(*modifier),
            SnapShotShortcut::KeyChord(_) => None,
        }
    }

    pub fn from_modifier_pair(modifier: SnapShotModifier) -> Self {
        if modifier == SnapShotModifier::Shift {
            SnapShotShortcut::BothShiftKeys
        } else {
            SnapShotShortcut::ModifierPair(modifier)
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            SnapShotShortcut::BothShiftKeys => json!({ "kind": "both-shift-keys" }),
            SnapShotShortcut::ModifierPair(modifier) => {
                json!({ "kind": "modifier-pair", "modifier": modifier.as_str() })
            }
            SnapShotShortcut::KeyChord(shortcut) => json!({
                "key": shortcut.key,
                "metaKey": shortcut.meta_key,
                "ctrlKey": shortcut.ctrl_key,
                "shiftKey": shortcut.shift_key,
                "altKey": shortcut.alt_key,
                "modKey": shortcut.mod_key,
            }),
        }
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        if let Some(kind) = obj.get("kind").and_then(Value::as_str) {
            return match kind {
                "both-shift-keys" => Some(SnapShotShortcut::BothShiftKeys),
                "modifier-pair" => {
                    let modifier = SnapShotModifier::parse(obj.get("modifier")?.as_str()?)?;
                    Some(SnapShotShortcut::ModifierPair(modifier))
                }
                _ => None,
            };
        }
        let shortcut: KeybindingShortcut = serde_json::from_value(value.clone()).ok()?;
        is_valid_snap_shot_key_chord(&shortcut).then_some(SnapShotShortcut::KeyChord(shortcut))
    }
}

/// Validates `SnapShotKeyChord` (`packages/contracts/src/settings.ts`):
/// requires a non-empty key and at least one modifier.
pub fn is_valid_snap_shot_key_chord(shortcut: &KeybindingShortcut) -> bool {
    !shortcut.key.is_empty()
        && shortcut.key.encode_utf16().count() <= 64
        && (shortcut.meta_key
            || shortcut.ctrl_key
            || shortcut.shift_key
            || shortcut.alt_key
            || shortcut.mod_key)
}

/// `SnapShotSound` (`packages/contracts/src/settings.ts`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SnapShotSound {
    #[default]
    SoftPop,
    CameraShutter,
}

impl SnapShotSound {
    pub const ALL: &'static [SnapShotSound] =
        &[SnapShotSound::SoftPop, SnapShotSound::CameraShutter];

    pub fn as_str(self) -> &'static str {
        match self {
            SnapShotSound::SoftPop => "soft-pop",
            SnapShotSound::CameraShutter => "camera-shutter",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "soft-pop" => Some(SnapShotSound::SoftPop),
            "camera-shutter" => Some(SnapShotSound::CameraShutter),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SnapShotSound::SoftPop => "Whoosh (Default)",
            SnapShotSound::CameraShutter => "Click",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            SnapShotSound::SoftPop => "Whoosh",
            SnapShotSound::CameraShutter => "Click",
        }
    }
}

/// `SnapShotSoundSelection` (`SnapShotSettings.logic.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnapShotSoundSelection {
    Off,
    Sound(SnapShotSound),
}

impl SnapShotSoundSelection {
    pub const ALL: &'static [SnapShotSoundSelection] = &[
        SnapShotSoundSelection::Off,
        SnapShotSoundSelection::Sound(SnapShotSound::SoftPop),
        SnapShotSoundSelection::Sound(SnapShotSound::CameraShutter),
    ];

    pub fn from_settings(play_sound: bool, sound: SnapShotSound) -> Self {
        if play_sound {
            SnapShotSoundSelection::Sound(sound)
        } else {
            SnapShotSoundSelection::Off
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SnapShotSoundSelection::Off => "off",
            SnapShotSoundSelection::Sound(sound) => sound.as_str(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(SnapShotSoundSelection::Off),
            other => SnapShotSound::parse(other).map(SnapShotSoundSelection::Sound),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SnapShotSoundSelection::Off => "Off",
            SnapShotSoundSelection::Sound(sound) => sound.label(),
        }
    }
}

/// `snapShotSoundPatch` (`SnapShotSettings.logic.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapShotSoundPatch {
    pub play_sound: bool,
    pub sound: Option<SnapShotSound>,
}

pub fn snap_shot_sound_patch(selection: SnapShotSoundSelection) -> SnapShotSoundPatch {
    match selection {
        SnapShotSoundSelection::Off => SnapShotSoundPatch { play_sound: false, sound: None },
        SnapShotSoundSelection::Sound(sound) => {
            SnapShotSoundPatch { play_sound: true, sound: Some(sound) }
        }
    }
}

// ---------------------------------------------------------------------------
// Shortcut parsing, display, conflict detection (`snapShotShortcut.ts`, `snapShot.ts`)
// ---------------------------------------------------------------------------

fn desktop_key_alias(key: &str) -> &str {
    match key {
        "return" => "enter",
        "page_up" => "pageup",
        "page_down" => "pagedown",
        "left" => "arrowleft",
        "right" => "arrowright",
        "up" => "arrowup",
        "down" => "arrowdown",
        "plus" => "+",
        "minus" => "-",
        other => other,
    }
}

fn is_supported_desktop_shortcut_key(key: &str) -> bool {
    if key == " " {
        return true;
    }
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && !c.is_whitespace()
        && c != '<'
        && c != '>'
    {
        return true;
    }
    let lower = key.to_ascii_lowercase();
    if let Some(num) = lower.strip_prefix('f')
        && let Ok(n) = num.parse::<u32>()
        && (1..=24).contains(&n)
    {
        return true;
    }
    matches!(
        lower.as_str(),
        "enter"
            | "tab"
            | "escape"
            | "backspace"
            | "delete"
            | "insert"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
            | "arrowleft"
            | "arrowright"
            | "arrowup"
            | "arrowdown"
            | "pause"
            | "printscreen"
    )
}

fn replace_word_modifiers(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        match word.to_ascii_lowercase().as_str() {
            "primary" => out.push_str("ctrl"),
            "super" | "win" => out.push_str("meta"),
            _ => out.push_str(word),
        }
        word.clear();
    };
    for c in input.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// `parseDesktopSnapShotShortcut` (`apps/web/src/lib/snapShotShortcut.ts`).
pub fn parse_desktop_snap_shot_shortcut(label: &str) -> Option<KeybindingShortcut> {
    let trimmed = label.trim();
    let text = if trimmed.len() >= 6
        && trimmed[..5].eq_ignore_ascii_case("press")
        && trimmed.as_bytes()[5].is_ascii_whitespace()
    {
        trimmed[5..].trim_start()
    } else {
        trimmed
    };
    if text.is_empty() {
        return None;
    }

    let expanded = if text.starts_with('<') {
        let mut rest = text;
        let mut parts = Vec::new();
        while let Some(after_open) = rest.strip_prefix('<') {
            let close = after_open.find('>')?;
            let token = &after_open[..close];
            if token.is_empty() || token.contains('<') {
                return None;
            }
            parts.push(token);
            rest = &after_open[close + 1..];
        }
        if parts.is_empty() || rest.is_empty() || rest.contains('<') || rest.contains('>') {
            return None;
        }
        let mut combined = parts.join("+");
        combined.push('+');
        combined.push_str(rest);
        combined
    } else {
        text.to_owned()
    };

    let input = replace_word_modifiers(&expanded);
    if input != "+" && input.ends_with('+') && !input.ends_with("++") {
        return None;
    }
    let mut shortcut = parse_keybinding_shortcut(&input)?;
    let key = desktop_key_alias(&shortcut.key).to_owned();
    if !is_supported_desktop_shortcut_key(&key) {
        return None;
    }
    shortcut.key = key;
    Some(shortcut)
}

/// `formatSnapShotShortcutLabel` (`apps/web/src/lib/snapShotShortcut.ts`).
pub fn format_snap_shot_shortcut_label(shortcut: &SnapShotShortcut, mac: bool) -> String {
    match shortcut {
        SnapShotShortcut::BothShiftKeys => SnapShotModifier::Shift.pair_label(mac),
        SnapShotShortcut::ModifierPair(modifier) => modifier.pair_label(mac),
        SnapShotShortcut::KeyChord(chord) => format_shortcut_label(chord, mac),
    }
}

/// `snapShotShortcutKeyLabels` (`apps/web/src/lib/snapShotShortcut.ts`).
pub fn snap_shot_shortcut_key_labels(
    shortcut: &SnapShotShortcut,
    mac: bool,
    windows: bool,
) -> Vec<String> {
    if let Some(modifier) = shortcut.modifier_pair() {
        let label = modifier.keycap_label(mac, windows).to_owned();
        return vec![label.clone(), label];
    }
    let SnapShotShortcut::KeyChord(chord) = shortcut else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(5);
    let mods = [
        (chord.ctrl_key || (chord.mod_key && !mac), SnapShotModifier::Control),
        (chord.alt_key, SnapShotModifier::Alt),
        (chord.shift_key, SnapShotModifier::Shift),
        (chord.meta_key || (chord.mod_key && mac), SnapShotModifier::Meta),
    ];
    for (enabled, modifier) in mods {
        if enabled {
            out.push(modifier.keycap_label(mac, windows).to_owned());
        }
    }
    out.push(format_shortcut_key_label(&chord.key));
    out
}

/// `sameSnapShotShortcut` (`apps/web/src/lib/snapShotShortcut.ts`).
pub fn same_snap_shot_shortcut(
    left: &SnapShotShortcut,
    right: &SnapShotShortcut,
    mac: bool,
) -> bool {
    match (left.modifier_pair(), right.modifier_pair()) {
        (Some(l), Some(r)) => l == r,
        (None, None) => match (left, right) {
            (SnapShotShortcut::KeyChord(l), SnapShotShortcut::KeyChord(r)) => {
                shortcut_conflict_key(l, mac) == shortcut_conflict_key(r, mac)
            }
            _ => false,
        },
        _ => false,
    }
}

/// `snapShotKeybindingConflict` (`apps/web/src/lib/snapShotShortcut.ts`).
pub fn snap_shot_keybinding_conflict<'a>(
    shortcut: &SnapShotShortcut,
    keybindings: &'a [ResolvedKeybindingRule],
    mac: bool,
) -> Option<&'a str> {
    let SnapShotShortcut::KeyChord(chord) = shortcut else {
        return None;
    };
    let key = shortcut_conflict_key(chord, mac);
    keybindings
        .iter()
        .find(|binding| shortcut_conflict_key(&binding.shortcut, mac) == key)
        .map(|binding| binding.command.as_str())
}

/// `snapShotShortcutSystemConflict` (`apps/desktop/src/snapShot/snapShot.ts`).
pub fn snap_shot_shortcut_system_conflict(shortcut: &SnapShotShortcut) -> Option<String> {
    let SnapShotShortcut::KeyChord(chord) = shortcut else {
        return None;
    };
    let modifier_count = [
        chord.mod_key,
        chord.meta_key,
        chord.ctrl_key,
        chord.alt_key,
        chord.shift_key,
    ]
    .into_iter()
    .filter(|flag| *flag)
    .count();
    if modifier_count != 1 {
        return None;
    }
    if chord.shift_key {
        return Some(
            "Shift combinations are used for typing and text selection. Add another modifier."
                .to_owned(),
        );
    }
    let key = chord.key.to_ascii_lowercase();
    if chord.mod_key || chord.meta_key {
        let common = match key.as_str() {
            "a" => Some("Select All"),
            "c" => Some("Copy"),
            "f" => Some("Find"),
            "n" => Some("New"),
            "o" => Some("Open"),
            "p" => Some("Print"),
            "q" => Some("Quit"),
            "s" => Some("Save"),
            "t" => Some("New Tab"),
            "v" => Some("Paste"),
            "w" => Some("Close Window"),
            "x" => Some("Cut"),
            "z" => Some("Undo"),
            _ => None,
        };
        if chord.mod_key
            && let Some(action) = common
        {
            return Some(format!("This shortcut is {action} in most apps."));
        }
        if chord.meta_key && matches!(key.as_str(), "l" | " " | "tab" | "q" | "h") {
            return Some("The system already uses this shortcut.".to_owned());
        }
        if chord.meta_key
            && let Some(action) = common
        {
            return Some(format!("This shortcut is {action} in most apps."));
        }
    }
    if chord.ctrl_key && matches!(key.as_str(), "c" | "d" | "z") {
        return Some("This shortcut controls running commands in terminals.".to_owned());
    }
    if chord.alt_key && key == "tab" {
        return Some("The system uses Alt+Tab to switch apps.".to_owned());
    }
    None
}

/// `snapShotShortcutRegistrationFailureMessage` (`apps/desktop/src/snapShot/snapShot.ts`).
pub fn snap_shot_shortcut_registration_failure_message(
    shortcut: &SnapShotShortcut,
    mac: bool,
) -> String {
    if let Some(modifier) = shortcut.modifier_pair() {
        format!("{} is not available on this system.", modifier.pair_label(mac))
    } else {
        "This shortcut is already used by the system or another app.".to_owned()
    }
}

/// Shortcut availability check result (`DesktopSnapShotShortcutAvailability`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutAvailability {
    pub available: bool,
    pub message: Option<String>,
}

/// Evaluates whether `shortcut` is eligible for global capture registration on this platform.
pub fn check_shortcut_availability(
    shortcut: &SnapShotShortcut,
    allow_modifier_pairs: bool,
    supported: bool,
) -> ShortcutAvailability {
    if !supported {
        return ShortcutAvailability {
            available: false,
            message: Some("SnapShots capture is not supported on this platform.".to_owned()),
        };
    }
    if shortcut.is_modifier_pair() && !allow_modifier_pairs {
        return ShortcutAvailability {
            available: false,
            message: Some("Add a letter, number, or function key to your shortcut.".to_owned()),
        };
    }
    if let Some(reason) = snap_shot_shortcut_system_conflict(shortcut) {
        return ShortcutAvailability { available: false, message: Some(reason) };
    }
    if let SnapShotShortcut::KeyChord(chord) = shortcut
        && !is_valid_snap_shot_key_chord(chord)
    {
        return ShortcutAvailability {
            available: false,
            message: Some("Snapshot shortcut requires a modifier.".to_owned()),
        };
    }
    ShortcutAvailability { available: true, message: None }
}

/// `CaptureShortcutConfig.tsx` default shortcut normalization for external desktop config files.
pub fn normalize_capture_shortcut_config_input(shortcut: &KeybindingShortcut) -> String {
    shortcut_to_keybinding_input(&KeybindingShortcut {
        ctrl_key: shortcut.ctrl_key || shortcut.mod_key,
        mod_key: false,
        ..shortcut.clone()
    })
}

/// `createRecordingRequestTracker` (`SnapShotSettings.logic.ts`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecordingRequestTracker {
    next_id: u64,
    current: Option<u64>,
}

impl RecordingRequestTracker {
    pub fn try_begin(&mut self) -> Option<u64> {
        if self.current.is_some() {
            return None;
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.current = Some(self.next_id);
        self.current
    }

    pub fn clear(&mut self) {
        self.current = None;
    }

    pub fn owns(&self, request: u64) -> bool {
        self.current == Some(request)
    }
}

// ---------------------------------------------------------------------------
// Setup state & wizard logic (`SnapShotSettings.logic.ts`, `SnapShotSetupDialog.logic.ts`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapShotMode {
    Direct,
    Portal,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSetupStep {
    Access,
    Shortcut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSetupBackend {
    Direct,
    Gnome,
    Kde,
    Hyprland,
    Niri,
    Portal,
    Picker,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MacSnapShotPermissions {
    pub screen_recording: bool,
    pub accessibility: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapShotBackendState {
    pub mode: SnapShotMode,
    pub shortcut: SnapShotShortcut,
    pub shortcut_registered: bool,
    pub shortcut_pending: bool,
    pub shortcut_verified: bool,
    pub shortcut_label: Option<String>,
    pub shortcut_message: Option<String>,
    pub message: Option<String>,
    pub mac_permissions: Option<MacSnapShotPermissions>,
    pub windows: bool,
}

impl SnapShotBackendState {
    pub fn macos(
        shortcut: SnapShotShortcut,
        permissions: MacSnapShotPermissions,
        include_accessibility: bool,
        shortcut_registered: bool,
        shortcut_verified: bool,
    ) -> Self {
        let message = mac_permissions_error_message(permissions, include_accessibility);
        Self {
            mode: SnapShotMode::Direct,
            shortcut,
            shortcut_registered,
            shortcut_pending: false,
            shortcut_verified,
            shortcut_label: None,
            shortcut_message: None,
            message: message.map(str::to_owned),
            mac_permissions: Some(permissions),
            windows: false,
        }
    }

    pub fn unsupported(shortcut: SnapShotShortcut) -> Self {
        Self {
            mode: SnapShotMode::Unavailable,
            shortcut,
            shortcut_registered: false,
            shortcut_pending: false,
            shortcut_verified: false,
            shortcut_label: None,
            shortcut_message: None,
            message: Some("Not supported on this platform.".to_owned()),
            mac_permissions: None,
            windows: false,
        }
    }
}

pub fn mac_permissions_error_message(
    permissions: MacSnapShotPermissions,
    include_accessibility: bool,
) -> Option<&'static str> {
    let needs_screen = !permissions.screen_recording;
    let needs_ax = include_accessibility && !permissions.accessibility;
    match (needs_screen, needs_ax) {
        (true, true) => Some(MAC_BOTH_PERMISSIONS_MESSAGE),
        (true, false) => Some(MAC_SCREEN_CAPTURE_PERMISSION_MESSAGE),
        (false, true) => Some(MAC_ACCESSIBILITY_PERMISSION_MESSAGE),
        (false, false) => None,
    }
}

pub fn capture_setup_backend(state: &SnapShotBackendState) -> CaptureSetupBackend {
    match state.mode {
        SnapShotMode::Portal => CaptureSetupBackend::Portal,
        SnapShotMode::Direct | SnapShotMode::Unavailable => CaptureSetupBackend::Direct,
    }
}

/// `captureSetupAccessReady` (`SnapShotSetupDialog.logic.ts`).
pub fn capture_setup_access_ready(state: &SnapShotBackendState) -> bool {
    state.mode != SnapShotMode::Unavailable && state.message.is_none()
}

/// `captureSetupMacPermissionsReady` (`SnapShotSetupDialog.logic.ts`).
pub fn capture_setup_mac_permissions_ready(
    state: &SnapShotBackendState,
    include_accessibility: bool,
) -> bool {
    let Some(permissions) = state.mac_permissions else {
        return true;
    };
    permissions.screen_recording && (!include_accessibility || permissions.accessibility)
}

/// `captureSetupCheckMessage` (`SnapShotSetupDialog.logic.ts`).
pub fn capture_setup_check_message(state: &SnapShotBackendState) -> &'static str {
    if state.message.is_some() {
        return "Still unable to check access. See Advanced for help.";
    }
    if capture_setup_backend(state) == CaptureSetupBackend::Picker {
        return "Ready. You'll choose a window each time.";
    }
    if capture_setup_access_ready(state) {
        "Ready. Continue to choose your shortcut."
    } else {
        "Not ready yet. Finish the step above."
    }
}

/// `captureSetupShortcutReady` (`SnapShotSetupDialog.logic.ts`).
pub fn capture_setup_shortcut_ready(state: &SnapShotBackendState, unsaved: bool) -> bool {
    if unsaved || !capture_setup_access_ready(state) {
        return false;
    }
    state.shortcut_registered || state.shortcut_pending
}

/// `captureSetupInitialStep` (`SnapShotSetupDialog.logic.ts`).
pub fn capture_setup_initial_step(
    state: &SnapShotBackendState,
    requested: Option<CaptureSetupStep>,
) -> CaptureSetupStep {
    if !capture_setup_access_ready(state) {
        return CaptureSetupStep::Access;
    }
    match requested {
        None => {
            if (state.mode == SnapShotMode::Direct && !state.shortcut_registered)
                || capture_setup_backend(state) == CaptureSetupBackend::Picker
            {
                CaptureSetupStep::Access
            } else {
                CaptureSetupStep::Shortcut
            }
        }
        Some(step) => step,
    }
}

/// `captureSetupShouldDisableOnClose` (`SnapShotSetupDialog.logic.ts`).
pub fn capture_setup_should_disable_on_close(was_enabled: bool, completed: bool) -> bool {
    !was_enabled && !completed
}

/// `snapShotSetupSummary` (`SnapShotSettings.logic.ts`).
pub fn snap_shot_setup_summary(state: &SnapShotBackendState, enabled: bool) -> &'static str {
    if state.message.is_some() {
        return "Capture needs attention";
    }
    if capture_setup_backend(state) == CaptureSetupBackend::Picker {
        return "Manual capture only. You'll choose a window each time";
    }
    if !enabled {
        return "Enable capture to continue";
    }
    if state.shortcut_pending {
        return "Waiting for shortcut permission";
    }
    if state.shortcut_verified {
        return "Ready to capture";
    }
    if state.shortcut_registered {
        if state.shortcut_label.is_some() {
            "Ready to capture"
        } else {
            "Shortcut saved"
        }
    } else {
        "Finish shortcut setup"
    }
}

/// `snapShotStatus` (`SnapShotSettings.logic.ts`).
pub fn snap_shot_status(state: Option<&SnapShotBackendState>, enabled: bool) -> String {
    let Some(state) = state else {
        return "Checking snapshots\u{2026}".to_owned();
    };
    if state.mode == SnapShotMode::Unavailable {
        return state
            .message
            .clone()
            .unwrap_or_else(|| "Not supported on this platform.".to_owned());
    }
    if !enabled {
        return "Turn this on to set up snapshots.".to_owned();
    }
    snap_shot_setup_summary(state, enabled).to_owned()
}

/// `snapShotShortcutStatus` (`SnapShotSettings.logic.ts`).
pub fn snap_shot_shortcut_status(state: Option<&SnapShotBackendState>) -> Option<String> {
    let state = state?;
    if state.shortcut_pending {
        return Some("Approve the shortcut permission prompt to continue.".to_owned());
    }
    if state.shortcut_registered {
        return (state.mode != SnapShotMode::Portal).then(|| "Shortcut saved.".to_owned());
    }
    state.shortcut_message.clone()
}

/// `snapShotSetupButtonLabel` (`SnapShotSettings.logic.ts`).
pub fn snap_shot_setup_button_label(state: Option<&SnapShotBackendState>) -> &'static str {
    let Some(state) = state else {
        return "Continue setup";
    };
    if capture_setup_access_ready(state) {
        "Manage capture"
    } else {
        "Continue setup"
    }
}

/// `snapShotSetupComplete` (`SnapShotSettings.logic.ts`).
pub fn snap_shot_setup_complete(
    state: Option<&SnapShotBackendState>,
    include_accessibility: bool,
) -> bool {
    let Some(state) = state else {
        return false;
    };
    if state.windows {
        return true;
    }
    state.mac_permissions.is_some()
        && capture_setup_access_ready(state)
        && capture_setup_mac_permissions_ready(state, include_accessibility)
        && state.shortcut_registered
}

/// `snapShotDescription` (`SnapShotSettings.logic.ts`).
pub fn snap_shot_description(state: Option<&SnapShotBackendState>) -> &'static str {
    if state.is_some_and(|s| {
        s.mode == SnapShotMode::Portal && capture_setup_backend(s) == CaptureSetupBackend::Picker
    }) {
        "Automatic capture isn't available here. Choose a window instead."
    } else {
        "Capture a window and attach it to your current draft."
    }
}

// ---------------------------------------------------------------------------
// Capture target resolution & image/source helpers (`SnapShotCoordinator.tsx`, `snapShotSource.ts`)
// ---------------------------------------------------------------------------

/// `CaptureTarget` (`SnapShotCoordinator.tsx`): a draft project id, a scoped thread, or a
/// question attachment draft pinned to a capture id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CaptureTarget {
    Draft { project_id: String },
    Thread { environment_id: String, thread_id: String },
    Question { draft_id: String },
}

/// Context needed to validate an existing capture target (`resolveExistingSnapShotTarget`).
pub struct ExistingTargetLookup<'a> {
    pub route_thread: Option<(&'a str, &'a str)>,
    pub has_draft: &'a dyn Fn(&str) -> bool,
    pub promoted_draft_thread: &'a dyn Fn(&str) -> Option<(String, String)>,
    pub has_thread_shell: &'a dyn Fn(&str, &str) -> bool,
}

/// `resolveExistingSnapShotTarget` (`SnapShotCoordinator.tsx`):
/// - If a draft was promoted to a thread, follow the promotion.
/// - If a draft still exists, keep it.
/// - For a thread, keep it if it is the current route or exists in the shell.
pub fn resolve_existing_snapshot_target(
    target: &CaptureTarget,
    lookup: &ExistingTargetLookup<'_>,
) -> Option<CaptureTarget> {
    match target {
        CaptureTarget::Draft { project_id } => {
            if let Some((environment_id, thread_id)) = (lookup.promoted_draft_thread)(project_id) {
                return Some(CaptureTarget::Thread { environment_id, thread_id });
            }
            (lookup.has_draft)(project_id).then(|| target.clone())
        }
        CaptureTarget::Thread { environment_id, thread_id } => {
            let is_current_route = lookup.route_thread
                == Some((environment_id.as_str(), thread_id.as_str()));
            if is_current_route || (lookup.has_thread_shell)(environment_id, thread_id) {
                Some(target.clone())
            } else {
                None
            }
        }
        CaptureTarget::Question { .. } => Some(target.clone()),
    }
}

/// `resolveSnapShotAttachmentTarget` (`SnapShotCoordinator.tsx`):
/// Pins both presence AND absence of an open question attachment draft so a question opened
/// after the capture began never claims an earlier capture.
pub fn resolve_snapshot_attachment_target(
    pins: &mut HashMap<String, Option<String>>,
    capture_id: &str,
    target: CaptureTarget,
    open_question_draft_id: Option<&str>,
) -> CaptureTarget {
    let open_for_target = match &target {
        CaptureTarget::Thread { .. } => open_question_draft_id,
        CaptureTarget::Draft { .. } | CaptureTarget::Question { .. } => None,
    };
    pins.entry(capture_id.to_owned())
        .or_insert_with(|| open_for_target.map(str::to_owned));
    if let Some(Some(pinned)) = pins.get(capture_id)
        && Some(pinned.as_str()) == open_for_target
    {
        return CaptureTarget::Question { draft_id: pinned.clone() };
    }
    target
}

/// Validates PNG magic bytes (`MacSnapShot.ts`).
pub fn has_valid_png_signature(bytes: &[u8]) -> bool {
    bytes.len() >= PNG_SIGNATURE.len() && bytes[..PNG_SIGNATURE.len()] == PNG_SIGNATURE
}

/// Computes target `(width, height)` scaled proportionally into `max_width x max_height`
/// without upscaling (`MacSnapShot.ts`).
pub fn fit_capture_dimensions(
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
) -> (u32, u32) {
    if width == 0 || height == 0 || max_width == 0 || max_height == 0 {
        return (width.max(1), height.max(1));
    }
    let scale = (f64::from(max_width) / f64::from(width))
        .min(f64::from(max_height) / f64::from(height))
        .min(1.0);
    if scale >= 1.0 {
        return (width, height);
    }
    let w = (f64::from(width) * scale).round().max(1.0) as u32;
    let h = (f64::from(height) * scale).round().max(1.0) as u32;
    (w, h)
}

/// Computes scaled dimensions to shrink an over-limit image toward `max_bytes`.
/// Returns `None` if already within `max_bytes` or already at `1x1`.
pub fn next_resize_dimensions_for_byte_limit(
    width: u32,
    height: u32,
    current_bytes: u64,
    max_bytes: u64,
) -> Option<(u32, u32)> {
    if current_bytes <= max_bytes || (width <= 1 && height <= 1) {
        return None;
    }
    let ratio = (max_bytes as f64 / current_bytes as f64).sqrt().clamp(0.25, 0.85);
    let next_w = ((f64::from(width) * ratio).floor() as u32).max(1).min(width.saturating_sub(1).max(1));
    let next_h = ((f64::from(height) * ratio).floor() as u32).max(1).min(height.saturating_sub(1).max(1));
    if next_w == width && next_h == height {
        None
    } else {
        Some((next_w, next_h))
    }
}

/// Builds a contract-conformant `SnapShotSource` JSON value (`packages/contracts/src/chatAttachment.ts`).
pub fn build_snapshot_source_json(
    captured_at_iso: &str,
    app_name: &str,
    window_title: &str,
    app_identifier: Option<&str>,
    accessible_text: Option<&str>,
    accessible_text_truncated: bool,
) -> Value {
    let app_name = truncate_trimmed(app_name, 255);
    let app_name = if app_name.is_empty() { "Window".to_owned() } else { app_name };
    let window_title = truncate_trimmed(window_title, 1_000);
    let mut obj = Map::new();
    obj.insert("kind".to_owned(), Value::String("snap-shot".to_owned()));
    obj.insert("capturedAt".to_owned(), Value::String(captured_at_iso.to_owned()));
    obj.insert("appName".to_owned(), Value::String(app_name));
    obj.insert("windowTitle".to_owned(), Value::String(window_title));
    if let Some(identifier) = app_identifier {
        let trimmed = truncate_trimmed(identifier, 255);
        if !trimmed.is_empty() {
            obj.insert("appIdentifier".to_owned(), Value::String(trimmed));
        }
    }
    if let Some(text) = accessible_text {
        let cleaned = text.replace('\0', "");
        let trimmed = cleaned.trim();
        if !trimmed.is_empty() {
            let char_count = trimmed.chars().count();
            let truncated =
                accessible_text_truncated || char_count > SNAP_SHOT_ACCESSIBLE_TEXT_MAX_CHARS;
            let bounded: String = trimmed.chars().take(SNAP_SHOT_ACCESSIBLE_TEXT_MAX_CHARS).collect();
            obj.insert("accessibleText".to_owned(), Value::String(bounded.clone()));
            obj.insert(
                "accessibility".to_owned(),
                json!({
                    "format": "flat-text",
                    "text": bounded,
                    "truncated": truncated,
                }),
            );
        }
    }
    Value::Object(obj)
}

/// `resizeSnapShotSource` (`apps/web/src/lib/snapShotSource.ts`): scales element-tree bounds
/// when the captured image was resized.
pub fn resize_snapshot_source_json(source: &Value, new_width: u32, new_height: u32) -> Value {
    let Some(obj) = source.as_object() else {
        return source.clone();
    };
    let Some(ax) = obj.get("accessibility").and_then(Value::as_object) else {
        return source.clone();
    };
    if ax.get("format").and_then(Value::as_str) != Some("element-tree") {
        return source.clone();
    }
    let Some(old_w) = ax.get("imageSize").and_then(|s| s.get("width")).and_then(Value::as_u64) else {
        return source.clone();
    };
    let Some(old_h) = ax.get("imageSize").and_then(|s| s.get("height")).and_then(Value::as_u64) else {
        return source.clone();
    };
    if old_w == 0 || old_h == 0 || (old_w == u64::from(new_width) && old_h == u64::from(new_height)) {
        return source.clone();
    }
    let scale_x = f64::from(new_width) / (old_w as f64);
    let scale_y = f64::from(new_height) / (old_h as f64);

    fn scale_node(node: &Value, scale_x: f64, scale_y: f64, max_w: u32, max_h: u32) -> Value {
        let Some(mut map) = node.as_object().cloned() else {
            return node.clone();
        };
        if let Some(bounds) = map.get("bounds").and_then(Value::as_object)
            && let (Some(bx), Some(by), Some(bw), Some(bh)) = (
                bounds.get("x").and_then(Value::as_f64),
                bounds.get("y").and_then(Value::as_f64),
                bounds.get("width").and_then(Value::as_f64),
                bounds.get("height").and_then(Value::as_f64),
            )
        {
            let x = ((bx * scale_x).round().max(0.0) as u32).min(max_w);
            let y = ((by * scale_y).round().max(0.0) as u32).min(max_h);
            let right = (((bx + bw) * scale_x).round().max(0.0) as u32).min(max_w);
            let bottom = (((by + bh) * scale_y).round().max(0.0) as u32).min(max_h);
            if right > x && bottom > y {
                map.insert(
                    "bounds".to_owned(),
                    json!({ "x": x, "y": y, "width": right - x, "height": bottom - y }),
                );
            } else {
                map.insert("bounds".to_owned(), Value::Null);
            }
        }
        if let Some(children) = map.get("children").and_then(Value::as_array) {
            let scaled: Vec<Value> = children
                .iter()
                .map(|child| scale_node(child, scale_x, scale_y, max_w, max_h))
                .collect();
            map.insert("children".to_owned(), Value::Array(scaled));
        }
        Value::Object(map)
    }

    let mut next_ax = ax.clone();
    next_ax.insert("imageSize".to_owned(), json!({ "width": new_width, "height": new_height }));
    if let Some(root) = ax.get("root") {
        next_ax.insert("root".to_owned(), scale_node(root, scale_x, scale_y, new_width, new_height));
    }
    let mut next_source = obj.clone();
    next_source.insert("accessibility".to_owned(), Value::Object(next_ax));
    Value::Object(next_source)
}

/// Builds a sanitized attachment filename for a captured window PNG.
pub fn snapshot_attachment_filename(app_name: &str, window_title: &str) -> String {
    let base = if !window_title.trim().is_empty() {
        window_title.trim()
    } else if !app_name.trim().is_empty() {
        app_name.trim()
    } else {
        "snapshot"
    };
    let slug: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let collapsed = slug
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let trimmed: String = collapsed.chars().take(60).collect();
    let stem = if trimmed.is_empty() { "snapshot" } else { trimmed.as_str() };
    format!("snapshot-{stem}.png")
}

fn truncate_trimmed(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::default_resolved_keybindings;

    #[test]
    fn parses_and_formats_desktop_snapshot_shortcuts() {
        let cases: &[(&str, &[&str])] = &[
            ("Press <Shift><Control>2", &["Ctrl", "\u{21e7}", "2"]),
            ("Ctrl+Shift+8", &["Ctrl", "\u{21e7}", "8"]),
            ("Meta+F12", &["Super", "F12"]),
            ("<Primary><Alt>Return", &["Ctrl", "Alt", "Enter"]),
            ("<Super>Left", &["Super", "Left"]),
            ("<Control>space", &["Ctrl", "Space"]),
            ("<Control>plus", &["Ctrl", "+"]),
            ("Ctrl++", &["Ctrl", "+"]),
            ("<Control>Page_Up", &["Ctrl", "PageUp"]),
            ("<Shift>Page_Down", &["\u{21e7}", "PageDown"]),
        ];
        for &(label, expected) in cases {
            let parsed = parse_desktop_snap_shot_shortcut(label).expect(label);
            let labels =
                snap_shot_shortcut_key_labels(&SnapShotShortcut::KeyChord(parsed), false, false);
            assert_eq!(labels, expected, "label {label}");
        }
        for invalid in [
            "",
            "Ctrl+",
            "Press the shortcut configured in your desktop",
            "<Unknown>2",
            "<Control>UnknownKey",
        ] {
            assert!(parse_desktop_snap_shot_shortcut(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn modifier_pair_labels_and_round_trip_values() {
        assert_eq!(
            format_snap_shot_shortcut_label(&SnapShotShortcut::BothShiftKeys, true),
            "Shift + Shift"
        );
        assert_eq!(
            format_snap_shot_shortcut_label(
                &SnapShotShortcut::ModifierPair(SnapShotModifier::Meta),
                true
            ),
            "Command + Command"
        );
        assert_eq!(
            format_snap_shot_shortcut_label(
                &SnapShotShortcut::ModifierPair(SnapShotModifier::Meta),
                false
            ),
            "Super + Super"
        );
        assert_eq!(
            snap_shot_shortcut_key_labels(
                &SnapShotShortcut::ModifierPair(SnapShotModifier::Alt),
                true,
                false
            ),
            vec!["\u{2325}", "\u{2325}"]
        );
        assert!(same_snap_shot_shortcut(
            &SnapShotShortcut::BothShiftKeys,
            &SnapShotShortcut::ModifierPair(SnapShotModifier::Shift),
            true,
        ));
        let chord = SnapShotShortcut::KeyChord(parse_keybinding_shortcut("mod+shift+2").unwrap());
        assert_eq!(SnapShotShortcut::from_value(&chord.to_value()), Some(chord));
        // Unmodified key chord is rejected.
        assert!(SnapShotShortcut::from_value(&json!({
            "key": "a", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": false
        }))
        .is_none());
    }

    #[test]
    fn detects_app_keybinding_and_system_conflicts() {
        let bindings = default_resolved_keybindings();
        let conflict = SnapShotShortcut::KeyChord(parse_keybinding_shortcut("mod+k").unwrap());
        assert_eq!(snap_shot_keybinding_conflict(&conflict, bindings, true), Some("commandPalette.toggle"));
        assert_eq!(
            snap_shot_keybinding_conflict(&SnapShotShortcut::BothShiftKeys, bindings, true),
            None
        );

        let shift_only = SnapShotShortcut::KeyChord(parse_keybinding_shortcut("shift+a").unwrap());
        assert!(snap_shot_shortcut_system_conflict(&shift_only).is_some());
        let cmd_c = SnapShotShortcut::KeyChord(parse_keybinding_shortcut("mod+c").unwrap());
        assert_eq!(
            snap_shot_shortcut_system_conflict(&cmd_c).as_deref(),
            Some("This shortcut is Copy in most apps.")
        );
        let cmd_shift_2 =
            SnapShotShortcut::KeyChord(parse_keybinding_shortcut("ctrl+shift+2").unwrap());
        assert!(snap_shot_shortcut_system_conflict(&cmd_shift_2).is_none());
    }

    #[test]
    fn setup_status_and_step_transitions_match_web_logic() {
        let ready = SnapShotBackendState::macos(
            SnapShotShortcut::BothShiftKeys,
            MacSnapShotPermissions { screen_recording: true, accessibility: true },
            true,
            true,
            false,
        );
        assert!(snap_shot_setup_complete(Some(&ready), true));
        assert_eq!(snap_shot_status(Some(&ready), false), "Turn this on to set up snapshots.");
        assert_eq!(snap_shot_status(Some(&ready), true), "Shortcut saved");
        assert_eq!(snap_shot_setup_button_label(Some(&ready)), "Manage capture");
        assert_eq!(capture_setup_initial_step(&ready, None), CaptureSetupStep::Shortcut);

        let missing_ax = SnapShotBackendState::macos(
            SnapShotShortcut::BothShiftKeys,
            MacSnapShotPermissions { screen_recording: true, accessibility: false },
            true,
            true,
            false,
        );
        assert!(!snap_shot_setup_complete(Some(&missing_ax), true));
        assert_eq!(snap_shot_status(Some(&missing_ax), true), "Capture needs attention");
        assert_eq!(snap_shot_setup_button_label(Some(&missing_ax)), "Continue setup");
        assert_eq!(capture_setup_initial_step(&missing_ax, None), CaptureSetupStep::Access);

        let ax_optional = SnapShotBackendState::macos(
            SnapShotShortcut::BothShiftKeys,
            MacSnapShotPermissions { screen_recording: true, accessibility: false },
            false,
            true,
            false,
        );
        assert!(snap_shot_setup_complete(Some(&ax_optional), false));
        assert!(capture_setup_should_disable_on_close(false, false));
        assert!(!capture_setup_should_disable_on_close(true, false));
        assert!(!capture_setup_should_disable_on_close(false, true));
    }

    #[test]
    fn recording_request_tracker_ignores_stale_requests() {
        let mut tracker = RecordingRequestTracker::default();
        let first = tracker.try_begin().unwrap();
        assert!(tracker.try_begin().is_none());
        tracker.clear();
        let second = tracker.try_begin().unwrap();
        assert!(!tracker.owns(first));
        assert!(tracker.owns(second));
    }

    #[test]
    fn target_resolution_follows_draft_promotion_and_pins_question_absence() {
        let lookup = ExistingTargetLookup {
            route_thread: Some(("env-1", "t-route")),
            has_draft: &|project_id| project_id == "p-open",
            promoted_draft_thread: &|project_id| {
                (project_id == "p-promoted").then(|| ("env-1".to_owned(), "t-99".to_owned()))
            },
            has_thread_shell: &|env, id| env == "env-1" && id == "t-shell",
        };
        assert_eq!(
            resolve_existing_snapshot_target(
                &CaptureTarget::Draft { project_id: "p-promoted".into() },
                &lookup
            ),
            Some(CaptureTarget::Thread {
                environment_id: "env-1".into(),
                thread_id: "t-99".into()
            })
        );
        assert_eq!(
            resolve_existing_snapshot_target(
                &CaptureTarget::Draft { project_id: "p-missing".into() },
                &lookup
            ),
            None
        );
        assert!(resolve_existing_snapshot_target(
            &CaptureTarget::Thread {
                environment_id: "env-1".into(),
                thread_id: "t-route".into()
            },
            &lookup
        )
        .is_some());

        // Pinning absence prevents a later question from stealing an earlier capture.
        let mut pins = HashMap::new();
        let thread = CaptureTarget::Thread {
            environment_id: "env-1".into(),
            thread_id: "t-shell".into(),
        };
        assert_eq!(
            resolve_snapshot_attachment_target(&mut pins, "cap-1", thread.clone(), None),
            thread
        );
        assert_eq!(
            resolve_snapshot_attachment_target(
                &mut pins,
                "cap-1",
                thread.clone(),
                Some("q-later")
            ),
            thread
        );

        // Pinning presence routes to the question while open, and falls back if closed.
        assert_eq!(
            resolve_snapshot_attachment_target(
                &mut pins,
                "cap-2",
                thread.clone(),
                Some("q-open")
            ),
            CaptureTarget::Question { draft_id: "q-open".into() }
        );
        assert_eq!(
            resolve_snapshot_attachment_target(&mut pins, "cap-2", thread.clone(), None),
            thread
        );
    }

    #[test]
    fn capture_dimension_and_source_resize_helpers() {
        assert_eq!(fit_capture_dimensions(5120, 2880, MAX_CAPTURE_WIDTH, MAX_CAPTURE_HEIGHT), (2560, 1440));
        assert_eq!(fit_capture_dimensions(1200, 800, MAX_CAPTURE_WIDTH, MAX_CAPTURE_HEIGHT), (1200, 800));
        assert!(next_resize_dimensions_for_byte_limit(2000, 1000, 5_000_000, SNAP_SHOT_MAX_IMAGE_BYTES).is_none());
        let smaller = next_resize_dimensions_for_byte_limit(2000, 1000, 20_000_000, SNAP_SHOT_MAX_IMAGE_BYTES).unwrap();
        assert!(smaller.0 < 2000 && smaller.1 < 1000);

        let source = json!({
            "kind": "snap-shot",
            "capturedAt": "2026-11-20T00:00:00Z",
            "appName": "Safari",
            "windowTitle": "Docs",
            "accessibility": {
                "format": "element-tree",
                "coordinateSpace": "captured-image",
                "imageSize": { "width": 2000, "height": 1000 },
                "truncated": false,
                "root": {
                    "role": "window",
                    "bounds": { "x": 100, "y": 200, "width": 400, "height": 200 },
                    "children": []
                }
            }
        });
        let resized = resize_snapshot_source_json(&source, 1000, 500);
        assert_eq!(resized["accessibility"]["imageSize"], json!({ "width": 1000, "height": 500 }));
        assert_eq!(
            resized["accessibility"]["root"]["bounds"],
            json!({ "x": 50, "y": 100, "width": 200, "height": 100 })
        );
    }
}
