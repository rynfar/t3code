use std::collections::HashSet;

use serde_json::{Value, json};

use super::*;

fn process(pid: u64, depth: usize, command: &str) -> ProcessEntry {
    ProcessEntry { pid, start_time_ms: pid * 10, cpu_percent: 0., rss_bytes: 0., command: command.into(), depth, has_children: false }
}

#[test]
fn decodes_process_diagnostics_with_its_option_error() {
    let decoded = decode_process_diagnostics(&json!({
        "serverPid": 35525, "readAt": "2026-11-02T17:58:59.451Z", "processCount": 1, "totalRssBytes": 2048, "totalCpuPercent": 1.25,
        "processes": [{ "pid": 7, "startTimeMs": 99, "ppid": 35525, "pgid": { "_tag": "None" }, "status": "S", "cpuPercent": 1.25, "rssBytes": 2048,
            "elapsed": "00:01", "command": "/usr/bin/node server.js", "depth": 0, "childPids": [8] }],
        "error": { "_tag": "Some", "value": { "message": "Resource monitor binary was not found for darwin/arm64." } },
    }))
    .unwrap();
    assert_eq!(decoded.server_pid, 35525);
    assert_eq!(
        decoded.processes[0],
        ProcessEntry { pid: 7, start_time_ms: 99, cpu_percent: 1.25, rss_bytes: 2048., command: "/usr/bin/node server.js".into(), depth: 0, has_children: true }
    );
    assert_eq!(decoded.error.as_deref(), Some("Resource monitor binary was not found for darwin/arm64."));
    assert_eq!(decode_process_diagnostics(&json!({ "serverPid": 1, "error": { "_tag": "None" } })).unwrap().error, None);
}

#[test]
fn collapsing_a_process_hides_its_subtree_only() {
    let processes = vec![process(1, 0, "a"), process(2, 1, "b"), process(3, 2, "c"), process(4, 1, "d"), process(5, 0, "e")];
    assert_eq!(visible_process_tree(&processes, &HashSet::new()), vec![0, 1, 2, 3, 4]);
    assert_eq!(visible_process_tree(&processes, &HashSet::from([2])), vec![0, 1, 3, 4]);
    assert_eq!(visible_process_tree(&processes, &HashSet::from([1])), vec![0, 4]);
}

#[test]
fn names_and_types_processes_like_the_web() {
    assert_eq!(process_name("\"/Applications/Code Helper\" --type=gpu"), "Code");
    assert_eq!(process_name("/usr/local/bin/codex exec --json"), "codex");
    assert_eq!(process_name("C:\\tools\\node.exe app.js"), "node.exe");
    assert_eq!(short_process_name(&"x".repeat(50)), format!("{}...", "x".repeat(39)));
    assert_eq!(process_type(&process(1, 0, "/opt/bin/Claude --print")), "Agent");
    assert_eq!(process_type(&process(1, 0, "node opencode-ai")), "Agent");
    assert_eq!(process_type(&process(1, 0, "node claudette")), "Process", "whole words only");
    assert_eq!(process_type(&process(1, 2, "codex")), "Subprocess");
}

#[test]
fn resource_history_bars_and_depths() {
    let history = decode_resource_history(&json!({
        "readAt": "2026-11-02T19:11:20.091Z", "sampleIntervalMs": 0, "retainedSampleCount": 0, "totalCpuSecondsApprox": 61,
        "buckets": [{ "avgCpuPercent": 10, "maxCpuPercent": 40 }, { "avgCpuPercent": 0, "maxCpuPercent": 0 }],
        "topProcesses": [
            { "processKey": "1:0", "pid": 1, "command": "node", "depth": 0, "isServerRoot": true },
            { "processKey": "2:0", "pid": 2, "command": "codex", "depth": 3, "isServerRoot": false },
            { "processKey": "3:0", "pid": 3, "command": "git", "depth": 5, "isServerRoot": false },
        ],
        "error": { "_tag": "None" },
    }))
    .unwrap();
    assert_eq!(history.bars(), vec![(100., 25.), (2., 2.)]);
    assert_eq!(history.interval_label(), "On demand");
    let depths: Vec<usize> = history.top_processes.iter().map(|process| history.visual_depth(process)).collect();
    assert_eq!(depths, [0, 1, 3]);
    assert_eq!(HISTORY_WINDOWS[DEFAULT_HISTORY_WINDOW].payload(), json!({ "windowMs": 900_000, "bucketMs": 60_000 }));
}

