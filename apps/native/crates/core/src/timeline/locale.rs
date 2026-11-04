//! Intl's numeric hour/minute options (`apps/web/src/timestampFormat.ts`).

use std::fmt;
use icu_calendar::Gregorian;
use icu_datetime::{DateTimeFormatterPreferences, NoCalendarFormatter, fieldsets::T, input::Time, pattern::{DateTimePattern, DayPeriodNameLength, FixedCalendarDateTimeNames}, preferences::HourCycle};
use icu_decimal::{DecimalFormatter, input::Decimal};
use icu_locale_core::{Locale, extensions::unicode::Key};
use writeable::{Part, PartsWrite, TryWriteable, Writeable};

use crate::client_settings::TimestampFormat;
use crate::sidebar::time::LocalTime;

#[derive(Clone, Copy)]
enum Cycle { H11, H12, H23, H24 }

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
    cycle: Cycle,
    width: usize,
    day_periods: Option<[String; 2]>,
}

pub struct IntlTimestampClock {
    formats: [ClockFormat; 3],
    hours: [String; 25],
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
        let base_cycle = pattern_fields(&regional).0?;
        let regional_twelve = NoCalendarFormatter::try_new({ let mut p = prefs; p.hour_cycle = Some(HourCycle::Clock12); p }, T::hm()).ok()?;
        let locale_cycle = requested.unwrap_or(base_cycle);
        let cycles = [locale_cycle, if locale_cycle.twelve() { locale_cycle } else { pattern_fields(&regional_twelve).0? }, if locale_cycle.twelve() { Cycle::H23 } else { locale_cycle }];
        let make = |cycle: Cycle| -> Option<ClockFormat> {
            let mut p = prefs;
            p.hour_cycle = Some(if cycle.twelve() { HourCycle::Clock12 } else { HourCycle::Clock24 });
            // V8 matches against the base locale before replacing the cycle; hc doesn't change that width.
            let width = if !cycle.twelve() && base_cycle.twelve() { 2 } else { 1 };
            let formatter = NoCalendarFormatter::try_new(p, T::hm()).ok()?;
            // ICU4C 78 uses AM/PM for Korean; ICU4X's names differ from the web oracle.
            let day_periods = if cycle.twelve() && locale.id.language.as_str() == "ko" {
                Some(["AM".to_owned(), "PM".to_owned()])
            } else if let Some(length) = pattern_fields(&formatter).1 {
                let mut names: FixedCalendarDateTimeNames<Gregorian, T> = FixedCalendarDateTimeNames::try_new(prefs).ok()?;
                names.include_day_period_names(length).ok()?;
                let symbol = match length { DayPeriodNameLength::Narrow => "aaaaa", DayPeriodNameLength::Wide => "aaaa", _ => "a" };
                let period = DateTimePattern::try_from_pattern_str(symbol).ok()?;
                Some([0, 12].map(|hour| names.with_pattern_unchecked(&period).format(&Time::try_new(hour, 0, 0, 0).expect("valid hour")).try_write_to_string().expect("day periods are loaded").into_owned()))
            } else { None };
            Some(ClockFormat { formatter, cycle, width, day_periods })
        };
        let decimal = DecimalFormatter::try_new((&locale).into(), Default::default()).ok()?;
        Some(Self {
            formats: [make(cycles[0])?, make(cycles[1])?, make(cycles[2])?],
            hours: std::array::from_fn(|hour| decimal.format(&Decimal::from(hour as u8)).to_string()),
        })
    }

    pub fn time(&self, local: LocalTime, format: TimestampFormat) -> String {
        let index = match format { TimestampFormat::Locale => 0, TimestampFormat::TwelveHour => 1, TimestampFormat::TwentyFourHour => 2 };
        let selected = &self.formats[index];
        let Ok(time) = Time::try_new(local.hour as u8, local.minute as u8, 0, 0) else { return String::new() };
        let hour = selected.cycle.value(local.hour) as usize;
        let padded = if selected.width == 2 && hour < 10 { format!("{}{}", self.hours[0], self.hours[hour]) } else { self.hours[hour].clone() };
        let mut writer = ClockParts { text: String::new(), hour: &padded, day_period: selected.day_periods.as_ref().map(|periods| periods[usize::from(local.hour >= 12)].as_str()) };
        let _ = selected.formatter.format(&time).write_to_parts(&mut writer);
        writer.text.replace(['\u{202f}', '\u{a0}'], " ")
    }
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
        for case in oracle.cases {
            let clock = IntlTimestampClock::new(&case.locale).unwrap();
            let iso = format!("2026-07-03T{}:00Z", case.time);
            let local = to_local(parse_iso(&iso).unwrap(), &FixedZone(0));
            let actual = clock.time(local, TimestampFormat::parse(&case.format).unwrap());
            if actual != case.expected {
                let index = match TimestampFormat::parse(&case.format).unwrap() { TimestampFormat::Locale => 0, TimestampFormat::TwelveHour => 1, TimestampFormat::TwentyFourHour => 2 };
                let pattern = clock.formats[index].formatter.format(&Time::start_of_day()).pattern();
                mismatches.push(format!("{} {} {}: {:?} != {:?}, pattern {pattern}", case.locale, case.time, case.format, actual, case.expected));
            }
        }
        assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    }
}
