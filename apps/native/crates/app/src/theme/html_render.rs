//! The active reader theme for `useHtmlRenderTheme.ts`, before contrast/glass transforms.
use gpui::App;
use gpui_component::ActiveTheme;
use t3_core::{client_settings::typography::FontRole, html_render::host, themes};

pub fn context(cx: &App) -> serde_json::Value {
    let state = cx.global::<super::ThemeState>();
    let prefs = &state.preferences;
    let dark = cx.theme().is_dark();
    let standard;
    let colors = if let Some(preview) = &state.preview {
        &preview.colors
    } else if let Some(colors) = themes::half_theme(&prefs.theme, &prefs.halves, dark, &prefs.custom, &state.environment).and_then(|t| t.half(dark)) {
        colors
    } else {
        standard = themes::standard(dark);
        &standard
    };
    host::theme(colors, dark, prefs.typography.family(FontRole::Interface), prefs.typography.family(FontRole::Code))
}
