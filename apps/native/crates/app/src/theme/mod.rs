//! One theme source. T3's palette (generated into `tokens.rs` from the web theme) is written into
//! gpui-component's `Theme` as light and dark configs, so primitives and app views read the same
//! values. Tokens gpui-component has no slot for (pills, message surface, sidebar rows…) are read
//! through `palette(cx)`. The theme, appearance mode, interface size and chat width follow the
//! client settings (`web/src/hooks/useTheme.ts`, `appearanceFonts.ts`), and switch atomically.

mod tokens;
pub mod html_render;
pub mod slots;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use t3_core::themes::{self, ThemeDefinition};
use t3_core::themes::css::{EmbeddedFontFace, EmbeddedFontFormat, ThemeCssFonts};

use gpui::{App, Global, Hsla, Rems, Window, WindowAppearance, px, rems};
use gpui_component::{Theme, ThemeConfig, ThemeMode};
use serde_json::json;
use t3_core::client_settings::typography::{FontDefaults, FontRole, MONO_STACK, Typography, resolve_font_stack};
use t3_core::client_settings::{AppearanceMode, ChatWidth, DiffColorScheme, ThemePreference};

#[allow(unused_imports)]
pub use slots::{
    SlotPseudoBar, SlotState, SlotStyleOverride, SlotStyleSet, SlotStylesheet, ThemeSlot,
    ThemeSlotExt, ThemeSlotHoverExt, ThemeSlotInteractiveExt, apply_slot_override,
    refine_slot_active, refine_slot_hover, slot_border_color, slot_has_active,
    slot_has_background, slot_has_floating_inset, slot_has_hover, slot_has_state_background,
    slot_style, slot_stylesheet, theme_slot_background_layer,
    theme_slot_clipped_background_layer, theme_slot_pseudo_before,
    theme_slot_state_background_layer,
};
pub use tokens::{BUILT_IN_THEMES, Palette, metrics};

/// The web's `text-white` on solid destructive buttons (design-system §6.1); not a theme role.
pub const WHITE: u32 = 0xffff_ffff;

struct ActiveFonts([gpui::SharedString; 4]);
impl Global for ActiveFonts {}

struct ActivePalette(Palette, usize);

impl Global for ActivePalette {}

/// What the theme follows: the stored preferences plus the OS appearance the window reports.
#[derive(Debug, Clone, PartialEq)]
pub struct Preferences {
    pub theme: ThemePreference,
    pub mode: AppearanceMode,
    pub interface_font_size: f32,
    pub typography: Typography,
    pub chat_width: ChatWidth,
    pub diff_color_scheme: DiffColorScheme,
    pub contrast: u32,
    pub glass: u32,
    pub halves: BTreeMap<String, String>,
    pub custom: Vec<ThemeDefinition>,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            theme: ThemePreference::Default(AppearanceMode::System),
            mode: AppearanceMode::System,
            interface_font_size: metrics::ROOT_FONT_SIZE,
            typography: Typography::default(),
            chat_width: ChatWidth::Comfortable,
            diff_color_scheme: DiffColorScheme::default(),
            contrast: 100,
            glass: 80,
            halves: BTreeMap::new(),
            custom: Vec::new(),
        }
    }
}

struct ThemeState {
    preferences: Preferences,
    system_dark: bool,
    environment: Vec<ThemeDefinition>,
    preview: Option<themes::editor::Preview>,
    saved: Option<Rc<SavedTheme>>,
    prepared_preview: Option<PreparedPreview>,
    background_image: Option<std::sync::Arc<gpui::RenderImage>>,
    loaded_font_hashes: HashSet<(u64, u64, usize)>,
}

#[derive(Clone)]
struct SavedTheme {
    preferences: Preferences,
    palettes: [Palette; 2],
    slot_stylesheets: [Arc<SlotStylesheet>; 2],
    configs: [Rc<ThemeConfig>; 2],
    fonts: [[gpui::SharedString; 4]; 2],
}
#[derive(Clone)]
struct PreparedPreview {
    input: themes::editor::Preview,
    saved: Rc<SavedTheme>,
    palette: Palette,
    slot_stylesheet: Arc<SlotStylesheet>,
    config: Rc<ThemeConfig>,
    fonts: [gpui::SharedString; 4],
}
impl Global for ThemeState {}

pub fn palette(cx: &App) -> Palette {
    cx.global::<ActivePalette>().0
}

pub fn hsla(color: u32) -> Hsla {
    gpui::rgba(color).into()
}

pub fn init(cx: &mut App) {
    cx.set_global(ThemeState {
        preferences: Preferences::default(),
        system_dark: false,
        environment: Vec::new(),
        preview: None,
        saved: None,
        prepared_preview: None,
        background_image: None,
        loaded_font_hashes: HashSet::new(),
    });
    cache_font_inventory(cx);
    install(cx);
}

/// Builds `Preferences` from a `ClientSettingsDoc`, shared by first-frame boot and runtime edits.
pub fn preferences_from_doc(doc: &t3_core::client_settings::ClientSettingsDoc) -> Preferences {
    Preferences {
        theme: doc.theme(),
        mode: doc.appearance_mode(),
        interface_font_size: doc.interface_font_size() as f32,
        typography: doc.typography(),
        chat_width: doc.chat_width(),
        diff_color_scheme: doc.diff_color_scheme(),
        contrast: doc.appearance_contrast(),
        glass: doc.glass_opacity(),
        halves: themes::halves(doc),
        custom: themes::library(doc),
    }
}

/// Applies saved preferences and cached environment themes (`t3code:environment-themes:v1`)
/// before the initial window opens so the very first frame paints the user's selected theme
/// without a light/dark or stock-palette flash (`pingdotgg/t3code#36042`).
pub fn init_from_doc(doc: &t3_core::client_settings::ClientSettingsDoc, cx: &mut App) {
    let preferences = preferences_from_doc(doc);
    let cached_environment = themes::default_sync::stored_environment_themes(doc);
    let state = cx.global_mut::<ThemeState>();
    state.preferences = preferences;
    state.environment = cached_environment;
    state.saved = None;
    install(cx);
    let mode = if is_dark(cx) { ThemeMode::Dark } else { ThemeMode::Light };
    Theme::change(mode, None, cx);
}

/// Loads saved client settings from `T3_NATIVE_HOME` and installs them before window creation.
pub fn boot_saved(cx: &mut App) {
    let raw = t3_client::store::load_client_settings().unwrap_or_default();
    let doc = t3_core::client_settings::ClientSettingsDoc::from_json(&raw);
    init_from_doc(&doc, cx);
}

