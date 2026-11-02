//! Snooze menu options: `SnoozePresetSetting` in `contracts/src/settings.ts`, the labels and ids
//! from `client-runtime/src/state/threadSettled.ts`, and the editor rules in
//! `web/src/components/settings/SnoozePresetsSettings.tsx` and `SnoozePresetsSettings.logic.ts`.
//! Wall-clock resolution (which day a
//! weekday lands on, whether today's time passed) takes the local weekday and minute as inputs.

use serde::{Deserialize, Serialize};

use crate::sidebar::settled::{PresetDay, SnoozePresetSetting};

pub const MAX_PRESETS: usize = 8;
pub const MAX_PRESET_MINUTES: u32 = 30 * 24 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnoozeDay {
    Today,
    Tomorrow,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl SnoozeDay {
    pub const ALL: [SnoozeDay; 9] = [
        SnoozeDay::Today,
        SnoozeDay::Tomorrow,
        SnoozeDay::Monday,
        SnoozeDay::Tuesday,
        SnoozeDay::Wednesday,
        SnoozeDay::Thursday,
        SnoozeDay::Friday,
        SnoozeDay::Saturday,
        SnoozeDay::Sunday,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SnoozeDay::Today => "today",
            SnoozeDay::Tomorrow => "tomorrow",
            SnoozeDay::Monday => "monday",
            SnoozeDay::Tuesday => "tuesday",
            SnoozeDay::Wednesday => "wednesday",
            SnoozeDay::Thursday => "thursday",
            SnoozeDay::Friday => "friday",
            SnoozeDay::Saturday => "saturday",
            SnoozeDay::Sunday => "sunday",
        }
    }

    /// `WHEN_LABELS`.
    pub fn label(self) -> &'static str {
        match self {
            SnoozeDay::Today => "Today",
            SnoozeDay::Tomorrow => "Tomorrow",
            SnoozeDay::Monday => "Monday",
            SnoozeDay::Tuesday => "Tuesday",
            SnoozeDay::Wednesday => "Wednesday",
            SnoozeDay::Thursday => "Thursday",
            SnoozeDay::Friday => "Friday",
            SnoozeDay::Saturday => "Saturday",
            SnoozeDay::Sunday => "Sunday",
        }
    }

    /// `WEEKDAY_INDEX`, Sunday first.
    fn weekday_index(self) -> Option<u32> {
        match self {
            SnoozeDay::Sunday => Some(0),
            SnoozeDay::Monday => Some(1),
            SnoozeDay::Tuesday => Some(2),
            SnoozeDay::Wednesday => Some(3),
            SnoozeDay::Thursday => Some(4),
            SnoozeDay::Friday => Some(5),
            SnoozeDay::Saturday => Some(6),
            SnoozeDay::Today | SnoozeDay::Tomorrow => None,
        }
    }
}

/// `after` wakes a fixed time from now; `at` wakes at a local time, a weekday being its next
/// occurrence after today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SnoozePreset {
    After { minutes: u32 },
    At { day: SnoozeDay, time: String },
}

impl SnoozePreset {
    pub fn is_valid(&self) -> bool {
        match self {
            SnoozePreset::After { minutes } => (1..=MAX_PRESET_MINUTES).contains(minutes),
            SnoozePreset::At { time, .. } => parse_time(time).is_some(),
        }
    }

    /// `snoozePresetId`.
    pub fn id(&self) -> String {
        match self {
            SnoozePreset::After { minutes } => format!("after:{minutes}"),
            SnoozePreset::At { day, time } => format!("at:{}:{time}", day.as_str()),
        }
    }

    /// `snoozePresetDaysAhead` for `at` presets, given today's weekday (0 = Sunday).
    pub fn days_ahead(&self, weekday: u32) -> u32 {
        match self {
            SnoozePreset::After { .. } => 0,
            SnoozePreset::At { day: SnoozeDay::Today, .. } => 0,
            SnoozePreset::At { day: SnoozeDay::Tomorrow, .. } => 1,
            SnoozePreset::At { day, .. } => {
                let target = day.weekday_index().unwrap_or(0);
                match (target + 7 - weekday % 7) % 7 {
                    0 => 7,
                    days => days,
                }
            }
        }
    }

