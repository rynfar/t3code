//! Intl's numeric hour/minute options (`apps/web/src/timestampFormat.ts`).

use std::fmt;
use icu_calendar::{Date, Gregorian};
use icu_datetime::{
    DateTimeFormatter, DateTimeFormatterPreferences, NoCalendarFormatter,
    fieldsets::{E, MD, T, YMD},
    input::Time,
    options::YearStyle,
    pattern::{DateTimePattern, DayPeriodNameLength, FixedCalendarDateTimeNames},
    preferences::HourCycle,
};
use icu_decimal::{DecimalFormatter, input::Decimal};
use icu_locale_core::{Locale, extensions::unicode::Key};
use writeable::{Part, PartsWrite, TryWriteable, Writeable};

use crate::client_settings::TimestampFormat;
use crate::sidebar::time::LocalTime;

#[derive(Clone, Copy)]
enum Cycle { H11, H12, H23, H24 }

include!("timestamp_patterns.rs");

/// Maps exact, language-only, and explicit-default-script tags whose V8 `Intl.DateTimeFormat`
/// clock patterns match the generated regional supplement (Node v22.22.3 / ICU 78.2).
/// Regional tags with distinct V8 patterns (e.g. `tr-CY`, `es`, `es-ES`, `es-MX`, `es-Latn-CO`)
/// are intentionally excluded.
fn best_patterns_key(id: &icu_locale_core::LanguageIdentifier) -> Option<&'static str> {
    let lang = id.language.as_str();
    let script = id.script.as_ref().map(|s| s.as_str());
    let region = id.region.as_ref().map(|r| r.as_str());
    match (lang, script, region) {
        ("es", None, Some("CO")) => Some("es-CO"),
        ("lt", None | Some("Latn"), None | Some("LT")) => Some("lt-LT"),
        ("ug", None | Some("Arab"), None | Some("CN")) => Some("ug-CN"),
        ("mr", None | Some("Deva"), None | Some("IN")) => Some("mr-IN"),
        ("bg", None | Some("Cyrl"), None | Some("BG")) => Some("bg-BG"),
        ("tr", None | Some("Latn"), None | Some("TR")) => Some("tr-TR"),
        _ => None,
    }
}

impl Cycle {
    fn twelve(self) -> bool { matches!(self, Self::H11 | Self::H12) }
    fn value(self, hour: i64) -> i64 {
        match self {
            Self::H11 => hour % 12,
            Self::H12 => (hour + 11) % 12 + 1,
            Self::H23 => hour,
            Self::H24 => if hour == 0 { 24 } else { hour },
        }
    }
}

struct ClockFormat {
    formatter: NoCalendarFormatter<T>,
    best_pattern: Option<(FixedCalendarDateTimeNames<Gregorian, T>, DateTimePattern)>,
    cycle: Cycle,
    width: usize,
    day_periods: Option<[String; 2]>,
}

pub struct IntlTimestampClock {
    formats: [ClockFormat; 3],
    hours: [String; 25],
    md: DateTimeFormatter<MD>,
    ymd: DateTimeFormatter<YMD>,
}

fn pattern_fields(formatter: &NoCalendarFormatter<T>) -> (Option<Cycle>, Option<DayPeriodNameLength>) {
    let pattern = formatter.format(&Time::start_of_day()).pattern().to_string();
    let (mut quoted, mut cycle, mut period) = (false, None, None);
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' { quoted = !quoted; }
        if quoted { continue }
        match ch {
            'K' => cycle = Some(Cycle::H11),
            'h' => cycle = Some(Cycle::H12),
            'H' => cycle = Some(Cycle::H23),
            'b' | 'B' => {
                let mut width = 1;
                while chars.peek() == Some(&ch) { chars.next(); width += 1; }
                period = Some(match width { 5 => DayPeriodNameLength::Narrow, 4 => DayPeriodNameLength::Wide, _ => DayPeriodNameLength::Abbreviated });
            }
            _ => {}
        }
    }
    (cycle, period)
}

