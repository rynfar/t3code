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

/// Two-finger horizontal trackpad history swipe navigation (`HistorySwipeNavigation.tsx`).
/// macOS only at the UI layer: Windows/Linux tilt wheels are indistinguishable from swipes.
pub mod swipe {
    pub const SWIPE_TRIGGER_DISTANCE: f32 = 120.0;
    pub const SWIPE_AXIS_LOCK_DISTANCE: f32 = 8.0;
    pub const SWIPE_HORIZONTAL_DOMINANCE: f32 = 2.0;
    pub const SWIPE_DOMINANCE_DISTANCE: f32 = 30.0;
    pub const MOMENTUM_SAMPLE_SIZE: usize = 5;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum HistoryDirection {
        Back,
        Forward,
    }

    impl HistoryDirection {
        pub fn is_back(self) -> bool {
            self == HistoryDirection::Back
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SwipePhase {
        Started,
        Moved,
        Ended,
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct SwipeIndicator {
        pub direction: HistoryDirection,
        pub progress: f32,
        pub y: f32,
        pub tracking: bool,
        pub opacity: f32,
    }

    impl SwipeIndicator {
        pub fn armed(self) -> bool {
            self.progress >= 1.0
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct SwipeWheel {
        /// DOM-convention horizontal delta (`delta_x < 0` means fingers swiped right = Back).
        pub delta_x: f32,
        pub delta_y: f32,
        pub y: f32,
        pub phase: SwipePhase,
        pub is_pixel_delta: bool,
        pub ctrl_key: bool,
        pub blocked: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SwipeAction {
        None,
        Navigate(HistoryDirection),
    }

    #[derive(Debug, Clone)]
    enum Gesture {
        Pending {
            y: f32,
            sum_x: f32,
            sum_y: f32,
        },
        Tracking {
            direction: HistoryDirection,
            distance: f32,
            deltas: Vec<f32>,
            travel_x: f32,
            travel_y: f32,
            y: f32,
        },
        Ignored,
    }

    pub fn is_vertical_scroll(travel_x: f32, travel_y: f32) -> bool {
        travel_x + travel_y >= SWIPE_DOMINANCE_DISTANCE
            && travel_x <= SWIPE_HORIZONTAL_DOMINANCE * travel_y
    }

    /// macOS momentum after fingers lift decays by a near-constant ratio per event.
    pub fn is_momentum_tail(deltas: &[f32]) -> bool {
        if deltas.len() < MOMENTUM_SAMPLE_SIZE {
            return false;
        }
        let recent = &deltas[deltas.len() - MOMENTUM_SAMPLE_SIZE..];
        if recent.iter().any(|&delta| delta <= 0.0) {
            return false;
        }
        let mut min_ratio = f32::INFINITY;
        let mut max_ratio = f32::NEG_INFINITY;
        for pair in recent.windows(2) {
            let ratio = pair[1] / pair[0];
            if !(0.7..1.0).contains(&ratio) {
                return false;
            }
            min_ratio = min_ratio.min(ratio);
            max_ratio = max_ratio.max(ratio);
        }
        max_ratio - min_ratio < 0.1
    }

    #[derive(Debug, Default)]
    pub struct HistorySwipe {
        gesture: Option<Gesture>,
        indicator: Option<SwipeIndicator>,
    }

    impl HistorySwipe {
        pub fn indicator(&self) -> Option<SwipeIndicator> {
            self.indicator
        }

        pub fn clear_indicator(&mut self) {
            if self.indicator.is_some_and(|i| !i.tracking) {
                self.indicator = None;
            }
        }

        pub fn set_indicator_opacity(&mut self, opacity: f32) {
            if let Some(indicator) = &mut self.indicator
                && !indicator.tracking
            {
                indicator.opacity = opacity.clamp(0.0, 1.0);
            }
        }

        pub fn reset(&mut self) {
            self.gesture = None;
            self.indicator = None;
        }

        fn hide_indicator(&mut self) {
            if let Some(indicator) = &mut self.indicator {
                indicator.tracking = false;
            }
        }

        fn release(&mut self) -> SwipeAction {
            let action = match &self.gesture {
                Some(Gesture::Tracking { direction, distance, .. })
                    if *distance >= SWIPE_TRIGGER_DISTANCE =>
                {
                    SwipeAction::Navigate(*direction)
                }
                _ => SwipeAction::None,
            };
            self.gesture = Some(Gesture::Ignored);
            self.hide_indicator();
            action
        }

        pub fn on_scroll_begin(&mut self) {
            if matches!(self.gesture, Some(Gesture::Tracking { .. })) {
                self.hide_indicator();
            }
            self.gesture = None;
        }

        pub fn on_scroll_end(&mut self) -> SwipeAction {
            let action = if matches!(self.gesture, Some(Gesture::Tracking { .. })) {
                self.release()
            } else {
                SwipeAction::None
            };
            // Stay `Ignored` until the next `SwipePhase::Started` so post-lift AppKit momentum
            // events (`NSEventPhaseNone` -> `TouchPhase::Moved`) cannot start a new gesture.
            self.gesture = Some(Gesture::Ignored);
            action
        }

        pub fn on_wheel(
            &mut self,
            event: SwipeWheel,
            can_navigate: impl Fn(HistoryDirection) -> bool,
        ) -> SwipeAction {
            match event.phase {
                SwipePhase::Started => self.on_scroll_begin(),
                SwipePhase::Ended => return self.on_scroll_end(),
                SwipePhase::Moved => {}
            }

            if self.gesture.is_none() {
                self.gesture = Some(if event.ctrl_key || !event.is_pixel_delta || event.blocked {
                    Gesture::Ignored
                } else {
                    Gesture::Pending { y: event.y, sum_x: 0.0, sum_y: 0.0 }
                });
            }

            let next_gesture = match self.gesture.take() {
                Some(Gesture::Ignored) | None => {
                    self.gesture = Some(Gesture::Ignored);
                    return SwipeAction::None;
                }
                Some(Gesture::Pending { y, mut sum_x, mut sum_y }) => {
                    if event.blocked {
                        self.gesture = Some(Gesture::Ignored);
                        return SwipeAction::None;
                    }
                    sum_x += event.delta_x;
                    sum_y += event.delta_y;
                    if sum_x.abs() + sum_y.abs() < SWIPE_AXIS_LOCK_DISTANCE {
                        self.gesture = Some(Gesture::Pending { y, sum_x, sum_y });
                        return SwipeAction::None;
                    }
                    if sum_x.abs() <= sum_y.abs() || is_vertical_scroll(sum_x.abs(), sum_y.abs()) {
                        self.gesture = Some(Gesture::Ignored);
                        return SwipeAction::None;
                    }
                    let direction = if sum_x < 0.0 {
                        HistoryDirection::Back
                    } else {
                        HistoryDirection::Forward
                    };
                    if !can_navigate(direction) {
                        self.gesture = Some(Gesture::Ignored);
                        return SwipeAction::None;
                    }
                    Gesture::Tracking {
                        direction,
                        distance: sum_x.abs(),
                        deltas: Vec::new(),
                        travel_x: sum_x.abs(),
                        travel_y: sum_y.abs(),
                        y,
                    }
                }
                Some(Gesture::Tracking {
                    direction,
                    mut distance,
                    mut deltas,
                    mut travel_x,
                    mut travel_y,
                    y,
                }) => {
                    if event.blocked {
                        self.gesture = Some(Gesture::Ignored);
                        self.hide_indicator();
                        return SwipeAction::None;
                    }
                    let delta = match direction {
                        HistoryDirection::Back => -event.delta_x,
                        HistoryDirection::Forward => event.delta_x,
                    };
                    if event.delta_x == 0.0 && event.delta_y == 0.0 {
                        self.gesture = Some(Gesture::Tracking {
                            direction,
                            distance,
                            deltas,
                            travel_x,
                            travel_y,
                            y,
                        });
                        return self.release();
                    }
                    travel_x += event.delta_x.abs();
                    travel_y += event.delta_y.abs();
                    if is_vertical_scroll(travel_x, travel_y) {
                        self.gesture = Some(Gesture::Ignored);
                        self.hide_indicator();
                        return SwipeAction::None;
                    }
                    if deltas.len() >= MOMENTUM_SAMPLE_SIZE {
                        deltas.remove(0);
                    }
                    deltas.push(delta);
                    if is_momentum_tail(&deltas) {
                        self.gesture = Some(Gesture::Tracking {
                            direction,
                            distance,
                            deltas,
                            travel_x,
                            travel_y,
                            y,
                        });
                        return self.release();
                    }
                    distance = (distance + delta).max(0.0);
                    Gesture::Tracking { direction, distance, deltas, travel_x, travel_y, y }
                }
            };

            if let Gesture::Tracking { direction, distance, y, .. } = &next_gesture {
                self.indicator = Some(SwipeIndicator {
                    direction: *direction,
                    progress: (*distance / SWIPE_TRIGGER_DISTANCE).min(1.0),
                    y: *y,
                    tracking: true,
                    opacity: 1.0,
                });
            }
            self.gesture = Some(next_gesture);
            SwipeAction::None
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn wheel(delta_x: f32, delta_y: f32, phase: SwipePhase) -> SwipeWheel {
            SwipeWheel {
                delta_x,
                delta_y,
                y: 200.0,
                phase,
                is_pixel_delta: true,
                ctrl_key: false,
                blocked: false,
            }
        }

        #[test]
        fn horizontal_swipe_back_triggers_on_release_past_threshold() {
            let mut swipe = HistorySwipe::default();
            assert_eq!(swipe.on_wheel(wheel(-10.0, 0.0, SwipePhase::Started), |_| true), SwipeAction::None);
            assert_eq!(swipe.indicator().map(|i| i.direction), Some(HistoryDirection::Back));
            assert!(!swipe.indicator().unwrap().armed());

            assert_eq!(swipe.on_wheel(wheel(-115.0, 1.0, SwipePhase::Moved), |_| true), SwipeAction::None);
            assert!(swipe.indicator().unwrap().armed());
            assert_eq!(swipe.on_wheel(wheel(0.0, 0.0, SwipePhase::Ended), |_| true), SwipeAction::Navigate(HistoryDirection::Back));
            assert!(!swipe.indicator().unwrap().tracking);
        }

        #[test]
        fn forward_swipe_below_threshold_does_not_navigate() {
            let mut swipe = HistorySwipe::default();
            swipe.on_wheel(wheel(40.0, 0.0, SwipePhase::Started), |_| true);
            swipe.on_wheel(wheel(40.0, 0.0, SwipePhase::Moved), |_| true);
            assert_eq!(swipe.on_wheel(wheel(0.0, 0.0, SwipePhase::Ended), |_| true), SwipeAction::None);
        }

        #[test]
        fn vertical_scroll_and_unavailable_direction_and_blocked_overlay_are_ignored() {
            let mut swipe = HistorySwipe::default();
            swipe.on_wheel(wheel(-5.0, 15.0, SwipePhase::Started), |_| true);
            swipe.on_wheel(wheel(-150.0, 0.0, SwipePhase::Moved), |_| true);
            assert!(swipe.indicator().is_none());

            let mut swipe = HistorySwipe::default();
            swipe.on_wheel(wheel(-20.0, 0.0, SwipePhase::Started), |dir| dir == HistoryDirection::Forward);
            assert!(swipe.indicator().is_none());

            let mut swipe = HistorySwipe::default();
            let mut ev = wheel(-30.0, 0.0, SwipePhase::Started);
            ev.blocked = true;
            swipe.on_wheel(ev, |_| true);
            assert!(swipe.indicator().is_none());
        }

        #[test]
        fn momentum_tail_releases_early_and_ignores_trailing_events() {
            assert!(is_momentum_tail(&[20.0, 17.0, 14.45, 12.28, 10.44]));
            assert!(!is_momentum_tail(&[20.0, 22.0, 18.0, 25.0, 19.0]));
            assert!(!is_momentum_tail(&[20.0, 17.0, 14.0]));

            let mut swipe = HistorySwipe::default();
            swipe.on_wheel(wheel(-120.0, 0.0, SwipePhase::Started), |_| true);
            let decay = [20.0_f32, 17.0, 14.45, 12.28, 10.44];
            let mut triggered = SwipeAction::None;
            for d in decay {
                let act = swipe.on_wheel(wheel(-d, 0.0, SwipePhase::Moved), |_| true);
                if act != SwipeAction::None {
                    triggered = act;
                }
            }
            assert_eq!(triggered, SwipeAction::Navigate(HistoryDirection::Back));
            // Trailing momentum events after release do not navigate again.
            assert_eq!(swipe.on_wheel(wheel(-8.0, 0.0, SwipePhase::Moved), |_| true), SwipeAction::None);
            assert_eq!(swipe.on_wheel(wheel(0.0, 0.0, SwipePhase::Ended), |_| true), SwipeAction::None);
        }

        #[test]
        fn post_lift_momentum_after_ended_never_starts_new_swipe_until_next_started() {
            // Case 1: Triggered swipe followed by AppKit momentum (`Moved` after `Ended`).
            let mut swipe = HistorySwipe::default();
            swipe.on_wheel(wheel(-20.0, 0.0, SwipePhase::Started), |_| true);
            swipe.on_wheel(wheel(-110.0, 0.0, SwipePhase::Moved), |_| true);
            assert_eq!(
                swipe.on_wheel(wheel(0.0, 0.0, SwipePhase::Ended), |_| true),
                SwipeAction::Navigate(HistoryDirection::Back)
            );
            // Post-lift momentum events arrive with `TouchPhase::Moved` and must not start a new swipe.
            for d in [45.0_f32, 38.0, 32.0, 27.0, 23.0, 0.0] {
                assert_eq!(swipe.on_wheel(wheel(-d, 0.0, SwipePhase::Moved), |_| true), SwipeAction::None);
            }
            assert!(!swipe.indicator().is_some_and(|i| i.tracking));

            // Case 2: Aborted flick (< 120px) followed by strong momentum must not navigate.
            let mut swipe = HistorySwipe::default();
            swipe.on_wheel(wheel(-20.0, 0.0, SwipePhase::Started), |_| true);
            swipe.on_wheel(wheel(-40.0, 0.0, SwipePhase::Moved), |_| true);
            assert_eq!(swipe.on_wheel(wheel(0.0, 0.0, SwipePhase::Ended), |_| true), SwipeAction::None);
            for d in [60.0_f32, 50.0, 42.0, 35.0, 29.0, 0.0] {
                assert_eq!(swipe.on_wheel(wheel(-d, 0.0, SwipePhase::Moved), |_| true), SwipeAction::None);
            }
            assert!(!swipe.indicator().is_some_and(|i| i.tracking));

            // Case 3: A fresh finger-down `SwipePhase::Started` re-arms swipes cleanly.
            swipe.on_wheel(wheel(-20.0, 0.0, SwipePhase::Started), |_| true);
            swipe.on_wheel(wheel(-110.0, 0.0, SwipePhase::Moved), |_| true);
            assert_eq!(
                swipe.on_wheel(wheel(0.0, 0.0, SwipePhase::Ended), |_| true),
                SwipeAction::Navigate(HistoryDirection::Back)
            );
        }
    }
}