pub fn preferences(cx: &App) -> &Preferences {
    &cx.global::<ThemeState>().preferences
}

/// Applies new preferences to every window at once; no transition runs (design system §9).
pub fn set_preferences(preferences: Preferences, cx: &mut App) {
    let state = cx.global_mut::<ThemeState>();
    if state.preferences == preferences {
        return;
    }
    state.preferences = preferences;
    state.saved = None;
    install(cx);
    let mode = if is_dark(cx) { ThemeMode::Dark } else { ThemeMode::Light };
    Theme::change(mode, None, cx);
    cx.refresh_windows();
}

/// A `// rem` metric (web px at the 16px root) as rems, so it follows the interface font size
/// the way the root element's rem size does (design system §10.5).
pub fn rem(px: f32) -> Rems {
    rems(px / metrics::ROOT_FONT_SIZE)
}

/// The transcript and composer lane: 46rem comfortable, 72rem wide, `None` for full width
/// (`index.css` `--chat-content-max-width`).
pub fn chat_max_width(cx: &App) -> Option<Rems> {
    match preferences(cx).chat_width {
        ChatWidth::Comfortable => Some(rem(metrics::CHAT_CONTENT_MAX_WIDTH)),
        ChatWidth::Wide => Some(rem(metrics::CHAT_CONTENT_MAX_WIDTH_WIDE)),
        ChatWidth::Full => None,
    }
}

pub fn follow_system_appearance(window: &mut Window, cx: &mut App) {
    apply(window.appearance(), window, cx);
    window
        .observe_window_appearance(|window, cx| apply(window.appearance(), window, cx))
        .detach();
}

fn apply(appearance: WindowAppearance, window: &mut Window, cx: &mut App) {
    cx.global_mut::<ThemeState>().system_dark = matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark);
    install(cx);
    let mode = if is_dark(cx) { ThemeMode::Dark } else { ThemeMode::Light };
    Theme::change(mode, Some(window), cx);
}

fn is_dark(cx: &App) -> bool {
    let state = cx.global::<ThemeState>();
    if let Some(preview) = &state.preview {
        return preview.dark;
    }
    let prefs = &state.preferences;
    themes::resolved_dark(
        &prefs.theme,
        &prefs.halves,
        themes::mode_dark(prefs.mode, state.system_dark),
        &prefs.custom,
        &state.environment,
    )
}

pub fn environment_themes(cx: &App) -> &[ThemeDefinition] {
    &cx.global::<ThemeState>().environment
}

/// The currently active saved/published theme id for the active appearance half, ignoring any
/// live editor draft preview so wallpaper retries stay bound to the applied theme
/// (`pingdotgg/t3code#38132`).
pub fn active_theme_id(cx: &App) -> Option<String> {
    let state = cx.global::<ThemeState>();
    let prefs = &state.preferences;
    let dark = themes::resolved_dark(
        &prefs.theme,
        &prefs.halves,
        themes::mode_dark(prefs.mode, state.system_dark),
        &prefs.custom,
        &state.environment,
    );
    themes::half_theme(&prefs.theme, &prefs.halves, dark, &prefs.custom, &state.environment)
        .map(|definition| definition.id.clone())
}

pub fn background_image(cx: &App) -> Option<std::sync::Arc<gpui::RenderImage>> {
    cx.global::<ThemeState>().background_image.clone()
}

pub fn set_background_image(image: Option<std::sync::Arc<gpui::RenderImage>>, cx: &mut App) {
    let state = cx.global_mut::<ThemeState>();
    let changed = match (&state.background_image, &image) {
        (None, None) => false,
        (Some(a), Some(b)) => !std::sync::Arc::ptr_eq(a, b),
        _ => true,
    };
    if changed {
        state.background_image = image;
        cx.refresh_windows();
    }
}

pub fn set_environment_themes(values: &[ThemeDefinition], cx: &mut App) {
    if cx.global::<ThemeState>().environment == values {
        return;
    }
    cx.global_mut::<ThemeState>().environment = values.to_vec();
    cx.global_mut::<ThemeState>().saved = None;
    install(cx);
    let mode = if is_dark(cx) { ThemeMode::Dark } else { ThemeMode::Light };
    Theme::change(mode, None, cx);
    cx.refresh_windows();
}

fn derived(
    theme: &ThemePreference,
    dark: bool,
    prefs: &Preferences,
    environment: &[ThemeDefinition],
) -> (Palette, Arc<SlotStylesheet>) {
    let definition = themes::half_theme(theme, &prefs.halves, dark, &prefs.custom, environment);
    let selected = definition.map(|t| t.id.as_str());
    let roles = definition.and_then(|t| t.half(dark));
    let theme_css = definition.and_then(|t| t.css.as_deref());
    let base = selected
        .and_then(|id| BUILT_IN_THEMES.iter().find(|t| t.id == id))
        .and_then(|t| if dark { t.dark } else { t.light })
        .unwrap_or(if dark { tokens::DARK } else { tokens::LIGHT });
    let Ok((colors, slot_sheet)) =
        themes::palette_and_slots_for_with_css(selected, roles, theme_css, dark, prefs.contrast, prefs.glass)
    else {
        return (base, Arc::new(SlotStylesheet::empty()));
    };
    let mut value = serde_json::to_value(base).expect("serializable palette");
    if let Some(map) = value.as_object_mut() {
        map.extend(colors.into_iter().map(|(k, v)| (k, v.into())));
    }
    let p = serde_json::from_value(value).unwrap_or(base);
    let p = if prefs.diff_color_scheme == DiffColorScheme::BlueOrange {
        blue_orange(p)
    } else {
        p
    };
    (p, Arc::new(slot_sheet))
}

fn blue_orange(mut palette: Palette) -> Palette {
    palette.diff_addition = palette.blue_orange_diff_addition;
    palette.diff_deletion = palette.blue_orange_diff_deletion;
    palette.diff_addition_foreground = palette.blue_orange_diff_addition_foreground;
    palette.diff_deletion_foreground = palette.blue_orange_diff_deletion_foreground;
    palette.diff_modified_icon = palette.blue_orange_diff_modified_icon;
    palette.diff_row_addition = palette.blue_orange_diff_row_addition;
    palette.diff_row_addition_gutter = palette.blue_orange_diff_row_addition_gutter;
    palette.diff_row_deletion = palette.blue_orange_diff_row_deletion;
    palette.diff_row_deletion_gutter = palette.blue_orange_diff_row_deletion_gutter;
    palette
}

