//! Preview browser viewport presets, responsive sizing, appearance preferences, profiles, and
//! default tab settings. Ports `packages/contracts/src/preview.ts`,
//! `packages/contracts/src/browserProfile.ts`, `packages/shared/src/previewViewport.ts`,
//! `apps/web/src/browser/browserViewportLayout.ts`, and `apps/web/src/browser/browserDefaults.ts`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{DEFAULT_ZOOM, ZOOM_LEVELS};

pub const DEFAULT_BROWSER_PROFILE_ID: &str = "default";
pub const INCOGNITO_BROWSER_PROFILE_ID: &str = "incognito";

pub const PREVIEW_VIEWPORT_MIN_DIMENSION: u32 = 240;
pub const PREVIEW_VIEWPORT_MAX_DIMENSION: u32 = 3840;
pub const PREVIEW_VIEWPORT_MAX_AREA: u64 = 3840 * 2160;

pub const BROWSER_DEVICE_TOOLBAR_HEIGHT: f32 = 38.;
pub const BROWSER_VIEWPORT_RESIZE_RAIL_SIZE: f64 = 10.;
const FALLBACK_RESPONSIVE_VIEWPORT_SIZE: (u32, u32) = (1024, 768);

/// `PreviewAppearancePreference`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppearancePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl AppearancePreference {
    pub const ALL: [AppearancePreference; 3] = [
        AppearancePreference::System,
        AppearancePreference::Light,
        AppearancePreference::Dark,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AppearancePreference::System => "system",
            AppearancePreference::Light => "light",
            AppearancePreference::Dark => "dark",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AppearancePreference::System => "System",
            AppearancePreference::Light => "Light",
            AppearancePreference::Dark => "Dark",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "system" => Some(AppearancePreference::System),
            "light" => Some(AppearancePreference::Light),
            "dark" => Some(AppearancePreference::Dark),
            _ => None,
        }
    }
}

/// `PreviewViewportSetting`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all = "camelCase")]
pub enum ViewportSetting {
    #[default]
    Fill,
    Freeform {
        width: u32,
        height: u32,
    },
    Preset {
        width: u32,
        height: u32,
        #[serde(rename = "presetId")]
        preset_id: String,
    },
}

