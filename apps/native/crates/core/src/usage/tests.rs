use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Value, json};

use super::chart::{self, Point};
use super::context_window::{self, context_window_snapshot};
use super::format::*;
use super::limits::{self, LimitEnvironment, LimitPace};
use super::merge::{self, AnsweredUsage, ChartMetric};
use super::*;
use crate::server_config::{ServerProvider, UsageLimitSource, UsageWindow};
use crate::sidebar::time::{FixedZone, Zone, parse_iso};
use crate::thread::ThreadProjection;

fn bucket(overrides: Value) -> Value {
    let mut value = json!({
        "day": "2026-08-07", "provider": "claude", "model": "claude-fable-5",
        "totals": {"uncachedInputTokens": 100, "cachedInputTokens": 1000, "cacheCreationTokens": 10, "outputTokens": 50, "reasoningTokens": 0},
        "costUsd": 10, "cacheSavingsUsd": 2, "costSource": "modelPriced", "records": 5, "unpricedRecords": 0, "sessions": 1
    });
    merge_json(&mut value, overrides);
    value
}

fn source(provider: &str, host: &str, home: &str, sessions: u64, status: &str) -> Value {
    json!({
        "fingerprint": {"hostId": host, "provider": provider, "resolvedHomePath": home, "volumeId": format!("vol-{host}")},
        "status": status, "scannedFiles": 1, "skippedFiles": 0, "malformedRecords": 0, "distinctSessions": sessions, "message": null
    })
}

fn summary(buckets: Vec<Value>, sources: Vec<Value>, extra: Value) -> UsageSummary {
    let mut value = json!({
        "contractVersion": 6, "readAt": "2026-08-07T00:00:00.000Z", "timeZone": "UTC",
        "sinceDay": "2026-08-01", "untilDay": "2026-08-31", "buckets": buckets, "sources": sources,
        "pricing": {"status": "fresh", "source": "litellm", "fetchedAt": null, "knownModels": 10}, "scanDurationMs": 1
    });
    merge_json(&mut value, extra);
    decode_summary(value).unwrap()
}

fn merge_json(base: &mut Value, extra: Value) {
    if let (Value::Object(base), Value::Object(extra)) = (base, extra) {
        base.extend(extra);
    }
}

fn answered(id: &str, summary: UsageSummary) -> AnsweredUsage {
    AnsweredUsage { environment_id: id.to_owned(), label: id.to_owned(), summary: Arc::new(summary) }
}

#[test]
fn decoding_skips_buckets_and_sources_this_build_cannot_read() {
    let decoded = summary(
        vec![bucket(json!({})), bucket(json!({"provider": "future"})), bucket(json!({"costSource": "guessed"}))],
        vec![source("claude", "mac", "/c", 1, "ok"), source("future", "mac", "/f", 1, "ok"), source("claude", "mac", "/d", 1, "rescanning")],
        json!({"futureField": true}),
    );
    assert_eq!(decoded.buckets.len(), 1);
    assert_eq!(decoded.sources.len(), 1);
    assert!(!decoded.is_refreshing());
    let refreshing = summary(vec![], vec![merge_into(source("codex", "mac", "/x", 0, "ok"), json!({"refreshing": true}))], json!({}));
    assert!(refreshing.is_refreshing());
}

fn merge_into(mut value: Value, extra: Value) -> Value {
    merge_json(&mut value, extra);
    value
}

#[test]
fn summary_input_serializes_only_what_is_set() {
    let window = make_window(30, parse_iso("2026-11-02T15:30:00Z").unwrap(), UsageResolution::Day, "America/Denver", &FixedZone(-7 * 3600));
    assert_eq!(window.payload(), json!({"sinceDay": "2026-10-04", "untilDay": "2026-11-02", "timeZone": "America/Denver", "resolution": "day"}));
    let filter = UsageModelFilter { include: None, exclude: Some(vec![UsageModelSelector { provider: None, model: "m".into() }]) };
    let filtered = window.with_model_filter(Some(filter)).awaiting_refresh();
    assert_eq!(filtered.payload()["awaitRefresh"], json!(true));
    assert_eq!(filtered.payload()["modelFilter"], json!({"exclude": [{"model": "m"}]}));
    assert!(filtered.same_window(&window));
}

