//! Multi-language UI localization: ports `packages/client-runtime/src/locales.ts`,
//! `apps/web/src/i18n/locales.ts`, `apps/web/src/i18n/runtime.ts`, and embeds the 10
//! translation catalogs directly from `apps/web/src/i18n/translations/*.json` and
//! `apps/desktop/src/i18n/translations/*.json`.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::Deserialize;

pub const SYSTEM_LANGUAGE: &str = "system";
pub const DEFAULT_LOCALE: &str = "en";
pub const SUPPORTED_LOCALES: &[&str] = &["en", "de", "es", "fr", "ja", "ko", "pt-BR", "ru", "sv", "zh"];

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
    ("sv", "Svenska"),
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

/// Result of `resolve_locale_selection`: the supported catalog locale plus the regional tag used
/// for date/number formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleSelection<'a> {
    pub catalog_locale: &'a str,
    pub formatting_locale: String,
}

/// Keep the matching system tag for regional formats while loading a supported catalog
/// (`resolveLocaleSelection` in `packages/client-runtime/src/locales.ts`).
pub fn resolve_locale_selection<'a, S: AsRef<str>>(
    language: &str,
    system_locales: &[S],
    supported: &[&'a str],
    fallback: &'a str,
) -> LocaleSelection<'a> {
    if let Some(&exact) = supported.iter().find(|&&locale| locale == language) {
        return LocaleSelection {
            catalog_locale: exact,
            formatting_locale: exact.to_owned(),
        };
    }
    for tag in system_locales {
        if let Some(matched) = match_supported_locale(tag.as_ref(), supported) {
            return LocaleSelection {
                catalog_locale: matched,
                formatting_locale: tag.as_ref().trim().replace('_', "-"),
            };
        }
    }
    LocaleSelection {
        catalog_locale: fallback,
        formatting_locale: fallback.to_owned(),
    }
}

/// Saved setting to active locale; `"system"` and unknown values follow the host's languages
/// (`resolveLocale` in `packages/client-runtime/src/locales.ts`).
pub fn resolve_locale<'a, S: AsRef<str>>(
    language: &str,
    system_locales: &[S],
    supported: &[&'a str],
    fallback: &'a str,
) -> &'a str {
    resolve_locale_selection(language, system_locales, supported, fallback).catalog_locale
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

/// GT-React / GT-i18n translation catalogs store plain strings alongside rich AST array entries.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CatalogEntry {
    Text(String),
    Rich(serde_json::Value),
}

