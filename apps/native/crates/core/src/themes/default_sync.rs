//! Server-driven default theme & appearance adoption and boot-cached environment themes from
//! `apps/web/src/hooks/useDefaultTheme.ts` and `apps/web/src/themePalette.ts`
//! (`pingdotgg/t3code#35247`, `pingdotgg/t3code#36042`).

use serde_json::{Map, Value};

use super::{ThemeDefinition, decode, library, resolve};
use crate::client_settings::{AppearanceMode, ClientSettingChange, ClientSettingsDoc, ThemePreference};

pub const APPLIED_DEFAULT_THEME_STORAGE_PREFIX: &str = "t3code:default-theme-applied:v2:";
pub const APPLIED_DEFAULT_APPEARANCE_STORAGE_PREFIX: &str = "t3code:default-appearance-applied:v1:";
pub const ENVIRONMENT_THEMES_STORAGE_KEY: &str = "t3code:environment-themes:v1";

/// Include set time so reasserting the same theme still applies (`defaultThemeGeneration`).
pub fn default_theme_generation(theme: &str, set_at: &str) -> String {
    if !set_at.is_empty() {
        format!("{theme}@{set_at}")
    } else {
        theme.to_owned()
    }
}

/// Returns the generation string to apply when a server default should take effect, or `None`.
pub fn default_theme_to_apply(
    environment_id: Option<&str>,
    default_theme: &str,
    default_theme_set_at: &str,
    applied_generation: Option<&str>,
    resolves: bool,
) -> Option<String> {
    if environment_id.is_none_or(str::is_empty) || default_theme.is_empty() {
        return None;
    }
    let generation = default_theme_generation(default_theme, default_theme_set_at);
    if applied_generation == Some(generation.as_str()) {
        return None;
    }
    // The setting and the palette it names arrive independently, so an id that
    // does not resolve yet is not a failure -- adoption re-checks when the
    // published set changes.
    if !resolves {
        return None;
    }
    Some(generation)
}

/// A single-appearance theme sets its own mode unless a mode was set with it or after it
/// (`themeSetsOwnMode` in `useDefaultTheme.ts`).
pub fn theme_sets_own_mode(
    default_theme_set_at: &str,
    default_appearance: &str,
    default_appearance_set_at: &str,
) -> bool {
    default_appearance.is_empty() || default_appearance_set_at < default_theme_set_at
}

/// Returns the sole appearance mode if `theme` only has light or only has dark (`singleAppearanceOf`).
pub fn single_appearance_of(theme: &ThemeDefinition) -> Option<AppearanceMode> {
    match (theme.half(false).is_some(), theme.half(true).is_some()) {
        (true, false) => Some(AppearanceMode::Light),
        (false, true) => Some(AppearanceMode::Dark),
        _ => None,
    }
}

/// Server-published theme/appearance default settings for one environment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DefaultThemeSettings<'a> {
    pub environment_id: &'a str,
    pub default_theme: &'a str,
    pub default_theme_set_at: &'a str,
    pub default_appearance: &'a str,
    pub default_appearance_set_at: &'a str,
}

