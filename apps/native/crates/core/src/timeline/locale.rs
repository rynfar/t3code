//! Intl's numeric hour/minute options (`apps/web/src/timestampFormat.ts`).

use std::fmt;
use icu_datetime::{DateTimeFormatterPreferences, NoCalendarFormatter, fieldsets::T, input::Time, preferences::HourCycle};
use icu_locale_core::Locale;
use writeable::{Part, PartsWrite, Writeable};

use crate::client_settings::TimestampFormat;
use crate::sidebar::time::LocalTime;

pub struct IntlTimestampClock {
    clock: NoCalendarFormatter<T>,
    twelve: NoCalendarFormatter<T>,
    twenty_four: NoCalendarFormatter<T>,
    default_is_twelve: bool,
}

impl IntlTimestampClock {
    pub fn new(tag: &str) -> Option<Self> {
        let locale = tag.parse::<Locale>().ok()?;
        let prefs = DateTimeFormatterPreferences::from(&locale);
        let clock = NoCalendarFormatter::try_new(prefs, T::hm()).ok()?;
        let mut resolved = NumericHour::default();
        clock.format(&Time::start_of_day()).write_to_parts(&mut resolved).ok()?;
        Some(Self {
            default_is_twelve: resolved.day_period,
            clock,
            twelve: NoCalendarFormatter::try_new({ let mut p = prefs; p.hour_cycle = Some(HourCycle::Clock12); p }, T::hm()).ok()?,
            twenty_four: NoCalendarFormatter::try_new({ let mut p = prefs; p.hour_cycle = Some(HourCycle::Clock24); p }, T::hm()).ok()?,
        })
    }

    pub fn time(&self, local: LocalTime, format: TimestampFormat) -> String {
        let (formatter, hour12) = match format {
            TimestampFormat::Locale => (&self.clock, self.default_is_twelve),
            TimestampFormat::TwelveHour => (&self.twelve, true),
            TimestampFormat::TwentyFourHour => (&self.twenty_four, false),
        };
        let Ok(time) = Time::try_new(local.hour as u8, local.minute as u8, 0, 0) else { return String::new() };
        // Intl preserves the matched 24-hour pattern's width when overriding a 12-hour locale.
        let numeric_hour = format != TimestampFormat::TwentyFourHour || !self.default_is_twelve;
        let hour = if hour12 { (local.hour + 11) % 12 + 1 } else { local.hour };
        let mut writer = NumericHour { unpad: numeric_hour && hour < 10, ..Default::default() };
        let _ = formatter.format(&time).write_to_parts(&mut writer);
        writer.text.replace(['\u{202f}', '\u{a0}'], " ")
    }
}

#[derive(Default)]
struct NumericHour {
    text: String,
    unpad: bool,
    day_period: bool,
}

impl fmt::Write for NumericHour {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.text.push_str(value);
        Ok(())
    }
}

impl PartsWrite for NumericHour {
    type SubPartsWrite = Self;
    fn with_part(&mut self, part: Part, mut write: impl FnMut(&mut Self) -> fmt::Result) -> fmt::Result {
        self.day_period |= part == icu_datetime::parts::DAY_PERIOD;
        let start = self.text.len();
        write(self)?;
        if self.unpad && part == icu_datetime::parts::HOUR && self.text[start..].chars().count() == 2 {
            self.text.remove(start);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::time::{FixedZone, parse_iso, to_local};

    #[test]
    fn locale_options_match_intl_numeric_time() {
        let local = to_local(parse_iso("2026-07-03T00:30:00Z").unwrap(), &FixedZone(0));
        for (locale, clock, twelve, twenty_four) in [
            ("en-US", "12:30 AM", "12:30 AM", "00:30"),
            ("en-GB", "0:30", "12:30 am", "0:30"),
            ("de-DE", "0:30", "12:30 AM", "0:30"),
            ("es-MX", "12:30 a.m.", "12:30 a.m.", "00:30"),
            ("fr-CA", "0 h 30", "12 h 30 a.m.", "0 h 30"),
            ("ar-EG", "١٢:٣٠ ص", "١٢:٣٠ ص", "٠٠:٣٠"),
        ] {
            let locale = IntlTimestampClock::new(locale).unwrap();
            for (format, expected) in [(TimestampFormat::Locale, clock), (TimestampFormat::TwelveHour, twelve), (TimestampFormat::TwentyFourHour, twenty_four)] {
                assert_eq!(locale.time(local, format), expected);
            }
        }
    }
}