impl ViewportSetting {
    pub fn is_fill(&self) -> bool {
        matches!(self, ViewportSetting::Fill)
    }

    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match *self {
            ViewportSetting::Fill => None,
            ViewportSetting::Freeform { width, height } | ViewportSetting::Preset { width, height, .. } => Some((width, height)),
        }
    }

    pub fn preset_id(&self) -> Option<&str> {
        match self {
            ViewportSetting::Preset { preset_id, .. } => Some(preset_id.as_str()),
            ViewportSetting::Fill | ViewportSetting::Freeform { .. } => None,
        }
    }

    /// Parses a `PreviewViewportSetting` JSON object, falling back to `Fill` on unknown or
    /// out-of-bounds shapes for forward compatibility.
    pub fn from_value(value: &Value) -> Self {
        let Ok(setting) = serde_json::from_value::<ViewportSetting>(value.clone()) else {
            return ViewportSetting::Fill;
        };
        match &setting {
            ViewportSetting::Fill => setting,
            ViewportSetting::Freeform { width, height } | ViewportSetting::Preset { width, height, .. }
                if validate_viewport_dimensions(*width, *height) =>
            {
                setting
            }
            _ => ViewportSetting::Fill,
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({ "_tag": "fill" }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    pub detail: &'static str,
    pub width: u32,
    pub height: u32,
}

/// Chromium `EmulatedDevices.ts` CSS viewport sizes in Chrome DevTools default-device order
/// (`PREVIEW_VIEWPORT_PRESETS`).
pub const VIEWPORT_PRESETS: &[ViewportPreset] = &[
    ViewportPreset { id: "iphone-se", label: "iPhone SE", category: "Phone", detail: "375 × 667", width: 375, height: 667 },
    ViewportPreset { id: "iphone-xr", label: "iPhone XR", category: "Phone", detail: "414 × 896", width: 414, height: 896 },
    ViewportPreset { id: "iphone-12-pro", label: "iPhone 12 Pro", category: "Phone", detail: "390 × 844", width: 390, height: 844 },
    ViewportPreset { id: "iphone-14-pro-max", label: "iPhone 14 Pro Max", category: "Phone", detail: "430 × 932", width: 430, height: 932 },
    ViewportPreset { id: "pixel-7", label: "Pixel 7", category: "Phone", detail: "412 × 915", width: 412, height: 915 },
    ViewportPreset { id: "samsung-galaxy-s8-plus", label: "Samsung Galaxy S8+", category: "Phone", detail: "360 × 740", width: 360, height: 740 },
    ViewportPreset { id: "samsung-galaxy-s20-ultra", label: "Samsung Galaxy S20 Ultra", category: "Phone", detail: "412 × 915", width: 412, height: 915 },
    ViewportPreset { id: "ipad-mini", label: "iPad Mini", category: "Tablet", detail: "768 × 1024", width: 768, height: 1024 },
    ViewportPreset { id: "ipad-air", label: "iPad Air", category: "Tablet", detail: "820 × 1180", width: 820, height: 1180 },
    ViewportPreset { id: "ipad-pro", label: "iPad Pro", category: "Tablet", detail: "1024 × 1366", width: 1024, height: 1366 },
    ViewportPreset { id: "surface-pro-7", label: "Surface Pro 7", category: "Tablet", detail: "912 × 1368", width: 912, height: 1368 },
    ViewportPreset { id: "surface-duo", label: "Surface Duo", category: "Phone", detail: "540 × 720", width: 540, height: 720 },
    ViewportPreset { id: "galaxy-z-fold-5", label: "Galaxy Z Fold 5", category: "Phone", detail: "344 × 882", width: 344, height: 882 },
    ViewportPreset { id: "asus-zenbook-fold", label: "Asus Zenbook Fold", category: "Tablet", detail: "853 × 1280", width: 853, height: 1280 },
    ViewportPreset { id: "samsung-galaxy-a51-71", label: "Samsung Galaxy A51/71", category: "Phone", detail: "412 × 914", width: 412, height: 914 },
    ViewportPreset { id: "nest-hub", label: "Nest Hub", category: "Tablet", detail: "1024 × 600", width: 1024, height: 600 },
    ViewportPreset { id: "nest-hub-max", label: "Nest Hub Max", category: "Tablet", detail: "1280 × 800", width: 1280, height: 800 },
];

pub fn find_viewport_preset(id: &str) -> Option<&'static ViewportPreset> {
    VIEWPORT_PRESETS.iter().find(|preset| preset.id == id)
}

pub fn preset_setting(id: &str) -> Option<ViewportSetting> {
    let preset = find_viewport_preset(id)?;
    Some(ViewportSetting::Preset { width: preset.width, height: preset.height, preset_id: preset.id.to_owned() })
}

pub fn validate_viewport_dimensions(width: u32, height: u32) -> bool {
    (PREVIEW_VIEWPORT_MIN_DIMENSION..=PREVIEW_VIEWPORT_MAX_DIMENSION).contains(&width)
        && (PREVIEW_VIEWPORT_MIN_DIMENSION..=PREVIEW_VIEWPORT_MAX_DIMENSION).contains(&height)
        && u64::from(width) * u64::from(height) <= PREVIEW_VIEWPORT_MAX_AREA
}

fn normalize_zoom_factor(zoom_factor: f64) -> f64 {
    if zoom_factor.is_finite() && zoom_factor > 0. { zoom_factor } else { 1. }
}

fn clamp_viewport_dimension(value: f64) -> u32 {
    value
        .round()
        .clamp(f64::from(PREVIEW_VIEWPORT_MIN_DIMENSION), f64::from(PREVIEW_VIEWPORT_MAX_DIMENSION)) as u32
}

/// `resizeAtAspectRatio`: clamps a locked-ratio resize inside min/max bounds and max pixel area.
pub fn resize_at_aspect_ratio(desired: f64, aspect_ratio: f64, primary_width: bool) -> (u32, u32) {
    let min_dim = f64::from(PREVIEW_VIEWPORT_MIN_DIMENSION);
    let max_dim = f64::from(PREVIEW_VIEWPORT_MAX_DIMENSION);
    let max_area = PREVIEW_VIEWPORT_MAX_AREA as f64;
    if primary_width {
        let minimum = min_dim.max(min_dim * aspect_ratio).ceil() as u32;
        let maximum = max_dim.min(max_dim * aspect_ratio).min((max_area * aspect_ratio).sqrt()).floor() as u32;
        let mut width = (desired.round() as i64).clamp(i64::from(minimum), i64::from(maximum.max(minimum))) as u32;
        let mut height = (f64::from(width) / aspect_ratio).round().max(1.) as u32;
        while u64::from(width) * u64::from(height) > PREVIEW_VIEWPORT_MAX_AREA && width > minimum {
            width -= 1;
            height = (f64::from(width) / aspect_ratio).round().max(1.) as u32;
        }
        (width, height)
    } else {
        let minimum = min_dim.max(min_dim / aspect_ratio).ceil() as u32;
        let maximum = max_dim.min(max_dim / aspect_ratio).min((max_area / aspect_ratio).sqrt()).floor() as u32;
        let mut height = (desired.round() as i64).clamp(i64::from(minimum), i64::from(maximum.max(minimum))) as u32;
        let mut width = (f64::from(height) * aspect_ratio).round().max(1.) as u32;
        while u64::from(width) * u64::from(height) > PREVIEW_VIEWPORT_MAX_AREA && height > minimum {
            height -= 1;
            width = (f64::from(height) * aspect_ratio).round().max(1.) as u32;
        }
        (width, height)
    }
}

/// `resizeFreeformViewport` for custom dimension edits and responsive clamping.
pub fn resize_freeform_viewport(
    start: (f64, f64),
    delta: (f64, f64),
    zoom_factor: f64,
    aspect_ratio: Option<f64>,
    primary_width: Option<bool>,
) -> (u32, u32) {
    let zoom = normalize_zoom_factor(zoom_factor);
    let desired_width = start.0 + delta.0 / zoom;
    let desired_height = start.1 + delta.1 / zoom;
    if let Some(ratio) = aspect_ratio.filter(|ratio| ratio.is_finite() && *ratio > 0.) {
        let width_axis = primary_width.unwrap_or_else(|| {
            ((desired_width - start.0).abs() / start.0.max(1.)) >= ((desired_height - start.1).abs() / start.1.max(1.))
        });
        return resize_at_aspect_ratio(if width_axis { desired_width } else { desired_height }, ratio, width_axis);
    }
    let mut width = clamp_viewport_dimension(desired_width);
    let mut height = clamp_viewport_dimension(desired_height);
    if u64::from(width) * u64::from(height) <= PREVIEW_VIEWPORT_MAX_AREA {
        return (width, height);
    }
    if delta.0.abs() >= delta.1.abs() {
        width = ((PREVIEW_VIEWPORT_MAX_AREA / u64::from(height)) as u32).max(PREVIEW_VIEWPORT_MIN_DIMENSION);
    } else {
        height = ((PREVIEW_VIEWPORT_MAX_AREA / u64::from(width)) as u32).max(PREVIEW_VIEWPORT_MIN_DIMENSION);
    }
    (width, height)
}

/// `resolveResponsiveBrowserViewportSize`: fits inside the device toolbar + resize rails.
pub fn resolve_responsive_viewport_size(container: (f64, f64), zoom_factor: f64) -> (u32, u32) {
    let area_width = (container.0 - BROWSER_VIEWPORT_RESIZE_RAIL_SIZE * 2.).max(1.);
    let area_height = (container.1 - f64::from(BROWSER_DEVICE_TOOLBAR_HEIGHT) - BROWSER_VIEWPORT_RESIZE_RAIL_SIZE).max(1.);
    let zoom = normalize_zoom_factor(zoom_factor);
    resize_freeform_viewport((area_width / zoom, area_height / zoom), (0., 0.), 1., None, None)
}

/// `browserResponsiveViewportForToggle` (`pingdotgg/t3code#35664`): toggles from `Fill` to the
/// default non-fill viewport, or a freeform viewport matching the current panel size.
pub fn responsive_viewport_for_toggle(
    default_viewport: &ViewportSetting,
    panel_rect: Option<(f64, f64)>,
    zoom_factor: f64,
) -> ViewportSetting {
    if !default_viewport.is_fill() {
        return default_viewport.clone();
    }
    let (width, height) = panel_rect
        .filter(|(w, h)| *w > 0. && *h > 0.)
        .map_or(FALLBACK_RESPONSIVE_VIEWPORT_SIZE, |rect| resolve_responsive_viewport_size(rect, zoom_factor));
    ViewportSetting::Freeform { width, height }
}

/// Rotates a non-fill viewport (swapping width & height) and inverts a locked aspect ratio.
pub fn rotate_viewport(setting: &ViewportSetting, aspect_ratio: Option<f64>) -> (ViewportSetting, Option<f64>) {
    let next = match setting {
        ViewportSetting::Fill => ViewportSetting::Fill,
        ViewportSetting::Freeform { width, height } => ViewportSetting::Freeform { width: *height, height: *width },
        ViewportSetting::Preset { width, height, preset_id } => {
            ViewportSetting::Preset { width: *height, height: *width, preset_id: preset_id.clone() }
        }
    };
    let next_ratio = aspect_ratio.filter(|ratio| ratio.is_finite() && *ratio > 0.).map(|ratio| 1. / ratio);
    (next, next_ratio)
}

/// `BrowserProfileKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserProfileKind {
    Persistent,
    Incognito,
}

