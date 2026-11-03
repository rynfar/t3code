use super::*;

#[test]
fn hides_descendants_by_parentage_even_when_rows_are_not_depth_first() {
    let process = |pid, ppid, depth| Process { identity: Identity { pid, start_time_ms: pid * 1000 }, ppid, depth, ..Default::default() };
    let processes = vec![process(1, 0, 0), process(10, 0, 0), process(2, 1, 1), process(11, 10, 1), process(3, 2, 2)];
    assert_eq!(visible_processes(&processes, &HashSet::from([processes[0].identity.clone()])), [0, 1, 3]);
    assert_eq!(visible_processes(&processes, &HashSet::new()), [0, 1, 2, 3, 4]);
    assert_eq!(visible_processes(&processes, &HashSet::from([Identity { pid: 1, start_time_ms: 999 }])), [0, 1, 2, 3, 4]);
}
#[test]
fn handles_cyclic_parentage_without_recursion() {
    let processes = vec![
        Process { identity: Identity { pid: 1, start_time_ms: 0 }, ppid: 2, ..Default::default() },
        Process { identity: Identity { pid: 2, start_time_ms: 0 }, ppid: 1, ..Default::default() },
    ];
    assert!(visible_processes(&processes, &HashSet::from([processes[0].identity.clone()])).is_empty());
}
#[test]
fn retry_and_chart_scales_match_web_logic() {
    for status in [SourceStatus::Starting, SourceStatus::Healthy, SourceStatus::Unknown] {
        assert!(!should_show_retry(Some(status), Some("error")));
    }
    for status in [SourceStatus::Degraded, SourceStatus::Unavailable, SourceStatus::Stopped] {
        assert!(should_show_retry(Some(status), None));
    }
    assert!(should_show_retry(None, Some("error")));
    assert!(!should_show_retry(None, None));
    assert_eq!(history_bar_height(0., 100., 2.), 0.);
    assert_eq!(history_bar_height(0.1, 100., 2.), 2.);
    assert_eq!(history_bar_height(25., 50., 2.), 50.);
    assert_eq!(history_cpu_scale_max(&[]), 1.);
    assert_eq!(history_cpu_scale_max(&[Bucket { avg_cpu_percent: 200., ..Default::default() }]), 200.);
}
#[test]
fn snapshot_history_and_retry_accept_future_fields_and_literals() {
    let value = json!({ "readAt": "2026-11-02T00:00:00Z", "sampleIntervalMs": 2000, "processes": [{"identity":{"pid":1,"startTimeMs":2},"category":"future","ioSemantics":"future","electronType":"future"}], "groups": {}, "power":{"source":"future","idle":"future","thermalState":"future"}, "speedLimitPercent":{"_tag":"Some","value":80}, "attribution":{}, "health":{"native":{"status":"future","lastError":{"_tag":"Some","value":"oops"}},"sidecarPid":{"_tag":"None"}}, "future":true });
    let snapshot: Snapshot = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(snapshot.processes[0].category, Category::Unknown);
    assert_eq!(snapshot.processes[0].electron_type, Some(ElectronType::Unknown));
    assert_eq!(snapshot.processes[0].io_semantics, IoSemantics::Unknown);
    assert_eq!(snapshot.power.source, PowerSource::Unknown);
    assert_eq!(snapshot.power.idle, BooleanState::Unknown);
    assert_eq!(snapshot.power.thermal_state, ThermalState::Unknown);
    assert_eq!(snapshot.speed_limit_percent, Some(80.));
    assert_eq!(snapshot.health.native.last_error.as_deref(), Some("oops"));
    assert_eq!(snapshot.health.native.status, SourceStatus::Unknown);
    let retry: RetryResult = serde_json::from_value(json!({"accepted":false,"snapshot":value,"future":1})).unwrap();
    assert_eq!(retry.snapshot, snapshot);
    let history: History = serde_json::from_value(json!({"readAt":"now","windowMs":900000,"bucketMs":30000,"sampleIntervalMs":2000,"retainedSampleCount":1,"buckets":[],"topProcesses":[{"category":"future","ioSemantics":"future"}],"health":{},"future":1})).unwrap();
    assert_eq!(history.top_processes[0].category, Category::Unknown);
}
#[test]
fn formats_web_units_and_process_name_fallback() {
    assert_eq!(bytes(1024.), "1.00 KB");
    assert_eq!(bytes(1023.6 * 1024.), "1.00 MB");
    assert_eq!(cpu_time(10_000.), "10s");
    assert_eq!(cpu_time(1_234.), "1.23s");
    assert_eq!(cpu_time(60_000.), "1m");
    assert_eq!(duration_micros(1250.), "1.25 ms");
    assert_eq!(sample_interval(2000), "2 seconds");
    assert_eq!(attribution_time(0.), "0s");
    assert_eq!(attribution_time(1200.), "1.20s");
    assert_eq!(process_name(&Process { command: "'/usr/bin/node' app.js".into(), ..Default::default() }), "node");
}

#[test]
fn a_period_change_never_retains_the_previous_period_on_loading_or_failure() {
    let mut state = HistoryState::default();
    state.select(1);
    let loaded = state.query.begin();
    state.query.finish(loaded, Ok(std::sync::Arc::new(History { window_ms: HISTORY_WINDOWS[1].window_ms, ..Default::default() })));
    state.select(1);
    assert!(state.query.data.is_some(), "same-period refresh may retain its data");
    let stale = state.query.begin();
    state.select(0);
    assert_eq!(state.selected, 0);
    let loading = state.query.begin();
    assert!(state.query.initial_loading());
    assert!(state.query.data.is_none(), "the selected 5m period must not show 15m data");
    assert!(state.query.finish(loading, Err("history unavailable".into())));
    assert!(state.query.data.is_none());
    assert_eq!(state.query.error.as_deref(), Some("history unavailable"));
    assert!(!state.query.finish(stale, Ok(std::sync::Arc::new(History::default()))));
    state.select(2);
    assert!(state.query.error.is_none());
    assert!(!state.query.pending);
    let old_visit = state.query.begin();
    state.reset();
    assert_eq!(state.selected, 1);
    let _new_visit = state.query.begin();
    assert!(!state.query.finish(old_visit, Ok(std::sync::Arc::new(History::default()))));
}