struct FontInventory {
    names: Vec<String>,
    mono: String,
    serif: String,
}
impl Global for FontInventory {}

fn cache_font_inventory(cx: &mut App) {
    let names = cx.text_system().all_font_names();
    let first_installed = |stack: &[&str]| {
        stack
            .iter()
            .find_map(|family| names.iter().find(|name| name.eq_ignore_ascii_case(family)))
            .map(String::to_owned)
            .unwrap_or_else(|| stack[0].to_owned())
    };
    let mono = first_installed(MONO_STACK);
    let serif = first_installed(&["Times", "Times New Roman", "Georgia"]);
    cx.set_global(FontInventory { names, mono, serif });
}

pub fn installed_font_families(cx: &App) -> &[String] {
    &cx.global::<FontInventory>().names
}

pub fn default_mono_family(cx: &App) -> &str {
    &cx.global::<FontInventory>().mono
}

fn embedded_font_fingerprint(bytes: &[u8]) -> (u64, u64, usize) {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x9e37_79b9_7f4a_7c15;
    for &b in bytes {
        h1 ^= u64::from(b);
        h1 = h1.wrapping_mul(0x0000_0100_0000_01b3);
        h2 = h2.wrapping_mul(0xbf58_476d_1ce4_e5b9) ^ u64::from(b);
    }
    (h1, h2, bytes.len())
}

/// Upper bound on decoded SFNT output bytes from an embedded `@font-face` payload (4x the 4 MiB
/// compressed input cap). Guards against crafted WOFF1 headers declaring huge `origLength` or
/// `totalSfntSize` values that would otherwise trigger unbounded `Vec::with_capacity` allocations
/// inside `wuff::decompress_woff1`.
const MAX_DECODED_SFNT_BYTES: usize = 16 * 1024 * 1024;

/// Validates WOFF1 header and table directory bounds before invoking `wuff::decompress_woff1`.
///
/// `wuff::decompress_woff1` passes each table's untrusted `origLength` straight to
/// `Vec::with_capacity`, so we verify every table's bounds and the cumulative reconstructed SFNT
/// size against [`MAX_DECODED_SFNT_BYTES`] up front.
fn validate_woff1_bounds(bytes: &[u8]) -> bool {
    const WOFF1_HEADER_LEN: usize = 44;
    const WOFF1_TABLE_ENTRY_LEN: usize = 20;
    const SFNT_HEADER_LEN: usize = 12;
    const SFNT_TABLE_ENTRY_LEN: usize = 16;

    if bytes.len() < WOFF1_HEADER_LEN || &bytes[0..4] != b"wOFF" {
        return false;
    }
    let read_u16 = |offset: usize| u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
    let read_u32 = |offset: usize| {
        u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ])
    };

    let declared_len = read_u32(8) as usize;
    let num_tables = read_u16(12) as usize;
    let reserved = read_u16(14);
    let total_sfnt_size = read_u32(16) as usize;

    if declared_len != bytes.len()
        || num_tables == 0
        || reserved != 0
        || total_sfnt_size == 0
        || total_sfnt_size > MAX_DECODED_SFNT_BYTES
    {
        return false;
    }

    let Some(dir_end) = num_tables
        .checked_mul(WOFF1_TABLE_ENTRY_LEN)
        .and_then(|n| WOFF1_HEADER_LEN.checked_add(n))
    else {
        return false;
    };
    if dir_end > bytes.len() {
        return false;
    }

    let Some(mut expected_sfnt_len) = num_tables
        .checked_mul(SFNT_TABLE_ENTRY_LEN)
        .and_then(|n| SFNT_HEADER_LEN.checked_add(n))
    else {
        return false;
    };
    if expected_sfnt_len > MAX_DECODED_SFNT_BYTES {
        return false;
    }

    for i in 0..num_tables {
        let entry = WOFF1_HEADER_LEN + i * WOFF1_TABLE_ENTRY_LEN;
        let woff_offset = read_u32(entry + 4) as usize;
        let woff_length = read_u32(entry + 8) as usize;
        let orig_length = read_u32(entry + 12) as usize;

        if woff_length > orig_length || orig_length > MAX_DECODED_SFNT_BYTES {
            return false;
        }
        let Some(table_end) = woff_offset.checked_add(woff_length) else {
            return false;
        };
        if table_end > bytes.len() {
            return false;
        }
        let Some(padded_orig) = orig_length.checked_add(3).map(|n| n & !3) else {
            return false;
        };
        let Some(next_total) = expected_sfnt_len.checked_add(padded_orig) else {
            return false;
        };
        if next_total > MAX_DECODED_SFNT_BYTES {
            return false;
        }
        expected_sfnt_len = next_total;
    }

    true
}

fn decode_woff1_bounded(bytes: &[u8]) -> Option<Vec<u8>> {
    if !validate_woff1_bounds(bytes) {
        return None;
    }
    let sfnt = wuff::decompress_woff1(bytes).ok()?;
    (sfnt.len() <= MAX_DECODED_SFNT_BYTES).then_some(sfnt)
}

fn decode_woff2_bounded(bytes: &[u8]) -> Option<Vec<u8>> {
    let sfnt = wuff::decompress_woff2(bytes).ok()?;
    (sfnt.len() <= MAX_DECODED_SFNT_BYTES).then_some(sfnt)
}

fn decode_embedded_font_sfnt(face: &EmbeddedFontFace) -> Option<Vec<u8>> {
    match face.format {
        EmbeddedFontFormat::Woff2 => decode_woff2_bounded(&face.bytes),
        EmbeddedFontFormat::Woff => decode_woff1_bounded(&face.bytes),
        EmbeddedFontFormat::TrueType | EmbeddedFontFormat::OpenType => {
            (face.bytes.len() <= MAX_DECODED_SFNT_BYTES).then(|| face.bytes.clone())
        }
        EmbeddedFontFormat::Unknown => {
            if face.bytes.starts_with(b"wOF2") {
                decode_woff2_bounded(&face.bytes)
            } else if face.bytes.starts_with(b"wOFF") {
                decode_woff1_bounded(&face.bytes)
            } else if face.bytes.starts_with(b"OTTO")
                || face.bytes.starts_with(&[0x00, 0x01, 0x00, 0x00])
                || face.bytes.starts_with(b"true")
            {
                (face.bytes.len() <= MAX_DECODED_SFNT_BYTES).then(|| face.bytes.clone())
            } else {
                None
            }
        }
    }
}

