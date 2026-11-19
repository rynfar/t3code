//! Multi-language UI localization: ports `packages/client-runtime/src/locales.ts`,
//! `apps/web/src/i18n/locales.ts`, `apps/web/src/i18n/runtime.ts`, and embeds the 9
//! translation catalogs from `apps/web/src/i18n/translations/*.json`.

use std::collections::HashMap;
use std::sync::OnceLock;

pub const SYSTEM_LANGUAGE: &str = "system";
pub const DEFAULT_LOCALE: &str = "en";
pub const SUPPORTED_LOCALES: &[&str] = &["en", "de", "es", "fr", "ja", "ko", "pt-BR", "ru", "zh"];

/// Shown in their own language so a user can find theirs from any UI language
/// (`NATIVE_LANGUAGE_NAMES` in `packages/client-runtime/src/locales.ts`).
pub const NATIVE_LANGUAGE_NAMES: &[(&str, &str)] = &[
    ("en", "English"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("fr", "Français"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("pt-BR", "Português (Brasil)"),
    ("ru", "Русский"),
    ("zh", "中文（简体）"),
];

pub fn native_language_name(locale: &str) -> &str {
    NATIVE_LANGUAGE_NAMES
        .iter()
        .find(|(code, _)| *code == locale)
        .map_or(locale, |(_, name)| *name)
}

/// Returns `saved` when it is one of `SUPPORTED_LOCALES`, else `SYSTEM_LANGUAGE`
/// (`SettingsPanels.tsx` language select value).
pub fn supported_language_value(saved: &str) -> &str {
    if SUPPORTED_LOCALES.contains(&saved) { saved } else { SYSTEM_LANGUAGE }
}

/// Our `zh` is Simplified, so Traditional tags only match an exact locale:
/// `/^zh[-_](hant|tw|hk|mo)\b/`.
fn is_traditional_chinese(normalized: &str) -> bool {
    let Some(rest) = normalized.strip_prefix("zh") else {
        return false;
    };
    let Some(after_sep) = rest.strip_prefix('-').or_else(|| rest.strip_prefix('_')) else {
        return false;
    };
    for prefix in ["hant", "tw", "hk", "mo"] {
        if let Some(tail) = after_sep.strip_prefix(prefix)
            && tail.chars().next().is_none_or(|ch| !ch.is_ascii_alphanumeric())
        {
            return true;
        }
    }
    false
}

/// `matchSupportedLocale` (`packages/client-runtime/src/locales.ts`).
pub fn match_supported_locale<'a>(tag: &str, supported: &[&'a str]) -> Option<&'a str> {
    let normalized = tag.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    let language = normalized.split(['-', '_']).next().unwrap_or("");
    let exact = supported.iter().copied().find(|locale| locale.eq_ignore_ascii_case(&normalized));
    if exact.is_some() || is_traditional_chinese(&normalized) {
        return exact;
    }
    supported.iter().copied().find(|locale| locale.eq_ignore_ascii_case(language))
}

/// Saved setting to active locale; `"system"` and unknown values follow the host's languages
/// (`resolveLocale` in `packages/client-runtime/src/locales.ts`).
pub fn resolve_locale<'a, S: AsRef<str>>(
    language: &str,
    system_locales: &[S],
    supported: &[&'a str],
    fallback: &'a str,
) -> &'a str {
    if let Some(&exact) = supported.iter().find(|&&locale| locale == language) {
        return exact;
    }
    for tag in system_locales {
        if let Some(matched) = match_supported_locale(tag.as_ref(), supported) {
            return matched;
        }
    }
    fallback
}

/// Parses POSIX UI-language environment variables (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`)
/// into ordered BCP-47-like tags for non-macOS / fallback OS preferred-language resolution.
pub fn parse_env_locales(
    language_env: Option<&str>,
    lc_all: Option<&str>,
    lc_messages: Option<&str>,
    lang: Option<&str>,
) -> Vec<String> {
    let clean = |raw: &str| -> Option<String> {
        let trimmed = raw.trim();
        let base = trimmed.split(['.', '@']).next().unwrap_or("").trim();
        if base.is_empty() || base.eq_ignore_ascii_case("C") || base.eq_ignore_ascii_case("POSIX") {
            return None;
        }
        Some(base.replace('_', "-"))
    };
    let mut out = Vec::new();
    if let Some(list) = language_env {
        for part in list.split(':') {
            if let Some(tag) = clean(part) {
                out.push(tag);
            }
        }
    }
    for candidate in [lc_all, lc_messages, lang].into_iter().flatten() {
        if let Some(tag) = clean(candidate)
            && !out.contains(&tag)
        {
            out.push(tag);
        }
    }
    out
}

