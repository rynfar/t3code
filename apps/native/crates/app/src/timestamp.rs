//! Host preferences for `apps/web/src/timestampFormat.ts`; formatting and zones live in core.

use std::cell::RefCell;
use t3_core::client_settings::TimestampFormat;
use t3_core::sidebar::time::{LocalTime, NamedZone};
use t3_core::timeline::{IntlTimestampClock, TimestampLocale};

pub fn viewer_zone() -> NamedZone {
    iana_time_zone::get_timezone().ok().and_then(|name| NamedZone::new(&name)).unwrap_or_else(|| NamedZone::new("UTC").expect("UTC is a bundled zone"))
}

pub struct SystemTimestampLocale;

fn with_locale<T>(format: impl FnOnce(&IntlTimestampClock) -> T) -> T {
    thread_local! { static CACHE: RefCell<Option<(String, IntlTimestampClock)>> = const { RefCell::new(None) }; }
    #[cfg(target_os = "macos")]
    let tag = objc2_foundation::NSLocale::currentLocale().localeIdentifier().to_string().split('@').next().unwrap_or("en-US").replace('_', "-");
    #[cfg(not(target_os = "macos"))]
    let tag = sys_locale::get_locale().unwrap_or_else(|| "en-US".to_owned());
    CACHE.with_borrow_mut(|cache| {
        if cache.as_ref().is_none_or(|(previous, _)| *previous != tag) {
            let locale = IntlTimestampClock::new(&tag).or_else(|| IntlTimestampClock::new("en-US")).expect("en-US is bundled");
            *cache = Some((tag, locale));
        }
        format(&cache.as_ref().expect("locale is cached").1)
    })
}

impl TimestampLocale for SystemTimestampLocale {
    fn time(&self, local: LocalTime, format: TimestampFormat) -> String {
        with_locale(|locale| locale.time(local, format))
    }
    fn numeric_date(&self, local: LocalTime, include_year: bool) -> String {
        #[cfg(target_os = "macos")]
        { numeric_date(local, include_year, &objc2_foundation::NSLocale::currentLocale()) }
        #[cfg(not(target_os = "macos"))]
        { if include_year { format!("{}/{}/{}", local.month, local.day, local.year) } else { format!("{}/{}", local.month, local.day) } }
    }
}

#[cfg(target_os = "macos")]
fn numeric_date(local: LocalTime, include_year: bool, locale: &objc2_foundation::NSLocale) -> String {
    use objc2::rc::Retained;
    use objc2_foundation::{NSDate, NSDateFormatter, NSString, NSTimeZone};
    use t3_core::sidebar::time::{DAY_MS, days_from_civil};
    #[derive(Default)]
    struct Dates {
        locale: String,
        formats: Option<[Retained<NSDateFormatter>; 2]>,
    }
    thread_local! { static CACHE: RefCell<Dates> = RefCell::default(); }
    CACHE.with_borrow_mut(|cache| {
        let tag = locale.localeIdentifier().to_string();
        if cache.locale != tag || cache.formats.is_none() {
            cache.locale = tag;
            cache.formats = Some(["Md", "yMd"].map(|template| {
                let formatter = NSDateFormatter::new();
                formatter.setTimeZone(Some(&NSTimeZone::timeZoneForSecondsFromGMT(0)));
                formatter.setLocale(Some(locale));
                formatter.setLocalizedDateFormatFromTemplate(&NSString::from_str(template));
                formatter
            }));
        }
        let date = NSDate::dateWithTimeIntervalSince1970((days_from_civil(local.year, local.month, local.day) * DAY_MS) as f64 / 1000.);
        cache.formats.as_ref().expect("date formats are cached")[usize::from(include_year)].stringFromDate(&date).to_string()
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use objc2_foundation::{NSLocale, NSString};
    use t3_core::sidebar::time::{FixedZone, parse_iso, to_local};

    #[test]
    fn host_numeric_dates_match_intl() {
        let local = to_local(parse_iso("2026-07-03T00:30:00Z").unwrap(), &FixedZone(0));
        for (tag, date, year) in [
            ("en_US", "7/3", "7/3/2026"),
            ("en_GB", "03/07", "03/07/2026"),
            ("de_DE", "3.7.", "3.7.2026"),
            ("es_MX", "3/7", "3/7/2026"),
            ("fr_CA", "07-03", "2026-07-03"),
            ("ar_EG", "٣\u{200f}/٧", "٣\u{200f}/٧\u{200f}/٢٠٢٦"),
        ] {
            let locale = NSLocale::localeWithLocaleIdentifier(&NSString::from_str(tag));
            assert_eq!(numeric_date(local, false, &locale), date);
            assert_eq!(numeric_date(local, true, &locale), year);
        }
    }
}
