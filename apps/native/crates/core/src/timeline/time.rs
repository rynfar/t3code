//! Timestamps and durations: `formatDuration` (`packages/shared/src/orchestrationTiming.ts`),
//! `formatTokens` (`packages/shared/src/usageFormat.ts`), and `apps/web/src/timestampFormat.ts`.

use crate::client_settings::TimestampFormat;
use crate::sidebar::time::{LocalTime, Ms, Zone, days_from_civil, to_local};

/// Locale formatting is injected, like the zone, so core never reads host preferences.
pub trait TimestampLocale {
    fn time(&self, local: LocalTime, format: TimestampFormat) -> String;
    fn numeric_date(&self, local: LocalTime, include_year: bool) -> String;
}

pub fn format_day_aware(iso: &str, now: Ms, zone: &dyn Zone, format: TimestampFormat, locale: &dyn TimestampLocale) -> String {
    format_calendar_timestamp(iso, now, zone, format, locale, false)
}

pub fn format_upcoming(iso: &str, now: Ms, zone: &dyn Zone, format: TimestampFormat, locale: &dyn TimestampLocale) -> String {
    format_calendar_timestamp(iso, now, zone, format, locale, true)
}

fn format_calendar_timestamp(iso: &str, now: Ms, zone: &dyn Zone, format: TimestampFormat, locale: &dyn TimestampLocale, upcoming: bool) -> String {
    let Some(ms) = parse_iso_ms(iso) else { return String::new() };
    let today = to_local(now, zone);
    let target = to_local(ms, zone);
    let day_diff = days_from_civil(target.year, target.month, target.day) - days_from_civil(today.year, today.month, today.day);
    let time = locale.time(target, format);
    match day_diff {
        0 => time,
        1 if upcoming => format!("tomorrow at {time}"),
        1.. if !upcoming => time,
        -1 => format!("yesterday at {time}"),
        _ => format!("{} {time}", locale.numeric_date(target, target.year != today.year)),
    }
}

/// `TimelineWorkEntryRow`: even a nonempty invalid reset keeps the zone suffix on the web.
pub fn usage_limit_label(reset: Option<&str>, now: Ms, zone: &dyn Zone, zone_name: &str, format: TimestampFormat, locale: &dyn TimestampLocale) -> String {
    match reset.filter(|reset| !reset.is_empty()) {
        Some(reset) => format!("Usage limit reached. Retry after {} ({zone_name}).", format_upcoming(reset, now, zone, format, locale)),
        None => "Usage limit reached.".to_owned(),
    }
}

/// Milliseconds since the Unix epoch for an ISO 8601 timestamp such as
/// `2026-01-01T00:00:05.250Z` or `2026-01-01T02:00:05+02:00`.
pub fn parse_iso_ms(value: &str) -> Option<i64> {
    let bytes = value.trim().as_bytes();
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let slice = bytes.get(range)?;
        if slice.is_empty() || !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(slice).ok()?.parse().ok()
    };
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') || !matches!(bytes.get(10), Some(b'T' | b't' | b' ')) {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut index = 19;
    let mut millis = 0i64;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        let digits = &bytes[start..index];
        if digits.is_empty() {
            return None;
        }
        for (position, digit) in digits.iter().take(3).enumerate() {
            millis += i64::from(digit - b'0') * 10i64.pow(2 - position as u32);
        }
    }
    let offset_minutes = match bytes.get(index) {
        Some(b'Z' | b'z') if index + 1 == bytes.len() => 0,
        Some(sign @ (b'+' | b'-')) => {
            let hours = num(index + 1..index + 3)?;
            let minutes = match bytes.get(index + 3) {
                Some(b':') => num(index + 4..index + 6)?,
                _ => num(index + 3..index + 5).unwrap_or(0),
            };
            let total = hours * 60 + minutes;
            if *sign == b'+' { total } else { -total }
        }
        None => 0,
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    Some(((days * 86_400 + hour * 3_600 + minute * 60 + second) - offset_minutes * 60) * 1_000 + millis)
}

pub fn format_duration(duration_ms: f64) -> String {
    if !duration_ms.is_finite() || duration_ms < 0. {
        return "0ms".into();
    }
    if duration_ms < 1_000. {
        return format!("{}ms", duration_ms.round().max(1.) as i64);
    }
    if duration_ms < 10_000. {
        let tenths = (duration_ms / 100.).round() / 10.;
        return if tenths.fract() == 0. { format!("{}s", tenths as i64) } else { format!("{tenths}s") };
    }
    let total = (duration_ms / 1_000.).round() as i64;
    if total < 60 {
        return format!("{total}s");
    }
    let (hours, minutes, seconds) = (total / 3_600, (total % 3_600) / 60, total % 60);
    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

pub fn format_tokens(value: u64) -> String {
    let value = value as f64;
    let trim = |scaled: f64| {
        let digits = if scaled >= 100. { 0 } else if scaled >= 10. { 1 } else { 2 };
        let text = format!("{scaled:.digits$}");
        if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_owned() } else { text }
    };
    for (scale, suffix) in [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")] {
        if value >= scale {
            return format!("{}{suffix}", trim(value / scale));
        }
    }
    let whole = value.round() as u64;
    let digits = whole.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `h:mm AM` in UTC offset by `offset_minutes`; the view passes the local offset.
pub fn format_short_time(iso: &str, offset_minutes: i64) -> Option<String> {
    let ms = parse_iso_ms(iso)? + offset_minutes * 60_000;
    let minutes_of_day = (ms.div_euclid(60_000)).rem_euclid(24 * 60);
    let (hour, minute) = (minutes_of_day / 60, minutes_of_day % 60);
    let (display, meridiem) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    Some(format!("{display}:{minute:02} {meridiem}"))
}