const RAW_BUNDLES: &[(&str, &str)] = &[
    ("en", include_str!("translations/en.json")),
    ("de", include_str!("translations/de.json")),
    ("es", include_str!("translations/es.json")),
    ("fr", include_str!("translations/fr.json")),
    ("ja", include_str!("translations/ja.json")),
    ("ko", include_str!("translations/ko.json")),
    ("pt-BR", include_str!("translations/pt-BR.json")),
    ("ru", include_str!("translations/ru.json")),
    ("zh", include_str!("translations/zh.json")),
];

struct Catalogs {
    /// `locale -> (english_source -> translated_text)`.
    by_locale: HashMap<&'static str, HashMap<String, String>>,
}

fn catalogs() -> &'static Catalogs {
    static CATALOGS: OnceLock<Catalogs> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        let en_raw: HashMap<String, String> =
            serde_json::from_str(RAW_BUNDLES[0].1).expect("valid embedded en.json catalog");
        let mut by_locale = HashMap::with_capacity(RAW_BUNDLES.len());
        for &(locale, raw_json) in &RAW_BUNDLES[1..] {
            let localized: HashMap<String, String> =
                serde_json::from_str(raw_json).expect("valid embedded locale catalog");
            let mut map = HashMap::with_capacity(en_raw.len());
            for (hash, english) in &en_raw {
                if let Some(translated) = localized.get(hash) {
                    map.insert(english.clone(), translated.clone());
                }
            }
            by_locale.insert(locale, map);
        }
        Catalogs { by_locale }
    })
}