/// Outputs of the web's `formatDuration` and `formatCpuTime` for the same inputs.
#[test]
fn durations_match_the_web() {
    for (ms, label) in [(1000., "1 s"), (1500., "1.50 s"), (10_000., "10 s"), (12_345., "12.3 s"), (999.6, "1000 ms"), (12., "12 ms")] {
        assert_eq!(format_duration(ms), label);
    }
    for (seconds, label) in [(0., "0s"), (5.5, "5.50s"), (12., "12s"), (61., "1.02m"), (600., "10m"), (3600., "1h"), (7322., "2.03h")] {
        assert_eq!(format_cpu_time(seconds), label);
    }
    assert_eq!(percent(1.25), "1.3%", "toFixed rounds an exact tie up");
    assert_eq!(percent(0.04), "0.0%");
}

#[test]
fn trace_ids_shorten_and_times_read_relative() {
    assert_eq!(shorten_trace_id("abc"), "abc");
    assert_eq!(shorten_trace_id("7ab1ad6354c469e8189da26d12be34bdff"), "7ab1ad6354c469e818...12be34bdff");
    let now = crate::sidebar::time::parse_iso("2026-11-02T12:00:00.000Z").unwrap();
    assert_eq!(relative_no_wrap("2026-11-02T11:55:00.000Z", now), "5m\u{a0}ago");
    assert_eq!(relative_no_wrap("2026-11-02T11:59:30.000Z", now), "just now");
    assert_eq!(relative_no_wrap("never", now), "");
    assert_eq!(last_checked(None, now), "Checking");
    assert_eq!(last_checked(Some("bad"), now), "Checked unavailable");
    assert_eq!(last_checked(Some("2026-11-02T09:00:00.000Z"), now), "Checked 3h ago");
    assert!(can_expand(&"x".repeat(181)));
    assert!(can_expand("a\nb"));
    assert!(!can_expand("short"));
}

#[test]
fn decodes_trace_diagnostics_and_words_partial_failures() {
    let decoded = decode_trace_diagnostics(&json!({
        "readAt": "2026-11-02T12:00:00.000Z", "recordCount": 1200, "parseErrorCount": 0, "failureCount": 3, "slowSpanThresholdMs": 1000, "slowSpanCount": 2,
        "topSpansByCount": [{ "name": "rpc", "count": 9, "failureCount": 1, "averageDurationMs": 12, "maxDurationMs": 40, "totalDurationMs": 108 }],
        "slowestSpans": [{ "name": "git", "durationMs": 4200, "endedAt": "2026-11-02T11:00:00.000Z", "traceId": "t", "spanId": "s" }],
        "commonFailures": [], "latestFailures": [], "latestWarningAndErrorLogs": [],
        "partialFailure": { "_tag": "Some", "value": true },
        "error": { "_tag": "Some", "value": { "kind": "trace-file-read-failed", "message": "EACCES" } },
    }))
    .unwrap();
    assert_eq!(decoded.top_spans_by_count[0].name, "rpc");
    assert_eq!(decoded.slow_span_tooltip(), "Spans with a duration of 1 s or longer.");
    assert_eq!(decoded.error_text().as_deref(), Some("Some trace files could not be read, so diagnostics may be incomplete. EACCES"));
}

