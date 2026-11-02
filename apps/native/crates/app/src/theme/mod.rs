//! One theme source. T3's palette (generated into `tokens.rs` from the web theme) is written into
//! gpui-component's `Theme` as light and dark configs, so primitives and app views read the same
//! values. Tokens gpui-component has no slot for (pills, message surface, sidebar rows…) are read
//! through `palette(cx)`.

mod tokens;

use std::rc::Rc;

use gpui::{App, Global, Hsla, Window, WindowAppearance};
use gpui_component::{Theme, ThemeConfig, ThemeMode};
use serde_json::json;

pub use tokens::{Palette, metrics};

struct ActivePalette(&'static Palette);

impl Global for ActivePalette {}

pub fn palette(cx: &App) -> &'static Palette {
    cx.global::<ActivePalette>().0
}

pub fn hsla(color: u32) -> Hsla {
    gpui::rgba(color).into()
}

pub fn init(cx: &mut App) {
    install(false, cx);
}

pub fn follow_system_appearance(window: &mut Window, cx: &mut App) {
    apply(window.appearance(), window, cx);
    window
        .observe_window_appearance(|window, cx| apply(window.appearance(), window, cx))
        .detach();
}

fn apply(appearance: WindowAppearance, window: &mut Window, cx: &mut App) {
    let dark = matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark);
    install(dark, cx);
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, Some(window), cx);
}

fn install(dark: bool, cx: &mut App) {
    cx.set_global(ActivePalette(if dark { &tokens::DARK } else { &tokens::LIGHT }));
    let theme = Theme::global_mut(cx);
    theme.light_theme = Rc::new(config("T3 Code Light", &tokens::LIGHT, false));
    theme.dark_theme = Rc::new(config("T3 Code Dark", &tokens::DARK, true));
}

fn hex(color: u32) -> String {
    format!("#{color:08x}")
}

fn config(name: &str, p: &Palette, dark: bool) -> ThemeConfig {
    let colors: serde_json::Map<String, serde_json::Value> = [
        ("background", hex(p.background)),
        ("foreground", hex(p.foreground)),
        ("border", hex(p.border)),
        ("input.border", hex(p.input)),
        ("ring", hex(p.ring)),
        ("caret", hex(p.foreground)),
        ("selection.background", hex(p.search_match_background)),
        ("link", hex(p.foreground)),
        ("link.hover", hex(p.foreground)),
        ("link.active", hex(p.foreground)),
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
        "font.size": metrics::ROOT_FONT_SIZE,
        "mono_font.family": "SF Mono",
        "mono_font.size": metrics::DEFAULT_CODE_FONT_SIZE,
        "radius": metrics::CONTROL_RADIUS as usize,
        "radius.lg": metrics::RADIUS_XL as usize,
        "colors": colors,
    });
    serde_json::from_value(value).unwrap_or_else(|error| {
        tracing::warn!(%error, "invalid theme config");
        ThemeConfig::default()
    })
}