/// Translates an English source string into `locale`. Returns `english` unchanged for `"en"` or
/// when no translation exists for the string.
pub fn translate<'a>(locale: &str, english: &'a str) -> &'a str {
    if locale == DEFAULT_LOCALE {
        return english;
    }
    catalogs()
        .by_locale
        .get(locale)
        .and_then(|map| map.get(english))
        .map(String::as_str)
        .unwrap_or(english)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    #[test]
    fn embedded_translation_catalogs_match_web_json_exactly() {
        let web_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../web/src/i18n/translations");
        assert!(web_dir.is_dir(), "expected web translations dir at {}", web_dir.display());
        let en_web: BTreeMap<String, String> = serde_json::from_str(
            &std::fs::read_to_string(web_dir.join("en.json")).expect("read web en.json"),
        )
        .expect("parse web en.json");
        assert_eq!(en_web.len(), 72, "expected 72 translation keys in en.json");

        for &(locale, embedded_raw) in RAW_BUNDLES {
            let web_path = web_dir.join(format!("{locale}.json"));
            let web_raw = std::fs::read_to_string(&web_path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", web_path.display()));
            let web_map: BTreeMap<String, String> =
                serde_json::from_str(&web_raw).unwrap_or_else(|e| panic!("parse web {locale}: {e}"));
            let embedded_map: BTreeMap<String, String> = serde_json::from_str(embedded_raw)
                .unwrap_or_else(|e| panic!("parse embedded {locale}: {e}"));
            assert_eq!(
                embedded_map, web_map,
                "embedded {locale}.json drifted from apps/web/src/i18n/translations/{locale}.json"
            );
            assert_eq!(embedded_map.len(), en_web.len(), "{locale}.json key count mismatch");

            // Every key in en.json translates to the exact value in this bundle.
            for (hash, english) in &en_web {
                let expected = web_map.get(hash).expect("matching hash");
                assert_eq!(
                    translate(locale, english),
                    expected.as_str(),
                    "translate({locale:?}, {english:?}) mismatch"
                );
            }
        }
    }

    #[test]
    fn native_language_names_and_supported_values_match_web() {
        assert_eq!(native_language_name("en"), "English");
        assert_eq!(native_language_name("de"), "Deutsch");
        assert_eq!(native_language_name("es"), "Español");
        assert_eq!(native_language_name("fr"), "Français");
        assert_eq!(native_language_name("ja"), "日本語");
        assert_eq!(native_language_name("ko"), "한국어");
        assert_eq!(native_language_name("pt-BR"), "Português (Brasil)");
        assert_eq!(native_language_name("ru"), "Русский");
        assert_eq!(native_language_name("zh"), "中文（简体）");
        assert_eq!(native_language_name("xx"), "xx");

        assert_eq!(supported_language_value("en"), "en");
        assert_eq!(supported_language_value("pt-BR"), "pt-BR");
        assert_eq!(supported_language_value("system"), SYSTEM_LANGUAGE);
        assert_eq!(supported_language_value("nl"), SYSTEM_LANGUAGE);
    }

    #[test]
    fn traditional_chinese_guard_and_locale_matching() {
        // Traditional Chinese tags never fall back to Simplified `zh`.
        for trad in ["zh-TW", "zh_TW", "zh-HK", "zh_HK", "zh-MO", "zh-Hant", "zh-Hant-TW", "zh_Hant_HK"] {
            assert_eq!(
                match_supported_locale(trad, SUPPORTED_LOCALES),
                None,
                "{trad} must not match Simplified zh"
            );
        }
        // Unless an exact Traditional locale is explicitly supported.
        assert_eq!(match_supported_locale("zh-TW", &["en", "zh", "zh-TW"]), Some("zh-TW"));

        // Simplified Chinese tags match `zh`.
        for simp in ["zh", "zh-CN", "zh_CN", "zh-Hans", "zh-Hans-CN", "zh-SG", "  ZH_hans  "] {
            assert_eq!(
                match_supported_locale(simp, SUPPORTED_LOCALES),
                Some("zh"),
                "{simp} should match zh"
            );
        }

        // Exact and base language matching.
        assert_eq!(match_supported_locale("pt-BR", SUPPORTED_LOCALES), Some("pt-BR"));
        assert_eq!(match_supported_locale("pt-br", SUPPORTED_LOCALES), Some("pt-BR"));
        assert_eq!(match_supported_locale("pt-PT", SUPPORTED_LOCALES), None);
        assert_eq!(match_supported_locale("de-AT", SUPPORTED_LOCALES), Some("de"));
        assert_eq!(match_supported_locale("es_MX", SUPPORTED_LOCALES), Some("es"));
        assert_eq!(match_supported_locale("fr-CA", SUPPORTED_LOCALES), Some("fr"));
        assert_eq!(match_supported_locale("ja-JP", SUPPORTED_LOCALES), Some("ja"));
        assert_eq!(match_supported_locale("ko-KR", SUPPORTED_LOCALES), Some("ko"));
        assert_eq!(match_supported_locale("ru-RU", SUPPORTED_LOCALES), Some("ru"));
    }

    #[test]
    fn resolve_locale_honors_explicit_setting_and_system_fallback() {
        let sys = vec!["zh-TW".to_owned(), "de-CH".to_owned(), "en-US".to_owned()];
        assert_eq!(resolve_locale("fr", &sys, SUPPORTED_LOCALES, DEFAULT_LOCALE), "fr");
        assert_eq!(resolve_locale(SYSTEM_LANGUAGE, &sys, SUPPORTED_LOCALES, DEFAULT_LOCALE), "de");
        assert_eq!(resolve_locale("unknown-tag", &sys, SUPPORTED_LOCALES, DEFAULT_LOCALE), "de");
        assert_eq!(resolve_locale(SYSTEM_LANGUAGE, &["zh-Hant-TW", "it-IT"], SUPPORTED_LOCALES, DEFAULT_LOCALE), "en");
        assert_eq!(resolve_locale(SYSTEM_LANGUAGE, &[] as &[&str], SUPPORTED_LOCALES, DEFAULT_LOCALE), "en");
    }

    #[test]
    fn parse_env_locales_follows_posix_ui_precedence() {
        assert_eq!(
            parse_env_locales(Some("fr_CA:de_DE.UTF-8:C"), Some("ja_JP.UTF-8"), Some("es_ES.UTF-8"), Some("en_US.UTF-8")),
            vec!["fr-CA", "de-DE", "ja-JP", "es-ES", "en-US"]
        );
        assert_eq!(
            parse_env_locales(None, Some("C.UTF-8"), Some("POSIX"), Some("pt_BR.UTF-8")),
            vec!["pt-BR"]
        );
        assert_eq!(parse_env_locales(None, None, None, Some("de_DE@euro")), vec!["de-DE"]);
    }
}