#[test]
fn hourly_window_rolls_twenty_four_hours_to_the_minute() {
    let now = parse_iso("2026-11-02T03:30:45.500Z").unwrap();
    let window = make_window(1, now, UsageResolution::Hour, "America/Denver", &FixedZone(-7 * 3600));
    assert_eq!(window.since_time.as_deref(), Some("2026-11-01T03:30:00.000Z"));
    assert_eq!(window.until_time.as_deref(), Some("2026-11-02T03:30:00.000Z"));
    assert_eq!((window.since_day.as_str(), window.until_day.as_str()), ("2026-10-31", "2026-11-01"));
    assert_eq!(window_periods(&window).len(), 24);
}

#[test]
fn number_formats_match_intl_and_to_fixed() {
    let tokens: Vec<String> = [999., 1500., 250_500., 12_345_678., 1e12, 1e6, 999_999., 1999.4].into_iter().map(format_tokens).collect();
    assert_eq!(tokens, ["999", "1.50K", "251K", "12.3M", "1T", "1M", "1000K", "2K"]);
    let usd: Vec<String> = [0., 1_234_567.891, -1234.005, 0.005, 2.675].into_iter().map(format_usd).collect();
    assert_eq!(usd, ["$0.00", "$1,234,567.89", "-$1,234.01", "$0.01", "$2.68"]);
    let percents: Vec<String> = [0.0004, 0.5, 0.12345, 0.].into_iter().map(format_percent).collect();
    assert_eq!(percents, ["<0.1%", "50.0%", "12.3%", "0.0%"]);
    assert_eq!((format_count(1234.5), format_count(2.5)), ("1,235".to_owned(), "3".to_owned()));
    assert_eq!([to_fixed(1.005, 2), to_fixed(2.5, 0), to_fixed(0.125, 2), to_fixed(-2.5, 0)], ["1.00", "3", "0.13", "-3"]);
}

#[test]
fn days_and_hours_enumerate_like_the_web() {
    assert_eq!(format_day_short("2026-11-02"), "Nov 2");
    assert_eq!(format_day_short("bad"), "bad");
    assert_eq!(enumerate_days("2026-02-27", "2026-03-01"), ["2026-02-27", "2026-02-28", "2026-03-01"]);
    assert!(enumerate_days("2026-03-01", "2026-02-27").is_empty());
    assert_eq!(enumerate_hour_starts("2026-11-02T10:30:00.000Z", "2026-11-02T12:00:00.000Z"), ["2026-11-02T10:30:00.000Z", "2026-11-02T11:30:00.000Z"]);
}

struct NewYork;

impl Zone for NewYork {
    fn offset_seconds_at(&self, utc_ms: i64) -> i64 {
        if utc_ms < parse_iso("2026-11-01T06:00:00Z").unwrap() { -4 * 3600 } else { -5 * 3600 }
    }
}

#[test]
fn hour_labels_follow_the_clock_setting_and_name_repeated_hours() {
    let zone = FixedZone(0);
    assert_eq!(format_hour_short("2026-08-11T12:37:00.000Z", &zone, true), "12 PM");
    assert_eq!(format_hour_short("2026-08-11T00:37:00.000Z", &zone, true), "12 AM");
    assert_eq!(format_hour_short("2026-08-11T09:37:00.000Z", &zone, false), "09:00");
    assert_eq!(format_hour_short("invalid", &zone, true), "invalid");
    assert_eq!(format_hour_short("2026-11-01T05:37:00.000Z", &NewYork, true), "1 AM GMT-4");
    assert_eq!(format_hour_short("2026-11-01T06:37:00.000Z", &NewYork, false), "01:00 GMT-5");
    assert_eq!(format_date_time_short("2026-11-02T15:00:00.000Z", &NewYork, true), "Nov 2, 10 AM");
    assert_eq!(format_relative_hour_short("2026-11-02T15:00:00.000Z", "2026-11-02T20:00:00.000Z", &NewYork, true), "10 AM today");
    assert_eq!(format_relative_hour_short("2026-11-01T15:00:00.000Z", "2026-11-02T20:00:00.000Z", &NewYork, true), "10 AM yesterday");
}