#[test]
fn signal_replies_map_to_toasts() {
    let process = process(42, 0, "x");
    assert_eq!(Signal::Kill.payload(&process), json!({ "pid": 42, "startTimeMs": 420, "signal": "SIGKILL" }));
    assert_eq!(signal_outcome(&json!({ "signaled": true, "message": { "_tag": "None" } }), Signal::Int), SignalOutcome::Signaled);
    let stale = json!({ "signaled": false, "message": { "_tag": "Some", "value": "Process 42 is not a live descendant of the server." } });
    assert_eq!(signal_outcome(&stale, Signal::Kill), SignalOutcome::AlreadyExited);
    let failed = json!({ "signaled": false, "message": { "_tag": "Some", "value": "EPERM" } });
    assert_eq!(signal_outcome(&failed, Signal::Kill), SignalOutcome::Failed("EPERM".into()));
    assert_eq!(signal_outcome(&json!({ "signaled": false, "message": { "_tag": "None" } }), Signal::Int), SignalOutcome::Failed("Failed to send SIGINT.".into()));
    assert_eq!(kill_confirmation(42).0, "Send SIGKILL to process 42?");
}

fn observability(value: Value) -> ServerObservability {
    serde_json::from_value(value).unwrap()
}

#[test]
fn telemetry_status_follows_overrides_and_checks() {
    let running = observability(json!({
        "logsDirectoryPath": "/logs", "otlpTracesUrl": "http://collector:4318/v1/traces", "otlpTracesSource": "environment",
        "otlpTracesVariable": "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "otlpLogsUrl": "http://saved", "otlpLogsSource": "settings",
    }));
    let traces = running_override(Some(&running), OtlpSignal::Traces).unwrap();
    assert_eq!(traces.label, "Set by environment");
    assert!(running_override(Some(&running), OtlpSignal::Logs).is_none(), "settings-sourced endpoints are editable");

    assert_eq!(endpoint_status("", None), ("Off".into(), Dot::Muted));
    assert_eq!(endpoint_status("http://a", None), ("Not checked".into(), Dot::Muted));
    let pending = EndpointCheck { url: "http://a".into(), result: None };
    assert_eq!(endpoint_status("http://a", Some(&pending)), ("Checking\u{2026}".into(), Dot::Warning));
    let accepted = EndpointCheck { url: "http://a".into(), result: Some(CheckResult::Accepted { latency_ms: 12.4 }) };
    assert_eq!(endpoint_status("http://a", Some(&accepted)), ("Connected \u{b7} 12 ms".into(), Dot::Success));
    assert_eq!(endpoint_status("http://b", Some(&accepted)).0, "Not checked", "a check of another URL does not count");
    let rejected = EndpointCheck { url: "http://a".into(), result: decode_check(&json!({ "_tag": "Rejected", "status": 404 })) };
    assert_eq!(endpoint_status("http://a", Some(&rejected)).0, "Rejected \u{b7} HTTP 404");
    assert_eq!(signal_status(Some(&traces), Some(("Timed out".into(), Dot::Destructive))), Some(("Set by environment \u{b7} Timed out".into(), Dot::Destructive)));
    assert_eq!(signal_status(Some(&traces), None), Some(("Set by environment".into(), Dot::Muted)));
}

#[test]
fn telemetry_saves_changed_endpoints_and_shows_what_runs() {
    let saved = ObservabilitySettings { otlp_traces_url: "http://a".into(), ..Default::default() };
    let values = vec![(OtlpSignal::Traces, " http://a ".to_owned()), (OtlpSignal::Metrics, "http://m".to_owned()), (OtlpSignal::Logs, String::new())];
    assert_eq!(changed_signals(&values, &saved), vec![OtlpSignal::Metrics]);
    assert_eq!(telemetry_save_payload(&values, &saved), json!({ "patch": { "observability": { "otlpMetricsUrl": "http://m" } } }));

    let running = observability(json!({ "otlpTracesUrl": "http://b", "otlpTracesSource": "settings" }));
    assert_eq!(running_line(OtlpSignal::Traces, &saved, Some(&running)), Some(("http://b".into(), None)));
    assert_eq!(running_line(OtlpSignal::Logs, &saved, Some(&running)), None);
    let pending = ObservabilitySettings { otlp_logs_url: "http://l".into(), ..Default::default() };
    assert_eq!(running_line(OtlpSignal::Logs, &pending, Some(&running)), Some(("Disabled".into(), None)), "saved but not yet running");
    assert!(is_http_url("HTTPS://x"));
    assert!(!is_http_url("http://"));
    assert!(!is_http_url("ftp://x"));
}
