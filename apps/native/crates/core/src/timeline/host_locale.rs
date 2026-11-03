//! `toSystemLocaleTag` and host time preferences (`apps/desktop/src/electron/ElectronApp.ts`).

use icu_locale_core::{Locale, extensions::unicode::{Key, Value}};

pub fn to_system_locale_tag(identifier: &str, hour_cycle: Option<&str>) -> String {
    let mut parts = identifier.split('@');
    let language_tag = parts.next().unwrap_or_default().replace('_', "-");
    let keywords = parts.next().unwrap_or_default();
    let Ok(mut locale) = language_tag.parse::<Locale>() else { return language_tag };
    let region = keywords.split(';').find_map(|entry| {
        let (key, value) = entry.split_once('=')?;
        if !key.eq_ignore_ascii_case("rg") { return None }
        let split = value.len().checked_sub(4)?;
        let (region, suffix) = (value.get(..split)?, value.get(split..)?);
        let valid = (region.len() == 2 && region.bytes().all(|b| b.is_ascii_alphabetic())) || (region.len() == 3 && region.bytes().all(|b| b.is_ascii_digit()));
        (valid && suffix.eq_ignore_ascii_case("zzzz")).then(|| region.to_ascii_uppercase().parse().ok()).flatten()
    });
    if let Some(region) = region { locale.id.region = Some(region); }
    if let Some(cycle) = hour_cycle.and_then(|value| value.parse::<Value>().ok()) {
        locale.extensions.unicode.keywords.set("hc".parse::<Key>().expect("hc key"), cycle);
    }
    locale.to_string()
}

pub fn mac_time_locale(identifier: &str, force_24: bool, force_12: bool) -> String {
    to_system_locale_tag(identifier, if force_24 { Some("h23") } else if force_12 { Some("h12") } else { None })
}

pub fn linux_time_locale(identifier: &str, lc_all: Option<&str>, lc_time: Option<&str>, lang: Option<&str>) -> String {
    let selected = [lc_all, lc_time, lang].into_iter().flatten().find(|value| !value.is_empty()).and_then(|value| value.split('.').next()).filter(|value| !matches!(*value, "C" | "POSIX"));
    to_system_locale_tag(selected.unwrap_or(identifier), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_locale_tag_keeps_region_and_hour_preferences() {
        for (identifier, cycle, expected) in [
            ("en_GB", None, "en-GB"),
            ("en-US@rg=nlzzzz", Some("h12"), "en-NL-u-hc-h12"),
            ("en_US@rg=nlzzzz", None, "en-NL"),
            ("en-GB", Some("h23"), "en-GB-u-hc-h23"),
            ("de_DE@calendar=buddhist", None, "de-DE"),
            ("en_US@calendar=gregorian;RG=419ZZZZ", None, "en-419"),
            ("en-US-u-hc-h12", Some("h23"), "en-US-u-hc-h23"),
            ("not_a_locale_!@rg=nlzzzz", Some("h23"), "not-a-locale-!"),
        ] {
            assert_eq!(to_system_locale_tag(identifier, cycle), expected);
        }
        assert_eq!(mac_time_locale("en_US@rg=nlzzzz", true, false), "en-NL-u-hc-h23");
        assert_eq!(mac_time_locale("en_US@rg=nlzzzz", false, true), "en-NL-u-hc-h12");
        assert_eq!(mac_time_locale("en_US@rg=nlzzzz", true, true), "en-NL-u-hc-h23");
        assert_eq!(mac_time_locale("en_US@rg=nlzzzz", false, false), "en-NL");
    }

    #[test]
    fn linux_clock_locale_uses_web_environment_precedence() {
        for (all, time, lang, expected) in [
            (None, Some("de_DE.UTF-8"), Some("en_US.UTF-8"), "de-DE"),
            (Some("fr_CA.UTF-8"), Some("de_DE.UTF-8"), Some("en_US.UTF-8"), "fr-CA"),
            (Some(""), Some("de_DE.UTF-8"), Some("en_US.UTF-8"), "de-DE"),
            (None, None, Some("en_GB.UTF-8"), "en-GB"),
            (Some("C.UTF-8"), Some("de_DE.UTF-8"), None, "fr-FR"),
            (None, Some("POSIX"), None, "fr-FR"),
            (None, None, None, "fr-FR"),
        ] {
            assert_eq!(linux_time_locale("fr-FR", all, time, lang), expected);
        }
    }
}