impl IntlTimestampClock {
    pub fn new(tag: &str) -> Option<Self> {
        let mut locale = tag.parse::<Locale>().ok()?;
        let requested = locale.extensions.unicode.keywords.remove("hc".parse::<Key>().expect("hc key")).and_then(|value| match value.to_string().as_str() {
            "h11" => Some(Cycle::H11), "h12" => Some(Cycle::H12), "h23" => Some(Cycle::H23), "h24" => Some(Cycle::H24), _ => None,
        });
        let prefs = DateTimeFormatterPreferences::from(&locale);
        let regional = NoCalendarFormatter::try_new(prefs, T::hm()).ok()?;
        let best = best_patterns_key(&locale.id).and_then(best_patterns);
        let base_cycle = best.as_ref().map(|(cycle, _)| *cycle).or_else(|| pattern_fields(&regional).0)?;
        let regional_twelve = NoCalendarFormatter::try_new({ let mut p = prefs; p.hour_cycle = Some(HourCycle::Clock12); p }, T::hm()).ok()?;
        let locale_cycle = requested.unwrap_or(base_cycle);
        let cycles = [locale_cycle, if locale_cycle.twelve() { locale_cycle } else { pattern_fields(&regional_twelve).0? }, if locale_cycle.twelve() { Cycle::H23 } else { locale_cycle }];
        let make = |cycle: Cycle| -> Option<ClockFormat> {
            let mut p = prefs;
            p.hour_cycle = Some(if cycle.twelve() { HourCycle::Clock12 } else { HourCycle::Clock24 });
            // V8 matches against the base locale before replacing the cycle; hc doesn't change that width.
            let width = best.as_ref().map(|(_, patterns)| patterns[usize::from(!cycle.twelve())].1)
                .unwrap_or(if !cycle.twelve() && base_cycle.twelve() { 2 } else { 1 });
            let best_pattern = if let Some((_, patterns)) = &best {
                let pattern = DateTimePattern::try_from_pattern_str(patterns[usize::from(!cycle.twelve())].0).ok()?;
                let mut names = FixedCalendarDateTimeNames::try_new(prefs).ok()?;
                names.include_for_pattern(&pattern).ok()?;
                Some((names, pattern))
            } else { None };
            let formatter = NoCalendarFormatter::try_new(p, T::hm()).ok()?;
            // ICU4C 78 uses AM/PM for Korean; ICU4X's names differ from the web oracle.
            let day_periods = if best_pattern.is_some() { None } else if cycle.twelve() && locale.id.language.as_str() == "ko" {
                Some(["AM".to_owned(), "PM".to_owned()])
            } else if let Some(length) = pattern_fields(&formatter).1 {
                let mut names: FixedCalendarDateTimeNames<Gregorian, T> = FixedCalendarDateTimeNames::try_new(prefs).ok()?;
                names.include_day_period_names(length).ok()?;
                let symbol = match length { DayPeriodNameLength::Narrow => "aaaaa", DayPeriodNameLength::Wide => "aaaa", _ => "a" };
                let period = DateTimePattern::try_from_pattern_str(symbol).ok()?;
                Some([0, 12].map(|hour| names.with_pattern_unchecked(&period).format(&Time::try_new(hour, 0, 0, 0).expect("valid hour")).try_write_to_string().expect("day periods are loaded").into_owned()))
            } else { None };
            Some(ClockFormat { formatter, best_pattern, cycle, width, day_periods })
        };
        let decimal = DecimalFormatter::try_new((&locale).into(), Default::default()).ok()?;
        let md = DateTimeFormatter::try_new(prefs, MD::short()).ok()?;
        let ymd = DateTimeFormatter::try_new(prefs, YMD::short().with_year_style(YearStyle::Full)).ok()?;
        Some(Self {
            formats: [make(cycles[0])?, make(cycles[1])?, make(cycles[2])?],
            hours: std::array::from_fn(|hour| decimal.format(&Decimal::from(hour as u8)).to_string()),
            md,
            ymd,
        })
    }

    pub fn numeric_date(&self, local: LocalTime, include_year: bool) -> String {
        let Ok(date) = Date::try_new_iso(local.year as i32, local.month as u8, local.day as u8) else {
            return if include_year {
                format!("{}/{}/{}", local.month, local.day, local.year)
            } else {
                format!("{}/{}", local.month, local.day)
            };
        };
        let formatted = if include_year {
            self.ymd.format(&date).to_string()
        } else {
            self.md.format(&date).to_string()
        };
        formatted.replace(['\u{202f}', '\u{a0}'], " ")
    }

