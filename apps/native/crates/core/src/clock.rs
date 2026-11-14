//! Client-to-server wall-clock offset tracking for live timers.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

static SERVER_CLOCK_OFFSETS: OnceLock<RwLock<HashMap<String, i64>>> = OnceLock::new();

fn offsets() -> &'static RwLock<HashMap<String, i64>> {
    SERVER_CLOCK_OFFSETS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Records the host wall-clock offset for `environment_id` from one
/// `server.getConfig` snapshot exchange.
///
/// The host stamped `server_time_ms` between `requested_at_ms` and
/// `received_at_ms`, so the true offset lies within that window. Keeping the
/// value nearest `0` leaves synced clocks unshifted rather than biasing every
/// timer by network latency.
pub fn record_server_clock(
    environment_id: &str,
    server_time_ms: Option<i64>,
    requested_at_ms: i64,
    received_at_ms: i64,
) {
    let Ok(mut map) = offsets().write() else {
        return;
    };
    let Some(server_time_ms) = server_time_ms else {
        map.remove(environment_id);
        return;
    };
    let offset = if server_time_ms > received_at_ms {
        server_time_ms - received_at_ms
    } else if server_time_ms < requested_at_ms {
        server_time_ms - requested_at_ms
    } else {
        0
    };
    map.insert(environment_id.to_owned(), offset);
}

/// Converts client epoch milliseconds to the environment's clock.
pub fn to_server_clock(environment_id: &str, client_ms: i64) -> i64 {
    let offset = offsets()
        .read()
        .ok()
        .and_then(|map| map.get(environment_id).copied())
        .unwrap_or(0);
    client_ms.saturating_add(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    #[test]
    fn counts_from_host_clock_running_ahead_minus_reply_latency() {
        record_server_clock("test-ahead", Some(HOUR + 1_000), 900, 1_050);
        assert_eq!(to_server_clock("test-ahead", 2_050), HOUR + 2_000);
    }

    #[test]
    fn counts_from_host_clock_running_behind_minus_request_latency() {
        record_server_clock("test-behind", Some(1_000), HOUR + 900, HOUR + 1_050);
        assert_eq!(to_server_clock("test-behind", HOUR + 1_900), 2_000);
    }

    #[test]
    fn leaves_clocks_within_round_trip_unshifted() {
        record_server_clock("test-synced", Some(5_000), 1_000, 9_000);
        assert_eq!(to_server_clock("test-synced", 12_345), 12_345);
    }

    #[test]
    fn clears_offset_when_server_time_missing() {
        record_server_clock("test-clear", Some(HOUR + 1_000), 900, 1_050);
        assert_eq!(to_server_clock("test-clear", 2_050), HOUR + 2_000);
        record_server_clock("test-clear", None, 2_000, 2_050);
        assert_eq!(to_server_clock("test-clear", 2_050), 2_050);
    }
}