fn register_embedded_fonts(faces: &[EmbeddedFontFace], cx: &mut App) {
    if faces.is_empty() {
        return;
    }
    let mut pending: Vec<Cow<'static, [u8]>> = Vec::new();
    for face in faces {
        let key = embedded_font_fingerprint(&face.bytes);
        if !cx.global_mut::<ThemeState>().loaded_font_hashes.insert(key) {
            continue;
        }
        match decode_embedded_font_sfnt(face) {
            Some(sfnt) if !sfnt.is_empty() => pending.push(Cow::Owned(sfnt)),
            _ => {
                tracing::debug!(family = %face.family, format = ?face.format, "skipping undecodable embedded theme font");
            }
        }
    }
    if pending.is_empty() {
        return;
    }
    match cx.text_system().add_fonts(pending) {
        Ok(()) => cache_font_inventory(cx),
        Err(error) => tracing::warn!(%error, "failed to register embedded theme fonts"),
    }
}

fn resolve_family(role: FontRole, theme_fonts: &ThemeCssFonts, cx: &App) -> gpui::SharedString {
    let inventory = cx.global::<FontInventory>();
    let user_family = preferences(cx).typography.family(role);
    let fallback = if role.monospace() {
        inventory.mono.clone().into()
    } else if role == FontRole::Prompt {
        resolve_family(FontRole::Interface, theme_fonts, cx)
    } else {
        gpui::SharedString::from(".SystemUIFont")
    };
    let effective_stack = if !user_family.is_empty() {
        user_family
    } else if role == FontRole::Interface {
        theme_fonts.sans.as_deref().unwrap_or("")
    } else if role == FontRole::Code
        || (role == FontRole::Terminal && !preferences(cx).typography.separate)
    {
        theme_fonts.mono.as_deref().unwrap_or("")
    } else {
        ""
    };
    resolve_font_stack(
        effective_stack,
        &inventory.names,
        FontDefaults {
            sans: ".SystemUIFont",
            mono: &inventory.mono,
            serif: &inventory.serif,
        },
        &fallback,
    )
    .to_owned()
    .into()
}

pub fn font_family(role: FontRole, cx: &App) -> gpui::SharedString {
    cx.global::<ActiveFonts>().0[role as usize].clone()
}

pub fn font_size(role: FontRole, cx: &App) -> f32 {
    let prefs = preferences(cx);
    if role == FontRole::Prompt && !prefs.typography.separate {
        return prefs.interface_font_size * 14. / 16.;
    }
    prefs.typography.size(role)
}

pub fn code_line_height(cx: &App) -> f32 {
    font_size(FontRole::Code, cx) * 20. / 13.
}

fn install(cx: &mut App) {
    let dark = is_dark(cx);
    let saved = cx.global::<ThemeState>().saved.clone();
    let saved = saved.unwrap_or_else(|| {
        let preferences = preferences(cx).clone();
        let environment = cx.global::<ThemeState>().environment.clone();
        let [(light_palette, light_slots), (dark_palette, dark_slots)] =
            [false, true].map(|dark| derived(&preferences.theme, dark, &preferences, &environment));
        register_embedded_fonts(light_slots.font_faces(), cx);
        register_embedded_fonts(dark_slots.font_faces(), cx);
        let palettes = [light_palette, dark_palette];
        let slot_stylesheets = [light_slots, dark_slots];
        let fonts = [
            FontRole::ALL.map(|role| resolve_family(role, slot_stylesheets[0].theme_fonts(), cx)),
            FontRole::ALL.map(|role| resolve_family(role, slot_stylesheets[1].theme_fonts(), cx)),
        ];
        let configs = std::array::from_fn(|i| {
            Rc::new(config(
                if i == 0 { "T3 Code Light" } else { "T3 Code Dark" },
                &palettes[i],
                &slot_stylesheets[i],
                i == 1,
                preferences.interface_font_size,
                &fonts[i][FontRole::Code as usize],
                &fonts[i][FontRole::Interface as usize],
                preferences.typography.size(FontRole::Code),
            ))
        });
        let saved = Rc::new(SavedTheme {
            preferences,
            palettes,
            slot_stylesheets,
            configs,
            fonts,
        });
        cx.global_mut::<ThemeState>().saved = Some(saved.clone());
        saved
    });
    let preferences = &saved.preferences;
    let mut palettes = saved.palettes;
    let mut slot_stylesheets = saved.slot_stylesheets.clone();
    let mut configs = saved.configs.clone();
    let mut fonts = saved.fonts.clone();
    if let Some(input) = cx.global::<ThemeState>().preview.clone() {
        let cached = cx
            .global::<ThemeState>()
            .prepared_preview
            .as_ref()
            .filter(|p| p.input == input && Rc::ptr_eq(&p.saved, &saved))
            .cloned();
        let prepared = cached.or_else(|| {
            let (values, slot_sheet) = themes::palette_and_slots_for_with_css(
                None,
                Some(&input.colors),
                input.css.as_deref(),
                input.dark,
                preferences.contrast,
                preferences.glass,
            )
            .ok()?;
            register_embedded_fonts(slot_sheet.font_faces(), cx);
            let preview_fonts =
                FontRole::ALL.map(|role| resolve_family(role, slot_sheet.theme_fonts(), cx));
            let base = if input.dark { tokens::DARK } else { tokens::LIGHT };
            let mut value = serde_json::to_value(base).ok()?;
            value.as_object_mut()?.extend(values.into_iter().map(|(k, v)| (k, v.into())));
            let palette = serde_json::from_value(value).ok()?;
            let palette = if preferences.diff_color_scheme == DiffColorScheme::BlueOrange {
                blue_orange(palette)
            } else {
                palette
            };
            let slot_stylesheet = Arc::new(slot_sheet);
            let config = Rc::new(config(
                "Theme editor preview",
                &palette,
                &slot_stylesheet,
                input.dark,
                preferences.interface_font_size,
                &preview_fonts[FontRole::Code as usize],
                &preview_fonts[FontRole::Interface as usize],
                preferences.typography.size(FontRole::Code),
            ));
            let prepared = PreparedPreview {
                input: input.clone(),
                saved: saved.clone(),
                palette,
                slot_stylesheet,
                config,
                fonts: preview_fonts,
            };
            cx.global_mut::<ThemeState>().prepared_preview = Some(prepared.clone());
            Some(prepared)
        });
        if let Some(prepared) = prepared {
            let i = usize::from(input.dark);
            palettes[i] = prepared.palette;
            slot_stylesheets[i] = prepared.slot_stylesheet;
            configs[i] = prepared.config;
            fonts[i] = prepared.fonts;
        }
    }
    let active_idx = usize::from(dark);
    let active_fonts = fonts[active_idx].clone();
    cx.set_global(ActiveFonts(active_fonts.clone()));
    let revision = cx.try_global::<ActivePalette>().map_or(0, |p| p.1.wrapping_add(1));
    cx.set_global(ActivePalette(palettes[active_idx], revision));
    let next_slots = Arc::clone(&slot_stylesheets[active_idx]);
    let slots_changed = cx
        .try_global::<slots::ActiveSlotStylesheet>()
        .is_none_or(|prev| !Arc::ptr_eq(&prev.0, &next_slots));
    if slots_changed {
        slots::clear_gradient_cache();
    }
    cx.set_global(slots::ActiveSlotStylesheet(next_slots));
    let theme = Theme::global_mut(cx);
    [theme.light_theme, theme.dark_theme] = configs;
    theme.font_size = px(preferences.interface_font_size);
    theme.font_family = active_fonts[FontRole::Interface as usize].clone();
    theme.mono_font_family = active_fonts[FontRole::Code as usize].clone();
    theme.mono_font_size = px(preferences.typography.size(FontRole::Code));
}

