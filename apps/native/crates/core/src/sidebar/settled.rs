//! Snooze and settle rules (`client-runtime/src/state/threadSettled.ts`) and the snooze
//! presets from `packages/contracts/src/settings.ts` (`DEFAULT_SNOOZE_PRESETS`).

use super::thread::SidebarThread;
use super::time::{DAY_MS, HOUR_MS, LocalTime, MINUTE_MS, Ms, Zone, from_local, to_local};

/// Failed starts can leave user messages unadopted; bound the pending window.
pub const QUEUED_TURN_START_GRACE_MS: Ms = 2 * MINUTE_MS;

pub fn has_queued_turn_start(thread: &SidebarThread, now: Ms) -> bool {
    if let Some(runtime) = &thread.runtime
        && matches!(runtime.status.as_str(), "preparing" | "queued" | "starting")
    {
        return true;
    }
    let Some(message_at) = thread.latest_user_message_at else { return false };
    // Message timestamps come from the sending device, so skew is bounded on both sides.
    if (now - message_at).abs() > QUEUED_TURN_START_GRACE_MS {
        return false;
    }
    let Some(run) = &thread.latest_run else { return true };
    [run.requested_at, run.started_at, run.completed_at]
        .into_iter()
        .all(|candidate| candidate.is_none_or(|at| at < message_at))
}

fn runtime_failed(thread: &SidebarThread) -> bool {
    thread.runtime.as_ref().is_some_and(|runtime| matches!(runtime.status.as_str(), "error" | "failed"))
}

/// `Some(None)` = failed with no usable time; `None` = not failed.
fn failure_edge_at(thread: &SidebarThread) -> Option<Option<Ms>> {
    if !runtime_failed(thread) {
        return None;
    }
    if let Some(run) = &thread.latest_run
        && run.status == "failed"
        && run.completed_at.is_some()
    {
        return Some(run.completed_at);
    }
    Some(thread.runtime.as_ref().and_then(|runtime| runtime.updated_at))
}

/// Only a request for the user or a fresh failure ends a snooze early.
pub fn raised_hand_while_snoozed(thread: &SidebarThread) -> bool {
    if thread.has_pending_approvals || thread.has_pending_user_input || thread.has_pending_runtime_request {
        return true;
    }
    if thread.pending_subagent_request.is_some() || thread.pending_guest_message_count > 0 {
        return true;
    }
    match failure_edge_at(thread) {
        None => false,
        Some(failed_at) => match thread.snoozed_at {
            None => true,
            Some(snoozed_at) => failed_at.is_some_and(|at| at > snoozed_at),
        },
    }
}

pub fn can_snooze(thread: &SidebarThread, now: Ms) -> bool {
    !(thread.has_pending_approvals
        || thread.has_pending_user_input
        || thread.has_pending_runtime_request
        || thread.pending_subagent_request.is_some()
        || thread.pending_guest_message_count > 0
        || has_queued_turn_start(thread, now))
}

pub fn effective_snoozed(thread: &SidebarThread, now: Ms) -> bool {
    match thread.snoozed_until {
        Some(wake_at) if wake_at > now => !raised_hand_while_snoozed(thread),
        _ => false,
    }
}

pub fn woke_at(thread: &SidebarThread, now: Ms) -> Option<Ms> {
    let wake_at = thread.snoozed_until?;
    if raised_hand_while_snoozed(thread) {
        if let Some(Some(failed_at)) = failure_edge_at(thread)
            && thread.snoozed_at.is_none_or(|snoozed_at| failed_at > snoozed_at)
        {
            return Some(failed_at);
        }
        return thread.runtime.as_ref().and_then(|runtime| runtime.updated_at).or(thread.snoozed_at);
    }
    (wake_at <= now).then_some(wake_at)
}

