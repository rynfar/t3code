//! Host locale formatting for `apps/web/src/timestampFormat.ts`; calendar decisions live in core.

use t3_core::client_settings::TimestampFormat;
use t3_core::sidebar::time::{LocalTime, Ms, Zone};
use t3_core::timeline::TimestampLocale;

pub struct SystemTimestampLocale;

pub struct ViewerZone {
    pub name: String,
    #[cfg(target_os = "macos")]
    zone: objc2::rc::Retained<objc2_foundation::NSTimeZone>,
}

impl ViewerZone {
    pub fn current() -> Self {
        let name = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_owned());
        #[cfg(target_os = "macos")]
        {
            use objc2_foundation::{NSString, NSTimeZone};
            match NSTimeZone::timeZoneWithName(&NSString::from_str(&name)) {
                Some(zone) => Self { name, zone },
                None => Self { name: "UTC".to_owned(), zone: NSTimeZone::timeZoneForSecondsFromGMT(0) },
            }
        }
        #[cfg(not(target_os = "macos"))]
        Self { name }
    }
}

impl Zone for ViewerZone {
    fn offset_seconds_at(&self, utc_ms: Ms) -> i64 {
        #[cfg(target_os = "macos")]
        { self.zone.secondsFromGMTForDate(&objc2_foundation::NSDate::dateWithTimeIntervalSince1970(utc_ms as f64 / 1000.)) as i64 }
        #[cfg(not(target_os = "macos"))]
        { crate::sidebar::controller::SystemZone.offset_seconds_at(utc_ms) }
    }
}

impl TimestampLocale for SystemTimestampLocale {
    fn time(&self, local: LocalTime, format: TimestampFormat) -> String {
        let template = match format {
            TimestampFormat::Locale => "jm",
            TimestampFormat::TwelveHour => "hm",
            TimestampFormat::TwentyFourHour => "Hm",
        };
        format_local(local, template)
    }

    fn numeric_date(&self, local: LocalTime, include_year: bool) -> String {
        format_local(local, if include_year { "yMd" } else { "Md" })
    }
}

#[cfg(target_os = "macos")]
fn format_local(local: LocalTime, template: &'static str) -> String {
    format_local_with_locale(local, template, &objc2_foundation::NSLocale::currentLocale())
}

#[cfg(target_os = "macos")]
fn format_local_with_locale(local: LocalTime, template: &'static str, locale: &objc2_foundation::NSLocale) -> String {
    use std::cell::RefCell;
    use objc2::rc::Retained;
    use objc2_foundation::{NSDate, NSDateFormatter, NSString, NSTimeZone};
    use t3_core::sidebar::time::{DAY_MS, HOUR_MS, MINUTE_MS, days_from_civil};

    #[derive(Default)]
    struct Formatters {
        locale: String,
        formats: Vec<(&'static str, Retained<NSDateFormatter>)>,
    }
    thread_local! { static CACHE: RefCell<Formatters> = RefCell::default(); }
    CACHE.with_borrow_mut(|cache| {
        let identifier = locale.localeIdentifier().to_string();
        if cache.locale != identifier {
            cache.locale = identifier;
            cache.formats.clear();
        }
        let index = cache.formats.iter().position(|(key, _)| *key == template).unwrap_or_else(|| {
            let formatter = NSDateFormatter::new();
            // Core has already resolved the zone at the instant; format its wall clock in UTC.
            formatter.setTimeZone(Some(&NSTimeZone::timeZoneForSecondsFromGMT(0)));
            formatter.setLocale(Some(locale));
            formatter.setLocalizedDateFormatFromTemplate(&NSString::from_str(template));
            // Intl's numeric hour is unpadded in locales whose default clock is 24-hour.
            let default = NSDateFormatter::dateFormatFromTemplate_options_locale(&NSString::from_str("jm"), 0, Some(locale));
            if matches!(template, "jm" | "Hm") && default.is_some_and(|pattern| pattern.to_string().contains('H')) {
                formatter.setDateFormat(Some(&NSString::from_str(&formatter.dateFormat().to_string().replace("HH", "H"))));
            }
            cache.formats.push((template, formatter));
            cache.formats.len() - 1
        });
        let ms = days_from_civil(local.year, local.month, local.day) * DAY_MS + local.hour * HOUR_MS + local.minute * MINUTE_MS;
        let date = NSDate::dateWithTimeIntervalSince1970(ms as f64 / 1000.);
        cache.formats[index].1.stringFromDate(&date).to_string().replace(['\u{202f}', '\u{a0}'], " ")
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use objc2_foundation::{NSLocale, NSString};
    use t3_core::sidebar::time::{FixedZone, parse_iso, to_local};

    #[test]
    fn host_locale_templates_match_intl_numeric_time_and_date() {
        let local = to_local(parse_iso("2026-07-03T00:30:00Z").unwrap(), &FixedZone(0));
        for (locale, clock, twelve, twenty_four, date, year) in [
            ("en_US", "12:30 AM", "12:30 AM", "00:30", "7/3", "7/3/2026"),
            ("en_GB", "0:30", "12:30 am", "0:30", "03/07", "03/07/2026"),
            ("de_DE", "0:30", "12:30 AM", "0:30", "3.7.", "3.7.2026"),
            ("ar_EG", "١٢:٣٠ ص", "١٢:٣٠ ص", "٠٠:٣٠", "٣\u{200f}/٧", "٣\u{200f}/٧\u{200f}/٢٠٢٦"),
        ] {
            let locale = NSLocale::localeWithLocaleIdentifier(&NSString::from_str(locale));
            for (template, expected) in [("jm", clock), ("hm", twelve), ("Hm", twenty_four), ("Md", date), ("yMd", year)] {
                assert_eq!(format_local_with_locale(local, template, &locale), expected);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn format_local(local: LocalTime, template: &'static str) -> String {
    match template {
        "Hm" => format!("{:02}:{:02}", local.hour, local.minute),
        "hm" | "jm" => format!("{}:{:02} {}", (local.hour + 11) % 12 + 1, local.minute, if local.hour < 12 { "AM" } else { "PM" }),
        "yMd" => format!("{}/{}/{}", local.month, local.day, local.year),
        _ => format!("{}/{}", local.month, local.day),
    }
}