/// A theme's swatch colors for the theme cards: (background, card, primary, foreground).
pub fn swatch(theme: &ThemePreference, dark: bool, cx: &App) -> [u32; 4] {
    if let Some(p) = theme
        .named_id()
        .and_then(|id| BUILT_IN_THEMES.iter().find(|t| t.id == id))
        .and_then(|t| if dark { t.dark } else { t.light })
    {
        return [p.background, p.card, p.primary, p.foreground];
    }
    let prefs = preferences(cx);
    if let Some(t) = theme
        .named_id()
        .and_then(|id| themes::resolve(id, &prefs.custom, environment_themes(cx)))
        && let Some(roles) = t.half(dark)
    {
        return themes::swatch(roles);
    }
    let p = if dark { tokens::DARK } else { tokens::LIGHT };
    [p.background, p.card, p.primary, p.foreground]
}

fn hex(color: u32) -> String {
    format!("#{color:08x}")
}

/// The web's syntax palette (`SYNTAX_*` from `@pierre/highlights`) over the code surface, as
/// gpui-component's Zed-format highlight theme.
fn highlight(p: &Palette, dark: bool) -> serde_json::Value {
    let styles = if dark { tokens::SYNTAX_DARK } else { tokens::SYNTAX_LIGHT };
    let syntax: serde_json::Map<String, serde_json::Value> = styles
        .iter()
        .map(|style| {
            let mut value = serde_json::Map::new();
            if let Some(color) = style.color {
                value.insert("color".to_owned(), hex(color).into());
            }
            if style.italic {
                value.insert("font_style".to_owned(), "italic".into());
            }
            if style.bold {
                value.insert("font_weight".to_owned(), 700.into());
            }
            (style.capture.to_owned(), serde_json::Value::Object(value))
        })
        .collect();
    json!({ "editor.background": hex(p.code_background), "editor.foreground": hex(p.code_foreground), "syntax": syntax })
}

fn with_alpha_scale(rgba: u32, opacity: f32) -> u32 {
    let alpha = ((rgba & 0xff) as f32 * opacity.clamp(0.0, 1.0)).round() as u32;
    (rgba & 0xffff_ff00) | (alpha & 0xff)
}

fn slot_bg_rgba(ov: &SlotStyleOverride) -> Option<u32> {
    if ov.display_none == Some(true) {
        return Some(0x0000_0000);
    }
    match ov.background.as_ref()? {
        t3_core::themes::slots::SlotBackground::Solid(c) => Some(*c),
        t3_core::themes::slots::SlotBackground::Transparent => Some(0x0000_0000),
        t3_core::themes::slots::SlotBackground::LinearGradient { stops, .. }
        | t3_core::themes::slots::SlotBackground::RadialGradient { stops, .. } => {
            stops.first().map(|s| s.color)
        }
        t3_core::themes::slots::SlotBackground::ThemeBackgroundImage { overlay_color, .. } => *overlay_color,
    }
}

fn slot_has_bg_or_opacity(ov: &SlotStyleOverride) -> bool {
    ov.display_none == Some(true) || ov.background.is_some() || ov.opacity.is_some()
}

fn apply_slot_color_override(base: u32, ov: &SlotStyleOverride) -> u32 {
    let color = slot_bg_rgba(ov).unwrap_or(base);
    match ov.opacity {
        Some(opacity) => with_alpha_scale(color, opacity),
        None => color,
    }
}

