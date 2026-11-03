//! OS reads for `ElectronApp.systemLocale`; the resolved locale is cached like web's timestampLocale.

use std::cell::OnceCell;
use t3_core::client_settings::TimestampFormat;
use t3_core::sidebar::time::{LocalTime, NamedZone};
use t3_core::timeline::{IntlTimestampClock, TimestampLocale};

pub fn viewer_zone() -> NamedZone {
    iana_time_zone::get_timezone().ok().and_then(|name| NamedZone::new(&name)).unwrap_or_else(|| NamedZone::new("UTC").expect("UTC is a bundled zone"))
}

pub struct SystemTimestampLocale;

struct LocaleFormats {
    clock: IntlTimestampClock,
    #[cfg(target_os = "macos")]
    dates: [objc2::rc::Retained<objc2_foundation::NSDateFormatter>; 2],
}

fn host_locale_tag() -> String {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSLocale, NSString, NSUserDefaults};
        let defaults = NSUserDefaults::standardUserDefaults();
        t3_core::timeline::mac_time_locale(
            &NSLocale::currentLocale().localeIdentifier().to_string(),
            defaults.boolForKey(&NSString::from_str("AppleICUForce24HourTime")),
            defaults.boolForKey(&NSString::from_str("AppleICUForce12HourTime")),
        )
    }
    #[cfg(target_os = "linux")]
    {
        let identifier = sys_locale::get_locale().unwrap_or_else(|| "en-US".to_owned());
        let all = std::env::var("LC_ALL").ok();
        let time = std::env::var("LC_TIME").ok();
        let lang = std::env::var("LANG").ok();
        t3_core::timeline::linux_time_locale(&identifier, all.as_deref(), time.as_deref(), lang.as_deref())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    { t3_core::timeline::to_system_locale_tag(&sys_locale::get_locale().unwrap_or_else(|| "en-US".to_owned()), None) }
}

impl LocaleFormats {
    fn new(tag: &str) -> Self {
        let (tag, clock) = match IntlTimestampClock::new(tag) {
            Some(clock) => (tag, clock),
            None => ("en-US", IntlTimestampClock::new("en-US").expect("en-US is bundled")),
        };
        #[cfg(target_os = "macos")]
        let dates = {
            use objc2_foundation::{NSDateFormatter, NSLocale, NSString, NSTimeZone};
            let locale = NSLocale::localeWithLocaleIdentifier(&NSString::from_str(tag));
            // Foundation's en-NL numeric template omits the zeros Intl keeps.
            // regionCode requires macOS 14; the bundle still supports macOS 13.
            #[allow(deprecated)]
            let padded = locale.languageCode().to_string() == "en" && locale.countryCode().is_some_and(|region| region.to_string() == "NL");
            let templates = if padded { ["MMdd", "yMMdd"] } else { ["Md", "yMd"] };
            templates.map(|template| {
                let formatter = NSDateFormatter::new();
                formatter.setTimeZone(Some(&NSTimeZone::timeZoneForSecondsFromGMT(0)));
                formatter.setLocale(Some(&locale));
                formatter.setLocalizedDateFormatFromTemplate(&NSString::from_str(template));
                formatter
            })
        };
        #[cfg(not(target_os = "macos"))]
        let _ = tag;
        Self { clock, #[cfg(target_os = "macos")] dates }
    }

    fn numeric_date(&self, local: LocalTime, include_year: bool) -> String {
        #[cfg(target_os = "macos")]
        {
            use t3_core::sidebar::time::{DAY_MS, days_from_civil};
            let date = objc2_foundation::NSDate::dateWithTimeIntervalSince1970((days_from_civil(local.year, local.month, local.day) * DAY_MS) as f64 / 1000.);
            self.dates[usize::from(include_year)].stringFromDate(&date).to_string()
        }
        #[cfg(not(target_os = "macos"))]
        { if include_year { format!("{}/{}/{}", local.month, local.day, local.year) } else { format!("{}/{}", local.month, local.day) } }
    }
}

fn with_locale<T>(format: impl FnOnce(&LocaleFormats) -> T) -> T {
    thread_local! { static CACHE: OnceCell<LocaleFormats> = const { OnceCell::new() }; }
    CACHE.with(|cache| format(cache.get_or_init(|| LocaleFormats::new(&host_locale_tag()))))
}

impl TimestampLocale for SystemTimestampLocale {
    fn time(&self, local: LocalTime, format: TimestampFormat) -> String {
        with_locale(|locale| locale.clock.time(local, format))
    }
    fn numeric_date(&self, local: LocalTime, include_year: bool) -> String {
        with_locale(|locale| locale.numeric_date(local, include_year))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use t3_core::sidebar::time::{FixedZone, parse_iso, to_local};

    #[test]
    fn host_numeric_dates_match_intl() {
        let local = to_local(parse_iso("2026-07-03T00:30:00Z").unwrap(), &FixedZone(0));
        for (tag, date, year) in [
            ("en_US", "7/3", "7/3/2026"),
            ("en_GB", "03/07", "03/07/2026"),
            ("en_US@rg=nlzzzz", "03/07", "03/07/2026"),
            ("en_Latn_US@rg=nlzzzz", "03/07", "03/07/2026"),
            ("en-US-u-hc-h23", "7/3", "7/3/2026"),
            ("de_DE", "3.7.", "3.7.2026"),
            ("es_MX", "3/7", "3/7/2026"),
            ("fr_CA", "07-03", "2026-07-03"),
            ("ar_EG", "٣\u{200f}/٧", "٣\u{200f}/٧\u{200f}/٢٠٢٦"),
        ] {
            let locale = LocaleFormats::new(&t3_core::timeline::to_system_locale_tag(tag, None));
            assert_eq!(locale.numeric_date(local, false), date);
            assert_eq!(locale.numeric_date(local, true), year);
        }
    }
}
