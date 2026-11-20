//! Reactive UI localization runtime (`apps/web/src/i18n/runtime.ts`): resolves the saved
//! `language` client setting against OS UI preferred languages (macOS `NSLocale.preferredLanguages`
//! / POSIX `LANGUAGE`/`LC_ALL`/`LC_MESSAGES`/`LANG` fallback — never the regional timestamp
//! locale) and looks up localized strings from `t3_core::i18n`.

use std::sync::OnceLock;

use gpui::App;
use t3_core::i18n::{
    DEFAULT_LOCALE, IcuArg, LocaleSelection, SUPPORTED_LOCALES, format_integer as core_format_integer,
    parse_env_locales, resolve_locale_selection, translate, translate_fmt,
};

fn env_preferred_languages() -> Vec<String> {
    let language_env = std::env::var("LANGUAGE").ok();
    let lc_all = std::env::var("LC_ALL").ok();
    let lc_messages = std::env::var("LC_MESSAGES").ok();
    let lang = std::env::var("LANG").ok();
    let mut locales = parse_env_locales(
        language_env.as_deref(),
        lc_all.as_deref(),
        lc_messages.as_deref(),
        lang.as_deref(),
    );
    if locales.is_empty()
        && let Some(sys) = sys_locale::get_locale()
        && !sys.trim().is_empty()
    {
        locales.push(sys);
    }
    locales
}

fn system_preferred_languages() -> &'static [String] {
    static PREFERRED: OnceLock<Vec<String>> = OnceLock::new();
    PREFERRED.get_or_init(|| {
        #[cfg(target_os = "macos")]
        {
            let langs: Vec<String> = objc2_foundation::NSLocale::preferredLanguages()
                .iter()
                .map(|lang| lang.to_string())
                .collect();
            if !langs.is_empty() {
                return langs;
            }
        }
        env_preferred_languages()
    })
}

/// Active resolved catalog locale + regional formatting locale (`resolveLocaleSelection`),
/// reactively derived from the saved `language` client setting.
pub fn active_locale_selection(cx: &App) -> LocaleSelection<'static> {
    let saved = crate::settings::store::try_client(cx)
        .map(|doc| doc.language())
        .unwrap_or(DEFAULT_LOCALE);
    resolve_locale_selection(saved, system_preferred_languages(), SUPPORTED_LOCALES, DEFAULT_LOCALE)
}

/// Active resolved UI locale (`"en"`, `"de"`, `"es"`, `"fr"`, `"ja"`, `"ko"`, `"pt-BR"`, `"ru"`,
/// `"sv"`, or `"zh"`), reactively derived from the saved `language` client setting.
pub fn active_locale(cx: &App) -> &'static str {
    active_locale_selection(cx).catalog_locale
}

/// Active regional formatting locale derived from the app language setting (preserving e.g.
/// `de-CH` or `sv-FI` when following system languages). Separate from OS regional timestamp
/// formatting in `crate::timestamp`.
pub fn active_formatting_locale(cx: &App) -> String {
    active_locale_selection(cx).formatting_locale
}

/// Formats an integer with grouping separators matching `active_formatting_locale(cx)`.
pub fn format_integer(cx: &App, value: i64) -> String {
    core_format_integer(&active_formatting_locale(cx), value)
}

/// Translates an English source string using the active UI locale.
pub fn t<'a>(cx: &App, english: &'a str) -> &'a str {
    translate(active_locale(cx), english)
}

/// Translates and formats an ICU MessageFormat string (`{var}`, `{count, plural, ...}`,
/// `{kind, select, ...}`) using the active UI locale.
pub fn t_fmt(cx: &App, english: &str, args: &[(&str, IcuArg<'_>)]) -> String {
    translate_fmt(active_locale(cx), english, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use t3_core::i18n::resolve_locale;

    #[test]
    fn system_preferred_languages_reads_os_ui_languages() {
        let langs = system_preferred_languages();
        // Should return a valid list without panicking; resolved locale must always be supported.
        let resolved = resolve_locale(
            t3_core::i18n::SYSTEM_LANGUAGE,
            langs,
            SUPPORTED_LOCALES,
            DEFAULT_LOCALE,
        );
        assert!(SUPPORTED_LOCALES.contains(&resolved));
    }
}
