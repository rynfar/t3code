//! Figures and windows for the Usage page: `packages/shared/src/usageFormat.ts`. Number formats
//! follow `Intl.NumberFormat("en-US")` and `Number.prototype.toFixed` exactly, including how each
//! rounds a tie. Times read in the viewer's zone, which `Zone` injects.

use std::collections::HashMap;

use super::merge::{ContractMismatch, MismatchDirection};
use super::{UsageProvider, UsageResolution, UsageSource, UsageSourceStatus, UsageSummaryInput};
use crate::sidebar::time::{HOUR_MS, MINUTE_MS, Ms, Zone, civil_from_days, days_from_civil, format_iso, parse_iso, to_local};

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `Number.prototype.toFixed`: the nearest decimal, and on an exact tie the larger magnitude.
pub fn to_fixed(value: f64, digits: usize) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let magnitude = value.abs();
    let exact = format!("{magnitude:.80}");
    let tie = exact.split_once('.').is_some_and(|(_, fraction)| {
        fraction.as_bytes().get(digits) == Some(&b'5') && fraction.as_bytes()[digits + 1..].iter().all(|digit| *digit == b'0')
    });
    let rounded = if tie { format!("{:.digits$}", magnitude.next_up()) } else { format!("{magnitude:.digits$}") };
    if value < 0. { format!("-{rounded}") } else { rounded }
}

/// Rounds the shortest round-trip decimal half away from zero, as ICU (and so `Intl`) does.
fn round_half_expand(magnitude: f64, digits: usize) -> (String, String) {
    let shortest = format!("{magnitude}");
    let (whole, fraction) = shortest.split_once('.').unwrap_or((&shortest, ""));
    let mut kept: Vec<u8> = whole.bytes().chain(fraction.bytes().chain(std::iter::repeat(b'0')).take(digits)).collect();
    if fraction.as_bytes().get(digits).is_some_and(|digit| *digit >= b'5') {
        let mut index = kept.len();
        loop {
            if index == 0 {
                kept.insert(0, b'1');
                break;
            }
            index -= 1;
            if kept[index] == b'9' {
                kept[index] = b'0';
            } else {
                kept[index] += 1;
                break;
            }
        }
    }
    let split = kept.len() - digits;
    let text = String::from_utf8(kept).unwrap_or_default();
    (text[..split].to_owned(), text[split..].to_owned())
}

fn group_thousands(digits: &str) -> String {
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `formatUsd`: `$1,234.56`.
pub fn format_usd(value: f64) -> String {
    let (whole, cents) = round_half_expand(value.abs(), 2);
    let sign = if value < 0. { "-" } else { "" };
    format!("{sign}${}.{cents}", group_thousands(&whole))
}

/// `Math.round`: halves go up.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

fn format_integer(value: f64) -> String {
    let rounded = js_round(value);
    let digits = format!("{:.0}", rounded.abs());
    if rounded < 0. { format!("-{}", group_thousands(&digits)) } else { group_thousands(&digits) }
}

/// `formatCount`: a rounded, grouped integer.
pub fn format_count(value: f64) -> String {
    format_integer(value)
}

fn trim(value: f64) -> String {
    let magnitude = value.abs();
    let digits = if magnitude >= 100. {
        0
    } else if magnitude >= 10. {
        1
    } else {
        2
    };
    let fixed = to_fixed(value, digits);
    match fixed.split_once('.') {
        Some((whole, fraction)) if fraction.bytes().all(|byte| byte == b'0') => whole.to_owned(),
        _ => fixed,
    }
}

/// `formatTokens`: `999`, `1.25K`, `12.5M`, `340B`.
pub fn format_tokens(value: f64) -> String {
    let magnitude = value.abs();
    for (scale, suffix) in [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")] {
        if magnitude >= scale {
            return format!("{}{suffix}", trim(value / scale));
        }
    }
    format_integer(value)
}

/// `formatPercent`: one decimal, with `<0.1%` for small non-zero shares.
pub fn format_percent(share: f64) -> String {
    format_percent_digits(share, 1)
}

pub fn format_percent_digits(share: f64, digits: usize) -> String {
    let percent = share * 100.;
    let smallest = 10f64.powi(-(digits as i32));
    if percent > 0. && percent < smallest {
        return format!("<{}%", to_fixed(smallest, digits));
    }
    format!("{}%", to_fixed(percent, digits))
}

pub fn format_malformed_usage_warning(count: u64) -> String {
    format!("{count} malformed or inconsistent usage record(s) were skipped; usage may be incomplete.")
}

/// `usageSourceMessages`: what the page says above the totals about incomplete reads.
pub fn usage_source_messages<'a>(sources: impl IntoIterator<Item = &'a UsageSource>) -> Vec<String> {
    let mut messages: Vec<String> = Vec::new();
    let add = |message: String, messages: &mut Vec<String>| {
        if !messages.contains(&message) {
            messages.push(message);
        }
    };
    let mut malformed: Vec<((String, UsageProvider, String, String), u64)> = Vec::new();
    for source in sources {
        let relevant = matches!(source.status, UsageSourceStatus::Partial | UsageSourceStatus::Failed)
            || source.fingerprint.provider == UsageProvider::Cursor
            || (source.status == UsageSourceStatus::Missing && source.malformed_records > 0);
        if source.action.is_some() || !relevant {
            continue;
        }
        let mut message = source.message.clone();
        if source.malformed_records > 0 {
            let fingerprint = &source.fingerprint;
            let key = (fingerprint.host_id.clone(), fingerprint.provider, fingerprint.resolved_home_path.clone(), fingerprint.volume_id.clone());
            match malformed.iter_mut().find(|(known, _)| *known == key) {
                Some((_, count)) => *count = (*count).max(source.malformed_records),
                None => malformed.push((key, source.malformed_records)),
            }
            // Servers also append this warning to message; keep any read failure before it.
            let warning = format_malformed_usage_warning(source.malformed_records);
            if let Some(text) = &message
                && let Some(stripped) = text.strip_suffix(&warning)
            {
                message = Some(stripped.trim().to_owned());
            }
        }
        if let Some(message) = message.filter(|message| !message.is_empty()) {
            add(message, &mut messages);
        }
    }
    let total: u64 = malformed.iter().map(|(_, count)| count).sum();
    if total > 0 {
        add(format_malformed_usage_warning(total), &mut messages);
    }
    messages
}

