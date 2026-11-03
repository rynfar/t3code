//! One theme source. T3's palette (generated into `tokens.rs` from the web theme) is written into
//! gpui-component's `Theme` as light and dark configs, so primitives and app views read the same
//! values. Tokens gpui-component has no slot for (pills, message surface, sidebar rows…) are read
//! through `palette(cx)`. The theme, appearance mode, interface size and chat width follow the
//! client settings (`web/src/hooks/useTheme.ts`, `appearanceFonts.ts`), and switch atomically.

mod tokens;

use std::rc::Rc;
use std::sync::LazyLock;

use gpui::{App, Global, Hsla, Rems, Window, WindowAppearance, px, rems};
use gpui_component::{Theme, ThemeConfig, ThemeMode};
use serde_json::json;
use t3_core::client_settings::{AppearanceMode, ChatWidth, DiffColorScheme, ThemePreference};

pub use tokens::{BUILT_IN_THEMES, Palette, metrics};

/// The web's `text-white` on solid destructive buttons (design-system §6.1); not a theme role.
pub const WHITE: u32 = 0xffff_ffff;

struct ActivePalette(&'static Palette);

impl Global for ActivePalette {}

/// What the theme follows: the stored preferences plus the OS appearance the window reports.
#[derive(Debug, Clone, PartialEq)]
pub struct Preferences {
    pub theme: ThemePreference,
    pub mode: AppearanceMode,
    pub interface_font_size: f32,
    pub chat_width: ChatWidth,
    pub diff_color_scheme: DiffColorScheme,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            theme: ThemePreference::Default(AppearanceMode::System),
            mode: AppearanceMode::System,
            interface_font_size: metrics::ROOT_FONT_SIZE,
            chat_width: ChatWidth::Comfortable,
            diff_color_scheme: DiffColorScheme::default(),
        }
    }
}

struct ThemeState {
    preferences: Preferences,
    system_dark: bool,
}

impl Global for ThemeState {}

pub fn palette(cx: &App) -> &'static Palette {
    cx.global::<ActivePalette>().0
}

pub fn hsla(color: u32) -> Hsla {
    gpui::rgba(color).into()
}

pub fn init(cx: &mut App) {
    cx.set_global(ThemeState { preferences: Preferences::default(), system_dark: false });
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
    state.preferences = preferences;
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
    match state.preferences.mode {
        AppearanceMode::System => state.system_dark,
        AppearanceMode::Light => false,
        AppearanceMode::Dark => true,
    }
}

/// The light and dark halves of the chosen theme: a built-in named theme, else T3 Code.
fn halves(theme: &ThemePreference) -> (&'static Palette, &'static Palette) {
    let named = theme.named_id().and_then(|id| BUILT_IN_THEMES.iter().find(|candidate| candidate.id == id));
    let light = named.and_then(|named| named.light.as_ref()).unwrap_or(&tokens::LIGHT);
    let dark = named.and_then(|named| named.dark.as_ref()).unwrap_or(&tokens::DARK);
    (light, dark)
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

fn diff_halves(preferences: &Preferences) -> (&'static Palette, &'static Palette) {
    if preferences.diff_color_scheme == DiffColorScheme::RedGreen {
        return halves(&preferences.theme);
    }
    static DEFAULT: LazyLock<(Palette, Palette)> =
        LazyLock::new(|| (blue_orange(tokens::LIGHT), blue_orange(tokens::DARK)));
    static NAMED: LazyLock<Vec<(Palette, Palette)>> = LazyLock::new(|| {
        BUILT_IN_THEMES
            .iter()
            .map(|theme| {
                (
                    blue_orange(theme.light.unwrap_or(tokens::LIGHT)),
                    blue_orange(theme.dark.unwrap_or(tokens::DARK)),
                )
            })
            .collect()
    });
    let pair = preferences
        .theme
        .named_id()
        .and_then(|id| BUILT_IN_THEMES.iter().position(|theme| theme.id == id))
        .map(|index| &NAMED[index])
        .unwrap_or(&DEFAULT);
    (&pair.0, &pair.1)
}

/// The first installed family of the web's `--font-mono` stack (`index.css`): SF Mono is not an
/// installed family on every Mac, and an unknown family falls back to the proportional UI font.
const MONO_STACK: &[&str] = &["SF Mono", "SFMono-Regular", "Menlo", "Consolas", "Liberation Mono"];

fn mono_family(cx: &App) -> &'static str {
    let installed = cx.text_system().all_font_names();
    MONO_STACK.iter().copied().find(|family| installed.iter().any(|name| name == family)).unwrap_or(MONO_STACK[0])
}

fn install(cx: &mut App) {
    let dark = is_dark(cx);
    let mono = mono_family(cx);
    let preferences = preferences(cx).clone();
    let (light_palette, dark_palette) = diff_halves(&preferences);
    cx.set_global(ActivePalette(if dark { dark_palette } else { light_palette }));
    let size = preferences.interface_font_size;
    let theme = Theme::global_mut(cx);
    theme.light_theme = Rc::new(config("T3 Code Light", light_palette, false, size, mono));
    theme.dark_theme = Rc::new(config("T3 Code Dark", dark_palette, true, size, mono));
    theme.font_size = px(size);
}

/// A theme's swatch colors for the theme cards: (background, card, primary, foreground).
pub fn swatch(theme: &ThemePreference, dark: bool) -> [u32; 4] {
    let (light, dark_palette) = halves(theme);
    let p = if dark { dark_palette } else { light };
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

fn config(name: &str, p: &Palette, dark: bool, font_size: f32, mono: &str) -> ThemeConfig {
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
        "font.family": ".SystemUIFont",
        "font.size": font_size,
        "mono_font.family": mono,
        "mono_font.size": metrics::DEFAULT_CODE_FONT_SIZE,
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