/// Flattens a GT rich AST entry into plain text with `{key}` variable placeholders so native
/// surfaces can look up and interpolate rich catalog entries as well.
fn flatten_rich_ast(node: &serde_json::Value, out: &mut String) {
    match node {
        serde_json::Value::String(text) => out.push_str(text),
        serde_json::Value::Array(items) => {
            for item in items {
                flatten_rich_ast(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(key) = map.get("k").and_then(serde_json::Value::as_str) {
                out.push('{');
                out.push_str(key);
                out.push('}');
            } else if let Some(child) = map.get("c") {
                flatten_rich_ast(child, out);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

const RAW_EN_BUNDLE: &str = include_str!("../../../../../web/src/i18n/translations/en.json");

const NON_DEFAULT_BUNDLES: [(&str, &str); 9] = [
    ("de", include_str!("../../../../../web/src/i18n/translations/de.json")),
    ("es", include_str!("../../../../../web/src/i18n/translations/es.json")),
    ("fr", include_str!("../../../../../web/src/i18n/translations/fr.json")),
    ("ja", include_str!("../../../../../web/src/i18n/translations/ja.json")),
    ("ko", include_str!("../../../../../web/src/i18n/translations/ko.json")),
    ("pt-BR", include_str!("../../../../../web/src/i18n/translations/pt-BR.json")),
    ("ru", include_str!("../../../../../web/src/i18n/translations/ru.json")),
    ("sv", include_str!("../../../../../web/src/i18n/translations/sv.json")),
    ("zh", include_str!("../../../../../web/src/i18n/translations/zh.json")),
];

const RAW_DESKTOP_EN_BUNDLE: &str = include_str!("../../../../../desktop/src/i18n/translations/en.json");

const NON_DEFAULT_DESKTOP_BUNDLES: [(&str, &str); 9] = [
    ("de", include_str!("../../../../../desktop/src/i18n/translations/de.json")),
    ("es", include_str!("../../../../../desktop/src/i18n/translations/es.json")),
    ("fr", include_str!("../../../../../desktop/src/i18n/translations/fr.json")),
    ("ja", include_str!("../../../../../desktop/src/i18n/translations/ja.json")),
    ("ko", include_str!("../../../../../desktop/src/i18n/translations/ko.json")),
    ("pt-BR", include_str!("../../../../../desktop/src/i18n/translations/pt-BR.json")),
    ("ru", include_str!("../../../../../desktop/src/i18n/translations/ru.json")),
    ("sv", include_str!("../../../../../desktop/src/i18n/translations/sv.json")),
    ("zh", include_str!("../../../../../desktop/src/i18n/translations/zh.json")),
];

struct ParsedEnglishCatalog {
    plain: BTreeMap<String, String>,
    rich: BTreeMap<String, String>,
}

fn parse_english_bundle(raw: &str) -> ParsedEnglishCatalog {
    let parsed: BTreeMap<String, CatalogEntry> =
        serde_json::from_str(raw).expect("valid embedded en.json catalog");
    let mut plain = BTreeMap::new();
    let mut rich = BTreeMap::new();
    for (hash, entry) in parsed {
        match entry {
            CatalogEntry::Text(text) => {
                plain.insert(hash, text);
            }
            CatalogEntry::Rich(value) => {
                let mut flattened = String::new();
                flatten_rich_ast(&value, &mut flattened);
                if !flattened.is_empty() {
                    rich.insert(hash, flattened);
                }
            }
        }
    }
    ParsedEnglishCatalog { plain, rich }
}

fn build_locale_map(en: &ParsedEnglishCatalog, raw_json: &str) -> HashMap<String, String> {
    let localized: HashMap<String, CatalogEntry> =
        serde_json::from_str(raw_json).expect("valid embedded locale catalog");
    let mut map = HashMap::with_capacity(en.plain.len() + en.rich.len());
    // Plain string entries take precedence over flattened rich AST entries.
    for (hash, english) in &en.plain {
        if let Some(CatalogEntry::Text(translated)) = localized.get(hash) {
            map.entry(english.clone()).or_insert_with(|| translated.clone());
        }
    }
    for (hash, english) in &en.rich {
        if let Some(entry) = localized.get(hash) {
            let translated = match entry {
                CatalogEntry::Text(text) => text.clone(),
                CatalogEntry::Rich(value) => {
                    let mut out = String::new();
                    flatten_rich_ast(value, &mut out);
                    out
                }
            };
            if !translated.is_empty() {
                map.entry(english.clone()).or_insert(translated);
            }
        }
    }
    map
}

fn english_catalog() -> &'static ParsedEnglishCatalog {
    static ENGLISH: OnceLock<ParsedEnglishCatalog> = OnceLock::new();
    ENGLISH.get_or_init(|| parse_english_bundle(RAW_EN_BUNDLE))
}

fn desktop_english_catalog() -> &'static ParsedEnglishCatalog {
    static DESKTOP_ENGLISH: OnceLock<ParsedEnglishCatalog> = OnceLock::new();
    DESKTOP_ENGLISH.get_or_init(|| parse_english_bundle(RAW_DESKTOP_EN_BUNDLE))
}

/// Lazily parses and caches one locale's web `english_source -> translated_text` map on first lookup.
fn web_locale_catalog(locale: &str) -> Option<&'static HashMap<String, String>> {
    static CACHES: [OnceLock<HashMap<String, String>>; 9] = [const { OnceLock::new() }; 9];
    let index = NON_DEFAULT_BUNDLES.iter().position(|&(code, _)| code == locale)?;
    Some(CACHES[index].get_or_init(|| build_locale_map(english_catalog(), NON_DEFAULT_BUNDLES[index].1)))
}

/// Lazily parses and caches one locale's desktop `english_source -> translated_text` map on first lookup.
fn desktop_locale_catalog(locale: &str) -> Option<&'static HashMap<String, String>> {
    static CACHES: [OnceLock<HashMap<String, String>>; 9] = [const { OnceLock::new() }; 9];
    let index = NON_DEFAULT_DESKTOP_BUNDLES.iter().position(|&(code, _)| code == locale)?;
    Some(CACHES[index].get_or_init(|| {
        build_locale_map(desktop_english_catalog(), NON_DEFAULT_DESKTOP_BUNDLES[index].1)
    }))
}

fn lookup_web_first(locale: &str, english: &str) -> Option<&'static str> {
    if locale == DEFAULT_LOCALE {
        return None;
    }
    web_locale_catalog(locale)
        .and_then(|map| map.get(english))
        .or_else(|| desktop_locale_catalog(locale).and_then(|map| map.get(english)))
        .map(String::as_str)
}

fn lookup_desktop_first(locale: &str, english: &str) -> Option<&'static str> {
    if locale == DEFAULT_LOCALE {
        return None;
    }
    desktop_locale_catalog(locale)
        .and_then(|map| map.get(english))
        .or_else(|| web_locale_catalog(locale).and_then(|map| map.get(english)))
        .map(String::as_str)
}

