//! One theme source. T3's palette (generated into `tokens.rs` from the web theme) is written into
//! gpui-component's `Theme` as light and dark configs, so primitives and app views read the same
//! values. Tokens gpui-component has no slot for (pills, message surface, sidebar rows…) are read
//! through `palette(cx)`. The theme, appearance mode, interface size and chat width follow the
//! client settings (`web/src/hooks/useTheme.ts`, `appearanceFonts.ts`), and switch atomically.

mod tokens;
pub mod html_render;

use std::collections::BTreeMap;
use std::rc::Rc;
use t3_core::themes::{self, ThemeDefinition};

use gpui::{App, Global, Hsla, Rems, Window, WindowAppearance, px, rems};
use gpui_component::{Theme, ThemeConfig, ThemeMode};
use serde_json::json;
use t3_core::client_settings::typography::{FontDefaults, FontRole, MONO_STACK, Typography, resolve_font_stack};
use t3_core::client_settings::{AppearanceMode, ChatWidth, DiffColorScheme, ThemePreference};

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
}

#[derive(Clone)]
struct SavedTheme {
    preferences: Preferences,
    palettes: [Palette; 2],
    configs: [Rc<ThemeConfig>; 2],
    fonts: [gpui::SharedString; 4],
}
#[derive(Clone)]
struct PreparedPreview {
    input: themes::editor::Preview,
    saved: Rc<SavedTheme>,
    palette: Palette,
    config: Rc<ThemeConfig>,
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
    });
    cache_font_inventory(cx);
    install(cx);
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
    let families_changed = state.preferences.typography.families != preferences.typography.families;
    state.preferences = preferences;
    state.saved = None;
    if families_changed {
        cache_font_inventory(cx);
    }
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