#[test]
fn source_messages_combine_skipped_records_once_per_source() {
    let malformed = |count: u64, extra: Value| -> UsageSource {
        let mut value = source("codex", "demo-host", "/demo/codex", 1, "partial");
        merge_json(&mut value, json!({"malformedRecords": count, "message": format_malformed_usage_warning(count)}));
        merge_json(&mut value, extra);
        serde_json::from_value(value).unwrap()
    };
    let codex = malformed(1, json!({}));
    let mut claude = malformed(11, json!({}));
    claude.fingerprint.provider = UsageProvider::Claude;
    claude.fingerprint.resolved_home_path = "/demo/claude".into();
    assert_eq!(usage_source_messages([&codex, &claude]), [format_malformed_usage_warning(12)]);
    assert_eq!(usage_source_messages([&codex, &malformed(11, json!({})), &codex]), [format_malformed_usage_warning(11)]);
    let read_failure = "2 transcript path(s) could not be read; usage may be incomplete.";
    let combined = malformed(1, json!({"message": format!("{read_failure} {}", format_malformed_usage_warning(1))}));
    let cursor = merge_into(source("cursor", "demo-host", "/demo/codex", 1, "ok"), json!({"message": "Cursor history is limited to the last 30 days."}));
    let cursor: UsageSource = serde_json::from_value(cursor).unwrap();
    assert_eq!(usage_source_messages([&combined, &cursor]), [read_failure.to_owned(), cursor.message.clone().unwrap(), format_malformed_usage_warning(1)]);
    let handled = malformed(1, json!({"action": "enableCursorKeychain"}));
    let healthy = malformed(1, json!({"status": "ok"}));
    assert!(usage_source_messages([&handled, &healthy]).is_empty());
}

#[test]
fn a_shared_transcript_directory_counts_once() {
    let shared = || source("claude", "mac", "/home/theo/.claude", 1, "ok");
    let merged = merge::merge_usage(
        &[answered("env-a", summary(vec![bucket(json!({}))], vec![shared()], json!({}))), answered("env-b", summary(vec![bucket(json!({}))], vec![shared()], json!({})))],
        USAGE_CONTRACT_VERSION,
    );
    assert_eq!((merged.cost_usd, merged.records, merged.sessions), (10., 5, 1));
    assert_eq!(merged.duplicate_sources.len(), 1);
    assert_eq!(merged.contributing_environments, ["env-a"]);
}

#[test]
fn a_later_partial_scan_adds_only_new_cells() {
    let complete = answered("old", summary(vec![bucket(json!({}))], vec![source("claude", "mac", "/home/theo/.claude", 1, "ok")], json!({"contractVersion": 4})));
    let partial_summary = summary(
        vec![
            bucket(json!({"sourcePath": "/home/theo/.claude", "costUsd": 4, "records": 2})),
            bucket(json!({"day": "2026-08-08", "sourcePath": "/home/theo/.claude", "costUsd": 3, "records": 1})),
        ],
        vec![source("claude", "mac", "/home/theo/.claude", 2, "partial")],
        json!({"readAt": "2026-08-08T01:00:00.000Z"}),
    );
    let partial = answered("new", partial_summary.clone());
    for ordered in [[complete.clone(), partial.clone()], [partial.clone(), complete.clone()]] {
        let merged = merge::merge_usage(&ordered, USAGE_CONTRACT_VERSION);
        assert_eq!((merged.cost_usd, merged.records, merged.sessions), (13., 6, 2));
        let days: Vec<(&str, f64)> = merged.daily.iter().map(|day| (day.day.as_str(), day.cost_usd)).collect();
        assert_eq!(days, [("2026-08-07", 10.), ("2026-08-08", 3.)]);
        assert_eq!(merged.duplicate_sources, ["new: /home/theo/.claude"]);
    }
    let filter = UsageModelFilter { include: None, exclude: Some(vec![UsageModelSelector { provider: None, model: "claude-fable-5".into() }]) };
    let filtered = answered("new", UsageSummary { model_filter: Some(filter), ..partial_summary });
    let merged = merge::merge_usage(&[complete, filtered], USAGE_CONTRACT_VERSION);
    assert_eq!((merged.cost_usd, merged.sessions), (10., 1));
}