/// `snoozeWakeLabel`.
pub fn snooze_wake_label(snoozed_until: Ms, now: Ms) -> String {
    let remaining = snoozed_until - now;
    if remaining <= 0 {
        "now".to_owned()
    } else if remaining < HOUR_MS {
        format!("{}m", ((remaining + MINUTE_MS - 1) / MINUTE_MS).max(1))
    } else if remaining < DAY_MS {
        format!("{}h", (remaining + HOUR_MS - 1) / HOUR_MS)
    } else {
        format!("{}d", (remaining + DAY_MS - 1) / DAY_MS)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetDay {
    Today,
    Tomorrow,
    /// 0 = Sunday.
    Weekday(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnoozePresetSetting {
    After { minutes: i64 },
    At { day: PresetDay, hour: i64, minute: i64 },
}

pub const DEFAULT_SNOOZE_PRESETS: [SnoozePresetSetting; 5] = [
    SnoozePresetSetting::After { minutes: 60 },
    SnoozePresetSetting::After { minutes: 180 },
    SnoozePresetSetting::At { day: PresetDay::Today, hour: 18, minute: 0 },
    SnoozePresetSetting::At { day: PresetDay::Tomorrow, hour: 9, minute: 0 },
    SnoozePresetSetting::At { day: PresetDay::Weekday(1), hour: 9, minute: 0 },
];

const WEEKDAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];
const SHORT_WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnoozePreset {
    pub id: String,
    pub label: String,
    pub when_label: String,
    pub snoozed_until: Ms,
}

pub fn snooze_preset_id(setting: &SnoozePresetSetting) -> String {
    match setting {
        SnoozePresetSetting::After { minutes } => format!("after:{minutes}"),
        SnoozePresetSetting::At { day, hour, minute } => {
            let day = match day {
                PresetDay::Today => "today",
                PresetDay::Tomorrow => "tomorrow",
                PresetDay::Weekday(index) => WEEKDAYS[index.rem_euclid(7) as usize],
            };
            format!("at:{day}:{hour:02}:{minute:02}")
        }
    }
}

fn pluralize(count: i64, unit: &str) -> String {
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

fn format_snooze_duration(minutes: i64) -> String {
    if minutes % (24 * 60) == 0 {
        pluralize(minutes / (24 * 60), "day")
    } else if minutes % 60 == 0 {
        pluralize(minutes / 60, "hour")
    } else {
        pluralize(minutes, "minute")
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The `toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })` shape in en-US.
pub fn default_time_label(time: LocalTime) -> String {
    let suffix = if time.hour < 12 { "AM" } else { "PM" };
    let hour12 = match time.hour % 12 {
        0 => 12,
        other => other,
    };
    format!("{hour12}:{:02} {suffix}", time.minute)
}

pub type TimeFormat<'a> = &'a dyn Fn(LocalTime) -> String;

pub fn resolve_snooze_preset(setting: &SnoozePresetSetting, now: Ms, zone: &dyn Zone, format_time: TimeFormat) -> Option<SnoozePreset> {
    let local_now = to_local(now, zone);
    let (wake, label) = match *setting {
        SnoozePresetSetting::After { minutes } => (now + minutes * MINUTE_MS, format!("In {}", format_snooze_duration(minutes))),
        SnoozePresetSetting::At { day, hour, minute } => {
            let days_ahead = match day {
                PresetDay::Today => 0,
                PresetDay::Tomorrow => 1,
                PresetDay::Weekday(index) => match (index - local_now.weekday + 7) % 7 {
                    0 => 7,
                    ahead => ahead,
                },
            };
            let wake = from_local(local_now.year, local_now.month, local_now.day + days_ahead, hour, minute, zone);
            if wake <= now {
                return None;
            }
            let label = match (days_ahead, day) {
                (0, _) => (if hour < 12 { "This morning" } else if hour < 17 { "This afternoon" } else { "This evening" }).to_owned(),
                (1, _) => "Tomorrow".to_owned(),
                (_, PresetDay::Weekday(1)) => "Next week".to_owned(),
                (7, PresetDay::Weekday(index)) => format!("Next {}", capitalize(WEEKDAYS[index.rem_euclid(7) as usize])),
                (_, PresetDay::Weekday(index)) => capitalize(WEEKDAYS[index.rem_euclid(7) as usize]),
                _ => "Tomorrow".to_owned(),
            };
            (wake, label)
        }
    };
    let local_wake = to_local(wake, zone);
    let time = format_time(local_wake);
    let label_names_day = matches!(setting, SnoozePresetSetting::At { .. }) && label != "Next week";
    let same_day = (local_wake.year, local_wake.month, local_wake.day) == (local_now.year, local_now.month, local_now.day);
    Some(SnoozePreset {
        id: snooze_preset_id(setting),
        label,
        when_label: if label_names_day || same_day {
            time
        } else {
            format!("{} {time}", SHORT_WEEKDAYS[local_wake.weekday as usize])
        },
        snoozed_until: wake,
    })
}

/// The snooze menu: soonest first, one row per wake minute.
pub fn resolve_snooze_presets(settings: &[SnoozePresetSetting], now: Ms, zone: &dyn Zone, format_time: TimeFormat) -> Vec<SnoozePreset> {
    let mut presets: Vec<SnoozePreset> = settings.iter().filter_map(|setting| resolve_snooze_preset(setting, now, zone, format_time)).collect();
    let minute = |preset: &SnoozePreset| preset.snoozed_until.div_euclid(MINUTE_MS);
    presets.sort_by_key(minute);
    let mut seen = Vec::new();
    presets.retain(|preset| {
        let key = minute(preset);
        let fresh = !seen.contains(&key);
        seen.push(key);
        fresh
    });
    presets
}

/// A menu choice as picked: a duration re-resolves so it starts at selection; a set time keeps
/// the wake that was shown.
pub fn pick_snooze_preset(shown: &[SnoozePreset], id: &str, now: Ms, settings: &[SnoozePresetSetting], zone: &dyn Zone, format_time: TimeFormat) -> Option<SnoozePreset> {
    let ahead = shown.iter().find(|preset| preset.id == id && preset.snoozed_until > now).cloned();
    settings
        .iter()
        .find(|setting| matches!(setting, SnoozePresetSetting::After { .. }) && snooze_preset_id(setting) == id)
        .and_then(|setting| resolve_snooze_preset(setting, now, zone, format_time))
        .or(ahead)
}

/// Whether a woken thread woke after the user last saw it.
pub fn is_woke_unseen(woke_at: Option<Ms>, settled: bool, last_visited_at: Option<Ms>, latest_run_completed_at: Option<Ms>) -> bool {
    let Some(woke_at) = woke_at.filter(|_| !settled) else { return false };
    [last_visited_at, latest_run_completed_at].into_iter().flatten().all(|seen| seen < woke_at)
}