/// Translates an English source string into `locale` (web catalog first, desktop catalog fallback).
/// Returns `english` unchanged for `"en"` or when no translation exists for the string.
pub fn translate<'a>(locale: &str, english: &'a str) -> &'a str {
    lookup_web_first(locale, english).unwrap_or(english)
}

/// Translates an English menu / desktop shell string into `locale` (desktop catalog first so
/// menu-bar items like `Edit`, `View`, `Window`, `Zoom`, `Undo`, `Paste` get desktop menu phrasing,
/// falling back to the web catalog).
pub fn translate_menu<'a>(locale: &str, english: &'a str) -> &'a str {
    lookup_desktop_first(locale, english).unwrap_or(english)
}

/// Argument value for ICU MessageFormat interpolation (`{var}`, `{count, plural, ...}`, `{kind, select, ...}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IcuArg<'a> {
    Text(Cow<'a, str>),
    Int(i64),
}

impl<'a> IcuArg<'a> {
    fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            Self::Text(s) => s.trim().parse::<i64>().ok(),
        }
    }

    fn write_to(&self, out: &mut String) {
        match self {
            Self::Text(s) => out.push_str(s),
            Self::Int(n) => out.push_str(&n.to_string()),
        }
    }

    fn as_str(&self) -> Cow<'_, str> {
        match self {
            Self::Text(s) => Cow::Borrowed(s.as_ref()),
            Self::Int(n) => Cow::Owned(n.to_string()),
        }
    }
}

impl<'a> From<&'a str> for IcuArg<'a> {
    fn from(value: &'a str) -> Self {
        Self::Text(Cow::Borrowed(value))
    }
}

impl<'a> From<&'a String> for IcuArg<'a> {
    fn from(value: &'a String) -> Self {
        Self::Text(Cow::Borrowed(value.as_str()))
    }
}

impl From<String> for IcuArg<'_> {
    fn from(value: String) -> Self {
        Self::Text(Cow::Owned(value))
    }
}

impl<'a> From<Cow<'a, str>> for IcuArg<'a> {
    fn from(value: Cow<'a, str>) -> Self {
        Self::Text(value)
    }
}

impl From<i64> for IcuArg<'_> {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<i32> for IcuArg<'_> {
    fn from(value: i32) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<u64> for IcuArg<'_> {
    fn from(value: u64) -> Self {
        Self::Int(i64::try_from(value).unwrap_or(i64::MAX))
    }
}

impl From<u32> for IcuArg<'_> {
    fn from(value: u32) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<usize> for IcuArg<'_> {
    fn from(value: usize) -> Self {
        Self::Int(i64::try_from(value).unwrap_or(i64::MAX))
    }
}

impl From<isize> for IcuArg<'_> {
    fn from(value: isize) -> Self {
        Self::Int(value as i64)
    }
}

/// CLDR cardinal plural category for integers across all 10 supported locales
/// (`en`, `de`, `es`, `fr`, `ja`, `ko`, `pt-BR`, `ru`, `sv`, `zh`).
pub fn plural_category(locale: &str, count: i64) -> &'static str {
    let normalized = locale.trim().to_ascii_lowercase();
    let base = normalized.split(['-', '_']).next().unwrap_or("en");
    let n = count.unsigned_abs();
    match base {
        // No grammatical cardinal plural distinction in CLDR.
        "ja" | "ko" | "zh" => "other",
        // French and Brazilian Portuguese use `one` for 0 and 1.
        "fr" | "pt" => {
            if n == 0 || n == 1 {
                "one"
            } else {
                "other"
            }
        }
        // Russian Slavic cardinal rules.
        "ru" => {
            let m10 = n % 10;
            let m100 = n % 100;
            if m10 == 1 && m100 != 11 {
                "one"
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                "few"
            } else if m10 == 0 || (5..=9).contains(&m10) || (11..=14).contains(&m100) {
                "many"
            } else {
                "other"
            }
        }
        // English, German, Spanish, Swedish, and default fallback.
        _ => {
            if n == 1 {
                "one"
            } else {
                "other"
            }
        }
    }
}