#[test]
fn merge_reports_contract_mismatches_and_skips_missing_sources() {
    let older = answered("old", summary(vec![bucket(json!({}))], vec![source("claude", "a", "/a", 1, "ok")], json!({"contractVersion": 3})));
    let newer = answered("new", summary(vec![bucket(json!({}))], vec![source("claude", "b", "/b", 1, "ok")], json!({"contractVersion": 7})));
    let missing = answered("missing", summary(vec![bucket(json!({}))], vec![source("claude", "c", "/c", 0, "missing")], json!({})));
    let merged = merge::merge_usage(&[older, newer, missing], USAGE_CONTRACT_VERSION);
    let directions: Vec<merge::MismatchDirection> = merged.contract_mismatches.iter().map(|mismatch| mismatch.direction).collect();
    assert_eq!(directions, [merge::MismatchDirection::ServerBehind, merge::MismatchDirection::ClientBehind]);
    assert_eq!(merged.cost_usd, 0.);
    assert_eq!(format_usage_contract_mismatch("Laptop", &merged.contract_mismatches[0]), "Laptop runs an older server version and is excluded from totals.");
}

#[test]
fn merge_splits_cost_and_marks_unpriced_models() {
    let merged = merge::merge_usage(
        &[answered(
            "env",
            summary(
                vec![
                    bucket(json!({"categoryCostUsd": {"input": 2, "cacheRead": 1, "cacheWrite": 1, "output": 4}, "fastCostUsd": 3, "speedPremiumUsd": 1})),
                    bucket(json!({"model": "mystery", "costUsd": 0, "unpricedRecords": 5, "costSource": "unpriced", "day": "2026-08-06"})),
                ],
                vec![source("claude", "mac", "/c", 3, "ok")],
                json!({}),
            ),
        )],
        USAGE_CONTRACT_VERSION,
    );
    assert_eq!(merged.category_cost.unsplit, 2.);
    assert_eq!((merged.speed_cost.standard, merged.speed_cost.fast, merged.speed_cost.premium), (7., 3., 1.));
    assert_eq!(merged.cost_quality.unpriced_share, 0.5);
    let mystery = merged.models.iter().find(|model| model.model == "mystery").unwrap();
    assert!(mystery.is_cost_unknown());
    assert_eq!(mystery.share(ChartMetric::Cost), None);
    assert_eq!(mystery.cost_per_million_tokens(), None);
    assert_eq!(merged.models[0].model, "claude-fable-5");
    assert_eq!(merged.sessions, 3);
    let segments = merge::cost_type_segments(&merged.category_cost);
    assert_eq!(merge::visible_segments(&segments).1, 10.);
    let fable = &merged.models[0];
    assert_eq!(fable.cache_hit_rate(), Some(1000. / 1110.));
}

#[test]
fn hidden_providers_drop_out_of_buckets_and_sources() {
    let both = summary(vec![bucket(json!({})), bucket(json!({"provider": "codex"}))], vec![source("claude", "m", "/c", 1, "ok"), source("codex", "m", "/x", 1, "ok")], json!({}));
    let hidden: BTreeSet<UsageProvider> = [UsageProvider::Codex].into();
    let shown = both.without_providers(&hidden);
    assert_eq!((shown.buckets.len(), shown.sources.len()), (1, 1));
    assert_eq!(provider_filter_label(&hidden), "7 providers");
    assert_eq!(provider_filter_label(&BTreeSet::new()), "All providers");
    assert_eq!(provider_filter_label(&UsageProvider::ORDER.into_iter().filter(|p| *p != UsageProvider::Claude).collect()), "Claude Code");
}

#[test]
fn preferences_fall_back_to_defaults_as_a_whole() {
    let defaults = UsagePagePreferences::default();
    assert_eq!((defaults.metric, defaults.period), (UsageMetric::Limits, UsagePeriod::Month));
    let saved = UsagePagePreferences::from_json(Some(&json!({"metric": "tokens", "windowDays": 7, "hiddenProviders": ["cursor"]})));
    assert_eq!((saved.metric, saved.period), (UsageMetric::Tokens, UsagePeriod::Week));
    assert_eq!(saved.to_json(), json!({"metric": "tokens", "windowDays": 7, "hiddenProviders": ["cursor"]}));
    assert_eq!(UsagePagePreferences::from_json(Some(&json!({"metric": "tokens", "windowDays": 14}))), defaults);
    assert_eq!(UsagePagePreferences::from_json(Some(&json!({"metric": "tokens", "windowDays": 7, "hiddenProviders": ["nope"]}))), defaults);
}

fn environment(summary: Option<UsageSummary>, pending: bool) -> EnvironmentUsage {
    EnvironmentUsage {
        environment_id: "env".into(),
        label: "Laptop".into(),
        is_connected: true,
        is_pending: pending,
        can_read_diagnostics: true,
        error: None,
        summary: summary.map(Arc::new),
        needs_cursor_keychain_access: false,
    }
}