fn derived(theme: &ThemePreference, dark: bool, prefs: &Preferences, environment: &[ThemeDefinition]) -> Palette {
    let definition = themes::half_theme(theme, &prefs.halves, dark, &prefs.custom, environment);
    let selected = definition.map(|t| t.id.as_str());
    let roles = definition.and_then(|t| t.half(dark));
    let base = selected
        .and_then(|id| BUILT_IN_THEMES.iter().find(|t| t.id == id))
        .and_then(|t| if dark { t.dark } else { t.light })
        .unwrap_or(if dark { tokens::DARK } else { tokens::LIGHT });
    let Ok(colors) = themes::palette_for(selected, roles, dark, prefs.contrast, prefs.glass) else {
        return base;
    };
    let mut value = serde_json::to_value(base).expect("serializable palette");
    if let Some(map) = value.as_object_mut() {
        map.extend(colors.into_iter().map(|(k, v)| (k, v.into())));
    }
    let p = serde_json::from_value(value).unwrap_or(base);
    if prefs.diff_color_scheme == DiffColorScheme::BlueOrange {
        blue_orange(p)
    } else {
        p
    }
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

fn resolve_family(role: FontRole, cx: &App) -> gpui::SharedString {
    let inventory = cx.global::<FontInventory>();
    let family = preferences(cx).typography.family(role);
    let fallback = if role.monospace() {
        inventory.mono.clone().into()
    } else if role == FontRole::Prompt {
        resolve_family(FontRole::Interface, cx)
    } else {
        gpui::SharedString::from(".SystemUIFont")
    };
    resolve_font_stack(
        family,
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
        let fonts = FontRole::ALL.map(|role| resolve_family(role, cx));
        let environment = cx.global::<ThemeState>().environment.clone();
        let palettes = [false, true].map(|dark| derived(&preferences.theme, dark, &preferences, &environment));
        let configs = std::array::from_fn(|i| {
            Rc::new(config(
                if i == 0 { "T3 Code Light" } else { "T3 Code Dark" },
                &palettes[i],
                i == 1,
                preferences.interface_font_size,
                &fonts[FontRole::Code as usize],
                &fonts[FontRole::Interface as usize],
                preferences.typography.size(FontRole::Code),
            ))
        });
        let saved = Rc::new(SavedTheme {
            preferences,
            palettes,
            configs,
            fonts,
        });
        cx.global_mut::<ThemeState>().saved = Some(saved.clone());
        saved
    });
    let preferences = &saved.preferences;
    cx.set_global(ActiveFonts(saved.fonts.clone()));
    let mut palettes = saved.palettes;
    let mut configs = saved.configs.clone();
    if let Some(input) = cx.global::<ThemeState>().preview.clone() {
        let cached = cx
            .global::<ThemeState>()
            .prepared_preview
            .as_ref()
            .filter(|p| p.input == input && Rc::ptr_eq(&p.saved, &saved))
            .cloned();
        let prepared = cached.or_else(|| {
            let values = themes::palette(Some(&input.colors), input.dark, preferences.contrast, preferences.glass).ok()?;
            let base = if input.dark { tokens::DARK } else { tokens::LIGHT };
            let mut value = serde_json::to_value(base).ok()?;
            value.as_object_mut()?.extend(values.into_iter().map(|(k, v)| (k, v.into())));
            let palette = serde_json::from_value(value).ok()?;
            let palette = if preferences.diff_color_scheme == DiffColorScheme::BlueOrange {
                blue_orange(palette)
            } else {
                palette
            };
            let config = Rc::new(config(
                "Theme editor preview",
                &palette,
                input.dark,
                preferences.interface_font_size,
                &saved.fonts[FontRole::Code as usize],
                &saved.fonts[FontRole::Interface as usize],
                preferences.typography.size(FontRole::Code),
            ));
            let prepared = PreparedPreview {
                input: input.clone(),
                saved: saved.clone(),
                palette,
                config,
            };
            cx.global_mut::<ThemeState>().prepared_preview = Some(prepared.clone());
            Some(prepared)
        });
        if let Some(prepared) = prepared {
            let i = usize::from(input.dark);
            palettes[i] = prepared.palette;
            configs[i] = prepared.config;
        }
    }
    let revision = cx.try_global::<ActivePalette>().map_or(0, |p| p.1.wrapping_add(1));
    cx.set_global(ActivePalette(palettes[usize::from(dark)], revision));
    let theme = Theme::global_mut(cx);
    [theme.light_theme, theme.dark_theme] = configs;
    theme.font_size = px(preferences.interface_font_size);
    theme.font_family = saved.fonts[FontRole::Interface as usize].clone();
    theme.mono_font_family = saved.fonts[FontRole::Code as usize].clone();
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

fn config(name: &str, p: &Palette, dark: bool, font_size: f32, mono: &str, sans: &str, code_size: f32) -> ThemeConfig {
    let colors: serde_json::Map<String, serde_json::Value> = [
        ("background", hex(p.background)),
        ("foreground", hex(p.foreground)),
        ("border", hex(p.border)),
        ("input.border", hex(p.input)),
        ("ring", hex(p.ring)),
        ("caret", hex(p.foreground)),
        ("selection.background", hex(p.search_match_background)),
        ("link", hex(p.info_foreground)),
        ("link.hover", hex(p.info_foreground)),
        ("link.active", hex(p.info_foreground)),
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
        ("scrollbar.background", "#00000000".to_owned()),
        ("scrollbar.thumb.background", hex(p.scrollbar_thumb)),
        ("scrollbar.thumb.hover.background", hex(p.scrollbar_thumb_hover)),
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
    let input = colors.map(|(colors, dark)| themes::editor::Preview::new(dark, colors));
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
    let roles = themes::half_theme(theme, &prefs.halves, dark, &prefs.custom, environment_themes(cx)).and_then(|t| t.half(dark));
    if let Some(roles) = roles {
        return themes::wireframe(roles);
    }
    let p = if dark { tokens::DARK } else { tokens::LIGHT };
    [
        p.background,
        p.sidebar,
        p.card,
        p.accent,
        p.message_surface,
        p.message_action,
        p.foreground,
    ]
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
    let config = config("test", &palette, dark, 16., "Menlo", "System", 13.);
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