#[allow(clippy::too_many_arguments)]
fn config(
    name: &str,
    p: &Palette,
    slots: &SlotStylesheet,
    dark: bool,
    font_size: f32,
    mono: &str,
    sans: &str,
    code_size: f32,
) -> ThemeConfig {
    let mut input_border = p.input;
    if let Some(set) = slots.get(ThemeSlot::Input) {
        if let Some(border) = set.base.border_color {
            input_border = match set.base.opacity {
                Some(opacity) => with_alpha_scale(border, opacity),
                None => border,
            };
        } else if let Some(bg) = slot_bg_rgba(&set.base).filter(|c| *c & 0xff > 0) {
            input_border = match set.base.opacity {
                Some(opacity) => with_alpha_scale(bg, opacity),
                None => bg,
            };
        }
    }

    let mut scrollbar_thumb = p.scrollbar_thumb;
    let mut scrollbar_thumb_hover = p.scrollbar_thumb_hover;
    if let Some(set) = slots.get(ThemeSlot::ScrollAreaThumb) {
        if slot_has_bg_or_opacity(&set.base) {
            scrollbar_thumb = apply_slot_color_override(scrollbar_thumb, &set.base);
            scrollbar_thumb_hover = scrollbar_thumb;
        }
        if slot_has_bg_or_opacity(&set.hover) {
            scrollbar_thumb_hover = apply_slot_color_override(scrollbar_thumb_hover, &set.hover);
        }
    }

    let mut scrollbar_bg = 0x0000_0000u32;
    if let Some(set) = slots.get(ThemeSlot::ScrollAreaScrollbar) {
        if slot_has_bg_or_opacity(&set.base) {
            scrollbar_bg = apply_slot_color_override(scrollbar_bg, &set.base);
        }
        if set.base.display_none == Some(true) {
            scrollbar_thumb = 0x0000_0000;
            scrollbar_thumb_hover = 0x0000_0000;
        } else if let Some(opacity) = set.base.opacity {
            scrollbar_thumb = with_alpha_scale(scrollbar_thumb, opacity);
            scrollbar_thumb_hover = with_alpha_scale(scrollbar_thumb_hover, opacity);
        }
    }

    let colors: serde_json::Map<String, serde_json::Value> = [
        ("background", hex(p.background)),
        ("foreground", hex(p.foreground)),
        ("border", hex(p.border)),
        ("input.border", hex(input_border)),
        ("ring", hex(p.ring)),
        ("caret", hex(p.foreground)),
        ("selection.background", hex(p.search_match_background)),
        ("link", hex(p.info_foreground)),
        ("link.hover", hex(p.info_foreground)),
        ("link.active", hex(p.info_foreground)),
        ("slider.background", hex(p.primary)),
        ("slider.thumb.background", hex(p.background)),
        ("primary.background", hex(p.primary)),
        ("primary.foreground", hex(p.primary_foreground)),
        ("primary.hover.background", hex(p.message_action_hover)),
        ("primary.active.background", hex(p.message_action_hover)),
        ("secondary.background", hex(p.secondary)),
        ("secondary.foreground", hex(p.secondary_foreground)),
        ("secondary.hover.background", hex(p.accent)),
        ("secondary.active.background", hex(p.accent)),
        ("muted.background", hex(p.muted)),
        ("muted.foreground", hex(p.muted_foreground)),
        ("accent.background", hex(p.accent)),
        ("accent.foreground", hex(p.accent_foreground)),
        ("popover.background", hex(p.popover)),
        ("popover.foreground", hex(p.popover_foreground)),
        ("danger.background", hex(p.destructive)),
        ("danger.foreground", hex(p.destructive_foreground)),
        ("warning.background", hex(p.warning)),
        ("warning.foreground", hex(p.warning_foreground)),
        ("success.background", hex(p.success)),
        ("success.foreground", hex(p.success_foreground)),
        ("info.background", hex(p.info)),
        ("info.foreground", hex(p.info_foreground)),
        ("list.hover.background", hex(p.accent)),
        ("list.active.background", hex(p.accent)),
        ("list.active.border", hex(p.ring)),
        ("scrollbar.background", hex(scrollbar_bg)),
        ("scrollbar.thumb.background", hex(scrollbar_thumb)),
        ("scrollbar.thumb.hover.background", hex(scrollbar_thumb_hover)),
        ("sidebar.background", hex(p.sidebar)),
        ("sidebar.foreground", hex(p.sidebar_foreground)),
        ("sidebar.border", hex(p.sidebar_border)),
        ("sidebar.accent.background", hex(p.sidebar_row_selected)),
        ("sidebar.accent.foreground", hex(p.sidebar_foreground)),
        ("sidebar.primary.background", hex(p.primary)),
        ("sidebar.primary.foreground", hex(p.primary_foreground)),
        ("title_bar.background", hex(p.app_chrome_background)),
        ("title_bar.border", hex(p.border)),
        ("window.border", hex(p.border)),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), serde_json::Value::String(value)))
    .collect();
    let value = json!({
        "name": name,
        "mode": if dark { "dark" } else { "light" },
        "font.family": sans,
        "font.size": font_size,
        "mono_font.family": mono,
        "mono_font.size": code_size,
        "radius": metrics::CONTROL_RADIUS as usize,
        "radius.lg": metrics::RADIUS_XL as usize,
        "colors": colors,
        "highlight": highlight(p, dark),
    });
    serde_json::from_value(value).unwrap_or_else(|error| {
        tracing::warn!(%error, "invalid theme config");
        ThemeConfig::default()
    })
}

pub fn revision(cx: &App) -> usize {
    cx.global::<ActivePalette>().1
}

/// Prepared once per editor change; rendering only reads the installed palette.
pub fn preview(colors: Option<(&themes::Colors, bool)>, cx: &mut App) {
    preview_with_css(colors.map(|(c, dark)| (c, None, dark)), cx);
}

/// Prepared once per editor change, including live custom-property CSS overrides.
pub fn preview_with_css(colors: Option<(&themes::Colors, Option<&str>, bool)>, cx: &mut App) {
    let input = colors.map(|(colors, css, dark)| themes::editor::Preview::new_with_css(dark, colors, css));
    if cx.global::<ThemeState>().preview == input {
        return;
    }
    cx.global_mut::<ThemeState>().preview = input;
    install(cx);
    let mode = if is_dark(cx) { ThemeMode::Dark } else { ThemeMode::Light };
    Theme::change(mode, None, cx);
    cx.refresh_windows();
}

pub fn wireframe(theme: &ThemePreference, dark: bool, cx: &App) -> [u32; 7] {
    let prefs = preferences(cx);
    themes::half_theme(theme, &prefs.halves, dark, &prefs.custom, environment_themes(cx))
        .map_or_else(|| standard_wireframe(dark), |theme| theme_wireframe(theme, dark))
}

fn theme_wireframe(theme: &ThemeDefinition, dark: bool) -> [u32; 7] {
    themes::wireframe(theme.half(dark).unwrap_or(&theme.colors))
}

fn standard_wireframe(dark: bool) -> [u32; 7] {
    // ThemePreviewCircles.tsx: canvas, sidebar, surface, accentSurface, messageSurface, messageAction.
    if dark {
        [0x0a0a0aff, 0x0f0f10ff, 0x121212ff, 0x27272aff, 0x27272aff, 0x8b9cffff, tokens::DARK.foreground]
    } else {
        [0xfcfcfcff, 0xfafafaff, 0xffffffff, 0xf4f4f5ff, 0xe4e4e7ff, 0x4f46e5ff, tokens::LIGHT.foreground]
    }
}

#[cfg(feature="snapshot")]
pub fn verify_saved_palette(cx:&App)->Result<(),String> {
    let state=cx.global::<ThemeState>();
    if state.preview.is_some() {return Err("A discarded editor restored its preview.".into());}
    let prefs=&state.preferences;
    let dark=themes::resolved_dark(&prefs.theme,&prefs.halves,themes::mode_dark(prefs.mode,state.system_dark),&prefs.custom,&state.environment);
    let saved=state.saved.as_ref().ok_or("The saved theme was not prepared.")?;
    if serde_json::to_value(palette(cx)).unwrap()!=serde_json::to_value(saved.palettes[usize::from(dark)]).unwrap() {
        return Err("The saved palette is not active.".into());
    }
    Ok(())
}