/// Finds the byte index of the `}` matching the `{` at `open_brace`, respecting ICU single-quote
/// escapes so quoted braces do not prematurely close an expression.
fn find_matching_brace(input: &str, open_brace: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut i = open_brace + 1;
    let mut depth = 1usize;
    let mut in_quote = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quote {
            if b == b'\'' {
                if bytes.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                in_quote = false;
            }
            i += 1;
            continue;
        }
        if b == b'\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            if matches!(bytes.get(i + 1), Some(b'{' | b'}' | b'#')) {
                in_quote = true;
                i += 1;
                continue;
            }
            i += 1;
            continue;
        }
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parses ICU `plural` / `select` branches (`selector {body} ...`), returning `(offset, branches)`.
fn parse_icu_branches(spec: &str, allow_offset: bool) -> (i64, Vec<(&str, &str)>) {
    let mut rest = spec.trim_start();
    let mut offset = 0i64;
    if allow_offset && let Some(after) = rest.strip_prefix("offset:") {
        let digits_len = after
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'-')
            .count();
        if digits_len > 0
            && let Ok(parsed) = after[..digits_len].parse::<i64>()
        {
            offset = parsed;
            rest = after[digits_len..].trim_start();
        }
    }

    let mut branches = Vec::new();
    while !rest.is_empty() {
        let Some(brace_pos) = rest.find('{') else {
            break;
        };
        let selector = rest[..brace_pos].trim();
        let Some(close_pos) = find_matching_brace(rest, brace_pos) else {
            break;
        };
        let body = &rest[brace_pos + 1..close_pos];
        if !selector.is_empty() {
            branches.push((selector, body));
        }
        rest = rest[close_pos + 1..].trim_start();
    }
    (offset, branches)
}

fn render_icu_segment(
    locale: &str,
    template: &str,
    args: &[(&str, IcuArg<'_>)],
    plural_count: Option<i64>,
    out: &mut String,
) {
    let bytes = template.as_bytes();
    let mut i = 0usize;
    let mut in_quote = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quote {
            if b == b'\'' {
                if bytes.get(i + 1) == Some(&b'\'') {
                    out.push('\'');
                    i += 2;
                    continue;
                }
                in_quote = false;
                i += 1;
                continue;
            }
            let ch = template[i..].chars().next().expect("valid UTF-8 char");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }

        if b == b'\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                out.push('\'');
                i += 2;
                continue;
            }
            let starts_quote = matches!(bytes.get(i + 1), Some(b'{' | b'}'))
                || (plural_count.is_some() && bytes.get(i + 1) == Some(&b'#'));
            if starts_quote {
                in_quote = true;
                i += 1;
                continue;
            }
            out.push('\'');
            i += 1;
            continue;
        }

        if b == b'#' && let Some(count) = plural_count {
            out.push_str(&count.to_string());
            i += 1;
            continue;
        }

        if b == b'{'
            && let Some(close) = find_matching_brace(template, i)
        {
            let inner = &template[i + 1..close];
            render_icu_expr(locale, inner, args, plural_count, out);
            i = close + 1;
            continue;
        }

        let ch = template[i..].chars().next().expect("valid UTF-8 char");
        out.push(ch);
        i += ch.len_utf8();
    }
}