    pub fn time(&self, local: LocalTime, format: TimestampFormat) -> String {
        let index = match format { TimestampFormat::Locale => 0, TimestampFormat::TwelveHour => 1, TimestampFormat::TwentyFourHour => 2 };
        let selected = &self.formats[index];
        let Ok(time) = Time::try_new(local.hour as u8, local.minute as u8, 0, 0) else { return String::new() };
        let hour = selected.cycle.value(local.hour) as usize;
        let padded = if selected.width == 2 && hour < 10 { format!("{}{}", self.hours[0], self.hours[hour]) } else { self.hours[hour].clone() };
        let mut writer = ClockParts { text: String::new(), hour: &padded, day_period: selected.day_periods.as_ref().map(|periods| periods[usize::from(local.hour >= 12)].as_str()) };
        if let Some((names, pattern)) = &selected.best_pattern {
            let _ = names.with_pattern_unchecked(pattern).format(&time).try_write_to_parts(&mut writer);
        } else {
            let _ = selected.formatter.format(&time).write_to_parts(&mut writer);
        }
        writer.text.replace(['\u{202f}', '\u{a0}'], " ")
    }
}

/// Formats a calendar date's short localized weekday (`Intl.DateTimeFormat(locale, { weekday: "short" })`,
/// `pingdotgg/t3code#39494`).
pub fn format_short_weekday(locale_tag: &str, year: i64, month: i64, day: i64) -> Option<String> {
    let locale = locale_tag.parse::<Locale>().ok()?;
    let prefs = DateTimeFormatterPreferences::from(&locale);
    let formatter = DateTimeFormatter::try_new(prefs, E::short()).ok()?;
    let date = Date::try_new_iso(year as i32, month as u8, day as u8).ok()?;
    Some(formatter.format(&date).to_string())
}

struct ClockParts<'a> {
    text: String,
    hour: &'a str,
    day_period: Option<&'a str>,
}

impl fmt::Write for ClockParts<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.text.push_str(value);
        Ok(())
    }
}