#[cfg(feature="snapshot")]
pub fn verify_preview_source(colors:&themes::Colors,dark:bool,cx:&App)->Result<(),String> {
    if cx.global::<ThemeState>().preview.as_ref()!=Some(&themes::editor::Preview::new(dark,colors)) {
        return Err("A replaced editor restored its preview.".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_syntax_theme(dark: bool) -> std::sync::Arc<gpui_component::highlighter::HighlightTheme> {
    let palette = if dark { tokens::DARK } else { tokens::LIGHT };
    let empty_slots = SlotStylesheet::empty();
    let config = config("test", &palette, &empty_slots, dark, 16., "Menlo", "System", 13.);
    std::sync::Arc::new(gpui_component::highlighter::HighlightTheme { name: "test".into(), appearance: config.mode, style: config.highlight.unwrap() })
}

// Command queries retain roles (such as variable.parameter) absent from gpui-component's SyntaxColors.
pub(crate) fn syntax_style(capture: &str, dark: bool) -> Option<gpui::HighlightStyle> {
    let styles = if dark { tokens::SYNTAX_DARK } else { tokens::SYNTAX_LIGHT };
    styles.iter().find(|style| style.capture == capture).map(|style| gpui::HighlightStyle {
        color: style.color.map(hsla),
        font_style: style.italic.then_some(gpui::FontStyle::Italic),
        font_weight: style.bold.then_some(gpui::FontWeight::BOLD),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    #[gpui::test]
    fn default_dark_wireframe_matches_web_preview(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            init(cx);
            let colors = wireframe(&ThemePreference::Default(AppearanceMode::System), true, cx);
            assert_eq!(&colors[..6], &[0x0a0a0aff, 0x0f0f10ff, 0x121212ff, 0x27272aff, 0x27272aff, 0x8b9cffff]);
        });
    }

    #[test]
    fn wireframe_falls_back_to_the_single_mode_of_a_named_theme() {
        let mut theme = themes::builtins()[0].clone();
        theme.variants.clear();
        let dark = theme.appearance == "dark";
        assert_eq!(theme_wireframe(&theme, !dark), theme_wireframe(&theme, dark));
    }

    use super::*;
    use gpui_component::ActiveTheme;
    use t3_core::client_settings::ClientSettingsDoc;

    #[gpui::test]
    fn first_frame_boot_paints_saved_environment_theme_and_css_before_connect(cx: &mut gpui::TestAppContext) {
        let mut published = themes::environment(&[json!({
            "id": "nightfall",
            "name": "Nightfall",
            "appearance": "dark",
            "canvas": "#1a1b26",
            "accent": "#7aa2f7",
            "css": ":root { --primary: #ff44aa; } .dark { --background: #101118; }"
        })]);
        let nightfall = published.pop().unwrap();
        let doc = ClientSettingsDoc::from_json(
            &json!({
                "t3code:theme": "nightfall",
                "t3code:theme-appearance-mode": "dark",
                "fontSizeInterface": 18,
                themes::ENVIRONMENT_THEMES_STORAGE_KEY: [nightfall],
            })
            .to_string(),
        );

        cx.update(|cx| {
            gpui_component::init(cx);
            init(cx);
            init_from_doc(&doc, cx);

            assert!(cx.theme().is_dark(), "saved dark environment theme activates dark mode immediately");
            assert_eq!(environment_themes(cx).len(), 1);
            assert_eq!(palette(cx).background, 0x1011_18ff);
            assert_eq!(palette(cx).primary, 0xff44_aaff);
            assert_eq!(cx.theme().font_size, px(18.));
        });
    }

    #[gpui::test]
    fn live_css_preview_overrides_palette_and_restores_on_clear(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            init(cx);
            let before_bg = palette(cx).background;
            let before_primary = palette(cx).primary;
            let colors = themes::standard(true);

            preview_with_css(
                Some((&colors, Some(".dark { --background: #223344; --primary: #55ee88; }"), true)),
                cx,
            );
            assert!(cx.theme().is_dark());
            assert_eq!(palette(cx).background, 0x2233_44ff);
            assert_eq!(palette(cx).primary, 0x55ee_88ff);

            preview(None, cx);
            assert_eq!(palette(cx).background, before_bg);
            assert_eq!(palette(cx).primary, before_primary);
        });
    }

    #[gpui::test]
    fn scrollbar_and_input_slots_update_theme_config_live_and_restore_on_clear(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            init(cx);
            let colors = themes::standard(true);

            // Establish stock dark baseline.
            preview_with_css(Some((&colors, None, true)), cx);
            let stock_scrollbar = cx.theme().scrollbar;
            let stock_thumb = cx.theme().scrollbar_thumb;
            let stock_thumb_hover = cx.theme().scrollbar_thumb_hover;
            let stock_input = cx.theme().input;

            // [data-slot="scroll-area-scrollbar"], [data-slot="scroll-area-thumb"](:hover), and [data-slot="input"] border.
            preview_with_css(
                Some((
                    &colors,
                    Some(
                        r#"
                        [data-slot="scroll-area-scrollbar"] { background: #102030; }
                        [data-slot="scroll-area-thumb"] { background: #445566; }
                        [data-slot="scroll-area-thumb"]:hover { background: #778899; }
                        [data-slot="input"] { border-color: #33aa77; background: #111111; }
                        "#,
                    ),
                    true,
                )),
                cx,
            );
            assert_eq!(cx.theme().scrollbar, hsla(0x1020_30ff));
            assert_eq!(cx.theme().scrollbar_thumb, hsla(0x4455_66ff));
            assert_eq!(cx.theme().scrollbar_thumb_hover, hsla(0x7788_99ff));
            assert_eq!(cx.theme().input, hsla(0x33aa_77ff));

            // Scrollbar container opacity scales both track and thumb; input background falls back when border-color omitted.
            preview_with_css(
                Some((
                    &colors,
                    Some(
                        r#"
                        [data-slot="scroll-area-scrollbar"] { background: #102030; opacity: 0.5; }
                        [data-slot="scroll-area-thumb"] { background: #445566; }
                        [data-slot="input"] { background: #224466; }
                        "#,
                    ),
                    true,
                )),
                cx,
            );
            assert_eq!(cx.theme().scrollbar, hsla(0x1020_3080));
            assert_eq!(cx.theme().scrollbar_thumb, hsla(0x4455_6680));
            assert_eq!(cx.theme().scrollbar_thumb_hover, hsla(0x4455_6680));
            assert_eq!(cx.theme().input, hsla(0x2244_66ff));

            // ::-webkit-scrollbar and ::-webkit-scrollbar-thumb aliases map into ThemeConfig too.
            preview_with_css(
                Some((
                    &colors,
                    Some(
                        r#"
                        ::-webkit-scrollbar { background: #221133; }
                        ::-webkit-scrollbar-thumb { background: #aa33cc; }
                        ::-webkit-scrollbar-thumb:hover { background: #ff55ee; }
                        "#,
                    ),
                    true,
                )),
                cx,
            );
            assert_eq!(cx.theme().scrollbar, hsla(0x2211_33ff));
            assert_eq!(cx.theme().scrollbar_thumb, hsla(0xaa33_ccff));
            assert_eq!(cx.theme().scrollbar_thumb_hover, hsla(0xff55_eeff));

            // display: none hides scrollbar track and thumb cleanly.
            preview_with_css(
                Some((
                    &colors,
                    Some(r#"[data-slot="scroll-area-scrollbar"] { display: none; }"#),
                    true,
                )),
                cx,
            );
            assert_eq!(cx.theme().scrollbar.a, 0.0);
            assert_eq!(cx.theme().scrollbar_thumb.a, 0.0);
            assert_eq!(cx.theme().scrollbar_thumb_hover.a, 0.0);

            // Clearing CSS restores stock ThemeConfig colors cleanly.
            preview_with_css(Some((&colors, None, true)), cx);
            assert_eq!(cx.theme().scrollbar, stock_scrollbar);
            assert_eq!(cx.theme().scrollbar_thumb, stock_thumb);
            assert_eq!(cx.theme().scrollbar_thumb_hover, stock_thumb_hover);
            assert_eq!(cx.theme().input, stock_input);
            preview(None, cx);
        });
    }

    #[gpui::test]
    fn theme_css_typography_defaults_apply_and_yield_to_user_preferences(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            init(cx);

            let installed = installed_font_families(cx).to_vec();
            let Some(picked) = installed
                .iter()
                .find(|name| !name.starts_with('.') && !name.eq_ignore_ascii_case("Menlo"))
                .cloned()
            else {
                return;
            };
            let colors = themes::standard(true);
            let css = format!(r#"body {{ font-family: "{picked}", sans-serif; }}"#);

            // With no user font preference, theme CSS `body { font-family }` applies to Interface & Prompt.
            preview_with_css(Some((&colors, Some(&css), true)), cx);
            assert_eq!(font_family(FontRole::Interface, cx).as_ref(), picked.as_str());
            assert_eq!(font_family(FontRole::Prompt, cx).as_ref(), picked.as_str());
            assert_eq!(cx.theme().font_family.as_ref(), picked.as_str());

            // Explicit user font preference takes precedence over theme CSS defaults.
            if let Some(user_font) = installed
                .iter()
                .find(|name| !name.starts_with('.') && !name.eq_ignore_ascii_case(&picked))
                .cloned()
            {
                let mut prefs = preferences(cx).clone();
                prefs.typography.families[FontRole::Interface as usize] = user_font.clone();
                set_preferences(prefs, cx);
                assert_eq!(font_family(FontRole::Interface, cx).as_ref(), user_font.as_str());
            }

            preview(None, cx);
        });
    }

    fn build_test_woff1(total_sfnt_size: u32, orig_length: u32, table_data: &[u8]) -> Vec<u8> {
        let table_offset: u32 = 44 + 20;
        let padded_table_len = (table_data.len() + 3) & !3;
        let total_len = table_offset as usize + padded_table_len;
        let mut buf = Vec::with_capacity(total_len);
        buf.extend_from_slice(b"wOFF");
        buf.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // flavor
        buf.extend_from_slice(&(total_len as u32).to_be_bytes()); // length
        buf.extend_from_slice(&1u16.to_be_bytes()); // numTables
        buf.extend_from_slice(&0u16.to_be_bytes()); // reserved
        buf.extend_from_slice(&total_sfnt_size.to_be_bytes()); // totalSfntSize
        buf.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
        buf.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
        buf.extend_from_slice(&[0u8; 20]); // metaOffset..privLength
        debug_assert_eq!(buf.len(), 44);
        // Single table directory entry (20 bytes)
        buf.extend_from_slice(b"head");
        buf.extend_from_slice(&table_offset.to_be_bytes()); // offset
        buf.extend_from_slice(&(table_data.len() as u32).to_be_bytes()); // compLength
        buf.extend_from_slice(&orig_length.to_be_bytes()); // origLength
        buf.extend_from_slice(&0u32.to_be_bytes()); // origChecksum
        buf.extend_from_slice(table_data);
        buf.resize(total_len, 0);
        buf
    }

    #[test]
    fn oversized_woff1_headers_are_rejected_without_allocating() {
        // Crafted WOFF1 declaring `origLength = u32::MAX` (compressed because compLength < origLength)
        // must be rejected before `wuff::decompress_woff1` attempts `Vec::with_capacity(u32::MAX)`.
        let bogus_table = build_test_woff1(64, u32::MAX, &[0x78, 0x9c, 0x03, 0x00]);
        for format in [EmbeddedFontFormat::Woff, EmbeddedFontFormat::Unknown] {
            let face = EmbeddedFontFace {
                family: "EvilWoff".to_owned(),
                format,
                bytes: bogus_table.clone(),
            };
            assert!(
                decode_embedded_font_sfnt(&face).is_none(),
                "expected oversized origLength WOFF1 ({format:?}) to be rejected"
            );
        }

        // Oversized `totalSfntSize` is also rejected.
        let bogus_total = build_test_woff1(u32::MAX, 4, &[1, 2, 3, 4]);
        let face_total = EmbeddedFontFace {
            family: "EvilTotal".to_owned(),
            format: EmbeddedFontFormat::Woff,
            bytes: bogus_total,
        };
        assert!(decode_embedded_font_sfnt(&face_total).is_none());

        // A small valid uncompressed WOFF1 still decodes cleanly into an SFNT header + table.
        let valid = build_test_woff1(12 + 16 + 4, 4, &[0xde, 0xad, 0xbe, 0xef]);
        let face_valid = EmbeddedFontFace {
            family: "SmallWoff".to_owned(),
            format: EmbeddedFontFormat::Woff,
            bytes: valid,
        };
        let sfnt = decode_embedded_font_sfnt(&face_valid).expect("valid minimal WOFF1 decodes");
        assert_eq!(&sfnt[0..4], &[0x00, 0x01, 0x00, 0x00]);
        assert!(sfnt.ends_with(&[0xde, 0xad, 0xbe, 0xef]));
    }
}