/// Computes the atomic `ClientSettingChange::ThemePatch` adopting `defaultTheme` and/or
/// `defaultAppearance` once per generation for `settings.environment_id`.
///
/// Mirrors `adoptDefaults()` (`adoptDefaultTheme()` then `adoptDefaultAppearance()`) from
/// `apps/web/src/hooks/useDefaultTheme.ts`.
pub fn adopt_defaults_patch(
    doc: &ClientSettingsDoc,
    settings: &DefaultThemeSettings<'_>,
    environment: &[ThemeDefinition],
) -> Map<String, Value> {
    if settings.environment_id.is_empty() {
        return Map::new();
    }

    let mut working = doc.clone();
    let mut patch = Map::new();
    let custom = library(&working);

    // 1. adoptDefaultTheme()
    let theme_key = format!("{APPLIED_DEFAULT_THEME_STORAGE_PREFIX}{}", settings.environment_id);
    let applied_theme = working.entries().get(&theme_key).and_then(Value::as_str);
    let definition = resolve(settings.default_theme, &custom, environment).cloned();
    if let Some(generation) = default_theme_to_apply(
        Some(settings.environment_id),
        settings.default_theme,
        settings.default_theme_set_at,
        applied_theme,
        definition.is_some(),
    ) && let Some(def) = definition
    {
        let canonical = super::canonical_theme_preference(settings.default_theme);
        patch.extend(working.apply_change(&ClientSettingChange::Theme(ThemePreference::Named(canonical))));
        if let Some(half) = single_appearance_of(&def)
            && theme_sets_own_mode(
                settings.default_theme_set_at,
                settings.default_appearance,
                settings.default_appearance_set_at,
            )
        {
            patch.extend(working.apply_change(&ClientSettingChange::AppearanceMode(half)));
        }
        let gen_patch = Map::from_iter([(theme_key, Value::String(generation))]);
        patch.extend(working.apply_change(&ClientSettingChange::ThemePatch(gen_patch)));
    }

    // 2. adoptDefaultAppearance()
    let appearance_key = format!(
        "{APPLIED_DEFAULT_APPEARANCE_STORAGE_PREFIX}{}",
        settings.environment_id
    );
    let applied_appearance = working.entries().get(&appearance_key).and_then(Value::as_str);
    if let Some(mode) = AppearanceMode::parse(settings.default_appearance)
        && let Some(generation) = default_theme_to_apply(
            Some(settings.environment_id),
            settings.default_appearance,
            settings.default_appearance_set_at,
            applied_appearance,
            true,
        )
    {
        patch.extend(working.apply_change(&ClientSettingChange::AppearanceMode(mode)));
        let gen_patch = Map::from_iter([(appearance_key, Value::String(generation))]);
        patch.extend(working.apply_change(&ClientSettingChange::ThemePatch(gen_patch)));
    }

    patch
}

/// Reads the last server-published environment theme definitions persisted for first-frame paint
/// (`readStoredEnvironmentThemes` in `apps/web/src/themePalette.ts`).
pub fn stored_environment_themes(doc: &ClientSettingsDoc) -> Vec<ThemeDefinition> {
    let mut out = Vec::new();
    if let Some(themes) = doc
        .entries()
        .get(ENVIRONMENT_THEMES_STORAGE_KEY)
        .and_then(Value::as_array)
    {
        for value in themes {
            if let Ok(theme) = decode(value, true)
                && !out.iter().any(|existing: &ThemeDefinition| existing.id == theme.id)
            {
                out.push(theme);
            }
        }
    }
    out
}

