//! OS reads for `ElectronApp.systemLocale` and live system time zone detection
//! (`pingdotgg/t3code#32371`); the resolved locale is cached like web's `timestampLocale`, while
//! the viewer's time zone refreshes when `TZ` or the OS zone changes.

use std::cell::{OnceCell, RefCell};
use std::time::{Duration, Instant};

use t3_core::client_settings::TimestampFormat;
use t3_core::sidebar::time::{LocalTime, NamedZone};
use t3_core::timeline::{IntlTimestampClock, TimestampLocale};

/// Recheck the OS time zone at most once per second so feed rows share one system read
/// (`ZONE_RECHECK_MS` in `pingdotgg/t3code#32371`).
const ZONE_RECHECK: Duration = Duration::from_secs(1);

struct ViewerZoneCache {
    checked_at: Option<Instant>,
    tz_env: Option<String>,
    zone: Option<NamedZone>,
}

impl ViewerZoneCache {
    const fn new() -> Self {
        Self { checked_at: None, tz_env: None, zone: None }
    }

    fn resolve(
        &mut self,
        now: Instant,
        tz_env: Option<String>,
        read_os_zone: impl FnOnce() -> Option<String>,
    ) -> NamedZone {
        let tz_changed = self.tz_env != tz_env;
        let fresh = !tz_changed
            && self
                .checked_at
                .is_some_and(|at| now.saturating_duration_since(at) < ZONE_RECHECK);
        if fresh && let Some(zone) = self.zone {
            return zone;
        }
        let from_tz = tz_env
            .as_deref()
            .map(|value| value.trim().strip_prefix(':').unwrap_or(value.trim()))
            .filter(|value| !value.is_empty())
            .and_then(NamedZone::new);
        let zone = from_tz
            .or_else(|| read_os_zone().and_then(|name| NamedZone::new(&name)))
            .unwrap_or_else(|| NamedZone::new("UTC").expect("UTC is a bundled zone"));
        self.checked_at = Some(now);
        self.tz_env = tz_env;
        self.zone = Some(zone);
        zone
    }
}

fn read_os_timezone_name() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::NSTimeZone;
        // CoreFoundation/NSTimeZone caches the system zone until explicitly reset; resetting on
        // the 1s recheck lets live OS zone changes update rendered timestamps (`pingdotgg/t3code#32371`).
        NSTimeZone::resetSystemTimeZone();
        let name = NSTimeZone::systemTimeZone().name().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    iana_time_zone::get_timezone().ok()
}

pub fn viewer_zone() -> NamedZone {
    thread_local! {
        static ZONE_CACHE: RefCell<ViewerZoneCache> = const { RefCell::new(ViewerZoneCache::new()) };
    }
    let tz_env = std::env::var("TZ").ok();
    ZONE_CACHE.with(|cache| cache.borrow_mut().resolve(Instant::now(), tz_env, read_os_timezone_name))
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
        { self.clock.numeric_date(local, include_year) }
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

/// Formats `ms` as local medium date + short time respecting `format` (for one-shot schedules and deliveries).
pub(crate) fn once_label(ms: i64, format: TimestampFormat) -> String {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    let Some(time) = Local.timestamp_millis_opt(ms).single() else {
        return String::new();
    };
    let local = LocalTime {
        year: i64::from(time.year()),
        month: i64::from(time.month()),
        day: i64::from(time.day()),
        hour: i64::from(time.hour()),
        minute: i64::from(time.minute()),
        second: i64::from(time.second()),
        millis: i64::from(time.nanosecond() / 1_000_000),
        weekday: i64::from(time.weekday().num_days_from_sunday()),
    };
    format!("{}, {}", time.format("%b %-d, %Y"), SystemTimestampLocale.time(local, format))
}

#[cfg(test)]
mod zone_tests {
    use super::*;
    use t3_core::sidebar::time::parse_iso;
    use t3_core::timeline::{IntlTimestampClock, format_day_aware, format_upcoming};

    #[test]
    fn viewer_zone_follows_system_timezone_and_tz_env_changes() {
        let mut cache = ViewerZoneCache::new();
        let t0 = Instant::now();
        let mut os_calls = 0usize;
        let same_day = "2026-11-07T04:45:43.877Z";
        let earlier = "2026-11-04T20:00:00.000Z";
        let now = parse_iso(same_day).unwrap();
        let en = IntlTimestampClock::new("en-US").unwrap();

        struct Clock<'a>(&'a IntlTimestampClock);
        impl TimestampLocale for Clock<'_> {
            fn time(&self, local: LocalTime, format: TimestampFormat) -> String {
                self.0.time(local, format)
            }
            fn numeric_date(&self, local: LocalTime, include_year: bool) -> String {
                self.0.numeric_date(local, include_year)
            }
        }
        let locale = Clock(&en);

        // Initial OS zone: America/New_York (UTC-5 in Nov -> 11:45 PM / 23:45 on Nov 6).
        let ny = cache.resolve(t0, None, || {
            os_calls += 1;
            Some("America/New_York".into())
        });
        assert_eq!(os_calls, 1);
        assert_eq!(format_day_aware(same_day, now, &ny, TimestampFormat::TwentyFourHour, &locale), "23:45");

        // Within 1 second, cached OS zone is reused without re-reading the OS.
        let ny_cached = cache.resolve(t0 + Duration::from_millis(500), None, || {
            os_calls += 1;
            Some("Asia/Bangkok".into())
        });
        assert_eq!(os_calls, 1);
        assert_eq!(format_day_aware(same_day, now, &ny_cached, TimestampFormat::TwentyFourHour, &locale), "23:45");

        // After 1 second, OS zone change to Asia/Bangkok (UTC+7 -> 11:45 on Nov 7) takes effect.
        let bkk = cache.resolve(t0 + Duration::from_secs(1), None, || {
            os_calls += 1;
            Some("Asia/Bangkok".into())
        });
        assert_eq!(os_calls, 2);
        assert_eq!(format_day_aware(same_day, now, &bkk, TimestampFormat::TwentyFourHour, &locale), "11:45");
        assert_eq!(format_day_aware(earlier, now, &bkk, TimestampFormat::TwentyFourHour, &locale), "11/5 03:00");
        assert_eq!(format_upcoming("2026-11-08T04:45:43.877Z", now, &bkk, TimestampFormat::TwentyFourHour, &locale), "tomorrow at 11:45");

        // An explicit TZ change takes effect immediately even within the 1s window.
        let utc = cache.resolve(t0 + Duration::from_millis(1_100), Some(":UTC".into()), || {
            os_calls += 1;
            Some("Asia/Bangkok".into())
        });
        assert_eq!(os_calls, 2);
        assert_eq!(format_day_aware(same_day, now, &utc, TimestampFormat::TwentyFourHour, &locale), "04:45");
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