    /// `snoozePresetLabel`: labels follow the actual day gap, so a Monday option on a Sunday
    /// reads "Tomorrow".
    pub fn label(&self, days_ahead: u32) -> String {
        match self {
            SnoozePreset::After { minutes } => format!("In {}", format_duration(*minutes)),
            SnoozePreset::At { time, .. } if days_ahead == 0 => {
                let hour = parse_time(time).map(|(hour, _)| hour).unwrap_or(0);
                match hour {
                    0..=11 => "This morning",
                    12..=16 => "This afternoon",
                    _ => "This evening",
                }
                .to_owned()
            }
            SnoozePreset::At { .. } if days_ahead == 1 => "Tomorrow".to_owned(),
            SnoozePreset::At { day: SnoozeDay::Monday, .. } => "Next week".to_owned(),
            SnoozePreset::At { day, .. } if days_ahead == 7 => format!("Next {}", day.label()),
            SnoozePreset::At { day, .. } => day.label().to_owned(),
        }
    }

    /// Whether the menu leaves this option out right now: a today option whose time has passed.
    pub fn passed(&self, now_minute_of_day: u32) -> bool {
        match self {
            SnoozePreset::At { day: SnoozeDay::Today, time } => {
                parse_time(time).is_none_or(|(hour, minute)| hour * 60 + minute <= now_minute_of_day)
            }
            _ => false,
        }
    }
}

/// `DEFAULT_SNOOZE_PRESETS`.
pub fn default_presets() -> Vec<SnoozePreset> {
    vec![
        SnoozePreset::After { minutes: 60 },
        SnoozePreset::After { minutes: 180 },
        SnoozePreset::At { day: SnoozeDay::Today, time: "18:00".into() },
        SnoozePreset::At { day: SnoozeDay::Tomorrow, time: "09:00".into() },
        SnoozePreset::At { day: SnoozeDay::Monday, time: "09:00".into() },
    ]
}

/// `nextCandidate`: offered in order when adding, skipping any already in the list.
pub fn next_candidate(presets: &[SnoozePreset]) -> SnoozePreset {
    let used: Vec<String> = presets.iter().map(SnoozePreset::id).collect();
    default_presets()
        .into_iter()
        .chain([
            SnoozePreset::After { minutes: 30 },
            SnoozePreset::At { day: SnoozeDay::Saturday, time: "09:00".into() },
            SnoozePreset::After { minutes: 24 * 60 },
        ])
        .find(|candidate| !used.contains(&candidate.id()))
        .unwrap_or(SnoozePreset::After { minutes: 60 })
}

/// `sameSnoozePresets`: row order is display order only, so it does not differ from the defaults.
pub fn same_presets(left: &[SnoozePreset], right: &[SnoozePreset]) -> bool {
    let ids = |presets: &[SnoozePreset]| {
        let mut ids: Vec<String> = presets.iter().map(SnoozePreset::id).collect();
        ids.sort();
        ids
    };
    ids(left) == ids(right)
}

/// `formatSnoozeDuration`: the largest unit that divides evenly.
pub fn format_duration(minutes: u32) -> String {
    let (count, unit) = split_minutes(minutes);
    let unit = unit.singular();
    if count == 1 { format!("1 {unit}") } else { format!("{count} {unit}s") }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationUnit {
    Minutes,
    Hours,
    Days,
}

impl DurationUnit {
    pub const ALL: [DurationUnit; 3] = [DurationUnit::Minutes, DurationUnit::Hours, DurationUnit::Days];

    pub fn minutes(self) -> u32 {
        match self {
            DurationUnit::Minutes => 1,
            DurationUnit::Hours => 60,
            DurationUnit::Days => 24 * 60,
        }
    }

    pub fn singular(self) -> &'static str {
        match self {
            DurationUnit::Minutes => "minute",
            DurationUnit::Hours => "hour",
            DurationUnit::Days => "day",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            DurationUnit::Minutes => "minutes",
            DurationUnit::Hours => "hours",
            DurationUnit::Days => "days",
        }
    }
}

/// `splitMinutes`.
pub fn split_minutes(minutes: u32) -> (u32, DurationUnit) {
    if minutes > 0 && minutes.is_multiple_of(DurationUnit::Days.minutes()) {
        (minutes / DurationUnit::Days.minutes(), DurationUnit::Days)
    } else if minutes > 0 && minutes.is_multiple_of(DurationUnit::Hours.minutes()) {
        (minutes / DurationUnit::Hours.minutes(), DurationUnit::Hours)
    } else {
        (minutes, DurationUnit::Minutes)
    }
}