#[test]
fn progress_names_what_is_still_scanning() {
    let partway = summary(vec![], vec![merge_into(source("codex", "m", "/x", 0, "ok"), json!({"refreshing": true}))], json!({}));
    assert_eq!(environment_progress(&environment(None, true), false), EnvironmentProgress::Loading);
    assert_eq!(environment_progress(&environment(Some(partway.clone()), true), false), EnvironmentProgress::Partway(vec![UsageProvider::Codex]));
    assert_eq!(environment_progress(&environment(Some(partway.clone()), false), false), EnvironmentProgress::Ready);
    assert_eq!(environment_progress(&environment(Some(partway.clone()), false), true), EnvironmentProgress::Stale);
    assert_eq!(environment_status_label(&environment(Some(partway.clone()), true), false), "Updating Codex…");
    let loading = loading_state(&[environment(Some(partway), true)], false, false, &BTreeSet::new());
    assert!(loading.partial && !loading.every_provider && loading.is_loading(UsageProvider::Codex));
    assert!(loading_state(&[environment(None, true)], false, true, &BTreeSet::new()).every_provider);
    let mismatched = summary(vec![], vec![], json!({"contractVersion": 2}));
    assert_eq!(environment_status_label(&environment(Some(mismatched), false), false), "Update required");
}

#[test]
fn access_needs_the_diagnostics_scope() {
    let session = |scopes: Value| -> crate::auth::AuthSessionState { serde_json::from_value(json!({"authenticated": true, "scopes": scopes})).unwrap() };
    let granted = resolve_usage_access(ConnectionPhase::Connected, Some(&session(json!(["diagnostics:read"]))), false);
    assert!(granted.can_read_diagnostics && granted.error.is_none());
    let legacy = resolve_usage_access(ConnectionPhase::Connected, Some(&session(json!(["orchestration:read"]))), false);
    assert!(legacy.can_read_diagnostics);
    let denied = resolve_usage_access(ConnectionPhase::Connected, Some(&session(json!([]))), false);
    assert_eq!(denied.error, Some("This connection does not have access to diagnostics and usage."));
    assert!(resolve_usage_access(ConnectionPhase::Connecting, None, false).is_pending);
    assert_eq!(resolve_usage_access(ConnectionPhase::Disconnected, None, false).error, Some("This environment is not connected."));
}

#[test]
fn chart_scales_to_the_tallest_single_series() {
    assert_eq!(chart::nice_scale(0., 4), (0., vec![0.]));
    assert_eq!(chart::nice_scale(37., 4), (40., vec![0., 10., 20., 30., 40.]));
    let periods: Vec<String> = ["2026-08-06", "2026-08-07", "2026-08-08"].map(str::to_owned).to_vec();
    let merged = merge::merge_usage(
        &[answered(
            "env",
            summary(
                vec![bucket(json!({"day": "2026-08-06", "costUsd": 30})), bucket(json!({"provider": "codex", "costUsd": 20}))],
                vec![source("claude", "m", "/c", 1, "ok"), source("codex", "m", "/x", 1, "ok")],
                json!({}),
            ),
        )],
        USAGE_CONTRACT_VERSION,
    );
    let built = chart::build_chart(&periods, &merged.daily, ChartMetric::Cost, &[UsageProvider::Codex, UsageProvider::Claude], &|_| false);
    assert_eq!(built.scale.max, 30.);
    assert_eq!(built.series[0].provider, UsageProvider::Claude);
    assert_eq!(built.columns[1].total, 20.);
    assert_eq!(built.step_x, 480.);
    let placeholder = chart::build_chart(&periods, &[], ChartMetric::Cost, &[UsageProvider::Codex], &|_| true);
    assert!(!placeholder.scale.labeled);
    assert_eq!(chart::hover_index(0.74, 3), Some(1));
}

#[test]
fn smoothed_curves_never_overshoot_their_points() {
    let points = [Point { x: 0., y: 260. }, Point { x: 1., y: 0. }, Point { x: 2., y: 0. }, Point { x: 3., y: 260. }];
    for segment in chart::smooth_curve(&points) {
        let (low, high) = (segment.from.y.min(segment.to.y), segment.from.y.max(segment.to.y));
        assert!(segment.c1.y >= low - 1e-9 && segment.c1.y <= high + 1e-9);
        assert!(segment.c2.y >= low - 1e-9 && segment.c2.y <= high + 1e-9);
    }
}