/// `BrowserProfile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserProfile {
    pub id: String,
    pub name: String,
    pub kind: BrowserProfileKind,
}

pub fn built_in_browser_profiles() -> Vec<BrowserProfile> {
    vec![
        BrowserProfile {
            id: DEFAULT_BROWSER_PROFILE_ID.to_owned(),
            name: "Default".to_owned(),
            kind: BrowserProfileKind::Persistent,
        },
        BrowserProfile {
            id: INCOGNITO_BROWSER_PROFILE_ID.to_owned(),
            name: "Incognito".to_owned(),
            kind: BrowserProfileKind::Incognito,
        },
    ]
}

/// `resolveBrowserProfiles`: built-in profiles first, then deduplicated user profiles forced to
/// persistent (only the built-in incognito profile is ephemeral).
pub fn resolve_browser_profiles(user_profiles: &[BrowserProfile]) -> Vec<BrowserProfile> {
    let mut resolved = built_in_browser_profiles();
    for profile in user_profiles {
        let id = profile.id.trim();
        let name = profile.name.trim();
        if id.is_empty() || id.len() > 64 || id.chars().any(char::is_control) || name.is_empty() {
            continue;
        }
        if resolved.iter().any(|existing| existing.id == id) {
            continue;
        }
        resolved.push(BrowserProfile {
            id: id.to_owned(),
            name: name.chars().take(48).collect(),
            kind: BrowserProfileKind::Persistent,
        });
    }
    resolved
}