/// Builds a `ThemePatch` persisting the latest live server-published environment themes for
/// first-frame paint on the next launch (`storePublishedEnvironmentThemes`).
pub fn environment_themes_patch(
    doc: &ClientSettingsDoc,
    themes: &[ThemeDefinition],
) -> Map<String, Value> {
    let next = serde_json::to_value(themes).unwrap_or_else(|_| Value::Array(Vec::new()));
    if doc.entries().get(ENVIRONMENT_THEMES_STORAGE_KEY) == Some(&next) {
        return Map::new();
    }
    Map::from_iter([(ENVIRONMENT_THEMES_STORAGE_KEY.to_owned(), next)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::themes;
    use serde_json::json;

    fn published_nightfall() -> ThemeDefinition {
        themes::environment(&[json!({
            "id": "nightfall",
            "name": "Nightfall",
            "appearance": "dark",
            "canvas": "#1a1b26",
            "accent": "#7aa2f7"
        })])
        .into_iter()
        .next()
        .unwrap()
    }

    #[test]
    fn generation_tracking_matches_web_default_theme_tests() {
        let gen1 = default_theme_generation("nightfall", "2026-08-28T00:00:00.000Z");
        assert_eq!(gen1, "nightfall@2026-08-28T00:00:00.000Z");
        assert_eq!(
            default_theme_to_apply(
                Some("env-1"),
                "nightfall",
                "2026-08-28T00:00:00.000Z",
                Some(&gen1),
                true,
            ),
            None
        );
        let gen2 = default_theme_to_apply(
            Some("env-1"),
            "nightfall",
            "2026-08-29T00:00:00.000Z",
            Some(&gen1),
            true,
        );
        assert!(gen2.is_some());
        assert_ne!(gen2.as_deref(), Some(gen1.as_str()));

        let theme_ts = "2026-11-13T10:00:00.000Z";
        assert!(theme_sets_own_mode(theme_ts, "", ""));
        assert!(theme_sets_own_mode(theme_ts, "dark", "2026-11-13T09:00:00.000Z"));
        assert!(!theme_sets_own_mode(theme_ts, "dark", theme_ts));
        assert!(!theme_sets_own_mode(theme_ts, "light", "2026-11-13T11:00:00.000Z"));
    }

    #[test]
    fn adopts_late_resolving_environment_theme_once_per_generation_and_respects_local_overrides() {
        let mut doc = ClientSettingsDoc::default();
        let settings = DefaultThemeSettings {
            environment_id: "env-1",
            default_theme: "nightfall",
            default_theme_set_at: "2026-11-13T10:00:00.000Z",
            default_appearance: "",
            default_appearance_set_at: "",
        };

        // Before the published palette arrives, nothing is applied or marked applied.
        assert!(adopt_defaults_patch(&doc, &settings, &[]).is_empty());

        // Once the published single-appearance dark theme arrives, both theme and dark mode apply.
        let env_themes = vec![published_nightfall()];
        let patch = adopt_defaults_patch(&doc, &settings, &env_themes);
        assert!(!patch.is_empty());
        doc.apply_change(&ClientSettingChange::ThemePatch(patch));
        assert_eq!(doc.theme(), ThemePreference::Named("nightfall".into()));
        assert_eq!(doc.appearance_mode(), AppearanceMode::Dark);

        // User switches locally to Grove + Light; the same server generation must NOT overwrite it.
        doc.apply_change(&ClientSettingChange::Theme(ThemePreference::Named("grove".into())));
        doc.apply_change(&ClientSettingChange::AppearanceMode(AppearanceMode::Light));
        assert!(adopt_defaults_patch(&doc, &settings, &env_themes).is_empty());

        // Re-asserting the same theme with a newer setAt timestamp DOES apply again.
        let newer = DefaultThemeSettings {
            default_theme_set_at: "2026-11-14T10:00:00.000Z",
            ..settings
        };
        let patch = adopt_defaults_patch(&doc, &newer, &env_themes);
        doc.apply_change(&ClientSettingChange::ThemePatch(patch));
        assert_eq!(doc.theme(), ThemePreference::Named("nightfall".into()));
        assert_eq!(doc.appearance_mode(), AppearanceMode::Dark);
    }

    #[test]
    fn explicit_default_appearance_overrides_single_appearance_theme_mode_when_newer_or_equal() {
        let mut doc = ClientSettingsDoc::default();
        let env_themes = vec![published_nightfall()];
        let settings = DefaultThemeSettings {
            environment_id: "env-1",
            default_theme: "nightfall",
            default_theme_set_at: "2026-11-13T10:00:00.000Z",
            default_appearance: "light",
            default_appearance_set_at: "2026-11-13T10:00:00.000Z",
        };
        let patch = adopt_defaults_patch(&doc, &settings, &env_themes);
        doc.apply_change(&ClientSettingChange::ThemePatch(patch));
        assert_eq!(doc.theme(), ThemePreference::Named("nightfall".into()));
        assert_eq!(doc.appearance_mode(), AppearanceMode::Light);
    }

    #[test]
    fn stored_environment_themes_round_trip_including_css() {
        let mut nightfall = published_nightfall();
        nightfall.css = Some(":root { --accent: #ff00aa; }".into());
        let mut doc = ClientSettingsDoc::default();
        let patch = environment_themes_patch(&doc, &[nightfall.clone()]);
        assert!(!patch.is_empty());
        doc.apply_change(&ClientSettingChange::ThemePatch(patch));
        assert!(environment_themes_patch(&doc, &[nightfall.clone()]).is_empty());
        let loaded = stored_environment_themes(&doc);
        assert_eq!(loaded, vec![nightfall]);
    }
}