impl PartsWrite for ClockParts<'_> {
    type SubPartsWrite = Self;
    fn with_part(&mut self, part: Part, mut write: impl FnMut(&mut Self) -> fmt::Result) -> fmt::Result {
        let start = self.text.len();
        write(self)?;
        if part == icu_datetime::parts::HOUR { self.text.replace_range(start.., self.hour); }
        // Intl's implicit day period uses AM/PM, while an explicit a field keeps its locale width.
        if part == icu_datetime::parts::DAY_PERIOD && let Some(period) = self.day_period { self.text.replace_range(start.., period); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::time::{FixedZone, parse_iso, to_local};

    #[test]
    fn locale_options_match_node_intl_oracle() {
        #[derive(serde::Deserialize)]
        struct Case { locale: String, time: String, format: String, expected: String }
        #[derive(serde::Deserialize)]
        struct Oracle { cases: Vec<Case> }
        let oracle: Oracle = serde_json::from_str(include_str!("fixtures/timestamp-intl.json")).unwrap();
        let mut mismatches = Vec::new();
        let regressions: Oracle = serde_json::from_str(include_str!("fixtures/timestamp-intl-regressions.json")).unwrap();
        let mut cases = oracle.cases;
        cases.extend(regressions.cases);
        if let Some(path) = std::env::var_os("T3_TIMESTAMP_SWEEP") {
            cases.extend(serde_json::from_str::<Oracle>(&std::fs::read_to_string(path).unwrap()).unwrap().cases);
        }
        for case in cases {
            let clock = IntlTimestampClock::new(&case.locale).unwrap();
            let iso = format!("2026-07-03T{}:00Z", case.time);
            let local = to_local(parse_iso(&iso).unwrap(), &FixedZone(0));
            let actual = clock.time(local, TimestampFormat::parse(&case.format).unwrap());
            if actual != case.expected {
                let index = match TimestampFormat::parse(&case.format).unwrap() { TimestampFormat::Locale => 0, TimestampFormat::TwelveHour => 1, TimestampFormat::TwentyFourHour => 2 };
                let selected = &clock.formats[index];
                let pattern = selected.best_pattern.as_ref().map(|(_, pattern)| pattern.to_string()).unwrap_or_else(|| selected.formatter.format(&Time::start_of_day()).pattern().to_string());
                mismatches.push(format!("{} {} {}: {:?} != {:?}, pattern {pattern}", case.locale, case.time, case.format, actual, case.expected));
            }
        }
        assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    }

    #[test]
    fn numeric_dates_use_locale_order_separators_and_numerals() {
        let local = to_local(parse_iso("2026-07-03T00:30:00Z").unwrap(), &FixedZone(0));
        // en-US: M/d and M/d/y
        let en_us = IntlTimestampClock::new("en-US").unwrap();
        assert_eq!(en_us.numeric_date(local, false), "7/3");
        assert_eq!(en_us.numeric_date(local, true), "7/3/2026");

        // en-GB: dd/MM and dd/MM/y
        let en_gb = IntlTimestampClock::new("en-GB").unwrap();
        assert_eq!(en_gb.numeric_date(local, false), "03/07");
        assert_eq!(en_gb.numeric_date(local, true), "03/07/2026");

        // de-DE: day before month with dots (not US English 7/3/2026)
        let de_de = IntlTimestampClock::new("de-DE").unwrap();
        assert!(de_de.numeric_date(local, false).contains('.'));
        assert!(de_de.numeric_date(local, true).ends_with("2026"));
        assert!(de_de.numeric_date(local, true).starts_with('3') || de_de.numeric_date(local, true).starts_with("03"));

        // fr-CA: ISO-like year-month-day with hyphens
        let fr_ca = IntlTimestampClock::new("fr-CA").unwrap();
        assert!(fr_ca.numeric_date(local, false).contains('-'));
        assert_eq!(fr_ca.numeric_date(local, true), "2026-07-03");

        // ja-JP: year first with slashes
        let ja_jp = IntlTimestampClock::new("ja-JP").unwrap();
        assert!(ja_jp.numeric_date(local, false).contains('/'));
        assert!(ja_jp.numeric_date(local, true).starts_with("2026/"));

        // ar-EG: Arabic-Indic digits
        let ar_eg = IntlTimestampClock::new("ar-EG").unwrap();
        assert!(ar_eg.numeric_date(local, false).contains('٣'));
        assert!(ar_eg.numeric_date(local, true).contains("٢٠٢٦"));

        // fa-IR: Persian calendar & Extended Arabic-Indic numerals (matches Node Intl)
        let fa_ir = IntlTimestampClock::new("fa-IR").unwrap();
        assert_eq!(fa_ir.numeric_date(local, false), "۴/۱۲");
        assert_eq!(fa_ir.numeric_date(local, true), "۱۴۰۵/۴/۱۲");

        // th-TH: Buddhist calendar year 2569 (matches Node Intl)
        let th_th = IntlTimestampClock::new("th-TH").unwrap();
        assert_eq!(th_th.numeric_date(local, false), "3/7");
        assert_eq!(th_th.numeric_date(local, true), "3/7/2569");

        // bg-BG: regular space before г. (normalized from NNBSP to match Node Intl)
        let bg_bg = IntlTimestampClock::new("bg-BG").unwrap();
        assert_eq!(bg_bg.numeric_date(local, false), "3.07");
        assert_eq!(bg_bg.numeric_date(local, true), "3.07.2026 г.");
    }

    #[test]
    fn short_weekday_follows_app_locale() {
        // 2026-07-03 is a Friday.
        assert_eq!(format_short_weekday("en-US", 2026, 7, 3).as_deref(), Some("Fri"));
        let de = format_short_weekday("de", 2026, 7, 3).unwrap();
        assert!(de.starts_with("Fr"), "expected German Friday abbreviation, got {de:?}");
        let fr = format_short_weekday("fr", 2026, 7, 3).unwrap();
        assert!(fr.starts_with("ven"), "expected French Friday abbreviation, got {fr:?}");
    }
}