/// Steps an `after` preset's amount in its unit, clamped to 1..=max (`DurationFields`).
pub fn step_amount(minutes: u32, delta: i32) -> u32 {
    let (amount, unit) = split_minutes(minutes);
    let max = MAX_PRESET_MINUTES / unit.minutes();
    let next = (i64::from(amount) + i64::from(delta)).clamp(1, i64::from(max));
    next as u32 * unit.minutes()
}

/// Switching the unit keeps the amount, clamped to the longest allowed snooze.
pub fn with_unit(minutes: u32, unit: DurationUnit) -> u32 {
    let (amount, _) = split_minutes(minutes);
    (amount * unit.minutes()).min(MAX_PRESET_MINUTES)
}

/// `withWhen`: "In" keeps or restores a duration, a day keeps or restores a time.
pub fn with_when(preset: &SnoozePreset, day: Option<SnoozeDay>, memory_minutes: u32, memory_time: &str) -> SnoozePreset {
    match (day, preset) {
        (None, SnoozePreset::After { .. }) => preset.clone(),
        (None, SnoozePreset::At { .. }) => SnoozePreset::After { minutes: memory_minutes },
        (Some(day), SnoozePreset::At { time, .. }) => SnoozePreset::At { day, time: time.clone() },
        (Some(day), SnoozePreset::After { .. }) => SnoozePreset::At { day, time: memory_time.to_owned() },
    }
}

/// The saved options as the sidebar's snooze menus resolve them (`Sidebar.snooze.ts`).
pub fn menu_settings(presets: &[SnoozePreset]) -> Vec<SnoozePresetSetting> {
    presets
        .iter()
        .filter_map(|preset| match preset {
            SnoozePreset::After { minutes } => Some(SnoozePresetSetting::After { minutes: i64::from(*minutes) }),
            SnoozePreset::At { day, time } => {
                let (hour, minute) = parse_time(time)?;
                let day = match day {
                    SnoozeDay::Today => PresetDay::Today,
                    SnoozeDay::Tomorrow => PresetDay::Tomorrow,
                    weekday => PresetDay::Weekday(i64::from(weekday.weekday_index().unwrap_or(0))),
                };
                Some(SnoozePresetSetting::At { day, hour: i64::from(hour), minute: i64::from(minute) })
            }
        })
        .collect()
}

/// The editor's local copy of the options (`SnoozePresetsRow`). Each edit builds on the draft
/// and is saved whole, so a second edit before the first save echoes back keeps the first.
/// Echoes of our own saves keep the draft; any other saved value replaces it.
#[derive(Debug, Clone, PartialEq)]
pub struct SnoozeDraft {
    presets: Vec<SnoozePreset>,
    written: Vec<Vec<SnoozePreset>>,
    /// `None` forces the next observation to adopt the saved value, as after a failed save.
    saved_source: Option<Vec<SnoozePreset>>,
}

impl SnoozeDraft {
    pub fn new(saved: &[SnoozePreset]) -> Self {
        SnoozeDraft { presets: saved.to_vec(), written: Vec::new(), saved_source: Some(saved.to_vec()) }
    }

    pub fn presets(&self) -> &[SnoozePreset] {
        &self.presets
    }

    /// Takes in the environment's saved options (`reconcileSnoozeDraft`).
    pub fn observe(&mut self, saved: &[SnoozePreset]) {
        if self.saved_source.as_deref() == Some(saved) {
            return;
        }
        self.saved_source = Some(saved.to_vec());
        match self.written.iter().position(|snapshot| snapshot.as_slice() == saved) {
            Some(acknowledged) => {
                self.written.drain(..=acknowledged);
            }
            None => {
                self.written.clear();
                self.presets = saved.to_vec();
            }
        }
    }

    /// Replaces the draft. Returns false when nothing changed and there is nothing to save.
    pub fn edit(&mut self, next: Vec<SnoozePreset>) -> bool {
        if next == self.presets {
            return false;
        }
        self.written.push(next.clone());
        self.presets = next;
        true
    }

    pub fn failed(&mut self) {
        self.written.clear();
        self.saved_source = None;
    }
}

/// `HH:MM`, 24-hour, as the contract's pattern accepts.
pub fn parse_time(time: &str) -> Option<(u32, u32)> {
    let (hour, minute) = time.split_once(':')?;
    if hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let (hour, minute) = (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?);
    (hour < 24 && minute < 60).then_some((hour, minute))
}
