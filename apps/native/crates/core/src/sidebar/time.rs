//! Timestamps as epoch milliseconds, plus the sidebar's compact labels
//! (`apps/web/src/timestampFormat.ts`, `client-runtime/src/state/models.ts`).

pub type Ms = i64;

pub const MINUTE_MS: Ms = 60_000;
pub const HOUR_MS: Ms = 60 * MINUTE_MS;
pub const DAY_MS: Ms = 24 * HOUR_MS;

/// RFC 3339 / `toISOString()` to epoch ms. Invalid input is `None`, as `Date.parse` gives NaN.
pub fn parse_iso(value: &str) -> Option<Ms> {
    let bytes = value.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let text = value.get(range)?;
        text.bytes().all(|b| b.is_ascii_digit()).then(|| text.parse().ok())?
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = &value[19..];
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let padded = format!("{:0<3}", &fraction[..digits.min(3)]);
        millis = padded.parse().ok()?;
        rest = &fraction[digits..];
    }
    let offset_minutes = match rest {
        "Z" | "z" => 0,
        _ if rest.len() == 6 && matches!(rest.as_bytes()[0], b'+' | b'-') && rest.as_bytes()[3] == b':' => {
            let hours: i64 = rest[1..3].parse().ok()?;
            let minutes: i64 = rest[4..6].parse().ok()?;
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            sign * (hours * 60 + minutes)
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    Some(((days * 24 + hour) * 60 + minute - offset_minutes) * MINUTE_MS + second * 1000 + millis)
}

pub fn parse_opt(value: Option<&str>) -> Option<Ms> {
    value.and_then(parse_iso)
}

/// `Date.prototype.toISOString()`.
pub fn format_iso(ms: Ms) -> String {
    let days = ms.div_euclid(DAY_MS);
    let (year, month, day) = civil_from_days(days);
    let in_day = ms.rem_euclid(DAY_MS);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        in_day / HOUR_MS,
        in_day % HOUR_MS / MINUTE_MS,
        in_day % MINUTE_MS / 1000,
        in_day % 1000
    )
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 31,
    }
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// `formatRelativeTimeLabel` through `compactSidebarTimeLabel`: "now", "5m", "3h", "2d".
pub fn compact_relative_label(timestamp: Ms, now: Ms) -> String {
    let seconds = (now - timestamp).max(0) / 1000;
    if seconds < 60 {
        return "now".to_owned();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
}

/// `formatWorkingDurationLabel`.
pub fn working_duration_label(elapsed: Ms) -> String {
    let seconds = (elapsed / 1000).max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// Local wall-clock fields for an instant, given the zone's offset at that instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub millis: i64,
    /// 0 = Sunday, as `Date.getDay()`.
    pub weekday: i64,
}

/// The local zone: its UTC offset in seconds at an instant. Injected so core stays pure.
pub trait Zone {
    fn offset_seconds_at(&self, utc_ms: Ms) -> i64;
}

#[derive(Debug, Clone, Copy)]
pub struct FixedZone(pub i64);

impl Zone for FixedZone {
    fn offset_seconds_at(&self, _utc_ms: Ms) -> i64 {
        self.0
    }
}

pub fn to_local(utc_ms: Ms, zone: &dyn Zone) -> LocalTime {
    let local = utc_ms + zone.offset_seconds_at(utc_ms) * 1000;
    let days = local.div_euclid(DAY_MS);
    let (year, month, day) = civil_from_days(days);
    let in_day = local.rem_euclid(DAY_MS);
    LocalTime {
        year,
        month,
        day,
        hour: in_day / HOUR_MS,
        minute: in_day % HOUR_MS / MINUTE_MS,
        second: in_day % MINUTE_MS / 1000,
        millis: in_day % 1000,
        weekday: (days + 4).rem_euclid(7),
    }
}

/// `new Date(year, month - 1, day, hour, minute)`: rolls over out-of-range days and resolves
/// a local time through the zone, moving forward across a spring-forward gap as JS does.
pub fn from_local(year: i64, month: i64, day: i64, hour: i64, minute: i64, zone: &dyn Zone) -> Ms {
    let naive = (days_from_civil(year, month, 1) + day - 1) * DAY_MS + hour * HOUR_MS + minute * MINUTE_MS;
    let first = naive - zone.offset_seconds_at(naive) * 1000;
    let second = naive - zone.offset_seconds_at(first) * 1000;
    if to_local_naive(second, zone) == naive { second } else { first.max(second) }
}

fn to_local_naive(utc_ms: Ms, zone: &dyn Zone) -> Ms {
    utc_ms + zone.offset_seconds_at(utc_ms) * 1000
}
