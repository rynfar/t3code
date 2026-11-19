//! OS & application wakeups (`packages/client-runtime/src/connection/wakeups.ts`) plus a
//! monotonic-vs-wall clock-jump wake detector for sleep/resume when platform notifications are
//! unavailable or running headless.

use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::broadcast;

/// `ConnectionWakeup` (`packages/client-runtime/src/connection/wakeups.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionWakeup {
    ApplicationActive,
    ApplicationActiveProbe,
    ApplicationActiveLong,
    ApplicationFocused,
    CredentialsChanged,
    NetworkChanged,
}

fn is_application_active_wakeup(reason: ConnectionWakeup) -> bool {
    matches!(
        reason,
        ConnectionWakeup::ApplicationActive
            | ConnectionWakeup::ApplicationActiveProbe
            | ConnectionWakeup::ApplicationActiveLong
    )
}

/// `resetsRetryBackoff` (`wakeups.ts`).
pub const fn resets_retry_backoff(reason: ConnectionWakeup) -> bool {
    matches!(
        reason,
        ConnectionWakeup::ApplicationActive
            | ConnectionWakeup::ApplicationActiveProbe
            | ConnectionWakeup::ApplicationActiveLong
            | ConnectionWakeup::NetworkChanged
    )
}

/// `shouldResubscribeAfterWakeup` (`wakeups.ts`).
pub const fn should_resubscribe_after_wakeup(reason: ConnectionWakeup) -> bool {
    matches!(
        reason,
        ConnectionWakeup::ApplicationActive
            | ConnectionWakeup::ApplicationActiveProbe
            | ConnectionWakeup::ApplicationActiveLong
    )
}

/// `shouldRelistAccountAfterWakeup` (`wakeups.ts`).
pub const fn should_relist_account_after_wakeup(reason: ConnectionWakeup) -> bool {
    matches!(
        reason,
        ConnectionWakeup::ApplicationActive
            | ConnectionWakeup::ApplicationActiveLong
            | ConnectionWakeup::ApplicationFocused
    )
}

/// `reachesConnectionSupervisor` (`wakeups.ts`): window focus alone never arms silent-socket probes.
pub const fn reaches_connection_supervisor(reason: ConnectionWakeup) -> bool {
    !matches!(reason, ConnectionWakeup::ApplicationFocused)
}

/// Probe timeout on a live session after a wakeup (`probeTimeoutFor` in `supervisor.ts`).
pub const fn probe_timeout_for(reason: ConnectionWakeup, nearby: bool) -> Option<Duration> {
    match reason {
        ConnectionWakeup::ApplicationActive => Some(Duration::from_secs(15)),
        ConnectionWakeup::ApplicationActiveLong => Some(Duration::from_millis(1_500)),
        ConnectionWakeup::ApplicationActiveProbe | ConnectionWakeup::NetworkChanged => {
            Some(if nearby { Duration::from_secs(3) } else { Duration::from_secs(15) })
        }
        ConnectionWakeup::ApplicationFocused | ConnectionWakeup::CredentialsChanged => None,
    }
}

/// Minimum wall-vs-monotonic or sleep-overrun gap treated as an OS suspend/wake event.
pub const CLOCK_JUMP_WAKE_THRESHOLD: Duration = Duration::from_secs(10);
/// Gap above which a resume is classified as `ApplicationActiveLong` (matches long suspension).
pub const CLOCK_JUMP_LONG_THRESHOLD: Duration = Duration::from_secs(30);
/// Background poll cadence for the clock-jump detector (low overhead, never touches UI).
pub const CLOCK_WATCH_INTERVAL: Duration = Duration::from_secs(5);

/// Classifies elapsed monotonic vs wall-clock deltas across one `expected_interval` sleep into
/// a wakeup reason. On macOS, `Instant` pauses during sleep while `SystemTime` advances; on
/// platforms where monotonic clock continues or process stops, `mono_elapsed` overshoots
/// `expected_interval`. Both are detected without false positives from small jitter.
pub fn classify_clock_jump(
    expected_interval: Duration,
    mono_elapsed: Duration,
    wall_elapsed: Option<Duration>,
) -> Option<ConnectionWakeup> {
    let mono_overrun = mono_elapsed.saturating_sub(expected_interval);
    let wall_overrun = wall_elapsed
        .map(|wall| wall.saturating_sub(expected_interval).max(wall.saturating_sub(mono_elapsed)))
        .unwrap_or(Duration::ZERO);
    let gap = mono_overrun.max(wall_overrun);
    if gap >= CLOCK_JUMP_LONG_THRESHOLD {
        Some(ConnectionWakeup::ApplicationActiveLong)
    } else if gap >= CLOCK_JUMP_WAKE_THRESHOLD {
        Some(ConnectionWakeup::ApplicationActive)
    } else {
        None
    }
}

/// Classifies an application-reactivation inactivity duration into the matching wakeup reason.
pub fn classify_app_resume(inactive_for: Duration) -> ConnectionWakeup {
    if inactive_for >= CLOCK_JUMP_LONG_THRESHOLD {
        ConnectionWakeup::ApplicationActiveLong
    } else if inactive_for >= Duration::from_secs(5) {
        ConnectionWakeup::ApplicationActive
    } else {
        ConnectionWakeup::ApplicationActiveProbe
    }
}

fn bus() -> &'static broadcast::Sender<ConnectionWakeup> {
    static BUS: OnceLock<broadcast::Sender<ConnectionWakeup>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(64).0)
}