const NOW: &str = "2026-09-03T12:00:00.000Z";

fn window(extra: Value) -> Value {
    merge_into(json!({"id": "five_hour", "kind": "session", "label": "Session", "usedPercent": 40, "windowDurationMins": 300, "resetsAt": "2026-09-03T14:00:00.000Z"}), extra)
}

fn provider(instance: &str, driver: &str, auth: Value, windows: Vec<Value>, checked: &str) -> ServerProvider {
    serde_json::from_value(json!({
        "instanceId": instance, "driver": driver, "enabled": true, "installed": true, "status": "ready", "auth": auth, "models": [],
        "usageLimits": {"checkedAt": checked, "windows": windows}
    }))
    .unwrap()
}

#[test]
fn pace_reads_the_window_clock() {
    let now = parse_iso(NOW).unwrap();
    let session: UsageWindow = serde_json::from_value(window(json!({}))).unwrap();
    assert_eq!(limits::pace_remaining_percent(&session, now), Some(40));
    assert_eq!(limits::pace_of(&session, now), Some(LimitPace::Under));
    assert_eq!(limits::pace_of(&UsageWindow { used_percent: 62., ..session.clone() }, now), Some(LimitPace::On));
    assert_eq!(limits::pace_of(&UsageWindow { used_percent: 80., ..session.clone() }, now), Some(LimitPace::Ahead));
    assert_eq!(limits::format_resets_in(&session, now).as_deref(), Some("resets in 2h 0m"));
    assert_eq!(limits::format_duration(3 * 86_400_000 + 5 * 3_600_000), "3d 5h");
    assert_eq!(limits::remaining_percent(&UsageWindow { used_percent: 2.5, ..session }), 98);
}

