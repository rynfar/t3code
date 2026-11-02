//! Timestamps and durations: `formatDuration` (`packages/shared/src/orchestrationTiming.ts`),
//! `formatTokens` (`packages/shared/src/usageFormat.ts`). Wire timestamps are ISO 8601 UTC.

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

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
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

/// `formatDayAwareTimestamp` (`apps/web/src/timestampFormat.ts`): the time today,
/// `yesterday at …` and `M/D …` (with the year when it differs) before that.
pub fn format_day_aware(iso: &str, now_ms: i64, offset_minutes: i64) -> Option<String> {
    let ms = parse_iso_ms(iso)?;
    let time = format_short_time(iso, offset_minutes)?;
    let local_day = |ms: i64| (ms + offset_minutes * 60_000).div_euclid(86_400_000);
    let day_diff = local_day(now_ms) - local_day(ms);
    if day_diff <= 0 {
        return Some(time);
    }
    if day_diff == 1 {
        return Some(format!("yesterday at {time}"));
    }
    let (year, month, day) = civil_from_days(local_day(ms));
    let (now_year, _, _) = civil_from_days(local_day(now_ms));
    Some(if year == now_year { format!("{month}/{day} {time}") } else { format!("{month}/{day}/{year} {time}") })
}

/// `formatUpcomingTimestamp` (`apps/web/src/timestampFormat.ts`): the time today,
/// `tomorrow at …`, `M/D …` (with the year when it differs) after that, day-aware before today.
pub fn format_upcoming(iso: &str, now_ms: i64, offset_minutes: i64) -> Option<String> {
    let ms = parse_iso_ms(iso)?;
    let time = format_short_time(iso, offset_minutes)?;
    let local_day = |ms: i64| (ms + offset_minutes * 60_000).div_euclid(86_400_000);
    match local_day(ms) - local_day(now_ms) {
        ..0 => format_day_aware(iso, now_ms, offset_minutes),
        0 => Some(time),
        1 => Some(format!("tomorrow at {time}")),
        _ => {
            let (year, month, day) = civil_from_days(local_day(ms));
            let (now_year, _, _) = civil_from_days(local_day(now_ms));
            Some(if year == now_year { format!("{month}/{day} {time}") } else { format!("{month}/{day}/{year} {time}") })
        }
    }
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
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