pub fn profile_name(profiles: &[BrowserProfile], profile_id: &str) -> String {
    profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .map_or_else(|| "Removed profile".to_owned(), |profile| profile.name.clone())
}

pub fn is_incognito_profile(profile_id: Option<&str>) -> bool {
    profile_id == Some(INCOGNITO_BROWSER_PROFILE_ID)
}

/// `BrowserDefaults` from `apps/web/src/browser/browserDefaults.ts`.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserDefaults {
    pub viewport: ViewportSetting,
    pub zoom_factor: f64,
    pub appearance: AppearancePreference,
    pub profiles: Vec<BrowserProfile>,
    pub profile_id: String,
}

impl Default for BrowserDefaults {
    fn default() -> Self {
        Self {
            viewport: ViewportSetting::Fill,
            zoom_factor: DEFAULT_ZOOM,
            appearance: AppearancePreference::System,
            profiles: built_in_browser_profiles(),
            profile_id: DEFAULT_BROWSER_PROFILE_ID.to_owned(),
        }
    }
}

impl BrowserDefaults {
    /// Reads `browserDefaultViewport`, `browserDefaultZoomFactor`, `browserDefaultAppearance`,
    /// `browserProfiles`, and `browserDefaultProfileId` from `ClientSettingsDoc::entries()`.
    pub fn from_settings_entries(entries: &Map<String, Value>) -> Self {
        let viewport = entries
            .get("browserDefaultViewport")
            .map(ViewportSetting::from_value)
            .unwrap_or(ViewportSetting::Fill);
        let zoom_factor = entries
            .get("browserDefaultZoomFactor")
            .and_then(Value::as_f64)
            .and_then(|z| ZOOM_LEVELS.iter().copied().find(|level| (*level - z).abs() < 1e-9))
            .unwrap_or(DEFAULT_ZOOM);
        let appearance = entries
            .get("browserDefaultAppearance")
            .and_then(Value::as_str)
            .and_then(AppearancePreference::parse)
            .unwrap_or(AppearancePreference::System);
        let user_profiles: Vec<BrowserProfile> = entries
            .get("browserProfiles")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(|item| serde_json::from_value(item.clone()).ok()).collect())
            .unwrap_or_default();
        let profiles = resolve_browser_profiles(&user_profiles);
        let default_profile_id = entries.get("browserDefaultProfileId").and_then(Value::as_str);
        let profile_id = default_profile_id
            .and_then(|id| {
                profiles
                    .iter()
                    .find(|profile| profile.id == id && profile.kind != BrowserProfileKind::Incognito)
                    .map(|profile| profile.id.clone())
            })
            .unwrap_or_else(|| DEFAULT_BROWSER_PROFILE_ID.to_owned());
        Self { viewport, zoom_factor, appearance, profiles, profile_id }
    }
}