#[test]
fn one_account_on_two_environments_and_a_hub_merges_into_one() {
    let now = parse_iso(NOW).unwrap();
    let native = provider("claude", "claudeAgent", json!({"status": "authenticated", "email": "Same@example.com"}), vec![window(json!({}))], "2026-09-03T11:00:00.000Z");
    let fresher = provider("claude", "claudeAgent", json!({"status": "authenticated", "email": "Same@example.com"}), vec![window(json!({"usedPercent": 55}))], "2026-09-03T11:30:00.000Z");
    let hub: UsageLimitSource = serde_json::from_value(json!({
        "id": "hub", "kind": "cliproxy", "label": "hub", "checkedAt": "2026-09-03T11:00:00.000Z",
        "accounts": [{"id": "claude-same@example.com.json", "driver": "claudeAgent", "email": "same@example.com", "plan": "Claude Subscription",
            "usageLimits": {"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [window(json!({"usedPercent": 10}))]}}]
    }))
    .unwrap();
    let (first, second) = ([native], [fresher]);
    let hubs = [hub];
    let environments = [
        LimitEnvironment { environment_id: "env-a", label: "Laptop", providers: &first, sources: &[] },
        LimitEnvironment { environment_id: "env-b", label: "Desktop", providers: &second, sources: &hubs },
    ];
    let accounts = limits::collect_limit_accounts(&environments);
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].key, "env-a:claude");
    assert_eq!(accounts[0].source_label, None);
    assert_eq!(accounts[0].plan.as_deref(), Some("Claude Subscription"));
    assert_eq!(accounts[0].environments.len(), 2);
    assert_eq!(accounts[0].limits.windows[0].used_percent, 55.);
    assert_eq!(accounts[0].redeem.as_ref().map(|redeem| redeem.environment_id.as_str()), Some("env-b"));
    let pools = limits::collect_limit_pools(accounts, now);
    assert_eq!(pools[0].windows[0].remaining_percent, 45);
    assert_eq!(pools[0].windows[0].pace, Some(LimitPace::On));
}

#[test]
fn codex_pools_windows_by_kind_whichever_slot_reported_them() {
    let now = parse_iso(NOW).unwrap();
    let account = |id: &str, slots: &[&str]| {
        let windows: Vec<Value> = slots.iter().map(|slot| json!({"id": slot, "kind": "weekly", "label": "Weekly", "usedPercent": 50, "windowDurationMins": 10080, "resetsAt": "2026-09-06T12:00:00.000Z"})).collect();
        json!({"id": id, "driver": "codex", "usageLimits": {"checkedAt": "2026-09-03T11:00:00.000Z", "windows": windows}})
    };
    let pooled = |accounts: Vec<Value>| -> Vec<String> {
        let source: UsageLimitSource = serde_json::from_value(json!({"id": "hub", "label": "hub", "checkedAt": NOW, "accounts": accounts})).unwrap();
        let sources = [source];
        let environments = [LimitEnvironment { environment_id: "env-a", label: "Laptop", providers: &[], sources: &sources }];
        limits::collect_limit_pools(limits::collect_limit_accounts(&environments), now).remove(0).windows.into_iter().map(|window| window.id).collect()
    };
    assert_eq!(pooled(vec![account("business", &["primary"]), account("plus", &["secondary"])]), ["weekly"]);
    assert_eq!(pooled(vec![account("a", &["secondary"]), account("b", &["primary", "secondary"])]), ["weekly", "weekly:secondary"]);
}

#[test]
fn a_spent_weekly_quota_blocks_the_session_until_it_resets() {
    let now = parse_iso(NOW).unwrap();
    let blocked = provider(
        "claude",
        "claudeAgent",
        json!({"status": "authenticated", "email": "a@example.com"}),
        vec![
            window(json!({"usedPercent": 40, "resetsAt": "2026-09-03T13:00:00.000Z"})),
            json!({"id": "seven_day", "kind": "weekly", "label": "Weekly", "usedPercent": 100, "windowDurationMins": 10080, "resetsAt": "2026-09-03T14:00:00.000Z"}),
        ],
        "2026-09-03T11:00:00.000Z",
    );
    let providers = [blocked];
    let environments = [LimitEnvironment { environment_id: "env", label: "Laptop", providers: &providers, sources: &[] }];
    let pools = limits::collect_limit_pools(limits::collect_limit_accounts(&environments), now);
    let session = &pools[0].windows[0];
    assert_eq!(session.remaining_percent, 0);
    assert_eq!(session.resets[0].at, parse_iso("2026-09-03T14:00:00.000Z").unwrap());
    assert_eq!(session.resets[0].restores_percent, 100);
    assert_eq!(session.pace, None);
}

#[test]
fn notices_and_external_links_explain_accounts_without_bars() {
    let unsupported = provider("codex", "codex", json!({"status": "authenticated"}), vec![], NOW);
    let mut failed = provider("cursor", "cursor", json!({"status": "authenticated"}), vec![], NOW);
    if let Some(limits) = failed.usage_limits.as_mut() {
        limits.unavailable = Some(crate::server_config::LimitsUnavailable { reason: "probeFailed".into(), message: None });
        limits.external_usage = Some(crate::server_config::ExternalUsage { label: "Cursor dashboard".into(), url: "https://cursor.com".into() });
    }
    let providers = [unsupported, failed];
    let environments = [LimitEnvironment { environment_id: "env", label: "Laptop", providers: &providers, sources: &[] }];
    assert_eq!(limits::collect_limit_notices(&environments), ["codex: No limits reported.", "cursor: Could not read limits."]);
    let links = limits::collect_external_usage_links(&environments);
    assert_eq!(links[0].accounts, ["cursor on Laptop"]);
    assert!(limits::collect_limit_accounts(&environments).is_empty());
}

#[test]
fn cursor_hides_overall_when_both_pools_report() {
    let now = parse_iso(NOW).unwrap();
    let windows: Vec<Value> = ["totalPercentUsed", "apiPercentUsed", "autoPercentUsed"]
        .iter()
        .map(|id| json!({"id": id, "kind": "monthly", "label": id, "usedPercent": 10}))
        .collect();
    let providers = [provider("cursor", "cursor", json!({"status": "authenticated"}), windows, NOW)];
    let environments = [LimitEnvironment { environment_id: "env", label: "Laptop", providers: &providers, sources: &[] }];
    let pools = limits::collect_limit_pools(limits::collect_limit_accounts(&environments), now);
    let shown: Vec<&str> = limits::display_limit_windows(&pools[0]).iter().map(|window| window.id.as_str()).collect();
    assert_eq!(shown, ["autoPercentUsed", "apiPercentUsed"]);
    assert_eq!(limits::cursor_usage_window_details("autoPercentUsed").map(|details| details.label), Some("Cursor Models"));
}

#[test]
fn usage_limits_report_lists_the_selected_driver() {
    let now = parse_iso(NOW).unwrap();
    let providers = [
        provider("codex", "codex", json!({"status": "authenticated", "label": "Plus"}), vec![window(json!({}))], NOW),
        provider("claude", "claudeAgent", json!({"status": "authenticated"}), vec![window(json!({}))], NOW),
    ];
    let report = limits::collect_provider_usage_limits("codex", &providers, &[], now).unwrap();
    assert_eq!(report.accounts.len(), 1);
    assert_eq!(report.accounts[0].label, "codex [codex]");
    assert_eq!(report.accounts[0].reset_credit_input, Some(limits::ResetCreditInput::Instance { instance_id: "codex".into() }));
    assert!(limits::collect_provider_usage_limits("missing", &providers, &[], now).is_none());
    assert_eq!(limits::account_initials("theo@t3.gg"), "TT");
    assert_eq!(limits::account_hue("a"), 97);
    assert_eq!(limits::reset_credits_summary(2, None, now, false), "2 reset credits banked");
}

fn projection(extra: Value) -> ThreadProjection {
    let mut value = json!({
        "thread": {
            "id": "t1", "projectId": "p1", "title": "Thread", "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "full-access",
            "interactionMode": "default", "branch": null, "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t1"},
            "activeProviderThreadId": "pt1", "createdAt": NOW, "updatedAt": NOW, "archivedAt": null
        },
        "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [],
        "plans": [], "turnItems": [], "checkpointScopes": [], "checkpoints": [],
        "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [], "updatedAt": NOW
    });
    merge_json(&mut value, extra);
    serde_json::from_value(value).unwrap()
}