/// `formatUsageContractMismatch`.
pub fn format_usage_contract_mismatch(environment_label: &str, mismatch: &ContractMismatch) -> String {
    match mismatch.direction {
        MismatchDirection::ServerBehind => format!("{environment_label} runs an older server version and is excluded from totals."),
        MismatchDirection::ClientBehind => {
            format!("This client is older than the server on {environment_label}; its usage is excluded from totals.")
        }
    }
}

/// `formatDayShort`: `2026-11-02` reads `Nov 2`.
pub fn format_day_short(day: &str) -> String {
    let mut parts = day.split('-').map(|part| part.parse::<i64>().ok());
    match (parts.next().flatten(), parts.next().flatten(), parts.next().flatten()) {
        (Some(_), Some(month), Some(day_of_month)) => {
            let name = usize::try_from(month - 1).ok().and_then(|index| MONTHS.get(index)).copied().unwrap_or("");
            format!("{name} {day_of_month}")
        }
        _ => day.to_owned(),
    }
}

fn parse_day(day: &str) -> Option<i64> {
    let mut parts = day.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    Some(days_from_civil(year, month, day))
}

fn format_day(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `enumerateDays`: every calendar day from `since` through `until`.
pub fn enumerate_days(since_day: &str, until_day: &str) -> Vec<String> {
    match (parse_day(since_day), parse_day(until_day)) {
        (Some(start), Some(end)) if end >= start => (start..=end).map(format_day).collect(),
        _ => Vec::new(),
    }
}

/// `enumerateHourStarts`: hour starts from `since` up to, not including, `until`.
pub fn enumerate_hour_starts(since_time: &str, until_time: &str) -> Vec<String> {
    match (parse_iso(since_time), parse_iso(until_time)) {
        (Some(start), Some(end)) if end > start => (0..).map(|step| start + step * HOUR_MS).take_while(|cursor| *cursor < end).map(format_iso).collect(),
        _ => Vec::new(),
    }
}

fn local_day(ms: Ms, zone: &dyn Zone) -> String {
    let local = to_local(ms, zone);
    format!("{:04}-{:02}-{:02}", local.year, local.month, local.day)
}

/// `makeWindow`: the past `days` calendar days in the viewer's zone through today, or the
/// past 24 hours to the minute for the hourly period.
pub fn make_window(days: u32, now_ms: Ms, resolution: UsageResolution, time_zone: &str, zone: &dyn Zone) -> UsageSummaryInput {
    let until_day = local_day(now_ms, zone);
    let base = UsageSummaryInput {
        since_day: until_day.clone(),
        until_day: until_day.clone(),
        time_zone: time_zone.to_owned(),
        resolution,
        since_time: None,
        until_time: None,
        await_refresh: false,
        model_filter: None,
    };
    if resolution == UsageResolution::Hour {
        let until = now_ms.div_euclid(MINUTE_MS) * MINUTE_MS;
        let since = until - 24 * HOUR_MS;
        return UsageSummaryInput {
            since_day: local_day(since, zone),
            until_day: local_day(until, zone),
            since_time: Some(format_iso(since)),
            until_time: Some(format_iso(until)),
            ..base
        };
    }
    // Calendar arithmetic, so a DST day still counts as one day.
    let start = parse_day(&until_day).map_or_else(|| until_day.clone(), |today| format_day(today - (i64::from(days) - 1)));
    UsageSummaryInput { since_day: start, ..base }
}

fn hour_label(hour: i64, hour12: bool) -> String {
    if hour12 {
        let (display, meridiem) = match hour {
            0 => (12, "AM"),
            1..=11 => (hour, "AM"),
            12 => (12, "PM"),
            _ => (hour - 12, "PM"),
        };
        format!("{display} {meridiem}")
    } else {
        format!("{hour:02}:00")
    }
}

fn zone_name(offset_seconds: i64) -> String {
    let minutes = offset_seconds / 60;
    let sign = if minutes < 0 { '-' } else { '+' };
    let (hours, rest) = (minutes.abs() / 60, minutes.abs() % 60);
    if rest == 0 { format!("GMT{sign}{hours}") } else { format!("GMT{sign}{hours}:{rest:02}") }
}

/// `formatHourShort`: `3 PM` or `15:00`. A wall-clock hour a fall-back transition repeats
/// carries its zone, so the two buckets stay distinguishable.
pub fn format_hour_short(hour_start: &str, zone: &dyn Zone, hour12: bool) -> String {
    let Some(instant) = parse_iso(hour_start) else { return hour_start.to_owned() };
    let wall = |ms: Ms| {
        let local = to_local(ms, zone);
        (local.year, local.month, local.day, local.hour)
    };
    let label = hour_label(to_local(instant, zone).hour, hour12);
    let repeated = [-HOUR_MS, HOUR_MS].iter().any(|offset| wall(instant + offset) == wall(instant));
    if repeated { format!("{label} {}", zone_name(zone.offset_seconds_at(instant))) } else { label }
}

/// `formatDateTimeShort`: `Nov 2, 3 PM` or `Nov 2, 15:00`.
pub fn format_date_time_short(instant: &str, zone: &dyn Zone, hour12: bool) -> String {
    let Some(ms) = parse_iso(instant) else { return instant.to_owned() };
    let local = to_local(ms, zone);
    let month = usize::try_from(local.month - 1).ok().and_then(|index| MONTHS.get(index)).copied().unwrap_or("");
    format!("{month} {}, {}", local.day, hour_label(local.hour, hour12))
}

/// `formatRelativeHourShort`: `3 PM today`, `3 PM yesterday`, else the date and hour.
pub fn format_relative_hour_short(hour_start: &str, relative_to: &str, zone: &dyn Zone, hour12: bool) -> String {
    let (Some(instant), Some(reference)) = (parse_iso(hour_start), parse_iso(relative_to)) else {
        return format_date_time_short(hour_start, zone, hour12);
    };
    let day = |ms: Ms| {
        let local = to_local(ms, zone);
        days_from_civil(local.year, local.month, local.day)
    };
    match day(reference) - day(instant) {
        0 => format!("{} today", format_hour_short(hour_start, zone, hour12)),
        1 => format!("{} yesterday", format_hour_short(hour_start, zone, hour12)),
        _ => format_date_time_short(hour_start, zone, hour12),
    }
}

/// The window the header names: `Oct 4 to Nov 2`, or two date-hours for the past 24 hours.
pub fn window_label(window: &UsageSummaryInput, zone: &dyn Zone, hour12: bool) -> String {
    match (&window.since_time, &window.until_time) {
        (Some(since), Some(until)) if window.resolution == UsageResolution::Hour => {
            format!("{} to {}", format_date_time_short(since, zone, hour12), format_date_time_short(until, zone, hour12))
        }
        _ => format!("{} to {}", format_day_short(&window.since_day), format_day_short(&window.until_day)),
    }
}

/// The periods a window's chart and table walk: hour starts for hourly windows, else days.
pub fn window_periods(window: &UsageSummaryInput) -> Vec<String> {
    match (&window.since_time, &window.until_time, window.resolution) {
        (Some(since), Some(until), UsageResolution::Hour) => enumerate_hour_starts(since, until),
        _ => enumerate_days(&window.since_day, &window.until_day),
    }
}

/// Labels for periods, cached per call so a 90-day table formats each day once.
pub fn period_labels(periods: &[String], hourly: bool, zone: &dyn Zone, hour12: bool) -> HashMap<String, String> {
    periods
        .iter()
        .map(|period| {
            let label = if hourly { format_hour_short(period, zone, hour12) } else { format_day_short(period) };
            (period.clone(), label)
        })
        .collect()
}