/// Broadcasts an OS/app wakeup signal to every active connection supervisor.
pub fn emit_wakeup(reason: ConnectionWakeup) {
    if is_application_active_wakeup(reason) || matches!(reason, ConnectionWakeup::NetworkChanged | ConnectionWakeup::CredentialsChanged | ConnectionWakeup::ApplicationFocused) {
        let _ = bus().send(reason);
    }
}

/// Subscribes to process-wide OS/app wakeup signals.
pub fn subscribe() -> broadcast::Receiver<ConnectionWakeup> {
    bus().subscribe()
}

/// Ensures the low-frequency monotonic/wall clock-jump wake watcher is running on `runtime`.
/// Safe to call repeatedly; starts at most one background task per process.
/// Disabled under `cfg(test)` so CPU scheduling jitter across parallel test suites does not emit
/// spurious process-wide wakeups into unrelated `FakeServer` sessions.
pub fn ensure_clock_jump_watcher(runtime: &tokio::runtime::Handle) {
    #[cfg(test)]
    {
        let _ = runtime;
    }
    #[cfg(not(test))]
    {
        use std::time::{Instant, SystemTime};
        static STARTED: OnceLock<()> = OnceLock::new();
        if STARTED.set(()).is_err() {
            return;
        }
        runtime.spawn(async {
            let mut last_mono = Instant::now();
            let mut last_wall = SystemTime::now();
            loop {
                tokio::time::sleep(CLOCK_WATCH_INTERVAL).await;
                let now_mono = Instant::now();
                let now_wall = SystemTime::now();
                let mono_elapsed = now_mono.saturating_duration_since(last_mono);
                let wall_elapsed = now_wall.duration_since(last_wall).ok();
                last_mono = now_mono;
                last_wall = now_wall;
                if let Some(reason) = classify_clock_jump(CLOCK_WATCH_INTERVAL, mono_elapsed, wall_elapsed) {
                    tracing::info!(?reason, ?mono_elapsed, ?wall_elapsed, "detected system wake clock jump");
                    emit_wakeup(reason);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wakeup_policies_match_web_wakeups_ts() {
        assert!(resets_retry_backoff(ConnectionWakeup::ApplicationActive));
        assert!(resets_retry_backoff(ConnectionWakeup::ApplicationActiveProbe));
        assert!(resets_retry_backoff(ConnectionWakeup::ApplicationActiveLong));
        assert!(resets_retry_backoff(ConnectionWakeup::NetworkChanged));
        assert!(!resets_retry_backoff(ConnectionWakeup::ApplicationFocused));
        assert!(!resets_retry_backoff(ConnectionWakeup::CredentialsChanged));

        assert!(should_resubscribe_after_wakeup(ConnectionWakeup::ApplicationActive));
        assert!(!should_resubscribe_after_wakeup(ConnectionWakeup::NetworkChanged));

        assert!(should_relist_account_after_wakeup(ConnectionWakeup::ApplicationFocused));
        assert!(!should_relist_account_after_wakeup(ConnectionWakeup::ApplicationActiveProbe));

        assert!(!reaches_connection_supervisor(ConnectionWakeup::ApplicationFocused));
        assert!(reaches_connection_supervisor(ConnectionWakeup::ApplicationActiveLong));

        assert_eq!(probe_timeout_for(ConnectionWakeup::ApplicationActiveLong, false), Some(Duration::from_millis(1_500)));
        assert_eq!(probe_timeout_for(ConnectionWakeup::ApplicationActiveProbe, true), Some(Duration::from_secs(3)));
        assert_eq!(probe_timeout_for(ConnectionWakeup::ApplicationActiveProbe, false), Some(Duration::from_secs(15)));
        assert_eq!(probe_timeout_for(ConnectionWakeup::ApplicationFocused, true), None);
    }

    #[test]
    fn clock_jump_classifier_detects_short_and_long_sleep_without_jitter_false_positives() {
        let interval = Duration::from_secs(5);
        // Normal tick with small scheduler jitter -> no wake.
        assert_eq!(classify_clock_jump(interval, Duration::from_millis(5_200), Some(Duration::from_millis(5_250))), None);
        // Backwards NTP step -> wall_elapsed None -> no wake.
        assert_eq!(classify_clock_jump(interval, Duration::from_secs(5), None), None);
        // macOS sleep where Instant pauses (5s) while wall clock jumps 18s -> ApplicationActive.
        assert_eq!(
            classify_clock_jump(interval, Duration::from_secs(5), Some(Duration::from_secs(18))),
            Some(ConnectionWakeup::ApplicationActive)
        );
        // Long sleep where wall clock jumps 45s -> ApplicationActiveLong.
        assert_eq!(
            classify_clock_jump(interval, Duration::from_secs(5), Some(Duration::from_secs(45))),
            Some(ConnectionWakeup::ApplicationActiveLong)
        );
        // Monotonic overrun (SIGSTOP / VM pause) -> ApplicationActiveLong.
        assert_eq!(
            classify_clock_jump(interval, Duration::from_secs(60), Some(Duration::from_secs(60))),
            Some(ConnectionWakeup::ApplicationActiveLong)
        );
    }

    #[test]
    fn broadcast_wakeup_reaches_subscribers() {
        // Use ApplicationFocused so parallel supervisor tests sharing the global bus are never probed.
        let mut rx = subscribe();
        emit_wakeup(ConnectionWakeup::ApplicationFocused);
        assert_eq!(rx.try_recv(), Ok(ConnectionWakeup::ApplicationFocused));
    }
}