#[test]
fn context_window_prefers_the_newest_turn_report() {
    let turns = json!({"providerTurns": [
        {"id": "a", "tokenUsage": {"usedTokens": 50000, "maxTokens": 200000, "updatedAt": "2026-09-03T12:00:00.000Z"}},
        {"id": "b", "tokenUsage": {"usedTokens": 190000, "maxTokens": 200000, "autoCompactThreshold": 180000, "updatedAt": "2026-09-03T12:05:00.000Z"}},
        {"id": "c"}
    ]});
    let snapshot = context_window_snapshot(&projection(turns)).unwrap();
    assert_eq!(snapshot.used_percentage, Some(95.));
    assert_eq!(context_window::meter_label(&snapshot), "Context window 95% used");
    assert_eq!(context_window::compaction_message(Some("GPT"), snapshot.auto_compact_threshold), "Compacts automatically at about 180,000 tokens.");
    let thread = json!({"providerThreads": [{"id": "pt1", "updatedAt": NOW, "contextUsage": {"usedTokens": 1200, "totalProcessedTokens": 99000, "cost": {"amount": 0.004, "currency": "USD"}}}]});
    let reported = context_window_snapshot(&projection(thread)).unwrap();
    assert_eq!((reported.max_tokens, reported.total_processed_tokens), (None, Some(99000.)));
    assert_eq!(context_window::format_context_cost(0.004, "USD"), "USD 0.0040");
    assert_eq!(context_window::meter_label(&reported), "Context window 1.2k tokens used");
    assert!(context_window_snapshot(&projection(json!({}))).is_none());
}

#[test]
fn context_tokens_and_percentages_read_compactly() {
    let tokens: Vec<String> = [Some(999.), Some(1500.), Some(12_400.), Some(999_499.), Some(1_250_000.), None].into_iter().map(context_window::format_context_tokens).collect();
    assert_eq!(tokens, ["999", "1.5k", "12k", "999k", "1.3m", "0"]);
    assert_eq!(context_window::format_meter_percentage(Some(4.04)).as_deref(), Some("4%"));
    assert_eq!(context_window::format_meter_percentage(Some(9.96)).as_deref(), Some("10%"));
    assert_eq!(context_window::format_meter_percentage(Some(42.5)).as_deref(), Some("43%"));
}

#[test]
fn cursor_keychain_prompt_stays_quiet_once_the_account_reports() {
    let needs = summary(vec![], vec![merge_into(source("cursor", "mac", "/cursor", 0, "ok"), json!({"action": "enableCursorKeychain"}))], json!({}));
    let account = summary(vec![], vec![source("cursor", "cursor.com", "/account", 0, "ok")], json!({}));
    assert_eq!(cursor_keychain_prompt(&[(Some(&needs), true)]), [0]);
    assert!(cursor_keychain_prompt(&[(Some(&needs), true), (Some(&account), false)]).is_empty());
}
