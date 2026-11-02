//! The quit shortcut under the "Quit confirmation" setting: `makeQuitShortcutHandler` in
//! `apps/desktop/src/window/QuitHold.ts`, minus the async settings read (the mode is passed in).
//! Times are milliseconds on any monotonic clock; the caller schedules `tick` at `deadline`.

use crate::client_settings::QuitConfirmationMode;

pub const HOLD_DURATION_MS: u64 = 1200;
pub const DOUBLE_PRESS_MS: u64 = 500;
/// Auto-repeat keydowns prove the hold: macOS drops a letter's keyUp while Command is down, so
/// once held, quitting waits for the release or a quiet period so trailing repeats are swallowed.
const RELEASE_GRACE_MS: u64 = 600;
const REPEAT_CADENCE_MULTIPLIER: u64 = 2;

/// Which hint the overlay shows (`QuitHoldOverlay.tsx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitHint {
    Hold,
    DoublePress,
}

impl QuitHint {
    pub fn message(self, shortcut: &str) -> String {
        match self {
            QuitHint::Hold => format!("Hold {shortcut} or press twice to quit"),
            QuitHint::DoublePress => format!("Press {shortcut} again to quit"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitEffect {
    Quit,
    ShowHint(QuitHint),
    HideHint,
}

/// One keydown. `modifier` is Command on macOS, Control elsewhere.
#[derive(Debug, Clone, Copy)]
pub struct QuitKeyDown<'a> {
    pub key: &'a str,
    pub modifier: bool,
    pub alt: bool,
    pub shift: bool,
    pub repeat: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deadline {
    Release,
    Quit,
}

#[derive(Debug, Default)]
pub struct QuitShortcut {
    holding: bool,
    mode: Option<QuitHint>,
    notified: bool,
    armed: bool,
    quit_on_release: bool,
    held_since: u64,
    last_press_at: Option<u64>,
    last_repeat_at: Option<u64>,
    repeat_cadence: u64,
    deadline: Option<(u64, Deadline)>,
}

impl QuitShortcut {
    pub fn deadline(&self) -> Option<u64> {
        self.deadline.map(|(at, _)| at)
    }

    fn release(&mut self, keep_double_press_hint: bool) -> Vec<QuitEffect> {
        if !self.holding && !self.notified {
            return Vec::new();
        }
        let keep = keep_double_press_hint && self.mode == Some(QuitHint::DoublePress) && self.notified;
        self.holding = false;
        self.armed = false;
        self.quit_on_release = false;
        self.last_repeat_at = None;
        self.repeat_cadence = 0;
        if keep {
            return Vec::new();
        }
        self.mode = None;
        self.deadline = None;
        if !self.notified {
            return Vec::new();
        }
        self.notified = false;
        vec![QuitEffect::HideHint]
    }

    fn quit_now(&mut self) -> Vec<QuitEffect> {
        let mut effects = self.release(false);
        self.last_press_at = None;
        effects.push(QuitEffect::Quit);
        effects
    }

    fn quit_after_quiet_period(&mut self, now: u64) {
        let quiet = RELEASE_GRACE_MS.max(self.repeat_cadence * REPEAT_CADENCE_MULTIPLIER);
        self.deadline = Some((now + quiet, Deadline::Quit));
    }

    /// Returns whether the key was the quit shortcut (and so must not reach anything else).
    pub fn key_down(&mut self, input: QuitKeyDown, mode: QuitConfirmationMode, now: u64) -> (bool, Vec<QuitEffect>) {
        let is_q = input.key.eq_ignore_ascii_case("q");
        if input.repeat && input.modifier && is_q {
            self.repeat_cadence = now.saturating_sub(self.last_repeat_at.unwrap_or(self.held_since));
            self.last_repeat_at = Some(now);
        }
        if self.quit_on_release {
            if is_q {
                self.quit_after_quiet_period(now);
            }
            return (true, Vec::new());
        }
        if !input.modifier || input.alt || input.shift || !is_q {
            // A fresh keydown also ends a hold whose keyUp macOS never sent.
            if input.repeat {
                return (false, Vec::new());
            }
            self.last_press_at = None;
            return (false, self.release(false));
        }
        if input.repeat {
            if self.mode == Some(QuitHint::Hold) && self.armed && now.saturating_sub(self.held_since) >= HOLD_DURATION_MS {
                self.armed = false;
                self.quit_on_release = true;
                self.quit_after_quiet_period(now);
            }
            return (true, Vec::new());
        }

        let previous = self.last_press_at.replace(now);
        let mut effects = if self.holding || self.notified { self.release(false) } else { Vec::new() };
        if previous.is_some_and(|at| now.saturating_sub(at) <= DOUBLE_PRESS_MS) {
            effects.extend(self.quit_now());
            return (true, effects);
        }
        self.holding = true;
        self.held_since = now;
        let (hint, window) = match mode {
            QuitConfirmationMode::Direct => {
                effects.extend(self.quit_now());
                return (true, effects);
            }
            QuitConfirmationMode::DoubleClick => (QuitHint::DoublePress, DOUBLE_PRESS_MS),
            QuitConfirmationMode::Hold => {
                self.armed = true;
                (QuitHint::Hold, HOLD_DURATION_MS + RELEASE_GRACE_MS)
            }
        };
        self.mode = Some(hint);
        self.notified = true;
        self.deadline = Some((now + window, Deadline::Release));
        effects.push(QuitEffect::ShowHint(hint));
        (true, effects)
    }

    /// The shortcut's letter came up.
    pub fn key_up(&mut self) -> Vec<QuitEffect> {
        let quit = self.quit_on_release;
        let mut effects = self.release(true);
        if quit {
            effects.push(QuitEffect::Quit);
        }
        effects
    }

    /// The modifier came up.
    pub fn modifier_released(&mut self, now: u64) -> Vec<QuitEffect> {
        if self.quit_on_release {
            self.quit_after_quiet_period(now);
            return Vec::new();
        }
        self.release(true)
    }

    pub fn tick(&mut self, now: u64) -> Vec<QuitEffect> {
        match self.deadline {
            Some((at, kind)) if now >= at => {
                self.deadline = None;
                match kind {
                    Deadline::Release => self.release(false),
                    Deadline::Quit => self.quit_now(),
                }
            }
            _ => Vec::new(),
        }
    }
}