/// Optional per-open overrides (e.g. reopening a closed tab with its saved URL/viewport/zoom/appearance).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenPreviewOverrides {
    pub url: Option<String>,
    pub viewport: Option<ViewportSetting>,
    pub profile_id: Option<String>,
    pub zoom_factor: Option<f64>,
    pub color_scheme: Option<AppearancePreference>,
}

/// Builds the `preview.open` RPC payload for a server browser tab (`openPreviewSession.ts`,
/// `pingdotgg/t3code#34943` & `pingdotgg/t3code#25874`): always forwards the effective viewport,
/// profile, zoom factor, and appearance preference.
pub fn open_session_payload(thread_id: &str, defaults: &BrowserDefaults, overrides: &OpenPreviewOverrides) -> Value {
    let viewport = overrides.viewport.as_ref().unwrap_or(&defaults.viewport);
    let profile_id = overrides.profile_id.as_deref().unwrap_or(&defaults.profile_id);
    let zoom_factor = overrides
        .zoom_factor
        .and_then(|z| ZOOM_LEVELS.iter().copied().find(|level| (*level - z).abs() < 1e-9))
        .unwrap_or(defaults.zoom_factor);
    let color_scheme = overrides.color_scheme.unwrap_or(defaults.appearance);
    let mut payload = json!({
        "threadId": thread_id,
        "runtime": "server",
        "viewport": viewport.to_value(),
        "profileId": profile_id,
        "zoomFactor": zoom_factor,
        "colorScheme": color_scheme.as_str(),
    });
    if let Some(url) = overrides.url.as_deref().filter(|url| !url.trim().is_empty()) {
        payload["url"] = Value::String(url.to_owned());
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_browser_defaults_and_falls_back_from_incognito_default_profile() {
        let entries: Map<String, Value> = serde_json::from_value(json!({
            "browserDefaultViewport": { "_tag": "preset", "width": 390, "height": 844, "presetId": "iphone-12-pro" },
            "browserDefaultZoomFactor": 0.8,
            "browserDefaultAppearance": "dark",
            "browserProfiles": [
                { "id": "work", "name": "Work", "kind": "persistent" },
                { "id": "incognito", "name": "Fake", "kind": "incognito" },
                { "id": "extra", "name": "Extra", "kind": "incognito" }
            ],
            "browserDefaultProfileId": "incognito"
        }))
        .unwrap();
        let defaults = BrowserDefaults::from_settings_entries(&entries);
        assert_eq!(defaults.zoom_factor, 0.8);
        assert_eq!(defaults.appearance, AppearancePreference::Dark);
        assert_eq!(
            defaults.viewport,
            ViewportSetting::Preset { width: 390, height: 844, preset_id: "iphone-12-pro".into() }
        );
        // Incognito cannot be selected as the default profile.
        assert_eq!(defaults.profile_id, DEFAULT_BROWSER_PROFILE_ID);
        assert_eq!(profile_name(&defaults.profiles, "work"), "Work");
        assert_eq!(profile_name(&defaults.profiles, "incognito"), "Incognito");
        assert_eq!(profile_name(&defaults.profiles, "deleted"), "Removed profile");
        // User profiles claiming incognito are forced persistent.
        assert_eq!(
            defaults.profiles.iter().find(|p| p.id == "extra").unwrap().kind,
            BrowserProfileKind::Persistent
        );
    }

    #[test]
    fn open_session_payload_includes_default_zoom_and_appearance_and_honors_overrides() {
        let entries: Map<String, Value> = serde_json::from_value(json!({
            "browserDefaultZoomFactor": 0.8,
            "browserDefaultAppearance": "dark",
            "browserProfiles": [{ "id": "work", "name": "Work", "kind": "persistent" }],
            "browserDefaultProfileId": "work"
        }))
        .unwrap();
        let defaults = BrowserDefaults::from_settings_entries(&entries);
        let payload = open_session_payload("thread-1", &defaults, &OpenPreviewOverrides::default());
        assert_eq!(
            payload,
            json!({
                "threadId": "thread-1",
                "runtime": "server",
                "viewport": { "_tag": "fill" },
                "profileId": "work",
                "zoomFactor": 0.8,
                "colorScheme": "dark",
            })
        );
        let restored = open_session_payload(
            "thread-1",
            &defaults,
            &OpenPreviewOverrides {
                url: Some("https://example.com/".into()),
                viewport: Some(ViewportSetting::Freeform { width: 1200, height: 800 }),
                profile_id: Some("default".into()),
                zoom_factor: Some(1.5),
                color_scheme: Some(AppearancePreference::Light),
            },
        );
        assert_eq!(
            restored,
            json!({
                "threadId": "thread-1",
                "runtime": "server",
                "url": "https://example.com/",
                "viewport": { "_tag": "freeform", "width": 1200, "height": 800 },
                "profileId": "default",
                "zoomFactor": 1.5,
                "colorScheme": "light",
            })
        );
    }

    #[test]
    fn responsive_viewport_for_toggle_starts_at_panel_size() {
        let toggled = responsive_viewport_for_toggle(&ViewportSetting::Fill, Some((820., 648.)), 1.0);
        // Area is (820 - 20) x (648 - 38 - 10) = 800 x 600.
        assert_eq!(toggled, ViewportSetting::Freeform { width: 800, height: 600 });
        // Falls back to 1024x768 when panel has not measured yet.
        assert_eq!(
            responsive_viewport_for_toggle(&ViewportSetting::Fill, None, 1.0),
            ViewportSetting::Freeform { width: 1024, height: 768 }
        );
        // Keeps non-fill default viewport.
        let preset = preset_setting("iphone-se").unwrap();
        assert_eq!(responsive_viewport_for_toggle(&preset, Some((820., 648.)), 1.0), preset);
    }

    #[test]
    fn rotate_and_aspect_ratio_resize_respect_bounds() {
        let (rotated, ratio) = rotate_viewport(
            &ViewportSetting::Preset { width: 375, height: 667, preset_id: "iphone-se".into() },
            Some(375. / 667.),
        );
        assert_eq!(rotated, ViewportSetting::Preset { width: 667, height: 375, preset_id: "iphone-se".into() });
        assert!((ratio.unwrap() - (667. / 375.)).abs() < 1e-6);
        // Aspect ratio 2:1, desired width 1000 -> 1000x500.
        assert_eq!(resize_at_aspect_ratio(1000., 2.0, true), (1000, 500));
        // Min clamp: 2:1 cannot have height < 240 -> min width 480.
        assert_eq!(resize_at_aspect_ratio(300., 2.0, true), (480, 240));
        assert!(validate_viewport_dimensions(3840, 2160));
        assert!(!validate_viewport_dimensions(3840, 3840));
        assert!(!validate_viewport_dimensions(200, 600));
    }
}