fn render_icu_expr(
    locale: &str,
    inner: &str,
    args: &[(&str, IcuArg<'_>)],
    plural_count: Option<i64>,
    out: &mut String,
) {
    let Some(first_comma) = inner.find(',') else {
        let name = inner.trim();
        if let Some((_, val)) = args.iter().find(|(k, _)| *k == name) {
            val.write_to(out);
        } else {
            out.push('{');
            out.push_str(inner);
            out.push('}');
        }
        return;
    };

    let name = inner[..first_comma].trim();
    let after_name = &inner[first_comma + 1..];
    let Some(second_comma) = after_name.find(',') else {
        // Simple formatted placeholder like `{n, number}` — render argument text if present.
        if let Some((_, val)) = args.iter().find(|(k, _)| *k == name) {
            val.write_to(out);
        } else {
            out.push('{');
            out.push_str(inner);
            out.push('}');
        }
        return;
    };

    let kind = after_name[..second_comma].trim();
    let spec = &after_name[second_comma + 1..];

    match kind {
        "plural" => {
            let Some(raw_count) = args.iter().find(|(k, _)| *k == name).and_then(|(_, v)| v.as_int()) else {
                out.push('{');
                out.push_str(inner);
                out.push('}');
                return;
            };
            let (offset, branches) = parse_icu_branches(spec, true);
            let exact_key = format!("={raw_count}");
            let adjusted = raw_count.saturating_sub(offset);
            let category = plural_category(locale, adjusted);
            let chosen = branches
                .iter()
                .find(|(sel, _)| *sel == exact_key)
                .or_else(|| branches.iter().find(|(sel, _)| *sel == category))
                .or_else(|| branches.iter().find(|(sel, _)| *sel == "other"))
                .map(|(_, body)| *body);
            if let Some(body) = chosen {
                render_icu_segment(locale, body, args, Some(adjusted), out);
            } else {
                out.push('{');
                out.push_str(inner);
                out.push('}');
            }
        }
        "select" => {
            let Some(value) = args.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str()) else {
                out.push('{');
                out.push_str(inner);
                out.push('}');
                return;
            };
            let (_, branches) = parse_icu_branches(spec, false);
            let chosen = branches
                .iter()
                .find(|(sel, _)| *sel == value.as_ref())
                .or_else(|| branches.iter().find(|(sel, _)| *sel == "other"))
                .map(|(_, body)| *body);
            if let Some(body) = chosen {
                render_icu_segment(locale, body, args, plural_count, out);
            } else {
                out.push('{');
                out.push_str(inner);
                out.push('}');
            }
        }
        _ => {
            out.push('{');
            out.push_str(inner);
            out.push('}');
        }
    }
}

/// Formats an ICU MessageFormat string for `locale` with `{var}`, `{count, plural, ...}` (with
/// CLDR plural rules and `#` replacement), and `{key, select, ...}`.
pub fn format_icu(locale: &str, template: &str, args: &[(&str, IcuArg<'_>)]) -> String {
    let mut out = String::with_capacity(template.len());
    render_icu_segment(locale, template, args, None, &mut out);
    out
}

/// Translates `english` into `locale` and formats ICU placeholders/plurals.
/// Untranslated strings fall back to English and apply English plural rules (matching web's
/// `apps/web/src/i18n/messages.ts`).
pub fn translate_fmt(locale: &str, english: &str, args: &[(&str, IcuArg<'_>)]) -> String {
    match lookup_web_first(locale, english) {
        Some(translated) => format_icu(locale, translated, args),
        None => format_icu(DEFAULT_LOCALE, english, args),
    }
}

/// Translates a desktop menu / shell string into `locale` (desktop catalog first) and formats ICU
/// placeholders/plurals.
pub fn translate_menu_fmt(locale: &str, english: &str, args: &[(&str, IcuArg<'_>)]) -> String {
    match lookup_desktop_first(locale, english) {
        Some(translated) => format_icu(locale, translated, args),
        None => format_icu(DEFAULT_LOCALE, english, args),
    }
}

/// Formats an integer with grouping separators derived from the active regional `formatting_locale`
/// (e.g., `1,920` for `en`, `1.920` for `de`, `1’920` for `de-CH`, `1\u{00a0}920` for `sv`/`ru`,
/// `1\u{202f}920` for `fr`).
pub fn format_integer(formatting_locale: &str, value: i64) -> String {
    let normalized = formatting_locale.trim().to_ascii_lowercase().replace('_', "-");
    let mut parts = normalized.split('-');
    let lang = parts.next().unwrap_or("en");
    let region = parts.find(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic()));
    let sep = match (lang, region) {
        (_, Some("ch")) => "\u{2019}",
        ("fr", _) => "\u{202f}",
        ("ru" | "sv", _) => "\u{00a0}",
        ("de" | "es" | "pt", _) => ".",
        _ => ",",
    };
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let len = digits.len();
    if len <= 3 {
        return if negative { format!("-{digits}") } else { digits };
    }
    let mut out = String::with_capacity(len + len / 3 * sep.len() + usize::from(negative));
    if negative {
        out.push('-');
    }
    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0 && (len - idx).is_multiple_of(3) {
            out.push_str(sep);
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    #[test]
    fn embedded_translation_catalogs_match_web_json_exactly() {
        let web_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../web/src/i18n/translations");
        assert!(web_dir.is_dir(), "expected web translations dir at {}", web_dir.display());
        let en_web_raw = std::fs::read_to_string(web_dir.join("en.json")).expect("read web en.json");
        assert_eq!(RAW_EN_BUNDLE, en_web_raw);
        let en_web: BTreeMap<String, CatalogEntry> =
            serde_json::from_str(&en_web_raw).expect("parse web en.json");
        assert!(!en_web.is_empty(), "expected non-empty web en.json");

        // All 10 supported locales exist on disk and are covered by embedded bundles.
        assert_eq!(SUPPORTED_LOCALES.len(), 1 + NON_DEFAULT_BUNDLES.len());
        assert_eq!(NATIVE_LANGUAGE_NAMES.len(), SUPPORTED_LOCALES.len());

        for &(locale, embedded_raw) in &NON_DEFAULT_BUNDLES {
            let web_path = web_dir.join(format!("{locale}.json"));
            let web_raw = std::fs::read_to_string(&web_path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", web_path.display()));
            assert_eq!(
                embedded_raw, web_raw,
                "embedded {locale}.json drifted from apps/web/src/i18n/translations/{locale}.json"
            );
            let web_map: BTreeMap<String, CatalogEntry> =
                serde_json::from_str(&web_raw).unwrap_or_else(|e| panic!("parse web {locale}: {e}"));

            // Build the expected set of valid string translations per English source (multiple
            // GT hashes can share one English string under distinct `$context` values).
            let mut valid_by_english: HashMap<&str, HashSet<&str>> = HashMap::new();
            for (hash, en_entry) in &en_web {
                if let CatalogEntry::Text(english) = en_entry
                    && let Some(CatalogEntry::Text(translated)) = web_map.get(hash)
                {
                    valid_by_english.entry(english.as_str()).or_default().insert(translated.as_str());
                }
            }
            assert!(!valid_by_english.is_empty(), "{locale}.json has no plain string entries");
            for (english, valid) in &valid_by_english {
                let actual = translate(locale, english);
                assert!(
                    valid.contains(actual),
                    "translate({locale:?}, {english:?}) returned {actual:?}, expected one of {valid:?}"
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
        assert_eq!(native_language_name("sv"), "Svenska");
        assert_eq!(native_language_name("zh"), "中文（简体）");
        assert_eq!(native_language_name("xx"), "xx");

        assert_eq!(supported_language_value("en"), "en");
        assert_eq!(supported_language_value("sv"), "sv");
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
        assert_eq!(match_supported_locale("sv-SE", SUPPORTED_LOCALES), Some("sv"));
    }

    #[test]
    fn resolve_locale_honors_explicit_setting_and_system_fallback() {
        let sys = vec!["zh-TW".to_owned(), "de_CH".to_owned(), "en-US".to_owned()];
        assert_eq!(resolve_locale("fr", &sys, SUPPORTED_LOCALES, DEFAULT_LOCALE), "fr");
        assert_eq!(resolve_locale(SYSTEM_LANGUAGE, &sys, SUPPORTED_LOCALES, DEFAULT_LOCALE), "de");
        assert_eq!(resolve_locale("unknown-tag", &sys, SUPPORTED_LOCALES, DEFAULT_LOCALE), "de");
        assert_eq!(resolve_locale(SYSTEM_LANGUAGE, &["zh-Hant-TW", "it-IT"], SUPPORTED_LOCALES, DEFAULT_LOCALE), "en");
        assert_eq!(resolve_locale(SYSTEM_LANGUAGE, &[] as &[&str], SUPPORTED_LOCALES, DEFAULT_LOCALE), "en");

        assert_eq!(
            resolve_locale_selection("fr", &sys, SUPPORTED_LOCALES, DEFAULT_LOCALE),
            LocaleSelection { catalog_locale: "fr", formatting_locale: "fr".to_owned() }
        );
        assert_eq!(
            resolve_locale_selection(SYSTEM_LANGUAGE, &sys, SUPPORTED_LOCALES, DEFAULT_LOCALE),
            LocaleSelection { catalog_locale: "de", formatting_locale: "de-CH".to_owned() }
        );
        assert_eq!(
            resolve_locale_selection(SYSTEM_LANGUAGE, &["sv_FI"], SUPPORTED_LOCALES, DEFAULT_LOCALE),
            LocaleSelection { catalog_locale: "sv", formatting_locale: "sv-FI".to_owned() }
        );
        assert_eq!(
            resolve_locale_selection(SYSTEM_LANGUAGE, &["it-IT"], SUPPORTED_LOCALES, DEFAULT_LOCALE),
            LocaleSelection { catalog_locale: "en", formatting_locale: "en".to_owned() }
        );
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

    #[test]
    fn embedded_desktop_catalogs_match_desktop_json_exactly() {
        let desktop_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../desktop/src/i18n/translations");
        assert!(desktop_dir.is_dir(), "expected desktop translations dir at {}", desktop_dir.display());
        let en_raw = std::fs::read_to_string(desktop_dir.join("en.json")).expect("read desktop en.json");
        assert_eq!(RAW_DESKTOP_EN_BUNDLE, en_raw);
        let en_desktop: BTreeMap<String, CatalogEntry> =
            serde_json::from_str(&en_raw).expect("parse desktop en.json");

        for &(locale, embedded_raw) in &NON_DEFAULT_DESKTOP_BUNDLES {
            let desktop_path = desktop_dir.join(format!("{locale}.json"));
            let disk_raw = std::fs::read_to_string(&desktop_path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", desktop_path.display()));
            assert_eq!(embedded_raw, disk_raw, "embedded desktop {locale}.json drifted");

            let disk_map: BTreeMap<String, CatalogEntry> =
                serde_json::from_str(&disk_raw).unwrap_or_else(|e| panic!("parse desktop {locale}: {e}"));
            let mut valid_by_english: HashMap<&str, HashSet<&str>> = HashMap::new();
            for (hash, en_entry) in &en_desktop {
                if let CatalogEntry::Text(english) = en_entry
                    && let Some(CatalogEntry::Text(translated)) = disk_map.get(hash)
                {
                    valid_by_english.entry(english.as_str()).or_default().insert(translated.as_str());
                }
            }
            for (english, valid) in &valid_by_english {
                let actual = translate_menu(locale, english);
                assert!(
                    valid.contains(actual),
                    "translate_menu({locale:?}, {english:?}) returned {actual:?}, expected one of {valid:?}"
                );
            }
        }
    }

    #[test]
    fn translate_fmt_supports_interpolation_plurals_hash_select_and_quoting() {
        // Simple variable substitution + localized catalog lookup.
        assert_eq!(
            translate_fmt("en", "Go to quoted response: {label}", &[("label", "Turn 2".into())]),
            "Go to quoted response: Turn 2"
        );
        assert_ne!(
            translate_fmt("de", "Go to quoted response: {label}", &[("label", "Turn 2".into())]),
            "Go to quoted response: Turn 2"
        );
        assert!(
            translate_fmt("de", "Go to quoted response: {label}", &[("label", "Turn 2".into())]).contains("Turn 2")
        );

        // English, Swedish, Japanese, and Russian cardinal pluralization on a real catalog key.
        let files_key = "{count, plural, one {{count} file} other {{count} files}}";
        assert_eq!(translate_fmt("en", files_key, &[("count", 1.into())]), "1 file");
        assert_eq!(translate_fmt("en", files_key, &[("count", 3.into())]), "3 files");
        assert_eq!(translate_fmt("sv", files_key, &[("count", 1.into())]), "1 fil");
        assert_eq!(translate_fmt("sv", files_key, &[("count", 3.into())]), "3 filer");
        assert_eq!(translate_fmt("ja", files_key, &[("count", 1.into())]), "1 個のファイル");
        assert_eq!(translate_fmt("ja", files_key, &[("count", 3.into())]), "3 個のファイル");
        // Russian Slavic cardinal rules: 1 -> one, 2 -> few, 5/11 -> many, 21 -> one, 22 -> few.
        assert_eq!(translate_fmt("ru", files_key, &[("count", 1.into())]), "1 файл");
        assert_eq!(translate_fmt("ru", files_key, &[("count", 2.into())]), "2 файла");
        assert_eq!(translate_fmt("ru", files_key, &[("count", 5.into())]), "5 файлов");
        assert_eq!(translate_fmt("ru", files_key, &[("count", 11.into())]), "11 файлов");
        assert_eq!(translate_fmt("ru", files_key, &[("count", 21.into())]), "21 файл");
        assert_eq!(translate_fmt("ru", files_key, &[("count", 22.into())]), "22 файла");

        // French and pt-BR treat 0 and 1 as `one`.
        assert_eq!(plural_category("fr", 0), "one");
        assert_eq!(plural_category("fr", 1), "one");
        assert_eq!(plural_category("fr", 2), "other");
        assert_eq!(plural_category("pt-BR", 0), "one");
        assert_eq!(plural_category("pt-BR", 2), "other");

        // Exact =0 / =1 selectors and `#` replacement inside plural branches (plus offset).
        let hash_tpl = "{count, plural, =0 {No items} one {# item} other {# items}}";
        assert_eq!(format_icu("en", hash_tpl, &[("count", 0.into())]), "No items");
        assert_eq!(format_icu("en", hash_tpl, &[("count", 1.into())]), "1 item");
        assert_eq!(format_icu("en", hash_tpl, &[("count", 4.into())]), "4 items");
        let offset_tpl = "{count, plural, offset:1 =0 {Nobody} =1 {Only you} one {You and # other} other {You and # others}}";
        assert_eq!(format_icu("en", offset_tpl, &[("count", 1.into())]), "Only you");
        assert_eq!(format_icu("en", offset_tpl, &[("count", 2.into())]), "You and 1 other");
        assert_eq!(format_icu("en", offset_tpl, &[("count", 5.into())]), "You and 4 others");

        // Select directive and ICU single-quote escaping without mangling English apostrophes.
        let select_tpl = "{kind, select, pr {Open PR} mr {Open MR} other {Open {kind}}}";
        assert_eq!(format_icu("en", select_tpl, &[("kind", "pr".into())]), "Open PR");
        assert_eq!(format_icu("en", select_tpl, &[("kind", "other_kind".into())]), "Open other_kind");
        assert_eq!(
            format_icu("en", "Don't escape '{braces}' or ''quotes'' for {name}", &[("name", "T3".into())]),
            "Don't escape {braces} or 'quotes' for T3"
        );

        // Rich AST entries flatten cleanly and translate with `{_gt_value_N}` placeholders.
        assert_eq!(translate("de", "Copy PR number"), "PR-Nummer kopieren");
        assert_eq!(
            translate_fmt("de", "Turn {_gt_value_1}", &[("_gt_value_1", 3.into())]),
            "Runde 3"
        );

        // Desktop menu formatting uses desktop catalog first.
        assert_eq!(
            translate_menu_fmt("de", "Hide {appName}", &[("appName", "T3 Code".into())]),
            "T3 Code ausblenden"
        );
        assert_eq!(
            translate_menu_fmt("ja", "Quit {appName}", &[("appName", "T3 Code".into())]),
            "T3 Code を終了"
        );

        // Regional formattingLocale integer grouping.
        assert_eq!(format_integer("en-US", 1920), "1,920");
        assert_eq!(format_integer("de", 1920), "1.920");
        assert_eq!(format_integer("de-CH", 1920), "1\u{2019}920");
        assert_eq!(format_integer("sv-FI", 1920), "1\u{00a0}920");
        assert_eq!(format_integer("fr-CA", 1920), "1\u{202f}920");
    }

    #[test]
    fn core_ui_keys_translate_across_all_non_english_locales() {
        // Catalog-coverage regression test (`pingdotgg/t3code#37954`, `#38649`, `#39224`, `#39225`).
        let web_keys = [
            // Sidebar & `#38649` scratch folder
            "No project",
            "Pinned",
            "Active",
            "Working",
            "Snoozed",
            "New thread",
            // Composer & chat
            "Send",
            "Steer",
            "Queue",
            "Copy",
            "Cite",
            "Edit",
            "Fork this thread",
            "Fork from this response",
            "Settled",
            "Cancel",
            "Close",
            "Save",
            // Panels & diffs
            "Terminal",
            "Files",
            "Pull requests",
            "Issues",
            // Quote & port messages (`#39225`)
            "Could not reach this port on the environment",
            "Go to quoted response: {label}",
            "The quoted text has changed",
            "Showing the source response. The saved quote is unchanged.",
            "Load earlier turns, then click the citation to try again. Your saved quote is unchanged.",
            "Unavailable",
            // Settings
            "Settings",
            "Language",
            "Appearance",
            "Keybindings",
            "About",
        ];
        // Desktop menu keys (`#39224`)
        let menu_keys = [
            "File",
            "Close Window",
            "Edit",
            "Undo",
            "Redo",
            "Cut",
            "Copy",
            "Paste",
            "Paste as Text",
            "Delete",
            "Select All",
            "View",
            "Toggle Full Screen",
            "Window",
            "Minimize",
            "Bring All to Front",
            "Help",
            "Hide {appName}",
            "Quit {appName}",
            "Settings...",
        ];

        for &locale in SUPPORTED_LOCALES.iter().filter(|&&l| l != DEFAULT_LOCALE) {
            for key in web_keys {
                let translated = translate(locale, key);
                assert!(
                    lookup_web_first(locale, key).is_some(),
                    "missing web translation in {locale} for {key:?}"
                );
                // Some technical terms (e.g. "Terminal", "Issues", "Copy") may intentionally stay
                // identical in a few Latin-script locales, so verify the non-technical core keys
                // strictly differ from English in every locale.
                if matches!(
                    key,
                    "No project"
                        | "New thread"
                        | "Cancel"
                        | "Close"
                        | "Save"
                        | "Settings"
                        | "Language"
                        | "Could not reach this port on the environment"
                        | "Go to quoted response: {label}"
                        | "The quoted text has changed"
                        | "Showing the source response. The saved quote is unchanged."
                        | "Load earlier turns, then click the citation to try again. Your saved quote is unchanged."
                        | "Unavailable"
                ) {
                    assert_ne!(
                        translated, key,
                        "expected non-English translation in {locale} for {key:?}"
                    );
                }
            }
            for key in menu_keys {
                let translated = translate_menu(locale, key);
                assert!(
                    lookup_desktop_first(locale, key).is_some(),
                    "missing desktop menu translation in {locale} for {key:?}"
                );
                if matches!(
                    key,
                    "Close Window"
                        | "Paste as Text"
                        | "Select All"
                        | "Toggle Full Screen"
                        | "Bring All to Front"
                        | "Hide {appName}"
                        | "Quit {appName}"
                ) {
                    assert_ne!(
                        translated, key,
                        "expected non-English menu translation in {locale} for {key:?}"
                    );
                }
            }
        }
    }
}
